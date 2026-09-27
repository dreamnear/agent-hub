//! 跨会话任务清单聚合（P5 preview 反馈）：harness 任务全局共享，单会话 jsonl
//! 缺失其他会话的 TaskUpdate（实测 a8de32a1：#5/#27/#32 的 completed 在别处）。
//! 扫描同项目目录全部会话 jsonl 的 Task 系工具事件，按 realId 合并；
//! 当前会话若有 TaskList 调用，以其 result 任务集校准（对齐 harness 当前视角）。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::drivers::claude::session::session_dir;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEntry {
    pub task_id: String,
    pub subject: String,
    pub status: String,
}

fn status_rank(status: &str) -> u8 {
    match status {
        "completed" => 3,
        "in_progress" => 2,
        _ => 1,
    }
}

/// 从 TaskCreate 的 result 文本提取真实 taskId（"Task #33 created successfully: ..."）。
fn created_id_from_result(text: &str) -> Option<String> {
    let pos = text.find("Task #")?;
    let rest = &text[pos + 6..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// TaskList 权威快照；无法识别的结果不覆盖事件集，明确空快照则清空。
fn parse_tasklist_snapshot(text: &str) -> Option<BTreeMap<String, TaskEntry>> {
    if text.trim() == "No tasks found" {
        return Some(BTreeMap::new());
    }
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('#') else {
            continue;
        };
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let fields = rest[digits.len()..].trim_start();
        let (status, subject) = fields.strip_prefix('[').and_then(|s| s.split_once(']'))?;
        if digits.is_empty() || !matches!(status, "pending" | "in_progress" | "completed") {
            return None;
        }
        out.insert(
            digits.clone(),
            TaskEntry {
                task_id: digits,
                subject: subject.trim().to_string(),
                status: status.to_string(),
            },
        );
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn result_text(obj: &serde_json::Map<String, Value>) -> Option<String> {
    match obj.get("content")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(arr) => Some(
            arr.iter()
                .filter_map(|x| x.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// 单文件流式扫描，累计进聚合状态（成本控制：仅预过滤含 Task 系工具名的行）。
/// fresh_ids：当前会话最后一次 TaskList 快照之后新建的任务 realId（快照后更新叠加用）。
// ponytail: 各桶均为独立累计态，收进 struct 只挪字段不改逻辑，扫描流水线保持平铺
#[allow(clippy::too_many_arguments)]
fn scan_file(
    path: &Path,
    is_current: bool,
    creates: &mut BTreeMap<String, String>, // Create useId → subject
    result_ids: &mut BTreeMap<String, String>, // Create useId → real taskId
    updates: &mut Vec<(String, String)>,    // (real taskId, status)
    tasklist_last: &mut Option<BTreeMap<String, TaskEntry>>, // 当前会话最后 TaskList 快照
    tasklist_use_ids: &mut HashSet<String>, // 当前会话 TaskList 的 useId
    fresh_ids: &mut HashSet<String>,        // 快照后新建任务
) {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut last_tasklist_text: Option<String> = None;
    let mut after_snapshot = false;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if !line.contains("\"TaskCreate\"")
            && !line.contains("\"TaskUpdate\"")
            && !line.contains("\"TaskList\"")
            && !line.contains("\"tool_result\"")
        {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(content) = rec["message"]["content"].as_array() else {
            continue;
        };
        for b in content {
            let Some(obj) = b.as_object() else { continue };
            match obj.get("type").and_then(Value::as_str) {
                Some("tool_use") => {
                    let name = obj.get("name").and_then(Value::as_str).unwrap_or("");
                    let id = obj.get("id").and_then(Value::as_str).unwrap_or("");
                    let input = obj.get("input");
                    match name {
                        "TaskCreate" => {
                            let subject = input
                                .and_then(|i| i.get("subject"))
                                .and_then(Value::as_str)
                                .unwrap_or("(untitled)")
                                .to_string();
                            if !id.is_empty() {
                                creates.entry(id.to_string()).or_insert(subject);
                            }
                        }
                        "TaskUpdate" => {
                            if let Some(tid) = input.and_then(|i| i.get("taskId")) {
                                let tid = tid.to_string().trim_matches('"').to_string();
                                if let Some(status) =
                                    input.and_then(|i| i.get("status")).and_then(Value::as_str)
                                {
                                    updates.push((tid, status.to_string()));
                                }
                            }
                        }
                        "TaskList" if is_current && !id.is_empty() => {
                            tasklist_use_ids.insert(id.to_string());
                        }
                        _ => {}
                    }
                }
                Some("tool_result") => {
                    let uid = obj.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                    if creates.contains_key(uid) {
                        if let Some(text) = result_text(obj) {
                            if let Some(real) = created_id_from_result(&text) {
                                result_ids.insert(uid.to_string(), real.clone());
                                if after_snapshot {
                                    fresh_ids.insert(real);
                                }
                            }
                        }
                    }
                    if is_current && tasklist_use_ids.contains(uid) {
                        last_tasklist_text = result_text(obj);
                        // 快照是权威的"当时"视角：其后新建任务才算 fresh；
                        // 出现更新快照即清空旧 fresh 集，防较旧口径复活已被权威排除的任务
                        // （review-ui-r7：TaskList(A)→Create→TaskList(B) 缺 #N 时 #N 不得叠加）
                        fresh_ids.clear();
                        after_snapshot = true;
                    }
                }
                _ => {}
            }
        }
    }
    if is_current {
        if let Some(text) = last_tasklist_text {
            *tasklist_last = parse_tasklist_snapshot(&text);
        }
    }
}

/// 聚合同项目目录全部会话的任务清单（跨会话合并 + 当前会话 TaskList 权威快照）。
/// 返回 None = 无权威视角（目录/会话缺失、无快照且无事件），调用方回退单会话口径；
/// Some(空) = 有效快照明确空集（如 "No tasks found"），前端不得回退复活旧任务。
/// 目录口径：agent.cwd 对应 slug 目录优先；当前会话文件不在其中（agent 注册 cwd
/// 与会话实际 worktree 不一致，r24 实测）时以 find_session_path 定位的真实目录兜底。
pub async fn aggregate_project_tasks(
    projects_dir: &Path,
    cwd: &Path,
    session_id: &str,
) -> Option<Vec<TaskEntry>> {
    let primary = session_dir(projects_dir, cwd)?;
    let mut dir = primary.clone();
    let mut current = dir.join(format!("{session_id}.jsonl"));
    if !current.exists() {
        if let Some(real) =
            crate::drivers::claude::session::find_session_path(projects_dir, session_id).await
        {
            dir = real
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| primary.clone());
            current = real;
        }
    }
    tokio::task::spawn_blocking(move || {
        let mut creates = BTreeMap::new();
        let mut result_ids = BTreeMap::new();
        let mut updates = Vec::new();
        let mut tasklist_last: Option<BTreeMap<String, TaskEntry>> = None;
        let mut tasklist_use_ids = HashSet::new();
        let mut fresh_ids = HashSet::new();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return None;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
            .collect();
        files.sort();
        for f in &files {
            scan_file(
                f,
                f == &current,
                &mut creates,
                &mut result_ids,
                &mut updates,
                &mut tasklist_last,
                &mut tasklist_use_ids,
                &mut fresh_ids,
            );
        }
        // 事件池组装：realId → (subject, 状态最高进度)
        let mut merged: BTreeMap<String, (String, u8)> = BTreeMap::new();
        for (uid, subject) in &creates {
            if let Some(real) = result_ids.get(uid) {
                merged
                    .entry(real.clone())
                    .or_insert_with(|| (subject.clone(), 1));
            }
        }
        for (tid, status) in &updates {
            if let Some(entry) = merged.get_mut(tid) {
                let rank = status_rank(status);
                if rank > entry.1 {
                    entry.1 = rank;
                }
            }
        }
        // 快照权威（tester-r27）：有效快照全量替换事件池（id/status/subject 以快照
        // 为准，事件仅补缺失标题），补齐目录外创建的任务（r27 缺 #29/30/31 根因）；
        // 快照后本会话新建任务（fresh）叠加保留；快照外其余历史任务维持既定过滤。
        // 无法识别的快照文本不覆盖事件池（防御，tester-r24 既有口径）。
        // 删除语义（反馈轮 9）：TaskUpdate status=deleted = 任务已从 harness 移除，
        // 无论快照是否仍列出（旧快照），一律从最终集合剔除。
        let deleted: HashSet<&str> = updates
            .iter()
            .filter(|(_, status)| status == "deleted")
            .map(|(tid, _)| tid.as_str())
            .collect();
        let merged = match tasklist_last {
            Some(snap) => {
                let mut calibrated: BTreeMap<String, (String, u8)> = snap
                    .into_iter()
                    .map(|(tid, task)| {
                        let subject = if task.subject.is_empty() {
                            merged
                                .get(&tid)
                                .map(|entry| entry.0.clone())
                                .unwrap_or_else(|| "(untitled)".into())
                        } else {
                            task.subject
                        };
                        (tid, (subject, status_rank(&task.status)))
                    })
                    .collect();
                for (uid, subject) in &creates {
                    let Some(real) = result_ids.get(uid) else {
                        continue;
                    };
                    if !fresh_ids.contains(real) || calibrated.contains_key(real) {
                        continue;
                    }
                    let mut rank = 1;
                    for (tid, status) in &updates {
                        if tid == real {
                            rank = rank.max(status_rank(status));
                        }
                    }
                    calibrated.insert(real.clone(), (subject.clone(), rank));
                }
                calibrated.retain(|tid, _| !deleted.contains(tid.as_str()));
                Some(calibrated)
            }
            None if merged.is_empty() => None,
            None => {
                merged.retain(|tid, _| !deleted.contains(tid.as_str()));
                Some(merged)
            }
        };
        let mut out: Vec<TaskEntry> = merged?
            .into_iter()
            .map(|(task_id, (subject, rank))| TaskEntry {
                task_id,
                subject,
                status: match rank {
                    3 => "completed".into(),
                    2 => "in_progress".into(),
                    _ => "pending".into(),
                },
            })
            .collect();
        out.sort_by_key(|t| t.task_id.parse::<u64>().unwrap_or(0));
        Some(out)
    })
    .await
    .unwrap_or(None)
}
