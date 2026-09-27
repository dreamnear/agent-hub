//! agents 配置管理器：`~/.claude/agents/*.md` 列表（frontmatter 摘要）/查看/编辑写回。

use std::path::PathBuf;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::AppError;

use super::SharedState;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/agents-config", axum::routing::get(list_configs))
        .route(
            "/api/agents-config/{name}",
            axum::routing::get(get_config).put(put_config),
        )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDocSummary {
    /// 文件名 stem——唯一路由标识（get/put 以此定位），幽文件路径不存在（P4 处置 P3-1）
    pub name: String,
    /// frontmatter name 仅展示（与路由键解耦）
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub tools: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AgentDoc {
    #[serde(flatten)]
    pub summary: AgentDocSummary,
    pub content: String,
}

fn agents_dir(cfg: &Config) -> PathBuf {
    cfg.agents_dir.clone()
}

/// name 白名单：字母数字、`-`、`_`，防路径穿越（仅 .md 由路由语义保证）。
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn list_configs(
    State(state): State<SharedState>,
) -> Result<Json<Vec<AgentDocSummary>>, AppError> {
    let dir = agents_dir(&state.cfg);
    // 目录不存在（用户从未创建 agents 配置）语义为空列表（P4 处置 P3-3）
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return Ok(Json(Vec::new()));
    };
    let mut out = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let raw = match tokio::fs::read_to_string(&path).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "skip unreadable agent doc");
                continue;
            }
        };
        let fm = parse_frontmatter(&raw);
        out.push(AgentDocSummary {
            name: path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            display_name: fm.get("name").cloned(),
            description: fm.get("description").cloned(),
            model: fm.get("model").cloned(),
            tools: fm.get("tools").cloned(),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(out))
}

async fn get_config(
    State(state): State<SharedState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<AgentDoc>, AppError> {
    if !valid_name(&name) {
        return Err(AppError::bad("非法 agents 配置名"));
    }
    let path = agents_dir(&state.cfg).join(format!("{name}.md"));
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|_| AppError::not_found(format!("agents 配置 {name} 不存在")))?;
    let fm = parse_frontmatter(&content);
    Ok(Json(AgentDoc {
        summary: AgentDocSummary {
            name,
            display_name: fm.get("name").cloned(),
            description: fm.get("description").cloned(),
            model: fm.get("model").cloned(),
            tools: fm.get("tools").cloned(),
        },
        content,
    }))
}

#[derive(Debug, Deserialize)]
pub struct PutBody {
    pub content: String,
}

/// 编辑写回（整文件覆盖）。空内容拒绝（防误清）。
async fn put_config(
    State(state): State<SharedState>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<PutBody>,
) -> Result<StatusCode, AppError> {
    if !valid_name(&name) {
        return Err(AppError::bad("非法 agents 配置名"));
    }
    if body.content.trim().is_empty() {
        return Err(AppError::bad("内容为空，拒绝写回"));
    }
    let path = agents_dir(&state.cfg).join(format!("{name}.md"));
    // 编辑接口不做隐式创建：目标不存在 → 404（P4 处置 P3-4；创建如需支持另立显式入口）
    if !path.is_file() {
        return Err(AppError::not_found(format!("agents 配置 {name} 不存在")));
    }
    tokio::fs::write(&path, &body.content)
        .await
        .map_err(|e| AppError::bad(format!("写回失败: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// 简易 frontmatter 解析：`---` 围栏内 `key: value` 行（零依赖，不做嵌套/列表展开）。
fn parse_frontmatter(raw: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return map;
    }
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}
