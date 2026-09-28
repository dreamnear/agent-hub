//! 远程轻量服务端探测 + 安装计划生成/执行（agent-hub-settings 任务 D2/D3）。
//!
//! 探测复用隧道同款 SSH 通道（`ssh_cmd`），远端跑只读命令（uname + curl /health +
//! /api/instances 存在性），一往返拿全 OS/架构/安装态。安装计划只生成命令清单，
//! install-plan 与 install-manual（direct）共用同一处定义，防两处漂移。
//! D3 执行：confirm + planHash/实例绑定校验通过才经同一 SSH 通道逐条执行，
//! 成功后回读远端 `~/.claude-view/token` 落进实例配置（不回读实例无法转可用）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::instances::InstanceConfig;
use crate::ssh_cmd;

/// 远端一键安装脚本（开源仓 main 分支；脚本自带 OS/架构探测、装二进制、
/// `--with-systemd` 注册并启动 systemd 单元——不自造远端安装逻辑）
pub const INSTALL_SH_URL: &str =
    "https://raw.githubusercontent.com/dreamnear/agent-hub/main/deploy/install.sh";
/// 二进制分发前缀（install.sh 的 RELEASE_BASE_URL）；资产名 claude-view-server-{os}-{arch}
pub const RELEASE_BASE_URL: &str =
    "https://github.com/dreamnear/agent-hub/releases/latest/download";
/// 计划清单未指定端口时的兜底（server 默认 bind 127.0.0.1:7800）
pub const DEFAULT_REMOTE_PORT: u16 = 7800;
/// 探测总超时（远端 curl 自带 --max-time 5；这里兜 SSH 握手卡死）
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// 安装脚本/分发源地址（env 覆盖：自建分发镜像时指向自有源；默认开源仓官方地址）。
/// 计划展示的命令原文即实际执行的 URL（所见即所执），覆盖后前端确认面板同步可见。
pub fn install_sh_url() -> String {
    std::env::var("AGENT_HUB_INSTALL_SH_URL").unwrap_or_else(|_| INSTALL_SH_URL.into())
}

pub fn release_base_url() -> String {
    std::env::var("AGENT_HUB_RELEASE_BASE_URL").unwrap_or_else(|_| RELEASE_BASE_URL.into())
}

/// 探测哨兵行：脚本首行输出；缺失 = SSH 层根本没跑起来（不可达/认证失败）
const MARKER: &str = "hub-probe/1";

/// 探测判定原因（四类可区分）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeReason {
    /// 已安装且可访问
    Installed,
    /// 远程端口无响应（未安装或服务未启动）
    NotInstalled,
    /// 端口有 HTTP 响应但不是 agent-hub（/api/instances 不存在）
    NotAgentHub,
    /// SSH 不可达 / 认证失败 / 远端命令未执行
    SshUnreachable,
}

/// 一次探测结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteProbe {
    pub installed: bool,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub reason: ProbeReason,
    /// 人读原因（前端可直接透出）
    pub detail: String,
}

/// 计划中的一步。`display` 即将执行的命令原文（确认面板逐条展示的就是它，
/// D3 执行的也是它——所见即所执）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStep {
    pub desc: String,
    pub display: String,
}

/// 安装计划（D3 执行时按 planId/planHash 校验"确认的=执行的"）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    pub plan_id: String,
    pub plan_hash: String,
    pub steps: Vec<PlanStep>,
}

/// 远端只读探测脚本：uname + health + agent-hub 标记路径，输出 key=value 行。
/// 经 ssh argv 传给远端 shell 执行，本机无 shell 参与（无注入面；端口是 u16）。
fn probe_script(remote_port: u16) -> String {
    format!(
        "echo '{MARKER}'; printf 'os='; uname -s; printf 'arch='; uname -m; \
         curl -fsS --max-time 5 http://127.0.0.1:{p}/health >/dev/null 2>&1 \
           && echo health=ok || echo health=down; \
         curl -fsS --max-time 5 http://127.0.0.1:{p}/api/instances >/dev/null 2>&1 \
           && echo hub=ok || echo hub=miss",
        p = remote_port
    )
}

