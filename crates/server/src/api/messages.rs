//! 对话 API：历史读取（GET messages）+ 统一消息路由发送（POST message）。

use std::path::Path;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::drivers::route_send;
use crate::error::AppError;
use crate::models::ChatMessage;

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

#[derive(Debug, Deserialize)]
struct PageQuery {
    before: Option<usize>,
    limit: Option<usize>,
}

/// 会话 jsonl 定位（分页复用）：cwd 已知走 session_dir 精确定位，否则全 projects 扫描兜底
async fn locate_session_file(
    state: &SharedState,
    session_id: &str,
    cwd: Option<&String>,
) -> Option<std::path::PathBuf> {
    match cwd {
        Some(cwd) => {
            crate::drivers::claude::session::session_dir(&state.cfg.claude_root, Path::new(cwd))
                .map(|dir| dir.join(format!("{session_id}.jsonl")))
        }
        None => {
            crate::drivers::claude::session::find_session_path(&state.cfg.claude_root, session_id)
                .await
        }
    }
}

/// 历史分页读取（P6 B12/B13）：末尾 N 条 + before 游标向前翻页；
/// 旧全量端点保留（/messages）作回归基线，前端已切分页端点。
async fn list_messages_page(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Query(q): Query<PageQuery>,
) -> Result<Json<MessagePageDto>, AppError> {
    ensure_driver(&driver)?;
    let agent = agent_of(&state, &id).await?;
    let session_id = agent
        .session_id
        .ok_or_else(|| AppError::not_found(format!("agent {id} 无关联会话")))?;
    let path = locate_session_file(&state, &session_id, agent.cwd.as_ref())
        .await
        .ok_or_else(|| AppError::not_found(format!("会话 {session_id} 未找到")))?;
    let limit = q
        .limit
        .unwrap_or(state.cfg.chat.page_size as usize)
        .clamp(1, 200);
    // 文件缺失/读失败 → 空页而非 500，前端显示空态
    let page =
        match crate::drivers::claude::session::read_session_page(&path, limit, q.before).await {
            Some(p) => MessagePageDto {
                messages: p.messages,
                first_line: p.first_line,
                has_more: p.has_more,
            },
            None => MessagePageDto {
                messages: Vec::new(),
                first_line: 0,
                has_more: false,
            },
        };
    Ok(Json(page))
}

