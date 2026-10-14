//! dsh（DeepSeek Harness）驱动（attach-service 第二实现，agent-hub-dsh-driver）：
//! 连 dsh 自带 web 服务（`dsh web`）。会话事实源在 dsh 自己的存储（event-sourced
//! JSONL + 投影），hub 零会话状态——hub 重启后 `session.list` 重新发现（验收标准 5）。
//!
//! 与 opencode 同构的差异点：
//! - RPC 是**信封制**（client-request/server-response，rpc.rs），非 REST；
//! - 事件流是 **WS**（`/api/events.mux`，GET 被 426 拒绝——server-request 包
//!   MuxFrame 单向推送，客户端上行即协议违规，事件泵 events.rs）；
//! - busy 事实源 = `session.list` 的 `running` 标志（每次现读服务端真值，无 hub 缓存）；
//! - 审批 = mux `approval/requested` 帧 → 现有权限弹卡线形（acp_permission），
//!   应答 `/api/respond`（client-response 回执帧 rpcId）；dsh 侧对未决审批有
//!   重连重放语义，hub 不做超时自动拒绝（防误拒真实操作）；
//! - developer preview 容错（需求决策 4）：history 解析失败降级「不可回放」占位。

pub mod api;
pub mod events;
pub mod mapping;
pub mod rpc;
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
    not_found, unsupported, AgentCatalog, AgentDriver, ModelCaps, ModelInfo, ModelSettings,
    SessionControl, SessionHistory, SessionModel, StartSpec,
};
use crate::models::{AgentSummary, ChatMessage, ChatMessageKind};

use mapping::KeyedChat;

/// 单条 RPC 超时（本机回环）
const REQ_TIMEOUT: Duration = Duration::from_secs(30);
/// prompt 调用预算（dsh prompt 异步受理返回快；预算只防挂死）
pub const SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// mux `approval/requested` 的 hub 侧登记（应答需回执 mux 帧的 rpcId）。
/// card 随登记缓存——会话房间后开时补发（用户没盯着也要能看到弹卡）。
#[derive(Debug, Clone)]
struct ApprovalPending {
    session_id: String,
    approval_id: String,
    /// 0.1 respond 回执键（V1 线形态应答用；V2 未接）
    rpc_id: String,
    card: ChatMessage,
}

#[derive(Clone)]
pub struct DshDriver {
    serve: serve::ServeManager,
    /// chat 房间表（AppState.chat_rooms 同源）
    rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    events_tx: broadcast::Sender<HubEvent>,
    /// host.describe 缓存（侧栏 detail 标注 `provider/model`；进程内一次）
    detail: Arc<RwLock<Option<String>>>,
    /// 已推送去重键（sessionId → seq 集）；page 播种 / 泵推送共用
    delivered: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    /// 未决审批（permKey → pending）；resolved 帧摘除
    approvals: Arc<Mutex<HashMap<String, ApprovalPending>>>,
    /// 认证上下文（launch token + 会话 cookie；config [dsh] token 通路）
    auth: rpc::Auth,
    /// 401 终态标记（侧栏「凭据失效」降级；成功调用即清除）
    auth_error: Arc<std::sync::atomic::AtomicBool>,
    /// 线协议形态（自适应双协议；serve 建立后能力探测一次并缓存）
    wire: Arc<RwLock<Option<rpc::Wire>>>,
    /// dsh 迁移器拒绝旧格式日志的会话（Past sessions 标「旧版格式」）
    legacy_format: Arc<Mutex<HashSet<String>>>,
    /// 已归档会话集（rm=archiveSession 响应回带；dsh session/list 不排归档，
    /// hub 自滤。上限：hub 重启后集合失忆，归档过的会话会重新出现——用户再删
    /// 一次即幂等重归档；全量恢复需订阅 workspace/follow 流，按需再加）
    archived: Arc<Mutex<HashSet<String>>>,
    /// $events 流 clientId（ready 帧回带；应答回包必带）
    events_client: Arc<Mutex<Option<String>>>,
    /// 未决提问（eventId → 会话 id + questions 原文）；answer/取消后摘除。
    /// 原文留存供房间后开 replay（提问到达时 push_chat 落空过）
    pending_questions: Arc<Mutex<HashMap<String, (String, serde_json::Value)>>>,
    /// dsh 已知会话集（list 每轮回填）：泵 follow 过滤依据——rooms 表跨驱动共享
    ///（claude 会话 id 也在里面），不滤则对非 dsh id 盲开 follow → not-found 刷屏
    known_sessions: Arc<Mutex<HashSet<String>>>,
    // ponytail: questions 原文随 pending 存内存，hub 重启丢卡（dsh 侧 turn 仍在等，
    // fail-closed 兜底）；持久化待 projection 直读再加
}

