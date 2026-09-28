//! CORS 层集成测试（agent-hub-multi-instance 批1 任务1；r80 扩：loopback Origin 回显）：
//! loopback Origin（127.0.0.1/::1 任意端口）→ 无论 allow_lan 回显 ACAO + Vary: Origin
//! （SSH 隧道实例远端 loopback-only 模式可跨源读）；公网 Origin → 仅 allow_lan=true
//! 时 `*`；非 /api、/ws 路径零 CORS 头。

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
async fn options(app: axum::Router, uri: &str, origin: &str) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("OPTIONS")
            .uri(uri)
            .header("Origin", origin)
            .header("Access-Control-Request-Method", "GET")
            .header("Access-Control-Request-Headers", "authorization")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn get_with_origin(
    app: axum::Router,
    uri: &str,
    origin: &str,
    bearer: bool,
) -> axum::http::Response<Body> {
    let mut b = Request::builder().uri(uri).header("Origin", origin);
    if bearer {
        b = b.header("authorization", "Bearer secret");
    }
    app.oneshot(b.body(Body::empty()).unwrap()).await.unwrap()
}

fn header<'r>(res: &'r axum::http::Response<Body>, name: &str) -> Option<&'r str> {
    res.headers().get(name).and_then(|v| v.to_str().ok())
}

/// allow_lan on：OPTIONS /api/agents → 204 且 CORS 头齐全；无 token 不 401（先于 auth）。
/// loopback Origin → 回显该 Origin（r80 契约）
#[tokio::test]
async fn preflight_short_circuits_before_auth() {
    let dir = tempfile::tempdir().unwrap();
    let res = options(
        app(cfg_with(&dir, true)),
        "/api/agents",
        "http://127.0.0.1:7800",
    )
    .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        header(&res, "access-control-allow-origin"),
        Some("http://127.0.0.1:7800")
    );
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

/// allow_lan on + 公网 Origin：预检回 `*`（既有行为不变）
#[tokio::test]
async fn preflight_public_origin_gets_star() {
    let dir = tempfile::tempdir().unwrap();
    let res = options(
        app(cfg_with(&dir, true)),
        "/api/agents",
        "http://192.168.1.100:7800",
    )
    .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(header(&res, "access-control-allow-origin"), Some("*"));
}

/// allow_lan on：/ws 前缀同样覆盖（WS 握手前的预检放行）
#[tokio::test]
async fn preflight_covers_ws_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let res = options(
        app(cfg_with(&dir, true)),
        "/ws/events",
        "http://127.0.0.1:7800",
    )
    .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        header(&res, "access-control-allow-origin"),
        Some("http://127.0.0.1:7800")
    );
}

/// allow_lan on：GET 带 loopback Origin → 回显 ACAO；401 响应同样带（浏览器不吞状态码）
#[tokio::test]
async fn actual_requests_get_acao_even_on_401() {
    let dir = tempfile::tempdir().unwrap();
    let origin = "http://127.0.0.1:7800";
    let res = get_with_origin(app(cfg_with(&dir, true)), "/api/agents", origin, true).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(header(&res, "access-control-allow-origin"), Some(origin));

    // 无 token 401 也带 ACAO：跨源下前端能读到 401（token 失效可辨）
    let res = get_with_origin(app(cfg_with(&dir, true)), "/api/agents", origin, false).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(header(&res, "access-control-allow-origin"), Some(origin));
}

/// **r80 修复主场景**：allow_lan **off**（loopback-only 远端，如 H253）+ loopback
/// Origin → GET/OPTIONS 均回显 ACAO + Vary: Origin——此前零 CORS 头导致隧道场景必死
#[tokio::test]
async fn loopback_origin_allowed_without_lan() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false));

    for origin in ["http://127.0.0.1:7800", "http://[::1]:58111"] {
        let res = get_with_origin(a.clone(), "/api/agents", origin, false).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(header(&res, "access-control-allow-origin"), Some(origin));
        assert_eq!(header(&res, "vary"), Some("Origin"));

        let res = options(a.clone(), "/api/agents", origin).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(header(&res, "access-control-allow-origin"), Some(origin));
        assert!(header(&res, "access-control-allow-headers")
            .unwrap_or("")
            .contains("Authorization"));
    }
}

/// allow_lan off + 公网 Origin：GET/OPTIONS 均零 CORS 头——公网源门控不变（防跨源读）
#[tokio::test]
async fn public_origin_gated_without_lan() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false));
    let res = get_with_origin(a.clone(), "/api/agents", "http://192.168.1.50:9000", false).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("access-control-allow-origin").is_none());

    let res = options(a, "/api/agents", "http://192.168.1.50:9000").await;
    assert!(res.headers().get("access-control-allow-origin").is_none());
    assert!(res.headers().get("access-control-allow-headers").is_none());
}

/// 静态路径（非 /api、/ws 前缀）即使 allow_lan 也不加 CORS 头
#[tokio::test]
async fn non_api_paths_stay_cors_free() {
    let dir = tempfile::tempdir().unwrap();
    let res = get_with_origin(
        app(cfg_with(&dir, true)),
        "/health",
        "http://127.0.0.1:7800",
        false,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.headers().get("access-control-allow-origin").is_none());
}
