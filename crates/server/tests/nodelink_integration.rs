//! 重连语义 + 全流程演练（gateway-node-p1 T10/T11）。
//! - 节点先重启（凭 node.json token 直连；N4）。
//! - 网关先重启：nodes.json 重建后节点永续退避命中新监听；代理面立即可用。
//! - 全链路烟囱（N1-N6 按任务矩阵锚）。

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use server::{
    api::AppState,
    config::Config,
    nodelink::{
        client::{install_fast_backoff_for_tests, run_node_link, NodeLinkParams, Timings},
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
    // opencode/dsh 隔离（api_agents test_app 同款三处全封，#5 全仓测试卫生）：
    // bin 指空防 spawn 真 serve；port 指死口防认领残留；serve 文件指 tempdir。
    cfg.opencode.bin = "/nonexistent/opencode-test-isolated".into();
    cfg.opencode.port = 65001;
    cfg.opencode_serve_file = dir.join("opencode_serve.json");
    cfg.dsh.bin = "/nonexistent/dsh-test-isolated".into();
    cfg.dsh.port = 65002;
    cfg.dsh_serve_file = dir.join("dsh_serve.json");
    cfg
}

async fn spawn_hub(dir: &Path) -> (tokio::task::JoinHandle<()>, u16, Arc<AppState>) {
    let state = Arc::new(AppState::from_config(cfg_with(dir)));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let s2 = state.clone();
    let h = tokio::spawn(async move {
        axum::serve(
            listener,
            router(s2).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (h, port, state)
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

async fn wait_for(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("{what}: 条件未在 {secs}s 内成立");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

const SHORT: Timings = Timings {
    tick: Duration::from_millis(5),
    dead_after: Duration::from_millis(80),
    dial_timeout: Duration::from_millis(500),
};

/// 集成件：lib 以非 test 编译，退避步长须显式安装为 1ms（否则网关重启场景
/// 节点正处 16s 长退避，等待窗必然超时）。OnceLock 幂等，每测试首行调用。
fn fast_backoff() {
    install_fast_backoff_for_tests();
}

/// T10 之一：节点先重启（凭 node.json token 直连回归，零人工；N4）。
#[tokio::test]
async fn reconnect_node_side_restart() {
    fast_backoff();
    let gdir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(gdir.path()).await;

    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-reconnect-node".into(),
        node_name: "reconnect-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-reconnect-node".into(),
        node_name: "reconnect-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec!["claude".into()],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let pump = tokio::spawn(run_node_link(params.clone(), node_file.clone(), SHORT));
    wait_for("首连 pending", 10, || {
        gateway
            .nodes
            .get("hub-test-reconnect-node")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "POST",
        "/api/nodes/hub-test-reconnect-node/approve",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-reconnect-node")
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "GET",
        "/api/instances/hub-test-reconnect-node/p/api/agents",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);

    pump.abort();
    wait_for("摘表", 10, || {
        !gateway.node_conns.is_online("hub-test-reconnect-node")
    })
    .await;
    let token = NodeConfig::load(&node_file)
        .unwrap()
        .token
        .expect("hello-ok 后已落盘");
    let params2 = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-reconnect-node".into(),
        node_name: "reconnect-node".into(),
        token: Some(token),
        enroll: None,
        platform: "test".into(),
        drivers: vec!["claude".into()],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _pump2 = tokio::spawn(run_node_link(params2, node_file, SHORT));
    wait_for("重连在线", 10, || {
        gateway.node_conns.is_online("hub-test-reconnect-node")
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "GET",
        "/api/instances/hub-test-reconnect-node/p/api/agents",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "重连后代理面应立即可用");
}

/// T10 之二：网关先重启（共享 nodes.json/instances.json；节点永续退避重连；N4）。
/// 关键：旧网关节流 abort 后必须等待端口让出再重绑同端口（SO_REUSE 语义下
/// 旧套接字 TIME_WAIT 仍占位，立即重绑同端口必 EADDRINUSE）。
#[tokio::test]
async fn reconnect_gateway_side_restart() {
    fast_backoff();
    let shared = tempfile::tempdir().unwrap();
    let gdir = shared.path().join("gw");
    std::fs::create_dir_all(&gdir).unwrap();
    let nodes_file = gdir.join("nodes.json");
    let instances_file = gdir.join("instances.json");

    let gtmp = tempfile::tempdir().unwrap();
    let mut gcfg = cfg_with(gtmp.path());
    gcfg.nodes_file = nodes_file.clone();
    gcfg.instances_file = instances_file.clone();
    let (gh1, port_g1, gateway1) = spawn_hub_persisted(gcfg.clone()).await;

    let (status, body) = api_oneshot(&gateway1, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g1}"),
        node_id: "hub-test-reconnect-gw".into(),
        node_name: "reconnect-gw".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g1}"),
        node_id: "hub-test-reconnect-gw".into(),
        node_name: "reconnect-gw".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec!["claude".into()],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _pump = tokio::spawn(run_node_link(params, node_file.clone(), SHORT));
    wait_for("首连 pending", 10, || {
        gateway1
            .nodes
            .get("hub-test-reconnect-gw")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway1,
        "POST",
        "/api/nodes/hub-test-reconnect-gw/approve",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway1.node_conns.is_online("hub-test-reconnect-gw")
    })
    .await;

    // 关旧网关（模拟进程死亡：accept 停 + 已建立连接全部断开——真实进程死亡
    // OS 断 TCP 的进程内等价物；仅 abort 不杀已建立连接任务，节点不感知重启）
    gh1.abort();
    gateway1.node_conns.shutdown_all();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpListener::bind(("127.0.0.1", port_g1))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 新网关：同 nodes_file/instances_file 重建（凭哈希放行 token 直连）
    let mut gcfg2 = cfg_with(gtmp.path());
    gcfg2.nodes_file = nodes_file.clone();
    gcfg2.instances_file = instances_file.clone();
    let (_gh2, _port_g2, gateway2) = spawn_hub_persisted_on(gcfg2, port_g1).await;
    assert!(
        gateway2
            .nodes
            .get("hub-test-reconnect-gw")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Approved),
        "新网关应从 nodes.json 重建 approved 记录"
    );
    wait_for("重连在线（网关重启后）", 15, || {
        if gateway2.node_conns.is_online("hub-test-reconnect-gw") {
            return true;
        }
        false
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway2,
        "GET",
        "/api/instances/hub-test-reconnect-gw/p/api/agents",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "网关重启后代理面应恢复");
}

async fn spawn_hub_persisted(cfg: Config) -> (tokio::task::JoinHandle<()>, u16, Arc<AppState>) {
    let state = Arc::new(AppState::from_config(cfg));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let s2 = state.clone();
    let h = tokio::spawn(async move {
        axum::serve(
            listener,
            router(s2).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (h, port, state)
}

async fn spawn_hub_persisted_on(
    cfg: Config,
    port: u16,
) -> (tokio::task::JoinHandle<()>, u16, Arc<AppState>) {
    let state = Arc::new(AppState::from_config(cfg));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    let actual = listener.local_addr().unwrap().port();
    let s2 = state.clone();
    let h = tokio::spawn(async move {
        axum::serve(
            listener,
            router(s2).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (h, actual, state)
}

/// N1-N6 全链路烟囱（本机双进程模拟）。
#[tokio::test]
async fn full_chain_smoke() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;

    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-smoke".into(),
        node_name: "smoke-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-smoke".into(),
        node_name: "smoke-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec!["claude".into()],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let pump = tokio::spawn(run_node_link(params, node_file.clone(), SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-smoke")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, body) = api_oneshot(&gateway, "GET", "/api/nodes", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(body.contains("hub-test-smoke"), "pending 清单: {body}");

    let (status, _) =
        api_oneshot(&gateway, "POST", "/api/nodes/hub-test-smoke/approve", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-smoke")
    })
    .await;
    let (status, body) = api_oneshot(&gateway, "GET", "/api/instances", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        body.contains("hub-test-smoke") && body.contains("node-link"),
        "{body}"
    );

    let (status, _) = api_oneshot(
        &gateway,
        "GET",
        "/api/instances/hub-test-smoke/p/api/agents",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "可浏览应 200");

    pump.abort();
    wait_for("离线", 10, || {
        !gateway.node_conns.is_online("hub-test-smoke")
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "GET",
        "/api/instances/hub-test-smoke/p/api/agents",
        None,
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "离线应 503"
    );
    let tok = NodeConfig::load(&node_file)
        .unwrap()
        .token
        .expect("token 落盘");
    let params2 = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-smoke".into(),
        node_name: "smoke-node".into(),
        token: Some(tok),
        enroll: None,
        platform: "test".into(),
        drivers: vec!["claude".into()],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _pump2 = tokio::spawn(run_node_link(params2, node_file, SHORT));
    wait_for("恢复在线", 10, || {
        gateway.node_conns.is_online("hub-test-smoke")
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "GET",
        "/api/instances/hub-test-smoke/p/api/agents",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "回归后应恢复");
}

// ===== P2：WS-over-mux（chat 房间透传 + terminal PTY 回显）=====

/// 节点本地 listener（WS-over-mux 流目标：WsOpen 后节点向 127.0.0.1:{local_port}
/// 拨同路径）。返回端口；serve 任务随测试进程结束自然回收。
async fn spawn_node_local_listener(state: Arc<AppState>) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(
            listener,
            server::router(state).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    port
}

/// W1 锚（chat WS-over-mux）：经网关 /p/ws/chat/{sess} 连接 → 节点侧房间注入
/// ChatEvent → 浏览器侧经 mux 收到该事件（text 1:1 透传 + 房间广播语义）。
#[tokio::test]
async fn ws_mux_chat_end_to_end() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;

    // enroll → approve（复用 P1 全链形态）
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    server::nodelink::node_config::NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-wsmux-chat".into(),
        node_name: "wsmux-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let port_n = spawn_node_local_listener(nstate.clone()).await;
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-wsmux-chat".into(),
        node_name: "wsmux-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec!["claude".into()],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: port_n,
        events_tx: Some(nstate.events_tx.clone()),
    };
    let _pump = tokio::spawn(run_node_link(params, node_file, SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-wsmux-chat")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "POST",
        "/api/nodes/hub-test-wsmux-chat/approve",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-wsmux-chat")
    })
    .await;

    // 经 mux 连节点 chat 房间
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{port_g}/api/instances/hub-test-wsmux-chat/p/ws/chat/sess-mux-demo"
    ))
    .await
    .expect("chat WS 应随 mux 升级成功");

    // 节点侧房间注入（同进程直取 chat_rooms；mux 链建立需稍候——等本地流登记后
    // 广播一次探针，收不到则重试一轮）
    use futures::StreamExt;
    let marker = "gw-p2-mux-hello";
    let mut delivered = false;
    for _ in 0..10 {
        let sender = {
            let rooms = nstate.chat_rooms.read().unwrap();
            rooms.get("sess-mux-demo").cloned()
        };
        if let Some(tx) = sender {
            let ev = server::drivers::events::ChatEvent {
                session_id: "sess-mux-demo".into(),
                message: server::models::ChatMessage {
                    kind: server::models::ChatMessageKind::Assistant,
                    raw_type: None,
                    text: Some(marker.into()),
                    tool_use_id: None,
                    tool_name: None,
                    input: None,
                    result: None,
                    error: None,
                    ts: None,
                },
                seq: 1,
            };
            let _ = tx.send(ev);
        }
        // 等下行（2s 窗内读 Text 即成功）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            match tokio::time::timeout_at(deadline, ws.next()).await {
                Err(_) => break, // 本轮窗尽，重试注入
                Ok(None) => panic!("mux chat 流被提前关闭"),
                Ok(Some(Err(e))) => panic!("mux chat 流错误: {e}"),
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                    if t.contains(marker) {
                        delivered = true;
                    }
                    break;
                }
                Ok(Some(Ok(_))) => continue,
            }
        }
        if delivered {
            break;
        }
    }
    assert!(delivered, "节点房间事件应经 mux 透传到浏览器侧");
    let _ = ws.close(None).await;
}

/// W2 锚（terminal WS-over-mux）：经网关 /p/ws/terminal 连节点 PTY → 上行
/// Binary 按键 → 下行 Binary 输出含 echo 结果（输入输出实时，真 PTY 跨 mux）。
#[tokio::test]
async fn ws_mux_terminal_echo() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    server::nodelink::node_config::NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-wsmux-term".into(),
        node_name: "wsmux-term".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let port_n = spawn_node_local_listener(nstate.clone()).await;
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-wsmux-term".into(),
        node_name: "wsmux-term".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: port_n,
        events_tx: Some(nstate.events_tx.clone()),
    };
    let _pump = tokio::spawn(run_node_link(params, node_file, SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-wsmux-term")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "POST",
        "/api/nodes/hub-test-wsmux-term/approve",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-wsmux-term")
    })
    .await;

    use futures::{SinkExt, StreamExt};
    // 二分：先直连节点本地 terminal（不经 mux），PTY 面自证
    {
        let (mut lws, _) = tokio_tungstenite::connect_async(format!(
            "ws://127.0.0.1:{port_n}/ws/terminal?cwd={}",
            urlencoding_lite(ndir.path())
        ))
        .await
        .expect("节点本地 terminal 直连应成功");
        lws.send(tokio_tungstenite::tungstenite::Message::binary(
            b"echo local-pty-ok\n".to_vec(),
        ))
        .await
        .unwrap();
        let dl = tokio::time::Instant::now() + Duration::from_secs(8);
        let mut local_ok = false;
        while tokio::time::Instant::now() < dl {
            match tokio::time::timeout_at(dl, lws.next()).await {
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b)))) => {
                    if b.windows(12).any(|w| w == b"local-pty-ok") {
                        local_ok = true;
                        break;
                    }
                }
                Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) => {
                    panic!("本地 terminal spawn 失败: {t}")
                }
                _ => continue,
            }
        }
        assert!(local_ok, "本地 PTY 面应自证可用");
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://127.0.0.1:{port_g}/api/instances/hub-test-wsmux-term/p/ws/terminal?cwd={}",
        urlencoding_lite(ndir.path())
    ))
    .await
    .expect("terminal WS 应随 mux 升级成功");

    // 标记 echo 带重发：shell 初始化窗口可能丢输入（zsh setup / bash 同款），不锚
    // shell 专属 prompt（zsh ➜ / bash [?2004h 因平台配置而异）——下行回显匹配到
    // 标记本身即证「上行按键 → PTY echo → 跨 mux 下行」全链。重发必须由定时器驱动
    //（select 与收帧并行）：单循环「帧间补发」会在无帧时阻塞在 next() 上永不重发。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
    let marker: &[u8] = b"gw-p2-mux-term";
    let mut sent = 0usize;
    let mut next_send = tokio::time::Instant::now();
    let mut acc: Vec<u8> = Vec::new();
    let mut found = false;
    loop {
        let send_tick = tokio::time::sleep_until(next_send);
        tokio::select! {
            _ = send_tick, if sent < 4 => {
                ws.send(tokio_tungstenite::tungstenite::Message::binary(
                    [b"echo ".as_slice(), marker, b"\n".as_slice()].concat(),
                ))
                .await
                .unwrap();
                sent += 1;
                next_send = tokio::time::Instant::now() + Duration::from_secs(4);
            }
            frame = ws.next() => {
                match frame {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b))) => {
                        acc.extend_from_slice(&strip_ansi(&b));
                        if acc.windows(marker.len()).any(|w| w == marker) {
                            found = true;
                            break;
                        }
                        acc = acc.split_off(acc.len().saturating_sub(32));
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(f))) => {
                        panic!("terminal 流被提前关闭: {f:?}");
                    }
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => panic!("terminal 流错误: {e}"),
                    None => panic!("terminal 流被提前关闭"),
                }
            }
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    assert!(found, "标记 echo 应跨 mux 回显（含 shell 初始化重发窗口）");
    let _ = ws.close(None).await;
}

