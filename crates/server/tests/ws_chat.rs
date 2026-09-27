use std::time::Duration;

use server::drivers::claude::session::{spawn_session_tail, ChatEvent};

#[tokio::test]
async fn tail_pushes_new_lines_with_seq_and_skips_bad_lines() {
    let dir = tempfile::tempdir().unwrap();
    // <projects_dir>/projects/<slug>/<sessionId>.jsonl
    let slug_dir = dir.path().join("projects").join("-Users-demo-proj-alpha");
    std::fs::create_dir_all(&slug_dir).unwrap();
    let jsonl = slug_dir.join("sess-t1.jsonl");
    std::fs::write(
        &jsonl,
        concat!(
            r#"{"type":"user","timestamp":"2026-09-16T08:00:00.000Z","message":{"role":"user","content":"first"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-09-16T08:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]}}"#,
            "\n"
        ),
    )
    .unwrap();

    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let handle = spawn_session_tail(dir.path().to_path_buf(), "sess-t1".into(), tx);

    // 等首轮游标建立（≥500ms tail 间隔）
    tokio::time::sleep(Duration::from_millis(700)).await;

    // 追加：好行 + 坏行 + 好行
    std::fs::write(
        &jsonl,
        concat!(
            r#"{"type":"user","timestamp":"2026-09-16T08:00:00.000Z","message":{"role":"user","content":"first"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-09-16T08:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]}}"#,
            "\n",
            r#"{"type":"user","timestamp":"2026-09-16T08:00:02.000Z","message":{"role":"user","content":"second"}}"#,
            "\n",
            "broken {{{\n",
            r#"{"type":"assistant","timestamp":"2026-09-16T08:00:03.000Z","message":{"role":"assistant","content":[{"type":"text","text":"third"}]}}"#,
            "\n"
        ),
    )
    .unwrap();

    // 1s 内收到两条（坏行跳过，不占 seq）
    let ev1 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("1s 内应收到第一条")
        .expect("channel open");
    assert_eq!(ev1.seq, 1);
    assert_eq!(ev1.session_id, "sess-t1");
    assert_eq!(ev1.message.text.as_deref(), Some("second"));
    assert_eq!(ev1.message.kind, server::models::ChatMessageKind::User);

    let ev2 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("1s 内应收到第二条")
        .expect("channel open");
    assert_eq!(ev2.seq, 2, "seq 递增");
    assert_eq!(ev2.message.text.as_deref(), Some("third"));

    handle.abort();
}

#[tokio::test]
async fn tail_exits_when_session_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, _rx) = tokio::sync::broadcast::channel::<ChatEvent>(4);
    let handle = spawn_session_tail(dir.path().to_path_buf(), "no-such".into(), tx);
    let r = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(r.is_ok(), "找不到会话文件应快速退出不 panic");
}
