//! 会话 jsonl 读取：`~/.claude/projects/<slug>/<sessionId>.jsonl`。
//! slug 编码实测（2026-09-16，tester-r2 教训同源）：`/` → `-`、`.` → `-`、其余字符保留。
//! 佐证样本：`/Users/kevin/.claude/jobs/aa14d3e3-tmp` → `-Users-kevin--claude-jobs-aa14d3e3-tmp`。

use std::path::{Path, PathBuf};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::task::JoinHandle;

use crate::drivers::events::{ChatEvent, MessagePage};
use crate::models::{ChatMessage, ChatMessageKind};

/// cwd → `<projects_dir>/projects/<slug>` 目录（tasks.md：projects_dir 是 `.claude` 根）；
/// 无法编码（空路径）返回 None。
pub fn session_dir(projects_dir: &Path, cwd: &Path) -> Option<PathBuf> {
    let s = cwd.to_str()?;
    if s.is_empty() {
        return None;
    }
    let slug: String = s
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    Some(projects_dir.join("projects").join(slug))
}

/// 读整个会话文件并解析（宽松：坏行 warn! 跳过；非对话类型行跳过）。
pub async fn parse_session_file(path: &Path) -> Option<Vec<ChatMessage>> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    Some(parse_session_lines(&raw))
}

/// 分页读取（P6 B12/B13）：无 before 取末尾至多 limit 条；有 before 取该行号之前
/// 至多 limit 条。jsonl append-only、头部行号稳定 → 物理行号可作安全游标。
/// ponytail: 整文件读但只解析窗口行（顺序 IO ~100ms 级远低于全量解析与传输成本）；
/// profiling 证明 IO 成瓶颈再上字节块倒读。
pub async fn read_session_page(
    path: &Path,
    limit: usize,
    before: Option<usize>,
) -> Option<MessagePage> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    let lines: Vec<&str> = raw.lines().collect();
    let end = before.unwrap_or(lines.len()).min(lines.len());
    // 从窗口上界向前逐行解析，累计到 limit 条即停；记录每条消息的源行号供游标对齐
    let mut picked: Vec<(usize, Vec<ChatMessage>)> = Vec::new();
    let mut start = end;
    while start > 0 {
        let line_no = start - 1;
        let msgs = parse_line(lines[line_no]);
        start = line_no;
        if msgs.is_empty() {
            continue; // 非对话行/坏行：跳过但继续向前
        }
        picked.push((line_no, msgs));
        let total: usize = picked.iter().map(|(_, m)| m.len()).sum();
        if total >= limit {
            break;
        }
    }
    picked.reverse();
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut rows: Vec<usize> = Vec::new();
    for (line_no, msgs) in picked {
        for m in msgs {
            rows.push(line_no);
            messages.push(m);
        }
    }
    // 最旧一行可能超量（一行多 block）→ 丢头部超出部分，保持恰好 ≤limit 条且为最近的
    if messages.len() > limit {
        let drop = messages.len() - limit;
        messages.drain(0..drop);
        rows.drain(0..drop);
    }
    Some(MessagePage {
        first_line: rows.first().copied().unwrap_or(end),
        has_more: start > 0,
        messages,
    })
}

/// Task 4 GET messages 入口：session_id 定位文件（cwd 已知时优先 session_dir，否则全 projects 扫描兜底）。
pub async fn read_session(projects_dir: &Path, session_id: &str) -> Option<Vec<ChatMessage>> {
    let path = find_session_path(projects_dir, session_id).await?;
    parse_session_file(&path).await
}

/// 尾部读窗口（context 提取）：usage 在长会话尾部高频出现，256KB 内必命中。
const CONTEXT_TAIL_WINDOW: u64 = 256 * 1024;

/// 会话当前 context 占用（agent-hub-daemon-capabilities）：转录最后一条 assistant
/// usage 的 input + cache_read + cache_creation ≈ 当前窗口占用。与 jobs state.json
/// 的 `tokens`（全回合累计，实测 920k vs 上下文 460k）语义不同，勿混用。
/// 尾部有限读（256KB 窗口，seek 不整读）；窗口内无 usage → None（fail-soft）。
pub async fn last_context_tokens(projects_dir: &Path, session_id: &str) -> Option<u64> {
    let path = find_session_path(projects_dir, session_id).await?;
    let mut f = tokio::fs::File::open(&path).await.ok()?;
    let len = f.metadata().await.ok()?.len();
    let start = len.saturating_sub(CONTEXT_TAIL_WINDOW);
    f.seek(std::io::SeekFrom::Start(start)).await.ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).await.ok()?;
    let raw = String::from_utf8_lossy(&buf);
    // 非零起点时首行可能被窗口切半：丢至首个完整行边界
    let window = if start > 0 {
        raw.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        &raw
    };
    for line in window.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // 坏行/切半行
        };
        let Some(u) = v.get("message").and_then(|m| m.get("usage")) else {
            continue;
        };
        let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let sum = get("input_tokens")
            + get("cache_read_input_tokens")
            + get("cache_creation_input_tokens");
        if sum > 0 {
            return Some(sum);
        }
    }
    None
}

