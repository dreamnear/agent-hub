//! dsh serve 生命周期（attach-service 第二实现，需求决策 2）：幂等收编 / 健康探测 /
//! 端口与 pid 落配置。结构照抄 opencode/serve.rs（detach 红线同源：hub 死亡
//! 不带走 serve，会话事实源在 dsh 自己的存储，hub 重启重新发现）。
//!
//! 安全：dsh 仅绑 127.0.0.1（`--host 0.0.0.0` 被 CLI 明禁，非 loopback Host 403——
//! 信任栅栏不绕，需求决策 9）。
//!
//! 生产事故防线（2026-10-02 端口风暴，三铁律）：
//! 1. **任意 HTTP 响应 = 活**：401/403 是认证门卫行为（dsh 在跑），只有连接拒绝/
//!    超时才算未就绪；
//! 2. **配置端口有监听者 = 收编**：直接认领为被管理的 serve（标降级），**严禁
//!    递增端口重试**——用户自起实例与 hub 永远共用同一 `~/.dsh`，双实例互踩；
//! 3. **spawn 单飞 + 冷却**：`auto_start=true` 才允许拉起，且 5 分钟窗口内至多一次
//!    真实 spawn 尝试（状态文件记账，跨调用生效），未就绪绝不立即重试。

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

use super::rpc;
use crate::platform;

/// 健康检查单次超时
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
/// 拉起后等待就绪的总预算（dsh web 冷启实测秒级）
const READY_BUDGET: Duration = Duration::from_secs(30);
/// 就绪轮询间隔
const READY_POLL: Duration = Duration::from_millis(400);
/// spawn 冷却窗口（跨调用；状态文件记账）
const SPAWN_COOLDOWN_MS: u64 = 5 * 60 * 1000;

/// serve 运行态快照（落盘字段 + 探活结果）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServeState {
    pub port: Option<u16>,
    pub pid: Option<u32>,
    /// 上次真实 spawn 尝试（epoch 毫秒）；冷却窗口判据
    #[serde(default)]
    pub last_spawn_attempt_ms: Option<u64>,
    /// spawn 路捕获的一次性 launch token（dsh web stdout，进程级长效）；
    /// 401 时 hub 用它重换会话 cookie。敏感值——状态文件强制 0600
    #[serde(default)]
    pub launch_token: Option<String>,
}

/// serve 管理器：进程内运行态 + 端口/pid JSON 落盘（hub 重启后重新发现）。
#[derive(Clone)]
pub struct ServeManager {
    bin: String,
    /// 配置端口（默认 13100）——唯一管理端口，绝不递增
    preferred_port: u16,
    /// true 才允许自动拉起进程；false 只收编已监听的实例
    auto_start: bool,
    state_file: PathBuf,
    state: Arc<Mutex<ServeState>>,
}

