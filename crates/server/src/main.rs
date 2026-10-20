use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use server::{
    api::AppState,
    config::Config,
    drivers::claude::watcher::{spawn_poll, spawn_watch},
    nodelink::client::{run_node_link, NodeLinkParams, Timings},
    nodelink::node_config::{default_node_name, validate_gateway_url, NodeArgs, NodeConfig},
    router,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "server=debug,tower_http=info".into()),
        )
        .init();
    // B2 会话能力桥：聚合代理入口（ADR-0005 §4.4，设计 §4.4）
    // 形态：`server mcp-proxy --session <id>` 以 stdio MCP server 身份运行，供 claude 会话子进程连接
    if let Some(sid) = mcp_proxy_session_from_args(std::env::args().skip(1)) {
        return server::plugins::mcp_proxy::run_stdio_proxy(sid).await;
    }
    // node 形态分支（gateway-node P1 任务 2/3）：--node + --gateway/env 进入节点模式
    let args = NodeArgs::parse(std::env::args().skip(1));
    let mut cfg = Config::load();
    // --plugin-dir <path>（可重复）：transient 插件单次加载（ADR-0001 第 4 条），
    // 与 env AGENT_HUB_PLUGIN_DIR 合并；transient 不写注册表持久层
    for p in plugin_dirs_from_args(std::env::args().skip(1)) {
        if !cfg.plugin_transient_dirs.contains(&p) {
            cfg.plugin_transient_dirs.push(p);
        }
    }
    if args.node {
        // N6 前置硬门禁（B1 tester FAIL-1/2 回修）：网关解析与红线校验先于一切
        // bind/后台任务——失败 eprintln + exit(1)，结构上无回退路径，绝不落回
        // hub 启动路径（事故教训：残缺 --node 落回 hub 绑正式口）
        let (node, node_file) = match prepare_node(&args, &cfg).await {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Error: {e:#}");
                std::process::exit(1);
            }
        };
        return start_node(args, cfg, node, node_file).await;
    }
    // allow_lan：监听 0.0.0.0 并确保 token 就绪（生成/持久化）；否则维持 127.0.0.1 免认证
    let bind = if cfg.allow_lan {
        server::auth::ensure_token(&cfg);
        SocketAddr::new(IpAddr::from([0, 0, 0, 0]), cfg.bind_addr.port())
    } else {
        // 安全加固（P4 处置 P2-5）：非回环 bind 在无认证（allow_lan=false）时强制回落 loopback
        if !cfg.bind_addr.ip().is_loopback() {
            tracing::warn!(
                requested = %cfg.bind_addr,
                "allow_lan=false 时非回环地址不安全，已回落 127.0.0.1"
            );
            SocketAddr::new(IpAddr::from([127, 0, 0, 1]), cfg.bind_addr.port())
        } else {
            cfg.bind_addr
        }
    };
    tracing::info!(
        addr = %bind,
        allow_lan = cfg.allow_lan,
        "agent-hub server starting"
    );
    let state = Arc::new(AppState::from_config(cfg.clone()));
    spawn_host_backends(&state, &cfg);
    // 插件装载序列（ADR-0002 第 2/4 条）：先于 serve 做启动扫描 + 恢复已启用插件；
    // 单插件失败只写故障记录并置故障态，不阻塞后续插件与主体启动
    state.plugins.startup().await;
    serve_app(state, bind).await
}

fn mcp_proxy_session_from_args(args: impl Iterator<Item = String>) -> Option<String> {
    let mut iter = args.peekable();
    while let Some(a) = iter.next() {
        if a == "mcp-proxy" {
            // 形如 `server mcp-proxy --session <id>` 或 `mcp-proxy --session <id>`（hub bin 透传 args）
            while let Some(n) = iter.next() {
                if n == "--session" {
                    if let Some(v) = iter.next() {
                        if !v.trim().is_empty() {
                            return Some(v);
                        }
                    }
                    return None;
                }
            }
        }
    }
    None
}

/// 从命令行提取 `--plugin-dir <path>` / `--plugin-dir=<path>`（可重复）。
fn plugin_dirs_from_args(args: impl Iterator<Item = String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut expect = false;
    for a in args {
        if expect {
            out.push(PathBuf::from(a));
            expect = false;
            continue;
        }
        if a == "--plugin-dir" {
            expect = true;
        } else if let Some(v) = a.strip_prefix("--plugin-dir=") {
            if !v.is_empty() {
                out.push(PathBuf::from(v));
            }
        }
    }
    out
}

