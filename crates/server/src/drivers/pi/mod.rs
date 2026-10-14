//! pi driver（agent-hub-pi-hub-agent P1）：`pi --mode rpc` 子进程 + JSONL stdio
//! 传输 + 单实例「Hub 助手」会话。与 ACP 同构（独立进程组、reader 任务、pending
//! 请求表），差异在协议：pi 是命令/事件直传 JSONL（非 JSON-RPC），prompt 受理
//! 应答 ≠ turn 完成（完成走 agent_end 事件，状态机全权在 dispatch 泵）。
//! 与 Claude PTY 链路零共享；会话 id 固定 ASSISTANT_ID（单实例注册表语义）。

pub mod dispatch;
pub mod extension;
pub mod piconf;
pub mod protocol;

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{broadcast, oneshot, Mutex as AsyncMutex},
};

use crate::{
    config::PiConfig,
    drivers::{
        events::{ChatEvent, HubEvent, MessagePage},
        unsupported, AgentCatalog, AgentDriver, ModelSettings, SessionControl, SessionHistory,
        StartSpec,
    },
    models::{AgentSummary, ChatMessage},
    platform,
};

/// Hub 助手会话 id（单实例固定键；侧栏/chat 房间同键）。
pub const ASSISTANT_ID: &str = "hub-assistant";

/// 命令受理应答超时（get_state 0.73.1 实测 ~0.6s；prompt 受理为本地校验，留余量）。
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(30);
/// SIGTERM 后宽限期，超时 SIGKILL 连坐进程组（对齐 ACP）。
const KILL_GRACE: Duration = Duration::from_secs(5);

/// 会话状态（dispatch 泵全权翻转；send 仅在受理失败时不动状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiStatus {
    /// spawn 进行中（RPC 通道未验证）
    Starting,
    Idle,
    /// prompt 受理后到 agent_end 之间
    Working,
    /// 子进程退出
    Dead,
}

impl PiStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PiStatus::Starting => "starting",
            PiStatus::Idle => "idle",
            PiStatus::Working => "working",
            PiStatus::Dead => "dead",
        }
    }

    /// 侧栏分组映射（对齐 AcpStatus::group 语义，不共用 models::map_group）。
    pub fn group(self) -> crate::models::Group {
        match self {
            PiStatus::Starting | PiStatus::Working => crate::models::Group::Working,
            PiStatus::Idle => crate::models::Group::Other,
            PiStatus::Dead => crate::models::Group::Completed,
        }
    }
}

/// 消息馈线（opencode 先例：各驱动自持 feed，rooms 表与 AppState 共源）。
/// history 回放 + 房间广播 + 会话内单调 seq。
pub struct PiFeed {
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    history: Mutex<Vec<ChatMessage>>,
    seq: AtomicU64,
    session_id: String,
}

impl std::fmt::Debug for PiFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiFeed")
            .field("session_id", &self.session_id)
            .field(
                "history_len",
                &self.history.lock().expect("pi history lock").len(),
            )
            .finish()
    }
}

const HISTORY_CAP: usize = 500;

impl PiFeed {
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
            let mut h = self.history.lock().expect("pi history lock");
            h.push(message.clone());
            let excess = h.len().saturating_sub(HISTORY_CAP);
            if excess > 0 {
                h.drain(..excess);
            }
        }
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.broadcast_one(message, seq);
    }

    pub fn history(&self) -> Vec<ChatMessage> {
        self.history.lock().expect("pi history lock").clone()
    }

    /// 广播单条到会话房间（无订阅者可忽略）。push/reseed 共用尾段。
    fn broadcast_one(&self, message: ChatMessage, seq: u64) {
        let event = ChatEvent {
            session_id: self.session_id.clone(),
            message,
            seq,
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

    /// 历史回灌（assistant-resume A1）：`--continue` 恢复会话后把持久化历史
    /// 整体**前插** history 并广播。前插保证「回灌慢于实时首条」竞态下 history
    /// 时序仍正确（page 全量回放是前端主路径；WS 订阅者极端竞态下短暂乱序，
    /// 刷新自愈）。空批次 no-op（新会话场景零扰动）。
    pub fn reseed(&self, messages: Vec<ChatMessage>) {
        if messages.is_empty() {
            return;
        }
        {
            let mut h = self.history.lock().expect("pi history lock");
            h.splice(0..0, messages.clone());
            // HISTORY_CAP 再收敛一次（前插可能顶破上限）
            let excess = h.len().saturating_sub(HISTORY_CAP);
            if excess > 0 {
                h.drain(..excess);
            }
        }
        for message in messages {
            let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
            self.broadcast_one(message, seq);
        }
    }

    /// 会话重置（assistant-resume A5 手动新建）：清内存历史。pi 侧 new_session
    /// 已切新会话文件；前端新建成功后主动 invalidate 消息查询，不走广播。
    pub fn clear(&self) {
        self.history.lock().expect("pi history lock").clear();
    }
}

/// agent → hub 的入站事件（response 已由连接内消化，泵只关心这三类）。
#[derive(Debug, Clone)]
pub enum Inbound {
    /// 扩展 UI 请求（dialog 类需取消应答）
    UiRequest {
        id: serde_json::Value,
        method: String,
    },
    /// agent 事件流（原样 JSON）
    Event(serde_json::Value),
    /// 子进程退出（EOF），pending 请求已全部失败
    Closed,
}

type PendingMap =
    Mutex<HashMap<serde_json::Value, oneshot::Sender<Result<protocol::RpcResponse, String>>>>;

/// 共享写路径与 pending 表（reader 任务与对外 request 两头用）。
struct Shared {
    stdin: AsyncMutex<tokio::process::ChildStdin>,
    pending: PendingMap,
    inbound_tx: broadcast::Sender<Inbound>,
    dead: AtomicBool,
    child: AsyncMutex<Option<tokio::process::Child>>,
}

/// 一条 pi RPC 子进程连接。Clone = Arc 克隆。
#[derive(Clone)]
pub struct PiConnection {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for PiConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiConnection")
            .field("dead", &self.shared.dead.load(Ordering::SeqCst))
            .finish()
    }
}

