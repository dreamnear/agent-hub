//! HTTP-over-mux 代理面 + 在线信号通道集成测试（gateway-node-p1 任务 8/9）。
//!
//! 全真栈：网关 hub（真 TCP）+ 节点侧真泵（run_node_link 回放面挂节点自身
//! router）——浏览器请求经网关 `/api/instances/{id}/p/*` 打包 http_req 注入
//! mux，节点本地 Router oneshot 重放后回注。验证 N1「可浏览」主链 + 边界
//! （离线 503 / 超帧 413 / 走私路径 / WS events 随节点断开关闭 / chat 503）。
//!
//! 环境全隔离：双侧数据目录 tempfile、网关随机口；节点 API 不监听（重放
//! 在节点进程内 oneshot，真实部署形态亦如此）。

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use futures::StreamExt;
use server::{
    api::AppState,
    config::Config,
    instances::{InstanceConfig, InstanceMode, InstancesStore},
    nodelink::{
        client::{run_node_link, NodeLinkParams, Timings},
        node_config::NodeConfig,
    },
    router,
};

fn cfg_with(dir: &Path) -> Config {
    let mut cfg = Config::load();
    cfg.claude_bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-claude.sh");
    cfg.jobs_dir = dir.join("jobs");
    cfg.instances_file = dir.join("instances.json");
    cfg.nodes_file = dir.join("nodes.json");
    cfg.node_file = dir.join("node.json");
    cfg.tasks_dir = dir.join("tasks");
    cfg.notes_file = dir.join("notes.json");
    cfg.projects_file = dir.join("projects.json");
    cfg
}

async fn spawn_hub(dir: &Path) -> (u16, Arc<AppState>) {
    let state = Arc::new(AppState::from_config(cfg_with(dir)));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let serve_state = state.clone();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router(serve_state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (port, state)
}

async fn api_oneshot(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
    body: Option<String>,
) -> (axum::http::StatusCode, String) {
    use tower::ServiceExt;
    let builder = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let req = match body {
        Some(b) => builder.body(axum::body::Body::from(b)).unwrap(),
        None => builder.body(axum::body::Body::empty()).unwrap(),
    };
    let res = router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// 等条件成立（100ms 轮询，deadline 兜底 panic）
async fn wait_for(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let c = cond();
        if c {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("{what}: 条件未在 {secs}s 内成立");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// 全链主测：enroll → approve（实例挂载）→ 泵换 token 在线 → 经 /p 浏览节点
/// agents（N1 锚）→ POST body 保真（实例创建落到节点侧）→ 走私路径拒绝 →
/// 节点停泵后离线 503
#[tokio::test]
async fn mux_proxy_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let (port_g, gateway) = spawn_hub(dir.path()).await;

    // enrollment：API 生成 code
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    // 节点侧：身份文件预置（网关 loopback 地址合法）+ 真泵（自身 router 回放）
    let node_dir = tempfile::tempdir().unwrap();
    let node_state = Arc::new(AppState::from_config(cfg_with(node_dir.path())));
    let node_file = node_dir.path().join("node.json");
    let node_id = "n-proxy-1";
    NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: node_id.into(),
        node_name: "proxy-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: node_id.into(),
        node_name: "proxy-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: node_state
            .drivers
            .names()
            .iter()
            .map(|s| s.to_string())
            .collect(),
        hub_version: "0.0.0".into(),
        router: router(node_state.clone()),
        local_port: 0,
        events_tx: None,
    };
    let pump = tokio::spawn(run_node_link(
        params,
        node_file,
        Timings {
            tick: Duration::from_millis(5),
            dead_after: Duration::from_millis(80),
            dial_timeout: Duration::from_millis(500),
        },
    ));

    // 首连 pending → approve（实例挂载，N1 前半）
    wait_for("节点首连 pending", 10, || {
        gateway
            .nodes
            .get(node_id)
            .map(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
            .unwrap_or(false)
    })
    .await;
    let (status, body) = api_oneshot(
        &gateway,
        "POST",
        &format!("/api/nodes/{node_id}/approve"),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");

    // 泵退避重连换取 token 并在线（approve 语义闭环）
    wait_for("节点在线（换 token 后常态会话）", 10, || {
        gateway.node_conns.is_online(node_id)
    })
    .await;
    let (status, body) = api_oneshot(&gateway, "GET", "/api/instances", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        body.contains(node_id) && body.contains("node-link"),
        "approve 后实例表应含 node-link: {body}"
    );

    // N1「可浏览」锚：经 /p 取节点 agents（节点 jobs 隔离 → 空清单 200）
    let (status, body) = api_oneshot(
        &gateway,
        "GET",
        &format!("/api/instances/{node_id}/p/api/agents?all=1"),
        None,
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "mux 代理取数失败: {body}"
    );
    let arr = serde_json::from_str::<serde_json::Value>(&body).unwrap();
    assert!(
        arr.is_array(),
        "节点 agents 应为数组（节点 jobs 隔离 → array，空或带样本均合法）: {body}"
    );

    // POST body 保真：经 /p 在节点侧创建实例 → 节点侧实例文件真实落盘
    let inst_body = serde_json::json!({
        "id": "i-node-local", "name": "via-mux", "mode": "direct",
        "url": "http://127.0.0.1:1", "token": null, "ssh": null,
        "remotePort": null, "localPort": null, "nodeMeta": null,
    });
    let (status, body) = api_oneshot(
        &gateway,
        "POST",
        &format!("/api/instances/{node_id}/p/api/instances"),
        Some(inst_body.to_string()),
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::OK,
        "POST 经 mux 失败: {body}"
    );
    let node_inst_raw = std::fs::read_to_string(node_dir.path().join("instances.json")).unwrap();
    assert!(
        node_inst_raw.contains("i-node-local"),
        "body 应真实落到节点实例文件: {node_inst_raw}"
    );

    // 走私路径不触达节点（SSRF 防线，proxy 既有锚形态）
    let (status, _) = api_oneshot(
        &gateway,
        "GET",
        &format!("/api/instances/{node_id}/p/https://evil.com/x"),
        None,
    )
    .await;
    assert!(
        status == axum::http::StatusCode::BAD_REQUEST
            || status == axum::http::StatusCode::NOT_FOUND,
        "走私路径应拒绝: {status}"
    );

    // 超帧 413（P1 边界）：>24MB body 拒绝
    let big = vec![b'x'; 25 * 1024 * 1024];
    let (status, body) = api_oneshot(
        &gateway,
        "POST",
        &format!("/api/instances/{node_id}/p/api/instances"),
        Some(String::from_utf8_lossy(&big).into_owned()),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::PAYLOAD_TOO_LARGE, "{body}");

    // 节点离线 → 代理 503（对齐隧道离线语义）
    pump.abort();
    wait_for("节点摘表", 5, || !gateway.node_conns.is_online(node_id)).await;
    let (status, body) = api_oneshot(
        &gateway,
        "GET",
        &format!("/api/instances/{node_id}/p/api/agents"),
        None,
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "{body}"
    );
    assert!(body.contains("节点离线"), "503 文案: {body}");
}

/// 任务 9 在线信号：/p/ws/events 随节点断开关闭（置灰载体）；
/// chat WS 503 显式 P2 文案
#[tokio::test]
async fn events_ws_holds_and_closes_with_node() {
    let dir = tempfile::tempdir().unwrap();
    let (port_g, gateway) = spawn_hub(dir.path()).await;
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    let node_dir = tempfile::tempdir().unwrap();
    let node_state = Arc::new(AppState::from_config(cfg_with(node_dir.path())));
    let node_file = node_dir.path().join("node.json");
    let node_id = "n-events-1";
    NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: node_id.into(),
        node_name: "events-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: node_id.into(),
        node_name: "events-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(node_state.clone()),
        local_port: 0,
        events_tx: None,
    };
    let pump = tokio::spawn(run_node_link(
        params,
        node_file,
        Timings {
            tick: Duration::from_millis(5),
            dead_after: Duration::from_millis(80),
            dial_timeout: Duration::from_millis(500),
        },
    ));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get(node_id)
            .map(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
            .unwrap_or(false)
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "POST",
        &format!("/api/nodes/{node_id}/approve"),
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || gateway.node_conns.is_online(node_id)).await;

    // 在线时 /p/ws/events 升级成功（连接保持）
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{port_g}/api/instances/{node_id}/p/ws/events"
    ))
    .await
    .expect("events WS 应随在线节点升级成功");

    // 节点停泵 → mux 断 → holder 关浏览器侧 WS（置灰载体）
    pump.abort();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => panic!("节点断开后 events WS 未关闭（置灰载体失效）"),
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(_))) => continue, // ping/pong 期内的帧
        }
    }

    // 离线时升级 → 503
    let res = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{port_g}/api/instances/{node_id}/p/ws/events"
    ))
    .await;
    assert!(res.is_err(), "离线节点 events WS 升级应失败（503）");
}

