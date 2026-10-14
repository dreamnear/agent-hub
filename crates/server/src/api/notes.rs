//! 工程便签 REST（agent-hub-notes）：GET/PUT /api/notes，键 = 归一化 cwd。
//! 同一工程的所有会话（Claude Code 与 ACP）读写同一份便签；
//! 安全红线：便签内容不进日志/tracing，错误消息不回显内容。

use axum::{
    extract::{Query, State},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{
    error::AppError,
    notes::{has_parent_segment, normalize_cwd, NotesStore},
};

use super::SharedState;

/// 单条便签内容上限（ocr-review 中：防大 payload 撑爆聚合序列化/磁盘）。
const MAX_CONTENT_BYTES: usize = 64 * 1024;

pub fn router() -> Router<SharedState> {
    Router::new().route("/api/notes", axum::routing::get(get_note).put(put_note))
}

#[derive(Debug, Deserialize)]
pub struct NoteQuery {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct NoteDto {
    /// 该工程无便签时为 null（前端按空文本处理）
    pub content: Option<String>,
}

async fn get_note(
    State(state): State<SharedState>,
    Query(q): Query<NoteQuery>,
) -> Result<Json<NoteDto>, AppError> {
    // ocr-review 中：GET 与 PUT 同一口径拒绝 `..`（公共校验函数），防不一致键空间
    if has_parent_segment(&q.path) {
        return Err(AppError::bad("路径不允许包含 .."));
    }
    let key = normalize_cwd(&q.path);
    let content = NotesStore::new(&state.cfg).get(&key).await;
    Ok(Json(NoteDto { content }))
}

#[derive(Debug, Deserialize)]
pub struct NoteBody {
    pub path: String,
    pub content: String,
}

async fn put_note(
    State(state): State<SharedState>,
    Json(body): Json<NoteBody>,
) -> Result<Json<NoteDto>, AppError> {
    if has_parent_segment(&body.path) {
        return Err(AppError::bad("路径不允许包含 .."));
    }
    if body.content.len() > MAX_CONTENT_BYTES {
        return Err(AppError(
            axum::http::StatusCode::PAYLOAD_TOO_LARGE,
            anyhow::anyhow!("便签内容超过 {MAX_CONTENT_BYTES} 字节上限"),
        ));
    }
    let key = normalize_cwd(&body.path);
    NotesStore::new(&state.cfg).set(&key, &body.content).await?;
    Ok(Json(NoteDto {
        content: Some(body.content),
    }))
}
