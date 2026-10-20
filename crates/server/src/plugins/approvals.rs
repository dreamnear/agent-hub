//! 高危审批流（ADR-0007 第 3 条 / 设计 §6.5，B2 批3）。
//!
//! hub 主进程持有一张内存审批单表：mcp-proxy（`server mcp-proxy` 子进程）在
//! tools/call 命中高危工具时经 loopback HTTP 落单并阻塞等待；用户在 hub UI
//! 批准/拒绝后，落单方取回决定执行或放弃。全部决定留痕（Approval/Rejection
//! 记录入插件故障记录域）。等待上限 120s（设计 §7），超时以 `-32005 busy`
//! 返回、由会话侧重试。
//!
//! 存储 = 纯内存（进程内 once-through 语义）：hub 重启丢待审批单，会话侧
//! 收到连接错误重试即可——不持久化（审批是一次性交互，落盘反增泄漏面）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

/// 等待决策上限（设计 §7：v1 阻塞等待 + 超时 120s）
pub const APPROVAL_WAIT: Duration = Duration::from_secs(120);

/// 一条待确认高危调用（BR-15：清单归 hub；确认点在 hub 侧闸门）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub id: String,
    pub plugin_id: String,
    /// 工具全名（hubplug_<pluginId>_<tool>），面向用户展示
    pub tool: String,
    /// 参数摘要（落单方 scrub 后的短摘要；凭据只出现引用名，BR-11）
    pub args_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// RFC3339 落单时刻
    pub requested_at: String,
}

/// 决策结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Reject,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Approve => "approve",
            Decision::Reject => "reject",
        }
    }
}

/// 决策完成通知（落单方 await）。
type DecisionTx = tokio::sync::oneshot::Sender<Decision>;

struct Entry {
    req: ApprovalRequest,
    created_at: Instant,
    decided_tx: Option<DecisionTx>,
    /// 已决策（等待者已收到；条目保留到 TTL 供查询端点读终态）
    decided: Option<Decision>,
}

/// 审批单表（hub 主进程单实例；Mutex 不跨 await——decide 为同步函数）。
#[derive(Default)]
pub struct ApprovalStore {
    pending: Mutex<HashMap<String, Entry>>,
}

/// 落单返回：单据 + 决策接收端（await 之）。
pub struct Issued {
    pub id: String,
    pub rx: tokio::sync::oneshot::Receiver<Decision>,
}

/// decide 结果（错误面向 HTTP 端点映射）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideError {
    /// 单不存在或已过 TTL 回收
    NotFound,
    /// 该单已被决策过（幂等二次提交拒绝）
    AlreadyDecided,
}

impl std::fmt::Display for DecideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecideError::NotFound => write!(f, "审批单不存在或已过期"),
            DecideError::AlreadyDecided => write!(f, "审批单已决策"),
        }
    }
}

impl std::error::Error for DecideError {}