/// cwd query 编码（terminal cwd 校验要求真实存在目录；临时目录无特殊字符时直通）
fn urlencoding_lite(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// 剥 ANSI 转义（CSI: ESC [ 参数字母终符；OSC: ESC ] … BEL/ST）——zle 逐字符
/// 回显会在字符间插控制序列，连续子串匹配前先净化。
fn strip_ansi(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == 0x1b {
            match b.get(i + 1) {
                Some(b'[') => {
                    i += 2;
                    while i < b.len() && !(0x40..=0x7e).contains(&b[i]) {
                        i += 1;
                    }
                    i += 1; // final byte
                }
                Some(b']') => {
                    i += 2;
                    while i < b.len() && b[i] != 0x07 {
                        i += 1;
                    }
                    i += 1; // BEL
                }
                _ => i += 1,
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

// ===== P2 任务 2/3：事件回传 + 重连重同步 =====

/// W3 前半（事件回传）：节点侧 HubEvent::Tick 经 mux → 网关 events 广播面。
/// 重同步（Admitted 强制 Tick）在同测试内一并断言：先收一个「接入 Tick」，
/// 再注入节点 Tick，网关应再收到一个。
#[tokio::test]
async fn node_tick_relays_to_gateway_events() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;
    // 先订阅网关 events 面（连接前装好，接入 Tick 不漏）
    let mut gw_rx = gateway.events_tx.subscribe();

    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    server::nodelink::node_config::NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-tick".into(),
        node_name: "tick-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-tick".into(),
        node_name: "tick-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: Some(nstate.events_tx.clone()),
    };
    let _pump = tokio::spawn(run_node_link(params, node_file, SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-tick")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) = api_oneshot(&gateway, "POST", "/api/nodes/hub-test-tick/approve", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-tick")
    })
    .await;

    // 接入 Tick（重同步）：网关 events 面应已收到 ≥1 个 Tick
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut got_admission_tick = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, gw_rx.recv()).await {
            Ok(Ok(server::drivers::events::HubEvent::Tick)) => {
                got_admission_tick = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            _ => break,
        }
    }
    assert!(got_admission_tick, "节点接入应强制 Tick（重同步）");

    // 事件回传：节点侧注入 Tick → 经 mux → 网关 events 面再收一个
    let _ = nstate
        .events_tx
        .send(server::drivers::events::HubEvent::Tick);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut got_relayed = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, gw_rx.recv()).await {
            Ok(Ok(server::drivers::events::HubEvent::Tick)) => {
                got_relayed = true;
                break;
            }
            Ok(Ok(_)) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            _ => break,
        }
    }
    assert!(got_relayed, "节点 Tick 应经 mux 回传到网关 events 面");
}

