//! 插件注册表：状态机（BR-01）+ 贡献物登记 + 持久态读写（ADR-0002 第 1/6 条）。
//!
//! **单一事实源**：一切插件读取路径走本表快照，不另设贡献物缓存（ADR-0002 第 3 条）。
//! 写操作单写者：全部迁移在 [`Registry`] 的写锁内串行执行；持久态原子写（tmp + rename，0600）。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{Manifest, Permissions};

/// 持久态三值（技术设计 §2.2）；UI 五态由持久态派生。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Ready,
    Stopped,
    Fault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultKind {
    LoadFailure,
    Crash,
    Violation,
    Approval,
    Rejection,
    /// 更新确认流留痕（B4 BR-13：应用/保持旧版均留痕，消息体区分）
    Update,
}

/// 待确认更新（B4 BR-13）：新版本已入库（`.versions/<id>/<hash>/`），
/// 确认前注册表继续以旧版服务（保持旧版可用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingUpdate {
    /// 新版本内容 hash（确认时防 TOCTOU 的比对键）
    pub content_hash: String,
    /// 新版本 manifest version（展示用）
    pub version: String,
    /// 发现时间
    pub discovered_at: String,
    /// 发现时旧版副本是否可用（false = B4 前安装的存量插件，无法恢复旧目录内容）
    pub old_version_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FaultRecord {
    pub at: String,
    pub kind: FaultKind,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSource {
    /// `drop-in` | `transient`
    pub kind: String,
    #[serde(rename = "ref")]
    pub reference: Option<String>,
}

impl PluginSource {
    pub fn drop_in() -> Self {
        Self {
            kind: "drop-in".into(),
            reference: None,
        }
    }

    pub fn transient(path: &std::path::Path) -> Self {
        Self {
            kind: "transient".into(),
            reference: Some(path.to_string_lossy().into_owned()),
        }
    }

    pub fn is_transient(&self) -> bool {
        self.kind == "transient"
    }
}

/// 持久化的插件条目（`plugins.json` 的 `plugins[]`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEntry {
    pub id: String,
    pub version: String,
    /// 目录内容 hash（安装事实源）
    pub content_hash: String,
    /// 权限声明面规范化后的 hash（BR-13 扩权判定）
    pub permissions_hash: String,
    pub source: PluginSource,
    pub installed_at: String,
    pub enabled: bool,
    pub state: PluginState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fault: Option<FaultRecord>,
    /// 待确认更新（B4 BR-13；None = 无）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_update: Option<PendingUpdate>,
}

/// 运行时条目 = 持久态 + manifest 快照 + 故障记录（B1 内存态，重启保留 lastFault）。
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub entry: PluginEntry,
    pub manifest: Manifest,
    pub plugin_dir: PathBuf,
    pub records: Vec<FaultRecord>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PersistedRegistry {
    #[serde(default)]
    schema_version: u32,
    #[serde(default)]
    plugins: Vec<PluginEntry>,
    /// 已清理发现记录（B4 tester-report-1 FAIL-2）：卸载 + purge 后抑制同 id+hash
    /// 的重发现/重审计（drop-in 目录按 ADR-0002 第 7 条保留，但其审计产物不应
    /// 被周期扫描无限重生）；目录内容变化（hash 不同）即自动失效——用户放新版
    /// 立即恢复正常发现面。
    #[serde(default)]
    dismissed: Vec<DismissedEntry>,
}

/// 一条已清理的发现记录（id + 目录内容 hash 精确对）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DismissedEntry {
    pub id: String,
    pub content_hash: String,
}

/// 合法迁移（BR-01 迁移表；非法边一律拒绝，防状态漂移）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// 启用且装载成功
    EnableOk,
    /// 启用但装载失败（停用 → 运行故障）
    EnableFailed,
    /// 运行中故障（崩溃 / 装载失败）
    Fault,
    /// 停用
    Disable,
    /// 故障重试成功（整体重登）
    RetryOk,
}

