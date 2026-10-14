//! `ah` bin：CLI 解析与输出组装；能力全在 lib（api/serve/attach），集成测试打 lib。

use ah::api;
use anyhow::bail;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ah", version, about = "agent-hub 命令行窗口")]
struct Cli {
    /// hub 地址（默认 http://127.0.0.1:7800，AH_URL 可覆盖，本参数优先）
    #[arg(long, global = true)]
    url: Option<String>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 列出会话（默认活跃；--all 含已结束；--json 透传原始 JSON）
    Agents {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// hub 守护形态：未运行则 detach 拉起 server（幂等）；--stop 停止；--status 查看状态
    Serve {
        /// server 二进制路径（缺省 AH_SERVER_BIN > 与 ah 同目录的 server）
        #[arg(long)]
        bin: Option<String>,
        #[arg(long)]
        stop: bool,
        #[arg(long)]
        status: bool,
    },
    /// 按名进入会话交互（精确名 → 唯一前缀模糊；Ctrl-C/Ctrl-D 退出）
    Attach { name: String },
}

/// 驱动 → monogram 字母（与 web/src/components/DriverBadge.tsx 同映射；未知驱动不虚构）。
fn monogram(driver: &str) -> &str {
    match driver {
        "claude" => "C",
        "opencode" => "O",
        "omp" => "P",
        "dsh" => "D",
        _ => "·",
    }
}

/// group → 状态词（三态 + 兜底，对齐 server models::map_group 语义）。
fn state_label(group: Option<&str>) -> &'static str {
    match group {
        Some("needs_input") => "待输入",
        Some("working") => "工作中",
        Some("completed") => "已完成",
        _ => "其他",
    }
}

