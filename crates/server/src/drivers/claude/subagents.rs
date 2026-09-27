//! subagent 会话发现（P5+）：`<slug>/<sid>/subagents/agent-*.jsonl` + 同名 `.meta.json`。
//! meta 提供 name/agentType/description/model（teammate 有 name，匿名 subagent 无）；
//! 起止时间取 jsonl 首末行 timestamp；运行状态无现成字段，按 mtime 新鲜度推断
//! （<2 分钟视为 active，调查报告 1.4/四-1 口径，teammate tool_result 配对留作后续增强）。

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::drivers::claude::session::{find_session_path, session_dir};

/// mtime 新于此秒数视为 active（tail 间隔 500ms 的富余量级，非精确认定）。
const ACTIVE_WINDOW_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentEntry {
    pub agent_id: String,
    pub name: String,
    pub agent_type: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub status: String,
    pub started_at: Option<String>,
    pub last_active_at: Option<String>,
}

/// 列出主会话的 subagent（目录缺失 = 无 subagent 视图，返回空集）。
pub async fn list_subagents(
    projects_dir: &Path,
    cwd: &Path,
    session_id: &str,
) -> Vec<SubagentEntry> {
    let main_file = match session_dir(projects_dir, cwd) {
        Some(dir) if dir.join(format!("{session_id}.jsonl")).is_file() => {
            Some(dir.join(format!("{session_id}.jsonl")))
        }
        _ => find_session_path(projects_dir, session_id).await,
    };
    let Some(slug_dir) = main_file.as_ref().and_then(|p| p.parent()) else {
        return Vec::new();
    };
    let subagents_dir = slug_dir.join(session_id).join("subagents");
    tokio::task::spawn_blocking(move || scan_subagents_dir(&subagents_dir))
        .await
        .unwrap_or_default()
}

fn scan_subagents_dir(dir: &Path) -> Vec<SubagentEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|e| e == "jsonl")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("agent-"))
        })
        .collect();
    files.sort();
    files.iter().filter_map(|p| scan_one(p)).collect()
}

fn scan_one(jsonl: &Path) -> Option<SubagentEntry> {
    let filename = jsonl.file_name()?.to_str()?;
    let agent_id = filename
        .strip_prefix("agent-")?
        .strip_suffix(".jsonl")?
        .to_string();
    // meta 宽松读取：缺失/坏 JSON 全降级（调查报告 四-1 降级口径）
    let meta: Value = std::fs::read_to_string(jsonl.with_extension("meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let meta_str = |key: &str| {
        meta.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    let (started_at, last_active_at) = first_last_timestamps(jsonl);
    // ponytail: mtime 新鲜度推断 active，teammate tool_result 配对如需精确再加
    let is_active = jsonl
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|e| e.as_secs() < ACTIVE_WINDOW_SECS);
    Some(SubagentEntry {
        name: meta_str("name").unwrap_or_else(|| agent_id.chars().take(8).collect()),
        agent_type: meta_str("agentType")
            .or_else(|| meta_str("customAgentType"))
            .unwrap_or_else(|| "subagent".into()),
        description: meta_str("description"),
        model: meta_str("model"),
        status: if is_active { "active" } else { "completed" }.into(),
        agent_id,
        started_at,
        last_active_at,
    })
}

/// 首末行 timestamp（只解析首行 + 最后一个非空行，成本与大文件解耦）。
fn first_last_timestamps(path: &Path) -> (Option<String>, Option<String>) {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return (None, None);
    };
    let mut first_raw: Option<String> = None;
    let mut last_raw = String::new();
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if first_raw.is_none() {
            first_raw = Some(line.clone());
        }
        last_raw = line;
    }
    (
        first_raw.as_deref().and_then(timestamp_of),
        timestamp_of(&last_raw),
    )
}

fn timestamp_of(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string)
}
