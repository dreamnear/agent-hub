//! ACP 会话注册表（批1 任务3）：hub 会话 id → 运行状态 / 连接。
//! 内存态：hub 重启即失（批3 任务13 落恢复方案）；AppState 持 Arc 共享。

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
};

use tokio::sync::broadcast;

use crate::drivers::claude::session::ChatEvent;
use crate::models::ChatMessage;

/// 单会话推送馈线（批2 任务8）：ChatMessage → 内存 history（会话内回放）+
/// chat 房间广播（WS 实时）。seq 会话内单调递增（pump 与 prompt 共用同一计数）。
/// history 上限 500 条（环形淘汰最旧；前端另有 bufferMax 淘汰，此为服务端上限）。
pub struct AcpFeed {
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    history: Mutex<Vec<ChatMessage>>,
    seq: AtomicU64,
    session_id: String,
}

const HISTORY_CAP: usize = 500;

impl std::fmt::Debug for AcpFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpFeed")
            .field("session_id", &self.session_id)
            .field(
                "history_len",
                &self.history.lock().expect("acp history lock").len(),
            )
            .finish()
    }
}

impl AcpFeed {
    pub fn new(
        session_id: String,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            rooms,
            history: Mutex::new(Vec::new()),
            seq: AtomicU64::new(0),
            session_id,
        })
    }

    /// 追加一条消息：先落 history（锁内截断），再广播房间（无订阅者可忽略）。
    pub fn push(&self, message: ChatMessage) {
        {
            let mut h = self.history.lock().expect("acp history lock");
            h.push(message.clone());
            let excess = h.len().saturating_sub(HISTORY_CAP);
            if excess > 0 {
                h.drain(..excess);
            }
        }
        let event = ChatEvent {
            session_id: self.session_id.clone(),
            message,
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
        };
        if let Some(sender) = self
            .rooms
            .read()
            .expect("chat_rooms lock")
            .get(&self.session_id)
        {
            let _ = sender.send(event);
        }
    }

    pub fn history(&self) -> Vec<ChatMessage> {
        self.history.lock().expect("acp history lock").clone()
    }
}

/// 会话运行状态（批1 最小集 + 批4 补 input 态；raw_state 映射批2 接 agents list）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpStatus {
    /// initialize/session/new 进行中
    Starting,
    /// 空闲（上一条 prompt 已收尾）
    Idle,
    /// prompt 进行中
    Working,
    /// 等待用户输入（批4：权限弹卡挂起，agent 被阻塞等应答）——
    /// ACP 协议无 needs_input 信号，由 hub 状态机推导：反向请求
    /// session/request_permission 注册挂起即翻此态，应答后回 Working。
    AwaitingInput,
    /// 子进程退出，会话不可用
    Dead,
}

impl AcpStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AcpStatus::Starting => "starting",
            AcpStatus::Idle => "idle",
            AcpStatus::Working => "working",
            AcpStatus::AwaitingInput => "awaiting_input",
            AcpStatus::Dead => "dead",
        }
    }

    /// agents list 分组映射（与 claude 侧 grouping 同构，批4 对齐三态）：
    /// starting/working→Working、awaiting_input→NeedsInput（对齐 claude blocked）、
    /// idle→Other（空闲待命，未退出不算完成）、dead→Completed（对齐 claude exited）。
    /// 不共用 models::map_group——claude 路径零触碰。
    pub fn group(self) -> crate::models::Group {
        match self {
            AcpStatus::Starting | AcpStatus::Working => crate::models::Group::Working,
            AcpStatus::AwaitingInput => crate::models::Group::NeedsInput,
            AcpStatus::Idle => crate::models::Group::Other,
            AcpStatus::Dead => crate::models::Group::Completed,
        }
    }
}

/// 一个 ACP 会话条目。
#[derive(Debug, Clone)]
pub struct AcpSession {
    /// ACP sessionId（hub 侧注册表同键）
    pub id: String,
    /// AcpAgentConfig.name（来源 agent 清单）
    pub agent: String,
    pub cwd: String,
    pub model: Option<String>,
    pub status: AcpStatus,
    pub conn: super::AcpConnection,
    /// 消息馈线（批2 任务8）：history 回放 + 房间广播 + seq 计数
    pub feed: Arc<AcpFeed>,
}

