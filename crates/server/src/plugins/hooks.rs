//! 生命周期钩子·观察档（B4，ADR-0014）：hub → backend 的 JSON-RPC notification
//! 下行分发。
//!
//! 语义红线（ADR-0014，实现即契约）：
//! - **观察档**：事后通知、只读、不可修改、不可阻塞——本模块对事件源零回压，
//!   发射点 fire-and-forget（`tokio::spawn`），发射失败只增丢弃计数；
//! - **at-most-once**：通知入每插件 FIFO 队列（上限默认 64），溢出丢弃 + 丢弃
//!   计数（`PluginView.hooks` 暴露，管理页可见）；不重试、不回压会话链路；
//! - **拦截档不开**：本模块只投递 [`super::api_surface::OBSERVER_HOOKS`] 名集，
//!   任何拦截形态（等待插件应答、修改事件载荷、拦截事件流）都不得在此出现——
//!   钩子处理中经 RPC 发起的一切调用照走既有权限域与 BR-15 确认，钩子不是绕行通道；
//! - **B1 零破坏**：无订阅插件时 `dispatch` 短路（一次注册表空查），会话链路
//!   新增成本趋近于零。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use super::api_surface::OBSERVER_HOOKS;
use super::registry::{now_rfc3339, PluginState};
use super::rpc::RpcChannel;

/// 每插件通知队列上限（ADR-0014 拍板结论 3；`[plugins] hook_queue_cap` 可调）。
pub const DEFAULT_QUEUE_CAP: usize = 64;
/// 单条通知投递预算：ensure_started（含握手）超时即放弃（at-most-once）。
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// 下行通知方法名：`hook/<钩子名>`（ADR-0014 §2 帧形态）。
pub fn hook_method(hook: &str) -> String {
    format!("hook/{hook}")
}

/// 一条待投递通知。
#[derive(Debug, Clone)]
struct HookNotification {
    hook: String,
    params: Value,
}

/// 单插件投递统计（`PluginView.hooks` 暴露）。
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HookStats {
    /// 该插件订阅的观察档钩子（即 manifest `contributes.hooks` 投影）
    pub subscribed: Vec<String>,
    /// 已投递条数（含启动以来全部）
    pub delivered: u64,
    /// 丢弃条数（队列溢出 / 投递失败；at-most-once 语义的可观察性）
    pub dropped: u64,
    /// 当前队列深度
    pub queue_depth: usize,
}

/// 每插件投递队列 + 计数（投递任务持有，dispatch 侧经共享句柄入队）。
struct PluginQueue {
    cap: usize,
    inner: StdMutex<QueueInner>,
}

#[derive(Default)]
struct QueueInner {
    items: VecDeque<HookNotification>,
    delivered: u64,
    dropped: u64,
}

impl PluginQueue {
    fn push(&self, n: HookNotification) -> bool {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.items.len() >= self.cap {
            g.dropped += 1;
            return false;
        }
        g.items.push_back(n);
        true
    }

    fn pop(&self) -> Option<HookNotification> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .items
            .pop_front()
    }

    fn stats(&self) -> (u64, u64, usize) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (g.delivered, g.dropped, g.items.len())
    }

    fn count_delivered(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).delivered += 1;
    }

    fn count_dropped(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).dropped += 1;
    }
}

/// 钩子分发总线： PluginManager 持有；drivers 经 [`global_dispatch`] 门面发射。
pub struct HooksBus {
    /// 回指 PluginManager（弱引用防环：manager 持 bus，bus 订阅查 manager 注册表）
    manager: StdMutex<Weak<super::PluginManager>>,
    queues: StdMutex<HashMap<String, Arc<PluginQueue>>>,
    queue_cap: usize,
}

impl std::fmt::Debug for HooksBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HooksBus")
            .field("queues", &self.queues.lock().map(|m| m.len()).unwrap_or(0))
            .finish()
    }
}

impl HooksBus {
    pub fn new(queue_cap: usize) -> Arc<Self> {
        Arc::new(Self {
            manager: StdMutex::new(Weak::new()),
            queues: StdMutex::new(HashMap::new()),
            queue_cap: queue_cap.max(1),
        })
    }

    /// 装配接线（PluginManager::new 调用）：回指弱引用。
    pub fn bind(&self, manager: &Arc<super::PluginManager>) {
        *self
            .manager
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Arc::downgrade(manager);
    }

