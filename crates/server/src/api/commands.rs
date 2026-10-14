//! 原生指令补全（P5 C4 / agent-hub-slash-skills）：内置命令 + 自定义命令 +
//! skills（全局/项目级，S1/S2）+ 插件命令/技能（S3）多数据源枚举。
//!
//! 远程降级（需求边界）：本接口无跨机透传——每实例 server 各自枚举本机文件系统
//! （本机/远程 hub/node-link 节点同构），即天然正确语义；老版本实例 server 忽略
//! 未知的 `cwd` 查询参数自动回落旧清单，不报错。远程透传枚举属 B 阶段。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use axum::{extract::{Query, State}, Json};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::api::SharedState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandItem {
    pub name: String,
    pub source: &'static str,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListCommandsQuery {
    /// agent 工作目录（S2）：提供时追加枚举 `<cwd>/.claude/` 下项目级命令与技能。
    /// 只读枚举根，与 docsList 同信任级（本地工具 + token 认证）。
    pub cwd: Option<String>,
}

/// 内置 slash 命令（claude CLI 常识集；本单不动，SDK 校准属 B 阶段）。
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
        return if v.is_empty() { None } else { Some(v.to_string()) };
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
        let raw = tokio::fs::read_to_string(claude_root.join("settings.json")).await.ok()?;
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

    let raw = match tokio::fs::read_to_string(claude_root.join("plugins/installed_plugins.json"))
        .await
    {
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
                let proj = item.get("projectPath").and_then(|p| p.as_str()).unwrap_or("");
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

/// 枚举全部来源（S1-S4）：builtin → 全局 custom/skill → 项目级（cwd 提供 时）
/// custom/skill → 插件 command/skill（`<plugin>:<name>` 前缀，与 TUI 一致）。
/// 同 (source, name) 去重取首见，输出按来源序 + 名内字典序。
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
        push(&mut out, &mut seen, (*n).to_string(), "builtin", Some((*d).to_string()));
    }

    // 全局自定义命令（S4：description 从 frontmatter 解析，原恒 None）
    for (name, path) in collect_md_commands(&claude_root.join("commands")).await {
        push(&mut out, &mut seen, name, "custom", md_description(&path).await);
    }

    // 全局技能（S1）
    for (name, description) in skills_in_dir(&claude_root.join("skills")).await {
        push(&mut out, &mut seen, name, "skill", description);
    }

    // 项目级（S2）：随 agent cwd
    if let Some(cwd) = cwd {
        let project_claude = Path::new(cwd).join(".claude");
        for (name, path) in collect_md_commands(&project_claude.join("commands")).await {
            push(&mut out, &mut seen, name, "custom", md_description(&path).await);
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
            push(&mut out, &mut seen, format!("{short}:{name}"), "plugin", description);
        }
    }

    out.sort_by(|a, b| {
        source_rank(a.source)
            .cmp(&source_rank(b.source))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

pub async fn list_commands(
    State(state): State<SharedState>,
    Query(q): Query<ListCommandsQuery>,
) -> Json<Vec<CommandItem>> {
    let cwd = q.cwd.as_deref().map(str::trim).filter(|s| !s.is_empty());
    Json(enumerate(&state.cfg.claude_root, cwd).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::TempDir;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn frontmatter_field_parses_scalar_variants() {
        let fm = "---\nname: \"quoted\"\ndescription: 含: 冒号的值\nother: x\n---\nbody";
        assert_eq!(frontmatter_field(fm, "name").as_deref(), Some("quoted"));
        assert_eq!(frontmatter_field(fm, "description").as_deref(), Some("含: 冒号的值"));
        assert_eq!(frontmatter_field(fm, "other").as_deref(), Some("x"));
        assert_eq!(frontmatter_field(fm, "missing"), None);
        // 块标量不解析（本单不做多行折叠）
        assert_eq!(frontmatter_field("---\ndesc: >\n  folded\n---\n", "desc"), None);
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
        write(&root.join("skills/gamma/SKILL.md"), "no frontmatter at all\n");

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
        write(&root.join("skills/only-global/SKILL.md"), "---\nname: only-global\n---\n");

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
        assert!(items.iter().any(
            |i| i.source == "plugin" && i.name == "on-plugin:web:run" && i.description.is_none()
        ));
        assert!(items.iter().any(|i| i.source == "plugin"
            && i.name == "on-plugin:sk-a"
            && i.description.as_deref() == Some("插件技能")));
        assert!(!items.iter().any(|i| i.name.contains("nope")));
        // 缺 enabledPlugins 键的插件视为未启用
        assert!(!items.iter().any(|i| i.source == "plugin" && i.name.contains("off-plugin")));
    }

    /// S3 补充：project 作用域插件仅 cwd 匹配 projectPath 时入列；无 cwd 不入列。
    #[tokio::test]
    async fn project_scoped_plugin_requires_matching_cwd() {
        let td = TempDir::new().unwrap();
        let root = td.path();
        let inst = td.path().join("cache/p/1.0.0");
        write(&inst.join("commands/local.md"), "---\ndescription: 项目专属\n---\n");
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
}
