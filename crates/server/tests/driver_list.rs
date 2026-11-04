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
    // 默认清单（all=false）#60 completed-group 新契约：daemon 已死条目在列且落
    // Completed 桶（对齐 CLI 2.1.314 全量库存）；done ≠ 退出——活 done 会话
    // 映射 Other 照常显示（误判修复回归锚点，勿动）
    let list = driver.list(false).await.unwrap();
    assert_eq!(
        list.len(),
        5,
        "默认面全量 5 条（daemon 已死的 exited 在列）"
    );
    let dead = list.iter().find(|a| a.id == "c9d0e1f2").unwrap();
    assert_eq!(
        dead.group,
        Group::Completed,
        "daemon 已死 → Completed 桶（默认面不再过滤）"
    );
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

    // all=true（历史面板）：全量口径不变；#60 起与缺省面内容一致（daemon 已死
    // 条目两面同落 Completed）
    let all = driver.list(true).await.unwrap();
    assert_eq!(all.len(), 5);
    let dead = all.iter().find(|a| a.id == "c9d0e1f2").unwrap();
    assert_eq!(dead.group, Group::Completed, "历史口径同样落 Completed 桶");
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
    // #60 起 exited 条目随默认面在列（Completed 桶），解除判据直接在缺省口径验证。
    d.interrupted_at
        .lock()
        .unwrap()
        .insert("c9d0e1f2".into(), std::time::SystemTime::now());
    let list = d.list(false).await.unwrap();
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

/// #60 回归锚（tester 轮次 1 FAIL 根因）：hub-stop 死亡会话可经 respawn 复活
/// ——daemon 注册表仍存该会话 flags（--mcp-config=配置文件），stop 不得回收该
/// 文件（否则复活 init 即崩 Invalid MCP configuration，send 恒 400）。回收点在
/// remove（注册表条目消失，文件再无引用者）；映射同样保留至 remove（remove 按
/// 它定位 hub-<uuid>.json 实际文件名）。
#[tokio::test]
async fn stop_keeps_mcp_config_for_respawn_remove_reclaims() {
    use server::drivers::AgentCatalog;

    let dir = tempfile::tempdir().unwrap();
    let mcp_dir = dir.path().join("plugin_mcp");
    std::fs::create_dir_all(&mcp_dir).unwrap();
    let cfg = mcp_dir.join("hub-test0000.json");
    std::fs::write(&cfg, "{}").unwrap();
    let d = ClaudeDriver::new_with_mcp(
        fake_bin(),
        dir.path().join("jobs"),
        PathBuf::new(),
        std::env::temp_dir().join(format!("driver-list-test-ignored-{}", std::process::id())),
        Arc::new(RwLock::new(
            HashMap::<String, broadcast::Sender<ChatEvent>>::new(),
        )),
        mcp_dir,
        PathBuf::from("server"),
        "127.0.0.1:0".into(),
    );
    d.mcp_config_paths
        .lock()
        .unwrap()
        .insert("a1b2c3d4".into(), cfg.clone());

    d.stop("a1b2c3d4").await.unwrap();
    assert!(
        cfg.exists(),
        "stop 不回收 MCP 配置（respawn flags 仍引用该文件）"
    );
    assert!(
        d.mcp_config_paths.lock().unwrap().contains_key("a1b2c3d4"),
        "映射保留至 remove（uuid 文件名的唯一清理线索）"
    );

    d.remove("a1b2c3d4").await.unwrap();
    assert!(!cfg.exists(), "remove 才是回收点（文件无引用者后清理）");
    assert!(!d.mcp_config_paths.lock().unwrap().contains_key("a1b2c3d4"));
}
