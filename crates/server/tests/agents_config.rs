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

fn app_with_agents_dir(dir: &tempfile::TempDir) -> axum::Router {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.agents_dir = dir.path().to_path_buf();
    router(Arc::new(AppState::from_config(cfg)))
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

fn seed(dir: &tempfile::TempDir) {
    std::fs::write(
        dir.path().join("architect.md"),
        "---\nname: architect\ndescription: 架构专家\ntools: [\"Read\"]\nmodel: opus\n---\n\n正文",
    )
    .unwrap();
}

#[tokio::test]
async fn list_shows_frontmatter_summary() {
    let dir = tempfile::tempdir().unwrap();
    seed(&dir);
    let (status, body) = req(app_with_agents_dir(&dir), "GET", "/api/agents-config", None).await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "architect");
    assert_eq!(arr[0]["description"], "架构专家");
    assert_eq!(arr[0]["model"], "opus");
}

#[tokio::test]
async fn get_and_put_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    seed(&dir);
    let app = app_with_agents_dir(&dir);

    let (status, body) = req(app.clone(), "GET", "/api/agents-config/architect", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["content"].as_str().unwrap().contains("正文"));

    let (status, _) = req(
        app.clone(),
        "PUT",
        "/api/agents-config/architect",
        Some(json!({"content": "---\nname: architect\ndescription: 更新后\n---\n\n新正文"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let on_disk = std::fs::read_to_string(dir.path().join("architect.md")).unwrap();
    assert!(on_disk.contains("更新后"), "写回应落盘真实文件");

    let (status, body) = req(app, "GET", "/api/agents-config/architect", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["description"], "更新后");
}

#[tokio::test]
async fn rejects_bad_name_and_empty_content() {
    let dir = tempfile::tempdir().unwrap();
    seed(&dir);
    let app = app_with_agents_dir(&dir);
    let (status, _) = req(
        app.clone(),
        "PUT",
        "/api/agents-config/..%2Fescape",
        Some(json!({"content": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = req(
        app,
        "PUT",
        "/api/agents-config/architect",
        Some(json!({"content": "   "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
