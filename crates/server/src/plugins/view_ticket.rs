//! 视图票据（ADR-0004 第 3 条）：内存表 + TTL。
//!
//! 票据 = 32 字节随机不透明串，**作用域 = 该插件 id 的 view 子树**，TTL 默认 120s。
//! 关键性质：票据不是 hub token，`auth_middleware` 只认 hub token，票据值域独立，
//! **换不到任何 `/api` 能力**。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 默认 TTL（ADR-0004 拍板结论 3）。
pub const DEFAULT_TTL_SECS: u64 = 120;

struct Ticket {
    plugin_id: String,
    expires_at: Instant,
}

pub struct TicketStore {
    ttl: Duration,
    tickets: Mutex<HashMap<String, Ticket>>,
}

impl std::fmt::Debug for TicketStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TicketStore")
            .field("ttl", &self.ttl)
            .finish()
    }
}

impl Default for TicketStore {
    fn default() -> Self {
        Self::with_ttl(Duration::from_secs(DEFAULT_TTL_SECS))
    }
}

impl TicketStore {
    pub fn with_ttl(ttl: Duration) -> Self {
        Self {
            ttl,
            tickets: Mutex::new(HashMap::new()),
        }
    }

    pub fn ttl_secs(&self) -> u64 {
        self.ttl.as_secs()
    }

    /// 签发：返回 (票据, TTL 秒)。每次签发前顺手清理过期项。
    pub fn issue(&self, plugin_id: &str) -> (String, u64) {
        // 32 字节随机：两个 v4 UUID 拼接（128bit × 2），hex 64 字符
        let ticket = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let mut guard = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        guard.retain(|_, t| t.expires_at > now);
        guard.insert(
            ticket.clone(),
            Ticket {
                plugin_id: plugin_id.to_string(),
                expires_at: now + self.ttl,
            },
        );
        (ticket, self.ttl.as_secs())
    }

    /// 校验：票据存在、未过期、作用域与插件 id 一致。
    pub fn validate(&self, plugin_id: &str, ticket: &str) -> bool {
        let guard = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        match guard.get(ticket) {
            Some(t) => t.plugin_id == plugin_id && t.expires_at > Instant::now(),
            None => false,
        }
    }

    /// 停用 / 卸载时清该插件全部票据（BR-03 同现同隐：入口消失，旧票据一并失效）。
    pub fn revoke_plugin(&self, plugin_id: &str) {
        let mut guard = self.tickets.lock().unwrap_or_else(|e| e.into_inner());
        guard.retain(|_, t| t.plugin_id != plugin_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_validate_scoped() {
        let s = TicketStore::default();
        let (t, ttl) = s.issue("plugin-a");
        assert_eq!(ttl, DEFAULT_TTL_SECS);
        assert_eq!(t.len(), 64);
        assert!(s.validate("plugin-a", &t));
        // 作用域不符：票据换不到别的插件
        assert!(!s.validate("plugin-b", &t));
        assert!(!s.validate("plugin-a", "not-a-ticket"));
    }

    #[test]
    fn expired_ticket_is_rejected() {
        let s = TicketStore::with_ttl(Duration::from_millis(20));
        let (t, _) = s.issue("plugin-a");
        std::thread::sleep(Duration::from_millis(40));
        assert!(!s.validate("plugin-a", &t));
    }

    #[test]
    fn revoke_clears_scoped_tickets() {
        let s = TicketStore::default();
        let (t1, _) = s.issue("plugin-a");
        let (t2, _) = s.issue("plugin-b");
        s.revoke_plugin("plugin-a");
        assert!(!s.validate("plugin-a", &t1));
        assert!(s.validate("plugin-b", &t2), "跨插件不受影响");
    }

    #[test]
    fn tickets_are_unique() {
        let s = TicketStore::default();
        let a = s.issue("p").0;
        let b = s.issue("p").0;
        assert_ne!(a, b);
    }
}
