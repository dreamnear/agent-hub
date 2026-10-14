//! omp（Oh My Pi）文件事实源驱动（agent-hub-omp-session-host 阶段 A）：
//! omp 本体无可外连服务（`omp acp` 一进程一会话），CLI 起的会话事实源在磁盘——
//! `~/.omp/agent/sessions/<slug>/<ts>_<uuid>.jsonl`（append-only 事件 DAG，**但
//! 文件头 title slot 是原地 overlay 重写，不能假设纯 append**）。
//!
//! 职责：只读扫目录 → 侧栏/历史/回放；接管（历史面板发送）= 现有 ACP spawn +
//! `session/load` 路径（复用 AcpDriver::resume_session + 注册表装配 + prompt）。
//!
//! 纪律：
//! - 只读打开，绝不双写（omp 无 session 级独占锁）
//! - 展示纪律：侧栏默认只显示活跃会话；历史走 Past sessions（all=true）
//! - 活跃判定 v1 近似 = jsonl mtime 窗口（90s 内有写入 = 活跃 turn）∪
//!   ACP 注册表活会话排除（hub-spawn 的 omp 走 acp 驱动显示，此处去重）。
//!   进程扫描（pgrep+lsof cwd）暂缓：omp 逐写开关文件句柄，lsof 扫描对
//!   「停在提示符的活进程」同样失效，mtime 窗口对「正在产出」等效且可测——
//!   成本/精度取舍记录于此，需要提示符级活性时再升级（ceiling 注记）
//! - 容错：单文件解析失败跳过不炸清单（对齐 dsh 降级纪律）
//! - history.db 未接（sqlite 依赖成本 > 收益）：标题走文件 title slot +
//!   首条用户消息回退

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime},
};

use anyhow::Result;
use futures::future::BoxFuture;
use tokio::sync::broadcast;

use crate::drivers::claude::subagents::SubagentEntry;
use crate::drivers::claude::tasks::TaskEntry;
use crate::drivers::{
    events::{ChatEvent, HubEvent, MessagePage},
    unsupported, AgentCatalog, AgentDriver, ModelSettings, SessionControl, SessionHistory,
    StartSpec,
};
use crate::models::{ChatMessage, ChatMessageKind};

use super::acp::{registry::AcpRegistry, AcpDriver, AcpSendRouter};

/// 活跃窗口：jsonl mtime 距今 within = 活跃 turn（v1 近似，见模块头取舍注记）
const ACTIVE_MTIME: Duration = Duration::from_secs(90);
/// 目录扫描缓存 TTL（侧栏轮询 10s 一次，扫描含文件头读取——TTL 3s 防重入放大）
const SCAN_TTL: Duration = Duration::from_secs(3);
/// 文件头读取上限（title overlay + session 行都在头部；整文件回放另有全量读取）
const HEAD_BYTES: usize = 64 * 1024;

/// 扫描出的会话元数据（文件事实源的一等公民）。
#[derive(Debug, Clone)]
struct FileSession {
    /// omp session id（uuid；= 文件名 `<ts>_<uuid>.jsonl` 的 uuid 段）
    id: String,
    cwd: String,
    /// 标题：文件 title slot（非空时）；缺省回退首条用户消息
    title: Option<String>,
    path: PathBuf,
    modified: SystemTime,
    /// 首条用户消息文本（标题回退 + 接管前预览）
    first_user: Option<String>,
}

/// 扫描缓存（TTL 内复用；INSTANT+清单 二元组）
type ScanCache = Option<(std::time::Instant, Vec<FileSession>)>;

#[derive(Clone)]
pub struct OmpDriver {
    sessions_dir: PathBuf,
    /// 接管目标（ACP spawn + session/load 的既有装配）
    acp: Arc<AcpDriver>,
    router: Arc<AcpSendRouter>,
    registry: Arc<AcpRegistry>,
    /// 接管用的 ACP agent 名（config [acp] 里 command 为 omp 的条目）
    acp_agent: String,
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    events_tx: broadcast::Sender<HubEvent>,
    scan: Arc<Mutex<ScanCache>>,
    /// 接管降级映射：session/load 不支持时 spawn 新会话（新 id），文件 id → 新 id，
    /// 防同一会话在清单出现两条（id 稳定性守卫）
    taken_over: Arc<Mutex<HashMap<String, String>>>,
}

