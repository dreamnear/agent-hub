//! CORS 层（agent-hub-multi-instance 批1 任务1）：浏览器直连远程实例的跨源放行。
//! 仅 allow_lan=true 时对 /api、/ws 前缀生效；loopback 单机（allow_lan=false）
//! 零 CORS 头——现状不变。Bearer 认证无 cookie，不开 allow-credentials。
//!
//! 层序约定：本中间件挂为**最外层**（lib.rs 中 `.layer()` 晚于 auth 即外层先跑），
//! OPTIONS 预检在此短路返回 204，**先于 auth_middleware**——预检请求不带
//! Authorization，落到 auth 会 401，浏览器拿不到预检结果跨源全断。
//! 401/4xx 响应同样被追加 ACAO 头：否则浏览器把 401 吞成 opaque 网络错误，
//! 前端无法区分「token 失效」与「实例离线」。

use axum::{
    extract::{Request, State},
    http::{header, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::api::SharedState;

/// 预检放行的请求头：Bearer token + JSON 体（浏览器预检只认显式枚举）
const ALLOW_HEADERS: &str = "Authorization, Content-Type";
const ALLOW_METHODS: &str = "GET, POST, PUT, DELETE, OPTIONS";
/// 预检结果缓存 10 分钟：减少每次请求前的 OPTIONS 往返
const MAX_AGE_SECS: &str = "600";

/// CORS 中间件：allow_lan 且路径命中 /api、/ws 前缀才参与，其余原样放行。
pub async fn cors_middleware(
    State(state): State<SharedState>,
    req: Request,
    next: Next,
) -> Response {
    // 单机同源（allow_lan=false）：不加任何 CORS 头，行为与多实例之前完全一致
    if !state.cfg.allow_lan {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if !path.starts_with("/api") && !path.starts_with("/ws") {
        return next.run(req).await;
    }
    if req.method() == axum::http::Method::OPTIONS {
        return preflight();
    }
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    res
}

/// OPTIONS 预检短路：204 + CORS 头，不进路由、不进 auth（中间件层序保证）。
fn preflight() -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    let h = res.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOW_HEADERS),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOW_METHODS),
    );
    h.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(MAX_AGE_SECS),
    );
    res
}
