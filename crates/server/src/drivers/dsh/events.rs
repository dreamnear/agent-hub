//! dsh 实时事件泵（0.1 / 0.2 双协议自适应）。
//!
//! **V2（0.2.0+，主路径）**：WS `/api/remote.mux`（`REMOTE_STREAM_MUX_PATH`，
//! 源码 packages/api/gateway/src/stream-protocol.ts）是多路逻辑流载体，**没有**
//! 被动推送——客户端必须 `{type:"open",streamId,endpoint,payload}` 逐流订阅：
//! - `session/control`（无参）：baseline + `{type:'projection',sessionId,key,value,seq}`
//!   帧 → 侧栏 Tick
//! - `session/follow`（args.request.address）：`snapshot{records}` 开窗 + `{type:'event'}`
//!   追加帧 → 打开的聊天房间逐条 push（key=seq 去重，与 page 播种天然对齐）
//!
//! 下行帧形：`{type:'item'|'error'|'end', streamId, ...}`。
//!
//! **V1（0.1.x 兼容）**：WS `/api/events.mux` 被动 server-request 信封
//! （`handle_envelope` 保留）。
//!
//! 断线：监督循环退避重连；room 关闭后残留 follow 流由 push 的开房检查过滤
//! （低量，不做主动 cancel——ponytail ceiling：房间数小，关房回收按需再加）。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::DshDriver;

/// 重连间隔（认证失败场景指数退避至上限——带认证实例未配 token 时不刷屏）
const RECONNECT: Duration = Duration::from_secs(3);
const RECONNECT_MAX: Duration = Duration::from_secs(60);
/// 房间差分周期（新开聊天房间 → follow 流的最长延迟）
const FOLLOW_DIFF: Duration = Duration::from_secs(2);

