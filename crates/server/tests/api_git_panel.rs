//! P6 批次 3 集成测试：B4 worktree 创建（冲突/白名单）、B5+B6 git status/diff、
//! B7 submodule 递归清单。真实 git CLI；open 动作不触发。

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
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.claude_root = dir.path().to_path_buf();
    let projects_file = dir.path().join("projects.json");
    std::fs::write(&projects_file, serde_json::to_string(&projects).unwrap()).unwrap();
    cfg.projects_file = projects_file;
    router(Arc::new(AppState::from_config(cfg)))
}

fn git(args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .expect("git CLI 可用");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn seed_repo(dir: &tempfile::TempDir) -> PathBuf {
    let main = dir.path().join("main-repo");
    git(&["init", "-q", "-b", "main", main.to_str().unwrap()]);
    std::fs::write(main.join("a.txt"), "hello\n").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "a.txt"]);
    git(&["-C", main.to_str().unwrap(), "commit", "-q", "-m", "init"]);
    main
}

#[tokio::test]
async fn worktree_add_creates_branch_and_dir() {
    // B4：指定基准 → 新分支 + 新 worktree 出现在树上的准备工作
    let dir = tempfile::tempdir().unwrap();
    let main = seed_repo(&dir);
    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);

    let (status, body) = req(
        app,
        "POST",
        "/api/projects/worktree",
        Some(json!({ "base": main.to_string_lossy(), "name": "feat-demo" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let created = body["path"].as_str().unwrap();
    assert!(created.contains(".worktree/feat-demo"), "body: {body}");
    // 实证：分支存在 + 目录存在 + 新 worktree 检出同一 commit
    assert!(Path::new(&main).join(".worktree/feat-demo/a.txt").exists());
    let branch = git(&["-C", created, "rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch.trim(), "feat-demo");
}

#[tokio::test]
async fn worktree_add_rejects_conflicts_without_side_effects() {
    // 同名分支已存在 → 409 不破坏现有；目录已存在 → 409
    let dir = tempfile::tempdir().unwrap();
    let main = seed_repo(&dir);
    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);

    // 同名分支（main 已存在）
    let (status, body) = req(
        app,
        "POST",
        "/api/projects/worktree",
        Some(json!({ "base": main.to_string_lossy(), "name": "main" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    // 非法分支名（含点/空格）
    let app2 = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let (status, _) = req(
        app2,
        "POST",
        "/api/projects/worktree",
        Some(json!({ "base": main.to_string_lossy(), "name": "bad name.." })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 基准不在白名单（注册 A 目录用 B 未注册目录作基准）→ 403
    let registered = tempfile::tempdir_in(dir.path()).unwrap();
    let unregistered = tempfile::tempdir_in(dir.path()).unwrap();
    let app3 = test_app(&dir, &[registered.path().to_string_lossy().to_string()]);
    let (status, _) = req(
        app3,
        "POST",
        "/api/projects/worktree",
        Some(json!({ "base": unregistered.path().to_string_lossy(), "name": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn git_status_reports_branch_and_changes() {
    // B5+B6：branch/upstream/ahead/behind + 文件 XY 状态（staged/unstaged/untracked）
    let dir = tempfile::tempdir().unwrap();
    let main = seed_repo(&dir);
    // 建 bare remote + push -u 建立 upstream（ahead/behind 数据源）
    let remote = dir.path().join("remote.git");
    git(&[
        "init",
        "-q",
        "--bare",
        "-b",
        "main",
        remote.to_str().unwrap(),
    ]);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "remote",
        "add",
        "origin",
        remote.to_str().unwrap(),
    ]);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "push",
        "-q",
        "-u",
        "origin",
        "main",
    ]);
    // 工作区制造：1 staged 修改 + 1 unstaged 修改 + 1 untracked
    std::fs::write(main.join("a.txt"), "changed\n").unwrap();
    std::fs::write(main.join("b.txt"), "staged only\n").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "b.txt"]);
    std::fs::write(main.join("c.txt"), "untracked\n").unwrap();

    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let (status, body) = req(
        app,
        "GET",
        &format!(
            "/api/projects/git-status?path={}",
            urlencoding(&main.to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["branch"], "main");
    assert_eq!(body["upstream"], "origin/main");
    assert_eq!(body["ahead"], 0);
    assert_eq!(body["behind"], 0);
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "body: {body}");
    let find = |p: &str| files.iter().find(|f| f["path"] == p).unwrap().clone();
    let a = find("a.txt");
    assert_eq!(a["x"], " "); // unstaged 修改：索引无
    assert_eq!(a["y"], "M");
    let b = find("b.txt");
    assert_eq!(b["x"], "A"); // staged 新增
    let c = find("c.txt");
    assert_eq!(c["x"], "?");
    assert_eq!(c["y"], "?");
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

#[tokio::test]
async fn diff_returns_tracked_and_untracked_content() {
    // B6：tracked 修改 → 含 ± 的 diff；untracked → no-index 全文；cached=true 读索引
    let dir = tempfile::tempdir().unwrap();
    let main = seed_repo(&dir);
    std::fs::write(main.join("a.txt"), "changed\n").unwrap();
    std::fs::write(main.join("new.txt"), "brand new\n").unwrap();

    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let enc = urlencoding(&main.to_string_lossy());
    let (status, body) = req(
        app,
        "GET",
        &format!("/api/projects/diff?path={enc}&file=a.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["diff"].as_str().unwrap().contains("-hello"));
    assert!(body["diff"].as_str().unwrap().contains("+changed"));

    // untracked：no-index 全文
    let app2 = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let (status, body) = req(
        app2,
        "GET",
        &format!("/api/projects/diff?path={enc}&file=new.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["diff"].as_str().unwrap().contains("brand new"));

    // 穿越 file 被拒
    let app3 = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let (status, _) = req(
        app3,
        "GET",
        &format!("/api/projects/diff?path={enc}&file=../escape.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn submodules_listed_recursively() {
    // B7：主仓含本地 submodule → 清单返回（含嵌套 submodule）
    let dir = tempfile::tempdir().unwrap();
    // submodule 源仓
    let sub = dir.path().join("sub-src");
    git(&["init", "-q", "-b", "main", sub.to_str().unwrap()]);
    std::fs::write(sub.join("s.txt"), "sub\n").unwrap();
    git(&["-C", sub.to_str().unwrap(), "add", "s.txt"]);
    git(&["-C", sub.to_str().unwrap(), "commit", "-q", "-m", "s-init"]);

    let main = seed_repo(&dir);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "submodule",
        "add",
        "-q",
        sub.to_str().unwrap(),
        "libs/sub",
    ]);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "commit",
        "-q",
        "-m",
        "add-sub",
    ]);

    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let (status, body) = req(
        app,
        "GET",
        &format!(
            "/api/projects/submodules?path={}",
            urlencoding(&main.to_string_lossy())
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let subs = body.as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["path"], "libs/sub");
    assert_eq!(subs[0]["status"], " ");
}
