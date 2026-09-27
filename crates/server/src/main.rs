use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use server::{
    api::AppState,
    config::Config,
    drivers::claude::watcher::{spawn_poll, spawn_watch},
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
    let cfg = Config::load();
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
    // 批3 任务13：启动即恢复持久化的 ACP 会话（session/load；omp 实测支持；
    // 不支持时条目自动剪除，会话按新建呈现）
    tokio::spawn(server::api::acp::restore_persisted_sessions(state.clone()));
    // 后台任务：jobs watcher + 兜底轮询（错误路径：watcher 不存在 warn! 退出，poll 持续 Tick）
    spawn_watch(cfg.jobs_dir.clone(), state.events_tx.clone());
    spawn_poll(state.events_tx.clone(), Duration::from_secs(cfg.poll_secs));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    // ConnectInfo 注入 loopback 判定（认证门控依赖）
    let st = state.clone();
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    // 停机：回收全部 SSH 隧道（kill 进程组防残留孤儿 ssh）
    st.tunnels.shutdown_all().await;
    Ok(())
}
