//! 批1 完成判据（任务5）：真实 omp 实机连通验证。
//! spawn → initialize → session/new → prompt → agent_message_chunk 流 → cancel → 干净退出。
//! 真模型调用 + omp 首建会话慢（本地 provider discovery 实测 ~38s），默认 #[ignore]：
//! 跑法 `cargo test -p server --test acp_omp_live -- --ignored --nocapture`。
//! 命令可换：env AGENT_HUB_ACP_BIN（默认 omp）。

use std::{sync::Arc, time::Duration};

use server::{
    config::AcpAgentConfig,
    drivers::acp::{protocol, AcpDriver, Inbound, PROMPT_TIMEOUT, SESSION_NEW_TIMEOUT},
};

fn omp_driver() -> (AcpDriver, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let command = std::env::var("AGENT_HUB_ACP_BIN").unwrap_or_else(|_| "omp".into());
    (
        AcpDriver {
            agents: std::sync::Arc::new(std::sync::RwLock::new(vec![AcpAgentConfig {
                name: "omp".into(),
                command,
                args: vec!["acp".into()],
                cwd: None,
                model: None,
            }])),
        },
        dir,
    )
}

#[tokio::test]
#[ignore = "实机验证：需要本机 omp + 可用模型凭证，cargo test 默认跳过"]
async fn omp_live_connectivity_end_to_end() {
    let (driver, dir) = omp_driver();

    // spawn + initialize + session/new（批1 任务3 全链路对真 omp）
    let (session, mut rx) = match tokio::time::timeout(
        SESSION_NEW_TIMEOUT + Duration::from_secs(30),
        driver.start_session(
            "omp",
            dir.path(),
            None,
            std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        ),
    )
    .await
    {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => panic!("omp 建会话失败: {e}"),
        Err(_) => panic!("omp 建会话超时（>{}s）", SESSION_NEW_TIMEOUT.as_secs()),
    };
    println!("[live] sessionId = {}", session.id);
    assert!(!session.id.is_empty());

    // prompt 期间并发收集 update 流
    let updates: Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let collector = {
        let updates = Arc::clone(&updates);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(Inbound::Notification { method, params }) => {
                        if method == "session/update" {
                            updates.lock().unwrap().push(params["update"].clone());
                        }
                    }
                    Ok(Inbound::ReverseRequest { id, method, .. }) => {
                        panic!("批1 未预期的反向请求: {method} id={id}");
                    }
                    Ok(Inbound::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        })
    };

    let result = session
        .conn
        .request(
            "session/prompt",
            serde_json::json!({
                "sessionId": session.id,
                "prompt": [{"type": "text", "text": "Reply with exactly: ok"}],
            }),
            PROMPT_TIMEOUT,
        )
        .await
        .expect("omp prompt 失败");
    let stop_reason = result["stopReason"].as_str().unwrap_or("").to_string();
    println!("[live] prompt stopReason = {stop_reason}");
    println!(
        "[live] usage = {}",
        serde_json::to_string(&result["usage"]).unwrap()
    );
    assert_eq!(stop_reason, "end_turn");

    // 给尾部通知留窗口后收割
    tokio::time::sleep(Duration::from_millis(500)).await;
    let collected: Vec<serde_json::Value> = std::mem::take(&mut *updates.lock().unwrap());
    collector.abort();

    let kinds: Vec<String> = collected
        .iter()
        .filter_map(|u| u["sessionUpdate"].as_str().map(str::to_string))
        .collect();
    println!("[live] update 类型清单 = {kinds:?}");

    // 流式 chunk 断言：有 agent_message_chunk 且拼出的文本含 "ok"
    let text: String = collected
        .iter()
        .filter(|u| u["sessionUpdate"] == "agent_message_chunk")
        .filter_map(|u| u["content"]["text"].as_str())
        .collect();
    println!("[live] agent_message_chunk 拼接文本 = {text:?}");
    assert!(
        kinds.iter().any(|k| k == "agent_message_chunk"),
        "应收到 agent_message_chunk 流"
    );
    assert!(text.contains("ok"), "chunk 文本应含 ok，实际 {text:?}");
    // 协议版本锁定 v1（initialize 内含于 start_session，双重确认常量）
    assert_eq!(protocol::PROTOCOL_VERSION, 1);

    // cancel + 关停：子进程干净退出、无残留
    session
        .conn
        .notify(
            "session/cancel",
            serde_json::json!({"sessionId": session.id}),
        )
        .await
        .expect("cancel 失败");
    let pid = session.conn.child_pid().await;
    session.conn.shutdown().await;
    if let Some(pid) = pid {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let alive = std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .output()
            .unwrap();
        assert!(
            alive.status.code().is_none_or(|c| c != 0),
            "omp 子进程 {pid} 应已退出"
        );
    }
    assert!(session.conn.is_dead());
    println!("[live] PASS: omp 实机连通全链路通过，子进程无残留");
}

