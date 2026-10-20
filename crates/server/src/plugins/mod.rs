//! 插件体系门面（ADR-0002 第 1/3 条）。
//!
//! `PluginManager` 是 `AppState` 上唯一的插件入口：装配 registry / scanner /
//! supervisor / rpc / credentials / tickets，对外只暴露领域方法。所有读取走注册表
//! 快照（不缓存贡献物清单），安装 / 停用 / 启用 / 卸载一律立即生效——
//! 路由树固定（catch-all 派发），永不因插件变化重建。

pub mod api_surface;
pub mod audit;
pub mod credentials;
pub mod manifest;
pub mod mcp_config;
pub mod mcp_proxy;
pub mod permissions;
pub mod registry;
pub mod rpc;
pub mod scanner;
pub mod supervisor;
pub mod view_ticket;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::config::Config;
use crate::drivers::events::HubEvent;

use audit::{AuditReport, Contribution};
use credentials::{CredentialStatus, CredentialsStore};
use manifest::Manifest;
use registry::{
    now_rfc3339, FaultKind, FaultRecord, InstalledPlugin, PluginEntry, PluginSource, PluginState,
    Registry, Transition,
};
use rpc::{HubCallHandler, HubFuture, RpcError};
use scanner::DiscoveredItem;
use supervisor::Supervisor;

/// 崩溃退避序列（ADR-0006 第 2 条）
pub const CRASH_BACKOFF_SECS: [u64; 3] = [10, 30, 60];
/// 崩溃退避次数上限（超过即稳定故障态，等待用户「重试加载」）
pub const MAX_CRASH_RETRIES: u32 = 3;

/// 插件域各路径（来自 Config，集中一份，避免散读）。
#[derive(Debug, Clone)]
pub struct PluginPaths {
    pub plugins_dir: PathBuf,
    pub state_file: PathBuf,
    pub credentials_file: PathBuf,
    pub audit_dir: PathBuf,
    pub data_dir: PathBuf,
    pub projects_file: PathBuf,
    pub scan_secs: u64,
    pub transient: Vec<PathBuf>,
}

impl PluginPaths {
    pub fn from_config(cfg: &Config) -> Self {
        Self {
            plugins_dir: cfg.plugins_dir.clone(),
            state_file: cfg.plugin_state_file.clone(),
            credentials_file: cfg.plugin_credentials_file.clone(),
            audit_dir: cfg.plugin_audit_dir.clone(),
            data_dir: cfg.plugin_data_dir.clone(),
            projects_file: cfg.projects_file.clone(),
            scan_secs: cfg.plugin_scan_secs,
            transient: cfg.plugin_transient_dirs.clone(),
        }
    }
}

/// 侧栏导航数据（ADR-0004 第 2 条）：仅「启用（就绪）」与「运行故障」且有视图贡献。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavEntry {
    pub plugin_id: String,
    pub plugin_name: String,
    pub plugin_version: String,
    pub view_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
    pub fault: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendInfo {
    pub running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub always_on: bool,
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionView {
    pub api: Vec<String>,
    pub data: Vec<String>,
    pub credentials: Vec<CredentialStatus>,
    pub high_risk: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewEntry {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<i64>,
}

/// `/api/plugins` 的条目视图（五态由持久态派生）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: PluginSource,
    pub enabled: bool,
    /// 持久态：`ready` | `stopped` | `fault`
    pub state: String,
    /// UI 五态：`enabled` | `disabled` | `fault`（未安装/安装失败不进注册表）
    pub ui_state: String,
    pub installed_at: String,
    pub content_hash: String,
    pub contributions: Vec<Contribution>,
    pub permissions: PermissionView,
    pub views: Vec<ViewEntry>,
    pub backend: BackendInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fault: Option<FaultRecord>,
    pub asset_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredView {
    pub items: Vec<DiscoveredItem>,
    pub last_scan_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub last_scan_at: String,
    pub discovered: usize,
}

#[derive(Debug, Clone)]
pub enum ViewFileError {
    NotFound,
    Denied(String),
}

pub struct ViewFile {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub no_store: bool,
}

#[derive(Default)]
struct RetryState {
    attempts: u32,
    next_at: Option<Instant>,
}

/// agent cwd 提供方（`hub.projects.effective` 聚合用）：返回全部 agent 的 cwd 列表。
pub type AgentCwdProvider = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send>>
        + Send
        + Sync,
>;

pub struct PluginManager {
    pub paths: PluginPaths,
    pub registry: Arc<Registry>,
    pub supervisor: Arc<Supervisor>,
    pub credentials: Arc<CredentialsStore>,
    pub tickets: Arc<view_ticket::TicketStore>,
    pub events_tx: broadcast::Sender<HubEvent>,
    /// 装配后回填的 agent cwd 提供方（`hub.projects.effective` 聚合用；
    /// 构造期为 None——drivers 在 AppState 装配序列里晚于插件门面建成）
    agent_cwds: StdMutex<Option<AgentCwdProvider>>,
    scan_cache: RwLock<scanner::ScanOutcome>,
    locks: StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    retries: StdMutex<HashMap<String, RetryState>>,
    asset_cache: RwLock<HashMap<String, u64>>,
}

impl std::fmt::Debug for PluginManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginManager")
            .field("plugins", &self.registry.list().len())
            .finish()
    }
}

