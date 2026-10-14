//! 原生指令补全（P5 C4 / agent-hub-slash-skills）：多数据源枚举。
//!
//! B 阶段（agent-hub-slash-skills-b）：数据源切为 SDK 校准源——spawn 一次性
//! `claude` 子进程走 stream-json 控制协议（`initialize` 控制请求），取与 CLI
//! TUI `/` 菜单一致的精确清单（builtin 随 CLI 版本、插件启用态含项目作用域、
//! 项目级技能随 cwd）。结果按 (claude_root, cwd) 缓存（SDK 源 TTL 300s）；
//! 子进程失败（CLI 缺失/超时/解析失败）回退 A 阶段文件枚举，回退结果同缓存
//! 机制（TTL 60s 兼作失败负缓存，吸收 agent-hub#2 的 commands.rs 缓存半项），
//! 降级不报错。
//!
//! 已知缺口（勘察定案，见 docs/reports/agent-hub-slash-skills-b/coder.md）：
//! SDK 校准源不含 MCP prompts——CLI stream-json 模式对 MCP server 懒连接，
//! 初始化清单与后续 commands_changed 推送均无 prompts 条目。
//!
//! 远程降级（需求边界）：本接口无跨机透传——每实例 server 各自枚举本机
//! （本机/远程 hub/node-link 节点同构）；远程透传评估属 B 阶段报告项。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

use crate::api::SharedState;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandItem {
    pub name: String,
    pub source: &'static str,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListCommandsQuery {
    /// agent 工作目录（S2）：提供时清单随 cwd（SDK 校准源按该 cwd spawn；
    /// 文件枚举回退追加 `<cwd>/.claude/` 下项目级命令与技能）。
    /// 只读枚举根，与 docsList 同信任级（本地工具 + token 认证）。
    pub cwd: Option<String>,
}

/// 缓存条目来源（单测断言 + 日志口径；命中逻辑不区分）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CmdSourceKind {
    /// SDK 校准源（子进程精确清单）
    Sdk,
    /// A 阶段文件枚举（回退）
    Files,
}

/// 指令清单缓存键：(claude_root, cwd)。
pub(crate) type CommandsCacheKey = (PathBuf, Option<String>);

/// 缓存条目：清单 + 来源 + 过期时刻。
#[derive(Debug, Clone)]
pub(crate) struct CommandsCacheEntry {
    pub items: Vec<CommandItem>,
    pub kind: CmdSourceKind,
    pub expires_at: Instant,
}

pub(crate) type CommandCache = std::sync::Arc<
    tokio::sync::Mutex<std::collections::HashMap<CommandsCacheKey, CommandsCacheEntry>>,
>;

pub(crate) fn new_command_cache() -> CommandCache {
    std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()))
}

/// SDK 源 TTL（coder 裁量，落报告）：子进程单次 ~0.5-1s，5 分钟平衡新鲜度
/// 与开销——装/卸插件、改 skills 后清单最多延迟 5 分钟。
const SDK_TTL: Duration = Duration::from_secs(300);
/// 文件枚举回退 TTL（兼失败负缓存）：枚举本就快（毫秒级），60s 挡住
/// CLI 缺失场景下每请求都 spawn 一次的开销，1 分钟自愈。
const FILES_TTL: Duration = Duration::from_secs(60);
/// SDK 子进程超时：正常 ~1s，10s 兜底（首启冷缓存/hook 慢的机器）。
const SDK_TIMEOUT: Duration = Duration::from_secs(10);

/// 内置 slash 命令（claude CLI 常识集；仅文件枚举回退路径使用——SDK 校准源
/// 的内置清单由 CLI 自报，随版本精确）。
const BUILTIN: &[(&str, &str)] = &[
    ("help", "显示帮助"),
    ("clear", "清空对话历史"),
    ("compact", "压缩上下文"),
    ("config", "打开配置"),
    ("cost", "显示 token 用量"),
    ("doctor", "诊断安装"),
    ("init", "初始化项目 CLAUDE.md"),
    ("login", "登录"),
    ("logout", "登出"),
    ("mcp", "MCP 服务管理"),
    ("memory", "编辑记忆文件"),
    ("model", "切换模型"),
    ("permissions", "权限管理"),
    ("review", "代码评审"),
    ("status", "状态查看"),
    ("vim", "vim 模式"),
];

