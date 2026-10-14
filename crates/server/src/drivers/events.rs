//! 驱动层公共类型（driver-unify 批1 任务 1.1，L5 归位）：ChatEvent / HubEvent /
//! MessagePage 原住 `drivers::claude/{session,watcher}.rs` 却服务全驱动（ACP 反向
//! import claude 模块）——搬至中性位置，解反向依赖。
//! 红线：字段、serde 名、seq 语义一字未动；MessagePage.first_line 仍是 jsonl
//! 物理行号游标（append-only 前提，勿改成消息序号——K3）。

use crate::models::ChatMessage;

/// WS chat 推送事件（serde camelCase）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEvent {
    pub session_id: String,
    pub message: ChatMessage,
    pub seq: u64,
}

/// WS 推送的顶层事件，前端收到任一事件即重拉 /api/agents。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "type")]
pub enum HubEvent {
    #[serde(rename = "jobs_changed")]
    JobsChanged { id: String },
    #[serde(rename = "tick")]
    Tick,
}

/// 分页信封（P6 B12/B13）：页内消息 + 首条源行号游标 + 是否还有更早消息。
#[derive(Debug)]
pub struct MessagePage {
    pub messages: Vec<ChatMessage>,
    /// 本页首条消息的源 jsonl 行号（0 基）；向前翻页游标（before=firstLine）
    pub first_line: usize,
    /// 窗口之前是否还有未读内容
    pub has_more: bool,
}
