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
    app_with_store_named(mode, "fake", store_file)
}

/// 变体：agent 名可指定（#45 占位剪除测试用 "omp" 名命中 omp 驱动的接管 agent 推导）
fn app_with_store_named(
    mode: &str,
    agent_name: &str,
    store_file: &Path,
) -> (axum::Router, std::sync::Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::load();
    // 隔离 opencode 驱动（可选驱动不进断言口径）：三处全封——
    // bin 指空防 spawn 真 serve；port 指死口防「认领健康端口」路径吃到本机
    // 残留 serve（实测二次污染源）；serve 状态文件指 tempdir 防读写真实状态。
    cfg.opencode.bin = "/nonexistent/opencode-test-isolated".into();
    cfg.opencode.port = 65001;
    cfg.opencode_serve_file = dir.path().join("opencode_serve.json");
    // dsh 同款三封（可选驱动不参与聚合断言）
    cfg.dsh.bin = "/nonexistent/dsh-test-isolated".into();
    cfg.dsh.port = 65002;
    cfg.dsh_serve_file = dir.path().join("dsh_serve.json");
    // omp sessions 目录指 tempdir（防读写真实 ~/.omp/agent/sessions——占位剪除
    // 探测与发现面过滤的确定性前提）
    cfg.omp.sessions_dir = dir.path().join("omp-sessions");
    let mut agent = fake_agent(mode);
    agent.name = agent_name.into();
    cfg.acp = AcpConfig {
        agents: vec![agent],
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

// —— #45 移除通道 + 占位剪除 ——

/// #45 移除通道：agents REST 的 /rm 对 acp 键可用（此前 unsupported → 404）——
/// 删除注册表条目 + 子进程收尸 + 持久化条目同步清除（重启不复活）。
#[tokio::test]
async fn remove_acp_row_via_rest_prunes_registry_and_store() {
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
    // 轮询等 persist 后台任务落盘（条目已在文件 = 移除路径有东西可清）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let raw = std::fs::read_to_string(&store_file).unwrap_or_default();
        if raw.contains(&sid) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "create 后持久化条目应存在"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (status, _) = req(&app, "POST", &format!("/api/agents/acp/{sid}/rm"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "acp 行 /rm 应可用");
    assert!(
        state.acp_registry.get(&sid).is_none(),
        "移除后注册表条目应删除"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let raw = std::fs::read_to_string(&store_file).unwrap_or_default();
        let persisted: Value = serde_json::from_str(&raw).unwrap_or(json!([]));
        if persisted.as_array().is_some_and(|a| a.is_empty()) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "移除后持久化条目应同步清除"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // 收尸：fake agent 子进程（release 内 spawn 的 shutdown 任务）跑完
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// 合成 omp 转录 fixture（with_message=false = 仅状态行零消息的占位形态）
fn write_omp_transcript(sessions_dir: &Path, sid: &str, with_message: bool) {
    let slug = sessions_dir.join("--tmp--");
    std::fs::create_dir_all(&slug).unwrap();
    let message = if with_message {
        r#"{"type":"message","id":"e1","timestamp":"2026-10-03T00:00:02Z","message":{"role":"user","timestamp":"2026-10-03T00:00:02Z","content":[{"type":"text","text":"真实对话"}]}}"#.to_string()
    } else {
        // hub 拉起的 acp 驱动进程写入的状态行形态（0 用户消息）
        r#"{"type":"custom","customType":"callbacks-state","data":{"callbacks":[]},"id":"dcda"}"#
            .to_string()
    };
    let body = format!(
        "{{\"type\":\"title\",\"v\":1,\"title\":\"\",\"pad\":\"    \"}}\n{{\"type\":\"session\",\"id\":\"{sid}\",\"cwd\":\"/tmp\",\"timestamp\":\"2026-10-03T00:00:00Z\"}}\n{message}\n",
    );
    std::fs::write(
        slug.join(format!("20261003T000000Z_{sid}.jsonl")),
        body,
    )
    .unwrap();
}

/// #45 占位剪除：omp 驱动条目 + 零消息转录 → 恢复期剪除（不 spawn 驱动进程 =
/// 无占位行、无转录追加），持久化条目同步清除。agent 用 loadable 模式（session/load
/// 可成功）——保证剪除来自占位判定本身，而非 load 兜底剪除。
#[tokio::test]
async fn restore_prunes_zero_message_omp_placeholder() {
    let store = tempfile::tempdir().unwrap();
    let store_file = store.path().join("acp_sessions.json");
    let (app, state1, workdir1) = app_with_store_named("loadable", "omp", &store_file);
    let (status, created) = req(
        &app,
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "omp", "cwd": workdir1.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();
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

    // 重启态：转录文件只有驱动状态行（零消息）→ restore 必须剪除。
    // fixture 落在 state2 的 sessions_dir（新 AppState 的 tempdir 路径）
    let (_app2, state2, workdir2) = app_with_store_named("loadable", "omp", &store_file);
    write_omp_transcript(&workdir2.path().join("omp-sessions"), &sid, false);
    restore_persisted_sessions(state2.clone()).await;

    assert!(
        state2.acp_registry.get(&sid).is_none(),
        "零消息占位条目不得恢复进注册表"
    );
    let persisted: Value =
        serde_json::from_str(&std::fs::read_to_string(&store_file).unwrap()).unwrap();
    assert_eq!(
        persisted.as_array().unwrap().len(),
        0,
        "占位条目应从持久化文件剪除（重启不复活）"
    );
}

/// #45 占位剪除对照：omp 条目转录有消息 → 照常恢复（真实会话重启可续）。
#[tokio::test]
async fn restore_keeps_omp_entry_with_transcript_messages() {
    let store = tempfile::tempdir().unwrap();
    let store_file = store.path().join("acp_sessions.json");
    let (app, state1, workdir1) = app_with_store_named("loadable", "omp", &store_file);
    let (status, created) = req(
        &app,
        "POST",
        "/api/acp/sessions",
        Some(json!({"agent": "omp", "cwd": workdir1.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sid = created["sessionId"].as_str().unwrap().to_string();
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

    // 转录有用户消息（真实会话）→ 恢复路径不剪除（fixture 落 state2 的 sessions_dir）
    let (_app2, state2, workdir2) = app_with_store_named("loadable", "omp", &store_file);
    write_omp_transcript(&workdir2.path().join("omp-sessions"), &sid, true);
    restore_persisted_sessions(state2.clone()).await;

    assert!(
        state2.acp_registry.get(&sid).is_some(),
        "有消息的真实会话应照常恢复"
    );
    if let Some(s) = state2.acp_registry.get(&sid) {
        s.conn.shutdown().await;
    }
}
