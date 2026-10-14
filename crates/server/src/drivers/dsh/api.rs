//! dsh 线协议强类型（agent-hub-dsh-driver）：形状取自 dsh v0.1.1-rc.2 **线协议
//! schema**（api-gateway zod 契约，2026-10-02 实抓 + 真实端点无 prompt 连通核对）。
//! serde 全宽松（未知字段容忍、Option 兜底）——developer preview 升级加字段不炸
//!（需求决策 3：只依赖 /api 线协议，禁止依赖包内部结构）。

use serde::Deserialize;

/// RPC 信封 `{"type":"server-response","rpcId":..,"result":{ok,value|error}}` 的
/// result 槽（rpcId 由调用方自比对，这里只解包）。
#[derive(Debug, Clone, Deserialize)]
pub struct RpcResult {
    pub ok: bool,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RpcError {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
}

/// `host.describe` value 子集（健康检查 + 版本探测 + 默认模型标注）。
#[derive(Debug, Clone, Deserialize)]
pub struct HostDescribe {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub provider: String,
}

/// `session.list` 单条（实测 2026-10-02：sessionId/updatedAt/running/blank/cwd/
/// agentPreset/projections.values.{title,tokenUsage,...}）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DshSession {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// 最近活动 epoch 毫秒（实测数字；缺省 0）
    #[serde(default)]
    pub updated_at: f64,
    /// agent 是否在跑（侧栏 busy 事实源，每次 list 现读服务端真值）
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub blank: bool,
    #[serde(default)]
    pub cwd: String,
    /// 0.2.0：subagent 会话随主清单内联（origin=subagent + parentSessionId）
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<String>,
    #[serde(default)]
    pub agent_available: bool,
    #[serde(default)]
    pub projections: Option<Projections>,
}

/// 投影块（asOfSeq + values 宽表；title/tokenUsage 等由宿主各自 schema 校验后宽传）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Projections {
    #[serde(default)]
    pub as_of_seq: i64,
    #[serde(default)]
    pub values: std::collections::BTreeMap<String, serde_json::Value>,
}

impl DshSession {
    /// 标题（title 投影缺省回退 sessionId 尾段，空会话不显空白）。
    pub fn title(&self) -> String {
        self.projections
            .as_ref()
            .and_then(|p| p.values.get("title"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                self.session_id
                    .rsplit('-')
                    .next()
                    .unwrap_or(&self.session_id)
                    .to_string()
            })
    }

    /// token 用量（tokenUsage 投影；形状宽传，取 input+output(+reasoning) 数字和，
    /// 缺省 None = 前端不显示）。
    pub fn tokens(&self) -> Option<u64> {
        let v = self.projections.as_ref()?.values.get("tokenUsage")?;
        let num = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let total = num("input") + num("output") + num("reasoning");
        (total > 0.0).then_some(total as u64)
    }

    /// 最近活动秒级 epoch（AgentSummary.started_at）。
    pub fn updated_epoch(&self) -> Option<i64> {
        (self.updated_at > 0.0).then(|| (self.updated_at / 1000.0) as i64)
    }
}

/// 会话事件（0.2.0 `session/page` records 条目与事件流帧共用；`{event,view?}`）。
/// event 本体 `{type,seq,time,data}`；view = 宿主算好的工具卡标注（`{for,view:{card}}`，
/// 仅锁定 for 判别）。
#[derive(Debug, Clone, Deserialize)]
pub struct HistoryEntry {
    pub event: SessionEvent,
    #[serde(default)]
    pub view: Option<ToolEventView>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub seq: i64,
    /// epoch 毫秒（数字）或 ISO 文本——宽松，映射层归一
    #[serde(default)]
    pub time: Option<serde_json::Value>,
    #[serde(default)]
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolEventView {
    pub r#for: String,
}

/// `session.history` value：`{events,hasMore,projections?}`（projections 仅尾页）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryValue {
    #[serde(default)]
    pub events: Vec<HistoryEntry>,
    #[serde(default)]
    pub has_more: bool,
}