impl DshDriver {
    pub fn new(
        serve: serve::ServeManager,
        rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
        events_tx: broadcast::Sender<HubEvent>,
        auth: rpc::Auth,
    ) -> Self {
        Self {
            serve,
            rooms,
            events_tx,
            detail: Arc::new(RwLock::new(None)),
            delivered: Arc::new(Mutex::new(HashMap::new())),
            approvals: Arc::new(Mutex::new(HashMap::new())),
            auth,
            auth_error: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            wire: Arc::new(RwLock::new(None)),
            legacy_format: Arc::new(Mutex::new(HashSet::new())),
            archived: Arc::new(Mutex::new(HashSet::new())),
            events_client: Arc::new(Mutex::new(None)),
            pending_questions: Arc::new(Mutex::new(HashMap::new())),
            known_sessions: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 当前线形态（未探测 → V2 最新兜底）。
    fn wire(&self) -> rpc::Wire {
        self.wire
            .read()
            .expect("dsh wire lock")
            .unwrap_or(rpc::Wire::V2)
    }

    /// 形态探测（base() 成功后调一次；404 漂移时由调用方 `maybe_reset_wire` 重探）。
    /// 探测在取锁前完成（guard 不跨 await）。
    async fn probe_wire(&self, base: &str) {
        if self.wire.read().expect("dsh wire lock").is_some() {
            return;
        }
        let probed = rpc::probe_wire(&self.auth, base).await;
        let mut slot = self.wire.write().expect("dsh wire lock");
        if slot.is_none() {
            tracing::info!(wire = ?probed, "dsh 线协议形态已探测");
            *slot = Some(probed);
        }
    }

    /// 端点 404（协议漂移，如 dsh 升级）→ 清缓存触发下次调用重探。
    fn maybe_reset_wire(&self, e: &anyhow::Error) {
        if e.to_string().contains("404") {
            *self.wire.write().expect("dsh wire lock") = None;
        }
    }

    /// 调用成功 → 清除 authError（重配 token 后下一次轮询自动恢复）。
    fn note_ok(&self) {
        self.auth_error
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// 调用失败 → AuthRejected 置位（侧栏「凭据失效」降级），其余错误不动标记。
    fn note_err(&self, e: &anyhow::Error) {
        if e.downcast_ref::<rpc::AuthRejected>().is_some() {
            self.auth_error
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// AgentDriver::auth_error 读数。
    pub fn auth_error(&self) -> bool {
        self.auth_error.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 当前会话 cookie（WS 泵带凭据连接用；未认证实例 None）。
    pub(crate) fn auth_cookie(&self) -> Option<String> {
        self.auth.cookie()
    }

    /// $events 流 ready：登记 clientId（应答回包必带）
    pub(crate) fn set_events_client(&self, client_id: &str) {
        *self.events_client.lock().expect("dsh events client lock") = Some(client_id.to_string());
    }

    /// dsh 已知会话快照（泵 follow 过滤）
    pub(crate) fn known_sessions_snapshot(&self) -> HashSet<String> {
        self.known_sessions.lock().expect("dsh known lock").clone()
    }

    /// clientId 快照（仅单测断言用；lib 编译目标下视为 dead code，理由：测试专用）
    #[allow(dead_code)]
    pub(crate) fn events_client_snapshot(&self) -> Option<String> {
        self.events_client
            .lock()
            .expect("dsh events client lock")
            .clone()
    }

    /// user-questions waterfall 落卡：登记 pending + 推提问卡到该会话房间。
    /// agentId 即 dsh 会话 id（agent 按会话键控）。
    pub(crate) fn register_question(
        &self,
        agent_id: &str,
        event_id: &str,
        questions: &serde_json::Value,
    ) {
        self.pending_questions
            .lock()
            .expect("dsh pending questions lock")
            .insert(
                event_id.to_string(),
                (agent_id.to_string(), questions.clone()),
            );
        let card = crate::models::ChatMessage {
            kind: crate::models::ChatMessageKind::Other,
            raw_type: Some("dsh_question".into()),
            tool_use_id: Some(event_id.to_string()),
            input: Some(serde_json::json!({
                "sessionUpdate": "dsh_question",
                "eventId": event_id,
                "questions": questions,
            })),
            ..crate::models::ChatMessage::default()
        };
        let pushed = self.push_chat(agent_id, &format!("q:{event_id}"), card);
        tracing::info!(
            session_id = %agent_id,
            event_id = %event_id,
            pushed,
            "dsh 提问卡处理完成（pushed>0=已推房间；0=房间未开待 replay）"
        );
        let _ = self.events_tx.send(HubEvent::Tick);
    }

    /// waterfall 取消（dsh fail-closed / 会话收场）：提问卡转失败收场条
    pub(crate) fn cancel_question(&self, event_id: &str) {
        if let Some((session_id, _)) = self
            .pending_questions
            .lock()
            .expect("dsh pending questions lock")
            .remove(event_id)
        {
            let receipt = crate::models::ChatMessage {
                kind: crate::models::ChatMessageKind::Other,
                raw_type: Some("dsh_question_closed".into()),
                tool_use_id: Some(format!("q:{event_id}")),
                text: Some("⏳ 提问已被 dsh 侧收场（超时失败关闭或会话结束）".into()),
                ..crate::models::ChatMessage::default()
            };
            self.push_chat(&session_id, &format!("q:{event_id}:closed"), receipt);
            let _ = self.events_tx.send(HubEvent::Tick);
        }
    }

    /// ask_user_question 应答 → POST /api/$events/result（结果信封
    /// {args:{clientId,eventId,outcome:{kind:'result',value}}}）；成功后摘 pending
    /// 并推已回答回执
    pub(crate) async fn answer_event(
        &self,
        event_id: &str,
        answers: serde_json::Value,
    ) -> Result<()> {
        let client_id = self
            .events_client
            .lock()
            .expect("dsh events client lock")
            .clone()
            .ok_or_else(|| anyhow!("dsh $events 流未就绪（clientId 未登记）"))?;
        let session_id = self
            .pending_questions
            .lock()
            .expect("dsh pending questions lock")
            .get(event_id)
            .map(|(sid, _)| sid.clone());
        let base = self.base().await?;
        let req = rpc::WireReq::new(
            ("", serde_json::Value::Null),
            (
                "$events/result",
                serde_json::json!({"args": {
                    "clientId": client_id,
                    "eventId": event_id,
                    "outcome": {"kind": "result", "value": answers},
                }}),
            ),
        );
        let wire = self.wire();
        let _: serde_json::Value =
            tokio::time::timeout(REQ_TIMEOUT, rpc::call(&self.auth, &base, wire, &req)).await??;
        self.pending_questions
            .lock()
            .expect("dsh pending questions lock")
            .remove(event_id);
        if let Some(session_id) = session_id {
            let receipt = crate::models::ChatMessage {
                kind: crate::models::ChatMessageKind::Other,
                raw_type: Some("dsh_question_answered".into()),
                tool_use_id: Some(format!("q:{event_id}")),
                text: Some("✅ 已回答，turn 继续".into()),
                ..crate::models::ChatMessage::default()
            };
            self.push_chat(&session_id, &format!("q:{event_id}:answered"), receipt);
        }
        let _ = self.events_tx.send(HubEvent::Tick);
        Ok(())
    }

    /// 幂等拉起并返回 base URL（list 即拉起点，同 opencode）。建立后做线形态
    /// 能力探测（自适应双协议）；首次成功顺带缓存宿主默认模型标注（fail-soft）。
    async fn base(&self) -> Result<String> {
        let port = self.serve.ensure_started().await?;
        // spawn 路凭据注入（Auth::set_token 仅在未配置时写——config token 优先）
        if let Some(t) = self.serve.launch_token() {
            self.auth.set_token(t);
        }
        let base = format!("http://127.0.0.1:{port}");
        if self.wire.read().expect("dsh wire lock").is_none() {
            self.probe_wire(&base).await;
        }
        if self.detail.read().expect("dsh detail lock").is_none() {
            let req = rpc::WireReq::new(
                ("host.describe", serde_json::json!({})),
                ("session/modelCatalog", serde_json::json!({"args": {}})),
            );
            let detail = match self.wire() {
                rpc::Wire::V1 => {
                    rpc::call::<api::HostDescribe>(&self.auth, &base, self.wire(), &req)
                        .await
                        .ok()
                        .filter(|d| !d.provider.is_empty() || !d.model.is_empty())
                        .map(|d| format!("{}/{}", d.provider, d.model))
                }
                rpc::Wire::V2 => {
                    rpc::call::<api::ModelCatalog>(&self.auth, &base, self.wire(), &req)
                        .await
                        .ok()
                        .and_then(|c| c.default)
                        .map(|d| format!("{}/{}", d.provider, d.model))
                }
            };
            if let Some(detail) = detail {
                *self.detail.write().expect("dsh detail lock") = Some(detail);
            }
        }
        Ok(base)
    }

    fn cached_detail(&self) -> Option<String> {
        self.detail.read().expect("dsh detail lock").clone()
    }

    /// mux WS 事件泵（events.rs；main 启动挂载，serve 未拉起时自行等待）。
    pub fn spawn_event_pump(&self) -> tokio::task::JoinHandle<()> {
        events::spawn_event_pump(self.clone())
    }

    /// 单条消息推入房间（泵/审批路径）。**房间未开不推也不标记**——待 ensure_chat_room
    /// 首建房间时补发（审批弹卡在用户打开会话前到达的场景）；已标记的键去重不重发。
    pub(crate) fn push_chat(&self, id: &str, key: &str, msg: ChatMessage) -> usize {
        {
            let rooms = self.rooms.read().expect("chat_rooms lock");
            if !rooms.contains_key(id) {
                return 0;
            }
        }
        let mut delivered = self.delivered.lock().expect("dsh delivered lock");
        if !delivered
            .entry(id.to_string())
            .or_default()
            .insert(key.to_string())
        {
            return 0;
        }
        let rooms = self.rooms.read().expect("chat_rooms lock");
        if let Some(s) = rooms.get(id) {
            let _ = s.send(ChatEvent {
                session_id: id.to_string(),
                seq: 1,
                message: msg,
            });
            1
        } else {
            0
        }
    }

    /// 拉取会话全量事件 → 已映射序列（不可达/不存在 → Err 上抛）。
    /// 0.2.0：session/history 并入 session/page（address + throughSeq）。
    async fn fetch_keyed(&self, id: &str) -> Result<Vec<KeyedChat>> {
        let entries = self.fetch_page(id, None).await?;
        Ok(entries.iter().flat_map(mapping::event_to_keyed).collect())
    }

    /// session/page 全量拉取（records → HistoryEntry 事件序列）。
    /// **throughSeq 是严格游标**（> 会话当前 seq 即 gateway/bad-request "past
    /// cursor"）——先从 session/list 投影取 asOfSeq，再以 asOfSeq 为 throughSeq。
    /// child = Some((parent, child)) 时走 subagent 地址（0.1 形态）。
    async fn fetch_page(
        &self,
        id: &str,
        child: Option<(&str, &str)>,
    ) -> Result<Vec<api::HistoryEntry>> {
        let base = self.base().await?;
        // 0.2.0 游标：asOfSeq 自 session/list 投影
        let v: RpcListValue = tokio::time::timeout(
            REQ_TIMEOUT,
            rpc::call(
                &self.auth,
                &base,
                rpc::Wire::V2,
                &rpc::WireReq::new(
                    ("", serde_json::Value::Null),
                    (
                        "session/list",
                        serde_json::json!({"args": {"_request": {}}}),
                    ),
                ),
            ),
        )
        .await??;
        let target = if let Some((_, child_id)) = child {
            child_id
        } else {
            id
        };
        let as_of_seq = v
            .items
            .iter()
            .find(|s| s.session_id == target)
            .and_then(|s| s.projections.as_ref())
            .map(|p| p.as_of_seq)
            .unwrap_or(-1);
        if as_of_seq < 0 {
            return Ok(Vec::new()); // 空会话（asOfSeq=-1 惯例）：零事件
        }
        // V2 subagent 地址（child 行派生）；V1 的 subagent 地址在 V1 分支内
        let v2_address = match child {
            Some((parent, child_id)) => serde_json::json!({
                "kind": "subagent", "parentSessionId": parent,
                "childSessionId": child_id, "mode": "unknown"}),
            None => serde_json::json!({"kind": "session", "sessionId": id}),
        };
        // V1：session.history 直 payload；V2：session/page args 包裹 + records 信封
        let wire = self.wire();
        let entries: Vec<api::HistoryEntry> = match wire {
            rpc::Wire::V1 => {
                // V1：主会话 = session.history；subagent = subagent.history（独立端点）
                let req = match child {
                    Some((parent, child_id)) => (
                        "subagent.history",
                        serde_json::json!({
                            "parentSessionId": parent,
                            "childSessionId": child_id,
                            "mode": "one-shot",
                        }),
                    ),
                    None => ("session.history", serde_json::json!({"sessionId": id})),
                };
                let h: api::HistoryValue = tokio::time::timeout(
                    REQ_TIMEOUT,
                    rpc::call(
                        &self.auth,
                        &base,
                        wire,
                        &rpc::WireReq::new(req, ("", serde_json::Value::Null)),
                    ),
                )
                .await??;
                h.events
            }
            rpc::Wire::V2 => {
                let h: api::PageValue = tokio::time::timeout(
                    REQ_TIMEOUT,
                    rpc::call(
                        &self.auth,
                        &base,
                        wire,
                        &rpc::WireReq::new(
                            ("", serde_json::Value::Null),
                            (
                                "session/page",
                                serde_json::json!({"args": {"request": {
                                    "address": v2_address,
                                    "throughSeq": as_of_seq,
                                }}}),
                            ),
                        ),
                    ),
                )
                .await??;
                h.records
                    .into_iter()
                    .map(|r| api::HistoryEntry {
                        event: r.event,
                        view: None,
                    })
                    .collect()
            }
        };
        Ok(entries)
    }

    /// 权限弹卡消息（acp_permission 线形，前端 AcpPermissionDialog 零改动渲染；
    /// options 双选如实呈现 dsh 的 allowed-once/rejected 二值语义）。
    pub(crate) fn approval_card(
        &self,
        tool_name: &str,
        reason: Option<&str>,
        call_id: Option<&str>,
        session_id: &str,
        approval_id: &str,
    ) -> ChatMessage {
        let perm_key = perm_key(session_id, approval_id);
        ChatMessage {
            kind: ChatMessageKind::Other,
            raw_type: Some("acp_permission".into()),
            tool_use_id: Some(perm_key),
            tool_name: Some(tool_name.to_string()),
            input: Some(serde_json::json!({
                "toolName": tool_name,
                "callId": call_id,
                "reason": reason,
                "options": [
                    {"optionId": "allowed-once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "rejected", "name": "Reject", "kind": "reject_once"},
                ],
            })),
            ..ChatMessage::default()
        }
    }
}

fn perm_key(session_id: &str, approval_id: &str) -> String {
    format!("perm:{session_id}:{approval_id}")
}

impl SessionControl for DshDriver {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let base = self.base().await?;
            // mode=queue：会话忙时排队（dsh 原生语义；steer 是插话，hub 不擅自用）；
            // requestId 仅 0.2.0 必填（进程内唯一即可）
            let req = rpc::WireReq::new(
                (
                    "session.prompt",
                    serde_json::json!({
                        "sessionId": id,
                        "mode": "queue",
                        "content": [{"type": "text", "text": text}],
                    }),
                ),
                (
                    "session/prompt",
                    serde_json::json!({"args": {"request": {
                        "requestId": rpc::rpc_uuid(),
                        "sessionId": id,
                        "mode": "queue",
                        "content": [{"type": "text", "text": text}],
                    }}}),
                ),
            );
            let wire = self.wire();
            let _: serde_json::Value =
                tokio::time::timeout(SEND_TIMEOUT, rpc::call(&self.auth, &base, wire, &req))
                    .await
                    .map_err(|_| anyhow!("dsh 发送超时（{SEND_TIMEOUT:?}）"))??;
            let _ = self.events_tx.send(HubEvent::Tick);
            Ok(())
        })
    }

    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let base = self.base().await?;
            let req = rpc::WireReq::new(
                ("session.cancel", serde_json::json!({"sessionId": id})),
                (
                    "session/cancel",
                    serde_json::json!({"request": {"sessionId": id}}),
                ),
            );
            let wire = self.wire();
            let _: serde_json::Value =
                tokio::time::timeout(REQ_TIMEOUT, rpc::call(&self.auth, &base, wire, &req))
                    .await??;
            let _ = self.events_tx.send(HubEvent::Tick);
            Ok(())
        })
    }

    fn is_finished(&self, _id: &str) -> bool {
        // 会话是持久记录，无「结束」概念（server-first）
        false
    }

    fn release(&self, id: &str) -> Result<()> {
        self.delivered
            .lock()
            .expect("dsh delivered lock")
            .remove(id);
        Ok(())
    }

    /// 审批应答：V1 走 /api/respond（client-response 回执帧 rpcId）；V2（0.2.0）
    /// 载体随 typert 网关迁移消失 → 显式 unsupported（弹卡登记/回执链路保留）。
    fn answer_permission(&self, id: &str, perm_id: &str, option_id: Option<&str>) -> Result<bool> {
        // V2（0.2.0-rc.2）：/api/respond 载体随 typert 网关迁移消失，待新 wire
        // 稳定后重接；V1 恢复 respond 应答链路
        if self.wire() == rpc::Wire::V2 {
            return Err(unsupported(
                "dsh",
                "answer_permission（0.2.0 审批应答通路迁移中，暂不可用）",
            ));
        }
        let pending = {
            let map = self.approvals.lock().expect("dsh approvals lock");
            map.get(perm_id).cloned()
        }
        .ok_or_else(|| not_found(format!("dsh 审批不存在或已收场: {perm_id}")))?;
        let _ = id; // 会话身份以登记时为准（防跨会话错配）
        let outcome = match option_id {
            Some("allowed-once") => "allowed-once",
            _ => "rejected",
        };
        // trait 是同步签名；respond 是本机回环快调用——进程内最小 runtime 桥接。
        // ⚠ 本 block_on 仅因唯一调用点（api/messages.rs）走 spawn_blocking 才安全
        // （阻塞线程非 runtime worker）；异步上下文直调 = 嵌套 runtime panic
        //（answer_question 急修 2026-10-04 的前车之鉴），勿在 async 链直调本方法
        static RT: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let rt = RT.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("dsh respond runtime")
        });
        let this = self.clone();
        let pending = pending.clone();
        let outcome_str = outcome.to_string();
        rt.block_on(async move {
            let base = this.base().await?;
            let envelope = serde_json::json!({
                "type": "client-response",
                "rpcId": pending.rpc_id,
                "result": {"ok": true, "value": {
                    "sessionId": pending.session_id,
                    "approvalId": pending.approval_id,
                    "outcome": outcome_str}},
            });
            rpc::post_envelope(&this.auth, &base, "respond", envelope).await
        })?;
        Ok(outcome == "allowed-once")
    }

    fn answer_question(
        &self,
        id: &str,
        event_id: &str,
        answers: serde_json::Value,
    ) -> BoxFuture<'_, Result<()>> {
        // **原生异步**（急修 2026-10-04）：BoxFuture 本就跑在 tokio worker 上，
        // 内嵌 block_on = 嵌套 runtime 必崩（实测 Empty reply）。answer_permission
        // 的同款桥接存活前提 = handler 走 spawn_blocking（阻塞线程非 worker），
        // 二者约束不同不可混用——见 answer_permission 内注释。
        let this = self.clone();
        let event_id = event_id.to_string();
        let _ = id; // 会话身份以登记时为准
        Box::pin(async move { this.answer_event(&event_id, answers).await })
    }
}

