//! pi inbound 泵（agent-hub-pi-hub-agent P1）：连接入站事件 → 状态机 + feed 落盘
//! + UI 请求自动取消。
//!
//! 状态机全权在泵（send 仅受理；agent_start/agent_end/Closed 翻转
//! Working/Idle/Dead 并广播 Tick），与 ACP 的「prompt 阻塞收场」模型不同。

use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::models::ChatMessage;

use super::{protocol, Inbound, PiDriver, PiStatus};

/// 启动后采样 get_state 的超时（真 pi 冷启动 <1s 实测；留足余量）。
const SAMPLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 历史回灌 get_messages 的超时（assistant-resume：长会话解析留足余量；
/// 失败仅留空历史不致命——后续对话照常）。
const RESEED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 挂泵：连接入站流直至 Closed。每次 ensure_started 挂一个（会话生命周期 = 泵生命周期）。
/// get_state 采样与历史回灌各在独立 task：pi（及假体）是单线程 stdin 循环，
/// 命令可能排在 prompt 之后很久才被应答，采样/回灌不得阻塞事件服务（UI 取消应答等）。
pub(super) fn spawn_pump(driver: PiDriver, rx: broadcast::Receiver<Inbound>, gen: u64) {
    let sampler = driver.clone();
    tokio::spawn(async move {
        // Starting 出场：get_state 采当前模型显示串；失败不致命（事件流照走）。
        // pi 无默认 model 时如实返回 id="unknown"（隔离 HOME 实测）——归一为
        // None（未配置语义），detail 留空比误导性好。
        if let Some(state) = request_state(&sampler).await {
            let model = state
                .get("model")
                .and_then(|m| m.get("id"))
                .and_then(|v| v.as_str())
                .filter(|id| !id.is_empty() && *id != "unknown")
                .map(str::to_string);
            if let Some(mut guard) = sampler.session_mut() {
                if let Some(s) = guard.as_mut() {
                    s.model = model;
                }
            }
            sampler.set_status(PiStatus::Idle);
            sampler.tick();
        }
    });
    // assistant-resume A1：`--continue` 恢复的会话把持久化历史回灌 feed（page
    // 全量回放是前端主路径）。失败仅留空历史（容错：pi 未恢复/旧版本无命令，
    // 不影响后续对话）。
    let reseeder = driver.clone();
    tokio::spawn(async move { reseed_history(&reseeder).await });
    tokio::spawn(async move {
        pump_loop(driver, rx, gen).await;
    });
}

/// get_messages 全量历史 → feed 前插回灌。新会话（A4）返回空批次，reseed 零扰动。
async fn reseed_history(driver: &PiDriver) {
    let Some(conn) = driver.session_conn() else {
        return;
    };
    let resp = match conn
        .request(protocol::get_messages_cmd(0), RESEED_TIMEOUT)
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, "pi get_messages 历史回灌失败（历史留空，不影响后续对话）");
            return;
        }
    };
    let Some(messages) = resp
        .data
        .as_ref()
        .and_then(|d| d.get("messages"))
        .and_then(|m| m.as_array())
    else {
        return;
    };
    let mapped: Vec<crate::models::ChatMessage> =
        messages.iter().flat_map(map_agent_message).collect();
    if let Some(feed) = driver.session_feed() {
        feed.reseed(mapped);
    }
}

async fn request_state(driver: &PiDriver) -> Option<serde_json::Value> {
    let conn = driver.session_conn()?;
    match conn
        .request(protocol::get_state_cmd(0), SAMPLE_TIMEOUT)
        .await
    {
        Ok(resp) => resp.data,
        Err(e) => {
            tracing::warn!(error = %e, "pi get_state 采样失败（继续事件流）");
            None
        }
    }
}

async fn pump_loop(driver: PiDriver, mut rx: broadcast::Receiver<Inbound>, gen: u64) {
    loop {
        match rx.recv().await {
            Ok(Inbound::Event(v)) => handle_event(&driver, &v, gen).await,
            Ok(Inbound::UiRequest { id, method }) => {
                // hub 无终端 UI：dialog 类一律取消应答（助手不被卡死，extension
                // 收到 undefined/false 自行降级）；fire-and-forget 忽略
                if protocol::ui_method_needs_reply(&method) {
                    if let Some(conn) = driver.session_conn() {
                        if let Err(e) = conn.respond_ui_cancelled(&id).await {
                            tracing::debug!(error = %e, "UI 取消应答失败");
                        }
                    }
                }
            }
            Ok(Inbound::Closed) => break,
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "pi inbound 泵落后，丢弃部分事件");
            }
            Err(RecvError::Closed) => break,
        }
    }
    // EOF：子进程退出 → Dead 收场（注册表条目保留，侧栏如实呈现 dead）。
    // gen 感知（回修轮 2）：stop→start 快速切换时旧泵 EOF 晚于新 spawn——跨代
    // 旧泵不得翻转/标死新会话、不得覆盖新 spawn 的记录（status_if_current/
    // set_status_if_current/record_start_failure_if_current 三处兜住）。
    // exit-3 失败面（前轮回修保留）：自己那代「从未完成握手」（Starting 态直接
    // 死，如 pi bin 损坏 exit-3）→ 回写启动失败（覆盖 spawn 层 Ok:true 失真）；
    // 正常服务过再退出（Idle/Working→Dead）属运行域不回写（侧栏 Dead 已见）。
    let was_starting = driver.status_if_current(gen) == Some(PiStatus::Starting);
    driver.set_status_if_current(gen, PiStatus::Dead);
    if was_starting {
        driver.record_start_failure_if_current(gen, "pi 进程在启动握手阶段退出（exit-3）".into());
    }
    driver.tick();
}

