pub mod api;
pub mod auth;
pub mod config;
pub mod cors;
pub mod drivers;
pub mod error;
pub mod harness;
pub mod instances;
pub mod models;
pub mod nodelink;
pub mod notes;
pub mod platform;
pub mod plugins;
pub mod projects;
pub mod remote_install;
pub mod ssh_cmd;
pub mod static_assets;
pub mod tasks;
pub mod tunnel;

use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{get, post},
    Router,
};

pub fn router(state: Arc<api::AppState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws/events", get(api::ws::events))
        // 节点出站反连握手（gateway-node P1 任务5）：帧内凭据，auth 中间件豁免
        .route("/ws/node", get(crate::nodelink::gateway::node_ws))
        // 纯工具终端（agent-hub-terminal T5）：cwd 参数化 shell PTY，零会话耦合；
        // 门禁走 /ws 全局 auth（?token= 照旧）
        .route("/ws/terminal", get(api::terminal::terminal))
        .route("/ws/chat/{sessionId}", get(api::ws::chat))
        .route(
            "/ws/chat/{sessionId}/subagents/{subagentId}",
            get(api::ws::chat_subagent),
        )
        .route("/api/auth/token", get(api::auth::token))
        .merge(api::agents_config::router())
        .merge(api::agents::router())
        .merge(api::acp::router())
        .merge(api::messages::router())
        .merge(api::git_tree::router())
        .merge(api::docs::router())
        .merge(api::instances::router())
        .merge(api::nodes::router())
        .merge(api::harness::router())
        .route("/api/commands", get(api::commands::list_commands))
        .route("/api/upload", post(api::upload::upload))
        .route("/api/images/{filename}", get(api::upload::get_image))
        // 静态段优先于 {filename} 参数段（matchit 语义），by-path 不被参数路由吞掉
        .route("/api/images/by-path", get(api::upload::get_image_by_path))
        .merge(api::projects::router())
        .merge(api::notes::router())
        .merge(api::pi::router())
        .merge(api::tasks::router())
        // SPA 静态托管：根与通配放在 API 之后挂（API 未命中时才落到这里）
        .route("/", get(static_assets::index))
        .route("/{*path}", get(static_assets::asset))
        // token 认证覆盖 API + WS（静态放行，见 auth::auth_middleware）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ))
        // 图片上传 base64 体（×1.37 膨胀）：默认 2MB 不够，提至 8MB（P4 tester-r6 反馈）
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        // CORS 最外层（multi-instance 批1 任务1）：axum 后挂的 layer 先跑——
        // 预检 OPTIONS 在此短路，先于 auth；401 响应也被追加 ACAO（浏览器不吞状态码）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            cors::cors_middleware,
        ))
        .with_state(state)
}

async fn health() -> StatusCode {
    StatusCode::OK
}
