//! ACP 会话 REST 端点（acp-omp 批1 任务4）：创建 / prompt / cancel + agent 清单。
//! prompt 走 message_routes 统一入口（MessageSender trait 的 ACP 语义）；
//! update 流经 dispatch 泵推 chat_rooms 房间（/ws/chat/{sessionId} 复用）。

use std::path::Path;

use axum::{
    extract::{Path as AxumPath, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::SharedState,
    drivers::acp::{registry, AcpSession, Inbound},
    error::AppError,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/acp/agents", get(list_acp_agents))
        .route("/api/acp/sessions", post(create_session))
        .route("/api/acp/sessions/{id}/prompt", post(prompt_session))
        .route("/api/acp/sessions/{id}/cancel", post(cancel_session))
        .route("/api/acp/sessions/{id}/messages", get(session_messages))
        .route("/api/acp/sessions/{id}/permission", post(answer_permission))
}

async fn list_acp_agents(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(json!({ "agents": state.acp.agents() }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionBody {
    pub agent: String,
    pub cwd: String,
    pub model: Option<String>,
}

/// 会话装配共用（批3 任务13 从 create_session 抽出）：chat 房间先行 + 注册 +
/// inbound 泵 + Tick（侧栏即时出现）。
fn attach_session(
    state: &SharedState,
    session: AcpSession,
    inbound_rx: tokio::sync::broadcast::Receiver<Inbound>,
) {
    let (room_tx, _) = tokio::sync::broadcast::channel(64);
    state
        .chat_rooms
        .write()
        .expect("chat_rooms lock")
        .insert(session.id.clone(), room_tx);
    state.acp_registry.insert(session.clone());
    crate::drivers::acp::dispatch::spawn_inbound_pump(
        session.id.clone(),
        session.conn.clone(),
        inbound_rx,
        state.acp_registry.clone(),
        session.feed.clone(),
        state.events_tx.clone(),
    );
    let _ = state
        .events_tx
        .send(crate::drivers::claude::watcher::HubEvent::Tick);
}

async fn create_session(
    State(state): State<SharedState>,
    Json(body): Json<CreateSessionBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    if state.acp.agent(&body.agent).is_none() {
        return Err(AppError::not_found(format!(
            "未知 ACP agent: {}（已配置: {:?}）",
            body.agent,
            state
                .acp
                .agents()
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>()
        )));
    }
    super::agents::validate_cwd(&body.cwd).await?;
    let (session, inbound_rx) = state
        .acp
        .start_session(
            &body.agent,
            Path::new(&body.cwd),
            body.model.clone(),
            state.chat_rooms.clone(),
        )
        .await?;
    attach_session(&state, session.clone(), inbound_rx);
    // 批3 任务13：create 即落盘（重启可恢复）
    state.acp_registry.persist_async();

    Ok(Json(json!({
        "sessionId": session.id,
        "agent": session.agent,
        "cwd": session.cwd,
        "model": session.model,
        "status": session.status.as_str(),
    })))
}

/// 重启恢复（批3 任务13）：持久化条目逐个走 session/load——成功（omp 实测支持）
/// → 挂回注册表，agent 重放 update 回填 feed（list 恢复历史会话）；
/// 失败 → 剪除条目（会话按新建呈现，不做假恢复）。终态重写持久化文件。
pub async fn restore_persisted_sessions(state: SharedState) {
    let entries = registry::load_persisted(&state.cfg.acp_sessions_file).await;
    if entries.is_empty() {
        return;
    }
    tracing::info!(count = entries.len(), "开始恢复 ACP 会话");
    let mut kept = Vec::new();
    for entry in entries {
        match state
            .acp
            .resume_session(
                &entry.agent,
                Path::new(&entry.cwd),
                &entry.id,
                state.chat_rooms.clone(),
            )
            .await
        {
            Ok((session, inbound_rx)) => {
                attach_session(&state, session, inbound_rx);
                kept.push(entry);
            }
            Err(e) => {
                tracing::warn!(id = %entry.id, error = %e, "ACP 会话恢复失败，剪除条目（按新建呈现）");
            }
        }
    }
    registry::save_persisted(&state.cfg.acp_sessions_file, &kept).await;
    tracing::info!(restored = kept.len(), "ACP 会话恢复完成");
}

#[derive(Debug, Deserialize)]
pub struct PromptBody {
    pub text: String,
}

async fn prompt_session(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<PromptBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_session(&state, &id)?;
    let sender = acp_sender(&state)?;
    // 用户消息入流（批2 任务8）：prompt 发起即落 history + 广播房间，
    // 前端乐观气泡随之销账；发送失败不回滚（对话记录如实保留尝试）
    if let Some(session) = state.acp_registry.get(&id) {
        session.feed.push(crate::models::ChatMessage {
            kind: crate::models::ChatMessageKind::User,
            raw_type: Some("acp_user".into()),
            text: Some(body.text.clone()),
            ..crate::models::ChatMessage::default()
        });
    }
    sender.send(&id, &body.text).await?;
    Ok(Json(json!({ "ok": true })))
}

/// 会话消息回放（批2 任务8）：内存 history 全量（firstLine/hasMore 对齐
/// MessagePage 信封，前端 useSession 同构消费；ACP 无翻页语义）。
async fn session_messages(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let session = state
        .acp_registry
        .get(&id)
        .ok_or_else(|| AppError::not_found(format!("ACP 会话不存在: {id}")))?;
    Ok(Json(json!({
        "messages": session.feed.history(),
        "firstLine": 0,
        "hasMore": false,
    })))
}

async fn cancel_session(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_session(&state, &id)?;
    let sender = acp_sender(&state)?;
    sender.interrupt(&id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// 权限应答（批3 任务11）：用户弹卡选择回流挂起的反向请求（optionId=None = 取消）。
/// 回执由 dispatch 统一推送（r77：应答/取消/超时全路径都有回执）。
/// r77 幂等：无挂起键（已应答/已超时/未知）一律 200 吞掉——重复应答不再以
/// 409 Conflict 惊扰用户（内部日志留痕）。
#[derive(Debug, Deserialize)]
pub struct PermissionAnswerBody {
    #[serde(rename = "permId")]
    pub perm_id: String,
    #[serde(rename = "optionId")]
    pub option_id: Option<String>,
}

async fn answer_permission(
    State(state): State<SharedState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<PermissionAnswerBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_session(&state, &id)?;
    let answered = state.acp_registry.resolve_permission(
        &body.perm_id,
        crate::drivers::acp::registry::PermissionAnswer {
            option_id: body.option_id,
        },
    );
    if !answered {
        tracing::info!(session_id = %id, perm_id = %body.perm_id, "权限应答落空（已应答/已超时/未知），幂等返回");
        return Ok(Json(json!({ "ok": true, "alreadyResolved": true })));
    }
    Ok(Json(json!({ "ok": true })))
}

fn ensure_session(state: &SharedState, id: &str) -> Result<(), AppError> {
    if state.acp_registry.get(id).is_none() {
        return Err(AppError::not_found(format!("ACP 会话不存在: {id}")));
    }
    Ok(())
}

fn acp_sender(
    state: &SharedState,
) -> Result<std::sync::Arc<dyn crate::drivers::MessageSender>, AppError> {
    state
        .message_routes
        .read()
        .expect("routes lock")
        .get("acp")
        .cloned()
        .ok_or_else(|| AppError::bad("ACP 消息路由未注册"))
}
