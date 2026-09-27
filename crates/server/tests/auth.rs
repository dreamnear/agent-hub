use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use server::{api::AppState, auth, config::Config, router};
use tower::ServiceExt;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

fn cfg_with(dir: &tempfile::TempDir, allow_lan: bool, token: Option<String>) -> Config {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.allow_lan = allow_lan;
    cfg.token = token;
    cfg
}

fn app(cfg: Config) -> axum::Router {
    router(Arc::new(AppState::from_config(cfg)))
}

async fn get(app: axum::Router, uri: &str, bearer: Option<&str>) -> StatusCode {
    let builder = Request::builder().uri(uri);
    let request = match bearer {
        Some(t) => builder
            .header("authorization", format!("Bearer {t}"))
            .body(Body::empty())
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    app.oneshot(request).await.unwrap().status()
}

#[test]
fn needs_auth_truth_table() {
    let lo: SocketAddr = "127.0.0.1:5000".parse().unwrap();
    let lan: SocketAddr = "192.168.1.8:5000".parse().unwrap();
    // 默认（allow_lan=false）永不认证
    assert!(!auth::needs_auth(false, Some(lo)));
    assert!(!auth::needs_auth(false, Some(lan)));
    assert!(!auth::needs_auth(false, None));
    // allow_lan=true：loopback 直通，非 loopback / 未知来源要求 token
    assert!(!auth::needs_auth(true, Some(lo)));
    assert!(auth::needs_auth(true, Some(lan)));
    assert!(auth::needs_auth(true, None));
}

/// allow_lan=false：默认免认证不破（无 token 200）
#[tokio::test]
async fn allow_lan_off_passthrough() {
    let dir = tempfile::tempdir().unwrap();
    let status = get(app(cfg_with(&dir, false, None)), "/api/agents", None).await;
    assert_eq!(status, StatusCode::OK);
}

/// allow_lan=true：无 token 401
#[tokio::test]
async fn allow_lan_on_missing_token_is_401() {
    let dir = tempfile::tempdir().unwrap();
    let status = get(
        app(cfg_with(&dir, true, Some("secret".into()))),
        "/api/agents",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// allow_lan=true：错 token 401
#[tokio::test]
async fn allow_lan_on_wrong_token_is_401() {
    let dir = tempfile::tempdir().unwrap();
    let status = get(
        app(cfg_with(&dir, true, Some("secret".into()))),
        "/api/agents",
        Some("wrong"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// allow_lan=true：正确 Bearer 200
#[tokio::test]
async fn allow_lan_on_correct_token_is_200() {
    let dir = tempfile::tempdir().unwrap();
    let status = get(
        app(cfg_with(&dir, true, Some("secret".into()))),
        "/api/agents",
        Some("secret"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// /api/auth/token：allow_lan=false → 404；allow_lan=true（loopback 语义）→ 200 带 token
#[tokio::test]
async fn auth_token_endpoint_gated_by_allow_lan() {
    let dir = tempfile::tempdir().unwrap();
    let (status, _) = req_json(app(cfg_with(&dir, false, None)), "/api/auth/token").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = req_json(
        app(cfg_with(&dir, true, Some("secret".into()))),
        "/api/auth/token",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["token"], "secret");
}

async fn req_json(app: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let res = app
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// token 持久化：首次生成写文件；二次（新调用）读同文件复用
#[test]
fn token_persist_and_reuse() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("token");
    let t1 = auth::load_or_generate(&f);
    assert!(!t1.is_empty());
    assert!(f.exists(), "token 应写盘");
    let t2 = auth::load_or_generate(&f);
    assert_eq!(t1, t2, "二次应复用同一 token");
}

/// uuid 形态冒烟（v4 = 36 字符 4 段）
#[test]
fn generated_token_is_uuid_shape() {
    let dir = tempfile::tempdir().unwrap();
    let t = auth::load_or_generate(&dir.path().join("t"));
    assert_eq!(t.len(), 36);
    assert_eq!(t.matches('-').count(), 4);
}

/// loopback ConnectInfo 直通：注入 loopback extension 后 allow_lan=true 无 token 也 200
#[tokio::test]
async fn loopback_bypasses_auth_even_when_allow_lan() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(cfg_with(&dir, true, Some("secret".into())));
    let lo: SocketAddr = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 5555);
    let req = Request::get("/api/agents")
        .extension(axum::extract::ConnectInfo(lo))
        .body(Body::empty())
        .unwrap();
    let status = app.oneshot(req).await.unwrap().status();
    assert_eq!(status, StatusCode::OK, "localhost 直通免认证");
}
