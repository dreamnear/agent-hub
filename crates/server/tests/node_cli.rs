//! node 形态 CLI 门禁（gateway-node-p1 B1 tester FAIL-1/2 回修锚）。
//!
//! 直接 spawn 真二进制断言：`--node` 残缺/非法参数**立即非零退出**且**绝不落回
//! hub 启动路径**（无 "agent-hub server starting" 日志）。**单测绿≠接线绿**：
//! node_config 的 N6 四态单测全绿不代表 main 装配路径真实可达校验层（2026-10-10
//! 事故即此形态——误启进程占住正式口 7800），本文件就是「main 层接线」的集成
//! 测试锚，校验层接线回归在此必被捕获。
//!
//! 环境全隔离：数据目录/端口均指向 tempfile 与隔离口（17998），误落任何
//! 启动路径都不触真实数据、不撞 7800。

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

/// 负载容忍窗（sweep S3）：child 是真二进制冷启动——全量并行跑法下（38 个测试
/// 目标同机抢 CPU）exec+tracing 初始化+config 加载曾压破 10s 窗（P1 起 flake
/// 形态）；30s 只放宽负载容忍，拒绝路径的「立即退出」语义仍由 kill+panic 兜底。
const DEADLINE: Duration = Duration::from_secs(30);

/// spawn 真二进制（stdout/stderr 同写日志文件便于轮询）；返回（child, 日志路径,
/// tempdir 守卫）。
fn spawn_node_cli(extra_args: &[&str]) -> (std::process::Child, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    // pi 助手隔离（sweep S3 根因之一）：auto_start 默认 true 会经
    // spawn_host_backends 拉起**真实 pi 进程**——每轮测试白白多一个外部进程
    // （kill 后成孤儿、负载窗口拖慢冷启动）。AGENT_HUB_CONFIG 指向临时
    // config.toml（[pi] auto_start=false），顺带把其余配置段读取也隔离出真家目录。
    let cfg = dir.path().join("config.toml");
    std::fs::write(&cfg, "[pi]\nauto_start = false\n").expect("write test config.toml");
    let log = dir.path().join("cli.log");
    let f = std::fs::File::create(&log).expect("log file");
    let mut c = Command::new(env!("CARGO_BIN_EXE_server"));
    c.args(extra_args)
        .env("AGENT_HUB_CONFIG", &cfg)
        .env("AGENT_HUB_NODE_FILE", dir.path().join("node.json"))
        .env("AGENT_HUB_TASKS_DIR", dir.path().join("tasks"))
        .env("JOBS_DIR", dir.path().join("jobs"))
        .env("INSTANCES_FILE", dir.path().join("instances.json"))
        .env("NOTES_FILE", dir.path().join("notes.json"))
        .env("PROJECTS_FILE", dir.path().join("projects.json"))
        .env("AGENT_HUB_BIND", "127.0.0.1:17998") // 误落 hub 路径也不撞 7800
        .env_remove("AGENT_HUB_NODE_URL")
        .env_remove("AGENT_HUB_ALLOW_LAN")
        .stdout(f.try_clone().expect("clone log"))
        .stderr(f);
    let child = c.spawn().expect("spawn server binary");
    (child, log, dir)
}

/// 读日志全文（文件重定向，进程存活也可读）
fn read_log(log: &std::path::Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

/// 等进程退出（deadline 兜底 kill + panic——拒绝路径进程不退出 = FAIL-2 回归）
fn wait_exit_or_fail(child: &mut std::process::Child, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + DEADLINE;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{what}: 进程未在 {DEADLINE:?} 内退出（拒绝路径必须立即 exit）");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// FAIL-1 锚：`--node` 缺 --gateway（且无 env / 既有 node.json）→ 立即非零退出
/// + 引导文案 + 绝无 hub 启动日志。
#[test]
fn node_without_gateway_exits_immediately_never_hub_path() {
    let (mut child, log, _dir) = spawn_node_cli(&["--node"]);
    let status = wait_exit_or_fail(&mut child, "--node 缺 --gateway");
    let output = read_log(&log);
    assert!(
        !status.success(),
        "缺 --gateway 必须非零退出，status={status}"
    );
    assert!(
        output.contains("node 模式需要网关地址"),
        "应带网关地址引导文案: {output}"
    );
    assert!(
        !output.contains("agent-hub server starting"),
        "绝不落回 hub 启动路径: {output}"
    );
}

/// FAIL-2 锚：非法 scheme（ftp://）→ N6 拒绝文案 + 立即非零退出 + 不进节点形态。
#[test]
fn node_with_invalid_scheme_exits_immediately() {
    let (mut child, log, _dir) = spawn_node_cli(&["--node", "--gateway", "ftp://x.test"]);
    let status = wait_exit_or_fail(&mut child, "--gateway ftp:// 非法 scheme");
    let output = read_log(&log);
    assert!(
        !status.success(),
        "非法 scheme 必须非零退出，status={status}"
    );
    assert!(
        output.contains("ws:// 或 wss://"),
        "应带 scheme 引导文案: {output}"
    );
    assert!(
        !output.contains("agent-hub node starting"),
        "校验失败不得进入节点形态: {output}"
    );
    assert!(
        !output.contains("agent-hub server starting"),
        "校验失败也不得落回 hub 路径: {output}"
    );
}

/// FAIL-2 同链锚：非 loopback 明文 ws 无豁免 → 拒绝 + 立即非零退出。
#[test]
fn node_with_plaintext_cross_network_exits_immediately() {
    let (mut child, log, _dir) = spawn_node_cli(&["--node", "--gateway", "ws://example.com:7901"]);
    let status = wait_exit_or_fail(&mut child, "--gateway ws://example.com 无豁免");
    let output = read_log(&log);
    assert!(!status.success(), "明文跨网必须非零退出，status={status}");
    assert!(
        output.contains("--allow-insecure") && output.contains("wss"),
        "应带 wss/--allow-insecure 引导文案: {output}"
    );
    assert!(!output.contains("agent-hub node starting"), "{output}");
    assert!(!output.contains("agent-hub server starting"), "{output}");
}

/// 正向锚：合法参数（死网关口）→ 进入节点形态日志、不落 hub 路径、进程存活
/// （泵永续退避），随后 kill 回收。
#[test]
fn node_with_valid_gateway_starts_node_path() {
    let (mut child, log, _dir) = spawn_node_cli(&[
        "--node",
        "--gateway",
        "ws://127.0.0.1:1",
        "--node-name",
        "hub-test-node-cli",
    ]);
    let deadline = Instant::now() + DEADLINE;
    loop {
        let output = read_log(&log);
        if output.contains("agent-hub node starting") {
            assert!(
                !output.contains("agent-hub server starting"),
                "合法 --node 也不得落回 hub 路径: {output}"
            );
            if child.try_wait().expect("try_wait").is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return;
        }
        if let Some(status) = child.try_wait().expect("try_wait") {
            let output = read_log(&log);
            panic!("合法参数下节点进程提前退出 status={status}，日志: {output}");
        }
        assert!(
            Instant::now() < deadline,
            "节点形态未在期限内启动: {}",
            read_log(&log)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
