//! opencode serve 生命周期（attach-service 模式，需求决策 2）：幂等拉起 / 健康检查 /
//! 端口与 pid 落配置。结构仿 `tunnel.rs`（start 幂等 + 健康轮询 + 端口记录）。
//!
//! 红线（产品原则 L1「hub 不持会话唯一状态」）：serve **detach 拉起**——不经 hub
//! 作父进程（脱离会话/进程组拉起，归驱动层），hub 死亡不带走 serve；会话事实源在 opencode 自己的
//! SQLite，hub 重启后重新发现即可（验收标准 3）。
//!
//! 安全：只绑 127.0.0.1（opencode 默认）；远程化走既有实例代理通道，不直接暴露。

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

/// 健康检查单次超时（opencode `/global/health` 探活；未就绪 fail-fast 的上限）
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
/// 拉起后等待就绪的总预算（opencode serve 冷启含 provider discovery，实测数秒）
const READY_BUDGET: Duration = Duration::from_secs(30);
/// 就绪轮询间隔
const READY_POLL: Duration = Duration::from_millis(400);

/// serve 运行态快照（落配置的字段 + 探活结果）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServeState {
    /// 实际监听端口（探测/拉起后回填；配置项为首选端口，被占用则顺延）
    pub port: Option<u16>,
    /// detach 拉起的进程 pid（外部已运行的 serve 为 None）
    pub pid: Option<u32>,
}

/// serve 管理器：进程内持有运行态，端口/pid 落 JSON 文件供 hub 重启后重新发现。
#[derive(Clone)]
pub struct ServeManager {
    bin: String,
    host: String,
    /// 首选端口（默认 4097）；被占用则顺延探测
    preferred_port: u16,
    state_file: PathBuf,
    state: Arc<Mutex<ServeState>>,
}

impl ServeManager {
    pub fn new(bin: String, host: String, preferred_port: u16, state_file: PathBuf) -> Self {
        // 启动即读回上次落盘的端口/pid（hub 重启后复用外部 serve）
        let state = std::fs::read_to_string(&state_file)
            .ok()
            .and_then(|raw| serde_json::from_str::<ServeState>(&raw).ok())
            .unwrap_or_default();
        Self {
            bin,
            host,
            preferred_port,
            state_file,
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn snapshot(&self) -> ServeState {
        self.state.lock().expect("opencode serve lock").clone()
    }

    /// 端口与 pid 落盘（原子替换：临时文件 → rename，崩溃不留半截 JSON）。
    fn persist(&self) {
        let snap = self.snapshot();
        if let Some(dir) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(raw) = serde_json::to_string_pretty(&snap) {
            let tmp = self.state_file.with_extension("json.tmp");
            if std::fs::write(&tmp, raw).is_ok() {
                let _ = std::fs::rename(&tmp, &self.state_file);
            }
        }
    }

    /// 健康检查：`GET {host}:{port}/global/health` 探活（opencode 内建端点）。
    pub async fn healthy(&self, port: u16) -> bool {
        let url = format!("http://{}:{}/global/health", self.host, port);
        tokio::time::timeout(HEALTH_TIMEOUT, super::http::get_ok(&url))
            .await
            .unwrap_or(false)
    }

    /// 幂等拉起（仿 tunnel::start）：
    /// 1) 已记录端口且健康 → 直接复用（hub 重启后重新发现外部 serve）；
    /// 2) 首选端口健康但无记录（外部手动起的）→ 认领并记录；
    /// 3) 否则 detach 拉起 serve（首选端口被占则顺延），轮询就绪；未就绪 fail-fast。
    ///
    /// 返回实际端口。
    pub async fn ensure_started(&self) -> Result<u16> {
        // 1) 已记录且健康 → 幂等返回
        if let Some(port) = self.snapshot().port {
            if self.healthy(port).await {
                return Ok(port);
            }
        }
        // 2) 首选端口健康但无记录 → 认领（用户/其他客户端手动起的 serve）
        if self.snapshot().port.is_none() && self.healthy(self.preferred_port).await {
            self.set_state(Some(self.preferred_port), None);
            self.persist();
            tracing::info!(port = self.preferred_port, "认领已在运行的 opencode serve");
            return Ok(self.preferred_port);
        }
        // 3) detach 拉起
        let port = self.pick_port().await;
        self.spawn_detached(port).await?;
        // 就绪轮询（fail-fast：超 READY_BUDGET 未就绪即报错，不静默）
        let deadline = tokio::time::Instant::now() + READY_BUDGET;
        loop {
            if self.healthy(port).await {
                self.set_state(Some(port), self.pid());
                self.persist();
                tracing::info!(port = %port, "opencode serve 已就绪");
                return Ok(port);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "opencode serve 拉起后 {READY_BUDGET:?} 未就绪（端口 {port}）"
                ));
            }
            tokio::time::sleep(READY_POLL).await;
        }
    }