/// 会话输出活跃度（反馈 7-2）：jsonl mtime 静默阈值（30s）内有写入 = 工作中有输出。
/// 前端以此替代消息块脉冲驱动的工作指示，块间静默不抖动。
async fn session_active(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<bool>, AppError> {
    ensure_driver(&driver)?;
    let agent = agent_of(&state, &id).await?;
    let Some(session_id) = agent.session_id else {
        return Ok(Json(false));
    };
    Ok(Json(
        crate::drivers::claude::session::session_is_active(&state.cfg.claude_root, &session_id, 30)
            .await,
    ))
}

/// subagent 会话清单（P5+）：目录缺失 = 无 subagent 视图，返回空集（前端不渲染不占位）。
async fn list_subagents(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<Vec<crate::drivers::claude::subagents::SubagentEntry>>, AppError> {
    ensure_driver(&driver)?;
    let agent = agent_of(&state, &id).await?;
    let (Some(cwd), Some(session_id)) = (&agent.cwd, &agent.session_id) else {
        return Ok(Json(Vec::new()));
    };
    Ok(Json(
        crate::drivers::claude::subagents::list_subagents(
            &state.cfg.claude_root,
            Path::new(cwd),
            session_id,
        )
        .await,
    ))
}

/// subagent 会话历史（只读）：find_subagent_path 定位（白名单校验防路径逃逸）后
/// 复用 parse_session_file——subagent jsonl 与主会话行结构同构（调查报告 1.5）。
async fn list_subagent_messages(
    State(state): State<SharedState>,
    AxumPath((driver, id, subagent_id)): AxumPath<(String, String, String)>,
) -> Result<Json<Vec<ChatMessage>>, AppError> {
    ensure_driver(&driver)?;
    let agent = agent_of(&state, &id).await?;
    let Some(session_id) = agent.session_id else {
        return Err(AppError::not_found(format!("agent {id} 无关联会话")));
    };
    let path = crate::drivers::claude::session::find_subagent_path(
        &state.cfg.claude_root,
        &session_id,
        &subagent_id,
    )
    .await
    .ok_or_else(|| AppError::not_found(format!("subagent {subagent_id} 未找到")))?;
    Ok(Json(
        crate::drivers::claude::session::parse_session_file(&path)
            .await
            .unwrap_or_default(),
    ))
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
    ensure_driver(&driver)?;
    let agent = agent_of(&state, &id).await?;
    let Some(session_id) = agent.session_id else {
        return Err(AppError::not_found(format!("agent {id} 无关联会话")));
    };
    let path = crate::drivers::claude::session::find_subagent_path(
        &state.cfg.claude_root,
        &session_id,
        &subagent_id,
    )
    .await
    .ok_or_else(|| AppError::not_found(format!("subagent {subagent_id} 未找到")))?;
    let limit = q
        .limit
        .unwrap_or(state.cfg.chat.page_size as usize)
        .clamp(1, 200);
    let page =
        match crate::drivers::claude::session::read_session_page(&path, limit, q.before).await {
            Some(p) => MessagePageDto {
                messages: p.messages,
                first_line: p.first_line,
                has_more: p.has_more,
            },
            None => MessagePageDto {
                messages: Vec::new(),
                first_line: 0,
                has_more: false,
            },
        };
    Ok(Json(page))
}

/// 跨会话任务清单聚合（P5 preview）：cwd → 项目目录全 jsonl 的 Task 事件合并，
/// 当前会话 TaskList 权威快照；cwd 未知返回 null（前端回退单会话口径）。
/// null 与 [] 语义区分（tester-r27）：[] = 权威空快照，前端不得回退复活旧任务。
async fn list_tasks(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<Option<Vec<crate::drivers::claude::tasks::TaskEntry>>>, AppError> {
    ensure_driver(&driver)?;
    let agent = agent_of(&state, &id).await?;
    let (Some(cwd), Some(session_id)) = (&agent.cwd, &agent.session_id) else {
        return Ok(Json(None));
    };
    let tasks = crate::drivers::claude::tasks::aggregate_project_tasks(
        &state.cfg.claude_root,
        Path::new(cwd),
        session_id,
    )
    .await;
    Ok(Json(tasks))
}

async fn list_messages(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<Json<Vec<ChatMessage>>, AppError> {
    ensure_driver(&driver)?;
    let agent = agent_of(&state, &id).await?;
    let session_id = agent
        .session_id
        .ok_or_else(|| AppError::not_found(format!("agent {id} 无关联会话")))?;
    // cwd 已知时精确定位 slug；否则 read_session 扫描兜底
    let msgs = match &agent.cwd {
        Some(cwd) => {
            let dir = crate::drivers::claude::session::session_dir(
                &state.cfg.claude_root,
                Path::new(cwd),
            );
            match dir {
                Some(dir) => {
                    crate::drivers::claude::session::parse_session_file(
                        &dir.join(format!("{session_id}.jsonl")),
                    )
                    .await
                }
                None => None,
            }
        }
        None => {
            crate::drivers::claude::session::read_session(&state.cfg.claude_root, &session_id).await
        }
    };
    Ok(Json(msgs.unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
pub struct SendBody {
    pub text: String,
}

/// 统一消息入口：route_send(driver, id, msg) → driver 层实现（Claude=attach PTY）。
async fn send_message(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
    Json(body): Json<SendBody>,
) -> Result<StatusCode, AppError> {
    ensure_driver(&driver)?;
    if body.text.trim().is_empty() {
        return Err(AppError::bad("消息为空"));
    }
    let routes = state.message_routes.read().expect("routes lock").clone();
    route_send(&routes, &driver, &id, body.text.trim())
        .await
        .map_err(|e| AppError::bad(format!("发送失败: {e}")))?;
    // 注入后落盘验证（反馈轮 28-A）：3s 后 jsonl mtime 未推进 = 消息可能未被 CLI
    // 处理（TUI 未渲染/模态吞输入）。后台任务不阻塞响应；working 排队正常也 >3s，
    // 故日志措辞区分「待核对」而非定论丢失。补齐 r25 注入日志链的最后一环。
    let session_id = agent_of(&state, &id).await.ok().and_then(|a| a.session_id);
    if let Some(sid) = session_id {
        let root = state.cfg.claude_root.clone();
        let id2 = id.clone();
        let before = crate::drivers::claude::session::session_mtime(&root, &sid).await;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            let after = crate::drivers::claude::session::session_mtime(&root, &sid).await;
            match (before, after) {
                (Some(b), Some(a)) if b == a => {
                    tracing::warn!(
                        agent = %id2,
                        session = %sid,
                        "send post-check: jsonl 3s 未推进（working 排队中或注入丢失，需核对）"
                    );
                }
                (None, None) => {
                    tracing::warn!(agent = %id2, session = %sid, "send post-check: 找不到会话 jsonl，无法验证落盘");
                }
                _ => {}
            }
        });
    }
    Ok(StatusCode::NO_CONTENT)
}

/// C2 中断：PTY 中断语义——resize 抖动 + Esc 补发（反馈轮 19：原实现裸发单字节
/// Esc 绕过 interrupt 完整路径，工具执行期被吞 2/4 轮）；成功后打中断标记并采样
/// jsonl mtime 基线，group 计算据此覆盖为空闲，防 CLI state 滞留 working 锁死发送。
async fn interrupt_agent(
    State(state): State<SharedState>,
    AxumPath((driver, id)): AxumPath<(String, String)>,
) -> Result<StatusCode, AppError> {
    ensure_driver(&driver)?;
    crate::drivers::claude::attach::validate_id(&id)
        .map_err(|e| AppError::bad(format!("非法 agent id: {e}")))?;
    let routes = state.message_routes.read().expect("routes lock").clone();
    let Some(sender) = routes.get(&driver) else {
        return Err(AppError::not_found(format!("未知 driver: {driver}")));
    };
    sender
        .interrupt(&id)
        .await
        .map_err(|e| AppError::bad(format!("中断失败: {e}")))?;
    // r69 竞态修复：基线必须落在中断收尾写入（abort 记录）之后——旧实现固定睡
    // 2s 单次采样，收尾写入晚于采样点时自己就满足「mtime 越过基线且 30s 活跃」
    // 解除判据，标记在下次 list 即被解除，回落 CLI 滞留的 working（r69 实测
    // 0/N 转闲）。改为轮询至 jsonl 静默（观察满 4s 且 2s 无前进）再取基线；
    // 10s 上限兜底中断失败场景——输出持续流动时取到的基线随即被越过，解除判据
    // 放行恢复权威 working，不残留假空闲。机制见 session::sample_quiet_mtime。
    let agent = agent_of(&state, &id).await.ok();
    if let Some(session_id) = agent.as_ref().and_then(|a| a.session_id.clone()) {
        let baseline = crate::drivers::claude::session::sample_quiet_mtime(
            &state.cfg.claude_root,
            &session_id,
            2_000,
            4_000,
            10_000,
        )
        .await;
        if let Some(t) = baseline {
            state
                .interrupted_at
                .lock()
                .expect("interrupted_at lock")
                .insert(id, t);
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn agent_of(state: &SharedState, id: &str) -> Result<crate::models::AgentSummary, AppError> {
    let mut agents = state.driver.list(true).await?;
    state.apply_interrupt_overrides(&mut agents).await;
    let found = agents.into_iter().find(|a| a.id == id);
    found.ok_or_else(|| AppError::not_found(format!("agent {id} 不存在")))
}

fn ensure_driver(driver: &str) -> Result<(), AppError> {
    if driver != "claude" {
        return Err(AppError::not_found(format!("未知 driver: {driver}")));
    }
    Ok(())
}
