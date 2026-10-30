//! ClaudeDriver 的 MessageSender 实现：统一 send 路由到 attach PTY 通道。
//! 含就绪时序（TUI 启动静默检测）、死条目复位（会话结束自动 release 重建）与
//! Done 会话自动 respawn（2026-10-02 全驱动展示纪律：Done 不进侧栏，从历史
//! 面板点开发送即复活——respawn 链路在此内化，调用方零感知）。

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use futures::future::BoxFuture;

use super::attach::{attach_sync, AttachSession};
use super::cli;
use crate::drivers::MessageSender;

/// 首发确认层有界参数（agent-hub-first-send-ready，issue #3）。取值理由见
/// `docs/reports/agent-hub-first-send-ready/coder.md` §有界参数：
/// - `CONFIRM_MS`：实测 TUI 回显延迟 ~60ms（正常路径），2500ms 给出 ~40× 余量，
///   同时容忍 TUI 重绘/忙碌时的排队渲染，避免误判为「未消费」而重复写入。
/// - `ATTEMPTS`：3 次（含首次）→ 最坏 ~7.6s 返回明确失败。上限只覆盖「首发」
///   这一瞬时风险窗，且首发是每 attach 一次性的，故该量级对 HTTP 请求可接受；
///   更长的 CLI 初始化窗（历史事故记录 20s~2.5min）不在此处硬等——那种会话
///   按 F1/F3 走明确失败上报（禁静默吞没），由用户重发。
const FIRST_SEND_CONFIRM_MS: u64 = 2500;
const FIRST_SEND_ATTEMPTS: u32 = 3;
/// 重写前清行静默：Ctrl-U 后等 TUI 处理完再写（实测 ~瞬时完成，150ms 留余量）
const FIRST_SEND_CLEAR_SETTLE_MS: u64 = 150;
/// 确认轮询间隔（远小于回显延迟即可，50ms 保证「未消费」也能快速判失败）
const FIRST_SEND_POLL_MS: u64 = 50;
/// 回显线索长度（归一化后取前 N 字符）：TUI 增量重绘会把长文本拆成多段，
/// 整段匹配会漏；16 字符足以区分正常会话输出（实测见 experiments.md §C）
const FIRST_SEND_NEEDLE_CHARS: usize = 16;
/// Ctrl-U（kill line）：清空 TUI 输入行，重写前用——实测可可靠清行，
/// 防「上一次其实写成功了但判据没认出来」时把正文写重复（experiments.md §D）
const CLEAR_LINE: &[u8] = b"\x15";

/// agentId → 复用 attach 会话。
pub struct ClaudeSendRouter {
    bin: PathBuf,
    attaches: Mutex<HashMap<String, Arc<Mutex<AttachSession>>>>,
    /// 已在当前 attach 上完成过「被确认的首发」的 agent：首发确认层只覆盖
    /// 每条 attach 生命周期内的第一次发送（F1 的场景 = spawn 后首发），此后
    /// 发送走原路径零额外开销——稳态零回归面（斜杠命令等 chip 渲染文本不参与
    /// 回显匹配，见 coder.md §方案取舍）。release/respawn 复位后重新布防。
    first_send_confirmed: Mutex<HashSet<String>>,
}

impl ClaudeSendRouter {
    pub fn new(bin: PathBuf) -> Self {
        Self {
            bin,
            attaches: Mutex::new(HashMap::new()),
            first_send_confirmed: Mutex::new(HashSet::new()),
        }
    }

    /// 已有则复用；否则 lazy attach。返回 (会话, 是否新建)。
    pub fn get_or_attach(&self, id: &str) -> Result<(Arc<Mutex<AttachSession>>, bool)> {
        if let Some(sess) = self.attaches.lock().expect("attaches lock").get(id) {
            return Ok((sess.clone(), false));
        }
        let sess = attach_sync(&self.bin, id)?;
        let sess = Arc::new(Mutex::new(sess));
        self.attaches
            .lock()
            .expect("attaches lock")
            .insert(id.to_string(), sess.clone());
        Ok((sess, true))
    }

