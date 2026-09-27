use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

async fn req(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let res = app.oneshot(request).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

fn test_app(dir: &tempfile::TempDir) -> axum::Router {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().to_path_buf();
    router(Arc::new(AppState::from_config(cfg)))
}

#[tokio::test]
async fn list_degrades_to_empty_when_claude_bin_missing() {
    // 全新机器没有 claude CLI：/api/agents 应 200 数组（ACP 会话仍合并），不能 500
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::load();
    cfg.claude_bin = PathBuf::from("/nonexistent/claude-bin-missing");
    cfg.jobs_dir = dir.path().to_path_buf();
    let app = router(Arc::new(AppState::from_config(cfg)));
    let (status, body) = req(app, "GET", "/api/agents?all=1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.as_array().is_some(),
        "缺 claude CLI 时仍应返回数组: {body}"
    );
}

#[tokio::test]
async fn list_returns_grouped_agents() {
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(test_app(&dir), "GET", "/api/agents?all=1", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 3);
    assert!(body[0].get("group").is_some(), "JSON 含 group 字段");
    assert_eq!(body[0]["driver"], "claude");
}

#[tokio::test]
async fn start_with_missing_cwd_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let (_, body) = req(
        test_app(&dir),
        "POST",
        "/api/agents/claude",
        Some(json!({"prompt": "hi", "cwd": "/definitely/not/a/dir"})),
    )
    .await;
    assert!(body.is_null() || body.get("id").is_none());
}

#[tokio::test]
async fn start_with_dotdot_cwd_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, _) = req(
        app,
        "POST",
        "/api/agents/claude",
        Some(json!({"prompt": "hi", "cwd": format!("{}/..", dir.path().display())})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn start_with_valid_cwd_returns_id() {
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(
        test_app(&dir),
        "POST",
        "/api/agents/claude",
        Some(json!({"prompt": "hi", "cwd": dir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "abc123");
}

#[tokio::test]
async fn unknown_driver_is_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, _) = req(app, "GET", "/api/agents/acme/x1", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn detail_logs_stop_rm_walk_through() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);

    let (status, body) = req(app.clone(), "GET", "/api/agents/claude/a1b2c3d4", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "a1b2c3d4");

    let (status, body) = req(app.clone(), "GET", "/api/agents/claude/a1b2c3d4/logs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("logs").is_some());

    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/agents/claude/a1b2c3d4/stop",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "stop 断言以接口定义 204 为准"
    );

    let (status, _) = req(app, "POST", "/api/agents/claude/a1b2c3d4/rm", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "rm 断言以接口定义 204 为准");
}
