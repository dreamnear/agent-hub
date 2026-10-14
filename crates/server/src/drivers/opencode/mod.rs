//! opencode driver（attach-service 模式首发，agent-hub-opencode-driver）：连 agent
//! 自带 HTTP 服务。**会话事实源在 opencode 自己的 SQLite**（server-first），hub 零会话
//! 状态——hub 重启后 `GET /session` 重新发现（验收标准 3 的结构性保证，产品原则 L1）。
//!
//! 与 ACP（spawn，hub 子进程）的本质区别：serve detach 拉起（不随 hub 死亡，serve.rs），
//! 会话列表/消息/发送全部走 opencode HTTP API（api.rs），hub 只做映射与转发。
//!
//! 事件流（需求决策 3）：SSE `GET /event` 订阅 →
//! - `session.status` / `session.idle` → busy 翻转 + Tick（侧栏实时）；
//! - `message.updated` / `message.part.updated` / `session.updated` → 增量对账推入
//!   chat 房间（现有推送链路）。SSE 断线由监督循环重连（sse_pump）。

pub mod api;
pub mod http;
pub mod mapping;
pub mod serve;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use anyhow::{anyhow, Result};
use futures::future::BoxFuture;
use tokio::sync::broadcast;

use crate::drivers::claude::subagents::SubagentEntry;
use crate::drivers::claude::tasks::TaskEntry;
use crate::drivers::{
    events::{ChatEvent, HubEvent, MessagePage},
    not_found, unsupported, AgentCatalog, AgentDriver, SessionControl, SessionHistory, StartSpec,
};
use crate::models::{AgentSummary, ChatMessage};

use mapping::KeyedChat;

/// 单条 HTTP 请求超时（opencode 本机回环；prompt 是真模型调用另用 SEND_TIMEOUT）
const REQ_TIMEOUT: Duration = Duration::from_secs(30);
/// prompt 超时（真模型调用，与 ACP PROMPT_TIMEOUT 同量级取宽松值）
pub const SEND_TIMEOUT: Duration = Duration::from_secs(600);
/// SSE 重连间隔
const SSE_RECONNECT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct OpencodeDriver {
    serve: serve::ServeManager,
    /// chat 房间表（与 AppState.chat_rooms 同源；feed 推送通道）
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    events_tx: broadcast::Sender<HubEvent>,
    /// 会话 busy 状态（sessionID → busy）。事实源是 opencode 的 session.status 事件 +
    /// send 在途标记；hub 重启后清零（下一次 send/SSE 重建），未知会话按 idle。
    busy: Arc<Mutex<HashMap<String, bool>>>,
    /// 已推送去重键（sessionID → key 集）。page()/full_history() 播种全量，
    /// SSE/send 增量推送据此跳过已在页内的消息（防重复渲染）。
    delivered: Arc<Mutex<HashMap<String, HashSet<String>>>>,
}

