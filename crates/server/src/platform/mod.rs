//! 平台驱动层（agent-hub-platform-driver 一期）：OS 级差异 API 的唯一容身处。
//! 业务模块（tasks / terminal / drivers/* / harness / tunnel / pi 配置面等）一律
//! 面向本层稳定接口编程，编译期按目标 OS 挂载实现（unix / windows 两个实现模块
//! 导出同名函数集，`pub use` 装配；unix 内 macos 与 linux 共享主线，差异点按
//! target_os 分叉）。业务逻辑零改动——本层只搬 API 归属，不改行为语义。
//!
//! 驱动面（对应需求 D1–D5）：
//! - D1 进程树管理：进程组/脱离会话 spawn、组杀与单杀、派生树收集
//! - D2 终端与 shell：默认 shell 解析、登录态参数、PATH 查找、路径分隔符判别
//! - D3 互斥文件锁：非阻塞独占 + 持有者死亡自动释放（fslock：unix flock /
//!   windows LockFileEx 双平台覆盖，选型见需求 D3 候选，实现在本文件无 OS 分叉）
//! - D4 环境与路径：HOME 解析、PATH 富化与搜索目录、可执行判定、权限位读写、
//!   敏感文件 0600
//! - D5 杂项：占位长眠进程（测试注入用）
//!
//! 红线（W1 收敛门）：本层之外禁止出现 `libc::` / `std::os::unix::` / `killpg` /
//! `setsid` / `/opt/homebrew` / `/bin/sleep` / `$SHELL` 直读——白名单仅本目录。
//! Windows 分支一期以「编译通过 + 结构正确」为准，行为正确性为二期真机验证欠账，
//! 清单见 docs/reports/agent-hub-platform-driver/platform-matrix.md。

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::*;
#[cfg(windows)]
pub use windows::*;

use std::io;
use std::path::{Path, PathBuf};

/// D1 杀进程的强度：Term = 优雅请求（unix SIGTERM / windows taskkill 不带 /F），
/// Kill = 强制（unix SIGKILL / windows taskkill /F）。errno/非零退出按「目标可能
/// 已退出」忽略，语义与收敛前各调用点一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Term,
    Kill,
}

// —— D3 互斥文件锁（fslock 双平台覆盖，无 OS 分叉） ——