/// 用户对权限请求的应答（批3 任务11）。`option_id: None` = 取消（红线：不放行）。
#[derive(Debug, Clone)]
pub struct PermissionAnswer {
    pub option_id: Option<String>,
}

#[derive(Default)]
pub struct AcpRegistry {
    sessions: std::sync::RwLock<HashMap<String, AcpSession>>,
    /// 批3 任务11：挂起的权限请求（permKey → 应答通道）。dispatch 泵注册等待，
    /// REST 应答端点回流。r77：键改会话内自增唯一 id（`perm:{sessionId}:{n}`），
    /// 不再含 agent 反向请求 id——omp 重用反向 id 时不再撞键（重复注册覆盖旧通道
    /// 会静默自动拒掉旧请求、前端 dismissed 状态错卡）。
    pending_perms: Mutex<HashMap<String, tokio::sync::oneshot::Sender<PermissionAnswer>>>,
    /// r77：权限卡键自增计数（会话内唯一）
    perm_seq: AtomicU64,
    /// O1：prompt 存活标志（send 进入置位，正常收尾/守卫 Drop/release 清除）。
    /// dispatch 应答收场据此判断是否可翻 Working——prompt future 已死时翻
    /// Working 将无人收尾，状态永久滞留。
    prompt_alive: Mutex<HashSet<String>>,
    /// 批3 任务13：会话持久化文件（None = 不落盘，测试用）
    store_file: Option<std::path::PathBuf>,
}

impl AcpRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 带持久化文件的注册表（批3 任务13；AppState::from_config 用）
    pub fn with_store(file: std::path::PathBuf) -> Self {
        Self {
            store_file: Some(file),
            ..Self::default()
        }
    }

    /// 落盘当前会话快照（批3 任务13：create/release 后调用；无配置文件则空转）。
    pub fn persist_async(&self) {
        let Some(file) = self.store_file.clone() else {
            return;
        };
        let entries = self.persisted_snapshot();
        tokio::spawn(async move { save_persisted(&file, &entries).await });
    }

    pub fn insert(&self, session: AcpSession) {
        self.sessions
            .write()
            .expect("acp registry lock")
            .insert(session.id.clone(), session);
    }

    pub fn get(&self, id: &str) -> Option<AcpSession> {
        self.sessions
            .read()
            .expect("acp registry lock")
            .get(id)
            .cloned()
    }

    pub fn remove(&self, id: &str) -> Option<AcpSession> {
        self.set_prompt_alive(id, false);
        self.sessions.write().expect("acp registry lock").remove(id)
    }

    pub fn list(&self) -> Vec<AcpSession> {
        self.sessions
            .read()
            .expect("acp registry lock")
            .values()
            .cloned()
            .collect()
    }

    pub fn set_status(&self, id: &str, status: AcpStatus) {
        if let Some(s) = self
            .sessions
            .write()
            .expect("acp registry lock")
            .get_mut(id)
        {
            s.status = status;
        }
    }

    /// O1：维护会话 prompt future 存活标志（send 进入/收尾与守卫 Drop 调用）。
    pub fn set_prompt_alive(&self, id: &str, alive: bool) {
        let mut set = self.prompt_alive.lock().expect("acp prompt lock");
        if alive {
            set.insert(id.to_string());
        } else {
            set.remove(id);
        }
    }

    /// O1：该会话是否仍有存活的 prompt future。
    pub fn is_prompt_alive(&self, id: &str) -> bool {
        self.prompt_alive
            .lock()
            .expect("acp prompt lock")
            .contains(id)
    }

    /// 注册挂起权限请求，返回应答接收端（批3 任务11）。r77：键由调用方用
    /// `next_perm_id` 生成（会话内唯一），同键重复注册只可能来自测试。
    pub fn register_permission(
        &self,
        key: String,
    ) -> tokio::sync::oneshot::Receiver<PermissionAnswer> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_perms
            .lock()
            .expect("acp perms lock")
            .insert(key, tx);
        rx
    }

    /// r77：下一个权限卡键序号（注册表级自增，跨会话唯一）。
    pub fn next_perm_id(&self) -> u64 {
        self.perm_seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// 回流用户应答；键不存在（已应答/已超时清理）返回 false。
    pub fn resolve_permission(&self, key: &str, answer: PermissionAnswer) -> bool {
        self.pending_perms
            .lock()
            .expect("acp perms lock")
            .remove(key)
            .is_some_and(|tx| tx.send(answer).is_ok())
    }

    /// r77：会话维度清挂起权限——会话取消/子进程死亡时唤醒所有在等的
    /// dispatch（各挂起方收到取消语义自行出回执、收尾，不再等满超时）。
    /// 返回唤醒数。
    pub fn resolve_session_perms(&self, session_id: &str) -> usize {
        let prefix = format!("perm:{session_id}:");
        let mut map = self.pending_perms.lock().expect("acp perms lock");
        let wake_keys: Vec<String> = map
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        for k in &wake_keys {
            if let Some(tx) = map.remove(k) {
                let _ = tx.send(PermissionAnswer { option_id: None });
            }
        }
        wake_keys.len()
    }

    /// 当前会话的持久化快照（批3 任务13：create/release 时落盘重写）。
    pub fn persisted_snapshot(&self) -> Vec<PersistedSession> {
        self.sessions
            .read()
            .expect("acp registry lock")
            .values()
            .map(|s| PersistedSession {
                id: s.id.clone(),
                agent: s.agent.clone(),
                cwd: s.cwd.clone(),
                model: s.model.clone(),
            })
            .collect()
    }
}

