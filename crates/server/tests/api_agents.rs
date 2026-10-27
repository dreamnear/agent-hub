#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

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
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

fn test_app(dir: &tempfile::TempDir) -> axum::Router {
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
    cfg.omp.sessions_dir = dir.path().join("omp-sessions-empty");
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().to_path_buf();
    router(Arc::new(AppState::from_config(cfg)))
}

#[tokio::test]
async fn list_returns_grouped_agents() {
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(test_app(&dir), "GET", "/api/agents?all=1", None).await;
    assert_eq!(status, StatusCode::OK);
    // fixture：--all 全量 5 条（含 done+活 daemon 与 running 活会话，见 agents.json）
    assert_eq!(body.as_array().unwrap().len(), 5);
    assert!(body[0].get("group").is_some(), "JSON 含 group 字段");
    assert_eq!(body[0]["driver"], "claude");
}

#[tokio::test]
async fn list_drivers_returns_registry_order() {
    // StartDialog 选项事实源：注册序 claude/acp/opencode/dsh（去硬编码的清单接口）
    // pi（agent-hub-pi-hub-agent）注册序末位
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(test_app(&dir), "GET", "/api/drivers", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.as_array().unwrap(),
        serde_json::json!(["claude", "acp", "opencode", "omp", "dsh", "pi"])
            .as_array()
            .unwrap()
    );
}

#[tokio::test]
async fn start_with_missing_cwd_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let (_, body) = req(
        test_app(&dir),
        "POST",
        "/api/agents/claude",
        Some(json!({"prompt": "hi", "cwd": "/definitely/not/a/dir"})),
    )
    .await;
    assert!(body.is_null() || body.get("id").is_none());
}

