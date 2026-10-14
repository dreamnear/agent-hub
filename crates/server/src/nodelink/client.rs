//! nodelink 客户端泵（节点侧，agent-hub-gateway-node-p1 任务 3）。
//!
//! 监督循环：出站连 gateway `/ws/node` → 发 hello（announce）→ 按 hello-ok /
//! pairing_required / 终态帧驱动会话。
//!
//! - **永续退避**（1s→60s 封顶不放弃）：与 tunnel.rs「5 次重连后放弃」的语义分叉点
//!   ——节点失联必须自动回归（N4），终态帧之外永不停止拨号。
//! - tick 15s 心跳 + 静默 2×tick 判死主动重连（tick watchdog 语义）；显式应答
//!   对端 ping（tungstenite 自动 pong 不可靠——dsh 泵既有结论）。
//! - `pairing_required` → debug 级日志（不刷屏）+ 退避重试等待审批。
//! - `rejected` / `node_token_rejected` → **终态**：泵挂起等人工（进程存活、
//!   零再拨号——N2/N3 锚）；hello-ok 首发带 node token → 0600 落盘后进常态。
//! - 凭据（token/enroll）进日志唯一形态 = [`super::mask_secret`] 掩码。

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt as _;

use super::node_config::NodeConfig;
use super::{backoff_secs, dead_after, mask_secret, Frame, PROTOCOL_VERSION, TICK_SECS};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

use tokio::sync::mpsc;

/// 泵参数（node 形态启动时构造；enroll 是一次性凭据，仅驻内存不落盘）。
/// router = 节点自身 API 路由（HTTP-over-mux 重放目标，任务 8）。
#[derive(Clone)]
pub struct NodeLinkParams {
    pub gateway_url: String,
    pub node_id: String,
    pub node_name: String,
    pub token: Option<String>,
    pub enroll: Option<String>,
    pub platform: String,
    pub drivers: Vec<String>,
    pub hub_version: String,
    pub router: axum::Router,
    /// 节点自身 loopback 端口（WS-over-mux 流目标：节点内本地 upgrade 地址）
    pub local_port: u16,
    /// 节点事件面（P2 任务 2：Tick 经 mux 回传网关；测试可 None）
    pub events_tx: Option<tokio::sync::broadcast::Sender<crate::drivers::events::HubEvent>>,
}

impl std::fmt::Debug for NodeLinkParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // router 不进 Debug（路由树巨大）；其余字段照常
        f.debug_struct("NodeLinkParams")
            .field("gateway_url", &self.gateway_url)
            .field("node_id", &self.node_id)
            .field("node_name", &self.node_name)
            .field("token", &self.token.as_ref().map(|_| "<masked>"))
            .field("enroll", &self.enroll.as_ref().map(|_| "<masked>"))
            .field("platform", &self.platform)
            .field("drivers", &self.drivers)
            .field("hub_version", &self.hub_version)
            .finish_non_exhaustive()
    }
}

/// 时序参数（生产 Default；测试注入短步长——仿 tunnel.rs FAKE_STEP 模式）。
#[derive(Debug, Clone, Copy)]
pub struct Timings {
    pub tick: Duration,
    pub dead_after: Duration,
    pub dial_timeout: Duration,
}

impl Default for Timings {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(TICK_SECS),
            dead_after: dead_after(),
            dial_timeout: Duration::from_secs(10),
        }
    }
}

/// 测试注入的退避步长（lib 单测默认 1ms；集成测试目标下 lib 以非 test 编译，
/// 须显式调 [`install_fast_backoff_for_tests`]，否则走生产 1s 步长）。
static FAKE_STEP: OnceLock<Duration> = OnceLock::new();
fn backoff_unit() -> Duration {
    FAKE_STEP.get().copied().unwrap_or(if cfg!(test) {
        Duration::from_millis(1)
    } else {
        Duration::from_secs(1)
    })
}

/// 集成测试专用：安装 1ms 退避步长（tests/nodelink_integration.rs 等真 TCP
/// 集成件调用；生产二进制零调用路径——OnceLock 恒空 → 1s 步长）。
pub fn install_fast_backoff_for_tests() {
    let _ = FAKE_STEP.set(Duration::from_millis(1));
}