/// 宿主本地后台（两形态共用，防漂移）：ACP 会话恢复 / pi 助手 / jobs watcher /
/// 兜底轮询 / 定时任务调度器 / opencode SSE 泵 / dsh mux WS 泵。
/// 隧道类不在其列（tunnels 仅 API 按需建立 + 停机统一回收）——node 形态天然
/// 不 spawn 隧道类后台（任务 2 完成判据）。
fn spawn_host_backends(state: &Arc<AppState>, cfg: &Config) {
    // 首跑引导（gateway-first-run R2）：全新数据目录 jobs 缺失会让 watcher 初始化
    // 失败退出（gw-demo 实测）；其余数据文件族（notes/instances/tasks/projects/
    // acp_sessions/ignored_interactive/token）已核缺失均按空自建或启动即生成
    //（R3 排查结论，见各 store 实现），唯目录需要显式创建。
    if let Err(e) = std::fs::create_dir_all(&cfg.jobs_dir) {
        tracing::warn!(error = %e, dir = %cfg.jobs_dir.display(), "jobs 目录创建失败（watcher 将不可用）");
    }
    // 批3 任务13：启动即恢复持久化的 ACP 会话（session/load；omp 实测支持；
    // 不支持时条目自动剪除，会话按新建呈现）
    tokio::spawn(server::api::acp::restore_persisted_sessions(state.clone()));
    // P2 auto_start：启动即拉起 Hub 助手（pi driver）；拉起失败不拖垮 hub
    //（侧栏无 pi 条目 + 日志 warn，设置页可再启）
    if state.cfg.pi.auto_start {
        let pi = state.pi.clone();
        tokio::spawn(async move {
            if let Err(e) = pi.ensure_started().await {
                tracing::warn!(error = %e, "Hub 助手（pi）启动失败，可经 agents start 重试");
            }
        });
    }
    // 后台任务：jobs watcher + 兜底轮询（错误路径：watcher 不存在 warn! 退出，poll 持续 Tick）
    spawn_watch(cfg.jobs_dir.clone(), state.events_tx.clone());
    spawn_poll(state.events_tx.clone(), Duration::from_secs(cfg.poll_secs));
    // 定时任务调度器（agent-hub-daemon-capabilities）：启动错过检测 + cron 匹配 + 异步执行
    tokio::spawn(server::tasks::spawn_scheduler(state.clone()));
    // opencode SSE 泵（agent-hub-opencode-driver）：事件 → busy 翻转 + chat 房间增量；
    // serve 未拉起时泵自行等待，不阻塞启动
    state.opencode.spawn_event_pump();
    // dsh mux WS 泵（agent-hub-dsh-driver）：事件 → chat 房间增量 + 审批弹卡 + Tick；
    // 同上，serve 未拉起时自行等待
    state.dsh.spawn_event_pump();
}

/// axum 服务启动 + 停机隧道回收（两形态共用）。
async fn serve_app(state: Arc<AppState>, bind: SocketAddr) -> anyhow::Result<()> {
    let r = router(state.clone());
    serve_router(state, bind, r).await
}

/// serve 本体（node 形态传入自建 router 的克隆，供 HTTP-over-mux 重放共用同一
/// 路由树——同代码同路由同测试面）。
async fn serve_router(
    state: Arc<AppState>,
    bind: SocketAddr,
    r: axum::Router,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            // EADDRINUSE 防呆（gateway-node-p1 B2 tester 观察项 1）：缺省口与
            // 正式 hub 相同，同机共存必须显式隔离
            anyhow::anyhow!(
                "端口 {bind} 已被占用：同机与正式 hub 共存必须用 AGENT_HUB_BIND \
                 指定隔离端口（正式服务通常占 7800）"
            )
        } else {
            anyhow::anyhow!(e)
        }
    })?;
    // ConnectInfo 注入 loopback 判定（认证门控依赖）
    let st = state.clone();
    axum::serve(
        listener,
        r.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    // 停机：回收全部 SSH 隧道（kill 进程组防残留孤儿 ssh）
    st.tunnels.shutdown_all().await;
    // 插件 backend 进程组随 hub 退出回收（ADR-0006 第 2 条：不遗留孤儿）
    st.plugins.shutdown().await;
    Ok(())
}