impl OmpDriver {
    pub fn new(
        sessions_dir: PathBuf,
        acp: Arc<AcpDriver>,
        router: Arc<AcpSendRouter>,
        registry: Arc<AcpRegistry>,
        acp_agent: String,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
        events_tx: broadcast::Sender<HubEvent>,
    ) -> Self {
        Self {
            sessions_dir,
            acp,
            router,
            registry,
            acp_agent,
            rooms,
            events_tx,
            scan: Arc::new(Mutex::new(None)),
            taken_over: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 目录扫描（TTL 缓存；单文件解析失败跳过）。
    fn scan(&self) -> Vec<FileSession> {
        {
            let cache = self.scan.lock().expect("omp scan lock");
            if let Some((at, sessions)) = cache.as_ref() {
                if at.elapsed() < SCAN_TTL {
                    return sessions.clone();
                }
            }
        }
        let sessions = self.scan_uncached();
        *self.scan.lock().expect("omp scan lock") =
            Some((std::time::Instant::now(), sessions.clone()));
        sessions
    }

    fn scan_uncached(&self) -> Vec<FileSession> {
        let mut out = Vec::new();
        let Ok(slugs) = std::fs::read_dir(&self.sessions_dir) else {
            return out;
        };
        for slug in slugs.flatten() {
            let Ok(files) = std::fs::read_dir(slug.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                if let Some(fs) = parse_session_file(&path) {
                    out.push(fs);
                }
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.modified));
        out
    }

    fn session_meta(&self, id: &str) -> Option<FileSession> {
        self.scan().into_iter().find(|s| s.id == id)
    }

    /// 接管（阶段 A 核心）：文件会话 → ACP spawn + session/load → 注册表装配
    /// → prompt。已活会话直接路由（幂等）。
    ///
    /// **prompt 派发即返回**（受理即清空语义）：session/prompt 是整轮同步调用
    ///（真实模型可跑数分钟），若在 POST 里 await 到轮结束，前端发送失败回滚
    /// 会把已送达的草稿恢复回输入框（用户实报「消息发出，输入框未清空」）。
    /// prompt 轮移入后台任务，轮内状态经 inbound 泵/注册表流转（与 claude
    /// attach 语义对齐）；写入失败（连接瞬断）经注册表状态呈现。
    async fn takeover_send(&self, id: &str, text: &str) -> Result<()> {
        // 0. 降级映射已存在（此前 spawn 新会话接管）→ 路由到映射后的活会话
        if let Some(mapped) = self
            .taken_over
            .lock()
            .expect("omp taken lock")
            .get(id)
            .cloned()
        {
            if self.registry.get(&mapped).is_some() {
                self.spawn_router_send(&mapped, text);
                return Ok(());
            }
        }
        // 1. registry 活会话（含 hub-spawn 同 id）→ 直接路由
        if self.registry.get(id).is_some() {
            self.spawn_router_send(id, text);
            return Ok(());
        }
        let meta = self
            .session_meta(id)
            .ok_or_else(|| anyhow::anyhow!("omp 会话文件不存在: {id}"))?;
        // 装配（对齐 api::acp::attach_session）：房间先行
        {
            let mut rooms = self.rooms.write().expect("chat_rooms lock");
            rooms.entry(id.to_string()).or_insert_with(|| {
                let (tx, _) = broadcast::channel(64);
                tx
            });
        }
        // 2. 正路：spawn + session/load（历史上下文保留，id 不变）
        match self
            .acp
            .resume_session(
                &self.acp_agent,
                Path::new(&meta.cwd),
                id,
                self.rooms.clone(),
            )
            .await
        {
            Ok((session, inbound_rx)) => {
                self.registry.insert(session.clone());
                crate::drivers::acp::dispatch::spawn_inbound_pump(
                    session.id.clone(),
                    session.conn.clone(),
                    inbound_rx,
                    self.registry.clone(),
                    session.feed.clone(),
                    self.events_tx.clone(),
                );
                let _ = self.events_tx.send(HubEvent::Tick);
                self.spawn_router_send(id, text);
                Ok(())
            }
            // 3. 降级：session/load 不支持（18.0.11 存疑）或会话失效 → spawn 新
            // 会话（不 load，无历史上下文）+ prompt；映射文件 id → 新 id，防清单双条
            Err(resume_err) => {
                tracing::warn!(
                    session_id = %id,
                    error = %resume_err,
                    "session/load 不可用，降级为 spawn 新会话继续（无历史上下文）"
                );
                let (session, inbound_rx) = self
                    .acp
                    .start_session(
                        &self.acp_agent,
                        Path::new(&meta.cwd),
                        None,
                        self.rooms.clone(),
                    )
                    .await?;
                let new_id = session.id.clone();
                self.registry.insert(session.clone());
                let spawn_id = new_id.clone();
                crate::drivers::acp::dispatch::spawn_inbound_pump(
                    session.id.clone(),
                    session.conn.clone(),
                    inbound_rx,
                    self.registry.clone(),
                    session.feed.clone(),
                    self.events_tx.clone(),
                );
                self.taken_over
                    .lock()
                    .expect("omp taken lock")
                    .insert(id.to_string(), new_id);
                let _ = self.events_tx.send(HubEvent::Tick);
                self.spawn_router_send(&spawn_id, text);
                Ok(())
            }
        }
    }

    /// 后台 prompt 派发：写入失败仅记日志（轮内状态由泵/注册表流转呈现，
    /// POST 已按「受理」返回）。
    fn spawn_router_send(&self, id: &str, text: &str) {
        let router = self.router.clone();
        let id = id.to_string();
        let text = text.to_string();
        tokio::spawn(async move {
            if let Err(e) = crate::drivers::MessageSender::send(router.as_ref(), &id, &text).await {
                tracing::warn!(session_id = %id, error = %e, "omp prompt 后台派发失败");
            }
        });
    }
}

/// 单文件头解析：session 行（id/cwd）+ title slot（非空标题）。
/// 解析失败/无 session 行 → None（容错跳过）。
fn parse_session_file(path: &Path) -> Option<FileSession> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    use std::io::Read;
    let mut head = vec![0u8; HEAD_BYTES];
    let read = reader.read(&mut head).unwrap_or(0);
    head.truncate(read);
    let text = String::from_utf8_lossy(&head);
    let mut id = None;
    let mut cwd = None;
    let mut title = None;
    let mut first_user = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // 坏行/半行（尾部截断）跳过
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("session") => {
                id = v.get("id").and_then(|x| x.as_str()).map(str::to_string);
                cwd = v.get("cwd").and_then(|x| x.as_str()).map(str::to_string);
            }
            Some("title") => {
                // title slot overlay：pad 是占位重写区，标题可能为空串
                let t = v.get("title").and_then(|x| x.as_str()).unwrap_or("");
                if !t.is_empty() {
                    title = Some(t.to_string());
                }
            }
            Some("message") if first_user.is_none() => {
                let m = &v["message"];
                if m.get("role").and_then(|r| r.as_str()) == Some("user") {
                    first_user = text_of(&m["content"]);
                }
            }
            _ => {}
        }
        if id.is_some() && cwd.is_some() && title.is_some() && first_user.is_some() {
            break;
        }
    }
    let id = id?;
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(FileSession {
        id,
        cwd: cwd.unwrap_or_default(),
        title,
        path: path.to_path_buf(),
        modified,
        first_user,
    })
}