/// session_id 白名单校验（禁路径分量）后在 `<projects_dir>/projects/*/` 下定位 `<id>.jsonl`。
pub async fn find_session_path(projects_dir: &Path, session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
    {
        return None;
    }
    let projects_root = projects_dir.join("projects");
    let mut entries = tokio::fs::read_dir(&projects_root).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if tokio::fs::metadata(&candidate)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some(candidate);
        }
    }
    None
}

/// subagent 会话 jsonl 定位（P5+）：`<slug>/<session_id>/subagents/agent-<subagent_id>.jsonl`。
/// session_id 校验同 find_session_path；subagent_id 白名单 `[a-z0-9-]`（harness 两形态
/// `<name>-<hex16>` / `<hex16>` 天然合规，同时防路径分量/逃逸）。
pub async fn find_subagent_path(
    projects_dir: &Path,
    session_id: &str,
    subagent_id: &str,
) -> Option<PathBuf> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
    {
        return None;
    }
    if subagent_id.is_empty()
        || !subagent_id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    let projects_root = projects_dir.join("projects");
    let mut entries = tokio::fs::read_dir(&projects_root).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let candidate = entry
            .path()
            .join(session_id)
            .join("subagents")
            .join(format!("agent-{subagent_id}.jsonl"));
        if tokio::fs::metadata(&candidate)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            return Some(candidate);
        }
    }
    None
}

/// 会话 jsonl 最近写入时刻（反馈轮 19：中断标记解除判据）。无文件/非法 id = None。
pub async fn session_mtime(projects_dir: &Path, session_id: &str) -> Option<std::time::SystemTime> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains("..")
    {
        return None;
    }
    let projects_root = projects_dir.join("projects");
    let Ok(mut entries) = tokio::fs::read_dir(&projects_root).await else {
        return None;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let candidate = entry.path().join(format!("{session_id}.jsonl"));
        if let Ok(meta) = tokio::fs::metadata(&candidate).await {
            if meta.is_file() {
                return meta.modified().ok();
            }
        }
    }
    None
}

/// 会话输出活跃度（反馈 7-2）：jsonl mtime 在静默阈值内 = 工作中且有输出。
/// 比消息块驱动的脉冲指示稳定（块间静默不抖动）；超阈值无输出才算完成回摆。
pub async fn session_is_active(projects_dir: &Path, session_id: &str, within_secs: u64) -> bool {
    session_mtime(projects_dir, session_id)
        .await
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|e| e.as_secs() < within_secs)
}

/// 中断收尾基线采样（r69 竞态修复）：轮询 jsonl mtime，观察满 `min_wait_ms`
/// 且静默 `quiet_ms` 无前进后取当前值返回；总耗时上限 `timeout_ms`。
///
/// 为什么不能固定延迟后单次采样：CLI 处理 Esc 后的中断收尾写入（abort 记录）
/// 落盘时刻不定，固定 2s 等待会被更晚的收尾写入击穿——收尾写入自己就满足
/// 「mtime 越过基线且 30s 活跃」的解除判据，中断标记在下次 list 即被解除，
/// 状态回落 CLI 滞留的 working（r69 实测 0/N 转闲）。静默采样保证基线落在
/// 收尾写入之后，此后 mtime 再前进只可能是真实新活动。
///
/// min_wait 的作用：纯静默路径（收尾写入尚未落盘）不得提前退出——退出点
/// = max(min_wait, quiet)。收尾写入晚于该退出点仍可能击穿基线（实测罕见，
/// abort 记录在 Esc 后数秒内落盘）；根治需改由 PTY 输出静默信号驱动采样。
///
/// timeout 兜底的中断失败场景（Esc 被吞、输出持续流动）：mtime 不断前进，
/// 返回最后的 mtime——解除判据随即放行，恢复权威 working，不残留假空闲。
pub async fn sample_quiet_mtime(
    projects_dir: &Path,
    session_id: &str,
    quiet_ms: u64,
    min_wait_ms: u64,
    timeout_ms: u64,
) -> Option<std::time::SystemTime> {
    const POLL_MS: u64 = 200;
    let mut baseline = session_mtime(projects_dir, session_id).await;
    let mut quiet: u64 = 0;
    let mut elapsed: u64 = 0;
    while elapsed < timeout_ms {
        tokio::time::sleep(std::time::Duration::from_millis(POLL_MS)).await;
        elapsed += POLL_MS;
        match session_mtime(projects_dir, session_id).await {
            Some(t) if baseline.is_some_and(|b| t > b) => {
                baseline = Some(t);
                quiet = 0;
            }
            _ => quiet += POLL_MS,
        }
        if quiet >= quiet_ms && elapsed >= min_wait_ms {
            break;
        }
    }
    baseline
}

