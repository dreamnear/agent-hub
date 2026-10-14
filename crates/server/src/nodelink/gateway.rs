//! /ws/node 网关侧（agent-hub-gateway-node-p1 任务 5）：握手帧鉴权 + 连接治理。
//!
//! - **首帧内凭据**（非 Bearer）：hello 帧 token / enroll code 二选一，缺失或
//!   错误回显式拒绝帧后断开（**不裸 close**——N3：显式分类不误报网络错误）。
//! - **握手分类**：enroll 校验（任务 4 状态机）→ 未批准：pending 登记 +
//!   `PairingRequired`；已批准：签发 node token（UUIDv4、存哈希、经本连接回）
//!   并回 `HelloOk`；token 哈希比对 approved 表通过则回 `HelloOk`；rejected
//!   记录回 `Rejected` 帧。
//! - **连接治理**：同 node_id 二连顶替（旧连接作废）；活连接表（node_id →
//!   命令通道）供代理面查询（B3 注入 http_req）；watchdog（2×tick 静默判死
//!   断开）；presence 刷新（platform/drivers/last_seen）。
//!
//! 路由挂载与 auth 中间件豁免见 lib.rs / auth/mod.rs（节点无 hub token，
//! 401 于中间件层 = N3 禁止的误报形态）。

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

use axum::{
    extract::{State, WebSocketUpgrade},
    response::Response,
};
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;

use super::{
    dead_after,
    registry::{EnrollError, NodeStatus, TokenCheck},
    Frame, MAX_FRAME_BYTES, TICK_SECS,
};
use crate::api::SharedState;

/// 握手首帧等待（超时未发 hello 即断；不给未认证连接挂着的理由）
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 活连接表（node_id → 句柄）。gen 计数保证顶替语义：旧连接退出时只有 gen
/// 匹配才摘表（防顶替后旧连接误摘新连接的登记）。
#[derive(Default, Clone)]
pub struct NodeConnections {
    inner: Arc<Mutex<HashMap<String, ConnHandle>>>,
    gen: Arc<AtomicU64>,
    /// reqId 对账表（HTTP-over-mux 代理面）
    pending: PendingTable,
    /// WS-over-mux 活流表（P2）：stream_id → 浏览器侧下行入口（proxy 泵的
    /// mpsc）。serve_node 收 WsData/WsClose 按表转发；节点断开 flush_streams。
    streams: StreamTable,
    /// 开流窗口缓冲（无流上行帧；register 时 flush）
    orphans: OrphanBuffer,
    /// 轮换回执等待表（sweep S1：TokenRotatedAck 对账 / 断开回滚）
    rotates: RotateWaiters,
}

/// stream_id → 浏览器侧下行入口 + seq 去重锚（W3：断线重放的旧 seq 丢弃）。
type StreamTable = Arc<Mutex<HashMap<String, StreamEntry>>>;

struct StreamEntry {
    down_tx: mpsc::Sender<axum::extract::ws::Message>,
    /// seq 去重锚：None = 首帧未定锚（任何首 seq 放行并记锚）——节点起始 seq=0，
    /// 初始锚 0 会把首帧恒判重放（P2 tester 缺陷①）；防重放只作用于同流后续帧。
    last_seq: Option<u64>,
}

/// 开流窗口缓冲（P2 竞态修复）：浏览器首包可能早于节点本地流登记到达
/// （WsOpen → 节点拨本地 → 插表，毫秒级窗口；gw-demo/集成测试实测踩中）。
/// 无流上行帧缓存于此（上限 64 帧背压），register_stream 时 flush。
type OrphanBuffer = Arc<Mutex<HashMap<String, Vec<axum::extract::ws::Message>>>>;

const ORPHAN_BUFFER_CAP: usize = 64;

#[derive(Clone)]
pub struct ConnHandle {
    gen: u64,
    /// 网关→节点命令通道（代理面注入 http_req）
    pub cmd_tx: mpsc::Sender<Frame>,
    /// 连接存活信号（代理面 /p/ws/events 保持与断开判定，任务 9）
    pub closed: tokio::sync::watch::Sender<bool>,
}

