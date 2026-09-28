//! CORS 层（agent-hub-multi-instance 批1 任务1；r80 修复：loopback Origin 放行）。
//!
//! 放行规则：
//! - 请求 `Origin` 主机为 loopback（127.0.0.1 / ::1，任意端口）→ **无论 allow_lan**
//!   回显该 Origin（附 Vary: Origin）。SSH 隧道实例的远端常以 loopback-only 模式跑
//!   （安全默认，如 H253），此前零 CORS 头 → hub 页面（127.0.0.1 源）跨源拉隧道
//!   端口必然失败。安全论证：允许 loopback-origin 跨源 = 本机页面可读本机 API，
//!   与既有信任模型一致（auth 层 loopback 连接本就免 token）；公网恶意源 Origin
//!   非 loopback 不放行（Origin 由浏览器按真实源填写，不可伪造，防 DNS rebinding
//!   式跨源读）。
//! - 其余（公网 Origin / 无 Origin 同源请求）→ 仅 allow_lan=true 时 `*` 放行
//!   （行为不变）。
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

/// Origin 主机是否 loopback（`http(s)://host[:port]`，host ∈ {127.0.0.1, ::1}，端口任意）。
/// 解析失败 / 非 http(s) scheme / 其他主机 → false（按公网源走 allow_lan 门控）。
fn is_loopback_origin(origin: &str) -> bool {
    let rest = match origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    {
        Some(r) => r,
        None => return false,
    };
    let host_port = rest.split('/').next().unwrap_or("");
    // IPv6 字面量带括号：[::1]:7800 → 括号内即主机；否则按最后一个 ':' 切掉端口
    let host = if let Some(inner) = host_port.strip_prefix('[') {
        match inner.split_once(']') {
            Some((h, _)) => h,
            None => return false,
        }
    } else {
        host_port.rsplit_once(':').map_or(host_port, |(h, _)| h)
    };
    host == "127.0.0.1" || host == "::1"
}

/// CORS 中间件：/api、/ws 前缀才参与，其余原样放行。
pub async fn cors_middleware(
    State(state): State<SharedState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api") && !path.starts_with("/ws") {
        return next.run(req).await;
    }
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let loopback = is_loopback_origin(origin);

    // 公网源仍受 allow_lan 门控；loopback 源不受（本机页面读本机 API）
    if !state.cfg.allow_lan && !loopback {
        return next.run(req).await;
    }

    // loopback → 回显 Origin（同机多端口各源各自匹配，Vary: Origin 防缓存串源）；
    // 其余 → `*`（Bearer 无 cookie，不开 allow-credentials）
    let acao = if loopback {
        // origin 读自合法 header（to_str 已过 ASCII 校验），回显必为合法值
        HeaderValue::from_str(origin).unwrap_or(HeaderValue::from_static("*"))
    } else {
        HeaderValue::from_static("*")
    };

    if req.method() == axum::http::Method::OPTIONS {
        let mut res = preflight(acao);
        if loopback {
            res.headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Origin"));
        }
        return res;
    }
    let mut res = next.run(req).await;
    res.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, acao);
    if loopback {
        res.headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    res
}

/// OPTIONS 预检短路：204 + CORS 头（ACAO 由调用方按 Origin 决定），不进路由、不进 auth（中间件层序保证）。
fn preflight(acao: HeaderValue) -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, acao);
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

#[cfg(test)]
mod tests {
    use super::is_loopback_origin;

    #[test]
    fn loopback_origins_any_port() {
        assert!(is_loopback_origin("http://127.0.0.1:7800"));
        assert!(is_loopback_origin("http://127.0.0.1")); // 默认端口省略
        assert!(is_loopback_origin("http://[::1]:58111"));
        assert!(is_loopback_origin("https://127.0.0.1:7800"));
    }

    #[test]
    fn non_loopback_or_malformed_origins_rejected() {
        assert!(!is_loopback_origin("http://192.168.1.100:7800"));
        assert!(!is_loopback_origin("http://example.com"));
        assert!(!is_loopback_origin("http://127.0.0.1.evil.com:7800")); // 前缀伪装
        assert!(!is_loopback_origin("null"));
        assert!(!is_loopback_origin(""));
        assert!(!is_loopback_origin("ftp://127.0.0.1:7800"));
        assert!(!is_loopback_origin("http://[::1:7800")); // 括号未闭合
        assert!(!is_loopback_origin("http://[fd00::1]:7800")); // 非回环 v6
    }
}
