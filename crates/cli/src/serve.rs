//! `ah serve`：hub 守护形态（生命周期铁律：serve 只负责拉起与探活，绝不托管会话状态——
//! 归 hub 管不归 hub 有）。pid 记录落 `~/.claude-view/ah.pid`（env AH_PID_FILE 覆盖）。

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::api::Hub;

/// pid 记录：pid + 拉起时的 hub url（stop 侧展示用；防误杀他口实例靠显式 --url 对照）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PidRecord {
    pub pid: u32,
    pub url: String,
}

/// pid 文件路径：env `AH_PID_FILE` > `~/.claude-view/ah.pid`。
pub fn pid_file() -> PathBuf {
    std::env::var("AH_PID_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            format!(
                "{}/.claude-view/ah.pid",
                std::env::var("HOME").unwrap_or_else(|_| ".".into())
            )
            .into()
        })
}

/// server 日志：env `AH_SERVER_LOG` > `~/.claude-view/ah-server.log`。
fn server_log() -> PathBuf {
    std::env::var("AH_SERVER_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            format!(
                "{}/.claude-view/ah-server.log",
                std::env::var("HOME").unwrap_or_else(|_| ".".into())
            )
            .into()
        })
}

/// server bin 解析：`--bin` > env `AH_SERVER_BIN` > 与 ah 同目录的 `server`
///（cargo release 同落 target/release/，双 bin 天然同目录）。
pub fn resolve_server_bin(override_bin: Option<&str>) -> anyhow::Result<PathBuf> {
    let p = override_bin
        .map(PathBuf::from)
        .or_else(|| std::env::var("AH_SERVER_BIN").ok().map(PathBuf::from))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|d| d.parent().map(|d| d.join("server")))
        })
        .context("无法定位 server 二进制（--bin 指定或与 ah 同目录放置）")?;
    if !p.exists() {
        bail!("server 二进制不存在: {}（--bin 指定路径）", p.display());
    }
    Ok(p)
}

pub fn read_pid(path: &Path) -> Option<PidRecord> {
    let raw = fs::read_to_string(path).ok()?;
    // 解析失败 = 陈旧损坏文件，按无记录处理（调用方清理）
    serde_json::from_str(&raw).ok()
}

