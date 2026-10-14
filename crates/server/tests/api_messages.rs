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

/// stdin 注入日志的确定性等待（CI flake 修）：POST 返回「已排队」后注入是
/// 异步落盘，即时 read 在负载窗口会读到空（run3 实锚 set_thinking… 518 行）。
/// 轮询等期望副作用到达（5s deadline，50ms 步长）——不到即失败，非 sleep 掩盖。
fn wait_stdin_contains(log: &Path, needle: &str) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let content = std::fs::read_to_string(log).unwrap_or_default();
        if content.contains(needle) {
            return content;
        }
        if std::time::Instant::now() >= deadline {
            return content;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
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
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
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

/// subagent 清单端点（2026-10-01 定案）：默认只回活跃；`?finished=1` 与
/// `?finished=true` 均显式 opt-in 全量（返工教训：axum typed bool 不收 `1`）。
#[tokio::test]
async fn subagents_endpoint_defaults_active_and_accepts_finished_param_forms() {
    use std::time::{Duration, SystemTime};

    let dir = tempfile::tempdir().unwrap();
    seed_session(&dir);
    let sub = dir
        .path()
        .join("projects")
        .join("-Users-demo-proj-alpha")
        .join("3f9a2b7c-1111-2222-3333-444455556666")
        .join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    // 活跃一支（刚写入，mtime 在窗口内）+ 已结束一支（mtime 回拨 1 小时）
    std::fs::write(sub.join("agent-live01.meta.json"), r#"{"name":"dr-coder"}"#).unwrap();
    std::fs::write(sub.join("agent-live01.jsonl"), "").unwrap();
    std::fs::write(
        sub.join("agent-done02.meta.json"),
        r#"{"name":"dr-planner"}"#,
    )
    .unwrap();
    let done = sub.join("agent-done02.jsonl");
    std::fs::write(&done, "").unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&done)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();

    let ids = |body: &Value| -> Vec<String> {
        body.as_array()
            .unwrap()
            .iter()
            .map(|s| s["agentId"].as_str().unwrap().to_string())
            .collect()
    };

    // 默认口径：只回活跃
    let (status, body) = req(
        test_app(&dir),
        "GET",
        "/api/agents/claude/a1b2c3d4/subagents",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ids(&body), vec!["live01".to_string()]);

    // 两种 opt-in 形态都 200 且全量（1 与 true 等价）
    for q in ["?finished=1", "?finished=true"] {
        let (status, body) = req(
            test_app(&dir),
            "GET",
            &format!("/api/agents/claude/a1b2c3d4/subagents{q}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "q={q}");
        let mut got = ids(&body);
        got.sort();
        assert_eq!(
            got,
            vec!["done02".to_string(), "live01".to_string()],
            "q={q}"
        );
    }
}

/// Done 会话 send 复活链路（2026-10-02 全驱动展示纪律）：job 退出态 → send 先走
/// respawn（fake 记录调用）再 attach 注入；running 态不触发。历史面板点开 Done
/// 会话发送即复活、回侧栏 Working 的链路依赖此步。两场景合一个测试顺序执行——
/// 共享 fixture 日志文件，并行测试会互踩（实测竞态）。
#[tokio::test]
async fn send_respawns_done_session_but_not_live() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let respawn_log = fixtures.join("respawn-calls.log");
    let _ = std::fs::remove_file(&respawn_log);

    // 场景 1：running 态 → 不触发 respawn（常态路径零额外开销）
    let id = "livejob1";
    let job = dir.path().join("jobs").join(id);
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        json!({"state": "running", "name": "live-one", "cwd": "/Users/demo/proj-alpha"})
            .to_string(),
    )
    .unwrap();
    let (status, _) = req(
        test_app(&dir),
        "POST",
        &format!("/api/agents/claude/{id}/message"),
        Some(json!({"text": "常规消息"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let calls = std::fs::read_to_string(&respawn_log).unwrap_or_default();
    assert!(
        !calls.lines().any(|l| l.contains(id)),
        "running 态不应触发 respawn: {calls:?}"
    );

    // 场景 2：退出态 → respawn 后注入（复活链路）
    let id = "donejob1";
    let job = dir.path().join("jobs").join(id);
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        json!({"state": "exited", "name": "done-one", "cwd": "/Users/demo/proj-alpha"}).to_string(),
    )
    .unwrap();
    let (status, body) = req(
        test_app(&dir),
        "POST",
        &format!("/api/agents/claude/{id}/message"),
        Some(json!({"text": "复活测试"})),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body={body}");
    let calls = std::fs::read_to_string(&respawn_log).unwrap_or_default();
    assert!(
        calls.lines().any(|l| l.contains(id)),
        "respawn 应被调用: {calls:?}"
    );
    let stdin_log = fixtures.join(format!("attach-stdin-{id}.txt"));
    let injected = wait_stdin_contains(&stdin_log, "复活测试");
    assert!(
        injected.contains("复活测试"),
        "消息应注入 TUI: {injected:?}"
    );
    let _ = std::fs::remove_file(&stdin_log);
    let _ = std::fs::remove_file(&respawn_log);
}

/// 会话外观设置（agent-hub-model-thinking）：claude 注入 /model 走 attach 通道，
/// working 时收场文案如实「已排队」；注入不产生对话消息（命令由 TUI 解释）。
#[tokio::test]
async fn set_model_injects_slash_command_and_reports_queue() {
    let dir = tempfile::tempdir().unwrap();
    let id = "modeljob1";
    let job = dir.path().join("jobs").join(id);
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        json!({"state": "working", "name": "busy-one", "cwd": "/Users/demo/proj-alpha",
               "respawnFlags": ["--model", "opus"]})
        .to_string(),
    )
    .unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let stdin_log = fixtures.join(format!("attach-stdin-{id}.txt"));
    let _ = std::fs::remove_file(&stdin_log);

    // 读：respawnFlags --model → 当前模型如实读出（尽力而为口径）
    let (status, body) = req(
        test_app(&dir),
        "GET",
        &format!("/api/agents/claude/{id}/model-settings"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["caps"]["canSet"], true);
    assert_eq!(
        body["caps"]["supportsThinking"], true,
        "/effort 斜杠命令（2026-10-02 用户确认）→ 支持思考"
    );
    assert_eq!(body["current"]["id"], "opus");
    assert_eq!(
        body["current"]["variant"],
        Value::Null,
        "无 --effort 如实 null"
    );
    // 档位枚举对齐 CLI --effort 合法值
    assert_eq!(
        body["models"][0]["variants"],
        json!(["low", "medium", "high", "xhigh", "max"])
    );
    assert!(
        body["models"].as_array().is_some_and(|m| !m.is_empty()),
        "内置清单非空"
    );

    // 设：working → 注入 /model sonnet，收场文案「已排队」
    let (status, body) = req(
        test_app(&dir),
        "POST",
        &format!("/api/agents/claude/{id}/model-settings"),
        Some(json!({"model": "sonnet"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["message"].as_str().unwrap().contains("已排队"),
        "working 应报已排队: {body}"
    );
    let injected = wait_stdin_contains(&stdin_log, "/model sonnet");
    assert!(
        injected.contains("/model sonnet"),
        "/model 命令应注入 TUI: {injected:?}"
    );
    assert!(!injected.contains("复活测试"), "命令注入不走消息正文路径");
    let _ = std::fs::remove_file(&stdin_log);
}

/// 空闲态设模型 → 收场「已切换」；respawnFlags 无 --model → current 如实 null。
#[tokio::test]
async fn set_model_idle_reports_switched_and_null_current_is_honest() {
    let dir = tempfile::tempdir().unwrap();
    let id = "idlemodel1";
    let job = dir.path().join("jobs").join(id);
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        json!({"state": "idle", "name": "calm-one", "cwd": "/Users/demo/proj-alpha"}).to_string(),
    )
    .unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let stdin_log = fixtures.join(format!("attach-stdin-{id}.txt"));
    let _ = std::fs::remove_file(&stdin_log);

    let (_, body) = req(
        test_app(&dir),
        "GET",
        &format!("/api/agents/claude/{id}/model-settings"),
        None,
    )
    .await;
    assert_eq!(body["current"], Value::Null, "无 --model 如实 null");

    let (status, body) = req(
        test_app(&dir),
        "POST",
        &format!("/api/agents/claude/{id}/model-settings"),
        Some(json!({"model": "haiku"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "已切换");
    let _ = std::fs::remove_file(&stdin_log);
}

/// 不支持驱动的设置 → 404（Unsupported → not_found 映射，app 层不漏驱动特例）。
#[tokio::test]
async fn set_model_on_unsupported_driver_is_404() {
    let dir = tempfile::tempdir().unwrap();
    // acp 驱动无会话模型语义；未注册的会话 id 由驱动域错误表达——直接打 acp
    let (status, _) = req(
        test_app(&dir),
        "POST",
        "/api/agents/acp/ghost-session/model-settings",
        Some(json!({"model": "opus"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "Unsupported → 404");
}

/// claude 思考档位即时设置（2026-10-02 用户确认 /effort <level>）：选档 →
/// stdin 落盘含 `/effort <level>`；working 态 → 排队文案；非法档位 → 显式报错。
#[tokio::test]
async fn set_thinking_injects_effort_and_queues_when_working() {
    let dir = tempfile::tempdir().unwrap();
    let id = "effortjob1";
    let job = dir.path().join("jobs").join(id);
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        json!({"state": "working", "name": "busy", "cwd": "/Users/demo/proj-alpha",
               "respawnFlags": ["--effort", "xhigh", "--model", "Opus"]})
        .to_string(),
    )
    .unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let stdin_log = fixtures.join(format!("attach-stdin-{id}.txt"));
    let _ = std::fs::remove_file(&stdin_log);

    // 读：respawnFlags --effort → 当前思考档位如实读出
    let (status, body) = req(
        test_app(&dir),
        "GET",
        &format!("/api/agents/claude/{id}/model-settings"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["current"]["variant"], "xhigh", "--effort 回写实读");
    assert_eq!(body["current"]["id"], "Opus");

    // 设：面板二段选择 = {model: 当前模型, variant: 新档位} → 注入 /effort
    let (status, body) = req(
        test_app(&dir),
        "POST",
        &format!("/api/agents/claude/{id}/model-settings"),
        Some(json!({"model": "Opus", "variant": "low"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert!(
        body["message"].as_str().unwrap().contains("已排队"),
        "working 应报已排队: {body}"
    );
    let injected = wait_stdin_contains(&stdin_log, "/effort low");
    assert!(
        injected.contains("/effort low"),
        "/effort 命令应注入 TUI: {injected:?}"
    );
    assert!(injected.contains("/model Opus"), "模型注入保持");

    // 非法档位 → 显式报错（不透传 TUI 静默失败）
    let (status, body) = req(
        test_app(&dir),
        "POST",
        &format!("/api/agents/claude/{id}/model-settings"),
        Some(json!({"model": "Opus", "variant": "ultra"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(
        body.as_str().map(|s| s.contains("档位")).unwrap_or(false)
            || body["error"]
                .as_str()
                .map(|s| s.contains("档位"))
                .unwrap_or(false),
        "错误应含档位说明: {body}"
    );
    let _ = std::fs::remove_file(&stdin_log);
}
