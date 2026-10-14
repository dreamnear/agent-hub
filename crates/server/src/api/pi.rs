//! pi 设置端点（agent-hub-pi-hub-agent P3 + P10）。
//!
//! P10 起单一事实源 = pi 原生配置（`~/.pi/agent/settings.json` 三键 +
//! `models.json` 自定义 provider 条目），hub 层 pi_settings.json 退役停写。
//! 写入经 `drivers::pi::piconf` 的 merge 语义（其他键/其他条目保留；ak 留空
//! 不覆盖）。脱敏红线：ak 读取即掩码，明文只在 PUT body → 写文件路径瞬时
//! 存在，响应与日志零明文。变更后异步重启助手生效（P3 语义不变）。
//! 删除类操作不经本端点（红线：不可逆操作用户亲自在 UI 操作）。

use axum::{extract::State, routing, Json, Router};
use serde::Deserialize;

use crate::{
    api::SharedState,
    drivers::pi::piconf::{self, PiAgentConf},
    error::AppError,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/api/pi/settings",
            routing::get(get_settings).put(put_settings),
        )
        .route("/api/pi/version", routing::get(get_version))
        .route("/api/pi/inject", routing::post(post_inject))
        .route("/api/pi/task-control", routing::post(post_task_control))
        .route("/api/pi/new-session", routing::post(post_new_session))
        .route("/api/pi/restart-status", routing::get(get_restart_status))
        .route("/api/search", routing::get(get_search))
}

/// P9：pi 版本读数（TTL 缓存探测；None = 未安装/不可执行，前端容错降级提示）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PiVersion {
    version: Option<String>,
}

async fn get_version(State(state): State<SharedState>) -> Json<PiVersion> {
    Json(PiVersion {
        version: state.pi.detect_version().await,
    })
}

fn agent_conf(state: &SharedState) -> PiAgentConf {
    PiAgentConf::new(state.pi.agent_dir.clone())
}

/// GET /api/pi/settings：原生三键 + provider 名下条目（ak 掩码态）。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsDto {
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    /// models.json 中 provider 名下条目（provider 未填或条目不存在 → null）
    provider_config: Option<crate::drivers::pi::piconf::ProviderEntry>,
    /// hub 侧功能开关：允许助手注入会话（授权双闸之一，默认关）
    allow_session_inject: bool,
    /// P13：允许助手控制定时任务（默认关——服务端闸防 prompt injection 诱导）
    allow_task_control: bool,
    /// P11：助手工作目录（null = 默认 $HOME/agent-hub）
    work_dir: Option<String>,
}

