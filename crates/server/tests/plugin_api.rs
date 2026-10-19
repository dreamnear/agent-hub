//! 插件体系 B1 集成测试（agent-hub-plugin-system B1 / agent-hub#27）。
//!
//! 覆盖 §9.1 端到端链路中「后端可达」的部分：发现 → 审计 → 安装 → 视图票据与资产
//! 门禁 → rpc（含权限越界留痕）→ 停用/启用 → 卸载 + 回归断言集 → 零插件基线。

use std::{path::Path, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// 测试专用凭据值（非真实凭据；断言注入路径用，绝不打印）
const FAKE_CRED: &str = "fake-credential-value-for-test";

fn cfg_with(dir: &tempfile::TempDir) -> Config {
    let mut cfg = Config::load();
    cfg.claude_bin = fake_bin();
    cfg.jobs_dir = dir.path().join("jobs");
    cfg.projects_file = dir.path().join("projects.json");
    cfg.notes_file = dir.path().join("notes.json");
    cfg.allow_lan = false;
    cfg.token = None;
    cfg.plugins_dir = dir.path().join("plugins");
    cfg.plugin_state_file = dir.path().join("plugins.json");
    cfg.plugin_credentials_file = dir.path().join("plugin_credentials.json");
    cfg.plugin_audit_dir = dir.path().join("plugin_audit");
    cfg.plugin_data_dir = dir.path().join("plugin_data");
    cfg.plugin_scan_secs = 0;
    cfg.plugin_transient_dirs = Vec::new();
    std::fs::create_dir_all(&cfg.plugins_dir).unwrap();
    cfg
}

fn app(cfg: Config) -> (Arc<AppState>, axum::Router) {
    let state = Arc::new(AppState::from_config(cfg));
    let r = router(state.clone());
    (state, r)
}

async fn req(
    a: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, axum::http::HeaderMap) {
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
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        headers,
    )
}

/// 文本响应版请求（AppError 走纯文本，不套 JSON 信封）。
async fn req_text(
    a: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// 写一个示例插件（sh backend + 原生 JS 视图；零直调 /api/*）。
fn write_plugin(dir: &tempfile::TempDir, id: &str) {
    let p = dir.path().join("plugins").join(id);
    std::fs::create_dir_all(p.join("view")).unwrap();
    std::fs::create_dir_all(p.join("backend")).unwrap();
    std::fs::write(
        p.join("plugin.json"),
        format!(
            r#"{{
  "schemaVersion": 1,
  "id": "{id}",
  "name": "Demo Workbench",
  "version": "0.1.0",
  "description": "B1 integration fixture",
  "hub": {{ "apiVersion": "1" }},
  "contributes": {{
    "tools": [{{ "id": "ping", "name": "Ping", "description": "roundtrip", "risk": "read",
                "impl": {{ "kind": "backend", "ref": "ping" }} }}],
    "views": [{{ "id": "home", "title": "Demo View", "entry": "view/index.html", "order": 1 }}]
  }},
  "permissions": {{ "data": ["projects:read"], "credentials": ["demo_pat"] }},
  "backend": {{ "command": "backend/main.sh", "args": [], "idleTimeoutSecs": 300 }}
}}"#
        ),
    )
    .unwrap();
    std::fs::write(
        p.join("view/index.html"),
        "<!doctype html><html><head><link rel=\"stylesheet\" href=\"style.css\" /></head><body><div id=\"root\"></div><script src=\"app.js\"></script></body></html>",
    )
    .unwrap();
    std::fs::write(
        p.join("view/app.js"),
        "document.getElementById('root').textContent='ok';\n",
    )
    .unwrap();
    std::fs::write(p.join("view/style.css"), "body { margin: 0; }\n").unwrap();
    std::fs::write(
        p.join("backend/main.sh"),
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"ok":true}}\n' "$id";;
    *tool.invoke*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id";;
  esac
done
"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            p.join("backend/main.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
}

