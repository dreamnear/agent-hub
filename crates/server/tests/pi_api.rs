//! pi 端点集成测试（agent-hub-pi-hub-agent BLOCKER 回修配套）：
//! tester 走查实锤 /api/pi/* 未挂载（lib.rs merge 行被误回滚，SPA fallback 吞了
//! GET 200 HTML）——本文件钉死「路由已挂载」契约，防再犯。

use axum::http::StatusCode;
use server::api::AppState;
use std::sync::Arc;
use tokio::sync::Mutex;

/// PI_AGENT_DIR set_var 是进程级全局：并行测试互踩（对方读到错目录）——
/// 惯例同 config.rs ENV_LOCK：调用 test_app 的测试全部串行化。
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

fn test_app(dir: &std::path::Path) -> Arc<AppState> {
    // PI_AGENT_DIR 由各测试体首行的 ENV_LOCK 守卫（test_app 读 env 固化进 AppState）
    // opencode/dsh 隔离（api_agents test_app 同款，#5 全仓测试卫生）：bin 指空
    // 防 spawn 真 serve（本机装有真 opencode 时每个测试都会拉起真进程）；
    // port 指死口防「认领健康端口」吃到本机残留 serve。
    let mut base = server::config::Config::load();
    base.opencode.bin = "/nonexistent/opencode-test-isolated".into();
    base.opencode.port = 65001;
    base.dsh.bin = "/nonexistent/dsh-test-isolated".into();
    base.dsh.port = 65002;
    let cfg = server::config::Config {
        projects_file: dir.join("projects.json"),
        token_file: dir.join("token"),
        agents_dir: dir.join("agents"),
        acp_sessions_file: dir.join("acp_sessions.json"),
        notes_file: dir.join("notes.json"),
        instances_file: dir.join("instances.json"),
        opencode_serve_file: dir.join("opencode_serve.json"),
        dsh_serve_file: dir.join("dsh_serve.json"),
        tasks_dir: dir.join("tasks"),
        ignored_interactive_file: dir.join("ignored.json"),
        pi_settings_file: dir.join("pi_settings.json"),
        pi_extension_file: dir.join("pi/hub-assistant.ts"),
        pi_session_dir: dir.join("pi/sessions"),
        ..base
    };
    // P10：PI_AGENT_DIR 隔离（Config::load 读 env——tempdir 内）
    std::env::set_var("PI_AGENT_DIR", dir.join("pi-agent"));
    Arc::new(AppState::from_config(cfg))
}

async fn req(
    app: Arc<AppState>,
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let app = axum::Router::new().merge(server::router(app));
    let mut builder = axum::http::Request::builder().method(method).uri(path);
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    // body 经 Builder::body 一次性落定（Builder 无分步 body setter）
    let req = builder
        .body(axum::body::Body::from(
            body.map(|b| b.to_string()).unwrap_or_default(),
        ))
        .unwrap();
    let res = tower::ServiceExt::oneshot(app, req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::String(
        String::from_utf8_lossy(&bytes).into_owned(),
    ));
    (status, json)
}

/// BLOCKER 钉子：GET /api/pi/settings 必须是 JSON 端点（非 SPA fallback HTML）。
/// 回归判据：响应可反序列化为对象且含 provider/model 键。
#[tokio::test]
async fn pi_settings_get_is_json_endpoint() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(test_app(dir.path()), "GET", "/api/pi/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("provider").is_some() && body.get("model").is_some(),
        "GET /api/pi/settings 应返回设置 JSON，实为: {body}"
    );
}

/// PUT roundtrip + 空白归一（api/pi.rs 语义的端到端确认）。
#[tokio::test]
async fn pi_settings_put_roundtrip_and_trim() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    let (status, body) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({"provider": "  anthropic  ", "model": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["provider"], "anthropic", "空白应 trim");
    assert_eq!(body["model"], serde_json::Value::Null, "空白串归一 null");
    // 持久化回读
    let (_, back) = req(app, "GET", "/api/pi/settings", None).await;
    assert_eq!(back["provider"], "anthropic");
}