async fn handle_event(driver: &PiDriver, v: &serde_json::Value, gen: u64) {
    let ev_type = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
    match ev_type {
        "agent_start" => {
            driver.set_status_if_current(gen, PiStatus::Working);
            driver.tick();
        }
        "agent_end" => {
            // abort/正常收尾统一回闲（gen 感知：跨代旧泵不翻转新会话）
            if driver.status_if_current(gen).is_some() {
                driver.set_status_if_current(gen, PiStatus::Idle);
                driver.tick();
            }
        }
        "message_end" => {
            if let Some(msg) = v.get("message") {
                map_message_end(driver, msg);
            }
        }
        "extension_error" => {
            tracing::warn!(
                extension = %v.get("extensionPath").and_then(|p| p.as_str()).unwrap_or("?"),
                event = %v.get("event").and_then(|e| e.as_str()).unwrap_or("?"),
                "pi extension 报错: {}",
                v.get("error").and_then(|e| e.as_str()).unwrap_or("?")
            );
        }
        // message_update 流式 delta / tool_execution_* / queue_update 等不落 feed：
        // message_end 已含全量（message_end role=user 由 send 受理时推送，跳过防重复）
        _ => {
            tracing::debug!(target: "pi", event = %ev_type, "忽略事件");
        }
    }
}

/// message_end → feed 消息（按 role 与 content 块型拆分）。
fn map_message_end(driver: &PiDriver, msg: &serde_json::Value) {
    let feed = match driver.session_feed() {
        Some(f) => f,
        None => return,
    };
    for m in map_agent_message(msg) {
        feed.push(m);
    }
}