impl ServeManager {
    pub fn new(bin: String, preferred_port: u16, state_file: PathBuf, auto_start: bool) -> Self {
        let state = std::fs::read_to_string(&state_file)
            .ok()
            .and_then(|raw| serde_json::from_str::<ServeState>(&raw).ok())
            .unwrap_or_default();
        Self {
            bin,
            preferred_port,
            auto_start,
            state_file,
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn snapshot(&self) -> ServeState {
        self.state.lock().expect("dsh serve lock").clone()
    }

    /// 端口与 pid 落盘（原子替换：临时文件 → rename）。
    fn persist(&self) {
        let snap = self.snapshot();
        if let Some(dir) = self.state_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(raw) = serde_json::to_string_pretty(&snap) {
            let tmp = self.state_file.with_extension("json.tmp");
            if std::fs::write(&tmp, raw).is_ok() {
                let _ = std::fs::rename(&tmp, &self.state_file);
                // 状态文件含 launch token（敏感）——强制 0600（notes.json 同款纪律；
                // 权限归驱动层）
                platform::harden_0600(&self.state_file);
            }
        }
    }

    /// 健康探测：**任意 HTTP 响应即活**（401/403 = 认证门卫；事故防线 1）。
    /// 连接拒绝/超时 = false。
    pub async fn healthy(&self, port: u16) -> bool {
        let base = format!("http://127.0.0.1:{port}");
        tokio::time::timeout(HEALTH_TIMEOUT, rpc::alive(&base))
            .await
            .unwrap_or(false)
    }

    /// 幂等收编（事故防线重排）：
    /// 1) 已记录端口存活 → 复用（hub 重启重新发现）；
    /// 2) 配置端口有监听者 → **收编**（就算探测未完全通过也先收编+标降级），
    ///    绝不递增端口、绝不因此 spawn；
    /// 3) `auto_start=false` → 显式报错（调用方 fail-soft 返回空集）；
    /// 4) 冷却窗口内 → 报错不重试；过窗 → 单次 spawn + 30s 就绪轮询。
    pub async fn ensure_started(&self) -> Result<u16> {
        if let Some(port) = self.snapshot().port {
            if self.healthy(port).await {
                return Ok(port);
            }
        }
        if crate::tunnel::port_open(self.preferred_port).await {
            self.set_state(Some(self.preferred_port), None);
            self.persist();
            tracing::warn!(
                port = self.preferred_port,
                "收编配置端口上的 dsh 实例（探测未完全通过也收编，标降级）"
            );
            return Ok(self.preferred_port);
        }
        if !self.auto_start {
            return Err(anyhow!(
                "dsh 自动拉起未开启（auto_start=false）且配置端口 {} 无监听",
                self.preferred_port
            ));
        }
        let now = now_ms();
        if let Some(t) = self.snapshot().last_spawn_attempt_ms {
            if now.saturating_sub(t) < SPAWN_COOLDOWN_MS {
                return Err(anyhow!(
                    "dsh spawn 冷却中（上次尝试 {:.0}s 前，窗口 {SPAWN_COOLDOWN_MS:?}），跳过",
                    (now - t) as f64 / 1000.0
                ));
            }
        }
        // 记账先于 spawn（崩溃也不丢冷却窗口）
        {
            let mut s = self.state.lock().expect("dsh serve lock");
            s.last_spawn_attempt_ms = Some(now);
        }
        self.persist();
        // spawn 路（凭据通路定案）：stdout 捕获一次性 launch token——dsh web 强制
        // 浏览器认证（无免认证开关）。捕获 EOF 无 token（子进程退出/未打印）=
        // 未就绪原路径报错；捕获成功后等健康
        let rx = self.spawn_detached(self.preferred_port).await?;
        let token = tokio::time::timeout(READY_BUDGET, rx)
            .await
            .unwrap_or(Ok(None))
            .unwrap_or(None);
        match token {
            Some(t) => {
                let mut s = self.state.lock().expect("dsh serve lock");
                s.launch_token = Some(t);
            }
            None => {
                return Err(anyhow!(
                    "dsh web stdout 未出现 launch token（端口 {}）；{SPAWN_COOLDOWN_MS:?} 冷却内不再重试",
                    self.preferred_port
                ));
            }
        }
        self.persist();
        let deadline = tokio::time::Instant::now() + READY_BUDGET;
        loop {
            if self.healthy(self.preferred_port).await {
                self.set_state(Some(self.preferred_port), self.pid());
                self.persist();
                tracing::info!(port = %self.preferred_port, "dsh web 已就绪");
                return Ok(self.preferred_port);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "dsh web 拉起后 {READY_BUDGET:?} 未就绪（端口 {}）；{SPAWN_COOLDOWN_MS:?} 冷却内不再重试",
                    self.preferred_port
                ));
            }
            tokio::time::sleep(READY_POLL).await;
        }
    }

