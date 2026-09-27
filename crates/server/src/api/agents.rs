//! agents REST API（axum 0.8 `{param}` 语法）。

use std::collections::HashMap;
use std::path::Path;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::models::AgentSummary;

use super::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/agents", get(list_agents))
        .route("/api/agents/{driver}", post(start_agent))
        .route("/api/agents/{driver}/{id}", get(get_agent))
        .route("/api/agents/{driver}/{id}/logs", get(agent_logs))
        .route("/api/agents/{driver}/{id}/stop", post(stop_agent))
        .route("/api/agents/{driver}/{id}/rm", post(remove_agent))
        .route("/api/agents/{driver}/{id}/respawn", post(respawn_agent))
}

async fn list_agents(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AgentSummary>>, AppError> {
    let all = params.get("all").map(|v| v == "1").unwrap_or(false);
    let mut agents = state.driver.list(all).await?;
    state.apply_interrupt_overrides(&mut agents).await;
    agents.extend(acp_summaries(&state));
    Ok(Json(agents))
}

/// ACP 会话 → 侧栏条目（acp-omp 批2 任务6）：id=ACP sessionId（chat 房间/WS 同键），
/// kind=acp（非 interactive，不被 bg 过滤隐藏）；started_at 注册表未记，留 None。
fn acp_summaries(state: &SharedState) -> Vec<AgentSummary> {
    state
        .acp_registry
        .list()
        .iter()
        .map(|s| AgentSummary {
            driver: "acp".into(),
            id: s.id.clone(),
            name: Some(s.agent.clone()),
            cwd: Some(s.cwd.clone()),
            kind: Some("acp".into()),
            raw_state: Some(s.status.as_str().into()),
            group: s.status.group(),
            detail: s.model.clone(),
            tokens: None,
            started_at: None,
            session_id: Some(s.id.clone()),
        })
        .collect()
}

async fn get_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<AgentSummary>, AppError> {
    ensure_driver(&driver)?;
    let mut agents = state.driver.list(true).await?;
    state.apply_interrupt_overrides(&mut agents).await;
    let found = agents.into_iter().find(|a| a.id == id);
    found
        .map(Json)
        .ok_or_else(|| AppError::not_found(format!("agent {id} 不存在")))
}

async fn agent_logs(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_driver(&driver)?;
    let logs = crate::drivers::claude::cli::logs(&state.driver.bin, &id).await?;
    Ok(Json(json!({ "logs": logs })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartBody {
    pub prompt: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub cwd: String,
    pub effort: Option<String>,
}

async fn start_agent(
    State(state): State<SharedState>,
    AxumPath(driver): AxumPath<String>,
    Json(body): Json<StartBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    ensure_driver(&driver)?;
    validate_cwd(&body.cwd).await?;
    let req = crate::drivers::claude::cli::StartReq {
        prompt: body.prompt,
        name: body.name,
        model: body.model,
        effort: body.effort,
    };
    let id = crate::drivers::claude::cli::start_bg(&state.driver.bin, Path::new(&body.cwd), &req)
        .await?;
    Ok(Json(json!({ "id": id })))
}

async fn stop_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    ensure_driver(&driver)?;
    crate::drivers::claude::cli::stop(&state.driver.bin, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    ensure_driver(&driver)?;
    crate::drivers::claude::cli::remove(&state.driver.bin, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// respawn：重启已完成/退出的 bg 会话（P3）。
async fn respawn_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    ensure_driver(&driver)?;
    crate::drivers::claude::attach::validate_id(&id)
        .map_err(|e| AppError::bad(format!("非法 agent id: {e}")))?;
    crate::drivers::claude::cli::respawn(&state.driver.bin, &id)
        .await
        .map_err(|e| {
            // CLI 对不存在会话报 "No job matching '<id>'" → 404（对齐 detail 路由语义）
            let msg = format!("{e:#}");
            if msg.contains("No job matching") {
                AppError::not_found(format!("agent {id} 不存在"))
            } else {
                AppError::bad(msg)
            }
        })?;
    Ok(StatusCode::NO_CONTENT)
}

fn ensure_driver(driver: &str) -> Result<(), AppError> {
    if driver != "claude" {
        return Err(AppError::not_found(format!("未知 driver: {driver}")));
    }
    Ok(())
}

/// cwd 安全校验：必须存在、是目录、不含 `..` 分量。
pub(crate) async fn validate_cwd(cwd: &str) -> Result<(), AppError> {
    if Path::new(cwd)
        .components()
        .any(|c| c == std::path::Component::ParentDir)
    {
        return Err(AppError::bad("cwd 不允许包含 .. 分量"));
    }
    let is_dir = tokio::fs::metadata(cwd)
        .await
        .map(|m| m.is_dir())
        .unwrap_or(false);
    if !is_dir {
        return Err(AppError::bad(format!("cwd 不存在或不是目录: {cwd}")));
    }
    Ok(())
}