/// reqId → 等待中的代理响应（HTTP-over-mux 对账；任务 8）。
/// 超时回收：proxy 侧带超时等待 + 自摘；节点断开 flush_node 即刻失败挂起请求。
type PendingMap =
    HashMap<(String, String), tokio::sync::oneshot::Sender<axum::http::Response<axum::body::Body>>>;

#[derive(Default, Clone)]
pub struct PendingTable {
    inner: Arc<Mutex<PendingMap>>,
}

impl PendingTable {
    pub fn insert(
        &self,
        node_id: &str,
        req_id: &str,
        tx: tokio::sync::oneshot::Sender<axum::http::Response<axum::body::Body>>,
    ) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((node_id.to_string(), req_id.to_string()), tx);
    }

    /// 取走（对账成功路径；take 防重放）
    pub fn take(
        &self,
        node_id: &str,
        req_id: &str,
    ) -> Option<tokio::sync::oneshot::Sender<axum::http::Response<axum::body::Body>>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(node_id.to_string(), req_id.to_string()))
    }

    /// 节点断开：该节点全部挂起请求即刻失败（不等 proxy 超时）
    fn flush_node(&self, node_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(id, _), _| id != node_id);
    }
}

/// 轮换回执等待表（sweep S1）：node_id → (期望新 token, oneshot)。
/// rotate 签发前挂起，serve_node 收 TokenRotatedAck 按 token 对账代际放行；
/// 连接断开/停机 flush 按失败放行（rotate 侧统一回滚旧哈希）。
type RotateWaiterMap = HashMap<String, (String, tokio::sync::oneshot::Sender<bool>)>;

#[derive(Default, Clone)]
pub struct RotateWaiters {
    inner: Arc<Mutex<RotateWaiterMap>>,
}

impl RotateWaiters {
    /// 原子占位（终审 BLOCKER 修）：在途轮换存在即拒绝（false）——双击/并发
    /// 轮换串行化。check-then-insert 分离会留 TOCTOU 窗（两请求都过检查后
    /// 互相覆盖，w1 的 tx drop 使 rotate1 误入超时回滚分支，回滚到节点从未
    /// 持有的 token_old0 → 哈希/磁盘双向错位锁死），必须单锁内判定+占位。
    pub fn try_insert(
        &self,
        node_id: &str,
        expected_token: String,
        tx: tokio::sync::oneshot::Sender<bool>,
    ) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.contains_key(node_id) {
            return false;
        }
        g.insert(node_id.to_string(), (expected_token, tx));
        true
    }

    /// 对账取走：token 匹配才放行（防上一轮迟到 ack 顶替新一轮等待）；不匹配丢弃。
    pub fn take(&self, node_id: &str, token: &str) -> Option<tokio::sync::oneshot::Sender<bool>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match g.get(node_id) {
            Some((expected, _)) if expected == token => g.remove(node_id).map(|(_, tx)| tx),
            _ => None,
        }
    }

    /// 无回执摘除（送达失败等，不发结果——调用方不等了）。
    pub fn remove(&self, node_id: &str) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(node_id);
    }

    /// 连接断开：挂起轮换按失败放行（rotate 侧回滚旧哈希）。
    pub fn flush_node(&self, node_id: &str) {
        if let Some((_, tx)) = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(node_id)
        {
            let _ = tx.send(false);
        }
    }

    /// 停机：全体按失败放行。
    pub fn flush_all(&self) {
        let waiters: Vec<_> = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *g).into_values().collect()
        };
        for (_, tx) in waiters {
            let _ = tx.send(false);
        }
    }
}

impl NodeConnections {
    pub fn new() -> Self {
        Self::default()
    }

    /// 节点在线？（代理面 503 判定用）
    pub fn is_online(&self, node_id: &str) -> bool {
        self.inner
            .lock()
            .map(|g| g.contains_key(node_id))
            .unwrap_or(false)
    }

    /// 命令通道（代理面注入 http_req）
    pub fn cmd_tx(&self, node_id: &str) -> Option<mpsc::Sender<Frame>> {
        self.inner
            .lock()
            .ok()?
            .get(node_id)
            .map(|h| h.cmd_tx.clone())
    }

