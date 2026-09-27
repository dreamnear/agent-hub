//! session/update 分发（批1 任务4）：update → ChatMessage 映射 + 会话级 inbound 泵。
//! 推送通道复用 chat_rooms 的 ChatEvent 房间（批2 前端零新增订阅机制）。

use std::{sync::Arc, time::Duration};

use tokio::sync::broadcast;

use super::{
    protocol,
    registry::{AcpStatus, PermissionAnswer},
    AcpConnection, AcpRegistry, Inbound, PERMISSION_TIMEOUT,
};
use crate::models::{ChatMessage, ChatMessageKind};

/// session/update params → ChatMessage（批2 起前端按 kind 渲染）。
/// 已知五类映射；未知类型（含 omp 实测的 available_commands_update /
/// session_info_update / usage_update）宽松忽略返 None。
/// raw_type 携带 acp_* 标记（批2 任务8/9/10）：前端 ACP 专属渲染分流依据，
/// 与 claude jsonl 消息（rawType 为 jsonl type）天然不撞。
pub fn chat_message_from_update(params: &serde_json::Value) -> Option<ChatMessage> {
    let update = &params["update"];
    let kind = update["sessionUpdate"].as_str()?;
    let text = || update["content"]["text"].as_str().map(str::to_string);
    let base = ChatMessage::default();
    Some(match kind {
        "agent_message_chunk" => ChatMessage {
            kind: ChatMessageKind::Assistant,
            raw_type: Some("acp_chunk".into()),
            text: text(),
            ..base
        },
        "agent_thought_chunk" => ChatMessage {
            kind: ChatMessageKind::Thinking,
            raw_type: Some("acp_chunk".into()),
            text: text(),
            ..base
        },
        "tool_call" => ChatMessage {
            kind: ChatMessageKind::ToolUse,
            raw_type: Some("acp_tool_call".into()),
            tool_use_id: update["toolCallId"].as_str().map(str::to_string),
            tool_name: update["title"]
                .as_str()
                .or_else(|| update["kind"].as_str())
                .map(str::to_string),
            input: Some(update.clone()),
            ..base
        },
        "tool_call_update" => ChatMessage {
            kind: ChatMessageKind::ToolResult,
            raw_type: Some("acp_tool_update".into()),
            tool_use_id: update["toolCallId"].as_str().map(str::to_string),
            result: Some(update.clone()),
            ..base
        },
        // plan 原样进 result，批2 任务10 映射 TaskRows
        "plan" => ChatMessage {
            kind: ChatMessageKind::Other,
            raw_type: Some("acp_plan".into()),
            result: Some(update.clone()),
            ..base
        },
        _ => return None,
    })
}

/// 权限应答 → ACP outcome（批3 任务11；红线：任何路径不默认放行）。
/// 用户显式选项且 optionId ∈ params["options"] → selected；伪造/取消/超时/通道失效
/// → 有拒绝项则替选拒绝，否则 cancelled（ocr-review 高：REST 提交的任意字符串
/// 不透传 agent，防绕过拒绝兜底的放行旁路）。
pub fn permission_outcome(
    params: &serde_json::Value,
    answer: &PermissionAnswer,
) -> serde_json::Value {
    let listed = params["options"].as_array().is_some_and(|arr| {
        arr.iter().any(|o| {
            o["optionId"]
                .as_str()
                .is_some_and(|oid| Some(oid) == answer.option_id.as_deref())
        })
    });
    match &answer.option_id {
        Some(oid) if listed => serde_json::json!({
            "outcome": {"outcome": "selected", "optionId": oid}
        }),
        _ => match protocol::reject_option_id(params) {
            Some(oid) => serde_json::json!({
                "outcome": {"outcome": "selected", "optionId": oid}
            }),
            None => serde_json::json!({"outcome": {"outcome": "cancelled"}}),
        },
    }
}