/// 工程历史会话条目（agent-hub-session-history）：CLI `--resume` 选择器同口径的
/// 行内信息投影。session_id 来自服务端目录扫描（非用户输入）；resume 回传时由
/// 端点按 find_session_path 同款白名单复验。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListEntry {
    pub session_id: String,
    /// 最近写入时刻（秒级 epoch；相对时间口径，CLI 选择器同款排序基准）
    pub mtime_secs: i64,
    /// jsonl 字节体积（CLI 选择器显示体积的原始数）
    pub size_bytes: u64,
    /// 会话 git 分支（转录行 `gitBranch`；无 → None）
    pub branch: Option<String>,
    /// 会话发生时的工作目录（转录行 `cwd`；与工程 cwd 不同即 worktree 会话，
    /// 前端比对后显示 worktree 尾段）
    pub cwd: Option<String>,
    /// 摘要：头部 `type:"summary"` 行优先（CLI resume 列表标题同源），
    /// 否则首条用户消息截断
    pub summary: Option<String>,
}

/// 头部读取窗口：summary 行与首条用户消息都在对话开头，64KB 足够；超长注入场景
/// 窗口切半的尾行按完整行边界丢弃（raw.lines 对切半行解析失败自然跳过）。
const LIST_HEAD_WINDOW: u64 = 64 * 1024;

/// 摘要文本上限（chars 计；前端单行省略，超长截断即可辨识）
const SUMMARY_MAX_CHARS: usize = 120;

/// 工程历史会话枚举（agent-hub-session-history）：只读列
/// `<projects_dir>/projects/<slug>/`（cwd → slug 走 session_dir 同款编码，防穿越），
/// mtime 倒序。目录不存在/不可读 → 空集（前端空态语义）；单文件 metadata 失败
/// 跳过（宽松，坏条目不拖垮整表）。
pub async fn list_project_sessions(projects_dir: &Path, cwd: &Path) -> Vec<SessionListEntry> {
    let Some(dir) = session_dir(projects_dir, cwd) else {
        return Vec::new();
    };
    let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let mtime_secs = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let (branch, cwd_row, summary) = peek_session_head(&path).await;
        out.push(SessionListEntry {
            session_id: name.to_string(),
            mtime_secs,
            size_bytes: meta.len(),
            branch,
            cwd: cwd_row,
            summary,
        });
    }
    out.sort_by(|a, b| {
        b.mtime_secs
            .cmp(&a.mtime_secs)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out
}

/// 头部窗口读取（有限读不整文件）：提取 (gitBranch, cwd, summary)。
/// 文件打不开/读失败 → 三 None（fail-soft，条目仍列出行内缺项显示）。
async fn peek_session_head(path: &Path) -> (Option<String>, Option<String>, Option<String>) {
    let mut f = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(_) => return (None, None, None),
    };
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let end = len.min(LIST_HEAD_WINDOW) as usize;
    let mut buf = vec![0u8; end];
    if f.read_exact(&mut buf).await.is_err() {
        return (None, None, None);
    }
    let raw = String::from_utf8_lossy(&buf);
    let (mut branch, mut cwd, mut summary) = (None, None, None);
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // 坏行/窗口切半行
        };
        if branch.is_none() {
            branch = v
                .get("gitBranch")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(Value::as_str).map(str::to_string);
        }
        if summary.is_none() {
            summary = head_summary_of(&v);
        }
        if branch.is_some() && cwd.is_some() && summary.is_some() {
            break;
        }
    }
    (branch, cwd, summary)
}

/// 单行摘要提取：`type:"summary"` 行的 summary 字段优先；否则首条非 isMeta
/// user 行的消息文本（字符串或 text 块数组）截断。task-notification 等 meta 行
/// 不作摘要（与 parse_line 的 harness 元注入口径同源）。
fn head_summary_of(v: &Value) -> Option<String> {
    let row_type = v.get("type").and_then(Value::as_str).unwrap_or("");
    if row_type == "summary" {
        return v
            .get("summary")
            .and_then(Value::as_str)
            .map(|s| truncate_chars(s, SUMMARY_MAX_CHARS));
    }
    if row_type != "user" || v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let text = match v.pointer("/message/content") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Array(blocks)) => blocks.iter().find_map(|b| {
            if b.get("type").and_then(Value::as_str) == Some("text") {
                b.get("text").and_then(Value::as_str)
            } else {
                None
            }
        }),
        _ => None,
    }?
    .trim();
    if text.is_empty() {
        return None;
    }
    Some(truncate_chars(text, SUMMARY_MAX_CHARS))
}

/// chars 边界安全截断（超长加 …）
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