/// frontmatter 读取上限：frontmatter 恒在文件头部，防巨型 md 全量读入。
const HEAD_CAP: u64 = 16 * 1024;

/// 极简 frontmatter 顶层标量解析（S4）：识别 `---` 围栏，取 `key: value` 行的
/// value（去成对引号、容忍值内冒号）；块标量（`|`/`>` 开头）不解析返回 None。
fn frontmatter_field(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            return None; // 围栏闭合仍未见目标 key
        }
        let Some((k, v)) = trimmed.split_once(':') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        if v.starts_with('|') || v.starts_with('>') {
            return None; // 块标量：本单不做多行折叠解析
        }
        let v = v.trim_matches(|c| c == '"' || c == '\'');
        return if v.is_empty() {
            None
        } else {
            Some(v.to_string())
        };
    }
    None
}

/// 读文件头部（frontmatter 提取专用）。
async fn read_head(path: &Path) -> Option<String> {
    let mut buf = Vec::new();
    tokio::fs::File::open(path)
        .await
        .ok()?
        .take(HEAD_CAP)
        .read_to_end(&mut buf)
        .await
        .ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// md 文件首个 frontmatter 字段（描述解析共用：S2/S3/S4）。
async fn md_description(path: &Path) -> Option<String> {
    let head = read_head(path).await?;
    frontmatter_field(&head, "description")
}

/// 递归收集 root 下 `*.md`（子目录 → `ns:name`），返回 (名, 全路径)，名内排序。
async fn collect_md_commands(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                let name = rel.trim_end_matches(".md").replace('/', ":");
                out.push((name, path));
            }
        }
    }
    out.sort();
    out
}

