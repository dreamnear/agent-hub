//! ACP REST 端点集成测试（批1 任务4）：create/prompt/cancel 全栈（fake agent），
//! update 三类经 chat_rooms 房间可观、未知类型不透出、权限占位自动拒绝。

use std::{path::Path, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use server::{
    api::AppState,
    config::{AcpAgentConfig, AcpConfig, ChatConfig, Config},
    drivers::{acp::AcpStatus, claude::session::ChatEvent},
    router,
};
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

fn fake_agent(mode: &str, log: Option<&Path>) -> AcpAgentConfig {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_fake_agent.py");
    let mut args = vec![
        "-u".to_string(),
        fixture.to_string_lossy().into_owned(),
        mode.into(),
    ];
    if let Some(log) = log {
        args.push(log.to_string_lossy().into_owned());
    }
    AcpAgentConfig {
        name: "fake".into(),
        command: "python3".into(),
        args,
        cwd: None,
        model: None,
    }
}

/// agents list 刷新是异步的（本地 <50ms，CI 慢机可达数秒）：轮询至条目出现。
/// 根因修复 r2：两处「动作后立即查 list 并 unwrap」在 CI 偶发 None。
async fn wait_for_agent(app: axum::Router, sid: &str) -> Value {
    for _ in 0..100 {
        let (_, body) = req(app.clone(), "GET", "/api/agents?all=1", None).await;
        let hit = body
            .as_array()
            .and_then(|a| a.iter().find(|x| x["id"] == sid).cloned());
        if let Some(e) = hit {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("agent {sid} 未在 5s 内出现在 /api/agents");
}

fn test_app(mode: &str, log: Option<&Path>) -> (axum::Router, Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::load();
    // 钉 fake claude：不依赖宿主机的真实 claude CLI（CI 裸机没有）
    cfg.claude_bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh");
    cfg.acp = AcpConfig {
        agents: vec![fake_agent(mode, log)],
    };
    cfg.chat = ChatConfig::default();
    // ocr-review 高：隔离落盘——Config::load() 默认指向真实 ~/.claude-view/acp_sessions.json，
    // 不指回 tempdir 的话测试会整文件覆写用户真实持久化会话
    cfg.acp_sessions_file = dir.path().join("acp_sessions.json");
    let state = Arc::new(AppState::from_config(cfg));
    (router(state.clone()), state, dir)
}

#[tokio::test]
async fn acp_api_create_prompt_updates_and_cancel() {
    let (app, state, dir) = test_app("basic", None);

    // 未知 agent → 404
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "nope", "cwd": dir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // cwd 不存在 → 400
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": "/nonexistent/xyz"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 创建
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": dir.path(), "model": "m1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["sessionId"].as_str().unwrap().to_string();
    assert_eq!(body["agent"], "fake");
    assert_eq!(body["model"], "m1");
    assert_eq!(
        state.acp_registry.get(&sid).unwrap().status,
        AcpStatus::Idle
    );

    // prompt 前订阅房间（API 同款通道）
    let mut rx = state
        .chat_rooms
        .read()
        .expect("chat_rooms lock")
        .get(&sid)
        .unwrap()
        .subscribe();

    // prompt 走统一路由
    let (status, _) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/prompt"),
        Some(json!({"text": "hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // prompt 结束回 Idle
    assert_eq!(
        state.acp_registry.get(&sid).unwrap().status,
        AcpStatus::Idle
    );

    // 房间可观五类事件（用户消息 + 四类 update；未知 update 不透出）；seq 递增
    let mut kinds = Vec::new();
    for _ in 0..5 {
        let ev: ChatEvent = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        kinds.push((ev.message.kind.as_str().to_string(), ev.seq));
    }
    assert_eq!(kinds[0].0, "user"); // 批2 任务8：prompt 用户消息入流
    assert_eq!(kinds[0].1, 1);
    assert_eq!(kinds[1].0, "assistant");
    assert_eq!(kinds[1].1, 2);
    assert_eq!(kinds[2].0, "tool_use");
    assert_eq!(kinds[2].1, 3);
    assert_eq!(kinds[3].0, "tool_result");
    assert_eq!(kinds[3].1, 4);
    assert_eq!(kinds[4].0, "other"); // plan
    assert_eq!(kinds[4].1, 5);
    // 无多余事件（unknown_xyz_update 被丢弃）
    assert!(tokio::time::timeout(Duration::from_millis(300), rx.recv())
        .await
        .is_err());

    // cancel → fake 退出 → 注册表翻 Dead
    let (status, _) = req(
        app,
        "POST",
        &format!("/api/acp/sessions/{sid}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let dead = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if state.acp_registry.get(&sid).unwrap().status == AcpStatus::Dead {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(dead.is_ok(), "cancel 后会话应翻 Dead");
}

/// 批3 任务11：权限弹卡链路——prompt 期间权限请求以 acp_permission 消息推入
/// feed/房间；REST /permission 应答 allow 后 agent 继续跑完 prompt；fake agent
/// 日志记录收到的应答 = 用户所选选项。
#[tokio::test]
async fn acp_api_permission_card_then_allow_answer() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fake-events.log");
    let (app, state, workdir) = test_app("permission", Some(&log_path));

    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["sessionId"].as_str().unwrap().to_string();

    // prompt 阻塞到权限应答（fake permission 模式等应答才继续），后台发起
    let prompt_app = app.clone();
    let prompt_uri = format!("/api/acp/sessions/{sid}/prompt");
    let prompt_task = tokio::spawn(async move {
        req(
            prompt_app,
            "POST",
            &prompt_uri,
            Some(json!({"text": "do risky"})),
        )
        .await
    });

    // 房间可观权限卡消息（permKey 回传 + options 全量，前端弹卡数据源）
    let mut rx = state
        .chat_rooms
        .read()
        .expect("chat_rooms lock")
        .get(&sid)
        .unwrap()
        .subscribe();
    let perm_msg = loop {
        let ev: ChatEvent = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if ev.message.raw_type.as_deref() == Some("acp_permission") {
            break ev.message;
        }
    };
    let perm_key = perm_msg.tool_use_id.clone().unwrap();
    assert!(perm_key.starts_with("perm:"), "permKey 形态: {perm_key}");
    assert_eq!(perm_msg.tool_name.as_deref(), Some("risky op"));
    assert_eq!(
        perm_msg.input.as_ref().unwrap()["options"][0]["optionId"],
        "opt-allow"
    );

    // 用户批准 → 200；prompt 整轮随后完成
    let (status, _) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/permission"),
        Some(json!({"permId": perm_key, "optionId": "opt-allow"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = prompt_task.await.unwrap();
    assert_eq!(status, StatusCode::OK);

    // 回执消息入流（多端弹卡同步关闭）
    let msgs = state.acp_registry.get(&sid).unwrap().feed.history();
    assert!(
        msgs.iter()
            .any(|m| m.raw_type.as_deref() == Some("acp_permission_resolved")
                && m.tool_use_id.as_deref() == Some(perm_key.as_str())),
        "应有 resolved 回执"
    );

    // fake agent 收到的应答 = 用户所选 allow 项
    let answer = read_permission_answer(&log_path).await;
    assert_eq!(
        answer["permission_answer"]["result"]["outcome"]["optionId"],
        "opt-allow"
    );
}

/// 批3 任务11：拒绝路径 + r77 幂等应答（红线语义：不默认放行；重复应答 200 吞掉，
/// 「Conflict」不再出现在用户界面）
#[tokio::test]
async fn acp_api_permission_reject_answer_then_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fake-events.log");
    let (app, state, workdir) = test_app("permission", Some(&log_path));

    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["sessionId"].as_str().unwrap().to_string();

    let prompt_app = app.clone();
    let prompt_uri = format!("/api/acp/sessions/{sid}/prompt");
    let prompt_task = tokio::spawn(async move {
        req(
            prompt_app,
            "POST",
            &prompt_uri,
            Some(json!({"text": "do risky"})),
        )
        .await
    });

    // 等权限卡出现 + 状态翻 awaiting_input（批4 三态对齐）——轮询 feed 与 registry
    let perm_key = loop {
        if let Some(s) = state.acp_registry.get(&sid) {
            if s.status == AcpStatus::AwaitingInput {
                if let Some(m) = s
                    .feed
                    .history()
                    .into_iter()
                    .find(|m| m.raw_type.as_deref() == Some("acp_permission"))
                {
                    assert!(
                        s.status == AcpStatus::AwaitingInput,
                        "弹卡挂起 → awaiting_input"
                    );
                    break m.tool_use_id.unwrap();
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    assert!(perm_key.starts_with("perm:"), "permKey 形态: {perm_key}");

    // 弹卡挂起期间，agents list 该会话 group=needs_input（批4：与 Claude blocked 同语义）
    let e = wait_for_agent(app.clone(), &sid).await;
    assert_eq!(e["rawState"], "awaiting_input");
    assert_eq!(e["group"], "needs_input");

    let (status, _) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/permission"),
        Some(json!({"permId": perm_key, "optionId": "opt-reject"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 应答回流 → prompt 整轮完成（send 收尾回 Idle）
    let (status, _) = prompt_task.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.acp_registry.get(&sid).unwrap().status,
        AcpStatus::Idle
    );

    // fake agent 收到的是用户所选拒绝项（不默认放行）
    let answer = read_permission_answer(&log_path).await;
    assert_eq!(
        answer["permission_answer"]["result"]["outcome"]["optionId"],
        "opt-reject"
    );

    // 同一 permKey 二次应答 → r77 幂等 200（不误放行：挂起键已消费，重复点击无效果）
    let (status, body) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/permission"),
        Some(json!({"permId": perm_key, "optionId": "opt-allow"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["alreadyResolved"], true, "重复应答应幂等吞掉");

    // 未知 permId（从未存在）→ 同样 200 幂等
    let (status, body) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/permission"),
        Some(json!({"permId": "perm:x:999", "optionId": "opt-allow"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["alreadyResolved"], true);

    // 未知会话 → 404
    let (status, _) = req(
        app,
        "POST",
        "/api/acp/sessions/ghost/permission",
        Some(json!({"permId": "perm:x:1", "optionId": "opt-allow"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// r77：连续多权限请求（omp 多工具调用实测形态）——每张卡唯一 permKey、
/// 逐个应答互不干扰、各出一张回执；agent 依次收到两个应答。
#[tokio::test]
async fn acp_api_multi_permission_cards_answered_sequentially() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fake-events.log");
    let (app, state, workdir) = test_app("permission-multi", Some(&log_path));

    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["sessionId"].as_str().unwrap().to_string();

    let prompt_app = app.clone();
    let prompt_uri = format!("/api/acp/sessions/{sid}/prompt");
    let prompt_task = tokio::spawn(async move {
        req(
            prompt_app,
            "POST",
            &prompt_uri,
            Some(json!({"text": "do risky"})),
        )
        .await
    });

    // 逐卡应答：出现一张卡 → 应答一张（第一张 allow，第二张 reject）
    let mut perm_keys = Vec::new();
    for want in ["opt-allow", "opt-reject"] {
        let perm_key = loop {
            let msgs = state.acp_registry.get(&sid).unwrap().feed.history();
            let pending: Vec<_> = msgs
                .iter()
                .filter(|m| m.raw_type.as_deref() == Some("acp_permission"))
                .filter(|m| {
                    !msgs.iter().any(|r| {
                        r.raw_type.as_deref() == Some("acp_permission_resolved")
                            && r.tool_use_id == m.tool_use_id
                    })
                })
                .collect();
            if let Some(m) = pending.last() {
                break m.tool_use_id.clone().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert!(
            !perm_keys.contains(&perm_key),
            "卡键必须唯一（r77）：{perm_key}"
        );
        let (status, _) = req(
            app.clone(),
            "POST",
            &format!("/api/acp/sessions/{sid}/permission"),
            Some(json!({"permId": perm_key, "optionId": want})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        // 等该卡回执落地（dispatch 推送）再找下一张，排除回执在途窗口
        loop {
            let msgs = state.acp_registry.get(&sid).unwrap().feed.history();
            if msgs.iter().any(|m| {
                m.raw_type.as_deref() == Some("acp_permission_resolved")
                    && m.tool_use_id.as_deref() == Some(perm_key.as_str())
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        perm_keys.push(perm_key);
    }

    let (status, _) = prompt_task.await.unwrap();
    assert_eq!(status, StatusCode::OK);

    // 两个卡键不同 + 各有回执
    assert_ne!(perm_keys[0], perm_keys[1]);
    let msgs = state.acp_registry.get(&sid).unwrap().feed.history();
    for key in &perm_keys {
        assert!(
            msgs.iter()
                .any(|m| m.raw_type.as_deref() == Some("acp_permission_resolved")
                    && m.tool_use_id.as_deref() == Some(key.as_str())),
            "卡 {key} 应有回执"
        );
    }

    // agent 依次收到两个应答 = 用户所选（allow → reject）
    let answers = read_permission_answers(&log_path, 2).await;
    assert_eq!(answers.len(), 2, "应有两个权限应答记录: {answers:?}");
    assert_eq!(
        answers[0]["permission_answer"]["result"]["outcome"]["optionId"],
        "opt-allow"
    );
    assert_eq!(
        answers[1]["permission_answer"]["result"]["outcome"]["optionId"],
        "opt-reject"
    );
}

/// r77：会话取消/死亡时挂起权限卡收场出回执——前端弹卡同步消失，不留挂着可点的卡。
#[tokio::test]
async fn acp_api_cancel_resolves_pending_permission_card() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fake-events.log");
    let (app, state, workdir) = test_app("permission", Some(&log_path));

    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = body["sessionId"].as_str().unwrap().to_string();

    let prompt_app = app.clone();
    let prompt_uri = format!("/api/acp/sessions/{sid}/prompt");
    let prompt_task = tokio::spawn(async move {
        req(
            prompt_app,
            "POST",
            &prompt_uri,
            Some(json!({"text": "do risky"})),
        )
        .await
    });

    // 等卡出现（挂起中）
    loop {
        let msgs = state.acp_registry.get(&sid).unwrap().feed.history();
        if msgs
            .iter()
            .any(|m| m.raw_type.as_deref() == Some("acp_permission"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 用户取消会话 → 挂起权限收场出回执
    let (status, _) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let session = state.acp_registry.get(&sid).unwrap();
        let resolved = session
            .feed
            .history()
            .into_iter()
            .any(|m| m.raw_type.as_deref() == Some("acp_permission_resolved"));
        if resolved && session.status == AcpStatus::Dead {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "取消后权限卡应收场出回执且会话翻 Dead"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = prompt_task.await;
}

/// 从 fake agent 事件日志读 permission_answer 记录（最多等 5s）
async fn read_permission_answer(log_path: &Path) -> Value {
    read_permission_answers(log_path, 1)
        .await
        .into_iter()
        .next()
        .expect("权限应答未落日志")
}

/// 读全部 permission_answer 记录（r77 多请求测试用；按 req_id 排序），等满 expected 条
async fn read_permission_answers(log_path: &Path, expected: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(raw) = std::fs::read_to_string(log_path) {
            let mut answers: Vec<Value> = raw
                .lines()
                .filter(|l| l.contains("permission_answer"))
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            answers.sort_by_key(|a| a["req_id"].as_i64().unwrap_or(0));
            if answers.len() >= expected {
                return answers;
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "权限应答未落日志");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 不存在会话的 prompt → 404
#[tokio::test]
async fn acp_api_unknown_session_404() {
    let (app, _state, _dir) = test_app("basic", None);
    let (status, _) = req(
        app,
        "POST",
        "/api/acp/sessions/ghost/prompt",
        Some(json!({"text": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 批2 任务7：model 透传 session/new（omp configOptions 语义）；未指定不带键。
#[tokio::test]
async fn acp_create_session_passes_model_to_session_new() {
    let dir = tempfile::tempdir().unwrap();
    let log_path = dir.path().join("fake-events.log");
    let (app, _state, workdir) = test_app("basic", Some(&log_path));

    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir.path(), "model": "claude-sonnet"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let entry: Value = loop {
        if let Ok(raw) = std::fs::read_to_string(&log_path) {
            if let Some(line) = raw.lines().find(|l| l.contains("session_new_params")) {
                break serde_json::from_str(line).unwrap();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session/new 参数未落日志"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let params = &entry["session_new_params"];
    assert_eq!(params["model"], "claude-sonnet");
    assert_eq!(params["cwd"], workdir.path().to_string_lossy().as_ref());

    // 未指定 model → 不带 model 键（宽松协议：多余键也可能被 agent 忽略，缺省不送）
    let log2 = workdir.path().join("fake-events2.log");
    let (app2, _s2, workdir2) = test_app("basic", Some(&log2));
    let (status, _) = req(
        app2,
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir2.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let entry: Value = loop {
        if let Ok(raw) = std::fs::read_to_string(&log2) {
            if let Some(line) = raw.lines().find(|l| l.contains("session_new_params")) {
                break serde_json::from_str(line).unwrap();
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "session/new 参数未落日志"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(entry["session_new_params"].get("model").is_none());
}

/// 批2 任务7：create 成功即广播 Tick（前端侧栏即时出现，免轮询等待）。
#[tokio::test]
async fn acp_create_session_emits_tick_event() {
    let (app, state, dir) = test_app("basic", None);
    let mut events_rx = state.events_tx.subscribe();

    let (status, _) = req(
        app,
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": dir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let ev = tokio::time::timeout(Duration::from_secs(2), events_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        ev,
        server::drivers::claude::watcher::HubEvent::Tick
    ));
}

/// 批2 任务6：agents list 融合 ACP 会话条目——driver=acp、raw_state=注册表状态、
/// group 三段式映射、session_id=ACP sessionId（WS chat 房间同键）。
#[tokio::test]
async fn acp_sessions_merged_into_agents_list() {
    let (app, state, dir) = test_app("basic", None);

    // 创建前：list 无 acp 条目
    let (_, body) = req(app.clone(), "GET", "/api/agents?all=1", None).await;
    assert!(body
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["driver"] != "acp"));

    let (status, created) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": dir.path(), "model": "m1"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();

    // 创建后：list 恰含一条 acp 条目，字段逐一对齐
    let e = wait_for_agent(app.clone(), &sid).await;
    assert_eq!(e["driver"], "acp");
    assert_eq!(e["id"], sid.as_str());
    assert_eq!(e["name"], "fake");
    assert_eq!(e["cwd"], dir.path().to_string_lossy().as_ref());
    assert_eq!(e["kind"], "acp");
    assert_eq!(e["rawState"], "idle");
    assert_eq!(e["group"], "other"); // idle → 空闲桶
    assert_eq!(e["sessionId"], sid.as_str());

    // prompt 后回 idle（working 中间态由状态机保证），cancel 后 dead → completed 桶
    let (status, _) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/prompt"),
        Some(json!({"text": "hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = req(app.clone(), "GET", "/api/agents?all=1", None).await;
    assert_eq!(status, StatusCode::OK);
    let e = body
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == sid.as_str())
        .unwrap();
    assert_eq!(e["rawState"], "idle");
    assert_eq!(e["group"], "other");
    drop(state);

    let (status, _) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/cancel"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (_, body) = req(app.clone(), "GET", "/api/agents?all=1", None).await;
        let e = body
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == sid.as_str())
            .unwrap()
            .clone();
        if e["rawState"] == "dead" {
            assert_eq!(e["group"], "completed"); // dead → 已完成桶
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "cancel 后未翻 dead");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 批2 任务8：prompt 用户消息 + update 流全部落 history（GET messages 回放），
/// 顺序 = 用户在前、chunk 按到达序；raw_type 携带 acp_* 标记供前端分流。
#[tokio::test]
async fn acp_prompt_history_replay_in_order() {
    let (app, _state, dir) = test_app("basic", None);
    let (status, created) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": dir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();

    let (status, _) = req(
        app.clone(),
        "POST",
        &format!("/api/acp/sessions/{sid}/prompt"),
        Some(json!({"text": "hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = req(
        app.clone(),
        "GET",
        &format!("/api/acp/sessions/{sid}/messages"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let msgs = body["messages"].as_array().unwrap();
    let kinds: Vec<&str> = msgs.iter().map(|m| m["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        vec!["user", "assistant", "tool_use", "tool_result", "other"]
    );
    assert_eq!(msgs[0]["text"], "hi");
    assert_eq!(msgs[0]["rawType"], "acp_user");
    assert_eq!(msgs[1]["rawType"], "acp_chunk");
    assert_eq!(msgs[1]["text"], "chunk0");
    assert_eq!(msgs[2]["rawType"], "acp_tool_call");
    assert_eq!(msgs[3]["rawType"], "acp_tool_update");
    assert_eq!(msgs[4]["rawType"], "acp_plan");
    // 分页信封对齐 MessagePage 形状（前端 useSession 同构消费）
    assert_eq!(body["firstLine"], 0);
    assert_eq!(body["hasMore"], false);

    // 未知会话 → 404
    let (status, _) = req(app, "GET", "/api/acp/sessions/ghost/messages", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// 批2 任务8：prompt 起止广播 Tick（前端 working/空闲即时翻转，不等轮询）。
#[tokio::test]
async fn acp_prompt_emits_tick_on_start_and_end() {
    let (app, state, dir) = test_app("basic", None);
    let (status, created) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": dir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();
    let mut events_rx = state.events_tx.subscribe();

    let (status, _) = req(
        app,
        "POST",
        &format!("/api/acp/sessions/{sid}/prompt"),
        Some(json!({"text": "hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // 至少两个 Tick（Working 置位 + 收尾回 Idle）；噪声事件（无）全为 Tick
    let mut ticks = 0;
    for _ in 0..2 {
        let ev = tokio::time::timeout(Duration::from_secs(2), events_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            ev,
            server::drivers::claude::watcher::HubEvent::Tick
        ));
        ticks += 1;
    }
    assert!(ticks >= 2);
}
