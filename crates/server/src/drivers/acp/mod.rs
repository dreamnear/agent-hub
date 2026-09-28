//! ACP driver（agent-hub-acp-omp 批1）：子进程 spawn + JSON-RPC/stdio 传输 +
//! initialize/session 生命周期 + 会话注册表。
//! 与 Claude PTY 链路零共享：独立 tokio 任务、独立 stdio 管道，不占用 attach 线程。

pub mod dispatch;
pub mod protocol;
pub mod registry;

pub use registry::{AcpRegistry, AcpSession, AcpStatus};

use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, RwLock,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{broadcast, oneshot, Mutex as AsyncMutex},
};

use crate::config::AcpAgentConfig;

/// initialize 握手超时（omp 实测 <1s）
pub const INIT_TIMEOUT: Duration = Duration::from_secs(30);
/// session/new 超时（omp 首次建会话含本地 provider discovery，实测 ~38s，放宽留余量）
pub const SESSION_NEW_TIMEOUT: Duration = Duration::from_secs(150);
/// 单条 prompt 超时（真模型调用，取宽松值）
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(600);
/// 权限请求等待用户应答上限（批3 任务11）：超时按拒绝收尾（红线：不默认放行）
pub const PERMISSION_TIMEOUT: Duration = PROMPT_TIMEOUT;
/// shutdown SIGTERM 后的宽限期，超时 SIGKILL 连坐进程组
const KILL_GRACE: Duration = Duration::from_secs(5);

/// agent → hub 的入站事件（任务2：通知分发 + 反向请求接收 + 退出标记）。
#[derive(Debug, Clone)]
pub enum Inbound {
    Notification {
        method: String,
        params: serde_json::Value,
    },
    ReverseRequest {
        id: serde_json::Value,
        method: String,
        params: serde_json::Value,
    },
    /// 子进程退出（EOF），pending 请求已全部失败
    Closed,
}

/// 共享写路径与 pending 表（reader 任务与对外 API 两头用）。
struct Shared {
    stdin: AsyncMutex<tokio::process::ChildStdin>,
    pending: Mutex<
        std::collections::HashMap<
            serde_json::Value,
            oneshot::Sender<Result<serde_json::Value, String>>,
        >,
    >,
    inbound_tx: broadcast::Sender<Inbound>,
    dead: AtomicBool,
    child: AsyncMutex<Option<tokio::process::Child>>,
}

/// 一条 ACP agent 子进程连接。Clone = Arc 克隆（注册表/路由共享同一连接）。
#[derive(Clone)]
pub struct AcpConnection {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for AcpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpConnection")
            .field("dead", &self.shared.dead.load(Ordering::SeqCst))
            .finish()
    }
}