/// content 块数组 → text 聚合（type=text 块；toolCall/thinking 由调用方分派）。
fn text_of(content: &serde_json::Value) -> Option<String> {
    match content {
        serde_json::Value::Array(blocks) => {
            let text: Vec<String> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .map(str::to_string)
                .collect();
            (!text.is_empty()).then(|| text.join("\n"))
        }
        serde_json::Value::String(s) => (!s.is_empty()).then(|| s.clone()),
        _ => None,
    }
}

/// 单条 message 事件 → 已映射消息序列（user/assistant/thinking/toolCall/
/// toolResult → 现有消息模型；其余角色/块宽松忽略）。
fn message_to_keyed(event: &serde_json::Value) -> Vec<KeyedChat> {
    let m = &event["message"];
    let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
    let ts = event
        .get("timestamp")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let eid = event.get("id").and_then(|x| x.as_str()).unwrap_or("");
    let mut out = Vec::new();
    let content = m.get("content").cloned().unwrap_or(serde_json::Value::Null);
    let blocks = content.as_array().cloned().unwrap_or_default();
    match role {
        "user" => {
            let text = text_of(&content).unwrap_or_default();
            out.push(KeyedChat {
                key: format!("{eid}:user"),
                msg: ChatMessage {
                    kind: ChatMessageKind::User,
                    text: Some(text),
                    ts: ts.clone(),
                    ..ChatMessage::default()
                },
            });
        }
        "assistant" => {
            for block in &blocks {
                let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match btype {
                    "text" => {
                        let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        if text.is_empty() {
                            continue;
                        }
                        out.push(KeyedChat {
                            key: format!("{eid}:text:{}", out.len()),
                            msg: ChatMessage {
                                kind: ChatMessageKind::Assistant,
                                text: Some(text.to_string()),
                                ts: ts.clone(),
                                ..ChatMessage::default()
                            },
                        });
                    }
                    "thinking" => {
                        let text = block.get("thinking").and_then(|t| t.as_str()).unwrap_or("");
                        if text.is_empty() {
                            continue;
                        }
                        out.push(KeyedChat {
                            key: format!("{eid}:think:{}", out.len()),
                            msg: ChatMessage {
                                kind: ChatMessageKind::Thinking,
                                text: Some(text.to_string()),
                                ts: ts.clone(),
                                ..ChatMessage::default()
                            },
                        });
                    }
                    "toolCall" => {
                        let call_id = block
                            .get("id")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        out.push(KeyedChat {
                            key: format!("{eid}:call:{}", out.len()),
                            msg: ChatMessage {
                                kind: ChatMessageKind::ToolUse,
                                tool_use_id: Some(call_id.clone()),
                                tool_name: Some(name.clone()),
                                input: Some(serde_json::json!({
                                    "sessionUpdate": "tool_call",
                                    "toolCallId": call_id,
                                    "title": name,
                                    "kind": "execute",
                                    "content": [],
                                })),
                                ts: ts.clone(),
                                ..ChatMessage::default()
                            },
                        });
                    }
                    _ => {}
                }
            }
        }
        "toolResult" => {
            let call_id = m
                .get("toolCallId")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let error = m.get("isError").and_then(|x| x.as_bool()).unwrap_or(false);
            out.push(KeyedChat {
                key: format!("{eid}:result"),
                msg: ChatMessage {
                    kind: ChatMessageKind::ToolResult,
                    tool_use_id: Some(call_id),
                    tool_name: m
                        .get("toolName")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    result: Some(serde_json::json!({
                        "status": if error { "failed" } else { "completed" },
                        "content": [{"type": "content", "content": {
                            "text": text_of(&content).unwrap_or_default()}}],
                    })),
                    error: Some(error),
                    ts,
                    ..ChatMessage::default()
                },
            });
        }
        _ => {}
    }
    out
}

