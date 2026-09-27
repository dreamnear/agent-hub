//! r71 图片上传集成测试：类型白名单、base64 解码、空体拒绝、UUID 命名落盘。

use std::{path::Path, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::Engine;
use serde_json::{json, Value};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

fn test_app(dir: &tempfile::TempDir) -> axum::Router {
    let mut cfg = Config::load();
    cfg.claude_bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh");
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.claude_root = dir.path().to_path_buf();
    let projects_file = dir.path().join("projects.json");
    std::fs::write(&projects_file, "[]").unwrap();
    cfg.projects_file = projects_file;
    router(Arc::new(AppState::from_config(cfg)))
}

async fn post_upload(app: axum::Router, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/api/upload")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
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

const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];

#[tokio::test]
async fn upload_png_saves_file_and_returns_path() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, data) = post_upload(
        app,
        json!({
            "filename": "截图 2026-09-23.png",
            "data_base64": base64::engine::general_purpose::STANDARD.encode(PNG_MAGIC),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let path = data["path"].as_str().expect("path 返回").to_string();
    let saved = std::fs::read(&path).unwrap();
    assert_eq!(saved, PNG_MAGIC);
    assert!(path.ends_with(".png"));
    // UUID 命名：不透传用户文件名（防路径注入/特殊字符）
    assert!(!path.contains("截图"));
}

#[tokio::test]
async fn upload_rejects_non_image_extension() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    // AppError 响应体是纯文本消息（error.rs into_response），非 JSON 信封
    let request = Request::builder()
        .method("POST")
        .uri("/api/upload")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "filename": "payload.txt",
                "data_base64": base64::engine::general_purpose::STANDARD.encode(b"hello"),
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("不支持的图片类型"), "body: {text}");
}