impl OpencodeDriver {
    pub fn new(
        serve: serve::ServeManager,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
        events_tx: broadcast::Sender<HubEvent>,
    ) -> Self {
        Self {
            serve,
            rooms,
            events_tx,
            busy: Arc::new(Mutex::new(HashMap::new())),
            delivered: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn base(&self) -> Result<String> {
        let port = self
            .serve
            .port()
            .ok_or_else(|| anyhow!("opencode serve 未就绪"))?;
        Ok(format!("http://127.0.0.1:{port}"))
    }

    fn set_busy(&self, id: &str, busy: bool) {
        self.busy
            .lock()
            .expect("opencode busy lock")
            .insert(id.to_string(), busy);
    }

    fn is_busy(&self, id: &str) -> bool {
        self.busy
            .lock()
            .expect("opencode busy lock")
            .get(id)
            .copied()
            .unwrap_or(false)
    }

    /// 拉取会话全量消息 → 已映射序列。不可达/不存在 → Err 上抛（调用方决定吞或报）。
    async fn fetch_keyed(&self, id: &str) -> Result<Vec<KeyedChat>> {
        let url = format!("{}/session/{id}/message", self.base()?);
        let msgs: Vec<api::OpencodeMessage> =
            tokio::time::timeout(REQ_TIMEOUT, http::get_json(&url)).await??;
        Ok(msgs.iter().flat_map(mapping::message_to_keyed).collect())
    }

    /// 把未推送过的条目推入 chat 房间并标记。返回推送条数。
    fn push_new(&self, id: &str, chats: Vec<KeyedChat>) -> usize {
        let mut delivered = self.delivered.lock().expect("opencode delivered lock");
        let seen = delivered.entry(id.to_string()).or_default();
        let rooms = self.rooms.read().expect("chat_rooms lock");
        let sender = rooms.get(id);
        let mut pushed = 0;
        for c in chats {
            if !seen.insert(c.key) {
                continue;
            }
            if let Some(s) = sender {
                let _ = s.send(ChatEvent {
                    session_id: id.to_string(),
                    seq: pushed as u64 + 1,
                    message: c.msg,
                });
            }
            pushed += 1;
        }
        pushed
    }

    /// SSE/send 完成后的增量回流：拉取 + 推新 +（有新条目时）Tick。
    /// ponytail ceiling：逐事件全量拉取再内存去重（每事件 O(n)），未做增量游标——
    /// opencode 的 part.delta 是流式碎片，用 updated 事件全量对账最简且正确；
    /// 会话消息量大（>1k 条）后改 messageID 游标或 delta 累积。
    async fn push_new_for_session(&self, id: &str) {
        match self.fetch_keyed(id).await {
            Ok(chats) => {
                if self.push_new(id, chats) > 0 {
                    let _ = self.events_tx.send(HubEvent::Tick);
                }
            }
            Err(e) => {
                tracing::debug!(session_id = %id, error = %e, "opencode 增量拉取失败（忽略）")
            }
        }
    }

    /// SSE 监督循环（任务5）：连接 `/event`，事件分流；断线退避重连。
    /// serve 未拉起时等待下一轮（首次 list 即拉起点，泵自然随后可用）。
    pub fn spawn_event_pump(&self) -> tokio::task::JoinHandle<()> {
        let driver = self.clone();
        tokio::spawn(async move {
            loop {
                // /global/event = 全库事件流（实测裸 /event 只推心跳——事件按目录
                // 作用域，global 变体才覆盖全部会话；2026-10-01 实测 39 帧全收到）
                let url = match driver.base() {
                    Ok(b) => format!("{b}/global/event"),
                    Err(_) => {
                        tokio::time::sleep(SSE_RECONNECT).await;
                        continue;
                    }
                };
                match http::sse_subscribe(&url).await {
                    Ok(mut rx) => {
                        tracing::info!("opencode SSE 已连接（global/event）");
                        while let Some(data) = rx.recv().await {
                            handle_event(&data, &driver).await;
                        }
                        tracing::debug!("opencode SSE 断开，重连");
                    }
                    Err(e) => tracing::debug!(error = %e, "opencode SSE 连接失败，重试"),
                }
                tokio::time::sleep(SSE_RECONNECT).await;
            }
        })
    }
}

/// 单条 SSE 事件分发（类型判别：状态翻转 / 增量回流；事件 schema 见 /doc event.subscribe）。
/// /global/event 的帧是 `{directory, project, payload:{...事件...}}` 包裹形态
///（实测，2026-10-01），裸 /event 为事件本体——这里统一拆 payload 后判型。
async fn handle_event(data: &str, driver: &OpencodeDriver) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let event = if v["payload"].is_object() {
        &v["payload"]
    } else {
        &v
    };
    let etype = event["type"].as_str().unwrap_or_default();
    let props = &event["properties"];
    let session_id = props["sessionID"].as_str().unwrap_or_default().to_string();
    match etype {
        "session.status" => {
            // status.type: busy | idle | retry（实测 busy/idle）
            let busy = props["status"]["type"].as_str() == Some("busy");
            if !session_id.is_empty() {
                let changed = driver.is_busy(&session_id) != busy;
                driver.set_busy(&session_id, busy);
                if changed {
                    let _ = driver.events_tx.send(HubEvent::Tick);
                }
            }
        }
        "session.idle" => {
            if !session_id.is_empty() {
                let changed = driver.is_busy(&session_id);
                driver.set_busy(&session_id, false);
                if changed {
                    let _ = driver.events_tx.send(HubEvent::Tick);
                }
            }
        }
        // 内容事件：增量回流（chat 房间推送 + Tick）
        "message.updated" | "message.part.updated" | "session.updated"
            if !session_id.is_empty() =>
        {
            driver.push_new_for_session(&session_id).await;
        }
        // server.connected/heartbeat/session.diff 等其余事件：忽略
        _ => {}
    }
}

impl SessionControl for OpencodeDriver {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let url = format!("{}/session/{id}/message", self.base()?);
            let body = api::PromptBody::text(text);
            // 发送即置 busy（侧栏即时翻转；opencode session.status 事件随后对账）
            self.set_busy(id, true);
            let _ = self.events_tx.send(HubEvent::Tick);
            // prompt 是同步长请求（真模型调用，实测 10s+）
            let result: Result<serde_json::Value> =
                tokio::time::timeout(SEND_TIMEOUT, http::post_json(&url, &body))
                    .await
                    .map_err(|_| anyhow!("opencode 发送超时（{SEND_TIMEOUT:?}）"))?;
            self.set_busy(id, false);
            let _ = self.events_tx.send(HubEvent::Tick);
            // 回流：prompt 完成后拉取增量（assistant 消息 + 工具结果）推入房间
            self.push_new_for_session(id).await;
            result.map(|_| ())
        })
    }

    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let url = format!("{}/session/{id}/abort", self.base()?);
            // abort 返回 true/false；忙态复位（用户取消即回闲，ACP 同语义）
            let _: serde_json::Value =
                tokio::time::timeout(REQ_TIMEOUT, http::post_json(&url, &serde_json::json!({})))
                    .await??;
            self.set_busy(id, false);
            let _ = self.events_tx.send(HubEvent::Tick);
            Ok(())
        })
    }

    fn is_finished(&self, id: &str) -> bool {
        // 会话是持久记录，无「结束」概念（server-first）
        let _ = id;
        false
    }

    fn release(&self, id: &str) -> Result<()> {
        // hub 侧仅清理去重/状态缓存（会话事实源在 opencode，不动它）
        self.busy.lock().expect("opencode busy lock").remove(id);
        self.delivered
            .lock()
            .expect("opencode delivered lock")
            .remove(id);
        Ok(())
    }

    fn answer_permission(
        &self,
        _id: &str,
        _perm_id: &str,
        _option_id: Option<&str>,
    ) -> Result<bool> {
        Err(unsupported("opencode", "answer_permission"))
    }
}