/// 一次会话的收场分类。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SessionOutcome {
    /// 连接失败/对端关闭/静默判死/pairing 等待——退避后重连。
    Retry,
    /// 终态：rejected / node_token_rejected——泵挂起等人工，零再拨号。
    Terminal(&'static str),
}

/// 泵监督循环：连接成功即重置退避；会话按收场分类分流；终态挂起永不返回。
pub async fn run_node_link(params: NodeLinkParams, node_file: PathBuf, timings: Timings) {
    tracing::info!(
        gateway = %params.gateway_url,
        node_id = %params.node_id,
        max_backoff_secs = super::BACKOFF_MAX_SECS,
        "NodeLink 泵启动（永续退避，终态帧之外不放弃）"
    );
    let mut attempt: u32 = 0;
    loop {
        attempt = attempt.saturating_add(1);
        tracing::info!(attempt, gateway = %params.gateway_url, "NodeLink 拨号");
        let session =
            match tokio::time::timeout(timings.dial_timeout, connect(&params.gateway_url)).await {
                Err(_) => {
                    tracing::debug!(attempt, backoff_secs = backoff_secs(attempt), "拨号超时");
                    None
                }
                Ok(Err(e)) => {
                    tracing::debug!(
                        attempt,
                        backoff_secs = backoff_secs(attempt),
                        error = %e,
                        "连接失败"
                    );
                    None
                }
                Ok(Ok(ws)) => Some(ws),
            };
        if let Some(ws) = session {
            attempt = 0; // 连上即重置退避（对齐 dsh 泵语义）
            match serve_session(ws, &params, &node_file, timings).await {
                SessionOutcome::Retry => {
                    tracing::debug!("NodeLink 会话结束，退避重连");
                }
                SessionOutcome::Terminal(kind) => {
                    tracing::warn!(
                        kind,
                        "NodeLink 终态：停止重连，等待人工处理（修正凭据或重新 enrollment 后重启进程恢复）"
                    );
                    // 泵挂起：进程存活、零再拨号（N2「拒绝不重连」/N3「显式终态」）
                    std::future::pending::<()>().await;
                }
            }
        }
        // Duration 只有 Mul<u32>（std 无 Mul<u64>）；backoff_secs ≤ 60，try_from 恒成功
        let secs = u32::try_from(backoff_secs(attempt)).unwrap_or(u32::MAX);
        tokio::time::sleep(backoff_unit().saturating_mul(secs)).await;
    }
}

