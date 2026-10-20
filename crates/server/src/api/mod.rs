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
pub mod nodes;
pub mod notes;
pub mod pi;
pub mod plugins;
pub mod projects;
pub mod proxy;
pub mod tasks;
pub mod terminal;
pub mod upload;
pub mod ws;

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use tokio::sync::broadcast;

use std::sync::Arc as StdArc;

use crate::{
    config::Config,
    drivers::acp::{AcpDriver, AcpRegistry},
    drivers::dsh,
    drivers::events::{ChatEvent, HubEvent},
    drivers::omp,
    drivers::opencode,
    drivers::pi::PiDriver,
    drivers::{ClaudeDriver, DriverRegistry, MessageRoutes},
};

/// git 工程树缓存类型（MINOR-2 收敛）：(生成时刻, 树)
type TreeCache =
    Arc<tokio::sync::RwLock<Option<(std::time::Instant, Vec<git_tree::GitTreeGroup>)>>>;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Config,
    pub events_tx: broadcast::Sender<HubEvent>,
    /// per-session 对话推送房间（sessionId → 房间 sender）；字段级扩展，不动 events_tx
    pub chat_rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>>,
    /// 统一消息路由（driver 名 → sender；P3 统一消息入口，ACP 预留）
    pub message_routes: Arc<RwLock<MessageRoutes>>,
    /// 统一驱动注册表（driver-unify）：app 层唯一的 agent 能力入口，
    /// claude 具体类型不挂 AppState
    pub drivers: DriverRegistry,
    /// git 工程树缓存（P6 批次 3，MINOR-2 收敛）：TTL 15s，写操作后失效
    pub tree_cache: TreeCache,
    /// ACP 会话注册表（acp-omp 批1 任务3；内存态，重启即失）
    pub acp_registry: Arc<AcpRegistry>,
    /// ACP 生命周期门面（agent 清单来自 cfg.acp）；Arc 化供 omp 文件驱动共用
    ///（接管 = spawn + session/load，同一 agent 配置）
    pub acp: StdArc<AcpDriver>,
    /// SSH 隧道管理器（multi-instance 批1 任务3）
    pub tunnels: Arc<crate::tunnel::TunnelManager>,
    /// opencode 驱动（agent-hub-opencode-driver；serve 生命周期 + SSE 泵挂其上）
    pub opencode: opencode::OpencodeDriver,
    /// dsh 驱动（agent-hub-dsh-driver；serve 生命周期 + mux WS 泵挂其上）
    pub dsh: dsh::DshDriver,
    /// omp 文件事实源驱动（agent-hub-omp-session-host 阶段 A）
    pub omp: omp::OmpDriver,
    /// pi 驱动（agent-hub-pi-hub-agent）：单实例「Hub 助手」生命周期 + 设置门面
    pub pi: StdArc<PiDriver>,
    /// 定时任务管理器（agent-hub-daemon-capabilities）：任务表 + Run 日志 + 串行守卫
    pub tasks: StdArc<crate::tasks::TaskManager>,
    /// 节点注册表（gateway-node P1 任务4）：nodes.json + enrollment 状态机
    pub nodes: StdArc<crate::nodelink::registry::NodeRegistry>,
    /// 节点活连接表（gateway-node P1 任务5）：node_id → 命令通道（代理面查询/注入）
    pub node_conns: crate::nodelink::gateway::NodeConnections,
    /// slash 指令清单缓存（agent-hub-slash-skills-b）：(claude_root, cwd) →
    /// { 条目, 来源, 过期时刻 }；SDK 校准源 TTL 300s，文件枚举回退 TTL 60s
    /// （pub(crate)：缓存类型为 commands 模块内部机制，不外泄）
    pub(crate) commands_cache: commands::CommandCache,
    /// 插件体系门面（agent-hub-plugin-system B1）：注册表/扫描/审计/监督器/凭据/票据。
    /// AppState 只挂这一个字段，不散挂子模块（ADR-0002 第 1 条）。
    pub plugins: StdArc<crate::plugins::PluginManager>,
}

