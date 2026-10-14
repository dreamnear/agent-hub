//! opencode parts → 现有消息模型映射（需求决策 4「不新建前端组件」）。
//!
//! 映射表（fixture: `tests/fixtures/opencode_messages.json`，2026-10-01 实测抓取）：
//! - `text`   → ChatMessageKind::Assistant（正文，前端 markdown 渲染）
//! - `reasoning` → ChatMessageKind::Thinking（前端现有 thinking 折叠卡）
//! - `tool`   → tool_use + tool_result 一对（前端 combineToolCalls 合并成卡）。
//!   input 复用 **ACP diff 卡线形**（`sessionUpdate:'tool_call'` + `content[].type:'diff'`
//!   块）→ 前端既有 AcpToolCardView 渲染工具卡 + DiffTable，零前端改动。
//!   diff 来源 = `state.metadata.filediff.patch`（unified diff，实测字段），
//!   解析出 -/+ 行喂 oldText/newText（ponytail ceiling 见 `unified_to_old_new`）。
//! - `patch`  → 仅 files[]+hash（无 diff 文本），文件级变化已由 edit/write 工具卡
//!   呈现 → 忽略，不重复渲染。
//! - `step-start`/`step-finish` → 生命周期标记，现有 UI 无对应语义 → 忽略。
//! - `file`/`compaction`/其余未知 → 宽松忽略（ACP 同款策略）。

use crate::models::{ChatMessage, ChatMessageKind};

use super::api::{OpencodeMessage, OpencodePart, OpencodeSession, ToolPart, ToolState};

/// 一条已映射消息 + 去重键（`{messageID}:{partID}`；page 播种 / SSE 推送去重共用）。
#[derive(Debug)]
pub struct KeyedChat {
    pub key: String,
    pub msg: ChatMessage,
}