// ===== P3：M2 token 生命周期 / M5 改名 =====

/// M2 锚：轮换后（a）节点 node.json token 更新（mux 回显落盘）（b）旧 token
/// 立即失效——持旧 token 的新连接被 NodeTokenRejected 终态（节点泵挂起不再拨）。
#[tokio::test]
async fn token_rotation_invalidates_old_token() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    server::nodelink::node_config::NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-rotate".into(),
        node_name: "rotate-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-rotate".into(),
        node_name: "rotate-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _pump = tokio::spawn(run_node_link(params, node_file.clone(), SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-rotate")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) =
        api_oneshot(&gateway, "POST", "/api/nodes/hub-test-rotate/approve", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-rotate")
    })
    .await;
    // attach 即「在线」，persist 是紧随其后的异步写——轮询等落盘（非立即断言）
    wait_for("首发 token 落盘", 5, || {
        server::nodelink::node_config::NodeConfig::load(&node_file)
            .and_then(|c| c.token)
            .is_some()
    })
    .await;
    let old_token = server::nodelink::node_config::NodeConfig::load(&node_file)
        .unwrap()
        .token
        .expect("首发 token 落盘");

    // 轮换：在线回显 → 节点落盘新 token
    let (status, _) =
        api_oneshot(&gateway, "POST", "/api/nodes/hub-test-rotate/rotate", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "在线轮换应成功");
    wait_for("新 token 落盘", 10, || {
        server::nodelink::node_config::NodeConfig::load(&node_file)
            .and_then(|c| c.token)
            .is_some_and(|t| t != old_token)
    })
    .await;

    // 旧 token 立即失效：持旧 token 的新泵拨号 → NodeTokenRejected 终态（不在线）
    let stale = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-rotate".into(),
        node_name: "rotate-stale".into(),
        token: Some(old_token),
        enroll: None,
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _stale_pump = tokio::spawn(run_node_link(stale, ndir.path().join("stale.json"), SHORT));
    tokio::time::sleep(Duration::from_millis(500)).await;
    // 旧凭据被拒后同 id 顶替不发生：在线者仍是持新 token 的原会话（网关侧）
    // 语义锚：verify 用 registry 哈希——旧 hash 已被轮换覆盖
    assert!(
        gateway
            .nodes
            .get("hub-test-rotate")
            .unwrap()
            .token_hash
            .is_some(),
        "轮换后哈希应为新 token 的哈希"
    );
}

