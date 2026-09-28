pub mod acp;
pub mod agents;
pub mod agents_config;
pub mod auth;
pub mod commands;
pub mod docs;
pub mod git_tree;
pub mod harness;
pub mod instances;
pub mod messages;
pub mod notes;
pub mod projects;
pub mod upload;
pub mod ws;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
};

use tokio::sync::broadcast;

use crate::{
    config::Config,
    drivers::acp::{AcpDriver, AcpRegistry},
    drivers::claude::session::ChatEvent,
    drivers::claude::watcher::HubEvent,
    drivers::claude::ClaudeDriver,
    drivers::MessageRoutes,
};

/// git 工程树缓存类型（MINOR-2 收敛）：(生成时刻, 树)
type TreeCache =
    Arc<tokio::sync::RwLock<Option<(std::time::Instant, Vec<git_tree::GitTreeGroup>)>>>;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub driver: ClaudeDriver,
    pub events_tx: broadcast::Sender<HubEvent>,
    /// per-session 对话推送房间（sessionId → 房间 sender）；字段级扩展，不动 events_tx
    pub chat_rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    /// 统一消息路由（driver 名 → sender；P3 统一消息入口，ACP 预留）
    pub message_routes: Arc<RwLock<MessageRoutes>>,
    /// git 工程树缓存（P6 批次 3，MINOR-2 收敛）：TTL 15s，写操作后失效
    pub tree_cache: TreeCache,
    /// 中断标记（反馈轮 19）：agentId → 中断收尾落定后的 jsonl mtime 基线。
    /// CLI 中断后 state 滞留 working（r49 实测 3 分钟+），list 时覆盖为空闲，
    /// 防发送按钮锁死；出现新活动（mtime 前进且 30s 活跃窗内）或 state 真实
    /// 离开工作态即解除，恢复权威 group——不回退 17 轮的转圈修复语义。
    pub interrupted_at: Arc<Mutex<HashMap<String, std::time::SystemTime>>>,
    /// ACP 会话注册表（acp-omp 批1 任务3；内存态，重启即失）
    pub acp_registry: Arc<AcpRegistry>,
    /// ACP 生命周期门面（agent 清单来自 cfg.acp）
    pub acp: AcpDriver,
    /// SSH 隧道管理器（multi-instance 批1 任务3）
    pub tunnels: Arc<crate::tunnel::TunnelManager>,
}

impl AppState {
    /// list 后应用中断标记（三段式：锁内快照 → 锁外 async 判据 → 锁内应用，
    /// std Mutex 不跨 await）。
    pub async fn apply_interrupt_overrides(&self, agents: &mut [crate::models::AgentSummary]) {
        use crate::drivers::claude::session as claude_session;
        use crate::models::Group;

        let marked: Vec<(String, std::time::SystemTime)> = {
            let map = self.interrupted_at.lock().expect("interrupted_at lock");
            agents
                .iter()
                .filter_map(|a| map.get(&a.id).map(|t| (a.id.clone(), *t)))
                .collect()
        };
        if marked.is_empty() {
            return;
        }

        // 解除判据：state 真实离开工作态；或 jsonl 持续新写入（mtime 越过基线
        // 且在 30s 活跃窗内——一次性中断收尾写入不满足，流式输出才满足）
        let mut relieved: Vec<String> = Vec::new();
        for (id, baseline) in &marked {
            let Some(a) = agents.iter().find(|a| &a.id == id) else {
                continue;
            };
            if !matches!(a.group, Group::Working | Group::NeedsInput) {
                relieved.push(id.clone());
                continue;
            }
            if let Some(sid) = a.session_id.as_deref() {
                let mtime = claude_session::session_mtime(&self.cfg.claude_root, sid).await;
                let active =
                    claude_session::session_is_active(&self.cfg.claude_root, sid, 30).await;
                if mtime.is_some_and(|m| m > *baseline) && active {
                    relieved.push(id.clone());
                }
            }
        }

        let mut map = self.interrupted_at.lock().expect("interrupted_at lock");
        for id in &relieved {
            map.remove(id);
        }
        for a in agents.iter_mut() {
            if map.contains_key(&a.id) && matches!(a.group, Group::Working | Group::NeedsInput) {
                a.group = Group::Other;
            }
        }
    }
}

impl AppState {
    pub fn from_config(cfg: Config) -> Self {
        let driver = ClaudeDriver {
            bin: cfg.claude_bin.clone(),
            jobs_dir: cfg.jobs_dir.clone(),
        };
        let (events_tx, _) = broadcast::channel(32);
        let acp_registry = Arc::new(AcpRegistry::with_store(cfg.acp_sessions_file.clone()));
        let mut routes: MessageRoutes = HashMap::new();
        routes.insert(
            "claude".into(),
            Arc::new(crate::drivers::claude::send::ClaudeSendRouter::new(
                cfg.claude_bin.clone(),
            )),
        );
        routes.insert(
            "acp".into(),
            Arc::new(crate::drivers::acp::AcpSendRouter::new(
                acp_registry.clone(),
                events_tx.clone(),
            )),
        );
        let acp = AcpDriver::from_config(&cfg.acp);
        Self {
            cfg,
            driver,
            events_tx,
            chat_rooms: Arc::new(RwLock::new(HashMap::new())),
            message_routes: Arc::new(RwLock::new(routes)),
            tree_cache: Arc::new(tokio::sync::RwLock::new(None)),
            interrupted_at: Arc::new(Mutex::new(HashMap::new())),
            acp_registry,
            acp,
            tunnels: Arc::new(crate::tunnel::TunnelManager::new()),
        }
    }
}

pub type SharedState = Arc<AppState>;