impl AcpConnection {
    /// spawn agent 子进程并启动 reader。独立进程组（unix），shutdown 连坐清除。
    pub async fn spawn(
        agent: &AcpAgentConfig,
        cwd: &Path,
    ) -> Result<(Self, broadcast::Receiver<Inbound>)> {
        let mut cmd = tokio::process::Command::new(&agent.command);
        cmd.args(&agent.args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 安全模板 §6：独立进程组 → killpg 连坐（agent 自己 fork 的子进程一并清除）
            .process_group(0);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("ACP agent 启动失败: {} {:?}", agent.command, agent.args))?;
        let stdin = child.stdin.take().context("stdin 未管道化")?;
        let stdout = child.stdout.take().context("stdout 未管道化")?;
        let stderr = child.stderr.take().context("stderr 未管道化")?;

        let (inbound_tx, inbound_rx) = broadcast::channel(256);
        let shared = Arc::new(Shared {
            stdin: AsyncMutex::new(stdin),
            pending: Mutex::new(std::collections::HashMap::new()),
            inbound_tx,
            dead: AtomicBool::new(false),
            child: AsyncMutex::new(Some(child)),
        });

        // stderr 转结构化日志（agent 诊断信息，不参与协议）
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "acp_agent_stderr", "{line}");
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
                    Ok(protocol::ParsedMessage::Response(resp)) => {
                        let tx = shared
                            .pending
                            .lock()
                            .expect("pending lock")
                            .remove(&resp.id);
                        match tx {
                            Some(tx) => {
                                let _ = tx.send(match resp.error {
                                    Some(err) => {
                                        Err(format!("rpc error {}: {}", err.code, err.message))
                                    }
                                    None => Ok(resp.result.unwrap_or(serde_json::Value::Null)),
                                });
                            }
                            None => {
                                tracing::debug!(target: "acp", id = %resp.id, "无待关联的响应（可能已超时）")
                            }
                        }
                    }
                    Ok(protocol::ParsedMessage::Notification(incoming)) => {
                        let _ = shared.inbound_tx.send(Inbound::Notification {
                            method: incoming.method,
                            params: incoming.params,
                        });
                    }
                    Ok(protocol::ParsedMessage::ReverseRequest(incoming)) => {
                        let _ = shared.inbound_tx.send(Inbound::ReverseRequest {
                            id: incoming.id.unwrap_or(serde_json::Value::Null),
                            method: incoming.method,
                            params: incoming.params,
                        });
                    }
                    Ok(protocol::ParsedMessage::Malformed) => {
                        tracing::debug!(target: "acp", "忽略非 JSON-RPC 帧")
                    }
                    Err(e) => tracing::warn!(target: "acp", error = %e, "坏帧解析失败"),
                }
            }
            // EOF：子进程退出 → 标记死亡、清空 pending、广播 Closed
            shared.dead.store(true, Ordering::SeqCst);
            let waiters: Vec<oneshot::Sender<Result<serde_json::Value, String>>> = shared
                .pending
                .lock()
                .expect("pending lock")
                .drain()
                .map(|(_, tx)| tx)
                .collect();
            for tx in waiters {
                let _ = tx.send(Err("ACP 连接已关闭".into()));
            }
            let _ = shared.inbound_tx.send(Inbound::Closed);
            // 连坐收尸：进程自然退出后补 killpg（清 agent 自己 fork 的残留）+ reap 防 zombie
            reap(&shared, KILL_GRACE).await;
        });
    }

    fn next_id(&self) -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    async fn write_line(&self, line: String) -> Result<()> {
        if self.shared.dead.load(Ordering::SeqCst) {
            anyhow::bail!("ACP 连接已关闭");
        }
        let mut stdin = self.shared.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// 请求-响应：分配 id、注册 oneshot、写帧、限时等待。
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let id = self.next_id();
        let (tx, rx) = oneshot::channel();
        self.shared
            .pending
            .lock()
            .expect("pending lock")
            .insert(serde_json::json!(id), tx);
        let line = serde_json::to_string(&protocol::RpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        })?;
        if let Err(e) = self.write_line(line).await {
            self.shared
                .pending
                .lock()
                .expect("pending lock")
                .remove(&serde_json::json!(id));
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(msg))) => anyhow::bail!("{msg}"),
            Ok(Err(_)) => anyhow::bail!("ACP 连接已关闭（请求未应答）"),
            Err(_) => {
                self.shared
                    .pending
                    .lock()
                    .expect("pending lock")
                    .remove(&serde_json::json!(id));
                anyhow::bail!("ACP 请求超时: {method}（{}s）", timeout.as_secs())
            }
        }
    }

    /// 出站通知（session/cancel 等，无应答）。
    pub async fn notify(&self, method: &str, params: serde_json::Value) -> Result<()> {
        let line = serde_json::to_string(&protocol::RpcNotification {
            jsonrpc: "2.0",
            method,
            params,
        })?;
        self.write_line(line).await
    }

    /// 应答反向请求（批1：权限占位拒绝）。
    pub async fn respond(&self, id: &serde_json::Value, result: &serde_json::Value) -> Result<()> {
        let line = serde_json::to_string(&protocol::RpcResult {
            jsonrpc: "2.0",
            id,
            result,
        })?;
        self.write_line(line).await
    }

    /// 反向请求错误应答（不支持的 fs/terminal 等方法）。
    pub async fn respond_error(
        &self,
        id: &serde_json::Value,
        code: i64,
        message: &str,
    ) -> Result<()> {
        let line = serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }))?;
        self.write_line(line).await
    }

    pub fn is_dead(&self) -> bool {
        self.shared.dead.load(Ordering::SeqCst)
    }

    /// agent 子进程 pid（shutdown 前定位孙进程做连坐验证用；已关停返回 None）。
    pub async fn child_pid(&self) -> Option<u32> {
        self.shared.child.lock().await.as_ref().and_then(|c| c.id())
    }

    /// 主动关停：SIGTERM 进程组 → 宽限 → SIGKILL 连坐；幂等。
    pub async fn shutdown(&self) {
        reap(&self.shared, KILL_GRACE).await;
    }
}

