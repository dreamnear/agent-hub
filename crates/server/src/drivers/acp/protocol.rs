//! ACP 协议类型（JSON-RPC over stdio，批1 任务2/3）。
//! 全线宽松解析：**禁用 deny_unknown_fields**——ACP 仍在活跃演进，未知字段一律忽略；
//! 能力块等易变结构保留原始 Value（需求约束 1：锁定 v1 实现 + schema 宽松）。

use serde::{Deserialize, Serialize};

/// hub 实现锁定的 ACP 大版本（omp 18.x 实测协商结果 = 1）。
pub const PROTOCOL_VERSION: i64 = 1;

/// 出站请求（客户端 → agent）。id 由连接层分配，jsonrpc 固定 "2.0"。
#[derive(Debug, Serialize)]
pub struct RpcRequest<'a> {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: &'a str,
    pub params: serde_json::Value,
}

/// 出站通知（客户端 → agent，无 id；如 session/cancel）。
#[derive(Debug, Serialize)]
pub struct RpcNotification<'a> {
    pub jsonrpc: &'static str,
    pub method: &'a str,
    pub params: serde_json::Value,
}

/// 出站应答（客户端 → agent，回应反向请求）。
#[derive(Debug, Serialize)]
pub struct RpcResult<'a> {
    pub jsonrpc: &'static str,
    pub id: &'a serde_json::Value,
    pub result: &'a serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

/// 入站响应（agent 对本端请求的应答）。id 可能是 number/string，保留 Value 关联。
#[derive(Debug, Deserialize)]
pub struct RpcResponse {
    pub id: serde_json::Value,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// 入站信封（通知或反向请求共用）：有 method；有 id = agent→client 请求（须应答）。
#[derive(Debug, Deserialize)]
pub struct RpcIncoming {
    pub method: String,
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// 一行 stdio 的判别解析：响应（有 id 无 method）/ 通知或反向请求（有 method）。
pub fn parse_line(line: &str) -> Result<ParsedMessage, serde_json::Error> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(default)]
        method: Option<String>,
        #[serde(default)]
        id: Option<serde_json::Value>,
        #[serde(default)]
        result: Option<serde_json::Value>,
        #[serde(default)]
        error: Option<RpcError>,
        #[serde(default)]
        params: serde_json::Value,
    }
    let env: Envelope = serde_json::from_str(line)?;
    match (env.method, env.id) {
        (Some(method), Some(id)) => Ok(ParsedMessage::ReverseRequest(RpcIncoming {
            method,
            id: Some(id),
            params: env.params,
        })),
        (Some(method), None) => Ok(ParsedMessage::Notification(RpcIncoming {
            method,
            id: None,
            params: env.params,
        })),
        (None, Some(id)) => Ok(ParsedMessage::Response(RpcResponse {
            id,
            result: env.result,
            error: env.error,
        })),
        (None, None) => Ok(ParsedMessage::Malformed),
    }
}

/// 一行 stdio 解析结果（判别三类 + 坏帧）。
#[derive(Debug)]
pub enum ParsedMessage {
    Response(RpcResponse),
    Notification(RpcIncoming),
    ReverseRequest(RpcIncoming),
    /// 非法帧（缺 method/id）：宽松跳过不致命
    Malformed,
}

/// initialize 应答（capabilities 保留 Value；未知字段忽略）。
#[derive(Debug, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion", default)]
    pub protocol_version: Option<i64>,
    #[serde(rename = "agentCapabilities", default)]
    pub agent_capabilities: serde_json::Value,
    #[serde(rename = "agentInfo", default)]
    pub agent_info: serde_json::Value,
}

/// session/new 应答。
#[derive(Debug, Deserialize)]
pub struct SessionNewResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "configOptions", default)]
    pub config_options: serde_json::Value,
}

/// session/prompt 应答。
#[derive(Debug, Deserialize)]
pub struct PromptResult {
    #[serde(rename = "stopReason", default)]
    pub stop_reason: String,
    #[serde(default)]
    pub usage: serde_json::Value,
}

