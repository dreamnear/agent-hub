//! driver 层统一抽象：消息发送路由（P3 统一消息入口）。
//! trait 签名面向未来 ACP driver（session prompt）预留——当前仅 ClaudeDriver 实现（PTY 通道），
//! 不实现真 ACP。routes map 以 driver 名索引，未知 driver 报错（对齐 P1 REST 404 语义）。

pub mod acp;
pub mod claude;

use anyhow::Result;
use futures::future::BoxFuture;

/// 统一消息发送抽象：向指定 agent 会话写入用户消息。
/// Claude 实现 = `claude attach <id>` PTY stdin（含就绪时序与死条目复位）；
/// 未来 ACP 实现 = session prompt RPC。
pub trait MessageSender: Send + Sync {
    /// 向 agent `id` 发送用户消息 `text`（实现方负责 TUI 提交语义与就绪时序）。
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>>;
    /// agent 会话是否已结束（用于上层清理死条目）。
    fn is_finished(&self, id: &str) -> bool;
    /// 清理 agent 会话资源（attach 退出后复位条目）。
    fn release(&self, id: &str) -> Result<()>;
    /// 原样写入字节（终端保真视图键盘透传；不做 TUI 提交转换）。
    fn send_raw_bytes(&self, id: &str, data: &[u8]) -> Result<()>;
    /// 中断 agent 当前处理（P5 C2；Claude 实现 = 注入 Esc，含就绪时序）。
    fn interrupt<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// downcast 支撑（终端视图取实现类专属订阅接口）。
    fn as_any(&self) -> &dyn std::any::Any;
}

/// driver 名 → sender 路由表（AppState 持有；启动时注册 "claude"）。
pub type MessageRoutes = std::collections::HashMap<String, std::sync::Arc<dyn MessageSender>>;

/// 统一路由发送：`send(driver, id, msg)` 需求签名的落地。未知 driver 报错。
pub fn route_send<'a>(
    routes: &'a MessageRoutes,
    driver: &'a str,
    id: &'a str,
    text: &'a str,
) -> BoxFuture<'a, Result<()>> {
    Box::pin(async move {
        let sender = routes
            .get(driver)
            .ok_or_else(|| anyhow::anyhow!("未知 driver: {driver}"))?;
        sender.send(id, text).await
    })
}
