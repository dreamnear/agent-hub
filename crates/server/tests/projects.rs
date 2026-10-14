use std::{path::Path, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

fn fake_bin() -> std::path::PathBuf {
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
    let data = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, data)
}

fn test_app_with_projects_file(
    dir: &tempfile::TempDir,
    projects_file: std::path::PathBuf,
) -> axum::Router {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().to_path_buf();
    cfg.projects_file = projects_file;
    router(Arc::new(AppState::from_config(cfg)))
}

#[tokio::test]
async fn projects_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("projects.json");
    let app = test_app_with_projects_file(&dir, file);

    let (status, body) = req(app.clone(), "GET", "/api/projects", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0, "默认空");

    let sub_a = tempfile::tempdir().unwrap();
    let sub_b = tempfile::tempdir().unwrap();
    let (status, _) = req(
        app.clone(),
        "PUT",
        "/api/projects",
        Some(json!({ "paths": [sub_a.path(), sub_b.path()] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = req(app, "GET", "/api/projects", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn put_rejects_nonexistent_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("projects.json");
    let app = test_app_with_projects_file(&dir, file);
    let (status, _) = req(
        app,
        "PUT",
        "/api/projects",
        Some(json!({ "paths": ["/definitely/not/a/dir"] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_rejects_dotdot() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("projects.json");
    let app = test_app_with_projects_file(&dir, file);
    let (status, _) = req(
        app,
        "PUT",
        "/api/projects",
        Some(json!({ "paths": [format!("{}/..", dir.path().display())] })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