impl PluginManager {
    pub fn new(cfg: &Config, events_tx: broadcast::Sender<HubEvent>) -> Arc<Self> {
        let paths = PluginPaths::from_config(cfg);
        let registry = Arc::new(Registry::open(paths.state_file.clone()));
        let credentials = Arc::new(CredentialsStore::load(paths.credentials_file.clone()));
        Arc::new(Self {
            registry,
            supervisor: Arc::new(Supervisor::new()),
            credentials,
            tickets: Arc::new(view_ticket::TicketStore::default()),
            events_tx,
            paths,
            agent_cwds: StdMutex::new(None),
            scan_cache: RwLock::new(scanner::ScanOutcome::default()),
            locks: StdMutex::new(HashMap::new()),
            retries: StdMutex::new(HashMap::new()),
            asset_cache: RwLock::new(HashMap::new()),
        })
    }

    /// 装配接线（AppState::from_config 末段调用）：drivers 全部注册完成后回填
    /// agent cwd 提供方，供 `hub.projects.effective` 聚合。
    pub fn set_agent_cwd_provider(&self, provider: AgentCwdProvider) {
        let mut guard = self.agent_cwds.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(provider);
    }

    fn broadcast(&self, id: &str) {
        let _ = self
            .events_tx
            .send(HubEvent::PluginsChanged { id: id.into() });
    }

    fn plugin_lock(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn handler(self: &Arc<Self>) -> Arc<dyn HubCallHandler> {
        let agent_cwds = self
            .agent_cwds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| Arc::new(|| Box::pin(async { Vec::new() })));
        Arc::new(HubRpcHandler {
            registry: self.registry.clone(),
            credentials: self.credentials.clone(),
            data_dir: self.paths.data_dir.clone(),
            projects_file: self.paths.projects_file.clone(),
            agent_cwds,
        })
    }

    fn scrubber(self: &Arc<Self>) -> Arc<dyn Fn(String) -> String + Send + Sync> {
        let creds = self.credentials.clone();
        Arc::new(move |s: String| creds.scrub(&s))
    }

    // ===== 启动装载（ADDR-0002 第 4 条：单插件失败不阻塞主体） =====

    /// 启动序列：恢复注册表条目 → 重新解析 manifest → 首次扫描 → 起后台任务。
    pub async fn startup(self: &Arc<Self>) {
        let entries = self.registry.list();
        for p in entries {
            let id = p.entry.id.clone();
            // 停用条目不参与启动装载（设计 §6.2）：目录缺失 / manifest 损坏都保持停用态，
            // 不翻转 enabled——否则用户停用 + 目录被删的插件会在重启后漂移为
            // 「启用 + 运行故障」，侧栏故障入口重现（review Round-1 BLOCKER：
            // enabled 位绕过 BR-01 迁移表单一入口）。显式启用/重试路径的装载
            // 失败分支已覆盖 dir-missing 呈态，停用态的呈态无需在此补写。
            if !p.entry.enabled {
                continue;
            }
            if !p.plugin_dir.as_os_str().is_empty() && p.plugin_dir.join("plugin.json").is_file() {
                // 已解析过目录（同进程内插入）
                continue;
            }
            // 持久态加载的条目：目录定位 = plugins_dir/<id>（drop-in）或 source.ref（transient）
            let dir = match &p.entry.source {
                s if s.is_transient() => s
                    .reference
                    .as_ref()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.paths.plugins_dir.join(&id)),
                _ => self.paths.plugins_dir.join(&id),
            };
            if !dir.join("plugin.json").is_file() {
                self.fail_load(
                    &id,
                    "dir-missing",
                    format!("插件目录不存在：{}（可能已被手工删除）", dir.display()),
                );
                continue;
            }
            match load_manifest(&dir) {
                Ok(m) => {
                    self.registry.update_snapshot(&id, m, dir);
                    if p.entry.enabled && p.entry.state != PluginState::Fault {
                        // 恢复启用态：重新登记为就绪（backend 懒启动，此刻不 spawn）
                        let _ = self.registry.apply(&id, Transition::EnableOk);
                    }
                }
                Err(msg) => self.fail_load(&id, "manifest-invalid", msg),
            }
            self.broadcast(&id);
        }

        // 首次扫描（发现 ≠ 安装）
        self.scan_now();

