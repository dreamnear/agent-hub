use axum::{
    extract::{ws::WebSocketUpgrade, Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
};
use tokio::sync::broadcast::error::RecvError;

use crate::api::SharedState;

pub async fn events(ws: WebSocketUpgrade, state: State<SharedState>) -> impl IntoResponse {
    let tx = state.events_tx.clone();
    ws.on_upgrade(move |mut socket| async move {
        let mut rx = tx.subscribe();
        // 任一事件即推送 JSON；Lagged 警告但 continue（spec §3.3 WS 通知为"去重提示"，而非可靠流）
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload =
                        serde_json::to_string(&ev).unwrap_or_else(|_| r#"{"type":"tick"}"#.into());
                    if socket
                        .send(axum::extract::ws::Message::Text(payload.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "events broadcast lagged, continuing");
                    continue;
                }
                Err(_) => break,
            }
        }
    })
}

/// per-session 对话推送：连接即订阅该会话房间（驱动侧取/建 + 恰一个 tail，
/// driver-unify 2.5 内化）。
pub async fn chat(
    ws: WebSocketUpgrade,
    AxumPath(session_id): AxumPath<String>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    // 房间取建与「每房间恰一个 tail」语义已下沉驱动（ensure_chat_room）；
    // 未知名 driver 为装配错误（非客户端输入）——尽力连接后即返回。
    let Some(d) = state.drivers.get("claude").ok() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "unknown driver").into_response();
    };
    let sender = d.ensure_chat_room(&session_id);
    ws.on_upgrade(move |mut socket| async move {
        let mut rx = sender.subscribe();
        // Lagged continue（P4 处置 P2-9：对齐 events/terminal 语义，通知流不因积压断连）
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload = serde_json::to_string(&ev).unwrap_or_default();
                    if socket
                        .send(axum::extract::ws::Message::Text(payload.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "chat broadcast lagged, continuing");
                    continue;
                }
                Err(_) => break,
            }
        }
    })
}

/// per-subagent 对话推送（P5+）：房间键带 subagent 路径段防与主会话冲突，
/// 房间取建与 tail 定位（白名单校验同 API）已下沉驱动（2.5 内化）。
pub async fn chat_subagent(
    ws: WebSocketUpgrade,
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let Some(d) = state.drivers.get("claude").ok() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "unknown driver").into_response();
    };
    let sender = d.ensure_subagent_room(&session_id, &subagent_id);
    ws.on_upgrade(move |mut socket| async move {
        let mut rx = sender.subscribe();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let payload = serde_json::to_string(&ev).unwrap_or_default();
                    if socket
                        .send(axum::extract::ws::Message::Text(payload.into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, "subagent chat broadcast lagged, continuing");
                    continue;
                }
                Err(_) => break,
            }
        }
    })
}