/// 一条 opencode 消息 → 已映射消息序列（key 含 messageID 前缀；全可忽略时返回空）。
pub fn message_to_keyed(msg: &OpencodeMessage) -> Vec<KeyedChat> {
    let ts = msg.info.time.as_ref().map(|t| t.created_iso());
    let mid = &msg.info.id;
    let mut out = Vec::new();
    match msg.info.role.as_str() {
        "user" => {
            // 用户消息正文聚合（多 text part 拼接；fixture 实测单 part）
            let text = msg
                .parts
                .iter()
                .filter_map(|p| p.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            out.push(KeyedChat {
                key: format!("{mid}:user"),
                msg: ChatMessage {
                    kind: ChatMessageKind::User,
                    text: Some(text),
                    ts,
                    ..ChatMessage::default()
                },
            });
        }
        "assistant" => {
            for part in &msg.parts {
                let pid = part.id();
                match part {
                    OpencodePart::Text(p) => {
                        // synthetic=true 是系统注入（memory 提示等），渲染噪音 → 跳过
                        if p.synthetic == Some(true) {
                            continue;
                        }
                        out.push(KeyedChat {
                            key: format!("{mid}:{pid}:text"),
                            msg: ChatMessage {
                                kind: ChatMessageKind::Assistant,
                                text: Some(p.text.clone()),
                                ts: ts.clone(),
                                ..ChatMessage::default()
                            },
                        });
                    }
                    OpencodePart::Reasoning(p) => out.push(KeyedChat {
                        key: format!("{mid}:{pid}:think"),
                        msg: ChatMessage {
                            kind: ChatMessageKind::Thinking,
                            text: Some(p.text.clone()),
                            ts: ts.clone(),
                            ..ChatMessage::default()
                        },
                    }),
                    OpencodePart::Tool(p) => {
                        // use/result 各自独立键（同 part 两条消息，去重互不吞）
                        out.push(KeyedChat {
                            key: format!("{mid}:{pid}:use"),
                            msg: tool_use_chat(p, ts.clone()),
                        });
                        if let Some(res) = tool_result_chat(p, ts.clone()) {
                            out.push(KeyedChat {
                                key: format!("{mid}:{pid}:result"),
                                msg: res,
                            });
                        }
                    }
                    // patch/step-*/未知：见模块头（无现有语义或已由工具卡呈现）
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// tool part → tool_use（工具卡壳 + diff 块，复用 ACP diff 卡线形）。
fn tool_use_chat(p: &ToolPart, ts: Option<String>) -> ChatMessage {
    let mut content = Vec::new();
    let title = p
        .state
        .metadata()
        .and_then(|m| m.title.clone())
        .unwrap_or_else(|| p.tool.clone());
    if let Some(fd) = p.state.metadata().and_then(|m| m.filediff.as_ref()) {
        let (old, new) = unified_to_old_new(&fd.patch);
        content.push(serde_json::json!({
            "type": "diff",
            "path": fd.file,
            "oldText": old,
            "newText": new,
        }));
    }
    ChatMessage {
        kind: ChatMessageKind::ToolUse,
        tool_use_id: Some(p.call_id.clone()),
        tool_name: Some(p.tool.clone()),
        input: Some(serde_json::json!({
            // 前端 AcpToolCardView 按 input.sessionUpdate==='tool_call' 分流（零前端改动）
            "sessionUpdate": "tool_call",
            "toolCallId": p.call_id,
            "title": title,
            "kind": tool_kind(&p.tool),
            "content": content,
        })),
        ts,
        ..ChatMessage::default()
    }
}

/// tool part 终态 → tool_result（completed/error；pending/running 不发——前端
/// result==null 即「运行中」灰点态）。
fn tool_result_chat(p: &ToolPart, ts: Option<String>) -> Option<ChatMessage> {
    match &p.state {
        ToolState::Completed { output, .. } => Some(ChatMessage {
            kind: ChatMessageKind::ToolResult,
            tool_use_id: Some(p.call_id.clone()),
            result: Some(serde_json::json!({
                "status": "completed",
                "content": [{"type": "content", "content": {"text": output}}],
            })),
            error: Some(false),
            ts,
            ..ChatMessage::default()
        }),
        ToolState::Error { error, .. } => Some(ChatMessage {
            kind: ChatMessageKind::ToolResult,
            tool_use_id: Some(p.call_id.clone()),
            result: Some(serde_json::json!({
                "status": "failed",
                "content": [{"type": "content", "content": {"text": error}}],
            })),
            error: Some(true),
            ts,
            ..ChatMessage::default()
        }),
        ToolState::Pending | ToolState::Running { .. } => None,
    }
}

/// opencode 工具名 → ACP 卡 kind（前端图标映射；未知归 read，iconForTool 兜底 think）。
fn tool_kind(name: &str) -> &'static str {
    match name {
        "edit" | "write" | "patch" | "multiedit" => "edit",
        "bash" | "task" => "execute",
        "read" | "grep" | "glob" | "list" | "webfetch" | "websearch" => "read",
        _ => "read",
    }
}

/// unified diff → (oldText, newText)：-行进 old、+行进 new（现有 ACP 卡按
/// oldText 全 del / newText 全 add 组织）。
///
/// context 行（' '）与 @@/Index/---/+++ 头一律不进渲染：两侧都记 context 会让
/// DiffTable 的 add/del 计数虚高（filediff.additions=1 的单行改动会显示 3/3），
/// 只取 -/+ 行计数才与 opencode 口径一致。
///
/// ponytail ceiling：无行号无上下文对齐，只呈现变更行。要完整 unified diff 需前端
/// 原生 diff 组件；届时直接把 `filediff.patch` 原文透传（升级路径：新卡片分支读
/// patch 字段）。
pub fn unified_to_old_new(patch: &str) -> (String, String) {
    let mut old = Vec::new();
    let mut new = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix('-') {
            if line.starts_with("---") {
                continue; // 文件头，非删除行
            }
            old.push(rest);
        } else if let Some(rest) = line.strip_prefix('+') {
            if line.starts_with("+++") {
                continue;
            }
            new.push(rest);
        }
        // context ' ' / hunk '@@' / 转义 '\\' 跳过
    }
    (old.join("\n"), new.join("\n"))
}

/// 会话 → 侧栏条目（AgentSummary）。group 语义三档（2026-10-03 展示纪律补档）：
/// busy → Working；非 busy 但 24h 内有活动（turn 结束 ≠ 关闭）→ Other（Idle 组
/// 照常显示）；超窗 → Completed 历史。opencode 非 busy 会话是 SQLite 历史记录
/// （无进程在等输入），时间事实源 = time.updated。
pub fn session_to_summary(s: &OpencodeSession, busy: bool) -> crate::models::AgentSummary {
    let state = if busy { "working" } else { "idle" };
    crate::models::AgentSummary {
        driver: "opencode".into(),
        id: s.id.clone(),
        name: Some(s.title.clone()),
        cwd: Some(s.directory.clone()),
        kind: Some("opencode".into()),
        raw_state: Some(state.into()),
        group: if busy {
            crate::models::Group::Working
        } else if crate::drivers::within_recent_window(
            s.time.as_ref().and_then(|t| t.last_activity_epoch()),
        ) {
            crate::models::Group::Other
        } else {
            crate::models::Group::Completed
        },
        detail: s
            .model
            .as_ref()
            .map(|m| format!("{}/{}", m.provider_id, m.id)),
        tokens: s.tokens.as_ref().map(|t| t.total()),
        // started_at 承载「最近活动」锚点（历史条目排序/展示口径；busy 会话同理取 updated）
        started_at: s.time.as_ref().and_then(|t| t.last_activity_epoch()),
        context_tokens: None,
        session_id: Some(s.id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> Vec<serde_json::Value> {
        let raw = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/").to_string() + name,
        )
        .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    /// 真实抓取 fixture（2026-10-01 space-bunny-free 实跑）：user/read/edit/text/
    /// patch/step 全覆盖，键与内容逐一断言
    #[test]
    fn maps_real_fixture_messages() {
        let raw_msgs = load_fixture("opencode_messages.json");
        let parsed: Vec<OpencodeMessage> = raw_msgs
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        assert_eq!(parsed.len(), 4, "fixture 4 条消息应全部可解析");

        // msg0：用户行
        let user = message_to_keyed(&parsed[0]);
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].key, "msg_TEST:user");
        assert_eq!(user[0].msg.kind, ChatMessageKind::User);
        assert!(user[0].msg.text.as_deref().unwrap().contains("edit tool"));

        // msg1：read 工具（completed，无 filediff）→ tool_use + tool_result，无 diff 块
        let m1 = message_to_keyed(&parsed[1]);
        assert_eq!(m1.len(), 2, "use + result");
        assert_eq!(m1[0].msg.tool_name.as_deref(), Some("read"));
        assert_ne!(m1[0].key, m1[1].key, "use/result 键必须独立（去重互不吞）");
        assert!(m1[1].key.ends_with(":result"));
        assert_eq!(m1[1].msg.error, Some(false));
        let input = m1[0].msg.input.as_ref().unwrap();
        assert_eq!(input["sessionUpdate"], "tool_call");
        assert_eq!(
            input["content"].as_array().unwrap().len(),
            0,
            "read 无 diff"
        );
        let out_text = m1[1].msg.result.as_ref().unwrap()["content"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(
            out_text.contains("line1"),
            "read 输出应进 result: {out_text}"
        );

        // msg2：edit 工具（filediff 有 unified diff）+ patch part（忽略）
        let m2 = message_to_keyed(&parsed[2]);
        assert_eq!(
            m2.len(),
            2,
            "patch part 不产生消息（文件变化已由 edit 卡呈现）"
        );
        assert_eq!(m2[0].msg.tool_name.as_deref(), Some("edit"));
        let content = m2[0].msg.input.as_ref().unwrap()["content"]
            .as_array()
            .unwrap();
        assert_eq!(content.len(), 1, "filediff → 一个 diff 块");
        assert_eq!(content[0]["type"], "diff");
        assert_eq!(content[0]["oldText"], "line2");
        assert_eq!(content[0]["newText"], "LINE2");
        assert!(
            content[0]["path"].as_str().unwrap().ends_with("target.txt"),
            "path 应为文件路径: {}",
            content[0]["path"]
        );

        // msg3：纯文本正文（done）
        let m3 = message_to_keyed(&parsed[3]);
        assert_eq!(m3.len(), 1);
        assert_eq!(m3[0].msg.kind, ChatMessageKind::Assistant);
        assert_eq!(m3[0].msg.text.as_deref(), Some("done"));
    }

    /// reasoning fixture → Thinking
    #[test]
    fn maps_reasoning_fixture() {
        let raw_msgs = load_fixture("opencode_reasoning.json");
        let parsed: Vec<OpencodeMessage> = raw_msgs
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect();
        let out = message_to_keyed(&parsed[0]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg.kind, ChatMessageKind::Thinking);
        assert!(out[0]
            .msg
            .text
            .as_deref()
            .unwrap()
            .contains("Thinking about"));
    }

    /// synthetic text part（系统注入）→ 跳过，不渲染
    #[test]
    fn synthetic_text_part_skipped() {
        let m: OpencodeMessage = serde_json::from_str(
            r#"{"info":{"id":"m1","role":"assistant"},
                "parts":[{"id":"p1","type":"text","text":"sys inject","synthetic":true}]}"#,
        )
        .unwrap();
        assert!(message_to_keyed(&m).is_empty());
    }

    #[test]
    fn unified_to_old_new_handles_headers_context_and_hunks() {
        let patch = "Index: /repo/f.txt\n===\n--- /repo/f.txt\n+++ /repo/f.txt\n@@ -1,3 +1,3 @@\n keep\n-old line\n+new line\n";
        let (old, new) = unified_to_old_new(patch);
        assert_eq!(old, "old line", "context 行不进渲染（计数口径）");
        assert_eq!(new, "new line");
        assert_eq!(unified_to_old_new(""), (String::new(), String::new()));
    }

    #[test]
    fn session_summary_maps_recent_window() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mk = |updated_ms: f64| OpencodeSession {
            id: "ses_w".into(),
            title: String::new(),
            directory: String::new(),
            model: None,
            variant: None,
            tokens: None,
            time: Some(super::super::api::OpencodeTime {
                created: updated_ms,
                updated: Some(updated_ms),
            }),
        };
        // busy → Working
        assert_eq!(
            session_to_summary(&mk(now as f64 * 1000.0), true).group,
            crate::models::Group::Working
        );
        // 23h → Other
        assert_eq!(
            session_to_summary(&mk((now - 23 * 3600) as f64 * 1000.0), false).group,
            crate::models::Group::Other
        );
        // 25h → Completed
        assert_eq!(
            session_to_summary(&mk((now - 25 * 3600) as f64 * 1000.0), false).group,
            crate::models::Group::Completed
        );
    }

    #[test]
    fn session_summary_maps_busy_and_idle() {
        let s = OpencodeSession {
            id: "ses_x".into(),
            title: "T".into(),
            directory: "/repo".into(),
            model: Some(super::super::api::OpencodeModelRef {
                provider_id: "opencode".into(),
                id: "space-bunny-free".into(),
                variant: None,
            }),
            tokens: Some(super::super::api::OpencodeTokens {
                input: 10.0,
                output: 5.0,
                reasoning: Some(1.0),
                cache: None,
            }),
            variant: None,
            time: Some(super::super::api::OpencodeTime {
                created: 1790831543346.0,
                updated: Some(1790831544999.0),
            }),
        };
        let busy = session_to_summary(&s, true);
        assert_eq!(busy.driver, "opencode");
        assert_eq!(busy.group, crate::models::Group::Working);
        assert_eq!(busy.raw_state.as_deref(), Some("working"));
        assert_eq!(busy.tokens, Some(16)); // input+output+reasoning
                                           // 非 busy = SQLite 历史（无进程待命）→ Completed（Done 组默认折叠，
                                           // 点开可读、发送即复活）；raw_state 保留 idle 原文（事实口径）
        let idle = session_to_summary(&s, false);
        assert_eq!(idle.group, crate::models::Group::Completed);
        assert_eq!(idle.raw_state.as_deref(), Some("idle"));
        assert_eq!(idle.cwd.as_deref(), Some("/repo"), "cwd 供前端族锚过滤");
        assert_eq!(idle.detail.as_deref(), Some("opencode/space-bunny-free"));
        // started_at = 最近活动（updated=…544999ms 优先于 created）
        assert_eq!(idle.started_at, Some(1790831544));
    }

    /// error 态工具 → failed result + error 标记（前端红点）
    #[test]
    fn error_tool_maps_failed_result() {
        let p: ToolPart = serde_json::from_str(
            r#"{"id":"prt_e","callID":"call_e","tool":"question",
                "state":{"status":"error","input":{},"error":"boom","time":{"start":1,"end":2}}}"#,
        )
        .unwrap();
        let m = message_to_keyed(&OpencodeMessage {
            info: super::super::api::OpencodeMessageInfo {
                id: "m".into(),
                role: "assistant".into(),
                time: None,
            },
            parts: vec![OpencodePart::Tool(p)],
        });
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].msg.error, Some(true));
        assert_eq!(
            m[1].msg.result.as_ref().unwrap()["status"],
            serde_json::json!("failed")
        );
    }
}
