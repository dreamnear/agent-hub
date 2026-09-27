//! subagent 发现与定位测试（P5+）：真实目录形态 fixture（调查报告 1.1/1.3）。
use std::fs;

use server::drivers::claude::session::find_subagent_path;
use server::drivers::claude::subagents::list_subagents;

const SID: &str = "aaaaaaaa-1111-2222-3333-444444444444";

fn fixture(dir: &std::path::Path) -> std::path::PathBuf {
    let proj = dir.join("projects").join("-Users-demo-proj");
    fs::create_dir_all(&proj).unwrap();
    fs::write(proj.join(format!("{SID}.jsonl")), "").unwrap();
    let sub = proj.join(SID).join("subagents");
    fs::create_dir_all(&sub).unwrap();
    sub
}

fn write_subagent(sub: &std::path::Path, agent_id: &str, meta: &str, jsonl_lines: &[&str]) {
    fs::write(sub.join(format!("agent-{agent_id}.meta.json")), meta).unwrap();
    fs::write(
        sub.join(format!("agent-{agent_id}.jsonl")),
        jsonl_lines.join("\n"),
    )
    .unwrap();
}

#[tokio::test]
async fn lists_subagents_from_directory_with_meta_and_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = fixture(tmp.path());
    write_subagent(
        &sub,
        "dr-planner-f9dcf57d886a89f9",
        r#"{"agentType":"dr-planner","description":"P1 计划转 tasks.md","name":"dr-planner","model":"opus"}"#,
        &[
            r#"{"type":"user","timestamp":"2026-09-16T07:12:40Z","message":{"content":"prompt"}}"#,
            r#"{"type":"assistant","timestamp":"2026-09-16T11:21:46Z","message":{"content":[{"type":"text","text":"done"}]}}"#,
        ],
    );
    // 匿名 subagent：无 meta（降级：name 取 id 前 8 位、type 取 "subagent"）
    write_subagent(
        &sub,
        "ae857e95bdb060cdf",
        "not-json",
        &[r#"{"type":"user","timestamp":"2026-09-16T08:00:00Z","message":{"content":"p"}}"#],
    );

    let list = list_subagents(tmp.path(), std::path::Path::new("/Users/demo/proj"), SID).await;
    assert_eq!(list.len(), 2);
    let named = list
        .iter()
        .find(|s| s.agent_id == "dr-planner-f9dcf57d886a89f9")
        .unwrap();
    assert_eq!(named.name, "dr-planner");
    assert_eq!(named.agent_type, "dr-planner");
    assert_eq!(named.description.as_deref(), Some("P1 计划转 tasks.md"));
    assert_eq!(named.model.as_deref(), Some("opus"));
    assert_eq!(named.started_at.as_deref(), Some("2026-09-16T07:12:40Z"));
    assert_eq!(
        named.last_active_at.as_deref(),
        Some("2026-09-16T11:21:46Z")
    );
    assert_eq!(named.status, "active"); // fixture 刚写入，mtime 在窗口内
    let anon = list
        .iter()
        .find(|s| s.agent_id == "ae857e95bdb060cdf")
        .unwrap();
    assert_eq!(anon.name, "ae857e95");
    assert_eq!(anon.agent_type, "subagent");
    assert_eq!(anon.description, None);
}

#[tokio::test]
async fn returns_empty_when_subagents_dir_missing_or_empty() {
    let tmp = tempfile::tempdir().unwrap();
    // 主会话存在但无 subagents 目录
    let proj = tmp.path().join("projects").join("-Users-demo-proj");
    fs::create_dir_all(&proj).unwrap();
    fs::write(proj.join(format!("{SID}.jsonl")), "").unwrap();
    let list = list_subagents(tmp.path(), std::path::Path::new("/Users/demo/proj"), SID).await;
    assert!(list.is_empty());

    // 空目录同样空集
    let sub = fixture(tmp.path());
    let list = list_subagents(tmp.path(), std::path::Path::new("/Users/demo/proj"), SID).await;
    assert!(list.is_empty());
    drop(sub);
}

#[tokio::test]
async fn find_subagent_path_locates_file_and_rejects_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let sub = fixture(tmp.path());
    write_subagent(&sub, "abc123", "{}", &[r#"{"type":"user"}"#]);

    let hit = find_subagent_path(tmp.path(), SID, "abc123").await.unwrap();
    assert!(hit.ends_with(format!("{SID}/subagents/agent-abc123.jsonl").as_str()));

    // 路径逃逸/非法字符集全部拒绝
    for evil in ["../..", "..", "a/b", "a\\b", "A_upper", "", "a.b"] {
        assert!(
            find_subagent_path(tmp.path(), SID, evil).await.is_none(),
            "应拒绝 {evil}"
        );
    }
    // 主会话 id 逃逸同样拒绝
    assert!(find_subagent_path(tmp.path(), "../../x", "abc123")
        .await
        .is_none());
}