    /// 发射一条观察档通知（fire-and-forget；同步返回前只做入队，投递异步进行）。
    ///
    /// 订阅判定：插件已启用、非故障态、manifest `contributes.hooks` 含该钩子
    /// （未声明不投递——不依赖插件拒绝，ADR-0014 §2）；无 backend 声明的插件
    /// 无法接收下行，跳过。
    pub fn dispatch(&self, hook: &str, params: Value) {
        if !OBSERVER_HOOKS.contains(&hook) {
            // 非观察档名（理论不可达：发射点全部在 hub 内）——防御性丢弃
            tracing::warn!(hook = %hook, "非观察档钩子名，丢弃（拦截档 v1 不开）");
            return;
        }
        let Some(manager) = self
            .manager
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upgrade()
        else {
            return;
        };
        for p in manager.registry.list() {
            if !p.entry.enabled || p.entry.state == PluginState::Fault {
                continue;
            }
            if !p.manifest.contributes.hooks.iter().any(|h| h == hook) {
                continue;
            }
            if p.manifest.backend.is_none() {
                continue;
            }
            let plugin_id = p.entry.id.clone();
            let queue = {
                let mut map = self.queues.lock().unwrap_or_else(|e| e.into_inner());
                map.entry(plugin_id.clone())
                    .or_insert_with(|| {
                        Arc::new(PluginQueue {
                            cap: self.queue_cap,
                            inner: StdMutex::new(QueueInner::default()),
                        })
                    })
                    .clone()
            };
            let payload = hook_payload_for(&p, hook, &params, &manager);
            if !queue.push(HookNotification {
                hook: hook.to_string(),
                params: payload,
            }) {
                tracing::debug!(plugin = %plugin_id, hook = %hook, "钩子队列已满，丢弃 + 计数");
                continue;
            }
            spawn_delivery(manager.clone(), plugin_id.clone(), queue);
        }
    }

    /// 单插件的钩子统计（`PluginView.hooks`；未订阅插件返回 None）。
    pub fn stats_for(&self, plugin_id: &str, subscribed: Vec<String>) -> Option<HookStats> {
        let map = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        let q = map.get(plugin_id)?;
        let (delivered, dropped, depth) = q.stats();
        Some(HookStats {
            subscribed,
            delivered,
            dropped,
            queue_depth: depth,
        })
    }

    /// 卸载/停用时清空队列与计数（队列随进程终止清空，ADR-0014 §2 隔离条款）。
    pub fn forget(&self, plugin_id: &str) {
        self.queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(plugin_id);
    }
}

/// 载荷最小化（ADR-0014 拍板结论 4）：`onMessageAppend` 默认只给
/// `{sessionId, seq}`；插件声明 `messages:read` 才附 `{kind, text≤2000}`。
/// 其余钩子载荷按事件源原样（发射点已最小化）。文本经 scrub 兜底。
fn hook_payload_for(
    p: &super::registry::InstalledPlugin,
    hook: &str,
    params: &Value,
    manager: &Arc<super::PluginManager>,
) -> Value {
    if hook != "onMessageAppend" {
        return params.clone();
    }
    let mut payload = json!({
        "sessionId": params.get("sessionId").cloned().unwrap_or(Value::Null),
        "seq": params.get("seq").cloned().unwrap_or(Value::Null),
    });
    let declares_messages = p
        .manifest
        .permissions
        .data
        .iter()
        .any(|d| d == "messages:read");
    if declares_messages {
        if let Some(kind) = params.get("kind") {
            payload["kind"] = kind.clone();
        }
        if let Some(text) = params.get("text").and_then(Value::as_str) {
            let scrubbed = manager.credentials.scrub(text);
            let clipped: String = scrubbed.chars().take(2000).collect();
            payload["text"] = Value::String(clipped);
        }
    }
    payload
}

/// 投递循环（每插件单飞：有队列即保证恰一个泵任务在跑）。
fn spawn_delivery(manager: Arc<super::PluginManager>, plugin_id: String, queue: Arc<PluginQueue>) {
    tokio::spawn(async move {
        while let Some(n) = queue.pop() {
            deliver_one(&manager, &plugin_id, &n, &queue).await;
        }
    });
}

/// 投递一条：懒启动 backend（hooks 是唤醒源之一）→ notification 下行。
/// 失败（启动失败/写入失败/超时）= 丢弃 + 计数，不重试（at-most-once）。
async fn deliver_one(
    manager: &Arc<super::PluginManager>,
    plugin_id: &str,
    n: &HookNotification,
    queue: &PluginQueue,
) {
    let Some(p) = manager.registry.get(plugin_id) else {
        queue.count_dropped();
        return;
    };
    if !p.entry.enabled {
        queue.count_dropped();
        return;
    }
    let run = match tokio::time::timeout(
        DELIVERY_TIMEOUT,
        manager.supervisor.ensure_started(
            &p,
            &manager.credentials,
            manager.handler(),
            manager.scrubber(),
        ),
    )
    .await
    {
        Ok(Ok(run)) => run,
        Ok(Err(e)) => {
            tracing::debug!(plugin = %plugin_id, hook = %n.hook, error = %e, "钩子投递：backend 启动失败，丢弃");
            queue.count_dropped();
            return;
        }
        Err(_) => {
            queue.count_dropped();
            return;
        }
    };
    let channel: Arc<RpcChannel> = run.channel.clone();
    if !channel.is_alive() {
        queue.count_dropped();
        return;
    }
    channel.notify(&hook_method(&n.hook), n.params.clone()).await;
    queue.count_delivered();
    tracing::debug!(plugin = %plugin_id, hook = %n.hook, "钩子通知已投递");
}