/// 反向请求处理（批3 任务11 弹卡链路）：权限请求推卡进 feed + 限时等用户应答
/// （REST /permission 端点回流）；超时按拒绝收尾（红线：不默认放行）。
/// 其余方法协议化 method-not-found。
async fn handle_reverse_request(
    conn: &AcpConnection,
    session_id: &str,
    id: serde_json::Value,
    method: String,
    params: serde_json::Value,
    registry: &AcpRegistry,
    feed: &super::registry::AcpFeed,
) {
    if method == "session/request_permission" {
        handle_permission_request(
            conn,
            session_id,
            &id,
            &params,
            registry,
            feed,
            PERMISSION_TIMEOUT,
        )
        .await;
    } else {
        tracing::debug!(session_id = %session_id, method, "不支持的反向请求，回 method not found");
        let _ = conn
            .respond_error(&id, -32601, &format!("hub 不支持反向请求: {method}"))
            .await;
    }
}

/// 权限请求全生命周期（批3 任务11 + r77 修复）：
/// - 卡键会话内唯一（registry 自增），agent 重用反向请求 id 不再撞卡；
/// - 回执（acp_permission_resolved）由本函数统一推送——应答/取消/超时全路径
///   都出回执，前端弹卡同步关闭且刷新/重连后不重弹已收场的卡；
/// - 醒来仅在连接存活时翻回 Working（会话已 Dead 不复活状态）。
///   `wait` 参数化仅为测试超时路径（生产走 PERMISSION_TIMEOUT）。
async fn handle_permission_request(
    conn: &AcpConnection,
    session_id: &str,
    rpc_id: &serde_json::Value,
    params: &serde_json::Value,
    registry: &AcpRegistry,
    feed: &super::registry::AcpFeed,
    wait: Duration,
) {
    let perm_key = format!("perm:{session_id}:{}", registry.next_perm_id());
    let rx = registry.register_permission(perm_key.clone());
    feed.push(ChatMessage {
        kind: ChatMessageKind::Other,
        raw_type: Some("acp_permission".into()),
        tool_use_id: Some(perm_key.clone()),
        tool_name: params["toolCall"]["title"].as_str().map(str::to_string),
        input: Some(params.clone()),
        ..ChatMessage::default()
    });
    // 批4 三态对齐：权限弹卡挂起 → awaiting_input（agent 被阻塞等应答）。
    registry.set_status(session_id, AcpStatus::AwaitingInput);
    tracing::info!(session_id = %session_id, perm_key = %perm_key, "权限请求已推卡，等待用户应答");
    let (answer, timed_out) = match tokio::time::timeout(wait, rx).await {
        Ok(Ok(a)) => (a, false),
        // 超时 / 通道失效（会话已摘除）= 取消语义，收尾仍走拒绝兜底
        _ => (PermissionAnswer { option_id: None }, true),
    };
    let receipt_text = if timed_out {
        "等待超时，已自动拒绝（不默认放行）".to_string()
    } else {
        match &answer.option_id {
            Some(oid) => format!("已应答: {oid}"),
            None => "已取消".into(),
        }
    };
    let outcome = permission_outcome(params, &answer);
    // 应答已回流 → 翻回 Working（prompt 继续在途）；send 收尾统一回 Idle。
    // 连接已死不翻转（会把 Dead 打回 Working，r77）；prompt future 已死不翻转
    // （无人收尾，状态永久滞留 working，O1）。
    if !conn.is_dead() && registry.is_prompt_alive(session_id) {
        registry.set_status(session_id, AcpStatus::Working);
    }
    feed.push(permission_receipt(&perm_key, &receipt_text));
    tracing::info!(session_id = %session_id, perm_key = %perm_key, outcome = %outcome, receipt = %receipt_text, "权限请求收场，应答回传 agent");
    let _ = conn.respond(rpc_id, &outcome).await;
}