/// 枚举 dir 下技能目录（S1/S2）：`<name>/SKILL.md`，name 取 frontmatter `name`
/// （缺失回退目录名），description 取 frontmatter `description`（缺失留空）。
async fn skills_in_dir(dir: &Path) -> Vec<(String, Option<String>)> {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry.path().is_dir() {
            continue;
        }
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.is_file() {
            continue;
        }
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let content = read_head(&skill_md).await.unwrap_or_default();
        let name = frontmatter_field(&content, "name").unwrap_or(dir_name);
        let description = frontmatter_field(&content, "description");
        out.push((name, description));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// 启用插件清单（S3）：读 `<claude_root>/plugins/installed_plugins.json` 各安装项，
/// 以 `<claude_root>/settings.json` 的 `enabledPlugins`（键 `plugin@marketplace`，
/// 值为 true）为启用闸；project 作用域安装项仅当 cwd 匹配其 `projectPath` 时纳入
/// （避免个人项目专属插件泄进其他 cwd 的清单）。
/// 密钥纪律：只取 installPath/scope/projectPath/plugin 名做枚举，两文件内容不落
/// 日志不回显（settings.json 含其他敏感键，只解析 enabledPlugins 一个字段）。
async fn enabled_plugins(claude_root: &Path, cwd: Option<&str>) -> Vec<(String, PathBuf)> {
    let enabled: HashSet<String> = async {
        let raw = tokio::fs::read_to_string(claude_root.join("settings.json"))
            .await
            .ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        Some(
            v.get("enabledPlugins")?
                .as_object()?
                .iter()
                .filter(|(_, en)| en.as_bool() == Some(true))
                .map(|(k, _)| k.clone())
                .collect(),
        )
    }
    .await
    .unwrap_or_default();

    let raw =
        match tokio::fs::read_to_string(claude_root.join("plugins/installed_plugins.json")).await {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(plugins) = v.get("plugins").and_then(|p| p.as_object()) else {
        return Vec::new();
    };

    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (key, installs) in plugins {
        if !enabled.contains(key) {
            continue;
        }
        let Some(short) = key.split('@').next().filter(|s| !s.is_empty()) else {
            continue;
        };
        let Some(items) = installs.as_array() else {
            continue;
        };
        for item in items {
            if item.get("scope").and_then(|s| s.as_str()) == Some("project") {
                let proj = item
                    .get("projectPath")
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                if cwd != Some(proj) {
                    continue;
                }
            }
            if let Some(p) = item.get("installPath").and_then(|p| p.as_str()) {
                out.push((short.to_string(), PathBuf::from(p)));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// 来源分组序（前端分组展示同序）：builtin < custom < skill < plugin。
fn source_rank(source: &str) -> u8 {
    match source {
        "builtin" => 0,
        "custom" => 1,
        "skill" => 2,
        _ => 3,
    }
}

/// 枚举全部来源（S1-S4，文件枚举回退路径）：builtin → 全局 custom/skill →
/// 项目级（cwd 提供时）custom/skill → 插件 command/skill（`<plugin>:<name>`
/// 前缀，与 TUI 一致）。同 (source, name) 去重取首见，输出按来源序 + 名内字典序。
async fn enumerate(claude_root: &Path, cwd: Option<&str>) -> Vec<CommandItem> {
    let mut out: Vec<CommandItem> = Vec::new();
    let mut seen: HashSet<(String, &'static str)> = HashSet::new();
    fn push(
        out: &mut Vec<CommandItem>,
        seen: &mut HashSet<(String, &'static str)>,
        name: String,
        source: &'static str,
        description: Option<String>,
    ) {
        if seen.insert((name.clone(), source)) {
            out.push(CommandItem {
                name,
                source,
                description,
            });
        }
    }

    for (n, d) in BUILTIN {
        push(
            &mut out,
            &mut seen,
            (*n).to_string(),
            "builtin",
            Some((*d).to_string()),
        );
    }

    // 全局自定义命令（S4：description 从 frontmatter 解析，原恒 None）
    for (name, path) in collect_md_commands(&claude_root.join("commands")).await {
        push(
            &mut out,
            &mut seen,
            name,
            "custom",
            md_description(&path).await,
        );
    }

    // 全局技能（S1）
    for (name, description) in skills_in_dir(&claude_root.join("skills")).await {
        push(&mut out, &mut seen, name, "skill", description);
    }

    // 项目级（S2）：随 agent cwd
    if let Some(cwd) = cwd {
        let project_claude = Path::new(cwd).join(".claude");
        for (name, path) in collect_md_commands(&project_claude.join("commands")).await {
            push(
                &mut out,
                &mut seen,
                name,
                "custom",
                md_description(&path).await,
            );
        }
        for (name, description) in skills_in_dir(&project_claude.join("skills")).await {
            push(&mut out, &mut seen, name, "skill", description);
        }
    }

    // 插件命令/技能（S3）
    for (short, install_path) in enabled_plugins(claude_root, cwd).await {
        for (name, path) in collect_md_commands(&install_path.join("commands")).await {
            push(
                &mut out,
                &mut seen,
                format!("{short}:{name}"),
                "plugin",
                md_description(&path).await,
            );
        }
        for (name, description) in skills_in_dir(&install_path.join("skills")).await {
            push(
                &mut out,
                &mut seen,
                format!("{short}:{name}"),
                "plugin",
                description,
            );
        }
    }

    out.sort_by(|a, b| {
        source_rank(a.source)
            .cmp(&source_rank(b.source))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// SDK 控制协议里的一条 slash 命令（`SlashCommand` 的所需子集）。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SdkCommand {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub builtin: Option<bool>,
}

/// SDK source 映射策略（B3，coder 裁量落报告）：SDK `SlashCommand` 无来源
/// 类型，只有 `builtin` 标志 + 名字形态可辨——builtin → "builtin"；带
/// `<plugin>:<name>` 冒号前缀 → "plugin"；其余（用户/项目级技能与自定义命令，
/// SDK 概念里统一为 skills）→ "skill"。前端按固定四组渲染，未映射值会被
/// 丢弃，故不引入新 source 值。
fn map_sdk_source(cmd: &SdkCommand) -> &'static str {
    if cmd.builtin == Some(true) {
        "builtin"
    } else if cmd.name.contains(':') {
        "plugin"
    } else {
        "skill"
    }
}

fn sdk_items(commands: Vec<SdkCommand>) -> Vec<CommandItem> {
    let mut out: Vec<CommandItem> = Vec::new();
    let mut seen: HashSet<(String, &'static str)> = HashSet::new();
    for cmd in commands {
        let source = map_sdk_source(&cmd);
        if seen.insert((cmd.name.clone(), source)) {
            out.push(CommandItem {
                name: cmd.name,
                source,
                description: cmd.description,
            });
        }
    }
    out.sort_by(|a, b| {
        source_rank(a.source)
            .cmp(&source_rank(b.source))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// 从一行 stdout 提取 initialize 控制应答：非目标行 → Ok(None)；命中 →
/// Ok(Some(命令清单))；应答报错（subtype != success）或形状不符 → Err。
/// 载荷嵌套在 `response.response.commands`（`SDKControlResponse` 内层为
/// `SDKControlInitializeResponse`）。
fn extract_sdk_commands(line: &str, want_id: &str) -> Result<Option<Vec<SdkCommand>>, String> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Ok(None); // 非 JSON 行（日志噪声）忽略
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("control_response") {
        return Ok(None);
    }
    let resp = match v.get("response") {
        Some(r) => r,
        None => return Ok(None),
    };
    if resp.get("request_id").and_then(|i| i.as_str()) != Some(want_id) {
        return Ok(None);
    }
    if resp.get("subtype").and_then(|s| s.as_str()) != Some("success") {
        return Err(format!(
            "initialize failed: {}",
            resp.get("response")
                .and_then(|r| serde_json::to_string(r).ok())
                .unwrap_or_default()
        ));
    }
    let commands = resp
        .pointer("/response/commands")
        .and_then(|c| c.as_array())
        .ok_or_else(|| "initialize response missing commands array".to_string())?;
    let mut out = Vec::with_capacity(commands.len());
    for (i, c) in commands.iter().enumerate() {
        let name = c
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| format!("command #{i} missing name"))?;
        if name.is_empty() {
            continue; // 无名条目不可补全，跳过
        }
        out.push(SdkCommand {
            name: name.to_string(),
            description: c
                .get("description")
                .and_then(|d| d.as_str())
                .map(String::from),
            builtin: c.get("builtin").and_then(|b| b.as_bool()),
        });
    }
    Ok(Some(out))
}

/// 一次性子进程获取 SDK 校准清单（B2）：spawn `claude`（stream-json 双向），
/// 写 `initialize` 控制请求，读行到匹配应答即杀。无模型 API 调用、无会话
/// 落盘（勘察实测）。任何失败（spawn/超时/解析）→ Err，调用方回退文件枚举。
pub(crate) async fn fetch_sdk_commands(
    claude_bin: &Path,
    cwd: &str,
) -> Result<Vec<CommandItem>, String> {
    let mut child = tokio::process::Command::new(claude_bin)
        .args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
        ])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn claude: {e}"))?;

    use tokio::io::AsyncWriteExt;
    let request = format!(
        "{}\n",
        serde_json::json!({
            "type": "control_request",
            "request_id": "hub-commands-1",
            "request": { "subtype": "initialize" },
        })
    );
    child
        .stdin
        .take()
        .ok_or_else(|| "claude stdin unavailable".to_string())?
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write control request: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "claude stdout unavailable".to_string())?;
    let mut lines = tokio::io::BufReader::new(stdout).lines();

    let read_all = async {
        while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
            if let Some(commands) = extract_sdk_commands(&line, "hub-commands-1")? {
                return Ok(commands);
            }
        }
        Err("claude exited before initialize response".to_string())
    };
    // 超时不包 read_all 的返回值类型差异：统一在结果上再包一层
    let commands: Vec<SdkCommand> = match tokio::time::timeout(SDK_TIMEOUT, read_all).await {
        Ok(Ok(commands)) => commands,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err("claude initialize timed out".to_string()),
    };
    Ok(sdk_items(commands))
}

/// 缓存层（B2/B6）：同 (claude_root, cwd) TTL 内直接命中；未命中先试 SDK
/// 校准源（cwd 为 None 时语义是「非项目清单」，文件枚举全局口径，不 spawn），
/// 失败回退文件枚举并同缓存（负缓存语义：回退结果 TTL 内不再 spawn）。
/// fetch / enumerate 注入以便单测。
pub(crate) async fn cached_commands_with<F, Fut, Ef, FutE>(
    cache: &CommandCache,
    claude_root: &Path,
    cwd: Option<&str>,
    fetch: F,
    enumerate: Ef,
) -> (Vec<CommandItem>, CmdSourceKind)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<CommandItem>, String>>,
    Ef: FnOnce() -> FutE,
    FutE: std::future::Future<Output = Vec<CommandItem>>,
{
    let key: CommandsCacheKey = (claude_root.to_path_buf(), cwd.map(String::from));
    {
        let guard = cache.lock().await;
        if let Some(entry) = guard.get(&key) {
            if entry.expires_at > Instant::now() {
                return (entry.items.clone(), entry.kind);
            }
        }
    }
    let (items, kind, ttl) = if cwd.is_none() {
        (enumerate().await, CmdSourceKind::Files, FILES_TTL)
    } else {
        match fetch().await {
            Ok(items) => (items, CmdSourceKind::Sdk, SDK_TTL),
            Err(err) => {
                tracing::warn!(cwd = cwd.unwrap_or(""), error = %err, "sdk commands fetch failed; falling back to file enumeration");
                (enumerate().await, CmdSourceKind::Files, FILES_TTL)
            }
        }
    };
    cache.lock().await.insert(
        key,
        CommandsCacheEntry {
            items: items.clone(),
            kind,
            expires_at: Instant::now() + ttl,
        },
    );
    (items, kind)
}

/// 生产入口：真实 fetch/enumerate 绑定。
async fn cached_commands(state: &crate::api::AppState, cwd: Option<&str>) -> Vec<CommandItem> {
    let claude_root = state.cfg.claude_root.clone();
    let bin = state.cfg.claude_bin.clone();
    let cwd_owned = cwd.map(String::from);
    let (items, _) = cached_commands_with(
        &state.commands_cache,
        &claude_root,
        cwd,
        || async move { fetch_sdk_commands(&bin, cwd_owned.as_deref().unwrap_or_default()).await },
        || {
            let claude_root = claude_root.clone();
            let cwd = cwd.map(String::from);
            async move { enumerate(&claude_root, cwd.as_deref()).await }
        },
    )
    .await;
    items
}

pub async fn list_commands(
    State(state): State<SharedState>,
    Query(q): Query<ListCommandsQuery>,
) -> Json<Vec<CommandItem>> {
    let cwd = q.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty());
    Json(cached_commands(&state, cwd).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn frontmatter_field_parses_scalar_variants() {
        let fm = "---\nname: \"quoted\"\ndescription: 含: 冒号的值\nother: x\n---\nbody";
        assert_eq!(frontmatter_field(fm, "name").as_deref(), Some("quoted"));
        assert_eq!(
            frontmatter_field(fm, "description").as_deref(),
            Some("含: 冒号的值")
        );
        assert_eq!(frontmatter_field(fm, "other").as_deref(), Some("x"));
        assert_eq!(frontmatter_field(fm, "missing"), None);
        // 块标量不解析（本单不做多行折叠）
        assert_eq!(
            frontmatter_field("---\ndesc: >\n  folded\n---\n", "desc"),
            None
        );
        assert_eq!(frontmatter_field("no fence", "desc"), None);
        assert_eq!(frontmatter_field("---\nempty:\n---\n", "empty"), None);
    }

    /// S1：全局技能进补全——name 取 frontmatter（缺失回退目录名），description
    /// 取 frontmatter（缺失留空）。
    #[tokio::test]
    async fn global_skills_with_frontmatter_fallback() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        write(
            &root.join("skills/alpha/SKILL.md"),
            "---\nname: alpha-skill\ndescription: 搜索技能\n---\nbody\n",
        );
        write(
            &root.join("skills/beta/SKILL.md"),
            "---\ndescription: 无 name 回退目录名\n---\n",
        );
        write(
            &root.join("skills/gamma/SKILL.md"),
            "no frontmatter at all\n",
        );

        let items = enumerate(root, None).await;
        let skills: Vec<_> = items.iter().filter(|i| i.source == "skill").collect();
        assert_eq!(skills.len(), 3);
        assert!(skills
            .iter()
            .any(|i| i.name == "alpha-skill" && i.description.as_deref() == Some("搜索技能")));
        assert!(skills
            .iter()
            .any(|i| i.name == "beta" && i.description.as_deref() == Some("无 name 回退目录名")));
        assert!(skills
            .iter()
            .any(|i| i.name == "gamma" && i.description.is_none()));
    }

    /// S2 + ns:name + S4：项目级命令/技能随 cwd 出现；子目录 ns:name；frontmatter
    /// 描述解析；无 cwd 时项目级条目消失。
    #[tokio::test]
    async fn project_sources_follow_cwd_and_ns_name() {
        let td = TempDir::new().unwrap();
        let root = td.path().join("claude");
        let proj = td.path().join("proj");
        write(
            &proj.join(".claude/commands/frontend/deploy.md"),
            "---\ndescription: 部署前端\n---\n",
        );
        write(
            &proj.join(".claude/skills/gamma/SKILL.md"),
            "---\nname: gamma-skill\n---\n",
        );
        write(
            &root.join("skills/only-global/SKILL.md"),
            "---\nname: only-global\n---\n",
        );

        let items = enumerate(&root, Some(proj.to_str().unwrap())).await;
        assert!(items.iter().any(|i| i.source == "custom"
            && i.name == "frontend:deploy"
            && i.description.as_deref() == Some("部署前端")));
        assert!(items
            .iter()
            .any(|i| i.source == "skill" && i.name == "gamma-skill"));
        assert!(items
            .iter()
            .any(|i| i.source == "skill" && i.name == "only-global"));

        let items = enumerate(&root, None).await;
        assert!(!items.iter().any(|i| i.name == "frontend:deploy"));
        assert!(!items.iter().any(|i| i.name == "gamma-skill"));
        assert!(items
            .iter()
            .any(|i| i.source == "skill" && i.name == "only-global"));
    }

    /// S3：插件命令/技能进补全——`<plugin>:<name>` 前缀、enabledPlugins 闸
    /// （未启用插件条目不入列）、frontmatter 描述、子目录 ns:name。
    #[tokio::test]
    async fn plugin_items_prefixed_and_enabled_gated() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        let on = td.path().join("cache/on-plugin/1.0.0");
        let off = td.path().join("cache/off-plugin/1.0.0");
        write(
            &on.join("commands/aside.md"),
            "---\ndescription: aside 命令\n---\n",
        );
        write(&on.join("commands/web/run.md"), "no fm\n");
        write(
            &on.join("skills/sk-a/SKILL.md"),
            "---\nname: sk-a\ndescription: 插件技能\n---\n",
        );
        write(
            &off.join("commands/nope.md"),
            "---\ndescription: 未启用不该出现\n---\n",
        );
        write(
            &root.join("plugins/installed_plugins.json"),
            &format!(
                r#"{{"version":2,"plugins":{{"on-plugin@mkt":[{{"scope":"user","installPath":"{}"}}],"off-plugin@mkt":[{{"scope":"user","installPath":"{}"}}]}}}}"#,
                on.display(),
                off.display()
            ),
        );
        write(
            &root.join("settings.json"),
            r#"{"enabledPlugins":{"on-plugin@mkt":true,"off-plugin@mkt":false}}"#,
        );

        let items = enumerate(root, None).await;
        assert!(items.iter().any(|i| i.source == "plugin"
            && i.name == "on-plugin:aside"
            && i.description.as_deref() == Some("aside 命令")));
        assert!(items.iter().any(|i| i.source == "plugin"
            && i.name == "on-plugin:web:run"
            && i.description.is_none()));
        assert!(items.iter().any(|i| i.source == "plugin"
            && i.name == "on-plugin:sk-a"
            && i.description.as_deref() == Some("插件技能")));
        assert!(!items.iter().any(|i| i.name.contains("nope")));
        // 缺 enabledPlugins 键的插件视为未启用
        assert!(!items
            .iter()
            .any(|i| i.source == "plugin" && i.name.contains("off-plugin")));
    }

    /// S3 补充：project 作用域插件仅 cwd 匹配 projectPath 时入列；无 cwd 不入列。
    #[tokio::test]
    async fn project_scoped_plugin_requires_matching_cwd() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        let inst = td.path().join("cache/p/1.0.0");
        write(
            &inst.join("commands/local.md"),
            "---\ndescription: 项目专属\n---\n",
        );
        write(
            &root.join("plugins/installed_plugins.json"),
            &format!(
                r#"{{"plugins":{{"p@mkt":[{{"scope":"project","projectPath":"/only/proj","installPath":"{}"}}]}}}}"#,
                inst.display()
            ),
        );
        write(
            &root.join("settings.json"),
            r#"{"enabledPlugins":{"p@mkt":true}}"#,
        );

        // cwd 匹配 → 入列
        let items = enumerate(root, Some("/only/proj")).await;
        assert!(items
            .iter()
            .any(|i| i.source == "plugin" && i.name == "p:local"));
        // 无 cwd / cwd 不匹配 → 不入列
        let items = enumerate(root, None).await;
        assert!(!items.iter().any(|i| i.name == "p:local"));
        let items = enumerate(root, Some("/other")).await;
        assert!(!items.iter().any(|i| i.name == "p:local"));
    }

    /// B7：SDK 控制应答解析——真实应答形态（载荷嵌套 response.response.commands），
    /// builtin/插件冒缀/用户技能三路映射，description 原样透传；同名去重取首见；
    /// 非目标行忽略；应答报错与缺 commands 数组报 Err。
    #[test]
    fn sdk_response_parse_and_source_mapping() {
        let line = serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "hub-commands-1",
                "response": {
                    "commands": [
                        { "name": "compact", "description": "压缩上下文", "argumentHint": "", "builtin": true },
                        { "name": "ecc:aside", "description": "(ecc) 快速旁问", "argumentHint": "" },
                        { "name": "adr-workflow", "description": "建 ADR 体系 (user)", "argumentHint": "" },
                        { "name": "daily-report", "description": "日报 (project)", "argumentHint": "" },
                        { "name": "compact", "description": "重复名取首见", "builtin": true }
                    ]
                }
            }
        })
        .to_string();
        let cmds = extract_sdk_commands(&line, "hub-commands-1")
            .unwrap()
            .expect("target response line");
        let items = sdk_items(cmds);
        assert_eq!(items.len(), 4); // 同名 compact 去重
        assert!(items.contains(&CommandItem {
            name: "compact".into(),
            source: "builtin",
            description: Some("压缩上下文".into()),
        }));
        assert!(items.contains(&CommandItem {
            name: "ecc:aside".into(),
            source: "plugin",
            description: Some("(ecc) 快速旁问".into()),
        }));
        assert!(items.contains(&CommandItem {
            name: "adr-workflow".into(),
            source: "skill",
            description: Some("建 ADR 体系 (user)".into()),
        }));
        assert!(items.contains(&CommandItem {
            name: "daily-report".into(),
            source: "skill",
            description: Some("日报 (project)".into()),
        }));
        // 排序：来源序 builtin < skill < plugin，名内字典序
        let sources: Vec<&str> = items.iter().map(|i| i.source).collect();
        let mut sorted = sources.clone();
        sorted.sort_by_key(|s| source_rank(s));
        assert_eq!(sources, sorted);

        // request_id 不匹配 → 非目标行
        assert!(extract_sdk_commands(&line, "other-id").unwrap().is_none());
        // 非 JSON 噪声行 → 忽略
        assert!(extract_sdk_commands("[hook] something", "hub-commands-1")
            .unwrap()
            .is_none());
        // 应答报错（subtype=error）→ Err
        let err_line = serde_json::json!({
            "type": "control_response",
            "response": { "subtype": "error", "request_id": "hub-commands-1", "response": "boom" }
        })
        .to_string();
        assert!(extract_sdk_commands(&err_line, "hub-commands-1").is_err());
        // 缺 commands 数组 → Err
        let missing = serde_json::json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": "hub-commands-1", "response": {} }
        })
        .to_string();
        assert!(extract_sdk_commands(&missing, "hub-commands-1").is_err());
    }

    /// B7：缓存命中——同 (claude_root, cwd) 二次请求不再触发 fetch（fetch 计数
    /// 不变），条目一致。
    #[tokio::test]
    async fn cache_hit_skips_second_fetch() {
        let cache = new_command_cache();
        let root = TempDir::new().unwrap().path().to_path_buf();
        let calls = AtomicUsize::new(0);
        let items = vec![CommandItem {
            name: "compact".into(),
            source: "builtin",
            description: None,
        }];

        let (first, kind) = cached_commands_with(
            &cache,
            &root,
            Some("/proj"),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, String>(items.clone()) }
            },
            || async { Vec::new() },
        )
        .await;
        assert_eq!(kind, CmdSourceKind::Sdk);
        assert_eq!(first.len(), 1);

        let (second, kind) = cached_commands_with(
            &cache,
            &root,
            Some("/proj"),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err::<Vec<CommandItem>, _>("should not be called".into()) }
            },
            || async { Vec::new() },
        )
        .await;
        assert_eq!(kind, CmdSourceKind::Sdk);
        assert_eq!(second, first);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 不同 cwd → 不同缓存键，fetch 再次触发
        let (_, _) = cached_commands_with(
            &cache,
            &root,
            Some("/other"),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, String>(Vec::new()) }
            },
            || async { Vec::new() },
        )
        .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// B7：失败回退——fetch Err → 文件枚举结果返回（kind=Files）不报错；回退
    /// 结果进缓存（负缓存语义：TTL 内二次请求不再 spawn，fetch 计数不变）。
    #[tokio::test]
    async fn fetch_failure_falls_back_to_files_and_caches() {
        let cache = new_command_cache();
        let td = TempDir::new().unwrap();
        let root = td.path();
        write(
            &root.join("skills/alpha/SKILL.md"),
            "---\nname: alpha-skill\ndescription: 回退可见\n---\n",
        );
        let proj = td.path().join("proj");
        write(
            &proj.join(".claude/commands/deploy.md"),
            "---\ndescription: 项目命令\n---\n",
        );
        let calls = AtomicUsize::new(0);

        let (items, kind) = cached_commands_with(
            &cache,
            root,
            Some(proj.to_str().unwrap()),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Err::<Vec<CommandItem>, _>("spawn claude: ENOENT".into()) }
            },
            || {
                let root = root.to_path_buf();
                let proj = proj.clone();
                async move { enumerate(&root, Some(proj.to_str().unwrap())).await }
            },
        )
        .await;
        assert_eq!(kind, CmdSourceKind::Files);
        assert!(
            !items.is_empty(),
            "file enumeration fallback must produce items"
        );
        assert!(items
            .iter()
            .any(|i| i.source == "skill" && i.name == "alpha-skill"));
        assert!(items
            .iter()
            .any(|i| i.source == "custom" && i.name == "deploy"));

        // 二次请求命中负缓存：fetch 不再被调
        let (again, kind) = cached_commands_with(
            &cache,
            root,
            Some(proj.to_str().unwrap()),
            || {
                calls.fetch_add(1, Ordering::SeqCst);
                async { Ok::<_, String>(Vec::new()) }
            },
            || async { Vec::new() },
        )
        .await;
        assert_eq!(kind, CmdSourceKind::Files);
        assert_eq!(again, items);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
