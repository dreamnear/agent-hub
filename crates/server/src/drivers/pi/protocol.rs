//! pi RPC 协议帧（agent-hub-pi-hub-agent P1）：JSONL over stdio，`\n` 分帧。
//! 入站三类——`type:"response"`（命令应答，带 id）、`type:"extension_ui_request"`
//! （扩展 UI 请求，dialog 类需应答）、其余为 agent 事件流；出站为命令行与 UI 应答行。
//! 协议事实（0.73.1 实测）：response 带 `command/success/data/error`；事件无 id。

use serde_json::Value;

/// 命令应答（`type:"response"`）。`success:false` 时 `error` 携带面向用户的失败文案。
#[derive(Debug, Clone)]
pub struct RpcResponse {
    pub id: Value,
    pub command: String,
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

/// 入站帧三分类 + 坏帧标记。
#[derive(Debug, Clone)]
pub enum Inbound {
    Response(RpcResponse),
    /// 扩展 UI 请求：`id` 必带；`method` = select/confirm/input/editor（dialog，需应答）
    /// 或 notify/setStatus/setWidget/setTitle/set_editor_text（fire-and-forget，忽略）。
    UiRequest {
        id: Value,
        method: String,
    },
    /// agent 事件（agent_start / message_end / tool_execution_* / …），原样上抛
    Event(Value),
    /// 非 JSON 行或缺 `type` 字段
    Malformed,
}

/// 解析一行入站帧。行尾 `\r` 容忍（协议要求客户端剥离）。
pub fn parse_line(line: &str) -> Inbound {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Inbound::Malformed;
    };
    match v.get("type").and_then(Value::as_str) {
        Some("response") => Inbound::Response(RpcResponse {
            id: v.get("id").cloned().unwrap_or(Value::Null),
            command: v
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            success: v.get("success").and_then(Value::as_bool).unwrap_or(false),
            data: v.get("data").cloned(),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
        }),
        Some("extension_ui_request") => Inbound::UiRequest {
            id: v.get("id").cloned().unwrap_or(Value::Null),
            method: v
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
        },
        Some(_) => Inbound::Event(v),
        None => Inbound::Malformed,
    }
}

/// dialog 类 UI 请求需应答（其余 fire-and-forget 忽略）。
pub fn ui_method_needs_reply(method: &str) -> bool {
    matches!(method, "select" | "confirm" | "input" | "editor")
}

/// 应答扩展 UI 请求为「取消」：hub 无终端 UI，助手不被 dialog 卡死
/// （extension 侧收到 undefined/false 自行降级）。
pub fn ui_response_cancelled(id: &Value) -> String {
    serde_json::json!({"type": "extension_ui_response", "id": id, "cancelled": true}).to_string()
}

/// prompt 命令。`id` 供受理应答关联（受理 ≠ 完成，完成走 agent_end 事件流）。
pub fn prompt_cmd(id: u64, message: &str) -> String {
    // streamingBehavior followUp（agent-hub-pi-busy-send S1）：忙态入队而非裸报
    // 错（pi 报 "Agent is already processing…"）——真机探针实证忙态 success:true
    // 入队、空闲轮处理后处理、空闲态带字段受理零回归。无条件携带避免 status
    // TOCTOU（检查时闲、RPC 时忙仍报错）；steer 不做交互入口（边界）。
    serde_json::json!({"id": id, "type": "prompt", "message": message, "streamingBehavior": "followUp"})
        .to_string()
}

/// abort 命令（中断当前处理）。
pub fn abort_cmd(id: u64) -> String {
    serde_json::json!({"id": id, "type": "abort"}).to_string()
}

/// get_state 命令（当前模型/会话文件读数）。
pub fn get_state_cmd(id: u64) -> String {
    serde_json::json!({"id": id, "type": "get_state"}).to_string()
}

/// get_messages 命令（assistant-resume A1：`--continue` 恢复会话后拉全量
/// 持久化历史回灌 feed——消息为 AgentMessage 形态，与 message_end 同构）。
pub fn get_messages_cmd(id: u64) -> String {
    serde_json::json!({"id": id, "type": "get_messages"}).to_string()
}