#[tokio::test]
async fn upload_rejects_invalid_base64() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, _) = post_upload(
        app,
        json!({ "filename": "a.png", "data_base64": "!!!not-base64!!!" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn upload_rejects_empty_body() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, _) = post_upload(app, json!({ "filename": "a.png", "data_base64": "" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ===== r72 图片受限读取：GET /api/images/{filename} =====

/// 上传一张图并返回（tempdir 守生命周期, 文件名, 带 token 的 app 重建器）
async fn upload_one_png() -> (tempfile::TempDir, String, Arc<axum::Router>) {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, data) = post_upload(
        app,
        json!({
            "filename": "shot.png",
            "data_base64": base64::engine::general_purpose::STANDARD.encode(PNG_MAGIC),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let path = data["path"].as_str().expect("path 返回").to_string();
    let name = path.rsplit('/').next().unwrap().to_string();
    // 后面还要打请求：包 Arc 绕 oneshot 消耗所有权（tempdir 必须活得比 app 久）
    let app = Arc::new(test_app(&dir));
    (dir, name, app)
}

#[tokio::test]
async fn get_uploaded_image_serves_bytes_with_content_type() {
    let (_dir, name, app) = upload_one_png().await;
    let request = Request::builder()
        .uri(format!("/api/images/{name}"))
        .body(Body::empty())
        .unwrap();
    let res = app.as_ref().clone().oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "image/png");
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), PNG_MAGIC);
}

#[tokio::test]
async fn get_image_rejects_non_image_extension() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let request = Request::builder()
        .uri("/api/images/secret.txt")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn get_image_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    for hostile in ["..%2F..%2Fetc%2Fpasswd", "a..b.png", "..", "sub%2Fdir.png"] {
        let request = Request::builder()
            .uri(format!("/api/images/{hostile}"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(request).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "hostile: {hostile}");
    }
}

#[tokio::test]
async fn get_image_missing_file_is_404_without_path_leak() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let request = Request::builder()
        .uri("/api/images/00000000-0000-0000-0000-000000000000.png")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    // 5xx/4xx 均不透出服务端绝对路径
    assert!(!text.contains("claude-view-uploads"), "body: {text}");
    assert!(!text.contains("tmp"), "body: {text}");
}

#[tokio::test]
async fn get_image_requires_token_when_allow_lan() {
    let (dir, name, _app) = upload_one_png().await;
    let mut cfg = Config::load();
    cfg.claude_bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh");
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.claude_root = dir.path().to_path_buf();
    let projects_file = dir.path().join("projects.json");
    std::fs::write(&projects_file, "[]").unwrap();
    cfg.projects_file = projects_file;
    cfg.allow_lan = true;
    cfg.token = Some("lan-token".into());
    let app = Arc::new(router(Arc::new(AppState::from_config(cfg))));

    // 无 token → 401
    let request = Request::builder()
        .uri(format!("/api/images/{name}"))
        .body(Body::empty())
        .unwrap();
    let res = app.as_ref().clone().oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // ?token= → 200（img 标签无法带 header，auth 中间件 query 校验）
    let request = Request::builder()
        .uri(format!("/api/images/{name}?token=lan-token"))
        .body(Body::empty())
        .unwrap();
    let res = app.as_ref().clone().oneshot(request).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

// ===== r72b TUI 粘贴图：GET /api/images/by-path?path=（claude-tmp 白名单） =====

/// 在真实 /tmp/claude-tmp 下造测试图（自建随机子目录，TempDir drop 时自清）。
/// 白名单前缀是字面 /tmp|/private/tmp/claude-tmp，不能用 std::env::temp_dir()
/// （macOS 返回 /var/folders/...，不在白名单内）。
fn claude_tmp_image(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = std::path::Path::new("/tmp/claude-tmp");
    std::fs::create_dir_all(root).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("hub-r72b-")
        .tempdir_in(root)
        .unwrap();
    let images = dir.path().join("images");
    std::fs::create_dir_all(&images).unwrap();
    let file = images.join(name);
    std::fs::write(&file, bytes).unwrap();
    (dir, file)
}

async fn get_by_path(
    app: &Arc<axum::Router>,
    path: &str,
) -> (StatusCode, Vec<u8>, Option<axum::http::header::HeaderValue>) {
    let request = Request::builder()
        .uri(format!("/api/images/by-path?path={path}"))
        .body(Body::empty())
        .unwrap();
    let res = app.as_ref().clone().oneshot(request).await.unwrap();
    let status = res.status();
    let ct = res.headers().get("content-type").cloned();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes, ct)
}

#[tokio::test]
async fn get_by_path_serves_claude_tmp_image() {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(test_app(&dir));
    let (_guard, file) = claude_tmp_image("22.png", PNG_MAGIC);
    let (status, bytes, ct) = get_by_path(&app, &file.display().to_string()).await;
    assert_eq!(status, StatusCode::OK, "path: {}", file.display());
    assert_eq!(ct.as_ref().map(|v| v.as_bytes()), Some(&b"image/png"[..]));
    assert_eq!(bytes, PNG_MAGIC);
}

#[tokio::test]
async fn get_by_path_serves_uploads_image_too() {
    // uploads 根同在 by-path 白名单：两条路由殊途同归（前端双形态渲染走同一端点族）
    let (_dir, name, app) = upload_one_png().await;
    let uploads = std::env::temp_dir().join("claude-view-uploads").join(&name);
    let (status, bytes, _) = get_by_path(&app, &uploads.display().to_string()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, PNG_MAGIC);
}

#[tokio::test]
async fn get_by_path_rejects_traversal_and_prefix_confusion() {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(test_app(&dir));

    // 穿越目标真实存在（防「不存在也 404」假阳性）
    let secret = std::env::temp_dir().join(format!("hub-r72b-secret-{}.png", std::process::id()));
    std::fs::write(&secret, PNG_MAGIC).unwrap();
    let uploads_root = std::env::temp_dir().join("claude-view-uploads");
    let escape = uploads_root.join("..").join(secret.file_name().unwrap());
    let evil_root = std::path::Path::new("/tmp/claude-tmp-evil");
    std::fs::create_dir_all(evil_root).unwrap();
    let evil = evil_root.join("hub-r72b-evil.png");
    std::fs::write(&evil, PNG_MAGIC).unwrap();

    // /etc/passwd：扩展名白名单先拒 400（无扩展名时消息回显请求方自带输入，
    // 不含服务端路径——不泄露）
    let (status, body, _) = get_by_path(&app, "/etc/passwd").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        !String::from_utf8_lossy(&body).contains("claude-view-uploads")
            && !String::from_utf8_lossy(&body).contains("var/folders"),
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    // png 后缀、文件真实存在但白名单外 → 404（与「不存在」同响应，无存在性 oracle）
    for hostile in [&escape.display().to_string(), &evil.display().to_string()] {
        let (status, _, _) = get_by_path(&app, hostile).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "hostile: {hostile}");
    }
    let _ = std::fs::remove_file(&secret);
    let _ = std::fs::remove_file(&evil);
}

#[tokio::test]
async fn get_by_path_rejects_non_image_extension() {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(test_app(&dir));
    let (_guard, file) = claude_tmp_image("secret.txt", b"hello");
    let (status, _, _) = get_by_path(&app, &file.display().to_string()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
