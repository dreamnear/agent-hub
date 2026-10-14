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

#[cfg(test)]
mod tests {
    use super::*;

    /// driver-unify 批1 任务 1.3：group 映射全分支覆盖（claude CLI state → 三态兜底）。
    /// 语义为既定行为，本测试做防回归锚定，不得修改映射结果。
    #[test]
    fn map_group_covers_all_cli_state_branches() {
        // NeedsInput：blocked（用户中断转闲的权威态，r49/r69 链路依赖）
        assert_eq!(map_group("blocked"), Group::NeedsInput);
        // Working：working/running/active 三种 CLI 工作态写法
        assert_eq!(map_group("working"), Group::Working);
        assert_eq!(map_group("running"), Group::Working);
        assert_eq!(map_group("active"), Group::Working);
        // Completed：exited/done/completed 三种退出态写法
        assert_eq!(map_group("exited"), Group::Completed);
        assert_eq!(map_group("done"), Group::Completed);
        assert_eq!(map_group("completed"), Group::Completed);
        // Other 兜底：未知字符串、空串（CLI 未上报 state 时 list 侧传 ""）
        assert_eq!(map_group("idle"), Group::Other);
        assert_eq!(map_group("unknown-state"), Group::Other);
        assert_eq!(map_group(""), Group::Other);
    }

    /// AgentSummary 的 group 序列化保持前端契约小驼峰字符串（前端零改动红线）。
    #[test]
    fn group_serializes_to_frontend_contract_strings() {
        let raw = serde_json::to_string(&Group::NeedsInput).unwrap();
        assert_eq!(raw, "\"needs_input\"");
        assert_eq!(
            serde_json::to_string(&Group::Working).unwrap(),
            "\"working\""
        );
        assert_eq!(
            serde_json::to_string(&Group::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(serde_json::to_string(&Group::Other).unwrap(), "\"other\"");
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
    /// 当前上下文占用（最后一条 assistant usage 的 input+cache_read+cache_creation）。
    /// 仅 claude 从转录尾部取得到（cumulative `tokens` 是全回合累计，语义不同）；
    /// 其余驱动暂无来源 → None（fail-soft，不造假数据）。
    pub context_tokens: Option<u64>,
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
