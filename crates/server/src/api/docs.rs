//! 文档查看器（P6 B8-B11 + 2026-10-02 全格式预览）：白名单内目录层级浏览 +
//! markdown/html/text 只读预览 + 图片/PDF 原始字节预览。只读；白名单校验与
//! git_tree 同源（canonicalize + 前缀匹配）；.env 类敏感文件按 binary 仅列出不可预览。

use std::path::Path;

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    response::Response,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::error::AppError;

use super::git_tree::{allowed_roots, ensure_tree_path};
use super::SharedState;

/// 统一拒绝文案：与 open-dir 同源，不泄露路径存在性（r40 观察）
const DENY_MSG: &str = "路径不在注册工程白名单内";

/// 预览大小上限：512KB（文本/JSON 路径，防大文件拖垮内存/传输；超出按 413 拒绝）
const MAX_PREVIEW_BYTES: u64 = 512 * 1024;

/// 图片/PDF 原始字节上限（20MB，超出按 413 拒绝）
const MAX_RAW_BYTES: u64 = 20 * 1024 * 1024;

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/docs/list", axum::routing::get(docs_list))
        .route("/api/docs/file", axum::routing::get(docs_file))
        .route("/api/docs/raw", axum::routing::get(docs_raw))
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
    /// 文件预览类型预判：markdown / html / text / image / pdf / binary；目录为 "dir"
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
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "avif" => "image",
        "pdf" => "pdf",
        "txt" | "log" | "json" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "csv"
        | "xml" | "sh" | "bash" | "zsh" | "py" | "rb" | "rs" | "go" | "java" | "js" | "jsx"
        | "ts" | "tsx" | "c" | "cc" | "cpp" | "h" | "css" | "scss" | "sql" | "proto" | "lock"
        | "gitignore" => "text",
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
    /// markdown / html / text / image / pdf / binary
    pub kind: String,
    pub name: String,
    /// image/pdf/binary 为 None（前端按 kind 走 /api/docs/raw 或空态）
    pub content: Option<String>,
    /// 文件大小（字节；未知二进制空态展示文件名/大小用）
    pub size: u64,
}

/// B9 文件预览：白名单内只读文件内容（UTF-8）；图片/PDF/二进制只回元信息不读内容。
async fn docs_file(
    State(state): State<SharedState>,
    Query(q): Query<DocPathQuery>,
) -> Result<Json<DocFileDto>, AppError> {
    let (file, name, meta) = resolve_whitelisted_file(&state, &q.path).await?;
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
    // 仅 text/markdown/html 读内容（非 UTF-8 → binary 回退）；image/pdf 内容走
    // /api/docs/raw；binary（含 .env 等敏感文件）一律不读不回传
    let content = if matches!(kind.as_str(), "text" | "markdown" | "html") {
        tokio::fs::read_to_string(&file).await.ok()
    } else {
        None
    };
    let kind = if content.is_none() && matches!(kind.as_str(), "text" | "markdown" | "html") {
        "binary".to_string()
    } else {
        kind
    };
    Ok(Json(DocFileDto {
        kind,
        name,
        content,
        size: meta.len(),
    }))
}

/// 白名单内只读文件定位（docs_file / docs_raw 共用）：拒绝 `..`，canonicalize 后
/// 前缀匹配注册工程白名单（安全红线，与 git_tree 同源，一行不松）。
async fn resolve_whitelisted_file(
    state: &SharedState,
    path: &str,
) -> Result<(std::path::PathBuf, String, std::fs::Metadata), AppError> {
    if path.contains("..") {
        return Err(AppError::forbidden(DENY_MSG));
    }
    let file = match tokio::fs::canonicalize(path).await {
        Ok(f) if f.is_file() => f,
        _ => return Err(AppError::forbidden(DENY_MSG)),
    };
    let allowed = allowed_roots(state)
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
    Ok((file, name, meta))
}

/// 原始字节端点（2026-10-02 全格式预览）：仅图片/PDF 扩展名放行（显式白名单），
/// 按扩展名给 Content-Type；20MB 上限。nosniff + 限 script 的 CSP 头防 SVG 直开执行。
async fn docs_raw(
    State(state): State<SharedState>,
    Query(q): Query<DocPathQuery>,
) -> Result<Response, AppError> {
    let (file, name, meta) = resolve_whitelisted_file(&state, &q.path).await?;
    let Some(content_type) = content_type_of(&name) else {
        return Err(AppError::not_found("该类型不支持原始字节预览"));
    };
    if meta.len() > MAX_RAW_BYTES {
        return Err(AppError(
            StatusCode::PAYLOAD_TOO_LARGE,
            anyhow::anyhow!("文件超过预览大小上限（20MB）"),
        ));
    }
    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|_| AppError::not_found("文件不存在"))?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            // 直接导航打开（非 <img> 场景）时禁脚本执行；样式允许（SVG 自带样式）
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'".to_owned(),
            ),
        ],
        Body::from(bytes),
    )
        .into_response())
}

/// 扩展名 → Content-Type（显式白名单：只有 image/pdf kind 会进 raw 端点）。
fn content_type_of(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let ext = Path::new(&lower)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "avif" => "image/avif",
        "pdf" => "application/pdf",
        _ => return None,
    })
}