/// 进程组连坐清除 + reap：SIGTERM → KILL_GRACE → SIGKILL。幂等（child 取出后二次调用空转）。
async fn reap(shared: &Shared, grace: Duration) {
    let mut guard = shared.child.lock().await;
    let Some(child) = guard.as_mut() else {
        return;
    };
    if let Some(pid) = child.id() {
        // SAFETY: killpg 传一个已存在进程组 id；errno 忽略（组可能已退出）
        unsafe {
            libc::killpg(pid as i32, libc::SIGTERM);
        }
    }
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(_) => {}
        Err(_) => {
            if let Some(pid) = child.id() {
                // SAFETY: 同上，宽限期满强制连坐
                unsafe {
                    libc::killpg(pid as i32, libc::SIGKILL);
                }
                let _ = child.wait().await;
            }
        }
    }
    *guard = None;
    shared.dead.store(true, Ordering::SeqCst);
}

/// MessageSender 的 ACP 语义实现（批1 任务4）：send = session/prompt（含状态翻转），
/// interrupt = session/cancel 通知，release = 关停 + 注册表摘除。
/// send_raw_bytes 无对应协议语义（终端保真视图仅限 Claude PTY 链路）。
#[derive(Clone)]
pub struct AcpSendRouter {
    registry: Arc<AcpRegistry>,
    /// 批2 任务8：prompt 起止广播 Tick → 前端侧栏/工作态即时翻转（免轮询等待）
    events_tx: tokio::sync::broadcast::Sender<crate::drivers::claude::watcher::HubEvent>,
}

impl AcpSendRouter {
    pub fn new(
        registry: Arc<AcpRegistry>,
        events_tx: tokio::sync::broadcast::Sender<crate::drivers::claude::watcher::HubEvent>,
    ) -> Self {
        Self {
            registry,
            events_tx,
        }
    }

    fn tick(&self) {
        let _ = self
            .events_tx
            .send(crate::drivers::claude::watcher::HubEvent::Tick);
    }
}

/// F1 状态守卫（批3 任务12 顺带修复）：prompt future 中途被弃（客户端断开 →
/// axum handler task 被 cancel）时，正常收尾代码不执行，rawState 永久滞留 working。
/// Drop 兜底把状态拉回 Idle/Dead + Tick。正常收尾先 disarm 再回闲，Drop 空转。
struct PromptStatusGuard {
    registry: Arc<AcpRegistry>,
    id: String,
    events_tx: tokio::sync::broadcast::Sender<crate::drivers::claude::watcher::HubEvent>,
    armed: bool,
}

impl PromptStatusGuard {
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for PromptStatusGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // O1：prompt future 已死，清存活标志（dispatch 应答收场据此不再翻 Working）
        self.registry.set_prompt_alive(&self.id, false);
        let dead = self
            .registry
            .get(&self.id)
            .is_none_or(|s| s.status == AcpStatus::Dead || s.conn.is_dead());
        self.registry.set_status(
            &self.id,
            if dead {
                AcpStatus::Dead
            } else {
                AcpStatus::Idle
            },
        );
        let _ = self
            .events_tx
            .send(crate::drivers::claude::watcher::HubEvent::Tick);
        tracing::warn!(session_id = %self.id, "prompt future 中途被弃，状态守卫回闲（F1）");
    }
}

