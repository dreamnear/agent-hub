//! watcher 规则面（`~/.agent-hub/watcher.json`，ADR-0017 §6）：schema、加载、
//! 逐条校验降级、作用域/内容匹配、模板渲染。
//!
//! 降级纪律：文件级坏（缺文件 / JSON 语法错 / version≠1 / rules 非数组）→ 整体
//! 禁用（零注入）+ 一条 warn；单条规则坏 → 只跳过该条 + warn，其余规则照常。
//! 配置读取日志只含路径与错因，不含文件正文（密钥零回显纪律）。
//!
//! 本模块为纯函数 + 加载，无锁、无状态——单测主战场。

use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use serde::Deserialize;

pub const DEFAULT_STATE_POLL_SECS: u64 = 5;
pub const DEFAULT_MAX_WATCHED_SESSIONS: usize = 20;
pub const DEFAULT_COOLDOWN_SECS: u64 = 300;
pub const DEFAULT_RATE_MAX: usize = 10;
pub const DEFAULT_RATE_WINDOW_SECS: u64 = 300;
/// 消息正文匹配上限（超长 tool_result / 附件按前缀截断匹配）
/// shortcut: 超长正文只匹配前 20k 字符，需要全量时再放开
pub const MAX_MATCH_CHARS: usize = 20_000;
/// 单条提醒文案上限（含前缀，单行）
pub const MAX_TEXT_CHARS: usize = 500;

/// 合法消息类别（`kinds` 白名单；与 `ChatMessageKind::as_str` 同源取值）。
const KNOWN_KINDS: &[&str] = &[
    "user",
    "assistant",
    "tool_use",
    "tool_result",
    "thinking",
    "other",
];

/// 规则公共字段白名单（未知字段 → 跳过该条，防拼错字段静默失效）。
const BASE_KEYS: &[&str] = &["id", "on", "enabled", "cooldownSecs", "scope", "template"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub max: usize,
    pub window_secs: u64,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            max: DEFAULT_RATE_MAX,
            window_secs: DEFAULT_RATE_WINDOW_SECS,
        }
    }
}

/// 作用域：`sessions` 与 `cwdPrefix` 取并集；两者皆空 = 命中全部会话。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    pub sessions: Vec<String>,
    pub cwd_prefix: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuleCommon {
    pub id: String,
    pub enabled: bool,
    pub cooldown_secs: u64,
    pub scope: Option<Scope>,
    pub template: Option<String>,
}

#[derive(Debug, Clone)]
pub enum WatchRule {
    NeedsInput(RuleCommon),
    Message {
        common: RuleCommon,
        kinds: Vec<String>,
        keywords: Vec<String>,
        regex: Vec<Regex>,
    },
    NoOutput {
        common: RuleCommon,
        threshold_secs: u64,
    },
    ContextTokens {
        common: RuleCommon,
        threshold: u64,
    },
}

impl WatchRule {
    pub fn common(&self) -> &RuleCommon {
        match self {
            WatchRule::NeedsInput(c)
            | WatchRule::Message { common: c, .. }
            | WatchRule::NoOutput { common: c, .. }
            | WatchRule::ContextTokens { common: c, .. } => c,
        }
    }

    /// 需要消息通道（会话房间）才能评估的规则（claude/dsh 的 onMessageAppend
    /// 以房间存在为前提，ADR-0017 §2）。
    pub fn needs_message_stream(&self) -> bool {
        matches!(self, WatchRule::Message { .. } | WatchRule::NoOutput { .. })
    }