/// 单次会话：握手（hello/announce）→ 帧循环（tick 心跳 / 静默判死 / 分类处理）。
pub(crate) async fn serve_session(
    ws: WsStream,
    params: &NodeLinkParams,
    node_file: &Path,
    timings: Timings,
) -> SessionOutcome {
    use base64::Engine as _; // WS 流帧 text/binary 互斥编解码
                             // split 成独立收发半（select 双向操作互不借用冲突；dsh proxy 双向泵同款）
    let (mut sink, mut incoming) = ws.split();
    // WS-over-mux 流引擎（P2）：多浏览器 WS 流经同一 mux 会话时，所有下行帧
    // （流数据/收流）经 outbound 通道汇聚后由本循环统一 sink 发送——流任务不
    // 直接持 sink，避免多写者竞争。streams = stream_id → 网关下行帧入口（每流
    // 一个 mpsc，std Mutex 只做同步投递，无 await 跨越 guard）。
    let (out_tx, mut out_rx) = mpsc::channel::<Frame>(256);
    // 事件回传泵（P2 任务 2）：节点 HubEvent::Tick → NodeTick 帧 → 网关。会话
    // 结束（本函数返回）时 rx 随作用域 drop，泵自动收口；下个会话重新 subscribe。
    if let Some(ev_tx) = &params.events_tx {
        let mut ev_rx = ev_tx.subscribe();
        let out = out_tx.clone();
        tokio::spawn(async move {
            loop {
                match ev_rx.recv().await {
                    Ok(crate::drivers::events::HubEvent::Tick) => {
                        if out.send(Frame::NodeTick).await.is_err() {
                            break;
                        }
                    }
                    Ok(_) => {} // 非 Tick 事件不回传（侧栏刷新只需活性信号）
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
    }
    let streams: Arc<std::sync::Mutex<std::collections::HashMap<String, mpsc::Sender<Message>>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    // 凭据决议（N4 网关重启回归锚）：长效 token 优先——enrollment 是一次性凭据
    // 且网关侧仅内存，网关重启即失；节点存活跨网关重启时若仍带死 code 重连，
    // 会被 Unknown→rejected 终态打死（节点永久失联需人工）。故每次连接前：
    // params 无 token 则回填 node.json 已落盘 token（hello-ok 签发后即持久化），
    // token 缺失才退回 enroll。
    let mut effective_token = params.token.clone();
    if effective_token.is_none() {
        if let Some(cfg) = NodeConfig::load(node_file) {
            effective_token = cfg.token;
        }
    }
    let credentials = effective_token
        .clone()
        .as_deref()
        .map(str::to_string)
        .or_else(|| params.enroll.clone())
        .unwrap_or_default();
    let mut hello_params = params.clone();
    hello_params.token = effective_token;
    if let Err(e) = sink
        .send(Message::Text(
            serde_json::to_string(&build_hello(&hello_params))
                .expect("hello 序列化")
                .into(),
        ))
        .await
    {
        tracing::debug!(error = %e, "hello 发送失败");
        return SessionOutcome::Retry;
    }
    tracing::info!(
        node_id = %params.node_id,
        node_name = %params.node_name,
        credentials = %mask_secret(&credentials),
        drivers = ?params.drivers,
        "NodeLink hello 已发送（等待 hello-ok / pairing_required / 终态）"
    );
    let mut last_inbound = tokio::time::Instant::now();
    loop {
        // 判死窗口 = 距最后一次入站帧的时长（tick 分支每轮抢先 5ms 级时不能每轮
        // 重建超时——那会重启窗口永不判死）；入站即刷新锚点，静默到期判死重连
        let until_dead = (last_inbound + timings.dead_after)
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        let tick_sleep = tokio::time::sleep(timings.tick);
        let outbound = out_rx.recv();
        tokio::select! {
            _ = tick_sleep => {
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return SessionOutcome::Retry;
                }
            }
            out = outbound => {
                // 流引擎下行（WS 流帧）：任一流任务的出站帧统一经本循环发送
                let Some(frame) = out else {
                    // 全部流任务结束且 out_tx 全 drop = 不应发生（主循环持 tx）
                    return SessionOutcome::Retry;
                };
                if sink
                    .send(Message::Text(
                        serde_json::to_string(&frame).expect("流帧序列化").into(),
                    ))
                    .await
                    .is_err()
                {
                    return SessionOutcome::Retry;
                }
            }
            waited = tokio::time::timeout(until_dead, incoming.next()) => {
                match waited {
                    Err(_) => {
                        tracing::warn!(silent = ?timings.dead_after, "静默超过判死窗口，主动重连");
                        return SessionOutcome::Retry;
                    }
                    Ok(None) => return SessionOutcome::Retry,
                    Ok(Some(Err(e))) => {
                        tracing::debug!(error = %e, "WS 读错误");
                        return SessionOutcome::Retry;
                    }
                    Ok(Some(Ok(frame))) => {
                        last_inbound = tokio::time::Instant::now();
                        match frame {
                            Message::Ping(p) => {
                                // 显式回 pong（tungstenite 自动应答不确定——dsh 泵既有结论）
                                let _ = sink.send(Message::Pong(p)).await;
                            }
                            Message::Text(t) => {
                                let text = t.to_string();
                                match serde_json::from_str::<Frame>(&text) {
                                    Ok(frame) => match frame {
                                        Frame::HelloOk { instance_id, node_token, .. } => {
                                            if let Some(tok) = node_token {
                                                if let Err(e) = persist_token(node_file, &tok).await {
                                                    tracing::error!(
                                                        error = %e,
                                                        "node token 落盘失败（本会话继续；重启后需重新 enrollment）"
                                                    );
                                                } else {
                                                    tracing::info!(
                                                        instance_id = %instance_id,
                                                        token = %mask_secret(&tok),
                                                        "node token 已签发并落盘（0600）"
                                                    );
                                                }
                                            } else {
                                                tracing::debug!(instance_id = %instance_id, "hello-ok：常态会话");
                                            }
                                        }
                                        Frame::PairingRequired => {
                                            // debug 级：审批等待期退避重试，不刷屏（N3 语义）
                                            tracing::debug!("节点待审批（pairing_required），退避重试等待操作者批准");
                                            return SessionOutcome::Retry;
                                        }
                                        Frame::Rejected { reason } => {
                                            tracing::warn!(reason = %reason, "NodeLink 被网关拒绝（终态）");
                                            return SessionOutcome::Terminal("rejected");
                                        }
                                        Frame::NodeTokenRejected { reason } => {
                                            tracing::warn!(reason = %reason, "node token 被网关拒绝（终态）");
                                            return SessionOutcome::Terminal("node_token_rejected");
                                        }
                                        Frame::HttpReq { .. } => {
                                            // HTTP-over-mux 回放（任务 8）：帧 → 本地
                                            // Router oneshot（合成 loopback ConnectInfo）
                                            // → HttpResp 回注网关
                                            let resp =
                                                replay_http(params.router.clone(), frame).await;
                                            if sink
                                                .send(Message::Text(
                                                    serde_json::to_string(&resp)
                                                        .expect("resp 帧序列化")
                                                        .into(),
                                                ))
                                                .await
                                                .is_err()
                                            {
                                                return SessionOutcome::Retry;
                                            }
                                        }
                                        // WS-over-mux 开流（P2 任务 1）：向节点本地
                                        // 同路径 upgrade 后双向泵——本地流 ↔ mux 帧
                                        // （text/binary 1:1 + seq 单调 + WsClose 收流）
                                        Frame::WsOpen {
                                            stream_id,
                                            path,
                                            query,
                                        } => {
                                            tokio::spawn(pump_node_ws(
                                                stream_id,
                                                path,
                                                query,
                                                params.local_port,
                                                out_tx.clone(),
                                                streams.clone(),
                                            ));
                                        }
                                        // 流下行：网关侧代理面转发的浏览器消息
                                        Frame::WsData {
                                            stream_id, text, data, ..
                                        } => {
                                            let msg = match (text, data) {
                                                (Some(t), _) => Some(Message::Text(t.into())),
                                                (_, Some(d)) => base64::engine::general_purpose::STANDARD
                                                    .decode(d.as_bytes())
                                                    .ok()
                                                    .map(|b| Message::Binary(b.into())),
                                                _ => None,
                                            };
                                            let Some(msg) = msg else {
                                                tracing::debug!(stream_id, "ws_data 无内容（text/data 双空），忽略");
                                                return SessionOutcome::Retry;
                                            };
                                            let guard = streams.lock().unwrap_or_else(|e| e.into_inner());
                                            if let Some(tx) = guard.get(&stream_id) {
                                                let _ = tx.try_send(msg); // 同步投递（guard 内禁 await）
                                            }
                                        }
                                        Frame::WsClose { stream_id, .. } => {
                                            if let Some(tx) = streams
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .remove(&stream_id)
                                            {
                                                let _ = tx.try_send(Message::Close(None));
                                            }
                                        }
                                        // token 轮换（P3 M2）：新长效凭据经在线连接
                                        // 回显 → 0600 落盘后立即生效（旧 token 失效）
                                        // sweep S1：persist 结果回执网关（帧只增；
                                        // 旧网关当意外帧忽略）——失败网关回滚旧哈希
                                        Frame::TokenRotated { token } => {
                                            let ok = match persist_token(node_file, &token).await {
                                                Ok(()) => {
                                                    tracing::info!(
                                                        token = %mask_secret(&token),
                                                        "轮换 token 已落盘（0600），旧 token 立即失效"
                                                    );
                                                    true
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        error = %e,
                                                        "轮换 token 落盘失败（本会话继续以旧凭据；网关将回滚）"
                                                    );
                                                    false
                                                }
                                            };
                                            let ack = Frame::TokenRotatedAck { token, ok };
                                            if sink
                                                .send(Message::Text(
                                                    serde_json::to_string(&ack)
                                                        .expect("ack 帧序列化")
                                                        .into(),
                                                ))
                                                .await
                                                .is_err()
                                            {
                                                return SessionOutcome::Retry;
                                            }
                                        }
                                        Frame::Hello { .. } | Frame::HttpResp { .. }
                                        | Frame::NodeTick | Frame::TokenRotatedAck { .. } => {
                                            tracing::debug!("意外的下行帧类型，忽略");
                                        }
                                    },
                                    Err(_) => tracing::debug!("非协议 JSON 帧，忽略"),
                                }
                            }
                            Message::Close(_) => return SessionOutcome::Retry,
                            _ => {} // pong/binary 等：仅作活性证据
                        }
                    }
                }
            }
        }
    }
}

/// WS-over-mux 单流任务（P2 任务 1）：向节点本地同路径 upgrade → 双向 1:1 泵
/// （本地流 ↔ mux 帧）。seq 每流单调；本地或对端任一侧关断即收流（WsClose）。
async fn pump_node_ws(
    stream_id: String,
    path: String,
    query: Option<String>,
    local_port: u16,
    out_tx: mpsc::Sender<Frame>,
    streams: Arc<std::sync::Mutex<std::collections::HashMap<String, mpsc::Sender<Message>>>>,
) {
    use base64::Engine as _; // 本地流 binary → base64 编进流帧
    let url = match query.as_deref().filter(|q| !q.is_empty()) {
        Some(q) => format!("ws://127.0.0.1:{local_port}/{path}?{q}"),
        None => format!("ws://127.0.0.1:{local_port}/{path}"),
    };
    // 下行通道先建先插表（拨号前）：网关首包在拨号窗口到达时入队等待
    let seq = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (down_tx, mut down_rx) = mpsc::channel::<Message>(64);
    streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(stream_id.clone(), down_tx);
    let (ws, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(v) => v,
        Err(e) => {
            // 本地路径不存在/未起：摘表 + 显式收流（浏览器侧收到 close，等同隧道离线）
            streams
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&stream_id);
            let _ = out_tx
                .send(Frame::WsClose {
                    stream_id,
                    reason: Some(format!("节点本地 upgrade 失败: {e}")),
                })
                .await;
            return;
        }
    };
    let (mut local_sink, mut local_in) = ws.split();
    loop {
        let from_local = local_in.next();
        let from_peer = down_rx.recv();
        tokio::select! {
            msg = from_local => match msg {
                Some(Ok(Message::Text(t))) => {
                    let f = Frame::WsData {
                        stream_id: stream_id.clone(),
                        seq: seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                        text: Some(t.to_string()),
                        data: None,
                    };
                    if out_tx.send(f).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Binary(b))) => {
                    let f = Frame::WsData {
                        stream_id: stream_id.clone(),
                        seq: seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst),
                        text: None,
                        data: Some(base64::engine::general_purpose::STANDARD.encode(&b)),
                    };
                    if out_tx.send(f).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ping/pong：底层保活，不透传
                Some(Err(e)) => {
                    tracing::debug!(stream_id, error = %e, "本地流读错误");
                    break;
                }
            },
            down = from_peer => match down {
                Some(m) => {
                    if local_sink.send(m).await.is_err() {
                        break;
                    }
                }
                None => break, // 网关侧收流
            },
        }
    }
    // 收流：无论哪侧先断，另一端随之关断（本地 Close + 网关 WsClose）
    let _ = local_sink.close().await;
    let _ = out_tx
        .send(Frame::WsClose {
            stream_id: stream_id.clone(),
            reason: None,
        })
        .await;
    streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&stream_id);
}

