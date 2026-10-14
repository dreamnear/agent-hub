//! dsh 线协议 → 现有消息/清单模型映射（需求决策 5「不新建前端组件」）。
//!
//! 事件面（`session.history` 条目与 mux `session/event` 帧同构）：
//! - `user/message`      → data 即消息（`{role,content:[blocks]}`）→ User 行
//! - `assistant/message`  → data.message.content：text → Assistant（markdown 渲染）、
//!   thinking/reasoning → Thinking 折叠卡、tool-call 形块 → ACP 工具卡线形
//! - `tool/call`         → 工具卡壳（tool/name/input 宽取）
//! - `tool/result`       → data.message content 文本 → ToolResult（ok/error 宽取）
//! - 其余（permission/*、sandbox/*、approval/*、agent/* 等 preset/控制事件）→ 不渲染
//!
//! developer preview 容错（需求决策 4）：content block 形状未知类型一律跳过，不炸；
//! history ok:false（实测 SessionPersistenceCorruptionError）由驱动层降级「不可回放」。

use crate::models::{ChatMessage, ChatMessageKind};

use super::api::{DshSession, HistoryEntry};

/// 一条已映射消息 + 去重键（事件 seq，会话内单调；page 播种 / 泵推送共用）。
#[derive(Debug)]
pub struct KeyedChat {
    pub key: String,
    pub msg: ChatMessage,
}

/// history 解析失败（corruption 等）时的占位消息文本——「列出但标不可回放」
/// 的呈现形态：会话可打开，正文一条说明卡，不炸整链。
pub const NON_REPLAY_NOTE: &str =
    "⚠ 此会话历史不可回放（dsh developer preview 存储校验未通过）。会话仍可继续发送。";

/// 认证失效占位（收编到带认证实例且未配 token）：会话可打开但读不到历史。
pub const AUTH_NOTE: &str =
    "⚠ dsh 实例认证失效：读取会话被拒（401）。请在设置页填入 dsh launch token 后重试。";

/// 非 corruption 的读取失败占位（网关/参数/解析对齐异常——与存储校验失败分开归因）。
pub const READ_FAILED_NOTE: &str =
    "⚠ 此会话历史暂时读取失败（dsh 网关响应异常，详见 hub 日志）。会话仍可继续发送。";

/// 0.2.0 迁移器拒绝旧格式日志（v1→v2 迁移失败，dsh 侧限制，非 hub bug）。
pub const LEGACY_FORMAT_NOTE: &str =
    "⚠ 此会话为 dsh 升级前的旧版格式，当前版本无法迁移继续（dsh 侧限制）。会话内容仍可从历史查看（如可读）。";

/// 会话 → 侧栏条目。group 语义与 opencode 对齐：running（agent 在跑）→ Working；
/// 其余是 dsh 存储里的历史记录 → Completed（Done 组默认折叠，发送即复活）。
/// `detail` = 宿主默认模型标注（host.describe 缓存，None 如实缺）。
pub fn session_to_summary(s: &DshSession, detail: Option<String>) -> crate::models::AgentSummary {
    crate::models::AgentSummary {
        driver: "dsh".into(),
        id: s.session_id.clone(),
        name: Some(s.title()),
        cwd: Some(s.cwd.clone()),
        kind: Some("dsh".into()),
        raw_state: Some(if s.running { "running" } else { "idle" }.into()),
        // 三档分组（展示纪律补档）：busy → Working；非 busy 但 24h 内有活动 →
        // Other（Idle 组照常显示，turn 结束 ≠ 关闭）；超窗 → Completed 历史面板
        group: if s.running {
            crate::models::Group::Working
        } else if crate::drivers::within_recent_window(s.updated_epoch()) {
            crate::models::Group::Other
        } else {
            crate::models::Group::Completed
        },
        detail,
        tokens: s.tokens(),
        context_tokens: None,
        started_at: s.updated_epoch(),
        session_id: Some(s.session_id.clone()),
    }
}

