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
    let data = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, data)
}

fn test_app(dir: &tempfile::TempDir) -> axum::Router {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.claude_root = dir.path().to_path_buf();
    // 固定 chat 分页默认值：防开发机真实 ~/.claude-view/config.toml 影响断言
    cfg.chat = server::config::ChatConfig::default();
    router(Arc::new(AppState::from_config(cfg)))
}

/// 预置 fixture 会话：cwd=/Users/demo/proj-alpha（agents.json 首条）→ slug 目录 + sessionId jsonl
fn seed_session(dir: &tempfile::TempDir) -> PathBuf {
    let slug_dir = dir.path().join("projects").join("-Users-demo-proj-alpha");
    std::fs::create_dir_all(&slug_dir).unwrap();
    let jsonl = slug_dir.join("3f9a2b7c-1111-2222-3333-444455556666.jsonl");
    std::fs::write(
        &jsonl,
        concat!(
            r#"{"type":"user","timestamp":"2026-09-16T08:00:00.000Z","message":{"role":"user","content":"first"}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-09-16T08:00:06.000Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_01","name":"Bash","input":{"command":"cargo build"}}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    jsonl
}

#[tokio::test]
async fn get_messages_returns_history_with_tool_use() {
    let dir = tempfile::tempdir().unwrap();
    seed_session(&dir);
    let (status, body) = req(
        test_app(&dir),
        "GET",
        "/api/agents/claude/a1b2c3d4/messages",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().expect("应返回数组");
    assert!(arr.len() >= 2);
    assert!(
        arr.iter().any(|m| m["kind"] == "tool_use"),
        "应含 kind:tool_use 结构，got: {body}"
    );
}

#[tokio::test]
async fn post_message_writes_attach_stdin() {
    let dir = tempfile::tempdir().unwrap();
    seed_session(&dir);
    let (status, _) = req(
        test_app(&dir),
        "POST",
        "/api/agents/claude/a1b2c3d4/message",
        Some(json!({"text": "hello via hub"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // fake-claude attach 的 cat 从 PTY 消费有延迟
    let f = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/attach-stdin-a1b2c3d4.txt");
    let mut content = String::new();
    for _ in 0..10 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if let Ok(c) = std::fs::read_to_string(&f) {
            content = c;
            if content.contains("hello via hub") {
                break;
            }
        }
    }
    assert!(
        content.contains("hello via hub"),
        "attach stdin 文件应含透传文本，got: {content:?}"
    );
    std::fs::remove_file(&f).ok();
}

#[tokio::test]
async fn post_message_empty_text_is_400() {
    let dir = tempfile::tempdir().unwrap();
    seed_session(&dir);
    let (status, _) = req(
        test_app(&dir),
        "POST",
        "/api/agents/claude/a1b2c3d4/message",
        Some(json!({"text": "  "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// P6 B12/B13：分页端点 envelope——末页 limit、before 游标向前翻页、hasMore 拉到头
#[tokio::test]
async fn messages_page_returns_envelope_with_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let slug_dir = dir.path().join("projects").join("-Users-demo-proj-alpha");
    std::fs::create_dir_all(&slug_dir).unwrap();
    let mut content = String::new();
    for i in 0..30 {
        content.push_str(&format!(
            r#"{{"type":"user","timestamp":"2026-09-16T08:00:{i:02}.000Z","message":{{"role":"user","content":"m{i}"}}}}"#,
        ));
        content.push('\n');
    }
    std::fs::write(
        slug_dir.join("3f9a2b7c-1111-2222-3333-444455556666.jsonl"),
        content,
    )
    .unwrap();

    // 末页：默认 limit（cfg 默认 20）→ 最新 20 条 + 游标
    let (status, body) = req(
        test_app(&dir),
        "GET",
        "/api/agents/claude/a1b2c3d4/messages/page",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["messages"].as_array().unwrap().len(), 20);
    assert_eq!(body["messages"][0]["text"], "m10");
    assert_eq!(body["messages"][19]["text"], "m29");
    assert_eq!(body["firstLine"], 10);
    assert_eq!(body["hasMore"], true);

    // before 游标向前翻页（limit 参数生效）
    let (status, body) = req(
        test_app(&dir),
        "GET",
        "/api/agents/claude/a1b2c3d4/messages/page?limit=5&before=10",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["messages"][0]["text"], "m5");
    assert_eq!(body["firstLine"], 5);
    assert_eq!(body["hasMore"], true);

    // 翻到头：before=0 → 空页 hasMore=false（前端显示「无更多消息」）
    let (status, body) = req(
        test_app(&dir),
        "GET",
        "/api/agents/claude/a1b2c3d4/messages/page?before=0",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["messages"].as_array().unwrap().len(), 0);
    assert_eq!(body["hasMore"], false);
}

/// P6 B14：chat 分页配置端点（前端缓冲淘汰阈值来源）
#[tokio::test]
async fn chat_config_returns_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(test_app(&dir), "GET", "/api/config/chat", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pageSize"], 20);
    assert_eq!(body["bufferMax"], 100);
}