/// 远端探测输出 → 判定（纯函数，单测锚点）。首行哨兵缺失 = SSH 层失败。
pub fn judge_probe(stdout: &str) -> RemoteProbe {
    let mut os = None;
    let mut arch = None;
    let mut health = false;
    let mut hub = false;
    let mut marker = false;
    for line in stdout.lines() {
        let line = line.trim();
        if line == MARKER {
            marker = true;
        } else if let Some(v) = line.strip_prefix("os=") {
            os = non_empty(v);
        } else if let Some(v) = line.strip_prefix("arch=") {
            arch = non_empty(v);
        } else if line == "health=ok" {
            health = true;
        } else if line == "hub=ok" {
            hub = true;
        }
    }
    if !marker {
        return RemoteProbe {
            installed: false,
            os,
            arch,
            reason: ProbeReason::SshUnreachable,
            detail: "SSH 连接失败或远端命令未执行（输出无探测哨兵）".into(),
        };
    }
    let (reason, detail) = if health && hub {
        (
            ProbeReason::Installed,
            "远程 127.0.0.1 上已安装并运行 agent-hub server".to_string(),
        )
    } else if health {
        (
            ProbeReason::NotAgentHub,
            "远程端口有 HTTP 响应但不是 agent-hub（/api/instances 不存在）".to_string(),
        )
    } else {
        (
            ProbeReason::NotInstalled,
            "远程端口无响应，agent-hub server 未安装或未启动".to_string(),
        )
    };
    RemoteProbe {
        installed: reason == ProbeReason::Installed,
        os,
        arch,
        reason,
        detail,
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn unreachable_ssh(detail: String) -> RemoteProbe {
    RemoteProbe {
        installed: false,
        os: None,
        arch: None,
        reason: ProbeReason::SshUnreachable,
        detail,
    }
}

/// 执行一次远程探测（只读命令；总超时兜底，超时/失败都归 ssh_unreachable）。
pub async fn probe(inst: &InstanceConfig, remote_port: u16) -> RemoteProbe {
    let mut cmd = match ssh_cmd::command(inst, &[], Some(&probe_script(remote_port))) {
        Ok(c) => c,
        Err(e) => return unreachable_ssh(e.to_string()),
    };
    cmd.kill_on_drop(true); // 超时放弃时不留孤儿 ssh
    let out = match tokio::time::timeout(PROBE_TIMEOUT, cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return unreachable_ssh(format!("ssh 进程执行失败: {e}")),
        Err(_) => return unreachable_ssh("SSH 探测超时".into()),
    };
    if !out.status.success() {
        // ssh 自身错误（连接拒绝/认证失败）走 stderr；截断防日志灌水
        let stderr = String::from_utf8_lossy(&out.stderr);
        let head: String = stderr.trim().chars().take(200).collect();
        return unreachable_ssh(if head.is_empty() {
            format!("ssh 退出码 {:?}", out.status.code())
        } else {
            head
        });
    }
    judge_probe(&String::from_utf8_lossy(&out.stdout))
}

/// 命令清单（唯一事实源：install-plan 与 install-manual 共用）。
/// 架构探测与 systemd 安装由 `deploy/install.sh` 自带，这里不感知远端 OS/架构。
/// 注意必须 `bash -s`：install.sh 是 bash 脚本（set -o pipefail / BASH_SOURCE），
/// 管道给 `sh` 在 Debian/Ubuntu（dash）下会死于 `set: Illegal option -o pipefail`
/// （E2E 实测 2026-09-28）。
pub fn plan_steps(remote_port: u16) -> Vec<PlanStep> {
    vec![
        PlanStep {
            desc: "下载并执行一键安装脚本（脚本自动探测 OS/架构，装二进制并注册启动 \
                   systemd 服务；/usr/local/bin 不可写且无免密 sudo 时自动回退 \
                   ~/.local/bin）"
                .into(),
            display: format!(
                "curl -fsSL {sh} | env RELEASE_BASE_URL={base} \
                 bash -s -- --with-systemd",
                sh = install_sh_url(),
                base = release_base_url(),
            ),
        },
        PlanStep {
            desc: "验证服务已启动（本机回环打 /health）".into(),
            display: format!("curl -fsS --max-time 5 http://127.0.0.1:{remote_port}/health"),
        },
    ]
}

/// 生成安装计划。planHash = FNV-1a64（实例 id + 各步命令原文）——不是密码学承诺，
/// 只用于 D3 校验"用户确认的清单与服务端将执行的清单一致"。
/// ponytail: FNV 手写免新依赖；若 planHash 升级为防篡改承诺，换 sha2 并入密。
pub fn build_plan(instance_id: &str, remote_port: Option<u16>) -> InstallPlan {
    let port = remote_port.unwrap_or(DEFAULT_REMOTE_PORT);
    let steps = plan_steps(port);
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in instance_id
        .bytes()
        .chain(steps.iter().flat_map(|s| s.display.bytes()))
    {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    InstallPlan {
        plan_id: uuid::Uuid::new_v4().to_string(),
        plan_hash: format!("{h:016x}"),
        steps,
    }
}

// ===== D3：安装执行（confirm + plan 校验）+ token 回读 =====

/// 单条远端命令执行总超时（安装可能耗时数分钟；两条命令各 300s 兜底）
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(300);

/// token 回读命令（server 默认 token 落 `~/.claude-view/token`，config.rs 同源）
const TOKEN_CMD: &str = "cat ~/.claude-view/token";

/// 一次远端命令执行结果。测试经 `execute` 的 runner 参数注入 mock，不出网。
pub struct ExecOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// 已缓存的计划（执行凭据：绑定实例 + 记录确认过的命令原文）
#[derive(Clone)]
struct CachedPlan {
    instance_id: String,
    plan_hash: String,
    steps: Vec<PlanStep>,
}

/// 进程内计划缓存：planId → 计划。同实例旧计划生成即失效（确认面板永远是最新清单）。
/// ponytail: 静态 Mutex 免改 AppState 构造链；计划短生命周期，量级个位数。
fn plan_cache() -> &'static Mutex<HashMap<String, CachedPlan>> {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, CachedPlan>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 生成并缓存计划（install-plan 端点调用；**只生成不执行**）
pub fn build_and_cache_plan(instance_id: &str, remote_port: Option<u16>) -> InstallPlan {
    let plan = build_plan(instance_id, remote_port);
    let mut cache = plan_cache().lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|_, c| c.instance_id != instance_id); // 同实例旧计划作废
    cache.insert(
        plan.plan_id.clone(),
        CachedPlan {
            instance_id: instance_id.to_string(),
            plan_hash: plan.plan_hash.clone(),
            steps: plan.steps.clone(),
        },
    );
    plan
}

/// 安装执行请求体（confirm 非 true 一律拒绝——防静默执行红线）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub plan_id: String,
    pub plan_hash: String,
    pub confirm: bool,
}

/// 执行拒绝原因（400，无任何命令产生）
#[derive(Debug, PartialEq, Eq)]
pub enum InstallRejection {
    /// 前端未传「用户已确认」标志
    NotConfirmed,
    /// planId 不在缓存（未生成过 / 已被新计划作废 / 成功后已消费）
    PlanNotFound,
    /// planHash 或实例绑定与缓存计划不一致（确认的清单 ≠ 将执行的清单）
    PlanMismatch,
}

/// 执行结果（HTTP 200；失败也是 200——远端状态不用 5xx 表达，与 probe 同口径）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    pub ok: bool,
    /// 逐步执行日志（步骤说明 + 命令 + 输出尾部），确认面板所见即所执
    pub logs: Vec<String>,
    /// 失败时的可读分类错误（sudo 非交互 / 下载失败 / 脚本不存在）
    pub error: Option<String>,
    /// 远端 token 是否已回读并落进实例配置
    pub token_stored: bool,
}

