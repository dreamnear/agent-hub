//! 局域网 token 认证：仅 allow_lan=true 生效；localhost（loopback）直通免认证。
//! 静态资源放行（无 token 也要能加载 TokenGate 页面），API + WS 全覆盖。

use std::net::SocketAddr;
use std::sync::OnceLock;

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use crate::api::SharedState;
use crate::config::Config;

/// token 缓存（env > token_file > 生成）。OnceLock 进程内只解析一次。
static TOKEN: OnceLock<String> = OnceLock::new();

/// 认证需求判定（纯函数，单测锚点）：allow_lan=true 且来源非 loopback 才需要。
/// remote=None（oneshot 测试/无连接信息）按安全默认处理：allow_lan=true 即要求 token。
pub fn needs_auth(allow_lan: bool, remote: Option<SocketAddr>) -> bool {
    match remote {
        Some(addr) => allow_lan && !addr.ip().is_loopback(),
        None => allow_lan,
    }
}

/// 解析当前 token：env（cfg.token）优先直读；否则 OnceLock 缓存的「文件读取或生成」结果。
pub fn token_of(cfg: &Config) -> String {
    if let Some(t) = &cfg.token {
        return t.clone();
    }
    TOKEN
        .get_or_init(|| load_or_generate(&cfg.token_file))
        .clone()
}

/// 文件读取或生成（二次启动读同一文件复用同 token）。独立成函数便于持久化单测。
pub fn load_or_generate(path: &std::path::Path) -> String {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let t = existing.trim().to_string();
        if !t.is_empty() {
            return t;
        }
    }
    let t = Uuid::new_v4().to_string();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, &t);
    tracing::info!(file = %path.display(), "generated lan token");
    t
}

/// 启动时预热 token（仅 allow_lan 需要；提前生成/持久化，首请求不落盘）。
pub fn ensure_token(cfg: &Config) {
    if cfg.allow_lan {
        let _ = token_of(cfg);
    }
}

/// 常量时间字节比较（等长 XOR 累积；长度不等直接 false——token 为定长 uuid，长度不泄密）。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn is_local(req: &Request) -> bool {
    req.extensions()
        .get::<axum::extract::ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().is_loopback())
        .unwrap_or(true) // 无连接信息（oneshot 测试）视为本地
}

/// 认证中间件（layer 覆盖全部路由）：
/// 1. /api/auth/token：loopback 直通（handler 自判 allow_lan），非 loopback 404（防 token 泄露）
/// 2. /api、/ws：needs_auth 时校验 Bearer，失败 401
/// 3. 其余（静态资源）：放行
pub async fn auth_middleware(
    State(state): State<SharedState>,
    req: Request,
    next: Next,
) -> Response {
    let cfg = &state.cfg;
    let path = req.uri().path();
    let local = is_local(&req);

    if path == "/api/auth/token" {
        if local && cfg.allow_lan {
            return next.run(req).await;
        }
        return StatusCode::NOT_FOUND.into_response();
    }

    if path.starts_with("/api") || path.starts_with("/ws") {
        if !needs_auth(
            cfg.allow_lan,
            req.extensions()
                .get::<axum::extract::ConnectInfo<SocketAddr>>()
                .map(|c| c.0),
        ) {
            return next.run(req).await;
        }
        // Bearer header（API）或 ?token=（WS 无法自定义 header）
        let expected = token_of(cfg);
        let header_ok = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(|v| constant_time_eq(v.as_bytes(), format!("Bearer {expected}").as_bytes()))
            .unwrap_or(false);
        let query_ok = req
            .uri()
            .query()
            .map(|q| {
                q.split('&').any(|kv| {
                    kv.strip_prefix("token=")
                        .map(|v| constant_time_eq(v.as_bytes(), expected.as_bytes()))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if header_ok || query_ok {
            return next.run(req).await;
        }
        return StatusCode::UNAUTHORIZED.into_response();
    }

    next.run(req).await
}
