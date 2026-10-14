//! opencode API 强类型（v1.18.32 `/doc` openapi 实抓 schema 收窄 + 2026-10-01 实测
//! 响应体核对）。serde 全宽松（未知字段容忍、Option 兜底）——opencode 升级加字段不炸。
//! 直连 SQLite 属范围外；这里只是 HTTP 响应的载体类型。

use serde::Deserialize;

/// `GET /session` 单条（Session schema 子集：driver 用到的字段）。
#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeSession {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// 会话工作目录（`?directory=` 过滤键）
    #[serde(default)]
    pub directory: String,
    /// 会话当前模型（v2 switchModel 写入后回读；null = 从未设，前端如实显示）
    #[serde(default)]
    pub model: Option<OpencodeModelRef>,
    /// 当前模型思考档位（switchModel 带 variant 时回读；与 model 同源）
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub tokens: Option<OpencodeTokens>,
    #[serde(default)]
    pub time: Option<OpencodeTime>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeModelRef {
    #[serde(rename = "providerID", alias = "providerId")]
    pub provider_id: String,
    /// 实测会话载荷字段为 `id`（openapi Session.model.id）；modelID 仅作消息侧别名
    #[serde(alias = "modelID")]
    pub id: String,
    /// 思考档位（switchModel 可带；消息侧实测也有 variant）
    #[serde(default)]
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeTokens {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub reasoning: Option<f64>,
    #[serde(default)]
    pub cache: Option<serde_json::Value>,
}

impl OpencodeTokens {
    /// 侧栏 tokens 列（input+output(+reasoning)；cache read 不计费口径不入）。
    pub fn total(&self) -> u64 {
        (self.input + self.output + self.reasoning.unwrap_or(0.0)) as u64
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeTime {
    /// epoch 毫秒（openapi integer；实测 JSON 数字）
    pub created: f64,
    /// 最近活动毫秒（会话清单实测带 updated；缺省回退 created）
    #[serde(default)]
    pub updated: Option<f64>,
}

impl OpencodeTime {
    /// ISO 文本（前端 `new Date()` 直解析；ChatMessage ts 同款零依赖直传）。
    pub fn created_iso(&self) -> String {
        let ms = self.created as i64;
        let secs = ms.div_euclid(1000);
        // 不引 chrono（一个格式化不值得新增依赖面）；亚秒精度不入 ts（前端展示秒级）
        humantime_rfc3339(secs)
    }

    /// 最近活动（秒级 epoch；updated 缺省回退 created）。
    /// AgentSummary.started_at 的取值源——对历史条目该字段承载「最近活动」口径。
    pub fn last_activity_epoch(&self) -> Option<i64> {
        Some(((self.updated.unwrap_or(self.created)) / 1000.0) as i64)
    }

    /// AgentSummary.started_at（秒级 epoch i64）。
    pub fn created_epoch(&self) -> Option<i64> {
        Some((self.created / 1000.0) as i64)
    }
}

/// 手拼 RFC3339 UTC（不引 chrono/时间库——一个格式化不值得新增依赖面）。
fn humantime_rfc3339(secs: i64) -> String {
    // civil-from-days 算法（Howard Hinnant）：epoch 天 → y/m/d
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let (h, mi, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// `GET /session/{id}/message` 单条（info + parts 信封）。
#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeMessage {
    pub info: OpencodeMessageInfo,
    #[serde(default)]
    pub parts: Vec<OpencodePart>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeMessageInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub time: Option<OpencodeTime>,
}

/// Part 多态（type 判别；未列出的类型 → Unknown 宽松忽略，ACP 同款策略）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum OpencodePart {
    Text(TextPart),
    Reasoning(ReasoningPart),
    Tool(ToolPart),
    /// patch：仅 files[]+hash（无 diff 文本），映射忽略（模块头注记）
    Patch(PatchPart),
    StepStart {},
    StepFinish {},
    #[serde(untagged)]
    Unknown(serde_json::Value),
}

impl OpencodePart {
    /// part 稳定 id（openapi 各 Part schema 均 required id；增量推送去重键）。
    pub fn id(&self) -> &str {
        match self {
            OpencodePart::Text(p) => &p.id,
            OpencodePart::Reasoning(p) => &p.id,
            OpencodePart::Tool(p) => &p.id,
            OpencodePart::Patch(p) => &p.id,
            OpencodePart::StepStart {} | OpencodePart::StepFinish {} => "",
            OpencodePart::Unknown(v) => v["id"].as_str().unwrap_or(""),
        }
    }

    pub(crate) fn as_text(&self) -> Option<String> {
        match self {
            OpencodePart::Text(p) => Some(p.text.clone()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextPart {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
    /// 系统注入标记（实测 memory 注入带 synthetic:true）→ 前端渲染噪音，映射跳过
    #[serde(default)]
    pub synthetic: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReasoningPart {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PatchPart {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub hash: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolPart {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "callID", alias = "call_id", default)]
    pub call_id: String,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub state: ToolState,
}

/// ToolState 四态（status 内部 tag 判别；未列字段宽松忽略——input/time 不需要）。
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ToolState {
    Pending,
    Running {
        #[serde(default)]
        metadata: ToolMetadata,
    },
    Completed {
        #[serde(default)]
        output: String,
        #[serde(default)]
        metadata: ToolMetadata,
    },
    Error {
        #[serde(default)]
        error: String,
        #[serde(default)]
        metadata: ToolMetadata,
    },
}

impl Default for ToolState {
    /// state 字段缺省兜底（正常载荷必有 state；畸形响应按 Pending 处理，不出运行中卡）。
    fn default() -> Self {
        ToolState::Pending
    }
}

impl ToolState {
    /// 态上的 metadata（title/filediff；pending 无）。
    pub fn metadata(&self) -> Option<&ToolMetadata> {
        match self {
            ToolState::Pending => None,
            ToolState::Running { metadata }
            | ToolState::Completed { metadata, .. }
            | ToolState::Error { metadata, .. } => Some(metadata),
        }
    }
}

/// tool state.metadata 收窄（title + filediff；实测 edit/write/patch 工具带）。
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolMetadata {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub filediff: Option<FileDiff>,
}

/// SnapshotFileDiff（file/patch/additions/deletions；patch 为 unified diff 文本）。
#[derive(Debug, Clone, Deserialize)]
pub struct FileDiff {
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub patch: String,
    #[serde(default)]
    pub additions: f64,
    #[serde(default)]
    pub deletions: f64,
}

/// `POST /session`（建会话）响应即 OpencodeSession。
/// `POST /session/{id}/message` body（prompt；默认模型由 opencode 自身配置决定，
/// hub 只透传文本——需求决策 6，不引入模型管理）。
#[derive(Debug, serde::Serialize)]
pub struct PromptBody<'a> {
    pub parts: [PromptTextPart<'a>; 1],
}

#[derive(Debug, serde::Serialize)]
pub struct PromptTextPart<'a> {
    #[serde(rename = "type")]
    pub part_type: &'a str,
    pub text: &'a str,
}

impl<'a> PromptBody<'a> {
    pub fn text(text: &'a str) -> Self {
        Self {
            parts: [PromptTextPart {
                part_type: "text",
                text,
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 实测响应形态核对：tool state 各态反序列化 + session tokens 求和
    #[test]
    fn parses_tool_state_variants() {
        // completed（带 filediff）
        let completed: ToolState = serde_json::from_str(
            r#"{"status":"completed","input":{"filePath":"/repo/t.txt"},"output":"Edit applied successfully.",
                "title":"t.txt","metadata":{"diagnostics":{},"diff":"x","filediff":{"file":"/repo/t.txt","patch":"-a\n+b","additions":1,"deletions":1},"truncated":false},"time":{"start":1,"end":2}}"#,
        )
        .unwrap();
        match &completed {
            ToolState::Completed { output, metadata } => {
                assert_eq!(output, "Edit applied successfully.");
                let fd = metadata.filediff.as_ref().unwrap();
                assert_eq!(fd.file, "/repo/t.txt");
            }
            other => panic!("应为 completed: {other:?}"),
        }

        // error 态（实测 question 工具 error，output=null）
        let errored: ToolState = serde_json::from_str(
            r#"{"status":"error","input":{"q":1},"error":"boom","time":{"start":1,"end":2}}"#,
        )
        .unwrap();
        match &errored {
            ToolState::Error { error, .. } => assert_eq!(error, "boom"),
            other => panic!("应为 error: {other:?}"),
        }

        // pending 态
        let pending: ToolState =
            serde_json::from_str(r#"{"status":"pending","input":{},"raw":""}"#).unwrap();
        assert!(matches!(pending, ToolState::Pending));

        // running 态（只有 input+time）
        let running: ToolState =
            serde_json::from_str(r#"{"status":"running","input":{},"time":{"start":1}}"#).unwrap();
        assert!(matches!(running, ToolState::Running { .. }));
    }

    #[test]
    fn parses_session_with_flat_and_nested_fields() {
        let s: OpencodeSession = serde_json::from_str(
            r#"{"id":"ses_1","slug":"s","projectID":"global","directory":"/repo","path":"repo",
                "cost":0,"tokens":{"input":10,"output":5,"reasoning":1,"cache":{"read":0,"write":0}},
                "title":"T","agent":"build","model":{"id":"space-bunny-free","providerID":"opencode"},
                "version":"1.18.32","time":{"created":1790831543346}}"#,
        )
        .unwrap();
        assert_eq!(s.id, "ses_1");
        assert_eq!(s.model.unwrap().id, "space-bunny-free");
        assert_eq!(s.tokens.unwrap().total(), 16);
        assert_eq!(s.time.unwrap().created_iso(), "2026-10-01T05:12:23Z");
    }

    /// part 多态：patch / step-start / file（未建模）→ Unknown/具体变体，均不 panic
    #[test]
    fn parses_part_polymorphism() {
        let patch: OpencodePart = serde_json::from_str(
            r#"{"id":"prt_1","sessionID":"ses","messageID":"msg","type":"patch","hash":"h","files":["/repo/a"]}"#,
        )
        .unwrap();
        assert!(matches!(patch, OpencodePart::Patch(_)));
        let step: OpencodePart = serde_json::from_str(
            r#"{"id":"prt_2","sessionID":"ses","messageID":"msg","type":"step-start","snapshot":"x"}"#,
        )
        .unwrap();
        assert!(matches!(step, OpencodePart::StepStart { .. }));
        let weird: OpencodePart = serde_json::from_str(
            r#"{"id":"prt_3","sessionID":"ses","messageID":"msg","type":"compaction","x":1}"#,
        )
        .unwrap();
        assert!(matches!(weird, OpencodePart::Unknown(_)));
    }

    #[test]
    fn rfc3339_known_instant() {
        // 1790831543346ms = 2026-10-01T05:12:23Z（与 fixture 实测同源）
        let t = OpencodeTime {
            created: 1790831543346.0,
            updated: None,
        };
        assert_eq!(t.created_iso(), "2026-10-01T05:12:23Z");
        assert_eq!(t.created_epoch(), Some(1790831543));
        assert_eq!(
            t.last_activity_epoch(),
            Some(1790831543),
            "updated 缺省回退 created"
        );
        let t2 = OpencodeTime {
            created: 1.0,
            updated: Some(2999.0),
        };
        assert_eq!(t2.last_activity_epoch(), Some(2), "updated 优先于 created");
    }

    #[test]
    fn prompt_body_serializes_flat_text_part() {
        let b = PromptBody::text("hi");
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["parts"][0]["type"], "text");
        assert_eq!(v["parts"][0]["text"], "hi");
    }
}

/// `GET /provider` 信封（实测 `{"all":[...]}`）。
#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeProviders {
    #[serde(default)]
    pub all: Vec<OpencodeProvider>,
}

/// `GET /provider` 单供应商（catalog；models 表扁平化归一给前端）。
#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeProvider {
    pub id: String,
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, OpencodeCatalogModel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpencodeCatalogModel {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// 供应商目录的免费标注（cost/free 字段存在性；实测 free 模型 name/cost 均带标注）
    #[serde(default)]
    pub cost: Option<serde_json::Value>,
    /// 思考档位：实测两形态——opencode 供应商为数组，其余为「档位名→配置」map
    ///（键即档位）。统一归一为数组；两态皆缺/空 = 无思考语义。
    #[serde(default, deserialize_with = "deserialize_variants")]
    pub variants: Option<Vec<String>>,
}

/// variants 双形态归一（缺省/空对象/空数组 → None）。
fn deserialize_variants<'de, D>(de: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v: Option<serde_json::Value> = serde::Deserialize::deserialize(de)?;
    Ok(match v {
        Some(serde_json::Value::Array(a)) => {
            let names: Vec<String> = a
                .into_iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
            (!names.is_empty()).then_some(names)
        }
        Some(serde_json::Value::Object(o)) => {
            let names: Vec<String> = o.keys().cloned().collect();
            (!names.is_empty()).then_some(names)
        }
        _ => None,
    })
}

impl OpencodeCatalogModel {
    /// 免费判定：cost 标注 free（实测 free 模型 `cost:{...,"free":true}` 或名字带 free）。
    pub fn is_free(&self) -> bool {
        if let Some(cost) = &self.cost {
            if cost.to_string().to_lowercase().contains("true") {
                return true;
            }
        }
        self.name.to_lowercase().contains("free") || self.id.to_lowercase().contains("free")
    }
}

/// `POST /api/session/{id}/model` body（v2 switchModel；ModelRef 形态 id+providerID）。
#[derive(Debug, serde::Serialize)]
pub struct SwitchModelBody<'a> {
    pub model: SwitchModelRef<'a>,
}

#[derive(Debug, serde::Serialize)]
pub struct SwitchModelRef<'a> {
    pub id: &'a str,
    #[serde(rename = "providerID")]
    pub provider_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<&'a str>,
}

#[cfg(test)]
mod model_tests {
    use super::*;

    /// 会话记录 model+variant 回读（v2 switchModel 后实测形态）
    #[test]
    fn parses_session_model_with_variant() {
        let s: OpencodeSession = serde_json::from_str(
            r#"{"id":"ses_1","directory":"/repo",
                "model":{"id":"space-bunny-free","providerID":"opencode"},
                "variant":"high","time":{"created":1}}"#,
        )
        .unwrap();
        let m = s.model.unwrap();
        assert_eq!(m.id, "space-bunny-free");
        assert_eq!(s.variant.as_deref(), Some("high"));
    }

    /// 供应商目录 → 免费判定与 variants 透传（/provider 实测形态）
    #[test]
    fn parses_provider_catalog_free_and_variants() {
        let raw = r#"{"id":"opencode","models":{
            "space-bunny-free":{"id":"space-bunny-free","name":"Space Bunny Free",
                "cost":null,"variants":["low","medium","high","xhigh","max"]},
            "big-pickle":{"id":"big-pickle","name":"Big Pickle","cost":null},
            "ling-free":{"id":"ling-free","name":"Ling Free",
                "cost":{"input":0,"output":0,"free":true},"variants":["low"]},
            "gpt-x":{"id":"gpt-x","name":"GPT X",
                "variants":{"none":{"reasoningEffort":"none"},"high":{"reasoningEffort":"high"}}}
        }}"#;
        let p: OpencodeProviders = serde_json::from_str(&format!(r#"{{"all":[{raw}]}}"#)).unwrap();
        let p = p.all.into_iter().next().unwrap();
        assert_eq!(p.models.len(), 4);
        let sb = p.models.get("space-bunny-free").unwrap();
        assert!(sb.is_free(), "名字带 free 判免费");
        assert_eq!(sb.variants.as_ref().unwrap().len(), 5);
        let bp = p.models.get("big-pickle").unwrap();
        assert!(!bp.is_free());
        assert!(bp.variants.is_none(), "无思考语义如实为空");
        let ling = p.models.get("ling-free").unwrap();
        assert!(ling.is_free(), "cost.free=true 判免费");
        // map 形态 variants：键归一为档位数组
        let gx = p.models.get("gpt-x").unwrap();
        assert_eq!(
            gx.variants.as_ref().unwrap(),
            &vec!["high".to_string(), "none".to_string()],
            "BTreeMap 键序"
        );
    }

    #[test]
    fn switch_model_body_serializes_model_ref() {
        let b = SwitchModelBody {
            model: SwitchModelRef {
                id: "space-bunny-free",
                provider_id: "opencode",
                variant: Some("high"),
            },
        };
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["model"]["providerID"], "opencode");
        assert_eq!(v["model"]["id"], "space-bunny-free");
        assert_eq!(v["model"]["variant"], "high");
    }
}
