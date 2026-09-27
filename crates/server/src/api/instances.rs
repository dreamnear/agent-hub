//! 多实例 REST（agent-hub-multi-instance 批1 任务2/3）：/api/instances CRUD +
//! /{id}/tunnel/start|stop|status。token 经 GET 下发给浏览器（浏览器直连方案固有属性）。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};

use crate::{
    api::SharedState,
    error::AppError,
    instances::{InstanceConfig, InstancesStore},
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/instances",
            get(list_instances)
                .post(create_instance)
                .put(update_instance),
        )
        .route("/api/instances/{id}", delete(delete_instance))
        .route("/api/instances/{id}/base-url", get(get_instance_base_url))
        .route("/api/instances/{id}/tunnel/start", post(tunnel_start))
        .route("/api/instances/{id}/tunnel/stop", post(tunnel_stop))
        .route("/api/instances/{id}/tunnel/status", get(tunnel_status))
}

/// direct 实例连接测试（打 /health）；ssh-tunnel 需先 start 隧道再测→前端走 base-url + /health
async fn get_instance_base_url(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let store = InstancesStore::new(&state.cfg);
    let inst = store
        .get(&id)
        .await
        .ok_or_else(|| AppError::not_found("实例不存在"))?;
    let base = inst.base_url().map_err(AppError::bad)?;
    Ok(Json(serde_json::json!({ "baseUrl": base })))
}

async fn list_instances(State(state): State<SharedState>) -> Json<Vec<InstanceConfig>> {
    Json(InstancesStore::new(&state.cfg).load().await)
}

/// wss → https 归一化（任务11）：direct 远程仅允许 https/wss 语义（requirement 红线）。
/// 实例 url 是 fetch 的 http(s) base，浏览器无法用 wss scheme 发 HTTP 请求，保存前
/// 统一改写为等价 https；ws（明文）不经此路径，validate 照常拒绝。
fn normalize_wss(mut inst: InstanceConfig) -> InstanceConfig {
    if inst.mode == crate::instances::InstanceMode::Direct {
        if let Some(url) = inst.url.take() {
            if let Some(rest) = url.strip_prefix("wss://") {
                inst.url = Some(format!("https://{rest}"));
            } else {
                inst.url = Some(url);
            }
        }
    }
    inst
}

async fn create_instance(
    State(state): State<SharedState>,
    Json(inst): Json<InstanceConfig>,
) -> Result<Json<InstanceConfig>, AppError> {
    let inst = normalize_wss(inst);
    inst.validate().map_err(AppError::bad)?;
    let store = InstancesStore::new(&state.cfg);
    // 同名/同 id 已存在 → 409（幂等冲突，前端提示）
    if store.get(&inst.id).await.is_some() {
        return Err(AppError::conflict("实例已存在"));
    }
    store.upsert(&inst).await?;
    Ok(Json(inst))
}

async fn update_instance(
    State(state): State<SharedState>,
    Json(inst): Json<InstanceConfig>,
) -> Result<Json<InstanceConfig>, AppError> {
    let inst = normalize_wss(inst);
    inst.validate().map_err(AppError::bad)?;
    let store = InstancesStore::new(&state.cfg);
    store.upsert(&inst).await?;
    Ok(Json(inst))
}

async fn delete_instance(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let store = InstancesStore::new(&state.cfg);
    let removed = store.delete(&id).await?;
    // 删除先 stop 隧道（进程回收）
    if let Some(inst) = &removed {
        if inst.mode == crate::instances::InstanceMode::SshTunnel {
            state.tunnels.stop(&id).await;
        }
    }
    Ok(if removed.is_some() {
        StatusCode::NO_CONTENT
    } else {
        // 不存在也 204（幂等删除）；前端据此刷新列表
        StatusCode::NO_CONTENT
    })
}

async fn tunnel_start(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let store = InstancesStore::new(&state.cfg);
    let inst = store
        .get(&id)
        .await
        .ok_or_else(|| AppError::not_found("实例不存在"))?;
    if inst.mode != crate::instances::InstanceMode::SshTunnel {
        return Err(AppError::bad("仅 ssh-tunnel 实例支持隧道"));
    }
    let local_port = state
        .tunnels
        .start(&inst)
        .await
        .map_err(|e| AppError::bad(e.to_string()))?;
    // 回写 local_port 供前端连接 + 持久化（重启后前端读到的 localPort 仍有效则复用）
    let mut updated = inst;
    updated.local_port = Some(local_port);
    store.upsert(&updated).await?;
    Ok(Json(serde_json::json!({ "localPort": local_port })))
}

async fn tunnel_stop(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let stopped = state.tunnels.stop(&id).await;
    Ok(Json(serde_json::json!({ "stopped": stopped })))
}

async fn tunnel_status(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let status = state.tunnels.status(&id).await;
    Ok(Json(serde_json::json!(status)))
}