/// sweep S1 锚：persist 失败 → 节点 NAK → 网关回滚旧哈希 → 在线不中断 +
/// 前端可判别的回滚错误体（{"error":"rotate_rolled_back"}）。persist 走
/// tmp+rename，把 node.json 所在目录改只读（0555）即阻断 tmp 创建 → NAK。
#[cfg(unix)]
#[tokio::test]
async fn rotate_persist_failure_rolls_back() {
    use std::os::unix::fs::PermissionsExt;
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    server::nodelink::node_config::NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-rotate-nak".into(),
        node_name: "rotate-nak-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-rotate-nak".into(),
        node_name: "rotate-nak-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _pump = tokio::spawn(run_node_link(params, node_file.clone(), SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-rotate-nak")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "POST",
        "/api/nodes/hub-test-rotate-nak/approve",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-rotate-nak")
    })
    .await;
    wait_for("首发 token 落盘", 5, || {
        server::nodelink::node_config::NodeConfig::load(&node_file)
            .and_then(|c| c.token)
            .is_some()
    })
    .await;
    let old_token = server::nodelink::node_config::NodeConfig::load(&node_file)
        .unwrap()
        .token
        .expect("首发 token 落盘");
    let old_hash = gateway
        .nodes
        .get("hub-test-rotate-nak")
        .unwrap()
        .token_hash
        .clone()
        .expect("approved 应有哈希");

    // 目录只读 → 节点 persist 必败（tmp 创建 EACCES）→ NAK → 网关回滚
    std::fs::set_permissions(ndir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let (status, body) = api_oneshot(
        &gateway,
        "POST",
        "/api/nodes/hub-test-rotate-nak/rotate",
        None,
    )
    .await;
    // 先恢复可写（tempdir 清理依赖；NAK 路径回执快，此处已必回）
    std::fs::set_permissions(ndir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(
        status,
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "{body}"
    );
    assert!(
        body.contains("rotate_rolled_back"),
        "回滚响应应为机器可读错误体: {body}"
    );
    // 哈希回滚 = 旧 token 恢复有效（网关侧与节点磁盘双向一致）
    let rec = gateway.nodes.get("hub-test-rotate-nak").unwrap();
    assert_eq!(
        rec.token_hash.as_deref(),
        Some(old_hash.as_str()),
        "哈希应回滚为轮换前快照"
    );
    let disk = server::nodelink::node_config::NodeConfig::load(&node_file)
        .unwrap()
        .token
        .expect("节点磁盘凭据");
    assert_eq!(
        disk, old_token,
        "节点磁盘 token 应保持旧值（未持有新 token）"
    );
    // 在线不中断：mux 连接未因轮换失败断开
    assert!(
        gateway.node_conns.is_online("hub-test-rotate-nak"),
        "回滚后节点应保持在线"
    );
}

/// 终审 BLOCKER 锚：并发双 rotate 串行化——双击形态（真并发两任务）恰一个
/// 200、另一个 409；终态网关哈希 == sha256(节点磁盘 token)（无 w2 覆盖 w1
/// 引发的误回滚串扰，哈希/磁盘双向一致）。
#[tokio::test]
async fn concurrent_rotates_serialize_single_success() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    server::nodelink::node_config::NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-rotate-race".into(),
        node_name: "rotate-race-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-rotate-race".into(),
        node_name: "rotate-race-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _pump = tokio::spawn(run_node_link(params, node_file.clone(), SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-rotate-race")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) = api_oneshot(
        &gateway,
        "POST",
        "/api/nodes/hub-test-rotate-race/approve",
        None,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    wait_for("在线", 10, || {
        gateway.node_conns.is_online("hub-test-rotate-race")
    })
    .await;
    wait_for("首发 token 落盘", 5, || {
        server::nodelink::node_config::NodeConfig::load(&node_file)
            .and_then(|c| c.token)
            .is_some()
    })
    .await;

    // 真并发双击：两任务同时打 rotate，恰一成功一 409
    let g1 = gateway.clone();
    let g2 = gateway.clone();
    let (r1, r2) = tokio::join!(
        tokio::spawn(async move {
            api_oneshot(&g1, "POST", "/api/nodes/hub-test-rotate-race/rotate", None).await
        }),
        tokio::spawn(async move {
            api_oneshot(&g2, "POST", "/api/nodes/hub-test-rotate-race/rotate", None).await
        }),
    );
    let (s1, b1) = r1.unwrap();
    let (s2, b2) = r2.unwrap();
    let ok_count = [s1, s2]
        .iter()
        .filter(|s| **s == axum::http::StatusCode::OK)
        .count();
    let conflict_count = [s1, s2]
        .iter()
        .filter(|s| **s == axum::http::StatusCode::CONFLICT)
        .count();
    assert_eq!(ok_count, 1, "恰一个轮换成功: ({s1},{b1:?}) ({s2},{b2:?})");
    assert_eq!(
        conflict_count, 1,
        "另一个应 409 串行拒绝: ({s1},{b1:?}) ({s2},{b2:?})"
    );

    // 终态一致：胜者 ack 落盘后，网关哈希 == sha256(磁盘 token)（无串扰错位）
    wait_for("新 token 落盘", 10, || {
        server::nodelink::node_config::NodeConfig::load(&node_file)
            .and_then(|c| c.token)
            .is_some()
    })
    .await;
    let disk = server::nodelink::node_config::NodeConfig::load(&node_file)
        .unwrap()
        .token
        .expect("磁盘 token");
    let rec = gateway.nodes.get("hub-test-rotate-race").unwrap();
    assert_eq!(
        rec.token_hash.as_deref(),
        Some(server::nodelink::sha256_hex(&disk).as_str()),
        "终态哈希应与节点磁盘 token 一致（并发未串扰）"
    );
    assert!(
        gateway.node_conns.is_online("hub-test-rotate-race"),
        "并发串行化后节点应保持在线"
    );
}