/// cwd 截断：超长保留尾部（就近目录部分比前缀更有辨识度）。
fn truncate_tail(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().skip(s.chars().count() - (max - 1)).collect();
    format!("…{cut}")
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// 对齐表格：状态 / 驱动 / 名称 / CWD / 会话（实例 id 前 8 位）。
fn render_agents(agents: &[api::AgentSummary]) -> String {
    let rows: Vec<(String, String, String, String, String)> = agents
        .iter()
        .map(|a| {
            (
                state_label(a.group.as_deref()).to_string(),
                monogram(&a.driver).to_string(),
                a.name.clone().unwrap_or_else(|| a.id.clone()),
                truncate_tail(a.cwd.as_deref().unwrap_or("-"), 42),
                a.id.chars().take(8).collect(),
            )
        })
        .collect();
    let w = (
        rows.iter()
            .map(|r| r.0.chars().count())
            .max()
            .unwrap_or(6)
            .max(6),
        rows.iter()
            .map(|r| r.1.chars().count())
            .max()
            .unwrap_or(4)
            .max(4),
        rows.iter()
            .map(|r| r.2.chars().count())
            .max()
            .unwrap_or(6)
            .max(6),
        rows.iter()
            .map(|r| r.3.chars().count())
            .max()
            .unwrap_or(3)
            .max(3),
    );
    let mut out = format!(
        "{}  {}  {}  {}  {}\n",
        pad("状态", w.0),
        pad("驱动", w.1),
        pad("名称", w.2),
        pad("CWD", w.3),
        "会话"
    );
    for r in &rows {
        out.push_str(&format!(
            "{}  {}  {}  {}  {}\n",
            pad(&r.0, w.0),
            pad(&r.1, w.1),
            pad(&r.2, w.2),
            pad(&r.3, w.3),
            r.4
        ));
    }
    out
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let hub = api::Hub::new(api::resolve_url(cli.url.as_deref()));
    match cli.cmd {
        Cmd::Agents { all, json } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&hub.agents_raw(all).await?)?
                );
            } else {
                let agents = hub.agents(all).await?;
                if agents.is_empty() {
                    println!("（无会话）");
                } else {
                    print!("{}", render_agents(&agents));
                }
            }
        }
        Cmd::Serve { bin, stop, status } => {
            let msg = if stop {
                ah::serve::stop(&ah::serve::pid_file())?
            } else if status {
                let url = api::resolve_url(cli.url.as_deref());
                ah::serve::status(&ah::serve::pid_file(), &url, &hub).await?
            } else {
                let url = api::resolve_url(cli.url.as_deref());
                ah::serve::start(bin.as_deref(), &url, &hub).await?
            };
            println!("{msg}");
        }
        Cmd::Attach { name } => {
            let all = hub.agents(true).await?;
            match ah::attach::resolve(&all, &name) {
                Ok(agent) => ah::attach::run(&hub, &agent).await?,
                Err(ah::attach::ResolveError::NotFound) => {
                    bail!("未找到会话: {name}（ah agents --all 查看清单）")
                }
                Err(ah::attach::ResolveError::Ambiguous(v)) => {
                    let mut m = format!("「{name}」匹配到多条候选，请用更精确的名字:\n");
                    for a in &v {
                        m.push_str(&format!(
                            "  {}  {}\n",
                            a.name.as_deref().unwrap_or("-"),
                            &a.id[..a.id.len().min(8)]
                        ));
                    }
                    bail!("{m}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(
        driver: &str,
        id: &str,
        name: Option<&str>,
        cwd: Option<&str>,
        group: &str,
    ) -> api::AgentSummary {
        serde_json::from_value(serde_json::json!({
            "driver": driver, "id": id, "name": name, "cwd": cwd,
            "kind": null, "rawState": null, "group": group,
            "detail": null, "tokens": null, "startedAt": null, "sessionId": null,
        }))
        .unwrap()
    }

    #[test]
    fn parses_server_agent_summary_json() {
        let a = agent(
            "claude",
            "abcd1234-xxxx",
            Some("日报"),
            Some("/Users/k/w"),
            "working",
        );
        assert_eq!(a.driver, "claude");
        assert_eq!(a.name.as_deref(), Some("日报"));
        assert_eq!(a.group.as_deref(), Some("working"));
    }

    #[test]
    fn table_renders_aligned_columns() {
        let agents = vec![
            agent(
                "claude",
                "aaaaaaaa-1",
                Some("日报"),
                Some("/Users/k/dev/very/long/project/path/branch/deep"),
                "working",
            ),
            agent("opencode", "bbbbbbbb-2", None, None, "needs_input"),
        ];
        let t = render_agents(&agents);
        let mut lines = t.lines();
        let header = lines.next().unwrap();
        assert!(header.contains("状态") && header.contains("驱动") && header.contains("名称"));
        let row1 = lines.next().unwrap();
        // 各行等字符数（CJK 状态词等宽，按字符数对齐）
        assert_eq!(row1.chars().count(), lines.next().unwrap().chars().count());
        assert!(row1.contains('C'));
        assert!(t.contains('O'));
        assert!(t.contains("待输入"));
        assert!(t.contains("…/k/dev/very"));
    }

    #[test]
    fn state_and_monogram_mappings() {
        assert_eq!(state_label(Some("needs_input")), "待输入");
        assert_eq!(state_label(Some("working")), "工作中");
        assert_eq!(state_label(Some("completed")), "已完成");
        assert_eq!(state_label(Some("weird")), "其他");
        assert_eq!(state_label(None), "其他");
        assert_eq!(monogram("claude"), "C");
        assert_eq!(monogram("opencode"), "O");
        assert_eq!(monogram("omp"), "P");
        assert_eq!(monogram("dsh"), "D");
        assert_eq!(monogram("future"), "·");
    }

    #[test]
    fn url_resolution_priority() {
        // --url > AH_URL > 默认（env 缺失场景）
        std::env::remove_var("AH_URL");
        assert_eq!(api::resolve_url(Some("http://x:1")), "http://x:1");
        std::env::set_var("AH_URL", "http://y:2");
        assert_eq!(api::resolve_url(Some("http://x:1")), "http://x:1");
        assert_eq!(api::resolve_url(None), "http://y:2");
        std::env::remove_var("AH_URL");
        assert_eq!(api::resolve_url(None), "http://127.0.0.1:7800");
    }
}