    /// 连接存活信号（/p/ws/events holder 断开判定）
    pub fn closed_rx(&self, node_id: &str) -> Option<tokio::sync::watch::Receiver<bool>> {
        self.inner
            .lock()
            .ok()?
            .get(node_id)
            .map(|h| h.closed.subscribe())
    }

    /// reqId 对账表（代理面注册挂起请求）
    pub fn pending(&self) -> &PendingTable {
        &self.pending
    }

    /// 轮换回执等待表（sweep S1）
    pub fn rotates(&self) -> &RotateWaiters {
        &self.rotates
    }

    /// 全体断开（网关优雅停机 / 测试模拟进程重启）：清空表 → 各 serve_node 的
    /// cmd_rx.recv() None 臂退出 → 连接任务 drop 关 TCP → 节点感知断开始退避
    /// 重连（N4 双向回归的网关侧断开语义）。
    pub fn shutdown_all(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
        // 挂起轮换按失败放行（rotate 侧回滚；停机后新哈希无人确认持有）
        self.rotates.flush_all();
    }

    // ===== WS-over-mux 活流表（P2 任务 1）=====

    /// 开流登记：返回下行入口（proxy 泵转发浏览器消息）。
    pub fn register_stream(
        &self,
        stream_id: &str,
        down_tx: mpsc::Sender<axum::extract::ws::Message>,
    ) {
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                stream_id.to_string(),
                StreamEntry {
                    down_tx: down_tx.clone(),
                    last_seq: None,
                },
            );
        // flush 开流窗口缓冲：浏览器首包早于本登记到达的帧按到达序补投
        if let Ok(mut orphans) = self.orphans.lock() {
            if let Some(buf) = orphans.remove(stream_id) {
                if let Some(entry) = self
                    .streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(stream_id)
                {
                    for m in buf {
                        let _ = entry.down_tx.try_send(m);
                    }
                }
            }
        }
    }

    /// 流下行转发（serve_node 收 WsData）：seq 去重（<= last 丢弃）+ 投递浏览器泵。
    /// 返回 false = 流不存在（已收口/节点顶替），调用方可忽略。
    fn forward_stream_data(
        &self,
        stream_id: &str,
        seq: u64,
        msg: axum::extract::ws::Message,
    ) -> bool {
        let mut g = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = g.get_mut(stream_id) {
            match entry.last_seq {
                Some(last) if seq <= last => {
                    tracing::debug!(stream_id, seq, last, "ws_data 旧 seq（重放），丢弃");
                    return true;
                }
                _ => {}
            }
            entry.last_seq = Some(seq);
            let _ = entry.down_tx.try_send(msg);
            return true;
        }
        drop(g);
        // 开流窗口：流尚未登记（WsOpen 在途/节点拨号中）——缓冲待 register flush
        if let Ok(mut orphans) = self.orphans.lock() {
            let buf = orphans.entry(stream_id.to_string()).or_default();
            if buf.len() < ORPHAN_BUFFER_CAP {
                buf.push(msg);
                tracing::debug!(
                    stream_id,
                    seq,
                    buffered = buf.len(),
                    "ws_data 早到，入开流窗口缓冲"
                );
                return true;
            }
            tracing::warn!(
                stream_id,
                "开流窗口缓冲满（{}），丢弃上行帧",
                ORPHAN_BUFFER_CAP
            );
        }
        false
    }

    /// 收流（serve_node 收 WsClose 或节点断开 flush）：摘表 + 通知浏览器泵关断。
    pub fn close_stream(&self, stream_id: &str, reason: Option<String>) {
        if let Some(entry) = self
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(stream_id)
        {
            let _ = entry
                .down_tx
                .try_send(axum::extract::ws::Message::Close(reason.map(|r| {
                    axum::extract::ws::CloseFrame {
                        code: axum::extract::ws::close_code::ERROR,
                        reason: r.into(),
                    }
                })));
        }
    }

    /// 活流清口（网关停机/测试）：摘全部流并通知各浏览器泵关断。
    pub fn flush_streams(&self) {
        if let Ok(mut o) = self.orphans.lock() {
            o.clear();
        }
        let entries: Vec<_> = self
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        for (_, entry) in entries {
            let _ = entry
                .down_tx
                .try_send(axum::extract::ws::Message::Close(None));
        }
    }

    /// 登记（同 id 二连顶替：旧条目被替换 → 旧通道所有者 recv None 退出）。
    /// 返回本次登记的 gen。
    fn attach(&self, node_id: &str, handle: ConnHandle) -> u64 {
        let gen = self.gen.fetch_add(1, Ordering::SeqCst);
        let mut handle = handle;
        handle.gen = gen;
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(node_id.to_string(), handle);
        gen
    }

    /// 摘表（仅 gen 匹配才摘——顶替后旧连接退出不得误摘新连接登记）
    fn detach(&self, node_id: &str, gen: u64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, h| !(id == node_id && h.gen == gen));
    }
}