    /// 真死判据（与 `ClaudeDriver::list` 同源）：**CLI 非全量清单缺席** = daemon
    /// 已死，须先 respawn；`done` ≠ 退出——活 daemon 停在提示符时 state 也是
    /// done 且在清单内（2026-10-02 反例实测），直接 attach 接管即可，respawn 会
    /// 打扰用户开着的 TUI。清单失败（CLI 抖动）按「不 respawn」降级——宁可
    /// attach 走原路径报错，也不对可能活着的会话多打一次 respawn。
    async fn job_needs_respawn(&self, id: &str) -> bool {
        match cli::agents_json(&self.bin, false).await {
            Ok(list) => !list.iter().any(|c| c.id == id),
            Err(_) => false,
        }
    }
}

impl ClaudeSendRouter {
    /// TUI 斜杠命令注入（agent-hub-model-thinking）：/model 等命令走同一条
    /// attach PTY 通道（死条目复位 + TUI 渲染门 + 提交 \r），但**不**走消息
    /// 正文路径——命令由 Claude TUI 自行解释，不产生对话消息。
    pub async fn inject_command(&self, id: &str, command: &str) -> Result<()> {
        if self.is_finished(id) {
            let _ = self.release(id);
        }
        let (sess, is_new) = self.get_or_attach(id)?;
        let need_tui_check = !sess
            .lock()
            .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?
            .has_output();
        tracing::info!(agent = %id, is_new, need_tui_check, "inject_command: attach");
        if need_tui_check {
            ensure_tui_ready(&sess).await?;
        }
        // 命令 + 提交 （TUI raw mode 只认 ）；同 send 的 spawn_blocking 纪律
        let payload = format!("{command}\r");
        let sess_for_write = sess.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut s = sess_for_write
                .lock()
                .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
            s.write_stdin(&payload)
        })
        .await
        .map_err(|e| anyhow::anyhow!("join command task: {e}"))??;
        // 提交 \r 单发重试（同 send 提交原子性：渲染期 \r 可能被吞）
        let sess_for_cr = sess.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut s = sess_for_cr
                .lock()
                .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
            s.write_stdin("\r")
        })
        .await
        .map_err(|e| anyhow::anyhow!("join enter task: {e}"))??;
        tracing::info!(agent = %id, command = %command, "inject_command: delivered");
        Ok(())
    }

    /// 终端保真视图：订阅该 agent 的 PTY 输出字节流（复用同一 AttachSession，不重复 spawn）。
    pub async fn subscribe_output(
        &self,
        id: &str,
    ) -> Result<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        let (sess, _) = self.get_or_attach(id)?;
        let rx = sess
            .lock()
            .map_err(|_| anyhow::anyhow!("attach 锁失效"))?
            .subscribe_output();
        Ok(rx)
    }
}

