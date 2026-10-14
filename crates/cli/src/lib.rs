//! `ah` 库体：hub 客户端（api）+ serve 生命周期（serve）+ attach 交互（attach）。
//! bin（main.rs）只做 CLI 解析与输出组装；集成测试直接打 lib。

pub mod api;
pub mod attach;
pub mod serve;

/// 测试/外部装配便捷入口：指定 hub 地址构造客户端。
pub fn hub(base: impl Into<String>) -> api::Hub {
    api::Hub::new(base.into())
}
