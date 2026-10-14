//! 定时任务 API（agent-hub-daemon-capabilities）：任务 CRUD + 手动触发 + Run 历史。
//! 全部走 TaskManager（存储原子写 / Run 滚动 / 串行守卫），校验与调度器共用 cron 判定。

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use chrono::Local;

use super::SharedState;
use crate::error::AppError;
use crate::tasks::{fire, next_run_at, TaskAction, TaskRun, TaskSpec};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/tasks", get(list_tasks).post(create_task))
        .route(
            "/api/tasks/{id}",
            axum::routing::put(update_task).delete(delete_task),
        )
        .route("/api/tasks/{id}/run", post(run_task))
        .route("/api/tasks/{id}/runs", get(task_runs))
        .route("/api/tasks/scheduler-status", get(scheduler_status))
}

/// 调度权可观测（agent-hub-schedule-lock S2）：true = 本实例持锁调度中；
/// false = standby（他实例持锁，本实例调度禁用，UI 徽标指示）。
async fn scheduler_status(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "primary": state.tasks.scheduler_primary() }))
}

#[derive(serde::Serialize)]
struct TaskDto {
    #[serde(flatten)]
    spec: TaskSpec,
    /// 本地时区 RFC3339；禁用/cron 非法 → null
    next_run_at: Option<String>,
}

fn dto(spec: TaskSpec) -> TaskDto {
    let next_run_at = if spec.enabled {
        next_run_at(&spec.cron, Local::now())
    } else {
        None
    };
    TaskDto { spec, next_run_at }
}

/// 建档/更新共用校验：名称、cron、动作字段；spawn 的 cwd 复用 API 层信任边界校验。
pub(crate) async fn validate(spec: &TaskSpec) -> Result<(), AppError> {
    if spec.name.trim().is_empty() {
        return Err(AppError::bad("任务名称不能为空"));
    }
    if !crate::tasks::cron_valid(&spec.cron) {
        return Err(AppError::bad(format!("cron 表达式不合法: {}", spec.cron)));
    }
    match &spec.action {
        TaskAction::Spawn {
            driver,
            cwd,
            prompt_template,
            ..
        } => {
            if driver.trim().is_empty() {
                return Err(AppError::bad("driver 不能为空"));
            }
            if prompt_template.trim().is_empty() {
                return Err(AppError::bad("prompt 模板不能为空"));
            }
            super::agents::validate_cwd(cwd).await?;
        }
        TaskAction::Inject {
            driver,
            session_id,
            text,
        } => {
            if driver.trim().is_empty() {
                return Err(AppError::bad("driver 不能为空"));
            }
            if session_id.trim().is_empty() {
                return Err(AppError::bad("sessionId 不能为空"));
            }
            if text.trim().is_empty() {
                return Err(AppError::bad("注入文本不能为空"));
            }
        }
    }
    Ok(())
}

async fn list_tasks(State(state): State<SharedState>) -> Json<Vec<TaskDto>> {
    Json(state.tasks.list().into_iter().map(dto).collect())
}

async fn create_task(
    State(state): State<SharedState>,
    Json(mut spec): Json<TaskSpec>,
) -> Result<Json<TaskDto>, AppError> {
    validate(&spec).await?;
    spec.id = format!("task-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    state
        .tasks
        .upsert(spec.clone())
        .map_err(|e| AppError::bad(format!("任务落盘失败: {e}")))?;
    Ok(Json(dto(spec)))
}

async fn update_task(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(mut spec): Json<TaskSpec>,
) -> Result<Json<TaskDto>, AppError> {
    if state.tasks.get(&id).is_none() {
        return Err(AppError::bad(format!("任务不存在: {id}")));
    }
    spec.id = id;
    validate(&spec).await?;
    state
        .tasks
        .upsert(spec.clone())
        .map_err(|e| AppError::bad(format!("任务落盘失败: {e}")))?;
    Ok(Json(dto(spec)))
}

async fn delete_task(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !state
        .tasks
        .remove(&id)
        .map_err(|e| AppError::bad(format!("任务删除落盘失败: {e}")))?
    {
        return Err(AppError::bad(format!("任务不存在: {id}")));
    }
    // Run 日志保留（requirement 决策 7：审计链），只删任务表
    Ok(Json(serde_json::json!({ "deleted": id })))
}

async fn run_task(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let spec = state
        .tasks
        .get(&id)
        .ok_or_else(|| AppError::bad(format!("任务不存在: {id}")))?;
    // 复用调度器同款 fire（串行守卫/Run 记录/异步执行全同）；触发时刻 = now
    fire(state.clone(), &spec, Local::now());
    Ok(Json(serde_json::json!({ "fired": id })))
}

async fn task_runs(State(state): State<SharedState>, Path(id): Path<String>) -> Json<Vec<TaskRun>> {
    Json(state.tasks.runs(&id))
}