/// 排他文件锁句柄：保活即持锁，drop（含进程死亡/kill -9）OS 自动释放。
pub struct FileLock(#[allow(dead_code)] fslock::LockFile); // 字段即持锁本体（保活语义），无需读取

/// D3：非阻塞独占获取 `path` 的文件锁（文件不存在则创建）。被他方持有返回 None。
/// 同进程两次独立 open 同样互斥（unix flock 绑 open file description，与跨进程
/// 语义同源；windows LockFileEx 按句柄互斥，同语义）。
pub fn try_lock_file(path: &Path) -> Option<FileLock> {
    let mut lock = fslock::LockFile::open(path).ok()?;
    lock.try_lock().ok()?.then_some(FileLock(lock))
}

// —— D4 便捷封装（跨平台同形，转发到各 OS 实现） ——

/// 用户 home 目录；未设置 panic（与收敛前 config.rs 各点同消息同路径）。
pub fn home_dir() -> PathBuf {
    home_dir_opt().expect("HOME 环境变量未设置，无法定位数据目录")
}

/// 同上，字符串形（config.rs 的 format! 路径拼接口径；类型与收敛前
/// `env::var("HOME")` 的 String 一致）。
pub fn home_dir_str() -> String {
    home_dir().to_string_lossy().into_owned()
}

/// 敏感文件写后强制 0600（结果忽略——与收敛前 dsh/serve persist 同语义）。
pub fn harden_0600(path: &Path) {
    let _ = set_mode(path, 0o600);
}

/// 同上，async 版（tokio::fs；错误上抛——与收敛前 piconf 同语义）。
pub async fn harden_0600_async(path: &Path) -> io::Result<()> {
    set_mode_async(path, 0o600).await
}

/// 搜索目录富化（继承 PATH + 平台常见安装位，已存在才追加、去重）后按平台分隔符
/// 切分。harness 探测的搜索路径来源。
pub fn search_path_dirs() -> Vec<PathBuf> {
    let path = extended_search_path(
        &std::env::var("PATH").unwrap_or_default(),
        &home_dir_opt().unwrap_or_default().to_string_lossy(),
    );
    path.split(PATH_SEP)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// host 开机时长（秒；P3 host stats presence）。mac 走 sysctl kern.boottime，
/// linux 走 /proc/stat 的 btime 秒差；windows P3 首版如实 0（需要时再接
/// windows API）。失败回退 0——presence 缺失如实，不阻塞 announce。
pub fn host_uptime_secs() -> u64 {
    #[cfg(target_os = "macos")]
    {
        use std::mem;
        // P3 缺陷②（tester 回边）修：原实现把 ASCII 串 b"kern.boottime\0" 当
        // MIB 整型数组传给 libc::sysctl——前两字节 0x6b/0x65 被解释成
        // name[0]=110/name[1]=101（CTL_KERN=1 不在首位），sysctl 恒失败回退
        // ENOENT → uptime 恒 0（错数据而非如实缺失）。按名查询用 sysctlbyname，
        // 输出仍为 struct timeval（开机时刻）。
        let name = c"kern.boottime".as_ptr();
        unsafe {
            let mut boot = libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            };
            let mut size = mem::size_of::<libc::timeval>();
            if libc::sysctlbyname(
                name,
                &mut boot as *mut _ as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            ) == 0
            {
                let now = libc::time(std::ptr::null_mut());
                if now > boot.tv_sec {
                    return (now - boot.tv_sec) as u64;
                }
            }
        }
        0
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(stat) = std::fs::read_to_string("/proc/stat") {
            for line in stat.lines() {
                if let Some(rest) = line.strip_prefix("btime ") {
                    if let Ok(btime) = rest.trim().parse::<u64>() {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        return now.saturating_sub(btime);
                    }
                }
            }
        }
        0
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P3 tester 缺陷②回归锚：host uptime 应为正（缺陷②前 mac 恒 0）
    #[test]
    fn host_uptime_secs_positive() {
        assert!(
            host_uptime_secs() > 0,
            "host uptime 应为正（boottime 可读）"
        );
    }

    /// D3 S1/S4 驱动层直测：同进程两次独立 open 互斥；drop（= 持锁方死亡路径）
    /// 后锁释放，等待方可接管（业务侧回归见 tasks::scheduler_lock_* 同款）。
    #[test]
    fn try_lock_file_is_exclusive_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "hub-platform-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("probe.lock");
        let first = try_lock_file(&path);
        assert!(first.is_some(), "首个持有方应取得锁");
        let second = try_lock_file(&path);
        assert!(second.is_none(), "持锁期间第二个持有方不得取得");
        drop(first);
        // Darwin 的 flock 释放存在短暂竞态（drop 返回 ≠ 内核立即可重取）：
        // 与生产 standby 的 tick 重试同款，窗口内多次尝试再断言
        let mut third = None;
        for _ in 0..50 {
            third = try_lock_file(&path);
            if third.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(third.is_some(), "锁释放后等待方应接管");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// home 目录解析：设置了就该拿到（CI/本机环境恒有）。
    #[test]
    fn home_dir_resolves() {
        assert!(home_dir_opt().is_some());
        assert!(home_dir().is_dir());
    }

    /// D2 默认 shell 解析（收敛自 api/terminal 原测）：$SHELL 优先且须绝对路径，
    /// 非绝对路径/未设置回退平台缺省。
    #[cfg(unix)]
    #[test]
    fn default_shell_prefers_env_and_absolute() {
        // 回退缺省随平台驱动（mac=zsh / 其余 unix=bash），断言取同源常量勿硬编码
        let fallback = if cfg!(target_os = "macos") {
            "/bin/zsh"
        } else {
            "/bin/bash"
        };
        // SAFETY: 单测串行场景下改进程 env 可接受（cargo test 默认多线程，但本测试
        // 只读回自己刚写的值，其他测试不读 SHELL）
        unsafe { std::env::set_var("SHELL", "/bin/fake-shell-for-test") };
        assert_eq!(default_shell(), PathBuf::from("/bin/fake-shell-for-test"));
        unsafe { std::env::set_var("SHELL", "relative-not-allowed") };
        assert_eq!(default_shell(), PathBuf::from(fallback)); // 非绝对路径回退缺省
        unsafe { std::env::remove_var("SHELL") };
        assert_eq!(default_shell(), PathBuf::from(fallback));
    }

    /// D4 搜索路径富化（收敛自 harness 原测，macOS 候选集口径）。
    #[cfg(all(unix, target_os = "macos"))]
    #[test]
    fn extended_search_path_appends_existing_dirs_and_dedupes() {
        let out = extended_search_path("/usr/bin:/opt/homebrew/bin", "/home/test");
        let parts: Vec<&str> = out.split(PATH_SEP).collect();
        assert_eq!(parts[0], "/usr/bin"); // 继承 PATH 保序在前
        assert_eq!(
            parts.iter().filter(|p| **p == "/opt/homebrew/bin").count(),
            1
        );
        assert!(parts.contains(&"/usr/local/bin"));
        assert!(!parts.iter().any(|p| p.contains(".cargo"))); // /home/test/.cargo/bin 不存在
    }
}