/// session/update 通知的 update 块。kind 判别 + 原样携带（flatten 未知字段）。
#[derive(Debug, Default, Deserialize)]
pub struct SessionUpdate {
    #[serde(rename = "sessionUpdate", default)]
    pub kind: String,
    /// agent_message_chunk / agent_thought_chunk 的内容块
    #[serde(default)]
    pub content: Option<ContentBlock>,
    #[serde(flatten)]
    pub rest: serde_json::Value,
}

/// session/update 通知参数。
#[derive(Debug, Deserialize)]
pub struct SessionUpdateParams {
    #[serde(rename = "sessionId", default)]
    pub session_id: String,
    #[serde(default)]
    pub update: SessionUpdate,
}

/// ACP 内容块（chunk 场景只消费 text；image/audio 等宽松忽略）。
#[derive(Debug, Default, Deserialize)]
pub struct ContentBlock {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// session/request_permission 的选项 kind（v1 稳定枚举）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

impl PermissionOptionKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "allow_once" => Self::AllowOnce,
            "allow_always" => Self::AllowAlways,
            "reject_once" => Self::RejectOnce,
            "reject_always" => Self::RejectAlways,
            _ => return None,
        })
    }
}

/// 从权限请求 params 里挑拒绝项 optionId（安全红线：不默认放行）。
/// 无拒绝项 → None（上层应答 outcome=cancelled，同样不放行）。
pub fn reject_option_id(params: &serde_json::Value) -> Option<String> {
    let options = params.get("options")?.as_array()?;
    options
        .iter()
        .find(|o| {
            o.get("kind")
                .and_then(|k| k.as_str())
                .and_then(PermissionOptionKind::parse)
                .is_some_and(|k| {
                    matches!(
                        k,
                        PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways
                    )
                })
        })
        .and_then(|o| {
            o.get("optionId")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_response_with_unknown_fields() {
        let line =
            r#"{"jsonrpc":"2.0","id":3,"result":{"protocolVersion":1,"extraField":{"x":1}}}"#;
        match parse_line(line).unwrap() {
            ParsedMessage::Response(r) => {
                assert_eq!(r.id, json!(3));
                assert_eq!(r.result.unwrap()["protocolVersion"], 1);
            }
            other => panic!("期望 Response，实际 {other:?}"),
        }
    }

    #[test]
    fn parse_notification_vs_reverse_request() {
        let notif = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s"}}"#;
        let rev = r#"{"jsonrpc":"2.0","id":9,"method":"session/request_permission","params":{}}"#;
        assert!(matches!(
            parse_line(notif).unwrap(),
            ParsedMessage::Notification(_)
        ));
        assert!(matches!(
            parse_line(rev).unwrap(),
            ParsedMessage::ReverseRequest(_)
        ));
        assert!(matches!(
            parse_line("{}").unwrap(),
            ParsedMessage::Malformed
        ));
        assert!(parse_line("not json").is_err());
    }

    #[test]
    fn session_update_loose_fields() {
        let v = json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}, "messageId": "m-1"}
        });
        let params: SessionUpdateParams = serde_json::from_value(v).unwrap();
        assert_eq!(params.session_id, "s1");
        assert_eq!(params.update.kind, "agent_message_chunk");
        assert_eq!(params.update.content.unwrap().text.unwrap(), "hi");
        // 未知字段进 rest（不拒绝）
        assert_eq!(params.update.rest["messageId"], "m-1");
    }

    #[test]
    fn reject_option_prefers_reject_kind() {
        let params = json!({"options": [
            {"optionId": "a", "kind": "allow_once"},
            {"optionId": "b", "kind": "reject_once"},
            {"optionId": "c", "kind": "reject_always"}
        ]});
        assert_eq!(reject_option_id(&params).as_deref(), Some("b"));
        // 无拒绝项 → None（不放行）
        let only_allow = json!({"options": [{"optionId": "a", "kind": "allow_once"}]});
        assert_eq!(reject_option_id(&only_allow), None);
    }

    #[test]
    fn request_serialization_shape() {
        let req = RpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "initialize",
            params: json!({"protocolVersion": 1}),
        };
        let line = serde_json::to_string(&req).unwrap();
        assert!(line.contains(r#""method":"initialize""#));
        assert!(line.contains(r#""id":1"#));
    }
}
