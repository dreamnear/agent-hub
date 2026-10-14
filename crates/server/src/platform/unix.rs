//! 平台驱动层 · unix 实现（macos 主线 + linux 差异点按 target_os 分叉）。
//! 从各业务模块原样收敛而来——行为语义零改动，只搬归属。

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

use super::Signal;

/// PATH 搜索分隔符（unix 惯例）。
pub const PATH_SEP: char = ':';

// —— D1 进程树管理 ——

/// 子进程入独立进程组：组内成员可被组杀连坐（收敛自 ssh_cmd / acp / pi / tunnel）。
pub fn new_process_group(cmd: &mut tokio::process::Command) {
    cmd.process_group(0);
}

/// 脱离当前会话/进程组拉起（detach 语义：hub 死亡不带走子进程；收敛自
/// opencode/serve 与 dsh/serve 的 pre_exec 拉起）。
pub fn detach_from_session(cmd: &mut tokio::process::Command) {
    // SAFETY: setsid() 仅改调用进程会话/进程组，无内存不安全；失败不致命
    //（hub 死亡时不再被连带——这正是 detach 的目的）。
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

fn sig_value(sig: Signal) -> i32 {
    match sig {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    }
}

/// 杀整个进程组（组 id = pid；unix PTY spawn / 入组 spawn 下 pgid == pid）。
// SAFETY: killpg 传一个已存在进程组 id；errno 忽略（组可能已退出）
pub fn kill_group(pid: u32, sig: Signal) {
    unsafe {
        libc::killpg(pid as i32, sig_value(sig));
    }
}

/// 杀单个进程。
// SAFETY: pid 来自子进程句柄或回收前 ps 全表枚举，无悬垂风险；errno 忽略（可能已退出）
pub fn kill_pid(pid: u32, sig: Signal) {
    unsafe {
        libc::kill(pid as i32, sig_value(sig));
    }
}

/// 派生树收集（回收前置步骤，趁根进程存活调）：ps 全表 (pid,ppid) → 自 root 沿
/// ppid 下行 BFS。job control 下 `cmd &` 后台任务 pgid 自立（组杀漏网）但 ppid
/// 仍指父进程——ppid 树不含 tty/ctty 时序竞态。错误返回空集（降级组杀）。
pub fn collect_descendants(root: u32) -> Vec<u32> {
    let out = match std::process::Command::new("ps")
        .args(["-e", "-o", "pid=,ppid="])
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let edges: Vec<(i32, i32)> = text
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse::<i32>().ok()?;
            let ppid = it.next()?.parse::<i32>().ok()?;
            Some((pid, ppid))
        })
        .collect();
    let mut descendants = Vec::new();
    let root_i = root as i32;
    let mut frontier = vec![root_i];
    let mut visited = std::collections::HashSet::new();
    visited.insert(root_i);
    while let Some(parent) = frontier.pop() {
        for (pid, ppid) in &edges {
            if *ppid == parent && visited.insert(*pid) {
                descendants.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    descendants.into_iter().map(|p| p as u32).collect()
}

// —— D2 终端与 shell ——

/// 默认交互 shell：$SHELL 优先（须绝对路径），缺省 macos /bin/zsh（工程机主
/// 场景）/ linux /bin/bash。收敛自 api/terminal resolve_shell。
pub fn default_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(if cfg!(target_os = "macos") {
                "/bin/zsh"
            } else {
                "/bin/bash"
            })
        })
}

/// PTY 交互登录态参数：unix 以 `-l` 起登录 shell（rc/alias 与用户本地终端一致）。
pub fn add_login_flag(cmd: &mut CommandBuilder) {
    cmd.arg("-l");
}

/// PATH 上找可执行（裸名；等价 `command -v`，不经 shell 不派生进程）。
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|c| is_executable(c))
}

/// 命令串是否含路径分隔符（含 = 按路径解析，不再做 PATH 搜索）。
pub fn has_path_separator(cmd: &str) -> bool {
    cmd.contains('/')
}

// —— D4 环境与路径 ——

/// 用户 home 目录（未设置返回 None，由调用方定兜底）。
pub fn home_dir_opt() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 可执行判定：存在 + 任一执行位（GUI 环境下不可执行的文件照样是死路径）。
pub fn is_executable(path: &Path) -> bool {
    path.is_file() && std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

/// 设权限位（测试造可执行/只读 fixture 与敏感文件 0600 共用）。
pub fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// 同上，async 版。
pub async fn set_mode_async(path: &Path, mode: u32) -> std::io::Result<()> {
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
}

/// 读权限位（低 9 位；读不到返回 None）。
pub fn file_mode(path: &Path) -> Option<u32> {
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

/// 敏感文件原子写的 OpenOptions（0600 建临时文件；rename 保留 mode，替换后
/// 正式文件同为 0600）。收敛自 notes.rs / instances.rs 的 `#[cfg(unix)] opts.mode()`。
pub fn private_write_options() -> tokio::fs::OpenOptions {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    opts
}

/// 搜索路径富化：继承 PATH + 常见安装位（macOS GUI 环境的 PATH 不含 homebrew
/// 前缀与 ~/.local/bin 等——与 desktop 壳 extended_path 同一先例；linux 无
/// homebrew 前缀）。已存在的目录才追加、去重、保序。
pub fn extended_search_path(inherited: &str, home: &str) -> String {
    let mut candidates: Vec<String> = vec!["/usr/local/bin".into(), "/usr/local/sbin".into()];
    if cfg!(target_os = "macos") {
        candidates.push("/opt/homebrew/bin".into());
        candidates.push("/opt/homebrew/sbin".into());
    }
    candidates.push(format!("{home}/.local/bin"));
    candidates.push(format!("{home}/.bun/bin"));
    candidates.push(format!("{home}/.cargo/bin"));
    let mut parts: Vec<String> = inherited
        .split(PATH_SEP)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    for c in &candidates {
        if Path::new(c).is_dir() && !parts.iter().any(|p| p == c) {
            parts.push(c.clone());
        }
    }
    parts.join(":")
}

/// 常见安装位的候选 bin 路径（探测/测试兜底用；PATH 查找之外的补充）。
pub fn bin_candidates(name: &str) -> Vec<PathBuf> {
    let mut prefixes: &[&str] = &["/usr/local", "/usr"];
    if cfg!(target_os = "macos") {
        prefixes = &["/opt/homebrew", "/usr/local"];
    }
    prefixes
        .iter()
        .map(|p| PathBuf::from(p).join("bin").join(name))
        .collect()
}

// —— D5 杂项 ——

/// 占位长眠进程（测试注入「运行中」句柄用；unix 用 /bin/sleep）。
pub fn sleeper_command(seconds: u64) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("/bin/sleep");
    cmd.arg(seconds.to_string());
    cmd
}