impl AgentCatalog for OpencodeDriver {
    fn list<'a>(&'a self, all: bool) -> BoxFuture<'a, Result<Vec<AgentSummary>>> {
        Box::pin(async move {
            // 未拉起 → 惰性拉起（首次 list 即 serve 拉起点）。
            // 会话清单走 experimental/session（全库无目录裁剪——实测裸 /session 按
            // serve 的目录上下文过滤，会漏其他目录的会话；attach 语义要求全库窗口）。
            // ponytail ceiling：experimental 端点（v1.18.32 实测稳定可用、条目同构）；
            // opencode 出稳定全局清单端点后换过去即可（结构零改动）。
            //
            // fail-soft：opencode 是可选驱动（未安装/bin 失效时不能拖死整个侧栏，
            // registry 聚合是 `?` 短路语义）——拉起失败记 debug 返回空集，claude/ACP
            // 照常出清单；安装状态可在 /api/harness（alive 探测）看到。
            let port = match self.serve.ensure_started().await {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!(error = %e, "opencode serve 未就绪，清单返回空集");
                    return Ok(Vec::new());
                }
            };
            let url = format!("http://127.0.0.1:{port}/experimental/session");
            let sessions: Vec<api::OpencodeSession> =
                tokio::time::timeout(REQ_TIMEOUT, http::get_json(&url)).await??;
            // 默认只列有活跃 turn 的会话（busy）：opencode 非 busy 会话是 SQLite
            // 历史（无进程），全部进侧栏几年后不可读（用户定案，参照 claude bg 只
            // 列活进程）。复活路径：对历史会话 send → busy 置位 → 下次 list 即出现。
            // all=true = 显式 opt-in 历史入口（web「历史会话」面板）：busy + 历史
            //（Completed），历史按最近活动倒序；本批不做搜索/分页（ceiling：会话量
            // 数千后在此截断或加 cursor）。
            // 三档（2026-10-03 补档）：busy → Working；24h 内有活动 → Other
            // （turn 结束 ≠ 关闭）；超窗 → 历史（all=true 收编，最近活动倒序）
            let (busy, rest): (Vec<&api::OpencodeSession>, Vec<&api::OpencodeSession>) =
                sessions.iter().partition(|s| self.is_busy(&s.id));
            let (recent, history): (Vec<&api::OpencodeSession>, Vec<&api::OpencodeSession>) =
                rest.iter().partition(|s| {
                    crate::drivers::within_recent_window(
                        s.time.as_ref().and_then(|t| t.last_activity_epoch()),
                    )
                });
            let mut out: Vec<AgentSummary> = busy
                .iter()
                .map(|s| mapping::session_to_summary(s, true))
                .chain(recent.iter().map(|s| mapping::session_to_summary(s, false)))
                .collect();
            if all {
                let mut history: Vec<AgentSummary> = history
                    .iter()
                    .map(|s| mapping::session_to_summary(s, false))
                    .collect();
                history.sort_by_key(|a| std::cmp::Reverse(a.started_at));
                out.extend(history);
            }
            Ok(out)
        })
    }
    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<AgentSummary>>> {
        Box::pin(async move {
            let url = format!("{}/session/{id}", self.base()?);
            match tokio::time::timeout(REQ_TIMEOUT, http::get_json::<api::OpencodeSession>(&url))
                .await
            {
                Ok(Ok(s)) => Ok(Some(mapping::session_to_summary(&s, self.is_busy(&s.id)))),
                Ok(Err(e)) if e.to_string().contains("404") => Ok(None),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(anyhow!("opencode GET 会话超时")),
            }
        })
    }

    fn start<'a>(&'a self, spec: &'a StartSpec) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let port = self.serve.ensure_started().await?;
            let url = format!(
                "http://127.0.0.1:{port}/session?directory={}",
                urlencode(&spec.cwd)
            );
            // 建会话即钉模型（spec.model "provider/model"；不传则由 opencode 自身配置
            // 决定——需求决策 6，hub 不做模型管理）。prompt 不带模型，透传会话钉定值。
            let mut body = serde_json::json!({});
            if let Some(m) = &spec.model {
                let (provider, id) = m.split_once('/').ok_or_else(|| {
                    anyhow!("opencode 模型应为 provider/model 形式（如 opencode/space-bunny-free）: {m}")
                })?;
                body["model"] = serde_json::json!({"providerID": provider, "id": id});
            }
            let created: api::OpencodeSession =
                tokio::time::timeout(REQ_TIMEOUT, http::post_json(&url, &body)).await??;
            Ok(created.id)
        })
    }

    fn stop<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        // 会话是持久记录（无进程可杀）；「停止」语义 = 中断当前处理（abort），
        // 与侧栏停止钮的用户意图一致。不删会话（删除走 remove）。
        SessionControl::cancel(self, id)
    }

    fn remove<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let url = format!("{}/session/{id}", self.base()?);
            match tokio::time::timeout(REQ_TIMEOUT, http::delete_ok(&url)).await {
                Ok(Ok(())) => {
                    let _ = self.release(id);
                    Ok(())
                }
                Ok(Err(e)) if e.to_string().contains("404") => {
                    Err(not_found(format!("opencode 会话不存在: {id}")))
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err(anyhow!("opencode DELETE 会话超时")),
            }
        })
    }

    fn respawn<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Err(unsupported("opencode", "respawn")) })
    }

    fn logs<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Err(unsupported("opencode", "logs")) })
    }
}