    /// 模板缺省时的文案（design §3.7）。
    pub fn default_template(&self) -> &'static str {
        match self {
            WatchRule::NeedsInput(_) => "会话 {session}（{driver} {cwd}）正在等待你的输入",
            WatchRule::Message { .. } => "会话 {session} 新消息命中 {matched}（规则 {rule}）",
            WatchRule::NoOutput { .. } => {
                "会话 {session} 已 {elapsed} 无新输出（阈值 {threshold}s，疑似卡死，规则 {rule}）"
            }
            WatchRule::ContextTokens { .. } => {
                "会话 {session} 上下文用量 {tokens} 超过阈值 {threshold}（规则 {rule}）"
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct WatchConfig {
    pub enabled: bool,
    pub state_poll_secs: u64,
    pub max_watched_sessions: usize,
    pub rate_limit: RateLimit,
    pub rules: Vec<WatchRule>,
}

impl WatchConfig {
    /// 禁用态（零注入）：加载失败 / 缺文件 / enabled=false。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            state_poll_secs: DEFAULT_STATE_POLL_SECS,
            max_watched_sessions: DEFAULT_MAX_WATCHED_SESSIONS,
            rate_limit: RateLimit::default(),
            rules: Vec::new(),
        }
    }

    /// 读盘加载（重启生效；不做热载）。任何文件级失败 → 禁用 + warn。
    pub async fn load(path: &Path) -> Self {
        let raw = match tokio::fs::read_to_string(path).await {
            Ok(raw) => raw,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), "watcher 配置缺失，watcher 禁用（零注入）");
                } else {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "watcher 配置读取失败，watcher 禁用（零注入）"
                    );
                }
                return Self::disabled();
            }
        };
        match Self::parse(&raw) {
            Ok(cfg) => {
                tracing::info!(
                    path = %path.display(),
                    enabled = cfg.enabled,
                    rules = cfg.rules.len(),
                    state_poll_secs = cfg.state_poll_secs,
                    "watcher 配置已加载"
                );
                cfg
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "watcher 配置无效，整体禁用（零注入）"
                );
                Self::disabled()
            }
        }
    }

    /// 纯解析 + 逐条校验（文件级错误 → Err；单条规则错误 → 跳过 + warn）。
    pub fn parse(raw: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| format!("JSON 解析失败: {e}"))?;
        let obj = value.as_object().ok_or("顶层必须是 JSON 对象")?;

        let version = obj
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or("缺少 version 字段")?;
        if version != 1 {
            return Err(format!("不支持的 version={version}（仅支持 1）"));
        }

        let enabled = obj
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let state_poll_secs = obj
            .get("statePollSecs")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(DEFAULT_STATE_POLL_SECS)
            .clamp(1, 60);
        let max_watched_sessions =
            obj.get("maxWatchedSessions")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(DEFAULT_MAX_WATCHED_SESSIONS as u64) as usize;
        let rate_limit = match obj.get("rateLimit") {
            Some(v) => {
                let w: WireRateLimit = serde_json::from_value(v.clone())
                    .map_err(|e| format!("rateLimit 非法: {e}"))?;
                RateLimit {
                    max: w.max,
                    window_secs: w.window_secs,
                }
            }
            None => RateLimit::default(),
        };
        let rule_values = match obj.get("rules") {
            None => Vec::new(),
            Some(serde_json::Value::Array(a)) => a.clone(),
            Some(_) => return Err("rules 必须是数组".into()),
        };

        let mut rules = Vec::with_capacity(rule_values.len());
        let mut ids: HashSet<String> = HashSet::new();
        for (index, rv) in rule_values.iter().enumerate() {
            match parse_rule(rv) {
                Ok(rule) => {
                    let id = rule.common().id.clone();
                    if !ids.insert(id.clone()) {
                        tracing::warn!(index, rule = %id, "watcher 规则 id 重复，跳过后者");
                        continue;
                    }
                    rules.push(rule);
                }
                Err(e) => tracing::warn!(index, error = %e, "watcher 规则非法，跳过该条"),
            }
        }

        Ok(Self {
            enabled,
            state_poll_secs,
            max_watched_sessions,
            rate_limit,
            rules,
        })
    }
}