/// 迁移是否合法（纯函数，单测锚点 = BR-01 表）。
pub fn transition_allowed(
    from: (bool, PluginState),
    t: Transition,
) -> Result<(bool, PluginState), String> {
    let (enabled, state) = from;
    let next = match t {
        Transition::EnableOk if !enabled => (true, PluginState::Ready),
        Transition::EnableFailed if !enabled => (true, PluginState::Fault),
        Transition::Fault if enabled && state != PluginState::Fault => {
            (enabled, PluginState::Fault)
        }
        Transition::Disable if enabled => (false, PluginState::Stopped),
        Transition::RetryOk if state == PluginState::Fault => (enabled, PluginState::Ready),
        _ => {
            return Err(format!(
                "非法迁移：enabled={enabled} state={state:?} 不接受 {t:?}"
            ))
        }
    };
    Ok(next)
}

/// 权限声明面规范化 hash（数组排序后序列化再 sha256；BR-13 判定依据）。
pub fn permissions_hash(p: &Permissions) -> String {
    let mut api = p.api.clone();
    let mut data = p.data.clone();
    let mut creds = p.credentials.clone();
    let mut high = p.high_risk.clone();
    api.sort();
    data.sort();
    creds.sort();
    high.sort();
    let canonical = serde_json::json!({
        "api": api,
        "data": data,
        "credentials": creds,
        "highRisk": high,
    });
    let mut h = Sha256::new();
    h.update(canonical.to_string().as_bytes());
    format!("sha256:{}", hex(&h.finalize()))
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub struct Registry {
    file: PathBuf,
    inner: RwLock<BTreeMap<String, InstalledPlugin>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("file", &self.file)
            .field("count", &self.list().len())
            .finish()
    }
}

impl Registry {
    /// 从持久态加载（文件缺失/损坏 = 空表 + warn；不阻塞启动）。
    pub fn open(file: PathBuf) -> Self {
        let persisted = match std::fs::read_to_string(&file) {
            Ok(raw) => serde_json::from_str::<PersistedRegistry>(&raw).unwrap_or_else(|e| {
                tracing::warn!(file = %file.display(), error = %e, "plugins.json 解析失败，按空表处理");
                PersistedRegistry::default()
            }),
            Err(_) => PersistedRegistry::default(),
        };
        let mut map = BTreeMap::new();
        for entry in persisted.plugins {
            // 持久态缺 manifest 快照：启动装载阶段会重新解析目录；此处先占位空 manifest
            // （dir-missing 时由装载阶段标故障）
            let records = entry
                .last_fault
                .clone()
                .map(|f| vec![f])
                .unwrap_or_default();
            map.insert(
                entry.id.clone(),
                InstalledPlugin {
                    entry,
                    manifest: placeholder_manifest(),
                    plugin_dir: PathBuf::new(),
                    records,
                },
            );
        }
        Self {
            file,
            inner: RwLock::new(map),
        }
    }

    pub fn file(&self) -> &std::path::Path {
        &self.file
    }

    pub fn list(&self) -> Vec<InstalledPlugin> {
        self.inner
            .read()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> Option<InstalledPlugin> {
        self.inner.read().ok()?.get(id).cloned()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.inner
            .read()
            .map(|m| m.contains_key(id))
            .unwrap_or(false)
    }

    /// 安装（新条目，或同 contentHash 幂等无操作）。
    /// 返回 (条目, 是否新写入)。
    pub fn insert_installed(
        &self,
        entry: PluginEntry,
        manifest: Manifest,
        plugin_dir: PathBuf,
    ) -> std::io::Result<(InstalledPlugin, bool)> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = guard.get(&entry.id) {
            if existing.entry.content_hash == entry.content_hash {
                // install 幂等：同 hash 重复安装 = 无操作
                return Ok((existing.clone(), false));
            }
        }
        let inst = InstalledPlugin {
            entry,
            manifest,
            plugin_dir,
            records: Vec::new(),
        };
        guard.insert(inst.entry.id.clone(), inst.clone());
        self.save_locked(&guard)?;
        Ok((inst, true))
    }