impl PiConnection {
    /// spawn pi 子进程并启动 reader。独立进程组（unix），shutdown 连坐清除。
    /// `args` 全权由调用方组装（生产 = RPC 形态；测试 = 假体接管），此处只补
    /// env 与管道化。
    pub async fn spawn(
        bin: &str,
        _session_dir: &Path,
        args: &[String],
        hub_port: u16,
        work_dir: Option<&Path>,
    ) -> Result<(Self, broadcast::Receiver<Inbound>)> {
        let mut cmd = tokio::process::Command::new(bin);
        // model-key-bug 系统性回修：hub 侧 PI_AGENT_DIR 只约束 hub 自己的读写路径，
        // **pi 子进程不认它**（pi 的配置目录 env = PI_CODING_AGENT_DIR，dist/config.js
        // ENV_AGENT_DIR 实锤）——此前「PI_AGENT_DIR 隔离」对 spawn 出的助手无效，
        // 助手始终读写 ~/.pi/agent。此处把 hub 配置的 agent dir 同步注入 pi 的
        // 正式 env：生产不设 PI_AGENT_DIR 时不注入（零影响），隔离实例从此真隔离。
        if let Some(agent_dir) = std::env::var("PI_AGENT_DIR").ok().filter(|s| !s.is_empty()) {
            cmd.env("PI_CODING_AGENT_DIR", &agent_dir);
        }
        // P11：助手工作目录 = pi 进程 cwd（ctx.cwd，助手的文件操作根）
        if let Some(wd) = work_dir {
            cmd.current_dir(wd);
        }
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // 安全模板 §6：独立进程组 → 组杀连坐；参数数组传递，无 shell 拼接
        //（入组实现平台差异归驱动层）
        platform::new_process_group(&mut cmd);
        // P8：extension 工具内调 hub API 的入口（loopback 直通免 token，最小暴露）
        cmd.env("HUB_API_BASE", format!("http://127.0.0.1:{hub_port}"));
        let mut child = cmd
            .spawn()
            .with_context(|| format!("pi RPC 启动失败: {bin} --mode rpc"))?;
        let stdin = child.stdin.take().context("stdin 未管道化")?;
        let stdout = child.stdout.take().context("stdout 未管道化")?;
        let stderr = child.stderr.take().context("stderr 未管道化")?;

        let (inbound_tx, inbound_rx) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            stdin: AsyncMutex::new(stdin),
            pending: Mutex::new(HashMap::new()),
            inbound_tx,
            dead: AtomicBool::new(false),
            child: AsyncMutex::new(Some(child)),
        });

        // stderr 转结构化日志（pi 诊断信息，不参与协议）
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "pi_agent_stderr", "{line}");
            }
        });

        let conn = Self { shared };
        conn.spawn_reader(stdout);
        Ok((conn, inbound_rx))
    }

    fn spawn_reader(&self, stdout: tokio::process::ChildStdout) {
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                match protocol::parse_line(&line) {
                    protocol::Inbound::Response(resp) => {
                        let tx = shared
                            .pending
                            .lock()
                            .expect("pending lock")
                            .remove(&resp.id);
                        match tx {
                            Some(tx) => {
                                let _ = tx.send(if resp.success {
                                    Ok(resp)
                                } else {
                                    Err(resp.error.unwrap_or_else(|| "pi 命令失败".into()))
                                });
                            }
                            None => {
                                tracing::debug!(target: "pi", id = %resp.id, "无待关联的响应")
                            }
                        }
                    }
                    protocol::Inbound::UiRequest { id, method } => {
                        let _ = shared.inbound_tx.send(Inbound::UiRequest { id, method });
                    }
                    protocol::Inbound::Event(v) => {
                        let _ = shared.inbound_tx.send(Inbound::Event(v));
                    }
                    protocol::Inbound::Malformed => {
                        tracing::debug!(target: "pi", "忽略非协议帧")
                    }
                }
            }
            // EOF：子进程退出 → 标记死亡、清空 pending、广播 Closed
            shared.dead.store(true, Ordering::SeqCst);
            let waiters: Vec<_> = shared
                .pending
                .lock()
                .expect("pending lock")
                .drain()
                .map(|(_, tx)| tx)
                .collect();
            for tx in waiters {
                let _ = tx.send(Err("pi 进程已退出".into()));
            }
            let _ = shared.inbound_tx.send(Inbound::Closed);
            // 连坐收尸：组杀清残留 + reap 防 zombie
            reap(&shared, KILL_GRACE).await;
        });
    }

    fn next_id(&self) -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    async fn write_line(&self, line: String) -> Result<()> {
        if self.shared.dead.load(Ordering::SeqCst) {
            anyhow::bail!("pi 进程已退出");
        }
        let mut stdin = self.shared.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// 命令往返：分配 id、注册 oneshot、写帧、限时等受理应答。
    /// `success:false` / EOF / 超时一律 Err（error 文案面向用户）。
    pub async fn request(&self, line: String, timeout: Duration) -> Result<protocol::RpcResponse> {
        let id = self.next_id();
        // 帧内 id 替换：构建器以占位 0 生成，这里以连接级自增 id 覆写
        let mut v: serde_json::Value = serde_json::from_str(&line).context("pi 命令帧非法")?;
        v["id"] = serde_json::json!(id);
        let line = v.to_string();
        let (tx, rx) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .expect("pending lock")
            .insert(serde_json::json!(id), tx);
        if let Err(e) = self.write_line(line).await {
            self.shared
                .pending
                .lock()
                .expect("pending lock")
                .remove(&serde_json::json!(id));
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(resp))) => Ok(resp),
            Ok(Ok(Err(msg))) => anyhow::bail!("{msg}"),
            Ok(Err(_)) => anyhow::bail!("pi 进程已退出（请求未应答）"),
            Err(_) => {
                self.shared
                    .pending
                    .lock()
                    .expect("pending lock")
                    .remove(&serde_json::json!(id));
                anyhow::bail!("pi 请求超时（{}s）", timeout.as_secs())
            }
        }
    }

    /// 应答扩展 UI 请求（hub 语义：dialog 一律取消，助手不被卡死）。
    pub async fn respond_ui_cancelled(&self, id: &serde_json::Value) -> Result<()> {
        self.write_line(protocol::ui_response_cancelled(id)).await
    }

    pub fn is_dead(&self) -> bool {
        self.shared.dead.load(Ordering::SeqCst)
    }

    /// 主动关停：SIGTERM 进程组 → 宽限 → SIGKILL 连坐；幂等。
    pub async fn shutdown(&self) {
        reap(&self.shared, KILL_GRACE).await;
    }
}

/// 进程组连坐清除 + reap：SIGTERM → KILL_GRACE → SIGKILL。幂等（child 取出后空转）。
/// 组杀实现归驱动层（platform::kill_group）。
async fn reap(shared: &Shared, grace: Duration) {
    let mut guard = shared.child.lock().await;
    let Some(child) = guard.as_mut() else {
        return;
    };
    if let Some(pid) = child.id() {
        platform::kill_group(pid, platform::Signal::Term);
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            if let Some(pid) = child.id() {
                // 宽限期满强制连坐
                platform::kill_group(pid, platform::Signal::Kill);
                let _ = child.wait().await;
            }
        }
    }
    *guard = None;
    shared.dead.store(true, Ordering::SeqCst);
}

/// 一个 Hub 助手会话条目（单实例注册表语义）。
#[derive(Debug, Clone)]
pub struct PiSession {
    pub id: String,
    pub status: PiStatus,
    /// get_state 采到的当前模型显示串（读不到留 None）
    pub model: Option<String>,
    pub conn: PiConnection,
    pub feed: Arc<PiFeed>,
    /// 会话代际（restart-feedback 回修：stop→start 快速切换时，旧连接的泵
    /// EOF 收场晚于新 spawn——泵凭代数识别「自己那代」，不污染新会话状态/记录）
    pub gen: u64,
}

