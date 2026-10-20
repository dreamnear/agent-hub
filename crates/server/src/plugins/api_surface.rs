//! 扩展 API 面单一定义源（ADR-0003 第 5 条）。
//!
//! 静态审计、权限判定、`GET /api/plugins/api-schema` 三者共用本文件的表：
//! 任何新增贡献物类型 / RPC 方法 / 权限项必须改这里，否则 schema 端点与校验漂移。
//! （单测 `schema_matches_definition` 锁死「端点输出 == 定义源」。）

use serde::Serialize;

/// 扩展 API 主版本（ADR-0003 第 1 条）：manifest `hub.apiVersion` 与它**相等**才通过。
pub const PLUGIN_API_VERSION: u32 = 1;

/// hub 产品版本（semver）：`hub.minHubVersion` 下限校验用。
/// 与 `CHANGELOG.md` 最新已发布版本同步——发版时随 CHANGELOG 一起更新。
pub const HUB_VERSION: &str = "0.6.0";

/// 贡献物类型（v1 五类，ADR-0011 加法：commands 新增）。
pub const CONTRIBUTION_TYPES: [&str; 5] = ["tools", "skills", "mcp", "views", "commands"];

/// `permissions.data` 固定域枚举（ADR-0007 第 2 条）。
pub const DATA_DOMAINS: [&str; 10] = [
    "sessions:read",
    "sessions:write",
    "messages:read",
    "messages:write",
    "projects:read",
    "notes:read",
    "notes:write",
    "tasks:read",
    "tasks:write",
    "instances:read",
];

/// hub 维护的高危清单（ADR-0007 第 3 条，v1 固定四项，插件不得摘除自己）。
pub const HIGH_RISK_ITEMS: [&str; 4] = [
    "approve-tool-calls",
    "send-messages-as-user",
    "write-session-content",
    "exfiltrate-data",
];

/// 高危项的展示名（审计卡与安装确认 UI 用）。
pub fn high_risk_label(item: &str) -> &'static str {
    match item {
        "approve-tool-calls" => "批准其他工具调用",
        "send-messages-as-user" => "代替用户发消息",
        "write-session-content" => "写既有会话内容",
        "exfiltrate-data" => "对外传输数据",
        _ => "未知高危项",
    }
}

/// postMessage 消息类型（ADR-0004 第 5 条，v1 固定集合）。
pub const HOST_TO_VIEW_MESSAGES: [&str; 4] =
    ["hub:init", "hub:theme", "hub:rpc-result", "hub:rpc-error"];
pub const VIEW_TO_HOST_MESSAGES: [&str; 3] = ["hub:ready", "hub:rpc", "hub:notify"];

/// 一个 RPC 方法的定义：签名、绑定权限、B1 是否已注册。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcMethodDef {
    pub method: &'static str,
    /// 需要声明的 `data` 域（空 = 不判数据域）
    pub data_domains: &'static [&'static str],
    /// 需要声明的凭据名（占位符 `<name>` 表示按调用参数里的名字判定）
    pub credentials: &'static [&'static str],
    /// 需要声明的高危项（空 = 非高危）
    pub high_risk: &'static [&'static str],
    /// B1 已注册的方法为 true；表内未注册方法不在本清单（加法承诺：后续批次追加）
    pub available: bool,
    pub description: &'static str,
}

