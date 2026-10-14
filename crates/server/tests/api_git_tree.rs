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
        // git≥2.38.1 默认禁 file 协议（CVE-2022-39253），fixture 子模块须 file 克隆
        .args(["-c", "protocol.file.allow=always"])
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

// —— submodule 行内改动徽标（agent-hub 工单）：submodules 端点并出 changes/untracked ——

/// temp 父仓 + 子模块：subrepo 提交一文件后挂为 libs/sub；返回 (tempdir, 父仓路径)。
fn seed_parent_with_submodule() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub-repo");
    let parent = dir.path().join("parent-repo");
    git(&["init", "-q", "-b", "main", sub.to_str().unwrap()]);
    std::fs::write(sub.join("s.txt"), "sub\n").unwrap();
    git(&["-C", sub.to_str().unwrap(), "add", "s.txt"]);
    git(&[
        "-C",
        sub.to_str().unwrap(),
        "commit",
        "-q",
        "-m",
        "sub init",
    ]);
    git(&["init", "-q", "-b", "main", parent.to_str().unwrap()]);
    std::fs::write(parent.join("p.txt"), "parent\n").unwrap();
    git(&["-C", parent.to_str().unwrap(), "add", "p.txt"]);
    git(&[
        "-C",
        parent.to_str().unwrap(),
        "commit",
        "-q",
        "-m",
        "parent init",
    ]);
    git(&[
        "-C",
        parent.to_str().unwrap(),
        "submodule",
        "add",
        sub.to_str().unwrap(),
        "libs/sub",
    ]);
    git(&[
        "-C",
        parent.to_str().unwrap(),
        "commit",
        "-q",
        "-m",
        "add submodule",
    ]);
    (dir, parent)
}

#[tokio::test]
async fn submodules_list_reports_changes_and_untracked_counts() {
    let (dir, parent) = seed_parent_with_submodule();
    let sub_worktree = parent.join("libs/sub");
    // ① 已跟踪文件改动 1 处 + 未跟踪 1 个
    std::fs::write(sub_worktree.join("s.txt"), "modified\n").unwrap();
    std::fs::write(sub_worktree.join("extra.txt"), "untracked\n").unwrap();
    let app = test_app(&dir, &[parent.to_string_lossy().to_string()]);
    let (status, body) = req(
        app,
        "GET",
        &format!("/api/projects/submodules?path={}", parent.to_string_lossy()),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let subs = body.as_array().expect("array");
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0]["path"], "libs/sub");
    assert_eq!(subs[0]["changes"], 1, "已跟踪改动计数: {body}");
    assert_eq!(subs[0]["untracked"], 1, "未跟踪计数: {body}");

    // ② 干净子模块：双 0（改动恢复后）
    std::fs::write(sub_worktree.join("s.txt"), "sub\n").unwrap();
    std::fs::remove_file(sub_worktree.join("extra.txt")).unwrap();
    let app2 = test_app(&dir, &[parent.to_string_lossy().to_string()]);
    let (status2, body2) = req(
        app2,
        "GET",
        &format!("/api/projects/submodules?path={}", parent.to_string_lossy()),
        None,
    )
    .await;
    assert_eq!(status2, StatusCode::OK);
    assert_eq!(body2[0]["changes"], 0, "干净应为 0: {body2}");
    assert_eq!(body2[0]["untracked"], 0, "干净应为 0: {body2}");
}

// —— diff 端点热修（untracked/binary/不存在）：加载卡死根因的防再犯锁定 ——

/// 单仓 fixture（不依赖 api_git_tree 既有 seed_repo 的双返回形状）
fn seed_simple_repo(dir: &tempfile::TempDir) -> PathBuf {
    let main = dir.path().join("simple-repo");
    git(&["init", "-q", "-b", "main", main.to_str().unwrap()]);
    std::fs::write(main.join("a.txt"), "hello\n").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "a.txt"]);
    git(&["-C", main.to_str().unwrap(), "commit", "-q", "-m", "init"]);
    main
}

#[tokio::test]
async fn diff_untracked_md_returns_full_add() {
    let dir = tempfile::tempdir().unwrap();
    let main = seed_simple_repo(&dir);
    std::fs::write(main.join("fresh.md"), "# new content\n").unwrap();
    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let (status, body) = req(
        app,
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=fresh.md",
            main.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["untracked"], true);
    assert_eq!(body["binary"], false);
    assert!(body["diff"].as_str().unwrap().contains("# new content"));
}

