pub mod attach;
pub mod cli;
pub mod jobs;
pub mod send;
pub mod session;
pub mod subagents;
pub mod tasks;
pub mod watcher;

use std::path::PathBuf;

use anyhow::Result;

use crate::models::{map_group, AgentSummary};

#[derive(Debug, Clone)]
pub struct ClaudeDriver {
    pub bin: PathBuf,
    pub jobs_dir: PathBuf,
}

impl ClaudeDriver {
    /// CLI 列表为基座（driver="claude"）；id 命中 jobs 目录时增强 detail/tokens，
    /// name/sessionId/cwd 冲突以 CLI 为准；jobs 读取失败静默降级为仅 CLI 信息。
    pub async fn list(&self, all: bool) -> Result<Vec<AgentSummary>> {
        let cli_list = match cli::agents_json(&self.bin, all).await {
            Ok(v) => v,
            // claude CLI 未安装（全新机器）：后台任务按空处理，ACP 会话不受影响；
            // CLI 存在但命令失败/超时仍照常报错
            Err(e)
                if e.chain().any(|c| {
                    c.downcast_ref::<std::io::Error>()
                        .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
                }) =>
            {
                tracing::warn!(bin = %self.bin.display(), "claude CLI 不可用（未安装？），后台任务列表按空处理");
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        let mut out = Vec::with_capacity(cli_list.len());
        for c in cli_list {
            let (detail, tokens) = match jobs::read_job_state(&self.jobs_dir, &c.id).await {
                Some(job) => (job.detail, job.tokens),
                None => (None, None),
            };
            out.push(AgentSummary {
                driver: "claude".into(),
                id: c.id,
                name: c.name,
                cwd: c.cwd,
                kind: c.kind,
                group: map_group(c.state.as_deref().unwrap_or("")),
                raw_state: c.state,
                detail,
                tokens,
                started_at: c.started_at,
                session_id: c.session_id,
            });
        }
        Ok(out)
    }
}
