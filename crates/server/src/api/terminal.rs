//! 纯工具终端 WS（agent-hub-terminal T5）：`GET /ws/terminal?cwd=<dir>&rows=&cols=`
//! ——起于任意已认证调用方指定目录的交互 shell（前端传选中会话 cwd），独立 PTY。
//! 与会话管理/消息注入链路零耦合（T2 纯工具边界：不 attach agent、不订阅会话流）。
//! 门禁：/ws 全局 auth_middleware 已覆盖（?token= 照旧），本模块零新增绕过。
//! 生命周期（T3）：WS 断开或 shell 退出即进程组 SIGKILL + wait 回收，无僵尸。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Deserialize;

use crate::api::SharedState;
use crate::platform;

#[derive(Deserialize)]
pub struct TerminalQuery {
    cwd: String,
    rows: Option<u16>,
    cols: Option<u16>,
}

/// cwd 校验：存在且为目录。不做 docs 式白名单——终端一旦打开即可 cd 任意目录，
/// 白名单只造成假边界；真正的边界是 /ws 全局 token 门禁（已认证 = 本机管理员语义）。
pub fn validate_cwd(cwd: &str) -> bool {
    Path::new(cwd).is_dir()
}

/// 独立 PTY 会话：shell 跑在伪终端里，master 保活、writer 透传按键、
/// child 可整组回收（kill_and_reap 防 zombie，习语同 drivers/claude/attach.rs）。
pub struct TermPty {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

/// spawn shell 于 PTY（shell 参数化便于测试注入 /bin/sh，免交互 rc 启动慢）。
pub fn spawn_shell(shell: &Path, cwd: &Path, rows: u16, cols: u16) -> Result<TermPty> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(shell);
    // 交互登录 shell：rc/alias 与用户本地终端一致（纯工具定位）；无 shell 拼接
    //（登录态参数平台差异归驱动层：unix `-l` / windows 免 banner 参数）
    platform::add_login_flag(&mut cmd);
    cmd.cwd(cwd);
    // TERM 标注 256 色全能力（xterm.js 前端按 256/truecolor 解析）
    cmd.env("TERM", "xterm-256color");
    let child = pair.slave.spawn_command(cmd)?;
    let writer = pair.master.take_writer()?;
    Ok(TermPty {
        master: pair.master,
        writer,
        child,
    })
}

impl TermPty {
    /// 原样字节写（终端键盘透传，不做 \r 转换）。
    pub fn write_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(PtySize {
            rows: rows.max(2),
            cols: cols.max(2),
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// 派生树 SIGKILL + 进程组 SIGKILL + wait 回收（无孤儿/zombie）。
    /// 派生树维度兜底 job control：交互 shell 的 `cmd &` 后台任务 pgid 自立、组杀
    /// 漏网（走查 FAIL-1 实证 PPID=1 存活）——ppid 树自 shell 下行圈定全部后裔，
    /// 不依赖 tty/ctty（spawn 期 ctty 附着有竞态，tty 方案轮 2 已证不可靠）。
    /// 顺序敏感：必须趁 shell 存活先收集——先杀 shell 则孤儿被 reparent（ppid=1），
    /// 树断裂漏杀。
    pub fn kill_and_reap(&mut self) {
        // ① 先收集派生树（shell 活着、ppid 链完整）
        let tree_pids: Vec<u32> = self
            .child
            .process_id()
            .map(platform::collect_descendants)
            .unwrap_or_default();
        // ② shell 进程组整杀（unix PTY spawn 下子进程是会话首进程，pgid == pid；
        // 杀组即整组，组杀实现归驱动层）
        if let Some(pid) = self.child.process_id() {
            platform::kill_group(pid, platform::Signal::Kill);
        }
        // ③ 派生树全量 SIGKILL（含 job control 自立进程组的后台任务）
        for tpid in tree_pids {
            platform::kill_pid(tpid, platform::Signal::Kill);
        }
        let _ = self.child.kill();
        // ④ wait 回收 zombie——WNOHANG 轮询上限 2s，不裸调阻塞 wait：shell 态异常
        // （如 job-control 时序）时阻塞 wait4 会挂死 pump 任务
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        tracing::warn!("kill_and_reap：shell 2s 未退出，放弃 wait（不再阻塞）");
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            }
        }
    }
}

