//! 平台驱动层 · windows 实现。
//! 一期以「编译通过 + 结构正确」为准：选型按 Windows 惯例等价映射（进程组 →
//! CREATE_NEW_PROCESS_GROUP、组杀 → taskkill /T /F、锁 → LockFileEx〔fslock
//! 双平台覆盖〕、默认 shell → COMSPEC/powershell），**行为正确性未经验证**，
//! 二期真机验证欠账清单见 docs/reports/agent-hub-platform-driver/platform-matrix.md。

use std::path::{Path, PathBuf};

use portable_pty::CommandBuilder;

use super::Signal;

/// PATH 搜索分隔符（windows 惯例）。
pub const PATH_SEP: char = ';';

/// 子进程入新进程组（等价 unix process_group(0) 的最小映射；组语义弱于 unix，
/// 见登记表）。
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// 脱离控制台拉起（detach 语义：hub 死亡不带走子进程）。
const DETACHED_PROCESS: u32 = 0x0000_0008;

// —— D1 进程树管理 ——

/// 子进程入独立进程组。
pub fn new_process_group(cmd: &mut tokio::process::Command) {
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

/// 脱离当前控制台/进程组拉起（detach 语义）。
pub fn detach_from_session(cmd: &mut tokio::process::Command) {
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// taskkill 封装：force = /F（强制终止），tree = /T（连坐整棵派生树）。
/// 忽略退出码——目标可能已退出（与 unix 分支 errno 忽略同语义）。
fn taskkill(pid: u32, force: bool, tree: bool) {
    let mut cmd = std::process::Command::new("taskkill");
    if force {
        cmd.arg("/F");
    }
    if tree {
        cmd.arg("/T");
    }
    cmd.arg("/PID").arg(pid.to_string());
    let _ = cmd.output();
}

/// 杀整棵派生树（windows 无进程组语义，taskkill /T 是树杀等价物；
/// Kill 强制 / Term 优雅请求——控制台程序对非强制请求普遍不响应，见登记表）。
pub fn kill_group(pid: u32, sig: Signal) {
    taskkill(pid, sig == Signal::Kill, true);
}

/// 杀单个进程。
pub fn kill_pid(pid: u32, sig: Signal) {
    taskkill(pid, sig == Signal::Kill, false);
}

/// 派生树收集：windows 不需要——kill_group 的 taskkill /T 自带整树连坐，
/// 返回空集（调用方语义 = 「无额外后裔需单杀」，树杀由组杀承担）。
pub fn collect_descendants(_root: u32) -> Vec<u32> {
    Vec::new()
}

// —— D2 终端与 shell ——

/// 默认交互 shell：COMSPEC 优先，缺省 powershell.exe。
pub fn default_shell() -> PathBuf {
    std::env::var_os("COMSPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("powershell.exe"))
}

/// PTY 交互 shell 参数：powershell 免 banner（登录态概念不适用，见登记表）。
pub fn add_login_flag(cmd: &mut CommandBuilder) {
    cmd.arg("-NoLogo");
}

/// PATH 上找可执行（裸名）：PATHEXT 常用集（exe/bat/cmd）+ 无扩展名兜底。
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let exts = ["exe", "bat", "cmd"];
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|d| {
        exts.iter()
            .map(|e| d.join(format!("{name}.{e}")))
            .chain(std::iter::once(d.join(name)))
            .find(|c| c.is_file())
    })
}

/// 命令串是否含路径分隔符（含 = 按路径解析，不再做 PATH 搜索）。
pub fn has_path_separator(cmd: &str) -> bool {
    cmd.contains('/') || cmd.contains('\\')
}

// —— D4 环境与路径 ——

/// 用户 home 目录（USERPROFILE 优先，HOME 兜底；未设置返回 None）。
pub fn home_dir_opt() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// 可执行判定：文件存在即可（windows 无执行位语义；扩展名可执行性由
/// CreateProcess 侧解析，见登记表）。
pub fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// 设权限位：windows 无 unix 权限位，0600 等硬化为 ACL 收紧——一期空实现
/// （行为欠账记登记表），签名保持跨平台一致。
pub fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// 同上，async 版。
pub async fn set_mode_async(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// 读权限位：windows 无对应语义，恒 None（调用方 unix 侧断言用）。
pub fn file_mode(_path: &Path) -> Option<u32> {
    None
}

/// 敏感文件原子写的 OpenOptions：windows 一期无权限位等价物（ACL 收紧记登记表）。
pub fn private_write_options() -> tokio::fs::OpenOptions {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    opts
}

/// 搜索路径富化：windows 一期原样返回（PATH 由系统托管；用户级安装位的富化
/// 留二期真机按实际需要补）。
pub fn extended_search_path(inherited: &str, _home: &str) -> String {
    inherited
        .split(PATH_SEP)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect::<Vec<_>>()
        .join(";")
}

/// 常见安装位候选：windows 一期不提供（PATH 查找已覆盖常规安装）。
pub fn bin_candidates(_name: &str) -> Vec<PathBuf> {
    Vec::new()
}

// —— D5 杂项 ——

/// 占位长眠进程（测试注入「运行中」句柄用；windows 用 cmd 内建 timeout）。
pub fn sleeper_command(seconds: u64) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("cmd");
    cmd.args(["/c", "timeout", "/t", &seconds.to_string(), "/nobreak"]);
    cmd
}
