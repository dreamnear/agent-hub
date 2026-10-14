//! driver 层统一抽象：统一驱动三面（driver-unify）+ 消息发送路由（P3 统一消息入口）。
//! routes map 以 driver 名索引，未知 driver 报错（对齐 P1 REST 404 语义）。
//!
//! 三 trait（`SessionControl`/`AgentCatalog`/`SessionHistory`）= AgentDriver = app 层
//! 唯一 agent 能力入口（批 3 完成切轨，api 层零 claude 直调）。ACP 缺失的能力一律
//! 显式 `unsupported` 错误，不留 panic。
//! 轨一 `MessageSender` + MessageRoutes 仅剩 ACP prompt 端点在用（api/acp.rs），
//! 待该端点切 SessionControl::send 后一并退场。

pub mod acp;
pub mod claude;
pub mod dsh;
pub mod events;
pub mod omp;
pub mod opencode;
pub mod pi;

// 门面重导出：api 层只经 `drivers::` 取驱动类型/条目类型，不出现 claude 子模块路径
// （driver-unify 2.7 判据：api/ 层零 drivers::claude）。
pub use claude::ClaudeDriver;

use anyhow::Result;
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::models::{AgentSummary, ChatMessage};
pub use claude::subagents::SubagentEntry;
pub use claude::tasks::TaskEntry;
use events::{ChatEvent, MessagePage};

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
    /// 中断 agent 当前处理（P5 C2；Claude 实现 = 注入 Esc，含就绪时序）。
    fn interrupt<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;
}

/// 显式「驱动不支持该能力」错误载体（api 层据此映射 404；ACP 缺失能力不写
/// unimplemented!() panic）。驱动域的「资源不存在」错误统一携带此标记 → 404。
#[derive(Debug, thiserror::Error)]
#[error("driver {driver} 不支持 {capability}")]
pub struct Unsupported {
    pub driver: &'static str,
    pub capability: &'static str,
}

pub fn unsupported(driver: &'static str, capability: &'static str) -> anyhow::Error {
    anyhow::Error::new(Unsupported { driver, capability })
}

/// 驱动域「资源不存在」（respawn 未知会话等；api 层映射 404，替代 CLI 文案匹配）。
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NotFound(pub String);

pub fn not_found(msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(NotFound(msg.into()))
}

/// 会话启动请求（`AgentCatalog::start` 入参；claude = `claude --bg` 参数，cwd 必填）。
#[derive(Debug, Clone)]
pub struct StartSpec {
    pub prompt: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub cwd: String,
}

/// 轨二 A：会话控制面（MessageSender 的加法扩展，签名对齐其现有方法）。
pub trait SessionControl: Send + Sync {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>>;
    /// 中断当前处理（= MessageSender::interrupt）。
    fn cancel<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;
    fn is_finished(&self, id: &str) -> bool;
    fn release(&self, id: &str) -> Result<()>;
    /// 权限应答；无权限语义（claude PTY 链路）→ Err(unsupported)。
    fn answer_permission(&self, id: &str, perm_id: &str, option_id: Option<&str>) -> Result<bool>;
    /// ask_user_question 应答（dsh $events waterfall 通路； eventId + answers）。
    /// 无此语义的驱动 → Err(unsupported)。
    fn answer_question(
        &self,
        id: &str,
        event_id: &str,
        answers: serde_json::Value,
    ) -> BoxFuture<'_, Result<()>> {
        let _ = (id, event_id, answers);
        Box::pin(async { Err(unsupported("driver", "answer_question")) })
    }
}

/// 轨二 B：会话目录面（ClaudeDriver::list 的泛化）。
pub trait AgentCatalog: Send + Sync {
    /// 清单（内含 group 归一与中断覆盖；无标记的语义由各驱动内部保证）。
    fn list<'a>(&'a self, all: bool) -> BoxFuture<'a, Result<Vec<AgentSummary>>>;
    /// 精确查询（替 api 层全表扫 + find）。
    fn get<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<AgentSummary>>>;
    fn start<'a>(&'a self, spec: &'a StartSpec) -> BoxFuture<'a, Result<String>>;
    fn stop<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;
    fn remove<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// 无此语义的驱动（ACP）→ Err(unsupported)。
    fn respawn<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>>;
    /// 无此语义的驱动（ACP）→ Err(unsupported)。
    fn logs<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<String>>;
}

