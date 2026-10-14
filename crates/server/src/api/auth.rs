//! token 数据接口：仅 loopback + allow_lan 可读（middleware 已门控非 loopback → 404）。

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

use crate::{api::SharedState, auth};

/// GET /api/auth/token → { "token": "..." }；allow_lan=false → 404。
/// 非 loopback 请求在 middleware 已被 404（防 token 泄露到局域网）。
pub async fn token(State(state): State<SharedState>) -> impl IntoResponse {
    if !state.cfg.allow_lan {
        return (StatusCode::NOT_FOUND, Json(json!({})));
    }
    let t = auth::token_of(&state.cfg);
    (StatusCode::OK, Json(json!({ "token": t })))
}