// —— wire 形态（serde 反序列化中间层；camelCase 字段名）——

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireRateLimit {
    #[serde(default = "default_rate_max")]
    max: usize,
    #[serde(default = "default_rate_window_secs")]
    window_secs: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireScope {
    #[serde(default)]
    sessions: Vec<String>,
    #[serde(default)]
    cwd_prefix: Vec<String>,
}

/// 公共字段（`on` 与变体专属字段按 serde 默认忽略；未知字段由白名单先行拦截）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireCommon {
    id: String,
    #[serde(default = "bool_true")]
    enabled: bool,
    #[serde(default = "default_cooldown_secs")]
    cooldown_secs: u64,
    #[serde(default)]
    scope: Option<WireScope>,
    #[serde(default)]
    template: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireMessage {
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireNoOutput {
    threshold_secs: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireContextTokens {
    threshold: u64,
}

fn bool_true() -> bool {
    true
}

fn default_cooldown_secs() -> u64 {
    DEFAULT_COOLDOWN_SECS
}

fn default_rate_max() -> usize {
    DEFAULT_RATE_MAX
}

fn default_rate_window_secs() -> u64 {
    DEFAULT_RATE_WINDOW_SECS
}

fn parse_rule(value: &serde_json::Value) -> Result<WatchRule, String> {
    let obj = value.as_object().ok_or("规则必须是 JSON 对象")?;
    let on = obj
        .get("on")
        .and_then(serde_json::Value::as_str)
        .ok_or("缺少 on 字段")?;
    let variant_keys: &[&str] = match on {
        "needs_input" => &[],
        "message" => &["kinds", "keywords", "regex"],
        "no_output" => &["thresholdSecs"],
        "context_tokens" => &["threshold"],
        other => return Err(format!("未知 on 值 {other}")),
    };
    // 未知字段 → 跳过该条（防拼错字段静默失效）
    if let Some(unknown) = obj
        .keys()
        .find(|k| !BASE_KEYS.contains(&k.as_str()) && !variant_keys.contains(&k.as_str()))
    {
        return Err(format!("未知字段 {unknown}"));
    }

    let wire: WireCommon =
        serde_json::from_value(value.clone()).map_err(|e| format!("公共字段非法: {e}"))?;
    if wire.id.trim().is_empty() {
        return Err("id 为空".into());
    }
    let common = RuleCommon {
        id: wire.id,
        enabled: wire.enabled,
        cooldown_secs: wire.cooldown_secs,
        scope: wire.scope.map(|s| Scope {
            sessions: s.sessions,
            cwd_prefix: s.cwd_prefix,
        }),
        template: wire.template,
    };

    match on {
        "needs_input" => Ok(WatchRule::NeedsInput(common)),
        "message" => {
            let w: WireMessage = serde_json::from_value(value.clone())
                .map_err(|e| format!("message 字段非法: {e}"))?;
            let mut kinds: Vec<String> = w
                .kinds
                .into_iter()
                .filter(|k| {
                    if KNOWN_KINDS.contains(&k.as_str()) {
                        true
                    } else {
                        tracing::warn!(rule = %common.id, kind = %k, "未知 kinds 值，忽略");
                        false
                    }
                })
                .collect();
            if kinds.is_empty() {
                kinds = vec!["assistant".to_string()];
            }
            let mut regex = Vec::new();
            for raw in w.regex {
                match Regex::new(&raw) {
                    Ok(re) => regex.push(re),
                    Err(e) => tracing::warn!(
                        rule = %common.id,
                        error = %e,
                        "正则编译失败，忽略该条正则（保留 keywords）"
                    ),
                }
            }
            let keywords: Vec<String> = w.keywords.into_iter().filter(|k| !k.is_empty()).collect();
            if keywords.is_empty() && regex.is_empty() {
                return Err("message 规则既无有效 keywords 也无有效 regex".into());
            }
            Ok(WatchRule::Message {
                common,
                kinds,
                keywords,
                regex,
            })
        }
        "no_output" => {
            let w: WireNoOutput = serde_json::from_value(value.clone())
                .map_err(|e| format!("no_output 字段非法: {e}"))?;
            if w.threshold_secs == 0 {
                return Err("thresholdSecs 必须 > 0".into());
            }
            Ok(WatchRule::NoOutput {
                common,
                threshold_secs: w.threshold_secs,
            })
        }
        "context_tokens" => {
            let w: WireContextTokens = serde_json::from_value(value.clone())
                .map_err(|e| format!("context_tokens 字段非法: {e}"))?;
            if w.threshold == 0 {
                return Err("threshold 必须 > 0".into());
            }
            Ok(WatchRule::ContextTokens {
                common,
                threshold: w.threshold,
            })
        }
        _ => unreachable!("on 值已在上面校验"),
    }
}

// —— 匹配（纯函数）——

/// 匹配用只读会话视图。
pub struct SessionMeta<'a> {
    pub driver: &'a str,
    pub id: &'a str,
    pub session_id: Option<&'a str>,
    pub name: Option<&'a str>,
    pub cwd: Option<&'a str>,
}

/// 作用域判定：`sessions` 与 `cwdPrefix` 取并集；缺省/两者皆空 = 全部命中。
pub fn scope_match(scope: &Option<Scope>, meta: &SessionMeta<'_>) -> bool {
    let Some(scope) = scope else {
        return true;
    };
    if scope.sessions.is_empty() && scope.cwd_prefix.is_empty() {
        return true;
    }
    let session_hit = scope.sessions.iter().any(|cand| {
        cand == meta.id
            || meta.session_id == Some(cand.as_str())
            || *cand == format!("{}:{}", meta.driver, meta.id)
    });
    let cwd_hit = meta.cwd.is_some_and(|cwd| {
        scope
            .cwd_prefix
            .iter()
            .any(|p| !p.is_empty() && cwd.starts_with(p.as_str()))
    });
    session_hit || cwd_hit
}

/// 内容命中的标识（`stateKey` 与 `{matched}` 占位符来源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchHit {
    Keyword(String),
    Regex(String),
}

impl MatchHit {
    /// 冷却/去重键的「状态」部分：同一消息命中不同关键词视为不同状态。
    pub fn state_key(&self) -> String {
        match self {
            MatchHit::Keyword(k) => format!("kw:{k}"),
            MatchHit::Regex(r) => format!("re:{r}"),
        }
    }

    pub fn display(&self) -> &str {
        match self {
            MatchHit::Keyword(k) => k,
            MatchHit::Regex(r) => r,
        }
    }
}

/// 内容命中：kind 过滤 → 大小写不敏感关键词 → 正则；同一消息对同一规则只取
/// 首个命中（keywords 优先，保持确定性）。
pub fn match_message(
    kinds: &[String],
    keywords: &[String],
    regex: &[Regex],
    kind: &str,
    text: &str,
) -> Option<MatchHit> {
    if !kinds.iter().any(|k| k == kind) {
        return None;
    }
    let clipped: String = text.chars().take(MAX_MATCH_CHARS).collect();
    if clipped.trim().is_empty() {
        return None;
    }
    let lower = clipped.to_lowercase();
    for kw in keywords {
        if lower.contains(&kw.to_lowercase()) {
            return Some(MatchHit::Keyword(kw.clone()));
        }
    }
    for re in regex {
        if re.is_match(&clipped) {
            return Some(MatchHit::Regex(re.as_str().to_string()));
        }
    }
    None
}

// —— 渲染（纯函数）——

/// 模板渲染上下文（`{session} {id} {driver} {cwd}` 等占位符取值）。
pub struct TemplateCtx<'a> {
    pub session: &'a str,
    pub id: &'a str,
    pub driver: &'a str,
    pub cwd: Option<&'a str>,
    pub elapsed_secs: Option<u64>,
    pub threshold: Option<u64>,
    pub tokens: Option<u64>,
}

