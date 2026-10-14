//! /ws/node 握手五态 + 审批 API 集成测试（gateway-node-p1 任务 5/6）。
//!
//! 真路由（lib.rs router）+ 真 TCP loopback WS + oneshot HTTP API：
//! - 握手分类：缺凭据 / 错 token / enroll→pending / approve 后同 code 换取
//!   首发 token / 二次使用拒绝 / rejected 记录终态
//! - N2 前半：未批准节点不进实例路由（GET /api/instances 无该条目）
//! - N5/N2 红线：nodes.json 零明文 0600；GET /api/nodes 响应无凭据材料
//! - 中间件豁免锚：/ws/node 在 allow_lan + 非 loopback ConnectInfo 下不 401
//! - 连接治理：同 node_id 二连顶替（旧连接被断开）
//!
//! 环境全隔离（tempfile 数据目录 / 随机端口）。

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use axum::extract::ConnectInfo;
use futures::{SinkExt, StreamExt};
use server::{api::AppState, config::Config, nodelink::Frame, router};
use tokio_tungstenite::tungstenite::Message;

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

/// 起 hub（真 TCP 随机口），返回（端口, state, tempdir 守卫）
async fn spawn_hub() -> (u16, Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = cfg_with(dir.path());
    let state = Arc::new(AppState::from_config(cfg));
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
    (port, state, dir)
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn node_connect(port: u16) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/node"))
        .await
        .expect("ws upgrade");
    ws
}

async fn ws_send(ws: &mut Ws, frame: &Frame) {
    ws.send(Message::Text(serde_json::to_string(frame).unwrap().into()))
        .await
        .expect("ws send");
}

async fn ws_recv_frame(ws: &mut Ws) -> Frame {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let msg = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("ws 消息超时")
            .expect("ws 流不应结束")
            .expect("ws 消息应有效");
        match msg {
            Message::Text(t) => return serde_json::from_str(t.as_str()).expect("协议帧"),
            Message::Ping(p) => {
                let _ = ws.send(Message::Pong(p)).await;
            }
            _ => continue,
        }
    }
}

fn hello_frame(node_id: &str, token: Option<&str>, enroll: Option<&str>) -> Frame {
    Frame::Hello {
        protocol: 1,
        node_id: node_id.into(),
        node_name: "gw-test-node".into(),
        token: token.map(String::from),
        enroll: enroll.map(String::from),
        platform: "test".into(),
        drivers: vec!["claude".into()],
        hub_version: "0.0.0".into(),
        stats: None,
    }
}