/// 轨二 C：会话历史/观测面。
pub trait SessionHistory: Send + Sync {
    /// 分页读取；None = 会话/文件未找到（api 层翻译为空页或 404）。
    fn page<'a>(
        &'a self,
        id: &'a str,
        limit: usize,
        before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>>;
    /// 旧全量读取（/messages 回归基线端点；claude = 全量解析，ACP = feed 回放）。
    fn full_history<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<ChatMessage>>>;
    /// 输出活跃度（claude = jsonl mtime 静默判定；ACP 无此口径）。
    fn is_active<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<bool>>;
    /// subagent 清单（默认口径：只回活跃条目，2026-10-01 产品纪律——历史条目默认
    /// 不进面板）；无 subagent 语义（ACP）→ 空集。
    fn subagents<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>>;
    /// subagent 全量清单（显式 opt-in 含已结束；历史入口取数用）。
    /// 默认实现 = 无历史口径（ACP/opencode），与 subagents 空集语义一致——
    /// 既有实现方无需改动。
    fn subagents_finished<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Vec<SubagentEntry>>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
    fn subagent_page<'a>(
        &'a self,
        id: &'a str,
        sub: &'a str,
        limit: usize,
        before: Option<usize>,
    ) -> BoxFuture<'a, Result<Option<MessagePage>>>;
    /// 跨会话任务聚合。None = 无权威视角（前端回退单会话口径）、Some(vec![]) = 权威空集
    /// ——两者语义勿合并（tester-r27 定案，K5）；无任务语义（ACP）→ None。
    fn tasks<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<Vec<TaskEntry>>>>;
    /// 取/建该会话的 chat 房间并保证恰一个 tail 在跑（ws.rs 房间去重语义内化）。
    fn ensure_chat_room(&self, session_id: &str) -> broadcast::Sender<ChatEvent>;
    fn ensure_subagent_room(
        &self,
        session_id: &str,
        subagent_id: &str,
    ) -> broadcast::Sender<ChatEvent>;
}

/// 三面之和 + 驱动名（DriverRegistry 注册键）。
/// + ModelSettings（会话外观，agent-hub-model-thinking）：默认实现 = 不支持。
///   各驱动按能力覆写——ACP 无模型清单/切换语义，继承默认即「如实不支持」。
pub trait AgentDriver: SessionControl + AgentCatalog + SessionHistory + ModelSettings {
    fn name(&self) -> &'static str;

    /// 认证失效标记（急修第二刀）：dsh 收编到带认证实例且未配 token 时为真，
    /// 侧栏「凭据失效」降级。默认恒 false（claude/opencode/ACP 无此语义）。
    fn auth_error(&self) -> bool {
        false
    }
}

/// 会话外观能力声明（agent-hub-model-thinking，driver-unify 决策 5：模型与思考
/// 是会话的外观，hub 只透传与展示，事实源在各驱动侧，hub 不存会话模型状态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCaps {
    /// 可切换模型（面板可写）
    pub can_set: bool,
    /// 可读当前模型（读不到如实 false，前端显示「由会话进程管理」）
    pub can_read: bool,
    /// 有思考强度语义（false → 前端只渲染模型芯片）
    pub supports_thinking: bool,
}

impl ModelCaps {
    /// 无模型语义的驱动（ACP 等）。
    pub const UNSUPPORTED: Self = Self {
        can_set: false,
        can_read: false,
        supports_thinking: false,
    };
}