/// new_session 命令（assistant-resume A5：手动新建 → pi 切内存态新会话，首条
/// 消息落盘后成为下次 spawn `--continue` 的恢复目标；旧会话文件保留在 session-dir
/// 不删。边界：零消息重启锚回上一会话——pi 0.73.1 实测语义）。
pub fn new_session_cmd(id: u64) -> String {
    serde_json::json!({"id": id, "type": "new_session"}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_response_with_error() {
        let line = r#"{"id":"r1","type":"response","command":"prompt","success":false,"error":"No API key found for anthropic."}"#;
        match parse_line(line) {
            Inbound::Response(r) => {
                assert_eq!(r.command, "prompt");
                assert!(!r.success);
                assert_eq!(r.error.as_deref(), Some("No API key found for anthropic."));
                assert!(r.data.is_none());
            }
            other => panic!("应解析为 Response，实为 {other:?}"),
        }
    }

    #[test]
    fn parses_response_with_data() {
        let line = r#"{"id":3,"type":"response","command":"get_state","success":true,"data":{"model":{"id":"m"}}}"#;
        match parse_line(line) {
            Inbound::Response(r) => {
                assert!(r.success);
                assert_eq!(r.data.unwrap().get("model").unwrap()["id"], "m");
            }
            other => panic!("应解析为 Response，实为 {other:?}"),
        }
    }

    #[test]
    fn parses_ui_request_and_builds_cancel() {
        let line = r#"{"type":"extension_ui_request","id":"u1","method":"confirm","title":"t"}"#;
        match parse_line(line) {
            Inbound::UiRequest { id, method } => {
                assert_eq!(id, "u1");
                assert_eq!(method, "confirm");
                assert!(ui_method_needs_reply(&method));
                let reply = ui_response_cancelled(&id);
                let v: Value = serde_json::from_str(&reply).unwrap();
                assert_eq!(v["type"], "extension_ui_response");
                assert_eq!(v["cancelled"], true);
            }
            other => panic!("应解析为 UiRequest，实为 {other:?}"),
        }
    }

    #[test]
    fn fire_and_forget_ui_methods_need_no_reply() {
        assert!(!ui_method_needs_reply("notify"));
        assert!(!ui_method_needs_reply("setStatus"));
        assert!(!ui_method_needs_reply("setWidget"));
        assert!(!ui_method_needs_reply("setTitle"));
    }

    #[test]
    fn parses_event_and_strips_cr() {
        let line = "{\"type\":\"agent_start\"}\r";
        match parse_line(line) {
            Inbound::Event(v) => assert_eq!(v["type"], "agent_start"),
            other => panic!("应解析为 Event，实为 {other:?}"),
        }
    }

    #[test]
    fn malformed_lines_classified() {
        assert!(matches!(parse_line("not json"), Inbound::Malformed));
        assert!(matches!(parse_line(r#"{"no_type":1}"#), Inbound::Malformed));
    }

    #[test]
    fn command_builders_carry_id_and_type() {
        let p: Value = serde_json::from_str(&prompt_cmd(7, "hello")).unwrap();
        assert_eq!(p["id"], 7);
        assert_eq!(p["type"], "prompt");
        assert_eq!(p["message"], "hello");
        // pi-busy-send S1：忙态入队字段恒携带（followUp；steer 不做交互入口）
        assert_eq!(p["streamingBehavior"], "followUp");
        let a: Value = serde_json::from_str(&abort_cmd(8)).unwrap();
        assert_eq!(a["type"], "abort");
        let g: Value = serde_json::from_str(&get_state_cmd(9)).unwrap();
        assert_eq!(g["type"], "get_state");
        let gm: Value = serde_json::from_str(&get_messages_cmd(10)).unwrap();
        assert_eq!(gm["id"], 10);
        assert_eq!(gm["type"], "get_messages");
        let ns: Value = serde_json::from_str(&new_session_cmd(11)).unwrap();
        assert_eq!(ns["id"], 11);
        assert_eq!(ns["type"], "new_session");
    }
}