/// 逐行解析：仅 user/assistant 行是对话；message.content 字符串或块数组，一块展开一条 ChatMessage。
fn parse_session_lines(raw: &str) -> Vec<ChatMessage> {
    let mut out = Vec::new();
    for line in raw.lines() {
        out.extend(parse_line(line));
    }
    out
}

/// 单行解析（tail 复用）；坏行返回空。
fn parse_line(line: &str) -> Vec<ChatMessage> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "skip bad session line");
            return Vec::new();
        }
    };
    let row_type = v.get("type").and_then(Value::as_str).unwrap_or("");
    if row_type == "attachment" {
        // 反馈轮 25-B：working 期排队消息在当前轮结束时被 CLI 以 absorbed_mid_turn
        // 吸收为 queued_command 附件（不落 user 行）——模型已实际处理（实测回复
        // "均已收到"），但 UI 不渲染该类型 = 用户观感"发消息丢失 agent 无反应"。
        // 还原为 User 消息：前端可见 + 乐观气泡对账天然销账。
        if v.pointer("/attachment/type").and_then(Value::as_str) == Some("queued_command") {
            let text = v
                .pointer("/attachment/prompt")
                .and_then(Value::as_str)
                .map(str::to_string);
            let ts = v
                .pointer("/attachment/timestamp")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(text) = text {
                return vec![ChatMessage {
                    kind: ChatMessageKind::User,
                    raw_type: Some("queued_command".to_string()),
                    text: Some(text),
                    ts,
                    ..Default::default()
                }];
            }
        }
        return Vec::new();
    }
    if row_type == "user" && v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        // harness 元注入（skill 注入正文、hook 上下文等）原生界面不作对话消息，
        // hub 同样不渲染（否则是几百行注入正文占满消息流）；task-notification
        // 通知保留——前端按系统块折叠成一行摘要条
        if !v.to_string().contains("<task-notification>") {
            return Vec::new();
        }
    }
    if row_type != "user" && row_type != "assistant" {
        return Vec::new(); // queue-operation/file-history-snapshot/mode 等非对话类型
    }
    let ts = v
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    let raw_type = Some(row_type.to_string());
    let mut out = Vec::new();
    match v.pointer("/message/content") {
        Some(Value::String(s)) => out.push(ChatMessage {
            kind: kind_of(row_type),
            raw_type: raw_type.clone(),
            text: Some(s.clone()),
            ts,
            ..Default::default()
        }),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                if let Some(m) =
                    block_to_message(block, row_type, raw_type.as_deref(), ts.as_deref())
                {
                    out.push(m);
                }
            }
        }
        _ => {}
    }
    out
}

fn kind_of(row_type: &str) -> ChatMessageKind {
    if row_type == "user" {
        ChatMessageKind::User
    } else {
        ChatMessageKind::Assistant
    }
}

fn block_to_message(
    block: &Value,
    row_type: &str,
    raw_type: Option<&str>,
    ts: Option<&str>,
) -> Option<ChatMessage> {
    let block_type = block.get("type").and_then(Value::as_str)?;
    let base = ChatMessage {
        raw_type: raw_type.map(str::to_string),
        ts: ts.map(str::to_string),
        ..Default::default()
    };
    match block_type {
        "text" => Some(ChatMessage {
            kind: kind_of(row_type),
            text: block
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string),
            ..base
        }),
        "thinking" => Some(ChatMessage {
            kind: ChatMessageKind::Thinking,
            text: block
                .get("thinking")
                .or_else(|| block.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string),
            ..base
        }),
        "tool_use" => Some(ChatMessage {
            kind: ChatMessageKind::ToolUse,
            tool_use_id: block.get("id").and_then(Value::as_str).map(str::to_string),
            tool_name: block
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            input: block.get("input").cloned(),
            ..base
        }),
        "tool_result" => Some(ChatMessage {
            kind: ChatMessageKind::ToolResult,
            tool_use_id: block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            text: block
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_string),
            result: block.get("content").cloned(),
            error: block
                .get("is_error")
                .and_then(Value::as_bool)
                .or(Some(false)),
            ..base
        }),
        _ => None, // 未知块类型跳过（宽松）
    }
}

/// tail 会话 jsonl（500ms 轮询行数游标），新行 → ChatEvent 入房间。
/// seq 从 1 起会话内递增；坏行跳过不占 seq。
/// ponytail: 房间任务不回收（本机工具，单会话轮询成本极低）；多用户需 idle 回收。
pub fn spawn_session_tail(
    projects_dir: PathBuf,
    session_id: String,
    tx: tokio::sync::broadcast::Sender<ChatEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let path = match find_session_path(&projects_dir, &session_id).await {
            Some(p) => p,
            None => {
                tracing::warn!(session_id = %session_id, "session jsonl 未找到，tail 退出");
                return;
            }
        };
        tail_file(path, session_id, tx).await;
    })
}