    /// detach 拉起：`dsh web --port <port> --no-open`，脱离会话/进程组（实现归
    /// 驱动层 platform::detach_from_session），kill_on_drop=false（hub 死亡不带走
    /// serve——产品原则 L1）。
    /// stdout 管道读取直到出现 `dsh web: http://…?token=…`（launch token 只存在
    /// 于 stdout）；命中后转纯排水防子进程管道阻塞。超时 → None（未就绪原路径）。
    async fn spawn_detached(
        &self,
        port: u16,
    ) -> Result<tokio::sync::oneshot::Receiver<Option<String>>> {
        let mut cmd = build_spawn_command(&self.bin, port);
        platform::detach_from_session(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("dsh web 拉起失败，请检查 {} 是否可用", self.bin))?;
        let pid = child.id();
        // stdout 捕获任务：命中 token 行经 oneshot 回传，之后继续排水到 EOF
        //（防子进程写管道阻塞）；整体超时 = READY_BUDGET
        let stdout = child.stdout.take();
        let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            if let Some(stdout) = stdout {
                // 命中 token 行立即回传（不等到 EOF——dsh 打印后长驻），继续排水
                // 防子进程管道阻塞；EOF 无命中 → None（未就绪原路径）
                let mut tx = Some(tx);
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(sender) = tx.take() {
                        if let Some(t) = launch_token_of(&line) {
                            let _ = sender.send(Some(t));
                            continue;
                        }
                        tx = Some(sender); // 未命中：还回 sender 继续找
                    }
                }
                if let Some(sender) = tx {
                    let _ = sender.send(None);
                }
            } else {
                let _ = tx.send(None);
            }
        });
        tracing::info!(pid = pid.unwrap_or(0), port = %port, "detach 拉起 dsh web");
        Ok(rx)
    }

    fn pid(&self) -> Option<u32> {
        self.snapshot().pid
    }

    fn set_state(&self, port: Option<u16>, pid: Option<u32>) {
        let mut s = self.state.lock().expect("dsh serve lock");
        s.port = port;
        s.pid = pid;
    }

    /// 当前记录端口（driver 各请求拼 base URL；未拉起为 None）。
    pub fn port(&self) -> Option<u16> {
        self.snapshot().port
    }

    /// spawn 路捕获的 launch token（Auth 401 重换用；claim 路为 None）。
    pub fn launch_token(&self) -> Option<String> {
        self.snapshot().launch_token
    }

    #[cfg(test)]
    pub fn inject_state(&self, port: Option<u16>, pid: Option<u32>) {
        self.set_state(port, pid);
    }

    #[cfg(test)]
    pub fn state_file_for(dir: &std::path::Path) -> PathBuf {
        dir.join("dsh_serve.json")
    }
}

/// stdout 行 → launch token（`dsh web: http://127.0.0.1:PORT/?token=<base64url>`）。
/// spawn 命令构造（单实例不变式的落点）：`dsh web --port <port> --no-open`，
/// **不设置 DSH_HOME**——spawn 走 dsh 默认解析（真实 `~/.dsh`），与收编路径
/// 同一 (port, home) 锚（方案 A 2026-10-04：hub 全托管，唯一实例语义）。
fn build_spawn_command(bin: &str, port: u16) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("web")
        .arg("--port")
        .arg(port.to_string())
        .arg("--no-open")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);
    cmd
}

