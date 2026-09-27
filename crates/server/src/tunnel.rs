//! SSH 隧道管理器（agent-hub-multi-instance 批1 任务3）：spawn 本机 `ssh -N -L`
//! 本地端口转发，管理隧道生命周期（启动/健康检查/断线重连/退出回收）。
//! 三种认证：证书路径（-i）/ authsock（SSH_AUTH_SOCK env）/ 密码（sshpass -e，
//! 密码经环境变量传入子进程——不进 argv 不落盘）。参数数组传递，禁 shell 拼接。

use std::{
    collections::HashMap,
    net::TcpListener,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use tokio::process::Command;

use crate::instances::{InstanceConfig, InstanceMode, SshAuth};

/// 重连退避指数基数（秒），上限 MAX_RETRIES 次后放弃
const MAX_RETRIES: u32 = 5;
/// 健康轮询间隔：子进程数秒未退出即判活，防空转占用 CPU
const IDLE_POLL: Duration = Duration::from_secs(2);

/// 测试注入的退避步长（生产用 1s 底数；测试 5ms 防慢测）。
#[cfg(test)]
static FAKE_STEP: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
#[cfg(test)]
fn backoff_unit() -> Duration {
    FAKE_STEP.get().copied().unwrap_or(Duration::from_millis(5))
}
#[cfg(not(test))]
fn backoff_unit() -> Duration {
    Duration::from_secs(1)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TunnelStatus {
    pub running: bool,
    pub local_port: Option<u16>,
    /// 阶段说明：not_started / running / failed（重连耗尽）
    pub state: String,
    pub retries: u32,
}

/// 一条隧道进程的共享句柄。
#[derive(Clone)]
struct TunnelHandle {
    child: Arc<tokio::sync::Mutex<Option<tokio::process::Child>>>,
    local_port: u16,
    /// stop/退出信号：置位后重连循环结束
    kill_flag: Arc<std::sync::atomic::AtomicBool>,
    /// 上次 ssh 连接参数快照（重连用，持 inst 克隆）
    inst: InstanceConfig,
}

/// 进程内隧道表（AppState 持有 Arc）。手动 Debug（TunnelHandle 无 Debug）。
#[derive(Clone, Default)]
pub struct TunnelManager {
    inner: Arc<Mutex<HashMap<String, TunnelHandle>>>,
}

impl std::fmt::Debug for TunnelManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelManager")
            .field("active", &self.inner.lock().expect("tunnel map lock").len())
            .finish()
    }
}