/// subagent 会话 tail（P5+）：定位逻辑换成 find_subagent_path，其余同主会话 tail。
pub fn spawn_subagent_tail(
    projects_dir: PathBuf,
    session_id: String,
    subagent_id: String,
    tx: tokio::sync::broadcast::Sender<ChatEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let path = match find_subagent_path(&projects_dir, &session_id, &subagent_id).await {
            Some(p) => p,
            None => {
                tracing::warn!(session_id = %session_id, subagent_id = %subagent_id, "subagent jsonl 未找到，tail 退出");
                return;
            }
        };
        tail_file(path, subagent_id, tx).await;
    })
}

/// tail 共享循环：500ms 轮询行数游标，新行解析后以 `session_label` 入房间。
async fn tail_file(
    path: PathBuf,
    session_label: String,
    tx: tokio::sync::broadcast::Sender<ChatEvent>,
) {
    let mut last_lines = count_lines(&path).await.unwrap_or(0);
    let mut seq: u64 = 0;
    loop {
        tokio::time::sleep(TAIL_INTERVAL).await;
        let total = count_lines(&path).await.unwrap_or(last_lines);
        if total <= last_lines {
            continue;
        }
        if let Ok(raw) = tokio::fs::read_to_string(&path).await {
            for line in raw.lines().skip(last_lines as usize) {
                for message in parse_line(line) {
                    seq += 1;
                    let _ = tx.send(ChatEvent {
                        session_id: session_label.clone(),
                        message,
                        seq,
                    });
                }
            }
        }
        last_lines = total;
    }
}