/// 一条 history 条目 → 已映射消息（控制/preset 事件 → None）。
pub fn event_to_keyed(entry: &HistoryEntry) -> Vec<KeyedChat> {
    let e = &entry.event;
    let ts = time_iso(&e.time);
    let key = e.seq.to_string();
    match e.event_type.as_str() {
        // data 即消息本体（{role, content:[blocks]}）
        "user/message" => vec![KeyedChat {
            key: key.clone(),
            msg: ChatMessage {
                kind: ChatMessageKind::User,
                text: Some(text_of(&e.data["content"])),
                ts,
                ..ChatMessage::default()
            },
        }],
        // data.message = 消息本体（content 块逐条映射——reasoning + text 共存时
        // 全部保留，只取首块会丢 assistant 正文【用户实报 2026-10-03】）
        "assistant/message" => message_blocks(&e.data["message"], ts, &key),
        "tool/call" => vec![KeyedChat {
            key: key.clone(),
            msg: tool_call_chat(&e.data, ts, &key),
        }],
        "tool/result" => vec![KeyedChat {
            key: key.clone(),
            msg: tool_result_chat(&e.data, ts, &key),
        }],
        _ => Vec::new(),
    }
}

/// assistant/message 的 content 块逐条映射（多 part 按序全保留——真实事件
/// reasoning + text 共存，只取首块会丢 assistant 正文【用户实报 2026-10-03】）：
/// text → Assistant、thinking/reasoning → Thinking、tool-call 形块 → 工具卡；
/// 未知类型跳过（宽松，preview 升级不炸）。全空 → 空序列。
fn message_blocks(message: &serde_json::Value, ts: Option<String>, key: &str) -> Vec<KeyedChat> {
    let Some(content) = message["content"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // 去重键后缀 = content 块下标（非可渲染序号）：同一 seq 不同 part 组合
    //（follow 快照重放 vs 增量帧）必须产出相同 key，push_chat 才能按 key 去重
    for (i, block) in content.iter().enumerate() {
        let btype = block["type"].as_str().unwrap_or_default();
        match btype {
            "text" => {
                let text = block["text"].as_str().unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                out.push(KeyedChat {
                    key: format!("{key}:b{i}"),
                    msg: ChatMessage {
                        kind: ChatMessageKind::Assistant,
                        text: Some(text.to_string()),
                        ts: ts.clone(),
                        ..ChatMessage::default()
                    },
                });
            }
            "thinking" | "reasoning" => {
                let text = block["text"]
                    .as_str()
                    .or_else(|| block["thinking"].as_str())
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                out.push(KeyedChat {
                    key: format!("{key}:b{i}"),
                    msg: ChatMessage {
                        kind: ChatMessageKind::Thinking,
                        text: Some(text.to_string()),
                        ts: ts.clone(),
                        ..ChatMessage::default()
                    },
                });
            }
            "tool-call" | "tool_use" | "toolCall" => {
                let card_key = format!("{key}:b{i}");
                // 0.2.0 块形 {type:'tool-call', id, name, arguments:<json 字符串>}
                let block_input = block["input"]
                    .as_object()
                    .map(|_| block["input"].clone())
                    .or_else(|| {
                        block["arguments"]
                            .as_str()
                            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                            .filter(|v| v.is_object())
                    });
                out.push(KeyedChat {
                    key: card_key.clone(),
                    msg: tool_card(
                        block["name"].as_str().unwrap_or_default(),
                        block_input.as_ref().unwrap_or(&serde_json::json!({})),
                        ts.clone(),
                        &card_key,
                    ),
                });
            }
            _ => {}
        }
    }
    out
}

/// `tool/call` 事件 → 工具卡（name/tool/toolName 宽取——块形状未经真实模型调用
/// 核对，开发期以 fake-dsh 样本锁定，终验修正）。
fn tool_call_chat(data: &serde_json::Value, ts: Option<String>, key: &str) -> ChatMessage {
    let name = data["tool"]["name"]
        .as_str()
        .or_else(|| data["name"].as_str())
        .or_else(|| data["toolName"].as_str())
        .unwrap_or("tool");
    // 0.2.0 实测（session-6e19bf1e scratch）：arguments 是 JSON **字符串**，
    // 先解包成对象再喂卡（按字符串读 path/command 全落空）
    let input = data["input"]
        .as_object()
        .map(|_| data["input"].clone())
        .or_else(|| {
            data["arguments"]
                .as_object()
                .map(|_| data["arguments"].clone())
        })
        .or_else(|| {
            data["arguments"]
                .as_str()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .filter(|v| v.is_object())
        })
        .unwrap_or(serde_json::json!({}));
    tool_card(name, &input, ts, key)
}

/// ACP 工具卡线形（前端 AcpToolCardView 零改动渲染）。
fn tool_card(name: &str, input: &serde_json::Value, ts: Option<String>, key: &str) -> ChatMessage {
    let title = input["path"]
        .as_str()
        .or_else(|| input["command"].as_str())
        .or_else(|| input["file_path"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string());
    ChatMessage {
        kind: ChatMessageKind::ToolUse,
        tool_use_id: Some(key.to_string()),
        tool_name: Some(name.to_string()),
        input: Some(serde_json::json!({
            "sessionUpdate": "tool_call",
            "toolCallId": key,
            "title": title,
            "kind": tool_kind(name),
            "content": [],
        })),
        ts,
        ..ChatMessage::default()
    }
}

/// `tool/result` 事件 → ToolResult（文本从 data.message.content 聚合；
/// ok/error 状态宽取，缺省 completed）。
fn tool_result_chat(data: &serde_json::Value, ts: Option<String>, key: &str) -> ChatMessage {
    let mut text = text_of(&data["message"]["content"]);
    if text.is_empty() {
        text = data["output"].as_str().unwrap_or_default().to_string();
    }
    let failed = data["ok"].as_bool() == Some(false)
        || data["isError"].as_bool() == Some(true)
        || data["error"].is_object();
    ChatMessage {
        kind: ChatMessageKind::ToolResult,
        tool_use_id: Some(key.to_string()),
        result: Some(serde_json::json!({
            "status": if failed { "failed" } else { "completed" },
            "content": [{"type": "content", "content": {"text": text}}],
        })),
        error: Some(failed),
        ts,
        ..ChatMessage::default()
    }
}

/// content 数组 → text 块聚合（JSON 值形态的 content 也容字符串直取）。
fn text_of(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"].as_str() == Some("text"))
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// 工具名 → ACP 卡 kind（前端图标映射，opencode mapping 同款口径）。
fn tool_kind(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("edit") || lower.contains("write") || lower.contains("patch") {
        "edit"
    } else if lower.contains("bash") || lower.contains("shell") || lower.contains("exec") {
        "execute"
    } else {
        "read"
    }
}

/// event.time 宽松归一：数字 epoch 毫秒 → RFC3339；字符串原样透传；缺省 None。
fn time_iso(time: &Option<serde_json::Value>) -> Option<String> {
    match time.as_ref() {
        Some(serde_json::Value::Number(n)) => {
            let ms = n.as_f64()?;
            // 0/负值当缺省（服务端缺时间戳不会发 0 或负 epoch）
            (ms > 0.0).then(|| humantime_rfc3339((ms / 1000.0) as i64))
        }
        Some(serde_json::Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// 手拼 RFC3339 UTC（civil-from-days，opencode/api.rs 同款口径；不引 chrono）。
fn humantime_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 146_096);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (h, mi, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(json: &str) -> HistoryEntry {
        serde_json::from_str(json).unwrap()
    }

    /// user/message：content text 聚合 → User 行；key=seq；time 毫秒归一
    #[test]
    fn maps_user_message() {
        let e = entry(
            r#"{"event":{"type":"user/message","seq":7,"time":1791012345678,
                "data":{"role":"user","content":[{"type":"text","text":"hello dsh"}]}}}"#,
        );
        let ks = event_to_keyed(&e);
        assert_eq!(ks.len(), 1);
        assert_eq!(ks[0].key, "7");
        assert_eq!(ks[0].msg.kind, ChatMessageKind::User);
        assert_eq!(ks[0].msg.text.as_deref(), Some("hello dsh"));
        assert_eq!(ks[0].msg.ts.as_deref(), Some("2026-10-03T07:25:45Z"));
    }

    /// assistant/message：text 块 → Assistant；thinking 块 → Thinking
    #[test]
    fn maps_assistant_message_variants() {
        let e = entry(concat!(
            r#"{"event":{"type":"assistant/message","seq":8,"data":{"message":{"role":"assistant","content":["#,
            r#"{"type":"thinking","text":"pondering"},{"type":"text","text":"answer body"}]}}}}"#,
        ));
        let ks = event_to_keyed(&e);
        assert_eq!(
            ks.len(),
            2,
            "reasoning + text 必须全部保留（用户实报丢正文根修）"
        );
        assert_eq!(ks[0].msg.kind, ChatMessageKind::Thinking);
        assert_eq!(ks[0].msg.text.as_deref(), Some("pondering"));
        assert_eq!(ks[1].msg.kind, ChatMessageKind::Assistant);
        assert_eq!(ks[1].msg.text.as_deref(), Some("answer body"));

        // 真实事件原文 fixture（0.2.0-rc.2 isolated 实测，无敏感内容）
        let real = entry(concat!(
            r#"{"event":{"type":"assistant/message","seq":16,"data":{"turn":1,"step":1,"message":{"role":"assistant","content":["#,
            r#"{"type":"reasoning","text":"The user said \"hi\"..."},"#,
            r#"{"type":"text","text":"Hi! I'm ready to help with the DeepSeek Harness workspace..."}]}}}}"#,
        ));
        let ks2 = event_to_keyed(&real);
        assert_eq!(ks2.len(), 2);
        assert_eq!(ks2[0].msg.kind, ChatMessageKind::Thinking);
        assert_eq!(
            ks2[1].msg.kind,
            ChatMessageKind::Assistant,
            "text part 必须映射成 assistant 正文（回归：只见 thinking 不见回复）"
        );
        assert!(
            ks2[1]
                .msg
                .text
                .as_deref()
                .unwrap()
                .contains("Hi! I'm ready to help"),
            "assistant 正文应完整读回"
        );

        let e2 = entry(
            r#"{"event":{"type":"assistant/message","seq":9,
                "data":{"message":{"role":"assistant","content":[{"type":"text","text":"final"}]}}}}"#,
        );
        let k2 = event_to_keyed(&e2);
        assert_eq!(k2.len(), 1);
        assert_eq!(k2[0].msg.kind, ChatMessageKind::Assistant);
        assert_eq!(k2[0].msg.text.as_deref(), Some("final"));
    }

    /// tool/call + tool/result：ACP 卡线形；result ok:false → failed
    #[test]
    fn maps_tool_call_and_result() {
        let call = entry(
            r#"{"event":{"type":"tool/call","seq":10,"time":1791012346000,
                "data":{"tool":{"name":"bash"},"input":{"command":"ls -la"}}}}"#,
        );
        let ks = event_to_keyed(&call);
        assert_eq!(ks.len(), 1);
        let k = &ks[0];
        assert_eq!(k.msg.kind, ChatMessageKind::ToolUse);
        let input = k.msg.input.as_ref().unwrap();
        assert_eq!(input["sessionUpdate"], "tool_call");
        assert_eq!(input["kind"], "execute");
        assert_eq!(input["title"], "ls -la");
        assert_eq!(k.msg.tool_name.as_deref(), Some("bash"));

        let res = entry(
            r#"{"event":{"type":"tool/result","seq":11,
                "data":{"ok":false,"message":{"content":[{"type":"text","text":"boom"}]}}}}"#,
        );
        let ks = event_to_keyed(&res);
        assert_eq!(ks.len(), 1);
        assert_eq!(ks[0].msg.kind, ChatMessageKind::ToolResult);
        assert_eq!(ks[0].msg.error, Some(true));
        assert_eq!(
            ks[0].msg.result.as_ref().unwrap()["status"],
            serde_json::json!("failed")
        );
    }

    /// 控制/preset 事件（permission/preset、sandbox/mode、agent/*）不渲染
    #[test]
    fn skips_control_events() {
        for t in [
            "permission/preset",
            "sandbox/mode",
            "approval/policy",
            "agent/status",
        ] {
            let raw = format!(r#"{{"event":{{"type":"{t}","seq":1,"data":{{}}}}}}"#);
            assert!(event_to_keyed(&entry(&raw)).is_empty(), "{t} 不应产生消息");
        }
    }

    /// 侧栏条目三档：running → Working；24h 内有活动 → Other；超窗 → Completed
    #[test]
    fn session_summary_maps_recent_window() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // busy → Working（不变）
        let busy: DshSession =
            serde_json::from_str(r#"{"sessionId":"session-r","running":true}"#).unwrap();
        assert_eq!(
            session_to_summary(&busy, None).group,
            crate::models::Group::Working
        );
        // updated 23h 前 → Other（Idle 组照常显示）
        let recent: DshSession = serde_json::from_str(&format!(
            r#"{{"sessionId":"session-n","updatedAt":{},"running":false}}"#,
            (now - 23 * 3600) * 1000
        ))
        .unwrap();
        assert_eq!(
            session_to_summary(&recent, None).group,
            crate::models::Group::Other
        );
        // updated 25h 前 → Completed 进历史
        let stale: DshSession = serde_json::from_str(&format!(
            r#"{{"sessionId":"session-o","updatedAt":{},"running":false}}"#,
            (now - 25 * 3600) * 1000
        ))
        .unwrap();
        assert_eq!(
            session_to_summary(&stale, None).group,
            crate::models::Group::Completed
        );
    }

    /// 侧栏条目：running → Working、其余 Completed；title/token/updatedAt 透传
    #[test]
    fn session_summary_maps_running_discipline() {
        let running: DshSession = serde_json::from_str(
            r#"{"sessionId":"session-r","updatedAt":1791012345678,"running":true,
                "cwd":"/repo","projections":{"values":{"title":"busy one","tokenUsage":{"input":3,"output":4}}}}"#,
        )
        .unwrap();
        let s = session_to_summary(&running, Some("deepseek-official/deepseek-v4-flash".into()));
        assert_eq!(s.driver, "dsh");
        assert_eq!(s.group, crate::models::Group::Working);
        assert_eq!(s.name.as_deref(), Some("busy one"));
        assert_eq!(s.tokens, Some(7));
        assert_eq!(
            s.detail.as_deref(),
            Some("deepseek-official/deepseek-v4-flash")
        );
        let idle: DshSession =
            serde_json::from_str(r#"{"sessionId":"session-i","running":false}"#).unwrap();
        let s = session_to_summary(&idle, None);
        assert_eq!(s.group, crate::models::Group::Completed);
        assert_eq!(s.raw_state.as_deref(), Some("idle"));
    }

    #[test]
    fn time_iso_handles_string_and_missing() {
        assert_eq!(
            time_iso(&Some(serde_json::json!("2026-10-02T00:00:00Z"))).as_deref(),
            Some("2026-10-02T00:00:00Z"),
            "字符串时间原样透传"
        );
        assert_eq!(time_iso(&None), None);
        assert_eq!(time_iso(&Some(serde_json::json!(0))), None, "0 毫秒当缺省");
    }
}