impl TunnelManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 探测空闲本地端口（bind 0 → 取端口 → 释放）。
    /// ponytail: bind 后释放有极小竞争窗口；ExitOnForwardFailure=yes 使 ssh 端口
    /// 占用时启动失败可见，经重连上限兜底（tasks.md 风险已标注）。
    pub fn alloc_local_port() -> Result<u16> {
        let l = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| anyhow!("端口探测失败: {e}"))?;
        Ok(l.local_addr()
            .map_err(|e| anyhow!("端口读取失败: {e}"))?
            .port())
    }

    /// 构造 ssh 命令（返回 Command + 本地端口）。local_port=Some 表示重连复用端口。
    /// 密码路径 sshpass -e：密码走 env，不进 argv。参数数组传递（无 shell 拼接）。
    fn build_command(inst: &InstanceConfig, local_port: u16) -> Result<Command> {
        let ssh = inst
            .ssh
            .as_ref()
            .ok_or_else(|| anyhow!("ssh-tunnel 实例缺 ssh 参数"))?;
        let remote_port = inst.remote_port.ok_or_else(|| anyhow!("缺远程目标端口"))?;

        let mut args = vec![
            "-N".to_string(),
            "-L".to_string(),
            format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"),
            "-o".to_string(),
            "ExitOnForwardFailure=yes".into(),
            "-o".to_string(),
            "ServerAliveInterval=15".into(),
            "-o".to_string(),
            "ServerAliveCountMax=3".into(),
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".into(),
        ];
        match ssh.auth {
            SshAuth::KeyPath => {
                let kp = ssh
                    .key_path
                    .as_ref()
                    .ok_or_else(|| anyhow!("key-path 认证缺密钥路径"))?;
                args.push("-i".into());
                args.push(kp.clone());
            }
            SshAuth::Authsock | SshAuth::Password => {}
        }
        if ssh.port != 22 {
            args.push("-p".into());
            args.push(ssh.port.to_string());
        }
        args.push(format!("{user}@{host}", user = ssh.user, host = ssh.host));

        let mut cmd = Command::new("ssh");
        cmd.args(&args)
            .process_group(0) // 独立进程组，exit 时 killpg 连坐
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if ssh.auth == SshAuth::Authsock {
            if let Ok(v) = std::env::var("SSH_AUTH_SOCK") {
                cmd.env("SSH_AUTH_SOCK", v);
            }
        }

        // 密码认证：前置 sshpass（探测缺失 → 引导错误；不阻塞证书/authsock 路径）
        if ssh.auth == SshAuth::Password {
            let pass = ssh.password.as_deref().unwrap_or("");
            if pass.is_empty() {
                anyhow::bail!("密码认证需配置密码");
            }
            if !sshpass_available() {
                anyhow::bail!(
                    "密码认证需要 sshpass（本机未安装）。请安装 sshpass 或改用证书路径 / authsock 认证"
                );
            }
            let mut sp = Command::new("sshpass");
            sp.arg("-e").arg("ssh").args(&args).env("SSHPASS", pass);
            return Ok(sp);
        }
        Ok(cmd)
    }

    /// 启动（幂等）：已运行返回已分配端口；否则 spawn + 起重连监督循环。
    pub async fn start(&self, inst: &InstanceConfig) -> Result<u16> {
        if inst.mode != InstanceMode::SshTunnel {
            anyhow::bail!("仅 ssh-tunnel 实例支持隧道");
        }
        // 已运行 → 幂等返回（句柄克隆在独立块内取出，std map 锁随块结尾释放
        // 后再 await 子进程锁——std guard 不跨 await）
        {
            let existing: Option<TunnelHandle> = {
                let map = self.inner.lock().expect("tunnel map lock");
                map.get(&inst.id).cloned()
            };
            if let Some(existing) = existing {
                if existing.child.lock().await.is_some() {
                    return Ok(existing.local_port);
                }
            }
        }
        let local_port = Self::alloc_local_port()?;
        let mut cmd = Self::build_command(inst, local_port)?;
        let child = cmd
            .spawn()
            .context("ssh 子进程启动失败，请检查 ssh 是否可用")?;
        let handle = TunnelHandle {
            child: Arc::new(tokio::sync::Mutex::new(Some(child))),
            local_port,
            kill_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            inst: inst.clone(),
        };
        let mgr = self.clone();
        let id = inst.id.clone();
        let h2 = handle.clone();
        self.inner
            .lock()
            .expect("tunnel map lock")
            .insert(id.clone(), handle);
        tokio::spawn(async move {
            mgr.supervise(id, h2).await;
        });
        Ok(local_port)
    }

    /// 监督循环：子进程退出且端口不通 → 指数退避重启；stop 或重连耗尽 → 结束。
    async fn supervise(&self, id: String, handle: TunnelHandle) {
        let mut retries = 0u32;
        loop {
            {
                let mut guard = handle.child.lock().await;
                let mut exited = false;
                if let Some(c) = guard.as_mut() {
                    let exited_now = tokio::time::timeout(IDLE_POLL, c.wait()).await.is_ok();
                    if exited_now {
                        exited = true;
                    }
                }
                if !exited {
                    // 进程存活且端口连通 = 健康 → 重置重试
                    if port_open(handle.local_port).await {
                        retries = 0;
                    }
                    drop(guard);
                    tokio::time::sleep(backoff_unit()).await;
                    continue;
                }
                // 子进程已退出：清空 child 槽
                *guard = None;
                drop(guard);
            }
            if handle.kill_flag.load(std::sync::atomic::Ordering::SeqCst) {
                break; // stop 已请求
            }
            // 端口仍通 = 隧道实际活着（僵尸）→ 不重连
            if port_open(handle.local_port).await {
                retries = 0;
                tokio::time::sleep(backoff_unit()).await;
                continue;
            }
            if retries >= MAX_RETRIES {
                tracing::warn!(instance = %id, "隧道重连已达上限，放弃");
                break;
            }
            retries += 1;
            // 重连：同参数重建（local_port 固定）
            let mut cmd = match Self::build_command(&handle.inst, handle.local_port) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(instance = %id, error = %e, "重连构造失败");
                    continue;
                }
            };
            let backoff = backoff_unit() * (1u32 << (retries - 1));
            tokio::time::sleep(backoff).await;
            if handle.kill_flag.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            match cmd.spawn() {
                Ok(child) => {
                    let existing = self
                        .inner
                        .lock()
                        .expect("tunnel map lock")
                        .get(&id)
                        .cloned();
                    if let Some(h) = existing {
                        *h.child.lock().await = Some(child);
                    }
                }
                Err(_) => { /* spawn 失败不算重连次数，下一轮再试 */ }
            }
        }
        // 结束：确保 child 槽清空（进程已回收）
        *handle.child.lock().await = None;
    }

    /// 停止：置 kill_flag → SIGTERM 进程组 → 宽限 → SIGKILL 连坐；幂等。
    pub async fn stop(&self, id: &str) -> bool {
        let Some(h) = self.inner.lock().expect("tunnel map lock").get(id).cloned() else {
            return false;
        };
        h.kill_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut guard = h.child.lock().await;
        let Some(c) = guard.as_mut() else {
            self.inner.lock().expect("tunnel map lock").remove(id);
            return false;
        };
        if let Some(pid) = c.id() {
            // SAFETY: killpg 传一个已存在进程组 id；errno 忽略（组可能已退出）
            unsafe {
                libc::killpg(pid as i32, libc::SIGTERM);
            }
        }
        match tokio::time::timeout(Duration::from_secs(3), c.wait()).await {
            Ok(_) => {}
            Err(_) => {
                if let Some(pid) = c.id() {
                    // SAFETY: 同上，宽限期满强制连坐
                    unsafe {
                        libc::killpg(pid as i32, libc::SIGKILL);
                    }
                    let _ = c.wait().await;
                }
            }
        }
        self.inner.lock().expect("tunnel map lock").remove(id);
        true
    }

    pub async fn status(&self, id: &str) -> TunnelStatus {
        let Some(h) = self.inner.lock().expect("tunnel map lock").get(id).cloned() else {
            return TunnelStatus {
                running: false,
                local_port: None,
                state: "not_started".into(),
                retries: 0,
            };
        };
        let running = h.child.lock().await.is_some();
        TunnelStatus {
            running,
            local_port: Some(h.local_port),
            state: if running {
                "running".into()
            } else {
                "failed".into()
            },
            retries: 0,
        }
    }

    /// 停机回收全部隧道（server shutdown）。
    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = self
            .inner
            .lock()
            .expect("tunnel map lock")
            .keys()
            .cloned()
            .collect();
        for id in ids {
            self.stop(&id).await;
        }
    }
}

