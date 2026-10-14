//! agents REST API（axum 0.8 `{param}` 语法）。
//! driver-unify 批 2.1：全部走 DriverRegistry（轨二），api 层零 claude 直调；
//! 中断覆盖已内化进 claude 驱动（批 2.3），list 只见成品。

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::drivers::{AgentDriver, StartSpec, Unsupported};
use crate::error::AppError;
use crate::models::AgentSummary;

use super::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/agents", get(list_agents))
        .route("/api/drivers", get(list_drivers))
        .route("/api/drivers/status", get(drivers_status))
        .route("/api/agents/{driver}", post(start_agent))
        .route("/api/agents/{driver}/{id}", get(get_agent))
        .route("/api/agents/{driver}/{id}/logs", get(agent_logs))
        .route("/api/agents/{driver}/{id}/stop", post(stop_agent))
        .route("/api/agents/{driver}/{id}/rm", post(remove_agent))
        .route("/api/agents/{driver}/{id}/respawn", post(respawn_agent))
        .route("/api/agents/{driver}/{id}/ignore", post(ignore_agent))
}

/// interactive「从列表移除」（R8）：按 sessionId 追加忽略记录，幂等；仅 claude
/// 驱动（interactive 条目只在 claude 清单出现）。
async fn ignore_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Json(body): Json<serde_json::Value>,
) -> Result<StatusCode, AppError> {
    if driver != "claude" {
        return Err(AppError::bad("ignore 仅支持 claude 驱动"));
    }
    if id.is_empty() {
        return Err(AppError::bad("id 为空"));
    }
    let sid = body
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if sid.is_empty() {
        return Err(AppError::bad("sessionId 为空"));
    }
    crate::drivers::claude::unlist_interactive(&state.cfg.ignored_interactive_file, sid)
        .map_err(app_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_agents(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AgentSummary>>, AppError> {
    let all = params.get("all").map(|v| v == "1").unwrap_or(false);
    let agents = state.drivers.list_all(all).await?;
    Ok(Json(agents))
}

async fn get_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<AgentSummary>, AppError> {
    let d = driver_of(&state, &driver)?;
    let found = d.get(&id).await.map_err(app_err)?;
    found
        .map(Json)
        .ok_or_else(|| AppError::not_found(format!("agent {id} 不存在")))
}

async fn agent_logs(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let d = driver_of(&state, &driver)?;
    let logs = d.logs(&id).await.map_err(app_err)?;
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

/// 可建驱动清单（StartDialog 选项事实源，注册序）：前端据此渲染 driver 选项，
/// 不再硬编码名单。
async fn list_drivers(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.drivers.names()))
}

/// 驱动降级状态（authError 等）：前端侧栏「凭据失效」banner 事实源。
async fn drivers_status(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for name in state.drivers.names() {
        if let Ok(d) = state.drivers.get(name) {
            map.insert(
                name.to_string(),
                serde_json::json!({ "authError": d.auth_error() }),
            );
        }
    }
    Json(serde_json::Value::Object(map))
}

async fn start_agent(
    State(state): State<SharedState>,
    AxumPath(driver): AxumPath<String>,
    Json(body): Json<StartBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let d = driver_of(&state, &driver)?;
    validate_cwd(&body.cwd).await?;
    let spec = StartSpec {
        prompt: body.prompt,
        name: body.name,
        model: body.model,
        effort: body.effort,
        cwd: body.cwd,
    };
    let id = d.start(&spec).await.map_err(app_err)?;
    Ok(Json(json!({ "id": id })))
}

async fn stop_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    let d = driver_of(&state, &driver)?;
    d.stop(&id).await.map_err(app_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn remove_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    let d = driver_of(&state, &driver)?;
    d.remove(&id).await.map_err(app_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// respawn：重启已完成/退出的 bg 会话（P3）。
async fn respawn_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    let d = driver_of(&state, &driver)?;
    d.respawn(&id).await.map_err(app_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// ensure_driver 的注册表替身：按名取驱动；未知名 → 404（行为与批前 ensure_driver 一致）。
fn driver_of(state: &SharedState, driver: &str) -> Result<Arc<dyn AgentDriver>, AppError> {
    state
        .drivers
        .get(driver)
        .map_err(|_| AppError::not_found(format!("未知 driver: {driver}")))
}

/// 驱动域错误 → HTTP：Unsupported/NotFound → 404（批1 定案；respawn 域错误替代
/// "No job matching" 字符串匹配，K7），其余 400。
pub(crate) fn app_err(e: anyhow::Error) -> AppError {
    if e.downcast_ref::<Unsupported>().is_some()
        || e.downcast_ref::<crate::drivers::NotFound>().is_some()
    {
        AppError::not_found(e.to_string())
    } else {
        AppError::bad(e.to_string())
    }
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
