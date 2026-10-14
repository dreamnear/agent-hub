//! harness 清单 REST（agent-hub-settings 批A 需求5/6）：`GET /api/harness` 探测 +
//! `POST /api/harness/{name}` 一键加入 ACP 配置。失效项由服务端标 `alive:false`，
//! 默认过滤在前端（见 HarnessPanel）。

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::SharedState,
    config::{self, AcpAgentConfig},
    error::AppError,
    harness,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/harness", get(list))
        .route("/api/harness/{name}", post(add))
}

async fn list(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let agents = state.acp.agents();
    Json(json!({ "harnesses": harness::discover(&agents).await }))
}

#[derive(Debug, Deserialize)]
pub struct AddBody {
    /// 探测出的可执行路径（不走 shell，数组传参）
    pub path: String,
}

/// 加入 ACP agent 配置：写盘 config.toml 的 `[[acp.agents]]` + 挂进运行时清单
/// （不重启即可用）。name 必须命中已知清单（`harness::kind_of`），顺带挡穿越/注入。
async fn add(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(body): Json<AddBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    let kind = harness::kind_of(&name)
        .ok_or_else(|| AppError::bad(format!("未知 harness: {name}（不在已知清单内）")))?;
    if kind != harness::HarnessKind::Acp {
        // CLI 类进 [[acp.agents]] 会在建会话握手时必败——服务端就拒，不只靠 UI 不给按钮
        return Err(AppError::bad(format!(
            "{name} 非 ACP harness，不可加入 agent 配置（hub 经原生驱动使用）"
        )));
    }
    if state.acp.agent(&name).is_some() {
        return Err(AppError::conflict("该 harness 已在 agent 配置中"));
    }
    if !harness::is_alive(std::path::Path::new(&body.path)) {
        return Err(AppError::bad("路径不存在或不可执行，拒绝加入配置"));
    }
    let agent = AcpAgentConfig {
        name: name.clone(),
        command: body.path,
        args: kind.args(),
        cwd: None,
        model: None,
    };
    // 先落盘后挂内存：写失败不留半拉子运行时状态
    harness::append_agent(&config::config_path(), &agent, &state.acp.agents())
        .map_err(AppError::bad)?;
    state.acp.add_agent(agent);
    Ok(Json(json!({ "added": true, "name": name })))
}