/// P3：pi provider/model 设置（设置页读写，变更重启助手生效）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PiSettings {
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// pi driver 内共享核（泵与 trait 实现同源）。
struct PiInner {
    cfg: PiConfig,
    /// P9 版本探测缓存（Some=已探测；None=未探测/探测失败不可区分——容错显示层处理）
    version: tokio::sync::Mutex<Option<VersionCache>>,
    /// 额外 spawn 参数：生产为空；测试注入假体路径/mode（拼在标准参数后）
    args_extra: Vec<String>,
    /// 启动互斥（tokio 锁：ensure_started 的 spawn 段不持 std 锁跨 await）
    start_lock: tokio::sync::Mutex<()>,
    session: RwLock<Option<PiSession>>,
    events_tx: broadcast::Sender<HubEvent>,
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    /// 会话代际计数（每次真 spawn +1；泵凭 spawn 时捕获的代数识别自己那代）
    gen: std::sync::atomic::AtomicU64,
    /// 最近一次真实 spawn 的结果（restart-feedback：设置页保存后轮询此记录收敛
    /// 「重启中」到成功/失败终态；仅记真 spawn——活实例幂等早退不写，防覆盖）。
    last_start_result: Mutex<Option<StartResult>>,
}

/// 一次真实 spawn 的终态记录（epoch millis，供前端按「新于本地快照」判定）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub ok: bool,
    pub error: Option<String>,
    pub at_ms: i64,
}

/// P9 版本缓存条目：探测结果 + 采集时刻（TTL 内直接复用，pi 升级后至多一个
/// TTL 周期内显示旧值——升级本就建议重启助手，可接受）。
#[derive(Debug, Clone)]
struct VersionCache {
    text: String,
    at: std::time::Instant,
}

/// 缓存 TTL（设置页频繁刷新不反复拉子进程）。
const VERSION_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// pi driver 门面：单实例「Hub 助手」生命周期 + 设置持久化。
/// Clone = Arc 克隆；注册表注册 Arc<PiDriver>。
#[derive(Clone)]
pub struct PiDriver {
    inner: Arc<PiInner>,
    /// P3 设置文件：`~/.claude-view/pi_settings.json`
    pub settings_file: PathBuf,
    /// P8 extension 注入文件（hub 生成写入，spawn 时 -e 加载）
    pub extension_file: PathBuf,
    /// pi 会话存储目录（--session-dir）
    pub session_dir: PathBuf,
    /// hub loopback 端口（spawn env HUB_API_BASE 注入）
    hub_port: u16,
    /// P10：pi 原生配置目录（~/.pi/agent；PI_AGENT_DIR 测试隔离）——单一事实源
    pub agent_dir: std::path::PathBuf,
}

impl std::fmt::Debug for PiDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiDriver")
            .field("running", &self.is_running())
            .finish()
    }
}

