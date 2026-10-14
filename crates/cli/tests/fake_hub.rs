//! fake hub 回环测试：axum 内存路由起 127.0.0.1:0 端口，客户端打真请求（不起真
//! 固定端口，不碰 7800/7902 任何实机实例）。

use std::sync::{Arc, Mutex};

use axum::{extract::Path as AxumPath, routing::get, routing::post, Json, Router};
use serde_json::json;

async fn spawn_hub(extra: Router<()>) -> String {
    let app = Router::new()
        .route(
            "/api/agents",
            get(|| async {
                Json(vec![json!({
                    "driver": "claude", "id": "aaaaaaaa-1111",
                    "name": "日报", "cwd": "/tmp/work", "kind": null,
                    "rawState": "working", "group": "working",
                    "detail": null, "tokens": 123, "startedAt": 1, "sessionId": null,
                })])
            }),
        )
        .route("/api/drivers", get(|| async { Json(json!(["claude"])) }))
        .merge(extra);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// 捕获 POST message 体的 fake 会话端点（发送形状断言用）。
async fn spawn_hub_with_capture() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let cap = captured.clone();
    let app = Router::new()
        .route(
            "/api/agents/{driver}/{id}/message",
            post(
                |AxumPath((driver, id)): AxumPath<(String, String)>,
                 Json(body): Json<serde_json::Value>| async move {
                    cap.lock().unwrap().push(json!({
                        "driver": driver, "id": id, "body": body,
                    }));
                    axum::http::StatusCode::NO_CONTENT
                },
            ),
        )
        .route(
            "/api/agents/{driver}/{id}/messages/page",
            get(|| async {
                Json(json!({
                    "messages": [
                        {"kind": "user", "text": "历史消息", "toolName": null},
                    ],
                    "firstLine": 0, "hasMore": false,
                }))
            }),
        );
    (spawn_hub(app).await, captured)
}

#[tokio::test]
async fn agents_roundtrip_parses_server_shape() {
    let base = spawn_hub(Router::new()).await;
    let hub = ah::hub(base);
    let agents = hub.agents(false).await.unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name.as_deref(), Some("日报"));
    assert_eq!(agents[0].group.as_deref(), Some("working"));
}

#[tokio::test]
async fn healthy_probes_drivers_endpoint() {
    let base = spawn_hub(Router::new()).await;
    assert!(ah::hub(base).healthy().await);
    assert!(!ah::hub("http://127.0.0.1:1").healthy().await);
}

#[tokio::test]
async fn send_posts_text_body_to_message_endpoint() {
    let (base, captured) = spawn_hub_with_capture().await;
    ah::hub(base)
        .send("claude", "aaaaaaaa-1111", "你好")
        .await
        .unwrap();
    let got = captured.lock().unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0]["driver"], "claude");
    assert_eq!(got[0]["id"], "aaaaaaaa-1111");
    assert_eq!(got[0]["body"]["text"], "你好");
}

#[tokio::test]
async fn page_parses_history_envelope() {
    let (base, _captured) = spawn_hub_with_capture().await;
    let page = ah::hub(base)
        .page("claude", "aaaaaaaa-1111", 30)
        .await
        .unwrap();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].text.as_deref(), Some("历史消息"));
}
