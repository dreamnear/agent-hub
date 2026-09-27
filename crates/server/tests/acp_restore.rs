//! ACP 会话恢复（批3 任务13）：会话元数据落盘（参照 projects.json 惯例），
//! 重启后按 ACP session/load 恢复——支持（omp 实测支持）→ list 恢复 + feed 重放；
//! 不支持 → 条目剪除，会话按新建呈现。create/release 落盘时机有覆盖。

use std::{path::Path, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use server::{
    api::{acp::restore_persisted_sessions, AppState},
    config::{AcpAgentConfig, AcpConfig, ChatConfig, Config},
    drivers::acp::AcpStatus,
    router,
};
use tower::ServiceExt;

async fn req(
    app: &axum::Router,
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
    let res = app.clone().oneshot(request).await.unwrap();
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

fn fake_agent(mode: &str) -> AcpAgentConfig {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_fake_agent.py");
    AcpAgentConfig {
        name: "fake".into(),
        command: "python3".into(),
        args: vec![
            "-u".into(),
            fixture.to_string_lossy().into_owned(),
            mode.into(),
        ],
        cwd: None,
        model: None,
    }
}

fn app_with_store(
    mode: &str,
    store_file: &Path,
) -> (axum::Router, std::sync::Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::load();
    cfg.acp = AcpConfig {
        agents: vec![fake_agent(mode)],
    };
    cfg.chat = ChatConfig::default();
    cfg.acp_sessions_file = store_file.to_path_buf();
    let state = std::sync::Arc::new(AppState::from_config(cfg));
    (router(state.clone()), state, dir)
}

/// 批3 任务13 出口A：重启（新 AppState）后 restore 经 session/load 恢复——
/// registry 有条目（Idle）、feed 有重放 chunk、agents list 可见。
#[tokio::test]
async fn restore_supported_session_after_restart() {
    let store = tempfile::tempdir().unwrap();
    let store_file = store.path().join("acp_sessions.json");

    // 第一次"运行"：建会话（create 内部落盘）
    let (app, state1, workdir1) = app_with_store("loadable", &store_file);
    let (status, created) = req(
        &app,
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir1.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();
    // persist 是后台任务，轮询等落盘
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if store_file.exists() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "create 后持久化文件应存在"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let persisted: Value =
        serde_json::from_str(&std::fs::read_to_string(&store_file).unwrap()).unwrap();
    assert_eq!(persisted[0]["id"], sid.as_str());

    // 模拟重启：全新 AppState（同 store 文件）→ restore
    drop(state1);
    let (_app2, state2, _workdir2) = app_with_store("loadable", &store_file);
    restore_persisted_sessions(state2.clone()).await;

    let restored = state2.acp_registry.get(&sid).expect("恢复后会话应在注册表");
    assert_eq!(restored.status, AcpStatus::Idle);
    // session/load 重放的 update 回填 feed（历史不假空）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let msgs = state2.acp_registry.get(&sid).unwrap().feed.history();
        if msgs.iter().any(|m| m.text.as_deref() == Some("replayed")) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "重放 chunk 未回填 feed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // list 可见（前端侧栏恢复）
    let (_, body) = req(&_app2, "GET", "/api/agents?all=1", None).await;
    assert!(body
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["driver"] == "acp" && a["id"] == sid.as_str()));

    if let Some(s) = state2.acp_registry.get(&sid) {
        s.conn.shutdown().await;
    }
}

/// 批3 任务13 出口B 兜底：agent 不支持 session/load → 条目剪除（会话按新建呈现），
/// 持久化文件同步清理，不留死条目。
#[tokio::test]
async fn restore_unsupported_prunes_entry() {
    let store = tempfile::tempdir().unwrap();
    let store_file = store.path().join("acp_sessions.json");

    // basic 模式建会话（该 agent 不支持 session/load → 兜底 method-not-found）
    let (app, state1, workdir1) = app_with_store("basic", &store_file);
    let (status, created) = req(
        &app,
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir1.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();
    // persist 是后台任务，轮询等落盘再模拟重启
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        if store_file.exists() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "create 后持久化文件应存在"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(state1);

    let (_app2, state2, _workdir2) = app_with_store("basic", &store_file);
    restore_persisted_sessions(state2.clone()).await;

    assert!(
        state2.acp_registry.get(&sid).is_none(),
        "不支持 load 的条目应被剪除"
    );
    let persisted: Value =
        serde_json::from_str(&std::fs::read_to_string(&store_file).unwrap()).unwrap();
    assert_eq!(
        persisted.as_array().unwrap().len(),
        0,
        "剪除后文件应为空清单"
    );
}

/// 批3 任务13：release（会话移除）同步清持久化条目
#[tokio::test]
async fn release_prunes_persisted_entry() {
    let store = tempfile::tempdir().unwrap();
    let store_file = store.path().join("acp_sessions.json");
    let (app, state, workdir) = app_with_store("basic", &store_file);
    let (status, created) = req(
        &app,
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "fake", "cwd": workdir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();

    let release_result = state
        .message_routes
        .read()
        .expect("routes lock")
        .get("acp")
        .unwrap()
        .release(&sid);
    assert!(release_result.is_ok(), "release 应成功");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let raw = std::fs::read_to_string(&store_file).unwrap_or_default();
        let persisted: Value = serde_json::from_str(&raw).unwrap_or(json!([]));
        if persisted.as_array().is_some_and(|a| a.is_empty()) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "release 后持久化条目应被清除"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