/// hello 帧（连接时声明节点身份与能力面——announce 事实源）。
fn build_hello(p: &NodeLinkParams) -> Frame {
    Frame::Hello {
        protocol: PROTOCOL_VERSION,
        node_id: p.node_id.clone(),
        node_name: p.node_name.clone(),
        token: p.token.clone(),
        enroll: p.enroll.clone(),
        platform: p.platform.clone(),
        drivers: p.drivers.clone(),
        hub_version: p.hub_version.clone(),
        // host stats presence（P3 任务 3）：announce 随 hello 上报
        stats: Some(crate::nodelink::HostStats {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            version: p.hub_version.clone(),
            uptime_secs: crate::platform::host_uptime_secs(),
        }),
    }
}

/// 回放单请求上限（32MB 帧预算 ÷ base64 膨胀 ≈ 24MB body；超出回 413，P1 边界）
const MAX_REPLAY_BODY: usize = 24 * 1024 * 1024;

/// HTTP-over-mux 回放（任务 8）：帧 → 本地 Router oneshot → HttpResp 帧。
/// 合成 ConnectInfo(loopback)——oneshot 无连接信息时 needs_auth(allow_lan, None)
/// = true，allow_lan 节点会全 401（proposal §8 预警项）；节点本地信任模型不变。
async fn replay_http(r: axum::Router, frame: Frame) -> Frame {
    let req_id = match &frame {
        Frame::HttpReq { req_id, .. } => req_id.clone(),
        _ => String::new(),
    };
    let bad_request = |msg: String| {
        super::response_to_frame(
            &req_id,
            axum::http::StatusCode::BAD_REQUEST,
            vec![],
            format!("非法回放请求: {msg}").into_bytes(),
        )
    };
    let mut request = match super::frame_to_request(frame) {
        Ok(r) => r,
        Err(e) => return bad_request(e),
    };
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            0,
        ))));
    // Router Service 错误为 Infallible
    let resp = match r.oneshot(request).await {
        Ok(resp) => resp,
        Err(e) => match e {},
    };
    let (parts, body) = resp.into_parts();
    let bytes = match tokio::time::timeout(
        Duration::from_secs(30),
        axum::body::to_bytes(body, MAX_REPLAY_BODY),
    )
    .await
    {
        Ok(Ok(b)) => b.to_vec(),
        Ok(Err(_)) => {
            // 超帧上限（P1 边界）：413 回注
            return super::response_to_frame(
                &req_id,
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                vec![],
                b"response too large (P1 boundary: 32MB mux frame cap)".to_vec(),
            );
        }
        Err(_) => {
            return super::response_to_frame(
                &req_id,
                axum::http::StatusCode::BAD_GATEWAY,
                vec![],
                b"replay body timeout".to_vec(),
            );
        }
    };
    let headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    super::response_to_frame(&req_id, parts.status, headers, bytes)
}

