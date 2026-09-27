//! P6 B1-B3 集成测试：git 工程树探测归组 + open-dir 路径白名单。
//! 真实 git CLI（外部依赖：本机 git）；open 动作用 dir_opener=true 短路，不真开 Finder。

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

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

fn test_app(dir: &tempfile::TempDir, projects: &[String]) -> axum::Router {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin(); // mock agent 清单：真实 cwd 不混入探测
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.claude_root = dir.path().to_path_buf();
    cfg.dir_opener = "true".into(); // /usr/bin/true：吃任意参数返回 0，不真开 Finder
    let projects_file = dir.path().join("projects.json");
    std::fs::write(&projects_file, serde_json::to_string(&projects).unwrap()).unwrap();
    cfg.projects_file = projects_file;
    router(Arc::new(AppState::from_config(cfg)))
}

fn git(args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git CLI 可用（外部依赖）");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 主仓 + linked worktree fixture；返回 (main, wt1) 路径。
fn seed_repo(dir: &tempfile::TempDir) -> (PathBuf, PathBuf) {
    let main = dir.path().join("main-repo");
    let wt1 = dir.path().join("wt-one");
    git(&["init", "-q", "-b", "main", main.to_str().unwrap()]);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "commit",
        "--allow-empty",
        "-q",
        "-m",
        "init",
    ]);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "worktree",
        "add",
        "-q",
        "-b",
        "feat-x",
        wt1.to_str().unwrap(),
    ]);
    (main, wt1)
}

#[tokio::test]
async fn tree_groups_main_with_linked_worktrees() {
    // B1+B2：注册 worktree 路径即可探测出主仓并归组；branch/head 数据就绪（B5 准备）
    let dir = tempfile::tempdir().unwrap();
    let (main, wt1) = seed_repo(&dir);
    let app = test_app(&dir, &[wt1.to_string_lossy().to_string()]);

    let (status, body) = req(app, "GET", "/api/projects/tree", None).await;
    assert_eq!(status, StatusCode::OK);
    let groups = body.as_array().expect("应返回树组数组");
    assert_eq!(groups.len(), 1, "同主仓只出一棵树: {body}");
    // macOS /tmp 是 symlink：git 输出 canonical 路径，期望值同样 canonicalize 后比较
    let canon = |p: &Path| {
        std::fs::canonicalize(p)
            .unwrap()
            .to_string_lossy()
            .to_string()
    };
    assert_eq!(groups[0]["main"]["path"], canon(&main));
    assert_eq!(groups[0]["main"]["isMain"], true);
    assert_eq!(groups[0]["main"]["branch"], "main");
    let wts = groups[0]["worktrees"].as_array().unwrap();
    assert_eq!(wts.len(), 1);
    assert_eq!(wts[0]["path"], canon(&wt1));
    assert_eq!(wts[0]["branch"], "feat-x");
    assert_eq!(wts[0]["isMain"], false);
}

#[tokio::test]
async fn tree_flattens_non_git_directory() {
    // 非 git 目录：平铺单节点组，isGit=false
    let dir = tempfile::tempdir().unwrap();
    let plain = tempfile::tempdir_in(dir.path()).unwrap();
    let app = test_app(&dir, &[plain.path().to_string_lossy().to_string()]);
    let (status, body) = req(app, "GET", "/api/projects/tree", None).await;
    assert_eq!(status, StatusCode::OK);
    let groups = body.as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["main"]["isGit"], false);
    assert_eq!(
        groups[0]["main"]["path"],
        plain.path().to_string_lossy().to_string()
    );
}

#[tokio::test]
async fn open_dir_enforces_whitelist() {
    // B3 安全：白名单内 204；任何拒绝（白名单外/穿越/不存在）统一 403 同文案，
    // 不泄露路径存在性（r40 tester 观察）；canonicalize 后前缀内放行
    let dir = tempfile::tempdir().unwrap();
    let (_main, wt1) = seed_repo(&dir);
    let app = test_app(&dir, &[wt1.to_string_lossy().to_string()]);

    // 白名单内（注册的 worktree 根）
    let (status, _) = req(
        app,
        "POST",
        "/api/projects/open-dir",
        Some(json!({ "path": wt1.to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // 白名单外（系统临时目录其他位置）
    let outside = tempfile::tempdir().unwrap();
    let app2 = test_app(&dir, &[wt1.to_string_lossy().to_string()]);
    let (status, _) = req(
        app2,
        "POST",
        "/api/projects/open-dir",
        Some(json!({ "path": outside.path().to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // `..` 穿越（入口即拒，统一 403）
    let app3 = test_app(&dir, &[wt1.to_string_lossy().to_string()]);
    let (status, _) = req(
        app3,
        "POST",
        "/api/projects/open-dir",
        Some(json!({ "path": format!("{}/../main-repo", wt1.to_string_lossy()) })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 不存在的目录同样 403（与白名单外不可区分）
    let app4 = test_app(&dir, &[wt1.to_string_lossy().to_string()]);
    let (status, _) = req(
        app4,
        "POST",
        "/api/projects/open-dir",
        Some(json!({ "path": "/definitely/not/exist" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn open_dir_allows_registered_root_prefix() {
    // 白名单前缀语义：注册目录内部的子目录亦放行
    let dir = tempfile::tempdir().unwrap();
    let (_main, wt1) = seed_repo(&dir);
    let sub = Path::new(&wt1).join("subdir");
    std::fs::create_dir(&sub).unwrap();
    let app = test_app(&dir, &[wt1.to_string_lossy().to_string()]);
    let (status, _) = req(
        app,
        "POST",
        "/api/projects/open-dir",
        Some(json!({ "path": sub.to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