/// SSH 执行通道 seam：生产传 `real_run`，测试传 mock（不发子进程）。
pub type RunFuture<'a> =
    std::pin::Pin<Box<dyn Future<Output = anyhow::Result<ExecOutput>> + Send + 'a>>;

/// 生产 runner：经共享 ssh_cmd 跑一条远端命令，超时 300s，输出截尾防灌水。
/// 返回的 future 只借用 inst（远端命令字符串已 owned）。
pub fn real_run<'a>(inst: &'a InstanceConfig, remote_cmd: &str) -> RunFuture<'a> {
    let owned = remote_cmd.to_string();
    Box::pin(async move {
        let mut cmd = ssh_cmd::command(inst, &[], Some(&owned))?;
        cmd.kill_on_drop(true); // 超时放弃时不留孤儿 ssh
        let out = tokio::time::timeout(EXEC_TIMEOUT, cmd.output()).await;
        match out {
            Err(_) => Ok(ExecOutput {
                success: false,
                stdout: String::new(),
                stderr: format!("远端命令超时（>{:?}）：{owned}", EXEC_TIMEOUT),
            }),
            Ok(Err(e)) => Err(anyhow::anyhow!("ssh 进程执行失败: {e}")),
            Ok(Ok(o)) => Ok(ExecOutput {
                success: o.status.success(),
                stdout: tail_2k(&String::from_utf8_lossy(&o.stdout)),
                stderr: tail_2k(&String::from_utf8_lossy(&o.stderr)),
            }),
        }
    })
}