/// hello-ok 首发的长效凭据落 node.json（0600；文件必须已由启动流程初始化）。
async fn persist_token(node_file: &Path, token: &str) -> anyhow::Result<()> {
    let mut cfg = NodeConfig::load(node_file)
        .ok_or_else(|| anyhow::anyhow!("node.json 不存在或不可解析，无法落盘 token"))?;
    cfg.token = Some(token.to_string());
    cfg.save(node_file).await?;
    Ok(())
}

async fn connect(gateway_url: &str) -> Result<WsStream, tokio_tungstenite::tungstenite::Error> {
    let url = format!("{}/ws/node", gateway_url.trim_end_matches('/'));
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodelink::{node_config::validate_gateway_url, MAX_FRAME_BYTES};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn test_timings() -> Timings {
        Timings {
            tick: Duration::from_millis(5),
            dead_after: Duration::from_millis(80),
            dial_timeout: Duration::from_millis(500),
        }
    }

    fn set_fast_step() {
        let _ = FAKE_STEP.set(Duration::from_millis(1));
    }

    fn frame_json(f: &Frame) -> String {
        serde_json::to_string(f).unwrap()
    }

    fn hello_ok_with_token(tok: &str) -> Frame {
        Frame::HelloOk {
            instance_id: "n-1".into(),
            max_frame_bytes: MAX_FRAME_BYTES,
            tick_secs: TICK_SECS,
            node_token: Some(tok.into()),
        }
    }

    /// 预置 node.json（启动流程语义：先有身份文件才有泵）。
    async fn seed_node_file(dir: &tempfile::TempDir) -> PathBuf {
        let file = dir.path().join("node.json");
        let cfg = NodeConfig {
            gateway_url: "ws://127.0.0.1:9".into(),
            node_id: "n-1".into(),
            node_name: "lab".into(),
            token: None,
            allow_insecure: false,
        };
        assert!(validate_gateway_url(&cfg.gateway_url, cfg.allow_insecure).is_ok());
        cfg.save(&file).await.unwrap();
        file
    }

    fn params_for(port: u16) -> NodeLinkParams {
        NodeLinkParams {
            gateway_url: format!("ws://127.0.0.1:{port}"),
            node_id: "n-1".into(),
            node_name: "lab".into(),
            token: None,
            enroll: Some("sek-enr-8888".into()),
            platform: "test".into(),
            drivers: vec!["claude".into()],
            hub_version: "0.0.0".into(),
            router: axum::Router::new(),
            local_port: 0,
            events_tx: None,
        }
    }

    /// 假网关对 hello 的回帧后处置
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Keep {
        /// 持续读入站（tungstenite 读路径自动回 pong——活性会话形态）
        Drain,
        /// 持有流**不读**：ping 堆积无人回 → 客户端侧真静默（判死测试专用；
        /// tungstenite 自动 pong 只发生在读路径，不读即无 pong）
        HoldSilent,
        /// 发完即关
        Close,
    }

    type Replies = Arc<dyn Fn(&Frame) -> (Vec<String>, Keep) + Send + Sync>;

    /// 假网关：accept 循环；每连接读首帧 hello → 按 replies 回帧 → 按 Keep 处置。
    /// counter 记连接数（拨号观测）。
    async fn spawn_fake_gateway(
        replies: Replies,
        counter: Arc<AtomicUsize>,
        hello_slot: Option<Arc<Mutex<Option<Frame>>>>,
    ) -> u16 {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let replies = replies.clone();
                let counter = counter.clone();
                let slot = hello_slot.clone();
                tokio::spawn(async move {
                    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    counter.fetch_add(1, Ordering::SeqCst);
                    let (mut sink, mut incoming) = ws.split();
                    let hello = loop {
                        match incoming.next().await {
                            Some(Ok(Message::Text(t))) => {
                                if let Ok(f) = serde_json::from_str::<Frame>(t.to_string().as_str())
                                {
                                    break f;
                                }
                            }
                            Some(Ok(_)) => continue,
                            _ => return,
                        }
                    };
                    if let Some(slot) = &slot {
                        *slot.lock().unwrap() = Some(hello.clone());
                    }
                    let (frames, keep) = replies(&hello);
                    for f in frames {
                        if sink.send(Message::Text(f.into())).await.is_err() {
                            return;
                        }
                    }
                    match keep {
                        Keep::Drain => while let Some(Ok(_)) = incoming.next().await {},
                        // 持流不读不关：客户端侧真静默
                        Keep::HoldSilent => futures::future::pending::<()>().await,
                        Keep::Close => {
                            let _ = sink.close().await;
                        }
                    }
                });
            }
        });
        port
    }

    /// N4 锚（永续退避）：连接失败必败可数（半开网关：接受 TCP 即断，WS 握手
    /// 永远失败）——失败远超 3 次仍持续拨号，与 tunnel 5 次放弃的语义分叉。
    #[tokio::test]
    async fn pump_retries_perpetually_on_connect_failure() {
        set_fast_step();
        let counter = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let half_open = counter.clone();
        tokio::spawn(async move {
            loop {
                let Ok((s, _)) = listener.accept().await else {
                    break;
                };
                half_open.fetch_add(1, Ordering::SeqCst);
                drop(s); // 立刻断开 → 客户端 WS 握手失败
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let file = seed_node_file(&dir).await;
        let params = params_for(port);
        tokio::spawn(run_node_link(params, file, test_timings()));
        tokio::time::sleep(Duration::from_millis(500)).await;
        let dials = counter.load(Ordering::SeqCst);
        assert!(
            dials >= 3,
            "连接失败必须永续重试（观察到 {dials} 次），不得走 tunnel 式放弃"
        );
    }

    /// N2/N3 锚：rejected 为终态——收到后泵挂起，零再拨号
    #[tokio::test]
    async fn pump_parks_after_rejected_and_never_redials() {
        set_fast_step();
        let counter = Arc::new(AtomicUsize::new(0));
        let slot: Arc<Mutex<Option<Frame>>> = Arc::new(Mutex::new(None));
        let dir = tempfile::tempdir().unwrap();
        let file = seed_node_file(&dir).await;
        let port = spawn_fake_gateway(
            Arc::new(|_h: &Frame| -> (Vec<String>, Keep) {
                (
                    vec![frame_json(&Frame::Rejected {
                        reason: "enroll code expired".into(),
                    })],
                    Keep::Drain,
                )
            }),
            counter.clone(),
            Some(slot.clone()),
        )
        .await;
        let params = params_for(port);
        tokio::spawn(run_node_link(params, file, test_timings()));
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "rejected 为终态：250ms 内（步长 1ms 本可容多次重拨）不得再拨号"
        );
        let hello = slot.lock().unwrap().clone().expect("hello 已捕获");
        match hello {
            Frame::Hello {
                node_id, enroll, ..
            } => {
                assert_eq!(node_id, "n-1");
                assert_eq!(enroll.as_deref(), Some("sek-enr-8888"), "首连带 enroll");
            }
            other => panic!("应为 hello: {other:?}"),
        }
    }

    /// pairing_required = Retry 语义：退避重试等待审批（OpenClaw PAIRING_REQUIRED 语义）
    #[tokio::test]
    async fn pump_retries_while_pairing_required() {
        set_fast_step();
        let counter = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let file = seed_node_file(&dir).await;
        let port = spawn_fake_gateway(
            Arc::new(|_h: &Frame| -> (Vec<String>, Keep) {
                (vec![frame_json(&Frame::PairingRequired)], Keep::Close)
            }),
            counter.clone(),
            None,
        )
        .await;
        let params = params_for(port);
        tokio::spawn(run_node_link(params, file, test_timings()));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            counter.load(Ordering::SeqCst) >= 2,
            "pairing 等待期必须持续退避重试（非终态）"
        );
    }

    /// N5 锚：hello-ok 首发带 node token → 0600 落盘
    #[tokio::test]
    async fn pump_persists_issued_token_0600() {
        set_fast_step();
        let counter = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let file = seed_node_file(&dir).await;
        let port = spawn_fake_gateway(
            Arc::new(|_h: &Frame| -> (Vec<String>, Keep) {
                (
                    vec![frame_json(&hello_ok_with_token("sek-tok-4242"))],
                    Keep::Drain,
                )
            }),
            counter.clone(),
            None,
        )
        .await;
        let params = params_for(port);
        tokio::spawn(run_node_link(params, file.clone(), test_timings()));
        tokio::time::sleep(Duration::from_millis(250)).await;
        let cfg = NodeConfig::load(&file).expect("node.json 可解析");
        assert_eq!(
            cfg.token.as_deref(),
            Some("sek-tok-4242"),
            "首发 token 已落盘"
        );
        assert_eq!(
            crate::platform::file_mode(&file),
            Some(0o600),
            "token 落盘必须 0600"
        );
    }

    /// 静默判死：对端不发任何帧 → 2×tick 窗口内主动收场（Retry）
    #[tokio::test]
    async fn serve_session_dead_on_silence() {
        set_fast_step();
        let counter = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let file = seed_node_file(&dir).await;
        let port = spawn_fake_gateway(
            Arc::new(|_h: &Frame| -> (Vec<String>, Keep) { (vec![], Keep::HoldSilent) }),
            counter.clone(),
            None,
        )
        .await;
        let params = params_for(port);
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/node"))
            .await
            .unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            serve_session(ws, &params, &file, test_timings()),
        )
        .await
        .expect("会话应在判死窗口后收场，不悬挂");
        assert_eq!(outcome, SessionOutcome::Retry);
    }

    /// 凭据不入日志锚（credentials_never_in_logs 模式）：token 与 enroll 明文
    /// 不得出现在会话日志，掩码形态（首 4 + 长度）是唯一合法出现方式。
    #[test]
    fn serve_session_logs_mask_credentials() {
        set_fast_step();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        // 会话建立与日志捕获分两段：连接在 subscriber 外完成，会话期日志经
        // with_default 捕获（线程局部，spawn 的任务不继承——被测代码路径全内联）
        let (ws, params, file) = rt.block_on(async {
            let counter = Arc::new(AtomicUsize::new(0));
            let port = spawn_fake_gateway(
                Arc::new(|_h: &Frame| -> (Vec<String>, Keep) {
                    (
                        vec![frame_json(&hello_ok_with_token("sek-tok-9999"))],
                        Keep::Drain,
                    )
                }),
                counter,
                None,
            )
            .await;
            let file = seed_node_file(&dir).await;
            let params = params_for(port);
            let (ws, _) =
                tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/node"))
                    .await
                    .unwrap();
            (ws, params, file)
        });
        let buf = SharedBuf::default();
        let writer = buf.clone();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(Arc::new(sub), || {
            rt.block_on(async {
                let _ = tokio::time::timeout(
                    Duration::from_secs(2),
                    serve_session(ws, &params, &file, test_timings()),
                )
                .await;
            });
        });
        let out = buf.string();
        assert!(!out.contains("sek-tok-9999"), "token 明文不得进日志: {out}");
        assert!(
            !out.contains("sek-enr-8888"),
            "enroll 明文不得进日志: {out}"
        );
        assert!(
            out.contains(&mask_secret("sek-enr-8888")),
            "凭据日志应为掩码形态（mask_secret）: {out}"
        );
    }

    /// 测试用共享日志缓冲（MakeWriter 目标；Arc<Mutex<Vec<u8>>> 非 io::Write，自实现）
    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn string(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}
