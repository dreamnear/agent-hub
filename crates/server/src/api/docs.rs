//! 文档查看器（P6 B8-B11）：白名单内目录层级浏览 + markdown/html/text 只读预览。
//! 只读；白名单校验与 git_tree 同源（canonicalize + 前缀匹配）；.env 类敏感文件按
//! binary 仅列出不可预览。

use std::path::Path;

use axum::{
    extract::{Query, State},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

use super::git_tree::{allowed_roots, ensure_tree_path};
use super::SharedState;

/// 统一拒绝文案：与 open-dir 同源，不泄露路径存在性（r40 观察）
const DENY_MSG: &str = "路径不在注册工程白名单内";

/// 预览大小上限：512KB（防大文件拖垮内存/传输；超出按 413 拒绝）
const MAX_PREVIEW_BYTES: u64 = 512 * 1024;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/docs/list", axum::routing::get(docs_list))
        .route("/api/docs/file", axum::routing::get(docs_file))
}

#[derive(Debug, Deserialize)]
pub struct DocPathQuery {
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocEntry {
    pub name: String,
    pub is_dir: bool,
    /// 文件预览类型预判：markdown / html / text / binary；目录为 "dir"
    pub kind: String,
}

/// B8 目录浏览：白名单内目录的内容清单（目录在前、名称排序）。
async fn docs_list(
    State(state): State<SharedState>,
    Query(q): Query<DocPathQuery>,
) -> Result<Json<Vec<DocEntry>>, AppError> {
    let dir = ensure_tree_path(&state, &q.path).await?;
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|_| AppError::not_found("目录不存在"))?;
    let mut out: Vec<DocEntry> = Vec::new();
    while let Some(e) = entries
        .next_entry()
        .await
        .map_err(|_| AppError::bad("目录读取失败"))?
    {
        let is_dir = e.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        let name = e.file_name().to_string_lossy().to_string();
        let kind = if is_dir {
            "dir".to_string()
        } else {
            kind_of(&name)
        };
        out.push(DocEntry { name, is_dir, kind });
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(Json(out))
}

/// 预览类型判定：扩展名 + 少数无扩展名文本文件名；其余一律 binary（仅列出）。
fn kind_of(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let ext = Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "txt" | "log" | "json" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "csv"
        | "xml" | "svg" | "sh" | "bash" | "zsh" | "py" | "rb" | "rs" | "go" | "js" | "jsx"
        | "ts" | "tsx" | "css" | "scss" | "sql" | "proto" | "lock" | "gitignore" => "text",
        _ => {
            match lower.as_str() {
                "dockerfile" | "makefile" | "license" | "readme" | ".gitignore"
                | ".gitattributes" | ".editorconfig" => "text",
                // .env / .pem / 密钥类无扩展名敏感文件不在此列 → binary 仅列出
                _ => "binary",
            }
        }
    }
    .to_string()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocFileDto {
    /// markdown / html / text / binary
    pub kind: String,
    pub name: String,
    /// binary 为 None（仅列出不可预览）
    pub content: Option<String>,
}

/// B9 文件预览：白名单内只读文件内容（UTF-8）；非 UTF-8 按二进制回退（仅列出）。
async fn docs_file(
    State(state): State<SharedState>,
    Query(q): Query<DocPathQuery>,
) -> Result<Json<DocFileDto>, AppError> {
    if q.path.contains("..") {
        return Err(AppError::forbidden(DENY_MSG));
    }
    let file = match tokio::fs::canonicalize(&q.path).await {
        Ok(f) if f.is_file() => f,
        _ => return Err(AppError::forbidden(DENY_MSG)),
    };
    let allowed = allowed_roots(&state)
        .await
        .iter()
        .any(|root| file.starts_with(root));
    if !allowed {
        return Err(AppError::forbidden(DENY_MSG));
    }
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let meta = tokio::fs::metadata(&file)
        .await
        .map_err(|_| AppError::not_found("文件不存在"))?;
    if meta.len() > MAX_PREVIEW_BYTES {
        return Err(AppError(
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            anyhow::anyhow!(format!(
                "文件超过预览大小上限（{}KB），仅列出不可预览",
                MAX_PREVIEW_BYTES / 1024
            )),
        ));
    }
    let kind = kind_of(&name);
    if kind == "binary" {
        return Ok(Json(DocFileDto {
            kind,
            name,
            content: None,
        }));
    }
    // 非 UTF-8 → 按二进制回退（仅列出）
    let content = tokio::fs::read_to_string(&file).await.ok();
    let kind = if content.is_some() {
        kind
    } else {
        "binary".to_string()
    };
    Ok(Json(DocFileDto {
        kind,
        name,
        content,
    }))
}
