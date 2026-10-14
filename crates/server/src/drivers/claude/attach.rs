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

/// 最近 PTY 输出的有界环形缓冲（agent-hub-first-send-ready 首发确认层）：
/// 「写入的文本是否被 CLI 消费」在 hub 侧唯一可得的证据是它出现在 TUI 渲染流里，
/// 故 drain 需保留最近一段输出供增量检索。`start` = 缓冲首字节对应的累计序号
/// （缓冲满后前移），`seq()` 返回累计 drain 字节数，调用方按 seq 取增量。
/// 上限 64KB 防内存膨胀（单 session 常驻，量级远小于一屏渲染的多轮刷新）。
struct OutputRing {
    start: u64,
    data: std::collections::VecDeque<u8>,
}

const OUTPUT_RING_CAP: usize = 64 * 1024;

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
    /// 最近输出环形缓冲（首发确认层）：锁内只做字节搬移，不含 await
    output_ring: Arc<Mutex<OutputRing>>,
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
    let ring = Arc::new(Mutex::new(OutputRing {
        start: 0,
        data: std::collections::VecDeque::new(),
    }));
    let ring_for_drain = ring.clone();
    let drain = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    last_for_drain.store(now_ms(), std::sync::atomic::Ordering::Relaxed);
                    saw_for_drain.store(true, std::sync::atomic::Ordering::Relaxed);
                    // 环形缓冲追加（首发确认层）：毒化锁不 panic——drain 一挂整条
                    // PTY 输出就没人读，slave 缓冲满会卡死 TUI，比丢一段缓冲严重得多
                    {
                        let mut r = ring_for_drain.lock().unwrap_or_else(|e| e.into_inner());
                        r.data.extend(&buf[..n]);
                        while r.data.len() > OUTPUT_RING_CAP {
                            r.data.pop_front();
                            r.start += 1;
                        }
                    }
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
        output_ring: ring,
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

    /// 累计 drain 字节数（首发确认层的增量读取基准）。缓冲前移不影响该值单调性。
    pub fn output_seq(&self) -> u64 {
        let r = self.output_ring.lock().unwrap_or_else(|e| e.into_inner());
        r.start + r.data.len() as u64
    }

    /// `seq` 之后新到的输出字节（增量）；缓冲已前移掉的部分取不到，返回仍保留的尾部。
    pub fn output_since(&self, seq: u64) -> Vec<u8> {
        let r = self.output_ring.lock().unwrap_or_else(|e| e.into_inner());
        let from = seq.max(r.start);
        let skip = (from - r.start) as usize;
        r.data.iter().skip(skip).copied().collect()
    }

    /// 终端是否已被 CLI 接管为 raw 模式（ICANON/ECHO 均已清）。
    /// 首发确认层用：canonical 态下行规程会自行回显键入字节（`Some(false)`）——
    /// 那不是「应用消费了输入」的证据，且此时写入可能在 CLI 转 raw 时被 flush
    /// （实测见 reports/agent-hub-first-send-ready/experiments.md §A）。
    /// `None` = 本平台查询不可用（非 unix / ioctl 失败）：调用方按「未知」保守处理。
    pub fn raw_mode(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            let fd = self.master.as_raw_fd()?;
            let mut t: libc::termios = unsafe { std::mem::zeroed() };
            if unsafe { libc::tcgetattr(fd, &mut t) } != 0 {
                return None;
            }
            Some(t.c_lflag & (libc::ICANON | libc::ECHO) == 0)
        }
        #[cfg(not(unix))]
        {
            None
        }
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
    /// 组杀实现归驱动层（platform::kill_group）。
    pub fn kill(&mut self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("attach child lock poisoned"))?;
        // unix PTY spawn 下子进程是会话首进程，pgid == pid，杀组即整组
        if let Some(pid) = child.process_id() {
            crate::platform::kill_group(pid, crate::platform::Signal::Kill);
        }
        child.kill()?;
        // wait 回收 zombie；SIGKILL 后必然快速返回
        let _ = child.wait();
        Ok(())
    }
}

/// 析构兜底：未经 `release()`/`kill()` 的 drop（异常路径 / server 关停）也要回收
/// attach 子进程——否则子进程活着时 drain 任务会永远阻塞在 master read 上，
/// 连带挂死 tokio blocking pool（首发单测曾以 8 分钟挂起实证此路径）。
impl Drop for AttachSession {
    fn drop(&mut self) {
        if !self.is_finished() {
            let _ = self.kill();
        }
    }
}
