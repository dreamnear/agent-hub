//! 同源实例代理（设备隔离修复）：ssh-tunnel 实例的隧道端口 bind 在 hub 本机
//! 127.0.0.1，浏览器（手机 frp / LAN 设备）直连 `127.0.0.1:{localPort}` 打到的是
//! 设备自身——远程实例在这些设备上永远离线。统一改走同源代理：
//! `/api/instances/{id}/p/{*path}` 由 hub server 内部转发到隧道本地端口
//! （server 与隧道同机，永远可达）。HTTP 与 WebSocket（Upgrade 请求）同路由：
//! WS 用 tokio-tungstenite 连上游后双向泵。
//!
//! 安全边界：仅转发到实例自身隧道的 localPort（从 TunnelManager 实时解析，
//! 隧道重连换端口自动跟随），不构成任意 SSRF 跳板；路径拒绝 scheme/双斜杠
//! 走私；鉴权沿用 hub 认证中间件（LAN + token 语义不变），上游认证按实例
//! 配置注入 token（浏览器/手机端无需预存 per-instance token）。token 不进日志。

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use tokio::sync::mpsc;

use base64::Engine as _;

use axum::{
    body::Body,
    extract::{ws::WebSocket, FromRequestParts, Path, Request, State, WebSocketUpgrade},
    http::{header, HeaderName, StatusCode},
    response::Response,
};
use futures::{SinkExt, StreamExt};

/// Result 抽取器形态：WS upgrade 解析失败（即普通 HTTP 请求）落 Err 分支 →
/// HTTP 转发。（axum 0.8 的 Option 抽取器要 OptionalFromRequestParts，
/// WebSocketUpgrade 未实现，Result 形态无此限制）
type WsUpgrade =
    Result<WebSocketUpgrade, <WebSocketUpgrade as FromRequestParts<SharedState>>::Rejection>;

use crate::{
    api::SharedState,
    error::AppError,
    instances::{InstanceConfig, InstanceMode, InstancesStore},
    tunnel::port_open,
};

/// 上游转发整体超时（隧道是本机回环，正常远小于此；防死隧道拖挂请求堆积）
const FORWARD_TIMEOUT: Duration = Duration::from_secs(30);
/// WS 上游建连超时（浏览器侧有 2s 重连，超时直接断让其重试）
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

type HttpClient =
    hyper_util::client::legacy::Client<hyper_util::client::legacy::connect::HttpConnector, Body>;
type WsUpstream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn http_client() -> &'static HttpClient {
    static CLIENT: OnceLock<HttpClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        use hyper_util::rt::TokioExecutor;
        hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http()
    })
}

/// 代理入口（任意方法 + WS Upgrade 同路由）：Upgrade 解析失败（普通请求）一律
/// 走 HTTP 转发——浏览器发的都是合法 upgrade，错误分支只承载「非 WS 请求」语义。
pub async fn instance_proxy(
    State(state): State<SharedState>,
    Path((id, path)): Path<(String, String)>,
    ws: WsUpgrade,
    req: Request,
) -> Result<Response, AppError> {
    let inst = InstancesStore::new(&state.cfg)
        .get(&id)
        .await
        .ok_or_else(|| AppError::not_found("实例不存在"))?;
    match inst.mode {
        InstanceMode::SshTunnel => {
            // localPort 从隧道管理器实时取（非持久化值）——隧道重启换端口自动跟随
            let status = state.tunnels.status(&id).await;
            let local_port = match status.running.then_some(status.local_port).flatten() {
                Some(p) => p,
                None => {
                    return Err(AppError::service_unavailable(
                        "隧道未运行（实例离线），请先启动隧道",
                    ))
                }
            };
            // 启动风暴修复：child 进程存活 ≠ 端口就绪——ssh 启动握手 / 重连退避期间本地
            // 监听未起，此时转发会挂满 FORWARD_TIMEOUT（30s）拖垮开页加载。端口不可达即
            // 503 快速失败（「启动中/retrying」与「未启动」同样不进转发路径）。
            if !port_open(local_port).await {
                return Err(AppError::service_unavailable(
                    "隧道未就绪（启动中或重连中），请稍候",
                ));
            }
            match ws {
                Ok(upg) => proxy_ws(local_port, inst.token.as_deref(), path, req, upg).await,
                Err(_) => proxy_http(local_port, &inst, path, req).await,
            }
        }
        // gateway-node P1 任务 8/9：node-link 经节点反连 mux（HTTP-over-mux + 在线信号）
        InstanceMode::NodeLink => node_instance_proxy(state, &id, path, ws, req).await,
        InstanceMode::Direct => Err(AppError::bad("direct 实例浏览器直连可达，无需代理")),
    }
}