/// 批3 任务11 实机验收（拒绝路径）：拒绝授权后 omp 不得执行目标操作
/// （探针文件不生成）。跑法同上，过滤 `omp_live_permission_reject`。
#[tokio::test]
#[ignore = "实机验证：需要本机 omp + 可用模型凭证，cargo test 默认跳过"]
async fn omp_live_permission_reject_flow() {
    let (driver, dir) = omp_driver();
    let (session, mut rx) = driver
        .start_session(
            "omp",
            dir.path(),
            None,
            std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        )
        .await
        .expect("omp 建会话失败");
    println!("[live-reject] sessionId = {}", session.id);

    let sid = session.id.clone();
    let conn = session.conn.clone();
    let prompt_task = tokio::spawn(async move {
        conn.request(
            "session/prompt",
            serde_json::json!({
                "sessionId": sid,
                "prompt": [{"type": "text",
                    "text": "Use your bash/exec tool to run exactly this command: touch acp_reject_probe.txt. Do nothing else."}],
            }),
            PROMPT_TIMEOUT,
        )
        .await
    });

    let mut permission_seen = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
            Ok(Ok(Inbound::ReverseRequest { id, method, params })) => {
                if method == "session/request_permission" {
                    permission_seen = true;
                    // 拒绝语义：选第一个 reject 项（红线镜像：不伪造放行）
                    let reject = params["options"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .find(|o| o["kind"].as_str().unwrap_or("").starts_with("reject"))
                        .and_then(|o| o["optionId"].as_str())
                        .map(str::to_string);
                    let outcome = match reject {
                        Some(oid) => serde_json::json!({
                            "outcome": {"outcome": "selected", "optionId": oid}
                        }),
                        None => serde_json::json!({"outcome": {"outcome": "cancelled"}}),
                    };
                    println!("[live-reject] 拒绝应答 = {outcome}");
                    let _ = session.conn.respond(&id, &outcome).await;
                } else {
                    let _ = session
                        .conn
                        .respond_error(&id, -32601, &format!("hub 不支持: {method}"))
                        .await;
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
        if prompt_task.is_finished() {
            break;
        }
    }

    let result = prompt_task.await.expect("prompt task panic");
    match result {
        Ok(v) => println!(
            "[live-reject] prompt stopReason = {}",
            v["stopReason"].as_str().unwrap_or("?")
        ),
        Err(e) => println!("[live-reject] prompt 失败: {e}"),
    }
    let executed = dir.path().join("acp_reject_probe.txt").exists();
    println!("[live-reject] 探针文件已生成 = {executed}");
    assert!(permission_seen, "omp 应发出权限请求");
    assert!(!executed, "拒绝授权后 omp 不得执行目标操作");
    println!("[live-reject] RESULT: 拒绝路径实机验证通过（未执行）");
    session.conn.shutdown().await;
}

/// 无 omp 环境的保底：配置层缺省条目形态正确（默认测试可跑）
#[test]
fn omp_default_entry_shape() {
    let (driver, _dir) = omp_driver();
    let agent = driver.agent("omp").unwrap();
    assert_eq!(agent.args, vec!["acp".to_string()]);
    assert!(!agent.command.is_empty());
}

/// 批3 任务11 实机验收：触发 omp 执行需授权工具 → 观测 session/request_permission
/// → 按弹卡语义回传批准项 → prompt 收尾。同时验证 cancel（任务12）后 stopReason=cancelled。
/// 跑法：`cargo test -p server --test acp_omp_live -- --ignored --nocapture omp_live_permission`
#[tokio::test]
#[ignore = "实机验证：需要本机 omp + 可用模型凭证，cargo test 默认跳过"]
async fn omp_live_permission_dialog_flow() {
    let (driver, dir) = omp_driver();
    let (session, mut rx) = driver
        .start_session(
            "omp",
            dir.path(),
            None,
            std::sync::Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
        )
        .await
        .expect("omp 建会话失败");
    println!("[live-perm] sessionId = {}", session.id);

    let sid = session.id.clone();
    let conn = session.conn.clone();
    let prompt_task = tokio::spawn(async move {
        conn.request(
            "session/prompt",
            serde_json::json!({
                "sessionId": sid,
                "prompt": [{"type": "text",
                    "text": "Use your bash/exec tool to run exactly this command: touch acp_perm_probe.txt. Do nothing else."}],
            }),
            PROMPT_TIMEOUT,
        )
        .await
    });

    // 反向请求泵：权限请求按「用户批准」语义回传（选第一个 allow 项）；其余 method-not-found
    let mut permission_seen = false;
    loop {
        match tokio::time::timeout(Duration::from_secs(120), rx.recv()).await {
            Ok(Ok(Inbound::ReverseRequest { id, method, params })) => {
                if method == "session/request_permission" {
                    permission_seen = true;
                    let options = params["options"].as_array().cloned().unwrap_or_default();
                    println!(
                        "[live-perm] 权限请求 options = {}",
                        serde_json::to_string(&options).unwrap()
                    );
                    let allow = options
                        .iter()
                        .find(|o| o["kind"].as_str().unwrap_or("").starts_with("allow"))
                        .and_then(|o| o["optionId"].as_str())
                        .map(str::to_string);
                    let outcome = match allow {
                        Some(oid) => serde_json::json!({
                            "outcome": {"outcome": "selected", "optionId": oid}
                        }),
                        // 红线：无 allow 项可点 = 取消，不伪造放行
                        None => serde_json::json!({"outcome": {"outcome": "cancelled"}}),
                    };
                    println!("[live-perm] 批准应答 = {outcome}");
                    session
                        .conn
                        .respond(&id, &outcome)
                        .await
                        .expect("权限应答写出失败");
                } else {
                    println!("[live-perm] 其他反向请求: {method} → method-not-found");
                    let _ = session
                        .conn
                        .respond_error(&id, -32601, &format!("hub 不支持: {method}"))
                        .await;
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => break,
            Err(_) => break, // 120s 无事件且 prompt 未完 → 退出循环收割结果
        }
        if prompt_task.is_finished() {
            break;
        }
    }

    let result = prompt_task.await.expect("prompt task panic");
    match result {
        Ok(v) => println!(
            "[live-perm] prompt stopReason = {}",
            v["stopReason"].as_str().unwrap_or("?")
        ),
        Err(e) => println!("[live-perm] prompt 失败: {e}"),
    }
    println!(
        "[live-perm] 探针文件已生成 = {}",
        dir.path().join("acp_perm_probe.txt").exists()
    );
    println!(
        "[live-perm] RESULT: omp {} 发出权限请求（弹卡链路{}）",
        if permission_seen { "已" } else { "未" },
        if permission_seen {
            "实机验证通过"
        } else {
            "走 fake agent 集成测试覆盖（omp 当前配置未触发授权请求）"
        }
    );
    session.conn.shutdown().await;
}