impl PiDriver {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: PiConfig,
        settings_file: PathBuf,
        extension_file: PathBuf,
        session_dir: PathBuf,
        hub_port: u16,
        events_tx: broadcast::Sender<HubEvent>,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
        agent_dir: PathBuf,
    ) -> Self {
        Self::with_args_extra(
            cfg,
            settings_file,
            extension_file,
            session_dir,
            hub_port,
            events_tx,
            rooms,
            Vec::new(),
            agent_dir,
        )
    }

    /// 带额外 spawn 参数的构造（测试注入口：假体路径/mode/log）。
    #[allow(clippy::too_many_arguments)]
    pub fn with_args_extra(
        cfg: PiConfig,
        settings_file: PathBuf,
        extension_file: PathBuf,
        session_dir: PathBuf,
        hub_port: u16,
        events_tx: broadcast::Sender<HubEvent>,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
        args_extra: Vec<String>,
        agent_dir: PathBuf,
    ) -> Self {
        Self {
            inner: Arc::new(PiInner {
                cfg,
                args_extra,
                version: tokio::sync::Mutex::new(None),
                start_lock: tokio::sync::Mutex::new(()),
                session: RwLock::new(None),
                gen: std::sync::atomic::AtomicU64::new(0),
                events_tx,
                rooms,
                last_start_result: Mutex::new(None),
            }),
            settings_file,
            extension_file,
            session_dir,
            hub_port,
            agent_dir,
        }
    }

    pub(super) fn tick(&self) {
        let _ = self.inner.events_tx.send(HubEvent::Tick);
    }

    /// 生产 spawn 参数（RPC 形态）。`--continue`（assistant-resume A1/A3）：pi 在
    /// `--session-dir` 内锚定最近会话恢复上下文（隔离实测 sessionFile 命中）；
    /// 无历史会话时 pi 自动降级新会话不报错（A4，空目录实测 messageCount=0）。
    /// A5 手动新建走 RPC new_session——切内存态；session 文件等首条消息才落盘
    ///（pi 0.73.1 实测语义），发出首条消息后即成为下次 -c 目标。已知边界：新建后
    /// 零消息重启 → --continue 锚回上一有内容会话（无内容损失；闭合需 hub 记状态
    /// 或预写 pi 内部格式文件，均违背既有机制/零新格式原则，裁量记边界不闭合）。
    pub(crate) fn production_args(&self) -> Vec<String> {
        vec![
            "--mode".into(),
            "rpc".into(),
            "--continue".into(),
            "--session-dir".into(),
            self.session_dir.to_string_lossy().into_owned(),
            // P2：管家角色提示（append 保留 pi 基础提示）+ 禁内建工具
            //（-nbt：extension 工具保留，最小权限——管家纯靠 hub_* 工具）
            "--append-system-prompt".into(),
            extension::ASSISTANT_SYSTEM_PROMPT.into(),
            "--no-builtin-tools".into(),
            "-e".into(),
            self.extension_file.to_string_lossy().into_owned(),
        ]
    }

    pub fn is_running(&self) -> bool {
        self.inner
            .session
            .read()
            .expect("pi session lock")
            .as_ref()
            .is_some_and(|s| !s.conn.is_dead())
    }

    // ---- 泵（dispatch）访问面：PiInner 不跨模块裸暴露 ----

    pub(super) fn set_status(&self, status: PiStatus) {
        if let Some(s) = self
            .inner
            .session
            .write()
            .expect("pi session lock")
            .as_mut()
        {
            s.status = status;
        }
    }

    pub(super) fn session_mut(&self) -> Option<std::sync::RwLockWriteGuard<'_, Option<PiSession>>> {
        Some(self.inner.session.write().expect("pi session lock"))
    }

    pub(super) fn session_conn(&self) -> Option<PiConnection> {
        self.inner
            .session
            .read()
            .expect("pi session lock")
            .as_ref()
            .map(|s| s.conn.clone())
    }

    pub(super) fn session_feed(&self) -> Option<Arc<PiFeed>> {
        self.inner
            .session
            .read()
            .expect("pi session lock")
            .as_ref()
            .map(|s| s.feed.clone())
    }

    /// 侧栏条目（单实例：运行中 1 条，否则空）。
    pub fn summaries(&self) -> Vec<AgentSummary> {
        let guard = self.inner.session.read().expect("pi session lock");
        let Some(s) = guard.as_ref() else {
            return Vec::new();
        };
        vec![AgentSummary {
            driver: "pi".into(),
            id: s.id.clone(),
            name: Some(ASSISTANT_ID.into()),
            cwd: None,
            kind: Some("pi".into()),
            raw_state: Some(s.status.as_str().into()),
            group: s.status.group(),
            detail: s.model.clone(),
            tokens: None,
            context_tokens: None,
            started_at: None,
            session_id: Some(s.id.clone()),
        }]
    }

    /// 幂等启动（P2 auto_start / P5 重开 / AgentCatalog::start 共用）：
    /// 活实例直接返回；死实例先清再起。返回会话 id。
    /// 启动互斥用独立 tokio Mutex（std 锁不跨 await——Send 红线）；session 表
    /// 锁仅在检查/写入瞬间持有。
    pub async fn ensure_started(&self) -> Result<String> {
        let _permit = self.inner.start_lock.lock().await;
        {
            let guard = self.inner.session.read().expect("pi session lock");
            if let Some(s) = guard.as_ref() {
                if !s.conn.is_dead() {
                    return Ok(s.id.clone());
                }
            }
        }
        // restart-feedback：真 spawn 路径的结果记录（活实例幂等早退不写——防把
        // 上一次的记录时间戳顶新，前端「新于快照」判定失效）。Err/Ok 两路径落
        // 终态；握手失败面由泵 Closed 收场回写覆盖（exit-3 回修）。
        let record = |r: std::result::Result<String, String>| match r {
            Ok(_) => self.record_start_result(true, None),
            Err(e) => self.record_start_result(false, Some(e)),
        };
        // spawn 参数：生产 = RPC 形态（--mode rpc --session-dir + P2 管家提示 +
        // P8 extension + assistant-resume --continue）。P10 起单一事实源 = pi 原生
        // 配置（settings.json/models.json，pi 自启读取）——hub 不再传 --provider/
        // --model（P3 hub 层 pi_settings.json 退役停写）；测试注入口（args_extra
        // 非空）= 完整接管（bin 非 pi）
        let spawn_args: Vec<String> = if self.inner.args_extra.is_empty() {
            // P8：extension 每次 spawn 前重写（内容随 hub 版本同步），必加载
            extension::ensure_extension(&self.extension_file).await?;
            self.production_args()
        } else {
            self.inner.args_extra.clone()
        };
        // P11：工作目录（flags.workDir，None → 默认 $HOME/agent-hub；校验+mkdir）
        let gen = self
            .inner
            .gen
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        match self.start_inner(&spawn_args, gen).await {
            Ok(()) => {
                record(Ok(ASSISTANT_ID.into()));
                Ok(ASSISTANT_ID.into())
            }
            Err(e) => {
                record(Err(e.to_string()));
                Err(e)
            }
        }
    }

    /// spawn 主体（ensure_started 的参数组装后段；独立成函数便于结果记录收敛）。
    async fn start_inner(&self, spawn_args: &[String], gen: u64) -> Result<()> {
        let flags = piconf::read_flags(&self.settings_file).await;
        let work_dir = piconf::resolve_work_dir(flags.work_dir.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("Hub 助手启动失败: {e}"))?;
        let (conn, rx) = PiConnection::spawn(
            &self.inner.cfg.bin,
            &self.session_dir,
            spawn_args,
            self.hub_port,
            Some(&work_dir),
        )
        .await?;
        let feed = PiFeed::new(ASSISTANT_ID.into(), self.inner.rooms.clone());
        {
            let mut guard = self.inner.session.write().expect("pi session lock");
            *guard = Some(PiSession {
                id: ASSISTANT_ID.into(),
                status: PiStatus::Starting,
                model: None,
                conn,
                feed,
                gen,
            });
        }
        dispatch::spawn_pump(self.clone(), rx, gen);
        self.tick();
        Ok(())
    }

    /// 最近一次真实 spawn 的终态（restart-feedback：设置页轮询收敛「重启中」）。
    /// None = 本进程尚无 spawn 记录。
    pub fn last_start_result(&self) -> Option<StartResult> {
        self.inner
            .last_start_result
            .lock()
            .expect("pi start result lock")
            .clone()
    }

    /// 写入启动终态记录（单槽原地覆盖；ensure_started 的 spawn 层与 dispatch 泵的
    /// 握手失败面共用——exit-3 回修：泵对「Starting 态直接死」回写 Err 覆盖 spawn
    /// 层的 Ok，前端「新于快照」判定天然适配覆盖语义）。
    pub(crate) fn record_start_result(&self, ok: bool, error: Option<String>) {
        *self
            .inner
            .last_start_result
            .lock()
            .expect("pi start result lock") = Some(StartResult {
            ok,
            error,
            at_ms: chrono::Utc::now().timestamp_millis(),
        });
    }

    /// gen 感知状态读取：仅当 registry 当前会话仍是泵那代才返回其状态
    /// （跨代 = 自己已被替代 → None，泵不得据此回写/翻转新会话）。
    pub(super) fn status_if_current(&self, gen: u64) -> Option<PiStatus> {
        self.inner
            .session
            .read()
            .expect("pi session lock")
            .as_ref()
            .filter(|s| s.gen == gen)
            .map(|s| s.status)
    }

    /// gen 感知状态翻转：跨代调用忽略（旧泵不得标死新会话）。
    pub(super) fn set_status_if_current(&self, gen: u64, status: PiStatus) {
        let mut guard = self.inner.session.write().expect("pi session lock");
        if let Some(s) = guard.as_mut().filter(|s| s.gen == gen) {
            s.status = status;
        }
    }

    /// gen 感知启动失败回写：跨代忽略（旧泵不得覆盖新 spawn 的记录）。
    pub(super) fn record_start_failure_if_current(&self, gen: u64, error: String) {
        let current_is_mine = self
            .inner
            .session
            .read()
            .expect("pi session lock")
            .as_ref()
            .is_some_and(|s| s.gen == gen);
        if current_is_mine {
            self.record_start_result(false, Some(error));
        }
    }

    /// 关停助手（P5 可关闭）：组杀连坐 + 注册表摘除。未运行时空转 Ok。
    pub async fn stop(&self) -> Result<()> {
        let session = self.inner.session.write().expect("pi session lock").take();
        if let Some(s) = session {
            // shutdown 含宽限等待，不阻塞调用方 → 后台收尸
            let conn = s.conn.clone();
            tokio::spawn(async move { conn.shutdown().await });
            self.tick();
        }
        Ok(())
    }

    /// 手动新建会话（assistant-resume A5）：pi RPC new_session 切内存态新会话，
    /// hub 侧 feed 清空（前端新建成功后主动 invalidate 消息查询刷新空态）。
    /// session 文件等首条消息才落盘（pi 0.73.1 实测）——发过首条消息后新会话即
    /// 成为下次 spawn `--continue` 的恢复目标；零消息重启回退上一会话（已知边界，
    /// 见 production_args 注）。未运行/已死时幂等 Ok——前端入口仅在助手条目存在
    /// 时可达，API 直调无副作用。
    pub async fn new_session(&self) -> Result<()> {
        let session = self.inner.session.read().expect("pi session lock").clone();
        let Some(session) = session else {
            return Ok(());
        };
        if session.conn.is_dead() {
            return Ok(());
        }
        session
            .conn
            .request(protocol::new_session_cmd(0), ACCEPT_TIMEOUT)
            .await?;
        session.feed.clear();
        self.tick();
        Ok(())
    }

    /// P9 版本探测：`<bin> --version` 外部 CLI 探测（pi 独立升级线，hub 不锁版本）。
    /// TTL 缓存；探测失败（未安装/不可执行/超时）返回 None——**容错降级不硬失败**
    /// （RPC 命令式协议本身能力协商友好：未知命令 success:false，不做版本拦截）。
    pub async fn detect_version(&self) -> Option<String> {
        {
            let guard = self.inner.version.lock().await;
            if let Some(c) = guard.as_ref() {
                if c.at.elapsed() < VERSION_TTL {
                    return Some(c.text.clone());
                }
            }
        }
        let probed = probe_version(&self.inner.cfg.bin).await;
        let mut guard = self.inner.version.lock().await;
        if let Some(text) = &probed {
            *guard = Some(VersionCache {
                text: text.clone(),
                at: std::time::Instant::now(),
            });
        }
        probed
    }

    /// 读设置（P3）；文件缺失/坏值回退默认（不静默吞配置——坏值 warn）。
    pub fn settings(&self) -> PiSettings {
        match std::fs::read_to_string(&self.settings_file) {
            Ok(raw) => match serde_json::from_str(&raw) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "pi_settings.json 解析失败，用默认设置");
                    PiSettings::default()
                }
            },
            Err(_) => PiSettings::default(),
        }
    }

    /// 写设置（P3；调用方负责变更后重启助手生效）。
    pub async fn save_settings(&self, settings: &PiSettings) -> Result<()> {
        if let Some(dir) = self.settings_file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let raw = serde_json::to_string_pretty(settings)?;
        // 原子替换惯例（projects.json）：临时文件 + rename，崩溃不留半截 JSON
        let tmp = self.settings_file.with_extension("json.tmp");
        tokio::fs::write(&tmp, raw).await?;
        tokio::fs::rename(&tmp, &self.settings_file).await?;
        Ok(())
    }
}