impl MessageSender for ClaudeSendRouter {
    fn send<'a>(&'a self, id: &'a str, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            // 死条目复位（P3 加固）：attach 进程已退出 → release 重建
            if self.is_finished(id) {
                let _ = self.release(id);
            }
            // 真死会话复活（2026-10-02 修正：done ≠ 退出）：respawn 守卫与 list 的
            // 活性判据同源——CLI 非全量清单缺席 = daemon 已死才 respawn；活 done
            //（daemon 停在提示符）在清单内，直接 attach 接管，不打扰用户开着的 TUI。
            if self.job_needs_respawn(id).await {
                cli::respawn(&self.bin, id)
                    .await
                    .with_context(|| format!("respawn {id} 失败"))?;
                let _ = self.release(id);
            }
            // 注入日志（反馈轮 25-B）：指纹=首 16 字符；各阶段结果可对账 CLI 回显
            tracing::info!(
                agent = %id,
                fingerprint = %text.chars().take(16).collect::<String>(),
                bytes = text.len(),
                "send: begin"
            );
            let (sess, is_new) = self.get_or_attach(id)?;
            // TUI 可用性门（反馈轮 28-A）：is_new 必然未渲染须检测；复用条目也须检测——
            // r28 取证：终端视角建的 attach 若 TUI 从未渲染，复用路径跳过检测直接注入
            // → text written 成功但消息零落盘（黑洞）。has_output=false 一律先等渲染。
            let need_tui_check = !sess
                .lock()
                .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?
                .has_output();
            tracing::info!(agent = %id, is_new, need_tui_check, "send: attach");
            if need_tui_check {
                ensure_tui_ready(&sess).await?;
            }
            // 同步 write_stdin 包 spawn_blocking（P4 处置 P2-1）：PTY slave 缓冲满时写会
            // 阻塞，不占 tokio worker 线程
            // 提交原子性（P5 preview 反馈）：长文本（图片路径等）写入后 TUI 渲染期 \r 可能
            // 被吞——文本与 \r 分两段写，段间静默 300ms 确保 TUI 消化
            let first_send = !self
                .first_send_confirmed
                .lock()
                .map_err(|_| anyhow::anyhow!("first_send_confirmed 锁失效"))?
                .contains(id);
            let payload = text.to_string();
            let payload_len = payload.len();
            if first_send {
                // 首发投递确认层（agent-hub-first-send-ready）：写入成功 ≠ CLI 消费。
                // 未确认则清行重写，全部用尽 → 明确失败（禁静默吞没）。
                write_first_send_confirmed(&sess, &payload, id).await?;
                self.first_send_confirmed
                    .lock()
                    .map_err(|_| anyhow::anyhow!("first_send_confirmed 锁失效"))?
                    .insert(id.to_string());
            } else {
                let sess_for_write = sess.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let mut s = sess_for_write
                        .lock()
                        .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
                    s.write_stdin(&payload)
                })
                .await
                .map_err(|e| anyhow::anyhow!("join write task: {e}"))??;
            }
            tracing::info!(agent = %id, bytes = payload_len, first_send, "send: text written");
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            // 提交 \r 重发（反馈轮 8 真机：文本进输入框但 \r 渲染期被吞不提交）：
            // 共发 3 次、间隔 400ms；已提交状态下多发的 \r = 空输入回车，Claude TUI
            // 无操作（不产生空消息），故无脑重发安全；sleep 在锁外不阻塞终端写
            for attempt in 0..3 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
                let sess_for_cr = sess.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let mut s = sess_for_cr
                        .lock()
                        .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
                    // TUI raw mode 下回车是 \r（Claude TUI 只监听 \r 提交）——P2 实测锚点
                    s.write_stdin("\r")
                })
                .await
                .map_err(|e| anyhow::anyhow!("join enter task: {e}"))??;
            }
            tracing::info!(agent = %id, attempts = 3, "send: enter (\\r) sent");
            Ok(())
        })
    }

    fn is_finished(&self, id: &str) -> bool {
        self.attaches
            .lock()
            .expect("attaches lock")
            .get(id)
            .map(|s| s.lock().map(|sess| sess.is_finished()).unwrap_or(true))
            .unwrap_or(false) // 无条目视为未结束（尚未 attach）
    }

    fn release(&self, id: &str) -> Result<()> {
        if let Some(sess) = self.attaches.lock().expect("attaches lock").remove(id) {
            if let Ok(mut s) = sess.lock() {
                s.kill()?;
            }
        }
        // 首发确认层随之复位：下一次 attach 是新的 TUI 生命周期，重新布防
        // （死条目复位 / Done 会话 respawn 都经 release → 语义与 F4 一致）
        if let Ok(mut done) = self.first_send_confirmed.lock() {
            done.remove(id);
        }
        Ok(())
    }

    /// 中断当前处理：resize 抖动触发 TUI 重绘 + 静默确认后注入 Esc。
    /// tester-r2 对照：启动期/渲染停滞期直接注入会被丢弃——重绘后再注入生效。
    /// 反馈轮 19：工具执行期单发 Esc 仍与 TUI 渲染/工具处理竞态被吞（r49 实测
    /// 2/4 轮失败）——照 send 的 \r 防吞模式：共发 3 次、间隔 400ms；工作态下
    /// 多发 Esc = 重复中断请求，Claude TUI 无二次副作用，故无脑重发安全。
    fn interrupt<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (sess, is_new) = self.get_or_attach(id)?;
            if is_new {
                ensure_tui_ready(&sess).await?;
            }
            // resize 抖动：同尺寸往返产生 WINCH，逼 TUI 重绘（含卡在模态/渲染停滞的场景）
            {
                let s = sess.lock().map_err(|_| anyhow::anyhow!("attach 锁失效"))?;
                s.poke_resize()?;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            for attempt in 0..3 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
                let sess_for_esc = sess.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let mut s = sess_for_esc
                        .lock()
                        .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
                    s.write_stdin_raw(b"\x1b")
                })
                .await
                .map_err(|e| anyhow::anyhow!("join esc task: {e}"))??;
            }
            Ok(())
        })
    }
}