#[tokio::test]
async fn diff_tracked_binary_flags_binary() {
    let dir = tempfile::tempdir().unwrap();
    let main = seed_simple_repo(&dir);
    std::fs::write(main.join("pic.png"), b"\x89PNG\r\n\x1a\n\x00\x01").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "pic.png"]);
    git(&["-C", main.to_str().unwrap(), "commit", "-q", "-m", "png"]);
    // 改动二进制 → git diff 出「Binary files ... differ」占位文本
    std::fs::write(main.join("pic.png"), b"\x00\x02\x03changed").unwrap();
    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);
    let (status, body) = req(
        app,
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=pic.png",
            main.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["binary"], true);
    assert_eq!(body["diff"], "");
}

#[tokio::test]
async fn diff_tracked_modified_regression_and_missing_400() {
    let dir = tempfile::tempdir().unwrap();
    let main = seed_simple_repo(&dir);
    std::fs::write(main.join("a.txt"), "hello v2\n").unwrap();
    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);
    // 回归：tracked 修改照常
    let (status, body) = req(
        app.clone(),
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=a.txt",
            main.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["untracked"], false);
    assert!(body["diff"].as_str().unwrap().contains("hello v2"));
    // 不存在文件：400（响应体纯文本「git diff 失败」，前端错误态承接）
    let (status, _body) = req(
        app,
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=ghost.txt",
            main.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// —— 图片对照验收发现的路径语义回归锁定：git-status（porcelain v1）行路径恒为
// 仓库根（toplevel）相对且覆盖全仓；diff/file 的 file 参数与行同源，path=子目录时
// 曾被 join 成双重前缀 → cwd≠仓库根的 agent 面板全文件 400（图片双栏被上游挡死）。 ——

/// 二进制断言用：不做 JSON 解析，回原始字节（图片端点回 Content-Type + bytes）。
async fn req_raw(app: axum::Router, uri: &str) -> (StatusCode, axum::body::Bytes) {
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
    (status, bytes)
}

#[tokio::test]
async fn diff_and_file_resolve_repo_root_for_subdir_path() {
    // path=子目录 + file=仓库根相对三态：tracked 修改、节点外 untracked png
    // （binary 标记）、tracked png 的 head/work 字节——修复前除 head 分支外全 400。
    let dir = tempfile::tempdir().unwrap();
    let main = seed_simple_repo(&dir);
    std::fs::create_dir_all(main.join("sub/deep")).unwrap();
    std::fs::write(main.join("sub/deep/b.txt"), "old\n").unwrap();
    // 夹具必须含 NUL：git 二进制判定看首 8000 字节有无 NUL，纯 \x89 会被当文本
    std::fs::write(main.join("pic.png"), b"\x00PNG-v1").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "."]);
    git(&["-C", main.to_str().unwrap(), "commit", "-q", "-m", "more"]);
    std::fs::write(main.join("sub/deep/b.txt"), "new\n").unwrap();
    std::fs::write(main.join("pic.png"), b"\x00PNG-v2").unwrap();
    // 仓库根 untracked png（在子目录节点之外，git-status 仍会列出）
    std::fs::write(main.join("new.png"), b"\x00PNG-new").unwrap();

    let sub = main.join("sub");
    let app = test_app(&dir, &[main.to_string_lossy().to_string()]);

    // tracked 修改：± 文本 diff 照常
    let (status, body) = req(
        app.clone(),
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=sub/deep/b.txt",
            sub.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["untracked"], false);
    assert!(body["diff"].as_str().unwrap().contains("-old"));
    assert!(body["diff"].as_str().unwrap().contains("+new"));

    // 节点外 untracked png：binary 占位（修复前 no-index 找不到文件 → 400）
    let (status, body) = req(
        app.clone(),
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=new.png",
            sub.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["binary"], true);
    assert_eq!(body["untracked"], true);

    // tracked png 双栏数据源：work=工作区字节、head=提交版字节
    let (status, bytes) = req_raw(
        app.clone(),
        &format!(
            "/api/projects/file?path={}&file=pic.png&rev=work",
            sub.to_string_lossy()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes.as_ref(), b"\x00PNG-v2");
    let (status, bytes) = req_raw(
        app,
        &format!(
            "/api/projects/file?path={}&file=pic.png&rev=head",
            sub.to_string_lossy()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes.as_ref(), b"\x00PNG-v1");
}

#[tokio::test]
async fn diff_deleted_file_returns_full_delete() {
    // 已删除文件（tracked + 工作区移除）：git diff HEAD -- 出全删 diff——
    // 验收发现工单：面板 D 行点开曾 400「无法生成 diff」，现左全文（- 行）右空。
    // staged 的本地删除（cached=true）也从索引读出全删（与 unstaged 镜像）。
    let dir = tempfile::tempdir().unwrap();
    let main = seed_simple_repo(&dir);
    std::fs::write(main.join("gone.txt"), "line one\nline two\n").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "gone.txt"]);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "commit",
        "-q",
        "-m",
        "add-gone",
    ]);
    std::fs::remove_file(main.join("gone.txt")).unwrap();
    let enc = main.to_string_lossy().to_string();

    let app = test_app(&dir, std::slice::from_ref(&enc));
    let (status, body) = req(
        app.clone(),
        "GET",
        &format!("/api/projects/diff?path={enc}&file=gone.txt",),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["untracked"], false);
    let diff = body["diff"].as_str().unwrap();
    assert!(diff.contains("-line one"));
    assert!(diff.contains("-line two"));

    // staged 删除（git rm）：cached=true 读索引差集，同样全删内容
    git(&["-C", main.to_str().unwrap(), "rm", "-q", "gone.txt"]);
    let (status, body) = req(
        app,
        "GET",
        &format!("/api/projects/diff?path={enc}&file=gone.txt&cached=true",),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["diff"].as_str().unwrap().contains("-line one"));
}

