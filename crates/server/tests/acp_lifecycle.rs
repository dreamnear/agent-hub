//! ACP 生命周期集成测试（批1 任务3）：fake agent 完整 initialize + session/new 流程、
//! 协议版本协商失败路径、注册表挂载。

use std::path::Path;

use server::{
    config::AcpAgentConfig,
    drivers::acp::{AcpDriver, AcpStatus},
};

fn fake_driver(mode: &str) -> (AcpDriver, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_fake_agent.py");
    let driver = AcpDriver {
        agents: vec![AcpAgentConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![
                "-u".into(),
                fixture.to_string_lossy().into_owned(),
                mode.into(),
            ],
            cwd: None,
            model: None,
        }],
    };
    (driver, dir)
}

/// 批2 任务8：start_session 需要房间表；测试用空表（不订阅不广播）
fn empty_rooms() -> std::sync::Arc<
    std::sync::RwLock<
        std::collections::HashMap<
            String,
            tokio::sync::broadcast::Sender<server::drivers::claude::session::ChatEvent>,
        >,
    >,
> {
    std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new()))
}

#[tokio::test]
async fn lifecycle_initialize_and_session_new_full_flow() {
    let (driver, dir) = fake_driver("basic");
    let (session, _rx) = driver
        .start_session("fake", dir.path(), Some("fake-model".into()), empty_rooms())
        .await
        .unwrap();
    assert_eq!(session.id, "fake-session-1");
    assert_eq!(session.status, AcpStatus::Idle);
    assert_eq!(session.agent, "fake");
    assert_eq!(session.model.as_deref(), Some("fake-model"));
    assert_eq!(session.cwd, dir.path().to_string_lossy());
    assert!(!session.conn.is_dead());
    session.conn.shutdown().await;
}

#[tokio::test]
async fn lifecycle_version_mismatch_fails() {
    let (driver, dir) = fake_driver("v2");
    let err = driver
        .start_session("fake", dir.path(), None, empty_rooms())
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("版本协商失败"),
        "错误应指向版本协商: {err}"
    );
    // 失败路径连接已收尸：无 fake agent 残留
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
}

#[tokio::test]
async fn lifecycle_unknown_agent_errors() {
    let (driver, dir) = fake_driver("basic");
    let err = driver
        .start_session("nope", dir.path(), None, empty_rooms())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("未知 ACP agent"));
}
