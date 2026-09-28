use std::fs;
use std::path::Path;

use server::drivers::claude::session::{parse_session_file, session_dir};
use server::models::{map_kind_label, ChatMessageKind};

fn fixture_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(
        "tests/fixtures/sessions/-Users-demo-proj-alpha/11111111-2222-3333-4444-555555555555.jsonl",
    )
}

#[test]
fn session_dir_maps_path_to_slug() {
    // 实测规则：'/' → '-'、'.' → '-'、其余保留（-Users-alice--claude-jobs-* 佐证 .claude→-claude）
    // tasks.md：projects_dir 是 .claude 根，返回 <root>/projects/<slug>
    let root = Path::new("/home/u/.claude");
    let dir = session_dir(root, Path::new("/Users/demo/proj_alpha")).unwrap();
    assert_eq!(dir, root.join("projects/-Users-demo-proj_alpha"));
    let dir2 = session_dir(root, Path::new("/Users/alice/.claude/jobs/aa14d3e3-tmp")).unwrap();
    assert_eq!(
        dir2,
        root.join("projects/-Users-alice--claude-jobs-aa14d3e3-tmp")
    );
}

#[tokio::test]
async fn read_session_parses_fixture_sequence() {
    let slug_dir = fixture_path().parent().unwrap().to_path_buf();
    let msgs = read_session_raw(&slug_dir, "11111111-2222-3333-4444-555555555555")
        .await
        .expect("fixture 应解析出消息");

    // 5 条好消息（坏行 + file-history-snapshot 跳过）
    assert_eq!(msgs.len(), 5, "got: {msgs:?}");

    assert_eq!(msgs[0].kind, ChatMessageKind::User);
    assert_eq!(msgs[0].text.as_deref(), Some("帮我看看构建错误"));
    assert_eq!(msgs[0].ts.as_deref(), Some("2026-09-16T08:00:00.000Z"));

    assert_eq!(msgs[1].kind, ChatMessageKind::Thinking);
    assert_eq!(msgs[1].text.as_deref(), Some("需要先看报错输出"));

    assert_eq!(msgs[2].kind, ChatMessageKind::ToolUse);
    assert_eq!(msgs[2].tool_name.as_deref(), Some("Bash"));
    assert_eq!(msgs[2].tool_use_id.as_deref(), Some("toolu_01"));
    assert!(msgs[2].input.is_some());

    assert_eq!(msgs[3].kind, ChatMessageKind::ToolResult);
    assert_eq!(msgs[3].tool_use_id.as_deref(), Some("toolu_01"));
    assert_eq!(msgs[3].text.as_deref(), Some("Compiling server v0.1.0"));

    assert_eq!(msgs[4].kind, ChatMessageKind::Assistant);
    assert_eq!(msgs[4].text.as_deref(), Some("构建通过了"));

    // serde 标签烟测
    assert_eq!(map_kind_label(ChatMessageKind::ToolUse), "tool_use");
}

/// 直接按 slug 目录读（绕过 cwd 映射，测试 fixture 定位用）。
async fn read_session_raw(
    slug_dir: &std::path::Path,
    session_id: &str,
) -> Option<Vec<server::models::ChatMessage>> {
    let path = slug_dir.join(format!("{session_id}.jsonl"));
    parse_session_file(&path).await
}

#[tokio::test]
async fn session_is_active_reflects_fresh_mtime_and_rejects_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    fs::create_dir_all(&proj).unwrap();
    let sid = "fresh-1111";
    fs::write(proj.join(format!("{sid}.jsonl")), "{\"type\":\"user\"}\n").unwrap();
    // 刚写入 → 在 30s 静默阈值内 = 活跃
    assert!(server::drivers::claude::session::session_is_active(tmp.path(), sid, 30).await);
    // 不存在的会话 → false；路径逃逸 → false
    assert!(!server::drivers::claude::session::session_is_active(tmp.path(), "missing", 30).await);
    assert!(!server::drivers::claude::session::session_is_active(tmp.path(), "../x", 30).await);
}