/// node-link 代理上限（24MB body ≈ 32MB mux 帧预算 ÷ base64 膨胀；超出 413）
const NODE_PROXY_BODY_CAP: usize = 24 * 1024 * 1024;

/// node-link 后端（gateway-node P1 任务 8/9）：
/// - HTTP：打包 http_req 注入节点 mux → reqId 对账等 http_resp → 回注浏览器；
///   节点离线 503（对齐隧道离线语义）；走私路径同款防线；token 语义 = inst.token
///   恒 None（节点凭据事实源在 nodes.json 哈希），浏览器 Authorization 随帧透传
///   （节点本地 loopback 直通不看它）
/// - WS：/ws/events 连接保持（在线信号通道，任务 9）——节点断开即关 → 前端既有
///   离线判定翻转置灰；其余 WS 路由显式 503（WS-over-mux 属 P2）
async fn node_instance_proxy(
    state: SharedState,
    id: &str,
    path: String,
    ws: WsUpgrade,
    req: Request,
) -> Result<Response, AppError> {
    ensure_plain_path(&path)?;
    // WS 路由白名单（P2）：events = 在线信号通道（P1 连接保持）；chat/
    // chat_subagent/terminal = WS-over-mux 流透传；其余 ws/ 路径显式 503
    if path.starts_with("ws/")
        && path != "ws/events"
        && !path.starts_with("ws/chat/")
        && path != "ws/terminal"
    {
        return Err(AppError::service_unavailable(
            "WS-over-mux 未开放该路由：仅 events/chat/chat_subagent/terminal",
        ));
    }
    match ws {
        Ok(upg) => {
            if path == "ws/events" {
                // P1 在线信号通道（连接保持，不是事件回传）
                let Some(closed) = state.node_conns.closed_rx(id) else {
                    return Err(AppError::service_unavailable("节点离线"));
                };
                if *closed.borrow() {
                    return Err(AppError::service_unavailable("节点离线"));
                }
                return Ok(upg.on_upgrade(move |socket| hold_node_events_ws(socket, closed)));
            }
            // P2 WS-over-mux：开流 → 双向泵（浏览器 socket ↔ mux 流帧）
            let Some(tx) = state.node_conns.cmd_tx(id) else {
                return Err(AppError::service_unavailable("节点离线"));
            };
            let stream_id = uuid::Uuid::new_v4().to_string();
            let (down_tx, down_rx) = mpsc::channel::<axum::extract::ws::Message>(64);
            state.node_conns.register_stream(&stream_id, down_tx);
            let query = req.uri().query().map(str::to_string);
            // hub 认证 token 不进节点（节点本地 loopback 直通）；hub token 若在
            // query 上也剥掉（语义同 proxy_http 的 rewrite_token_query）
            let query = query.filter(|q| !q.is_empty()).map(|q| {
                q.split('&')
                    .filter(|kv| !kv.is_empty() && !kv.starts_with("token="))
                    .collect::<Vec<_>>()
                    .join("&")
            });
            let open = crate::nodelink::Frame::WsOpen {
                stream_id: stream_id.clone(),
                path: path.clone(),
                query,
            };
            if tx.send(open).await.is_err() {
                state
                    .node_conns
                    .close_stream(&stream_id, Some("节点离线".into()));
                return Err(AppError::service_unavailable("节点离线"));
            }
            Ok(upg.on_upgrade(move |socket| pump_node_mux_ws(socket, stream_id, tx, down_rx)))
        }
        Err(_) => {
            let Some(tx) = state.node_conns.cmd_tx(id) else {
                return Err(AppError::service_unavailable("节点离线"));
            };
            let (parts, body) = req.into_parts();
            let body_bytes = axum::body::to_bytes(body, NODE_PROXY_BODY_CAP)
                .await
                .map_err(|_| {
                    AppError(
                        axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                        anyhow::anyhow!(
                            "请求体超过 node-link 代理上限（24MB；P1 边界 32MB mux 帧）"
                        ),
                    )
                })?;
            let req_id = uuid::Uuid::new_v4().to_string();
            let headers: Vec<(String, String)> = parts
                .headers
                .iter()
                .filter(|(k, _)| !crate::nodelink::is_hop_by_hop(k.as_str()))
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let frame = crate::nodelink::Frame::HttpReq {
                req_id: req_id.clone(),
                method: parts.method.as_str().to_string(),
                path,
                query: parts.uri.query().map(str::to_string),
                headers,
                body: base64::engine::general_purpose::STANDARD.encode(&body_bytes),
            };
            let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
            state.node_conns.pending().insert(id, &req_id, resp_tx);
            if tx.send(frame).await.is_err() {
                state.node_conns.pending().take(id, &req_id);
                return Err(AppError::service_unavailable("节点离线"));
            }
            let resp = tokio::time::timeout(FORWARD_TIMEOUT, resp_rx).await;
            match resp {
                Ok(Ok(resp)) => Ok(resp),
                Ok(Err(_)) => Err(AppError::service_unavailable("节点离线（连接中断）")),
                Err(_) => {
                    state.node_conns.pending().take(id, &req_id); // 超时回收（防泄漏）
                    Err(AppError(
                        axum::http::StatusCode::GATEWAY_TIMEOUT,
                        anyhow::anyhow!("节点回放超时（30s）"),
                    ))
                }
            }
        }
    }
}

