//! 插件体系门面（ADR-0002 第 1/3 条）。
//!
//! `PluginManager` 是 `AppState` 上唯一的插件入口：装配 registry / scanner /
//! supervisor / rpc / credentials / tickets，对外只暴露领域方法。所有读取走注册表
//! 快照（不缓存贡献物清单），安装 / 停用 / 启用 / 卸载一律立即生效——
//! 路由树固定（catch-all 派发），永不因插件变化重建。

pub mod api_surface;
pub mod approvals;
pub mod audit;
pub mod credentials;
pub mod hooks;
pub mod install;
pub mod manifest;
pub mod mcp_config;
pub mod mcp_proxy;
pub mod permissions;
pub mod theme_tokens;
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
    /// 钩子通知队列上限（B4 ADR-0014；`[plugins] hook_queue_cap`，默认 64）
    pub hook_queue_cap: usize,
    /// 审计产物每插件保留份数（B4 治理；`[plugins] audit_keep`，默认 10）
    pub audit_keep: usize,
    /// 索引市场源清单文件（B4 ADR-0008 §3）
    pub market_file: PathBuf,
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
            hook_queue_cap: cfg.plugin_hook_queue_cap,
            audit_keep: cfg.plugin_audit_keep,
            market_file: cfg.plugin_market_file.clone(),
        }
    }
}

/// 索引市场缓存（B4：300s TTL，避免每次 GET 重克隆索引仓库）
#[derive(Clone)]
struct MarketCache {
    at: Instant,
    sources: Vec<install::MarketSourceView>,
    entries: Vec<install::MarketEntry>,
}

const MARKET_CACHE_TTL: Duration = Duration::from_secs(300);

/// 空闲翻转到滑确认状态机（B4 方案 1 收口，主会话 2026-10-24 裁决）。
///
/// 设计动因（收敛分析，tester-report-4）：探针读「空」有两义——真实空闲，或
/// **单次读降级**（claude CLI 高并发读时 `list` 内部静默降级 Ok(空集)，无 Err、
/// 与 /api/agents 的并发调用呈输家分歧；dsh 401 同族降级 Ok(空)）。把一次空
/// 观测当空闲会误发 onIdle；忙信号则只有真实工作会话才会产生（降级只造假空、
/// 不造假忙）。
///
/// 因此：忙 → 立即武装；连续 `CONFIRM_ZEROS` 次空观测（5s × 3 ≈ 15s 确认窗，
/// 约两倍于单次降级窗口的估计宽度）才判定忙→闲翻转并发射；任何忙观测重置
/// 去抖计数（忙可信）。未武装（启动即全空）不发射——无忙基线无翻转。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleFlipDetector {
    /// 曾观测到忙（now > 0）
    armed: bool,
    /// 连续空观测计数
    zero_streak: u32,
}

impl IdleFlipDetector {
    /// 判闲需连续空观测次数（约 15s，5s 间隔）
    pub const CONFIRM_ZEROS: u32 = 3;

    pub fn new() -> Self {
        Self {
            armed: false,
            zero_streak: 0,
        }
    }

    /// 喂一次采样。返回 true = 判定忙→闲翻转（调用方应发射 onIdle）。
    pub fn feed(&mut self, now: usize) -> bool {
        if now > 0 {
            self.armed = true;
            self.zero_streak = 0;
            return false;
        }
        if !self.armed {
            return false; // 从未观测到忙：启动即全空 = 无翻转基线
        }
        self.zero_streak += 1;
        if self.zero_streak >= Self::CONFIRM_ZEROS {
            self.armed = false;
            self.zero_streak = 0;
            return true;
        }
        false
    }
}