/// 事件泵监督循环（main 启动时 spawn；serve 未拉起时等待下一轮）。
pub fn spawn_event_pump(driver: DshDriver) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = RECONNECT;
        loop {
            // serve 未拉起：等下一轮（首次 list 即拉起点）
            let Some(port) = driver.serve.port() else {
                tokio::time::sleep(RECONNECT).await;
                continue;
            };
            // 双协议：V2 remote.mux（流订阅）；V1 events.mux（被动信封）
            let v2 = driver.wire() != super::rpc::Wire::V1;
            let path = if v2 {
                "/api/remote.mux"
            } else {
                "/api/events.mux"
            };
            let mut req: tokio_tungstenite::tungstenite::http::Request<()> =
                tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
                    format!("ws://127.0.0.1:{port}{path}").as_str(),
                )
                .expect("mux ws request");
            // Host authority 必须与 hub HTTP 全链一致（127.0.0.1 无端口）：
            // dsh cookie 名 = sha256(Host authority)（browser-auth.ts cookieName），
            // tungstenite 默认 Host 带 :port → authority 不匹配 → WS 恒 401
            //（HTTP 全通唯 WS 401 的根因，2026-10-03）
            req.headers_mut().insert(
                tokio_tungstenite::tungstenite::http::header::HOST,
                tokio_tungstenite::tungstenite::http::HeaderValue::from_static("127.0.0.1"),
            );
            if let Some(c) = driver.auth_cookie() {
                req.headers_mut().insert(
                    tokio_tungstenite::tungstenite::http::header::COOKIE,
                    tokio_tungstenite::tungstenite::http::HeaderValue::from_str(&c)
                        .expect("cookie header"),
                );
            }
            match tokio_tungstenite::connect_async(req).await {
                Ok((mut ws, _)) => {
                    backoff = RECONNECT;
                    tracing::info!("dsh mux WS 已连接（{path}）");
                    if v2 {
                        run_mux_pump(&driver, &mut ws).await;
                    } else {
                        run_v1_pump(&driver, &mut ws).await;
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "dsh mux WS 连接失败，退避重连");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECONNECT_MAX);
                    continue;
                }
            }
            tokio::time::sleep(RECONNECT).await;
        }
    })
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// V2：remote.mux 多路流泵（连接成功 ≠ 有数据；以收到流帧为准）。
async fn run_mux_pump(driver: &DshDriver, ws: &mut WsStream) {
    // streamId → sessionId（control 流单独记）
    let mut follows: HashMap<String, String> = HashMap::new();
    // follow 开流被 session/not-found 拒的会话（该 host 不解析此会话——典型：
    // 同 home 双活 host 拓扑下会话归另一进程活体所有）→ 连接级拉黑不重开，
    // 防差分无限重开风暴（实测 1984 次）
    let mut dead_sessions: HashSet<String> = HashSet::new();
    let mut next_id = 0u64;
    // 开控制流：session/control 无参（args 空）
    let open = serde_json::json!({
        "type": "open", "streamId": "control",
        "endpoint": "session/control", "payload": {"args": {}},
    });
    if ws
        .send(Message::Text(open.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    // 开 $events 流（网关转发 Remote 事件：ask_user_question waterfall 等）。
    // payload 必须恰为 {args:{}}（gateway openRemoteEvents 校验，多一键即拒）。
    let open_events = serde_json::json!({
        "type": "open", "streamId": "events",
        "endpoint": "$events", "payload": {"args": {}},
    });
    if ws
        .send(Message::Text(open_events.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    // control/events 流存活标记：error/end 后由周期差分重开（提问卡通道不能
    // 一次 error 即永失——pending waterfall 重发机制依赖流存活）
    let mut streams_open = true;
    loop {
        match tokio::time::timeout(FOLLOW_DIFF, ws.next()).await {
            Ok(Some(Ok(Message::Text(t)))) => {
                handle_mux_frame(
                    driver,
                    t.to_string(),
                    &mut follows,
                    &mut dead_sessions,
                    &mut streams_open,
                )
                .await;
            }
            Ok(Some(Ok(Message::Ping(p)))) => {
                // 回 pong 保活（tungstenite 自动应答不确定，显式回）
                let _ = ws.send(Message::Pong(p)).await;
            }
            Ok(Some(Ok(Message::Close(c)))) => {
                tracing::debug!(?c, "dsh mux WS 关闭");
                return;
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(e))) => {
                tracing::debug!(error = %e, "dsh mux WS 读错误");
                return;
            }
            Ok(None) => return,
            Err(_elapsed) => {
                // 房间差分：打开中的聊天房间逐一开 follow 流（快照播种 + 追加）
                let open_ids: Vec<String> = driver
                    .rooms
                    .read()
                    .expect("chat_rooms lock")
                    .keys()
                    .cloned()
                    .collect();
                let known = driver.known_sessions_snapshot();
                let followed: HashSet<String> = follows.values().cloned().collect();
                // rooms 表跨驱动共享：只 follow dsh 自己的会话（非 dsh id 盲开
                // = not-found 刷屏）
                for id in open_ids
                    .into_iter()
                    .filter(|id| known.is_empty() || known.contains(id))
                {
                    if followed.contains(&id) || dead_sessions.contains(&id) {
                        continue;
                    }
                    let stream_id = format!("f{next_id}");
                    next_id += 1;
                    let open = serde_json::json!({
                        "type": "open", "streamId": stream_id,
                        "endpoint": "session/follow",
                        "payload": {"args": {"request": {
                            "address": {"kind": "session", "sessionId": id},
                        }}},
                    });
                    if ws
                        .send(Message::Text(open.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    follows.insert(stream_id, id);
                }
                // control/events 流掉线重开（error/end 摘除后由差分补开）
                if !streams_open {
                    let open = serde_json::json!({
                        "type": "open", "streamId": "control",
                        "endpoint": "session/control", "payload": {"args": {}},
                    });
                    if ws
                        .send(Message::Text(open.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let open_events = serde_json::json!({
                        "type": "open", "streamId": "events",
                        "endpoint": "$events", "payload": {"args": {}},
                    });
                    if ws
                        .send(Message::Text(open_events.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    streams_open = true;
                }
            }
        }
    }
}

/// V2 单帧分发（pub(crate) 供驱动单测直调，不经真 WS）。
pub(super) async fn handle_mux_frame(
    driver: &DshDriver,
    text: String,
    follows: &mut HashMap<String, String>,
    dead_sessions: &mut HashSet<String>,
    streams_open: &mut bool,
) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let stream_id = v["streamId"].as_str().unwrap_or_default().to_string();
    match v["type"].as_str() {
        Some("item") => {
            let value = &v["value"];
            if stream_id == "control" {
                // baseline/projection → 侧栏刷新（title/tokens/running）
                *streams_open = true;
                let _ = driver
                    .events_tx
                    .send(crate::drivers::events::HubEvent::Tick);
            } else if stream_id == "events" {
                handle_events_stream(driver, value).await;
            } else if let Some(session_id) = follows.get(&stream_id).cloned() {
                match value["type"].as_str() {
                    // follow 开窗：records 信封（与 page 同构，key 去重对齐播种）
                    Some("snapshot") => {
                        let Ok(records) = serde_json::from_value::<Vec<super::api::HistoryEntry>>(
                            value["records"].clone(),
                        ) else {
                            return;
                        };
                        for entry in records {
                            push_history_entry(driver, &session_id, &entry).await;
                        }
                    }
                    // 追加：{type:'event', event:{...}}
                    Some("event") => {
                        let Ok(event) = serde_json::from_value::<super::api::SessionEvent>(
                            value["event"].clone(),
                        ) else {
                            return;
                        };
                        let entry = super::api::HistoryEntry { event, view: None };
                        push_history_entry(driver, &session_id, &entry).await;
                    }
                    // assistant-stream 等 process-local 帧：不订阅（assistantStream 未开）
                    _ => {}
                }
            }
        }
        Some("error") => {
            let code = v["error"]["code"].as_str().unwrap_or("?").to_string();
            if stream_id == "control" || stream_id == "events" {
                *streams_open = false;
            }
            // session/not-found：该 host 不解析此会话（跨 host 活体拓扑）→ 拉黑
            // 本连接不再重开；其余错误仍走差分重开
            if code == "session/not-found" {
                if let Some(sid) = follows.remove(&stream_id) {
                    tracing::debug!(
                        session_id = %sid,
                        "dsh follow 被拒 not-found，本连接拉黑（该会话归另一 host 活体）"
                    );
                    dead_sessions.insert(sid);
                    return;
                }
            }
            tracing::warn!(
                stream_id = %stream_id,
                code = %code,
                "dsh mux 流错误，摘除待差分重开"
            );
            follows.remove(&stream_id);
        }
        Some("end") => {
            if stream_id == "control" || stream_id == "events" {
                *streams_open = false;
            }
            follows.remove(&stream_id);
        }
        _ => {}
    }
}

/// $events 流帧（网关转发 Remote 事件）：ready 登记 clientId；waterfall 分发
/// （user-questions/request → 提问卡）；cancellation → 提问收场；emit → Tick。
async fn handle_events_stream(driver: &DshDriver, value: &serde_json::Value) {
    tracing::info!(
        frame = value["type"].as_str().unwrap_or("?"),
        event = value["event"].as_str().unwrap_or("-"),
        event_id = value["eventId"].as_str().unwrap_or("-"),
        "dsh $events 帧到达"
    );
    match value["type"].as_str() {
        Some("ready") => {
            if let Some(client_id) = value["clientId"].as_str() {
                driver.set_events_client(client_id);
            }
        }
        Some("waterfall") => {
            let event = value["event"].as_str().unwrap_or_default();
            let event_id = value["eventId"].as_str().unwrap_or_default().to_string();
            match event {
                // ask_user_question（源码 interaction/user-questions：Cordis
                // 'user-questions/request' waterfall，request={questions,...}；
                // agentId 即 dsh 会话 id）
                "user-questions/request" => {
                    if event_id.is_empty() {
                        return;
                    }
                    let agent_id = value["agentId"].as_str().unwrap_or_default();
                    if agent_id.is_empty() {
                        return;
                    }
                    driver.register_question(agent_id, &event_id, &value["request"]["questions"]);
                }
                other => {
                    tracing::debug!(event = other, "dsh $events waterfall 未接，忽略");
                }
            }
        }
        Some("cancellation") => {
            let event_id = value["eventId"].as_str().unwrap_or_default();
            if !event_id.is_empty() {
                driver.cancel_question(event_id);
            }
        }
        _ => {
            // emit 帧（广播型通知）：侧栏刷新即可
            let _ = driver
                .events_tx
                .send(crate::drivers::events::HubEvent::Tick);
        }
    }
}

/// 单条 history 事件 → 打开中的房间逐 part 推送（去重）+ Tick；房间未开静默。
async fn push_history_entry(
    driver: &DshDriver,
    session_id: &str,
    entry: &super::api::HistoryEntry,
) {
    let open = driver
        .rooms
        .read()
        .expect("chat_rooms lock")
        .contains_key(session_id);
    if !open {
        return;
    }
    // 多 part 事件逐条推送（assistant reasoning+text 共存全保留）
    for k in super::mapping::event_to_keyed(entry) {
        if driver.push_chat(session_id, &k.key, k.msg) > 0 {
            let _ = driver
                .events_tx
                .send(crate::drivers::events::HubEvent::Tick);
        }
    }
}

/// V1：events.mux 被动信封泵（0.1.x 兼容路径）。
async fn run_v1_pump(driver: &DshDriver, ws: &mut WsStream) {
    while let Some(msg) = ws.next().await {
        let data = match msg {
            Ok(Message::Text(t)) => t.to_string(),
            Ok(Message::Ping(p)) => {
                let _ = ws.send(Message::Pong(p)).await;
                continue;
            }
            Ok(Message::Close(c)) => {
                tracing::debug!(?c, "dsh mux WS 关闭");
                return;
            }
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!(error = %e, "dsh mux WS 读错误");
                return;
            }
        };
        if let Some((rpc_id, payload)) = parse_envelope(&data) {
            handle_envelope(&rpc_id, &payload, driver).await;
        }
    }
}

/// 拆 V1 server-request 信封：`{type:"server-request","rpcId","method","payload"}`。
fn parse_envelope(data: &str) -> Option<(String, serde_json::Value)> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    if v["type"].as_str() != Some("server-request") {
        return None;
    }
    Some((v["rpcId"].as_str()?.to_string(), v["payload"].clone()))
}

/// V1 单帧分发（pub(crate) 供驱动单测直调，不经真 WS）。
pub(super) async fn handle_envelope(rpc_id: &str, frame: &serde_json::Value, driver: &DshDriver) {
    let ftype = frame["type"].as_str().unwrap_or_default();
    match ftype {
        "session/event" => {
            let session_id = frame["sessionId"].as_str().unwrap_or_default();
            if session_id.is_empty() {
                return;
            }
            // 房间未开不拉全量（打开会话时 page 播种，泵只补增量）
            let open = driver
                .rooms
                .read()
                .expect("chat_rooms lock")
                .contains_key(session_id);
            if !open {
                return;
            }
            // mux 帧的 event 槽是 SessionEvent 本体（history 的 HistoryEntry 才带
            // {event,view} 包裹）——这里直解 SessionEvent 再包壳复用映射
            let event: Option<super::api::SessionEvent> =
                serde_json::from_value(frame["event"].clone()).ok();
            if let Some(event) = event {
                let entry = super::api::HistoryEntry { event, view: None };
                push_history_entry(driver, session_id, &entry).await;
            }
        }
        "session/projection" => {
            // 侧栏投影（title/tokenUsage/running 等）变化 → 前端重拉
            let _ = driver
                .events_tx
                .send(crate::drivers::events::HubEvent::Tick);
        }
        "approval/requested" => {
            let session_id = frame["sessionId"].as_str().unwrap_or_default();
            let approval_id = frame["approvalId"].as_str().unwrap_or_default();
            if session_id.is_empty() || approval_id.is_empty() {
                return;
            }
            let key = super::perm_key(session_id, approval_id);
            let pending = super::ApprovalPending {
                session_id: session_id.to_string(),
                approval_id: approval_id.to_string(),
                rpc_id: rpc_id.to_string(),
                card: driver.approval_card(
                    frame["toolName"].as_str().unwrap_or("tool"),
                    frame["reason"].as_str(),
                    frame["callId"].as_str(),
                    session_id,
                    approval_id,
                ),
            };
            // 重连重放去重：同 approvalId 已登记（宿主重放 rpcId 稳定）不重推卡
            let is_new = {
                let mut approvals = driver.approvals.lock().expect("dsh approvals lock");
                match approvals.entry(key.clone()) {
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(pending.clone());
                        true
                    }
                    std::collections::hash_map::Entry::Occupied(_) => false,
                }
            };
            if !is_new {
                return;
            }
            driver.push_chat(session_id, &key, pending.card);
            let _ = driver
                .events_tx
                .send(crate::drivers::events::HubEvent::Tick);
        }
        "approval/resolved" => {
            let session_id = frame["sessionId"].as_str().unwrap_or_default();
            let approval_id = frame["approvalId"].as_str().unwrap_or_default();
            if session_id.is_empty() || approval_id.is_empty() {
                return;
            }
            let key = super::perm_key(session_id, approval_id);
            let existed = driver
                .approvals
                .lock()
                .expect("dsh approvals lock")
                .remove(&key)
                .is_some();
            if !existed {
                return;
            }
            let outcome = frame["outcome"].as_str().unwrap_or("resolved");
            let receipt = crate::models::ChatMessage {
                kind: crate::models::ChatMessageKind::Other,
                raw_type: Some("acp_permission_resolved".into()),
                tool_use_id: Some(key.clone()),
                text: Some(match outcome {
                    "allowed-once" => "已放行（本次）".to_string(),
                    "rejected" => "已拒绝".to_string(),
                    other => format!("已收场: {other}"),
                }),
                ..crate::models::ChatMessage::default()
            };
            driver.push_chat(session_id, &format!("{key}:resolved"), receipt);
            let _ = driver
                .events_tx
                .send(crate::drivers::events::HubEvent::Tick);
        }
        // session/queue（在途消息条）、session/jobs、question/*、stream/error 等：忽略
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::SessionHistory;
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};
    use tokio::sync::broadcast;

    /// V1 信封解析：server-request 拆包；非信封/坏 JSON 拒绝
    #[test]
    fn parse_envelope_splits_payload() {
        let (rpc_id, payload) = parse_envelope(
            r#"{"type":"server-request","rpcId":"r1","method":"session/projection",
                "payload":{"type":"session/projection","sessionId":"s","key":"title","value":"t","seq":1}}"#,
        )
        .unwrap();
        assert_eq!(rpc_id, "r1");
        assert_eq!(payload["type"], "session/projection");
        assert!(parse_envelope("not json").is_none());
        assert!(parse_envelope(r#"{"type":"client-request"}"#).is_none());
    }

    fn pump_driver(
        rooms: HashMap<String, broadcast::Sender<crate::drivers::events::ChatEvent>>,
        tx: broadcast::Sender<crate::drivers::events::HubEvent>,
    ) -> DshDriver {
        DshDriver::new(
            super::super::serve::ServeManager::new(
                "dsh".into(),
                4097,
                super::super::serve::ServeManager::state_file_for(
                    tempfile::tempdir().unwrap().path(),
                ),
                false,
            ),
            Arc::new(RwLock::new(rooms)),
            tx,
            super::super::rpc::Auth::default(),
        )
    }

    /// V2 control 流：baseline/projection item → Tick；未知 value 静默
    #[tokio::test]
    async fn mux_control_items_tick() {
        let (tx, mut rx) = broadcast::channel(8);
        let mut rooms = HashMap::new();
        rooms.insert("s".to_string(), broadcast::channel(8).0);
        let d = pump_driver(rooms, tx);
        let mut follows = HashMap::new();
        let mut dead_sessions = HashSet::new();
        let mut streams_open = true;
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"item","streamId":"control",
                "value":{"type":"projection","sessionId":"s","key":"title","value":"x","seq":1}})
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::drivers::events::HubEvent::Tick)
        ));
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"item","streamId":"control","value":{"type":"baseline"}})
                .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::drivers::events::HubEvent::Tick)
        ));
        // 未知流（未登记 follow）：静默
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"item","streamId":"f99",
                "value":{"type":"event","event":{"type":"user/message","seq":1,"data":{}}}})
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert!(rx.try_recv().is_err(), "未登记流不推送");
    }

    /// V2 follow 流：event 追加推打开中的房间（逐 part）+ Tick；重复 seq 去重；
    /// 房间未开静默；snapshot records 播种
    #[tokio::test]
    async fn mux_follow_pushes_open_room_only() {
        let (tx, mut rx) = broadcast::channel::<crate::drivers::events::HubEvent>(8);
        let (room_tx, _) = broadcast::channel::<crate::drivers::events::ChatEvent>(8);
        let mut rooms = HashMap::new();
        rooms.insert("s-open".to_string(), room_tx);
        let d = pump_driver(rooms, tx);
        let mut room_rx_open = d.ensure_chat_room("s-open").subscribe();
        let mut follows = HashMap::new();
        let mut dead_sessions = HashSet::new();
        let mut streams_open = true;
        follows.insert("f0".to_string(), "s-open".to_string());

        // 追加事件：多 part（reasoning+text）→ 2 条消息
        handle_mux_frame(
            &d,
            serde_json::json!({
                "type": "item", "streamId": "f0",
                "value": {"type": "event", "event": {"type": "assistant/message", "seq": 5,
                    "data": {"message": {"role": "assistant", "content": [
                        {"type": "reasoning", "text": "pondering"},
                        {"type": "text", "text": "answer body"}]}}}}})
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        let ev = room_rx_open.try_recv().unwrap();
        assert_eq!(ev.message.kind, crate::models::ChatMessageKind::Thinking);
        let ev = room_rx_open.try_recv().unwrap();
        assert_eq!(ev.message.kind, crate::models::ChatMessageKind::Assistant);
        assert!(matches!(
            rx.try_recv(),
            Ok(crate::drivers::events::HubEvent::Tick)
        ));

        // 同 seq 重推（断线重开 follow 重放快照）→ 去重不重发
        handle_mux_frame(
            &d,
            serde_json::json!({
                "type": "item", "streamId": "f0",
                "value": {"type": "snapshot", "records": [
                    {"type": "event", "event": {"type": "assistant/message", "seq": 5,
                     "data": {"message": {"role": "assistant", "content": [
                        {"type": "text", "text": "answer body"}]}}}}]}}
            )
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert!(room_rx_open.try_recv().is_err(), "快照重放按 key 去重");
        // 排干前面累积的 Tick（多 part 每条消息各发一次）
        while rx.try_recv().is_ok() {}

        // 房间未开的会话：静默
        follows.insert("f1".to_string(), "s-closed".to_string());
        handle_mux_frame(
            &d,
            serde_json::json!({
                "type": "item", "streamId": "f1",
                "value": {"type": "event", "event": {"type": "user/message", "seq": 1,
                    "data": {"content": [{"type": "text", "text": "hi"}]}}}}
            )
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert!(rx.try_recv().is_err(), "未开房不推送不 Tick");

        // $events waterfall：user-questions/request → 提问卡推打开中的房间
        handle_mux_frame(
            &d,
            serde_json::json!({
                "type": "item", "streamId": "events",
                "value": {"type": "waterfall", "event": "user-questions/request",
                    "eventId": "evt-1", "agentId": "s-open",
                    "request": {"questions": [
                        {"id": "q1", "question": "继续吗？",
                         "options": [{"label": "继续"}, {"label": "取消"}]}]}}}
            )
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        let ev = room_rx_open.try_recv().unwrap();
        assert_eq!(ev.message.raw_type.as_deref(), Some("dsh_question"));
        assert_eq!(ev.message.input.as_ref().unwrap()["eventId"], "evt-1");
        assert_eq!(
            ev.message.input.as_ref().unwrap()["questions"][0]["id"],
            "q1",
            "提问卡必须带 questions 原文（选项渲染事实源）"
        );
        // ready 帧登记 clientId
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"item","streamId":"events",
                "value":{"type":"ready","clientId":"cli-1","host":{"home":"/h"}}})
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert_eq!(d.events_client_snapshot().as_deref(), Some("cli-1"));
        // cancellation → 提问收场条
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"item","streamId":"events",
                "value":{"type":"cancellation","eventId":"evt-1"}})
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        let ev = room_rx_open.try_recv().unwrap();
        assert_eq!(ev.message.raw_type.as_deref(), Some("dsh_question_closed"));
        // not-found 错误 → 会话拉黑（防风暴；差分不再重开）
        follows.insert("f2".to_string(), "s-open".to_string());
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"error","streamId":"f2",
                "error":{"code":"session/not-found","message":"x","details":{}}})
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert!(
            dead_sessions.contains("s-open") && !follows.contains_key("f2"),
            "not-found 会话必须进连接级黑名单（防差分风暴）"
        );
        // error/end 帧：摘登记（差分会重开）
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"error","streamId":"f0",
                "error":{"code":"x","message":"gone","details":{}}})
            .to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        handle_mux_frame(
            &d,
            serde_json::json!({"type":"end","streamId":"f1"}).to_string(),
            &mut follows,
            &mut dead_sessions,
            &mut streams_open,
        )
        .await;
        assert!(!follows.contains_key("f0") && !follows.contains_key("f1"));
        // 确认 SessionHistory trait 在作用域（room 生命周期由 page/ensure 维护）
        let _ = <DshDriver as SessionHistory>::page;
    }
}