/// 持久化的会话元数据（批3 任务13）：重启后经 ACP session/load 恢复；
/// 状态不持久化（恢复后一律 Idle）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedSession {
    pub id: String,
    pub agent: String,
    pub cwd: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// 落盘 / 读取（projects.json 惯例：整文件重写，目录不存在则创建）。
/// ocr-review 低：写临时文件后 rename 原子替换——崩溃不留半截 JSON
/// （半截文件会被 load_persisted 兜底成空清单，重启丢失全部会话）。
pub async fn save_persisted(file: &std::path::Path, entries: &[PersistedSession]) {
    if let Some(dir) = file.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    match serde_json::to_string_pretty(entries) {
        Ok(raw) => {
            let tmp = file.with_extension("json.tmp");
            if let Err(e) = tokio::fs::write(&tmp, raw).await {
                tracing::warn!(error = %e, "ACP 会话持久化临时写入失败");
                return;
            }
            if let Err(e) = tokio::fs::rename(&tmp, file).await {
                tracing::warn!(error = %e, "ACP 会话持久化原子替换失败");
            }
        }
        Err(e) => tracing::warn!(error = %e, "ACP 会话持久化序列化失败"),
    }
}

pub async fn load_persisted(file: &std::path::Path) -> Vec<PersistedSession> {
    match tokio::fs::read_to_string(file).await {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AcpAgentConfig;

    /// 批2 任务6：状态 → 三段式分组映射（dead 对齐 claude exited → Completed）
    #[test]
    fn status_group_mapping() {
        use crate::models::Group;
        assert_eq!(AcpStatus::Starting.group(), Group::Working);
        assert_eq!(AcpStatus::Working.group(), Group::Working);
        assert_eq!(AcpStatus::AwaitingInput.group(), Group::NeedsInput);
        assert_eq!(AcpStatus::Idle.group(), Group::Other);
        assert_eq!(AcpStatus::Dead.group(), Group::Completed);
    }

    fn session(id: &str, conn: super::super::AcpConnection) -> AcpSession {
        AcpSession {
            id: id.into(),
            agent: "fake".into(),
            cwd: "/tmp".into(),
            model: None,
            status: AcpStatus::Starting,
            conn,
            feed: AcpFeed::new(id.into(), Arc::new(RwLock::new(HashMap::new()))),
        }
    }

    /// 批2 任务8：feed push → history 回放 + 房间广播 + seq 递增；超上限淘汰最旧
    #[test]
    fn feed_push_history_broadcast_seq_cap() {
        let rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        rooms
            .write()
            .expect("rooms")
            .insert("s1".into(), tokio::sync::broadcast::channel(8).0);
        let feed = AcpFeed::new("s1".into(), rooms);
        let mut rx = feed
            .rooms
            .read()
            .expect("rooms")
            .get("s1")
            .unwrap()
            .subscribe();
        for i in 0..3 {
            feed.push(ChatMessage {
                kind: crate::models::ChatMessageKind::Assistant,
                text: Some(format!("m{i}")),
                ..ChatMessage::default()
            });
        }
        assert_eq!(feed.history().len(), 3);
        let ev = rx.try_recv().unwrap();
        assert_eq!(ev.seq, 1);
        assert_eq!(ev.message.text.as_deref(), Some("m0"));
        // cap：501 条 → 最旧被淘汰
        for i in 0..600 {
            feed.push(ChatMessage {
                text: Some(format!("x{i}")),
                ..ChatMessage::default()
            });
        }
        let h = feed.history();
        assert_eq!(h.len(), HISTORY_CAP);
        assert_eq!(h.last().unwrap().text.as_deref(), Some("x599"));
    }

    /// env var 全局态无、spawn 真连接（注册表条目含连接体，零 UB 造法）
    #[tokio::test]
    async fn insert_get_update_remove() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_fake_agent.py");
        let agent = AcpAgentConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![
                "-u".into(),
                fixture.to_string_lossy().into_owned(),
                "basic".into(),
            ],
            cwd: None,
            model: None,
        };
        let (conn, _rx) = super::super::AcpConnection::spawn(&agent, dir.path())
            .await
            .unwrap();

        let reg = AcpRegistry::new();
        assert!(reg.get("s1").is_none());
        reg.insert(session("s1", conn));
        let got = reg.get("s1").unwrap();
        assert_eq!(got.status, AcpStatus::Starting);
        assert_eq!(got.agent, "fake");
        reg.set_status("s1", AcpStatus::Working);
        assert_eq!(reg.get("s1").unwrap().status, AcpStatus::Working);
        assert_eq!(reg.list().len(), 1);
        assert!(reg.remove("s1").is_some());
        assert!(reg.get("s1").is_none());
        assert!(reg.remove("s1").is_none());
        // 收尾：真实子进程 reap
        for s in reg.list() {
            s.conn.shutdown().await;
        }
    }

    #[tokio::test]
    async fn set_status_missing_is_noop() {
        let reg = AcpRegistry::new();
        reg.set_status("ghost", AcpStatus::Dead);
        assert!(reg.list().is_empty());
    }

    /// 批3 任务11：权限请求注册 → 应答回流；未注册键返回 false；二次应答 false
    #[tokio::test]
    async fn permission_pending_roundtrip() {
        let reg = AcpRegistry::new();
        let rx = reg.register_permission("perm:s1:9".into());
        assert!(
            !reg.resolve_permission("perm:ghost", PermissionAnswer { option_id: None }),
            "未注册键应返回 false"
        );
        assert!(reg.resolve_permission(
            "perm:s1:9",
            PermissionAnswer {
                option_id: Some("opt-allow".into()),
            },
        ));
        let answered = rx.await.unwrap();
        assert_eq!(answered.option_id.as_deref(), Some("opt-allow"));
        // 已消费：二次应答 false
        assert!(!reg.resolve_permission("perm:s1:9", PermissionAnswer { option_id: None },));
    }

    /// ocr-review 低：持久化原子替换——落盘可回读、无 .tmp 残留
    #[tokio::test]
    async fn persisted_roundtrip_atomic_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("acp_sessions.json");
        let entries = vec![PersistedSession {
            id: "s1".into(),
            agent: "omp".into(),
            cwd: "/repo".into(),
            model: None,
        }];
        save_persisted(&file, &entries).await;
        assert!(file.exists());
        let back = load_persisted(&file).await;
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "s1");
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(residue.is_empty(), "不应残留临时文件: {residue:?}");
    }
}
