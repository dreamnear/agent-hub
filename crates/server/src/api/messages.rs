//! 对话 API：历史读取（GET messages）+ 统一消息路由发送（POST message）。
//! driver-unify 批 2.2：历史面端点全部走 SessionHistory trait（轨二），api 层零
//! claude 直调（除 send/interrupt 两条，2.3/2.4 处理）。

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::drivers::{AgentDriver, SubagentEntry, TaskEntry};
use crate::error::AppError;
use crate::models::ChatMessage;

use super::agents::app_err;
use super::SharedState;

/// chat 分页配置（P6 B14）：前端启动读取一次，用于缓冲淘汰阈值
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatConfigDto {
    page_size: u32,
    buffer_max: u32,
}

async fn chat_config(State(state): State<SharedState>) -> Json<ChatConfigDto> {
    Json(ChatConfigDto {
        page_size: state.cfg.chat.page_size,
        buffer_max: state.cfg.chat.buffer_max,
    })
}

/// 分页信封（P6 B12/B13）：firstLine 为向前翻页游标，hasMore 标识还有更早消息
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MessagePageDto {
    messages: Vec<ChatMessage>,
    first_line: usize,
    has_more: bool,
}

impl MessagePageDto {
    /// None 页（会话/文件未找到）→ 空页而非 500，前端显示空态（批前语义）。
    fn from_page(p: Option<crate::drivers::events::MessagePage>) -> Self {
        match p {
            Some(p) => Self {
                messages: p.messages,
                first_line: p.first_line,
                has_more: p.has_more,
            },
            None => Self {
                messages: Vec::new(),
                first_line: 0,
                has_more: false,
            },
        }
    }
}

#[derive(Debug, Deserialize)]
struct PageQuery {
    before: Option<usize>,
    limit: Option<usize>,
}

/// 历史分页读取（P6 B12/B13）：末尾 N 条 + before 游标向前翻页；
/// 旧全量端点保留（/messages）作回归基线，前端已切分页端点。
async fn list_messages_page(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Query(q): Query<PageQuery>,
) -> Result<Json<MessagePageDto>, AppError> {
    let d = driver_of(&state, &driver)?;
    let limit = q
        .limit
        .unwrap_or(state.cfg.chat.page_size as usize)
        .clamp(1, 200);
    let page = d.page(&id, limit, q.before).await.map_err(app_err)?;
    Ok(Json(MessagePageDto::from_page(page)))
}