impl SessionHistory for OpencodeDriver {
    fn page<'a>(
        &'a self,
        id: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move {
            // opencode 全量回放（无游标分页）：firstLine=0/hasMore=false（ACP 同构信封）；
            // 前端 loadOlder 因 cursor=0 自然短路。播种 delivered 供增量推送去重。
            let chats = self.fetch_keyed(id).await.map_err(|e| {
                if e.to_string().contains("404") {
                    not_found(format!("opencode 会话不存在: {id}"))
                } else {
                    e
                }
            })?;
            {
                let mut delivered = self.delivered.lock().expect("opencode delivered lock");
                let seen = delivered.entry(id.to_string()).or_default();
                seen.extend(chats.iter().map(|c| c.key.clone()));
            }
            let messages = chats.into_iter().map(|c| c.msg).collect();
            Ok(Some(MessagePage {
                messages,
                first_line: 0,
                has_more: false,
            }))
        })
    }

    fn full_history<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<ChatMessage>>> {
        Box::pin(async move {
            // 旧全量端点 = page 同源回放（播种 + 取消息一次完成）
            match SessionHistory::page(self, id, usize::MAX, None).await? {
                Some(p) => Ok(p.messages),
                None => Ok(Vec::new()),
            }
        })
    }

    fn is_active<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool>> {
        // claude 的 jsonl mtime 口径不适用；opencode 的活跃 = 会话 busy（send 在途 /
        // session.status 事件）。前端 sessionActive 轮询据此显示流入脉冲。
        Box::pin(async move { Ok(self.is_busy(id)) })
    }

    fn subagents<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>> {
        // v1 无 subagent 建模（opencode parentID 子会话后续再说）；空集即「无」语义
        Box::pin(async move { Ok(Vec::new()) })
    }

    fn subagent_page<'a>(
        &'a self,
        _id: &'a str,
        _sub: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move { Err(unsupported("opencode", "subagent_page")) })
    }

    fn tasks<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Option<Vec<TaskEntry>>>> {
        // 无权威任务视角（opencode todo 未建模）→ None，前端回退单会话口径（K5 语义）
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
        let room = format!("{session_id}/subagents/{subagent_id}");
        self.ensure_chat_room(&room)
    }
}