async fn get_settings(State(state): State<SharedState>) -> Result<Json<SettingsDto>, AppError> {
    let conf = agent_conf(&state);
    let native = conf.read_settings().await.map_err(AppError::from)?;
    let provider_config = match &native.default_provider {
        Some(name) => Some(conf.read_provider(name).await.map_err(AppError::from)?),
        None => None,
    };
    let flags = piconf::read_flags(&state.pi.settings_file).await;
    Ok(Json(SettingsDto {
        provider: native.default_provider,
        model: native.default_model,
        thinking: native.default_thinking_level,
        provider_config,
        allow_session_inject: flags.allow_session_inject,
        allow_task_control: flags.allow_task_control,
        work_dir: flags.work_dir,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PutSettingsBody {
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    /// hub 侧开关（bool 全量提交；None = 不动）
    allow_session_inject: Option<bool>,
    /// P13 同上
    allow_task_control: Option<bool>,
    /// P11 工作目录（空/None = 默认 $HOME/agent-hub；PUT 时预校验早反馈）
    work_dir: Option<String>,
    /// models.json 条目段：任一非空才写；apiKey 明文入口（写入后不落内存/日志）
    base_url: Option<String>,
    api: Option<String>,
    api_key: Option<String>,
    models: Option<Vec<String>>,
}

/// thinking 六档白名单（docs/settings.md defaultThinkingLevel 枚举）。
const THINKING_LEVELS: [&str; 6] = ["off", "minimal", "low", "medium", "high", "xhigh"];

fn normalize(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// PUT /api/pi/settings：表单全量提交（未改字段原值提交回来）。
/// - provider/model/thinking → 原生 settings.json（None/空串 = 清除键，回退 pi 默认）
/// - baseUrl/api/models 任一非空 → 写 models.json 条目（条目名 = provider 值，
///   内建名同名条目 = pi 文档语义的 override）；apiKey 非空覆盖、空保留
/// - 写后异步重启助手生效（P3 语义不变）
async fn put_settings(
    State(state): State<SharedState>,
    Json(body): Json<PutSettingsBody>,
) -> Result<Json<SettingsDto>, AppError> {
    let provider = normalize(body.provider);
    let model = normalize(body.model);
    let thinking = normalize(body.thinking);
    if let Some(t) = &thinking {
        if !THINKING_LEVELS.contains(&t.as_str()) {
            return Err(AppError::bad(format!(
                "thinking 取值限 {THINKING_LEVELS:?}，实为 {t}"
            )));
        }
    }
    let base_url = normalize(body.base_url);
    let api = normalize(body.api);
    // F-1：空数组视同 None（Some([]) ≠ None 曾使「未用自定义段」误判 wants_entry）
    let models = body.models.map(|v| {
        let ids: Vec<String> = v
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        (!ids.is_empty()).then_some(ids)
    });
    let models = models.flatten();
    let conf = agent_conf(&state);

    // 校验前置（观察项①：任何 400 不得发生在 write_settings 之后——消除部分写入）：
    // 条目段任一非空 = 用户在用自定义段，provider 名必填
    let wants_entry = base_url.is_some() || api.is_some() || models.is_some();
    let entry = if wants_entry {
        let Some(name) = provider.as_ref() else {
            return Err(AppError::bad(
                "自定义 baseUrl/协议/ak 需要先填 provider 名（条目键）",
            ));
        };
        Some((
            name.clone(),
            base_url.clone(),
            api.clone(),
            body.api_key.clone(),
            models.clone(),
        ))
    } else {
        None
    };

    // model-key-bug 写前拦截（观察项①：400 在任何 write 之前）：Model ∉ 自定义
    // 条目清单 = 死配置（pi 按条目清单解析模型归属，无归属则 key 解析不到）。
    // 条目「提交后形态」的清单：wants_entry 用本次提交（留空 = 既有清单）；非
    // wants_entry 但 models.json 已有同名条目 → 既有清单；无条目（内建 provider
    // 面，走 auth.json/内建注册表）放行。
    if let (Some(p), Some(m)) = (provider.as_deref(), model.as_deref()) {
        let submitted_now: Option<Vec<String>> = if wants_entry {
            entry
                .as_ref()
                .and_then(|(n, _, _, _, ids)| (n == p).then(|| ids.clone()))
                .flatten()
        } else {
            None
        };
        let entry_models = match submitted_now {
            Some(ids) => Some(ids),
            None => {
                let existing = conf.read_provider(p).await.map_err(AppError::from)?;
                let has_entry = existing.base_url.is_some()
                    || existing.api.is_some()
                    || !existing.models.is_empty()
                    || existing.api_key_masked.is_some();
                has_entry
                    .then_some(existing.models)
                    .filter(|m| !m.is_empty())
            }
        };
        crate::drivers::pi::piconf::validate_model_in_entry(p, m, entry_models.as_deref())
            .map_err(AppError::bad)?;
    }
    // 新建自定义条目未给 key → 拦截（schema 要求 apiKey 必填，缺字段/空串都会让
    // 整个 models.json 拒载——写进去即不可用）
    if wants_entry {
        if let (Some(p), None) = (
            provider.as_deref(),
            body.api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        ) {
            let existing = conf.read_provider(p).await.map_err(AppError::from)?;
            let has_stored_key = existing.api_key_masked.is_some();
            let is_new = existing.base_url.is_none() && existing.api.is_none() && !has_stored_key;
            if is_new {
                return Err(AppError::bad(
                    "新建自定义 provider 条目需要 API key（models.json 条目缺 key 会导致 pi 拒载全部自定义配置）",
                ));
            }
        }
    }
    // 存量自检：models.json 任一条目 apiKey 空串 → pi schema 拒载整个文件（全部
    // 自定义条目失效）——写前拦截并指名修复（D3：存量手工条目零改写，修复交用户）
    let empty_keys =
        crate::drivers::pi::piconf::find_empty_api_key_entries(&conf.models_file()).await;
    if !empty_keys.is_empty() {
        return Err(AppError::bad(format!(
            "models.json 存在 apiKey 为空的条目：{empty_keys:?}（pi 会拒载整个 models.json，导致全部自定义 provider 失效）。请先为这些条目补上有效 key（或删除条目）后重试"
        )));
    }

    conf.write_settings(&crate::drivers::pi::piconf::NativeSettings {
        default_provider: provider.clone(),
        default_model: model.clone(),
        default_thinking_level: thinking.clone(),
    })
    .await
    .map_err(AppError::from)?;

    if let Some((name, base_url, api, api_key, models)) = entry {
        conf.write_provider(
            &name,
            base_url.as_deref(),
            api.as_deref(),
            api_key.as_deref(),
            models.as_deref(),
        )
        .await
        .map_err(AppError::from)?;
    }

    // P11 工作目录预校验（早反馈；ensure_started 启动前再校验兜底手改文件）
    let work_dir_raw = normalize(body.work_dir.clone());
    if work_dir_raw.is_some() {
        piconf::resolve_work_dir(work_dir_raw.as_deref())
            .await
            .map_err(|e| AppError::bad(e.to_string()))?;
    }
    // hub 侧 flags 落盘（开关/工作目录；字段 None = 表单未含此段不动）
    if body.allow_session_inject.is_some()
        || body.allow_task_control.is_some()
        || body.work_dir.is_some()
    {
        let mut flags = piconf::read_flags(&state.pi.settings_file).await;
        if let Some(v) = body.allow_session_inject {
            flags.allow_session_inject = v;
        }
        if let Some(v) = body.allow_task_control {
            flags.allow_task_control = v;
        }
        flags.work_dir = work_dir_raw.clone();
        if let Some(dir) = state.pi.settings_file.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| AppError::from(anyhow::anyhow!(e)))?;
        }
        let raw =
            serde_json::to_string_pretty(&flags).map_err(|e| AppError::from(anyhow::anyhow!(e)))?;
        let tmp = state.pi.settings_file.with_extension("json.tmp");
        tokio::fs::write(&tmp, raw)
            .await
            .map_err(|e| AppError::from(anyhow::anyhow!(e)))?;
        tokio::fs::rename(&tmp, &state.pi.settings_file)
            .await
            .map_err(|e| AppError::from(anyhow::anyhow!(e)))?;
    }

    // 变更重启助手生效（P3）；重启失败不影响设置已保存的事实
    let pi = state.pi.clone();
    tokio::spawn(async move {
        if let Err(e) = pi.stop().await {
            tracing::warn!(error = %e, "pi 助手重启（stop 段）失败");
        }
        if let Err(e) = pi.ensure_started().await {
            tracing::warn!(error = %e, "pi 助手重启（start 段）失败");
        }
    });

    // 响应 = GET 形态（ak 掩码；脱敏红线）
    let provider_config = match &provider {
        Some(name) => Some(conf.read_provider(name).await.map_err(AppError::from)?),
        None => None,
    };
    let flags = piconf::read_flags(&state.pi.settings_file).await;
    Ok(Json(SettingsDto {
        provider,
        model,
        thinking, // 已 normalize（响应原样回显提交值）
        provider_config,
        allow_session_inject: flags.allow_session_inject,
        allow_task_control: flags.allow_task_control,
        work_dir: flags.work_dir,
    }))
}

// —— P10 增量：助手注入（授权双闸）与跨会话搜索 ——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InjectBody {
    driver: String,
    id: String,
    text: String,
}

/// POST /api/pi/inject：助手向指定会话注入消息（授权双闸的服务端闸）。
/// 开关关 → 403；interactive 会话 → 403（R7：前台 TUI 注入纪律）；
/// 通过后走既有 route_send 链路（注入行为与 UI 发送完全同构）。
/// 返回 {status:"accepted"|"failed", message?}——助手可如实转告用户。
async fn post_inject(
    State(state): State<SharedState>,
    Json(body): Json<InjectBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !piconf::read_flags(&state.pi.settings_file)
        .await
        .allow_session_inject
    {
        return Err(AppError::forbidden(
            "助手会话注入未开启（设置 → Hub 助手 → 允许助手注入会话）",
        ));
    }
    let text = body.text.trim();
    if text.is_empty() {
        return Err(AppError::bad("消息为空"));
    }
    // interactive 红线：目标条目 kind=interactive 拒绝（前台 TUI 注入纪律 R7）
    let d = state
        .drivers
        .get(&body.driver)
        .map_err(|e| AppError::bad(e.to_string()))?;
    if let Some(summary) = d.get(&body.id).await.map_err(AppError::from)? {
        if summary.kind.as_deref() == Some("interactive") {
            return Err(AppError::forbidden(
                "interactive 前台会话不可注入（纪律 R7）——请在终端亲手操作",
            ));
        }
    }
    // 失败也以 200 + status=failed 返回（extension 工具语义：助手要能拿到
    // 结构化失败原因转告用户，而非 HTTP 错误丢细节）。
    // std 锁守卫不跨 await：先克隆路由表再异步发送（Send 红线）
    let routes = state.message_routes.read().expect("routes lock").clone();
    match crate::drivers::route_send(&routes, &body.driver, &body.id, text).await {
        Ok(()) => Ok(Json(serde_json::json!({
            "status": "accepted",
            "message": "消息已受理注入（若目标会话正忙，处理结果以会话实际输出为准）",
        }))),
        Err(e) => Ok(Json(serde_json::json!({
            "status": "failed",
            "message": e.to_string(),
        }))),
    }
}

// —— assistant-resume A5：手动新建会话 ——

/// POST /api/pi/new-session：助手手动新建会话。pi RPC new_session 切内存态新会话
/// （首条消息落盘后成为下次 spawn `--continue` 的恢复目标；零消息重启回退上一
/// 会话——pi 0.73.1 实测边界），feed 清空；前端成功后主动 invalidate 消息查询
/// 刷新空态。未运行时幂等成功（无条目时 UI 无此入口）。
async fn post_new_session(
    State(state): State<SharedState>,
) -> Result<Json<serde_json::Value>, AppError> {
    state.pi.new_session().await.map_err(AppError::from)?;
    Ok(Json(serde_json::json!({ "status": "ok" })))
}

// —— restart-feedback：保存并重启的终态反馈 ——

/// GET /api/pi/restart-status：最近一次助手真实 spawn 的终态（None=尚无记录）。
/// 设置页保存后轮询此端点，按「新于本地快照」的记录收敛「重启中」到成功/失败终态。
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RestartStatusDto {
    last_result: Option<crate::drivers::pi::StartResult>,
}

async fn get_restart_status(State(state): State<SharedState>) -> Json<RestartStatusDto> {
    Json(RestartStatusDto {
        last_result: state.pi.last_start_result(),
    })
}

// —— P13：助手定时任务控制（服务端闸，与 inject 同款）——

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskControlBody {
    /// "toggle"（启停，需 enabled）| "run"（手动触发）| "create"（助手 cron，需
    /// name/cron/prompt）| "delete"（删任务）
    action: String,
    /// toggle/run/delete 必填；create 忽略（id 服务端生成）
    #[serde(default)]
    task_id: String,
    /// toggle 必填（true=启用 false=停用）；run 忽略
    enabled: Option<bool>,
    /// create 必填：任务名 / cron 表达式（5 段，对齐 TaskManager）/ 注入 prompt
    name: Option<String>,
    cron: Option<String>,
    prompt: Option<String>,
}

/// POST /api/pi/task-control：助手启停/手动触发定时任务（P13 服务端闸）。
/// 闸关 → 403（默认关；评审 MINOR-2：工具直调共用 /api/tasks/* 无服务端闸，
/// 与 inject 不对称，prompt injection 场景可被诱导写操作——专用端点收口，
/// UI 的 /api/tasks/* 不受影响）。toggle 复用 TaskManager.upsert；run 复用
/// 调度器同款 fire（串行守卫/Run 记录/异步执行全同）。任务不存在 → 400。
/// hub_tasks_list 走只读 GET /api/tasks，不设闸。
async fn post_task_control(
    State(state): State<SharedState>,
    Json(body): Json<TaskControlBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !piconf::read_flags(&state.pi.settings_file)
        .await
        .allow_task_control
    {
        return Err(AppError::forbidden(
            "助手任务控制未开启（设置 → Hub 助手 → 允许助手控制定时任务）",
        ));
    }
    let task_id = body.task_id.trim();
    match body.action.as_str() {
        // assistant-cron C1：助手会话内建 cron——落 TaskManager（tasks.json 持久）
        // 为 Inject{pi, hub-assistant} 任务；到点 Inject 走 pi send（followUp 恒
        // 携带：忙态入队/空闲即处理 = 「唤醒既有会话」语义）。source 标记
        // "assistant" 供 Tasks 分区展示（C4）。闸在函数入口统一判定。
        "create" => {
            let name = body.name.as_deref().map(str::trim).unwrap_or("");
            let cron = body.cron.as_deref().map(str::trim).unwrap_or("");
            let prompt = body.prompt.as_deref().map(str::trim).unwrap_or("");
            if name.is_empty() {
                return Err(AppError::bad("name 不能为空"));
            }
            if cron.is_empty() {
                return Err(AppError::bad("cron 不能为空"));
            }
            if prompt.is_empty() {
                return Err(AppError::bad("prompt 不能为空"));
            }
            let spec = crate::tasks::TaskSpec {
                id: format!("task-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                name: name.to_string(),
                cron: cron.to_string(),
                enabled: true,
                source: Some("assistant".into()),
                action: crate::tasks::TaskAction::Inject {
                    driver: "pi".into(),
                    session_id: crate::drivers::pi::ASSISTANT_ID.into(),
                    text: prompt.to_string(),
                },
            };
            super::tasks::validate(&spec).await?;
            state
                .tasks
                .upsert(spec.clone())
                .map_err(|e| AppError::bad(format!("任务落盘失败: {e}")))?;
            Ok(Json(serde_json::json!({
                "id": spec.id,
                "name": spec.name,
                "cron": spec.cron,
                "enabled": spec.enabled,
                "source": spec.source,
                "note": "created; fires inject the prompt into this assistant session",
            })))
        }
        // assistant-cron C2：会话内删除定时——删后到点不再触发（remove 同步落盘）
        "delete" => {
            if task_id.is_empty() {
                return Err(AppError::bad("taskId 不能为空"));
            }
            let removed = state
                .tasks
                .remove(task_id)
                .map_err(|e| AppError::bad(format!("任务删除落盘失败: {e}")))?;
            if !removed {
                return Err(AppError::bad(format!("任务不存在: {task_id}")));
            }
            Ok(Json(serde_json::json!({ "deleted": task_id })))
        }
        "toggle" => {
            if task_id.is_empty() {
                return Err(AppError::bad("taskId 不能为空"));
            }
            let Some(enabled) = body.enabled else {
                return Err(AppError::bad("toggle 需要 enabled（true=启用 false=停用）"));
            };
            let mut spec = state
                .tasks
                .get(task_id)
                .ok_or_else(|| AppError::bad(format!("任务不存在: {task_id}")))?;
            spec.enabled = enabled;
            state
                .tasks
                .upsert(spec.clone())
                .map_err(|e| AppError::bad(format!("任务落盘失败: {e}")))?;
            Ok(Json(serde_json::json!({
                "id": spec.id,
                "name": spec.name,
                "enabled": spec.enabled,
            })))
        }
        "run" => {
            if task_id.is_empty() {
                return Err(AppError::bad("taskId 不能为空"));
            }
            let spec = state
                .tasks
                .get(task_id)
                .ok_or_else(|| AppError::bad(format!("任务不存在: {task_id}")))?;
            // 触发时刻 = now（与 UI 手动触发完全同构）
            crate::tasks::fire(state.clone(), &spec, chrono::Local::now());
            Ok(Json(serde_json::json!({
                "fired": spec.id,
                "note": "task fired; check its runs/log for outcome",
            })))
        }
        other => Err(AppError::bad(format!(
            "action 取值限 \"toggle\" | \"run\" | \"create\" | \"delete\"，实为 {other}"
        ))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchQuery {
    q: String,
    /// 每会话扫描深度（默认 100 条，上限 500）
    limit: Option<usize>,
}

/// 跨会话关键词搜索（P10 增量）：扫全驱动各会话近期消息，返回命中会话 +
/// 片段。参数化限深防全量扫描拖垮：q 最短 2 字符、每会话默认 100 条、
/// 并发 8、总命中 50 截断。
async fn get_search(
    State(state): State<SharedState>,
    axum::extract::Query(q): axum::extract::Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let needle = q.q.trim().to_lowercase();
    if needle.chars().count() < 2 {
        return Err(AppError::bad("搜索词至少 2 个字符"));
    }
    let depth = q.limit.unwrap_or(100).clamp(1, 500);
    let agents = state.drivers.list_all(true).await.map_err(AppError::from)?;
    let iter = agents
        .into_iter()
        .filter(|a| a.kind.as_deref() != Some("interactive"));
    let mut hits: Vec<serde_json::Value> = Vec::new();
    for a in iter {
        if hits.len() >= 50 {
            break;
        }
        let Ok(d) = state.drivers.get(&a.driver) else {
            continue;
        };
        let msgs = match d.full_history(&a.id).await {
            Ok(m) => m,
            Err(_) => continue,
        };
        let window = msgs.iter().rev().take(depth);
        let mut matches: Vec<serde_json::Value> = Vec::new();
        for m in window {
            let Some(text) = &m.text else { continue };
            let lower = text.to_lowercase();
            if let Some(pos) = lower.find(&needle) {
                // 字符边界安全的命中窗：命中点前 40 字符起、至多 120 字符片段
                let byte_from = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .rfind(|i| *i + 40 <= pos || *i == 0)
                    .unwrap_or(0);
                let snippet: String = text[byte_from..].chars().take(120).collect();
                matches.push(serde_json::json!({
                    "kind": m.kind.as_str(),
                    "snippet": snippet,
                    "toolName": m.tool_name,
                }));
                if matches.len() >= 3 {
                    break; // 每会话至多 3 条命中片段（防长会话刷屏）
                }
            }
        }
        if !matches.is_empty() {
            hits.push(serde_json::json!({
                "driver": a.driver,
                "id": a.id,
                "name": a.name,
                "cwd": a.cwd,
                "group": a.group,
                "matches": matches,
            }));
        }
    }
    Ok(Json(
        serde_json::json!({ "hits": hits, "total": hits.len() }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 请求体反序列化：全字段可选（表单全量/部分更新两形态）。
    #[test]
    fn put_body_parses_partial() {
        let v: PutSettingsBody = serde_json::from_str(r#"{"provider":"anthropic"}"#).unwrap();
        assert_eq!(v.provider.as_deref(), Some("anthropic"));
        assert!(v.model.is_none());
        let v: PutSettingsBody = serde_json::from_str("{}").unwrap();
        assert!(v.provider.is_none() && v.base_url.is_none() && v.api_key.is_none());
        // models 数组 + apiKey 形态
        let v: PutSettingsBody = serde_json::from_str(
            r#"{"provider":"lmapi","baseUrl":"https://x","api":"openai-completions","apiKey":"sk-1","models":["a","b"]}"#,
        )
        .unwrap();
        assert_eq!(v.models.unwrap(), vec!["a", "b"]);
        assert_eq!(v.api_key.as_deref(), Some("sk-1"));
    }

    /// thinking 白名单。
    #[test]
    fn thinking_levels_whitelist() {
        assert!(THINKING_LEVELS.contains(&"high"));
        assert!(!THINKING_LEVELS.contains(&"ultra"));
    }
}