/// 权限回执消息（r77：全路径统一由此构建，前端 detectPendingPermission 据此消卡）。
pub fn permission_receipt(perm_key: &str, text: &str) -> ChatMessage {
    ChatMessage {
        kind: ChatMessageKind::Other,
        raw_type: Some("acp_permission_resolved".into()),
        tool_use_id: Some(perm_key.to_string()),
        text: Some(text.to_string()),
        ..ChatMessage::default()
    }
}

/// 会话级 inbound 泵：连接事件 → feed（history + 房间推送）/ 反向请求占位应答 /
/// 死亡标记。会话创建时启动，收 Closed 即退出（注册表状态翻转 Dead + Tick 刷侧栏）。
pub fn spawn_inbound_pump(
    session_id: String,
    conn: AcpConnection,
    mut rx: broadcast::Receiver<Inbound>,
    registry: Arc<AcpRegistry>,
    feed: Arc<super::registry::AcpFeed>,
    events_tx: tokio::sync::broadcast::Sender<crate::drivers::claude::watcher::HubEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(Inbound::Notification { method, params }) => {
                    if method != "session/update" {
                        tracing::debug!(target: "acp", session_id = %session_id, method, "忽略非 update 通知");
                        continue;
                    }
                    let Some(message) = chat_message_from_update(&params) else {
                        tracing::debug!(target: "acp", session_id = %session_id, "宽松忽略未知 update 类型");
                        continue;
                    };
                    feed.push(message);
                }
                Ok(Inbound::ReverseRequest { id, method, params }) => {
                    // ocr-review 高：权限请求内部最长等 PERMISSION_TIMEOUT，
                    // 同步 await 会挂住整个泵——session/update 堆积触发 Lagged 丢帧、
                    // Closed 延迟。每个反向请求 spawn 独立任务，泵立即回到 recv。
                    let conn = conn.clone();
                    let session_id = session_id.clone();
                    let registry = Arc::clone(&registry);
                    let feed = Arc::clone(&feed);
                    tokio::spawn(async move {
                        handle_reverse_request(
                            &conn,
                            &session_id,
                            id,
                            method,
                            params,
                            &registry,
                            &feed,
                        )
                        .await;
                    });
                }
                Ok(Inbound::Closed) => {
                    registry.set_status(&session_id, AcpStatus::Dead);
                    let _ = events_tx.send(crate::drivers::claude::watcher::HubEvent::Tick);
                    tracing::warn!(session_id = %session_id, "ACP 子进程退出，会话标记 dead");
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(dropped = n, session_id = %session_id, "ACP inbound lagged，继续");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn maps_five_known_kinds_like_omp() {
        // agent_message_chunk（omp 实测形态，含宽松 messageId）
        let chunk = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ok"}, "messageId": "m-1"}
        });
        let msg = chat_message_from_update(&chunk).unwrap();
        assert_eq!(msg.kind, ChatMessageKind::Assistant);
        assert_eq!(msg.text.as_deref(), Some("ok"));

        // agent_thought_chunk
        let thought = json!({
            "update": {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}}
        });
        let msg = chat_message_from_update(&thought).unwrap();
        assert_eq!(msg.kind, ChatMessageKind::Thinking);
        assert_eq!(msg.text.as_deref(), Some("hmm"));

        // tool_call
        let tool = json!({
            "update": {"sessionUpdate": "tool_call", "toolCallId": "tc1", "title": "Edit file", "kind": "edit",
                        "content": [{"type": "diff", "path": "/a", "oldText": "x", "newText": "y"}]}
        });
        let msg = chat_message_from_update(&tool).unwrap();
        assert_eq!(msg.kind, ChatMessageKind::ToolUse);
        assert_eq!(msg.tool_use_id.as_deref(), Some("tc1"));
        assert_eq!(msg.tool_name.as_deref(), Some("Edit file"));
        assert_eq!(msg.input.as_ref().unwrap()["kind"], "edit");

        // tool_call_update
        let tool_upd = json!({
            "update": {"sessionUpdate": "tool_call_update", "toolCallId": "tc1", "status": "completed"}
        });
        let msg = chat_message_from_update(&tool_upd).unwrap();
        assert_eq!(msg.kind, ChatMessageKind::ToolResult);
        assert_eq!(msg.tool_use_id.as_deref(), Some("tc1"));
        assert_eq!(msg.result.as_ref().unwrap()["status"], "completed");

        // plan → Other（批2 TaskRows）
        let plan = json!({
            "update": {"sessionUpdate": "plan", "entries": [{"content": "step1", "priority": "medium", "status": "pending"}]}
        });
        let msg = chat_message_from_update(&plan).unwrap();
        assert_eq!(msg.kind, ChatMessageKind::Other);
        assert_eq!(
            msg.result.as_ref().unwrap()["entries"][0]["content"],
            "step1"
        );
    }

    #[test]
    fn unknown_and_non_update_kinds_are_ignored() {
        // omp 实测会推的三类非内容 update：宽松忽略
        for kind in [
            "available_commands_update",
            "session_info_update",
            "usage_update",
            "unknown_xyz_update",
        ] {
            let v = json!({"update": {"sessionUpdate": kind}});
            assert!(chat_message_from_update(&v).is_none(), "{kind} 应忽略");
        }
        // 无 update 字段
        assert!(chat_message_from_update(&json!({})).is_none());
    }

    /// 批3 任务11：权限 outcome 映射——显式选项透传；超时/取消不放行（红线）
    #[test]
    fn permission_outcome_never_defaults_to_allow() {
        let params = json!({"options": [
            {"optionId": "opt-allow", "kind": "allow_once"},
            {"optionId": "opt-reject", "kind": "reject_once"}
        ]});
        // 显式 allow 透传
        let outcome = permission_outcome(
            &params,
            &PermissionAnswer {
                option_id: Some("opt-allow".into()),
            },
        );
        assert_eq!(outcome["outcome"]["optionId"], "opt-allow");
        // 取消（用户点取消 / 超时）：有拒绝项 → 替选拒绝，绝不落到 allow 项
        let outcome = permission_outcome(&params, &PermissionAnswer { option_id: None });
        assert_eq!(outcome["outcome"]["outcome"], "selected");
        assert_eq!(outcome["outcome"]["optionId"], "opt-reject");
        // 无拒绝项 → cancelled（同样不放行）
        let only_allow = json!({"options": [{"optionId": "opt-allow", "kind": "allow_once"}]});
        let outcome = permission_outcome(&only_allow, &PermissionAnswer { option_id: None });
        assert_eq!(outcome["outcome"]["outcome"], "cancelled");
    }

    /// ocr-review 高：显式 optionId 必须属于 params["options"]——REST 层伪造的
    /// 任意字符串不得透传 agent（伪造 "opt-allow" 绕过拒绝兜底 = 默认放行旁路）。
    /// 非法值回落 reject_option_id / cancelled，与取消语义同路径。
    #[test]
    fn forged_option_id_falls_back_to_reject() {
        let params = json!({"options": [
            {"optionId": "opt-allow", "kind": "allow_once"},
            {"optionId": "opt-reject", "kind": "reject_once"}
        ]});
        // 对照：合法值正常透传
        let legit = permission_outcome(
            &params,
            &PermissionAnswer {
                option_id: Some("opt-reject".into()),
            },
        );
        // 伪造场景用非法值重跑
        let forged = permission_outcome(
            &params,
            &PermissionAnswer {
                option_id: Some("forged-allow-everything".into()),
            },
        );
        assert_eq!(legit["outcome"]["optionId"], "opt-reject", "合法值透传");
        assert_eq!(
            forged["outcome"]["optionId"], "opt-reject",
            "伪造 optionId 必须替选拒绝项，不得透传"
        );
        // 无拒绝项时伪造 → cancelled
        let only_allow = json!({"options": [{"optionId": "opt-allow", "kind": "allow_once"}]});
        let forged2 = permission_outcome(
            &only_allow,
            &PermissionAnswer {
                option_id: Some("forged".into()),
            },
        );
        assert_eq!(forged2["outcome"]["outcome"], "cancelled");
        // options 缺失/非数组：一律不透传（cancelled）
        let no_opts = json!({});
        let forged3 = permission_outcome(
            &no_opts,
            &PermissionAnswer {
                option_id: Some("x".into()),
            },
        );
        assert_eq!(forged3["outcome"]["outcome"], "cancelled");
    }

    /// r77：超时路径出回执（acp_permission_resolved）——前端弹卡同步消失、
    /// 刷新不重弹已收场的卡；agent 收到拒绝兜底（不默认放行）。
    #[tokio::test]
    async fn permission_timeout_pushes_receipt_and_rejects() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("fake-events.log");
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_fake_agent.py");
        let agent = crate::config::AcpAgentConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![
                "-u".into(),
                fixture.to_string_lossy().into_owned(),
                "permission".into(),
                log_path.to_string_lossy().into_owned(),
            ],
            cwd: None,
            model: None,
        };
        let (conn, mut inbound_rx) = super::super::AcpConnection::spawn(&agent, dir.path())
            .await
            .unwrap();
        let registry = AcpRegistry::new();
        let feed = crate::drivers::acp::registry::AcpFeed::new(
            "fake-session-1".into(),
            Arc::new(std::sync::RwLock::new(HashMap::new())),
        );
        // 完整流：prompt 让 agent 主动发起权限请求（阻塞等应答），随后短等待直调
        // 超时分支收尾（真分支代码，生产仅把 wait 换 PERMISSION_TIMEOUT）
        let prompt_conn = conn.clone();
        let prompt_task = tokio::spawn(async move {
            prompt_conn
                .request(
                    "session/prompt",
                    json!({"sessionId": "fake-session-1", "prompt": [{"type": "text", "text": "go"}]}),
                    Duration::from_secs(10),
                )
                .await
        });
        let (rpc_id, params) = loop {
            match tokio::time::timeout(Duration::from_secs(5), inbound_rx.recv()).await {
                Ok(Ok(Inbound::ReverseRequest { id, method, params }))
                    if method == "session/request_permission" =>
                {
                    break (id, params);
                }
                Ok(Ok(_)) => continue,
                _ => panic!("未收到权限反向请求"),
            }
        };
        // 无人应答 → 短等待走超时分支
        handle_permission_request(
            &conn,
            "fake-session-1",
            &rpc_id,
            &params,
            &registry,
            &feed,
            Duration::from_millis(120),
        )
        .await;

        // 回执入 feed：超时文案 + 同键 resolved
        let msgs = feed.history();
        let card = msgs
            .iter()
            .find(|m| m.raw_type.as_deref() == Some("acp_permission"))
            .expect("权限卡应入 feed");
        let key = card.tool_use_id.clone().unwrap();
        let receipt = msgs
            .iter()
            .find(|m| {
                m.raw_type.as_deref() == Some("acp_permission_resolved")
                    && m.tool_use_id.as_deref() == Some(key.as_str())
            })
            .expect("超时后应有 resolved 回执（r77）");
        assert!(
            receipt.text.as_deref().unwrap_or_default().contains("超时"),
            "回执应标明超时自动拒绝: {:?}",
            receipt.text
        );
        // agent 侧收到拒绝兜底（reject_once 替选，不默认放行）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let raw = loop {
            if let Ok(raw) = std::fs::read_to_string(&log_path) {
                if raw.contains("permission_answer") {
                    break raw;
                }
            }
            assert!(tokio::time::Instant::now() < deadline, "应答未落日志");
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let entry: serde_json::Value = serde_json::from_str(
            raw.lines()
                .find(|l| l.contains("permission_answer"))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            entry["permission_answer"]["result"]["outcome"]["optionId"], "opt-reject",
            "超时应替选拒绝项"
        );
        let _ = prompt_task.await;
        conn.shutdown().await;
    }

    /// r77：卡键唯一性——同 registry 连续两次注册得到不同键（agent 反向请求 id
    /// 重用不再撞键）
    #[test]
    fn perm_keys_unique_per_request() {
        let registry = AcpRegistry::new();
        let k1 = format!("perm:s1:{}", registry.next_perm_id());
        let k2 = format!("perm:s1:{}", registry.next_perm_id());
        assert_ne!(k1, k2);
        assert!(k1.starts_with("perm:s1:"));
    }

    /// r77：resolve_session_perms 唤醒本会话全部挂起、不动其他会话
    #[tokio::test]
    async fn resolve_session_perms_wakes_only_target_session() {
        let registry = AcpRegistry::new();
        let rx_a = registry.register_permission("perm:s1:1".into());
        let rx_b = registry.register_permission("perm:s1:2".into());
        let rx_c = registry.register_permission("perm:s2:1".into());
        assert_eq!(registry.resolve_session_perms("s1"), 2);
        // s1 两个挂起被唤醒（取消语义）
        assert!(rx_a.await.is_ok());
        assert!(rx_b.await.is_ok());
        // s2 不受影响，仍可正常回流
        assert!(registry.resolve_permission(
            "perm:s2:1",
            PermissionAnswer {
                option_id: Some("opt-allow".into()),
            },
        ));
        assert!(rx_c.await.is_ok());
    }

    /// ocr-review 高：权限请求挂起期间泵不得被阻塞——agent 退出（Closed）必须
    /// 即时被观测并把会话置 Dead。修复前泵在 ReverseRequest 分支同步 await
    /// handle_permission_request（PERMISSION_TIMEOUT 量级），Closed 迟到。
    #[tokio::test]
    async fn pump_observes_closed_while_permission_pending() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/acp_fake_agent.py");
        let agent = crate::config::AcpAgentConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![
                "-u".into(),
                fixture.to_string_lossy().into_owned(),
                "permission".into(),
            ],
            cwd: None,
            model: None,
        };
        let (conn, inbound_rx) = super::super::AcpConnection::spawn(&agent, dir.path())
            .await
            .unwrap();
        let registry = Arc::new(AcpRegistry::new());
        let feed = crate::drivers::acp::registry::AcpFeed::new(
            "fake-session-1".into(),
            Arc::new(std::sync::RwLock::new(HashMap::new())),
        );
        registry.insert(crate::drivers::acp::registry::AcpSession {
            id: "fake-session-1".into(),
            agent: "fake".into(),
            cwd: dir.path().to_string_lossy().into_owned(),
            model: None,
            status: AcpStatus::Idle,
            conn: conn.clone(),
            feed: Arc::clone(&feed),
        });
        let (events_tx, _events_rx) = tokio::sync::broadcast::channel(8);
        let pump = super::spawn_inbound_pump(
            "fake-session-1".into(),
            conn.clone(),
            inbound_rx,
            Arc::clone(&registry),
            Arc::clone(&feed),
            events_tx,
        );
        // prompt 让 agent 发起权限请求并阻塞等应答（不答）
        let prompt_conn = conn.clone();
        let prompt_task = tokio::spawn(async move {
            let _ = prompt_conn
                .request(
                    "session/prompt",
                    json!({"sessionId": "fake-session-1", "prompt": [{"type": "text", "text": "go"}]}),
                    Duration::from_secs(10),
                )
                .await;
        });
        // 等权限卡进 feed（挂起期确立）
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let pending = feed
                .history()
                .iter()
                .any(|m| m.raw_type.as_deref() == Some("acp_permission"));
            if pending {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "权限请求未到达");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // agent 侧 EOF → 退出 → 泵应收到 Closed 并置 Dead（修复前：泵被 600s await 挂住）
        conn.shutdown().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if registry
                .get("fake-session-1")
                .is_none_or(|s| s.status == AcpStatus::Dead)
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "权限挂起期间 Closed 未被泵观测（泵被反向请求阻塞）"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        pump.abort();
        let _ = prompt_task.await;
    }
}