/// /ws/node 路由 handler（lib.rs 挂载；auth 中间件豁免该路径）
pub async fn node_ws(State(state): State<SharedState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| handle_node_socket(state, socket))
}

/// 单连接生命周期：握手 → 连接治理 → serve 循环。
async fn handle_node_socket(
    state: Arc<crate::api::AppState>,
    socket: axum::extract::ws::WebSocket,
) {
    let (mut sink, mut incoming) = socket.split();
    // 首帧 hello（超时/非协议帧 = 显式拒绝后断开，不裸 close）
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming.next()).await;
    let hello = match first {
        Ok(Some(Ok(axum::extract::ws::Message::Text(t)))) => {
            serde_json::from_str::<Frame>(t.as_str()).ok()
        }
        _ => None,
    };
    let Some(Frame::Hello {
        node_id,
        node_name,
        token,
        enroll,
        platform,
        drivers,
        stats,
        ..
    }) = hello
    else {
        let _ = sink
            .send(ax_text(&Frame::NodeTokenRejected {
                reason: "握手失败：首帧不是合法 hello（或超时）".into(),
            }))
            .await;
        return;
    };
    // 握手分类（注册表为唯一事实源）
    let now = SystemTime::now();
    let admission = admit(
        &state.nodes,
        &node_id,
        &node_name,
        &token,
        &enroll,
        &platform,
        &drivers,
        stats.clone(),
        now,
    );
    match admission {
        Admission::Reject(frame) => {
            let _ = sink.send(ax_text(&frame)).await;
        }
        Admission::Pairing => {
            // 客户端退避重试等待审批；socket 即断（P1 语义）
            let _ = sink.send(ax_text(&Frame::PairingRequired)).await;
        }
        Admission::Admitted { node_token } => {
            // 二连顶替 + presence 刷新 + hello-ok
            let (cmd_tx, mut cmd_rx) = mpsc::channel::<Frame>(64);
            let (closed, _) = tokio::sync::watch::channel(false);
            let gen = state.node_conns.attach(
                &node_id,
                ConnHandle {
                    gen: 0,
                    cmd_tx,
                    closed,
                },
            );
            state
                .nodes
                .refresh_presence_with_stats(&node_id, &platform, &drivers, stats.clone());
            // 实例 nodeMeta 自愈回写（B3 tester FAIL-1 第二层）：approve 快照可能
            // 早于真实元数据登记（历史挂载/网关重启时序），节点每次接入即对齐——
            // 仅刷新已挂载的 node-link 实例，不复活被手动删除的实例
            let store = crate::instances::InstancesStore::new(&state.cfg);
            if let Some(mut inst) = store.get(&node_id).await {
                if inst.mode == crate::instances::InstanceMode::NodeLink {
                    let meta = crate::instances::NodeMeta {
                        platform: platform.clone(),
                        drivers: drivers.clone(),
                    };
                    if inst.node_meta.as_ref() != Some(&meta) {
                        inst.node_meta = Some(meta);
                        let _ = store.upsert(&inst).await;
                    }
                }
            }
            let ok = Frame::HelloOk {
                instance_id: node_id.clone(),
                max_frame_bytes: MAX_FRAME_BYTES,
                tick_secs: TICK_SECS,
                node_token,
            };
            if sink.send(ax_text(&ok)).await.is_err() {
                state.node_conns.detach(&node_id, gen);
                return;
            }
            tracing::info!(node_id = %node_id, node_name = %node_name, "节点已接入（hello-ok）");
            // 重连重同步（P2 任务 3）：节点接入即强制 Tick——前端 ws/events 收
            // tick 重拉 agents，节点实例侧栏即时对齐（置灰解除的刷新信号）
            let _ = state.events_tx.send(crate::drivers::events::HubEvent::Tick);
            serve_node(sink, incoming, &mut cmd_rx, state.clone(), node_id, gen).await;
        }
    }
}