/// node 形态前置（gateway-node P1 任务 2；B1 tester FAIL-1/2 回修后置位）：
/// 网关地址解析（--gateway > env > node.json）+ N6 两处校验 + node.json 就绪
/// （0600）。任何失败 = 拒绝启动，调用方 exit(1)——本函数不存在回退路径。
async fn prepare_node(args: &NodeArgs, cfg: &Config) -> anyhow::Result<(NodeConfig, PathBuf)> {
    let node_file = cfg.node_file.clone();
    // gateway_url 决定顺序：--gateway > env AGENT_HUB_NODE_URL（空串视为未设）>
    // 既有 node.json
    let existing = NodeConfig::load(&node_file);
    let gateway_url = args
        .gateway
        .clone()
        .or_else(|| {
            std::env::var("AGENT_HUB_NODE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| existing.as_ref().map(|n| n.gateway_url.clone()))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "node 模式需要网关地址：--gateway <ws(s)://host:port>，\
                 或 env AGENT_HUB_NODE_URL，或既有 node.json 的 gatewayUrl"
            )
        })?;
    let allow_insecure =
        args.allow_insecure || existing.as_ref().map(|n| n.allow_insecure).unwrap_or(false);
    // N6 校验处 1（启动入口，先于一切 bind/后台）
    validate_gateway_url(&gateway_url, allow_insecure).map_err(anyhow::Error::msg)?;
    let node = match existing {
        Some(mut n) => {
            // 身份（node_id/token）保留；地址与豁免按本次启动刷新
            n.gateway_url = gateway_url;
            n.allow_insecure = allow_insecure;
            if let Some(name) = &args.node_name {
                n.node_name = name.clone();
            }
            n
        }
        None => NodeConfig {
            gateway_url,
            node_id: uuid::Uuid::new_v4().to_string(),
            node_name: args.node_name.clone().unwrap_or_else(default_node_name),
            token: None,
            allow_insecure,
        },
    };
    // N6 校验处 2（save 内）+ 0600 落盘
    node.save(&node_file).await?;
    Ok((node, node_file))
}

/// node 形态启动（校验已全部通过后才进入）：强制 loopback bind + 宿主后台 +
/// NodeLink 泵 + axum serve。
async fn start_node(
    args: NodeArgs,
    cfg: Config,
    node: NodeConfig,
    node_file: PathBuf,
) -> anyhow::Result<()> {
    // 强制 loopback bind：即使环境带 allow_lan，node 形态也不对非回环监听
    let port = cfg.bind_addr.port();
    if !cfg.bind_addr.ip().is_loopback() {
        tracing::warn!(
            requested = %cfg.bind_addr,
            "node 模式强制回环 bind（节点零新增暴露面）"
        );
    }
    let bind = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!(
        addr = %bind,
        gateway = %node.gateway_url,
        node_id = %node.node_id,
        node_name = %node.node_name,
        paired = node.token.is_some(),
        "agent-hub node starting"
    );
    let state = Arc::new(AppState::from_config(cfg.clone()));
    // 节点 = 完整宿主：驱动/调度器等本地设施与 gateway 形态共用同一启动序列
    spawn_host_backends(&state, &cfg);
    // 路由树构建一次：axum serve 与 HTTP-over-mux 重放共用（同代码同路由）
    let r = router(state.clone());
    // enroll code 决定顺序（sweep S2）：--enroll argv > env AGENT_HUB_ENROLL
    // （空串视为未设）——env 传递防 code 进 ps(1)/systemd unit（world-readable）；
    // 与 gateway_url 的 AGENT_HUB_NODE_URL 同族取法。
    let enroll = args.enroll.or_else(|| {
        std::env::var("AGENT_HUB_ENROLL")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });
    // NodeLink 泵（任务 3）：驱动清单来自注册表（announce 事实源）
    let params = NodeLinkParams {
        gateway_url: node.gateway_url.clone(),
        node_id: node.node_id.clone(),
        node_name: node.node_name.clone(),
        token: node.token.clone(),
        enroll,
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        drivers: state
            .drivers
            .names()
            .into_iter()
            .map(str::to_string)
            .collect(),
        hub_version: env!("CARGO_PKG_VERSION").to_string(),
        router: r.clone(),
        // WS-over-mux 流目标：节点自身 loopback 端口（与 serve 同口）
        local_port: port,
        // 事件回传（P2 任务 2）：节点 Tick 经 mux 回网关 events 面
        events_tx: Some(state.events_tx.clone()),
    };
    tokio::spawn(run_node_link(params, node_file, Timings::default()));
    serve_router(state, bind, r).await
}
