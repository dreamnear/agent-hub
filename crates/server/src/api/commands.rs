//! 原生指令补全（P5 C4）：内置命令 + `~/.claude/commands/*.md` 自定义命令数据源。

use axum::{extract::State, Json};
use serde::Serialize;

use crate::api::SharedState;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandItem {
    pub name: String,
    pub source: &'static str,
    pub description: Option<String>,
}

/// 内置 slash 命令（claude CLI 常识集；自定义命令覆盖同名的以自定义为准）。
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

pub async fn list_commands(State(state): State<SharedState>) -> Json<Vec<CommandItem>> {
    let mut out: Vec<CommandItem> = BUILTIN
        .iter()
        .map(|(n, d)| CommandItem {
            name: (*n).to_string(),
            source: "builtin",
            description: Some((*d).to_string()),
        })
        .collect();

    // 自定义命令：~/.claude/commands/**/*.md（子目录 → namespace:name）
    let root = state.cfg.claude_root.join("commands");
    let mut stack = vec![root.clone()];
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
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                let name = rel.trim_end_matches(".md").replace('/', ":");
                out.push(CommandItem {
                    name,
                    source: "custom",
                    description: None,
                });
            }
        }
    }
    Json(out)
}