/// P9 版本端点已挂载（JSON 形态；探测值依环境——只钉路由与键）。
#[tokio::test]
async fn pi_version_endpoint_mounted() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let (status, body) = req(test_app(dir.path()), "GET", "/api/pi/version", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("version").is_some(),
        "GET /api/pi/version 应返回 version 键形态，实为: {body}"
    );
}

// ---- P10 集成（provider 配置面：原生读写/保留语义/脱敏）----

use std::sync::Arc as StdArc;

async fn put_pi(app: StdArc<AppState>, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    req(app, "PUT", "/api/pi/settings", Some(body)).await
}

/// P10 主链：PUT 全量 → models.json 条目落盘（0600）+ settings 三键 → GET 掩码回显，
/// **响应全文不含明文 key**（脱敏红线）。
#[tokio::test]
async fn p10_put_full_roundtrip_masked_response() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    let (status, body) = put_pi(
        app.clone(),
        serde_json::json!({
            "provider": "my-relay",
            "model": "gpt-x",
            "thinking": "high",
            "baseUrl": "https://relay.example/v1",
            "api": "openai-completions",
            "apiKey": "sk-plaintext-abcdef123456",
            "models": ["gpt-x"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实为 {body}");
    let text = body.to_string();
    assert!(
        !text.contains("sk-plaintext"),
        "PUT 响应不得含明文 key: {text}"
    );
    assert_eq!(body["thinking"], "high");
    assert_eq!(
        body["providerConfig"]["baseUrl"],
        "https://relay.example/v1"
    );
    let masked = body["providerConfig"]["apiKeyMasked"].as_str().unwrap();
    assert!(!masked.contains("plaintext"), "掩码不得含中段: {masked}");

    // GET 同脱敏
    let (_, get_body) = req(app, "GET", "/api/pi/settings", None).await;
    let get_text = get_body.to_string();
    assert!(
        !get_text.contains("sk-plaintext"),
        "GET 响应不得含明文: {get_text}"
    );
    assert_eq!(
        get_body["providerConfig"]["apiKeyMasked"],
        body["providerConfig"]["apiKeyMasked"]
    );

    // 盘上文件 0600 + 含明文（写入路径）
    #[cfg(unix)]
    {
        let f = dir.path().join("pi-agent/models.json");
        let mode = server::platform::file_mode(&f).unwrap();
        assert_eq!(mode, 0o600, "models.json 应 0600");
    }
}

/// P10 保留语义：手工条目（lmapi）不被另一个 provider 的写入触碰；
/// 同条目二次 PUT 不带 apiKey → 原 key 保留（留空不覆盖）。
#[tokio::test]
async fn p10_preserves_manual_entries_and_blank_key() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("pi-agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("models.json"),
        r#"{"providers":{"lmapi":{"baseUrl":"https://keep","api":"anthropic-messages","apiKey":"ah-keepme","models":[{"id":"m0"}]}}}"#,
    )
    .unwrap();
    let app = test_app(dir.path());
    let (status, _) = put_pi(
        app.clone(),
        serde_json::json!({
            "provider": "my-relay",
            "baseUrl": "https://relay/v1",
            "api": "openai-completions",
            "apiKey": "sk-new-12345678",
            "models": ["m1"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // 手工条目逐字保留
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(agent.join("models.json")).unwrap()).unwrap();
    assert_eq!(raw["providers"]["lmapi"]["apiKey"], "ah-keepme");
    assert_eq!(raw["providers"]["lmapi"]["baseUrl"], "https://keep");
    // 同条目二次 PUT 不带 apiKey → 保留
    let (status, body) = put_pi(
        app,
        serde_json::json!({
            "provider": "my-relay",
            "baseUrl": "https://relay/v2",
            "api": "openai-completions",
            "models": ["m1"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(agent.join("models.json")).unwrap()).unwrap();
    assert_eq!(
        raw["providers"]["my-relay"]["apiKey"], "sk-new-12345678",
        "留空 apiKey 不得覆盖: {body}"
    );
    assert_eq!(raw["providers"]["my-relay"]["baseUrl"], "https://relay/v2");
}

/// P10 校验：thinking 白名单 400；baseUrl 无 provider 名 400。
#[tokio::test]
async fn p10_validation_400s() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    let (status, body) = put_pi(app.clone(), serde_json::json!({"thinking": "ultra"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "实为 {body}");
    let (status, body) = put_pi(
        app,
        serde_json::json!({"baseUrl": "https://x", "api": "openai-completions"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "实为 {body}");
}

/// P10 settings.json 其他键保留（pi 内部键不被三键 merge 清掉）。
#[tokio::test]
async fn p10_settings_merge_keeps_other_keys() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("pi-agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"hideThinkingBlock":true,"defaultModel":"old"}"#,
    )
    .unwrap();
    let app = test_app(dir.path());
    let (status, _) = put_pi(
        app,
        serde_json::json!({"provider":"anthropic","model":"new","thinking":"low"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(agent.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(raw["hideThinkingBlock"], true, "pi 内部键保留");
    assert_eq!(raw["defaultModel"], "new");
    assert_eq!(raw["defaultThinkingLevel"], "low");
}

// ---- P10 增量轮：注入授权闸 + 跨会话搜索 ----

/// 注入授权闸：默认关 → 403；开 → 走既有链路 accepted；未知会话 → 200 failed
/// 形态（extension 工具要拿结构化原因转告用户）。interactive 403 分支依赖
/// interactive 会话环境（集成环境无——tester 实测覆盖）。
#[tokio::test]
async fn inject_gate_and_statuses() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    // 默认关 → 403
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/inject",
        Some(serde_json::json!({"driver":"claude","id":"x","text":"hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "实为 {body}");
    // 开 → 未知会话走链路 → 200 + status=failed（claude attach 不存在）
    let (status, body) = put_pi(app.clone(), serde_json::json!({"allowSessionInject": true})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["allowSessionInject"], true);
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/inject",
        Some(serde_json::json!({"driver":"claude","id":"ghost","text":"hi"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "failed", "实为 {body}");
    // flags 持久化：回读 true
    let (_, back) = req(app, "GET", "/api/pi/settings", None).await;
    assert_eq!(back["allowSessionInject"], true);
}

/// 跨会话搜索：空库 hits 0；短 q 400。
#[tokio::test]
async fn search_empty_and_validation() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    let (status, body) = req(app.clone(), "GET", "/api/search?q=needle", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0, "空库应零命中: {body}");
    let (status, _) = req(app, "GET", "/api/search?q=a", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "短 q 应 400");
}

/// P11 工作目录：PUT 落盘 flags + 指向文件 400（预校验早反馈）。
#[tokio::test]
async fn p11_work_dir_put_and_validation() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    // 合法路径（不存在 → 服务端 resolve 时 mkdir）
    let wd = dir.path().join("ws");
    let (status, body) = put_pi(
        app.clone(),
        serde_json::json!({"workDir": wd.to_str().unwrap()}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实为 {body}");
    assert_eq!(body["workDir"], wd.to_str().unwrap());
    assert!(wd.is_dir(), "resolve 应已建目录");
    // 指向文件 → 400
    let f = dir.path().join("afile");
    std::fs::write(&f, "x").unwrap();
    let (status, body) = put_pi(app, serde_json::json!({"workDir": f.to_str().unwrap()})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.as_str().unwrap_or("").contains("不是目录"),
        "实为 {body}"
    );
}

// ---- P13：助手任务控制服务端闸（allowTaskControl 默认关）----

/// 闸三态 + 执行语义：默认关 toggle/run 均 403；只读清单（GET /api/tasks）恒可用
///（hub_tasks_list 不设闸的端点侧锚）；开 → toggle 翻转 enabled 落盘、run 触发
/// 200；任务不存在 / toggle 缺 enabled / action 非法 → 400；flags 持久化回读。
#[tokio::test]
async fn p13_task_control_gate_and_exec() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    // 种真实任务（TaskManager 启动加载 <tasks_dir>/tasks.json）
    let tasks_dir = dir.path().join("tasks");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    std::fs::write(
        tasks_dir.join("tasks.json"),
        r#"[{"id":"task-demo0001","name":"demo","cron":"0 9 * * *","enabled":false,"action":"inject","driver":"claude","session_id":"s1","text":"hi"}]"#,
    )
    .unwrap();
    let app = test_app(dir.path());
    // 默认关 → toggle / run 都 403
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"toggle","taskId":"task-demo0001","enabled":true})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "实为 {body}");
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"run","taskId":"task-demo0001"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // 只读清单不设闸
    let (status, list) = req(app.clone(), "GET", "/api/tasks", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["id"], "task-demo0001");
    // 开 → toggle 200 且落盘生效
    let (status, body) = put_pi(app.clone(), serde_json::json!({"allowTaskControl": true})).await;
    assert_eq!(status, StatusCode::OK, "实为 {body}");
    assert_eq!(body["allowTaskControl"], true);
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"toggle","taskId":"task-demo0001","enabled":true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实为 {body}");
    assert_eq!(body["id"], "task-demo0001");
    assert_eq!(body["enabled"], true);
    let (_, list) = req(app.clone(), "GET", "/api/tasks", None).await;
    assert_eq!(list[0]["enabled"], true, "toggle 应落盘");
    // run 200 fired（复用调度器 fire；inject 动作对 ghost 会话异步失败不影响本断言）
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"run","taskId":"task-demo0001"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实为 {body}");
    assert_eq!(body["fired"], "task-demo0001");
    // 校验：任务不存在 / toggle 缺 enabled / action 非法 → 400
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"toggle","taskId":"nope","enabled":true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"toggle","taskId":"task-demo0001"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"bogus","taskId":"task-demo0001"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // flags 持久化回读
    let (_, back) = req(app, "GET", "/api/pi/settings", None).await;
    assert_eq!(back["allowTaskControl"], true);
}

/// assistant-cron C1-C3/C5：助手会话内 cron create/delete——闸关 create 403；
/// 开 → create 落 tasks.json（source=assistant + Inject{pi, hub-assistant}，
/// C5 持久化=直接读文件断言）；cron 非法 / prompt 缺 → 400；delete 后清单
/// 不含该任务（C2 到点不再触发的事实源）且文件同步；delete 不存在 → 400。
#[tokio::test]
async fn p13b_assistant_cron_create_delete_persist() {
    let _env = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let tasks_dir = dir.path().join("tasks");
    std::fs::create_dir_all(&tasks_dir).unwrap();
    let app = test_app(dir.path());
    // 闸默认关 → create 403（C3 明确拒绝非静默）
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"create","name":"晨报","cron":"0 9 * * *","prompt":"汇报今日安排"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "实为 {body}");
    // 开闸
    let (status, _) = put_pi(app.clone(), serde_json::json!({"allowTaskControl": true})).await;
    assert_eq!(status, StatusCode::OK);
    // create：200 + 响应字段；持久化文件断言（C5）
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"create","name":"晨报","cron":"0 9 * * *","prompt":"汇报今日安排 {{date}}"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实为 {body}");
    let created_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["source"], "assistant");
    assert_eq!(body["enabled"], true);
    let raw = std::fs::read_to_string(tasks_dir.join("tasks.json")).unwrap();
    assert!(raw.contains(&created_id), "create 应落盘 tasks.json");
    assert!(raw.contains("assistant"), "source 标记应落盘");
    assert!(raw.contains("hub-assistant"), "唤醒目标应为 pi 助手会话");
    // 清单可见（C4 数据面）：GET /api/tasks 含该任务且 source 回传
    let (_, list) = req(app.clone(), "GET", "/api/tasks", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["source"], "assistant");
    assert_eq!(list[0]["action"], "inject");
    // 校验：cron 非法 / prompt 缺 → 400
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"create","name":"x","cron":"not-a-cron","prompt":"y"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"create","name":"x","cron":"0 9 * * *","prompt":" "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // delete：200 → 清单与文件同步不含（C2 到点不再触发的事实源）
    let (status, body) = req(
        app.clone(),
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"delete","taskId":created_id})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "实为 {body}");
    assert_eq!(body["deleted"], created_id);
    let (_, list) = req(app.clone(), "GET", "/api/tasks", None).await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    let raw = std::fs::read_to_string(tasks_dir.join("tasks.json")).unwrap();
    assert!(!raw.contains(&created_id), "delete 应同步落盘");
    // delete 不存在 → 400
    let (status, _) = req(
        app,
        "POST",
        "/api/pi/task-control",
        Some(serde_json::json!({"action":"delete","taskId":created_id})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---- model-key-bug：Model ∉ 自定义条目清单 = 死配置，写前拦截 ----

#[tokio::test]
async fn put_rejects_model_outside_entry_list() {
    let _g = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    // 先写合法条目（prov-x3 清单 = opus/sonnet）
    let (st, _) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({
            "provider": "prov-x3", "model": "claude-opus-5",
            "baseUrl": "https://x.example/v1", "api": "anthropic-messages",
            "apiKey": "sk-test", "models": ["claude-opus-5", "claude-sonnet-5"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // 死组合（用户实机同款）：Model=m-x3 ∉ 清单 → 400 且消息含两条修正路径
    let (st, body) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({
            "provider": "prov-x3", "model": "m-x3",
            "baseUrl": "https://x.example/v1", "api": "anthropic-messages",
            "apiKey": "sk-test", "models": ["claude-opus-5", "claude-sonnet-5"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // AppError 响应体为纯文本（req helper fallback 为 String 值）
    let msg = body.as_str().unwrap_or_default();
    assert!(
        msg.contains("m-x3") && msg.contains("加入模型清单"),
        "实为: {msg}"
    );
    // 合法组合放行（Model ∈ 清单）
    let (st, _) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({
            "provider": "prov-x3", "model": "claude-sonnet-5",
            "baseUrl": "https://x.example/v1", "api": "anthropic-messages",
            "apiKey": "sk-test", "models": ["claude-opus-5", "claude-sonnet-5"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn put_validates_against_existing_entry_when_models_omitted() {
    let _g = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    // 既有条目（清单 opus/sonnet）
    let (st, _) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({
            "provider": "prov-e", "model": "claude-opus-5",
            "baseUrl": "https://e.example/v1", "api": "anthropic-messages",
            "apiKey": "sk-e", "models": ["claude-opus-5", "claude-sonnet-5"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // 只改 Model（models 留空 = 保留既有清单语义）→ 死组合仍被既有清单拦截
    let (st, body) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({ "provider": "prov-e", "model": "m-ghost" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(body.as_str().unwrap_or_default().contains("m-ghost"));
    // 内建 provider（models.json 无条目）放行
    let (st, _) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({ "provider": "anthropic", "model": "claude-opus-5" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn put_rejects_when_existing_entry_has_empty_api_key() {
    let _g = ENV_LOCK.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let app = test_app(dir.path());
    let agent_dir = dir.path().join("pi-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    // 存量脏数据：prov-c3.apiKey 空串（pi schema minLength 1 → 整个 models.json 拒载）
    std::fs::write(
        agent_dir.join("models.json"),
        r#"{"providers":{"prov-c3":{"baseUrl":"https://c3","api":"anthropic-messages","apiKey":"","models":[{"id":"c3-m"}]},"ok-p":{"baseUrl":"https://ok","api":"anthropic-messages","apiKey":"sk-ok","models":[{"id":"ok-m"}]}}}"#,
    )
    .unwrap();
    // 合法保存也被拦截（写了也不生效——pi 整体拒载），错误指名空 key 条目
    let (st, body) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({
            "provider": "ok-p", "model": "ok-m",
            "baseUrl": "https://ok", "api": "anthropic-messages",
            "apiKey": "sk-ok", "models": ["ok-m"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let msg = body.as_str().unwrap_or_default();
    assert!(
        msg.contains("prov-c3") && msg.contains("拒载"),
        "实为: {msg}"
    );
    // 用户修复空 key 后 → 放行
    let fixed = r#"{"providers":{"prov-c3":{"baseUrl":"https://c3","api":"anthropic-messages","apiKey":"sk-c3","models":[{"id":"c3-m"}]},"ok-p":{"baseUrl":"https://ok","api":"anthropic-messages","apiKey":"sk-ok","models":[{"id":"ok-m"}]}}}"#;
    std::fs::write(agent_dir.join("models.json"), fixed).unwrap();
    let (st, _) = req(
        app.clone(),
        "PUT",
        "/api/pi/settings",
        Some(serde_json::json!({
            "provider": "ok-p", "model": "ok-m",
            "baseUrl": "https://ok", "api": "anthropic-messages",
            "apiKey": "sk-ok", "models": ["ok-m"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}