impl AgentCatalog for DshDriver {
    fn list<'a>(&'a self, all: bool) -> BoxFuture<'a, Result<Vec<AgentSummary>>> {
        Box::pin(async move {
            // fail-soft：dsh 是可选驱动，拉起失败不拖死整个侧栏（opencode 同款）
            let base = match self.base().await {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!(error = %e, "dsh web 未就绪，清单返回空集");
                    return Ok(Vec::new());
                }
            };
            let req = rpc::WireReq::new(
                ("session.list", serde_json::json!({})),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            );
            let wire = self.wire();
            let call = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call::<RpcListValue>(&self.auth, &base, wire, &req),
            )
            .await
            .map_err(|_| anyhow!("dsh session.list 超时"))
            .and_then(|inner| inner);
            match &call {
                Ok(_) => self.note_ok(),
                Err(e) => {
                    self.maybe_reset_wire(e); // 协议漂移（dsh 升级）→ 下次重探
                    self.note_err(e);
                    tracing::debug!(error = %e, "dsh session.list 失败，清单降级空集");
                }
            }
            let v = call.unwrap_or(RpcListValue { items: Vec::new() });
            // 0.2.0：subagent 会话内联主清单（origin=subagent）——侧栏只列主会话，
            // subagent 走 SessionHistory::subagents 派生（V1 无 origin 字段，过滤天然空转）
            // rm=归档自滤（2026-10-03 两连 ②）：dsh session/list 不排归档会话，
            // hub 按存下的归档集滤（集合来源=archiveSession 响应回带）
            let archived = self.archived.lock().expect("dsh archived lock").clone();
            {
                let mut known = self.known_sessions.lock().expect("dsh known lock");
                *known = v.items.iter().map(|s| s.session_id.clone()).collect();
            }
            let items: Vec<api::DshSession> = v
                .items
                .into_iter()
                .filter(|s| s.origin.as_deref() != Some("subagent"))
                .filter(|s| !archived.contains(&s.session_id))
                .collect();
            let detail = self.cached_detail();
            // 展示纪律（需求决策 8 + 2026-10-03 补档）：三分——running → Working；
            // 非 running 但 24h 内有活动 → Other（Idle 组照常显示，turn 结束 ≠ 关闭）；
            // 超窗 → 历史（Past sessions 收编，最近活动倒序）。
            // 复活路径：对历史会话 send → running 置位 → 下次 list 出现。
            let now_sorted = |mut xs: Vec<AgentSummary>| {
                xs.sort_by_key(|a| std::cmp::Reverse(a.started_at));
                xs
            };
            let legacy = self.legacy_format.lock().expect("dsh legacy lock").clone();
            let summaries: Vec<AgentSummary> = items
                .iter()
                .map(|s| {
                    let mut a = mapping::session_to_summary(s, detail.clone());
                    if legacy.contains(&s.session_id) {
                        a.name = a.name.take().map(|n| format!("【旧版格式】{n}"));
                    }
                    a
                })
                .collect();
            let (mut out, history): (Vec<AgentSummary>, Vec<AgentSummary>) =
                summaries.into_iter().partition(|a| {
                    a.group == crate::models::Group::Working
                        || a.group == crate::models::Group::Other
                });
            if all {
                out.extend(now_sorted(history));
            }
            Ok(out)
        })
    }

    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<AgentSummary>>> {
        Box::pin(async move {
            let base = self.base().await?;
            let req = rpc::WireReq::new(
                ("session.list", serde_json::json!({})),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            );
            let wire = self.wire();
            let call = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call::<RpcListValue>(&self.auth, &base, wire, &req),
            )
            .await
            .map_err(|_| anyhow!("dsh session.list 超时"))
            .and_then(|inner| inner);
            match &call {
                Ok(_) => self.note_ok(),
                Err(e) => {
                    self.maybe_reset_wire(e); // 协议漂移（dsh 升级）→ 下次重探
                    self.note_err(e);
                    tracing::debug!(error = %e, "dsh session.list 失败，清单降级空集");
                }
            }
            let v = call.unwrap_or(RpcListValue { items: Vec::new() });
            // 0.2.0：subagent 会话内联主清单（origin=subagent）——侧栏只列主会话，
            // subagent 走 SessionHistory::subagents 派生（V1 无 origin 字段，过滤天然空转）
            // rm=归档自滤（2026-10-03 两连 ②）：dsh session/list 不排归档会话，
            // hub 按存下的归档集滤（集合来源=archiveSession 响应回带）
            let archived = self.archived.lock().expect("dsh archived lock").clone();
            {
                let mut known = self.known_sessions.lock().expect("dsh known lock");
                *known = v.items.iter().map(|s| s.session_id.clone()).collect();
            }
            let items: Vec<api::DshSession> = v
                .items
                .into_iter()
                .filter(|s| s.origin.as_deref() != Some("subagent"))
                .filter(|s| !archived.contains(&s.session_id))
                .collect();
            let detail = self.cached_detail();
            Ok(items
                .iter()
                .find(|s| s.session_id == id)
                .map(|s| mapping::session_to_summary(s, detail)))
        })
    }

    fn start<'a>(&'a self, spec: &'a StartSpec) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let base = self.base().await?;
            let req = rpc::WireReq::new(
                ("session.create", serde_json::json!({"cwd": spec.cwd})),
                (
                    "session/create",
                    serde_json::json!({"args": {"request": {"cwd": spec.cwd}}}),
                ),
            );
            let wire = self.wire();
            let created: serde_json::Value =
                tokio::time::timeout(REQ_TIMEOUT, rpc::call(&self.auth, &base, wire, &req))
                    .await??;
            let id = created["sessionId"]
                .as_str()
                .ok_or_else(|| anyhow!("dsh session.create 缺 sessionId"))?
                .to_string();
            if !spec.prompt.trim().is_empty() {
                self.send(&id, &spec.prompt).await?;
            }
            let _ = self.events_tx.send(HubEvent::Tick);
            Ok(id)
        })
    }

    fn stop<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        // 「停止」= 中断当前处理（session.cancel）；会话记录不删
        SessionControl::cancel(self, id)
    }

    fn remove<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // dsh 删除语义 = workspace.archiveSession（workspace-controller/
            // src/index.ts:154；请求 {sessionId, stopActivity?}）。stopActivity=true：
            // 先停活跃工作再归档，免活跃会话被 workspace/session-active 拒绝。
            let base = self.base().await?;
            let req = rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "workspace/archiveSession",
                    serde_json::json!({"args": {"request": {
                        "sessionId": id, "stopActivity": true,
                    }}}),
                ),
            );
            let wire = self.wire();
            let value: serde_json::Value =
                tokio::time::timeout(REQ_TIMEOUT, rpc::call(&self.auth, &base, wire, &req))
                    .await??;
            // 响应回带全量归档集（WorkspaceArchiveValue）——这是 hub 唯一能拿到
            // 归档事实的通道（无 unary 查询端点），存下供 list 自滤
            if let Some(ids) = value["archivedSessionIds"].as_array() {
                let mut archived = self.archived.lock().expect("dsh archived lock");
                *archived = ids
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect();
            }
            let _ = self.events_tx.send(HubEvent::Tick);
            Ok(())
        })
    }

    fn respawn<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Err(unsupported("dsh", "respawn")) })
    }

    fn logs<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Err(unsupported("dsh", "logs")) })
    }
}

/// `session.list` value 信封（`{items:[...]}`）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RpcListValue {
    #[serde(default)]
    pub items: Vec<api::DshSession>,
}