/// 归一化模型条目（opencode=/provider 扁平化；claude=内置常用清单，注释注明来源）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    /// 可读短名（芯片与面板显示用）
    pub name: String,
    /// 免费档（opencode 供应商目录标注；claude 内置清单全 false）
    pub free: bool,
    /// 思考档位（工程既有语义原样透传；空 = 该模型无思考语义）
    pub variants: Vec<String>,
}

/// 会话当前外观读数（读回自驱动；None = 驱动未记录，前端如实显示）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModel {
    pub id: String,
    pub provider: String,
    /// 思考档位（None = 未设/驱动无语义）
    pub variant: Option<String>,
}

/// 会话外观三透传：清单 / 当前读 / 设置。默认全部不支持（未覆写的驱动如实上报）。
pub trait ModelSettings: Send + Sync {
    fn model_caps(&self) -> ModelCaps {
        ModelCaps::UNSUPPORTED
    }

    fn list_models<'a>(&'a self) -> BoxFuture<'a, Result<Vec<ModelInfo>>> {
        Box::pin(async move { Err(unsupported("driver", "list_models")) })
    }

    fn session_model<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, Result<Option<SessionModel>>> {
        Box::pin(async move { Err(unsupported("driver", "session_model")) })
    }

    /// 设置会话模型/思考；返回面向用户的收场文案（opencode=已切换，
    /// claude=已切换或 working 排队「已排队」）。
    fn set_session_model<'a>(
        &'a self,
        _id: &'a str,
        _model: &'a str,
        _variant: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Err(unsupported("driver", "set_session_model")) })
    }
}

/// 驱动注册表：替 `AppState.driver: ClaudeDriver` 具体类型与 api 层 driver 名单硬编码。
/// Vec 保注册序——`list_all` 聚合顺序与批前一致（claude 先、acp 后；HashMap 序随机不可用）。
#[derive(Clone, Default)]
pub struct DriverRegistry {
    drivers: Vec<Arc<dyn AgentDriver>>,
}

impl DriverRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册驱动（name() 为键；重名覆盖——仅启动期注册两驱动，无运行期动态注册）。
    pub fn register(&mut self, driver: Arc<dyn AgentDriver>) {
        let name = driver.name();
        match self.drivers.iter_mut().find(|d| d.name() == name) {
            Some(slot) => *slot = driver,
            None => self.drivers.push(driver),
        }
    }

    /// 按 driver 名取（替 ensure_driver 硬编码；未知名 Err，api 层映射 404）。
    pub fn get(&self, driver: &str) -> Result<Arc<dyn AgentDriver>> {
        self.drivers
            .iter()
            .find(|d| d.name() == driver)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知 driver: {driver}"))
    }

    /// 聚合全部驱动清单（注册序）。
    pub async fn list_all(&self, all: bool) -> Result<Vec<AgentSummary>> {
        let mut out = Vec::new();
        for d in &self.drivers {
            out.extend(d.list(all).await?);
        }
        Ok(out)
    }

    /// 驱动名清单（注册序）——StartDialog「新建 Agent」选项的事实源（去硬编码）。
    pub fn names(&self) -> Vec<&'static str> {
        self.drivers.iter().map(|d| d.name()).collect()
    }
}

/// 「近期活动」窗口（agent-hub 展示纪律补档，2026-10-03）：dsh/opencode 的会话
/// 没有进程生命周期，turn 结束 ≠ 关闭——非 busy 但窗口内有活动的会话保留在侧栏
/// （Group::Other，Idle 组照常显示），超窗才归历史。事实源 = 驱动自己的时间戳，
/// hub 不引入会话状态存储。常量可调。
pub const RECENT_WINDOW_SECS: i64 = 24 * 60 * 60;

/// 最近活动时间（秒级 epoch）是否落在 RECENT_WINDOW_SECS 内。None（无时间戳）
/// = 不在窗口（归历史，不误伤）。
pub(crate) fn within_recent_window(updated_epoch: Option<i64>) -> bool {
    let Some(updated) = updated_epoch else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (now - updated).abs() < RECENT_WINDOW_SECS
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