pub fn pid_alive(pid: u32) -> bool {
    // kill(pid, 0) 只探活不发信号；EPERM（存在但无权）也算活
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// detach 拉起：独立进程组（不随 ah 退出/终端 SIGHUP 波及），输出并 append 进日志文件。
fn spawn_detached(bin: &Path, args: &[&str], log: &Path) -> anyhow::Result<u32> {
    let logf = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .with_context(|| format!("打开日志文件失败: {}", log.display()))?;
    let stderr = logf.try_clone().context("复用日志句柄失败")?;
    let child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(logf)
        .stderr(stderr)
        .process_group(0) // SAFETY 语义：unix 专属 API，无内存安全影响
        .spawn()
        .with_context(|| format!("拉起失败: {}", bin.display()))?;
    Ok(child.id())
}

async fn wait_healthy(hub: &Hub, tries: usize) -> bool {
    for _ in 0..tries {
        if hub.healthy().await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    false
}

/// 幂等 start：健康检查通过即认为在跑；否则 detach 拉起 + pid 落盘 + 探活（≤15s）。
pub async fn start(bin: Option<&str>, url: &str, hub: &Hub) -> anyhow::Result<String> {
    if hub.healthy().await {
        return Ok(format!("hub 已在运行于 {url}，无需拉起"));
    }
    let bin_path = resolve_server_bin(bin)?;
    let log = server_log();
    // server 读 env 装配（AGENT_HUB_BIND 等），继承 ah 进程环境即可定向到目标端口
    let pid = spawn_detached(&bin_path, &[], &log)?;
    fs::write(
        pid_file(),
        serde_json::to_string(&PidRecord {
            pid,
            url: url.into(),
        })?,
    )
    .with_context(|| format!("写 pid 文件失败: {}", pid_file().display()))?;
    if wait_healthy(hub, 75).await {
        Ok(format!(
            "已拉起 server（pid={pid}，url={url}，日志={}）",
            log.display()
        ))
    } else {
        let _ = stop(&pid_file());
        bail!("拉起后 15s 健康检查未通过，已回滚；日志: {}", log.display())
    }
}

/// stop：SIGTERM → 3s 宽限 → SIGKILL；stale pid（进程已不在）直接清理。
pub fn stop(path: &Path) -> anyhow::Result<String> {
    let Some(rec) = read_pid(path) else {
        return Ok("未在运行（无 pid 文件）".into());
    };
    if !pid_alive(rec.pid) {
        let _ = fs::remove_file(path);
        return Ok(format!(
            "pid {} 已不存在（stale），已清理 pid 文件",
            rec.pid
        ));
    }
    // 定向 kill 记录的 pid（非 pkill）；只动 ah serve 自己拉起的实例
    unsafe { libc::kill(rec.pid as libc::pid_t, libc::SIGTERM) };
    for _ in 0..30 {
        if !pid_alive(rec.pid) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if pid_alive(rec.pid) {
        unsafe { libc::kill(rec.pid as libc::pid_t, libc::SIGKILL) };
    }
    let _ = fs::remove_file(path);
    Ok(format!("已停止 server（pid={}，url={}）", rec.pid, rec.url))
}

/// status：pid 文件 + 健康探针双面报告。
pub async fn status(path: &Path, url: &str, hub: &Hub) -> anyhow::Result<String> {
    let pid_part = match read_pid(path) {
        Some(rec) => {
            if pid_alive(rec.pid) {
                format!("pid={} 运行中（记录 url={}）", rec.pid, rec.url)
            } else {
                format!("pid={} 已退出（stale 记录）", rec.pid)
            }
        }
        None => "无 pid 记录（非 ah serve 拉起，或已清理）".to_string(),
    };
    let health = if hub.healthy().await {
        format!("{url} 健康检查通过")
    } else {
        format!("{url} 不可达")
    };
    Ok(format!("{pid_part}\n{health}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_roundtrip_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ah.pid");
        assert_eq!(read_pid(&path), None);
        write_record(&path, 4242, "http://127.0.0.1:7902");
        assert_eq!(
            read_pid(&path),
            Some(PidRecord {
                pid: 4242,
                url: "http://127.0.0.1:7902".into()
            })
        );
        // 坏内容 = 陈旧损坏，按无记录处理
        fs::write(&path, "not json").unwrap();
        assert_eq!(read_pid(&path), None);
    }

    fn write_record(path: &Path, pid: u32, url: &str) {
        fs::write(
            path,
            serde_json::to_string(&PidRecord {
                pid,
                url: url.into(),
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn dead_pid_detected_as_stale() {
        // macOS pid 上限 99998，该值必不存在（ESRCH）
        assert!(!pid_alive(999_999_999));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ah.pid");
        write_record(&path, 999_999_999, "http://127.0.0.1:7902");
        let msg = stop(&path).unwrap();
        assert!(msg.contains("stale"), "{msg}");
        assert!(!path.exists(), "stale 记录应被清理");
    }

    #[test]
    fn stop_without_pid_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let msg = stop(&dir.path().join("none.pid")).unwrap();
        assert!(msg.contains("未在运行"), "{msg}");
    }

    /// 真实 detach 拉起（/bin/sleep 当靶子，自杀清理；手动验证项配自动兜底）
    #[test]
    fn spawn_detached_runs_and_is_killable() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("s.log");
        let pid = spawn_detached(Path::new("/bin/sleep"), &["5"], &log).unwrap();
        assert!(pid_alive(pid), "拉起的进程应存活");
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }

    #[test]
    fn server_bin_override_wins() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("my-server");
        fs::write(&fake, "#!/bin/sh\n").unwrap();
        assert_eq!(
            resolve_server_bin(Some(fake.to_str().unwrap())).unwrap(),
            fake
        );
        // 缺失路径报错而非静默
        assert!(resolve_server_bin(Some("/nonexistent/xx")).is_err());
    }
}