fn write_credentials(dir: &tempfile::TempDir) {
    std::fs::write(
        dir.path().join("plugin_credentials.json"),
        format!(r#"{{"demo_pat":"{FAKE_CRED}"}}"#),
    )
    .unwrap();
}

async fn install_demo(a: axum::Router, id: &str) -> Value {
    let (status, discovered, _) = req(a.clone(), "GET", "/api/plugins/discovered", None).await;
    assert_eq!(status, StatusCode::OK);
    let item = discovered["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("未发现插件 {id}：{discovered}"));
    assert_eq!(item["status"], "new");
    let hash = item["report"]["contentHash"].as_str().unwrap().to_string();
    let (status, body, _) = req(
        a,
        "POST",
        "/api/plugins/install",
        Some(json!({ "id": id, "contentHash": hash })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "安装失败：{body}");
    body
}

#[tokio::test]
async fn b1_end_to_end_install_view_ticket_rpc_disable_uninstall() {
    let dir = tempfile::tempdir().unwrap();
    // 凭据必须在构建 AppState 之前落盘（凭据表在构造期加载）
    write_credentials(&dir);
    let cfg = cfg_with(&dir);
    let (state, a) = app(cfg);
    write_plugin(&dir, "demo-plugin");

    // 零插件基线：清单为空、无导航贡献物
    let (status, list, _) = req(a.clone(), "GET", "/api/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 0);
    assert!(
        state.plugins.nav_entries().is_empty(),
        "零插件时导航集合为空"
    );

    // 安装
    let body = install_demo(a.clone(), "demo-plugin").await;
    assert_eq!(body["plugin"]["uiState"], "enabled");
    assert_eq!(body["plugin"]["state"], "ready");
    assert_eq!(body["alreadyInstalled"], false);
    assert_eq!(body["effect"], "immediate");

    // 贡献物 + 凭据按名（值零回显）
    let plugin = &body["plugin"];
    assert_eq!(plugin["contributions"].as_array().unwrap().len(), 2);
    assert_eq!(plugin["permissions"]["credentials"][0]["name"], "demo_pat");
    assert_eq!(plugin["permissions"]["credentials"][0]["configured"], true);
    let raw = plugin.to_string();
    assert!(
        !raw.contains(FAKE_CRED),
        "凭据值不得出现在 /api/plugins 响应中"
    );

    // 导航组出现入口
    let nav = state.plugins.nav_entries();
    assert_eq!(nav.len(), 1);
    assert_eq!(nav[0].title, "Demo View");
    assert!(!nav[0].fault);

    // 幂等：同 hash 重复安装无副作用
    let (status, again, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/install",
        Some(json!({ "id": "demo-plugin", "contentHash": plugin["contentHash"] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["alreadyInstalled"], true);

    // 视图票据 + 资产门禁
    let (status, ticket, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/view-ticket",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ticket["expiresInSecs"], 120);
    let t = ticket["ticket"].as_str().unwrap().to_string();
    assert_eq!(t.len(), 64, "票据为 32 字节随机不透明串");

    // 形态一（入口）：query 票据（ADR-0004 原形）
    let (status, _, headers) = req(
        a.clone(),
        "GET",
        &format!("/plugin-view/demo-plugin/index.html?ticket={t}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let csp = headers
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("connect-src 'none'"), "CSP: {csp}");
    assert!(csp.contains("default-src 'none'"));
    assert_eq!(headers.get("cache-control").unwrap(), "no-store");

    // 形态二（tester 1 轮 FAIL-1 修复）：票据入路径段——静态 HTML 的相对引用
    // （app.js / style.css）由浏览器按文档目录解析时自动继承，不依赖 cookie/query。
    // 此处模拟浏览器真实路径：子资源请求**不带 query、不带 cookie**。
    let (status, _, headers) = req(
        a.clone(),
        "GET",
        &format!("/plugin-view/demo-plugin/~{t}/index.html"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "路径段形态入口必须可达");
    assert!(
        !headers.contains_key("set-cookie"),
        "cookie 承接已废弃（sandbox iframe 不随发），响应不得再种票据 cookie"
    );
    for asset in ["app.js", "style.css"] {
        let (status, _, _) = req(
            a.clone(),
            "GET",
            &format!("/plugin-view/demo-plugin/~{t}/{asset}"),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "次级资产 {asset}（无 query/cookie）必须 200"
        );
    }

    // 无票据 / 错作用域 / 越界路径一律拒绝
    let (status, _, _) = req(
        a.clone(),
        "GET",
        "/plugin-view/demo-plugin/index.html",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "无票据必须 403");
    // 浏览器真实失败形态：次级资产无任何票据（query/cookie/路径段都没有）
    let (status, _, _) = req(a.clone(), "GET", "/plugin-view/demo-plugin/app.js", None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "无票据次级资产必须 403（tester 复现路径）"
    );
    let (status, _, _) = req(
        a.clone(),
        "GET",
        &format!("/plugin-view/other-plugin/~{t}/index.html"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "票据作用域限单插件（路径段形态同）"
    );
    let (status, _, _) = req(
        a.clone(),
        "GET",
        &format!("/plugin-view/demo-plugin/%2e%2e%2fplugin.json?ticket={t}"),
        None,
    )
    .await;
    assert!(
        status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
        "路径逃逸必须被拒，实际 {status}"
    );

    // rpc → 懒启动 backend → tool.invoke
    let (status, res, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.backend.ping", "params": { "args": {} } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rpc 失败：{res}");
    assert_eq!(res["result"]["content"][0]["text"], "pong");
    assert!(
        state.plugins.list()[0].backend.running,
        "首次 RPC 后 backend 应已懒启动"
    );

    // hub 代执行：已声明凭据可取（值只在载荷），未声明凭据被拒 + 违规留痕
    let (status, res, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.credentials.use", "params": { "name": "demo_pat" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(res["result"]["value"], FAKE_CRED);

    let (status, _, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.credentials.use", "params": { "name": "evil_pat" } })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "未声明凭据必须拒绝");
    let (status, records, _) =
        req(a.clone(), "GET", "/api/plugins/demo-plugin/records", None).await;
    assert_eq!(status, StatusCode::OK);
    let recs = records["records"].as_array().unwrap();
    assert!(
        recs.iter().any(|r| r["kind"] == "violation"),
        "越界调用必须留痕：{records}"
    );
    assert!(
        !records.to_string().contains(FAKE_CRED),
        "留痕不得回显凭据值"
    );

    // 未注册方法一律拒绝（B1 白名单之外）
    let (status, _, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.sessions.list", "params": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 停用：入口与能力同现同隐（BR-03），旧票据作废
    let (status, body, _) = req(a.clone(), "POST", "/api/plugins/demo-plugin/disable", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["uiState"], "disabled");
    assert_eq!(body["effect"], "immediate");
    assert!(
        state.plugins.nav_entries().is_empty(),
        "停用后导航入口必须消失"
    );
    let (status, _, _) = req(
        a.clone(),
        "GET",
        &format!("/plugin-view/demo-plugin/index.html?ticket={t}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "停用后旧票据失效");
    let (status, _, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/view-ticket",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 再启用：恢复
    let (status, body, _) = req(a.clone(), "POST", "/api/plugins/demo-plugin/enable", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["uiState"], "enabled");
    assert_eq!(state.plugins.nav_entries().len(), 1);

    // 卸载：贡献物/入口/登记消失 + 回归断言集自动执行 + 目录保留
    let (status, body, _) = req(a.clone(), "DELETE", "/api/plugins/demo-plugin", None).await;
    assert_eq!(status, StatusCode::OK, "卸载失败：{body}");
    assert_eq!(body["removed"], true);
    assert_eq!(body["regression"]["passed"], true, "回归断言集：{body}");
    assert!(
        dir.path().join("plugins/demo-plugin").is_dir(),
        "目录默认保留"
    );
    assert!(state.plugins.list().is_empty(), "登记必须清空");
    assert!(state.plugins.nav_entries().is_empty());
    let (status, records, _) =
        req(a.clone(), "GET", "/api/plugins/demo-plugin/records", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        records["records"].as_array().unwrap().is_empty(),
        "卸载后记录面为未安装"
    );

    // 零回归回放：卸载到零插件后再走一遍主体链路
    let (status, list, _) = req(a.clone(), "GET", "/api/plugins", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(list.as_array().unwrap().is_empty());
    let (status, _, _) = req(a.clone(), "GET", "/api/agents", None).await;
    assert_eq!(status, StatusCode::OK, "主体 agents 链路在插件全清后仍可用");
}

#[tokio::test]
async fn killed_backend_turns_fault_then_manual_retry_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&dir);
    let (state, a) = app(cfg);
    write_plugin(&dir, "demo-plugin");
    install_demo(a.clone(), "demo-plugin").await;

    // 先跑一次 RPC 把 backend 拉起来（懒启动）
    let (status, _, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.backend.ping", "params": { "args": {} } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let pid = state.plugins.list()[0]
        .backend
        .pid
        .expect("backend 应已启动并记录 pid");

    // 手工 kill（等价验收步骤 5：进程被外部杀掉）
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill 可执行");
    assert!(killed.success(), "kill 失败");

    // 监视器一周期（2s tick 的逻辑本体，测试里直接调以免 sleep）
    for _ in 0..40 {
        state.plugins.monitor_once().await;
        if state.plugins.list()[0].ui_state == "fault" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let v = &state.plugins.list()[0];
    assert_eq!(v.ui_state, "fault", "进程被杀必须呈故障态（BR-07）");
    let msg = v.last_fault.as_ref().unwrap().message.clone();
    assert!(msg.contains("崩溃"), "故障原因须可查：{msg}");

    // 故障态下能力调用给明确错误，不挂起（BR-07）
    let (status, body, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.backend.ping", "params": { "args": {} } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["message"].as_str().unwrap().contains("故障"), "{body}");

    // 手动重试：整体重登成功（BR-09）
    let (status, body, _) = req(a.clone(), "POST", "/api/plugins/demo-plugin/retry", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["plugin"]["uiState"], "enabled");
    let (status, res, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.backend.ping", "params": { "args": {} } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "重试后能力应恢复：{res}");
    assert_eq!(res["result"]["content"][0]["text"], "pong");
    let (_, records, _) = req(a.clone(), "GET", "/api/plugins/demo-plugin/records", None).await;
    assert!(records["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["message"].as_str().unwrap_or("").contains("已恢复")));
}

#[tokio::test]
async fn audit_gate_rejects_and_hash_mismatch_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&dir);
    let (_state, a) = app(cfg);

    // 审计失败插件 A：视图入口缺失 → 拒绝安装、可定位错因（path-missing）
    let missing = dir.path().join("plugins").join("missing-view");
    std::fs::create_dir_all(&missing).unwrap();
    std::fs::write(
        missing.join("plugin.json"),
        r#"{"schemaVersion":1,"id":"missing-view","name":"Broken","version":"0.1.0","hub":{"apiVersion":"1"},
           "contributes":{"views":[{"id":"v","title":"V","entry":"view/missing.html"}]}}"#,
    )
    .unwrap();
    // 审计失败插件 B：id 非法 → id-format（字段级可定位）
    let badid = dir.path().join("plugins").join("bad-id");
    std::fs::create_dir_all(&badid).unwrap();
    std::fs::write(
        badid.join("plugin.json"),
        r#"{"schemaVersion":1,"id":"Bad_Id","name":"Broken","version":"0.1.0","hub":{"apiVersion":"1"}}"#,
    )
    .unwrap();

    let (status, disc, _) = req(a.clone(), "GET", "/api/plugins/discovered", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = disc["items"].as_array().unwrap();
    let missing_item = items.iter().find(|i| i["id"] == "missing-view").unwrap();
    assert_eq!(missing_item["status"], "audit_failed");
    assert!(missing_item["report"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["rule"] == "path-missing"));
    let badid_item = items.iter().find(|i| i["id"] == "bad-id").unwrap();
    assert_eq!(badid_item["status"], "audit_failed");
    assert!(badid_item["report"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["field"] == "id" && e["rule"] == "id-format"));

    // 拒绝安装（错误态卡无安装按钮，后端同口径拒绝）
    let (status, _, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/install",
        Some(json!({
            "id": "missing-view",
            "contentHash": missing_item["report"]["contentHash"]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // hash 不一致（TOCTOU）拒绝
    write_plugin(&dir, "demo-plugin");
    // 扫描只按周期/手动触发（ADR-0002 第 2 条）：此处显式刷新拿最新发现面
    let (status, _, _) = req(a.clone(), "POST", "/api/plugins/scan", None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, disc, _) = req(a.clone(), "GET", "/api/plugins/discovered", None).await;
    let ok_item = disc["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "demo-plugin")
        .unwrap();
    let (status, raw) = req_text(
        a.clone(),
        "POST",
        "/api/plugins/install",
        Some(json!({ "id": "demo-plugin", "contentHash": "sha256:deadbeef" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(raw.contains("hash"), "错因须提示 hash 不一致：{raw}");
    let _ = ok_item;

    // 只审计不安装（作者自检入口）
    let (status, report, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/audit",
        Some(json!({ "path": dir.path().join("plugins/demo-plugin").to_string_lossy() })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["installable"], true);

    // api-schema 由定义源驱动
    let (status, schema, _) = req(a.clone(), "GET", "/api/plugins/api-schema", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(schema["apiVersion"], 1);
    assert_eq!(schema["contributions"].as_array().unwrap().len(), 4);
    assert_eq!(schema["highRisk"].as_array().unwrap().len(), 4);
    let methods = schema["rpcMethods"].as_array().unwrap();
    assert!(methods.iter().any(|m| m["method"] == "hub.credentials.use"));
}

/// 本地 GitLab mock：断言插件 backend 注入的 PRIVATE-TOKEN 与凭据代持值一致
/// （凭据注入路径的端到端证据，值零回显——只比对不打印）。
async fn spawn_gitlab_mock(expect_token: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0u8; 4096];
            let Ok(n) = sock.read(&mut buf).await else {
                continue;
            };
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let auth_ok = req.contains(&format!("PRIVATE-TOKEN: {expect_token}"));
            let body = if auth_ok {
                // 返回两条 issue（字段对齐 GitLab API 投影）
                r#"[{"iid":7,"title":"从凭据注入的请求返回","state":"opened","labels":["demo"],
                     "assignee":{"name":"tester"},"updated_at":"2026-10-19T09:00:00Z","web_url":"https://example.test/i/7"},
                    {"iid":6,"title":"更早的条目","state":"closed","labels":[],
                     "assignee":null,"updated_at":"2026-10-18T09:00:00Z","web_url":"https://example.test/i/6"}]"#
            } else {
                "[]"
            };
            let status = if auth_ok { 200 } else { 401 };
            let resp = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{body}",
                len = body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    format!("http://{addr}/api/v4")
}

/// 真示例插件端到端（§9.1 B1 checklist 的服务端可达部分）：drop-in → 审计 → 安装
/// → 视图资产 → rpc → 懒启动 node backend → hub.credentials.use 凭据注入 →
/// 本地 mock GitLab → 结果回传。E2E 以 GITLAB_API_BASE 指向本地 mock（零外网依赖）。
#[tokio::test]
async fn bundled_example_plugin_full_chain_with_node_backend() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/gitlab-issues-lite");
    assert!(src.join("plugin.json").is_file(), "示例插件应随仓交付");
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("plugins/gitlab-issues-lite");
    copy_dir(&src, &dst);

    // 凭据先于 AppState 构造落盘（构造期加载）；mock 地址经 env 传给 backend（子进程继承）
    let cred_path = dir.path().join("plugin_credentials.json");
    std::fs::write(
        &cred_path,
        r#"{"gitlab_pat":"fake-credential-value-for-test"}"#,
    )
    .unwrap();
    let base = spawn_gitlab_mock("fake-credential-value-for-test").await;
    std::env::set_var("GITLAB_API_BASE", &base);
    std::env::set_var("GITLAB_PROJECT", "demo-repo");

    let mut cfg = cfg_with(&dir);
    cfg.plugin_credentials_file = cred_path;
    let (_state, a) = app(cfg);

    // 安装（drop-in 发现面）
    let body = install_demo(a.clone(), "gitlab-issues-lite").await;
    assert_eq!(body["plugin"]["uiState"], "enabled");
    // 审计卡内容：贡献物 2 条 + 凭据按名
    let plugin = &body["plugin"];
    assert_eq!(plugin["contributions"].as_array().unwrap().len(), 2);
    assert_eq!(
        plugin["permissions"]["credentials"][0]["name"],
        "gitlab_pat"
    );
    let raw = plugin.to_string();
    assert!(
        !raw.contains("fake-credential-value-for-test"),
        "凭据值零回显"
    );

    // 视图资产：index.html + app.js 均经票据下发
    let (status, ticket, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/gitlab-issues-lite/view-ticket",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let t = ticket["ticket"].as_str().unwrap().to_string();
    let (status, _, headers) = req(
        a.clone(),
        "GET",
        &format!("/plugin-view/gitlab-issues-lite/index.html?ticket={t}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers
        .get("content-security-policy")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("connect-src 'none'"));
    let (status, _, _) = req(
        a.clone(),
        "GET",
        &format!("/plugin-view/gitlab-issues-lite/app.js?ticket={t}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "多文件视图次级资源可取");

    // rpc 全链路：宿主判定 → 懒启动 node backend → 凭据注入 → mock GitLab → 结果
    let (status, res, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/gitlab-issues-lite/rpc",
        Some(json!({ "method": "hub.backend.fetch_issues", "params": { "args": {} } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "rpc 失败：{res}");
    let text = res["result"]["content"][0]["text"]
        .as_str()
        .expect("text block");
    let data: Value = serde_json::from_str(text).unwrap();
    assert_eq!(data["project"], "demo-repo");
    assert_eq!(data["total"], 2);
    assert_eq!(data["issues"][0]["iid"], 7);
    assert!(data["issues"][0]["title"]
        .as_str()
        .unwrap()
        .contains("凭据注入"));
    assert!(
        !res.to_string().contains("fake-credential-value-for-test"),
        "rpc 应答之外不得出现凭据值"
    );

    // 停用 → 懒启动的 backend 进程组被回收，能力不可发现
    let (status, body, _) = req(
        a.clone(),
        "POST",
        "/api/plugins/gitlab-issues-lite/disable",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["plugin"]["backend"]["running"], false,
        "停用必须回收 backend 进程组"
    );
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

#[tokio::test]
async fn startup_restores_enabled_plugins_and_reports_dir_missing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&dir);
    write_plugin(&dir, "demo-plugin");
    let (state, a) = app(cfg);

    // 先安装一次，落持久态
    install_demo(a.clone(), "demo-plugin").await;
    assert_eq!(state.plugins.list()[0].ui_state, "enabled");

    // 重建 AppState（模拟重启）：持久态 + 目录 → 恢复为启用
    let cfg2 = cfg_with(&dir);
    let (state2, _a2) = app(cfg2);
    state2.plugins.startup().await;
    let list = state2.plugins.list();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, "demo-plugin");
    assert_eq!(list[0].ui_state, "enabled", "重启后启用态应恢复");

    // 目录被手工删除（多主工操作）→ 不自动卸载，呈故障态 + 可定位原因（dir-missing）
    std::fs::remove_dir_all(dir.path().join("plugins/demo-plugin")).unwrap();
    let cfg3 = cfg_with(&dir);
    let (state3, _a3) = app(cfg3);
    state3.plugins.startup().await;
    let list = state3.plugins.list();
    assert_eq!(list.len(), 1, "目录消失不自动卸载（保留注册记录）");
    assert_eq!(list[0].ui_state, "fault");
    let msg = list[0].last_fault.as_ref().unwrap().message.clone();
    assert!(msg.contains("dir-missing"), "故障原因须可定位：{msg}");
}

/// 凭据界面设置（2026-10-19 预览反馈增量）：设置/清除 值入口 +
/// 全程零回显（响应、/api/plugins、记录面均不得出现值）+ 落盘 0600。
#[tokio::test]
async fn credential_can_be_set_and_cleared_from_management_api() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&dir);
    write_plugin(&dir, "demo-plugin");
    let cred_file = dir.path().join("plugin_credentials.json");
    let (state, a) = app(cfg);
    install_demo(a.clone(), "demo-plugin").await;

    // 初始未配置
    let (_, list, _) = req(a.clone(), "GET", "/api/plugins", None).await;
    assert_eq!(
        list[0]["permissions"]["credentials"][0]["configured"],
        false
    );

    // 设置：只回状态，不回值
    let secret = "fake-credential-value-for-test";
    let (status, body) = req_text(
        a.clone(),
        "POST",
        "/api/plugins/credentials/demo_pat",
        Some(json!({ "value": secret })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.contains("configured"), true);
    assert!(!body.contains(secret), "响应不得回显凭据值：{body}");

    // 状态翻转 + 文件落盘 0600 + 值可读回（仅测试侧直读，非 API 通道）
    let (_, list, _) = req(a.clone(), "GET", "/api/plugins", None).await;
    assert_eq!(list[0]["permissions"]["credentials"][0]["configured"], true);
    assert!(!list.to_string().contains(secret), "清单不得回显凭据值");
    let raw = std::fs::read_to_string(&cred_file).unwrap();
    assert!(raw.contains(secret), "值应落盘 hub 侧凭据文件");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cred_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "凭据文件必须 0600");
    }

    // 设置后视图/backend 的 hub.credentials.use 立刻取到新值（同进程内即时生效）
    let (status, res) = req_text(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.credentials.use", "params": { "name": "demo_pat" } })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(res.contains(secret), "凭据注入路径可取到值");
    let (_, records, _) = req(a.clone(), "GET", "/api/plugins/demo-plugin/records", None).await;
    assert!(
        !records.to_string().contains(secret),
        "记录面不得回显凭据值"
    );

    // 清除：回到未配置态
    let (status, body) = req_text(
        a.clone(),
        "DELETE",
        "/api/plugins/credentials/demo_pat",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("false"), "{body}");
    let (_, list, _) = req(a.clone(), "GET", "/api/plugins", None).await;
    assert_eq!(
        list[0]["permissions"]["credentials"][0]["configured"],
        false
    );
    assert!(!std::fs::read_to_string(&cred_file)
        .unwrap()
        .contains(secret));
    // 清除后取用失败（未配置），不是静默空值
    let (status, body) = req_text(
        a.clone(),
        "POST",
        "/api/plugins/demo-plugin/rpc",
        Some(json!({ "method": "hub.credentials.use", "params": { "name": "demo_pat" } })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("credential-unconfigured"), "{body}");

    // 非法名 / 空值拒绝
    let (status, _) = req_text(
        a.clone(),
        "POST",
        "/api/plugins/credentials/bad%20name",
        Some(json!({ "value": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = req_text(
        a.clone(),
        "POST",
        "/api/plugins/credentials/demo_pat",
        Some(json!({ "value": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let _ = state;
}

/// review Round-1 BLOCKER 回归锚：停用条目在重启装载中绝不翻转 enabled——
/// 用户停用 + 目录被删 → 重启后必须仍是「已停用」，不得漂移为「启用 + 运行故障」
/// （BR-03 同现同隐 / BR-01 迁移表单一入口）。
#[tokio::test]
async fn startup_keeps_disabled_entry_disabled_even_when_dir_missing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_with(&dir);
    write_plugin(&dir, "demo-plugin");
    let (state, a) = app(cfg);
    install_demo(a.clone(), "demo-plugin").await;
    let (status, body, _) = req(a.clone(), "POST", "/api/plugins/demo-plugin/disable", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(state.plugins.list()[0].ui_state, "disabled");

    // 停用 + 目录被删 + 重启
    std::fs::remove_dir_all(dir.path().join("plugins/demo-plugin")).unwrap();
    let cfg2 = cfg_with(&dir);
    let (state2, _a2) = app(cfg2);
    state2.plugins.startup().await;
    let list = state2.plugins.list();
    assert_eq!(list.len(), 1, "停用条目保留注册记录");
    assert_eq!(list[0].enabled, false, "enabled 位不得被启动装载翻转");
    assert_eq!(
        list[0].ui_state, "disabled",
        "停用 + 目录缺失重启后仍是停用态"
    );
    assert_eq!(list[0].state, "stopped");
    assert!(
        state2.plugins.nav_entries().is_empty(),
        "停用态不得出现故障入口（BR-03）"
    );
}