/// /p/ws/events 连接保持（任务 9）：网关周期 ping 保活；节点断开（closed 信号
/// 或 sender drop）即关浏览器侧 WS → 前端既有离线判定翻转置灰；节点回归后前端
/// 重连本通道即恢复。
///
/// WS-over-mux 浏览器侧泵（P2 任务 1）：浏览器 socket ↔ mux 流帧。
/// - 上行：socket 读 → WsData（seq 每流单调，节点侧由 mux 层透传语义保序）
/// - 下行：down_rx（serve_node 转发的节点流数据 / Close）→ socket 写
/// - 任一侧断开 → WsClose 收流（节点侧本地流随之关断）
async fn pump_node_mux_ws(
    socket: axum::extract::ws::WebSocket,
    stream_id: String,
    cmd_tx: mpsc::Sender<crate::nodelink::Frame>,
    mut down_rx: mpsc::Receiver<axum::extract::ws::Message>,
) {
    let (mut cs, mut cr) = socket.split();
    let seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    use base64::Engine as _;
    loop {
        let up = cr.next();
        let down = down_rx.recv();
        tokio::select! {
            msg = up => match msg {
                Some(Ok(axum::extract::ws::Message::Text(t))) => {
                    let f = crate::nodelink::Frame::WsData {
                        stream_id: stream_id.clone(),
                        seq: seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                        text: Some(t.to_string()),
                        data: None,
                    };
                    if cmd_tx.send(f).await.is_err() {
                        break;
                    }
                }
                Some(Ok(axum::extract::ws::Message::Binary(b))) => {
                    let f = crate::nodelink::Frame::WsData {
                        stream_id: stream_id.clone(),
                        seq: seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                        text: None,
                        data: Some(base64::engine::general_purpose::STANDARD.encode(&b)),
                    };
                    if cmd_tx.send(f).await.is_err() {
                        break;
                    }
                }
                Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ping/pong：底层保活，不进 mux
                Some(Err(_)) => break,
            },
            down = down => match down {
                Some(m) => {
                    if cs.send(m).await.is_err() {
                        break;
                    }
                }
                None => break, // 收流（节点侧/网关侧断开）
            },
        }
    }
    let _ = cs.close().await;
    let _ = cmd_tx
        .send(crate::nodelink::Frame::WsClose {
            stream_id,
            reason: None,
        })
        .await;
}
async fn hold_node_events_ws(
    mut socket: axum::extract::ws::WebSocket,
    mut closed: tokio::sync::watch::Receiver<bool>,
) {
    let mut ping =
        tokio::time::interval(std::time::Duration::from_secs(crate::nodelink::TICK_SECS));
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if socket
                    .send(axum::extract::ws::Message::Ping(Vec::new().into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            r = closed.changed() => {
                // 两种形态都 = 节点断开：显式置 true / sender 随摘表 drop
                let _ = r;
                let _ = socket.send(axum::extract::ws::Message::Close(None)).await;
                return;
            }
        }
    }
}