/// 尾部读文件 → 事件流（append-only 事件 DAG 按文件序即时间序；坏行跳过——
/// title slot overlay 只重写头部，尾部行稳定）。
fn read_events(path: &Path) -> Vec<serde_json::Value> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("message"))
        .collect()
}

#[derive(Debug)]
struct KeyedChat {
    /// 事件 id 派生的去重键（文件事实源全量回放无增量推送，v1 暂无读方；
    /// 保留以便后续增量 tail 复用）
    #[allow(dead_code)]
    key: String,
    msg: ChatMessage,
}

impl OmpDriver {
    fn history_keyed(&self, id: &str) -> Result<Vec<KeyedChat>> {
        let Some(meta) = self.session_meta(id) else {
            return Err(anyhow::anyhow!("omp 会话文件不存在: {id}"));
        };
        Ok(read_events(&meta.path)
            .iter()
            .flat_map(message_to_keyed)
            .collect())
    }
}

impl SessionControl for OmpDriver {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { self.takeover_send(id, text).await })
    }

    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // 活会话（接管后）取消 = ACP session/cancel；文件会话无进程可取消
            if self.registry.get(id).is_some() {
                return crate::drivers::MessageSender::interrupt(self.router.as_ref(), id).await;
            }
            Err(unsupported("omp", "cancel（文件会话无运行中轮次）"))
        })
    }

    fn is_finished(&self, id: &str) -> bool {
        // 文件事实源：会话记录永在（无结束概念）；接管后由注册表口径接管
        self.registry
            .get(id)
            .is_none_or(|s| s.status == crate::drivers::acp::AcpStatus::Dead)
    }

    fn release(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    /// 权限应答：接管会话挂起的弹卡由 ACP 侧注册表持有——透传 registry
    /// resolve（幂等：已应答/已超时/未知 → false）。
    fn answer_permission(&self, _id: &str, perm_id: &str, option_id: Option<&str>) -> Result<bool> {
        Ok(self.registry.resolve_permission(
            perm_id,
            super::acp::registry::PermissionAnswer {
                option_id: option_id.map(str::to_string),
            },
        ))
    }
}

