//! 事件驱动的会话监听/提醒机制（watcher hook，ADR-0017）：hub 内置消费者。
//!
//! 事件源复用 `plugins::hooks` 观察档发射点（进程内观察者槽），状态/用量类条件
//! 由本模块自身状态巡检 diff 出穿越点；命中规则后向 hub 助手注入一条提醒。
//! 插件面（observer-only / RPC / 权限）零改动——本模块不注册插件、不暴露端点。
//!
//! - [`rules`]：`watcher.json` schema、加载与逐条降级、作用域/内容匹配、渲染
//! - [`engine`]：会话状态表、冷却/去重/限流、触发→注入编排

pub mod engine;
pub mod rules;