impl AgentDriver for OpencodeDriver {
    fn name(&self) -> &'static str {
        "opencode"
    }
}

/// 轨一 MessageSender（message_routes 表契约；与 ACP 同款委托 SessionControl）。
impl crate::drivers::MessageSender for OpencodeDriver {
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

/// 最小 URL 查询转义（cwd 路径；`%` 编码防 `&`/空格破坏 query）。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 会话外观三透传（agent-hub-model-thinking）：
/// - 清单 = `/provider` 扁平化归一（id/provider/短名/免费/思考档位）
/// - 当前读 = `GET /session/{id}` 的 model+variant（switchModel 后 opencode 自记）
/// - 设置 = `POST /api/session/{id}/model`（opencode 侧持久，hub 不存状态）
impl crate::drivers::ModelSettings for OpencodeDriver {
    fn model_caps(&self) -> crate::drivers::ModelCaps {
        crate::drivers::ModelCaps {
            can_set: true,
            can_read: true,
            supports_thinking: true,
        }
    }

    fn list_models<'a>(&'a self) -> BoxFuture<'a, Result<Vec<crate::drivers::ModelInfo>>> {
        Box::pin(async move {
            let url = format!("{}/provider", self.base()?);
            let providers: api::OpencodeProviders =
                tokio::time::timeout(REQ_TIMEOUT, http::get_json(&url)).await??;
            let providers = providers.all;
            let mut out: Vec<crate::drivers::ModelInfo> = providers
                .into_iter()
                .flat_map(|p| {
                    let provider = p.id.clone();
                    p.models.into_values().map(move |m| {
                        let free = m.is_free();
                        let variants = m.variants.unwrap_or_default();
                        crate::drivers::ModelInfo {
                            provider: provider.clone(),
                            name: if m.name.is_empty() {
                                m.id.clone()
                            } else {
                                m.name
                            },
                            id: m.id,
                            free,
                            variants,
                        }
                    })
                })
                .collect();
            // 免费档在前，其余按 provider+id 稳定排序（面板首屏可用；全量透传，
            // 渲染上限/搜索属前端策略——ceiling：数千条后加游标）
            out.sort_by(|a, b| {
                b.free
                    .cmp(&a.free)
                    .then_with(|| a.provider.cmp(&b.provider))
                    .then_with(|| a.id.cmp(&b.id))
            });
            Ok(out)
        })
    }

    fn session_model<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<crate::drivers::SessionModel>>> {
        Box::pin(async move {
            let url = format!("{}/session/{id}", self.base()?);
            let s: api::OpencodeSession =
                tokio::time::timeout(REQ_TIMEOUT, http::get_json(&url)).await??;
            Ok(s.model.map(|m| crate::drivers::SessionModel {
                id: m.id,
                provider: m.provider_id,
                // opencode 一轮跑完会把 variant 归一成 "default"（实测）——对外口径
                // 等同未设档位，芯片不显示噪音
                variant: m.variant.or(s.variant).filter(|v| v != "default"),
            }))
        })
    }

    fn set_session_model<'a>(
        &'a self,
        id: &'a str,
        model: &'a str,
        variant: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let url = format!("{}/api/session/{id}/model", self.base()?);
            let body = api::SwitchModelBody {
                model: api::SwitchModelRef {
                    id: model,
                    provider_id: "opencode",
                    variant,
                },
            };
            let _: serde_json::Value =
                tokio::time::timeout(REQ_TIMEOUT, http::post_json(&url, &body)).await??;
            Ok("已切换".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_keeps_path_chars_escapes_query_breakers() {
        assert_eq!(urlencode("/Users/kevin/tmp dir"), "/Users/kevin/tmp%20dir");
        assert_eq!(urlencode("/a&b=c"), "/a%26b%3Dc");
    }

    /// busy 状态机：默认 idle、set/is、release 清除
    #[test]
    fn busy_state_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let driver = OpencodeDriver::new(
            serve::ServeManager::new(
                "opencode".into(),
                "127.0.0.1".into(),
                4097,
                serve::ServeManager::state_file_for(dir.path()),
            ),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
        );
        assert!(!driver.is_busy("s1"));
        driver.set_busy("s1", true);
        assert!(driver.is_busy("s1"));
        driver.release("s1").unwrap();
        assert!(!driver.is_busy("s1"), "release 应清 busy 缓存");
    }

    /// push_new：同键二次推不重复；无订阅者也计数（seq 语义与 AcpFeed 一致）
    #[test]
    fn push_new_dedups_by_key() {
        let dir = tempfile::tempdir().unwrap();
        let driver = OpencodeDriver::new(
            serve::ServeManager::new(
                "opencode".into(),
                "127.0.0.1".into(),
                4097,
                serve::ServeManager::state_file_for(dir.path()),
            ),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
        );
        let chat = |k: &str| KeyedChat {
            key: k.into(),
            msg: ChatMessage::default(),
        };
        assert_eq!(driver.push_new("s1", vec![chat("a"), chat("b")]), 2);
        // 同键重复 → 只推新的
        assert_eq!(driver.push_new("s1", vec![chat("b"), chat("c")]), 1);
    }

    /// handle_event：session.status 翻 busy + Tick；session.idle 回闲
    #[tokio::test]
    async fn events_flip_busy_and_tick() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = broadcast::channel(8);
        let driver = OpencodeDriver::new(
            serve::ServeManager::new(
                "opencode".into(),
                "127.0.0.1".into(),
                4097,
                serve::ServeManager::state_file_for(dir.path()),
            ),
            Arc::new(RwLock::new(HashMap::new())),
            tx,
        );
        handle_event(
            r#"{"type":"session.status","properties":{"sessionID":"s9","status":{"type":"busy"}}}"#,
            &driver,
        )
        .await;
        assert!(driver.is_busy("s9"));
        assert!(matches!(rx.try_recv(), Ok(HubEvent::Tick)));
        // 同态重复 → 不再 Tick
        handle_event(
            r#"{"type":"session.status","properties":{"sessionID":"s9","status":{"type":"busy"}}}"#,
            &driver,
        )
        .await;
        assert!(rx.try_recv().is_err());
        // idle → 回闲 + Tick
        handle_event(
            r#"{"type":"session.idle","properties":{"sessionID":"s9"}}"#,
            &driver,
        )
        .await;
        assert!(!driver.is_busy("s9"));
        assert!(matches!(rx.try_recv(), Ok(HubEvent::Tick)));
        // 坏 JSON / 未知类型：静默
        handle_event("not json", &driver).await;
        handle_event(r#"{"type":"server.heartbeat"}"#, &driver).await;

        // /global/event 包裹形态（{directory,project,payload}）同样翻 busy
        handle_event(
            r#"{"directory":"/repo","project":"global","payload":{"type":"session.status","properties":{"sessionID":"s9","status":{"type":"busy"}}}}"#,
            &driver,
        )
        .await;
        assert!(driver.is_busy("s9"), "payload 包裹形态必须被拆包判型");
    }

    /// 侧栏只列活跃 turn（用户定案：历史全部默认不可见，参照 claude bg 只列活进程）。
    /// 复活路径回归：send/busy 置位 → 会话出现且 group=Working。
    /// 假 opencode server（axum 内存路由）——不起真进程，路径全真（claim 健康端口）。
    #[tokio::test]
    async fn list_shows_only_busy_and_revives_on_send() {
        let app = axum::Router::new()
            .route(
                "/global/health",
                axum::routing::get(|| async { "ok" }),
            )
            .route(
                "/experimental/session",
                axum::routing::get(|| async {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs_f64()
                        * 1000.0;
                    axum::Json(vec![
                        serde_json::json!({"id":"ses_busy","title":"active turn","directory":"/repo/a"}),
                        serde_json::json!({"id":"ses_recent","title":"recent chat","directory":"/repo/c",
                                           "time":{"created":now_ms - 2.0*3600.0*1000.0,
                                                   "updated":now_ms - 2.0*3600.0*1000.0}}),
                        serde_json::json!({"id":"ses_dead","title":"old history","directory":"/repo/b",
                                           "time":{"created":now_ms - 25.0*3600.0*1000.0,
                                                   "updated":now_ms - 25.0*3600.0*1000.0}}),
                    ])
                }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let driver = OpencodeDriver::new(
            serve::ServeManager::new(
                "opencode".into(),
                "127.0.0.1".into(),
                port,
                serve::ServeManager::state_file_for(dir.path()),
            ),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
        );
        // 冷启动：recent 档（2h 内活动）保留在侧栏 Other；dead 不进
        //（2026-10-03 补档：turn 结束 ≠ 关闭，会话持续可续）
        let listed = AgentCatalog::list(&driver, false).await.unwrap();
        assert_eq!(listed.len(), 1, "recent 档保留在侧栏");
        assert_eq!(listed[0].id, "ses_recent");
        assert_eq!(listed[0].group, crate::models::Group::Other);

        // busy 置位（hub send 或外部 session.status 事件同路径）→ 出现且 Working
        driver.set_busy("ses_busy", true);
        let listed = AgentCatalog::list(&driver, false).await.unwrap();
        assert_eq!(listed.len(), 2, "busy + recent");
        let busy_entry = listed.iter().find(|a| a.id == "ses_busy").unwrap();
        assert_eq!(busy_entry.group, crate::models::Group::Working);

        // all=1（历史 opt-in 入口）：busy + recent + dead（历史按最近活动在前）
        let listed = AgentCatalog::list(&driver, true).await.unwrap();
        assert_eq!(listed.len(), 3, "all=1 含 busy + recent + dead");
        let dead = listed.iter().find(|a| a.id == "ses_dead").unwrap();
        assert_eq!(dead.group, crate::models::Group::Completed);
        assert_eq!(dead.raw_state.as_deref(), Some("idle"));

        // 复活闭环：死会话置 busy（= send 完成的事后状态）→ 回默认侧栏
        driver.set_busy("ses_dead", true);
        let listed = AgentCatalog::list(&driver, false).await.unwrap();
        assert_eq!(listed.len(), 3, "发送复活后回侧栏（busy + recent + 复活）");
    }
}
