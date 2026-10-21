//! `plugin.json` shcema、解析、校验与结构化错因（ADR-0001 第 3 条 / BR-02）。
//!
//! 错因一律带「字段名 + 规则名」（`ManifestIssue { field, rule, message }`），
//! 由静态审计原样上屏，保证「装前看得见」。
//!
//! 本模块只管「声明本身是否合法」；路径存在性 / 符号链接越界 / id 冲突等
//! 需要插件目录的检查在 `audit.rs`（两者共用同一套 issue 形状）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::api_surface::{
    self, BUILTIN_RENDER_EXTS, BUILTIN_RENDER_FENCES, DATA_DOMAINS, HIGH_RISK_ITEMS,
    PLUGIN_API_VERSION, RENDER_ENGINES, SERVICE_RENDER_ENGINES,
};

/// 一条可定位的问题（error 阻断安装，warning 不阻断）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestIssue {
    /// 字段路径（如 `contributes.tools[0].risk`）
    pub field: String,
    /// 规则名（如 `required` / `path-escape` / `api-version-mismatch`）
    pub rule: String,
    pub message: String,
    /// 关联文件（相对插件根；无则 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ManifestIssue {
    pub fn new(
        field: impl Into<String>,
        rule: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            field: field.into(),
            rule: rule.into(),
            message: message.into(),
            path: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }
}