#[tokio::test]
async fn diff_falls_back_to_node_dir_for_non_git_dir() {
    // 非 git 目录（rev-parse 失败）：回退节点目录语义——本地相对 file 走 no-index
    // 全文的行为保持不变（fail-soft）；不存在文件仍 400。
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("local.md"), "brand new\n").unwrap();
    let app = test_app(&dir, &[plain.to_string_lossy().to_string()]);

    let (status, body) = req(
        app.clone(),
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=local.md",
            plain.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["untracked"], true);
    assert!(body["diff"].as_str().unwrap().contains("brand new"));

    let (status, _body) = req(
        app,
        "GET",
        &format!(
            "/api/projects/diff?path={}&file=ghost.md",
            plain.to_string_lossy()
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---- git-timeline：提交历史端点（只读，T5 红线）----

#[tokio::test]
async fn git_log_returns_commits_with_pagination() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = seed_repo(&dir);
    // seed_repo 之上多造 2 个 commit（总 3），验证限量 + hasMore + skip
    std::fs::write(main.join("c2.txt"), "2").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "."]);
    git(&[
        "-C",
        main.to_str().unwrap(),
        "commit",
        "-m",
        "second commit",
    ]);
    std::fs::write(main.join("c3.txt"), "3").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "."]);
    git(&["-C", main.to_str().unwrap(), "commit", "-m", "third commit"]);

    let app = test_app(&dir, &[main.to_str().unwrap().into()]);
    // 全量（limit 大）：3 条、hasMore=false
    let (st, body) = req(
        app.clone(),
        "GET",
        &format!("/api/projects/git-log?path={}&limit=200", main.display()),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["commits"].as_array().unwrap().len(), 3);
    assert_eq!(body["hasMore"], json!(false));
    // 最新在前（subject 倒序）+ 字段齐全
    let first = &body["commits"][0];
    assert_eq!(first["subject"], "third commit");
    assert!(!first["hash"].as_str().unwrap().is_empty());
    assert!(!first["author"].as_str().unwrap().is_empty());
    assert!(!first["relTime"].as_str().unwrap().is_empty());
    // 限量：limit=2 → 2 条 + hasMore=true
    let (st, body) = req(
        app.clone(),
        "GET",
        &format!("/api/projects/git-log?path={}&limit=2", main.display()),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["commits"].as_array().unwrap().len(), 2);
    assert_eq!(body["hasMore"], json!(true));
    // skip=2 → 只剩最早 1 条
    let (st, body) = req(
        app.clone(),
        "GET",
        &format!(
            "/api/projects/git-log?path={}&limit=2&skip=2",
            main.display()
        ),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["commits"][0]["subject"], "init");
    assert_eq!(body["hasMore"], json!(false));
}

#[tokio::test]
async fn git_log_empty_repo_returns_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("empty-repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&["-C", repo.to_str().unwrap(), "init"]);
    let app = test_app(&dir, &[repo.to_str().unwrap().into()]);
    let (st, body) = req(
        app.clone(),
        "GET",
        &format!("/api/projects/git-log?path={}", repo.display()),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["commits"].as_array().unwrap().len(), 0);
    assert_eq!(body["hasMore"], json!(false));
}

// ---- git-timeline-files：提交文件维清单 + 单文件提交 diff（只读，F1-F5）----