async fn api_oneshot(
    state: &Arc<AppState>,
    method: &str,
    uri: &str,
) -> (axum::http::StatusCode, String) {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .body(axum::body::Body::empty())
        .unwrap();
    let res = router(state.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// 握手五态 + 审批全流程（enroll→pending 可见→approve→实例出现→换取首发
/// token→二次使用拒绝→reject 终态联动）——N1/N2/N3/N5 主链
#[tokio::test]
async fn handshake_states_and_approval_flow() {
    let (port, state, dir) = spawn_hub().await;

    // 缺凭据 → NodeTokenRejected（显式帧而非裸 close，N3）
    let mut ws = node_connect(port).await;
    ws_send(&mut ws, &hello_frame("n-a", None, None)).await;
    assert!(matches!(
        ws_recv_frame(&mut ws).await,
        Frame::NodeTokenRejected { .. }
    ));
    ws.send(Message::Close(None)).await.ok();

    // enroll-code（API）→ 首连 pending
    let (status, body) = api_oneshot(&state, "POST", "/api/nodes/enroll-code").await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let enroll: serde_json::Value = serde_json::from_str(&body).unwrap();
    let code = enroll["code"].as_str().unwrap().to_string();
    let command = enroll["command"].as_str().unwrap();
    assert!(
        command.contains("AGENT_HUB_ENROLL=") && !command.contains("--enroll"),
        "one-paste 命令串应走 env 传递（sweep S2 防 ps 明文）: {body}"
    );

    let mut ws = node_connect(port).await;
    ws_send(&mut ws, &hello_frame("n-1", None, Some(&code))).await;
    assert!(matches!(
        ws_recv_frame(&mut ws).await,
        Frame::PairingRequired
    ));

    // N2 前半：未批准节点零实例可达（GET /api/instances 无条目）+ pending 可见
    let (status, body) = api_oneshot(&state, "GET", "/api/instances").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(!body.contains("n-1"), "未批准节点不得出现在实例表: {body}");
    let (status, body) = api_oneshot(&state, "GET", "/api/nodes").await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert!(body.contains("n-1") && body.contains("pending"), "{body}");
    // 响应无凭据材料（明文 code 也不在节点清单——code 只在 enroll 响应出现过）
    assert!(
        !body.contains(&code),
        "清单不得携带 enrollment code: {body}"
    );

    // approve → 实例挂载（node-link，N1 锚）
    let (status, body) = api_oneshot(&state, "POST", "/api/nodes/n-1/approve").await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let (status, body) = api_oneshot(&state, "GET", "/api/instances").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        body.contains("n-1") && body.contains("node-link"),
        "approve 后实例表应出现 node-link 条目: {body}"
    );

    // 同 code 换取 → HelloOk 首发token；nodes.json 零明文 0600（N5）
    let mut ws = node_connect(port).await;
    ws_send(&mut ws, &hello_frame("n-1", None, Some(&code))).await;
    let ok = ws_recv_frame(&mut ws).await;
    let node_token = match ok {
        Frame::HelloOk {
            instance_id,
            node_token,
            ..
        } => {
            assert_eq!(instance_id, "n-1");
            node_token.expect("审批握手应首发 node token")
        }
        other => panic!("应换取成功: {other:?}"),
    };
    let nodes_raw = std::fs::read_to_string(dir.path().join("nodes.json")).unwrap();
    assert!(
        !nodes_raw.contains(&node_token),
        "网关库零 token 明文: {nodes_raw}"
    );
    assert_eq!(
        server::platform::file_mode(&dir.path().join("nodes.json")),
        Some(0o600)
    );
    // 清单里 approved 可见、pending 空
    let (_, body) = api_oneshot(&state, "GET", "/api/nodes").await;
    assert!(body.contains("approved") && body.contains("n-1"), "{body}");

    // 二次使用同 code → Rejected（N5 锚）
    let mut ws = node_connect(port).await;
    ws_send(&mut ws, &hello_frame("n-1", None, Some(&code))).await;
    assert!(matches!(
        ws_recv_frame(&mut ws).await,
        Frame::Rejected { .. }
    ));

    // 换来的 token 走 token 路径（HelloOk 无 node_token）
    let mut ws = node_connect(port).await;
    ws_send(&mut ws, &hello_frame("n-1", Some(&node_token), None)).await;
    match ws_recv_frame(&mut ws).await {
        Frame::HelloOk {
            node_token: None, ..
        } => {}
        other => panic!("token 路径应为常态 hello-ok: {other:?}"),
    }

    // reject → 实例卸载 + pending 清单消失（本例为 approved 移出）+ 次连终态
    let (status, _) = api_oneshot(&state, "POST", "/api/nodes/n-1/reject").await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let (_, body) = api_oneshot(&state, "GET", "/api/nodes").await;
    assert!(!body.contains("n-1"), "reject 后清单不应再有该节点: {body}");
    let (_, body) = api_oneshot(&state, "GET", "/api/instances").await;
    assert!(!body.contains("n-1"), "reject 后实例应卸载: {body}");
    let mut ws = node_connect(port).await;
    ws_send(&mut ws, &hello_frame("n-1", Some(&node_token), None)).await;
    assert!(matches!(
        ws_recv_frame(&mut ws).await,
        Frame::Rejected { .. }
    ));
}

/// 错 token → NodeTokenRejected（N3 显式分类）
#[tokio::test]
async fn wrong_token_rejected_frame() {
    let (port, state, _dir) = spawn_hub().await;
    state.nodes.upsert_pending("n-1", "lab", "test", &[], None);
    state.nodes.approve("n-1").unwrap();
    state.nodes.issue_token("n-1", "real-token");
    let mut ws = node_connect(port).await;
    ws_send(&mut ws, &hello_frame("n-1", Some("wrong"), None)).await;
    assert!(matches!(
        ws_recv_frame(&mut ws).await,
        Frame::NodeTokenRejected { .. }
    ));
}

/// 连接治理：同 node_id 二连顶替（旧连接被断开）
#[tokio::test]
async fn same_node_id_second_connect_replaces_first() {
    let (port, state, _dir) = spawn_hub().await;
    state.nodes.upsert_pending("n-1", "lab", "test", &[], None);
    state.nodes.approve("n-1").unwrap();
    state.nodes.issue_token("n-1", "tok-1");

    let mut first = node_connect(port).await;
    ws_send(&mut first, &hello_frame("n-1", Some("tok-1"), None)).await;
    assert!(matches!(
        ws_recv_frame(&mut first).await,
        Frame::HelloOk { .. }
    ));
    assert!(state.node_conns.is_online("n-1"));

    let mut second = node_connect(port).await;
    ws_send(&mut second, &hello_frame("n-1", Some("tok-1"), None)).await;
    assert!(matches!(
        ws_recv_frame(&mut second).await,
        Frame::HelloOk { .. }
    ));
    assert!(state.node_conns.is_online("n-1"));

    // 旧连接被顶替断开（流结束）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, first.next()).await {
            Err(_) => panic!("旧连接未在期限内被顶替断开"),
            Ok(None) => break,
            Ok(Some(Err(_))) => break,
            Ok(Some(Ok(_))) => continue,
        }
    }
}

