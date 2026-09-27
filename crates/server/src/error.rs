use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// 统一 handler 错误：带状态码 + 消息透出，详细错误日志在服务端。
#[derive(Debug, thiserror::Error)]
#[error("{1}")]
pub struct AppError(pub StatusCode, pub anyhow::Error);

impl AppError {
    pub fn bad(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, anyhow::anyhow!(msg.into()))
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, anyhow::anyhow!(msg.into()))
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self(StatusCode::FORBIDDEN, anyhow::anyhow!(msg.into()))
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self(StatusCode::CONFLICT, anyhow::anyhow!(msg.into()))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // 4xx 是调用方问题记 warn，5xx 才是服务端故障记 error
        if self.0.is_client_error() {
            tracing::warn!(status = %self.0, error = %self.1, "handler rejected request");
        } else {
            tracing::error!(status = %self.0, error = %self.1, "handler error");
        }
        (self.0, self.1.to_string()).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, e)
    }
}