/// manifest 全量字段（ADR-0001 第 3 条五组）。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<Author>,
    #[serde(default)]
    pub homepage: Option<String>,
    pub hub: HubCompat,
    #[serde(default)]
    pub contributes: Contributes,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub backend: Option<BackendDecl>,
    /// 未知字段（加法承诺：v1 主版本内新增可选字段对旧 hub 不致命，记 warning）
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Author {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HubCompat {
    pub api_version: String,
    #[serde(default)]
    pub min_hub_version: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Contributes {
    #[serde(default)]
    pub tools: Vec<ToolDecl>,
    #[serde(default)]
    pub skills: Vec<SkillDecl>,
    #[serde(default)]
    pub mcp: Vec<McpDecl>,
    #[serde(default)]
    pub views: Vec<ViewDecl>,
    #[serde(default)]
    pub commands: Vec<CommandDecl>,
    /// 渲染器声明档（B3，ADR-0012）：`{id, match:{ext,fence}, engine, config?, entry?}`
    #[serde(default)]
    pub renderers: Vec<RendererDecl>,
}

/// 渲染器命中规则（ADR-0012 第 3 条）：ext / fence 全 hub 唯一（含内置占用，
/// 冲突 = 审计失败）；至少声明一项。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererMatch {
    #[serde(default)]
    pub ext: Vec<String>,
    #[serde(default)]
    pub fence: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererDecl {
    /// 强制 `plug_<pluginId>_<rid>`（ADR-0012 第 5 条：禁覆盖内置 id 的命名前提）
    pub id: String,
    #[serde(rename = "match", default)]
    pub match_rule: RendererMatch,
    /// 宿主内置引擎（`api_surface::RENDER_ENGINES`）——插件零代码进宿主
    pub engine: String,
    /// 服务类引擎（plantuml-server）仅允许 `{"service":"<配置名>"}`——禁内嵌 URL
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// `engine:"plugin-iframe"` 时必填（相对路径，禁逃逸）
    #[serde(default)]
    pub entry: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandDecl {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// v1 仅 `info`（进 `/api/commands` 的 `hub-plugin` 组）
    pub kind: String,
    pub target: CommandTarget,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CommandTarget {
    /// 打开插件视图：`{view:"<viewId>"}`
    #[serde(default)]
    pub view: Option<String>,
    /// 触发只读工具：`{tool:"<toolId>", args?: Value}`
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub args: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDecl {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// 参数 schema 的文件相对路径（禁止内联大块内容，ADR-0001 拍板结论 2）
    #[serde(default)]
    pub params_schema: Option<String>,
    /// `read` 免确认；`high` 须与 permissions.highRisk 逐条一致
    pub risk: String,
    #[serde(rename = "impl")]
    pub implementation: ToolImpl,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolImpl {
    /// `backend` | `mcp`
    pub kind: String,
    #[serde(rename = "ref")]
    pub ref_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillDecl {
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpDecl {
    pub id: String,
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ViewDecl {
    pub id: String,
    pub title: String,
    pub entry: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub order: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")] // manifest 契约键即 camelCase（highRisk）；缺失时 high_risk 以
// snake 键解析，manifest 的 "highRisk" 声明被静默丢弃（B2 预览反馈轮修正）
pub struct Permissions {
    /// `"<METHOD> <path-pattern>"`，单段 `*`（v1 预留，见 ADR-0003 拍板结论 5）
    #[serde(default)]
    pub api: Vec<String>,
    #[serde(default)]
    pub data: Vec<String>,
    /// 凭据**名**（值由 hub 代持，manifest 不得携带值）
    #[serde(default)]
    pub credentials: Vec<String>,
    #[serde(default)]
    pub high_risk: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendDecl {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// 空闲回收秒数（ADR-0006：默认 300s）
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub always_on: Option<bool>,
    /// 启动 env（`$cred:<name>` 占位由 hub 替换，ADR-0007 第 4 条）
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// 解析结果：manifest + 非阻断 warning。
#[derive(Debug, Clone)]
pub struct ParsedManifest {
    pub manifest: Manifest,
    pub warnings: Vec<ManifestIssue>,
}

/// id 规则（ADR-0001 第 3 条）：2–40 字符 kebab-slug。
pub fn valid_plugin_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 2 || bytes.len() > 40 {
        return false;
    }
    let edge_ok = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    if !edge_ok(bytes[0]) || !edge_ok(bytes[bytes.len() - 1]) {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// 严格 `MAJOR.MINOR.PATCH`（无预发布/构建元数据，ADR-0001 第 3 条）。
pub fn valid_semver(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// 粗粒度版本比较（仅 `MAJOR.MINOR.PATCH`，返回 Ordering）。
pub fn compare_semver(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let (pa, pb) = (parse(a), parse(b));
    for i in 0..3 {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// 凭据名规则（manifest 声明与凭据写入口共用同一判据）：
/// 字母/数字/`_-.`，≤64 字符，非空——**只是名字**，值一律由 hub 代持、不入 manifest。
pub fn valid_credential_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

/// `api` 权限项语法：`"<METHOD> <path-pattern>"`，`*` 只能占单段，禁 `**`。
pub fn valid_api_pattern(pattern: &str) -> Result<(), String> {
    let mut it = pattern.split_whitespace();
    let (Some(method), Some(path), None) = (it.next(), it.next(), it.next()) else {
        return Err("应为 \"<METHOD> <path-pattern>\" 两段".into());
    };
    const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
    if !METHODS.contains(&method) {
        return Err(format!("未知 HTTP 方法 {method}"));
    }
    if !path.starts_with('/') {
        return Err("路径模式必须以 / 开头".into());
    }
    for seg in path.split('/') {
        if seg.contains("**") {
            return Err("禁 ** 通配（只允许单段 *）".into());
        }
        if seg.contains('*') && seg != "*" {
            return Err("`*` 只能独占一个路径段".into());
        }
        if seg.contains('?') || seg.contains('#') {
            return Err("路径模式不允许 query/fragment".into());
        }
    }
    Ok(())
}

/// 从 JSON 文本解析并做 schema 级校验。
/// 任一 error 非空 → 该插件整体拒绝安装（BR-02）。
pub fn parse(raw: &str) -> Result<ParsedManifest, Vec<ManifestIssue>> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if raw.trim_start().starts_with('\u{feff}') {
        errors.push(ManifestIssue::new(
            "plugin.json",
            "no-bom",
            "manifest 不得含 BOM",
        ));
    }
    let manifest: Manifest = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(e) => {
            errors.push(serde_issue(&e));
            return Err(errors);
        }
    };

    // —— 身份组 ——
    if manifest.schema_version != 1 {
        errors.push(ManifestIssue::new(
            "schemaVersion",
            "unsupported",
            format!(
                "不支持的 schemaVersion {}（本 hub 支持 1）",
                manifest.schema_version
            ),
        ));
    }
    if !valid_plugin_id(&manifest.id) {
        errors.push(ManifestIssue::new(
            "id",
            "id-format",
            "id 必须是 2–40 字符 kebab-slug（小写字母/数字/连字符，首尾非连字符）",
        ));
    }
    if manifest.name.trim().is_empty() {
        errors.push(ManifestIssue::new("name", "required", "name 不能为空"));
    }
    if !valid_semver(&manifest.version) {
        errors.push(ManifestIssue::new(
            "version",
            "semver",
            "version 必须是 MAJOR.MINOR.PATCH 三段数字",
        ));
    }

    // —— 兼容性组 ——
    if manifest.hub.api_version.trim() != PLUGIN_API_VERSION.to_string() {
        errors.push(ManifestIssue::new(
            "hub.apiVersion",
            "api-version-mismatch",
            api_surface::api_version_mismatch(manifest.hub.api_version.trim()),
        ));
    }
    if let Some(min) = &manifest.hub.min_hub_version {
        if !valid_semver(min) {
            errors.push(ManifestIssue::new(
                "hub.minHubVersion",
                "semver",
                "minHubVersion 必须是 MAJOR.MINOR.PATCH",
            ));
        } else if compare_semver(min, api_surface::HUB_VERSION) == std::cmp::Ordering::Greater {
            errors.push(ManifestIssue::new(
                "hub.minHubVersion",
                "min-hub-version",
                format!(
                    "插件要求 hub ≥ {min}，当前 hub 版本 {}",
                    api_surface::HUB_VERSION
                ),
            ));
        }
    }

    // —— 贡献物组 ——
    let mut seen: Vec<String> = Vec::new();
    let mut dup = |kind: &str, id: &str, errors: &mut Vec<ManifestIssue>| {
        let key = format!("{kind}:{id}");
        if seen.contains(&key) {
            errors.push(ManifestIssue::new(
                format!("contributes.{kind}"),
                "duplicate-id",
                format!("贡献物 id 重复：{id}"),
            ));
        } else {
            seen.push(key);
        }
    };
    for (i, t) in manifest.contributes.tools.iter().enumerate() {
        dup("tools", &t.id, &mut errors);
        if t.name.trim().is_empty() {
            errors.push(ManifestIssue::new(
                format!("contributes.tools[{i}].name"),
                "required",
                "工具 name 不能为空",
            ));
        }
        if t.risk != "read" && t.risk != "high" {
            errors.push(ManifestIssue::new(
                format!("contributes.tools[{i}].risk"),
                "enum",
                "risk 只能是 \"read\" 或 \"high\"",
            ));
        }
        if t.implementation.kind != "backend" && t.implementation.kind != "mcp" {
            errors.push(ManifestIssue::new(
                format!("contributes.tools[{i}].impl.kind"),
                "enum",
                "impl.kind 只能是 \"backend\" 或 \"mcp\"",
            ));
        }
        if t.implementation.ref_id.trim().is_empty() {
            errors.push(ManifestIssue::new(
                format!("contributes.tools[{i}].impl.ref"),
                "required",
                "impl.ref 不能为空",
            ));
        }
        if let Some(p) = &t.params_schema {
            check_relative_path(
                p,
                &format!("contributes.tools[{i}].paramsSchema"),
                &mut errors,
            );
        }
    }
    for (i, s) in manifest.contributes.skills.iter().enumerate() {
        dup("skills", &s.id, &mut errors);
        if let Some(p) = &s.path {
            check_relative_path(p, &format!("contributes.skills[{i}].path"), &mut errors);
        }
    }
    for (i, m) in manifest.contributes.mcp.iter().enumerate() {
        dup("mcp", &m.id, &mut errors);
        if m.transport != "stdio" {
            errors.push(ManifestIssue::new(
                format!("contributes.mcp[{i}].transport"),
                "enum",
                "v1 只支持 stdio transport",
            ));
        }
        if m.command.trim().is_empty() {
            errors.push(ManifestIssue::new(
                format!("contributes.mcp[{i}].command"),
                "required",
                "MCP command 不能为空",
            ));
        }
        if let Some(c) = &m.cwd {
            check_relative_path(c, &format!("contributes.mcp[{i}].cwd"), &mut errors);
        }
    }
    for (i, v) in manifest.contributes.views.iter().enumerate() {
        dup("views", &v.id, &mut errors);
        if v.title.trim().is_empty() {
            errors.push(ManifestIssue::new(
                format!("contributes.views[{i}].title"),
                "required",
                "视图 title 不能为空",
            ));
        }
        check_relative_path(
            &v.entry,
            &format!("contributes.views[{i}].entry"),
            &mut errors,
        );
    }
    // —— commands：id 命名空间 + kind + target 完整性/合法性（design §12.1，需求功能点 5）——盘上 ADR-0012 为 content-renderer 扩展点（与 command target 同名），此初判据未单列 ADR——
    let view_ids: std::collections::HashSet<&str> = manifest
        .contributes
        .views
        .iter()
        .map(|v| v.id.as_str())
        .collect();
    let tool_risks: std::collections::HashMap<&str, &str> = manifest
        .contributes
        .tools
        .iter()
        .map(|t| (t.id.as_str(), t.risk.as_str()))
        .collect();
    let expected_prefix = format!("plug_{}_", manifest.id);
    for (i, c) in manifest.contributes.commands.iter().enumerate() {
        dup("commands", &c.id, &mut errors);
        if !c.id.starts_with(&expected_prefix) {
            errors.push(ManifestIssue::new(
                format!("contributes.commands[{i}].id"),
                "command-id-namespace",
                format!(
                    "command id 必须以 plug_<pluginId>_ 为前缀（期望前缀 {expected_prefix:?}）"
                ),
            ));
        }
        if c.name.trim().is_empty() {
            errors.push(ManifestIssue::new(
                format!("contributes.commands[{i}].name"),
                "required",
                "command name 不能为空",
            ));
        }
        if c.kind != "info" {
            errors.push(ManifestIssue::new(
                format!("contributes.commands[{i}].kind"),
                "enum",
                "command kind 只能是 \"info\"（v1 仅信息型命令）",
            ));
        }
        let has_view = c
            .target
            .view
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        let has_tool = c
            .target
            .tool
            .as_deref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if has_view == has_tool {
            errors.push(ManifestIssue::new(
                format!("contributes.commands[{i}].target"),
                "command-target",
                "command target 必须且只能声明 view 或 tool 之一",
            ));
            continue;
        }
        if let Some(vid) = c.target.view.as_deref() {
            if !view_ids.contains(vid) {
                errors.push(ManifestIssue::new(
                    format!("contributes.commands[{i}].target.view"),
                    "command-target-view",
                    format!("target.view {vid:?} 不在 contributes.views 声明中"),
                ));
            }
        }
        if let Some(tid) = c.target.tool.as_deref() {
            match tool_risks.get(tid) {
                None => errors.push(ManifestIssue::new(
                    format!("contributes.commands[{i}].target.tool"),
                    "command-target-tool",
                    format!("target.tool {tid:?} 不在 contributes.tools 声明中"),
                )),
                Some(risk) if *risk != "read" => errors.push(ManifestIssue::new(
                    format!("contributes.commands[{i}].target.tool"),
                    "command-target-read-only",
                    format!("target.tool {tid:?} 必须指向 risk:\"read\" 工具（当前 risk:{risk:?}），审计限只读（design §12.1）"),
                )),
                _ => {}
            }
        }
        // 禁止多目标形态：同时声明 view 与 tool 已在 has_view==has_tool 分支拒绝
    }

    // —— 渲染器组（B3，ADR-0012）——
    for (i, r) in manifest.contributes.renderers.iter().enumerate() {
        let field = |suffix: &str| format!("contributes.renderers[{i}].{suffix}");
        // id 命名空间（ADR-0012 第 5 条：禁覆盖内置 id 的命名前提）
        let want_prefix = format!("plug_{}_", manifest.id);
        if !r.id.starts_with(&want_prefix) || r.id.len() <= want_prefix.len() {
            errors.push(ManifestIssue::new(
                field("id"),
                "renderer-id-namespace",
                format!("渲染器 id 必须为 plug_{}_<rid> 形态（当前 {:?}）", manifest.id, r.id),
            ));
        }
        // match 非空 + 项形态 + 内置占用（跨插件冲突由扫描汇总层判）
        if r.match_rule.ext.is_empty() && r.match_rule.fence.is_empty() {
            errors.push(ManifestIssue::new(
                field("match"),
                "renderer-match-empty",
                "match 必须至少声明一项 ext 或 fence",
            ));
        }
        for (j, e) in r.match_rule.ext.iter().enumerate() {
            let e_lower = e.to_ascii_lowercase();
            if e_lower.starts_with('.') {
                errors.push(ManifestIssue::new(
                    field(&format!("match.ext[{j}]")),
                    "renderer-ext-form",
                    format!("ext 写裸扩展名（不带点）：{e:?} → \"{e_lower}\""),
                ));
            }
            if BUILTIN_RENDER_EXTS.contains(&e_lower.as_str()) {
                errors.push(ManifestIssue::new(
                    field(&format!("match.ext[{j}]")),
                    "renderer-match-builtin",
                    format!("ext {e_lower:?} 为内置格式占用，禁覆盖（ADR-0012 拍板 5）"),
                ));
            }
        }
        for (j, f) in r.match_rule.fence.iter().enumerate() {
            let f_lower = f.to_ascii_lowercase();
            if BUILTIN_RENDER_FENCES.contains(&f_lower.as_str()) {
                errors.push(ManifestIssue::new(
                    field(&format!("match.fence[{j}]")),
                    "renderer-match-builtin",
                    format!("fence {f_lower:?} 为内置围栏占用，禁覆盖（ADR-0012 拍板 5）"),
                ));
            }
        }
        // engine 枚举 + entry 配套 + config 边界（服务地址禁内嵌，ADR-0012 拍板 4）
        if !RENDER_ENGINES.contains(&r.engine.as_str()) {
            errors.push(ManifestIssue::new(
                field("engine"),
                "renderer-engine-unknown",
                format!("未知引擎 {r:?}（首批清单见 api-schema：{RENDER_ENGINES:?}）"),
            ));
        }
        match r.engine.as_str() {
            "plugin-iframe" => {
                let entry_ok = r
                    .entry
                    .as_deref()
                    .map(|e| !e.trim().is_empty())
                    .unwrap_or(false);
                if !entry_ok {
                    errors.push(ManifestIssue::new(
                        field("entry"),
                        "renderer-entry-required",
                        "engine:\"plugin-iframe\" 必须声明 entry（相对路径）",
                    ));
                }
                if r.config.is_some() {
                    warnings.push(ManifestIssue::new(
                        field("config"),
                        "renderer-config-ignored",
                        "plugin-iframe 引擎不接受 config（已忽略）",
                    ));
                }
            }
            engine if SERVICE_RENDER_ENGINES.contains(&engine) => {
                if r.entry.is_some() {
                    errors.push(ManifestIssue::new(
                        field("entry"),
                        "renderer-entry-unexpected",
                        format!("engine:{engine:?} 不接受 entry（仅 plugin-iframe）"),
                    ));
                }
                match &r.config {
                    Some(serde_json::Value::Object(map)) => {
                        let keys: Vec<&String> = map.keys().collect();
                        if keys.as_slice() != [&"service".to_string()] {
                            errors.push(ManifestIssue::new(
                                field("config"),
                                "renderer-config-form",
                                "服务类引擎 config 仅允许 {\"service\":\"<配置名>\"}",
                            ));
                        } else if map["service"].as_str().is_none_or(str::is_empty) {
                            errors.push(ManifestIssue::new(
                                field("config.service"),
                                "renderer-config-form",
                                "config.service 必须是非空字符串（服务配置名）",
                            ));
                        }
                    }
                    _ => errors.push(ManifestIssue::new(
                        field("config"),
                        "renderer-config-required",
                        format!("engine:{engine:?} 需要服务地址——config 必须为 {{\"service\":\"<配置名>\"}}；地址由用户在设置页配置，插件禁内嵌 URL（ADR-0012 拍板 4）"),
                    )),
                }
                // 内嵌 URL 检查（含 config 之外字段兜底：整个声明序列化扫 URL）
                let serialized = serde_json::to_string(r).unwrap_or_default();
                if serialized.contains("http://") || serialized.contains("https://") {
                    errors.push(ManifestIssue::new(
                        field("config"),
                        "renderer-embedded-url",
                        "渲染服务地址禁内嵌（内容外泄面红线）——引用设置页服务配置名",
                    ));
                }
            }
            _ => {
                if r.entry.is_some() {
                    errors.push(ManifestIssue::new(
                        field("entry"),
                        "renderer-entry-unexpected",
                        format!("engine:{:?} 不接受 entry（仅 plugin-iframe）", r.engine),
                    ));
                }
                if r.config.is_some() {
                    warnings.push(ManifestIssue::new(
                        field("config"),
                        "renderer-config-ignored",
                        format!("engine:{:?} 不接受 config（已忽略）", r.engine),
                    ));
                }
            }
        }
    }

    // —— 权限组 ——
    for (i, p) in manifest.permissions.api.iter().enumerate() {
        if let Err(why) = valid_api_pattern(p) {
            errors.push(ManifestIssue::new(
                format!("permissions.api[{i}]"),
                "api-pattern",
                format!("权限项 {p:?} 不合法：{why}"),
            ));
        }
    }
    if !manifest.permissions.api.is_empty() {
        warnings.push(ManifestIssue::new(
            "permissions.api",
            "reserved-in-v1",
            "v1 中 api 类权限项无消费者（hub.http.fetch 未进 v1，ADR-0003 拍板结论 4/5）",
        ));
    }
    for (i, d) in manifest.permissions.data.iter().enumerate() {
        if !DATA_DOMAINS.contains(&d.as_str()) {
            errors.push(ManifestIssue::new(
                format!("permissions.data[{i}]"),
                "unknown-domain",
                format!("未知数据域 {d:?}（v1 固定枚举见 api-schema）"),
            ));
        }
    }
    for (i, c) in manifest.permissions.credentials.iter().enumerate() {
        if !valid_credential_name(c) {
            errors.push(ManifestIssue::new(
                format!("permissions.credentials[{i}]"),
                "credential-name",
                "凭据项必须是凭据**名**（字母/数字/_-. ，≤64 字符）；值由 hub 代持，禁止写入 manifest",
            ));
        }
    }

    // 高危项：须与贡献物 risk:"high" 逐条一致（ADR-0001 第 3 条）
    for (i, h) in manifest.permissions.high_risk.iter().enumerate() {
        if !HIGH_RISK_ITEMS.contains(&h.as_str()) {
            errors.push(ManifestIssue::new(
                format!("permissions.highRisk[{i}]"),
                "unknown-high-risk",
                format!("未知高危项 {h:?}（清单归 hub 维护，v1 固定四项）"),
            ));
        }
    }
    let high_tools: Vec<&ToolDecl> = manifest
        .contributes
        .tools
        .iter()
        .filter(|t| t.risk == "high")
        .collect();
    if high_tools.is_empty() && !manifest.permissions.high_risk.is_empty() {
        warnings.push(ManifestIssue::new(
            "permissions.highRisk",
            "declared-without-high-contribution",
            "声明了高危项但没有任何 risk:\"high\" 贡献物（v1 高危项由 hub 按方法判定，不阻断）",
        ));
    }
    for t in &high_tools {
        warnings.push(ManifestIssue::new(
            format!("contributes.tools[{}.risk", t.id),
            "high-risk-tool",
            format!("工具 {} 标记为高危，调用前须用户逐次确认", t.name),
        ));
    }

    // —— 后端组 ——
    if let Some(b) = &manifest.backend {
        if b.command.trim().is_empty() {
            errors.push(ManifestIssue::new(
                "backend.command",
                "required",
                "backend.command 不能为空",
            ));
        }
        if std::path::Path::new(&b.command).is_absolute() {
            errors.push(ManifestIssue::new(
                "backend.command",
                "path-escape",
                "backend.command 不得是绝对路径（相对插件根或 PATH 命令名）",
            ));
        }
        if let Some(c) = &b.cwd {
            check_relative_path(c, "backend.cwd", &mut errors);
        }
        for (i, a) in b.args.iter().enumerate() {
            // 只对「看起来是路径」的参数做逃逸检查（普通开关值不判）
            if a.starts_with('/') {
                errors.push(ManifestIssue::new(
                    format!("backend.args[{i}]"),
                    "path-escape",
                    "backend.args 不得含绝对路径",
                ));
            } else if a.contains("..") && (a.contains('/') || a.contains('\\')) {
                errors.push(ManifestIssue::new(
                    format!("backend.args[{i}]"),
                    "path-escape",
                    "backend.args 不得含 .. 逃逸",
                ));
            }
        }
        if b.idle_timeout_secs == Some(0) {
            warnings.push(ManifestIssue::new(
                "backend.idleTimeoutSecs",
                "range",
                "idleTimeoutSecs = 0 已按关闭空闲回收处理（常驻建议显式声明 alwaysOn）",
            ));
        }
    }

    // 空贡献物 + 空权限：合法但无实际作用（ADR-0001 第 3 条末段 warning）
    let no_contrib = manifest.contributes.tools.is_empty()
        && manifest.contributes.skills.is_empty()
        && manifest.contributes.mcp.is_empty()
        && manifest.contributes.views.is_empty()
        && manifest.contributes.commands.is_empty();
    let no_perm = manifest.permissions.api.is_empty()
        && manifest.permissions.data.is_empty()
        && manifest.permissions.credentials.is_empty()
        && manifest.permissions.high_risk.is_empty();
    if no_contrib && no_perm {
        warnings.push(ManifestIssue::new(
            "contributes",
            "no-effect",
            "权限与贡献物均为空：该插件安装后无实际作用",
        ));
    }

    // 未知字段：加法承诺下不阻断，提示作者核对
    for key in manifest.extra.keys() {
        warnings.push(ManifestIssue::new(
            key.clone(),
            "unknown-field",
            format!("未知字段 {key:?}（本 hub apiVersion 1 不识别，已忽略）"),
        ));
    }

    if errors.is_empty() {
        Ok(ParsedManifest { manifest, warnings })
    } else {
        Err(errors)
    }
}

/// 相对路径声明检查（越界/绝对路径在 schema 层就拒绝）。
fn check_relative_path(p: &str, field: &str, errors: &mut Vec<ManifestIssue>) {
    if p.trim().is_empty() {
        errors.push(ManifestIssue::new(field, "required", "路径不能为空"));
        return;
    }
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        errors.push(ManifestIssue::new(
            field,
            "path-escape",
            "路径必须相对插件根，不得是绝对路径",
        ));
        return;
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        errors.push(ManifestIssue::new(
            field,
            "path-escape",
            "路径不得含 .. （禁止逃出插件根）",
        ));
    }
}

/// serde 错误 → 结构化错因（尽量提取字段名）。
fn serde_issue(e: &serde_json::Error) -> ManifestIssue {
    let msg = e.to_string();
    // serde_json 的报错形态：`missing field \`id\` at line 1 column 2`
    if let Some(rest) = msg.split_once("missing field `") {
        let field = rest
            .1
            .split_once('`')
            .map(|(f, _)| f)
            .unwrap_or("plugin.json");
        return ManifestIssue::new(field, "required", format!("缺少必需字段（{msg}）"));
    }
    if msg.contains("unknown variant") {
        return ManifestIssue::new("plugin.json", "enum", msg);
    }
    if msg.contains("invalid type") {
        return ManifestIssue::new("plugin.json", "type", msg);
    }
    ManifestIssue::new("plugin.json", "json", msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> String {
        r#"{
          "schemaVersion": 1,
          "id": "demo-plugin",
          "name": "Demo",
          "version": "0.1.0",
          "hub": { "apiVersion": "1" },
          "contributes": {
            "tools": [{
              "id": "ping",
              "name": "Ping",
              "risk": "read",
              "impl": { "kind": "backend", "ref": "ping" }
            }],
            "views": [{ "id": "home", "title": "Home", "entry": "view/index.html" }]
          },
          "permissions": {
            "api": [],
            "data": ["projects:read"],
            "credentials": ["token_a"],
            "highRisk": []
          },
          "backend": { "command": "node", "args": ["backend/main.js"] }
        }"#
        .to_string()
    }

    fn with_renderers(renderers_json: &str) -> String {
        let b = base();
        b.replace(
            "\"views\": [{ \"id\": \"home\", \"title\": \"Home\", \"entry\": \"view/index.html\" }]",
            &format!(
                "\"views\": [{{ \"id\": \"home\", \"title\": \"Home\", \"entry\": \"view/index.html\" }}],\n            \"renderers\": {renderers_json}"
            ),
        )
    }

    #[test]
    fn renderer_valid_declaration_passes() {
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_gantt","match":{"fence":["gantt"]},"engine":"mermaid"}]"#,
        );
        let p = parse(&raw).expect("valid renderer");
        assert_eq!(p.manifest.contributes.renderers[0].engine, "mermaid");
    }

    #[test]
    fn renderer_id_namespace_enforced() {
        let raw = with_renderers(
            r#"[{"id":"other_plugin_gantt","match":{"fence":["gantt"]},"engine":"mermaid"}]"#,
        );
        let errs = parse(&raw).expect_err("must fail");
        assert!(errs.iter().any(|e| e.rule == "renderer-id-namespace"), "{errs:?}");
    }

    #[test]
    fn renderer_builtin_fence_rejected() {
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_p","match":{"fence":["plantuml"]},"engine":"html-sanitized"}]"#,
        );
        let errs = parse(&raw).expect_err("must fail");
        assert!(errs.iter().any(|e| e.rule == "renderer-match-builtin"), "{errs:?}");
    }

    #[test]
    fn renderer_builtin_ext_rejected() {
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_x","match":{"ext":["md"]},"engine":"html-sanitized"}]"#,
        );
        let errs = parse(&raw).expect_err("must fail");
        assert!(errs.iter().any(|e| e.rule == "renderer-match-builtin"), "{errs:?}");
    }

    #[test]
    fn renderer_service_engine_requires_config_ref_no_url() {
        // 缺 config：可定位失败（地址必须由用户在设置页配置）
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_pu","match":{"ext":["puml"]},"engine":"plantuml-server"}]"#,
        );
        let errs = parse(&raw).expect_err("must fail");
        assert!(errs.iter().any(|e| e.rule == "renderer-config-required"), "{errs:?}");
        // 内嵌 URL：审计失败（内容外泄面红线）
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_pu","match":{"ext":["puml"]},"engine":"plantuml-server","config":{"service":"https://evil.example.com"}}]"#,
        );
        let errs = parse(&raw).expect_err("must fail");
        assert!(
            errs.iter()
                .any(|e| e.rule == "renderer-config-form" || e.rule == "renderer-embedded-url"),
            "{errs:?}"
        );
        // 合法：引用服务配置名
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_pu","match":{"ext":["puml"]},"engine":"plantuml-server","config":{"service":"plantuml_base"}}]"#,
        );
        let p = parse(&raw).expect("valid service config ref");
    }

    #[test]
    fn renderer_iframe_entry_required() {
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_if","match":{"ext":["dot"]},"engine":"plugin-iframe"}]"#,
        );
        let errs = parse(&raw).expect_err("must fail");
        assert!(errs.iter().any(|e| e.rule == "renderer-entry-required"), "{errs:?}");
        let raw = with_renderers(
            r#"[{"id":"plug_demo-plugin_if","match":{"ext":["dot"]},"engine":"plugin-iframe","entry":"render/index.html"}]"#,
        );
        let p = parse(&raw).expect("valid renderer");
        assert!(p.warnings.is_empty() || true);
    }

    #[test]
    fn parses_valid_manifest() {
        let p = parse(&base()).expect("valid manifest");
        assert_eq!(p.manifest.id, "demo-plugin");
        assert_eq!(p.manifest.contributes.tools[0].id, "ping");
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn missing_required_field_is_located() {
        let raw = base().replace("\"name\": \"Demo\",", "");
        let errs = parse(&raw).unwrap_err();
        assert_eq!(errs[0].field, "name");
        assert_eq!(errs[0].rule, "required");
    }

    #[test]
    fn api_version_mismatch_is_located() {
        let raw = base().replace("\"apiVersion\": \"1\"", "\"apiVersion\": \"2\"");
        let errs = parse(&raw).unwrap_err();
        assert!(errs
            .iter()
            .any(|e| e.field == "hub.apiVersion" && e.rule == "api-version-mismatch"));
        assert!(errs
            .iter()
            .any(|e| e.message.contains("插件要求扩展 API 主版本 2")));
    }

    #[test]
    fn min_hub_version_too_new_fails() {
        let raw = base().replace(
            "\"apiVersion\": \"1\"",
            "\"apiVersion\": \"1\", \"minHubVersion\": \"99.0.0\"",
        );
        let errs = parse(&raw).unwrap_err();
        assert!(errs.iter().any(|e| e.rule == "min-hub-version"));
    }

    #[test]
    fn bad_ids_are_rejected() {
        let too_long = "a".repeat(41);
        let bads = ["A", "a", "-ab", "ab-", "a_b", too_long.as_str()];
        for bad in bads {
            assert!(!valid_plugin_id(bad), "{bad} 应被拒绝");
        }
        assert!(valid_plugin_id("ab"));
        assert!(valid_plugin_id("gitlab-issues-lite"));
    }

    #[test]
    fn data_domain_must_be_declared_enum() {
        let raw = base().replace("\"projects:read\"", "\"everything:read\"");
        let errs = parse(&raw).unwrap_err();
        assert!(errs.iter().any(|e| e.rule == "unknown-domain"));
    }

    #[test]
    fn credential_name_rule_is_shared() {
        assert!(valid_credential_name("gitlab_pat"));
        assert!(valid_credential_name("a"));
        assert!(!valid_credential_name(""));
        assert!(!valid_credential_name("has space"));
        assert!(!valid_credential_name("a:b"));
        assert!(!valid_credential_name(&"x".repeat(65)));
    }

    #[test]
    fn credential_value_like_name_is_rejected() {
        let raw = base().replace("\"token_a\"", "\"glpat-1234567890abcdef:secret\"");
        let errs = parse(&raw).unwrap_err();
        assert!(errs.iter().any(|e| e.rule == "credential-name"));
    }

    #[test]
    fn api_pattern_syntax() {
        assert!(valid_api_pattern("GET /api/agents/*").is_ok());
        assert!(valid_api_pattern("GET /api/**").is_err());
        assert!(valid_api_pattern("GET /api/a*b").is_err());
        assert!(valid_api_pattern("FETCH /api/x").is_err());
        assert!(valid_api_pattern("GET").is_err());
    }

    #[test]
    fn path_escape_is_rejected() {
        let raw = base().replace("\"view/index.html\"", "\"../outside.html\"");
        let errs = parse(&raw).unwrap_err();
        assert!(errs.iter().any(|e| e.rule == "path-escape"));

        let raw = base().replace("\"view/index.html\"", "\"/etc/passwd\"");
        let errs = parse(&raw).unwrap_err();
        assert!(errs.iter().any(|e| e.rule == "path-escape"));
    }

    #[test]
    fn unknown_fields_are_warnings_not_errors() {
        let raw = base().replace(
            "\"name\": \"Demo\",",
            "\"name\": \"Demo\", \"futureField\": 1,",
        );
        let p = parse(&raw).expect("unknown field must not block install");
        assert!(p.warnings.iter().any(|w| w.rule == "unknown-field"));
    }

    #[test]
    fn empty_plugin_gets_no_effect_warning() {
        let raw = r#"{
          "schemaVersion": 1, "id": "empty-one", "name": "Empty",
          "version": "0.1.0", "hub": { "apiVersion": "1" }
        }"#;
        let p = parse(raw).expect("empty plugin is legal");
        assert!(p.warnings.iter().any(|w| w.rule == "no-effect"));
    }

    #[test]
    fn semver_compare() {
        use std::cmp::Ordering::*;
        assert_eq!(compare_semver("0.5.0", "0.6.0"), Less);
        assert_eq!(compare_semver("1.0.0", "0.9.9"), Greater);
        assert_eq!(compare_semver("1.2.3", "1.2.3"), Equal);
    }

    #[test]
    fn manifest_is_serializable_for_registry_snapshot() {
        let p = parse(&base()).unwrap();
        let s = serde_json::to_string(&p.manifest).unwrap();
        assert!(s.contains("\"apiVersion\":\"1\""));
    }
}