/// 提交历史 fixture：init（空）→ second（+a.txt +b.txt）→ third（改 a.txt + mv b→c）。
/// 返回 (main, hash_init, hash_second, hash_third)。hash 用 git-log 端点取（%h 短 hash）。
async fn seed_commits(dir: &tempfile::TempDir) -> (PathBuf, String, String, String) {
    let (main, _) = seed_repo(dir);
    std::fs::write(main.join("a.txt"), "a").unwrap();
    std::fs::write(main.join("b.txt"), "b").unwrap();
    git(&["-C", main.to_str().unwrap(), "add", "."]);
    git(&["-C", main.to_str().unwrap(), "commit", "-m", "second"]);
    std::fs::write(main.join("a.txt"), "a2").unwrap();
    git(&["-C", main.to_str().unwrap(), "mv", "b.txt", "c.txt"]);
    // -a 把 a.txt 工作区改动一并入提交（mv 已在索引；漏 stage 则只剩 rename）
    git(&["-C", main.to_str().unwrap(), "commit", "-a", "-m", "third"]);

    let app = test_app(dir, &[main.to_str().unwrap().into()]);
    let (_, body) = req(
        app,
        "GET",
        &format!("/api/projects/git-log?path={}&limit=10", main.display()),
        None,
    )
    .await;
    let commits = body["commits"].as_array().unwrap();
    let hash = |i: usize| commits[i]["hash"].as_str().unwrap().to_string();
    (main, hash(2), hash(1), hash(0)) // log 新在前：init/second/third
}

#[tokio::test]
async fn git_commit_files_lists_status_paths_with_rename_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    let (main, h_init, h_second, h_third) = seed_commits(&dir).await;
    let app = test_app(&dir, &[main.to_str().unwrap().into()]);
    let url = |h: &str, extra: &str| {
        format!(
            "/api/projects/git-commit-files?path={}&hash={h}{extra}",
            main.display()
        )
    };
    // second：新增两文件（A）
    let (st, body) = req(app.clone(), "GET", &url(&h_second, ""), None).await;
    assert_eq!(st, StatusCode::OK);
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["status"], json!("A"));
    assert_eq!(files[0]["path"], json!("a.txt"));
    assert_eq!(files[0].get("origPath"), Some(&Value::Null));
    assert_eq!(files[1]["path"], json!("b.txt"));
    assert_eq!(body["hasMore"], json!(false));
    // third：改 a.txt（M）+ 改名 b→c（R 分数态，origPath 承载原路径——-M 探测；
    // 条目排序不依赖，按 path 查找）
    let (st, body) = req(app.clone(), "GET", &url(&h_third, ""), None).await;
    assert_eq!(st, StatusCode::OK);
    let files = body["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    let m = files.iter().find(|f| f["path"] == json!("a.txt")).unwrap();
    assert_eq!(m["status"], json!("M"));
    let r = files.iter().find(|f| f["path"] == json!("c.txt")).unwrap();
    assert!(r["status"].as_str().unwrap().starts_with('R'));
    assert_eq!(r["origPath"], json!("b.txt"));
    // 限量：limit=1 → 1 条 + hasMore（内存截断，F4 大 commit 不卡）
    let (st, body) = req(app.clone(), "GET", &url(&h_third, "&limit=1"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["files"].as_array().unwrap().len(), 1);
    assert_eq!(body["hasMore"], json!(true));
    // 根提交（--allow-empty init）：空清单非错误
    let (st, body) = req(app.clone(), "GET", &url(&h_init, ""), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["files"].as_array().unwrap().len(), 0);
    // 坏 hash 拒绝（含 `-` 开头 flag 注入面——hex 白名单拦截）
    for bad in ["zzzzzzz", "-upstream"] {
        let (st, _) = req(app.clone(), "GET", &url(bad, ""), None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "bad hash {bad}");
    }
}

#[tokio::test]
async fn git_commit_diff_returns_patch_and_rejects_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let (main, h_init, h_second, h_third) = seed_commits(&dir).await;
    let app = test_app(&dir, &[main.to_str().unwrap().into()]);
    let url = |h: &str, f: &str| {
        format!(
            "/api/projects/git-commit-diff?path={}&hash={h}&file={f}",
            main.display()
        )
    };
    // third 对 a.txt：改 a → a2，patch 含删/增行
    let (st, body) = req(app.clone(), "GET", &url(&h_third, "a.txt"), None).await;
    assert_eq!(st, StatusCode::OK);
    let diff = body["diff"].as_str().unwrap();
    assert!(diff.contains("-a") && diff.contains("+a2"), "diff: {diff}");
    assert_eq!(body["untracked"], json!(false));
    // second 对 a.txt：父提交为空树语义（a 为新增）→ +a
    let (st, body) = req(app.clone(), "GET", &url(&h_second, "a.txt"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(body["diff"].as_str().unwrap().contains("+a"));
    // init（空根提交）对任意文件：空 diff 非错误
    let (st, body) = req(app.clone(), "GET", &url(&h_init, "a.txt"), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["diff"], json!(""));
    // 越界 file 与坏 hash 拒绝
    let (st, _) = req(app.clone(), "GET", &url(&h_third, "..%2Fa.txt"), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = req(app, "GET", &url("ffffff", "a.txt"), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}
