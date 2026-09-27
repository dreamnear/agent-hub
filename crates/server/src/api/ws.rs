use axum::{
    extract::{ws::WebSocketUpgrade, Path as AxumPath, State},
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

/// per-session 对话推送：连接即订阅该会话房间（无则建），tail 任务增量推 ChatEvent。
pub async fn chat(
    ws: WebSocketUpgrade,
    AxumPath(session_id): AxumPath<String>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    // 取/建房间（短临界区，不跨 await 持锁）；Entry 区分新建/已有，保证每房间恰一个 tail
    let (sender, need_tail) = {
        let mut rooms = state.chat_rooms.write().expect("chat_rooms lock poisoned");
        match rooms.entry(session_id.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => (e.get().clone(), false),
            std::collections::hash_map::Entry::Vacant(e) => {
                let (tx, _) = tokio::sync::broadcast::channel(64);
                e.insert(tx.clone());
                (tx, true)
            }
        }
    };
    if need_tail {
        crate::drivers::claude::session::spawn_session_tail(
            state.cfg.claude_root.clone(),
            session_id.clone(),
            sender.clone(),
        );
    }
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
/// tail 定位走 find_subagent_path（白名单校验同 API）。
pub async fn chat_subagent(
    ws: WebSocketUpgrade,
    AxumPath((session_id, subagent_id)): AxumPath<(String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    let room = format!("{session_id}/subagents/{subagent_id}");
    let (sender, need_tail) = {
        let mut rooms = state.chat_rooms.write().expect("chat_rooms lock poisoned");
        match rooms.entry(room) {
            std::collections::hash_map::Entry::Occupied(e) => (e.get().clone(), false),
            std::collections::hash_map::Entry::Vacant(e) => {
                let (tx, _) = tokio::sync::broadcast::channel(64);
                e.insert(tx.clone());
                (tx, true)
            }
        }
    };
    if need_tail {
        crate::drivers::claude::session::spawn_subagent_tail(
            state.cfg.claude_root.clone(),
            session_id,
            subagent_id,
            sender.clone(),
        );
    }
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

/// 终端保真视图（P4）：PTY 字节流 → WS binary → xterm.js；上行 Text({type:"keys",data}) → attach stdin 原样写入。
/// 与聊天视角共享同一 AttachSession（get_or_attach 复用，不重复 spawn）。
pub async fn terminal(
    ws: WebSocketUpgrade,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    State(state): State<SharedState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        // driver 校验 + 复用 attach
        if driver != "claude" {
            let _ = socket
                .send(axum::extract::ws::Message::Text("unknown driver".into()))
                .await;
            return;
        }
        let sender = state
            .message_routes
            .read()
            .expect("routes lock")
            .get("claude")
            .cloned();
        let Some(sender) = sender else {
            return;
        };
        let router = match sender
            .as_any()
            .downcast_ref::<crate::drivers::claude::send::ClaudeSendRouter>()
        {
            Some(r) => r,
            None => return,
        };
        let mut rx = match router.subscribe_output(&id).await {
            Ok(rx) => rx,
            Err(e) => {
                let _ = socket
                    .send(axum::extract::ws::Message::Text(
                        format!("attach failed: {e}").into(),
                    ))
                    .await;
                return;
            }
        };

        // 上行：Text({type:"keys",data}) → 原样写 stdin（终端按键语义，不做 \r 转换）
        // 下行：binary PTY 字节 → socket binary
        loop {
            tokio::select! {
                out = rx.recv() => {
                    match out {
                        Ok(bytes) => {
                            if socket
                                .send(axum::extract::ws::Message::Binary(bytes.into()))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(RecvError::Lagged(n)) => {
                            tracing::warn!(dropped = n, "terminal output lagged, continuing");
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                incoming = socket.recv() => {
                    match incoming {
                        Some(Ok(axum::extract::ws::Message::Text(t))) => {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                                if v["type"] == "keys" {
                                    if let Some(data) = v["data"].as_str() {
                                        let _ = sender.send_raw_bytes(&id, data.as_bytes());
                                    }
                                }
                            }
                        }
                        _ => break,
                    }
                }
            }
        }
    })
}