impl super::MessageSender for AcpSendRouter {
    fn send<'a>(
        &'a self,
        id: &'a str,
        text: &'a str,
    ) -> futures::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let session = self
                .registry
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("未知 ACP 会话: {id}"))?;
            if session.status == AcpStatus::Dead || session.conn.is_dead() {
                anyhow::bail!("ACP 会话已结束: {id}");
            }
            self.registry.set_status(id, AcpStatus::Working);
            self.registry.set_prompt_alive(id, true);
            self.tick();
            let guard = PromptStatusGuard {
                registry: Arc::clone(&self.registry),
                id: id.to_string(),
                events_tx: self.events_tx.clone(),
                armed: true,
            };
            let result = session
                .conn
                .request(
                    "session/prompt",
                    serde_json::json!({
                        "sessionId": id,
                        "prompt": [{"type": "text", "text": text}],
                    }),
                    PROMPT_TIMEOUT,
                )
                .await;
            // 收尾状态：连接死了置 Dead，否则回 Idle；清 prompt 存活标志（O1）
            self.registry.set_prompt_alive(id, false);
            let dead = session.conn.is_dead();
            self.registry.set_status(
                id,
                if dead {
                    AcpStatus::Dead
                } else {
                    AcpStatus::Idle
                },
            );
            // r77：会话死亡时唤醒所有挂起权限（dispatch 各自出回执收场，
            // 不等 PERMISSION_TIMEOUT；泵被 dispatch 阻塞，收不到 Closed）
            if dead {
                self.registry.resolve_session_perms(id);
            }
            self.tick();
            guard.disarm();
            result.map(|_| ())
        })
    }

    fn is_finished(&self, id: &str) -> bool {
        self.registry
            .get(id)
            .is_none_or(|s| s.status == AcpStatus::Dead)
    }

    fn release(&self, id: &str) -> Result<()> {
        if let Some(session) = self.registry.remove(id) {
            // shutdown 含宽限等待，不阻塞同步签名 → 后台收尸
            tokio::spawn(async move { session.conn.shutdown().await });
        }
        // 批3 任务13：移除同步清持久化条目
        self.registry.persist_async();
        Ok(())
    }

    fn send_raw_bytes(&self, _id: &str, _data: &[u8]) -> Result<()> {
        anyhow::bail!("ACP 会话不支持终端原始字节透传")
    }

    fn interrupt<'a>(&'a self, id: &'a str) -> futures::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let session = self
                .registry
                .get(id)
                .ok_or_else(|| anyhow::anyhow!("未知 ACP 会话: {id}"))?;
            session
                .conn
                .notify("session/cancel", serde_json::json!({"sessionId": id}))
                .await?;
            // 批3 任务12 正式语义：与 Claude 中断一致——用户取消即回闲
            // （agent 随后的 prompt 应答 stopReason=cancelled 仍会走 send 收尾，幂等）
            self.registry.set_status(id, AcpStatus::Idle);
            // r77：取消即唤醒挂起权限弹卡（dispatch 收到取消语义出回执收场，
            // 前端弹卡同步消失，不留挂着可点的卡）
            self.registry.resolve_session_perms(id);
            self.tick();
            Ok(())
        })
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// 生命周期门面（批1 任务3）：spawn → initialize 握手 → session/new。
/// agents 带内部可写：harness 面板「加入配置」后无需重启即可用（settings 批A 需求5）。
#[derive(Debug, Clone)]
pub struct AcpDriver {
    pub agents: Arc<RwLock<Vec<AcpAgentConfig>>>,
}

impl AcpDriver {
    pub fn from_config(acp: &crate::config::AcpConfig) -> Self {
        Self {
            agents: Arc::new(RwLock::new(acp.agents.clone())),
        }
    }

    pub fn agent(&self, name: &str) -> Option<AcpAgentConfig> {
        self.agents
            .read()
            .expect("acp agents lock")
            .iter()
            .find(|a| a.name == name)
            .cloned()
    }