impl AgentCatalog for OmpDriver {
    fn list<'a>(&'a self, all: bool) -> BoxFuture<'a, Result<Vec<crate::models::AgentSummary>>> {
        Box::pin(async move {
            let sessions = self.scan();
            // hub-spawn 的 omp 活会话在 ACP 注册表（acp 驱动显示口径）——此处去重
            let live: HashSet<String> = self
                .registry
                .list()
                .into_iter()
                .map(|s| s.id)
                .chain(
                    self.taken_over
                        .lock()
                        .expect("omp taken lock")
                        .keys()
                        .cloned(),
                )
                .collect();
            let now = SystemTime::now();
            let mut out = Vec::new();
            for s in sessions {
                if live.contains(&s.id) {
                    continue;
                }
                let active = now
                    .duration_since(s.modified)
                    .map(|d| d < ACTIVE_MTIME)
                    .unwrap_or(false);
                if !active && !all {
                    continue; // 展示纪律：默认只显示活跃会话
                }
                let name = s
                    .title
                    .clone()
                    .or_else(|| s.first_user.clone().map(|t| t.chars().take(60).collect()))
                    .unwrap_or_else(|| format!("omp {}", &s.id[..s.id.len().min(8)]));
                out.push(crate::models::AgentSummary {
                    driver: "omp".into(),
                    id: s.id.clone(),
                    name: Some(name),
                    cwd: Some(s.cwd.clone()),
                    kind: Some("omp".into()),
                    raw_state: Some(if active { "working" } else { "idle" }.into()),
                    group: if active {
                        crate::models::Group::Working
                    } else {
                        crate::models::Group::Completed
                    },
                    detail: Some(s.path.to_string_lossy().into_owned()),
                    tokens: None,
                    context_tokens: None,
                    started_at: s
                        .modified
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .ok()
                        .map(|d| d.as_secs() as i64),
                    session_id: Some(s.id),
                });
            }
            Ok(out)
        })
    }

    fn get<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<crate::models::AgentSummary>>> {
        Box::pin(async move {
            Ok(AgentCatalog::list(self, true)
                .await?
                .into_iter()
                .find(|a| a.id == id))
        })
    }

    fn start<'a>(&'a self, _spec: &'a StartSpec) -> BoxFuture<'a, Result<String>> {
        // 新建会话走 StartDialog 的 ACP 路径（omp acp spawn）；文件驱动无 spawn
        Box::pin(async move { Err(unsupported("omp", "start（新建走 ACP 路径）")) })
    }

    fn stop<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        SessionControl::cancel(self, id)
    }

    fn remove<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<()>> {
        // 只读事实源：绝不删除用户磁盘会话
        Box::pin(async move { Err(unsupported("omp", "remove（只读事实源）")) })
    }

    fn respawn<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Err(unsupported("omp", "respawn")) })
    }

    fn logs<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Err(unsupported("omp", "logs")) })
    }
}