impl ApprovalStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// 落单（mcp-proxy 经 loopback POST 调用）。同参数重复落单**不去重**：
    /// 每次调用都是独立的一次高危执行请求（BR-15 逐次确认）。
    pub fn issue(
        &self,
        plugin_id: &str,
        tool: &str,
        args_digest: &str,
        session_id: Option<&str>,
        requested_at: &str,
    ) -> Issued {
        let id = format!("apr-{}", uuid::Uuid::new_v4());
        let req = ApprovalRequest {
            id: id.clone(),
            plugin_id: plugin_id.to_string(),
            tool: tool.to_string(),
            args_digest: args_digest.to_string(),
            session_id: session_id.map(str::to_string),
            requested_at: requested_at.to_string(),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id.clone(),
                Entry {
                    req,
                    created_at: Instant::now(),
                    decided_tx: Some(tx),
                    decided: None,
                },
            );
        Issued { id, rx }
    }

    /// 决策（UI 端点调用）：通知等待者 + 保留终态到 TTL。同步函数（锁不跨 await）。
    pub fn decide(&self, id: &str, decision: Decision) -> Result<ApprovalRequest, DecideError> {
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = map.get_mut(id) else {
            return Err(DecideError::NotFound);
        };
        if entry.decided.is_some() {
            return Err(DecideError::AlreadyDecided);
        }
        entry.decided = Some(decision);
        if let Some(tx) = entry.decided_tx.take() {
            // 等待方已超时放弃时 send 失败——终态已在表里，非错误
            let _ = tx.send(decision);
        }
        Ok(entry.req.clone())
    }

    /// 单条读取（查询端点 / mcp-proxy 长轮询）。
    pub fn get(&self, id: &str) -> Option<(ApprovalRequest, Option<Decision>)> {
        let map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).map(|e| (e.req.clone(), e.decided))
    }

    /// 待确认清单（UI 列表；已决策的不出清单）。
    pub fn pending_list(&self) -> Vec<ApprovalRequest> {
        let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        Self::sweep(&mut map);
        let mut out: Vec<ApprovalRequest> = map
            .values()
            .filter(|e| e.decided.is_none())
            .map(|e| e.req.clone())
            .collect();
        out.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
        out
    }

    /// TTL 回收（APPROVAL_WAIT 之外的条目移除；每次 list 时顺带清理）。
    fn sweep(map: &mut HashMap<String, Entry>) {
        let cutoff = Instant::now() - APPROVAL_WAIT;
        map.retain(|_, e| e.created_at > cutoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn issue_then_decide_approves_waiter() {
        let store = ApprovalStore::new();
        let issued = store.issue(
            "p1",
            "hubplug_p1_del",
            "{}",
            Some("s1"),
            "2026-10-21T00:00:00Z",
        );
        // 查询端点视角：待确认清单含该单
        assert_eq!(store.pending_list().len(), 1);
        // 决策前单条读取：未决
        let (req, decided) = store.get(&issued.id).unwrap();
        assert_eq!(req.plugin_id, "p1");
        assert_eq!(decided, None);
        // 批准 → 等待方收到 Approve
        let stored = store.decide(&issued.id, Decision::Approve).unwrap();
        assert_eq!(stored.tool, "hubplug_p1_del");
        assert_eq!(issued.rx.await.unwrap(), Decision::Approve);
        // 已决策条目不在待确认清单，但终态可读
        assert!(store.pending_list().is_empty());
        let (_, decided) = store.get(&issued.id).unwrap();
        assert_eq!(decided, Some(Decision::Approve));
        // 二次决策拒绝（防重放）
        assert!(matches!(
            store.decide(&issued.id, Decision::Reject),
            Err(DecideError::AlreadyDecided)
        ));
    }

    #[tokio::test]
    async fn reject_notifies_waiter_and_unknown_id_maps_not_found() {
        let store = ApprovalStore::new();
        let issued = store.issue("p2", "hubplug_p2_send", "{}", None, "2026-10-21T00:00:01Z");
        store.decide(&issued.id, Decision::Reject).unwrap();
        assert_eq!(issued.rx.await.unwrap(), Decision::Reject);
        assert!(matches!(
            store.decide("apr-nope", Decision::Approve),
            Err(DecideError::NotFound)
        ));
    }

    #[test]
    fn pending_list_sorted_by_requested_at() {
        let store = ApprovalStore::new();
        store.issue("p3", "t3", "{}", None, "2026-10-21T00:00:02Z");
        store.issue("p3", "t3", "{}", None, "2026-10-21T00:00:01Z");
        let list = store.pending_list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].requested_at, "2026-10-21T00:00:01Z");
    }

    /// 决策后等待方先超时放弃（rx 被 drop）→ decide 仍成功且终态可读
    ///（覆盖 tx.send 失败分支）。
    #[tokio::test]
    async fn decide_after_waiter_dropped_still_records() {
        let store = ApprovalStore::new();
        let issued = store.issue("p4", "t4", "{}", None, "2026-10-21T00:00:03Z");
        drop(issued.rx);
        let stored = store.decide(&issued.id, Decision::Approve).unwrap();
        assert_eq!(stored.id, issued.id);
        let (_, decided) = store.get(&issued.id).unwrap();
        assert_eq!(decided, Some(Decision::Approve));
    }
}
