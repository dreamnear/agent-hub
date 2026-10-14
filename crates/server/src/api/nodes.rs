//! 节点审批 REST（agent-hub-gateway-node-p1 任务 6；API 先行，UI 卡片后补）。
//!
//! - `POST /api/nodes/enroll-code` → {code（明文仅此一次）, command, expiresIn}
//! - `GET  /api/nodes` → {pending, approved} 清单——token 哈希与任何凭据材料
//!   **不出响应**（红线：网关零明文，哈希也不外泄）
//! - `POST /api/nodes/{node_id}/approve` → 注册表 pending→approved +
//!   node-link 实例挂载（侧栏分组即挂载锚，N1）
//! - `POST /api/nodes/{node_id}/reject` → rejected 终态（N2）+ 实例卸载
//!
//! 审批路由走既有 hub 认证（/api 前缀自然覆盖；loopback 直通语义不变）。

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap},
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use std::time::SystemTime;

use crate::{
    api::SharedState,
    error::AppError,
    instances::{InstanceConfig, InstanceMode, InstancesStore, NodeMeta},
    nodelink::registry::NodeStatus,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/nodes/enroll-code", post(enroll_code))
        .route("/api/nodes", get(list_nodes))
        .route("/api/nodes/{node_id}/approve", post(approve_node))
        .route("/api/nodes/{node_id}/reject", post(reject_node))
        // P3 M2/M5：token 轮换（在线回显）+ 改名（registry+实例表同步）
        .route("/api/nodes/{node_id}/rotate", post(rotate_token))
        .route("/api/nodes/{node_id}/rename", post(rename_node))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrollResponse {
    /// 明文仅此一次（网关只存哈希）
    code: String,
    /// one-paste 命令串（目标机器粘贴执行；网关地址按 Host 头推导）
    command: String,
    expires_in_secs: u64,
}

async fn enroll_code(State(state): State<SharedState>, headers: HeaderMap) -> Json<EnrollResponse> {
    let now = SystemTime::now();
    let (code, expires_at) = state.nodes.create_enrollment(now);
    let gw = gateway_url_from_headers(&headers);
    let expires_in = expires_at
        .duration_since(now)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    tracing::info!(expires_in_secs = expires_in, "enrollment code 已生成");
    // sweep S2：code 改 env 前缀传递（AGENT_HUB_ENROLL）——argv 形态会把 code
    // 暴露给 ps(1)/同机进程面；env 前缀只出现在粘贴者自己的 shell 里（粘贴流
    // 固有暴露，与 AGENT_HUB_TOKEN 惯用法一致），server 侧同族取法兜底。
    Json(EnrollResponse {
        command: format!("AGENT_HUB_ENROLL={code} server --node --gateway {gw}"),
        code,
        expires_in_secs: expires_in,
    })
}

/// 网关地址推导：Host 头（浏览器访问 hub 的公网/本机地址）——loopback 主机给
/// ws://（本机测试），其余给 wss://（frp TLS 终结后的公网入口）。
fn gateway_url_from_headers(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1:7800")
        .to_string();
    let hostname = host.split(':').next().unwrap_or("127.0.0.1");
    if crate::instances::is_loopback_host(hostname) {
        format!("ws://{host}")
    } else {
        format!("wss://{host}")
    }
}

/// 节点视图（**无 token 哈希**——凭据材料不出响应）
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeView {
    node_id: String,
    node_name: String,
    platform: String,
    drivers: Vec<String>,
    created_at: String,
    last_seen: Option<String>,
    /// host stats presence（P3）：旧节点 None 如实缺失（N-1 窗口）
    stats: Option<crate::nodelink::HostStats>,
}

fn view_of(r: crate::nodelink::registry::NodeRecord) -> NodeView {
    NodeView {
        node_id: r.node_id,
        node_name: r.node_name,
        platform: r.platform,
        drivers: r.drivers,
        created_at: r.created_at,
        last_seen: r.last_seen,
        stats: r.stats,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NodeList {
    pending: Vec<NodeView>,
    approved: Vec<NodeView>,
}

async fn list_nodes(State(state): State<SharedState>) -> Json<NodeList> {
    let mut out = NodeList {
        pending: Vec::new(),
        approved: Vec::new(),
    };
    for r in state.nodes.list() {
        match r.status {
            NodeStatus::Pending => out.pending.push(view_of(r)),
            NodeStatus::Approved => out.approved.push(view_of(r)),
            // rejected 终态不出清单（与 pending 清单消失判据一致）
            NodeStatus::Rejected => {}
        }
    }
    Json(out)
}

async fn approve_node(
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    state.nodes.approve(&node_id).map_err(|e| match e {
        crate::nodelink::registry::ApproveError::Unknown => AppError::not_found("节点不存在"),
        crate::nodelink::registry::ApproveError::AlreadyRejected => {
            AppError::bad("已拒绝的节点不可再批准（终态）")
        }
    })?;
    // node-link 实例挂载（侧栏分组即挂载锚，N1）：token 不落实例配置
    //（node 凭据唯一事实源 = nodes.json 哈希，零明文）
    let record = state
        .nodes
        .get(&node_id)
        .ok_or_else(|| AppError::not_found("节点不存在"))?;
    let inst = InstanceConfig {
        id: record.node_id.clone(),
        name: record.node_name.clone(),
        mode: InstanceMode::NodeLink,
        url: None,
        token: None,
        ssh: None,
        remote_port: None,
        local_port: None,
        node_meta: Some(NodeMeta {
            platform: record.platform.clone(),
            drivers: record.drivers.clone(),
        }),
    };
    InstancesStore::new(&state.cfg)
        .upsert(&inst)
        .await
        .map_err(|e| AppError::bad(format!("node-link 实例挂载失败: {e}")))?;
    tracing::info!(node_id = %node_id, "节点已批准并挂载 node-link 实例");
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn reject_node(
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !state.nodes.reject(&node_id) {
        return Err(AppError::not_found("节点不存在"));
    }
    // 实例卸载（rejected 终态：节点次连收 rejected 帧停止重连，与客户端终态闭环）
    InstancesStore::new(&state.cfg)
        .delete(&node_id)
        .await
        .map_err(|e| AppError::bad(format!("node-link 实例卸载失败: {e}")))?;
    tracing::info!(node_id = %node_id, "节点已拒绝并卸载实例");
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// token 轮换（P3 M2）：生成新长效凭据 → **仅经该节点在线连接回显**
/// 轮换回执等待窗（sweep S1）：节点 persist 是本地小文件写，正常亚毫秒级
/// 回执；8s 覆盖慢盘/慢网仍远小于操作者感知阈值。超时按失败回滚（此时无
/// 凭据归属确认，「已回滚，请重试」是唯一可给前端的一致性答复）。
const ROTATE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// 轮换失败回滚的机器可读错误体（前端按 code 映射 i18n 文案；AppError 是
/// 纯文本 body 无法承载 code，故此处直接组响应）。
fn rolled_back_response() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({ "error": "rotate_rolled_back" })),
    )
        .into_response()
}

/// （proposal §4：新 token 不落响应体，mux 是唯一交付通道）→ 送达后覆盖
/// 哈希（旧 token 立即失效）。离线/送达失败拒绝轮换（防新凭据无人持有）。
/// sweep S1：签发前留底旧哈希 + 挂起回执等待——节点 persist 失败（NAK）、
/// 连接断开、回执超时三者一律回滚旧哈希，前端提示「轮换失败已回滚，请重试」
/// （杜绝「旧 token 已失效但节点未持有新 token」的锁死窗口）。
async fn rotate_token(
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;
    // 仅 approved 可轮换（pending 无凭据可换；rejected 终态不可逆）
    match state.nodes.get(&node_id).map(|r| r.status) {
        Some(NodeStatus::Approved) => {}
        Some(_) => return Err(AppError::bad("节点未处于 approved 状态，不可轮换")),
        None => return Err(AppError::not_found("节点不存在")),
    }
    let Some(tx) = state.node_conns.cmd_tx(&node_id) else {
        return Err(AppError::service_unavailable(
            "节点离线，轮换需节点在线（新 token 仅经在线连接回显）",
        ));
    };
    let new_token = uuid::Uuid::new_v4().to_string();
    // 签发前留底旧哈希（回滚快照）+ 挂起回执等待（送达前注册，防 ack 先到）。
    // 终审 BLOCKER 修：try_insert 原子占位——在途轮换存在即 409 拒绝（并发
    // 轮换串行化）。否则双击下 rotate2 覆盖 w1 使 rotate1 误入超时回滚分支
    // （回滚到节点从未持有的 token_old0），终态哈希/节点盘双向错位锁死。
    let old_hash = state.nodes.token_hash(&node_id);
    let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
    if !state
        .node_conns
        .rotates()
        .try_insert(&node_id, new_token.clone(), ack_tx)
    {
        return Err(AppError::conflict("轮换进行中，请稍候"));
    }
    // 先送达（mux 在线是唯一交付通道）再覆盖哈希——送达失败即轮换未发生
    if tx
        .send(crate::nodelink::Frame::TokenRotated {
            token: new_token.clone(),
        })
        .await
        .is_err()
    {
        state.node_conns.rotates().remove(&node_id);
        return Err(AppError::service_unavailable("节点离线（连接中断）"));
    }
    state.nodes.issue_token(&node_id, &new_token);
    match tokio::time::timeout(ROTATE_ACK_TIMEOUT, ack_rx).await {
        // ACK：persist 成功——新哈希保持（现状语义）
        Ok(Ok(true)) => {
            tracing::info!(
                node_id = %node_id,
                token = %crate::nodelink::mask_secret(&new_token),
                "token 已轮换（节点已确认落盘，旧 token 立即失效）"
            );
            Ok(Json(serde_json::json!({ "ok": true })).into_response())
        }
        // NAK（persist 失败）/ 等待方断开（连接死亡 flush）/ 超时：一律回滚
        outcome => {
            state.node_conns.rotates().remove(&node_id);
            state.nodes.restore_token_hash(&node_id, old_hash);
            tracing::warn!(
                node_id = %node_id,
                ack = ?outcome.map(|r| r.unwrap_or(false)),
                "token 轮换未获节点确认，已回滚旧哈希（旧 token 恢复有效）"
            );
            Ok(rolled_back_response())
        }
    }
}

/// 改名（P3 M5）：registry 记录名 + 实例表名同步（侧栏分组名即时生效）。
async fn rename_node(
    State(state): State<SharedState>,
    Path(node_id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::bad("节点名不能为空"));
    }
    state.nodes.rename(&node_id, name).map_err(|e| match e {
        crate::nodelink::registry::ApproveError::Unknown => AppError::not_found("节点不存在"),
        crate::nodelink::registry::ApproveError::AlreadyRejected => {
            AppError::bad("已拒绝的节点不可改名")
        }
    })?;
    // 实例表名同步（该节点已挂载时；未挂载跳过）
    let store = InstancesStore::new(&state.cfg);
    if let Some(mut inst) = store.get(&node_id).await {
        inst.name = name.to_string();
        store
            .upsert(&inst)
            .await
            .map_err(|e| AppError::bad(format!("实例表改名失败: {e}")))?;
    }
    tracing::info!(node_id = %node_id, name = %name, "节点已改名");
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
struct RenameBody {
    name: String,
}
