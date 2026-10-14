//! ACP 传输层集成测试（批1 任务2）：fake agent 子进程验证
//! 帧编解码、请求 id 关联、子进程异常退出资源回收、进程组 kill 连坐。

use std::{path::Path, time::Duration};

use server::{
    config::AcpAgentConfig,
    drivers::acp::{AcpConnection, Inbound, INIT_TIMEOUT, PROMPT_TIMEOUT, SESSION_NEW_TIMEOUT},
};

fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_fake_agent.py")
}

fn fake_agent(mode: &str) -> (AcpAgentConfig, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (
        AcpAgentConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![
                "-u".into(),
                fixture().to_string_lossy().into_owned(),
                mode.into(),
            ],
            cwd: None,
            model: None,
        },
        dir,
    )
}

#[tokio::test]
async fn transport_request_id_correlation_and_updates() {
    let (agent, _dir) = fake_agent("basic");
    let (conn, mut inbound) = AcpConnection::spawn(&agent, _dir.path()).await.unwrap();

    // initialize / session/new / prompt 三次并发度低的串行请求：id 一一对应
    let init = conn
        .request(
            "initialize",
            serde_json::json!({"protocolVersion": 1}),
            INIT_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(init["protocolVersion"], 1);

    let new = conn
        .request(
            "session/new",
            serde_json::json!({"cwd": _dir.path(), "mcpServers": []}),
            SESSION_NEW_TIMEOUT,
        )
        .await
        .unwrap();
    let sid = new["sessionId"].as_str().unwrap().to_string();
    assert_eq!(sid, "fake-session-1");

    let prompt = conn
        .request(
            "session/prompt",
            serde_json::json!({"sessionId": sid, "prompt": [{"type": "text", "text": "hi"}]}),
            PROMPT_TIMEOUT,
        )
        .await
        .unwrap();
    assert_eq!(prompt["stopReason"], "end_turn");

    // 通知流：5 条 session/update（4 已知 + 1 未知类型，传输层原样透传）
    let mut kinds = Vec::new();
    while kinds.len() < 5 {
        match tokio::time::timeout(Duration::from_secs(5), inbound.recv())
            .await
            .unwrap()
        {
            Ok(Inbound::Notification { method, params }) => {
                assert_eq!(method, "session/update");
                kinds.push(
                    params["update"]["sessionUpdate"]
                        .as_str()
                        .unwrap()
                        .to_string(),
                );
            }
            Ok(other) => panic!("期望 Notification，实际 {other:?}"),
            Err(e) => panic!("通知流中断: {e}"),
        }
    }
    assert!(kinds.contains(&"agent_message_chunk".to_string()));
    assert!(kinds.contains(&"unknown_xyz_update".to_string()));
    assert!(!conn.is_dead());
    conn.shutdown().await;
}

#[tokio::test]
async fn transport_child_crash_fails_pending_and_marks_dead() {
    let (agent, _dir) = fake_agent("crash-on-prompt");
    let (conn, mut inbound) = AcpConnection::spawn(&agent, _dir.path()).await.unwrap();
    conn.request(
        "initialize",
        serde_json::json!({"protocolVersion": 1}),
        INIT_TIMEOUT,
    )
    .await
    .unwrap();

    // prompt 触发子进程 exit(70)：pending 请求须失败而非挂死
    let result = conn
        .request(
            "session/prompt",
            serde_json::json!({"sessionId": "fake-session-1", "prompt": []}),
            PROMPT_TIMEOUT,
        )
        .await;
    assert!(result.is_err(), "子进程退出后 pending 请求应报错");

    // Closed 事件可观（注册表状态翻转依据）
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match inbound.recv().await {
                Ok(Inbound::Closed) => return true,
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return false,
                Err(_) => continue,
            }
        }
    })
    .await
    .unwrap();
    assert!(closed, "应收到 Inbound::Closed");
    assert!(conn.is_dead());

    // 死连接上的新请求快速失败（不挂死）
    let again = conn
        .request(
            "initialize",
            serde_json::json!({"protocolVersion": 1}),
            Duration::from_secs(2),
        )
        .await;
    assert!(again.is_err());
}

#[tokio::test]
async fn transport_shutdown_kills_process_group() {
    // fork-child 模式：agent 先 fork `sleep 30` 孙进程，shutdown 须连坐清除
    let (agent, _dir) = fake_agent("fork-child");
    let (conn, _inbound) = AcpConnection::spawn(&agent, _dir.path()).await.unwrap();
    conn.request(
        "initialize",
        serde_json::json!({"protocolVersion": 1}),
        INIT_TIMEOUT,
    )
    .await
    .unwrap();

    // 记录孙进程 pid（agent 的直接子进程），shutdown 后定点验证其消失。
    // 不用 pgrep -f 宽匹配：本机其他进程可能同名造成误报（实测踩坑）。
    let agent_pid = conn.child_pid().await.expect("agent pid");
    let mut grandchild: Option<u32> = None;
    for _ in 0..20 {
        let out = std::process::Command::new("pgrep")
            .args(["-P", &agent_pid.to_string()])
            .output()
            .unwrap();
        let line = String::from_utf8_lossy(&out.stdout);
        if let Some(pid) = line.lines().next().and_then(|l| l.trim().parse().ok()) {
            grandchild = Some(pid);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let grandchild = grandchild.expect("孙进程 sleep 应已 fork");

    conn.shutdown().await;
    assert!(conn.is_dead(), "shutdown 后连接应标记死亡");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let alive = std::process::Command::new("ps")
        .args(["-p", &grandchild.to_string()])
        .output()
        .unwrap();
    assert!(
        alive.status.code().is_none_or(|c| c != 0),
        "孙进程 {grandchild} 应被进程组连坐清除"
    );
}

/// 直接 spawn 校验：命令不存在时报错不 panic（错误路径）
#[tokio::test]
async fn transport_spawn_missing_binary_errors() {
    let dir = tempfile::tempdir().unwrap();
    let agent = AcpAgentConfig {
        name: "nope".into(),
        command: "/nonexistent/acp-binary-xyz".into(),
        args: vec![],
        cwd: None,
        model: None,
    };
    assert!(AcpConnection::spawn(&agent, dir.path()).await.is_err());
}