impl SessionHistory for OmpDriver {
    fn page<'a>(
        &'a self,
        id: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move {
            let chats = self.history_keyed(id)?;
            Ok(Some(MessagePage {
                messages: chats.into_iter().map(|c| c.msg).collect(),
                first_line: 0,
                has_more: false,
            }))
        })
    }

    fn full_history<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<ChatMessage>>> {
        Box::pin(async move { Ok(self.history_keyed(id)?.into_iter().map(|c| c.msg).collect()) })
    }

    fn is_active<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            if self.registry.get(id).is_some() {
                return Ok(true);
            }
            let Some(meta) = self.session_meta(id) else {
                return Ok(false);
            };
            Ok(SystemTime::now()
                .duration_since(meta.modified)
                .map(|d| d < ACTIVE_MTIME)
                .unwrap_or(false))
        })
    }

    fn subagents<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn subagent_page<'a>(
        &'a self,
        _id: &'a str,
        _sub: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move { Err(unsupported("omp", "subagent_page")) })
    }

    fn tasks<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Option<Vec<TaskEntry>>>> {
        Box::pin(async move { Ok(None) })
    }

    fn ensure_chat_room(&self, session_id: &str) -> broadcast::Sender<ChatEvent> {
        let mut rooms = self.rooms.write().expect("chat_rooms lock poisoned");
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
        self.ensure_chat_room(&format!("{session_id}/subagents/{subagent_id}"))
    }
}

impl ModelSettings for OmpDriver {
    // omp 文件会话无模型语义（model_change 事件随 turn 记录，v1 不透传）
}

impl AgentDriver for OmpDriver {
    fn name(&self) -> &'static str {
        "omp"
    }
}

