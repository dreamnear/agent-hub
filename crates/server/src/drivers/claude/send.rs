//! ClaudeDriver 的 MessageSender 实现：统一 send 路由到 attach PTY 通道。
//! 含就绪时序（TUI 启动静默检测）与死条目复位（会话结束自动 release 重建）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures::future::BoxFuture;

use super::attach::{attach_sync, AttachSession};
use crate::drivers::MessageSender;

/// agentId → 复用 attach 会话。
pub struct ClaudeSendRouter {
    bin: PathBuf,
    attaches: Mutex<HashMap<String, Arc<Mutex<AttachSession>>>>,
}

impl ClaudeSendRouter {
    pub fn new(bin: PathBuf) -> Self {
        Self {
            bin,
            attaches: Mutex::new(HashMap::new()),
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
}

impl ClaudeSendRouter {
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
            // 被吞——文本与提交 \r 分两段写，段间静默 300ms 确保 TUI 消化
            let sess_for_write = sess.clone();
            let payload = text.to_string();
            let payload_len = payload.len();
            tokio::task::spawn_blocking(move || -> Result<()> {
                let mut s = sess_for_write
                    .lock()
                    .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
                s.write_stdin(&payload)
            })
            .await
            .map_err(|e| anyhow::anyhow!("join write task: {e}"))??;
            tracing::info!(agent = %id, bytes = payload_len, "send: text written");
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

    fn send_raw_bytes(&self, id: &str, data: &[u8]) -> Result<()> {
        let sess = self.get_or_attach(id)?.0;
        let mut s = sess
            .lock()
            .map_err(|_| anyhow::anyhow!("attach 会话锁失效"))?;
        s.write_stdin_raw(data)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
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