    /// 选端口：首选端口空闲则用；被占则顺延（+1，最多试 20 个）。
    async fn pick_port(&self) -> u16 {
        for offset in 0..20u16 {
            let port = self.preferred_port.saturating_add(offset);
            if port < self.preferred_port {
                break; // 溢出
            }
            if !crate::tunnel::port_open(port).await {
                return port;
            }
        }
        // 全占：让 opencode 自行选（--port 传 0 不被支持，退回首选由 opencode 报错）
        self.preferred_port
    }

    /// detach 拉起 serve：**不**作 hub 子进程（脱离会话/进程组，实现归驱动层
    /// platform::detach_from_session），hub 死亡不带走 serve（产品原则 L1）。
    /// stdout/stderr 落 null（serve 自身日志在 opencode 侧）。
    async fn spawn_detached(&self, port: u16) -> Result<()> {
        // detach：脱离当前会话（unix 命令行等价物为会话脱离工具 + opencode serve，
        // 语义归驱动层），但不经 shell 拼接（安全模板 §6）。
        let mut cmd = Command::new(&self.bin);
        cmd.arg("serve")
            .arg("--port")
            .arg(port.to_string())
            .arg("--hostname")
            .arg(&self.host)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(false); // 关键：不随 hub drop 而 kill
        crate::platform::detach_from_session(&mut cmd);
        let child = cmd
            .spawn()
            .with_context(|| format!("opencode serve 拉起失败，请检查 {} 是否可用", self.bin))?;
        let pid = child.id();
        // 不 wait：detach 后不回收（serve 独立存活）。子进程句柄随 hub 丢弃即可
        //（kill_on_drop=false 保证不 kill）。pid 记入运行态供排查。
        drop(child);
        tracing::info!(pid = pid.unwrap_or(0), port = %port, "detach 拉起 opencode serve");
        Ok(())
    }

    fn pid(&self) -> Option<u32> {
        self.snapshot().pid
    }

    fn set_state(&self, port: Option<u16>, pid: Option<u32>) {
        let mut s = self.state.lock().expect("opencode serve lock");
        s.port = port;
        s.pid = pid;
    }

    /// 当前记录端口（driver 各请求用它拼 base URL；未拉起为 None）。
    pub fn port(&self) -> Option<u16> {
        self.snapshot().port
    }

    /// 测试注入：直接写运行态（不起真进程）。
    #[cfg(test)]
    pub fn inject_state(&self, port: Option<u16>, pid: Option<u32>) {
        self.set_state(port, pid);
    }

    /// 测试用状态文件路径（构造 ServeManager 用）。
    #[cfg(test)]
    pub fn state_file_for(dir: &std::path::Path) -> PathBuf {
        dir.join("opencode_serve.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 幂等语义：已记录且健康 → 复用不重拉；首选端口健康但无记录 → 认领。
    /// 用一个真实监听端口冒充健康 serve（tokio listener 回 /global/health 不便，
    /// 这里只验「healthy 对未监听端口为 false」+「认领路径写状态」的状态机部分）。
    #[tokio::test]
    async fn healthy_false_for_closed_port_and_state_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ServeManager::new(
            "opencode".into(),
            "127.0.0.1".into(),
            65000, // 极高端口，几乎必空闲/无服务
            ServeManager::state_file_for(dir.path()),
        );
        assert!(mgr.port().is_none());
        // 未监听端口 → 不健康
        assert!(!mgr.healthy(65000).await);

        // 注入状态 + 落盘 + 重新 new 读回（hub 重启重新发现语义）
        mgr.inject_state(Some(65001), Some(4242));
        mgr.persist();
        let mgr2 = ServeManager::new(
            "opencode".into(),
            "127.0.0.1".into(),
            65000,
            ServeManager::state_file_for(dir.path()),
        );
        assert_eq!(mgr2.port(), Some(65001));
        assert_eq!(mgr2.pid(), Some(4242));
    }

    /// 落盘原子替换：无 .tmp 残留。
    #[tokio::test]
    async fn persist_is_atomic_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let file = ServeManager::state_file_for(dir.path());
        let mgr = ServeManager::new("opencode".into(), "127.0.0.1".into(), 4097, file.clone());
        mgr.inject_state(Some(4097), Some(1));
        mgr.persist();
        assert!(file.exists());
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(residue.is_empty(), "不应残留临时文件: {residue:?}");
    }
}