// ===== 全局门面（drivers 发射点用；AppState 装配时 set_global）=====

static GLOBAL: StdMutex<Option<Arc<HooksBus>>> = StdMutex::new(None);

/// 装配接线：hub 启动序列把 PluginManager 的总线挂为进程级门面
/// （drivers 无 AppState 句柄，经本门面 fire-and-forget）。
pub fn set_global(bus: Arc<HooksBus>) {
    if let Ok(mut g) = GLOBAL.lock() {
        *g = Some(bus);
    }
}

/// 发射点统一入口（ADR-0014 §3 埋点纪律：不散落直调 supervisor/rpc）。
/// 总线未装配或无订阅者时短路，事件源零感知。
pub fn global_dispatch(hook: &str, params: Value) {
    let bus = GLOBAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if let Some(bus) = bus {
        bus.dispatch(hook, params);
    }
}

/// 观察档名集再导出（发射点/文档单源）。
pub fn observer_hooks() -> &'static [&'static str] {
    &OBSERVER_HOOKS
}

/// `onMessageAppend` 发射（drivers 泵点统一入口）：全载荷发射，逐插件
/// 载荷最小化（messages:read 门）在 [`HooksBus::dispatch`] 内做。
pub fn dispatch_message_append(session_id: &str, seq: u64, kind: &str, text: Option<&str>) {
    global_dispatch(
        "onMessageAppend",
        json!({
            "sessionId": session_id,
            "seq": seq,
            "kind": kind,
            "text": text,
        }),
    );
}

/// `onSessionCreate` 发射：会话创建完成（spawn/接管成功后）。
pub fn dispatch_session_created(session_id: &str, driver: &str, cwd: Option<&str>, name: Option<&str>) {
    global_dispatch(
        "onSessionCreate",
        json!({
            "sessionId": session_id,
            "driver": driver,
            "cwd": cwd,
            "name": name,
        }),
    );
}

/// `onSessionClose` 发射：会话结束/被移除。
pub fn dispatch_session_closed(session_id: &str, driver: &str, reason: Option<&str>) {
    global_dispatch(
        "onSessionClose",
        json!({
            "sessionId": session_id,
            "driver": driver,
            "reason": reason,
        }),
    );
}

/// 事件时间戳工具（发射点构造 params 用）。
pub fn hook_now() -> String {
    now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::api_surface;

    #[test]
    fn hook_method_prefix() {
        assert_eq!(hook_method("onIdle"), "hook/onIdle");
    }

    #[test]
    fn observer_hooks_match_adr() {
        // ADR-0014 拍板结论 1 的五个钩子名
        assert_eq!(
            observer_hooks(),
            &[
                "onSessionCreate",
                "onSessionClose",
                "onMessageAppend",
                "onIdle",
                "onJobsChanged"
            ]
        );
    }

    #[test]
    fn queue_cap_drops_and_counts() {
        let q = PluginQueue {
            cap: 2,
            inner: StdMutex::new(QueueInner::default()),
        };
        let mk = |i: u64| HookNotification {
            hook: "onIdle".into(),
            params: json!({ "i": i }),
        };
        assert!(q.push(mk(1)));
        assert!(q.push(mk(2)));
        assert!(!q.push(mk(3)), "满队列丢弃");
        let (delivered, dropped, depth) = q.stats();
        assert_eq!((delivered, dropped, depth), (0, 1, 2));
        assert!(q.pop().is_some());
        q.count_delivered();
        let (delivered, dropped, depth) = q.stats();
        assert_eq!((delivered, dropped, depth), (1, 1, 1));
    }

    #[test]
    fn global_dispatch_short_circuits_without_bus() {
        // 未装配门面（测试进程未 set_global）时静默短路，不 panic
        global_dispatch("onIdle", json!({}));
    }

    #[test]
    fn intercept_names_are_not_observer() {
        // 防御性断言：拦截档名永不进观察档集合
        for name in api_surface::INTERCEPT_HOOK_NAMES {
            assert!(!OBSERVER_HOOKS.contains(&name));
        }
    }
}