impl crate::drivers::MessageSender for OmpDriver {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        SessionControl::send(self, id, text)
    }

    fn is_finished(&self, id: &str) -> bool {
        SessionControl::is_finished(self, id)
    }

    fn release(&self, _id: &str) -> Result<()> {
        Ok(())
    }

    fn interrupt<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        SessionControl::cancel(self, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成 fixture（schema 按调查记录，不抄用户会话内容）：
    /// title overlay（pad 占位重写区）+ session + user/assistant（text/thinking/
    /// toolCall）+ toolResult + custom（tool_execution_start 等忽略类）+ 坏行
    fn write_fixture(path: &Path, session_id: &str, cwd: &str) {
        let lines = [
            r#"{"type":"title","v":1,"pad":"0000","title":"合成会话","updatedAt":1}"#,
            r#"{"type":"session","id":"SESSION_ID","cwd":"CWD","timestamp":"2026-10-03T00:00:00Z","version":4}"#,
            "not json 坏行",
            r#"{"type":"model_change","id":"m0","model":"qwen-local","parentId":null,"timestamp":"2026-10-03T00:00:01Z"}"#,
            r#"{"type":"message","id":"e1","parentId":null,"timestamp":"2026-10-03T00:00:02Z","message":{"role":"user","timestamp":"2026-10-03T00:00:02Z","content":[{"type":"text","text":"帮我看看目录"}]}}"#,
            r#"{"type":"custom","customType":"tool_execution_start","payload":{"x":1}}"#,
            r#"{"type":"message","id":"e2","parentId":"e1","timestamp":"2026-10-03T00:00:03Z","message":{"role":"assistant","timestamp":"2026-10-03T00:00:03Z","content":[{"type":"thinking","thinking":"思考中"},{"type":"toolCall","id":"tc1","name":"bash","intent":"read","arguments":{"command":"ls"}},{"type":"text","text":"目录如下"}]}}"#,
            r#"{"type":"message","id":"e3","parentId":"e2","timestamp":"2026-10-03T00:00:04Z","message":{"role":"toolResult","toolCallId":"tc1","toolName":"bash","isError":false,"timestamp":"2026-10-03T00:00:04Z","content":[{"type":"text","text":"file.txt"}]}}"#,
        ];
        let body = lines
            .join("\n")
            .replace("SESSION_ID", session_id)
            .replace("CWD", cwd);
        std::fs::write(path, body).unwrap();
    }

    fn fixture_driver() -> (OmpDriver, tempfile::TempDir, Arc<AcpRegistry>) {
        let dir = tempfile::tempdir().unwrap();
        let slug = dir.path().join("--tmp--");
        std::fs::create_dir_all(&slug).unwrap();
        write_fixture(
            &slug.join("20261003T000000Z_fake-load-1.jsonl"),
            "fake-load-1",
            "/tmp",
        );
        write_fixture(&slug.join("20261003T010000Z_old-1.jsonl"), "old-1", "/tmp");
        // old-1 mtime 拉老（默认刚写入 = 活跃窗口内）
        let old = SystemTime::now() - Duration::from_secs(3600);
        let f = std::fs::File::options()
            .write(true)
            .open(slug.join("20261003T010000Z_old-1.jsonl"))
            .unwrap();
        f.set_modified(old).unwrap();

        let registry = Arc::new(AcpRegistry::new());
        let router = Arc::new(AcpSendRouter::new(
            registry.clone(),
            broadcast::channel(8).0,
            Arc::new(RwLock::new(HashMap::new())),
        ));
        let acp = Arc::new(AcpDriver {
            agents: std::sync::Arc::new(std::sync::RwLock::new(vec![
                crate::config::AcpAgentConfig {
                    name: "omp".into(),
                    command: "python3".into(),
                    args: vec![
                        "-u".into(),
                        std::path::PathBuf::from(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/acp_fake_agent.py"
                        ))
                        .to_string_lossy()
                        .into_owned(),
                        "loadable".into(),
                    ],
                    cwd: None,
                    model: None,
                },
            ])),
        });
        let driver = OmpDriver::new(
            dir.path().to_path_buf(),
            acp,
            router,
            registry.clone(),
            "omp".into(),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
        );
        (driver, dir, registry)
    }

    /// 扫描 + 展示纪律：活跃（mtime 窗口）进侧栏 Working，历史 Completed 仅
    /// all=true；标题走 title slot；registry 活会话排除（hub-spawn 口径归 acp）
    #[tokio::test]
    async fn lists_active_and_history_with_discipline() {
        let (d, _dir, _registry) = fixture_driver();
        // registry 活会话排除：同 id 的 omp 会话归 acp 驱动显示
        let listed = AgentCatalog::list(&d, true).await.unwrap();
        assert_eq!(listed.len(), 2, "all=1：活跃 + 历史");
        let active = listed.iter().find(|a| a.id == "fake-load-1").unwrap();
        assert_eq!(active.group, crate::models::Group::Working);
        assert_eq!(
            active.name.as_deref(),
            Some("合成会话"),
            "标题走 title slot"
        );
        let old = listed.iter().find(|a| a.id == "old-1").unwrap();
        assert_eq!(old.group, crate::models::Group::Completed);

        let listed = AgentCatalog::list(&d, false).await.unwrap();
        assert_eq!(listed.len(), 1, "默认只列活跃（展示纪律）");
        assert_eq!(listed[0].id, "fake-load-1");
    }

    /// 历史回放：user/assistant(text/thinking/toolCall)/toolResult → 消息模型
    #[tokio::test]
    async fn history_maps_all_message_shapes() {
        let (d, _dir, _registry) = fixture_driver();
        let page = SessionHistory::page(&d, "fake-load-1", 50, None)
            .await
            .unwrap()
            .unwrap();
        let msgs = &page.messages;
        assert_eq!(msgs[0].kind, ChatMessageKind::User);
        assert_eq!(msgs[0].text.as_deref(), Some("帮我看看目录"));
        // assistant 拆条：thinking → toolCall 卡 → text
        assert_eq!(msgs[1].kind, ChatMessageKind::Thinking);
        assert_eq!(msgs[2].kind, ChatMessageKind::ToolUse);
        assert_eq!(msgs[2].tool_name.as_deref(), Some("bash"));
        assert_eq!(msgs[2].tool_use_id.as_deref(), Some("tc1"));
        assert_eq!(msgs[3].kind, ChatMessageKind::Assistant);
        assert_eq!(msgs[3].text.as_deref(), Some("目录如下"));
        assert_eq!(msgs[4].kind, ChatMessageKind::ToolResult);
        assert_eq!(msgs[4].error, Some(false));
        assert!(msgs[4]
            .result
            .as_ref()
            .unwrap()
            .to_string()
            .contains("file.txt"));
        // 坏行跳过不炸（title overlay/坏行未产生消息）
    }

    /// 【接管链】历史点开 → send → ACP spawn + session/load + 注册表装配 →
    /// prompt 派发即返回（受理即清空语义：POST 不等整轮，前端乐观清 draft 不被
    /// 误回滚）。loadable fake agent：load 同 id 回显 + 重放 chunk；prompt 推
    /// updates。轮询等 feed 到齐（后台派发确定性）。
    #[tokio::test]
    async fn takeover_send_resumes_via_acp_and_prompts() {
        let (d, _dir, registry) = fixture_driver();
        // 受理即返回：POST 不等整轮模型执行
        SessionControl::send(&d, "fake-load-1", "继续这个任务")
            .await
            .unwrap();
        assert!(
            registry.get("fake-load-1").is_some(),
            "接管后注册表应持有会话"
        );
        // 轮询等后台 prompt 派发完成（feed 到齐：load 重放 + prompt chunk）
        let mut history: Vec<String> = Vec::new();
        for _ in 0..50 {
            if let Some(session) = registry.get("fake-load-1") {
                history = session
                    .feed
                    .history()
                    .iter()
                    .filter_map(|m| m.text.clone())
                    .collect();
                if history.iter().any(|t| t.contains("chunk0")) {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            history.iter().any(|t| t.contains("replayed")),
            "session/load 重放 chunk 应入 feed: {history:?}"
        );
        assert!(
            history.iter().any(|t| t.contains("chunk0")),
            "prompt 后的 agent_message_chunk 应入 feed（证明 prompt 已发）: {history:?}"
        );
        // registry 活会话排除：接管后的 id 从 omp 文件清单消失（归 acp 驱动显示口径）
        let listed = AgentCatalog::list(&d, true).await.unwrap();
        assert!(
            listed.iter().all(|a| a.id != "fake-load-1"),
            "接管后 registry 活会话应从 omp 文件清单排除"
        );
    }

    /// 【降级链】session/load 不支持（non-loadable fake）→ spawn 新会话继续
    /// （不 load，无历史上下文），映射文件 id → 新 id，清单不出现双条
    #[tokio::test]
    async fn takeover_falls_back_to_fresh_spawn_when_load_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let slug = dir.path().join("--tmp--");
        std::fs::create_dir_all(&slug).unwrap();
        write_fixture(
            &slug.join("20261003T000000Z_fail-1.jsonl"),
            "fail-1",
            "/tmp",
        );
        let registry = Arc::new(AcpRegistry::new());
        let router = Arc::new(AcpSendRouter::new(
            registry.clone(),
            broadcast::channel(8).0,
            Arc::new(RwLock::new(HashMap::new())),
        ));
        let acp = Arc::new(AcpDriver {
            agents: std::sync::Arc::new(std::sync::RwLock::new(vec![
                crate::config::AcpAgentConfig {
                    name: "omp".into(),
                    command: "python3".into(),
                    args: vec![
                        "-u".into(),
                        std::path::PathBuf::from(concat!(
                            env!("CARGO_MANIFEST_DIR"),
                            "/tests/fixtures/acp_fake_agent.py"
                        ))
                        .to_string_lossy()
                        .into_owned(),
                        "basic".into(), // 非 loadable：session/load 走 method-not-found
                    ],
                    cwd: None,
                    model: None,
                },
            ])),
        });
        let d = OmpDriver::new(
            dir.path().to_path_buf(),
            acp,
            router,
            registry.clone(),
            "omp".into(),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
        );
        // load 失败 → 降级 spawn 新会话：send 受理返回 Ok，prompt 已派发
        SessionControl::send(&d, "fail-1", "hi").await.unwrap();
        // 新会话在注册表（session/new 的 fake-session-1），原文件 id 不在
        assert!(
            registry.get("fake-session-1").is_some(),
            "降级 spawn 新会话应入注册表"
        );
        assert!(registry.get("fail-1").is_none());
        // 清单排除降级映射的文件 id（防双条）
        let listed = AgentCatalog::list(&d, true).await.unwrap();
        assert!(
            listed.iter().all(|a| a.id != "fail-1"),
            "降级后文件 id 应排除"
        );
    }

    /// 容错：文件不存在 → 明确错误（不 panic）
    #[tokio::test]
    async fn history_missing_file_errors() {
        let (d, _dir, _registry) = fixture_driver();
        let e = SessionHistory::page(&d, "nope", 50, None)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("不存在"));
    }
}
