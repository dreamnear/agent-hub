//! claude CLI 封装。命令形状（spec §3.2 已实地验证）：
//! 列表 `claude agents --json [--all]`；操作 `claude logs|stop|rm <id>`；
//! 启动 `claude --bg [--name x] [--model y] "<prompt>"`（stdout 首行为 short id）。
//! 参数一律数组传递（禁 shell 拼接），全部带超时。

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use tokio::{
    process::Command,
    time::{timeout, Duration},
};

const CMD_TIMEOUT: Duration = Duration::from_secs(30);
const START_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CliOutput {
    pub id: String,
    pub cwd: Option<String>,
    pub kind: Option<String>,
    pub started_at: Option<i64>,
    pub session_id: Option<String>,
    pub name: Option<String>,
    pub state: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StartReq {
    pub prompt: String,
    pub name: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// spawn 子进程并收集 stdout；参数数组传递、超时保护、非零退出带 stderr。
async fn run(bin: &Path, args: &[&str], cwd: Option<&Path>, limit: Duration) -> Result<String> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = timeout(limit, cmd.output())
        .await
        .context("claude CLI 超时")?
        .context("spawn claude CLI 失败")?;
    if !out.status.success() {
        bail!(
            "claude {} 失败: {}",
            args.first().unwrap_or(&"?"),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 宽松解析：坏 JSON 记 error 日志返回空（列表主信号不应 panic）。
pub fn parse_agents_json(raw: &str) -> Vec<CliOutput> {
    match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "parse agents --json failed");
            Vec::new()
        }
    }
}

pub async fn agents_json(bin: &Path, include_all: bool) -> Result<Vec<CliOutput>> {
    let mut args = vec!["agents", "--json"];
    if include_all {
        args.push("--all");
    }
    let out = run(bin, &args, None, CMD_TIMEOUT).await?;
    Ok(parse_agents_json(&out))
}

pub async fn logs(bin: &Path, id: &str) -> Result<String> {
    run(bin, &["logs", id], None, CMD_TIMEOUT).await
}

pub async fn stop(bin: &Path, id: &str) -> Result<()> {
    run(bin, &["stop", id], None, CMD_TIMEOUT).await.map(|_| ())
}

pub async fn remove(bin: &Path, id: &str) -> Result<()> {
    run(bin, &["rm", id], None, CMD_TIMEOUT).await.map(|_| ())
}

/// 重启后台会话（P3）：`claude respawn <id>`（id 校验在 API 层，与 P1 一致）。
pub async fn respawn(bin: &Path, id: &str) -> Result<()> {
    run(bin, &["respawn", id], None, CMD_TIMEOUT)
        .await
        .map(|_| ())
}

pub async fn start_bg(bin: &Path, cwd: &Path, req: &StartReq) -> Result<String> {
    let mut args: Vec<String> = vec!["--bg".into()];
    if let Some(n) = &req.name {
        args.push("--name".into());
        args.push(n.clone());
    }
    if let Some(m) = &req.model {
        args.push("--model".into());
        args.push(m.clone());
    }
    if let Some(e) = &req.effort {
        args.push("--effort".into());
        args.push(e.clone());
    }
    args.push(req.prompt.clone());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(bin, &refs, Some(cwd), START_TIMEOUT).await?;
    let id = out.lines().next().unwrap_or("").trim().to_string();
    if id.is_empty() {
        bail!("claude --bg 未返回任务 id");
    }
    Ok(id)
}
