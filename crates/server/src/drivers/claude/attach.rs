//! `claude attach <id>` PTY 桥：spawn 于伪终端，write_stdin 透传用户输入，
//! kill 进程组回收（不留孤儿）。id 白名单校验防 argv 注入。
//! 实测（2026-09-16）：`claude attach <id>` 存在，语义「Open the background session in this terminal」。

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

use tokio::task::JoinHandle;

/// id 白名单：字母数字、`-`、`_`；禁空/路径分量/shell 元字符。
pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("agent id 为空");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("agent id 含非法字符: {id}");
    }
    Ok(())
}

pub struct AttachSession {
    /// PTY master，保活防止 slave 写端 EIO；resize 抖动可触发 TUI 重绘（WINCH）
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    /// drain 最近一次读到输出的时刻（unix ms）——TUI 就绪检测依据（P3 加固）
    last_output_ms: Arc<std::sync::atomic::AtomicU64>,
    /// drain 是否读到过任何输出（反馈轮 28-A）：last_output_ms 初值是 attach 时刻
    /// 而非 0，「从未渲染」与「渲染后静默」无法靠 last 区分；此标志补上该判别
    saw_output: Arc<std::sync::atomic::AtomicBool>,
    /// PTY 输出字节广播（P4 终端保真视图订阅；无订阅者时 send 错误即丢弃）
    output_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    /// 后台 drain PTY 输出的任务（不读会撑满 slave 缓冲区卡死 TUI）
    _drain: JoinHandle<()>,
}

/// spawn `claude attach <id>` 于 PTY。签名按 tasks.md 定 async（spawn 实为非阻塞，直通）。
pub async fn attach(bin: &Path, id: &str) -> Result<AttachSession> {
    attach_sync(bin, id)
}

/// 同步核心（send.rs 的 get_or_attach 在同步锁上下文调用，不经 async 包装）。
pub fn attach_sync(bin: &Path, id: &str) -> Result<AttachSession> {
    validate_id(id)?;
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new(bin);
    cmd.arg("attach");
    cmd.arg(id);
    // 参数数组传递（cmd.arg），无 shell 拼接
    let child = pair.slave.spawn_command(cmd)?;

    // drain：持续读掉 PTY 输出防缓冲区满；EOF/错误即静默退出；每次读到更新 last_output
    // 并广播给终端保真视图订阅者（P4：字节流 → WS → xterm.js）
    let mut reader = pair.master.try_clone_reader()?;
    let last_output_ms = Arc::new(std::sync::atomic::AtomicU64::new(now_ms()));
    let saw_output = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let last_for_drain = last_output_ms.clone();
    let saw_for_drain = saw_output.clone();
    let (output_tx, _) = tokio::sync::broadcast::channel::<Vec<u8>>(256);
    let output_for_drain = output_tx.clone();
    let drain = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    last_for_drain.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
                    saw_for_drain.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = output_for_drain.send(buf[..n].to_vec());
                }
            }
        }
    });

    let writer = pair.master.take_writer()?;
    Ok(AttachSession {
        master: pair.master,
        writer,
        child: Mutex::new(child),
        last_output_ms,
        saw_output,
        output_tx,
        _drain: drain,
    })
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl AttachSession {
    /// 写入 stdin 透传 agent。
    pub fn write_stdin(&mut self, input: &str) -> Result<()> {
        self.write_stdin_raw(input.as_bytes())
    }

    /// 原样字节写（终端键盘透传）。
    pub fn write_stdin_raw(&mut self, data: &[u8]) -> Result<()> {
        self.writer.write_all(data)?;
        self.writer.flush()?;
        Ok(())
    }

    /// drain 最近输出时刻快照（调用方不持锁轮询，避免 guard 跨 await 破坏 Send）。
    pub fn output_snapshot_ms(&self) -> u64 {
        self.last_output_ms
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// TUI 是否渲染过（反馈轮 28-A）：注入前的可用性判据——从未输出 = TUI 未起/挂死，
    /// 此时 write_stdin 进黑洞（r28 取证：text written 成功但消息零落盘零回显）。
    pub fn has_output(&self) -> bool {
        self.saw_output.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 终端保真视图订阅 PTY 输出字节流（P4）。
    pub fn subscribe_output(&self) -> tokio::sync::broadcast::Receiver<Vec<u8>> {
        self.output_tx.subscribe()
    }

    /// resize 抖动（P4 r3/C2：同尺寸往返触发 SIGWINCH → TUI 重绘，中断注入前用）
    pub fn poke_resize(&self) -> Result<()> {
        let (rows, cols) = (24u16, 80u16);
        self.master.resize(PtySize {
            rows: rows + 1,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    /// 子进程是否已退出（try_wait 非阻塞；错误按已退出处理触发复位）。
    pub fn is_finished(&self) -> bool {
        self.child
            .lock()
            .map(|mut c| c.try_wait().map(|o| o.is_some()).unwrap_or(true))
            .unwrap_or(true)
    }

    /// 进程组 SIGKILL（shell 脚本的子进程同组一并回收），随后 wait 防 zombie。
    pub fn kill(&mut self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("attach child lock poisoned"))?;
        // unix PTY spawn 用 setsid：子进程是 session leader，pgid == pid，杀 -pid 即整组
        if let Some(pid) = child.process_id() {
            // SAFETY: libc::kill 对进程组发 SIGKILL，pid 来自子进程自身，无悬垂风险
            let rc = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            if rc != 0 {
                // 组已退出（如子进程先行结束）不算失败
                tracing::debug!(pid, "进程组 SIGKILL 返回非零（可能已退出）");
            }
        }
        child.kill()?;
        // wait 回收 zombie；SIGKILL 后必然快速返回
        let _ = child.wait();
        Ok(())
    }
}