/// SSRF 防线：path 必须是纯路径——拒绝 `scheme://`（URL join 语义会换主机）、
/// 协议相对 `//host`、以及 CRLF 走私。
fn ensure_plain_path(path: &str) -> Result<(), AppError> {
    if path.contains("://") || path.starts_with("//") || path.contains(['\r', '\n']) {
        return Err(AppError::bad("代理路径不合法"));
    }
    Ok(())
}

/// hop-by-hop 头不转发（RFC 9110）；host 由 hyper 依上游 uri 重写
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

async fn proxy_http(
    local_port: u16,
    inst: &InstanceConfig,
    path: String,
    req: Request,
) -> Result<Response, AppError> {
    ensure_plain_path(&path)?;
    let (parts, body) = req.into_parts();
    // query 原样透传（远端自校验）
    let pq = match parts.uri.query() {
        Some(q) => format!("/{path}?{q}"),
        None => format!("/{path}"),
    };
    let uri: axum::http::Uri = format!("http://127.0.0.1:{local_port}{pq}")
        .parse()
        .map_err(|e| AppError::bad(format!("代理路径不合法: {e}")))?;
    let mut builder = axum::http::Request::builder()
        .method(parts.method.clone())
        .uri(uri);
    for (k, v) in &parts.headers {
        if is_hop_by_hop(k) {
            continue;
        }
        // 实例配置 token 优先注入（手机端无预存 per-instance token）；
        // 未配置则透传浏览器带来的凭据（手动配置场景语义不变）
        if k == header::AUTHORIZATION && inst.token.is_some() {
            continue;
        }
        builder = builder.header(k, v);
    }
    if let Some(t) = &inst.token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let upstream = builder
        .body(Body::from_stream(body.into_data_stream()))
        .map_err(|e| AppError(StatusCode::BAD_GATEWAY, anyhow::anyhow!(e)))?;
    let res = tokio::time::timeout(FORWARD_TIMEOUT, http_client().request(upstream)).await;
    let res = match res {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return Err(AppError(
                StatusCode::BAD_GATEWAY,
                anyhow::anyhow!("隧道转发失败: {e}"),
            ))
        }
        Err(_) => {
            return Err(AppError(
                StatusCode::GATEWAY_TIMEOUT,
                anyhow::anyhow!("隧道转发超时"),
            ))
        }
    };
    let (up, body) = res.into_parts();
    let mut builder = Response::builder().status(up.status).version(up.version);
    for (k, v) in &up.headers {
        if is_hop_by_hop(k) {
            continue;
        }
        builder = builder.header(k, v);
    }
    builder
        .body(Body::new(body))
        .map_err(|e| AppError(StatusCode::BAD_GATEWAY, anyhow::anyhow!(e)))
}

/// 上游 query 重写：剥掉浏览器带来的 hub `token=`（那是 hub 认证用的，远端不认），
/// 实例配置有 token 则注入；未配置则其余参数原样保留（手动 per-instance token
/// 场景由 HTTP 透传兜底，WS 无预存凭据即断开重试——见模块注释）。
fn rewrite_token_query(query: &str, inst_token: Option<&str>) -> String {
    let mut kept: Vec<String> = query
        .split('&')
        .filter(|kv| !kv.is_empty() && !kv.starts_with("token="))
        .map(String::from)
        .collect();
    if let Some(t) = inst_token {
        kept.push(format!("token={t}"));
    }
    kept.join("&")
}