fn sshpass_available() -> bool {
    // 探测 PATH 中是否存在 sshpass 二进制（只判存，不执行）
    std::process::Command::new("which")
        .arg("sshpass")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn port_open(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn alloc_port_is_unique_and_reusable() {
        let p = TunnelManager::alloc_local_port().unwrap();
        assert!(p > 0 && u32::from(p) < 65536);
        // 已释放 → 可再 bind（不冲突即证明逻辑释放了）
        let l = TcpListener::bind(("127.0.0.1", p)).unwrap();
        drop(l);
    }

    /// 密码认证：本机无 sshpass → 必返引导性错误（不阻塞证书/authsock 路径）
    #[tokio::test]
    async fn password_without_sshpass_gives_guidance() {
        if sshpass_available() {
            return;
        }
        let inst = InstanceConfig {
            id: "i1".into(),
            name: "x".into(),
            mode: InstanceMode::SshTunnel,
            url: None,
            token: None,
            ssh: Some(crate::instances::SshConfig {
                host: "localhost".into(),
                port: 22,
                user: "u".into(),
                auth: SshAuth::Password,
                key_path: None,
                password: Some("secret".into()),
            }),
            remote_port: Some(22),
            local_port: None,
        };
        let err = TunnelManager::build_command(&inst, 0).expect_err("密码认证应失败");
        assert!(
            err.to_string().contains("sshpass"),
            "缺 sshpass 应给引导性错误: {err}"
        );
    }
}