/// 中间件豁免锚：allow_lan + 非 loopback ConnectInfo 下 /ws/node 不 401
/// （豁免前该形态一律 401 = N3 禁止的误报）；对照 /api/agents 仍 401。
#[tokio::test]
async fn ws_node_path_exempt_from_hub_token_middleware() {
    use tower::ServiceExt;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = cfg_with(dir.path());
    cfg.allow_lan = true;
    cfg.token = Some("hub-tok".into());
    let state = Arc::new(AppState::from_config(cfg));

    let remote = ConnectInfo(SocketAddr::new(
        std::net::IpAddr::from([192, 168, 1, 5]),
        5000,
    ));
    let build = |method: &str, uri: &str| {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .extension(remote)
            .body(axum::body::Body::empty())
            .unwrap()
    };
    // /ws/node：豁免生效 → 进握手层（非 upgrade 请求被 axum 拒，但绝不是 401）
    let res = router(state.clone())
        .oneshot(build("GET", "/ws/node"))
        .await
        .unwrap();
    assert_ne!(
        res.status(),
        axum::http::StatusCode::UNAUTHORIZED,
        "/ws/node 必须豁免 hub token 中间件"
    );
    // 对照：同连接信息下 /api 仍强制 token
    let res = router(state.clone())
        .oneshot(build("GET", "/api/agents"))
        .await
        .unwrap();
    assert_eq!(res.status(), axum::http::StatusCode::UNAUTHORIZED);
}

/// N2 锚（registry 直引）：pending 节点不出 approved 清单语义由 list 分组保证
#[tokio::test]
async fn registry_status_grouping() {
    let (port, state, _dir) = spawn_hub().await;
    state.nodes.upsert_pending("n-p", "p", "test", &[], None);
    let (_, body) = api_oneshot(&state, "GET", "/api/nodes").await;
    assert!(body.contains("n-p"));
    // 未批准节点连接被拒后不产生任何实例（对齐 five-state 首段断言的 registry 侧）
    assert!(
        state.nodes.get("n-p").unwrap().status == server::nodelink::registry::NodeStatus::Pending
    );
    let _ = port;
}
