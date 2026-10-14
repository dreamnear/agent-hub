use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use server::drivers::claude::ClaudeDriver;
use server::drivers::events::ChatEvent;
use server::models::Group;
use std::sync::RwLock;
use tokio::sync::broadcast;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

/// 测试驱动构造（list() 只消费 bin/jobs_dir；sender/rooms 为 trait 面字段，空表即可；
/// ignored_interactive_file 指向 tempfile 根下的唯一名——list 会读它做忽略过滤，
/// 读失败回退空集，用例互不污染）。
fn driver(bin: PathBuf, jobs_dir: PathBuf) -> ClaudeDriver {
    ClaudeDriver::new(
        bin,
        jobs_dir,
        PathBuf::new(),
        std::env::temp_dir().join(format!("driver-list-test-ignored-{}", std::process::id())),
        Arc::new(RwLock::new(
            HashMap::<String, broadcast::Sender<ChatEvent>>::new(),
        )),
    )
}

#[tokio::test]
async fn merges_jobs_detail_and_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("a1b2c3d4");
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        r#"{"detail":"from jobs","tokens":999,"name":"jobs-name"}"#,
    )
    .unwrap();

    let driver = driver(fake_bin(), dir.path().to_path_buf());
    // 默认清单（all=false）：daemon 已死的 Completed 不进侧栏（2026-10-02 展示
    // 纪律）；done ≠ 退出——活 done 会话映射 Other 照常显示（误判修复回归锚点）
    let list = driver.list(false).await.unwrap();
    assert_eq!(
        list.len(),
        4,
        "活跃清单 4 条（exited 不列，done+活 daemon 在列）"
    );
    assert!(list.iter().all(|a| a.group != Group::Completed));
    let done_alive = list.iter().find(|a| a.id == "f0a1b2c3").unwrap();
    assert_eq!(
        done_alive.group,
        Group::Other,
        "done+活 daemon → Idle（活会话被误藏的反例回归）"
    );
    assert_eq!(done_alive.raw_state.as_deref(), Some("done"));

    let a = list.iter().find(|a| a.id == "a1b2c3d4").unwrap();
    assert_eq!(a.driver, "claude");
    assert_eq!(a.group, Group::NeedsInput);
    assert_eq!(a.detail.as_deref(), Some("from jobs"));
    assert_eq!(a.tokens, Some(999));
    assert_eq!(a.name.as_deref(), Some("refactor-auth"), "CLI 优先于 jobs");

    // all=true（历史面板 opt-in）：保留全量；--all 独有的 exited 条目才是 Completed
    let all = driver.list(true).await.unwrap();
    assert_eq!(all.len(), 5);
    let dead = all.iter().find(|a| a.id == "c9d0e1f2").unwrap();
    assert_eq!(
        dead.group,
        Group::Completed,
        "--all 独有（daemon 已死）→ 历史口径"
    );
}

#[tokio::test]
async fn degrades_silently_on_bad_jobs_file() {
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("a1b2c3d4");
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(job.join("state.json"), "<<<half-written>>>").unwrap();

    let driver = driver(fake_bin(), dir.path().to_path_buf());
    let list = driver.list(false).await.unwrap();
    let a = list.iter().find(|a| a.id == "a1b2c3d4").unwrap();
    assert_eq!(a.detail, None, "坏 jobs 文件静默降级");
    assert_eq!(a.group, Group::NeedsInput, "分组仍来自 CLI raw_state");
}

/// driver-unify 2.3：中断标记内化后 list 覆盖判据防回归（r69/R9 锚定）——
/// CLI state 滞留 working/blocked（NeedsInput 属覆盖集）时被覆盖为 Other；
/// 解除后（标记不存在）恢复权威 group。覆盖逻辑逐行平移自 AppState，本测试
/// 冻结「覆盖生效」与「无标记不覆盖」两个分支。
#[tokio::test]
async fn interrupt_mark_overrides_group_and_relieves_by_state_change() {
    let dir = tempfile::tempdir().unwrap();
    let d = driver(fake_bin(), dir.path().to_path_buf());

    // a1b2c3d4 = blocked（NeedsInput，在覆盖集内）；打标后应显示 Other
    d.interrupted_at
        .lock()
        .unwrap()
        .insert("a1b2c3d4".into(), std::time::SystemTime::now());
    let list = d.list(false).await.unwrap();
    let a = list.iter().find(|a| a.id == "a1b2c3d4").unwrap();
    assert_eq!(a.group, Group::Other, "中断标记应覆盖 NeedsInput → Other");

    // 解除路径 1：state 真实离开工作态（c9d0e1f2 = exited，即不在覆盖集）→
    // 打标无意义，list 不覆盖且标记被清除（解除判据第一分支）。
    // list(true)：exited 条目默认清单已过滤（Done 不进侧栏），解除判据在
    // 全量口径上验证——过滤发生在 apply_interrupt_overrides 之后不冲突。
    d.interrupted_at
        .lock()
        .unwrap()
        .insert("c9d0e1f2".into(), std::time::SystemTime::now());
    let list = d.list(true).await.unwrap();
    let c = list.iter().find(|a| a.id == "c9d0e1f2").unwrap();
    assert_eq!(c.group, Group::Completed, "非工作态不覆盖");
    assert!(
        !d.interrupted_at.lock().unwrap().contains_key("c9d0e1f2"),
        "state 已离开工作态 → 标记应解除"
    );

    // 无标记（新驱动实例）→ 权威 group 直通（blocked 且无会话文件的标记只能靠
    // mtime 前进解除，故用干净实例验证「不打标即不覆盖」）
    let d2 = driver(fake_bin(), dir.path().to_path_buf());
    let list = d2.list(false).await.unwrap();
    let a = list.iter().find(|a| a.id == "a1b2c3d4").unwrap();
    assert_eq!(a.group, Group::NeedsInput);
}

/// gateway-first-run R1 锚：CLI 非零退出（首跑注册表未初始化形态）→ list 降级
/// 空清单 Ok（此前传播为 500）。all=true 同口径。写路径（spawn/send）不受影响。
#[tokio::test]
async fn list_degrades_to_empty_when_cli_fails() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("failing-claude.sh");
    std::fs::write(
        &bin,
        "#!/bin/sh\necho 'cat: agents-active.json: No such file or directory' >&2\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let jobs = dir.path().join("jobs"); // 不存在（首跑形态）
    let d = driver(bin, jobs);
    let list = d.list(false).await.unwrap();
    assert!(list.is_empty(), "R1：CLI 失败应降级空清单而非 500");
    let all = d.list(true).await.unwrap();
    assert!(all.is_empty(), "R1：--all 同口径降级");
}