impl Default for IdleFlipDetector {
    fn default() -> Self {
        Self::new()
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
    /// 渲染器声明（B3，ADR-0012）：宿主转译进围栏/文件渲染注册表的数据源
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renderers: Vec<RendererEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_tokens: Option<ThemeTokensView>,
    pub file_decorations: Vec<FileDecorationView>,
    pub services: Vec<ServiceView>,
    /// 生命周期钩子订阅与投递统计（B4，ADR-0014：丢弃计数管理页可见）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_stats: Option<hooks::HookStats>,
    pub backend: BackendInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fault: Option<FaultRecord>,
    pub asset_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererEntry {
    pub id: String,
    /// 小写化的 fence 命中（空数组 = 无 fence）
    pub match_fence: Vec<String>,
    /// 裸扩展名（小写，无点）
    pub match_ext: Vec<String>,
    pub engine: String,
    /// 服务配置名（plantuml-server 类；其余引擎为 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// plugin-iframe 档的入口路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeTokensView {
    pub tokens: std::collections::BTreeMap<String, ThemeTokenEntryView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeTokenEntryView {
    pub light: String,
    pub dark: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDecorationView {
    pub id: String,
    pub matchExt: Vec<String>,
    pub icon: String,
    /// 成对值 {light,dark} 或 引用 "$token:..."（字符串形态，非对象）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
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

/// 空闲探针提供方（B4 onIdle 埋点）：返回当前运行中（Working/NeedsInput）会话数。
pub type IdleProbeProvider = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = usize> + Send>> + Send + Sync,
>;

/// 会话清单提供方（B4 `hub.sessions.list` 只读投影）：drivers 全量聚合。
pub type SessionsProvider = Arc<
    dyn Fn()
        -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Vec<crate::models::AgentSummary>> + Send>,
        > + Send
        + Sync,
>;

/// 单会话最近消息提供方（B4 `hub.sessions.messages` 只读投影）：sessionId 解析 +
/// 既有分页读取复用（不碰消息数据层）。
pub type MessagesProvider = Arc<
    dyn Fn(
            &str,
            usize,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Vec<crate::models::ChatMessage>, String>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

pub struct PluginManager {
    pub paths: PluginPaths,
    pub registry: Arc<Registry>,
    pub supervisor: Arc<Supervisor>,
    pub credentials: Arc<CredentialsStore>,
    pub tickets: Arc<view_ticket::TicketStore>,
    /// 高危审批单表（B2 批3）：mcp-proxy 经 loopback 落单 + UI 决策，
    /// Approval/Rejection 决定入插件故障记录域
    pub approvals: Arc<approvals::ApprovalStore>,
    /// 生命周期钩子·观察档总线（B4，ADR-0014）：dispatch 经门面/本句柄均可
    pub hooks: Arc<hooks::HooksBus>,
    pub events_tx: broadcast::Sender<HubEvent>,
    /// 装配后回填的 agent cwd 提供方（`hub.projects.effective` 聚合用；
    /// 构造期为 None——drivers 在 AppState 装配序列里晚于插件门面建成）
    agent_cwds: StdMutex<Option<AgentCwdProvider>>,
    /// 空闲探针提供方（B4 onIdle：返回当前运行中会话数；装配序列回填）
    idle_probe: StdMutex<Option<IdleProbeProvider>>,
    /// 空闲轮询任务已启动（回填即启动，防重入双轮）
    idle_poll_started: std::sync::atomic::AtomicBool,
    /// 会话只读投影提供方（B4 hub.sessions.*；装配序列回填，未装配拒答）
    sessions: StdMutex<Option<SessionsProvider>>,
    messages: StdMutex<Option<MessagesProvider>>,
    /// 工程便签存储路径（B3 `hub.notes.read/write`；与 /api/notes 同一文件）
    notes_file: StdMutex<Option<PathBuf>>,
    /// 定时任务管理器回填（B3 `hub.tasks.list/toggle`；与 /api/tasks 同一实例）
    tasks: StdMutex<Option<Arc<crate::tasks::TaskManager>>>,
    scan_cache: RwLock<scanner::ScanOutcome>,
    market_cache: RwLock<Option<MarketCache>>,
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
        let bus = hooks::HooksBus::new(paths.hook_queue_cap);
        let me = Arc::new(Self {
            registry,
            supervisor: Arc::new(Supervisor::new()),
            credentials,
            tickets: Arc::new(view_ticket::TicketStore::default()),
            approvals: Arc::new(approvals::ApprovalStore::new()),
            hooks: bus.clone(),
            events_tx,
            paths,
            agent_cwds: StdMutex::new(None),
            idle_probe: StdMutex::new(None),
            idle_poll_started: std::sync::atomic::AtomicBool::new(false),
            sessions: StdMutex::new(None),
            messages: StdMutex::new(None),
            notes_file: StdMutex::new(None),
            tasks: StdMutex::new(None),
            scan_cache: RwLock::new(scanner::ScanOutcome::default()),
            market_cache: RwLock::new(None),
            locks: StdMutex::new(HashMap::new()),
            retries: StdMutex::new(HashMap::new()),
            asset_cache: RwLock::new(HashMap::new()),
        });
        bus.bind(&me);
        // 进程级门面接线（tester-report-1 FAIL-1 回修）：drivers 埋点经
        // hooks::global_dispatch 发射，漏挂 = 全部埋点静默短路——装配点收敛在
        // PluginManager::new 单点（hub/node 两形态与测试共用），不散落启动序列
        hooks::set_global(bus);
        me
    }

    /// 空闲探针回填（B4 onIdle 埋点；AppState 装配序列调用）。
    /// **回填即启动轮询任务**（tester-report-2 修复：原 spawn 放在 startup()
    /// 读后回填字段，装配序一旦颠倒探针任务静默不启动——启动权归属回填侧，
    /// 与探测器到位零窗口）。单管理器只启一轮（防重入）。
    pub fn set_idle_probe(&self, probe: IdleProbeProvider) {
        self.set_idle_probe_with_interval(probe, Duration::from_secs(5));
    }

    pub fn set_idle_probe_with_interval(&self, probe: IdleProbeProvider, interval: Duration) {
        self.idle_probe
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(probe.clone());
        if self
            .idle_poll_started
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let bus = self.hooks.clone();
        tokio::spawn(async move {
            // 忙→闲翻转点（统一归一逻辑处，单点）：去抖状态机——单次空观测
            // （读降级形态）不翻转（tester-report-4 收敛分析 + 主会话方案 1 裁决）
            let mut detector = IdleFlipDetector::new();
            let mut prev: usize = 0;
            loop {
                let now = probe().await;
                if now != prev {
                    // 采样变化留痕（tester-report-3 建议的现场自证；翻转可观测）
                    tracing::debug!(from = prev, to = now, "onIdle 探针计数变化");
                }
                if detector.feed(now) {
                    tracing::info!("onIdle：忙→闲确认（连续 {} 次空观测）", IdleFlipDetector::CONFIRM_ZEROS);
                    bus.dispatch("onIdle", json!({ "idle": true, "runningCount": 0 }));
                }
                prev = now;
                tokio::time::sleep(interval).await;
            }
        });
    }

    /// 会话清单只读投影回填（B4 hub.sessions.list；AppState 装配序列调用）。
    pub fn set_sessions_provider(&self, provider: SessionsProvider) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(provider);
    }

    /// 单会话消息只读投影回填（B4 hub.sessions.messages）。
    pub fn set_messages_provider(&self, provider: MessagesProvider) {
        self.messages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(provider);
    }

    /// 钩子发射（观察档；本句柄与全局门面等价，进程内优先走门面）。
    pub fn dispatch_hook(&self, hook: &str, params: Value) {
        self.hooks.dispatch(hook, params);
    }

    /// 装配接线（AppState::from_config 末段调用）：drivers 全部注册完成后回填
    /// agent cwd 提供方，供 `hub.projects.effective` 聚合。
    pub fn set_agent_cwd_provider(&self, provider: AgentCwdProvider) {
        let mut guard = self.agent_cwds.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(provider);
    }

    /// 回填便签文件路径（B3；AppState 装配序列调用）
    pub fn set_notes_file(&self, path: PathBuf) {
        self.notes_file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(path);
    }

    /// 回填定时任务管理器（B3；AppState 装配序列调用）
    pub fn set_tasks_handle(&self, tm: Arc<crate::tasks::TaskManager>) {
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(tm);
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
        let notes_file = self
            .notes_file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let messages = self
            .messages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Arc::new(HubRpcHandler {
            registry: self.registry.clone(),
            credentials: self.credentials.clone(),
            data_dir: self.paths.data_dir.clone(),
            projects_file: self.paths.projects_file.clone(),
            agent_cwds,
            notes_file,
            tasks,
            sessions,
            messages,
            approvals: self.approvals.clone(),
            events_tx: self.events_tx.clone(),
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

        // B4 onJobsChanged（ADR-0014 §12.5 矩阵：HubEvent 单点转发）——
        // 订阅既有广播通道，JobsChanged → onJobsChanged，drivers 零改动
        let bus = self.hooks.clone();
        let mut rx = self.events_tx.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(HubEvent::JobsChanged { id }) => {
                        bus.dispatch("onJobsChanged", json!({ "id": id }));
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "钩子 JobsChanged 订阅落后（事件已丢）");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
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
            renderers: m
                .contributes
                .renderers
                .iter()
                .map(|r| RendererEntry {
                    id: r.id.clone(),
                    match_fence: r
                        .match_rule
                        .fence
                        .iter()
                        .map(|f| f.to_ascii_lowercase())
                        .collect(),
                    match_ext: r
                        .match_rule
                        .ext
                        .iter()
                        .map(|e| e.to_ascii_lowercase())
                        .collect(),
                    engine: r.engine.clone(),
                    service: r
                        .config
                        .as_ref()
                        .and_then(|c| c.get("service"))
                        .and_then(|s| s.as_str())
                        .map(str::to_string),
                    entry: r.entry.clone(),
                })
                .collect(),
            theme_tokens: m.contributes.theme_tokens.as_ref().map(|tt| ThemeTokensView {
                tokens: tt
                    .tokens
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            ThemeTokenEntryView {
                                light: v.light.clone(),
                                dark: v.dark.clone(),
                            },
                        )
                    })
                    .collect(),
            }),
            file_decorations: m
                .contributes
                .file_decorations
                .iter()
                .map(|d| FileDecorationView {
                    id: d.id.clone(),
                    matchExt: d.match_rule.ext.iter().map(|e| e.to_ascii_lowercase().trim_start_matches('.').to_string()).collect(),
                    icon: d.icon.clone(),
                    color: d.color.clone(),
                })
                .collect(),
            services: m
                .contributes
                .services
                .iter()
                .map(|s| ServiceView {
                    id: s.id.clone(),
                    label: s.label.clone(),
                    placeholder: s.placeholder.clone(),
                    default: s.default.clone(),
                })
                .collect(),
            hook_stats: if m.contributes.hooks.is_empty() {
                None
            } else {
                self.hooks.stats_for(&p.entry.id, m.contributes.hooks.clone())
            },
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
        // B4 BR-13：更新候选进待确认流——新版副本入库（.versions）、旧版内容
        // 恢复进插件目录（保持旧版可用），注册表登记 pending_update
        for item in outcome.items.iter().filter(|i| i.status == "update") {
            if let Err(e) = self.stage_pending_update(item) {
                tracing::warn!(plugin = %item.id, error = %e, "更新候选入库失败（保持未确认态）");
            }
        }
        // B4 治理（需求功能点 6）：按保留策略清理审计产物（每插件保留 N 份）
        match audit::prune_artifacts(&self.paths.audit_dir, self.paths.audit_keep) {
            Ok(removed) if removed > 0 => {
                tracing::info!(removed, keep = self.paths.audit_keep, "审计产物按保留策略清理");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "审计产物清理失败"),
        }
        // 已入库待确认项（目录已恢复旧版，不再出现在扫描候选里）从持久态
        // 重新投影进 discovered 面（审计源 = .versions 副本）
        let mut items = outcome.items;
        for p in self.registry.list() {
            let Some(pending) = p.entry.pending_update.clone() else {
                continue;
            };
            if items
                .iter()
                .any(|i| i.id == p.entry.id && i.status == "update")
            {
                continue;
            }
            let copy = version_dir(&self.paths, &p.entry.id, &pending.content_hash);
            if !copy.join("plugin.json").is_file() {
                continue;
            }
            let report = audit::audit_dir(&copy, &self.credentials);
            let _ = audit::persist(&report, &self.paths.audit_dir);
            items.push(scanner::DiscoveredItem {
                id: p.entry.id.clone(),
                dir_name: p.entry.id.clone(),
                path: copy.to_string_lossy().into_owned(),
                source_kind: p.entry.source.kind.clone(),
                status: "update".into(),
                installed_content_hash: Some(p.entry.content_hash.clone()),
                update_diff: Some(scanner::compute_update_diff(&p, &report)),
                report,
            });
        }
        items.sort_by(|a, b| {
            let rank = |s: &str| match s {
                "audit_failed" => 0,
                "update" => 1,
                _ => 2,
            };
            rank(&a.status)
                .cmp(&rank(&b.status))
                .then_with(|| a.id.cmp(&b.id))
        });
        let discovered = items.len();
        let last = outcome.last_scan_at.clone();
        if let Ok(mut c) = self.scan_cache.write() {
            *c = scanner::ScanOutcome {
                items,
                last_scan_at: last.clone(),
            };
        }
        let _ = self
            .events_tx
            .send(HubEvent::PluginsChanged { id: "scan".into() });
        ScanResult {
            last_scan_at: last,
            discovered,
        }
    }

    /// 更新候选入库（B4 BR-13）：新版副本 → `.versions/<id>/<hash>/`；旧版副本
    /// 存在时恢复旧版目录内容（保持旧版可用——backend 懒启动跑的代码与注册表
    /// 快照一致）；登记 pending_update。已在待确认态（同 hash）则幂等跳过。
    fn stage_pending_update(&self, item: &scanner::DiscoveredItem) -> Result<(), String> {
        let Some(installed) = self.registry.get(&item.id) else {
            return Err("已装条目消失".into());
        };
        if installed
            .entry
            .pending_update
            .as_ref()
            .is_some_and(|p| p.content_hash == item.report.content_hash)
        {
            return Ok(()); // 同版本已在待确认态
        }
        if item.source_kind == "transient" {
            return Ok(()); // transient 不参与更新比对（ADR-0001 第 4 条）
        }
        let new_copy = version_dir(&self.paths, &item.id, &item.report.content_hash);
        copy_dir_recursive(Path::new(&item.path), &new_copy)
            .map_err(|e| format!("新版副本入库失败：{e}"))?;
        let old_copy = version_dir(&self.paths, &item.id, &installed.entry.content_hash);
        let old_available = old_copy.join("plugin.json").is_file();
        if old_available {
            // 旧版恢复：目录内容回滚到已装版本（新副本已在 .versions，不丢失）
            let live = self.paths.plugins_dir.join(&item.id);
            std::fs::remove_dir_all(&live).map_err(|e| format!("目录重置失败：{e}"))?;
            copy_dir_recursive(&old_copy, &live).map_err(|e| format!("旧版恢复失败：{e}"))?;
        }
        let _ = self.registry.set_pending_update(
            &item.id,
            Some(registry::PendingUpdate {
                content_hash: item.report.content_hash.clone(),
                version: item.report.plugin_version.clone(),
                discovered_at: registry::now_rfc3339(),
                old_version_available: old_available,
            }),
        );
        prune_versions(&self.paths, &item.id, VERSIONS_KEEP);
        tracing::info!(
            plugin = %item.id,
            new_hash = %item.report.content_hash,
            old_restored = old_available,
            "更新候选已入库待确认（BR-13）"
        );
        Ok(())
    }

    /// 应用更新（B4 BR-13 确认流）：`.versions` 副本重新审计（TOCTOU 再校验）→
    /// 停 backend → 目录内容替换 → 注册表条目替换 → 留痕 + 广播。
    pub async fn apply_update(
        self: &Arc<Self>,
        id: &str,
        content_hash: &str,
    ) -> Result<PluginView, String> {
        let lock = self.plugin_lock(id);
        let _guard = lock.lock().await;
        let installed = self
            .registry
            .get(id)
            .ok_or_else(|| format!("插件 {id} 不在注册表中"))?;
        let pending = installed
            .entry
            .pending_update
            .clone()
            .ok_or_else(|| format!("插件 {id} 无待确认更新"))?;
        if pending.content_hash != content_hash {
            return Err(format!(
                "待确认更新 hash 不一致（登记 {}/请求 {content_hash}），请刷新后重试",
                pending.content_hash
            ));
        }
        let copy = version_dir(&self.paths, id, content_hash);
        if !copy.join("plugin.json").is_file() {
            return Err(format!("新版本副本缺失：{}", copy.display()));
        }
        // 重新审计 + hash 再校验（防 TOCTOU：入库后副本被改）
        let report = audit::audit_dir(&copy, &self.credentials);
        if !report.errors.is_empty() {
            let first = &report.errors[0];
            return Err(format!(
                "更新副本审计未通过：{}（字段 {} / 规则 {}）",
                first.message, first.field, first.rule
            ));
        }
        if report.content_hash != content_hash {
            return Err("副本内容在入库后已变化（hash 不一致），已拒绝应用".into());
        }
        // 内容替换：运行中的 backend 先停（目录内容与进程镜像不一致期间不可调用）
        self.supervisor.stop(id).await;
        self.tickets.revoke_plugin(id);
        let live = self.paths.plugins_dir.join(id);
        std::fs::remove_dir_all(&live).map_err(|e| format!("目录替换失败：{e}"))?;
        copy_dir_recursive(&copy, &live).map_err(|e| format!("目录替换失败：{e}"))?;
        // 注册表条目替换：version/hash/权限面/pending 清除 + manifest 快照刷新
        let parsed = manifest::parse(
            &std::fs::read_to_string(live.join("plugin.json"))
                .map_err(|e| format!("读取 plugin.json 失败：{e}"))?,
        )
        .map_err(|errs| {
            format!(
                "manifest 校验失败：{}",
                errs.first().map(|e| e.message.clone()).unwrap_or_default()
            )
        })?;
        self.registry
            .apply_update(
                id,
                parsed.manifest.version.clone(),
                content_hash.to_string(),
                registry::permissions_hash(&parsed.manifest.permissions),
                parsed.manifest,
                live,
            )
            .map_err(|e| format!("注册表更新失败：{e}"))?;
        let _ = self.registry.push_record(
            id,
            FaultRecord {
                at: now_rfc3339(),
                kind: FaultKind::Update,
                // MINOR① 回修：from = 更新前已装版本（pending.version 是新版）
                message: format!(
                    "更新已应用：{} → {}（hash {}）",
                    installed.entry.version,
                    report.plugin_version,
                    short_hash(content_hash)
                ),
            },
        );
        self.scan_now();
        self.broadcast(id);
        let p = self.registry.get(id).expect("entry exists");
        Ok(self.view_of(&p))
    }

    /// 保持旧版（B4 BR-13 拒绝分支）：清除待确认态 + 删除新版副本 + 留痕。
    /// 旧版自始至终未被替换（目录恢复语义），无需回滚动作。
    pub async fn keep_old_version(
        self: &Arc<Self>,
        id: &str,
        content_hash: &str,
    ) -> Result<PluginView, String> {
        let lock = self.plugin_lock(id);
        let _guard = lock.lock().await;
        let installed = self
            .registry
            .get(id)
            .ok_or_else(|| format!("插件 {id} 不在注册表中"))?;
        let pending = installed
            .entry
            .pending_update
            .clone()
            .ok_or_else(|| format!("插件 {id} 无待确认更新"))?;
        if pending.content_hash != content_hash {
            return Err(format!(
                "待确认更新 hash 不一致（登记 {}/请求 {content_hash}）",
                pending.content_hash
            ));
        }
        let _ = self.registry.set_pending_update(id, None);
        let copy = version_dir(&self.paths, id, content_hash);
        if copy.is_dir() {
            let _ = std::fs::remove_dir_all(&copy);
        }
        let _ = self.registry.push_record(
            id,
            FaultRecord {
                at: now_rfc3339(),
                kind: FaultKind::Update,
                message: format!(
                    "更新已拒绝，保持旧版 {}（拒绝 {}，hash {}）",
                    installed.entry.version,
                    pending.version,
                    short_hash(content_hash)
                ),
            },
        );
        self.scan_now();
        self.broadcast(id);
        let p = self.registry.get(id).expect("entry exists");
        Ok(self.view_of(&p))
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

        // 已装且同 hash：幂等无操作；hash 变化 = 更新候选（BR-13 确认流走
        // /update/apply 端点——扫描器已把候选入待确认区，此处拒绝静默覆盖）
        if let Some(existing) = self.registry.get(id) {
            if existing.entry.content_hash == content_hash {
                return Ok((self.view_of(&existing), true));
            }
            return Err(format!(
                "插件 {id} 内容已变化（已装 {} / 新目录 {content_hash}）——请走更新确认流（管理页「更新待确认」卡片，BR-13）",
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
            pending_update: None,
        };
        let (installed, created) = self
            .registry
            .insert_installed(entry, manifest, dir)
            .map_err(|e| format!("注册表写入失败：{e}"))?;
        // 安装即解除清理登记（FAIL-2 回修配套：同 id 新装恢复常规发现面）
        self.registry.undismiss_discovery(id);
        if created {
            // 装载 = 贡献物登记（backend 懒启动）：解析无误即就绪
            if let Err(e) = self.registry.apply(id, Transition::EnableOk) {
                tracing::warn!(plugin = %id, error = %e, "安装后启用迁移失败");
                self.fail_load(id, "load-failed", e);
            }
            let _ = audit::persist(&item.report, &self.paths.audit_dir);
            // B4：安装即快照版本副本（未来更新的「保持旧版」恢复源）
            if item.source_kind != "transient" {
                let snapshot = version_dir(&self.paths, id, &item.report.content_hash);
                if let Err(e) = copy_dir_recursive(Path::new(&item.path), &snapshot) {
                    tracing::warn!(plugin = %id, error = %e, "版本快照失败（更新恢复源缺失）");
                } else {
                    prune_versions(&self.paths, id, VERSIONS_KEEP);
                }
            }
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
        self.hooks.forget(id);
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
        self.hooks.forget(id);
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
            // B4 治理：purge_data 同包 purge_audit（卸载可选随卸载清理，需求功能点 6）
            let _ = audit::purge_plugin_artifacts(&self.paths.audit_dir, id);
            let versions = versions_root(&self.paths).join(id);
            if versions.is_dir() {
                let _ = std::fs::remove_dir_all(&versions);
            }
            // FAIL-2 回修：drop-in 目录按 ADR-0002 保留（dirKept），但登记清理记录
            // 抑制周期扫描对同内容目录的重发现/重审计（产物不再重生）；用户放新版
            // （hash 变化）即恢复正常发现面
            let live = self.paths.plugins_dir.join(id);
            if live.join("plugin.json").is_file() {
                let hash = audit::content_hash(&live);
                self.registry.dismiss_discovery(id, &hash);
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
        let rel = rel.trim_start_matches('/');
        let rel = if rel.is_empty() { "index.html" } else { rel };
        // 渲染器资产（B3 尾批 F-14）：entry 语义统一为**相对插件目录**（与审计期
        // 校验同口径）；视图资产仍相对 view_root（views[0].entry 父目录，B1 语义）。
        // 纯渲染器插件（无 views）此前 view_root=None 恒 404——渲染器命中时不再依赖它。
        let (root, rel) = match renderer_asset_root(&p, rel) {
            Some((rdir, rrel)) => (rdir, rrel),
            None => {
                let vroot = view_root(&p).ok_or(ViewFileError::NotFound)?;
                (vroot, rel.to_string())
            }
        };
        let candidate = root.join(&rel);
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
            // B2 审批流已交付（会话注入 / mcp-proxy 双路对称化，M2 清账）——
            // 此处落单阻塞等待与 HubRpcHandler 同路径（批准继续执行，拒绝不执行）
            let digest = {
                let scrubbed = self.credentials.scrub(&params.to_string());
                let mut s: String = scrubbed.chars().take(200).collect();
                if s.is_empty() {
                    s = "{}".into();
                }
                s
            };
            let sess = params.get("sessionId").and_then(serde_json::Value::as_str);
            let tool_full = format!("hubplug_{id}_{tool_id}");
            let issued = self
                .approvals
                .issue(id, &tool_full, &digest, sess, &now_rfc3339());
            let _ = self.events_tx.send(HubEvent::PluginApprovalRequested {
                id: issued.id.clone(),
                plugin_id: id.to_string(),
                tool: tool_full.clone(),
                args_digest: digest.clone(),
            });
            let decision = match tokio::time::timeout(approvals::APPROVAL_WAIT, issued.rx).await {
                Ok(Ok(d)) => d,
                Ok(Err(_)) | Err(_) => {
                    return Err(RpcError::Busy("高危确认等待超时（120s），可重试".into()));
                }
            };
            // O-1 双留痕收敛（B3 review-1 M13）：决策留痕唯一落点 = decide_approval；
            // 本执行点不再重复落 Approval/Rejection（同一次决策此前落两条）
            if decision == approvals::Decision::Reject {
                return Err(RpcError::Violation(format!(
                    "高危工具 {tool_full} 被用户拒绝，未执行"
                )));
            }
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

    /// 高危审批落单（B2 批3）：mcp-proxy 经 loopback POST 调用。落单即广播
    /// `plugin_approval_requested`（设计 §3：/ws/events 推送驱动 UI 弹确认）。
    /// 返回 (审批单 id, 决策接收端)——mcp-proxy await 之（120s 上限在其侧）。
    pub fn issue_approval(
        &self,
        plugin_id: &str,
        tool: &str,
        args_digest: &str,
        session_id: Option<&str>,
    ) -> approvals::Issued {
        let issued = self
            .approvals
            .issue(plugin_id, tool, args_digest, session_id, &now_rfc3339());
        let _ = self.events_tx.send(HubEvent::PluginApprovalRequested {
            id: issued.id.clone(),
            plugin_id: plugin_id.to_string(),
            tool: tool.to_string(),
            args_digest: args_digest.to_string(),
        });
        issued
    }

    // ===== 分发面（B4，ADR-0008 §2/§3）=====

    /// git 直装（clone → 校验 → 落盘 → 扫描进确认面）。
    pub async fn install_git(
        self: &Arc<Self>,
        req: &install::GitInstallRequest,
    ) -> Result<install::GitInstallOutcome, String> {
        install::git_install(self, req).await
    }

    /// 索引源添加（用户显式；形态校验前置——错误源不进清单）。
    pub fn market_add_source(&self, source: &str) -> Result<(), String> {
        install::valid_git_source(source)?;
        let mut store = install::MarketSources::load(&self.paths.market_file);
        if store.sources.iter().any(|s| s == source) {
            return Ok(()); // 幂等
        }
        store.sources.push(source.to_string());
        store.save(&self.paths.market_file)
    }

    /// 索引源移除。
    pub fn market_remove_source(&self, source: &str) -> Result<usize, String> {
        let mut store = install::MarketSources::load(&self.paths.market_file);
        let before = store.sources.len();
        store.sources.retain(|s| s != source);
        let removed = before - store.sources.len();
        store.save(&self.paths.market_file)?;
        Ok(removed)
    }

    /// 索引源清单 + 条目聚合（TTL 缓存 300s；force = 绕缓存重拉）。
    pub async fn market_list(
        self: &Arc<Self>,
        force: bool,
    ) -> (Vec<install::MarketSourceView>, Vec<install::MarketEntry>) {
        if !force {
            if let Ok(c) = self.market_cache.read() {
                if let Some(c) = c.as_ref() {
                    if c.at.elapsed() < MARKET_CACHE_TTL {
                        return (c.sources.clone(), c.entries.clone());
                    }
                }
            }
        }
        let store = install::MarketSources::load(&self.paths.market_file);
        let mut views = Vec::new();
        let mut entries = Vec::new();
        for source in &store.sources {
            match install::fetch_market_source(self, source).await {
                Ok((name, list)) => {
                    entries.extend(list.iter().cloned());
                    views.push(install::MarketSourceView {
                        source: source.clone(),
                        name,
                        plugin_count: list.len(),
                        error: None,
                    });
                }
                Err(e) => views.push(install::MarketSourceView {
                    source: source.clone(),
                    name: None,
                    plugin_count: 0,
                    error: Some(self.credentials.scrub(&e)),
                }),
            }
        }
        if let Ok(mut c) = self.market_cache.write() {
            *c = Some(MarketCache {
                at: Instant::now(),
                sources: views.clone(),
                entries: entries.clone(),
            });
        }
        (views, entries)
    }

    /// 从索引触发安装：定位条目 → git 直装（进既有确认面）。
    pub async fn market_install(
        self: &Arc<Self>,
        source: &str,
        id: &str,
    ) -> Result<install::GitInstallOutcome, String> {
        let (_, entries) = self.market_list(false).await;
        let entry = entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("索引中无插件 {id:?}（源 {source:?} 未收录或未刷新）"))?;
        self.install_git(&entry.into()).await
    }

    /// 审计产物清点（管理页可见性）：文件数与体积。
    pub fn audit_artifact_stats(&self) -> (usize, u64) {
        audit::artifact_stats(&self.paths.audit_dir)
    }

    /// 手动审计产物清理（保留策略同扫描期；返回清理数）。
    pub fn audit_prune(&self) -> Result<usize, String> {
        audit::prune_artifacts(&self.paths.audit_dir, self.paths.audit_keep)
            .map_err(|e| e.to_string())
    }

    /// 审批决策（UI 端点调用）：留痕 Approval/Rejection 入插件故障记录域 +
    /// 通知等待方。插件已卸载时决定照常生效（留痕目标已消失则跳过落痕）。
    pub fn decide_approval(
        &self,
        id: &str,
        decision: approvals::Decision,
    ) -> Result<approvals::ApprovalRequest, approvals::DecideError> {
        let req = self.approvals.decide(id, decision)?;
        let (kind, verb) = match decision {
            approvals::Decision::Approve => (FaultKind::Approval, "批准"),
            approvals::Decision::Reject => (FaultKind::Rejection, "拒绝"),
        };
        if self.registry.get(&req.plugin_id).is_some() {
            let _ = self.registry.push_record(
                &req.plugin_id,
                FaultRecord {
                    at: now_rfc3339(),
                    kind,
                    message: self.credentials.scrub(&format!(
                        "高危调用 {} · 用户{verb} · {} · 参数 {}",
                        req.tool, req.id, req.args_digest
                    )),
                },
            );
        }
        Ok(req)
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
    /// 工程便签文件（B3 hub.notes.*；None = 未装配，方法拒答）
    notes_file: Option<PathBuf>,
    /// 定时任务管理器（B3 hub.tasks.*；None = 未装配，方法拒答）
    tasks: Option<Arc<crate::tasks::TaskManager>>,
    /// 会话只读投影（B4 hub.sessions.list；None = 未装配，方法拒答）
    sessions: Option<SessionsProvider>,
    /// 单会话消息只读投影（B4 hub.sessions.messages；None = 未装配，方法拒答）
    messages: Option<MessagesProvider>,
    agent_cwds: AgentCwdProvider,
    /// 高危审批单表（B2 批3）：`hub.sessions.send` 等高危方法命中
    /// HighRiskNeedsApproval 时落单阻塞等待（设计 §7）
    approvals: Arc<approvals::ApprovalStore>,
    events_tx: broadcast::Sender<HubEvent>,
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
        let approvals = self.approvals.clone();
        let events_tx = self.events_tx.clone();
        let notes_file = self.notes_file.clone();
        let tasks = self.tasks.clone();
        let sessions = self.sessions.clone();
        let messages = self.messages.clone();
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
                match deny.kind {
                    permissions::DenyKind::HighRiskNeedsApproval => {
                        // 批3 高危审批门（ADR-0007 第 3 条 / 设计 §7）：落单阻塞
                        // 等待（120s 超时 -32005 busy 由调用方重试）；决策留痕
                        // Approval/Rejection；拒绝不执行。确认点恒在 hub（BR-15）
                        let digest = {
                            let scrubbed = credentials.scrub(&params.to_string());
                            let mut s: String = scrubbed.chars().take(200).collect();
                            if s.is_empty() {
                                s = "{}".into();
                            }
                            s
                        };
                        let issued =
                            approvals.issue(&plugin_id, &method, &digest, None, &now_rfc3339());
                        let _ = events_tx.send(HubEvent::PluginApprovalRequested {
                            id: issued.id.clone(),
                            plugin_id: plugin_id.clone(),
                            tool: method.clone(),
                            args_digest: digest,
                        });
                        let decision =
                            match tokio::time::timeout(approvals::APPROVAL_WAIT, issued.rx).await {
                                Ok(Ok(d)) => d,
                                Ok(Err(_)) | Err(_) => {
                                    return Err(RpcError::Busy(
                                        "高危确认等待超时（120s），可重试".into(),
                                    ));
                                }
                            };
                        // O-1 双留痕收敛（B3 review-1 M13）：决策留痕唯一落点 =
                        // decide_approval（UI 决策端点）；此处不再重复落 Approval/
                        // Rejection——工具执行点与方法层只在执行结果上有差异时另行留痕
                        if decision == approvals::Decision::Reject {
                            return Err(RpcError::Violation(format!(
                                "高危方法 {method} 被用户拒绝，未执行"
                            )));
                        }
                        // 批准：继续执行该方法（落到下方 match）
                    }
                    other => {
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
                        return Err(match other {
                            permissions::DenyKind::CredentialUnknown
                            | permissions::DenyKind::DataDenied => {
                                RpcError::Violation(deny.message)
                            }
                            _ => RpcError::Unauthorized(deny.message),
                        });
                    }
                }
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
                // —— B3 视图 API 完整化（notes/tasks 数据域；sessions 族单列缺口）——
                "hub.notes.read" => {
                    let path = params
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or_else(|| RpcError::PluginError("缺少 path 参数".into()))?;
                    if path.contains("..") {
                        return Err(RpcError::PluginError("路径不允许包含 ..".into()));
                    }
                    let notes_file = notes_file.clone().ok_or_else(|| {
                        RpcError::PluginError("notes 存储未装配（hub 内部状态）".into())
                    })?;
                    // 与 /api/notes 同一存储（NotesStore；键 = 规范化 cwd，async 同源）
                    let store = crate::notes::NotesStore { file: notes_file };
                    let content = store.get(path).await;
                    Ok(json!({ "content": content }))
                }
                "hub.notes.write" => {
                    let path = params
                        .get("path")
                        .and_then(Value::as_str)
                        .ok_or_else(|| RpcError::PluginError("缺少 path 参数".into()))?;
                    let content = params
                        .get("content")
                        .and_then(Value::as_str)
                        .ok_or_else(|| RpcError::PluginError("缺少 content 参数".into()))?;
                    if path.contains("..") {
                        return Err(RpcError::PluginError("路径不允许包含 ..".into()));
                    }
                    if content.len() > 256 * 1024 {
                        return Err(RpcError::PluginError("便签写入上限 256KB".into()));
                    }
                    let notes_file = notes_file.clone().ok_or_else(|| {
                        RpcError::PluginError("notes 存储未装配（hub 内部状态）".into())
                    })?;
                    let store = crate::notes::NotesStore { file: notes_file };
                    store
                        .set(path, content)
                        .await
                        .map_err(|e| RpcError::PluginError(format!("写入失败：{e}")))?;
                    Ok(json!({ "ok": true }))
                }
                "hub.tasks.list" => {
                    let tm = tasks.clone().ok_or_else(|| {
                        RpcError::PluginError("tasks 未装配（hub 内部状态）".into())
                    })?;
                    let tasks: Vec<Value> = tm
                        .list()
                        .iter()
                        .map(|t| {
                            json!({
                                "id": t.id,
                                "name": t.name,
                                "cron": t.cron,
                                "enabled": t.enabled,
                            })
                        })
                        .collect();
                    Ok(json!({ "tasks": tasks }))
                }
                "hub.tasks.toggle" => {
                    let id = params
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| RpcError::PluginError("缺少 id 参数".into()))?;
                    let tm = tasks.clone().ok_or_else(|| {
                        RpcError::PluginError("tasks 未装配（hub 内部状态）".into())
                    })?;
                    let mut spec = tm.get(id).ok_or_else(|| {
                        RpcError::PluginError(format!("任务 {id} 不存在"))
                    })?;
                    spec.enabled = !spec.enabled;
                    let enabled = spec.enabled;
                    tm.upsert(spec)
                        .map_err(|e| RpcError::PluginError(format!("启停失败：{e}")))?;
                    Ok(json!({ "id": id, "enabled": enabled }))
                }
                "hub.projects.list" => {
                    let projects: Vec<String> = std::fs::read_to_string(&projects_file)
                        .ok()
                        .and_then(|raw| serde_json::from_str(&raw).ok())
                        .unwrap_or_default();
                    Ok(json!({ "projects": projects }))
                }
                // —— B4 会话只读投影（需求功能点 2；hub.sessions.send 永久不做）——
                "hub.sessions.list" => {
                    let provider = sessions.clone().ok_or_else(|| {
                        RpcError::PluginError("sessions 投影未装配（hub 内部状态）".into())
                    })?;
                    let agents = provider().await;
                    let items: Vec<Value> = agents
                        .iter()
                        .map(|a| {
                            json!({
                                "driver": a.driver,
                                "id": a.id,
                                "sessionId": a.session_id,
                                "name": a.name,
                                "cwd": a.cwd,
                                "state": a.group.as_str(),
                                "detail": a.detail,
                                "startedAt": a.started_at,
                            })
                        })
                        .collect();
                    Ok(json!({ "sessions": items }))
                }
                "hub.sessions.messages" => {
                    let session_id = params
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| RpcError::PluginError("缺少 sessionId 参数".into()))?;
                    let limit = params
                        .get("limit")
                        .and_then(Value::as_u64)
                        .unwrap_or(20)
                        .clamp(1, 200) as usize;
                    let provider = messages.clone().ok_or_else(|| {
                        RpcError::PluginError("messages 投影未装配（hub 内部状态）".into())
                    })?;
                    let msgs = provider(session_id, limit).await.map_err(|e| {
                        RpcError::PluginError(credentials.scrub(&e))
                    })?;
                    let items: Vec<Value> = msgs
                        .iter()
                        .map(|m| {
                            json!({
                                "kind": m.kind.as_str(),
                                "text": m.text.as_deref().map(|t| credentials.scrub(t)),
                                "ts": m.ts,
                            })
                        })
                        .collect();
                    Ok(json!({ "sessionId": session_id, "messages": items }))
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

/// 旧版副本根（B4 更新流，ADR-0008 拍板结论 4 的加法目录约定：
/// `plugins/.versions/<id>/<hash>/`；scanner 一层深扫描遇无 plugin.json 目录静默跳过）。
fn versions_root(paths: &PluginPaths) -> PathBuf {
    paths.plugins_dir.join(".versions")
}

/// 每插件保留的版本副本份数（ADR-0008 拍板结论 4 的 B4 细化：保留 3 份，
/// 新副本入库时按 mtime 剪枝）。
const VERSIONS_KEEP: usize = 3;

/// 单版本副本目录：`.versions/<id>/<hash 去 sha256: 前 16 hex>/`。
fn version_dir(paths: &PluginPaths, id: &str, content_hash: &str) -> PathBuf {
    versions_root(paths).join(id).join(short_hash(content_hash))
}

fn short_hash(content_hash: &str) -> String {
    content_hash
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect()
}

/// 目录递归复制（不跟随符号链接——副本内 symlink 跳过，与审计口径一致）。
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let target = dst.join(entry.file_name());
        if meta.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// 版本副本剪枝（按 mtime 留新弃旧；keep=0 关闭）。
fn prune_versions(paths: &PluginPaths, id: &str, keep: usize) {
    if keep == 0 {
        return;
    }
    let dir = versions_root(paths).join(id);
    let Ok(read) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut entries: Vec<(PathBuf, std::time::SystemTime)> = read
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let modified = e.metadata().and_then(|m| m.modified()).ok()?;
            Some((e.path(), modified))
        })
        .collect();
    if entries.len() <= keep {
        return;
    }
    entries.sort_by(|a, b| b.1.cmp(&a.1));
    for (path, _) in entries.into_iter().skip(keep) {
        let _ = std::fs::remove_dir_all(path);
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

/// 渲染器资产根（B3 尾批 F-14）：rel 命中渲染器 entry 或其 entry 目录内的文件
/// 时，资产相对**插件目录**解析（与审计期 resolve_inside 同口径——三层 rel 口径
/// 不一致的统一）；symlink/逃逸防护沿用 view_file 的 canonicalize 检查
/// （root=插件目录，逃逸判定不放宽）。
fn renderer_asset_root(p: &InstalledPlugin, rel: &str) -> Option<(PathBuf, String)> {
    let clean = rel.trim_start_matches('/');
    if clean.is_empty() || clean.contains("..") {
        // M20：含 .. 段不做前缀命中（归一化前缀会放宽逃逸面——render/../backend
        // 形态可达渲染器目录之外的插件根内文件，与「entry 目录内」语义不符；
        // 该形态落入 view_root/插件根 canonicalize 检查的原判定路径）
        return None;
    }
    for r in &p.manifest.contributes.renderers {
        let Some(entry) = &r.entry else { continue };
        let e_parent = Path::new(entry)
            .parent()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        if clean == entry.as_str() {
            return Some((p.plugin_dir.clone(), clean.to_string()));
        }
        if !e_parent.is_empty() && clean.starts_with(&format!("{e_parent}/")) {
            return Some((p.plugin_dir.clone(), clean.to_string()));
        }
    }
    None
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
    fn hook_observer_template_passes_audit() {
        // 模板即样例即测试（ADR-0015）：hook-observer 模板占位替换后必须过
        // 静态审计（零 errors）且贡献物清单含 hook 类
        let template = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/templates/hook-observer");
        assert!(template.join("plugin.json").is_file());
        let tmp = tempfile::tempdir().unwrap();
        copy_dir_recursive(&template, tmp.path()).unwrap();
        let manifest_path = tmp.path().join("plugin.json");
        let raw = std::fs::read_to_string(&manifest_path)
            .unwrap()
            .replace("{{PLUGIN_ID}}", "hook-observer-demo")
            .replace("{{PLUGIN_NAME}}", "Hook Observer Demo")
            .replace("{{AUTHOR}}", "agent-hub");
        std::fs::write(&manifest_path, raw).unwrap();
        let creds = credentials::CredentialsStore::load(
            std::env::temp_dir().join("hub-test-no-such-credentials.json"),
        );
        let report = audit::audit_dir(tmp.path(), &creds);
        assert!(report.installable, "{:?}", report.errors);
        assert!(
            report
                .contributions
                .iter()
                .any(|c| c.kind == "hook" && c.id == "onSessionCreate"),
            "{:?}",
            report.contributions
        );
    }

    #[test]
    fn idle_flip_detector_debounces_read_degradation() {
        // B4 方案 1 语义锁（主会话 2026-10-24 裁决）：
        // 单次空观测（读降级形态）不翻转；连续 CONFIRM_ZEROS 次空才判闲；
        // 忙观测重置去抖；未武装（启动即全空）不发射。
        let mut d = IdleFlipDetector::new();
        // 1) 启动即全空：无忙基线 → 永不翻转
        for _ in 0..(IdleFlipDetector::CONFIRM_ZEROS + 2) {
            assert!(!d.feed(0), "未武装不得翻转");
        }
        // 2) 忙 → 偶发单零（读降级）→ 忙：不翻转
        assert!(!d.feed(1));
        assert!(!d.feed(0)); // 单次降级
        assert!(!d.feed(1)); // 忙重置去抖
        // 3) 忙 → 连续 CONFIRM_ZEROS 次空：第 3 次翻转，之后重置
        assert!(!d.feed(1));
        assert!(!d.feed(0));
        assert!(!d.feed(0), "2/3 次空仍不翻");
        assert!(d.feed(0), "3/3 次空 → 判闲翻转");
        assert!(!d.feed(0), "翻转后重置：需重新武装");
        // 4) 可再武装再翻：忙 → 连续零 → 再翻
        assert!(!d.feed(2));
        assert!(!d.feed(0));
        assert!(!d.feed(0));
        assert!(d.feed(0));
    }

    #[test]
    fn idle_flip_detector_confirms_window_is_reasonable() {
        // 确认窗 = 5s × 3 ≈ 15s（约两倍于单次降级窗口估计宽度）；常量即语义
        assert_eq!(IdleFlipDetector::CONFIRM_ZEROS, 3);
    }

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
                pending_update: None,
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