/// 握手裁决（注册表唯一事实源；分类帧在此定形）。
#[allow(clippy::too_many_arguments)]
fn admit(
    nodes: &super::registry::NodeRegistry,
    node_id: &str,
    node_name: &str,
    token: &Option<String>,
    enroll: &Option<String>,
    platform: &str,
    drivers: &[String],
    stats: Option<crate::nodelink::HostStats>,
    now: SystemTime,
) -> Admission {
    if let Some(tok) = token {
        return match nodes.verify_token(node_id, tok) {
            TokenCheck::Verified => Admission::Admitted { node_token: None },
            TokenCheck::Rejected => Admission::Reject(Box::new(Frame::Rejected {
                reason: "节点已被拒绝（rejected 终态）".into(),
            })),
            TokenCheck::Invalid => Admission::Reject(Box::new(Frame::NodeTokenRejected {
                reason: "node token 无效或节点未批准".into(),
            })),
        };
    }
    if let Some(code) = enroll {
        return match nodes.enroll_validate(code, now) {
            Err(EnrollError::Unknown) => Admission::Reject(Box::new(Frame::Rejected {
                reason: "enrollment code 无效（未知）".into(),
            })),
            Err(EnrollError::Expired) => Admission::Reject(Box::new(Frame::Rejected {
                reason: "enrollment code 已过期（10 分钟有效期）".into(),
            })),
            Err(EnrollError::AlreadyUsed) => Admission::Reject(Box::new(Frame::Rejected {
                reason: "enrollment code 已使用（换取 token 后作废）".into(),
            })),
            Ok(()) => {
                // rejected 记录优先于 code 有效性（终态语义）
                if let Some(rec) = nodes.get(node_id) {
                    if rec.status == NodeStatus::Rejected {
                        return Admission::Reject(Box::new(Frame::Rejected {
                            reason: "节点已被拒绝（rejected 终态）".into(),
                        }));
                    }
                }
                match nodes.get(node_id).map(|r| r.status) {
                    Some(NodeStatus::Approved) => {
                        // 换取即消费 + 签发（UUIDv4；存哈希零明文）
                        nodes.enroll_consume(code);
                        let token_plain = uuid::Uuid::new_v4().to_string();
                        nodes.issue_token(node_id, &token_plain);
                        Admission::Admitted {
                            node_token: Some(token_plain),
                        }
                    }
                    _ => {
                        // 首连挂起：pending 登记（code 未消费，审批后换 token）。
                        // 元数据随 hello 落库（B3 tester FAIL-1：此前硬编码空值，
                        // approve 快照因此恒空、实例 nodeMeta 冻结）
                        nodes.upsert_pending(node_id, node_name, platform, drivers, stats.clone());
                        nodes.enroll_mark_pending(code);
                        Admission::Pairing
                    }
                }
            }
        };
    }
    Admission::Reject(Box::new(Frame::NodeTokenRejected {
        reason: "缺少凭据：hello 需带 node token 或 enrollment code".into(),
    }))
}

#[derive(Debug)]
enum Admission {
    /// 回显式拒绝帧后断开（Box 压变体大小差——Frame 随 P3 stats 增大）
    Reject(Box<Frame>),
    /// pending 登记 + 退避重试等待审批
    Pairing,
    /// 接入（node_token = 审批握手首发的长效凭据，其余 None）
    Admitted { node_token: Option<String> },
}