async fn proxy_ws(
    local_port: u16,
    inst_token: Option<&str>,
    path: String,
    req: Request,
    upg: WebSocketUpgrade,
) -> Result<Response, AppError> {
    ensure_plain_path(&path)?;
    let upstream_query = rewrite_token_query(req.uri().query().unwrap_or(""), inst_token);
    let ws_url = format!("ws://127.0.0.1:{local_port}/{path}");
    let ws_url = if upstream_query.is_empty() {
        ws_url
    } else {
        format!("{ws_url}?{upstream_query}")
    };
    Ok(upg.on_upgrade(move |socket| async move {
        match tokio::time::timeout(
            WS_CONNECT_TIMEOUT,
            tokio_tungstenite::connect_async(&ws_url),
        )
        .await
        {
            Ok(Ok((upstream, _))) => pump_ws(socket, upstream).await,
            Ok(Err(e)) => tracing::warn!(error = %e, "实例 WS 代理上游连接失败"),
            Err(_) => tracing::warn!("实例 WS 代理上游连接超时"),
        }
    }))
}

/// 双向泵：任一侧关断/出错即整体退出（对端 socket 随之关闭）。
async fn pump_ws(client: WebSocket, upstream: WsUpstream) {
    let (mut cs, mut cr) = client.split();
    let (mut us, mut ur) = upstream.split();
    loop {
        tokio::select! {
            biased;
            from_client = cr.next() => match from_client {
                Some(Ok(m)) => {
                    let Some(up) = client_msg_to_upstream(m) else { continue };
                    if us.send(up).await.is_err() {
                        break;
                    }
                }
                _ => break,
            },
            from_upstream = ur.next() => match from_upstream {
                Some(Ok(m)) => {
                    let Some(down) = upstream_msg_to_client(m) else { continue };
                    if cs.send(down).await.is_err() {
                        break;
                    }
                }
                _ => break,
            },
        }
    }
    let _ = cs.close().await;
    let _ = us.close().await;
}

use axum::extract::ws::Message as ClientMessage;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as UpCloseFrame;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;

fn client_msg_to_upstream(m: ClientMessage) -> Option<UpstreamMessage> {
    Some(match m {
        ClientMessage::Text(t) => UpstreamMessage::Text(t.as_str().into()),
        ClientMessage::Binary(b) => UpstreamMessage::Binary(b),
        ClientMessage::Ping(p) => UpstreamMessage::Ping(p),
        ClientMessage::Pong(p) => UpstreamMessage::Pong(p),
        ClientMessage::Close(c) => UpstreamMessage::Close(c.map(|f| UpCloseFrame {
            code: CloseCode::Iana(f.code),
            reason: f.reason.as_str().into(),
        })),
    })
}