pub async fn terminal(
    ws: WebSocketUpgrade,
    Query(q): Query<TerminalQuery>,
    State(_state): State<SharedState>,
) -> impl IntoResponse {
    if !validate_cwd(&q.cwd) {
        return (StatusCode::BAD_REQUEST, "cwd not a dir").into_response();
    }
    let rows = q.rows.unwrap_or(24).max(2);
    let cols = q.cols.unwrap_or(80).max(2);
    let cwd = PathBuf::from(&q.cwd);
    ws.on_upgrade(move |socket| async move { pump(socket, cwd, rows, cols).await })
}

/// WS ⇄ PTY 双向泵：上行 Binary = 原始按键字节、Text = 控制帧（resize）；下行全 Binary。
/// 泵退出（WS 断 / shell 退）必经 kill_and_reap（T3 无僵尸）。
async fn pump(mut socket: WebSocket, cwd: PathBuf, rows: u16, cols: u16) {
    let shell = platform::default_shell();
    let mut pty = match spawn_shell(&shell, &cwd, rows, cols) {
        Ok(p) => p,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("spawn failed: {e}").into()))
                .await;
            return;
        }
    };
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    // drain：阻塞读 PTY 输出（不读会撑满 slave 缓冲区卡死 shell），EOF = shell 退出
    let mut reader = match pty.master.try_clone_reader() {
        Ok(r) => r,
        Err(_) => {
            // 回收含 WNOHANG 轮询（异常分支最长 2s）——spawn_blocking 隔离，
            // 不阻塞 tokio worker（reviewer MINOR）
            let _ = tokio::task::spawn_blocking(move || pty.kill_and_reap()).await;
            return;
        }
    };
    let drain = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.send(buf[..n].to_vec()).is_err() {
                        break; // 泵已退出（WS 断），停止搬运
                    }
                }
            }
        }
    });

    loop {
        tokio::select! {
            out = out_rx.recv() => {
                match out {
                    Some(bytes) => {
                        if socket.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break, // drain 结束 = shell 退出
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Binary(data))) => {
                        if pty.write_raw(&data).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        // 控制帧：{"type":"resize","rows":r,"cols":c}（其余忽略）
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if v["type"] == "resize" {
                                let _ = pty.resize(
                                    v["rows"].as_u64().unwrap_or(24) as u16,
                                    v["cols"].as_u64().unwrap_or(80) as u16,
                                );
                            }
                        }
                    }
                    _ => break, // None = WS 关闭 / 非 Binary 非 Text 帧一律终止
                }
            }
        }
    }
    // 回收含 WNOHANG 轮询（异常分支最长 2s）——spawn_blocking 隔离，
    // 不阻塞 tokio worker（reviewer MINOR；pty 整体移入阻塞线程，回收语义不变）
    let _ = tokio::task::spawn_blocking(move || pty.kill_and_reap()).await;
    drain.abort(); // drain 多半已 EOF 自退；abort 兜底防泄漏
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_cwd_checks_dir_existence() {
        let tmp = std::env::temp_dir().join(format!("hub-term-cwd-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(validate_cwd(tmp.to_str().unwrap()));
        assert!(!validate_cwd("/nonexistent/hub-term-dir"));
        let file = tmp.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        assert!(!validate_cwd(file.to_str().unwrap())); // 文件不算目录
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn reap_covers_job_control_background() {
        // 走查 FAIL-1 回归：job control（set -m）下后台任务自成进程组、组杀漏网——
        // 派生树兜底必须把本终端后裔清零（关页签后无任何存活进程）
        let tmp = std::env::temp_dir().join(format!("hub-term-bg-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut pty =
            spawn_shell(Path::new("/bin/sh"), &tmp, 24, 80).expect("spawn /bin/sh in pty");
        pty.write_raw(b"set -m\nsleep 300 &\necho BGPID=$!\n")
            .unwrap();
        // 后台任务 pid 由 shell 自报（echo $!）——pgrep -f 子串匹配会撞无关进程
        // （其他会话长命令串含 "sleep 300" 子串，实测误报）；读侧走线程 + 有界
        // channel（直接在测试线程阻塞 read 有 set -m tcsetpgrp 竞态挂死风险）
        let mut reader = pty.master.try_clone_reader().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<Result<i32, String>>();
        let acc_shared = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let acc_handle = acc_shared.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Err("reader EOF".into()));
                        break;
                    }
                    Ok(n) => {
                        let mut acc = acc_handle.lock().unwrap();
                        acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                        // rsplit 取最后一次出现后的段：输入回显里 "echo BGPID=$!" 会
                        // 先于真值出现（nth(1) 永远卡在回显段 → 永不匹配）
                        let digits: String = acc
                            .rsplit("BGPID=")
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take_while(|c| c.is_ascii_digit())
                            .collect();
                        if let Ok(pid) = digits.parse::<i32>() {
                            let _ = tx.send(Ok(pid));
                            return;
                        }
                    }
                }
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let bg_pid = loop {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(Ok(pid)) => break pid,
                Ok(Err(tail)) => panic!("reader 提前退出: {tail:?}"),
                Err(mpsc_err) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "5s 内未收到 shell 自报的后台 pid（BGPID=）: {mpsc_err:?}，实收={:?}",
                        acc_shared.lock().map(|g| g.clone()).unwrap_or_default()
                    );
                }
            }
        };
        pty.kill_and_reap();
        // 孤儿化后按 pid 复查：后台任务必须死亡（tty 清零断言不可靠——session 撤销
        // 清 ctty 后 ps -t 列不到，但进程还活着，正是本轮实证的杀空路径）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            // 僵尸态（Z）视为已死：sh 先亡时孤儿由 launchd 收割，收割前 ps 仍列出 Z 条目
            let out = std::process::Command::new("ps")
                .args(["-p", &bg_pid.to_string(), "-o", "stat="])
                .output()
                .unwrap();
            let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
            // 已死判据：无条目（进程消失）/ Z（僵尸，launchd 待收割）/ ? 开头（ps 对
            // 消失中进程的占位输出）——三者皆非才算存活
            if stat.is_empty() || stat.starts_with('?') || stat.contains('Z') {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "后台任务 {} 回收后仍存活（stat={stat}，job control 孤儿）",
                bg_pid
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn spawn_echo_reap_roundtrip() {
        // 真实 PTY 往返：/bin/sh 里 echo 标记 → 读回 → kill_and_reap 无僵尸
        let tmp = std::env::temp_dir().join(format!("hub-term-run-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let mut pty =
            spawn_shell(Path::new("/bin/sh"), &tmp, 24, 80).expect("spawn /bin/sh in pty");
        // 标记 echo（前台 shell 就绪后执行；读侧带超时轮询容忍启动延迟）
        pty.write_raw(b"echo hub-term-marker-$((1+1))\n").unwrap();

        let mut reader = pty.master.try_clone_reader().unwrap();
        let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let c2 = collected.clone();
        let drain = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                c2.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let found = loop {
            if std::time::Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            let snapshot = collected.lock().unwrap().clone();
            if String::from_utf8_lossy(&snapshot).contains("hub-term-marker-2") {
                break true;
            }
        };
        assert!(found, "PTY 输出未含回显标记");
        // resize 不炸（SIGWINCH 送达）
        pty.resize(30, 100).unwrap();
        pty.kill_and_reap();
        let _ = drain.join();
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