/// 模板渲染：未知占位符原样保留；输出单行、≤ [`MAX_TEXT_CHARS`] 字符。
/// 前缀 `[hub-watcher] ` 由调用方追加（计入同一 500 字符上限）。
pub fn render(
    template: Option<&str>,
    default_template: &str,
    rule_id: &str,
    hit: Option<&MatchHit>,
    ctx: &TemplateCtx<'_>,
) -> String {
    let tpl = template.unwrap_or(default_template);
    let pairs: [(&str, String); 9] = [
        ("session", ctx.session.to_string()),
        ("id", ctx.id.to_string()),
        ("driver", ctx.driver.to_string()),
        ("cwd", ctx.cwd.unwrap_or("-").to_string()),
        ("rule", rule_id.to_string()),
        (
            "matched",
            hit.map(|h| h.display().to_string()).unwrap_or_default(),
        ),
        (
            "elapsed",
            ctx.elapsed_secs.map(fmt_elapsed).unwrap_or_default(),
        ),
        (
            "threshold",
            ctx.threshold.map(|v| v.to_string()).unwrap_or_default(),
        ),
        (
            "tokens",
            ctx.tokens.map(|v| v.to_string()).unwrap_or_default(),
        ),
    ];
    let rendered = substitute(tpl, &pairs);
    let single_line = rendered.replace(['\n', '\r'], " ");
    single_line.chars().take(MAX_TEXT_CHARS).collect()
}