/// 已认证连接循环：网关→节点命令注入（B3 管道）+ 入站帧 + watchdog 判死。
async fn serve_node(
    mut sink: futures::stream::SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>,
    mut incoming: futures::stream::SplitStream<axum::extract::ws::WebSocket>,
    cmd_rx: &mut mpsc::Receiver<Frame>,
    state: Arc<crate::api::AppState>,
    node_id: String,
    gen: u64,
) {
    let dead_after = dead_after();
    let mut last_inbound = tokio::time::Instant::now();
    loop {
        let until_dead = (last_inbound + dead_after)
            .checked_duration_since(tokio::time::Instant::now())
            .unwrap_or(Duration::ZERO);
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(frame) => {
                    if sink.send(ax_text(&frame)).await.is_err() {
                        break;
                    }
                }
                None => break, // 顶替：新连接接管，本连接退出
            },
            waited = tokio::time::timeout(until_dead, incoming.next()) => match waited {
                Err(_) => {
                    tracing::warn!(node_id = %node_id, silent = ?dead_after, "节点静默超判死窗口，断开");
                    break;
                }
                Ok(None) => break,
                Ok(Some(Err(_))) => break,
                Ok(Some(Ok(msg))) => {
                    last_inbound = tokio::time::Instant::now();
                    match msg {
                        axum::extract::ws::Message::Ping(p) => {
                            let _ = sink.send(axum::extract::ws::Message::Pong(p)).await;
                        }
                        axum::extract::ws::Message::Text(t) => {
                            match serde_json::from_str::<Frame>(t.as_str()) {
                                // 回放结果对账（HTTP-over-mux 任务 8）：帧 → axum
                                // Response → 经挂起 oneshot 发还代理面
                                Ok(parsed) => match parsed {
                                    Frame::HttpResp { ref req_id, .. } => {
                                        match super::response_from_frame(&parsed) {
                                            Ok(resp) => {
                                                if let Some(tx) = state
                                                    .node_conns
                                                    .pending()
                                                    .take(&node_id, req_id)
                                                {
                                                    let _ = tx.send(resp);
                                                } else {
                                                    tracing::debug!(
                                                        node_id = %node_id,
                                                        req_id = %req_id,
                                                        "http_resp 无挂起请求（超时已回收），丢弃"
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                tracing::debug!(node_id = %node_id, error = %e, "http_resp 重建失败")
                                            }
                                        }
                                    }
                                    // WS-over-mux 流下行（P2）：流数据 → seq 去重 →
                                    // 浏览器泵投递；收流 → 摘表 + Close 通知
                                    Frame::WsData {
                                        ref stream_id,
                                        seq,
                                        ref text,
                                        ref data,
                                    } => {
                                        use base64::Engine as _;
                                        let msg = match (text, data) {
                                            (Some(t), _) => Some(
                                                axum::extract::ws::Message::Text(t.as_str().into()),
                                            ),
                                            (_, Some(d)) => base64::engine::general_purpose::STANDARD
                                                .decode(d.as_bytes())
                                                .ok()
                                                .map(|b| axum::extract::ws::Message::Binary(b.into())),
                                            _ => None,
                                        };
                                        match msg {
                                            Some(m) => {
                                                if !state.node_conns.forward_stream_data(stream_id, seq, m) {
                                                }
                                            }
                                            None => tracing::debug!(stream_id, "ws_data 无内容（text/data 双空），忽略"),
                                        }
                                    }
                                    Frame::WsClose { ref stream_id, ref reason } => {
                                        state.node_conns.close_stream(stream_id, reason.clone());
                                    }
                                    // 事件回传（P2 任务 2）：节点 Tick 转投网关 events
                                    // 广播面（前端 ws/events 收 tick 即重拉 agents）
                                    Frame::NodeTick => {
                                        let _ = state.events_tx.send(crate::drivers::events::HubEvent::Tick);
                                    }
                                    // 轮换回执（sweep S1）：persist 结果按 token 代际
                                    // 对账放行——rotate 侧超时/失败统一回滚旧哈希
                                    Frame::TokenRotatedAck { ref token, ok } => {
                                        if let Some(tx) =
                                            state.node_conns.rotates().take(&node_id, token)
                                        {
                                            let _ = tx.send(ok);
                                        } else {
                                            tracing::debug!(
                                                node_id = %node_id,
                                                token = %super::mask_secret(token),
                                                ok,
                                                "rotate_ack 无挂起轮换（超时已回收/代际不匹配），丢弃"
                                            );
                                        }
                                    }
                                    _ => tracing::debug!(node_id = %node_id, "意外帧类型，忽略"),
                                },
                                Err(_) => tracing::debug!(node_id = %node_id, "非协议 JSON 帧，忽略"),
                            }
                        }
                        axum::extract::ws::Message::Close(_) => break,
                        _ => {} // pong/binary：活性证据
                    }
                }
            },
        }
    }
    state.node_conns.detach(&node_id, gen);
    // 节点断开：挂起代理请求即刻失败（不等 proxy 30s 超时；detach 掉 ConnHandle
    // 后 closed watch sender 随之 drop，holder 侧以 Closed 事件收口）
    state.node_conns.pending().flush_node(&node_id);
    // 挂起轮换按失败放行（sweep S1：断开即无回执，rotate 侧回滚旧哈希）
    state.node_conns.rotates().flush_node(&node_id);
    tracing::info!(node_id = %node_id, "节点连接结束");
}