/// pi AgentMessage → feed 消息（纯映射；message_end 与 get_messages 历史回灌
/// 共用——两路消息同为 AgentMessage 形态，回灌复用即保证口径一致）。
/// 一条 AgentMessage 可拆多条（assistant 多块 / toolResult 一条）。
fn map_agent_message(msg: &serde_json::Value) -> Vec<ChatMessage> {
    use crate::models::ChatMessageKind;
    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or_default();
    match role {
        // 用户消息由事件流落 feed（send 受理即返不推——受理失败零残留，
        // 且与 assistant 回复保序）；hub 前端发送侧本有乐观渲染。
        // content 按 pi UserMessage 形态提取：string 或 blocks 数组（实测真 pi
        // 发 blocks——JSON 透传会让前端乐观气泡销账失败 → 双显，回修轮 2）
        "user" => {
            let text = match msg.get("content") {
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                Some(serde_json::Value::Array(blocks)) => {
                    let joined = blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("");
                    (!joined.is_empty()).then_some(joined)
                }
                _ => None,
            };
            vec![ChatMessage {
                kind: crate::models::ChatMessageKind::User,
                text,
                ..ChatMessage::default()
            }]
        }
        "assistant" => {
            let mut out = Vec::new();
            if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                for block in blocks {
                    let btype = block.get("type").and_then(|t| t.as_str());
                    match btype {
                        Some("thinking") => out.push(ChatMessage {
                            kind: ChatMessageKind::Thinking,
                            text: block
                                .get("thinking")
                                .and_then(|t| t.as_str())
                                .map(str::to_string),
                            ..ChatMessage::default()
                        }),
                        Some("text") => out.push(ChatMessage {
                            kind: ChatMessageKind::Assistant,
                            text: block
                                .get("text")
                                .and_then(|t| t.as_str())
                                .map(str::to_string),
                            ..ChatMessage::default()
                        }),
                        Some("toolCall") => out.push(ChatMessage {
                            kind: ChatMessageKind::ToolUse,
                            tool_use_id: block
                                .get("id")
                                .and_then(|i| i.as_str())
                                .map(str::to_string),
                            tool_name: block
                                .get("name")
                                .and_then(|n| n.as_str())
                                .map(str::to_string),
                            input: block.get("arguments").cloned(),
                            ..ChatMessage::default()
                        }),
                        _ => {}
                    }
                }
            }
            out
        }
        "toolResult" => vec![ChatMessage {
            kind: ChatMessageKind::ToolResult,
            tool_use_id: msg
                .get("toolCallId")
                .and_then(|i| i.as_str())
                .map(str::to_string),
            tool_name: msg
                .get("toolName")
                .and_then(|n| n.as_str())
                .map(str::to_string),
            result: msg.get("content").cloned(),
            error: msg.get("isError").and_then(|e| e.as_bool()),
            ..ChatMessage::default()
        }],
        // bashExecution（RPC bash 命令产物）hub 不发，忽略
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::super::{PiFeed, ASSISTANT_ID};
    use super::*;

    /// message_end 映射：assistant 多块拆 thinking/text/toolUse；toolResult 落
    /// result/error；user 跳过（send 已推）。
    #[tokio::test]
    async fn message_end_mapping() {
        use crate::config::PiConfig;
        use crate::models::ChatMessageKind;
        let dir = tempfile::tempdir().unwrap();
        let (events_tx, _rx) = broadcast::channel(8);
        let rooms: Arc<
            std::sync::RwLock<
                HashMap<String, broadcast::Sender<crate::drivers::events::ChatEvent>>,
            >,
        > = Arc::new(std::sync::RwLock::new(HashMap::new()));
        // 房间预挂，验证广播路径
        let (room_tx, mut room_rx) = broadcast::channel(16);
        rooms
            .write()
            .expect("lock")
            .insert(ASSISTANT_ID.into(), room_tx);
        let d = PiDriver::new(
            PiConfig {
                bin: "pi".into(),
                auto_start: false,
            },
            dir.path().join("settings.json"),
            dir.path().join("ext.ts"),
            dir.path().join("sessions"),
            7999,
            events_tx,
            rooms.clone(),
            dir.path().join("pi-agent"),
        );
        // 手工塞一个会话（feed 与 driver 共享同一张 rooms 表——广播路径真实）
        let feed = PiFeed::new(ASSISTANT_ID.into(), rooms);
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pi_fake_agent.py")
            .to_string_lossy()
            .into_owned();
        let (conn, _conn_rx) = super::super::PiConnection::spawn(
            "python3",
            dir.path(),
            &["-u".into(), fixture, "basic".into()],
            7999,
            None,
        )
        .await
        .unwrap();
        if let Some(mut guard) = d.session_mut() {
            *guard = Some(super::super::PiSession {
                id: ASSISTANT_ID.into(),
                status: PiStatus::Working,
                model: None,
                conn,
                feed: feed.clone(),
                gen: 1,
            });
        }

        // 真 pi 的 user content 是 blocks 数组（0.73.1 用户实测）——非 string
        let user_msg = serde_json::json!({
            "role":"user",
            "content":[{"type":"text","text":"hi"}]
        });
        let assistant_msg = serde_json::json!({
            "role":"assistant",
            "content":[
                {"type":"thinking","thinking":"thought"},
                {"type":"text","text":"reply"},
                {"type":"toolCall","id":"c1","name":"hub_list_sessions","arguments":{}}
            ]
        });
        let tool_result = serde_json::json!({
            "role":"toolResult","toolCallId":"c1","toolName":"hub_list_sessions",
            "content":[{"type":"text","text":"[]"}],"isError":false
        });
        map_message_end(&d, &user_msg);
        map_message_end(&d, &assistant_msg);
        map_message_end(&d, &tool_result);

        let h = feed.history();
        assert_eq!(h.len(), 5, "user/thinking/text/toolUse/toolResult 各一条");
        assert_eq!(h[0].kind, ChatMessageKind::User);
        assert_eq!(h[0].text.as_deref(), Some("hi"));
        assert_eq!(h[1].kind, ChatMessageKind::Thinking);
        assert_eq!(h[1].text.as_deref(), Some("thought"));
        assert_eq!(h[2].kind, ChatMessageKind::Assistant);
        assert_eq!(h[2].text.as_deref(), Some("reply"));
        assert_eq!(h[3].kind, ChatMessageKind::ToolUse);
        assert_eq!(h[3].tool_name.as_deref(), Some("hub_list_sessions"));
        assert_eq!(h[3].tool_use_id.as_deref(), Some("c1"));
        assert_eq!(h[4].kind, ChatMessageKind::ToolResult);
        assert_eq!(h[4].error, Some(false));
        // 房间广播同步到达（feed.push 路径）
        assert!(room_rx.try_recv().is_ok());

        // agent_end 收场：连接活着 → 回闲
        d.set_status(PiStatus::Working);
        handle_event(&d, &serde_json::json!({"type":"agent_end"}), 1).await;
        let status = d.session_mut().and_then(|g| g.as_ref().map(|s| s.status));
        assert_eq!(status, Some(PiStatus::Idle), "agent_end 应回闲");
        if let Some(conn) = d.session_conn() {
            conn.shutdown().await;
        }
    }
}