const TAIL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// 行数统计（逐行计数，尾部无换行的最后一行也算）。
async fn count_lines(path: &Path) -> Option<u64> {
    let raw = tokio::fs::read_to_string(path).await.ok()?;
    Some(raw.lines().count() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_line(n: usize) -> String {
        format!(
            r#"{{"type":"user","timestamp":"2026-01-01T00:00:{n:02}Z","message":{{"content":"msg {n}"}}}}"#
        )
    }

    async fn fixture(lines: &[String]) -> std::path::PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        tokio::fs::write(&path, lines.join("\n")).await.unwrap();
        // tempdir 在返回后释放 → 泄漏目录换稳定路径（测试进程生命周期内可接受）
        std::mem::forget(dir);
        path
    }

    #[tokio::test]
    async fn system_meta_user_rows_are_dropped_except_task_notifications() {
        // harness 元注入（skill 注入正文等）原生界面不作对话消息，hub 同样不渲染；
        // task-notification 通知保留（前端按系统块折叠为摘要条）
        let lines = vec![
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Base directory for this skill: /x/skills/brainstorming\n\n# Brainstorming Idea"}}"#.to_string(),
            r#"{"type":"user","message":{"role":"user","content":"<task-notification>\n<task-id>t1</task-id>\n<status>completed</status>\n<summary>后台命令已完成</summary>\n</task-notification>"}}"#.to_string(),
            r#"{"type":"user","message":{"role":"user","content":"用户真说的话"}}"#.to_string(),
        ];
        let path = fixture(&lines).await;
        let msgs = parse_session_file(&path).await.unwrap();
        assert!(msgs
            .iter()
            .all(|m| m.text.as_deref() != Some("Base directory for this skill: /x/skills/brainstorming\n\n# Brainstorming Idea")));
        // 通知与真实用户消息保留
        assert!(msgs.iter().any(|m| m
            .text
            .as_deref()
            .is_some_and(|t| t.contains("<task-notification>"))));
        assert!(msgs
            .iter()
            .any(|m| m.text.as_deref() == Some("用户真说的话")));
    }

    #[tokio::test]
    async fn queued_command_attachment_renders_as_user_message() {
        // 反馈轮 25-B：working 期排队消息被 CLI absorbed_mid_turn 吸收为
        // queued_command 附件（无 user 行）——必须还原为 User 消息，否则 UI 丢失
        let lines = vec![
            user_line(1),
            r#"{"type":"attachment","timestamp":"2026-09-20T15:28:26Z","attachment":{"type":"queued_command","prompt":"R25WORK-1 请只回复OK","timestamp":"2026-09-20T15:26:19.416Z","origin":{"kind":"human"},"humanTurn":true}}"#.to_string(),
            r#"{"type":"attachment","timestamp":"2026-09-20T15:28:26Z","attachment":{"type":"file-history-snapshot","other":1}}"#.to_string(),
        ];
        let path = fixture(&lines).await;
        let msgs = parse_session_file(&path).await.unwrap();
        let queued: Vec<_> = msgs
            .iter()
            .filter(|m| m.raw_type.as_deref() == Some("queued_command"))
            .collect();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].kind, ChatMessageKind::User);
        assert_eq!(queued[0].text.as_deref(), Some("R25WORK-1 请只回复OK"));
        // 其它 attachment 类型仍被忽略
        assert!(msgs
            .iter()
            .all(|m| m.text.as_deref() != Some("{\"other\":1}")));
    }

    #[tokio::test]
    async fn tail_page_returns_latest_messages_with_cursor() {
        let lines: Vec<String> = (0..20).map(user_line).collect();
        let path = fixture(&lines).await;
        let page = read_session_page(&path, 6, None).await.unwrap();
        assert_eq!(page.messages.len(), 6);
        assert_eq!(page.messages[0].text.as_deref(), Some("msg 14"));
        assert_eq!(page.messages[5].text.as_deref(), Some("msg 19"));
        assert_eq!(page.first_line, 14);
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn before_cursor_pages_backwards_until_exhausted() {
        let lines: Vec<String> = (0..20).map(user_line).collect();
        let path = fixture(&lines).await;
        let p1 = read_session_page(&path, 6, None).await.unwrap();
        let p2 = read_session_page(&path, 6, Some(p1.first_line))
            .await
            .unwrap();
        assert_eq!(p2.messages.len(), 6);
        assert_eq!(p2.messages[0].text.as_deref(), Some("msg 8"));
        assert_eq!(p2.first_line, 8);
        assert!(p2.has_more);
        let p3 = read_session_page(&path, 6, Some(p2.first_line))
            .await
            .unwrap();
        // 滑动窗口语义：p3 = 行 2-7 的 6 条；最后一页 p4 = 行 0-1 拉到头
        assert_eq!(p3.messages.len(), 6);
        assert_eq!(p3.messages[0].text.as_deref(), Some("msg 2"));
        assert!(p3.has_more);
        let p4 = read_session_page(&path, 6, Some(p3.first_line))
            .await
            .unwrap();
        assert_eq!(p4.messages.len(), 2);
        assert_eq!(p4.messages[0].text.as_deref(), Some("msg 0"));
        assert!(!p4.has_more);
    }

    #[tokio::test]
    async fn skips_bad_and_non_dialogue_lines() {
        let mut lines: Vec<String> = vec!["not json".into(), r#"{"type":"summary"}"#.into()];
        lines.extend((0..10).map(user_line));
        lines.push(String::new());
        let path = fixture(&lines).await;
        let page = read_session_page(&path, 5, None).await.unwrap();
        assert_eq!(page.messages.len(), 5);
        assert_eq!(page.messages[0].text.as_deref(), Some("msg 5"));
        assert_eq!(page.first_line, 7); // 行 7 = 首条计入消息（0/1 为坏行与非对话行）
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn multi_block_line_truncates_to_exact_limit() {
        // 一行 content 数组展开多条消息：超量时丢头部保尾部，恰好 limit 条
        let wide = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"a"},{"type":"text","text":"b"},{"type":"text","text":"c"}]}}"#.to_string();
        let lines: Vec<String> = (0..3).map(user_line).chain([wide]).collect();
        let path = fixture(&lines).await;
        let page = read_session_page(&path, 4, None).await.unwrap();
        assert_eq!(page.messages.len(), 4);
        assert_eq!(page.messages[0].text.as_deref(), Some("msg 2"));
        assert_eq!(page.messages[3].text.as_deref(), Some("c"));
    }

    #[tokio::test]
    async fn before_zero_or_beyond_file_edge_cases() {
        let lines: Vec<String> = (0..4).map(user_line).collect();
        let path = fixture(&lines).await;
        // before=0 → 空页无更多
        let page = read_session_page(&path, 6, Some(0)).await.unwrap();
        assert!(page.messages.is_empty());
        assert!(!page.has_more);
        // before 超界 clamp 到文件尾 → 等价末页
        let page = read_session_page(&path, 2, Some(999)).await.unwrap();
        assert_eq!(page.messages.len(), 2);
        assert_eq!(page.messages[0].text.as_deref(), Some("msg 2"));
    }

    /// projects/<slug>/<sid>.jsonl 结构的临时目录（session_mtime 按 slug 搜索）。
    async fn mtime_fixture(sid: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("projects").join("proj");
        tokio::fs::create_dir_all(&proj).await.unwrap();
        let path = proj.join(format!("{sid}.jsonl"));
        tokio::fs::write(&path, "init\n").await.unwrap();
        (dir, path)
    }

    #[tokio::test]
    async fn interrupt_baseline_sample_waits_out_late_closeout_write() {
        // r69 竞态回归：中断收尾写入（abort 记录）落盘时刻不定——若基线采样在
        // 收尾写入之前，收尾写入自己就满足「mtime 越过基线且 30s 活跃」解除判据，
        // 中断标记立即解除回落 CLI 滞留的 working（r69 实测 0/N 转闲）。
        // 采样必须等到 jsonl 静默后取值：返回基线 ≥ 收尾写入的 mtime。
        let sid = "baserace-sid";
        let (dir, path) = mtime_fixture(sid).await;
        std::mem::forget(dir); // 同 fixture：泄漏换稳定路径

        // 模拟 CLI 中断收尾：2.2s 后才落盘（晚于旧实现的固定 2s 采样点，
        // 早于真实现的 max(quiet, min_wait)=3.5s 退出点）
        let wpath = path.clone();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
            use tokio::io::AsyncWriteExt;
            let mut f = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&wpath)
                .await
                .unwrap();
            f.write_all(b"closeout\n").await.unwrap();
            // tokio File 带缓冲：不 flush 则 metadata 读到旧 mtime，断言失真
            f.flush().await.unwrap();
            std::fs::metadata(&wpath).unwrap().modified().unwrap()
        });

        let baseline = sample_quiet_mtime(dir_for(&path), sid, 600, 3_500, 8_000)
            .await
            .unwrap();
        let closeout_mtime = writer.await.unwrap();
        assert!(
            baseline >= closeout_mtime,
            "基线必须不早于中断收尾写入的 mtime，否则解除判据被收尾写入自己击穿"
        );
    }

    #[tokio::test]
    async fn interrupt_baseline_sample_breaks_early_on_quiet() {
        // 无新写入：静默阈值一到即返回当前 mtime，不得耗尽 timeout 空等
        let sid = "quiet-sid";
        let (dir, path) = mtime_fixture(sid).await;
        std::mem::forget(dir);
        let start = std::time::Instant::now();
        let baseline = sample_quiet_mtime(dir_for(&path), sid, 500, 0, 30_000)
            .await
            .unwrap();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "静默应提前退出而非耗尽 timeout（实测 {:?}）",
            start.elapsed()
        );
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(baseline, mtime);
    }

    /// 由 jsonl 路径反推 projects 根（tempdir 泄漏后 path 仍有效）。
    /// ancestors()[0] 是路径自身：file→proj→projects→根，取 nth(3)。
    fn dir_for(path: &std::path::Path) -> &std::path::Path {
        path.ancestors().nth(3).unwrap()
    }
}

