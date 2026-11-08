//! 事件驱动的会话监听/提醒机制（watcher hook，ADR-0017）：hub 内置消费者。
//!
//! 事件源复用 `plugins::hooks` 观察档发射点（进程内观察者槽），状态/用量类条件
//! 由本模块自身状态巡检 diff 出穿越点；命中规则后向 hub 助手注入一条提醒。
//! 插件面（observer-only / RPC / 权限）零改动——本模块不注册插件、不暴露端点。
//!
//! - `rules`：`watcher.json` schema、加载与逐条降级、作用域/内容匹配、渲染
//! - `engine`：会话状态表、冷却/去重/限流、触发→注入编排

pub mod engine;
pub mod rules;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::AppState;
use crate::drivers::pi::ASSISTANT_ID;
use crate::plugins::hooks;

use engine::Engine;

/// 防重入守卫：hub / node 两形态共用启动序列，`spawn` 全进程只生效一次。
static STARTED: AtomicBool = AtomicBool::new(false);
/// 首次 tick 延迟：与既有 idle 探针（5s）错峰，降低并发 list 降级概率。
const FIRST_TICK_DELAY: Duration = Duration::from_secs(2);

/// 装配入口（`spawn_host_backends` 调用）：读配置 → 注册内部观察者 → 巡检循环。
/// 配置缺失/坏/禁用 → 零注入 + 一条日志（降级方向一律「少打扰」）。
pub fn spawn(state: &Arc<AppState>) {
    if STARTED.swap(true, Ordering::SeqCst) {
        tracing::debug!("hub-watcher: 已启动，忽略重复 spawn");
        return;
    }
    let state = state.clone();
    tokio::spawn(async move { run(state).await });
}

async fn run(state: Arc<AppState>) {
    let cfg = rules::WatchConfig::load(&state.cfg.watch_file).await;
    if !cfg.enabled || cfg.rules.is_empty() {
        tracing::info!(
            enabled = cfg.enabled,
            rules = cfg.rules.len(),
            "hub-watcher: 未启用（零注入）"
        );
        return;
    }
    let engine = Arc::new(StdMutex::new(Engine::new(cfg)));
    let poll_secs = lock(&engine).state_poll_secs();

    // 内部观察者（hooks 发射点旁挂）：Weak 回指防环；panic 由 hooks 侧 catch_unwind 隔离
    let weak = Arc::downgrade(&state);
    let obs_engine = engine.clone();
    hooks::set_internal_observer(Arc::new(move |hook: &str, params: &Value| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        on_hook_event(&state, &obs_engine, hook, params);
    }));

    tracing::info!(poll_secs, "hub-watcher: 已启用");
    tokio::time::sleep(FIRST_TICK_DELAY).await;
    loop {
        tick(&state, &engine).await;
        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }
}

/// 状态巡检一次：list_all 快照 → 引擎判定 → 开房 → 注入。
/// 空观测/单驱动失败（注册表已 fail-soft 成空集）＝ 无事发生（引擎侧不清态）。
async fn tick(state: &Arc<AppState>, engine: &Arc<StdMutex<Engine>>) {
    let Ok(agents) = state.drivers.list_all(true).await else {
        return;
    };
    let outcome = lock(engine).on_snapshot(&agents, Instant::now());
    for plan in outcome.rooms {
        match state.drivers.get(&plan.driver) {
            Ok(driver) => {
                // 幂等：claude = 起该会话 tail（房间首建才起）；dsh = 建房
                driver.ensure_chat_room(&plan.room_key);
                tracing::debug!(
                    driver = %plan.driver,
                    session = %plan.session_key,
                    "hub-watcher: 为消息类规则打开会话房间"
                );
            }
            Err(e) => tracing::debug!(
                driver = %plan.driver,
                error = %e,
                "hub-watcher: 未知驱动，跳过开房"
            ),
        }
    }
    dispatch_injects(state, outcome.texts);
}

/// hooks 内部观察者回调（同步、微秒级；注入动作异步 spawn）。
fn on_hook_event(
    state: &Arc<AppState>,
    engine: &Arc<StdMutex<Engine>>,
    hook: &str,
    params: &Value,
) {
    match hook {
        "onMessageAppend" => {
            let Some(label) = params.get("sessionId").and_then(Value::as_str) else {
                return;
            };
            let Some(kind) = params.get("kind").and_then(Value::as_str) else {
                return;
            };
            let text = params.get("text").and_then(Value::as_str).unwrap_or("");
            let texts = lock(engine).on_message(label, kind, text, Instant::now());
            dispatch_injects(state, texts);
        }
        "onSessionClose" => {
            let Some(label) = params.get("sessionId").and_then(Value::as_str) else {
                return;
            };
            lock(engine).on_session_close(label);
        }
        _ => {}
    }
}

fn dispatch_injects(state: &Arc<AppState>, texts: Vec<String>) {
    if texts.is_empty() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::debug!("hub-watcher: 无 tokio 运行时，提醒丢弃");
        return;
    };
    for text in texts {
        let state = state.clone();
        handle.spawn(async move { inject(&state, text).await });
    }
}

/// 触达：复用既有会话注入路径（pi `prompt_cmd` 恒定 followUp → 忙时入队、空闲
/// 后处理，不丢）。未运行 → `ensure_started` 一次后重试一次；仍失败只 warn +
/// 计数（at-most-once，不重试、不落盘）。
async fn inject(state: &Arc<AppState>, text: String) {
    let Ok(pi) = state.drivers.get("pi") else {
        tracing::warn!("hub-watcher: pi 驱动未注册，提醒丢弃");
        return;
    };
    match pi.send(ASSISTANT_ID, &text).await {
        Ok(()) => tracing::info!(length = text.chars().count(), "hub-watcher: 提醒已投递"),
        Err(e) if pi.is_finished(ASSISTANT_ID) => {
            tracing::info!(error = %e, "hub-watcher: Hub 助手未运行，尝试拉起一次");
            match state.pi.ensure_started().await {
                Ok(_) => match pi.send(ASSISTANT_ID, &text).await {
                    Ok(()) => tracing::info!("hub-watcher: 拉起后提醒已投递"),
                    Err(e2) => tracing::warn!(
                        error = %e2,
                        "hub-watcher: 拉起后注入仍失败，丢弃（at-most-once）"
                    ),
                },
                Err(e2) => tracing::warn!(error = %e2, "hub-watcher: Hub 助手拉起失败，丢弃提醒"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "hub-watcher: 注入失败，丢弃（at-most-once）"),
    }
}

fn lock(engine: &Arc<StdMutex<Engine>>) -> std::sync::MutexGuard<'_, Engine> {
    engine.lock().unwrap_or_else(|e| e.into_inner())
}
