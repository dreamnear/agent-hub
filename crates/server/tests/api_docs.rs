//! P6 B8-B11 集成测试：文档目录浏览白名单、多格式预览分发、穿越拒绝。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::Value;
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

async fn req(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
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

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

fn test_app(dir: &tempfile::TempDir, projects: &[String]) -> axum::Router {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.claude_root = dir.path().to_path_buf();
    let projects_file = dir.path().join("projects.json");
    std::fs::write(&projects_file, serde_json::to_string(&projects).unwrap()).unwrap();
    cfg.projects_file = projects_file;
    router(Arc::new(AppState::from_config(cfg)))
}

/// 文档 fixture：README.md / page.html / app.log / app.bin / .env / docs/nested/deep.txt
fn seed_docs(root: &Path) {
    std::fs::create_dir_all(root.join("docs/nested")).unwrap();
    std::fs::write(root.join("README.md"), "# 标题\n\n- 列表\n").unwrap();
    std::fs::write(
        root.join("page.html"),
        "<html><body><script>alert(1)</script><p>hi</p></body></html>",
    )
    .unwrap();
    std::fs::write(root.join("app.log"), "2026-09-19 line1\n").unwrap();
    std::fs::write(root.join("app.bin"), [0u8, 159, 146, 150]).unwrap();
    std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(root.join("docs/nested/deep.txt"), "deep text\n").unwrap();
}

#[tokio::test]
async fn docs_list_walks_directories_inside_whitelist() {
    // B8：白名单内目录浏览（目录在前）；子目录可递归进入
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    seed_docs(&root);
    let enc = |s: &Path| urlenc(&s.to_string_lossy());
    let app = test_app(&dir, &[root.to_string_lossy().to_string()]);

    let (status, body) = req(app, &format!("/api/docs/list?path={}", enc(&root))).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let entries = body.as_array().unwrap();
    // 目录在前：docs 是首条
    assert_eq!(entries[0]["name"], "docs");
    assert_eq!(entries[0]["isDir"], true);
    // kind 预判
    let kind_of = |n: &str| {
        entries
            .iter()
            .find(|e| e["name"] == n)
            .map(|e| e["kind"].clone())
            .unwrap()
    };
    assert_eq!(kind_of("README.md"), "markdown");
    assert_eq!(kind_of("page.html"), "html");
    assert_eq!(kind_of("app.log"), "text");
    assert_eq!(kind_of("app.bin"), "binary");

    // 子目录可达（B8 任意层级）
    let nested = root.join("docs/nested");
    let app2 = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, body) = req(app2, &format!("/api/docs/list?path={}", enc(&nested))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["name"], "deep.txt");
}

fn urlenc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

#[tokio::test]
async fn docs_list_rejects_outside_and_traversal() {
    // 安全：白名单外 403；`..` 穿越 403；统一文案不泄露存在性
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    let outside = tempfile::tempdir_in(dir.path()).unwrap();
    let app = test_app(&dir, &[root.to_string_lossy().to_string()]);

    // 白名单外
    let (status, _) = req(
        app,
        &format!(
            "/api/docs/list?path={}",
            urlenc(&outside.path().to_string_lossy())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 穿越（含 .. 字面量）
    let app2 = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, _) = req(
        app2,
        &format!(
            "/api/docs/list?path={}",
            urlenc(&format!("{}/../outside", root.to_string_lossy()))
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn docs_file_dispatches_by_format() {
    // B9：markdown 原文 / html 原样（前端 sandbox 渲染）/ text / binary 仅列出 / .env 仅列出
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    seed_docs(&root);
    let enc = |s: &str| urlenc(s);
    let app = test_app(&dir, &[root.to_string_lossy().to_string()]);

    // markdown
    let (status, body) = req(
        app,
        &format!(
            "/api/docs/file?path={}",
            enc(&root.join("README.md").to_string_lossy())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], "markdown");
    assert!(body["content"].as_str().unwrap().contains("# 标题"));

    // html（原样返回，脚本禁用在前端 sandbox）
    let app2 = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, body) = req(
        app2,
        &format!(
            "/api/docs/file?path={}",
            enc(&root.join("page.html").to_string_lossy())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], "html");
    assert!(body["content"].as_str().unwrap().contains("<script>"));

    // binary 仅列出（content null）
    let app3 = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, body) = req(
        app3,
        &format!(
            "/api/docs/file?path={}",
            enc(&root.join("app.bin").to_string_lossy())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], "binary");
    assert!(body["content"].is_null());

    // .env 敏感文件：kind=binary 仅列出，内容不回传
    let app4 = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, body) = req(
        app4,
        &format!(
            "/api/docs/file?path={}",
            enc(&root.join(".env").to_string_lossy())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["kind"], "binary");
    assert!(body["content"].is_null());
}

#[tokio::test]
async fn docs_file_rejects_outside_traversal_and_oversize() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    seed_docs(&root);
    // 白名单外文件
    let outside = tempfile::tempdir_in(dir.path()).unwrap();
    std::fs::write(outside.path().join("secret.txt"), "outside\n").unwrap();
    let app = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, _) = req(
        app,
        &format!(
            "/api/docs/file?path={}",
            urlenc(&outside.path().join("secret.txt").to_string_lossy())
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 穿越（与 open-dir 同语义统一 403）
    let app2 = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, _) = req(
        app2,
        &format!(
            "/api/docs/file?path={}",
            urlenc(&format!("{}/../secret.txt", root.to_string_lossy()))
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 超大文本（>512KB）→ 413
    let big = root.join("big.log");
    std::fs::write(&big, "x".repeat(600 * 1024)).unwrap();
    let app3 = test_app(&dir, &[root.to_string_lossy().to_string()]);
    let (status, _) = req(
        app3,
        &format!("/api/docs/file?path={}", urlenc(&big.to_string_lossy())),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}