/// 单趟扫描替换 `{name}`，避免值中出现的占位符样式被二次替换。
fn substitute(tpl: &str, pairs: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match pairs.iter().find(|(k, _)| *k == name) {
                    Some((_, v)) => out.push_str(v),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn fmt_elapsed(secs: u64) -> String {
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_rules(raw: &str) -> WatchConfig {
        WatchConfig::parse(raw).expect("配置应可解析")
    }

    fn ids(cfg: &WatchConfig) -> Vec<&str> {
        cfg.rules.iter().map(|r| r.common().id.as_str()).collect()
    }

    fn meta<'a>(id: &'a str, session_id: Option<&'a str>, cwd: Option<&'a str>) -> SessionMeta<'a> {
        SessionMeta {
            driver: "claude",
            id,
            session_id,
            name: None,
            cwd,
        }
    }

    // —— 第 1 组：解析与降级 ——

    #[test]
    fn parses_full_valid_config() {
        let cfg = cfg_rules(
            r#"{
              "version": 1,
              "enabled": true,
              "statePollSecs": 3,
              "maxWatchedSessions": 5,
              "rateLimit": { "max": 4, "windowSecs": 120 },
              "rules": [
                { "id": "a", "on": "needs_input", "template": "hi {session}" },
                { "id": "b", "on": "message", "keywords": ["ERROR"], "regex": ["(?i)build failed"] },
                { "id": "c", "on": "no_output", "thresholdSecs": 60 },
                { "id": "d", "on": "context_tokens", "threshold": 100 }
              ]
            }"#,
        );
        assert!(cfg.enabled);
        assert_eq!(cfg.state_poll_secs, 3);
        assert_eq!(cfg.max_watched_sessions, 5);
        assert_eq!(
            cfg.rate_limit,
            RateLimit {
                max: 4,
                window_secs: 120
            }
        );
        assert_eq!(ids(&cfg), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn defaults_apply_when_fields_missing() {
        let cfg = cfg_rules(r#"{ "version": 1, "rules": [{ "id": "a", "on": "needs_input" }] }"#);
        assert!(cfg.enabled, "enabled 缺省为 true");
        assert_eq!(cfg.state_poll_secs, DEFAULT_STATE_POLL_SECS);
        assert_eq!(cfg.max_watched_sessions, DEFAULT_MAX_WATCHED_SESSIONS);
        assert_eq!(cfg.rate_limit, RateLimit::default());
        let common = cfg.rules[0].common();
        assert!(common.enabled, "规则 enabled 缺省 true");
        assert_eq!(common.cooldown_secs, DEFAULT_COOLDOWN_SECS);
    }

    #[test]
    fn state_poll_secs_clamped() {
        assert_eq!(
            cfg_rules(r#"{ "version": 1, "statePollSecs": 0, "rules": [] }"#).state_poll_secs,
            1
        );
        assert_eq!(
            cfg_rules(r#"{ "version": 1, "statePollSecs": 999, "rules": [] }"#).state_poll_secs,
            60
        );
    }

    #[test]
    fn rejects_whole_file_on_bad_version_or_shape() {
        assert!(WatchConfig::parse("not json").is_err());
        assert!(WatchConfig::parse(r#"{ "version": 2, "rules": [] }"#).is_err());
        assert!(
            WatchConfig::parse(r#"{ "rules": [] }"#).is_err(),
            "version 必填"
        );
        assert!(
            WatchConfig::parse(r#"{ "version": 1, "rules": {} }"#).is_err(),
            "rules 非数组 → 整体禁用"
        );
    }

    #[test]
    fn skips_unknown_on_value() {
        let cfg = cfg_rules(
            r#"{ "version": 1, "rules": [
                { "id": "bad", "on": "whenever" },
                { "id": "ok", "on": "needs_input" }
            ] }"#,
        );
        assert_eq!(ids(&cfg), vec!["ok"]);
    }

    #[test]
    fn skips_duplicate_ids() {
        let cfg = cfg_rules(
            r#"{ "version": 1, "rules": [
                { "id": "dup", "on": "needs_input" },
                { "id": "dup", "on": "no_output", "thresholdSecs": 10 }
            ] }"#,
        );
        assert_eq!(ids(&cfg), vec!["dup"]);
        assert!(
            matches!(cfg.rules[0], WatchRule::NeedsInput(_)),
            "后者被跳过"
        );
    }

    #[test]
    fn bad_regex_is_dropped_but_keywords_kept() {
        let cfg = cfg_rules(
            r#"{ "version": 1, "rules": [
                { "id": "m", "on": "message", "keywords": ["boom"], "regex": ["("] }
            ] }"#,
        );
        match &cfg.rules[0] {
            WatchRule::Message {
                keywords, regex, ..
            } => {
                assert_eq!(keywords, &vec!["boom".to_string()]);
                assert!(regex.is_empty());
            }
            other => panic!("应为 message 规则: {other:?}"),
        }
    }

    #[test]
    fn message_rule_needs_keywords_or_regex() {
        let cfg = cfg_rules(
            r#"{ "version": 1, "rules": [
                { "id": "empty", "on": "message" },
                { "id": "onlybadregex", "on": "message", "regex": ["("] }
            ] }"#,
        );
        assert!(cfg.rules.is_empty(), "无有效 keywords/regex 的规则被跳过");
    }

    #[test]
    fn skips_rule_with_unknown_field() {
        let cfg = cfg_rules(
            r#"{ "version": 1, "rules": [
                { "id": "typo", "on": "no_output", "thresholdSecond": 10 },
                { "id": "ok", "on": "no_output", "thresholdSecs": 10 }
            ] }"#,
        );
        assert_eq!(ids(&cfg), vec!["ok"]);
    }

    #[test]
    fn rejects_nonpositive_thresholds() {
        let cfg = cfg_rules(
            r#"{ "version": 1, "rules": [
                { "id": "n", "on": "no_output", "thresholdSecs": 0 },
                { "id": "c", "on": "context_tokens", "threshold": 0 },
                { "id": "ok", "on": "needs_input" }
            ] }"#,
        );
        assert_eq!(ids(&cfg), vec!["ok"]);
    }

    #[test]
    fn message_kinds_default_and_unknown_filtered() {
        let cfg = cfg_rules(
            r#"{ "version": 1, "rules": [
                { "id": "default", "on": "message", "keywords": ["x"] },
                { "id": "filtered", "on": "message", "keywords": ["x"], "kinds": ["tool_use", "bogus"] }
            ] }"#,
        );
        match &cfg.rules[0] {
            WatchRule::Message { kinds, .. } => assert_eq!(kinds, &vec!["assistant".to_string()]),
            other => panic!("{other:?}"),
        }
        match &cfg.rules[1] {
            WatchRule::Message { kinds, .. } => assert_eq!(kinds, &vec!["tool_use".to_string()]),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_rules_disables_injection_but_keeps_enabled_flag() {
        let cfg = cfg_rules(r#"{ "version": 1, "rules": [] }"#);
        assert!(cfg.enabled);
        assert!(cfg.rules.is_empty());
    }

    // —— 第 2 组：作用域 ——

    #[test]
    fn default_scope_matches_all() {
        let m = meta("agent-1", Some("sess-1"), Some("/Users/x/proj"));
        assert!(scope_match(&None, &m));
        let empty = Some(Scope::default());
        assert!(scope_match(&empty, &m));
    }

    #[test]
    fn scope_sessions_matches_agent_id_session_id_and_driver_id() {
        let m = meta("agent-1", Some("sess-1"), None);
        for cand in ["agent-1", "sess-1", "claude:agent-1"] {
            let scope = Some(Scope {
                sessions: vec![cand.to_string()],
                cwd_prefix: vec![],
            });
            assert!(scope_match(&scope, &m), "{cand} 应命中");
        }
        let miss = Some(Scope {
            sessions: vec!["other".into()],
            cwd_prefix: vec![],
        });
        assert!(!scope_match(&miss, &m));
    }

    #[test]
    fn scope_cwd_prefix_and_union() {
        let m = meta("agent-1", None, Some("/Users/x/proj/sub"));
        let by_cwd = Some(Scope {
            sessions: vec![],
            cwd_prefix: vec!["/Users/x/proj".into()],
        });
        assert!(scope_match(&by_cwd, &m));
        let union = Some(Scope {
            sessions: vec!["agent-1".into()],
            cwd_prefix: vec!["/elsewhere".into()],
        });
        assert!(scope_match(&union, &m), "并集：会话命中即可");
        let miss = Some(Scope {
            sessions: vec![],
            cwd_prefix: vec!["/elsewhere".into()],
        });
        assert!(!scope_match(&miss, &m));
    }

    #[test]
    fn scope_cwd_missing_never_matches_prefix() {
        let m = meta("agent-1", None, None);
        let scope = Some(Scope {
            sessions: vec![],
            cwd_prefix: vec!["/Users".into()],
        });
        assert!(!scope_match(&scope, &m));
    }

    // —— 第 3 组：内容匹配 ——

    fn regexes(patterns: &[&str]) -> Vec<Regex> {
        patterns.iter().map(|p| Regex::new(p).unwrap()).collect()
    }

    #[test]
    fn match_keyword_case_insensitive() {
        let kinds = vec!["assistant".to_string()];
        let keywords = vec!["Error".to_string()];
        assert_eq!(
            match_message(&kinds, &keywords, &[], "assistant", "an ERROR happened"),
            Some(MatchHit::Keyword("Error".into()))
        );
        assert_eq!(
            match_message(&kinds, &keywords, &[], "assistant", "all good"),
            None
        );
    }

    #[test]
    fn match_regex() {
        let kinds = vec!["assistant".to_string()];
        let hit = match_message(
            &kinds,
            &[],
            &regexes(&["(?i)build failed"]),
            "assistant",
            "BUILD FAILED at step 3",
        );
        assert_eq!(hit, Some(MatchHit::Regex("(?i)build failed".into())));
    }

    #[test]
    fn match_respects_kind_filter() {
        let kinds = vec!["assistant".to_string()];
        let keywords = vec!["x".to_string()];
        assert_eq!(match_message(&kinds, &keywords, &[], "user", "x"), None);
        assert_eq!(match_message(&[], &keywords, &[], "assistant", "x"), None);
    }

    #[test]
    fn match_empty_text_never_hits() {
        let kinds = vec!["assistant".to_string()];
        assert_eq!(
            match_message(&kinds, &["".into()], &regexes(&[".*"]), "assistant", "   "),
            None
        );
    }

    #[test]
    fn match_keyword_wins_over_regex_and_is_deterministic() {
        let kinds = vec!["assistant".to_string()];
        let hit = match_message(
            &kinds,
            &["first".into(), "second".into()],
            &regexes(&["first"]),
            "assistant",
            "first second",
        );
        assert_eq!(hit, Some(MatchHit::Keyword("first".into())));
    }

    #[test]
    fn match_clips_long_body() {
        let kinds = vec!["assistant".to_string()];
        let mut text = "a".repeat(MAX_MATCH_CHARS);
        text.push_str("NEEDLE");
        assert_eq!(
            match_message(&kinds, &["NEEDLE".into()], &[], "assistant", &text),
            None
        );
    }

    // —— 第 4 组：渲染 ——

    fn ctx<'a>(session: &'a str, cwd: Option<&'a str>) -> TemplateCtx<'a> {
        TemplateCtx {
            session,
            id: "agent-1",
            driver: "claude",
            cwd,
            elapsed_secs: Some(900),
            threshold: Some(600),
            tokens: Some(150_000),
        }
    }

    #[test]
    fn render_default_templates() {
        let c = ctx("demo", Some("/w"));
        let hit = MatchHit::Keyword("ERROR".into());
        assert_eq!(
            render(
                None,
                "会话 {session}（{driver} {cwd}）正在等待你的输入",
                "r",
                None,
                &c
            ),
            "会话 demo（claude /w）正在等待你的输入"
        );
        assert_eq!(
            render(None, "命中 {matched}", "r", Some(&hit), &c),
            "命中 ERROR"
        );
        assert_eq!(
            render(
                None,
                "已 {elapsed} 无输出，阈值 {threshold}s",
                "r",
                None,
                &c
            ),
            "已 15m 无输出，阈值 600s"
        );
        assert_eq!(
            render(None, "{id} {driver} {rule} {tokens}", "rule-7", None, &c),
            "agent-1 claude rule-7 150000"
        );
    }

    #[test]
    fn render_keeps_unknown_placeholders() {
        let c = ctx("demo", None);
        assert_eq!(
            render(Some("{session} {unknown}"), "x", "r", None, &c),
            "demo {unknown}"
        );
    }

    #[test]
    fn render_truncates_to_limit_and_stays_single_line() {
        let long = "x".repeat(MAX_TEXT_CHARS + 100);
        let out = render(Some(&long), "x", "r", None, &ctx("s", None));
        assert_eq!(out.chars().count(), MAX_TEXT_CHARS);
        let out = render(Some("a\nb"), "x", "r", None, &ctx("s", None));
        assert_eq!(out, "a b");
    }
}