fn ax_text(frame: &Frame) -> axum::extract::ws::Message {
    axum::extract::ws::Message::Text(serde_json::to_string(frame).expect("帧序列化").into())
}

// ===== 单测：连接表顶替语义（纯内存，无 WS）=====
#[cfg(test)]
mod tests {
    use super::super::{constant_time_eq_str, sha256_hex};
    use super::*;

    fn channel() -> (mpsc::Sender<Frame>, mpsc::Receiver<Frame>) {
        mpsc::channel(8)
    }

    /// 同 id 二连顶替：旧通道 recv None 退出；detach 只摘匹配 gen
    #[tokio::test]
    async fn attach_replaces_and_detach_is_generation_scoped() {
        let conns = NodeConnections::new();
        let (tx1, mut rx1) = channel();
        let (closed1, _) = tokio::sync::watch::channel(false);
        let g1 = conns.attach(
            "n-1",
            ConnHandle {
                gen: 0,
                cmd_tx: tx1.clone(),
                closed: closed1,
            },
        );
        assert!(conns.is_online("n-1"));
        assert!(conns.cmd_tx("n-1").is_some());

        // 二连顶替
        let (tx2, mut rx2) = channel();
        let (closed2, _) = tokio::sync::watch::channel(false);
        let g2 = conns.attach(
            "n-1",
            ConnHandle {
                gen: 0,
                cmd_tx: tx2.clone(),
                closed: closed2,
            },
        );
        assert_ne!(g1, g2);
        assert!(conns.is_online("n-1"));

        // 顶替后旧连接退出（顶替时 map 值被替换 → 旧通道 map 侧 clone 已 drop；
        // 本地 tx1 仍在调用方手里 → 再 drop 即全 owner 消失）
        conns.detach("n-1", g1);
        assert!(conns.is_online("n-1"), "旧 gen detach 不得误摘新登记");
        drop(tx1);
        assert!(
            rx1.recv().await.is_none(),
            "旧连接 recv None = 顶替退出信号"
        );

        // 新连接退出：本地 tx2 + map 登记（detach=g2）双 drop → rx2 None
        conns.detach("n-1", g2);
        assert!(!conns.is_online("n-1"));
        drop(tx2);
        assert!(rx2.recv().await.is_none());
    }

    /// P2 tester 缺陷①锚：首帧任意 seq（含 0）放行——初始无锚；防重放只作用于
    /// 同流后续帧（seq=0 首帧后，再来 seq=0 丢弃）。
    #[test]
    fn stream_first_frame_any_seq_accepted() {
        let conns = NodeConnections::new();
        let (down_tx, mut down_rx) = mpsc::channel(8);
        conns.register_stream("s-1", down_tx);

        // 首帧 seq=0 放行（缺陷①修复前恒吞）
        assert!(
            conns.forward_stream_data("s-1", 0, axum::extract::ws::Message::Text("first".into()),),
            "首帧 seq=0 应放行"
        );
        match down_rx.try_recv() {
            Ok(axum::extract::ws::Message::Text(t)) => assert_eq!(t, "first"),
            other => panic!("首帧应投递: {other:?}"),
        }

        // 防重放语义不动：后续 seq=0（重放）丢弃、不投递
        assert!(conns.forward_stream_data(
            "s-1",
            0,
            axum::extract::ws::Message::Text("replay".into()),
        ));
        assert!(down_rx.try_recv().is_err(), "重放帧不得投递");

        // 新 seq 单调放行
        assert!(conns.forward_stream_data(
            "s-1",
            5,
            axum::extract::ws::Message::Text("next".into()),
        ));
        match down_rx.try_recv() {
            Ok(axum::extract::ws::Message::Text(t)) => assert_eq!(t, "next"),
            other => panic!("新 seq 应投递: {other:?}"),
        }
    }

