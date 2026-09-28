//! harness 探测/加入配置 API 集成测试（agent-hub-settings 批A 需求5/6）。
//! 合并单测试顺序执行：`AGENT_HUB_CONFIG` 是 env 全局态，并行会互相串配置路径。

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{json, Value};
use server::{api::AppState, config::Config, router};
use tower::ServiceExt;

async fn req(a: axum::Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().method(method).uri(uri);
    let request = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let res = a.oneshot(request).await.expect("oneshot");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, v)
}

fn exec_bit(p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

#[tokio::test]
async fn harness_list_marks_dead_and_add_persists_to_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_file = dir.path().join("config.toml");
    // 预置一条自定义 agent（无 omp）→ 内置默认不生效，omp 视为未配置可加入
    std::fs::write(
        &config_file,
        "[[acp.agents]]\nname = \"x\"\ncommand = \"/bin/true\"\nargs = []\n",
    )
    .unwrap();
    std::env::set_var("AGENT_HUB_CONFIG", &config_file);

    let app = router(Arc::new(AppState::from_config(Config::load())));

    // 场景 1：GET 列出已知清单全量（失效项 alive:false，默认过滤在前端）
    let (status, body) = req(app.clone(), "GET", "/api/harness", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = body["harnesses"].as_array().expect("harnesses array");
    assert!(list.len() >= 4, "已知清单应全量返回: {body}");
    let omp = list.iter().find(|h| h["name"] == "omp").expect("omp");
    assert_eq!(omp["kind"], "acp");
    assert_eq!(omp["configured"], json!(false), "预置清单无 omp: {body}");

    // 场景 2：未知 name / CLI 类 / 死路径 → 拒绝，不落盘
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/harness/nope",
        Some(json!({ "path": "/bin/sh" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/harness/claude",
        Some(json!({ "path": "/bin/sh" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}"); // CLI 类不可加入
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/harness/omp",
        Some(json!({ "path": "/nope/omp" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let raw = std::fs::read_to_string(&config_file).unwrap();
    assert!(
        !raw.contains("claude") && !raw.contains("/nope"),
        "被拒的加入不得落盘: {raw}"
    );

    // 场景 3：可执行路径 → 写盘 + 运行时清单即时生效（不重启）
    let fake = dir.path().join("omp");
    std::fs::write(&fake, "#!/bin/sh\necho omp/18.0.11\n").unwrap();
    exec_bit(&fake);
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/harness/omp",
        Some(json!({ "path": fake.to_str().unwrap() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["added"], json!(true));
    let raw = std::fs::read_to_string(&config_file).unwrap();
    assert!(raw.contains("[[acp.agents]]"), "{raw}");
    assert!(raw.contains("omp"), "{raw}");

    let (_, acp) = req(app, "GET", "/api/acp/agents", None).await;
    let names: Vec<&str> = acp["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["x", "omp"],
        "加入后应即时出现在 agent 清单: {acp}"
    );

    std::env::remove_var("AGENT_HUB_CONFIG");
}