impl SessionHistory for DshDriver {
    fn page<'a>(
        &'a self,
        id: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move {
            // 全量回放（无游标分页）：firstLine=0；hasMore 透传（ACP 同构信封）。
            let chats = match self.fetch_keyed(id).await {
                Ok(c) => c,
                // developer preview 容错（需求决策 4）：corruption 等 → 占位说明卡，
                // 会话可打开可继续，不炸整链（实测 SessionPersistenceCorruptionError）。
                Err(e) if e.to_string().contains("not-found") => {
                    return Err(not_found(format!("dsh 会话不存在: {id}")))
                }
                Err(e) => {
                    self.note_err(&e);
                    // 归因四分（2026-10-03 收尾）：认证 / 存储校验 / 迁移器拒绝
                    // 旧格式（0.2.0 v1→v2 迁移失败，dsh 侧限制）/ 其余网关异常
                    let raw = e.to_string();
                    let legacy = raw.contains("refuses this format")
                        || raw.contains("resume failed")
                        || raw.contains("tool-call-delta");
                    if legacy {
                        self.legacy_format
                            .lock()
                            .expect("dsh legacy lock")
                            .insert(id.to_string());
                    }
                    let (key, note) = if self.auth_error() {
                        ("auth-failed", mapping::AUTH_NOTE)
                    } else if legacy {
                        ("legacy-format", mapping::LEGACY_FORMAT_NOTE)
                    } else if raw.contains("SessionPersistenceCorruption") {
                        ("non-replayable", mapping::NON_REPLAY_NOTE)
                    } else {
                        ("read-failed", mapping::READ_FAILED_NOTE)
                    };
                    tracing::warn!(session_id = %id, error = %e, key = key, "dsh history 读取失败，降级占位");
                    vec![KeyedChat {
                        key: key.into(),
                        msg: ChatMessage {
                            kind: ChatMessageKind::Assistant,
                            text: Some(note.to_string()),
                            ..ChatMessage::default()
                        },
                    }]
                }
            };
            {
                let mut delivered = self.delivered.lock().expect("dsh delivered lock");
                delivered
                    .entry(id.to_string())
                    .or_default()
                    .extend(chats.iter().map(|c| c.key.clone()));
            }
            // pending 提问卡 page 回填（第六轮收尾）：提问卡是非持久房间内存物，
            // waterfall 到达时房间未开 → push_chat 落空（pushed=0），replay 只在
            // ensure_chat_room 首建触发、API 读页不建房 → 卡永失。此处按 pending
            // 登记追加到页尾（前端按键去重，answer 后摘登记即随 refetch 消失）
            let mut chats = chats;
            if !id.is_empty() {
                let pendings: Vec<(String, serde_json::Value)> = {
                    let pq = self
                        .pending_questions
                        .lock()
                        .expect("dsh pending questions lock");
                    pq.iter()
                        .filter(|(_, (sid, _))| sid == id)
                        .map(|(eid, (_, qs))| (eid.clone(), qs.clone()))
                        .collect()
                };
                for (eid, qs) in pendings {
                    let card = crate::models::ChatMessage {
                        kind: crate::models::ChatMessageKind::Other,
                        raw_type: Some("dsh_question".into()),
                        tool_use_id: Some(eid.clone()),
                        input: Some(serde_json::json!({
                            "sessionUpdate": "dsh_question",
                            "eventId": eid,
                            "questions": qs,
                        })),
                        ..crate::models::ChatMessage::default()
                    };
                    chats.push(KeyedChat {
                        key: format!("q:{eid}"),
                        msg: card,
                    });
                }
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
            match SessionHistory::page(self, id, usize::MAX, None).await? {
                Some(p) => Ok(p.messages),
                None => Ok(Vec::new()),
            }
        })
    }

    fn is_active<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool>> {
        // 活跃 = session.list 的 running（服务端真值，无 hub 缓存）
        Box::pin(async move {
            let base = self.base().await?;
            let req = rpc::WireReq::new(
                ("session.list", serde_json::json!({})),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            );
            let wire = self.wire();
            let call = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call::<RpcListValue>(&self.auth, &base, wire, &req),
            )
            .await
            .map_err(|_| anyhow!("dsh session.list 超时"))
            .and_then(|inner| inner);
            match &call {
                Ok(_) => self.note_ok(),
                Err(e) => {
                    self.maybe_reset_wire(e); // 协议漂移（dsh 升级）→ 下次重探
                    self.note_err(e);
                    tracing::debug!(error = %e, "dsh session.list 失败，清单降级空集");
                }
            }
            let v = call.unwrap_or(RpcListValue { items: Vec::new() });
            // 0.2.0：subagent 会话内联主清单（origin=subagent）——侧栏只列主会话，
            // subagent 走 SessionHistory::subagents 派生（V1 无 origin 字段，过滤天然空转）
            // rm=归档自滤（2026-10-03 两连 ②）：dsh session/list 不排归档会话，
            // hub 按存下的归档集滤（集合来源=archiveSession 响应回带）
            let archived = self.archived.lock().expect("dsh archived lock").clone();
            {
                let mut known = self.known_sessions.lock().expect("dsh known lock");
                *known = v.items.iter().map(|s| s.session_id.clone()).collect();
            }
            let items: Vec<api::DshSession> = v
                .items
                .into_iter()
                .filter(|s| s.origin.as_deref() != Some("subagent"))
                .filter(|s| !archived.contains(&s.session_id))
                .collect();
            let v = RpcListValue { items };
            Ok(v.items.iter().any(|s| s.session_id == id && s.running))
        })
    }

    fn subagents<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>> {
        Box::pin(async move {
            // 默认口径：只回活跃条目（2026-10-01 产品纪律，全驱动一致）
            Ok(subagent_children(self, id)
                .await?
                .into_iter()
                .filter(|e| e.status == "running")
                .collect())
        })
    }

    fn subagents_finished<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>> {
        Box::pin(async move { subagent_children(self, id).await })
    }

    fn subagent_page<'a>(
        &'a self,
        id: &'a str,
        sub: &'a str,
        _limit: usize,
        _before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>> {
        Box::pin(async move {
            // 0.2.0：subagent 历史并入 session/page（subagent 地址）
            let entries = self.fetch_page(id, Some((id, sub))).await?;
            let messages = entries
                .iter()
                .flat_map(mapping::event_to_keyed)
                .map(|k| k.msg)
                .collect();
            Ok(Some(MessagePage {
                messages,
                first_line: 0,
                has_more: false,
            }))
        })
    }

    fn tasks<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Option<Vec<TaskEntry>>>> {
        // 无权威任务视角 → None，前端回退单会话口径（K5 语义）
        Box::pin(async move { Ok(None) })
    }

    fn ensure_chat_room(&self, session_id: &str) -> broadcast::Sender<ChatEvent> {
        let mut rooms = self.rooms.write().expect("chat_rooms lock poisoned");
        match rooms.entry(session_id.to_string()) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let (tx, _) = broadcast::channel(64);
                e.insert(tx.clone());
                drop(rooms);
                // 房间首建：补发登记中的未决审批卡（用户在弹卡到达后才打开会话的场景；
                // 已推过的键被 delivered 去重，不重发）
                let pendings: Vec<ApprovalPending> = {
                    let approvals = self.approvals.lock().expect("dsh approvals lock");
                    approvals
                        .values()
                        .filter(|p| p.session_id == session_id)
                        .cloned()
                        .collect()
                };
                for p in pendings {
                    let key = perm_key(&p.session_id, &p.approval_id);
                    self.push_chat(session_id, &key, p.card.clone());
                }
                // 未决提问卡同理补发（提问到达时房间未开 → register_question 的
                // push_chat 落空；此处按 pending 登记 replay，delivered 去重防重）
                let pendings: Vec<(String, serde_json::Value)> = {
                    let pq = self
                        .pending_questions
                        .lock()
                        .expect("dsh pending questions lock");
                    pq.iter()
                        .filter(|(_, (sid, _))| sid == session_id)
                        .map(|(eid, (_, qs))| (eid.clone(), qs.clone()))
                        .collect()
                };
                for (eid, qs) in pendings {
                    let card = crate::models::ChatMessage {
                        kind: crate::models::ChatMessageKind::Other,
                        raw_type: Some("dsh_question".into()),
                        tool_use_id: Some(eid.clone()),
                        input: Some(serde_json::json!({
                            "sessionUpdate": "dsh_question",
                            "eventId": eid,
                            "questions": qs,
                        })),
                        ..crate::models::ChatMessage::default()
                    };
                    self.push_chat(session_id, &format!("q:{eid}"), card);
                }
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

/// V1 `subagent.list` child 行 → SubagentEntry（diagnostic 损坏行跳过）。
fn subagent_entry(v: &serde_json::Value) -> Option<SubagentEntry> {
    if v["kind"].as_str() != Some("child") {
        return None;
    }
    let id = v["id"].as_str()?.to_string();
    Some(SubagentEntry {
        agent_id: id.clone(),
        name: v["label"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| id.clone()),
        agent_type: v["mode"].as_str().unwrap_or("one-shot").to_string(),
        description: None,
        model: None,
        status: v["activity"].as_str().unwrap_or("inactive").to_string(),
        started_at: None,
        last_active_at: None,
    })
}

/// subagent 清单：V1 = subagent.list 独立端点；V2 = session/list 内联
/// origin=subagent + parentSessionId 行派生。
async fn subagent_children(driver: &DshDriver, id: &str) -> Result<Vec<SubagentEntry>> {
    let base = driver.base().await?;
    let wire = driver.wire();
    match wire {
        rpc::Wire::V1 => {
            let listing: api::SubagentListValue = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call(
                    &driver.auth,
                    &base,
                    wire,
                    &rpc::WireReq::new(
                        ("subagent.list", serde_json::json!({"parentSessionId": id})),
                        ("", serde_json::Value::Null),
                    ),
                ),
            )
            .await??;
            Ok(listing.entries.iter().filter_map(subagent_entry).collect())
        }
        rpc::Wire::V2 => {
            let listing: RpcListValue = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call(
                    &driver.auth,
                    &base,
                    wire,
                    &rpc::WireReq::new(
                        ("", serde_json::Value::Null),
                        (
                            "session/list",
                            serde_json::json!({"args": {"_request": {}}}),
                        ),
                    ),
                ),
            )
            .await??;
            Ok(listing
                .items
                .iter()
                .filter(|s| {
                    s.origin.as_deref() == Some("subagent")
                        && s.parent_session_id.as_deref() == Some(id)
                })
                .map(|s| SubagentEntry {
                    agent_id: s.session_id.clone(),
                    name: s.title(),
                    agent_type: "subagent".into(),
                    description: None,
                    model: None,
                    status: if s.running { "running" } else { "inactive" }.to_string(),
                    started_at: None,
                    last_active_at: None,
                })
                .collect())
        }
    }
}

// —— 会话外观三透传（需求决策 6）：清单 llm.models、当前读 session.models、
// 设置 session.selectModel；hub 不存状态 ——

impl ModelSettings for DshDriver {
    fn model_caps(&self) -> ModelCaps {
        ModelCaps {
            can_set: true,
            can_read: true,
            supports_thinking: true,
        }
    }

    fn list_models<'a>(&'a self) -> BoxFuture<'a, Result<Vec<ModelInfo>>> {
        Box::pin(async move {
            let base = self.base().await?;
            let wire = self.wire();
            // V1 llm.models {groups}；V2 session/modelCatalog {default,groups}
            let m: api::LlmModels = match wire {
                rpc::Wire::V1 => {
                    tokio::time::timeout(
                        REQ_TIMEOUT,
                        rpc::call(
                            &self.auth,
                            &base,
                            wire,
                            &rpc::WireReq::new(
                                ("llm.models", serde_json::json!({})),
                                ("", serde_json::Value::Null),
                            ),
                        ),
                    )
                    .await??
                }
                rpc::Wire::V2 => {
                    let c: Result<api::ModelCatalog> = tokio::time::timeout(
                        REQ_TIMEOUT,
                        rpc::call(
                            &self.auth,
                            &base,
                            wire,
                            &rpc::WireReq::new(
                                ("", serde_json::Value::Null),
                                ("session/modelCatalog", serde_json::json!({"args": {}})),
                            ),
                        ),
                    )
                    .await
                    .map_err(|_| anyhow!("dsh session/modelCatalog 超时"))
                    .and_then(|inner| inner);
                    // 目录读失败 fail-soft（空组 = 面板空清单，不炸）
                    let c = c.unwrap_or(api::ModelCatalog {
                        default: None,
                        groups: Vec::new(),
                    });
                    api::LlmModels { groups: c.groups }
                }
            };
            let mut out: Vec<ModelInfo> = m
                .groups
                .into_iter()
                .flat_map(|g| {
                    let provider = g.id;
                    g.models.into_iter().map(move |model| ModelInfo {
                        // 目录无免费标注——dsh 侧如实全 false（需求决策 6「标注如实」）
                        free: false,
                        variants: model
                            .reasoning
                            .map(|r| r.efforts.into_iter().map(|e| e.id).collect())
                            .unwrap_or_default(),
                        provider: provider.clone(),
                        name: if model.name.is_empty() {
                            model.id.clone()
                        } else {
                            model.name
                        },
                        id: model.id,
                    })
                })
                .collect();
            out.sort_by(|a, b| a.provider.cmp(&b.provider).then_with(|| a.id.cmp(&b.id)));
            Ok(out)
        })
    }

    fn session_model<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<SessionModel>>> {
        Box::pin(async move {
            let base = self.base().await?;
            if self.wire() == rpc::Wire::V1 {
                let m: api::SessionModels = tokio::time::timeout(
                    REQ_TIMEOUT,
                    rpc::call(
                        &self.auth,
                        &base,
                        rpc::Wire::V1,
                        &rpc::WireReq::new(
                            ("session.models", serde_json::json!({"sessionId": id})),
                            ("", serde_json::Value::Null),
                        ),
                    ),
                )
                .await??;
                return Ok(m.current.map(|c| SessionModel {
                    id: c.model,
                    provider: c.provider,
                    variant: c.reasoning_effort,
                }));
            }
            // 0.2.0：当前模型 = session/list 条目的 modelSelection 投影（hub 不存状态）
            let v: RpcListValue = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call(
                    &self.auth,
                    &base,
                    rpc::Wire::V2,
                    &rpc::WireReq::new(
                        ("", serde_json::Value::Null),
                        (
                            "session/list",
                            serde_json::json!({"args": {"_request": {}}}),
                        ),
                    ),
                ),
            )
            .await??;
            let sel = v
                .items
                .iter()
                .find(|s| s.session_id == id)
                .and_then(|s| s.projections.as_ref())
                .and_then(|p| p.values.get("modelSelection"))
                .cloned();
            if let Some(sel) = sel {
                let sel: api::ModelSelection = serde_json::from_value(sel)
                    .map_err(|e| anyhow!("解析 modelSelection 投影失败: {e}"))?;
                return Ok(Some(SessionModel {
                    id: sel.model,
                    provider: sel.provider,
                    variant: sel.reasoning_effort,
                }));
            }
            // 投影缺省 → 目录 default（宿主默认，如实标注）
            let c: api::ModelCatalog = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call(
                    &self.auth,
                    &base,
                    rpc::Wire::V2,
                    &rpc::WireReq::new(
                        ("", serde_json::Value::Null),
                        ("session/modelCatalog", serde_json::json!({"args": {}})),
                    ),
                ),
            )
            .await??;
            Ok(c.default.map(|d| SessionModel {
                id: d.model,
                provider: d.provider,
                variant: d.reasoning_effort,
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
            let base = self.base().await?;
            // provider 归属：目录回查（模型 id → 组 id）；查不到回退目录 default
            let m: api::ModelCatalog = tokio::time::timeout(
                REQ_TIMEOUT,
                rpc::call(
                    &self.auth,
                    &base,
                    rpc::Wire::V2,
                    &rpc::WireReq::new(
                        ("", serde_json::Value::Null),
                        ("session/modelCatalog", serde_json::json!({"args": {}})),
                    ),
                ),
            )
            .await??;
            let provider = m
                .groups
                .iter()
                .find(|g| g.models.iter().any(|x| x.id == model))
                .map(|g| g.id.clone());
            let provider = match provider {
                Some(p) => p,
                None => m
                    .default
                    .map(|c| c.provider)
                    .ok_or_else(|| anyhow!("dsh 目录中无模型 {model} 且目录无 default provider"))?,
            };
            let payload = serde_json::json!({
                "sessionId": id,
                "provider": provider,
                "model": model,
                "reasoningEffort": variant,
            });
            let wire = self.wire();
            let sel_req = rpc::WireReq::new(
                ("session.selectModel", payload.clone()),
                (
                    "session/selectModel",
                    serde_json::json!({"args": {"request": payload}}),
                ),
            );
            let _: serde_json::Value =
                tokio::time::timeout(REQ_TIMEOUT, rpc::call(&self.auth, &base, wire, &sel_req))
                    .await??;
            Ok("已切换".into())
        })
    }
}