/// 首发投递确认层（agent-hub-first-send-ready，issue #3）：
/// 现状缺口 = `ensure_tui_ready` 的判据是「TUI 已渲染」，不是「CLI 已开始消费 stdin」——
/// 写入成功（HTTP 200 + `written` 日志）与「被消费」是两件事：实测 PTY 在 canonical
/// 态会自行回显键入字节（父进程看得到），而 CLI 转 raw 时会 flush 掉窗口期未读输入
/// （experiments.md §A）。故首发改为**以 TUI 回显为消费证据**：
/// 写入 → 轮询最近输出中是否出现该文本 → 未出现则 Ctrl-U 清行后重写（防重复提交），
/// 尝试用尽仍未确认 → `bail!` 明确失败（禁静默降级为「假装成功」，F1/F3）。
/// 只覆盖每条 attach 生命周期的首次发送；提交原子性（`\r`×3）与忙态放行零改动（F2/F4）。
async fn write_first_send_confirmed(
    sess: &Arc<Mutex<AttachSession>>,
    text: &str,
    agent: &str,
) -> Result<()> {
    let needle = echo_needle(text);
    let started = tokio::time::Instant::now();
    for attempt in 1..=FIRST_SEND_ATTEMPTS {
        if attempt > 1 {
            // 上一次写入未被消费（回显缺失）→ 清行，防「其实已进输入框」时写重复：
            // 残留正文会被紧随的 \r 提交两次（experiments.md §D：Ctrl-U 可可靠清行）
            write_raw(sess, CLEAR_LINE).await?;
            tokio::time::sleep(std::time::Duration::from_millis(FIRST_SEND_CLEAR_SETTLE_MS)).await;
        }
        let seq = sess
            .lock()
            .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?
            .output_seq();
        write_raw(sess, text.as_bytes()).await?;
        if wait_echo(sess, seq, &needle).await {
            tracing::info!(
                agent = %agent,
                attempt,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "first-send: echo confirmed (CLI consumed the injected text)"
            );
            return Ok(());
        }
        tracing::warn!(
            agent = %agent,
            attempt,
            attempts = FIRST_SEND_ATTEMPTS,
            "first-send: no echo within window (CLI not consuming?)"
        );
    }
    // 用尽仍失败：清行再报错（#57）——最后一次写入的残留正文若留在 TUI 输入行，
    // 用户重试时会与 web 侧保留的草稿拼成两段提交；TUI 侧回到空行才与
    // 「失败留草稿在 web」的语义对齐（Ctrl-U 同 attempt>1 的既有清行形态）
    write_raw(sess, CLEAR_LINE).await?;
    anyhow::bail!(
        "首发注入未被 CLI 消费：{} 次写入均在 {}ms 内无回显（会话可能仍在初始化或 TUI 未接管 stdin），请稍后重试",
        FIRST_SEND_ATTEMPTS,
        FIRST_SEND_CONFIRM_MS
    )
}