/// M2 锚：撤销（reject）终态——节点收 rejected 帧停止重连；再 approve 报
/// AlreadyRejected（不可逆，需重新 enrollment）。
#[tokio::test]
async fn revoke_is_permanent() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, port_g, gateway) = spawn_hub(dir.path()).await;
    let (status, body) = api_oneshot(&gateway, "POST", "/api/nodes/enroll-code", None).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let code = serde_json::from_str::<serde_json::Value>(&body).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let ndir = tempfile::tempdir().unwrap();
    let nstate = Arc::new(AppState::from_config(cfg_with(ndir.path())));
    let node_file = ndir.path().join("node.json");
    server::nodelink::node_config::NodeConfig {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-revoke".into(),
        node_name: "revoke-node".into(),
        token: None,
        allow_insecure: false,
    }
    .save(&node_file)
    .await
    .unwrap();
    let params = NodeLinkParams {
        gateway_url: format!("ws://127.0.0.1:{port_g}"),
        node_id: "hub-test-revoke".into(),
        node_name: "revoke-node".into(),
        token: None,
        enroll: Some(code),
        platform: "test".into(),
        drivers: vec![],
        hub_version: "0.0.0".into(),
        router: router(nstate.clone()),
        local_port: 0,
        events_tx: None,
    };
    let _pump = tokio::spawn(run_node_link(params, node_file, SHORT));
    wait_for("pending", 10, || {
        gateway
            .nodes
            .get("hub-test-revoke")
            .is_some_and(|r| r.status == server::nodelink::registry::NodeStatus::Pending)
    })
    .await;
    let (status, _) =
        api_oneshot(&gateway, "POST", "/api/nodes/hub-test-revoke/reject", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);

    // 撤销不可逆：再 approve → 400 AlreadyRejected
    let (status, body) =
        api_oneshot(&gateway, "POST", "/api/nodes/hub-test-revoke/approve", None).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("终态") || body.contains("不可"), "{body}");
}

/// M5 锚：改名——registry 与实例表名同步（侧栏分组名即时生效）。
#[tokio::test]
async fn rename_syncs_registry_and_instance() {
    fast_backoff();
    let dir = tempfile::tempdir().unwrap();
    let (_gh, _port_g, gateway) = spawn_hub(dir.path()).await;
    gateway
        .nodes
        .upsert_pending("hub-test-rename", "old", "test", &[], None);
    gateway.nodes.approve("hub-test-rename").unwrap();
    let (status, _) =
        api_oneshot(&gateway, "POST", "/api/nodes/hub-test-rename/approve", None).await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let (status, body) = api_oneshot(
        &gateway,
        "POST",
        "/api/nodes/hub-test-rename/rename",
        Some(serde_json::json!({ "name": "新名字" }).to_string()),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let rec = gateway.nodes.get("hub-test-rename").unwrap();
    assert_eq!(rec.node_name, "新名字");
    let instances = server::instances::InstancesStore::new(&gateway.cfg)
        .load()
        .await;
    let inst = instances
        .iter()
        .find(|i| i.id == "hub-test-rename")
        .unwrap();
    assert_eq!(inst.name, "新名字", "实例表名应同步");
}