        // 周期扫描（ADR-0002 第 2 条；0 = 关闭）
        if self.paths.scan_secs > 0 {
            let me = self.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(me.paths.scan_secs));
                tick.tick().await; // 首次立即返回（首次扫描已在 startup 做过）
                loop {
                    tick.tick().await;
                    me.scan_now();
                }
            });
        }

        // 崩溃检测 / 退避重启 / 空闲回收
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                me.monitor_once().await;
            }
        });
    }

    /// 后台监视一周期（纯逻辑可测）：死进程 → 崩溃记录 + 退避重试；空闲 → 回收。
    pub async fn monitor_once(self: &Arc<Self>) {
        // 1) 崩溃检测：进程已死但注册表里仍是启用态
        for run in self.supervisor.snapshot() {
            if !run.channel.is_alive() {
                self.supervisor.forget(&run.plugin_id);
                self.on_crash(&run.plugin_id).await;
            }
        }
        // 2) 到点的退避重启
        let due: Vec<String> = {
            let map = self.retries.lock().unwrap_or_else(|e| e.into_inner());
            map.iter()
                .filter(|(_, st)| st.next_at.map(|t| t <= Instant::now()).unwrap_or(false))
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in due {
            self.attempt_restart(&id).await;
        }
        // 3) 空闲回收
        self.supervisor.reap_idle().await;
    }

    async fn on_crash(self: &Arc<Self>, id: &str) {
        let Some(p) = self.registry.get(id) else {
            return;
        };
        if !p.entry.enabled {
            return;
        }
        let rec = FaultRecord {
            at: now_rfc3339(),
            kind: FaultKind::Crash,
            message: "崩溃：backend 进程已退出（code crash）".into(),
        };
        let _ = self.registry.set_fault(id, rec);
        self.schedule_retry(id, "崩溃");
        self.broadcast(id);
    }

    fn schedule_retry(&self, id: &str, reason: &str) {
        let mut map = self.retries.lock().unwrap_or_else(|e| e.into_inner());
        let st = map.entry(id.to_string()).or_default();
        if st.attempts >= MAX_CRASH_RETRIES {
            tracing::warn!(plugin = %id, reason = %reason, "崩溃退避次数已用尽，保持故障态等待手动重试");
            st.next_at = None;
            return;
        }
        let backoff = CRASH_BACKOFF_SECS[st.attempts as usize];
        st.attempts += 1;
        st.next_at = Some(Instant::now() + Duration::from_secs(backoff));
        tracing::info!(plugin = %id, backoff_secs = backoff, attempt = st.attempts, "安排退避重启");
    }

    async fn attempt_restart(self: &Arc<Self>, id: &str) {
        {
            let mut map = self.retries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(st) = map.get_mut(id) {
                st.next_at = None;
            }
        }
        let Some(p) = self.registry.get(id) else {
            return;
        };
        if !p.entry.enabled {
            return;
        }
        match self
            .supervisor
            .ensure_started(&p, &self.credentials, self.handler(), self.scrubber())
            .await
        {
            Ok(_) => {
                let _ = self.registry.apply(id, Transition::RetryOk);
                let _ = self.registry.push_record(
                    id,
                    FaultRecord {
                        at: now_rfc3339(),
                        kind: FaultKind::Crash,
                        message: "自动重启 · 已恢复（整体重登）".into(),
                    },
                );
                self.retries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(id);
                self.broadcast(id);
            }
            Err(e) => {
                let msg = self.credentials.scrub(&e);
                let _ = self.registry.set_fault(
                    id,
                    FaultRecord {
                        at: now_rfc3339(),
                        kind: FaultKind::LoadFailure,
                        message: format!("自动重启失败：{msg}"),
                    },
                );
                self.schedule_retry(id, "自动重启失败");
                self.broadcast(id);
            }
        }
    }

    fn fail_load(&self, id: &str, code: &str, message: String) {
        let rec = FaultRecord {
            at: now_rfc3339(),
            kind: FaultKind::LoadFailure,
            message: format!("装载失败（{code}）：{}", self.credentials.scrub(&message)),
        };
        let _ = self.registry.set_fault(id, rec);
        self.broadcast(id);
    }

    // ===== 查询面 =====

    pub fn list(&self) -> Vec<PluginView> {
        self.registry
            .list()
            .into_iter()
            .map(|p| self.view_of(&p))
            .collect()
    }

    fn view_of(&self, p: &InstalledPlugin) -> PluginView {
        let ui_state = if !p.entry.enabled {
            "disabled"
        } else if p.entry.state == PluginState::Fault {
            "fault"
        } else {
            "enabled"
        };
        let m = &p.manifest;
        let running = self
            .supervisor
            .snapshot()
            .into_iter()
            .find(|r| r.plugin_id == p.entry.id && r.channel.is_alive());
        PluginView {
            id: p.entry.id.clone(),
            name: if m.name.is_empty() {
                p.entry.id.clone()
            } else {
                m.name.clone()
            },
            version: p.entry.version.clone(),
            description: m.description.clone(),
            source: p.entry.source.clone(),
            enabled: p.entry.enabled,
            state: match p.entry.state {
                PluginState::Ready => "ready",
                PluginState::Stopped => "stopped",
                PluginState::Fault => "fault",
            }
            .into(),
            ui_state: ui_state.into(),
            installed_at: p.entry.installed_at.clone(),
            content_hash: p.entry.content_hash.clone(),
            contributions: audit::contributions_of(m),
            permissions: PermissionView {
                api: m.permissions.api.clone(),
                data: m.permissions.data.clone(),
                credentials: self.credentials.statuses(&m.permissions.credentials),
                high_risk: m
                    .permissions
                    .high_risk
                    .iter()
                    .map(|h| api_surface::high_risk_label(h).to_string())
                    .collect(),
            },
            views: m
                .contributes
                .views
                .iter()
                .map(|v| ViewEntry {
                    id: v.id.clone(),
                    title: v.title.clone(),
                    order: v.order,
                })
                .collect(),
            backend: BackendInfo {
                running: running.is_some(),
                pid: running.as_ref().and_then(|r| r.pid),
                always_on: m
                    .backend
                    .as_ref()
                    .and_then(|b| b.always_on)
                    .unwrap_or(false),
                idle_timeout_secs: m
                    .backend
                    .as_ref()
                    .and_then(|b| b.idle_timeout_secs)
                    .unwrap_or(supervisor::DEFAULT_IDLE_SECS),
            },
            last_fault: p.entry.last_fault.clone(),
            asset_bytes: self.asset_bytes(p),
        }
    }

    /// 视图资产体积（ADR-0010 第 4 条展示用）：按 (id, contentHash) 缓存，目录变更即重算。
    fn asset_bytes(&self, p: &InstalledPlugin) -> u64 {
        let cache_key = format!("{}:{}", p.entry.id, p.entry.content_hash);
        if let Ok(c) = self.asset_cache.read() {
            if let Some(v) = c.get(&cache_key) {
                return *v;
            }
        }
        let bytes = view_root(p).map(|r| dir_bytes(&r, 0)).unwrap_or(0);
        if let Ok(mut c) = self.asset_cache.write() {
            c.insert(cache_key, bytes);
        }
        bytes
    }

    /// 侧栏导航组数据（BR-03：停用/卸载后入口消失；故障态保留 + 徽标）。
    pub fn nav_entries(&self) -> Vec<NavEntry> {
        let mut out = Vec::new();
        for p in self.registry.list() {
            if !p.entry.enabled {
                continue;
            }
            let fault = p.entry.state == PluginState::Fault;
            let fault_message = if fault {
                p.entry.last_fault.as_ref().map(|f| f.message.clone())
            } else {
                None
            };
            for v in &p.manifest.contributes.views {
                out.push(NavEntry {
                    plugin_id: p.entry.id.clone(),
                    plugin_name: if p.manifest.name.is_empty() {
                        p.entry.id.clone()
                    } else {
                        p.manifest.name.clone()
                    },
                    plugin_version: p.entry.version.clone(),
                    view_id: v.id.clone(),
                    title: v.title.clone(),
                    icon: v.icon.clone(),
                    order: v.order,
                    fault,
                    fault_message: fault_message.clone(),
                });
            }
        }
        out.sort_by(|a, b| {
            a.order
                .unwrap_or(1000)
                .cmp(&b.order.unwrap_or(1000))
                .then_with(|| a.plugin_id.cmp(&b.plugin_id))
                .then_with(|| a.view_id.cmp(&b.view_id))
        });
        out
    }

    pub fn discovered(&self) -> DiscoveredView {
        if self
            .scan_cache
            .read()
            .map(|c| c.last_scan_at.is_empty())
            .unwrap_or(true)
        {
            // 从未扫描（理论上 startup 已扫过）：同步补一次，保证首屏有数据
            self.scan_now();
        }
        let cache = self
            .scan_cache
            .read()
            .map(|c| c.clone())
            .unwrap_or_default();
        DiscoveredView {
            items: cache.items,
            last_scan_at: cache.last_scan_at,
        }
    }

    /// 手动/周期扫描：只发现，不安装。
    pub fn scan_now(&self) -> ScanResult {
        let outcome = scanner::scan(
            &self.paths.plugins_dir,
            &self.paths.transient,
            &self.registry,
            &self.credentials,
        );
        for item in &outcome.items {
            if let Err(e) = audit::persist(&item.report, &self.paths.audit_dir) {
                tracing::warn!(plugin = %item.id, error = %e, "审计产物落盘失败");
            }
        }
        let discovered = outcome.items.len();
        let last = outcome.last_scan_at.clone();
        if let Ok(mut c) = self.scan_cache.write() {
            *c = outcome;
        }
        let _ = self
            .events_tx
            .send(HubEvent::PluginsChanged { id: "scan".into() });
        ScanResult {
            last_scan_at: last,
            discovered,
        }
    }

    /// 只审计不安装（插件作者自检入口，等价 `claude plugin validate`）。
    pub fn audit_target(
        &self,
        path: Option<&str>,
        id: Option<&str>,
    ) -> Result<AuditReport, String> {
        let dir = match (path, id) {
            (Some(p), _) => PathBuf::from(p),
            (None, Some(id)) => self
                .registry
                .get(id)
                .map(|p| p.plugin_dir)
                .ok_or_else(|| format!("插件 {id} 未安装"))?,
            (None, None) => return Err("需要 path 或 id".into()),
        };
        if !dir.is_dir() {
            return Err(format!("目录不存在：{}", dir.display()));
        }
        Ok(audit::audit_dir(&dir, &self.credentials))
    }

    // ===== 安装 / 启停 / 卸载 =====

    /// 安装并启用（BR-02；hash 再校验防 TOCTOU）。
    pub async fn install(
        self: &Arc<Self>,
        id: &str,
        content_hash: &str,
    ) -> Result<(PluginView, bool), String> {
        let lock = self.plugin_lock(id);
        let _guard = lock.lock().await;

        // 已装且同 hash：幂等无操作；hash 变化 = 更新候选（BR-13 确认流在 B4，B1 拒绝静默覆盖）
        if let Some(existing) = self.registry.get(id) {
            if existing.entry.content_hash == content_hash {
                return Ok((self.view_of(&existing), true));
            }
            return Err(format!(
                "插件 {id} 内容已变化（已装 {} / 新目录 {content_hash}）；更新确认流在后续批次交付，本次拒绝静默覆盖（BR-13）",
                existing.entry.content_hash
            ));
        }

        // 重新审计（目录可能已变）
        let outcome = scanner::scan(
            &self.paths.plugins_dir,
            &self.paths.transient,
            &self.registry,
            &self.credentials,
        );
        let item = outcome
            .items
            .iter()
            .find(|i| i.id == id)
            .ok_or_else(|| format!("未发现插件 {id}（请先扫描）"))?;
        if !item.report.errors.is_empty() {
            let first = &item.report.errors[0];
            return Err(format!(
                "审计未通过，拒绝安装：{}（字段 {} / 规则 {}）",
                first.message, first.field, first.rule
            ));
        }
        if item.report.content_hash != content_hash {
            return Err(format!(
                "目录内容在审计后已变化（hash 不一致：审计 {content_hash} / 当前 {}），已拒绝安装，请刷新后重新确认",
                item.report.content_hash
            ));
        }
        let dir = PathBuf::from(&item.path);
        let raw = std::fs::read_to_string(dir.join("plugin.json"))
            .map_err(|e| format!("读取 plugin.json 失败：{e}"))?;
        let parsed = manifest::parse(&raw).map_err(|errs| {
            format!(
                "manifest 校验失败：{}",
                errs.first().map(|e| e.message.clone()).unwrap_or_default()
            )
        })?;
        let manifest = parsed.manifest;
        let entry = PluginEntry {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            content_hash: item.report.content_hash.clone(),
            permissions_hash: registry::permissions_hash(&manifest.permissions),
            source: if item.source_kind == "transient" {
                PluginSource::transient(&dir)
            } else {
                PluginSource::drop_in()
            },
            installed_at: now_rfc3339(),
            // 先落「未启用 + stopped」，再走一次合法迁移到启用/就绪
            //（BR-01 迁移表是唯一入口，不在 insert 里造非法初态）
            enabled: false,
            state: PluginState::Stopped,
            last_fault: None,
        };
        let (installed, created) = self
            .registry
            .insert_installed(entry, manifest, dir)
            .map_err(|e| format!("注册表写入失败：{e}"))?;
        if created {
            // 装载 = 贡献物登记（backend 懒启动）：解析无误即就绪
            if let Err(e) = self.registry.apply(id, Transition::EnableOk) {
                tracing::warn!(plugin = %id, error = %e, "安装后启用迁移失败");
                self.fail_load(id, "load-failed", e);
            }
            let _ = audit::persist(&item.report, &self.paths.audit_dir);
        }
        self.scan_now();
        self.broadcast(id);
        let p = self.registry.get(id).unwrap_or(installed);
        Ok((self.view_of(&p), !created))
    }

    pub async fn enable(self: &Arc<Self>, id: &str) -> Result<PluginView, String> {
        let lock = self.plugin_lock(id);
        let _guard = lock.lock().await;
        let Some(p) = self.registry.get(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        if p.entry.enabled && p.entry.state != PluginState::Fault {
            return Ok(self.view_of(&p));
        }
        if p.entry.enabled {
            // 故障态「启用」= 重试同路径（BR-01：运行故障 →（重试成功）→ 启用）
            drop(_guard);
            return self.retry(id).await;
        }
        // 启用：重新解析 manifest → 重新登记贡献物（技术设计 §6.3）
        let dir = plugin_dir_of(&p, &self.paths);
        if !dir.join("plugin.json").is_file() {
            let _ = self.registry.apply(id, Transition::EnableFailed);
            self.fail_load(
                id,
                "dir-missing",
                format!("插件目录不存在：{}（可能已被手工删除）", dir.display()),
            );
            let p = self.registry.get(id).expect("entry exists");
            return Ok(self.view_of(&p));
        }
        match load_manifest(&dir) {
            Ok(m) => {
                self.registry.update_snapshot(id, m, dir);
                let _ = self.registry.apply(id, Transition::EnableOk);
            }
            Err(msg) => {
                let _ = self.registry.apply(id, Transition::EnableFailed);
                self.fail_load(id, "manifest-invalid", msg);
            }
        }
        self.broadcast(id);
        let p = self.registry.get(id).expect("entry exists");
        Ok(self.view_of(&p))
    }

    pub async fn disable(self: &Arc<Self>, id: &str) -> Result<PluginView, String> {
        let lock = self.plugin_lock(id);
        let _guard = lock.lock().await;
        let Some(p) = self.registry.get(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        if !p.entry.enabled {
            return Ok(self.view_of(&p));
        }
        // 停用：先停进程组与票据，再从查询面消失（无缓存窗口）
        self.supervisor.stop(id).await;
        self.tickets.revoke_plugin(id);
        self.retries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let _ = self.registry.apply(id, Transition::Disable);
        self.broadcast(id);
        let p = self.registry.get(id).expect("entry exists");
        Ok(self.view_of(&p))
    }

    /// 故障重试（BR-09 整体重登）：先清残留运行时资源，再重新解析 + 重新登记。
    pub async fn retry(self: &Arc<Self>, id: &str) -> Result<PluginView, String> {
        let lock = self.plugin_lock(id);
        let _guard = lock.lock().await;
        let Some(p) = self.registry.get(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        // 1) 清理（不出现「旧登记 + 新装载」混合态）
        self.supervisor.stop(id).await;
        self.tickets.revoke_plugin(id);
        self.retries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        // 2) 重新解析 + 重新登记
        let dir = plugin_dir_of(&p, &self.paths);
        if !dir.join("plugin.json").is_file() {
            self.fail_load(
                id,
                "dir-missing",
                format!("插件目录不存在：{}（可能已被手工删除）", dir.display()),
            );
            self.broadcast(id);
            let p = self.registry.get(id).expect("entry exists");
            return Ok(self.view_of(&p));
        }
        match load_manifest(&dir) {
            Ok(m) => {
                self.registry.update_snapshot(id, m, dir);
                let t = if p.entry.enabled {
                    Transition::RetryOk
                } else {
                    Transition::EnableOk
                };
                let _ = self.registry.apply(id, t);
                let _ = self.registry.push_record(
                    id,
                    FaultRecord {
                        at: now_rfc3339(),
                        kind: FaultKind::LoadFailure,
                        message: "手动重试 · 已恢复（整体重登）".into(),
                    },
                );
            }
            Err(msg) => {
                self.fail_load(id, "manifest-invalid", msg);
            }
        }
        self.broadcast(id);
        let p = self.registry.get(id).expect("entry exists");
        Ok(self.view_of(&p))
    }

    /// 卸载（BR-05）：清运行时资源 + 移出注册表；目录默认保留；自持数据默认保留。
    pub async fn uninstall(
        self: &Arc<Self>,
        id: &str,
        purge_data: bool,
    ) -> Result<PluginEntry, String> {
        let lock = self.plugin_lock(id);
        let _guard = lock.lock().await;
        let Some(p) = self.registry.get(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        self.supervisor.stop(id).await;
        self.tickets.revoke_plugin(id);
        self.retries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let removed = self
            .registry
            .remove(id)
            .ok_or_else(|| format!("插件 {id} 不在注册表中"))?;
        if purge_data {
            let data = self.paths.data_dir.join(id);
            if data.is_dir() {
                if let Err(e) = std::fs::remove_dir_all(&data) {
                    tracing::warn!(plugin = %id, error = %e, "插件自持数据清除失败");
                }
            }
        }
        let _ = p;
        self.scan_now();
        self.broadcast(id);
        Ok(removed)
    }

    // ===== 视图票据 / 资产 =====

    pub fn issue_ticket(&self, id: &str) -> Result<(String, u64), String> {
        let Some(p) = self.registry.get(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        if !p.entry.enabled {
            return Err(format!("插件 {id} 已停用"));
        }
        Ok(self.tickets.issue(id))
    }

    /// 视图资产读取：路径必须落在该插件的 view 子树内（符号链接越界同样拒绝）。
    pub fn view_file(&self, id: &str, rel: &str) -> Result<ViewFile, ViewFileError> {
        let p = self
            .registry
            .get(id)
            .ok_or_else(|| ViewFileError::Denied("插件未安装".into()))?;
        if !p.entry.enabled {
            return Err(ViewFileError::Denied("插件已停用".into()));
        }
        let Some(root) = view_root(&p) else {
            return Err(ViewFileError::NotFound);
        };
        let rel = rel.trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };
        let candidate = root.join(rel);
        if !candidate.is_file() {
            return Err(ViewFileError::NotFound);
        }
        let root_canon = root
            .canonicalize()
            .map_err(|_| ViewFileError::Denied("视图目录不可读".into()))?;
        let canon = candidate
            .canonicalize()
            .map_err(|_| ViewFileError::NotFound)?;
        if !canon.starts_with(&root_canon) {
            return Err(ViewFileError::Denied("路径逃出视图目录".into()));
        }
        let bytes = std::fs::read(&canon).map_err(|_| ViewFileError::NotFound)?;
        let no_store = rel.ends_with(".html");
        Ok(ViewFile {
            bytes,
            mime: mime_for(&canon),
            no_store,
        })
    }

    // ===== 调用面 =====

    /// 视图经宿主转发的 RPC：`hub.backend.<toolId>` 转发插件 backend；
    /// 其余方法走 hub 代执行（权限判定在 handler 内，越界留痕）。
    pub async fn rpc_call(
        self: &Arc<Self>,
        id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        if let Some(tool_id) = method.strip_prefix("hub.backend.") {
            return self.invoke_tool(id, tool_id, params).await;
        }
        // 视图侧命名空间 `hub.api.<domain>.<action>` 映射到 hub 方法表
        // （`hub.api.projects.list` → `hub.projects.list`，技术设计 §4.2）
        let normalized = match method.strip_prefix("hub.api.") {
            Some(rest) => format!("hub.{rest}"),
            None => method.to_string(),
        };
        let handler = self.handler();
        handler.handle_call(id, &normalized, params).await
    }

    async fn invoke_tool(
        self: &Arc<Self>,
        id: &str,
        tool_id: &str,
        params: Value,
    ) -> Result<Value, RpcError> {
        let Some(p) = self.registry.get(id) else {
            return Err(RpcError::PluginError(format!("插件 {id} 未安装")));
        };
        if !p.entry.enabled {
            return Err(RpcError::Unauthorized(format!("插件 {id} 已停用")));
        }
        if p.entry.state == PluginState::Fault {
            return Err(RpcError::PluginError(format!(
                "插件 {id} 处于运行故障态：{}",
                p.entry
                    .last_fault
                    .as_ref()
                    .map(|f| f.message.clone())
                    .unwrap_or_else(|| "原因未知".into())
            )));
        }
        let Some(tool) = p
            .manifest
            .contributes
            .tools
            .iter()
            .find(|t| t.id == tool_id)
        else {
            let msg = format!("工具 {tool_id} 未在 manifest 中声明");
            let _ = self.registry.push_record(
                id,
                FaultRecord {
                    at: now_rfc3339(),
                    kind: FaultKind::Violation,
                    message: self.credentials.scrub(&format!("越界调用：{msg}")),
                },
            );
            return Err(RpcError::Violation(msg));
        };
        if tool.risk == "high" {
            // B1 无审批流（B2 交付）：高危工具拒绝执行而不是静默放行
            return Err(RpcError::Unauthorized(format!(
                "高危工具 {tool_id} 需逐次确认（审批流在 B2 交付），当前版本拒绝执行"
            )));
        }
        if tool.implementation.kind != "backend" {
            return Err(RpcError::PluginError(format!(
                "工具 {tool_id} 的实现类型 {} 在 B1 未支持",
                tool.implementation.kind
            )));
        }
        let args = params.get("args").cloned().unwrap_or(json!({}));
        let call_id = params
            .get("callId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("view-{}", uuid::Uuid::new_v4()));
        let session_id = params.get("sessionId").and_then(Value::as_str);
        self.supervisor
            .invoke_tool(
                &p,
                &self.credentials,
                self.handler(),
                self.scrubber(),
                tool_id,
                args,
                &call_id,
                session_id,
            )
            .await
    }

    /// 记录一条违规（调用方在任何拒绝点使用）。
    pub fn record_violation(&self, id: &str, message: &str) {
        if self.registry.get(id).is_none() {
            return;
        }
        let _ = self.registry.push_record(
            id,
            FaultRecord {
                at: now_rfc3339(),
                kind: FaultKind::Violation,
                message: self.credentials.scrub(message),
            },
        );
    }

    pub fn records(&self, id: &str, limit: usize) -> Vec<FaultRecord> {
        let mut r = self.registry.records(id);
        if limit > 0 && r.len() > limit {
            r = r.split_off(r.len() - limit);
        }
        r
    }

    /// hub 退出：回收全部 backend 进程组。
    pub async fn shutdown(&self) {
        self.supervisor.shutdown_all().await;
    }
}

/// backend → hub 的方法派发（权限判定在 `permissions::check` 单点完成）。
struct HubRpcHandler {
    registry: Arc<Registry>,
    credentials: Arc<CredentialsStore>,
    data_dir: PathBuf,
    projects_file: PathBuf,
    agent_cwds: AgentCwdProvider,
}

impl HubCallHandler for HubRpcHandler {
    fn handle_call(&self, plugin_id: &str, method: &str, params: Value) -> HubFuture {
        let plugin_id = plugin_id.to_string();
        let method = method.to_string();
        let registry = self.registry.clone();
        let credentials = self.credentials.clone();
        let data_dir = self.data_dir.clone();
        let projects_file = self.projects_file.clone();
        let agent_cwds = self.agent_cwds.clone();
        let violation_self = Arc::new(());
        let _ = violation_self;
        Box::pin(async move {
            let Some(p) = registry.get(&plugin_id) else {
                return Err(RpcError::Unauthorized(format!("插件 {plugin_id} 未安装")));
            };
            if !p.entry.enabled {
                return Err(RpcError::Unauthorized(format!("插件 {plugin_id} 已停用")));
            }
            let call = permissions::PluginCall {
                method: method.clone(),
                params: params.clone(),
            };
            if let Err(deny) = permissions::check(&call, &p.manifest.permissions) {
                // 每次越界留痕（BR-12）
                let _ = registry.push_record(
                    &plugin_id,
                    FaultRecord {
                        at: now_rfc3339(),
                        kind: FaultKind::Violation,
                        message: credentials
                            .scrub(&format!("越界调用 {}：{}", method, deny.message)),
                    },
                );
                return Err(match deny.kind {
                    permissions::DenyKind::CredentialUnknown
                    | permissions::DenyKind::DataDenied
                    | permissions::DenyKind::HighRiskNeedsApproval => {
                        RpcError::Violation(deny.message)
                    }
                    permissions::DenyKind::NotDeclared => RpcError::Unauthorized(deny.message),
                });
            }

            match method.as_str() {
                "hub.credentials.use" => {
                    let name = params
                        .get("name")
                        .and_then(Value::as_str)
                        .ok_or_else(|| RpcError::PluginError("缺少 name 参数".into()))?;
                    let Some(value) = credentials.get(name) else {
                        return Err(RpcError::PluginError(format!(
                            "credential-unconfigured: 凭据 {name} 未配置"
                        )));
                    };
                    // 值只在应答载荷出现；日志侧由 scrub 兜底
                    Ok(json!({ "name": name, "value": value }))
                }
                "hub.storage.get" => {
                    let key = storage_key(&params)?;
                    let path = data_dir.join(&plugin_id).join(&key);
                    match std::fs::read_to_string(&path) {
                        Ok(raw) => Ok(json!({ "value": raw })),
                        Err(_) => Ok(json!({ "value": null })),
                    }
                }
                "hub.storage.set" => {
                    let key = storage_key(&params)?;
                    let value = params
                        .get("value")
                        .and_then(Value::as_str)
                        .ok_or_else(|| RpcError::PluginError("缺少 value 参数".into()))?;
                    if value.len() > 1024 * 1024 {
                        return Err(RpcError::PluginError("单次写入上限 1MB".into()));
                    }
                    let dir = data_dir.join(&plugin_id);
                    std::fs::create_dir_all(&dir)
                        .map_err(|e| RpcError::PluginError(format!("创建数据目录失败：{e}")))?;
                    let path = dir.join(&key);
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(&path, value)
                        .map_err(|e| RpcError::PluginError(format!("写入失败：{e}")))?;
                    Ok(json!({ "ok": true }))
                }
                "hub.log.emit" => {
                    let msg = params.get("message").and_then(Value::as_str).unwrap_or("");
                    tracing::info!(plugin = %plugin_id, "plugin log: {}", credentials.scrub(msg));
                    Ok(json!({ "ok": true }))
                }
                "hub.projects.list" => {
                    let projects: Vec<String> = std::fs::read_to_string(&projects_file)
                        .ok()
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_default();
                    Ok(json!({ "projects": projects }))
                }
                "hub.projects.effective" => {
                    // 可验证工程清单（预览反馈第 3 轮）：工程注册表 ∪ 全部 agent cwd 去重，
                    // 每项含 path 与 source（registry | agent-cwd）——视图下拉据此让用户
                    // 看到自己日常的真实工程。聚合既有数据源，无新状态（ADR-0003 只增）。
                    let mut items: Vec<Value> = Vec::new();
                    let mut seen = std::collections::HashSet::new();
                    let registry_projects: Vec<String> = std::fs::read_to_string(&projects_file)
                        .ok()
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_default();
                    for p in registry_projects {
                        if seen.insert(p.clone()) {
                            items.push(json!({ "path": p, "source": "registry" }));
                        }
                    }
                    // agent cwd：`all=1` 全量清单（含已完成会话），cwd 缺失的条目跳过
                    for cwd in agent_cwds().await {
                        if seen.insert(cwd.clone()) {
                            items.push(json!({ "path": cwd, "source": "agent-cwd" }));
                        }
                    }
                    Ok(json!({ "projects": items }))
                }
                other => {
                    let _ = registry.push_record(
                        &plugin_id,
                        FaultRecord {
                            at: now_rfc3339(),
                            kind: FaultKind::Violation,
                            message: format!("未知 RPC 方法 {other}"),
                        },
                    );
                    Err(RpcError::Unauthorized(format!("未知 RPC 方法 {other}")))
                }
            }
        })
    }
}

fn storage_key(params: &Value) -> Result<String, RpcError> {
    let key = params
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::PluginError("缺少 key 参数".into()))?;
    let p = Path::new(key);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(RpcError::Violation(
            "storage key 不得逃出插件数据目录".into(),
        ));
    }
    Ok(key.to_string())
}

fn load_manifest(dir: &Path) -> Result<Manifest, String> {
    let raw = std::fs::read_to_string(dir.join("plugin.json"))
        .map_err(|e| format!("读取 plugin.json 失败：{e}"))?;
    manifest::parse(&raw).map(|p| p.manifest).map_err(|errs| {
        errs.iter()
            .map(|e| format!("[{} / {}] {}", e.field, e.rule, e.message))
            .collect::<Vec<_>>()
            .join("；")
    })
}

/// 目录定位：drop-in 恒为 `plugins_dir/<id>`（ADR-0001 第 4 条）；
/// transient 用 source.ref 指的路径（不写持久层）。
fn plugin_dir_of(p: &InstalledPlugin, paths: &PluginPaths) -> PathBuf {
    if p.entry.source.is_transient() {
        p.entry
            .source
            .reference
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| paths.plugins_dir.join(&p.entry.id))
    } else {
        paths.plugins_dir.join(&p.entry.id)
    }
}

/// 目录体积（视图资产展示用；不跟随符号链接）。
fn dir_bytes(dir: &Path, depth: usize) -> u64 {
    if depth > 16 {
        return 0;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0;
    for entry in read.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            total += dir_bytes(&path, depth + 1);
        } else {
            total += meta.len();
        }
    }
    total
}

/// 视图资产根：第一个视图 entry 的父目录（缺省 `view/`，ADR-0001 目录约定）。
fn view_root(p: &InstalledPlugin) -> Option<PathBuf> {
    let entry = p.manifest.contributes.views.first()?.entry.clone();
    let parent = Path::new(&entry).parent().unwrap_or(Path::new(""));
    Some(p.plugin_dir.join(parent))
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_root_uses_view_subtree() {
        let raw = r#"{"schemaVersion":1,"id":"vp","name":"V","version":"0.1.0","hub":{"apiVersion":"1"},
          "contributes":{"views":[{"id":"a","title":"A","entry":"view/index.html"}]}}"#;
        let m = manifest::parse(raw).unwrap().manifest;
        let p = InstalledPlugin {
            entry: PluginEntry {
                id: "vp".into(),
                version: "0.1.0".into(),
                content_hash: "x".into(),
                permissions_hash: "y".into(),
                source: PluginSource::drop_in(),
                installed_at: now_rfc3339(),
                enabled: true,
                state: PluginState::Ready,
                last_fault: None,
            },
            manifest: m,
            plugin_dir: PathBuf::from("/tmp/plugin-v"),
            records: vec![],
        };
        assert_eq!(view_root(&p), Some(PathBuf::from("/tmp/plugin-v/view")));
    }

    #[test]
    fn storage_key_rejects_escape() {
        assert!(storage_key(&json!({"key":"state.json"})).is_ok());
        assert!(storage_key(&json!({"key":"sub/state.json"})).is_ok());
        assert!(storage_key(&json!({"key":"../escape.json"})).is_err());
        assert!(storage_key(&json!({"key":"/etc/passwd"})).is_err());
    }

    #[test]
    fn mime_mapping() {
        assert_eq!(
            mime_for(Path::new("a/index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_for(Path::new("a/app.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_for(Path::new("a/x.bin")), "application/octet-stream");
    }
}