/// 单次写入（spawn_blocking 纪律同 send：PTY 写可能阻塞，不占 tokio worker）
async fn write_raw(sess: &Arc<Mutex<AttachSession>>, data: &[u8]) -> Result<()> {
    let sess_for_write = sess.clone();
    let payload = data.to_vec();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut s = sess_for_write
            .lock()
            .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
        s.write_stdin_raw(&payload)
    })
    .await
    .map_err(|e| anyhow::anyhow!("join write task: {e}"))?
}

/// 在 `seq` 之后的输出里等 TUI 回显（消费证据）。锁内只取快照，轮询 sleep 在锁外。
async fn wait_echo(sess: &Arc<Mutex<AttachSession>>, seq: u64, needle: &[u8]) -> bool {
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(FIRST_SEND_CONFIRM_MS);
    let mut baseline = seq;
    loop {
        let (raw, cur_seq, tail) = {
            let s = match sess.lock() {
                Ok(s) => s,
                Err(_) => return false,
            };
            (s.raw_mode(), s.output_seq(), s.output_since(baseline))
        };
        match raw {
            // canonical（或已退回 canonical）态：行规程回显不是应用消费证据，
            // 且此时写入可能被 CLI 转 raw 时 flush —— 基线前移丢弃这段，继续等
            Some(false) => baseline = cur_seq,
            _ => {
                // 与线索同口径归一化后比对（TUI 重绘在字符间插转义/换行）
                if contains_needle(&normalize_output(&tail), needle) {
                    return true;
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(FIRST_SEND_POLL_MS)).await;
    }
}

/// 回显线索：归一化正文的前 `FIRST_SEND_NEEDLE_CHARS` 个字符。
/// 纯函数（单测锚点）——长文本在 TUI 里会换行/被增量重绘拆段，整段匹配会漏。
fn echo_needle(text: &str) -> Vec<u8> {
    let mut n = normalize_output(text.as_bytes());
    n.truncate(FIRST_SEND_NEEDLE_CHARS);
    n
}

/// 归一化 PTY 输出：剥 ANSI/OSC 转义 → 去空白与控制字节 → ASCII 小写。
/// TUI（Ink）按格重绘，会在字符之间插入光标定位转义、并对已渲染部分做增量 diff，
/// 逐字节相等匹配会漏（实测 experiments.md §C）。纯函数（单测锚点）。
fn normalize_output(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            i += 1;
            match bytes.get(i) {
                // CSI：ESC [ 参数中间字节… 终止字母（0x40-0x7E）
                Some(b'[') => {
                    i += 1;
                    while let Some(&c) = bytes.get(i) {
                        i += 1;
                        if (0x40..=0x7e).contains(&c) {
                            break;
                        }
                    }
                }
                // OSC：ESC ] … BEL 或 ST(ESC \)——必须识别终止符，否则会吞掉正文
                Some(b']') => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                // 其余双字节转义（ESC ( B / ESC = / ESC > 等）
                Some(_) => i += 1,
                None => {}
            }
            continue;
        }
        i += 1;
        // 去空白与控制字节（回显里空格/换行/光标控制都不稳定）
        if b <= 0x20 || b == 0x7f {
            continue;
        }
        out.push(b.to_ascii_lowercase());
    }
    out
}