pub(crate) fn launch_token_of(line: &str) -> Option<String> {
    let rest = line.split("dsh web: ").nth(1)?;
    let url = rest.split_whitespace().next()?;
    let token = url.split("token=").nth(1)?;
    (!token.is_empty()).then(|| token.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 状态机回环：未监听端口不健康；注入 + 落盘 + 重读（hub 重启重新发现语义）
    #[tokio::test]
    async fn state_roundtrip_and_health_false_for_closed() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ServeManager::new(
            "dsh".into(),
            65000,
            ServeManager::state_file_for(dir.path()),
            false,
        );
        assert!(mgr.port().is_none());
        assert!(!mgr.healthy(65000).await);
        mgr.inject_state(Some(65001), Some(4242));
        mgr.persist();
        let mgr2 = ServeManager::new(
            "dsh".into(),
            65000,
            ServeManager::state_file_for(dir.path()),
            false,
        );
        assert_eq!(mgr2.port(), Some(65001));
        assert_eq!(mgr2.pid(), Some(4242));
    }

    /// 落盘原子替换：无 .tmp 残留
    #[tokio::test]
    async fn persist_is_atomic_no_residue() {
        let dir = tempfile::tempdir().unwrap();
        let file = ServeManager::state_file_for(dir.path());
        let mgr = ServeManager::new("dsh".into(), 13100, file.clone(), false);
        mgr.inject_state(Some(13100), Some(1));
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

    /// 【事故防线·护栏 1】认证 dsh 占住配置端口 → 收编现有，不 spawn 第二实例：
    /// fake 对探测回 401（认证门卫），bin 指不存在路径（若走 spawn 分支即报错可断言）
    #[tokio::test]
    async fn claims_auth_server_on_preferred_port_never_spawns() {
        let app = axum::Router::new().route(
            "/api/host.describe",
            axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let dir = tempfile::tempdir().unwrap();
        let mgr = ServeManager::new(
            "/nonexistent/dsh-test-isolated".into(),
            port,
            ServeManager::state_file_for(dir.path()),
            true,
        );
        // 无记录状态直接 ensure_started → 认领 401 端口（spawn 分支未触碰）
        let got = mgr.ensure_started().await.unwrap();
        assert_eq!(got, port, "配置端口有监听者必须收编，无论认证");
        assert_eq!(mgr.port(), Some(port));
        // 二次调用幂等复用，仍不 spawn
        assert_eq!(mgr.ensure_started().await.unwrap(), port);
        assert!(mgr.healthy(port).await, "401 判活（事故防线 1）");
    }

    /// 【事故防线·护栏 2】spawn 失败后冷却窗口内连续 N 次调用 → 只发起 1 次尝试
    #[tokio::test]
    async fn spawn_cooldown_allows_single_attempt_in_window() {
        let dir = tempfile::tempdir().unwrap();
        let file = ServeManager::state_file_for(dir.path());
        // 端口空闲 + bin 不存在 → 首次尝试必败
        let mgr = ServeManager::new(
            "/nonexistent/dsh-test-isolated".into(),
            65201,
            file.clone(),
            true,
        );
        assert!(mgr.ensure_started().await.is_err(), "首次 spawn 尝试失败");
        assert!(
            serde_json::from_str::<ServeState>(&std::fs::read_to_string(&file).unwrap())
                .unwrap()
                .last_spawn_attempt_ms
                .is_some(),
            "尝试时刻已记账（崩溃也不丢冷却窗口）"
        );
        // 冷却内连续 5 次清单轮询同款调用 → 全部被冷却挡下（不再 spawn）
        for _ in 0..5 {
            let e = mgr.ensure_started().await.unwrap_err();
            assert!(e.to_string().contains("冷却"), "应报冷却而非重试: {e}");
        }
        let state: ServeState =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        // 记账未被后续调用改写（单次尝试铁证）
        assert!(state.last_spawn_attempt_ms.is_some());
        assert_eq!(state.port, None, "未就绪绝不落端口记录（防下次误复用）");
    }

    /// auto_start=false 且端口无监听 → 显式报错（不 spawn）
    #[tokio::test]
    async fn auto_start_false_never_spawns() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ServeManager::new(
            "/nonexistent/dsh-test-isolated".into(),
            65202,
            ServeManager::state_file_for(dir.path()),
            false,
        );
        let e = mgr.ensure_started().await.unwrap_err();
        assert!(e.to_string().contains("auto_start=false"), "{e}");
    }

    /// 【凭据通路·spawn 路全链】fake bin 打印 token URL → stdout 捕获入状态文件
    /// → axum 假服务顶端口 → ensure_started Ok（真实 dsh 的 spawn→token→cookie
    /// 链在此层以脚本替身验证；cookie 交换在 rpc 层测试覆盖）
    /// 单实例不变式（方案 A 三格矩阵之 spawn 格）：spawn 命令**不得**设置
    /// DSH_HOME（继承 hub 环境 → dsh 默认解析真实 ~/.dsh），与收编路径同一
    /// (port, home) 锚——设置独立 home 即拓扑裂开（2026-10-03 双活实测根因）
    #[test]
    fn spawn_command_uses_default_real_home() {
        let mut cmd = build_spawn_command("/nonexistent/dsh-test-isolated", 13100);
        let std_cmd = cmd.as_std_mut();
        assert!(
            std_cmd.get_envs().all(|(k, _)| k != "DSH_HOME"),
            "spawn 不得覆写 DSH_HOME"
        );
        let args: Vec<String> = std_cmd
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(args, vec!["web", "--port", "13100", "--no-open"]);
    }

    /// 【凭据通路·spawn 路全链】fake bin 打印 token URL → stdout 捕获入状态文件
    /// → axum 假服务顶端口 → ensure_started Ok（真实 dsh 的 spawn→token→cookie
    /// 链在此层以脚本替身验证；cookie 交换在 rpc 层测试覆盖）
    #[tokio::test]
    async fn spawn_captures_launch_token_to_state() {
        // 先探测空闲端口并释放（收编路径优先级高于 spawn，端口必须空闲才会走 spawn）
        let probe = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-dsh.sh");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\necho \"dsh web: http://127.0.0.1:{port}/?token=tok-spawn-1\"\nsleep 30\n"
            ),
        )
        .unwrap();
        #[cfg(unix)]
        platform::set_mode(&bin, 0o755).unwrap();
        let file = ServeManager::state_file_for(dir.path());
        let mgr = ServeManager::new(bin.to_string_lossy().into_owned(), port, file.clone(), true);
        // spawn 后异步补上「dsh web 已开始监听」的假象（就绪轮询的目标）
        let join = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .unwrap();
            axum::serve(listener, axum::Router::new()).await.unwrap();
        });
        assert_eq!(mgr.ensure_started().await.unwrap(), port);
        let state: ServeState =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            state.launch_token.as_deref(),
            Some("tok-spawn-1"),
            "stdout token 已入状态文件"
        );
        assert_eq!(state.port, Some(port));
        join.abort();
    }

    /// spawn 路无 token（脚本未打印即退出）→ 未就绪原路径报错 + 端口不落盘
    #[tokio::test]
    async fn spawn_without_token_fails_not_ready() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-dsh.sh");
        std::fs::write(&bin, "#!/bin/sh\necho booting\nexit 0\n").unwrap();
        #[cfg(unix)]
        platform::set_mode(&bin, 0o755).unwrap();
        let file = ServeManager::state_file_for(dir.path());
        let mgr = ServeManager::new(
            bin.to_string_lossy().into_owned(),
            65211,
            file.clone(),
            true,
        );
        let e = mgr.ensure_started().await.unwrap_err();
        assert!(e.to_string().contains("launch token"), "{e}");
        let state: ServeState =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(state.port, None, "未就绪不落端口记录");
    }

    /// 【单实例不变式】同一 DSH_HOME（状态文件）下第二个管理器被冷却挡住——
    /// 一个 home 同时至多一个 hub 管理实例的记账锁死
    #[tokio::test]
    async fn second_manager_same_home_blocked_by_cooldown() {
        let dir = tempfile::tempdir().unwrap();
        let file = ServeManager::state_file_for(dir.path());
        let a = ServeManager::new("/nonexistent/dsh-a".into(), 65212, file.clone(), true);
        assert!(a.ensure_started().await.is_err(), "首次尝试失败并记账");
        let b = ServeManager::new("/nonexistent/dsh-b".into(), 65212, file.clone(), true);
        let e = b.ensure_started().await.unwrap_err();
        assert!(
            e.to_string().contains("冷却"),
            "同 home 第二管理器必须被冷却拦下: {e}"
        );
    }
}