/// WS 路由边界（P2 后语义）：chat 已入 mux 白名单——离线节点 503「节点离线」
/// （显式不误报，对齐隧道离线语义）；未放行 ws/ 路径仍 503「未开放」。
/// 实例种子必须先落：代理入口先解析实例配置（缺实例 404 先于 WS 边界判定）。
#[tokio::test]
async fn chat_ws_gives_explicit_p2_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let (_port_g, gateway) = spawn_hub(dir.path()).await;
    gateway.nodes.upsert_pending("n-x", "x", "test", &[], None);
    gateway.nodes.approve("n-x").unwrap();
    gateway.nodes.issue_token("n-x", "tok");
    InstancesStore::new(&gateway.cfg)
        .upsert(&InstanceConfig {
            id: "n-x".into(),
            name: "x".into(),
            mode: InstanceMode::NodeLink,
            url: None,
            token: None,
            ssh: None,
            remote_port: None,
            local_port: None,
            node_meta: None,
        })
        .await
        .unwrap();
    // chat 已入 mux 白名单：离线节点 → 503 节点离线（显式语义，非裸断）
    let (status, body) =
        api_oneshot(&gateway, "GET", "/api/instances/n-x/p/ws/chat/sess-1", None).await;
    assert_eq!(
        status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "{body}"
    );
    assert!(body.contains("节点离线"), "离线文案: {body}");
    // 未放行 ws/ 路径 → 503 未开放文案（P2 边界收口锚）
    let (status, body) =
        api_oneshot(&gateway, "GET", "/api/instances/n-x/p/ws/other-route", None).await;
    assert_eq!(
        status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "{body}"
    );
    assert!(body.contains("未开放"), "未放行文案: {body}");
}