fn tail_2k(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= 2000 {
        s.to_string()
    } else {
        chars[chars.len() - 2000..].iter().collect()
    }
}

/// 失败分类（可读错误，tasks.md 风险项：sudo 非交互失败必须单列，否则用户看到天书）
fn classify_install_error(stdout: &str, stderr: &str) -> String {
    let hay = format!("{stdout}\n{stderr}");
    if hay.contains("sudo") {
        "远端 sudo 非交互失败：当前 SSH 用户无免密 sudo。可为该用户配免密 sudo，\
         或重跑安装（脚本在 /usr/local/bin 不可写且无免密 sudo 时自动回退 ~/.local/bin）"
            .into()
    } else if hay.contains("curl:")
        || hay.contains("Could not resolve")
        || hay.contains("Connection refused")
    {
        "下载失败：远端访问 GitHub Release 不可达（内网/受限网络机器需先打通外网）".into()
    } else if hay.contains("not found") || hay.contains("No such file") {
        "脚本或文件不存在（install.sh 地址失效或远端环境缺依赖）".into()
    } else {
        "安装脚本执行失败（详见执行日志）".into()
    }
}

/// 执行安装计划：逐条跑确认过的命令 → 成功后回读远端 token 落实例配置。
/// 成功即消费计划（幂等：同 planId 二次执行拒绝，防重复安装误触）；失败保留计划
/// 供同清单重试。凭据只经 SSHPASS 环境变量传递，日志只含命令原文与输出尾部。
pub async fn execute<F>(
    inst: &InstanceConfig,
    req: &InstallRequest,
    store: &crate::instances::InstancesStore,
    run: F,
) -> Result<InstallOutcome, InstallRejection>
where
    F: for<'a> Fn(&'a InstanceConfig, &'a str) -> RunFuture<'a>,
{
    // 红线 1：确认标志——不确认不执行任何命令（校验先于一切副作用）
    if !req.confirm {
        return Err(InstallRejection::NotConfirmed);
    }
    // 红线 2：确认的清单 = 将执行的清单（planId 定位 + planHash/实例绑定校验）
    let cached = {
        let cache = plan_cache().lock().unwrap_or_else(|e| e.into_inner());
        cache.get(&req.plan_id).cloned()
    };
    let Some(plan) = cached else {
        return Err(InstallRejection::PlanNotFound);
    };
    if plan.instance_id != inst.id || plan.plan_hash != req.plan_hash {
        return Err(InstallRejection::PlanMismatch);
    }

    let mut outcome = InstallOutcome {
        ok: true,
        logs: Vec::new(),
        error: None,
        token_stored: false,
    };
    for step in &plan.steps {
        let res = run(inst, &step.display).await;
        match res {
            Ok(out) if out.success => {
                outcome
                    .logs
                    .push(format!("[ok] {}\n$ {}", step.desc, step.display));
                if !out.stdout.trim().is_empty() {
                    outcome.logs.push(out.stdout.trim_end().to_string());
                }
            }
            Ok(out) => {
                let class = classify_install_error(&out.stdout, &out.stderr);
                outcome.logs.push(format!(
                    "[fail] {}\n$ {}\nstdout: {}\nstderr: {}",
                    step.desc,
                    step.display,
                    out.stdout.trim_end(),
                    out.stderr.trim_end()
                ));
                outcome.ok = false;
                outcome.error = Some(class);
                return Ok(outcome); // 失败保留计划，前端可原清单重试
            }
            Err(e) => {
                outcome
                    .logs
                    .push(format!("[fail] {}\n$ {}", step.desc, step.display));
                outcome.ok = false;
                outcome.error = Some(e.to_string());
                return Ok(outcome);
            }
        }
    }

    // token 回读（没有这一步实例无法转可用）：经同一 SSH 通道读远端 token 落配置
    match run(inst, TOKEN_CMD).await {
        Ok(out) if out.success => {
            let token = out.stdout.trim().to_string();
            if token.is_empty() {
                outcome.ok = false;
                outcome.error = Some(
                    "安装完成但 token 回读为空：远端 ~/.claude-view/token 不存在或为空，\
                     请确认 systemd 服务已启动"
                        .into(),
                );
            } else {
                let mut fresh = inst.clone();
                fresh.token = Some(token);
                match store.upsert(&fresh).await {
                    Ok(()) => outcome.token_stored = true,
                    Err(e) => {
                        outcome.ok = false;
                        outcome.error = Some(format!("token 落实例配置失败: {e}"));
                    }
                }
            }
        }
        Ok(out) => {
            outcome.ok = false;
            outcome.error = Some(format!(
                "token 回读失败（{}）",
                out.stderr.trim().chars().take(200).collect::<String>()
            ));
        }
        Err(e) => {
            outcome.ok = false;
            outcome.error = Some(format!("token 回读失败: {e}"));
        }
    }

    // 成功即消费计划（幂等）；失败保留供重试
    if outcome.ok {
        plan_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&req.plan_id);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已装：哨兵 + os/arch + health ok + hub ok
    #[test]
    fn judge_installed() {
        let p = judge_probe("hub-probe/1\nos=Linux\narch=aarch64\nhealth=ok\nhub=ok\n");
        assert!(p.installed);
        assert_eq!(p.reason, ProbeReason::Installed);
        assert_eq!(p.os.as_deref(), Some("Linux"));
        assert_eq!(p.arch.as_deref(), Some("aarch64"));
    }

    /// 未装：SSH 通但端口不通（连接拒绝）
    #[test]
    fn judge_not_installed() {
        let p = judge_probe("hub-probe/1\nos=Darwin\narch=arm64\nhealth=down\nhub=miss\n");
        assert!(!p.installed);
        assert_eq!(p.reason, ProbeReason::NotInstalled);
        assert_eq!(p.os.as_deref(), Some("Darwin"));
    }

    /// 非 agent-hub：health 200 但 /api/instances 不存在
    #[test]
    fn judge_not_agent_hub() {
        let p = judge_probe("hub-probe/1\nos=Linux\narch=x86_64\nhealth=ok\nhub=miss\n");
        assert!(!p.installed);
        assert_eq!(p.reason, ProbeReason::NotAgentHub);
    }

    /// SSH 不可达：无哨兵输出
    #[test]
    fn judge_ssh_unreachable() {
        let p = judge_probe("ssh: connect to host 1.2.3.4 port 22: Connection refused\n");
        assert!(!p.installed);
        assert_eq!(p.reason, ProbeReason::SshUnreachable);
        let p2 = judge_probe("");
        assert_eq!(p2.reason, ProbeReason::SshUnreachable);
    }

    /// 探测脚本：只读命令 + 端口插值（u16 无注入面）
    #[test]
    fn probe_script_is_readonly_and_contains_port() {
        let s = probe_script(7801);
        assert!(s.contains(MARKER));
        assert!(s.contains("http://127.0.0.1:7801/health"));
        assert!(s.contains("uname -s"));
        assert!(!s.contains("install"));
    }

    /// 计划：确定性哈希 + 步骤含命令原文；不同实例/端口哈希不同
    #[test]
    fn plan_hash_stable_and_content_bound() {
        let a = build_plan("i1", Some(7800));
        let b = build_plan("i1", Some(7800));
        assert_eq!(a.plan_hash, b.plan_hash, "同实例同端口哈希应稳定");
        assert_ne!(a.plan_id, b.plan_id, "planId 每次新生成");
        assert_eq!(a.steps, b.steps);

        assert_ne!(build_plan("i2", Some(7800)).plan_hash, a.plan_hash);
        assert_ne!(build_plan("i1", Some(7801)).plan_hash, a.plan_hash);

        // 命令清单形态：install.sh URL + RELEASE_BASE_URL + --with-systemd
        assert_eq!(a.steps.len(), 2);
        assert!(a.steps[0].display.contains(INSTALL_SH_URL));
        assert!(a.steps[0].display.contains(RELEASE_BASE_URL));
        assert!(a.steps[0].display.ends_with("--with-systemd"));
        assert!(a.steps[1].display.contains("http://127.0.0.1:7800/health"));

        // 未指定端口 → 默认 7800
        assert!(build_plan("i1", None).steps[1]
            .display
            .contains(&format!(":{DEFAULT_REMOTE_PORT}/health")));
    }

    // ===== D3 执行链（mock runner，不发子进程）=====

    fn ssh_inst(id: &str) -> InstanceConfig {
        // 每个 D3 测试用独立实例 id：共享静态计划缓存里同实例旧计划会被新计划作废
        InstanceConfig {
            id: id.into(),
            name: "remote".into(),
            mode: crate::instances::InstanceMode::SshTunnel,
            url: None,
            token: None,
            ssh: Some(crate::instances::SshConfig {
                host: "example.com".into(),
                port: 22,
                user: "u".into(),
                auth: crate::instances::SshAuth::Authsock,
                key_path: None,
                password: None,
            }),
            remote_port: Some(7800),
            local_port: None,
        }
    }

    fn exec_req(plan: &InstallPlan, confirm: bool) -> InstallRequest {
        InstallRequest {
            plan_id: plan.plan_id.clone(),
            plan_hash: plan.plan_hash.clone(),
            confirm,
        }
    }

    /// mock runner：安装命令成功；token 命令回读固定 token；第 N 次失败可编程
    fn mock_ok_run<'a>(_i: &'a InstanceConfig, cmd: &'a str) -> RunFuture<'a> {
        let token_cmd = cmd.starts_with("cat ");
        Box::pin(async move {
            if token_cmd {
                Ok(ExecOutput {
                    success: true,
                    stdout: "tok-remote-123\n".into(),
                    stderr: String::new(),
                })
            } else {
                Ok(ExecOutput {
                    success: true,
                    stdout: "installed\n".into(),
                    stderr: String::new(),
                })
            }
        })
    }

    fn temp_store() -> (tempfile::TempDir, crate::instances::InstancesStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::instances::InstancesStore {
            file: dir.path().join("instances.json"),
        };
        (dir, store)
    }

    /// runner 桩：被调即 panic（拒绝路径必须零命令执行）。
    /// ponytail: fn 项而非闭包——闭包对 for<'a> HRTB 边界的推断不可靠。
    fn panic_run<'a>(_i: &'a InstanceConfig, _c: &'a str) -> RunFuture<'a> {
        panic!("未确认/校验不过的请求不得执行任何命令");
    }

    // 次序编程桩：第 1 次 sudo 失败、第 2 次 curl 失败（thread_local 计数；
    // #[tokio::test] 默认单线程运行时，无跨线程竞争）
    thread_local! {
        static SEQ_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    fn seq_fail_run<'a>(_i: &'a InstanceConfig, _c: &'a str) -> RunFuture<'a> {
        let n = SEQ_CALLS.with(|c| c.get());
        SEQ_CALLS.with(|c| c.set(n + 1));
        Box::pin(async move {
            let stderr = if n == 0 {
                "sudo: a password is required".to_string()
            } else {
                "curl: (6) Could not resolve host".to_string()
            };
            Ok(ExecOutput {
                success: false,
                stdout: String::new(),
                stderr,
            })
        })
    }

    /// confirm=false 拒绝且**零命令执行**（runner 一旦被调即 panic）
    #[tokio::test]
    async fn rejects_unconfirmed_without_running_anything() {
        let plan = build_and_cache_plan("d1", Some(7800));
        let inst = ssh_inst("d1");
        let err = execute(&inst, &exec_req(&plan, false), &no_store(), panic_run)
            .await
            .unwrap_err();
        assert_eq!(err, InstallRejection::NotConfirmed);
    }

    fn no_store() -> crate::instances::InstancesStore {
        // 拒绝路径不触 store；给一个不存在的文件即可
        crate::instances::InstancesStore {
            file: std::path::PathBuf::from("/nonexistent/instances.json"),
        }
    }

    /// planHash 不匹配拒绝（确认后被换的清单不可执行）；planId 不存在同样拒绝
    #[tokio::test]
    async fn rejects_hash_mismatch_and_unknown_plan() {
        let plan = build_and_cache_plan("d2", Some(7800));
        let inst = ssh_inst("d2");
        let mut req = exec_req(&plan, true);
        req.plan_hash = "deadbeef".into();
        let err = execute(&inst, &req, &no_store(), panic_run)
            .await
            .unwrap_err();
        assert_eq!(err, InstallRejection::PlanMismatch);

        let mut req2 = exec_req(&plan, true);
        req2.plan_id = "no-such-plan".into();
        let err = execute(&inst, &req2, &no_store(), panic_run)
            .await
            .unwrap_err();
        assert_eq!(err, InstallRejection::PlanNotFound);
    }

    /// 成功路径：token 落实例配置；日志含两步说明；计划成功后消费（幂等拒绝重放）
    #[tokio::test]
    async fn success_stores_token_and_consumes_plan() {
        let plan = build_and_cache_plan("d3", Some(7800));
        let inst = ssh_inst("d3");
        let (_dir, store) = temp_store();
        store.upsert(&inst).await.unwrap();

        let outcome = execute(&inst, &exec_req(&plan, true), &store, mock_ok_run)
            .await
            .unwrap();
        assert!(outcome.ok, "error={:?}", outcome.error);
        assert!(outcome.token_stored);
        assert_eq!(
            store.get("d3").await.unwrap().token.as_deref(),
            Some("tok-remote-123")
        );
        assert!(outcome.logs.iter().any(|l| l.contains("一键安装脚本")));
        assert!(outcome.logs.iter().any(|l| l.contains("/health")));

        // 幂等：同 planId 成功后重放 → 计划已消费 → 拒绝（防重复安装误触）
        let err = execute(&inst, &exec_req(&plan, true), &store, mock_ok_run)
            .await
            .unwrap_err();
        assert_eq!(err, InstallRejection::PlanNotFound);
    }

    /// 失败路径：分类可辨（sudo 单列）；计划保留 → 同清单可重试；token 未落
    #[tokio::test]
    async fn failure_classifies_and_keeps_plan_for_retry() {
        let plan = build_and_cache_plan("d4", Some(7800));
        let inst = ssh_inst("d4");
        let (_dir, store) = temp_store();

        let o1 = execute(&inst, &exec_req(&plan, true), &store, seq_fail_run)
            .await
            .unwrap();
        assert!(!o1.ok);
        assert!(o1.error.unwrap().contains("sudo"), "sudo 失败必须单列");
        assert!(!o1.token_stored);

        // 同 planId 重试（计划未被消费）→ 再次执行
        let o2 = execute(&inst, &exec_req(&plan, true), &store, seq_fail_run)
            .await
            .unwrap();
        assert!(!o2.ok);
        assert!(o2.error.unwrap().contains("下载失败"));
    }

    /// 凭据红线：日志/错误文本不出现实例密码与回读 token 明文
    #[tokio::test]
    async fn credentials_never_in_logs() {
        let mut inst = ssh_inst("d5");
        inst.ssh.as_mut().unwrap().auth = crate::instances::SshAuth::Password;
        inst.ssh.as_mut().unwrap().password = Some("s3cr3t-pass".into());
        let plan = build_and_cache_plan(&inst.id, Some(7800));
        let (_dir, store) = temp_store();
        let outcome = execute(&inst, &exec_req(&plan, true), &store, mock_ok_run)
            .await
            .unwrap();
        let dump = format!("{:?}", outcome.logs);
        assert!(!dump.contains("s3cr3t-pass"), "密码不得进日志: {dump}");
        assert!(
            !dump.contains("tok-remote-123"),
            "token 明文不得进日志（配置里落库即可）"
        );
    }
}
