//! CORS 层集成测试（agent-hub-multi-instance 批1 任务1）：
//! allow_lan on → OPTIONS 预检 204 且 CORS 头齐全（且不被 auth 401）、
//! GET 带 Origin 响应有 ACAO；allow_lan off → 同请求零 CORS 头（单机零变化）。

use std::{path::Path, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

fn fake_bin() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

fn cfg_with(dir: &tempfile::TempDir, allow_lan: bool) -> Config {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.allow_lan = allow_lan;
    // allow_lan=true 时设固定 token：预检不带 Bearer 也必须 204（短路先于 auth 的证明）
    cfg.token = Some("secret".into());
    cfg
}

fn app(cfg: Config) -> axum::Router {
    router(Arc::new(AppState::from_config(cfg)))
}

/// 预检请求：不带 Authorization（浏览器预检语义）
async fn options(app: axum::Router, uri: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("OPTIONS")
            .uri(uri)
            .header("Origin", "http://127.0.0.1:7800")
            .header("Access-Control-Request-Method", "GET")
            .header("Access-Control-Request-Headers", "authorization")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get_with_origin(app: axum::Router, uri: &str, bearer: bool) -> axum::http::Response<Body> {
    let mut b = Request::builder()
        .uri(uri)
        .header("Origin", "http://127.0.0.1:7800");
    if bearer {
        b = b.header("authorization", "Bearer secret");
    }
    app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap()
}

fn header<'r>(res: &'r axum::http::Response<Body>, name: &str) -> Option<&'r str> {
    res.headers().get(name).and_then(|v| v.to_str().ok())
}

/// allow_lan on：OPTIONS /api/agents → 204 且 CORS 头齐全；无 token 不 401（先于 auth）
#[tokio::test]
async fn preflight_short_circuits_before_auth() {
    let dir = tempfile::tempdir().unwrap();
    let res = options(app(cfg_with(&dir, true)), "/api/agents").await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(header(&res, "access-control-allow-origin"), Some("*"));
    let ah = header(&res, "access-control-allow-headers").unwrap_or("");
    assert!(
        ah.contains("Authorization"),
        "放行头须含 Authorization: {ah}"
    );
    assert!(ah.contains("Content-Type"), "放行头须含 Content-Type: {ah}");
    let am = header(&res, "access-control-allow-methods").unwrap_or("");
    assert!(am.contains("GET") && am.contains("POST") && am.contains("DELETE"));
    // 预检不要求 allow-credentials（Bearer 无 cookie）
    assert!(res
        .headers()
        .get("access-control-allow-credentials")
        .is_none());
}

/// allow_lan on：/ws 前缀同样覆盖（WS 握手前的预检放行）
#[tokio::test]
async fn preflight_covers_ws_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let res = options(app(cfg_with(&dir, true)), "/ws/events").await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(header(&res, "access-control-allow-origin"), Some("*"));
}

/// allow_lan on：GET 带 Origin → 响应追加 ACAO；401 响应同样带 ACAO（浏览器不吞状态码）
#[tokio::test]
async fn actual_requests_get_acao_even_on_401() {
    let dir = tempfile::tempdir().unwrap();
    let res = get_with_origin(app(cfg_with(&dir, true)), "/api/agents", true).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(header(&res, "access-control-allow-origin"), Some("*"));

    // 无 token 401 也带 ACAO：跨源下前端能读到 401（token 失效可辨）
    let res = get_with_origin(app(cfg_with(&dir, true)), "/api/agents", false).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(header(&res, "access-control-allow-origin"), Some("*"));
}

/// allow_lan off（loopback 单机）：GET/OPTIONS 均零 CORS 头——现状零变化
#[tokio::test]
async fn allow_lan_off_has_zero_cors_headers() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false));
    let res = get_with_origin(a.clone(), "/api/agents", false).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("access-control-allow-origin").is_none());

    let res = options(a, "/api/agents").await;
    assert!(res.headers().get("access-control-allow-origin").is_none());
    assert!(res.headers().get("access-control-allow-headers").is_none());
}

/// 静态路径（非 /api、/ws 前缀）即使 allow_lan 也不加 CORS 头
#[tokio::test]
async fn non_api_paths_stay_cors_free() {
    let dir = tempfile::tempdir().unwrap();
    let res = get_with_origin(app(cfg_with(&dir, true)), "/health", false).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("access-control-allow-origin").is_none());
}
