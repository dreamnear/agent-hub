use serde::Serialize;

/// driver 无关的三段式分组 + Other 兜底。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    NeedsInput,
    Working,
    Completed,
    Other,
}

impl Group {
    /// 序列化为前端使用的小驼峰字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            Group::NeedsInput => "needs_input",
            Group::Working => "working",
            Group::Completed => "completed",
            Group::Other => "other",
        }
    }
}

impl Serialize for Group {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// blocked→NeedsInput；working|running|active→Working；exited|done|completed→Completed；其他→Other。
pub fn map_group(raw_state: &str) -> Group {
    match raw_state {
        "blocked" => Group::NeedsInput,
        "working" | "running" | "active" => Group::Working,
        "exited" | "done" | "completed" => Group::Completed,
        _ => Group::Other,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub driver: String,
    pub id: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub kind: Option<String>,
    pub raw_state: Option<String>,
    pub group: Group,
    pub detail: Option<String>,
    pub tokens: Option<u64>,
    pub started_at: Option<i64>,
    pub session_id: Option<String>,
}

/// 会话消息类别（P2 对话 Tab 结构化渲染）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatMessageKind {
    #[default]
    User,
    Assistant,
    ToolUse,
    ToolResult,
    Thinking,
    Other,
}

impl ChatMessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatMessageKind::User => "user",
            ChatMessageKind::Assistant => "assistant",
            ChatMessageKind::ToolUse => "tool_use",
            ChatMessageKind::ToolResult => "tool_result",
            ChatMessageKind::Thinking => "thinking",
            ChatMessageKind::Other => "other",
        }
    }
}

impl Serialize for ChatMessageKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// 前端标签映射（Group::as_str 同款模式）。
pub fn map_kind_label(kind: ChatMessageKind) -> &'static str {
    kind.as_str()
}

/// 会话消息（一行 jsonl 的一个 content 块展开为一条；serde 宽松，字段全 Option）。
/// ts 保留 ISO8601 原文（tasks.md 计划为 i64；实测 timestamp 为 ISO 字符串，
/// 零依赖直传前端 new Date() 解析更简——偏离已注记 tasks.md）。
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub kind: ChatMessageKind,
    pub raw_type: Option<String>,
    pub text: Option<String>,
    pub tool_use_id: Option<String>,
    pub tool_name: Option<String>,
    pub input: Option<serde_json::Value>,
    pub result: Option<serde_json::Value>,
    /// tool_result 块的 error 标记（C6 卡片红/绿点判定）
    pub error: Option<bool>,
    pub ts: Option<String>,
}