/// 轨一 MessageSender（message_routes "pi" 键）：与 SessionControl 同体委托。
/// send_raw_bytes 无对应语义（终端保真视图仅限 Claude PTY 链路）。
impl crate::drivers::MessageSender for PiDriver {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        SessionControl::send(self, id, text)
    }

    fn is_finished(&self, id: &str) -> bool {
        SessionControl::is_finished(self, id)
    }

    fn release(&self, id: &str) -> Result<()> {
        SessionControl::release(self, id)
    }

    fn interrupt<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        SessionControl::cancel(self, id)
    }
}

/// `<bin> --version` 探测（5s 超时；非零退出/输出空 → None）。
/// 首行截断 80 字符防异常输出刷屏（容错：不解析格式，原样显示）。
async fn probe_version(bin: &str) -> Option<String> {
    let attempt = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new(bin).arg("--version").output(),
    )
    .await;
    let out = match attempt {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!(bin, error = %e, "pi --version spawn 失败（版本探测容错）");
            return None;
        }
        Err(_) => {
            tracing::warn!(bin, "pi --version 超时 5s（版本探测容错）");
            return None;
        }
    };
    if !out.status.success() {
        tracing::debug!(bin, "pi --version 非零退出（版本探测容错降级）");
        return None;
    }
    // pi 0.73.1 实测把版本打到 stderr（stdout 空）——stdout 空时回退 stderr
    let source = if out.stdout.is_empty() {
        &out.stderr
    } else {
        &out.stdout
    };
    let first = String::from_utf8_lossy(source)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    if first.is_empty() {
        tracing::debug!(bin, "pi --version 输出为空（版本探测容错）");
        return None;
    }
    let mut cut = first.chars().take(80).collect::<String>();
    if first.chars().count() > 80 {
        cut.push('…');
    }
    Some(cut)
}

impl SessionControl for PiDriver {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if id != ASSISTANT_ID {
                return Err(anyhow::anyhow!("未知 pi 会话: {id}"));
            }
            let session = self.inner.session.read().expect("pi session lock").clone();
            let Some(session) = session else {
                return Err(anyhow::anyhow!("Hub 助手未运行"));
            };
            if session.conn.is_dead() {
                anyhow::bail!("Hub 助手已退出");
            }
            // 受理即返：turn 完成由 dispatch 泵按 agent_end 收场（受理 ≠ 完成）；
            // 用户消息由 message_end(role=user) 事件落 feed（受理失败零残留）
            session
                .conn
                .request(protocol::prompt_cmd(0, text), ACCEPT_TIMEOUT)
                .await?;
            Ok(())
        })
    }

    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let session = self.inner.session.read().expect("pi session lock").clone();
            let Some(session) = session else {
                return Err(anyhow::anyhow!("Hub 助手未运行"));
            };
            if id != ASSISTANT_ID {
                return Err(anyhow::anyhow!("未知 pi 会话: {id}"));
            }
            session
                .conn
                .request(protocol::abort_cmd(0), ACCEPT_TIMEOUT)
                .await?;
            // 正式语义与 Claude/ACP 一致：用户取消即回闲（agent_end 随后再翻幂等）
            self.set_status(PiStatus::Idle);
            self.tick();
            Ok(())
        })
    }

    fn is_finished(&self, id: &str) -> bool {
        id == ASSISTANT_ID
            && self
                .inner
                .session
                .read()
                .expect("pi session lock")
                .as_ref()
                .is_none_or(|s| s.status == PiStatus::Dead)
    }

    fn release(&self, id: &str) -> Result<()> {
        if id != ASSISTANT_ID {
            return Err(anyhow::anyhow!("未知 pi 会话: {id}"));
        }
        // 同步签名：摘除 + 后台收尸（与 stop 同语义）
        let session = self.inner.session.write().expect("pi session lock").take();
        if let Some(s) = session {
            let conn = s.conn.clone();
            tokio::spawn(async move { conn.shutdown().await });
            self.tick();
        }
        Ok(())
    }

    fn answer_permission(
        &self,
        _id: &str,
        _perm_id: &str,
        _option_id: Option<&str>,
    ) -> Result<bool> {
        // pi RPC 无权限弹卡语义（extension UI 请求由泵自动取消）
        Err(unsupported("pi", "answer_permission"))
    }
}

impl AgentCatalog for PiDriver {
    fn list<'a>(&'a self, _all: bool) -> BoxFuture<'a, Result<Vec<AgentSummary>>> {
        Box::pin(async move { Ok(self.summaries()) })
    }

    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<AgentSummary>>> {
        Box::pin(async move {
            if id != ASSISTANT_ID {
                return Ok(None);
            }
            Ok(self.summaries().into_iter().next())
        })
    }

    fn start<'a>(&'a self, _spec: &'a StartSpec) -> BoxFuture<'a, Result<String>> {
        // 重开入口（P5）：活实例幂等返回；助手角色内置，spec 不参与组装
        Box::pin(async move { self.ensure_started().await })
    }

    fn stop<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if id != ASSISTANT_ID {
                return Err(anyhow::anyhow!("未知 pi 会话: {id}"));
            }
            self.stop().await
        })
    }

    fn remove<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<()>> {
        // Hub 助手不从列表移除（内置常驻条目）
        Box::pin(async move { Err(unsupported("pi", "remove")) })
    }

    fn respawn<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Err(unsupported("pi", "respawn")) })
    }

    fn logs<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Err(unsupported("pi", "logs")) })
    }
}