/// 归一化后子串匹配（空线索一律判否：正文全是空白时不得据此认作已消费）
fn contains_needle(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// TUI 就绪检测（P3 引入；tester-r2 时序实验依据）：启动输出出现后静默 800ms 视为渲染完成。
/// 锁内只取快照，轮询 sleep 在锁外（MutexGuard 不跨 await）。
/// 反馈轮 24-C：检测超时但 TUI 存活（出现过输出）= busy 态（agent working，输出持续
/// 流动永不静默）——放行注入：pty 实测 Claude CLI 支持工作中排队，敲入文本+\r 后
/// 当前轮结束自动处理；\r×3 防吞已覆盖渲染期竞争。从未有输出（TUI 未起）仍失败。
async fn ensure_tui_ready(sess: &Arc<Mutex<AttachSession>>) -> Result<()> {
    const QUIET_MS: u64 = 800;
    let started = tokio::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(10);
    loop {
        let (has_output, last) = {
            let s = sess
                .lock()
                .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
            (s.has_output(), s.output_snapshot_ms())
        };
        // 反馈轮 28-A：以 has_output（drain 真读到过字节）替代 last>0——
        // last 初值是 attach 时刻，「从未渲染」曾被误判为「渲染后静默」直接放行
        if has_output && super::attach::now_ms().saturating_sub(last) >= QUIET_MS {
            tracing::info!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "tui: quiet, ready"
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            if has_output {
                tracing::info!("tui: busy timeout (agent working), inject anyway → CLI queue");
                return Ok(());
            }
            tracing::warn!("tui: no output at all, attach failed");
            anyhow::bail!("attach TUI 就绪检测超时");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{contains_needle, echo_needle, normalize_output};

    /// 归一化：剥 CSI/OSC 转义、去空白与控制字节、ASCII 小写。
    /// 实测形态（experiments.md §C）：TUI 在字符之间插光标定位转义。
    #[test]
    fn normalize_strips_escapes_whitespace_and_case() {
        // CSI（含带 ? 的私有模式）+ OSC（BEL 终止）都应被剥掉，且不吞正文
        let raw = b"\x1b[?25lHELLO\x1b[39m \x1b]0;title\x07World\r\n";
        assert_eq!(normalize_output(raw), b"helloworld".to_vec());

        // 字符间插入转义（增量重绘）：归一化后正文应重新连成一片
        let interleaved = b"ab\x1b[1;3Hc\x1b[0md";
        assert_eq!(normalize_output(interleaved), b"abcd".to_vec());

        // OSC 用 ST（ESC \）终止——不识别终止符会把标题正文吞进匹配串
        let st_terminated = b"\x1b]1337;iTerm\x1b\\tail";
        assert_eq!(normalize_output(st_terminated), b"tail".to_vec());
    }

    /// 线索 = 归一化正文前 16 字符：长文本在 TUI 里换行/分块，整段匹配会漏。
    #[test]
    fn needle_is_truncated_normalized_prefix() {
        assert_eq!(echo_needle("Hello World"), b"helloworld".to_vec());
        let long = echo_needle(&"x".repeat(40));
        assert_eq!(long.len(), 16);
        // CJK 与标点：非空白字节原样保留（不上 ASCII 小写，避免破坏多字节编码）
        assert_eq!(echo_needle("中文 探针").len(), "中文探针".len());
    }

    /// 匹配语义：命中/未命中、空线索一律判否（禁止「正文全空白时误判已消费」）。
    #[test]
    fn contains_needle_requires_non_empty_match() {
        assert!(contains_needle(b"xxhelloworldyy", b"helloworld"));
        assert!(!contains_needle(b"xxhello-worldyy", b"helloworld"));
        assert!(!contains_needle(b"hi", b"helloworld"));
        assert!(!contains_needle(b"any", b""));
    }

    /// 端到端纯函数链：真实回显形态（Box 边框 + 正文 + 光标移动）应判为已消费。
    #[test]
    fn echo_pipeline_recognizes_tui_render() {
        let text = "把线上的问题先修掉";
        let needle = echo_needle(text);
        // 模拟 TUI 渲染：边框、正文字符间夹光标定位转义、尾部状态栏
        let render = format!(
            "\x1b[2K│ ❯ {}\x1b[10;1H─── status ───",
            "把线上的问题先修\x1b[1;3H掉"
        );
        let tail = normalize_output(render.as_bytes());
        assert!(
            contains_needle(&tail, &needle),
            "回显渲染应被识别为消费证据: needle={needle:?} tail={tail:?}"
        );
    }
}