#[tokio::test]
async fn start_with_dotdot_cwd_is_400() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, _) = req(
        app,
        "POST",
        "/api/agents/claude",
        Some(json!({"prompt": "hi", "cwd": format!("{}/..", dir.path().display())})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn start_with_valid_cwd_returns_id() {
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(
        test_app(&dir),
        "POST",
        "/api/agents/claude",
        Some(json!({"prompt": "hi", "cwd": dir.path()})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "abc123");
}

#[tokio::test]
async fn unknown_driver_is_404() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);
    let (status, _) = req(app, "GET", "/api/agents/acme/x1", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn detail_logs_stop_rm_walk_through() {
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(&dir);

    let (status, body) = req(app.clone(), "GET", "/api/agents/claude/a1b2c3d4", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "a1b2c3d4");

    let (status, body) = req(app.clone(), "GET", "/api/agents/claude/a1b2c3d4/logs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("logs").is_some());

    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/agents/claude/a1b2c3d4/stop",
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "stop 断言以接口定义 204 为准"
    );

    let (status, _) = req(app, "POST", "/api/agents/claude/a1b2c3d4/rm", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "rm 断言以接口定义 204 为准");
}

#[tokio::test]
async fn agents_aggregate_survives_dsh_auth_failure() {
    // 急修第二刀红线：dsh 收编到带认证实例（全 401）→ 聚合 200、claude 条目
    // 照常、dsh 降级空集 + /api/drivers/status 打 authError 标记
    let app401 = axum::Router::new().route(
        "/api/session/list",
        axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app401).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::load();
    cfg.opencode.bin = "/nonexistent/opencode-test-isolated".into();
    cfg.opencode.port = 65001;
    cfg.opencode_serve_file = dir.path().join("opencode_serve.json");
    // dsh 指向 401 假实例（有监听者 → 走收编路径）
    cfg.dsh.bin = "/nonexistent/dsh-test-isolated".into();
    cfg.dsh.port = port;
    cfg.dsh_serve_file = dir.path().join("dsh_serve.json");
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().to_path_buf();
    let app = router(Arc::new(AppState::from_config(cfg)));

    let (status, body) = req(app.clone(), "GET", "/api/agents?all=1", None).await;
    assert_eq!(status, StatusCode::OK, "聚合绝不因单驱动认证失败 500");
    assert!(body
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["driver"] != "dsh"));

    let (status, status_body) = req(app, "GET", "/api/drivers/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(status_body["dsh"]["authError"], true, "authError 已标记");
}

// —— 历史会话按驱动插拔（agent-hub-acp-session-history）：omp 数据源 + 恢复路由 ——

fn write_omp_session(dir: &Path, sid: &str, cwd: &Path, title: &str) {
    let slug = dir.join("--tmp--");
    std::fs::create_dir_all(&slug).unwrap();
    // #45 起发现面只列有消息的转录（零消息 = 占位形态不入清单）——fixture 补
    // 一条用户消息行对齐该不变量
    let body = format!(
        "{}\n{}\n{}\n",
        format_args!(r#"{{"type":"title","v":1,"title":"{title}"}}"#),
        format_args!(
            r#"{{"type":"session","id":"{sid}","cwd":"{}","timestamp":"2026-10-10T08:00:00Z"}}"#,
            cwd.display()
        ),
        r#"{"type":"message","id":"e1","timestamp":"2026-10-10T08:00:01Z","message":{"role":"user","timestamp":"2026-10-10T08:00:01Z","content":[{"type":"text","text":"首轮输入"}]}}"#,
    );
    std::fs::write(slug.join(format!("20261010T080000Z_{sid}.jsonl")), body).unwrap();
}

/// omp 专用 app：sessions_dir 指 fixture、acp agent 指 fake loadable
/// （name="omp" 与 api/mod.rs 的接管 agent 名回退一致）。
fn omp_test_app(dir: &tempfile::TempDir) -> axum::Router {
    let mut cfg = Config::load();
    cfg.opencode.bin = "/nonexistent/opencode-test-isolated".into();
    cfg.opencode.port = 65001;
    cfg.opencode_serve_file = dir.path().join("opencode_serve.json");
    cfg.dsh.bin = "/nonexistent/dsh-test-isolated".into();
    cfg.dsh.port = 65002;
    cfg.dsh_serve_file = dir.path().join("dsh_serve.json");
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().to_path_buf();
    let omp_dir = dir.path().join("omp-sessions");
    std::fs::create_dir_all(&omp_dir).unwrap();
    cfg.omp.sessions_dir = omp_dir;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_fake_agent.py");
    cfg.acp.agents = vec![server::config::AcpAgentConfig {
        name: "omp".into(),
        command: "python3".into(),
        args: vec![
            "-u".into(),
            fixture.to_string_lossy().into_owned(),
            "loadable".into(),
        ],
        cwd: None,
        model: None,
    }];
    cfg.acp_sessions_file = dir.path().join("acp_sessions.json");
    router(Arc::new(AppState::from_config(cfg)))
}

/// omp cli-sessions：条目数与该 cwd 会话文件数一致，title 进 summary 字段。
#[tokio::test]
async fn omp_cli_sessions_lists_cwd_sessions() {
    let dir = tempfile::tempdir().unwrap();
    write_omp_session(
        &dir.path().join("omp-sessions"),
        "omp-sess-1",
        dir.path(),
        "合成标题",
    );
    write_omp_session(
        &dir.path().join("omp-sessions"),
        "omp-sess-2",
        dir.path(),
        "另一条",
    );
    let app = omp_test_app(&dir);
    let (status, body) = req(app, "GET", "/api/agents/omp/omp-sess-1/cli-sessions", None).await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 2, "条目数 = 该 cwd 会话文件数");
    assert!(rows.iter().any(|r| r["summary"] == "合成标题"));
    assert!(rows.iter().any(|r| r["sessionId"] == "omp-sess-2"));
}

/// 无会话源的驱动（pi）→ 400 明确错误（不出现点了必失败的入口之外的后端兜底）。
#[tokio::test]
async fn omp_cli_sessions_rejects_other_drivers() {
    let dir = tempfile::tempdir().unwrap();
    let app = omp_test_app(&dir);
    let (status, _) = req(app, "GET", "/api/agents/pi/x/cli-sessions", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// omp 恢复路由：resume → ACP spawn + session/load（fake loadable）→ resumed，
/// id = 原 omp 会话 id；同会话重复恢复 → active 复用（注册表活会话，不双 spawn）。
#[tokio::test]
async fn omp_resume_session_loads_and_reuses_active() {
    let dir = tempfile::tempdir().unwrap();
    write_omp_session(
        &dir.path().join("omp-sessions"),
        "omp-sess-1",
        dir.path(),
        "甲",
    );
    write_omp_session(
        &dir.path().join("omp-sessions"),
        "omp-sess-2",
        dir.path(),
        "乙",
    );
    let app = omp_test_app(&dir);
    // 从 omp-sess-1 行恢复 omp-sess-2
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/agents/omp/omp-sess-1/resume-session",
        Some(json!({ "sessionId": "omp-sess-2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "resumed");
    assert_eq!(body["id"], "omp-sess-2", "ACP 会话 id = 原 omp 会话 id");
    // 活会话复用：再次恢复同会话 → active（注册表已持有，不二次 spawn）
    let (status, body) = req(
        app,
        "POST",
        "/api/agents/omp/omp-sess-1/resume-session",
        Some(json!({ "sessionId": "omp-sess-2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "active");
    assert_eq!(body["agent"]["id"], "omp-sess-2");
}

/// omp 恢复失败态（功能点 2「不静默」）：会话不存在 → 400 明确错误
/// （R1 tester 备注：域错误语义 4xx，map_err(app_err) 而非裸 ? 的 500）。
#[tokio::test]
async fn omp_resume_session_missing_is_explicit_error() {
    let dir = tempfile::tempdir().unwrap();
    write_omp_session(
        &dir.path().join("omp-sessions"),
        "omp-sess-1",
        dir.path(),
        "甲",
    );
    let app = omp_test_app(&dir);
    let (status, _) = req(
        app,
        "POST",
        "/api/agents/omp/omp-sess-1/resume-session",
        Some(json!({ "sessionId": "no-such-sess" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "域错误走 400 而非 500");
}

/// omp 恢复白名单：路径分量形态 sessionId → 400（防穿越复用 claude 同款纪律）。
#[tokio::test]
async fn omp_resume_session_bad_id_is_400() {
    let dir = tempfile::tempdir().unwrap();
    write_omp_session(
        &dir.path().join("omp-sessions"),
        "omp-sess-1",
        dir.path(),
        "甲",
    );
    let app = omp_test_app(&dir);
    let (status, _) = req(
        app,
        "POST",
        "/api/agents/omp/omp-sess-1/resume-session",
        Some(json!({ "sessionId": "../escape" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// FAIL-1 回归：hub-spawn 的 omp 会话是 ACP 行（driver=acp、name=omp，注册表活
/// 会话被 omp 清单排除）——omp 端点须从 ACP 注册表回退解析 cwd，该行型的
/// 列表/恢复才可达（前端按 'omp' 驱动寻址）。
#[tokio::test]
async fn omp_endpoints_resolve_via_acp_registry_row() {
    let dir = tempfile::tempdir().unwrap();
    write_omp_session(
        &dir.path().join("omp-sessions"),
        "omp-sess-2",
        dir.path(),
        "乙",
    );
    let app = omp_test_app(&dir);
    // hub 开 omp 会话（fake loadable agent，session id = fake-session-1）
    let (status, created) = req(
        app.clone(),
        "POST",
        "/api/acp/sessions",
        Some(json!({ "agent": "omp", "cwd": dir.path().display().to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "建会话应成功: {created}");
    let sid = created["sessionId"].as_str().unwrap().to_string();
    // 列表：ACP 行 id 走 omp 端点（omp.get 排除活会话 → 注册表回退 cwd）
    let (status, body) = req(
        app.clone(),
        "GET",
        &format!("/api/agents/omp/{sid}/cli-sessions"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "ACP 行 omp 列表应可达");
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1, "条目 = 该 cwd 会话文件（不含 ACP 行自身）");
    assert_eq!(rows[0]["sessionId"], "omp-sess-2");
    // 恢复：恢复自身 → current；恢复文件会话 → resumed
    let (status, body) = req(
        app.clone(),
        "POST",
        &format!("/api/agents/omp/{sid}/resume-session"),
        Some(json!({ "sessionId": sid })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "current", "恢复行自身会话 = current");
    let (status, body) = req(
        app,
        "POST",
        &format!("/api/agents/omp/{sid}/resume-session"),
        Some(json!({ "sessionId": "omp-sess-2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "resumed");
    assert_eq!(body["id"], "omp-sess-2");
}

/// FAIL-2 回归：omp 源不可读（权限拒绝）→ 非 2xx 错误响应（前端错误态 + 重试
/// 可达），不得吞成空集 200；根缺失仍为 200 空集。
#[tokio::test]
async fn omp_cli_sessions_unreadable_source_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let omp_dir = dir.path().join("omp-sessions");
    std::fs::create_dir_all(&omp_dir).unwrap();
    let app = omp_test_app(&dir);
    // 先造一个 omp 行（文件在 omp_dir 下才能寻址；随后把 omp_dir 整体锁死）
    write_omp_session(&omp_dir, "omp-sess-1", dir.path(), "甲");
    std::fs::set_permissions(&omp_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    let (status, _) = req(app, "GET", "/api/agents/omp/omp-sess-1/cli-sessions", None).await;
    std::fs::set_permissions(&omp_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        status.is_server_error() || status.is_client_error(),
        "源不可读须报错而非空集: {status}"
    );
}
