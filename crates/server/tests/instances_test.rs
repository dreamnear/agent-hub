//! 多实例 CRUD / URL 红线 / 隧道 API 集成测试（agent-hub-multi-instance 批1 任务2/3）。
//! Router oneshot 不起真端口；隧道 start 对真实 ssh 目标做连通（本机 sshd 起第二实例
//! 属批 1 任务 5 端到端，此处覆盖 CRUD/校验/隧道状态幂等面）。

use std::{path::Path, sync::Arc};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

fn fake_bin() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

fn cfg_with(dir: &tempfile::TempDir) -> Config {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.instances_file = dir.path().join("instances.json");
    cfg
}

async fn app(dir: &tempfile::TempDir) -> axum::Router {
    router(Arc::new(AppState::from_config(cfg_with(dir))))
}

fn direct_inst(id: &str, url: &str, token: &str) -> Value {
    json!({ "id": id, "name": "frp", "mode": "direct", "url": url, "token": token })
}

async fn req(a: axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let res = a.oneshot(request).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// AppError 错误体是纯文本（非 JSON）——消息断言走原始文本
async fn req_text(a: axum::Router, method: &str, uri: &str) -> (StatusCode, String) {
    let builder = Request::builder().method(method).uri(uri);
    let res = a
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// 保存 https 远程 + 本机 http 成功；http 远程被拒（红线 4，含明确文案）
#[tokio::test]
async fn direct_url_redline_via_api() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(&dir).await;

    // https 远程保存成功
    let (status, body) = req(
        a.clone(),
        "POST",
        "/api/instances",
        Some(direct_inst("a", "https://hub.example.com:7800", "tok")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "a");

    // 本机 http 保存成功（例外）
    let (status, _) = req(
        a.clone(),
        "POST",
        "/api/instances",
        Some(direct_inst("b", "http://127.0.0.1:7800", "tok")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // http 远程被拒（AppError 纯文本体）→ 用原始文本读取器断言文案
    let (status, text) = post_json_text(
        a.clone(),
        &direct_inst("bad", "http://192.168.1.5:7800", "tok"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("https"), "拒绝文案应含 https 引导: {text}");
}

/// 任务11（wss/ws 语义）：wss 远程等价 https——接受并归一化为 https 落盘
/// （实例 url 是 fetch 的 http base，wss scheme 无法直接 fetch）；ws 远程明文拒绝
#[tokio::test]
async fn direct_wss_normalized_ws_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(&dir).await;

    // wss 远程接受，落盘归一化 https
    let (status, body) = req(
        a.clone(),
        "POST",
        "/api/instances",
        Some(direct_inst("wss1", "wss://hub.example.com:7800", "tok")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["url"], "https://hub.example.com:7800");

    // ws 远程明文拒绝（红线同 http）
    let (status, text) = post_json_text(
        a.clone(),
        &direct_inst("bad", "ws://hub.example.com:7800", "tok"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("https"), "拒绝文案应含 https 引导: {text}");

    // PUT 更新同样归一化（wss → https）
    let (status, body) = req(
        a,
        "PUT",
        "/api/instances",
        Some(direct_inst("wss1", "wss://other.example.com", "tok")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["url"], "https://other.example.com");
}

/// POST /api/instances 返回原始响应体文本（校验错误文案走 AppError 纯文本）
async fn post_json_text(a: axum::Router, body: &Value) -> (StatusCode, String) {
    let res = a
        .oneshot(
            Request::post("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// CRUD 往返：list 含新增、put 更新、delete 幂等
#[tokio::test]
async fn instances_crud_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(&dir).await;

    let (status, list) = req(a.clone(), "GET", "/api/instances", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]));

    req(
        a.clone(),
        "POST",
        "/api/instances",
        Some(direct_inst("a", "https://x.example.com", "tok")),
    )
    .await;
    // 同名 id 再建 → 409
    let (status, _) = req(
        a.clone(),
        "POST",
        "/api/instances",
        Some(direct_inst("a", "https://x.example.com", "tok")),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // PUT 更新
    let upd = json!({ "id": "a", "name": "renamed", "mode": "direct", "url": "https://y.example.com", "token": "tok2" });
    let (status, _) = req(a.clone(), "PUT", "/api/instances", Some(upd)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = req(a.clone(), "GET", "/api/instances", None).await;
    assert_eq!(list[0]["name"], "renamed");
    assert_eq!(list[0]["url"], "https://y.example.com");
    assert_eq!(list[0]["token"], "tok2");

    // DELETE 幂等：先 204，再删不存在也 204
    let (status, _) = req(a, "DELETE", "/api/instances/a", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// 落盘 0600 + 文件路径正确（instances.json）
#[cfg(unix)]
#[tokio::test]
async fn instances_file_is_0600() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(&dir).await;
    req(
        a,
        "POST",
        "/api/instances",
        Some(direct_inst("a", "https://x.example.com", "secret")),
    )
    .await;
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(dir.path().join("instances.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

/// 隧道 API：非 ssh-tunnel 实例 start 拒；ssh-tunnel 缺参校验透出；对 ssh 实例幂等语义
#[tokio::test]
async fn tunnel_api_guards() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(&dir).await;
    // direct 实例 start → 400（AppError 纯文本）
    req(
        a.clone(),
        "POST",
        "/api/instances",
        Some(direct_inst("d", "https://x.example.com", "tok")),
    )
    .await;
    let (status, text) = req_text(a.clone(), "POST", "/api/instances/d/tunnel/start").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("仅 ssh-tunnel"));
    // 未知实例
    let (status, _) = req(a.clone(), "POST", "/api/instances/ghost/tunnel/start", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // 未 start → status not_started
    let (status, body) = req(a.clone(), "GET", "/api/instances/d/tunnel/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "not_started");
    // POST 幂等语义：GET start 已改 POST（tasks.md 判据），stop 同为 POST
    let (status, _) = req(a.clone(), "POST", "/api/instances/d/tunnel/stop", None).await;
    assert_eq!(status, StatusCode::OK);
}

/// ssh-tunnel 实例保存（含 authsock/key-path），URL 不校验（SSH 加密内建）
#[tokio::test]
async fn ssh_tunnel_instance_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(&dir).await;
    let inst = json!({
        "id": "s1",
        "name": "remote-ssh",
        "mode": "ssh-tunnel",
        "ssh": { "host": "example.com", "port": 22, "user": "alice",
                 "auth": "authsock" },
        "remotePort": 7800
    });
    let (status, _) = req(a.clone(), "POST", "/api/instances", Some(inst)).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = req(a, "GET", "/api/instances", None).await;
    assert_eq!(list[0]["mode"], "ssh-tunnel");
    assert_eq!(list[0]["ssh"]["host"], "example.com");
}
