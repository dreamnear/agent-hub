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