fn upstream_msg_to_client(m: UpstreamMessage) -> Option<ClientMessage> {
    use axum::extract::ws::CloseFrame;
    Some(match m {
        UpstreamMessage::Text(t) => ClientMessage::Text(t.as_str().into()),
        UpstreamMessage::Binary(b) => ClientMessage::Binary(b),
        UpstreamMessage::Ping(p) => ClientMessage::Ping(p),
        UpstreamMessage::Pong(p) => ClientMessage::Pong(p),
        UpstreamMessage::Close(c) => ClientMessage::Close(c.map(|f| CloseFrame {
            code: f.code.into(),
            reason: f.reason.as_str().into(),
        })),
        // tungstenite 协议内部原始帧，不透传
        UpstreamMessage::Frame(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{api::AppState, config::Config, router};
    use axum::{
        body::Body as RespBody,
        extract::ws::{Message as WsMsg, WebSocketUpgrade},
        http::{HeaderMap, Request as HttpRequest},
        routing::get,
        Json, Router as AxumRouter,
    };
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;
    use tower::ServiceExt;

    fn fake_bin() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join("fake-claude.sh")
    }

    fn cfg_with(dir: &tempfile::TempDir) -> Config {
        let mut cfg = Config::load();
        cfg.claude_bin = fake_bin();
        cfg.jobs_dir = dir.path().join("jobs");
        cfg.instances_file = dir.path().join("instances.json");
        cfg
    }

    fn tunnel_inst(id: &str, token: Option<&str>) -> InstanceConfig {
        InstanceConfig {
            id: id.into(),
            name: "t".into(),
            mode: InstanceMode::SshTunnel,
            url: None,
            token: token.map(String::from),
            ssh: Some(crate::instances::SshConfig {
                host: "localhost".into(),
                port: 22,
                user: "u".into(),
                auth: crate::instances::SshAuth::Authsock,
                key_path: None,
                password: None,
            }),
            remote_port: Some(7800),
            local_port: None,
            node_meta: None,
        }
    }

    async fn save_instance(dir: &tempfile::TempDir, inst: &InstanceConfig) {
        InstancesStore::new(&cfg_with(dir))
            .upsert(inst)
            .await
            .unwrap();
    }

    fn app(dir: &tempfile::TempDir) -> axum::Router {
        router(Arc::new(AppState::from_config(cfg_with(dir))))
    }

    async fn oneshot(
        a: axum::Router,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, String) {
        let mut builder = HttpRequest::builder().method(method).uri(uri);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        let res = a
            .oneshot(builder.body(RespBody::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            headers,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

    /// 起一个记录 Authorization / query 的上游（模拟远端实例 server）
    async fn upstream_server() -> (u16, Arc<std::sync::Mutex<Vec<(String, String)>>>) {
        let seen: Arc<std::sync::Mutex<Vec<(String, String)>>> = Default::default();
        let seen2 = seen.clone();
        let app = AxumRouter::new().route(
            "/api/agents",
            get(move |headers: HeaderMap, uri: axum::http::Uri| {
                let seen = seen2.clone();
                async move {
                    seen.lock().unwrap().push((
                        headers
                            .get(header::AUTHORIZATION)
                            .map(|v| v.to_str().unwrap().to_string())
                            .unwrap_or_default(),
                        uri.query().unwrap_or("").to_string(),
                    ));
                    Json(json!([{ "id": "up-agent" }]))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (port, seen)
    }

    #[tokio::test]
    async fn proxies_get_and_injects_config_token() {
        let dir = tempfile::tempdir().unwrap();
        let (up_port, seen) = upstream_server().await;
        let inst = tunnel_inst("i1", Some("inst-tok"));
        save_instance(&dir, &inst).await;
        let state = Arc::new(AppState::from_config(cfg_with(&dir)));
        state.tunnels.inject_running("i1", up_port);

        let a = router(state);
        let (status, _, body) = oneshot(
            a,
            "GET",
            "/api/instances/i1/p/api/agents?all=1",
            &[("authorization", "Bearer browser-junk")],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "转发失败: {body}");
        assert!(body.contains("up-agent"), "应回上游响应: {body}");
        // 配置 token 覆盖浏览器头；query 原样透传
        assert_eq!(seen.lock().unwrap()[0].0, "Bearer inst-tok");
        assert_eq!(seen.lock().unwrap()[0].1, "all=1");
    }

    #[tokio::test]
    async fn no_config_token_passes_browser_header_through() {
        let dir = tempfile::tempdir().unwrap();
        let (up_port, seen) = upstream_server().await;
        save_instance(&dir, &tunnel_inst("i1", None)).await;
        let state = Arc::new(AppState::from_config(cfg_with(&dir)));
        state.tunnels.inject_running("i1", up_port);
        let (status, _, _) = oneshot(
            router(state),
            "GET",
            "/api/instances/i1/p/api/agents",
            &[("authorization", "Bearer per-instance-tok")],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(seen.lock().unwrap()[0].0, "Bearer per-instance-tok");
    }

    /// 未启动（无隧道句柄）→ 503「隧道未运行」
    #[tokio::test]
    async fn tunnel_down_gives_503() {
        let dir = tempfile::tempdir().unwrap();
        save_instance(&dir, &tunnel_inst("i1", None)).await;
        let (status, _, body) =
            oneshot(app(&dir), "GET", "/api/instances/i1/p/api/agents", &[]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("隧道未运行"), "503 应带明确错误: {body}");
    }

    /// 启动中/重连退避（child 存活但本地监听未起）也 503——启动风暴修复：
    /// 此态转发会挂满 FORWARD_TIMEOUT（30s），必须在进转发路径前快速失败
    #[tokio::test]
    async fn tunnel_starting_gives_503() {
        let dir = tempfile::tempdir().unwrap();
        save_instance(&dir, &tunnel_inst("i1", None)).await;
        // inject_running = child 存活（running=true）；端口取「探测即释放」的死端口
        // = 启动握手/退避窗口的本地监听未起形态（探测-重bind 竞争同 alloc_port 测试口径）
        let dead_port = crate::tunnel::TunnelManager::alloc_local_port().unwrap();
        let state = Arc::new(AppState::from_config(cfg_with(&dir)));
        state.tunnels.inject_running("i1", dead_port);
        let (status, _, body) =
            oneshot(router(state), "GET", "/api/instances/i1/p/api/agents", &[]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("隧道未就绪"), "未就绪应快速 503: {body}");
    }

    #[tokio::test]
    async fn unknown_instance_and_non_tunnel_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (status, _, _) =
            oneshot(app(&dir), "GET", "/api/instances/nope/p/api/agents", &[]).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // direct 实例不可代理（浏览器直连可达，无需中转）
        InstancesStore::new(&cfg_with(&dir))
            .upsert(&InstanceConfig {
                id: "d1".into(),
                name: "d".into(),
                mode: InstanceMode::Direct,
                url: Some("https://a.example.com".into()),
                token: None,
                ssh: None,
                remote_port: None,
                local_port: None,
                node_meta: None,
            })
            .await
            .unwrap();
        let (status, _, _) = oneshot(app(&dir), "GET", "/api/instances/d1/p/api/agents", &[]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// SSRF 防线：scheme:// 与 //host 走私路径 400，不转发
    #[tokio::test]
    async fn smuggled_paths_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (up_port, seen) = upstream_server().await;
        save_instance(&dir, &tunnel_inst("i1", None)).await;
        let state = Arc::new(AppState::from_config(cfg_with(&dir)));
        state.tunnels.inject_running("i1", up_port);
        let a = router(state);
        for evil in [
            "/api/instances/i1/p/https://evil.com/x",
            "/api/instances/i1/p//evil.com/x",
        ] {
            let (status, _, _) = oneshot(a.clone(), "GET", evil, &[]).await;
            // 400 = handler 守卫拒绝；404 = matchit 归一化后未命中代理路由——
            // 两者都保证走私路径不触达上游
            assert!(
                status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
                "走私路径应拒绝: {evil} → {status}"
            );
        }
        assert!(seen.lock().unwrap().is_empty(), "被拒路径不得触达上游");
    }

    #[tokio::test]
    async fn proxies_post_body_and_content_type() {
        let dir = tempfile::tempdir().unwrap();
        let port;
        let seen_body: Arc<std::sync::Mutex<Option<String>>> = Default::default();
        let seen_ct: Arc<std::sync::Mutex<Option<String>>> = Default::default();
        {
            let sb = seen_body.clone();
            let sct = seen_ct.clone();
            let app = AxumRouter::new().route(
                "/api/agents/claude/a1/message",
                axum::routing::post(move |ct: HeaderMap, body: String| async move {
                    *sb.lock().unwrap() = Some(body);
                    *sct.lock().unwrap() = ct
                        .get(header::CONTENT_TYPE)
                        .map(|v| v.to_str().unwrap().to_string());
                    "ok"
                }),
            );
            let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            port = l.local_addr().unwrap().port();
            tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        }
        save_instance(&dir, &tunnel_inst("i1", None)).await;
        let state = Arc::new(AppState::from_config(cfg_with(&dir)));
        state.tunnels.inject_running("i1", port);

        let req = HttpRequest::builder()
            .method("POST")
            .uri("/api/instances/i1/p/api/agents/claude/a1/message")
            .header(header::CONTENT_TYPE, "application/json")
            .body(RespBody::from(r#"{"text":"hi"}"#))
            .unwrap();
        let res = router(state).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            seen_body.lock().unwrap().as_deref(),
            Some(r#"{"text":"hi"}"#)
        );
        assert_eq!(seen_ct.lock().unwrap().as_deref(), Some("application/json"));
    }

    #[tokio::test]
    async fn lan_auth_applies_to_proxy_route() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = cfg_with(&dir);
        cfg.allow_lan = true;
        cfg.token = Some("hub-tok".into());
        save_instance(&dir, &tunnel_inst("i1", None)).await;
        let a = router(Arc::new(AppState::from_config(cfg)));
        // 无 token → 401（中间件覆盖代理路由）
        let (status, _, _) = oneshot(a.clone(), "GET", "/api/instances/i1/p/api/agents", &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // hub token → 过中间件（到隧道解析层：未注入隧道 → 503 而非 401）
        let (status, _, _) = oneshot(
            a,
            "GET",
            "/api/instances/i1/p/api/agents?token=hub-tok",
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    /// WS 代理端到端：经真实 hub server（TCP）upgrade → 上游 WS 回传一条消息；
    /// 上游把收到的 query 回显，验证实例 token 注入（浏览器带的是 hub token）。
    #[tokio::test]
    async fn ws_proxy_pumps_and_injects_token() {
        use futures::StreamExt as _;
        let dir = tempfile::tempdir().unwrap();
        // 上游：/ws/events 把 query 原样作为文本发回
        let up_app = AxumRouter::new().route(
            "/ws/events",
            get(|ws: WebSocketUpgrade, uri: axum::http::Uri| async move {
                ws.on_upgrade(move |sock| async move {
                    let (mut sink, _) = sock.split();
                    use futures::SinkExt as _;
                    let _ = sink
                        .send(WsMsg::Text(
                            format!("query={}", uri.query().unwrap_or("")).into(),
                        ))
                        .await;
                })
            }),
        );
        let up_l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let up_port = up_l.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(up_l, up_app).await.unwrap() });

        save_instance(&dir, &tunnel_inst("i1", Some("inst-tok"))).await;
        let state = Arc::new(AppState::from_config(cfg_with(&dir)));
        state.tunnels.inject_running("i1", up_port);
        let hub_l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let hub_port = hub_l.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(hub_l, router(state)).await.unwrap() });

        let (mut ws, _) = tokio_tungstenite::connect_async(format!(
            "ws://127.0.0.1:{hub_port}/api/instances/i1/p/ws/events?token=hub-side-token"
        ))
        .await
        .expect("经代理 upgrade 应成功");
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("应收到上游消息")
            .expect("流不应空")
            .expect("消息应有效");
        let text = match msg {
            UpstreamMessage::Text(t) => t.to_string(),
            other => panic!("应为文本消息: {other:?}"),
        };
        // hub token 被剥掉、实例 token 注入上游
        assert_eq!(
            text, "query=token=inst-tok",
            "上游应收注入后的 token: {text}"
        );
    }

    /// 隧道未运行时 WS upgrade 拒绝（503，浏览器侧表现为连接失败 → 离线态）
    #[tokio::test]
    async fn ws_proxy_503_when_tunnel_down() {
        let dir = tempfile::tempdir().unwrap();
        save_instance(&dir, &tunnel_inst("i1", None)).await;
        let a = app(&dir);
        let (status, _, body) = oneshot(
            a,
            "GET",
            "/api/instances/i1/p/ws/events",
            &[
                ("connection", "Upgrade"),
                ("upgrade", "websocket"),
                ("sec-websocket-version", "13"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    }

    #[test]
    fn rewrite_token_query_semantics() {
        assert_eq!(
            rewrite_token_query("token=hub&x=1", Some("t")),
            "x=1&token=t"
        );
        assert_eq!(rewrite_token_query("x=1", Some("t")), "x=1&token=t");
        assert_eq!(rewrite_token_query("token=hub", None), "");
        assert_eq!(rewrite_token_query("", Some("t")), "token=t");
    }

    #[test]
    fn plain_path_guard() {
        assert!(ensure_plain_path("api/agents").is_ok());
        assert!(ensure_plain_path("ws/events").is_ok());
        assert!(ensure_plain_path("https://evil.com").is_err());
        assert!(ensure_plain_path("//evil.com").is_err());
        assert!(ensure_plain_path("a\r\nb").is_err());
    }
}
