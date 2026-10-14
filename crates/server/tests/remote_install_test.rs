//! 远程探测 + 安装计划 API 集成测试（agent-hub-settings 任务 D2）。
//! Router oneshot 不起真端口；SSH 面用 127.0.0.1:1（连接拒绝，秒回）当不可达靶。

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

async fn app() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.instances_file = dir.path().join("instances.json");
    dir.close().unwrap();
    router(Arc::new(AppState::from_config(cfg)))
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

/// AppError 错误体是纯文本——文案断言走原始文本
async fn req_text(a: axum::Router, method: &str, uri: &str) -> (StatusCode, String) {
    let res = a
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
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

async fn mk_ssh_inst(a: &axum::Router, id: &str, host: &str, remote_port: Option<u16>) {
    let mut inst = json!({
        "id": id,
        "name": "remote",
        "mode": "ssh-tunnel",
        "ssh": { "host": host, "port": 22, "user": "u", "auth": "authsock" }
    });
    if let Some(p) = remote_port {
        inst["remotePort"] = json!(p);
    }
    let (status, _) = req(a.clone(), "POST", "/api/instances", Some(inst)).await;
    assert_eq!(status, StatusCode::OK);
}

async fn mk_direct_inst(a: &axum::Router, id: &str) {
    let (status, _) = req(
        a.clone(),
        "POST",
        "/api/instances",
        Some(json!({
            "id": id, "name": "frp", "mode": "direct",
            "url": "https://hub.example.com", "token": "tok"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

/// 探测端点守卫：direct 实例 / 未知实例 拒绝可辨（缺远程端口的 ssh 实例保存即拒，
/// 探测端不可达该态——handler 仍防御手改配置文件）
#[tokio::test]
async fn probe_guards() {
    let a = app().await;
    mk_direct_inst(&a, "d").await;
    let (status, text) = req_text(a.clone(), "GET", "/api/instances/d/remote-probe").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("仅 ssh-tunnel"), "{text}");

    let (status, _) = req(a, "GET", "/api/instances/ghost/remote-probe", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// SSH 不可达 → 200 + reason=ssh_unreachable（判定在响应体，不用 5xx 表达远端状态）
#[tokio::test]
async fn probe_unreachable_reports_reason() {
    let a = app().await;
    mk_ssh_inst(&a, "dead", "127.0.0.1", Some(1)).await; // 127.0.0.1:1 连接拒绝
    let (status, body) = req(a, "GET", "/api/instances/dead/remote-probe", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["installed"], false);
    assert_eq!(body["reason"], "ssh_unreachable");
    assert!(!body["detail"].as_str().unwrap_or("").is_empty());
}

/// 安装计划：只生成不执行——SSH 不可达的实例照样秒回计划（真 spawn 会挂/失败）；
/// planHash 对同一实例稳定；direct 实例拒计划（无 SSH 通道）
#[tokio::test]
async fn install_plan_generates_without_execution() {
    let a = app().await;
    mk_ssh_inst(&a, "dead", "127.0.0.1", Some(1)).await;

    let (status, p1) = req(a.clone(), "POST", "/api/instances/dead/install-plan", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, p2) = req(a.clone(), "POST", "/api/instances/dead/install-plan", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(p1["planHash"], p2["planHash"], "同实例哈希应稳定");
    assert_ne!(p1["planId"], p2["planId"], "planId 每次新生成");

    let steps = p1["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2);
    assert!(steps[0]["display"].as_str().unwrap().contains("install.sh"));
    assert!(steps[0]["display"]
        .as_str()
        .unwrap()
        .contains("--with-systemd"));
    assert!(steps[0]["display"]
        .as_str()
        .unwrap()
        .contains("RELEASE_BASE_URL="));

    mk_direct_inst(&a, "d").await;
    let (status, text) = req_text(a, "POST", "/api/instances/d/install-plan").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(text.contains("仅 ssh-tunnel"), "{text}");
}

/// install-manual 与 ssh-tunnel 计划同一份命令清单（同源防漂移）
#[tokio::test]
async fn install_manual_matches_plan_steps() {
    let a = app().await;
    // 远程端口 = server 默认 7800 → 与 manual 的清单逐字一致
    mk_ssh_inst(&a, "s", "example.com", Some(7800)).await;
    let (_, plan) = req(a.clone(), "POST", "/api/instances/s/install-plan", None).await;
    let (status, manual) = req(a, "GET", "/api/instances/install-manual", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(plan["steps"], manual["steps"], "两处清单必须同源");
    assert!(manual["planHash"].is_string());
}

// ===== D3：执行端点（confirm / planHash 校验；SSH 靶不可达 → 失败分类 200）=====

/// confirm=false → 400 拒绝且不执行（SSH 靶是连接拒绝的死地址，若真执行会走
/// 失败 200 路径而非 400——状态码本身证明拒绝发生在任何命令之前）
#[tokio::test]
async fn install_rejects_missing_confirm() {
    let a = app().await;
    mk_ssh_inst(&a, "dead", "127.0.0.1", Some(1)).await;
    let (_, plan) = req(a.clone(), "POST", "/api/instances/dead/install-plan", None).await;
    let res = a
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances/dead/install")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"planId": plan["planId"], "planHash": plan["planHash"], "confirm": false})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("confirm"), "{text}");
}

/// planHash 不匹配 → 400（确认的清单 ≠ 将执行的清单，拒绝执行）
#[tokio::test]
async fn install_rejects_hash_mismatch() {
    let a = app().await;
    mk_ssh_inst(&a, "dead", "127.0.0.1", Some(1)).await;
    let (_, plan) = req(a.clone(), "POST", "/api/instances/dead/install-plan", None).await;
    let (status, body) = req(
        a,
        "POST",
        "/api/instances/dead/install",
        Some(json!({"planId": plan["planId"], "planHash": "deadbeefdeadbeef", "confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].is_null(), "错误体是纯文本: {body}");
}

/// planId 未生成过 → 400；SSH 不可达的真执行 → 200 + ok=false + 失败分类
#[tokio::test]
async fn install_unknown_plan_then_real_run_fails_classified() {
    let a = app().await;
    mk_ssh_inst(&a, "dead", "127.0.0.1", Some(1)).await;
    let (status, body) = req(
        a.clone(),
        "POST",
        "/api/instances/dead/install",
        Some(json!({"planId": "ghost", "planHash": "x", "confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.is_null() || body.as_str().is_some(),
        "纯文本错误: {body}"
    );

    // 真执行（127.0.0.1:1 连接拒绝，秒回）：200 + ok=false（远端状态不用 5xx）
    let (_, plan) = req(a.clone(), "POST", "/api/instances/dead/install-plan", None).await;
    let (status, body) = req(
        a,
        "POST",
        "/api/instances/dead/install",
        Some(json!({"planId": plan["planId"], "planHash": plan["planHash"], "confirm": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false);
    assert!(!body["error"].as_str().unwrap_or("").is_empty(), "{body}");
}