    /// 注册表条目替换（装载成功后刷新 manifest 快照 / 目录）。
    pub fn update_snapshot(&self, id: &str, manifest: Manifest, plugin_dir: PathBuf) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = guard.get_mut(id) {
            p.manifest = manifest;
            p.plugin_dir = plugin_dir;
        }
    }

    /// 状态迁移（BR-01 单一入口）+ 立即持久化。
    pub fn apply(&self, id: &str, t: Transition) -> Result<PluginEntry, String> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let Some(p) = guard.get_mut(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        let next = transition_allowed((p.entry.enabled, p.entry.state), t)?;
        p.entry.enabled = next.0;
        p.entry.state = next.1;
        if next.1 == PluginState::Ready {
            // 恢复成功：故障摘要保留在 records 里，lastFault 清空（可观察的「已恢复」由记录承载）
            p.entry.last_fault = None;
        }
        let entry = p.entry.clone();
        let _ = self.save_locked(&guard);
        Ok(entry)
    }

    /// 直接置为故障态（装载/运行失败统一入口；非法边同样拒绝）。
    pub fn set_fault(&self, id: &str, record: FaultRecord) -> Result<PluginEntry, String> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let Some(p) = guard.get_mut(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        // 停用态下不应出现运行故障：先确保 enabled（停用→启用装载失败 → 故障）
        if !p.entry.enabled {
            p.entry.enabled = true;
        }
        p.entry.state = PluginState::Fault;
        p.entry.last_fault = Some(record.clone());
        p.records.push(record);
        trim_records(&mut p.records);
        let entry = p.entry.clone();
        let _ = self.save_locked(&guard);
        Ok(entry)
    }

    /// 追加一条记录（violation / approval / rejection 等不影响状态位）。
    pub fn push_record(&self, id: &str, record: FaultRecord) -> Option<FaultRecord> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let p = guard.get_mut(id)?;
        p.records.push(record.clone());
        trim_records(&mut p.records);
        p.entry.last_fault = Some(record.clone());
        let _ = self.save_locked(&guard);
        Some(record)
    }

    pub fn records(&self, id: &str) -> Vec<FaultRecord> {
        self.get(id).map(|p| p.records).unwrap_or_default()
    }

    /// 卸载：移除条目 + 持久化。
    pub fn remove(&self, id: &str) -> Option<PluginEntry> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let removed = guard.remove(id)?.entry;
        let _ = self.save_locked(&guard);
        Some(removed)
    }

    /// 更新确认流的条目替换（B4 BR-13）：版本/hash/权限面/待确认态一次写入 +
    /// manifest 快照刷新。状态位不动（启用/停用/故障语义与更新正交）。
    pub fn apply_update(
        &self,
        id: &str,
        version: String,
        content_hash: String,
        permissions_hash: String,
        manifest: Manifest,
        plugin_dir: PathBuf,
    ) -> Result<PluginEntry, String> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let Some(p) = guard.get_mut(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        p.entry.version = version;
        p.entry.content_hash = content_hash;
        p.entry.permissions_hash = permissions_hash;
        p.entry.pending_update = None;
        p.manifest = manifest;
        p.plugin_dir = plugin_dir;
        let entry = p.entry.clone();
        let _ = self.save_locked(&guard);
        Ok(entry)
    }

    /// 待确认更新登记/清除（B4；清除 = keep_old 路径）。
    pub fn set_pending_update(
        &self,
        id: &str,
        pending: Option<PendingUpdate>,
    ) -> Result<PluginEntry, String> {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let Some(p) = guard.get_mut(id) else {
            return Err(format!("插件 {id} 不在注册表中"));
        };
        p.entry.pending_update = pending;
        let entry = p.entry.clone();
        let _ = self.save_locked(&guard);
        Ok(entry)
    }

    /// 发现记录清理登记（B4 FAIL-2 回修）：卸载 + purge 后记录 (id, hash)，
    /// scanner 据此跳过同目录的重发现/重审计。同 id 只保留最新一条。
    pub fn dismiss_discovery(&self, id: &str, content_hash: &str) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let persisted = self.load_persisted_locked(&guard);
        let mut dismissed: Vec<DismissedEntry> = persisted
            .dismissed
            .into_iter()
            .filter(|d| d.id != id)
            .collect();
        dismissed.push(DismissedEntry {
            id: id.to_string(),
            content_hash: content_hash.to_string(),
        });
        self.save_dismissed_locked(&guard, dismissed);
    }

    /// 安装后解除清理登记（同 id 全部条目）。
    pub fn undismiss_discovery(&self, id: &str) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let persisted = self.load_persisted_locked(&guard);
        if persisted.dismissed.iter().any(|d| d.id == id) {
            let dismissed = persisted
                .dismissed
                .into_iter()
                .filter(|d| d.id != id)
                .collect();
            self.save_dismissed_locked(&guard, dismissed);
        }
    }

    /// (id, hash) 是否在清理登记内（scanner 跳过判据）。
    pub fn is_discovery_dismissed(&self, id: &str, content_hash: &str) -> bool {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let persisted = self.load_persisted_locked(&guard);
        persisted.dismissed.iter().any(|d| d.id == id && d.content_hash == content_hash)
    }

    /// 读持久文件里的 dismissed 段（inner 锁内调用；文件是唯一持久位，
    /// 内存表不缓存——扫描频率低，逐次读文件足够）。
    fn load_persisted_locked(&self, _guard: &BTreeMap<String, InstalledPlugin>) -> PersistedRegistry {
        std::fs::read_to_string(&self.file)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn save_dismissed_locked(
        &self,
        guard: &BTreeMap<String, InstalledPlugin>,
        dismissed: Vec<DismissedEntry>,
    ) {
        let persisted = PersistedRegistry {
            schema_version: 1,
            plugins: guard
                .values()
                .filter(|p| !p.entry.source.is_transient())
                .map(|p| p.entry.clone())
                .collect(),
            dismissed,
        };
        if let Ok(raw) = serde_json::to_string_pretty(&persisted) {
            let tmp = self.file.with_extension("json.tmp");
            if std::fs::write(&tmp, raw).is_ok() && std::fs::rename(&tmp, &self.file).is_ok() {
                crate::platform::harden_0600(&self.file);
            }
        }
    }

    fn save_locked(&self, map: &BTreeMap<String, InstalledPlugin>) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // transient 插件不写持久层（ADR-0001 第 4 条：不写注册表持久层、不参与更新比对）
        let persisted = PersistedRegistry {
            schema_version: 1,
            plugins: map
                .values()
                .filter(|p| !p.entry.source.is_transient())
                .map(|p| p.entry.clone())
                .collect(),
            dismissed: self.load_persisted_locked(&map).dismissed,
        };
        let raw = serde_json::to_string_pretty(&persisted)?;
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, &self.file)?;
        crate::platform::harden_0600(&self.file);
        Ok(())
    }
}