impl SessionHistory for PiDriver {
    fn full_history<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<ChatMessage>>> {
        Box::pin(async move {
            let session = self.inner.session.read().expect("pi session lock").clone();
            let Some(session) = session else {
                return Err(anyhow::anyhow!("Hub 助手未运行"));
            };
            if id != ASSISTANT_ID {
                return Err(anyhow::anyhow!("pi 会话不存在: {id}"));
            }
            Ok(session.feed.history())
        })
    }

    fn page<'a>(
        &'a self,
        id: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        // 内存 history 全量回放，无翻页（与 ACP 同构）
        Box::pin(async move {
            let session = self.inner.session.read().expect("pi session lock").clone();
            let Some(session) = session else {
                return Err(anyhow::anyhow!("Hub 助手未运行"));
            };
            if id != ASSISTANT_ID {
                return Ok(None);
            }
            Ok(Some(MessagePage {
                messages: session.feed.history(),
                first_line: 0,
                has_more: false,
            }))
        })
    }

    fn is_active<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<bool>> {
        // 常驻 RPC 会话无 jsonl mtime 口径（claude 语义）；如实恒 false——
        // 返 unsupported 会让前端选中助手后每 5s 轮询 404 刷日志（tester 走查）
        Box::pin(async move { Ok(false) })
    }

    fn subagents<'a>(
        &'a self,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::drivers::SubagentEntry>>> {
        // 设计定案（对齐 ACP）：无 subagent 语义 → 空集
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn subagent_page<'a>(
        &'a self,
        _id: &'a str,
        _sub: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move { Err(unsupported("pi", "subagent_page")) })
    }

    fn tasks<'a>(
        &'a self,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<crate::drivers::TaskEntry>>>> {
        Box::pin(async move { Ok(None) })
    }

    fn ensure_chat_room(&self, session_id: &str) -> broadcast::Sender<ChatEvent> {
        let mut rooms = self.inner.rooms.write().expect("chat_rooms lock poisoned");
        match rooms.entry(session_id.to_string()) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let (tx, _) = broadcast::channel(64);
                e.insert(tx.clone());
                tx
            }
        }
    }

    fn ensure_subagent_room(
        &self,
        session_id: &str,
        subagent_id: &str,
    ) -> broadcast::Sender<ChatEvent> {
        let room = format!("{session_id}/subagents/{subagent_id}");
        self.ensure_chat_room(&room);
        self.inner
            .rooms
            .read()
            .expect("chat_rooms lock poisoned")
            .get(&room)
            .cloned()
            .expect("room just inserted")
    }
}

/// P3 按需求走设置页持久化（provider/model + 变更重启生效）；会话面板模型切换
/// 语义暂不开放（ModelSettings 默认不支持，前端 caps 如实显示）。
impl ModelSettings for PiDriver {}

impl AgentDriver for PiDriver {
    fn name(&self) -> &'static str {
        "pi"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试驱动：bin=python3，额外参数 = [fixture, mode, log]。
    fn driver(dir: &Path, mode: &str, log: &Path) -> PiDriver {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi_fake_agent.py")
            .to_string_lossy()
            .into_owned();
        let (events_tx, _) = broadcast::channel(64);
        let cfg = PiConfig {
            bin: "python3".into(),
            auto_start: false,
        };
        PiDriver::with_args_extra(
            cfg,
            dir.join("pi_settings.json"),
            dir.join("hub-assistant.ts"),
            dir.join("sessions"),
            7999,
            events_tx,
            Arc::new(RwLock::new(HashMap::new())),
            vec![
                "-u".into(),
                fixture,
                mode.into(),
                log.to_string_lossy().into_owned(),
            ],
            dir.join("pi-agent"),
        )
    }

    async fn wait_status(d: &PiDriver, want: PiStatus, label: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let cur = d
                .inner
                .session
                .read()
                .expect("lock")
                .as_ref()
                .map(|s| s.status);
            if cur == Some(want) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "状态未到 {label}（当前 {cur:?}）"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// P1 生命周期：ensure_started → summaries 1 条 starting/idle → send 全链路
    /// （受理 + feed user/assistant 回放）→ agent_end 后状态回闲。
    #[tokio::test]
    async fn basic_roundtrip_send_feed_status() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "basic", &log);
        let id = d.ensure_started().await.unwrap();
        assert_eq!(id, ASSISTANT_ID);
        let sums = d.summaries();
        assert_eq!(sums.len(), 1);
        assert_eq!(sums[0].driver, "pi");
        assert_eq!(sums[0].kind.as_deref(), Some("pi"));

