//! 工程便签 API 集成测试（agent-hub-notes）：roundtrip + 归一化共享 + 401 鉴权
//! + `..` 拒绝 + 落盘 0600。

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

fn cfg_with(dir: &tempfile::TempDir, allow_lan: bool, token: Option<String>) -> Config {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.notes_file = dir.path().join("notes.json");
    cfg.allow_lan = allow_lan;
    cfg.token = token;
    cfg
}

fn app(cfg: Config) -> axum::Router {
    router(Arc::new(AppState::from_config(cfg)))
}

async fn req(a: axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .header("authorization", "Bearer test-token")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder
            .header("authorization", "Bearer test-token")
            .body(Body::empty())
            .unwrap(),
    };
    let res = a.oneshot(request).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// PUT 后 GET 同内容；尾斜杠/重复斜杠变体命中同一键（跨链路共享的锚点）
#[tokio::test]
async fn notes_roundtrip_and_normalized_key_shared() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false, None));

    let (status, _) = req(
        a.clone(),
        "PUT",
        "/api/notes",
        Some(json!({ "path": "/repo/main", "content": "端口: 8080\n账号 admin/test123" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 原路径 / 尾斜杠 / 双斜杠：同一张便签
    for variant in ["/repo/main", "/repo/main/", "//repo/main/"] {
        let (status, body) = req(
            a.clone(),
            "GET",
            &format!("/api/notes?path={}", uri_encode(variant)),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["content"], "端口: 8080\n账号 admin/test123",
            "变体 {variant} 应命中同一便签"
        );
    }

    // 未写过内容的工程 → content null
    let (status, body) = req(a, "GET", "/api/notes?path=/repo/other", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["content"].is_null());
}

fn uri_encode(s: &str) -> String {
    s.replace('/', "%2F")
}

/// `..` 路径 400（对齐 projects 写路径防线）
#[tokio::test]
async fn put_rejects_dotdot() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false, None));
    let (status, _) = req(
        a,
        "PUT",
        "/api/notes",
        Some(json!({ "path": "/repo/../main", "content": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// ocr-review 中：GET 与 PUT 对 `..` 校验一致——GET 同样 400，
/// 防两端口径不一形成不一致键空间（且含 `..` 的键未来若落文件操作即穿越）。
#[tokio::test]
async fn get_rejects_dotdot() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false, None));
    let (status, _) = req(
        a,
        "GET",
        &format!("/api/notes?path={}", uri_encode("/repo/../main")),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "GET 也必须拒绝 .. 路径");
}

/// ocr-review 中：单条便签限长（防大 payload 撑爆聚合序列化/磁盘）——超限 413
#[tokio::test]
async fn put_rejects_oversized_content() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false, None));
    let big = "x".repeat(64 * 1024 + 1);
    let (status, _) = req(
        a,
        "PUT",
        "/api/notes",
        Some(json!({ "path": "/repo/main", "content": big })),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "超 64KiB 应 413");
}

/// allow_lan=true：无 token 读写一律 401（鉴权中间件覆盖便签端点）
#[tokio::test]
async fn notes_require_token_when_allow_lan() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = cfg_with(&dir, true, Some("secret".into()));
    cfg.notes_file = dir.path().join("notes.json");
    let a = app(cfg);

    let res = a
        .clone()
        .oneshot(
            Request::get("/api/notes?path=/repo/main")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "GET 无 token 401");

    let res = a
        .oneshot(
            Request::put("/api/notes")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"path":"/repo/main","content":"x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "PUT 无 token 401");
}

/// 落盘权限 0600（经 API 全链路验证，验收红线 3）
#[cfg(unix)]
#[tokio::test]
async fn api_persists_file_with_0600() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(cfg_with(&dir, false, None));
    let (status, _) = req(
        a,
        "PUT",
        "/api/notes",
        Some(json!({ "path": "/repo/main", "content": "secret" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    use std::os::unix::fs::PermissionsExt;
    let file = dir.path().join("notes.json");
    let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}