impl AppState {
    pub fn from_config(cfg: Config) -> Self {
        let (events_tx, _) = broadcast::channel(32);
        let acp_registry = Arc::new(AcpRegistry::with_store(cfg.acp_sessions_file.clone()));
        // chat 房间表先行创建：AppState 与两个驱动共享同一张表（AcpFeed/ensure_*_room 同源）
        let chat_rooms: Arc<RwLock<HashMap<String, broadcast::Sender<ChatEvent>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        // 插件门面先建（events_tx 已就绪；AppState 落盘后由 main 触发装载序列）
        let plugins = crate::plugins::PluginManager::new(&cfg, events_tx.clone());
        // 单实例共享：sender 由 ClaudeDriver::new 自建，轨一
        // message_routes 取其句柄——attaches 表唯一（双实例 = 双重 attach 破坏
        // TUI 注入时序）
        let driver = ClaudeDriver::new(
            cfg.claude_bin.clone(),
            cfg.jobs_dir.clone(),
            cfg.claude_root.clone(),
            cfg.ignored_interactive_file.clone(),
            chat_rooms.clone(),
        );
        // 定时任务目录先取（Self 字面量里 cfg 会被 move）
        let tasks_dir = cfg.tasks_dir.clone();
        // 节点注册表文件先取（同上；gateway-node P1 任务4）
        let nodes_file = cfg.nodes_file.clone();
        let acp_router = Arc::new(crate::drivers::acp::AcpSendRouter::new(
            acp_registry.clone(),
            events_tx.clone(),
            chat_rooms.clone(),
        ));
        let mut routes: MessageRoutes = HashMap::new();
        routes.insert("claude".into(), driver.sender_handle());
        routes.insert("acp".into(), acp_router.clone());
        let mut drivers = DriverRegistry::new();
        drivers.register(Arc::new(driver));
        drivers.register(acp_router.clone());
        let acp = StdArc::new(AcpDriver::from_config(&cfg.acp));
        // omp agent 名（接管用）：config [acp] agents 里 command 为 omp 的首个条目
        let omp_acp_agent = cfg
            .acp
            .agents
            .iter()
            .find(|a| a.command.ends_with("/omp") || a.command == "omp")
            .map(|a| a.name.clone())
            .unwrap_or_else(|| "omp".into());
        // opencode driver（attach-service）：serve 生命周期归 hub 管理、serve 进程
        // detach（不随 hub 死亡）；会话事实源在 opencode SQLite
        let serve = opencode::serve::ServeManager::new(
            cfg.opencode.bin.clone(),
            "127.0.0.1".into(),
            cfg.opencode.port,
            cfg.opencode_serve_file.clone(),
        );
        let opencode_driver =
            opencode::OpencodeDriver::new(serve, chat_rooms.clone(), events_tx.clone());
        drivers.register(Arc::new(opencode_driver.clone()));
        routes.insert(
            "opencode".into(),
            Arc::new(opencode_driver.clone()) as Arc<dyn crate::drivers::MessageSender>,
        );
        // dsh driver（attach-service 第二实现）：serve 生命周期归 hub 管理、detach；
        // 会话事实源在 dsh 存储，mux WS 泵在 main 启动时挂上
        let dsh_serve = dsh::serve::ServeManager::new(
            cfg.dsh.bin.clone(),
            cfg.dsh.port,
            cfg.dsh_serve_file.clone(),
            cfg.dsh.auto_start,
        );
        let dsh_auth = dsh::rpc::Auth::new(cfg.dsh.token.clone());
        let dsh_driver =
            dsh::DshDriver::new(dsh_serve, chat_rooms.clone(), events_tx.clone(), dsh_auth);
        // omp 文件事实源驱动（阶段 A）：只读扫目录 + 接管（ACP spawn + session/load）
        let omp_driver = omp::OmpDriver::new(
            cfg.omp.sessions_dir.clone(),
            acp.clone(),
            acp_router.clone(),
            acp_registry.clone(),
            omp_acp_agent,
            chat_rooms.clone(),
            events_tx.clone(),
        );
        drivers.register(StdArc::new(omp_driver.clone()));
        drivers.register(Arc::new(dsh_driver.clone()));
        routes.insert(
            "dsh".into(),
            Arc::new(dsh_driver.clone()) as Arc<dyn crate::drivers::MessageSender>,
        );
        // pi driver（agent-hub-pi-hub-agent）：单实例 Hub 助手；hub_port 供 spawn
        // env HUB_API_BASE 注入（P8 extension 内调 loopback API，免 token）
        // P10：pi 原生配置目录（~/.pi/agent；PI_AGENT_DIR 隔离测试）——单一事实源
        let pi_agent_dir = std::env::var("PI_AGENT_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| dirs_home().join(".pi/agent"));
        let pi_driver = StdArc::new(PiDriver::new(
            cfg.pi.clone(),
            cfg.pi_settings_file.clone(),
            cfg.pi_extension_file.clone(),
            cfg.pi_session_dir.clone(),
            cfg.bind_addr.port(),
            events_tx.clone(),
            chat_rooms.clone(),
            pi_agent_dir,
        ));
        drivers.register(pi_driver.clone());
        routes.insert(
            "pi".into(),
            pi_driver.clone() as Arc<dyn crate::drivers::MessageSender>,
        );
        // 插件 RPC「可验证工程清单」（hub.projects.effective）聚合 agent cwd：
        // drivers 全部注册完成后回填提供方（加法接线，不动既有字段）
        let drivers_for_plugins = drivers.clone();
        plugins.set_agent_cwd_provider(Arc::new(move || {
            let drivers = drivers_for_plugins.clone();
            Box::pin(async move {
                drivers
                    .list_all(true)
                    .await
                    .map(|agents| agents.into_iter().filter_map(|a| a.cwd).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
        }));
        Self {
            cfg,
            events_tx,
            chat_rooms,
            message_routes: Arc::new(RwLock::new(routes)),
            drivers,
            tree_cache: Arc::new(tokio::sync::RwLock::new(None)),
            acp_registry,
            acp,
            tunnels: Arc::new(crate::tunnel::TunnelManager::new()),
            opencode: opencode_driver,
            dsh: dsh_driver,
            omp: omp_driver,
            pi: pi_driver,
            tasks: StdArc::new(crate::tasks::TaskManager::new(tasks_dir)),
            nodes: StdArc::new(crate::nodelink::registry::NodeRegistry::new(nodes_file)),
            node_conns: crate::nodelink::gateway::NodeConnections::new(),
            commands_cache: commands::new_command_cache(),
            plugins,
        }
    }
}

pub type SharedState = Arc<AppState>;

/// HOME 根（pi 原生配置定位；env -i 测试环境由调用方保证 HOME 已设；解析归驱动层）。
fn dirs_home() -> std::path::PathBuf {
    crate::platform::home_dir_opt().expect("HOME 环境变量未设置")
}
