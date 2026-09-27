use std::time::Duration;

use server::drivers::claude::watcher::{spawn_poll, spawn_watch, HubEvent};
use tokio::sync::broadcast;

#[tokio::test]
async fn hub_event_serializes_tagged() {
    let changed = serde_json::to_string(&HubEvent::JobsChanged { id: "abc".into() }).unwrap();
    assert_eq!(changed, r#"{"type":"jobs_changed","id":"abc"}"#);
    let tick = serde_json::to_string(&HubEvent::Tick).unwrap();
    assert_eq!(tick, r#"{"type":"tick"}"#);
}

#[tokio::test]
async fn watch_emits_jobs_changed_on_existing_dir() {
    let dir = tempfile::tempdir().unwrap();
    // 先建好子目录再 spawn_watch：tempdir 新建目录 + FSEvents 首次事件注册延迟可能 >3s，
    // 监听已存在目录内的写入可避开（tester-r2 FAIL-4）
    let job = dir.path().join("x");
    std::fs::create_dir_all(&job).unwrap();

    let (tx, mut rx) = broadcast::channel(16);
    let handle = spawn_watch(dir.path().to_path_buf(), tx);

    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(job.join("state.json"), r#"{"state":"running"}"#).unwrap();

    // debounce 300ms + flush 周期 + FSEvents 首事件延迟，收口到 5s 容差
    let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("5s 内应收到事件")
        .expect("channel 不应关闭");
    assert_eq!(ev, HubEvent::JobsChanged { id: "x".into() });

    handle.abort();
}

#[tokio::test]
async fn watch_exits_gracefully_when_dir_missing() {
    let (tx, _rx) = broadcast::channel(1);
    let handle = spawn_watch(std::path::PathBuf::from("/definitely/not/here"), tx);
    let result = handle.await;
    assert!(result.is_ok(), "watcher 应正常退出不 panic");
}

#[tokio::test]
async fn poll_emits_two_ticks() {
    let (tx, mut rx) = broadcast::channel(16);
    let handle = spawn_poll(tx, Duration::from_millis(100));

    let first = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap();
    assert_eq!(first.unwrap(), HubEvent::Tick);
    let second = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .unwrap();
    assert_eq!(second.unwrap(), HubEvent::Tick);

    handle.abort();
}

#[tokio::test]
async fn poll_continues_when_no_receivers() {
    let (tx, rx) = broadcast::channel::<HubEvent>(16);
    let handle = spawn_poll(tx.clone(), Duration::from_millis(100));

    // 立即丢弃所有 receiver
    drop(rx);

    // 等待 poll 经历多次 tick（应该不退出）
    tokio::time::sleep(Duration::from_millis(500)).await;

    // 创建新 receiver，验证 poll 仍在运行
    let mut rx2 = tx.subscribe();
    let result = tokio::time::timeout(Duration::from_secs(1), rx2.recv())
        .await
        .expect("poll 应继续发送，新 receiver 应能收到事件")
        .expect("channel 不应关闭");
    assert_eq!(result, HubEvent::Tick);

    handle.abort();
}