    /// admit 纯逻辑：缺凭据 / 未知 code / 审批后换取 / 二次使用
    #[test]
    fn admit_classification() {
        let dir = tempfile::tempdir().unwrap();
        let nodes = super::super::registry::NodeRegistry::new(dir.path().join("nodes.json"));
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let (code, _) = nodes.create_enrollment(now);

        // 缺凭据
        let none: Option<String> = None;
        assert!(matches!(
            admit(&nodes, "n-1", "lab", &none, &none, "", &[], None, now),
            Admission::Reject(frame) if matches!(*frame, Frame::NodeTokenRejected { .. })
        ));
        // 未知 code
        assert!(matches!(
            admit(
                &nodes,
                "n-1",
                "lab",
                &none,
                &Some("enr-nope".into()),
                "",
                &[],
                None,
                now
            ),
            Admission::Reject(frame) if matches!(*frame, Frame::Rejected { .. })
        ));
        // 首连：pending 登记 + Pairing
        assert!(matches!(
            admit(
                &nodes,
                "n-1",
                "lab",
                &none,
                &Some(code.clone()),
                "macos-aarch64",
                &["claude".into()],
                Some(crate::nodelink::HostStats {
                    os: "macos".into(),
                    arch: "aarch64".into(),
                    version: "0.1.0".into(),
                    uptime_secs: 42,
                }),
                now
            ),
            Admission::Pairing
        ));
        assert_eq!(nodes.get("n-1").unwrap().status, NodeStatus::Pending);
        // 首连元数据随 hello 落库（B3 tester FAIL-1 修复：Pairing 臂不再硬编码空值）
        assert_eq!(nodes.get("n-1").unwrap().platform, "macos-aarch64");
        assert_eq!(nodes.get("n-1").unwrap().drivers, vec!["claude"]);
        // approve 后同 code 换取：首发 token
        nodes.approve("n-1").unwrap();
        let admitted = match admit(
            &nodes,
            "n-1",
            "lab",
            &none,
            &Some(code.clone()),
            "",
            &[],
            None,
            now,
        ) {
            Admission::Admitted { node_token } => node_token.expect("首发 token"),
            other => panic!("应换取成功: {other:?}"),
        };
        // 二次使用同 code → Rejected
        assert!(matches!(
            admit(
                &nodes,
                "n-1",
                "lab",
                &none,
                &Some(code.clone()),
                "",
                &[],
                None,
                now
            ),
            Admission::Reject(frame) if matches!(*frame, Frame::Rejected { .. })
        ));
        // 换来的 token 走 token 路径 Verified
        assert!(matches!(
            admit(
                &nodes,
                "n-1",
                "lab",
                &Some(admitted),
                &none,
                "",
                &[],
                None,
                now
            ),
            Admission::Admitted { node_token: None }
        ));
        // 错 token → NodeTokenRejected
        assert!(matches!(
            admit(
                &nodes,
                "n-1",
                "lab",
                &Some("wrong".into()),
                &none,
                "",
                &[],
                None,
                now
            ),
            Admission::Reject(frame) if matches!(*frame, Frame::NodeTokenRejected { .. })
        ));
    }

    /// sha256/constant_time_eq_str 契约（registry 侧已有；此处锚 gateway 引用面）
    #[test]
    fn hash_utils_contract() {
        assert!(constant_time_eq_str(&sha256_hex("a"), &sha256_hex("a")));
        assert!(!constant_time_eq_str(&sha256_hex("a"), &sha256_hex("b")));
    }
}