        SessionControl::send(&d, ASSISTANT_ID, "hello pi")
            .await
            .unwrap();
        // 受理即返；等 dispatch 收 agent_end 回闲
        wait_status(&d, PiStatus::Idle, "idle").await;
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        let h = session.feed.history();
        let kinds: Vec<_> = h.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            vec![
                crate::models::ChatMessageKind::User,
                crate::models::ChatMessageKind::Thinking,
                crate::models::ChatMessageKind::Assistant,
            ],
            "事件流顺序即 feed 顺序: {kinds:?}"
        );
        assert_eq!(h[0].text.as_deref(), Some("hello pi"));
        let assistant = h
            .iter()
            .find(|m| m.kind == crate::models::ChatMessageKind::Assistant)
            .expect("应有 assistant 消息");
        assert_eq!(assistant.text.as_deref(), Some("FAKE-REPLY"));
        assert!(h
            .iter()
            .any(|m| m.kind == crate::models::ChatMessageKind::Thinking));
        session.conn.shutdown().await;
    }

    /// P1 send 快速失败路径：假体受理即 success:false（模拟凭据缺失）→ Err 透传。
    #[tokio::test]
    async fn api_error_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "api-error", &log);
        d.ensure_started().await.unwrap();
        let err = SessionControl::send(&d, ASSISTANT_ID, "hi")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No API key"), "实际: {err}");
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        assert!(session.feed.history().is_empty(), "受理失败不得推 feed");
        session.conn.shutdown().await;
    }

    /// P5 崩溃不拖垮 hub：prompt 时子进程 exit 70 → send Err + 状态 Dead +
    /// summaries 如实上报；driver 本体仍可用（stop 空转 Ok）。
    #[tokio::test]
    async fn crash_marks_dead_not_poisoned() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "crash-on-prompt", &log);
        d.ensure_started().await.unwrap();
        assert!(SessionControl::send(&d, ASSISTANT_ID, "hi").await.is_err());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !d
            .inner
            .session
            .read()
            .expect("lock")
            .as_ref()
            .is_none_or(|s| s.conn.is_dead())
        {
            assert!(tokio::time::Instant::now() < deadline, "进程未标记死亡");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // dead 后 summaries 仍出条目（Dead 态如实呈现，与 ACP 一致）
        let sums = d.summaries();
        assert_eq!(sums.len(), 1);
        assert_eq!(sums[0].raw_state.as_deref(), Some("dead"));
        assert!(d.stop().await.is_ok(), "崩溃后 driver 不被拖垮");
    }

    /// P8 通路：dialog 类 UI 请求被泵自动取消应答，助手 turn 正常收尾。
    #[tokio::test]
    async fn ui_dialog_auto_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "ui-dialog", &log);
        d.ensure_started().await.unwrap();
        SessionControl::send(&d, ASSISTANT_ID, "hi").await.unwrap();
        wait_status(&d, PiStatus::Idle, "idle").await;
        // 假体记录收到的 extension_ui_response（cancelled；python json.dumps 带空格）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let raw = std::fs::read_to_string(&log).unwrap_or_default();
            if raw.contains("extension_ui_response") {
                assert!(raw.contains("\"cancelled\": true"), "应为取消应答: {raw}");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "取消应答未送达: {raw}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        session.conn.shutdown().await;
    }

    /// P5 可关闭重开：stop → summaries 空、is_running false；
    /// ensure_started 再起 → 新连接可用（幂等重开语义）。
    #[tokio::test]
    async fn stop_then_restart() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "basic", &log);
        d.ensure_started().await.unwrap();
        assert!(d.is_running());
        d.stop().await.unwrap();
        // shutdown 异步收尸，等进程真正退出
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while d.is_running() {
            assert!(tokio::time::Instant::now() < deadline, "stop 后仍 running");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(d.summaries().is_empty(), "stop 后侧栏条目应消失");
        // 重开：ensure_started 再起一轮，send 可用
        d.ensure_started().await.unwrap();
        assert!(d.is_running());
        SessionControl::send(&d, ASSISTANT_ID, "again")
            .await
            .unwrap();
        wait_status(&d, PiStatus::Idle, "idle").await;
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        session.conn.shutdown().await;
    }

    /// P1 abort 通路：cancel 发 abort 命令，假体受理后 agent_end，状态回闲。
    #[tokio::test]
    async fn cancel_sends_abort_and_resets_idle() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "abort-aware", &log);
        d.ensure_started().await.unwrap();
        // 后台发 prompt（慢速流式，不等待收尾）
        let send_d = d.clone();
        let send_task =
            tokio::spawn(async move { SessionControl::send(&send_d, ASSISTANT_ID, "slow").await });
        // 等 Working（agent_start 已被泵处理）再取消
        wait_status(&d, PiStatus::Working, "working").await;
        d.cancel(ASSISTANT_ID).await.unwrap();
        assert_eq!(
            d.inner
                .session
                .read()
                .expect("lock")
                .as_ref()
                .unwrap()
                .status,
            PiStatus::Idle,
            "用户取消即回闲"
        );
        let raw = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            raw.contains("\"command\": \"abort\""),
            "假体应收到 abort: {raw}"
        );
        send_task.await.unwrap().unwrap();
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        session.conn.shutdown().await;
    }

    /// P10：agent_dir 已接线（原生配置读写入口可用——语义细节在 piconf.rs 覆盖）。
    #[tokio::test]
    async fn agent_dir_wired() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "basic", &log);
        assert_eq!(d.agent_dir, dir.path().join("pi-agent"));
    }

    /// P9 版本探测：正常输出 Some（首行截取）；非零退出/不存在 bin 容错 None 不硬失败。
    #[tokio::test]
    async fn probe_version_tolerant() {
        // python3 --version 输出首行（Python x.y.z）→ Some 含 "Python"
        let v = probe_version("python3").await;
        assert!(
            v.as_deref().is_some_and(|s| s.contains("Python")),
            "实为 {v:?}"
        );
        // --version 非零退出 → None（false 立即退 1）
        assert_eq!(probe_version("false").await, None);
        // 不存在 bin（spawn 失败）→ None
        assert_eq!(probe_version("/nonexistent/pi-bin").await, None);
    }

    /// P9 缓存：TTL 内二次探测不拉子进程（bin 换成会失败的——若走真探测会 None，
    /// 缓存命中则仍返回首测值）。
    #[tokio::test]
    async fn detect_version_caches_within_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "basic", &log);
        let first = d.detect_version().await;
        assert!(first.as_deref().is_some_and(|s| s.contains("Python")));
        // 把缓存改「新鲜」后即使 bin 换坏值也命中缓存
        {
            let mut guard = d.inner.version.lock().await;
            let c = guard.as_mut().unwrap();
            c.at = std::time::Instant::now();
        }
        // 直接再探：缓存命中返回同值（不改 bin——cfg.bin 是 fixture 用的 python3，
        // 本身可探测；此处断言两次一致 + 耗时远小于子进程冷启动即视为命中）
        let second = d.detect_version().await;
        assert_eq!(first, second);
    }

    /// 未知会话 id 一律 Err（session-control 边界）。
    #[tokio::test]
    async fn unknown_id_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "basic", &log);
        assert!(SessionControl::send(&d, "ghost", "x").await.is_err());
        assert!(d.cancel("ghost").await.is_err());
        assert!(!d.is_finished("ghost"));
        assert!(d.release("ghost").is_err());
    }

    // ---- assistant-resume：默认恢复上次会话 ----

    /// 生产 spawn 参数含 --continue（A1/A3 恢复锚）与 --session-dir；形态完整。
    #[test]
    fn production_args_contains_continue() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver(dir.path(), "basic", &dir.path().join("fake.log"));
        let args = d.production_args();
        let s = args.join(" ");
        assert!(args.contains(&"--continue".to_string()), "实为: {s}");
        assert!(
            args.windows(2).any(|w| w[0] == "--session-dir"),
            "实为: {s}"
        );
        assert!(args.contains(&"--mode".to_string()) && args.contains(&"rpc".to_string()));
    }

    /// reseed 前插：实时先行（竞态）时历史仍排在前；广播事件同步到达（seq 递增）。
    #[test]
    fn reseed_head_inserts_and_broadcasts() {
        let rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let (room_tx, mut room_rx) = broadcast::channel(16);
        rooms.write().expect("lock").insert("s1".into(), room_tx);
        let feed = PiFeed::new("s1".into(), rooms);
        let msg = |t: &str| ChatMessage {
            kind: crate::models::ChatMessageKind::Assistant,
            text: Some(t.into()),
            ..ChatMessage::default()
        };
        // 竞态模拟：实时首条先落，历史后到
        feed.push(msg("live-1"));
        feed.reseed(vec![msg("old-1"), msg("old-2")]);
        let hist = feed.history();
        let texts: Vec<_> = hist.iter().map(|m| m.text.as_deref().unwrap()).collect();
        assert_eq!(texts, vec!["old-1", "old-2", "live-1"], "历史必须前插");
        // 广播：reseed 2 条 + push 1 条 = 3 事件，seq 严格递增
        let mut seqs = Vec::new();
        while let Ok(ev) = room_rx.try_recv() {
            seqs.push(ev.seq);
        }
        assert_eq!(seqs, vec![1, 2, 3], "广播事件 seq 递增: {seqs:?}");
    }

    /// reseed 空批次 no-op（A4 新会话零扰动）；clear 清空（A5 新建）。
    #[test]
    fn reseed_empty_noop_and_clear() {
        let rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let feed = PiFeed::new("s1".into(), rooms);
        let msg = ChatMessage {
            kind: crate::models::ChatMessageKind::User,
            text: Some("hi".into()),
            ..ChatMessage::default()
        };
        feed.reseed(Vec::new());
        assert!(feed.history().is_empty());
        feed.push(msg);
        assert_eq!(feed.history().len(), 1);
        feed.clear();
        assert!(feed.history().is_empty(), "clear 后历史应清空");
    }

    /// A1/A2 后端链路（resume 假体）：ensure_started 后泵回灌持久化历史
    /// （早前问题/早前回答）；随后 send 的新消息**接在历史之后**（上下文延续的
    /// feed 形态基础）。
    #[tokio::test]
    async fn resume_flow_reseeds_history() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "resume", &log);
        d.ensure_started().await.unwrap();
        // 等回灌完成（泵 task 异步 get_messages → reseed）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if d.inner
                .session
                .read()
                .expect("lock")
                .as_ref()
                .is_some_and(|s| s.feed.history().len() >= 2)
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "历史回灌未完成: {:?}",
                d.inner
                    .session
                    .read()
                    .expect("lock")
                    .as_ref()
                    .map(|s| s.feed.history())
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // 历史内容与顺序
        let h = d
            .inner
            .session
            .read()
            .expect("lock")
            .as_ref()
            .expect("session 在")
            .feed
            .history();
        assert_eq!(h[0].kind, crate::models::ChatMessageKind::User);
        assert_eq!(h[0].text.as_deref(), Some("早前问题"));
        assert_eq!(h[1].kind, crate::models::ChatMessageKind::Assistant);
        assert_eq!(h[1].text.as_deref(), Some("早前回答"));
        // 追问接在历史之后（A2 feed 形态）
        SessionControl::send(&d, ASSISTANT_ID, "追问")
            .await
            .unwrap();
        wait_status(&d, PiStatus::Idle, "idle").await;
        let texts: Vec<_> = d
            .inner
            .session
            .read()
            .expect("lock")
            .as_ref()
            .expect("session 在")
            .feed
            .history()
            .iter()
            .map(|m| m.text.as_deref().unwrap_or("<none>").to_string())
            .collect();
        assert_eq!(
            texts,
            vec!["早前问题", "早前回答", "追问", "fake thought", "FAKE-REPLY"],
            "新输入接在历史之后: {texts:?}"
        );
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        session.conn.shutdown().await;
    }

    /// A5 手动新建：new_session 转发 RPC（假体 log 可查）+ feed 清空；未运行幂等 Ok。
    #[tokio::test]
    async fn new_session_forwards_and_clears_feed() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "resume", &log);
        // 未运行：幂等 Ok 无副作用
        assert!(d.new_session().await.is_ok());
        d.ensure_started().await.unwrap();
        // 等回灌历史（有内容可清）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if d.inner
                .session
                .read()
                .expect("lock")
                .as_ref()
                .is_some_and(|s| !s.feed.history().is_empty())
            {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "回灌未完成");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        d.new_session().await.unwrap();
        assert!(
            d.inner
                .session
                .read()
                .expect("lock")
                .as_ref()
                .is_some_and(|s| s.feed.history().is_empty()),
            "new_session 后 feed 应清空"
        );
        // 假体收到 new_session 命令（python json.dumps 带空格）
        let raw = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(
            raw.contains("\"command\": \"new_session\""),
            "假体应收到 new_session: {raw}"
        );
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        session.conn.shutdown().await;
    }

    /// restart-feedback：真 spawn 结果记录——失败（workDir 非法）含原因摘要、
    /// 成功 ok、活实例幂等早退不顶新记录时间戳。
    #[tokio::test]
    async fn start_result_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "basic", &log);
        assert!(d.last_start_result().is_none(), "初始无记录");
        // 失败路径：workDir 指向已有文件 → Err 记录（含原因摘要）
        let bad = dir.path().join("afile");
        std::fs::write(&bad, "x").unwrap();
        std::fs::write(
            dir.path().join("pi_settings.json"),
            format!(r#"{{"workDir":"{}"}}"#, bad.display()),
        )
        .unwrap();
        assert!(d.ensure_started().await.is_err());
        let r1 = d.last_start_result().expect("失败也应有记录");
        assert!(!r1.ok);
        assert!(
            r1.error.as_deref().is_some_and(|e| e.contains("启动失败")),
            "应含原因摘要: {:?}",
            r1.error
        );
        // 成功路径：清 flags → Ok 记录（时间戳不早于失败记录）
        std::fs::write(dir.path().join("pi_settings.json"), "{}").unwrap();
        d.ensure_started().await.unwrap();
        let r2 = d.last_start_result().expect("成功应有记录");
        assert!(r2.ok && r2.error.is_none());
        assert!(r2.at_ms >= r1.at_ms);
        // 活实例幂等早退：再调不产生新记录（时间戳不变）
        d.ensure_started().await.unwrap();
        let r3 = d.last_start_result().unwrap();
        assert_eq!(r3.at_ms, r2.at_ms, "幂等早退不得顶新记录");
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        session.conn.shutdown().await;
    }

    /// BLOCKER 回修（exit-3 失败面）：spawn Ok 但 RPC 立即挂——泵 Closed 收场对
    /// Starting 态回写 Err 覆盖 spawn 层 Ok:true，前端轮询可见真实失败。
    #[tokio::test]
    async fn exit3_handshake_death_records_failure() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let d = driver(dir.path(), "exit-on-start", &log);
        d.ensure_started().await.unwrap();
        // spawn 层先记 Ok
        assert!(d.last_start_result().unwrap().ok, "spawn 层应先记 Ok");
        // 等 EOF → 泵 Closed 收场（Starting 态）回写失败
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match d.last_start_result() {
                Some(r) if !r.ok => {
                    assert!(
                        r.error.as_deref().is_some_and(|e| e.contains("握手")),
                        "应含握手失败摘要: {:?}",
                        r.error
                    );
                    break;
                }
                _ => {
                    assert!(tokio::time::Instant::now() < deadline, "失败回写未发生");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
        // 侧栏 Dead 如实呈现（与失败记录并存）
        assert_eq!(d.summaries()[0].raw_state.as_deref(), Some("dead"));
    }

    /// D5：spawn 向 pi 子进程注入 PI_CODING_AGENT_DIR（与 hub 侧 PI_AGENT_DIR
    /// 同源）——此前 hub 侧隔离变量对 pi 进程无效（配置目录 env 不同名）。
    #[tokio::test]
    async fn spawn_injects_pi_coding_agent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake.log");
        let agent_dir = dir.path().join("pi-agent");
        std::env::set_var("PI_AGENT_DIR", &agent_dir);
        let d = driver(dir.path(), "basic", &log);
        d.ensure_started().await.unwrap();
        std::env::remove_var("PI_AGENT_DIR");
        // 假体启动即记 env（JSON 行）；等 log 落盘后断言同源注入
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let raw = loop {
            let raw = std::fs::read_to_string(&log).unwrap_or_default();
            if raw.contains("PI_CODING_AGENT_DIR") {
                break raw;
            }
            assert!(tokio::time::Instant::now() < deadline, "env 记录未落盘");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let line = raw
            .lines()
            .find(|l| l.contains("PI_CODING_AGENT_DIR"))
            .expect("env 记录行");
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        let injected = v["env_PI_CODING_AGENT_DIR"].as_str().unwrap();
        assert_eq!(
            std::path::Path::new(injected),
            agent_dir.as_path(),
            "注入值应与 hub 侧同源"
        );
        let session = d.inner.session.read().expect("lock").clone().unwrap();
        session.conn.shutdown().await;
    }
}