    /// 清单快照（锁内克隆即出，不跨 await）
    pub fn agents(&self) -> Vec<AcpAgentConfig> {
        self.agents.read().expect("acp agents lock").clone()
    }

    /// 运行时追加（调用方负责先落盘 config.toml）
    pub fn add_agent(&self, agent: AcpAgentConfig) {
        self.agents.write().expect("acp agents lock").push(agent);
    }

    /// spawn + initialize 握手（版本协商锁定 v1）——start/resume 共用前缀。
    async fn connect_initialized(
        &self,
        agent: &AcpAgentConfig,
        cwd: &Path,
    ) -> Result<(AcpConnection, broadcast::Receiver<Inbound>)> {
        let (conn, rx) = AcpConnection::spawn(agent, cwd).await?;
        let init_value = match conn
            .request(
                "initialize",
                serde_json::json!({
                    "protocolVersion": protocol::PROTOCOL_VERSION,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "agent-hub", "version": env!("CARGO_PKG_VERSION")},
                }),
                INIT_TIMEOUT,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                conn.shutdown().await;
                return Err(e);
            }
        };
        let init: protocol::InitializeResult = serde_json::from_value(init_value)?;
        match init.protocol_version {
            Some(v) if v == protocol::PROTOCOL_VERSION => {}
            other => {
                conn.shutdown().await;
                anyhow::bail!(
                    "ACP 协议版本协商失败: agent 支持 {other:?}，hub 锁定 v{}",
                    protocol::PROTOCOL_VERSION
                );
            }
        }
        Ok((conn, rx))
    }

    /// 完整建会话：spawn + initialize（版本协商，锁定 v1）+ session/new（cwd 必填）。
    /// clientCapabilities 留空：不声明 fs/terminal 能力，本地文件访问由 agent 自管（需求约定）。
    /// model 走 session/new params（批2 任务7，agent 不支持时宽松忽略）。
    /// rooms：chat 房间表（批2 任务8），会话 feed 挂其上做 history + 房间广播。
    pub async fn start_session(
        &self,
        agent_name: &str,
        cwd: &Path,
        model: Option<String>,
        rooms: std::sync::Arc<
            std::sync::RwLock<
                HashMap<
                    String,
                    tokio::sync::broadcast::Sender<crate::drivers::claude::session::ChatEvent>,
                >,
            >,
        >,
    ) -> Result<(AcpSession, broadcast::Receiver<Inbound>)> {
        let agent = self
            .agent(agent_name)
            .ok_or_else(|| anyhow::anyhow!("未知 ACP agent: {agent_name}"))?;
        let (conn, rx) = self.connect_initialized(&agent, cwd).await?;
        // model 走 session/new params（批2 任务7）：omp 宽松解析，不支持时忽略（best-effort）
        let mut new_params = serde_json::json!({"cwd": cwd.to_string_lossy(), "mcpServers": []});
        if let Some(m) = &model {
            new_params["model"] = serde_json::json!(m);
        }
        let new_value = match conn
            .request("session/new", new_params, SESSION_NEW_TIMEOUT)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                conn.shutdown().await;
                return Err(e);
            }
        };
        let new: protocol::SessionNewResult = serde_json::from_value(new_value)?;
        Ok((
            AcpSession {
                feed: registry::AcpFeed::new(new.session_id.clone(), rooms),
                id: new.session_id,
                agent: agent_name.into(),
                cwd: cwd.to_string_lossy().into_owned(),
                model,
                status: AcpStatus::Idle,
                conn,
            },
            rx,
        ))
    }

    /// 恢复既有会话（批3 任务13）：session/load（omp 18.2.1 实测支持，秒回）。
    /// 成功后 agent 会重放历史 update（feed 自动回填）；agent 不支持或会话失效
    /// → 关停连接并上抛，调用方剪除持久化条目（会话按新建呈现）。
    pub async fn resume_session(
        &self,
        agent_name: &str,
        cwd: &Path,
        session_id: &str,
        rooms: std::sync::Arc<
            std::sync::RwLock<
                HashMap<
                    String,
                    tokio::sync::broadcast::Sender<crate::drivers::claude::session::ChatEvent>,
                >,
            >,
        >,
    ) -> Result<(AcpSession, broadcast::Receiver<Inbound>)> {
        let agent = self
            .agent(agent_name)
            .ok_or_else(|| anyhow::anyhow!("未知 ACP agent: {agent_name}"))?;
        let (conn, rx) = self.connect_initialized(&agent, cwd).await?;
        match conn
            .request(
                "session/load",
                serde_json::json!({
                    "sessionId": session_id,
                    "cwd": cwd.to_string_lossy(),
                    "mcpServers": [],
                }),
                SESSION_NEW_TIMEOUT,
            )
            .await
        {
            Ok(_) => Ok((
                AcpSession {
                    feed: registry::AcpFeed::new(session_id.to_string(), rooms),
                    id: session_id.to_string(),
                    agent: agent_name.into(),
                    cwd: cwd.to_string_lossy().into_owned(),
                    model: None,
                    status: AcpStatus::Idle,
                    conn,
                },
                rx,
            )),
            Err(e) => {
                conn.shutdown().await;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::MessageSender;

    /// 生命周期常量健全性：session/new 必须宽于 initialize（omp 首建实测慢一个量级）
    #[test]
    fn timeout_constants_ordered() {
        assert!(INIT_TIMEOUT < SESSION_NEW_TIMEOUT && SESSION_NEW_TIMEOUT < PROMPT_TIMEOUT);
    }

    /// 批3 任务12：interrupt 发 session/cancel 后状态回 Idle + Tick 广播
    #[tokio::test]
    async fn interrupt_sends_cancel_and_resets_idle() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_fake_agent.py");
        let agent = crate::config::AcpAgentConfig {
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
        let (conn, _rx) = AcpConnection::spawn(&agent, dir.path()).await.unwrap();
        let registry = Arc::new(AcpRegistry::new());
        registry.insert(AcpSession {
            id: "s1".into(),
            agent: "fake".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            model: None,
            status: AcpStatus::Working,
            conn,
            feed: registry::AcpFeed::new(
                "s1".into(),
                Arc::new(std::sync::RwLock::new(HashMap::new())),
            ),
        });
        let (events_tx, mut events_rx) = tokio::sync::broadcast::channel(8);
        let router = AcpSendRouter::new(registry.clone(), events_tx);

        router.interrupt("s1").await.unwrap();
        assert_eq!(
            registry.get("s1").unwrap().status,
            AcpStatus::Idle,
            "中断后状态应回空闲"
        );
        assert!(matches!(
            events_rx.try_recv(),
            Ok(crate::drivers::claude::watcher::HubEvent::Tick)
        ));

        // 未知会话 → Err
        assert!(router.interrupt("ghost").await.is_err());
        if let Some(s) = registry.get("s1") {
            s.conn.shutdown().await;
        }
    }

    /// 批3 任务12 + F1：prompt future 中途被弃（客户端断开 → handler task cancelled）
    /// 时状态守卫拉回 Idle，rawState 不得永久滞留 working
    #[tokio::test]
    async fn aborted_prompt_future_resets_status_f1() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_fake_agent.py");
        let agent = crate::config::AcpAgentConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![
                "-u".into(),
                fixture.to_string_lossy().into_owned(),
                "permission".into(),
            ],
            cwd: None,
            model: None,
        };
        let (conn, _rx) = AcpConnection::spawn(&agent, dir.path()).await.unwrap();
        let registry = Arc::new(AcpRegistry::new());
        registry.insert(AcpSession {
            id: "s1".into(),
            agent: "fake".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            model: None,
            status: AcpStatus::Idle,
            conn,
            feed: registry::AcpFeed::new(
                "s1".into(),
                Arc::new(std::sync::RwLock::new(HashMap::new())),
            ),
        });
        let (events_tx, _events_rx) = tokio::sync::broadcast::channel(8);
        let router = AcpSendRouter::new(registry.clone(), events_tx);

        // permission 模式的 fake agent 挂起等应答 → send 停留在 Working
        let task_router = router.clone();
        let handle = tokio::spawn(async move { task_router.send("s1", "hi").await });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if registry
                .get("s1")
                .is_some_and(|s| s.status == AcpStatus::Working)
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "send 未进入 Working"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        // 模拟客户端断开：handler future 被 cancel
        handle.abort();
        let _ = handle.await;
        assert_eq!(
            registry.get("s1").unwrap().status,
            AcpStatus::Idle,
            "F1：断开后不得滞留 working"
        );
        if let Some(s) = registry.get("s1") {
            s.conn.shutdown().await;
        }
    }

    /// O1（r77 tester）：断开 + 应答序列不得永久滞留 working——prompt future 被弃
    /// （守卫回 Idle）后用户才应答挂起权限卡，dispatch 收场不得翻回 Working
    /// （prompt 已死，无人收尾）。先断开（Idle）→ 再应答 → 终态必须 Idle。
    #[tokio::test]
    async fn answered_pending_card_after_prompt_abort_stays_idle_o1() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_fake_agent.py");
        let agent = crate::config::AcpAgentConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![
                "-u".into(),
                fixture.to_string_lossy().into_owned(),
                "permission".into(),
            ],
            cwd: None,
            model: None,
        };
        let (conn, inbound_rx) = AcpConnection::spawn(&agent, dir.path()).await.unwrap();
        let registry = Arc::new(AcpRegistry::new());
        let feed = registry::AcpFeed::new(
            "s1".into(),
            Arc::new(std::sync::RwLock::new(HashMap::new())),
        );
        registry.insert(AcpSession {
            id: "s1".into(),
            agent: "fake".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            model: None,
            status: AcpStatus::Idle,
            conn: conn.clone(),
            feed: feed.clone(),
        });
        let (events_tx, _events_rx) = tokio::sync::broadcast::channel(8);
        let router = AcpSendRouter::new(registry.clone(), events_tx.clone());
        // 真 inbound 泵：权限卡推送 + 应答收场逻辑都在泵内
        dispatch::spawn_inbound_pump(
            "s1".into(),
            conn.clone(),
            inbound_rx,
            registry.clone(),
            feed.clone(),
            events_tx,
        );

        // prompt 挂起在权限请求上
        let task_router = router.clone();
        let handle = tokio::spawn(async move { task_router.send("s1", "hi").await });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let perm_key = loop {
            let card = feed
                .history()
                .into_iter()
                .find(|m| m.raw_type.as_deref() == Some("acp_permission"));
            if let Some(m) = card {
                break m.tool_use_id.unwrap();
            }
            assert!(tokio::time::Instant::now() < deadline, "权限卡未推送");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        // 客户端断开：prompt future 被 cancel → 守卫回 Idle（F1 已验）
        handle.abort();
        let _ = handle.await;
        assert_eq!(
            registry.get("s1").unwrap().status,
            AcpStatus::Idle,
            "断开后应回 Idle（F1）"
        );

        // 随后用户应答挂起权限卡 → dispatch 收场
        assert!(registry.resolve_permission(
            &perm_key,
            registry::PermissionAnswer {
                option_id: Some("opt-allow".into()),
            },
        ));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let session = registry.get("s1").unwrap();
            let resolved = session.feed.history().into_iter().any(|m| {
                m.raw_type.as_deref() == Some("acp_permission_resolved")
                    && m.tool_use_id.as_deref() == Some(perm_key.as_str())
            });
            if resolved {
                assert_eq!(
                    session.status,
                    AcpStatus::Idle,
                    "O1：prompt 已死，应答收场不得翻回 Working"
                );
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "应答后回执未出现");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // fake agent 应答后自行跑完 prompt（无人等待），连接仍需显式收尸
        tokio::time::sleep(Duration::from_millis(200)).await;
        conn.shutdown().await;
    }
}