/// v1 RPC 方法表（backend → hub 方向；ADR-0003 第 3.b 条 / 技术设计 §4.1）。
///
/// B1 注册其中最小集；`hub.rpc` 全量方法在 B3 追加（只增不改）。
pub const RPC_METHODS: &[RpcMethodDef] = &[
    RpcMethodDef {
        method: "hub.credentials.use",
        data_domains: &[],
        credentials: &["<name>"],
        high_risk: &[],
        available: true,
        description: "按名取用 hub 代持的凭据（值只在应答载荷出现，零回显）",
    },
    RpcMethodDef {
        method: "hub.storage.get",
        data_domains: &[],
        credentials: &[],
        high_risk: &[],
        available: true,
        description: "读取插件自持数据（限 plugin_data/<id>/）",
    },
    RpcMethodDef {
        method: "hub.storage.set",
        data_domains: &[],
        credentials: &[],
        high_risk: &[],
        available: true,
        description: "写入插件自持数据（限 plugin_data/<id>/）",
    },
    RpcMethodDef {
        method: "hub.log.emit",
        data_domains: &[],
        credentials: &[],
        high_risk: &[],
        available: true,
        description: "写 hub 日志（hub 侧脱敏）",
    },
    RpcMethodDef {
        method: "hub.projects.list",
        data_domains: &["projects:read"],
        credentials: &[],
        high_risk: &[],
        available: true,
        description: "工程清单",
    },
    RpcMethodDef {
        method: "hub.projects.effective",
        data_domains: &["projects:read"],
        credentials: &[],
        high_risk: &[],
        available: true,
        description: "可验证工程清单（工程注册表 ∪ 全部 agent cwd 去重，每项含 path 与 source）",
    },
    // —— 以下为 v1 目标方法，B3 注册（只增不改；未注册期间调用 = 未知方法拒绝）——
    RpcMethodDef {
        method: "hub.sessions.list",
        data_domains: &["sessions:read"],
        credentials: &[],
        high_risk: &[],
        available: false,
        description: "会话清单（B3）",
    },
    RpcMethodDef {
        method: "hub.sessions.messages",
        data_domains: &["messages:read"],
        credentials: &[],
        high_risk: &[],
        available: false,
        description: "单会话最近消息（B3）",
    },
    RpcMethodDef {
        method: "hub.notes.read",
        data_domains: &["notes:read"],
        credentials: &[],
        high_risk: &[],
        available: false,
        description: "工程便签读取（B3）",
    },
    RpcMethodDef {
        method: "hub.notes.write",
        data_domains: &["notes:write"],
        credentials: &[],
        high_risk: &[],
        available: false,
        description: "工程便签写入（B3）",
    },
    RpcMethodDef {
        method: "hub.tasks.list",
        data_domains: &["tasks:read"],
        credentials: &[],
        high_risk: &[],
        available: false,
        description: "定时任务清单（B3）",
    },
    RpcMethodDef {
        method: "hub.tasks.toggle",
        data_domains: &["tasks:write"],
        credentials: &[],
        high_risk: &["write-session-content"],
        available: false,
        description: "定时任务启停（B3）",
    },
    RpcMethodDef {
        method: "hub.sessions.send",
        data_domains: &["sessions:write"],
        credentials: &[],
        high_risk: &["send-messages-as-user"],
        available: false,
        description: "代发消息（B2/B3，逐次确认）",
    },
];

/// 主版本不匹配的可定位文案（ADR-0003 第 1 条）。
pub fn api_version_mismatch(declared: &str) -> String {
    format!("插件要求扩展 API 主版本 {declared}，本 hub 支持 {PLUGIN_API_VERSION}")
}

/// schema 端点输出（`GET /api/plugins/api-schema`，ADR-0003 第 5 条）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiSchema {
    pub api_version: u32,
    pub hub_version: &'static str,
    pub contributions: [&'static str; 5],
    pub data_domains: [&'static str; 10],
    pub high_risk: Vec<HighRiskItem>,
    pub rpc_methods: Vec<RpcMethodDef>,
    pub host_to_view_messages: [&'static str; 4],
    pub view_to_host_messages: [&'static str; 3],
    /// 弃用项（ADR-0003 第 2 条）：v1 无弃用项
    pub deprecated: Vec<String>,
    /// v1 权限面说明（ADR-0003 拍板结论 4/5）：`api` 类声明接受但无消费者
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HighRiskItem {
    pub id: &'static str,
    pub label: &'static str,
}

/// 由定义源生成 schema（端点 handler 直接调用；单测锁死一致性）。
pub fn schema() -> ApiSchema {
    ApiSchema {
        api_version: PLUGIN_API_VERSION,
        hub_version: HUB_VERSION,
        contributions: CONTRIBUTION_TYPES,
        data_domains: DATA_DOMAINS,
        high_risk: HIGH_RISK_ITEMS
            .iter()
            .map(|id| HighRiskItem {
                id,
                label: high_risk_label(id),
            })
            .collect(),
        rpc_methods: RPC_METHODS.to_vec(),
        host_to_view_messages: HOST_TO_VIEW_MESSAGES,
        view_to_host_messages: VIEW_TO_HOST_MESSAGES,
        deprecated: Vec::new(),
        notes: vec![
            "permissions.api 在 v1 仅作细粒度预留（hub.http.fetch 未进 v1），声明被接受但不生效"
                .into(),
            "插件不得直接调用 /api/*；一切 hub 访问经 RPC（ADR-0003 第 4 条）".into(),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_matches_definition() {
        let s = schema();
        assert_eq!(s.api_version, PLUGIN_API_VERSION);
        assert_eq!(s.rpc_methods.len(), RPC_METHODS.len());
        // 方法名唯一
        let mut names: Vec<&str> = s.rpc_methods.iter().map(|m| m.method).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), RPC_METHODS.len(), "RPC 方法名必须唯一");
        // 高危项与 ADR 固定清单一致
        assert_eq!(s.high_risk.len(), 4);
        // 凭据占位符只允许出现在 hub.credentials.use
        for m in RPC_METHODS {
            if m.credentials.contains(&"<name>") {
                assert_eq!(m.method, "hub.credentials.use");
            }
        }
    }

    #[test]
    fn data_domains_are_unique() {
        let mut d = DATA_DOMAINS.to_vec();
        d.sort_unstable();
        d.dedup();
        assert_eq!(d.len(), DATA_DOMAINS.len());
    }
}
