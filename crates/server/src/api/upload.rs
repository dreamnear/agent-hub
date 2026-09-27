//! C3 图片发送：上传图片落临时目录，消息携带路径引用（agent 经 Read 工具读图）。
//! r72 图片受限读取：GET /api/images/{filename} 供前端消息流内嵌缩略图。
//! r72b TUI 粘贴图：GET /api/images/by-path?path= 扩目录白名单（claude-tmp）。

use axum::{
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::SharedState;
use crate::error::AppError;

#[derive(Debug, Deserialize)]
pub struct UploadBody {
    /// 文件名（取扩展名判定类型）
    pub filename: String,
    /// base64 编码的图片内容
    pub data_base64: String,
}

const ALLOWED_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp"];

/// 上传根目录（上传与受限读取共用同一落盘位置）
fn upload_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("claude-view-uploads")
}

/// POST /api/upload → { "path": "/tmp/claude-view-uploads/<uuid>.<ext>" }
pub async fn upload(
    State(_state): State<SharedState>,
    Json(body): Json<UploadBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ext = body
        .filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if !ALLOWED_EXT.contains(&ext.as_str()) {
        return Err(AppError::bad(format!(
            "不支持的图片类型: {ext}（允许 png/jpg/jpeg/gif/webp）"
        )));
    }
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.data_base64.as_bytes())
        .map_err(|e| AppError::bad(format!("base64 解码失败: {e}")))?;
    if bytes.is_empty() {
        return Err(AppError::bad("图片内容为空"));
    }
    let dir = upload_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::bad(format!("创建上传目录失败: {e}")))?;
    let path = dir.join(format!("{}.{}", Uuid::new_v4(), ext));
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| AppError::bad(format!("写图失败: {e}")))?;
    Ok(Json(
        serde_json::json!({ "path": path.display().to_string() }),
    ))
}

/// 扩展名 → content-type；白名单外拒绝（防任意文件读取）
fn image_content_type(ext: &str) -> Result<&'static str, AppError> {
    match ext {
        "png" => Ok("image/png"),
        "jpg" | "jpeg" => Ok("image/jpeg"),
        "webp" => Ok("image/webp"),
        "gif" => Ok("image/gif"),
        other => Err(AppError::bad(format!(
            "不支持的图片类型: {other}（允许 png/jpg/jpeg/webp/gif）"
        ))),
    }
}

/// 图片响应公共头：本机私有内容，只许浏览器缓存，不经共享缓存转发
fn image_response(content_type: &'static str, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "private, max-age=3600"),
        ],
        bytes,
    )
        .into_response()
}

/// GET /api/images/{filename}：限 uploads 根的图片受限读取（LAN 模式经 auth
/// 中间件 token 校验；loopback 免认证语义与其余 /api 一致）。
/// filename 段必须是 [^/]+ 形态：拒绝 '..'、'/'、'\\'（防路径穿越），扩展名
/// 白名单（防任意文件读取）。错误消息不含服务端路径（5xx/4xx 均不泄露绝对路径）。
pub async fn get_image(Path(filename): Path<String>) -> Result<Response, AppError> {
    if filename.is_empty()
        || filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
    {
        return Err(AppError::bad("非法文件名"));
    }
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let content_type = image_content_type(&ext)?;
    let bytes = tokio::fs::read(upload_dir().join(&filename))
        .await
        .map_err(|_| AppError::not_found("图片不存在或已清理"))?;
    Ok(image_response(content_type, bytes))
}

/// GET /api/images/by-path?path=<绝对路径>：目录前缀白名单受限读（r72b，TUI
/// 粘贴图落 Claude CLI 临时目录 /tmp/claude-tmp/**）。canonicalize 归一（解符号
/// 链接 + 折叠 `..`）后判白名单：uploads 根按路径组件比较（自带目录边界）+
/// claude-tmp 字符串前缀（macOS /tmp 是 /private/tmp 的符号链接，canonicalize
/// 后归一为 /private/tmp 形态，两种前缀都列兼容非 macOS；尾随 '/' 保证目录
/// 边界，claude-tmp-evil 混淆不命中）。白名单外与文件不存在同 404（不构成
/// 存在性探测 oracle），错误消息不含服务端路径。
#[derive(Debug, Deserialize)]
pub struct ImagePathQuery {
    pub path: String,
}

fn is_allowed_image_path(canonical: &std::path::Path) -> bool {
    if upload_dir()
        .canonicalize()
        .is_ok_and(|root| canonical.starts_with(root))
    {
        return true;
    }
    canonical.to_str().is_some_and(|s| {
        s.starts_with("/private/tmp/claude-tmp/") || s.starts_with("/tmp/claude-tmp/")
    })
}

pub async fn get_image_by_path(Query(q): Query<ImagePathQuery>) -> Result<Response, AppError> {
    let ext = q.path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let content_type = image_content_type(&ext)?;
    let canonical = tokio::fs::canonicalize(&q.path)
        .await
        .map_err(|_| AppError::not_found("图片不存在或已清理"))?;
    if !is_allowed_image_path(&canonical) {
        return Err(AppError::not_found("图片不存在或已清理"));
    }
    let bytes = tokio::fs::read(&canonical)
        .await
        .map_err(|_| AppError::not_found("图片不存在或已清理"))?;
    Ok(image_response(content_type, bytes))
}
