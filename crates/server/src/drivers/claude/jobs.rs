//! `~/.claude/jobs/<short-id>/` 文件读取：state.json（增强 detail/tokens）+ timeline.jsonl。
//! claude CLI 半写保护：解析失败一律 None/跳过，不报错。

use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct JobState {
    pub state: Option<String>,
    pub detail: Option<String>,
    pub tokens: Option<u64>,
    pub name: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TimelineEvent {
    pub at: Option<i64>,
    pub state: Option<String>,
    pub detail: Option<String>,
    pub text: Option<String>,
}

/// 解析 state.json；坏 JSON（半写）返回 None。
pub fn parse_state(raw: &str) -> Option<JobState> {
    serde_json::from_str(raw).ok()
}

/// 逐行解析 timeline.jsonl；坏行 warn! 跳过。
pub fn parse_timeline(raw: &str) -> Vec<TimelineEvent> {
    raw.lines()
        .filter_map(|line| match serde_json::from_str(line) {
            Ok(ev) => Some(ev),
            Err(e) => {
                tracing::warn!(error = %e, "skip bad timeline line");
                None
            }
        })
        .collect()
}

/// 读单个任务的 state.json；不存在或解析失败 → None（静默降级）。
pub async fn read_job_state(jobs_dir: &Path, id: &str) -> Option<JobState> {
    let raw = tokio::fs::read_to_string(jobs_dir.join(id).join("state.json"))
        .await
        .ok()?;
    parse_state(&raw)
}
