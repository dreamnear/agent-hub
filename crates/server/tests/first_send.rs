//! 首发投递确认层（agent-hub-first-send-ready，issue dreamnear/agent-hub#3）集成测试。
//!
//! fake CLI（`fixtures/first_send_fake.py`，模式随 id 后缀）复刻实测吞没机制：
//! 窗口期（canonical 不读 stdin）写入 → 行规程回显可见但无人消费 → CLI 转 raw
//! （TCSAFLUSH）时窗口期输入被内核丢弃。确认层据此验证三件事：
//! 1. 正常路径零重试（回显 ~瞬时）——稳态无行为变化（F4）；
//! 2. 窗口期吞没 → 界内清行重写恢复，正文恰好一份（不重复提交）；
//! 3. 持续黑洞 → 尝试用尽，明确失败（禁静默吞没，F1/F3）。
//!
//! 三场景共享 env（FS_FAKE_DIR/FS_FAKE_IDS）与 fake 进程生命周期，合一个测试顺序
//! 执行（并行会互踩，同 api_messages.rs 惯例）。fake 的落盘按 chunk 追加、与 hub
//! 侧写入存在微秒级时差，断言前一律轮询等待（禁一次性读后即判）。

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use server::drivers::claude::send::ClaudeSendRouter;
use server::drivers::MessageSender;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/first_send_fake.py")
}

fn log_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("fs-fake-{id}.log"))
}

fn read_log(dir: &Path, id: &str) -> String {
    std::fs::read_to_string(log_path(dir, id)).unwrap_or_default()
}

fn count(occurrences: &str, needle: &str) -> usize {
    occurrences.matches(needle).count()
}

/// 轮询 fake 日志直到 needle 出现 ≥ min_count 次（或超时返回当前内容供断言报错）
fn wait_log(dir: &Path, id: &str, needle: &str, min_count: usize, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let log = read_log(dir, id);
        if count(&log, needle) >= min_count || Instant::now() >= deadline {
            return log;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[tokio::test]
async fn first_send_confirmation_paths() {
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("FS_FAKE_DIR", dir.path());
    std::env::set_var("FS_FAKE_IDS", "fs-ready,fs-window5,fs-blackhole");
    let router = ClaudeSendRouter::new(fake_bin());

    // ---- 场景 1（正常路径）：立即回显 → 一次写入即确认，零重试（F4 稳态）----
    let t = Instant::now();
    router
        .send("fs-ready", "hello first send")
        .await
        .expect("正常路径应确认成功");
    let ready_elapsed = t.elapsed();
    let log = wait_log(dir.path(), "fs-ready", r"\r", 3, Duration::from_secs(3));
    assert_eq!(
        count(&log, "hello first send"),
        1,
        "正文应恰好写入一次（无重试），log={log:?}"
    );
    assert!(
        count(&log, r"\r") >= 3,
        "提交 \\r×3 原子性语义应保持，log={log:?}"
    );
    assert!(
        ready_elapsed < Duration::from_secs(3),
        "零重试路径不应显著变慢（ensure_tui_ready ~0.9s + 300ms + \\r×3 800ms），got {ready_elapsed:?}"
    );
    router.release("fs-ready").unwrap();

    // ---- 场景 2（窗口期吞没 → 恢复）：attempt 1 在 canonical 期写入、被转 raw
    //      时 flush；确认层判「未消费」→ 清行重写 → 恰好一份正文（F1 核心场景）----
    let t = Instant::now();
    router
        .send("fs-window5", "windowed message")
        .await
        .expect("窗口期吞没后应在界内重试恢复");
    let window_elapsed = t.elapsed();
    let log = wait_log(
        dir.path(),
        "fs-window5",
        "windowed message",
        1,
        Duration::from_secs(5),
    );
    assert_eq!(
        count(&log, "windowed message"),
        1,
        "恢复后正文应恰好一份（清行重写不产生重复），log={log:?}"
    );
    assert!(
        count(&log, "\\x15") >= 1,
        "重写前应有清行动作（Ctrl-U），log={log:?}"
    );
    assert!(
        window_elapsed >= Duration::from_millis(2500),
        "经历了未确认轮次才算真正测到吞没路径，got {window_elapsed:?}"
    );

    // ---- 场景 2b（确认后稳态）：同 attach 的第二次发送走原路径，零确认开销----
    let t = Instant::now();
    router
        .send("fs-window5", "steady state message")
        .await
        .expect("确认后的常规发送不受影响");
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "已确认 attach 的后续发送不应再等待回显窗，got {:?}",
        t.elapsed()
    );
    wait_log(
        dir.path(),
        "fs-window5",
        "steady state message",
        1,
        Duration::from_secs(3),
    );
    assert_eq!(
        count(&read_log(dir.path(), "fs-window5"), "steady state message"),
        1
    );
    router.release("fs-window5").unwrap();

    // ---- 场景 3（持续黑洞 → 明确失败）：零回显证据 → 用尽 3 次尝试后报错，
    //      禁静默降级为「假装成功」（F1/F3）----
    let t = Instant::now();
    let err = router
        .send("fs-blackhole", "blackholed message")
        .await
        .expect_err("黑洞会话必须走明确失败路径");
    let hole_elapsed = t.elapsed();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("首发注入未被 CLI 消费"),
        "失败信息应可辨识（用户可据之重试），got: {msg}"
    );
    assert!(
        hole_elapsed >= Duration::from_millis(6000),
        "用尽 3×2500ms 确认窗才算有界重试生效，got {hole_elapsed:?}"
    );
    assert!(
        hole_elapsed < Duration::from_secs(20),
        "重试必须有界（禁无限挂起），got {hole_elapsed:?}"
    );
    let log = wait_log(
        dir.path(),
        "fs-blackhole",
        "blackholed message",
        3,
        Duration::from_secs(3),
    );
    assert_eq!(
        count(&log, "blackholed message"),
        3,
        "应恰好重写到尝试上限（3 次写入），不多不少"
    );
    router.release("fs-blackhole").unwrap();
}