impl AgentDriver for DshDriver {
    fn name(&self) -> &'static str {
        "dsh"
    }

    fn auth_error(&self) -> bool {
        DshDriver::auth_error(self)
    }
}

/// 轨一 MessageSender（message_routes 表契约；委托 SessionControl）。
impl crate::drivers::MessageSender for DshDriver {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试驱动构造：bin 指向不存在路径——ensure_started 一旦因 fake 缺健康路由
    /// 落到 spawn 分支即报错（穿透即炸），绝不静默拉起真 dsh（测试纪律）。
    fn driver_with_dir(dir: &tempfile::TempDir) -> DshDriver {
        DshDriver::new(
            serve::ServeManager::new(
                "/nonexistent/dsh-test-isolated".into(),
                4097,
                serve::ServeManager::state_file_for(dir.path()),
                false,
            ),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
            rpc::Auth::default(),
        )
    }

    /// fake RPC 回包（回显请求 rpcId——客户端做信封回声校验，硬编码必失配）。
    fn rpc_ok(req: serde_json::Value, value: serde_json::Value) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "type": "server-response", "rpcId": req["rpcId"],
            "result": {"ok": true, "value": value}}))
    }

    /// echo 版 describe（与 rpc_ok 同理回显）。
    async fn fake_describe_echo(
        req: axum::extract::Json<serde_json::Value>,
    ) -> axum::Json<serde_json::Value> {
        rpc_ok(
            req.0,
            serde_json::json!({"version": "0.0.1", "cwd": "/repo",
                                         "provider": "deepseek-official",
                                         "model": "deepseek-v4-flash"}),
        )
    }

    /// 展示纪律：running→侧栏（Working），其余仅 all=1（Completed、最近活动倒序）
    #[tokio::test]
    async fn list_busy_only_and_history_opt_in() {
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
            "/api/session/list",
            axum::routing::post(
                |req: axum::extract::Json<serde_json::Value>| async move {
                    let now_ms =
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs_f64()
                            * 1000.0;
                    rpc_ok(req.0, serde_json::json!({"items": [
                        {"sessionId": "session-run", "agentAvailable": false, "blank": false, "updatedAt": now_ms, "running": true,
                         "cwd": "/repo/a", "projections": {"values": {"title": "busy"}}},
                        {"sessionId": "session-recent", "agentAvailable": false, "blank": false, "updatedAt": now_ms - 2.0*3600.0*1000.0,
                         "running": false, "cwd": "/repo/b",
                         "projections": {"values": {"title": "recent"}}},
                        {"sessionId": "session-dead", "agentAvailable": false, "blank": false, "updatedAt": now_ms - 25.0*3600.0*1000.0,
                         "running": false, "cwd": "/repo/c", "projections": {"values": {"title": "old"}}}
                    ]}))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        // 三档（2026-10-03 补档）：running → 侧栏 Working；recent（2h）→ 侧栏
        // Other；dead（25h）→ 仅 all=1 历史
        let listed = AgentCatalog::list(&d, false).await.unwrap();
        assert_eq!(listed.len(), 2, "默认列 running + recently-alive");
        assert_eq!(listed[0].id, "session-run");
        assert_eq!(listed[0].group, crate::models::Group::Working);
        assert_eq!(listed[1].id, "session-recent");
        assert_eq!(listed[1].group, crate::models::Group::Other);

        let listed = AgentCatalog::list(&d, true).await.unwrap();
        assert_eq!(listed.len(), 3, "all=1 收编历史");
        assert_eq!(listed[2].id, "session-dead");
        assert_eq!(listed[2].group, crate::models::Group::Completed);
    }

    /// fail-soft：serve 不可用 → 清单空集不炸（不拖死侧栏聚合）
    #[tokio::test]
    async fn list_soft_fails_when_serve_down() {
        let dir = tempfile::tempdir().unwrap();
        // bin 指向不存在 + 状态文件无记录 → 拉起必败
        let d = DshDriver::new(
            serve::ServeManager::new(
                "/nonexistent/dsh-test-isolated".into(),
                65001,
                serve::ServeManager::state_file_for(dir.path()),
                false,
            ),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
            rpc::Auth::default(),
        );
        let listed = AgentCatalog::list(&d, false).await.unwrap();
        assert!(listed.is_empty());
    }

    /// history corruption 容错：ok:false → 「不可回放」占位页（会话不炸、可继续）
    #[tokio::test]
    async fn page_degrades_to_non_replayable_on_corruption() {
        let app = axum::Router::new()
            .route(
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(
                        req.0,
                        serde_json::json!({"items": [{"sessionId": "session-bad", "agentAvailable": false, "blank": false,
                            "updatedAt": 1.0, "running": false, "cwd": "/repo",
                            "projections": {"asOfSeq": 5, "values": {}}}]}),
                    )
                }),
            )
            .route(
                "/api/session/page",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    assert_eq!(req.0["method"], "session/page");
                    let args = &req.0["payload"]["args"]["request"];
                    assert_eq!(args["address"]["sessionId"], "session-bad");
                    assert_eq!(
                        args["throughSeq"],
                        serde_json::json!(5),
                        "throughSeq 必须取投影 asOfSeq（严格游标）"
                    );
                    axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": req.0["rpcId"],
                            "result": {"ok": false, "error": {
                                "code": "SessionPersistenceCorruptionError",
                                "message": "schema validation failed"}}}))
                }),
            )
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        let page = SessionHistory::page(&d, "session-bad", 50, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(page.messages.len(), 1);
        assert!(
            page.messages[0]
                .text
                .as_deref()
                .unwrap()
                .contains("不可回放"),
            "占位卡应说明不可回放"
        );
    }

    /// dsh 0.2.0 迁移器拒绝旧格式日志（急修批次 2026-10-03）：page 失败带迁移
    /// 签名 → 会话标「旧版格式」——正文占位卡说明 dsh 侧限制，清单名加前缀标
    #[tokio::test]
    async fn page_marks_legacy_format_when_migrator_refuses() {
        let app = axum::Router::new()
            .route(
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(
                        req.0,
                        serde_json::json!({"items": [{"sessionId": "session-legacy", "agentAvailable": false, "blank": false,
                            "updatedAt": 1.0, "running": false, "cwd": "/repo",
                            "projections": {"asOfSeq": 3, "values": {"title": "old one"}}}]}),
                    )
                }),
            )
            .route(
                "/api/session/page",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": req.0["rpcId"],
                            "result": {"ok": false, "error": {
                                "code": "MigrationError",
                                "message": "migrator refuses this format (tool-call-delta)"}}}))
                }),
            )
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        let page = SessionHistory::page(&d, "session-legacy", 50, None)
            .await
            .unwrap()
            .unwrap();
        assert!(
            page.messages[0]
                .text
                .as_deref()
                .unwrap()
                .contains("旧版格式"),
            "占位卡应说明旧版格式（dsh 侧迁移限制）"
        );
        // 清单标记：Past sessions 名字加「【旧版格式】」前缀
        let listed = AgentCatalog::list(&d, true).await.unwrap();
        let legacy = listed.iter().find(|a| a.id == "session-legacy").unwrap();
        assert!(
            legacy.name.as_deref().unwrap().starts_with("【旧版格式】"),
            "清单名应带旧版格式标，got {:?}",
            legacy.name
        );
    }

    /// 拉隔离真 dsh（DSH_HOME=不回收的 /tmp 临时目录——dsh 进程存活期内必须
    /// 在，由系统 /tmp 清理兜底）；读 stdout 抓 launch token。返回（子进程, 端口,
    /// token）。
    async fn start_isolated_dsh(bin: &str) -> (tokio::process::Child, u16, String) {
        let home = tempfile::tempdir().unwrap().keep();
        start_isolated_dsh_in(bin, &home).await
    }

    /// 同上但复用既有 DSH_HOME（跨进程重启实验用）。
    async fn start_isolated_dsh_in(
        bin: &str,
        home: &std::path::Path,
    ) -> (tokio::process::Child, u16, String) {
        // 抓空闲端口：绑 0 拿号即弃（dsh --port 也支持 0，但 URL 行解析需要已知端口）
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut child = tokio::process::Command::new(bin)
            .args(["web", "--port", &port.to_string(), "--no-open"])
            .env("DSH_HOME", home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("拉起隔离真 dsh 失败");
        // 读 stdout 直到 dsh web: URL 行；此后转排水（防管道写满阻塞 dsh）
        let stdout = child.stdout.take().expect("dsh stdout");
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut token: Option<String> = None;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while token.is_none() && std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(2), lines.next_line()).await {
                Ok(Ok(Some(line))) => token = serve::launch_token_of(&line),
                Ok(Ok(None)) => break,
                Ok(Err(_)) | Err(_) => continue,
            }
        }
        let token = token.expect("隔离 dsh 未打出 launch token（30s）");
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        (child, port, token)
    }

    /// 隔离真 dsh 实测（0.2.0 防假阴性，2026-10-03 急修）：DSH_HOME=临时目录拉真
    /// `dsh web` → hub 同款信封 create + prompt（v4 UUID requestId）→ 轮询
    /// session/page 直到 user/message 落盘。fake 断言对信封形状自证，这条对
    /// **真网关**验证（历史教训：非 UUID requestId 被 codec 静默吞，accepted:true
    /// 假阳性零落盘）。dsh bin 不存在时跳过（CI 安全）。
    #[tokio::test]
    async fn real_dsh_prompt_persists_user_message() {
        let Some(bin) = real_dsh_bin() else {
            eprintln!("skip: dsh bin 不存在（CI 安全跳过）");
            return;
        };
        let ws = tempfile::tempdir().unwrap();
        let (mut child, port, token) = start_isolated_dsh(&bin).await;
        let pid = child.id();
        let probe = run_real_dsh_probe(port, &token, ws.path()).await;
        // 无论断言成败都收掉自己拉起的进程（只杀自己 spawn 的这个 pid）
        if let Some(pid) = pid {
            crate::platform::kill_pid(pid, crate::platform::Signal::Term);
        }
        let _ = child.wait().await;
        probe.expect("隔离真 dsh 探针断言失败");
    }

    /// WS 认证回归（遗留一，2026-10-03）：HTTP 全通唯 WS 401 的根因 = dsh
    /// cookie 名绑定 Host authority（browser-auth.ts cookieName=sha256(authority)），
    /// hub HTTP 全链覆写 Host=127.0.0.1（rpc.rs），WS 拨号必须同 authority。
    /// 锁：HTTP 200 后泵同款拨号 10s 内连上 remote.mux 并收到流帧。
    #[tokio::test]
    async fn real_dsh_mux_ws_accepts_hub_cookie() {
        let Some(bin) = real_dsh_bin() else {
            eprintln!("skip: dsh bin 不存在（CI 安全跳过）");
            return;
        };
        let (mut child, port, token) = start_isolated_dsh(&bin).await;
        let result = run_ws_probe(port, &token).await;
        if let Some(pid) = child.id() {
            crate::platform::kill_pid(pid, crate::platform::Signal::Term);
        }
        let _ = child.wait().await;
        result.expect("隔离真 dsh WS 探针断言失败");
    }

    /// WS 探针：HTTP session/list 200（Auth 链自动换 cookie）→ 泵同款拨号
    /// （Host 覆写 + cookie）→ open control 流 → 10s 内收到 `{type:"item"}` 帧。
    async fn run_ws_probe(port: u16, token: &str) -> Result<()> {
        use futures::StreamExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        use tokio_tungstenite::tungstenite::http::{header::COOKIE, header::HOST, HeaderValue};
        use tokio_tungstenite::tungstenite::Message;
        let base = format!("http://127.0.0.1:{port}");
        let auth = rpc::Auth::default();
        auth.set_token(token.to_string());
        // 前置：HTTP 链通（401 → Auth 自动重换 cookie → 200）
        let _: serde_json::Value = rpc::call(
            &auth,
            &base,
            rpc::Wire::V2,
            &rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            ),
        )
        .await?;
        let cookie = auth
            .cookie()
            .ok_or_else(|| anyhow!("session/list 200 后 Auth 无 cookie"))?;
        // 泵同款拨号（Host authority 覆写 + cookie 随行）
        let mut req = IntoClientRequest::into_client_request(
            format!("ws://127.0.0.1:{port}/api/remote.mux").as_str(),
        )?;
        req.headers_mut()
            .insert(HOST, HeaderValue::from_static("127.0.0.1"));
        req.headers_mut()
            .insert(COOKIE, HeaderValue::from_str(&cookie)?);
        let (mut ws, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(req),
        )
        .await
        .map_err(|_| anyhow!("WS 10s 内未连上（401 循环？）"))??;
        // open control 流（泵同款）——连接成功 ≠ 有数据，以收到流帧为准
        let open = serde_json::json!({
            "type": "open", "streamId": "control",
            "endpoint": "session/control", "payload": {"args": {}},
        });
        use futures::SinkExt;
        ws.send(Message::Text(open.to_string().into())).await?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    let v: serde_json::Value = serde_json::from_str(t.to_string().as_str())?;
                    if v["type"] == "item" {
                        eprintln!("PASS: WS 已连上且收到流帧（control baseline/projection item）");
                        return Ok(());
                    }
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(e))) => return Err(anyhow!("WS 读帧失败: {e}")),
                Ok(None) => return Err(anyhow!("WS 已关闭且未见流帧")),
                Err(_) => return Err(anyhow!("WS 10s 内未收到任何流帧")),
            }
        }
        Err(anyhow!("WS 未见流帧（超时）"))
    }

    async fn run_real_dsh_probe(port: u16, token: &str, ws_cwd: &std::path::Path) -> Result<()> {
        let base = format!("http://127.0.0.1:{port}");
        let auth = rpc::Auth::default();
        auth.set_token(token.to_string());
        // create（cwd=探针工作目录）——401 自动换 cookie 走 Auth 链
        let created: serde_json::Value = rpc::call(
            &auth,
            &base,
            rpc::Wire::V2,
            &rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/create",
                    serde_json::json!({"args": {"request": {
                        "cwd": ws_cwd.to_string_lossy(),
                    }}}),
                ),
            ),
        )
        .await?;
        let sid = created["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("create 无 sessionId: {created}"))?
            .to_string();
        // prompt：hub 同款形状（v4 UUID requestId——根因修复点）
        let _prompted: serde_json::Value = rpc::call(
            &auth,
            &base,
            rpc::Wire::V2,
            &rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/prompt",
                    serde_json::json!({"args": {"request": {
                        "requestId": rpc::rpc_uuid(),
                        "sessionId": sid,
                        "mode": "queue",
                        "content": [{"type": "text", "text": "hub probe ping"}],
                    }}}),
                ),
            ),
        )
        .await?;
        // 轮询 page 直到 user/message 落盘（真存储事实源）
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let Ok(list) = rpc::call::<serde_json::Value>(
                &auth,
                &base,
                rpc::Wire::V2,
                &rpc::WireReq::new(
                    ("", serde_json::Value::Null),
                    (
                        "session/list",
                        serde_json::json!({"args": {"_request": {}}}),
                    ),
                ),
            )
            .await
            else {
                continue;
            };
            let as_of = list["items"]
                .as_array()
                .and_then(|items| items.iter().find(|i| i["sessionId"].as_str() == Some(&sid)))
                .and_then(|i| i["projections"]["asOfSeq"].as_i64())
                .unwrap_or(0);
            let Ok(page) = rpc::call::<api::PageValue>(
                &auth,
                &base,
                rpc::Wire::V2,
                &rpc::WireReq::new(
                    ("", serde_json::Value::Null),
                    (
                        "session/page",
                        serde_json::json!({"args": {"request": {
                            "address": {"kind": "session", "sessionId": sid},
                            "throughSeq": as_of,
                        }}}),
                    ),
                ),
            )
            .await
            else {
                continue;
            };
            if page
                .records
                .iter()
                .any(|r| r.event.event_type == "user/message")
            {
                eprintln!("PASS: user/message 已落盘（session/{sid}，asOfSeq={as_of}）");
                return Ok(());
            }
        }
        Err(anyhow!(
            "30s 内 session/page 未见 user/message——prompt 静默丢失"
        ))
    }

    /// hub 同款 prompt 信封（v4 UUID requestId + queue 模式 + text part）。
    async fn hub_prompt(
        auth: &rpc::Auth,
        base: &str,
        sid: &str,
        text: &str,
    ) -> Result<serde_json::Value> {
        rpc::call(
            auth,
            base,
            rpc::Wire::V2,
            &rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/prompt",
                    serde_json::json!({"args": {"request": {
                        "requestId": rpc::rpc_uuid(),
                        "sessionId": sid,
                        "mode": "queue",
                        "content": [{"type": "text", "text": text}],
                    }}}),
                ),
            ),
        )
        .await
    }

    /// 读 page 统计 user/message 事件数（轮询断言的事实源）。
    async fn user_message_count(auth: &rpc::Auth, base: &str, sid: &str) -> usize {
        let Ok(list) = rpc::call::<serde_json::Value>(
            auth,
            base,
            rpc::Wire::V2,
            &rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            ),
        )
        .await
        else {
            return 0;
        };
        let as_of = list["items"]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["sessionId"].as_str() == Some(sid)))
            .and_then(|i| i["projections"]["asOfSeq"].as_i64())
            .unwrap_or(0);
        let Ok(page) = rpc::call::<api::PageValue>(
            auth,
            base,
            rpc::Wire::V2,
            &rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/page",
                    serde_json::json!({"args": {"request": {
                        "address": {"kind": "session", "sessionId": sid},
                        "throughSeq": as_of,
                    }}}),
                ),
            ),
        )
        .await
        else {
            return 0;
        };
        page.records
            .iter()
            .filter(|r| r.event.event_type == "user/message")
            .count()
    }

    /// 跨进程重启 prompt 落盘（第三轮 2026-10-03）：进程 A 种一条 user/message →
    /// 杀进程 → 同 DSH_HOME 起进程 B → hub 同款 prompt 旧会话 → 存储必须新增
    /// user/message。这正是用户场景（对上进程遗留会话发消息）。
    #[tokio::test]
    async fn real_dsh_prompt_persists_across_process_restart() {
        let Some(bin) = real_dsh_bin() else {
            eprintln!("skip: dsh bin 不存在（CI 安全跳过）");
            return;
        };
        let home = tempfile::tempdir().unwrap().keep();
        // 进程 A：种会话 + 种一条 user/message
        let (mut a, port_a, token_a) = start_isolated_dsh_in(&bin, &home).await;
        let base_a = format!("http://127.0.0.1:{port_a}");
        let auth_a = rpc::Auth::default();
        auth_a.set_token(token_a);
        let created: serde_json::Value = rpc::call(
            &auth_a,
            &base_a,
            rpc::Wire::V2,
            &rpc::WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/create",
                    serde_json::json!({"args": {"request": {
                        "cwd": home.to_string_lossy(),
                    }}}),
                ),
            ),
        )
        .await
        .expect("进程 A create 失败");
        let sid = created["sessionId"].as_str().unwrap().to_string();
        hub_prompt(&auth_a, &base_a, &sid, "seed from process A")
            .await
            .expect("进程 A 种子 prompt 失败");
        let mut seeded = 0;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            seeded = user_message_count(&auth_a, &base_a, &sid).await;
            if seeded > 0 {
                break;
            }
        }
        assert!(seeded > 0, "进程 A 种子 user/message 未落盘");
        // 收 A，等同锁释放
        if let Some(pid) = a.id() {
            crate::platform::kill_pid(pid, crate::platform::Signal::Term);
        }
        let _ = a.wait().await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        // 进程 B：hub 同款 prompt 遗留会话 → 存储必须新增
        let (mut b, port_b, token_b) = start_isolated_dsh_in(&bin, &home).await;
        let base_b = format!("http://127.0.0.1:{port_b}");
        let auth_b = rpc::Auth::default();
        auth_b.set_token(token_b);
        let outcome = async {
            hub_prompt(&auth_b, &base_b, &sid, "restart probe from hub").await?;
            for _ in 0..40 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if user_message_count(&auth_b, &base_b, &sid).await > seeded {
                    eprintln!(
                        "PASS: 遗留会话 prompt 已落盘（session/{sid}，user/message {seeded}→+1）"
                    );
                    return Ok(());
                }
            }
            Err(anyhow!("重启后 hub prompt 30s 未新增 user/message"))
        }
        .await;
        if let Some(pid) = b.id() {
            crate::platform::kill_pid(pid, crate::platform::Signal::Term);
        }
        let _ = b.wait().await;
        outcome.expect("跨进程重启 prompt 探针失败");
    }

    /// 归档语义实证（两连 bug ② 2026-10-03）：真 dsh archiveSession 后
    /// session/list **仍含**该会话（归档不排清单——ArchivedSessionGate 只挡
    /// agent 步进）——hub 必须自滤。此测试锁定该语义防 dsh 升级漂移。
    #[tokio::test]
    async fn real_dsh_archive_removes_session_from_list() {
        let Some(bin) = real_dsh_bin() else {
            eprintln!("skip: dsh bin 不存在（CI 安全跳过）");
            return;
        };
        let (mut child, port, token) = start_isolated_dsh(&bin).await;
        let ws = tempfile::tempdir().unwrap();
        let outcome = async {
            let base = format!("http://127.0.0.1:{port}");
            let auth = rpc::Auth::default();
            auth.set_token(token.clone());
            let created: serde_json::Value = rpc::call(
                &auth,
                &base,
                rpc::Wire::V2,
                &rpc::WireReq::new(
                    ("", serde_json::Value::Null),
                    (
                        "session/create",
                        serde_json::json!({"args": {"request": {
                            "cwd": ws.path().to_string_lossy(),
                        }}}),
                    ),
                ),
            )
            .await?;
            let sid = created["sessionId"].as_str().unwrap().to_string();
            let _: serde_json::Value = rpc::call(
                &auth,
                &base,
                rpc::Wire::V2,
                &rpc::WireReq::new(
                    ("", serde_json::Value::Null),
                    (
                        "workspace/archiveSession",
                        serde_json::json!({"args": {"request": {
                            "sessionId": sid, "stopActivity": true,
                        }}}),
                    ),
                ),
            )
            .await?;
            // 轮询 list 直到消失（归档写盘可能略有延迟）
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let listed: serde_json::Value = rpc::call(
                    &auth,
                    &base,
                    rpc::Wire::V2,
                    &rpc::WireReq::new(
                        ("", serde_json::Value::Null),
                        (
                            "session/list",
                            serde_json::json!({"args": {"_request": {}}}),
                        ),
                    ),
                )
                .await?;
                let present = listed["items"]
                    .as_array()
                    .map(|items| items.iter().any(|i| i["sessionId"].as_str() == Some(&sid)))
                    .unwrap_or(true);
                if present {
                    eprintln!(
                        "PASS: 归档后 session/list 仍含该会话（{sid}）——dsh 不排清单，hub 自滤口径成立"
                    );
                    return Ok(());
                }
            }
            Err(anyhow!("归档后会话从 session/list 消失——dsh 语义漂移，hub 自滤可撤"))
        }
        .await;
        if let Some(pid) = child.id() {
            crate::platform::kill_pid(pid, crate::platform::Signal::Term);
        }
        let _ = child.wait().await;
        outcome.expect("真 dsh 归档探针失败");
    }

    /// 找 dsh bin（常见安装位候选 + PATH 查找，均归驱动层），找不到 → None（测试跳过）。
    fn real_dsh_bin() -> Option<String> {
        crate::platform::bin_candidates("dsh")
            .into_iter()
            .find(|p| p.exists())
            .or_else(|| crate::platform::find_in_path("dsh"))
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// rm → workspace.archiveSession 形状（两连 bug ② 2026-10-03）：dsh 删除语义
    /// = 归档；fake 记录断言 method + args.request.{sessionId,stopActivity}
    #[tokio::test]
    async fn remove_posts_archive_session_envelope() {
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen_c = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
                // 探测依据：session/list 有响应 → V2（缺此路由会被判 V1）
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(
                        req.0,
                        serde_json::json!({"items": [{"sessionId": "session-x",
                            "updatedAt": 1.0, "running": false, "cwd": "/repo",
                            "agentAvailable": false, "blank": false}]}),
                    )
                }),
            )
            .route(
                "/api/workspace/archiveSession",
                axum::routing::post(move |req: axum::extract::Json<serde_json::Value>| {
                    let seen = seen_c.clone();
                    async move {
                        seen.lock().expect("seen lock").push(req.0.clone());
                        rpc_ok(
                            req.0,
                            serde_json::json!({"archivedSessionIds": ["session-x"]}),
                        )
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        AgentCatalog::remove(&d, "session-x").await.unwrap();
        {
            let seen = seen.lock().expect("seen lock");
            assert_eq!(seen.len(), 1);
            let req = &seen[0];
            assert_eq!(req["method"], "workspace/archiveSession");
            assert_eq!(req["payload"]["args"]["request"]["sessionId"], "session-x");
            assert_eq!(
                req["payload"]["args"]["request"]["stopActivity"],
                serde_json::json!(true),
                "归档须带 stopActivity（先停活跃工作）"
            );
        } // guard 先放（clippy：不跨 await）
          // hub 自滤：归档集回写后 list 不再含该会话（dsh 不排归档，hub 滤）
        let listed = AgentCatalog::list(&d, true).await.unwrap();
        assert!(
            !listed.iter().any(|a| a.id == "session-x"),
            "归档后的会话不应再出现在 hub 清单"
        );
    }

    /// $events/result 应答信封（ask_user_question 批次）：answer_event 必须发
    /// {args:{clientId,eventId,outcome:{kind:'result',value}}}——clientId 来自
    /// ready 帧登记，eventId 定位 pending；成功后摘登记
    #[tokio::test]
    async fn answer_question_posts_events_result_envelope() {
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen_c = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(req.0, serde_json::json!({"items": []}))
                }),
            )
            .route(
                "/api/$events/result",
                axum::routing::post(move |req: axum::extract::Json<serde_json::Value>| {
                    let seen = seen_c.clone();
                    async move {
                        seen.lock().expect("seen lock").push(req.0.clone());
                        rpc_ok(req.0, serde_json::Value::Null)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        d.set_events_client("cli-1");
        d.register_question(
            "session-x",
            "evt-1",
            &serde_json::json!([{"id": "q1", "question": "继续？"}]),
        );
        let answers = serde_json::json!({"answers": [{"id": "q1", "selected": ["继续"]}]});
        // 走 trait BoxFuture 路径（回归嵌套 runtime panic：block_on 桥接在
        // tokio worker 上必崩，原生异步后此测试即真实调用形态）
        SessionControl::answer_question(&d, "session-x", "evt-1", answers)
            .await
            .expect("answer_event 应成功");
        let seen = seen.lock().expect("seen lock");
        assert_eq!(seen.len(), 1);
        let req = &seen[0];
        assert_eq!(req["method"], "$events/result");
        assert_eq!(req["payload"]["args"]["clientId"], "cli-1");
        assert_eq!(req["payload"]["args"]["eventId"], "evt-1");
        assert_eq!(
            req["payload"]["args"]["outcome"]["kind"],
            serde_json::json!("result")
        );
        assert_eq!(
            req["payload"]["args"]["outcome"]["value"]["answers"][0]["id"], "q1",
            "answers 原样入 outcome.value（AskUserQuestionAnswer）"
        );
    }

    /// 提问卡 page 回填验收（第六轮收尾）：waterfall 到达时房间未开（pushed=0）
    /// → 打开（page）→ pending 提问卡必须出现在页尾——用户实弹「打开会话卡仍在」
    #[tokio::test]
    async fn page_backfills_pending_question_cards() {
        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        // 房间未开：register 走 push_chat 落空（pushed=0 路径）
        d.register_question(
            "session-x",
            "evt-9",
            &serde_json::json!([{"id": "q1", "question": "A 还是 B？",
                "options": [{"label": "A"}, {"label": "B"}]}]),
        );
        let page = SessionHistory::page(&d, "session-x", 50, None)
            .await
            .unwrap()
            .unwrap();
        let card = page
            .messages
            .iter()
            .find(|m| m.raw_type.as_deref() == Some("dsh_question"))
            .expect("page 必须回填 pending 提问卡");
        let input = card.input.as_ref().unwrap();
        assert_eq!(input["eventId"], "evt-9");
        assert_eq!(input["questions"][0]["id"], "q1", "questions 原文随页回填");
        assert_eq!(
            input["questions"][0]["options"].as_array().map(|o| o.len()),
            Some(2),
            "选项随页回填（提问卡渲染事实源）"
        );
    }

    /// send 注入形状：session.prompt 收到 queue 信封 + text content（fake 记录断言）
    #[tokio::test]
    async fn send_posts_prompt_envelope_with_queue_mode() {
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen_c = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(req.0, serde_json::json!({"items": []}))
                }),
            )
            .route(
                "/api/session/prompt",
                axum::routing::post(
                    move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                        let seen = seen_c.clone();
                        async move {
                            seen.lock().unwrap().push(body.clone());
                            axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": body["rpcId"],
                            "result": {"ok": true, "value": {"accepted": true}}}))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        SessionControl::send(&d, "session-1", "hi dsh")
            .await
            .unwrap();
        let reqs = seen.lock().unwrap();
        let req = &reqs[0];
        assert_eq!(req["type"], "client-request");
        assert_eq!(req["method"], "session/prompt");
        assert_eq!(req["payload"]["args"]["request"]["sessionId"], "session-1");
        assert_eq!(req["payload"]["args"]["request"]["mode"], "queue");
        assert!(
            req["payload"]["args"]["request"]["requestId"].is_string(),
            "0.2.0 必填 requestId"
        );
        assert_eq!(
            req["payload"]["args"]["request"]["content"][0]["type"],
            "text"
        );
        assert_eq!(
            req["payload"]["args"]["request"]["content"][0]["text"],
            "hi dsh"
        );
    }

    /// 审批链路：request 帧登记 + 推卡；answer → /api/respond allowed-once；
    /// resolved 帧摘除；未知 permId → not_found
    #[tokio::test]
    async fn approval_flow_requests_responds_and_resolves() {
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen_c = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
            "/api/respond",
            axum::routing::post(
                move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                    let seen = seen_c.clone();
                    async move {
                        seen.lock().unwrap().push(body.clone());
                        axum::Json(serde_json::json!({"type":"server-response","rpcId":body["rpcId"],"result":{"ok":true,"value":{}}}))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = broadcast::channel(16);
        let mut rooms = HashMap::new();
        let (room_tx, _) = broadcast::channel::<ChatEvent>(16);
        rooms.insert("session-1".to_string(), room_tx);
        let rooms = Arc::new(RwLock::new(rooms));
        let d = DshDriver::new(
            serve::ServeManager::new(
                "/nonexistent/dsh-test-isolated".into(),
                4097,
                serve::ServeManager::state_file_for(dir.path()),
                false,
            ),
            rooms,
            tx,
            rpc::Auth::default(),
        );
        d.serve.inject_state(Some(port), None);
        let mut room_rx = d.ensure_chat_room("session-1").subscribe();

        // approval/requested 帧 → 登记 + 推卡 + Tick
        events::handle_envelope(
            "rpc-1",
            &serde_json::json!({
                "type": "approval/requested", "sessionId": "session-1",
                "approvalId": "ap-1", "toolName": "bash", "callId": "c1", "reason": "run ls"
            }),
            &d,
        )
        .await;
        let card = room_rx.try_recv().unwrap().message;
        assert_eq!(card.raw_type.as_deref(), Some("acp_permission"));
        assert_eq!(card.tool_name.as_deref(), Some("bash"));
        let key = card.tool_use_id.unwrap();
        assert!(key.starts_with("perm:session-1:"));
        assert!(matches!(rx.try_recv(), Ok(HubEvent::Tick)));

        // 重复帧（重连重放）不重推卡
        events::handle_envelope(
            "rpc-1",
            &serde_json::json!({
                "type": "approval/requested", "sessionId": "session-1",
                "approvalId": "ap-1", "toolName": "bash"
            }),
            &d,
        )
        .await;
        assert!(room_rx.try_recv().is_err(), "重放不应重推卡");

        // 0.2.0 迁移注记：/api/respond 载体消失 → 应答显式 unsupported（如实不支
        // 持，弹卡链路保留）；登记仍可应答通道恢复后即时重接
        let d2 = d.clone();
        let key2 = key.clone();
        let e = tokio::task::spawn_blocking(move || {
            SessionControl::answer_permission(&d2, "session-1", &key2, Some("allowed-once"))
        })
        .await
        .unwrap()
        .unwrap_err();
        assert!(e.to_string().contains("0.2.0"), "{e}");
        assert!(
            seen.lock().unwrap().is_empty(),
            "迁移期不应发出 respond 调用"
        );

        // resolved 帧 → 摘除 + 回执卡
        events::handle_envelope(
            "rpc-2",
            &serde_json::json!({
                "type": "approval/resolved", "sessionId": "session-1",
                "approvalId": "ap-1", "outcome": "allowed-once"
            }),
            &d,
        )
        .await;
        let receipt = room_rx.try_recv().unwrap().message;
        assert_eq!(receipt.raw_type.as_deref(), Some("acp_permission_resolved"));
        // 收场后再登记：重放去重保持（不重推卡）
        events::handle_envelope(
            "rpc-1",
            &serde_json::json!({
                "type": "approval/requested", "sessionId": "session-1",
                "approvalId": "ap-1", "toolName": "bash"
            }),
            &d,
        )
        .await;
        assert!(room_rx.try_recv().is_err(), "重放不应重推卡");
    }

    /// subagent 清单：child 活跃过滤 + diagnostic 跳过 + finished 全量
    #[tokio::test]
    async fn subagents_filter_running_and_skip_diagnostics() {
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
                "/api/session/list",
                axum::routing::post(
                    |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                        assert_eq!(
                            body["payload"]["args"]["_request"],
                            serde_json::json!({})
                        );
                        rpc_ok(
                            body,
                            serde_json::json!({"items": [
                                {"sessionId": "session-p", "agentAvailable": false, "blank": false, "updatedAt": 1.0, "running": false, "cwd": "/p"},
                                {"sessionId": "session-s1", "agentAvailable": false, "blank": false, "updatedAt": 2.0, "running": true, "cwd": "/p",
                                 "origin": "subagent", "parentSessionId": "session-p"},
                                {"sessionId": "session-s2", "agentAvailable": false, "blank": false, "updatedAt": 3.0, "running": false, "cwd": "/p",
                                 "origin": "subagent", "parentSessionId": "session-p",
                                 "projections": {"values": {"title": "explorer"}}}
                            ]}),
                        )
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        let active = SessionHistory::subagents(&d, "session-p").await.unwrap();
        assert_eq!(active.len(), 1, "默认只回活跃");
        assert_eq!(active[0].agent_id, "session-s1");
        assert_eq!(active[0].status, "running");
        let all = SessionHistory::subagents_finished(&d, "session-p")
            .await
            .unwrap();
        assert_eq!(all.len(), 2, "全量含 inactive");
        assert_eq!(all[1].name, "explorer");
    }

    /// 模型/思考透传：清单 llm.models 扁平化、当前读 session.models、
    /// 设置 selectModel 的 provider 归属回查与 reasoningEffort 注入
    #[tokio::test]
    async fn model_settings_roundtrip_via_catalog() {
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen_c = seen.clone();
        // 线协议夹具走原始字符串（0.2.0 session/modelCatalog 实测形态），避开括号汤
        const LLM_MODELS_RESP: &str = r#"{"default":{"provider":"deepseek-official","model":"deepseek-v4-flash","reasoningEffort":"high"},"routableProviders":["deepseek-official"],"groups":[
            {"id":"deepseek-official","name":"DeepSeek","models":[
                {"id":"deepseek-v4-flash","name":"DeepSeek-V4-Flash",
                 "reasoning":{"efforts":[{"id":"off","name":"Off"},{"id":"low","name":"Low"},
                                          {"id":"high","name":"High"},{"id":"max","name":"Max"}],
                              "defaultEffort":"high"}},
                {"id":"deepseek-v4-pro","name":"DeepSeek-V4-Pro"}]}]}"#;
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(req.0, serde_json::json!({"items": []}))
                }),
            )
            .route(
                "/api/session/modelCatalog",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(
                        req.0,
                        serde_json::from_str::<serde_json::Value>(LLM_MODELS_RESP).unwrap(),
                    )
                }),
            );
        let app = app.route(
            "/api/session/selectModel",
            axum::routing::post(
                move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                    let seen = seen_c.clone();
                    async move {
                        seen.lock().unwrap().push(body.clone());
                        axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": body["rpcId"],
                            "result": {"ok": true, "value": {"selected": {
                                "provider": "deepseek-official", "model": "deepseek-v4-pro",
                                "reasoningEffort": "low"}}}}))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);

        let models = ModelSettings::list_models(&d).await.unwrap();
        assert_eq!(models.len(), 2);
        assert!(!models[0].free, "目录无免费标注 → 如实 false");
        assert_eq!(models[0].variants, vec!["off", "low", "high", "max"]);

        let cur = ModelSettings::session_model(&d, "session-1")
            .await
            .unwrap()
            .unwrap();
        // 0.2.0：投影缺省回退目录 default
        assert_eq!(cur.id, "deepseek-v4-flash");
        assert_eq!(cur.variant.as_deref(), Some("high"));

        let msg = ModelSettings::set_session_model(&d, "session-1", "deepseek-v4-pro", Some("low"))
            .await
            .unwrap();
        assert_eq!(msg, "已切换");
        let req = seen.lock().unwrap()[0].clone();
        assert_eq!(req["method"], "session/selectModel");
        assert_eq!(
            req["payload"]["args"]["request"]["provider"],
            "deepseek-official"
        );
        assert_eq!(
            req["payload"]["args"]["request"]["model"],
            "deepseek-v4-pro"
        );
        assert_eq!(req["payload"]["args"]["request"]["reasoningEffort"], "low");
    }

    /// start：session.create（cwd）→（prompt 非空时）send 链式
    #[tokio::test]
    async fn start_creates_session_and_sends_prompt() {
        let created = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let created_c = created.clone();
        let prompted = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let prompted_c = prompted.clone();
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(req.0, serde_json::json!({"items": []}))
                }),
            )
            .route(
                "/api/session/create",
                axum::routing::post(
                    move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                        let created = created_c.clone();
                        async move {
                            created.lock().unwrap().push(body.clone());
                            axum::Json(serde_json::json!({
                                "type": "server-response", "rpcId": body["rpcId"],
                                "result": {"ok": true, "value": {"sessionId": "session-new", "agentAvailable": false, "blank": false}}}))
                        }
                    },
                ),
            )
            .route(
                "/api/session/prompt",
                axum::routing::post(
                    move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                        let prompted = prompted_c.clone();
                        async move {
                            prompted.lock().unwrap().push(body.clone());
                            axum::Json(serde_json::json!({
                                "type": "server-response", "rpcId": body["rpcId"],
                                "result": {"ok": true, "value": {"accepted": true}}}))
                        }
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // axum accept 就绪

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        let spec = StartSpec {
            prompt: "make a plan".into(),
            name: None,
            model: None,
            effort: None,
            cwd: "/repo".into(),
        };
        let id = AgentCatalog::start(&d, &spec).await.unwrap();
        assert_eq!(id, "session-new");
        assert_eq!(
            created.lock().unwrap()[0]["payload"]["args"]["request"]["cwd"],
            "/repo"
        );
        assert_eq!(
            prompted.lock().unwrap()[0]["payload"]["args"]["request"]["content"][0]["text"],
            "make a plan"
        );
    }

    /// 【行为矩阵】无凭据 + 认证实例（全 401）→ 清单空集 + authError 置位，
    /// 不上抛（急修第二刀红线：单驱动失败绝不 500）
    #[tokio::test]
    async fn auth_failure_degrades_to_empty_and_marks() {
        let app = axum::Router::new().route(
            "/api/session/list",
            axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let dir = tempfile::tempdir().unwrap();
        let d = DshDriver::new(
            serve::ServeManager::new(
                "/nonexistent/dsh-test-isolated".into(),
                4097,
                serve::ServeManager::state_file_for(dir.path()),
                false,
            ),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
            rpc::Auth::default(),
        );
        d.serve.inject_state(Some(port), None);
        // 无凭据 + 认证实例：清单空集（Ok）+ authError 置位
        let listed = AgentCatalog::list(&d, false).await.unwrap();
        assert!(listed.is_empty(), "认证失败清单必须降级空集");
        assert!(d.auth_error(), "authError 标记必须置位");
        // page 同路径：authError 占位卡（可打开可继续，不炸链）
        let page = SessionHistory::page(&d, "session-1", 50, None)
            .await
            .unwrap()
            .unwrap();
        assert!(page.messages[0]
            .text
            .as_deref()
            .unwrap()
            .contains("认证失效"));
    }

    /// 【行为矩阵】有凭据（token 换 cookie）+ 认证实例 → 清单正常 + authError 清除
    #[tokio::test]
    async fn auth_with_token_lists_normally() {
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::get(|uri: axum::http::Uri| async move {
                    use axum::response::IntoResponse;
                    if uri
                        .query()
                        .map(|q| q.contains("token=tok-ok"))
                        .unwrap_or(false)
                    {
                        (
                            axum::http::StatusCode::SEE_OTHER,
                            [("set-cookie", "dsh-auth=ck1; Path=/")],
                        )
                            .into_response()
                    } else {
                        axum::http::StatusCode::UNAUTHORIZED.into_response()
                    }
                }),
            )
            .route(
                "/api/session/list",
                axum::routing::post(
                    |(headers, req): (
                        axum::http::HeaderMap,
                        axum::extract::Json<serde_json::Value>,
                    )| async move {
                        use axum::response::IntoResponse;
                        let authed = headers
                            .get(axum::http::header::COOKIE)
                            .and_then(|v| v.to_str().ok())
                            .map(|c| c.contains("dsh-auth=ck1"))
                            .unwrap_or(false);
                        if !authed {
                            return axum::http::StatusCode::UNAUTHORIZED.into_response();
                        }
                        axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": req.0["rpcId"],
                            "result": {"ok": true, "value": {"items": [
                                {"sessionId": "session-live", "agentAvailable": false, "blank": false, "updatedAt": 1791012345000.0,
                                 "running": true, "cwd": "/repo",
                                 "projections": {"values": {"title": "busy"}}}]}}}))
                        .into_response()
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let dir = tempfile::tempdir().unwrap();
        let d = DshDriver::new(
            serve::ServeManager::new(
                "/nonexistent/dsh-test-isolated".into(),
                4097,
                serve::ServeManager::state_file_for(dir.path()),
                false,
            ),
            Arc::new(RwLock::new(HashMap::new())),
            broadcast::channel(8).0,
            rpc::Auth::new(Some("tok-ok".into())),
        );
        d.serve.inject_state(Some(port), None);
        let listed = AgentCatalog::list(&d, false).await.unwrap();
        assert_eq!(listed.len(), 1, "带 token 认证应通过");
        assert_eq!(listed[0].id, "session-live");
        assert!(!d.auth_error(), "成功调用清除 authError");
    }

    /// 【回归矩阵·V1】0.1 点式形态：probe 404 → V1 自适应；list/prompt/models
    /// 走点式端点 + 直 payload（下次协议再变 = 改形态映射，不全量重写）
    #[tokio::test]
    async fn legacy_v1_wire_regression() {
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen_c = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/session.list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(
                        req.0,
                        serde_json::json!({"items": [
                            {"sessionId": "session-v1", "agentAvailable": false, "blank": false, "updatedAt": 1791012345000.0,
                             "running": true, "cwd": "/repo",
                             "projections": {"values": {"title": "v1 busy"}}}
                        ]}),
                    )
                }),
            )
            .route(
                "/api/session.prompt",
                axum::routing::post(
                    move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                        let seen = seen_c.clone();
                        async move {
                            seen.lock().unwrap().push(body.clone());
                            axum::Json(serde_json::json!({
                                "type": "server-response", "rpcId": body["rpcId"],
                                "result": {"ok": true, "value": {"accepted": true}}}))
                        }
                    },
                ),
            )
            .route(
                "/api/llm.models",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(
                        req.0,
                        serde_json::json!({"groups": [
                            {"id": "deepseek-official", "name": "DeepSeek", "models": [
                                {"id": "deepseek-v4-flash", "name": "DeepSeek-V4-Flash"}]}]}),
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        // probe：session/list 404 → 自适应 V1
        let listed = AgentCatalog::list(&d, false).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "session-v1");
        assert_eq!(listed[0].group, crate::models::Group::Working);

        SessionControl::send(&d, "session-v1", "hi v1")
            .await
            .unwrap();
        let req = seen.lock().unwrap()[0].clone();
        assert_eq!(req["method"], "session.prompt");
        assert_eq!(req["payload"]["sessionId"], "session-v1", "V1 直 payload");

        let models = ModelSettings::list_models(&d).await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].provider, "deepseek-official");
    }

    /// 【fresh 会话全链】create→prompt→page 读回 assistant 文本——0.2.0 真形状
    /// fixture 锁死（session/list asOfSeq 游标 + page records 信封 + assistant
    /// 事件解包）；E2E「历史不可回放」误标的根修回归
    #[tokio::test]
    async fn fresh_session_page_reads_back_assistant_text() {
        let seen = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
        let seen_c = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/host.describe",
                axum::routing::post(fake_describe_echo),
            )
            .route(
                "/api/session/create",
                axum::routing::post(
                    |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| async move {
                        axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": body["rpcId"],
                            "result": {"ok": true, "value": {"sessionId": "session-fresh", "agentAvailable": false, "blank": false}}}))
                    },
                ),
            )
            .route(
                "/api/session/list",
                axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                    rpc_ok(
                        req.0,
                        serde_json::json!({"items": [{"sessionId": "session-fresh", "agentAvailable": false, "blank": false,
                            "updatedAt": 1.0, "running": false, "cwd": "/tmp",
                            "projections": {"asOfSeq": 3, "values": {
                                "title": "e2e",
                                "modelSelection": {"provider": "deepseek-official",
                                                   "model": "deepseek-v4-flash",
                                                   "reasoningEffort": "high"}}}}]}),
                    )
                }),
            )
            .route(
                "/api/session/prompt",
                axum::routing::post(
                    move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                        let seen = seen_c.clone();
                        async move {
                            seen.lock().unwrap().push(body.clone());
                            axum::Json(serde_json::json!({
                                "type": "server-response", "rpcId": body["rpcId"],
                                "result": {"ok": true, "value": {"accepted": true}}}))
                        }
                    },
                ),
            )
            .route(
                "/api/session/page",
                axum::routing::post(
                    |req: axum::extract::Json<serde_json::Value>| async move {
                        // 真 0.2.0 page 响应形状：records 信封 + assistant/message 事件
                        axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": req.0["rpcId"],
                            "result": {"ok": true, "value": {"records": [
                                {"event": {"type": "user/message", "seq": 1,
                                           "data": {"role": "user", "content": [
                                               {"type": "text", "text": "hi"}]}}},
                                 {"event": {"type": "assistant/message", "seq": 3,
                                            "data": {"message": {"role": "assistant",
                                                "content": [{"type": "text", "text": "hello from qwen"}]}}}}
                            ], "hasMore": false}}}))
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let dir = tempfile::tempdir().unwrap();
        let d = driver_with_dir(&dir);
        d.serve.inject_state(Some(port), None);
        // start 链（create + prompt）
        let spec = StartSpec {
            prompt: "hi".into(),
            name: None,
            model: None,
            effort: None,
            cwd: "/tmp".into(),
        };
        let id = AgentCatalog::start(&d, &spec).await.unwrap();
        assert_eq!(id, "session-fresh");
        // page 读回 assistant 文本（E2E 误标根修）
        let page = SessionHistory::page(&d, &id, 50, None)
            .await
            .unwrap()
            .unwrap();
        let texts: Vec<&str> = page
            .messages
            .iter()
            .filter_map(|m| m.text.as_deref())
            .collect();
        assert!(
            texts.iter().any(|t| t.contains("hello from qwen")),
            "page 应读回 assistant 文本: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("hi")),
            "user 行也应读回: {texts:?}"
        );
        // 当前模型读回（modelSelection 投影）
        let cur = ModelSettings::session_model(&d, &id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cur.id, "deepseek-v4-flash");
        assert_eq!(cur.variant.as_deref(), Some("high"));
    }
}