/// 会话输出活跃度（反馈 7-2）：jsonl mtime 静默阈值（30s）内有写入 = 工作中有输出。
/// 前端以此替代消息块脉冲驱动的工作指示，块间静默不抖动。
async fn session_active(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<bool>, AppError> {
    let d = driver_of(&state, &driver)?;
    let active = d.is_active(&id).await.map_err(app_err)?;
    Ok(Json(active))
}

/// subagent 会话清单（P5+）：目录缺失 = 无 subagent 视图，返回空集（前端不渲染不占位）。
/// 默认只回活跃条目（2026-10-01 产品纪律）；`?finished=1` 显式 opt-in 全量（历史入口）。
async fn list_subagents(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Query(q): Query<SubagentsQuery>,
) -> Result<Json<Vec<SubagentEntry>>, AppError> {
    let d = driver_of(&state, &driver)?;
    let finished = matches!(q.finished.as_deref(), Some("1") | Some("true"));
    let list = if finished {
        d.subagents_finished(&id).await.map_err(app_err)?
    } else {
        d.subagents(&id).await.map_err(app_err)?
    };
    Ok(Json(list))
}

#[derive(Debug, Deserialize)]
struct SubagentsQuery {
    /// 宽容解析 `1`/`true`（axum typed bool 不收 `1` 会 400；对齐 agents 清单 `all=1` 惯例）
    finished: Option<String>,
}

/// subagent 会话历史（只读）：定位（白名单校验防路径逃逸）后全量解析——
/// subagent jsonl 与主会话行结构同构（调查报告 1.5）。
async fn list_subagent_messages(
    State(state): State<SharedState>,
    AxumPath((driver, id, subagent_id)): AxumPath<(String, String, String)>,
) -> Result<Json<Vec<ChatMessage>>, AppError> {
    let d = driver_of(&state, &driver)?;
    let page = d
        .subagent_page(&id, &subagent_id, usize::MAX, None)
        .await
        .map_err(app_err)?;
    Ok(Json(page.map(|p| p.messages).unwrap_or_default()))
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/config/chat", axum::routing::get(chat_config))
        .route(
            "/api/agents/{driver}/{id}/messages",
            axum::routing::get(list_messages),
        )
        .route(
            "/api/agents/{driver}/{id}/messages/page",
            axum::routing::get(list_messages_page),
        )
        .route(
            "/api/agents/{driver}/{id}/message",
            axum::routing::post(send_message),
        )
        .route(
            "/api/agents/{driver}/{id}/interrupt",
            axum::routing::post(interrupt_agent),
        )
        .route(
            "/api/agents/{driver}/{id}/tasks",
            axum::routing::get(list_tasks),
        )
        .route(
            "/api/agents/{driver}/{id}/model-settings",
            axum::routing::get(get_model_settings).post(put_model_settings),
        )
        .route(
            "/api/agents/{driver}/{id}/permission",
            axum::routing::post(answer_permission),
        )
        .route(
            "/api/agents/{driver}/{id}/question",
            axum::routing::post(answer_question),
        )
        .route(
            "/api/agents/{driver}/{id}/session-active",
            axum::routing::get(session_active),
        )
        .route(
            "/api/agents/{driver}/{id}/subagents",
            axum::routing::get(list_subagents),
        )
        .route(
            "/api/agents/{driver}/{id}/subagents/{subagentId}/messages",
            axum::routing::get(list_subagent_messages),
        )
        .route(
            "/api/agents/{driver}/{id}/subagents/{subagentId}/messages/page",
            axum::routing::get(list_subagent_messages_page),
        )
}

/// subagent 会话历史分页（P6 B12/B13）：定位逻辑同 list_subagent_messages，读取走分页窗口
async fn list_subagent_messages_page(
    State(state): State<SharedState>,
    AxumPath((driver, id, subagent_id)): AxumPath<(String, String, String)>,
    Query(q): Query<PageQuery>,
) -> Result<Json<MessagePageDto>, AppError> {
    let d = driver_of(&state, &driver)?;
    let limit = q
        .limit
        .unwrap_or(state.cfg.chat.page_size as usize)
        .clamp(1, 200);
    let page = d
        .subagent_page(&id, &subagent_id, limit, q.before)
        .await
        .map_err(app_err)?;
    Ok(Json(MessagePageDto::from_page(page)))
}

/// 跨会话任务清单聚合（P5 preview）：cwd → 项目目录全 jsonl 的 Task 事件合并，
/// 当前会话 TaskList 权威快照；cwd 未知返回 null（前端回退单会话口径）。
/// null 与 [] 语义区分（tester-r27）：[] = 权威空快照，前端不得回退复活旧任务。
async fn list_tasks(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<Option<Vec<TaskEntry>>>, AppError> {
    let d = driver_of(&state, &driver)?;
    Ok(Json(d.tasks(&id).await.map_err(app_err)?))
}

/// 旧全量端点（/messages）：驱动 full_history 转调（回归基线，前端已切分页端点）。
async fn list_messages(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<Vec<ChatMessage>>, AppError> {
    let d = driver_of(&state, &driver)?;
    let msgs = d.full_history(&id).await.map_err(app_err)?;
    Ok(Json(msgs))
}

#[derive(Debug, Deserialize)]
pub struct SendBody {
    pub text: String,
}

/// 统一消息入口：空文本校验 → driver.send（Claude=attach PTY + 3s 落盘后置校验，
/// 2.4 已内化进驱动）→ 204。
async fn send_message(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Json(body): Json<SendBody>,
) -> Result<StatusCode, AppError> {
    if body.text.trim().is_empty() {
        return Err(AppError::bad("消息为空"));
    }
    let d = driver_of(&state, &driver)?;
    d.send(&id, body.text.trim())
        .await
        .map_err(|e| AppError::bad(format!("发送失败: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// C2 中断：全链路内化进 claude 驱动（批 2.3）——驱动侧 validate → PTY 中断
/// （resize 抖动 + Esc 补发）→ r69 基线采样打标 → list 时覆盖为空闲。
async fn interrupt_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    let d = driver_of(&state, &driver)?;
    d.cancel(&id).await.map_err(|e| {
        // 驱动域错误（id 白名单等）按原文透出，不带「中断失败」前缀（与批前文案一致）
        if e.downcast_ref::<crate::drivers::NotFound>().is_some() {
            AppError::bad(e.to_string())
        } else {
            AppError::bad(format!("中断失败: {e}"))
        }
    })?;
    Ok(StatusCode::NO_CONTENT)
}

// —— 会话外观（agent-hub-model-thinking）：读/设走 driver ModelSettings 透传，
// hub 不存会话模型状态（显示值一律读回自驱动）——

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModelSettingsDto {
    caps: crate::drivers::ModelCaps,
    current: Option<crate::drivers::SessionModel>,
    models: Vec<crate::drivers::ModelInfo>,
}

/// 当前模型/思考 + 能力声明 + 可选清单（一次取齐，面板单请求可用）。
/// 驱动不支持（caps 全 false，如 ACP）→ models 空、current 空，前端据此只显示
/// 「由会话进程管理」。
async fn get_model_settings(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<ModelSettingsDto>, AppError> {
    let d = driver_of(&state, &driver)?;
    let caps = d.model_caps();
    let (current, models) = if caps.can_read || caps.can_set {
        let current = d.session_model(&id).await.map_err(app_err).ok().flatten();
        let models = if caps.can_set {
            d.list_models().await.map_err(app_err)?
        } else {
            Vec::new()
        };
        (current, models)
    } else {
        (None, Vec::new())
    };
    Ok(Json(ModelSettingsDto {
        caps,
        current,
        models,
    }))
}

#[derive(serde::Deserialize)]
struct SetModelBody {
    model: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(serde::Serialize)]
struct SetModelOutcome {
    message: String,
}

/// 设置会话模型/思考；返回驱动收场文案（opencode=已切换，claude working=已排队）。
/// 空模型名校验同 send（系统边界 fail-fast）。
async fn put_model_settings(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Json(body): Json<SetModelBody>,
) -> Result<Json<SetModelOutcome>, AppError> {
    if body.model.trim().is_empty() {
        return Err(AppError::bad("模型为空"));
    }
    let d = driver_of(&state, &driver)?;
    let message = d
        .set_session_model(&id, body.model.trim(), body.variant.as_deref())
        .await
        .map_err(|e| {
            if e.downcast_ref::<crate::drivers::Unsupported>().is_some() {
                AppError::not_found(format!("驱动 {driver} 不支持模型设置"))
            } else {
                AppError::bad(format!("设置失败: {e}"))
            }
        })?;
    Ok(Json(SetModelOutcome { message }))
}

// —— 权限应答（agent-hub-dsh-driver 需求决策 7）：通用 driver 路由，接现有权限
// 弹卡链路（ACP 走自有 /api/acp/sessions/{id}/permission 不动；dsh 的
// answer_permission 内部转 /api/respond）——

#[derive(serde::Deserialize)]
struct PermissionBody {
    perm_id: String,
    #[serde(default)]
    option_id: Option<String>,
}

/// ask_user_question 应答（dsh $events waterfall；answers=AskUserQuestionAnswer）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionBody {
    pub event_id: String,
    pub answers: serde_json::Value,
}

async fn answer_question(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Json(body): Json<QuestionBody>,
) -> Result<StatusCode, AppError> {
    let d = driver_of(&state, &driver)?;
    d.answer_question(&id, &body.event_id, body.answers)
        .await
        .map_err(app_err)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn answer_permission(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Json(body): Json<PermissionBody>,
) -> Result<Json<bool>, AppError> {
    let d = driver_of(&state, &driver)?;
    // trait 是同步签名（dsh 内部自持 mini-runtime 做 respond 快调用）——从 async
    // 处理链调用必须走 spawn_blocking（不得阻塞 runtime worker）
    let perm_id = body.perm_id;
    let option_id = body.option_id;
    let allowed = tokio::task::spawn_blocking(move || {
        d.answer_permission(&id, &perm_id, option_id.as_deref())
    })
    .await
    .map_err(|e| AppError::bad(format!("应答任务失败: {e}")))?
    .map_err(|e| {
        if e.downcast_ref::<crate::drivers::Unsupported>().is_some()
            || e.downcast_ref::<crate::drivers::NotFound>().is_some()
        {
            AppError::not_found(e.to_string())
        } else {
            AppError::bad(format!("应答失败: {e}"))
        }
    })?;
    Ok(Json(allowed))
}

/// driver 名 → 注册表驱动（未知名 → 404，行为与批前 ensure_driver 一致）。
fn driver_of(
    state: &SharedState,
    driver: &str,
) -> Result<std::sync::Arc<dyn AgentDriver>, AppError> {
    state
        .drivers
        .get(driver)
        .map_err(|_| AppError::not_found(format!("未知 driver: {driver}")))
}