/// 记录表上限（防单插件高频违规把内存/磁盘打爆；保留最近 500 条）
const MAX_RECORDS: usize = 500;

fn trim_records(records: &mut Vec<FaultRecord>) {
    if records.len() > MAX_RECORDS {
        let drop_n = records.len() - MAX_RECORDS;
        records.drain(0..drop_n);
    }
}

/// 持久态加载时的占位 manifest（真实值由启动装载重新解析目录后刷新）。
fn placeholder_manifest() -> Manifest {
    Manifest {
        schema_version: 1,
        id: String::new(),
        name: String::new(),
        version: String::new(),
        description: None,
        author: None,
        homepage: None,
        hub: super::manifest::HubCompat {
            api_version: "1".into(),
            min_hub_version: None,
        },
        contributes: Default::default(),
        permissions: Default::default(),
        backend: None,
        extra: Default::default(),
    }
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> PluginEntry {
        PluginEntry {
            id: id.into(),
            version: "0.1.0".into(),
            content_hash: "sha256:aa".into(),
            permissions_hash: "sha256:bb".into(),
            source: PluginSource::drop_in(),
            installed_at: now_rfc3339(),
            enabled: true,
            state: PluginState::Ready,
            last_fault: None,
            pending_update: None,
        }
    }

    fn registry() -> (tempfile::TempDir, Registry) {
        let dir = tempfile::tempdir().unwrap();
        let r = Registry::open(dir.path().join("plugins.json"));
        (dir, r)
    }

    #[test]
    fn transition_table_matches_br01() {
        use PluginState::*;
        // 合法边
        assert_eq!(
            transition_allowed((false, Stopped), Transition::EnableOk),
            Ok((true, Ready))
        );
        assert_eq!(
            transition_allowed((false, Stopped), Transition::EnableFailed),
            Ok((true, Fault))
        );
        assert_eq!(
            transition_allowed((true, Ready), Transition::Fault),
            Ok((true, Fault))
        );
        assert_eq!(
            transition_allowed((true, Ready), Transition::Disable),
            Ok((false, Stopped))
        );
        assert_eq!(
            transition_allowed((true, Fault), Transition::RetryOk),
            Ok((true, Ready))
        );
        assert_eq!(
            transition_allowed((true, Fault), Transition::Disable),
            Ok((false, Stopped))
        );
        // 非法边
        assert!(transition_allowed((true, Ready), Transition::EnableOk).is_err());
        assert!(transition_allowed((true, Ready), Transition::RetryOk).is_err());
        assert!(transition_allowed((false, Stopped), Transition::Disable).is_err());
        assert!(transition_allowed((false, Stopped), Transition::Fault).is_err());
    }

    #[test]
    fn install_is_idempotent_on_same_hash() {
        let (_d, r) = registry();
        let m = placeholder_manifest();
        let (_, created) = r
            .insert_installed(entry("p1"), m.clone(), PathBuf::from("/tmp/p1"))
            .unwrap();
        assert!(created);
        let (_, created2) = r
            .insert_installed(entry("p1"), m, PathBuf::from("/tmp/p1"))
            .unwrap();
        assert!(!created2, "同 contentHash 重复安装必须无操作");
        assert_eq!(r.list().len(), 1);
    }

    #[test]
    fn persisted_state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plugins.json");
        {
            let r = Registry::open(file.clone());
            r.insert_installed(
                entry("p1"),
                placeholder_manifest(),
                PathBuf::from("/tmp/p1"),
            )
            .unwrap();
            r.apply("p1", Transition::Disable).unwrap();
        }
        let r2 = Registry::open(file.clone());
        let p = r2.get("p1").expect("should reload");
        assert!(!p.entry.enabled);
        assert_eq!(p.entry.state, PluginState::Stopped);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "plugins.json 必须 0600");
        }
    }

    #[test]
    fn fault_sets_last_fault_and_record() {
        let (_d, r) = registry();
        r.insert_installed(
            entry("p1"),
            placeholder_manifest(),
            PathBuf::from("/tmp/p1"),
        )
        .unwrap();
        let rec = FaultRecord {
            at: now_rfc3339(),
            kind: FaultKind::Crash,
            message: "backend exited (code 1)".into(),
        };
        let e = r.set_fault("p1", rec.clone()).unwrap();
        assert_eq!(e.state, PluginState::Fault);
        assert_eq!(r.records("p1").len(), 1);
        let e = r.apply("p1", Transition::RetryOk).unwrap();
        assert_eq!(e.state, PluginState::Ready);
        assert!(
            e.last_fault.is_none(),
            "恢复后 lastFault 清空，历史进 records"
        );
        assert_eq!(r.records("p1").len(), 1, "历史记录保留可查（BR-09）");
    }

    #[test]
    fn permissions_hash_is_order_insensitive() {
        let a = Permissions {
            api: vec!["GET /a".into(), "GET /b".into()],
            data: vec!["sessions:read".into(), "projects:read".into()],
            credentials: vec!["a".into(), "b".into()],
            high_risk: vec![],
        };
        let b = Permissions {
            api: vec!["GET /b".into(), "GET /a".into()],
            data: vec!["projects:read".into(), "sessions:read".into()],
            credentials: vec!["b".into(), "a".into()],
            high_risk: vec![],
        };
        assert_eq!(permissions_hash(&a), permissions_hash(&b));
        let c = Permissions {
            credentials: vec!["a".into()],
            ..b
        };
        assert_ne!(permissions_hash(&a), permissions_hash(&c));
    }

    #[test]
    fn remove_drops_entry() {
        let (_d, r) = registry();
        r.insert_installed(
            entry("p1"),
            placeholder_manifest(),
            PathBuf::from("/tmp/p1"),
        )
        .unwrap();
        assert!(r.remove("p1").is_some());
        assert!(!r.contains("p1"));
    }
}