#[cfg(test)]
mod list_tests {
    use super::*;

    /// `<projects_root>/projects/<slug>/` 目录（session_dir 同构布局）；
    /// slug 由 cwd 编码而来（PROJ_CWD → "-w-proj"），保证枚举路径与 fixture 对齐。
    const PROJ_CWD: &str = "/w/proj";
    const PROJ_SLUG: &str = "-w-proj";

    async fn list_fixture(case: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("hub-sesslist-{}-{}", case, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("projects").join(PROJ_SLUG);
        tokio::fs::create_dir_all(&proj).await.unwrap();
        (dir, proj)
    }

    /// 枚举：只收 .jsonl、mtime 倒序、行内字段（branch/cwd/summary/size）提取正确。
    #[tokio::test]
    async fn lists_jsonl_sorted_by_mtime_desc_with_metadata() {
        let (root, proj) = list_fixture("sorted").await;
        // mtime 显式钉时间戳（文件系统精度不可靠，sleep 间隔在部分卷上会并列）
        let set_mtime = |name: &str, epoch_secs: u64| {
            let f = std::fs::File::options()
                .append(true)
                .open(proj.join(name))
                .unwrap();
            f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(epoch_secs))
                .unwrap();
        };
        // 旧会话（summary 行在头）→ 早写 + 早 mtime
        tokio::fs::write(
            proj.join("aaaa-old.jsonl"),
            concat!(
                r#"{"type":"summary","summary":"Old session about widgets","leafUuid":"x"}"#, "\n",
                r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","cwd":"/w/old","gitBranch":"main","message":{"content":"hi"}}"#,
            ),
        )
        .await
        .unwrap();
        // 新会话（无 summary 行 → 首条 user 消息兜底）
        tokio::fs::write(
            proj.join("bbbb-new.jsonl"),
            concat!(
                r#"{"type":"user","timestamp":"2026-01-02T00:00:00Z","cwd":"/w/new","gitBranch":"feat/x","message":{"content":"修复登录页"}}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":"好的"}}"#,
            ),
        )
        .await
        .unwrap();
        // 噪声条目：非 jsonl 文件 + 同名目录 + 空名防御不适用（文件系统保证有 stem），
        // 都必须被排除
        tokio::fs::write(proj.join("notes.txt"), "x").await.unwrap();
        tokio::fs::create_dir_all(proj.join("cccd-dir.jsonl"))
            .await
            .unwrap();
        set_mtime("aaaa-old.jsonl", 1_000_000);
        set_mtime("bbbb-new.jsonl", 1_000_060);

        let list = list_project_sessions(&root, Path::new(PROJ_CWD)).await;

        assert_eq!(list.len(), 2, "非 jsonl 文件与同名目录必须排除");
        assert_eq!(list[0].session_id, "bbbb-new");
        assert_eq!(list[0].branch.as_deref(), Some("feat/x"));
        assert_eq!(list[0].cwd.as_deref(), Some("/w/new"));
        assert_eq!(list[0].summary.as_deref(), Some("修复登录页"));
        assert!(list[0].size_bytes > 0);
        assert!(list[0].mtime_secs >= list[1].mtime_secs, "mtime 倒序");
        assert_eq!(list[1].session_id, "aaaa-old");
        assert_eq!(
            list[1].summary.as_deref(),
            Some("Old session about widgets"),
            "summary 行优先于首条用户消息"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// slug 编码实测口径回归：`/` 与 `.` 一律映射 `-`（含 `..` 分量被一并编码成
    /// `--`，不存在目录穿越形态）；空 cwd → None。
    #[tokio::test]
    async fn session_dir_encoding_matches_recorded_slug_rules() {
        let root = Path::new("/root/.claude");
        let dir = session_dir(root, Path::new("/Users/kevin/.claude/jobs/aa14d3e3-tmp")).unwrap();
        assert_eq!(
            dir,
            root.join("projects/-Users-kevin--claude-jobs-aa14d3e3-tmp"),
            "佐证样本：session.rs 头注释 2026-09-16 实测记录"
        );
        // `..` 分量被编码，不产生 ParentDir 分量（防穿越）；整段编码进单层 slug
        let dir = session_dir(root, Path::new("/a/../../etc")).unwrap();
        assert!(!dir
            .components()
            .any(|c| c == std::path::Component::ParentDir));
        assert_eq!(
            dir.parent().unwrap(),
            root.join("projects"),
            "必须仍是 projects 下的单层 slug 目录"
        );
        assert!(dir.to_str().unwrap().ends_with("etc"));
        // 空 cwd → None（调用方空集语义）
        assert!(session_dir(root, Path::new("")).is_none());
    }

    /// 目录缺失 / cwd 空：空集而非错误（前端空态语义）。
    #[tokio::test]
    async fn missing_dir_and_empty_cwd_return_empty_list() {
        let root =
            std::env::temp_dir().join(format!("hub-sesslist-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(list_project_sessions(&root, Path::new("/no/such/proj"))
            .await
            .is_empty());
        assert!(list_project_sessions(&root, Path::new("")).await.is_empty());
    }

    /// 摘要兜底：无 summary 行取首条非 isMeta user 文本并截 120 chars（chars 边界
    /// 安全，不 panic）；isMeta（含 task-notification）不作摘要。
    #[tokio::test]
    async fn summary_falls_back_to_first_human_user_message_truncated() {
        let (root, proj) = list_fixture("summary").await;
        let long = "长".repeat(300);
        let meta_line =
            r#"{"type":"user","isMeta":true,"message":{"content":"<task-notification>注入正文"}}"#;
        let user_line = format!(
            r#"{{"type":"user","message":{{"content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        tokio::fs::write(proj.join("summ.jsonl"), format!("{meta_line}\n{user_line}"))
            .await
            .unwrap();
        let list = list_project_sessions(&root, Path::new("/w/proj")).await;
        assert_eq!(list.len(), 1);
        let s = list[0].summary.as_deref().unwrap();
        assert_eq!(s.chars().count(), 121, "120 chars + …");
        assert!(s.ends_with('…'));
        assert!(!s.contains("task-notification"));
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    /// fixture 即真实转录形态片段：最后一条 assistant usage 决定 context。
    #[tokio::test]
    async fn extracts_last_usage_sum_from_tail() {
        let dir = std::env::temp_dir().join(format!("hub-ctx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("projects").join("-tmp-ctx");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("sess-ctx.jsonl"),
            concat!(
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
                "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":100,\"cache_creation_input_tokens\":5}}}\n",
                "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"usage\":{\"input_tokens\":1224,\"cache_read_input_tokens\":458880,\"cache_creation_input_tokens\":0,\"output_tokens\":216}}}\n",
            ),
        )
        .unwrap();
        // 取的是最后一条：input 1224 + cache_read 458880 = 460104（output 不计窗口）
        assert_eq!(
            last_context_tokens(&dir, "sess-ctx").await,
            Some(1224 + 458880)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn none_when_no_usage_or_missing_file() {
        let dir = std::env::temp_dir().join(format!("hub-ctx-e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let proj = dir.join("projects").join("-tmp-ctx");
        std::fs::create_dir_all(&proj).unwrap();
        // 转录存在但无 usage → None
        std::fs::write(proj.join("empty.jsonl"), "{\"type\":\"user\"}\n").unwrap();
        assert_eq!(last_context_tokens(&dir, "empty").await, None);
        // 转录不存在 → None
        assert_eq!(last_context_tokens(&dir, "no-file").await, None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