/// `session.models` value：`{current,routable,groups,failures}`（当前读 + 每会话目录）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModels {
    #[serde(default)]
    pub current: Option<ModelSelection>,
    #[serde(default)]
    pub routable: bool,
    #[serde(default)]
    pub groups: Vec<ModelGroup>,
}

/// 0.2.0 `session/page` value：`{records:[{event}],hasMore}`。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageValue {
    #[serde(default)]
    pub records: Vec<PageRecord>,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PageRecord {
    pub event: SessionEvent,
}

/// 0.2.0 `session/modelCatalog` value：`{default,routableProviders,groups,failures}`。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalog {
    #[serde(default)]
    pub default: Option<ModelSelection>,
    #[serde(default)]
    pub groups: Vec<ModelGroup>,
}

/// `llm.models` value：`{groups:[...]}`（全局目录，无 current）。
#[derive(Debug, Clone, Deserialize)]
pub struct LlmModels {
    #[serde(default)]
    pub groups: Vec<ModelGroup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelGroup {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub models: Vec<CatalogModel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatalogModel {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// 思考档位（efforts[].id + defaultEffort；缺省 = 无思考语义）
    #[serde(default)]
    pub reasoning: Option<ReasoningMeta>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningMeta {
    #[serde(default)]
    pub efforts: Vec<Effort>,
    #[serde(default)]
    pub default_effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Effort {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// 模型选择三元组（current 读数与 selectModel 入参同形）。
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// `subagent.list` 条目（kind=child 可用行 / kind=diagnostic 损坏行——
/// developer preview 存储校验失败如实降级为不可展开行）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentListValue {
    #[serde(default)]
    pub entries: Vec<serde_json::Value>,
    #[serde(default)]
    pub parent_available: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测 session.list 条目（2026-10-02 13081 端口真端点核对）全字段解析
    #[test]
    fn parses_real_session_list_item() {
        let raw = r#"{"sessionId":"session-89147341","updatedAt":1791012345678,"running":false,
            "blank":true,"cwd":"/repo","agentPreset":"build",
            "projections":{"asOfSeq":2,"values":{"title":"test agent","tokenUsage":{"input":10,"output":5,"reasoning":1,"cacheRead":0}}}}"#;
        let s: DshSession = serde_json::from_str(raw).unwrap();
        assert_eq!(s.session_id, "session-89147341");
        assert!(!s.running);
        assert_eq!(s.title(), "test agent");
        assert_eq!(s.tokens(), Some(16));
        assert_eq!(s.updated_epoch(), Some(1791012345));
    }

    /// title/tokenUsage 投影缺省：回退 sessionId 尾段、tokens None
    #[test]
    fn session_defaults_are_honest() {
        let s: DshSession =
            serde_json::from_str(r#"{"sessionId":"session-abc-def","running":true}"#).unwrap();
        assert!(s.running);
        assert_eq!(s.title(), "def");
        assert_eq!(s.tokens(), None);
        assert_eq!(s.updated_epoch(), None);
    }

    /// 实测 session.history 事件包裹形态（空会话真端点：permission/preset 等 preset 事件）
    #[test]
    fn parses_history_envelope() {
        let raw = r#"{"events":[
            {"event":{"type":"permission/preset","seq":0,"time":1791012345000,"data":{"preset":"build"}}},
            {"event":{"type":"sandbox/mode","seq":1,"time":1791012346000,"data":{}},"view":{"for":"call","view":{"card":"bash"}}}
        ],"hasMore":false,"projections":{"asOfSeq":1,"values":{}}}"#;
        let h: HistoryValue = serde_json::from_str(raw).unwrap();
        assert_eq!(h.events.len(), 2);
        assert_eq!(h.events[0].event.event_type, "permission/preset");
        assert_eq!(h.events[0].event.seq, 0);
        assert!(h.events[1].view.is_some(), "view 槽可解析");
        assert!(!h.has_more);
    }
}
