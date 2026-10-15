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
    /// 恢复目标 session id（agent-hub-session-history）：`--bg --resume <id>`。
    /// 实测（2026-10-15）：prompt 可省（空串即不尾随）；CLI 对活会话自动走
    /// 「start a copy」分支、死会话走「continues under the same ID」。恢复路径
    /// 不传 --name（CLI 自动带原会话 saved options，见 woke 输出实测）。
    pub resume: Option<String>,
}

/// 启动参数构造（纯函数便于单测）：`--bg` 打头 → `--resume`（恢复）→
/// `--name` → `--model` → `--effort` → prompt 尾随（空 = 不追加）。
fn build_start_args(req: &StartReq) -> Vec<String> {
    let mut args: Vec<String> = vec!["--bg".into()];
    if let Some(sid) = &req.resume {
        args.push("--resume".into());
        args.push(sid.clone());
    }
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
    if !req.prompt.is_empty() {
        args.push(req.prompt.clone());
    }
    args
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

/// interactive 前台会话合成稳定 id（R7 会话粘连修复）：用户在终端手跑的
/// `claude`（kind="interactive"）不在 daemon 清单，CLI 返回无 id（serde default
/// 空串）——多条空 id 在前端选中键 `${instanceId}:${id}` 上塌陷为同键，两行恒
/// 同选中、点选无效（用户实测「粘在一起选不中」）。合成 `itv-<sessionId 前 12>`：
/// 同会话稳定（转录定位照常），`claude attach itv-*` 天然失效（interactive 不经
/// hub 管理，输入通道由前端按 kind 禁用，R7 验收）。
pub fn synthesize_interactive_id(session_id: &str) -> String {
    // chars 迭代防非 ASCII sessionId 字节边界 panic（r-review MINOR-1；sessionId
    // 现实为 UUID/ASCII，此处纯防御）
    let taken: String = session_id.chars().take(12).collect();
    format!("itv-{taken}")
}

/// interactive「从列表移除」过滤（R8）：忽略清单中的 sessionId 命中 interactive
/// 条目（无 daemon id）则隐藏；同时懒清理——清单里的 sessionId 若已不在本轮
/// CLI 清单任何条目中 = 底层进程消失、条目自然死亡，忽略记录一并清除（retain）。
/// 纯函数便于单测；文件 IO 由调用方承担。
pub fn filter_ignored(
    base: Vec<CliOutput>,
    ignored: &mut std::collections::HashSet<String>,
) -> Vec<CliOutput> {
    let alive: std::collections::HashSet<String> = base
        .iter()
        .filter_map(|c| c.session_id.clone())
        .filter(|s| !s.is_empty())
        .collect();
    ignored.retain(|sid| alive.contains(sid));
    base.into_iter()
        .filter(|c| {
            !(c.id.is_empty() && c.session_id.as_deref().is_some_and(|s| ignored.contains(s)))
        })
        .collect()
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
    let args = build_start_args(req);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run(bin, &refs, Some(cwd), START_TIMEOUT).await?;
    let id = extract_bg_id(&out);
    if id.is_empty() {
        bail!("claude --bg 未返回任务 id");
    }
    Ok(id)
}

/// 去 ANSI 转义序列（`\x1b[` 参数字节…终止字母）：新 CLI `--bg` 输出带色码
/// （如 "backgrounded · \x1b[36m<id>\x1b[39m"），不剥会让整行（含转义字节）
/// 被当成任务 id（driver-unify 批3 修复）。
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            // 参数字节后必有终止字母（0x40–0x7E），吞到即止
            for n in chars.by_ref() {
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 从 `claude --bg` stdout 首行提取任务 id：strip ANSI 后取最后一个非空 token，
/// 全 hex 且 ≤16 位才认作 id；否则回退整行 trim（兼容旧 CLI 纯 id 输出与 fixture）。
pub fn parse_bg_id(line: &str) -> String {
    let clean = strip_ansi(line);
    let line = clean.trim();
    if let Some(tok) = line.rsplit([' ', '\t', '·']).find(|t| !t.is_empty()) {
        if tok.len() <= 16 && tok.chars().all(|c| c.is_ascii_hexdigit()) {
            return tok.to_string();
        }
    }
    line.to_string()
}

/// 短 hex id 判据（parse_bg_id 同口径）：非空、≤16 位、全 ASCII hex。
fn is_hex_id(tok: &str) -> bool {
    !tok.is_empty() && tok.len() <= 16 && tok.chars().all(|c| c.is_ascii_hexdigit())
}

/// 从 `claude --bg` 全量 stdout 提取任务 id（agent-hub-session-history 扩展）。
/// 实测（2026-10-15）resume 输出首行是 note 行、backgrounded 行在后：
/// - 死会话：`note: woke session <sid> with its saved options (--name, --model).`
///   + `backgrounded · <id> · <name> (idle — send a prompt to start)`（bg id = sid 前 8）
/// - 活会话：`note: session <sid> is already running ..., so this started a copy as <id>.`
///   + `backgrounded · <id> (idle — ...)`（hub 侧重复保护应先拦住此分支，竞态窗口才到达）
///
/// 现有 parse_bg_id 对这些形态取首行必错（note 行 / 尾 token 非 hex 回退整行）。
///
/// 策略：逐行倒序找 backgrounded 行，strip ANSI 后按 `·`/空白切 token 取首个
/// 短 hex（`· <id> · <name>` 与 `· <id> (idle…)` 两种排布都命中 id）；无
/// backgrounded 行回退首行 parse_bg_id（旧 CLI / fixture 兼容）。
pub fn extract_bg_id(stdout: &str) -> String {
    for line in stdout.lines().rev() {
        if !line.contains("backgrounded") {
            continue;
        }
        let clean = strip_ansi(line);
        if let Some(tok) = clean.split(['·', ' ', '\t']).find(|t| is_hex_id(t)) {
            return tok.to_string();
        }
    }
    stdout.lines().next().map(parse_bg_id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::parse_bg_id;

    /// driver-unify 批3：新 CLI --bg 输出带 ANSI 色码的 id 行（实测 2026-09-29）。
    #[test]
    fn parses_id_from_ansi_decorated_line() {
        assert_eq!(
            parse_bg_id("backgrounded · \x1b[36m92d38ecf\x1b[39m"),
            "92d38ecf"
        );
        // 多色码变体（reset 用 [0m）
        assert_eq!(
            parse_bg_id("\x1b[2mlaunching\x1b[22m \x1b[32mab12cd34\x1b[0m"),
            "ab12cd34"
        );
    }

    // —— agent-hub-session-history：启动参数构造与 resume 输出解析 ——

    use super::{build_start_args, extract_bg_id, StartReq};

    fn req(prompt: &str, name: Option<&str>, resume: Option<&str>) -> StartReq {
        StartReq {
            prompt: prompt.into(),
            name: name.map(Into::into),
            model: None,
            effort: None,
            resume: resume.map(Into::into),
        }
    }

    /// 恢复参数构造：`--resume <sid>` 紧随 `--bg`；不传 --name（CLI 自动带原会话
    /// saved options，且规避 #10 的 --name 解析错位）；空 prompt 不尾随。
    #[test]
    fn resume_args_carry_resume_without_name_or_prompt() {
        let args = build_start_args(&req("", None, Some("15c9e382-1658-4703-bdcb-2d236739943f")));
        assert_eq!(
            args,
            vec![
                "--bg".to_string(),
                "--resume".to_string(),
                "15c9e382-1658-4703-bdcb-2d236739943f".to_string()
            ]
        );
    }

    /// 普通 start 参数序保持原行为：--bg [--name] [--model] [--effort] prompt。
    #[test]
    fn plain_start_args_keep_legacy_order() {
        let mut r = req("hello", Some("myname"), None);
        r.model = Some("opus".into());
        r.effort = Some("high".into());
        let args = build_start_args(&r);
        assert_eq!(
            args,
            vec![
                "--bg".to_string(),
                "--name".to_string(),
                "myname".to_string(),
                "--model".to_string(),
                "opus".to_string(),
                "--effort".to_string(),
                "high".to_string(),
                "hello".to_string()
            ]
        );
    }

    /// 死会话 resume 实测形态（2026-10-15）：首行 note（woke），backgrounded 行
    /// 在后且带 `· <name> (idle — …)` 尾巴——提取必须命中 bg id（= sid 前 8），
    /// 不得被 note 行或非 hex 尾 token 带偏。
    #[test]
    fn extract_id_from_woke_resume_output() {
        let out = concat!(
            "note: woke session e5b57ea3-986f-4bd6-bba7-57e0eb7c6b84 with its saved options (--name, --model).\n",
            "backgrounded \u{b7} e5b57ea3 \u{b7} hub-test-sesshist-probe2 (idle \u{2014} send a prompt to start)\n",
            "  claude agents             list sessions\n",
        );
        assert_eq!(extract_bg_id(out), "e5b57ea3");
    }

    /// 活会话 resume 实测形态（copy 分支）：note 行里也出现 8hex（as <id>.），
    /// 但提取只认 backgrounded 行——两行都含目标 id，结果一致不歧义。
    #[test]
    fn extract_id_from_copy_resume_output() {
        let out = concat!(
            "note: session 15c9e382-1658-4703-bdcb-2d236739943f is already running in the background, so this started a copy as a2b4de6c. `claude attach 15c9e382` opens the original.\n",
            "backgrounded \u{b7} a2b4de6c (idle \u{2014} send a prompt to start)\n",
        );
        assert_eq!(extract_bg_id(out), "a2b4de6c");
    }

    /// 普通 start 新形态（ANSI 色码 backgrounded 行）与旧形态（首行纯 id）兼容。
    #[test]
    fn extract_id_falls_back_to_legacy_first_line() {
        assert_eq!(
            extract_bg_id("backgrounded \u{b7} \x1b[36m92d38ecf\x1b[39m\n  claude agents ...\n"),
            "92d38ecf"
        );
        assert_eq!(extract_bg_id("abc123\n"), "abc123");
        // 无任何可解析行 → 空串（start_bg bail）
        assert_eq!(extract_bg_id(""), "");
    }

    /// 旧 CLI / fixture 输出纯 id，行为不变。
    #[test]
    fn keeps_plain_id_line() {
        assert_eq!(parse_bg_id("abc123"), "abc123");
        assert_eq!(parse_bg_id("  abc123  "), "abc123");
    }

    /// 非 hex 尾 token：回退整行 trim（不猜 id，保守兼容未知输出形态）。
    #[test]
    fn falls_back_to_whole_line_when_tail_not_hex() {
        assert_eq!(parse_bg_id("job created: x9y"), "job created: x9y");
    }

    /// R7 会话粘连：interactive 合成 id 稳定且经 validate_id 白名单（字母数字-_）。
    #[test]
    fn synthesizes_interactive_id_stable_and_whitelisted() {
        let sid = "839daca8-a398-48b6-9ba2-5a0dec0daae1";
        let id = super::synthesize_interactive_id(sid);
        assert_eq!(id, "itv-839daca8-a39");
        assert_eq!(id, super::synthesize_interactive_id(sid)); // 同会话稳定
                                                               // 白名单（attach::validate_id 同判据：字母数字-_;本地断言免跨模块依赖）
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        // 短 session id：取全量不 panic
        assert_eq!(super::synthesize_interactive_id("abc"), "itv-abc");
        // 非 ASCII：chars 迭代不 panic（MINOR-1 防御点；interactive 输入通道已禁用，
        // 含非 ASCII 的合成 id 不再进入 attach 路径）
        let non_ascii = super::synthesize_interactive_id("会话-segment-839daca8");
        assert!(non_ascii.starts_with("itv-"));
        assert_eq!(
            non_ascii,
            super::synthesize_interactive_id("会话-segment-839daca8")
        );
    }

    /// R8 忽略清单生命周期：命中隐藏；进程消失后忽略记录懒清理；bg 条目不受影响。
    #[test]
    fn filter_ignored_hides_and_lazily_cleans() {
        use super::CliOutput;
        use std::collections::HashSet;
        let mk = |id: &str, sid: Option<&str>| CliOutput {
            id: id.into(),
            session_id: sid.map(Into::into),
            ..Default::default()
        };
        let base = vec![
            mk("", Some("sid-a")), // interactive（命中忽略 → 隐藏）
            mk("bg-1", Some("sid-bg")), // bg 条目不受影响
                                   // sid-dead 不在 base = 进程已消失（其忽略记录本轮被懒清理）
        ];
        let mut ignored: HashSet<String> = ["sid-a".to_string(), "sid-dead".to_string()]
            .into_iter()
            .collect();
        let out = super::filter_ignored(base, &mut ignored);
        // sid-a 命中隐藏；sid-dead 进程消失 → 记录清理；bg 保留
        assert_eq!(out.len(), 1);
        assert!(out
            .iter()
            .any(|c| c.session_id.as_deref() == Some("sid-bg")));
        assert!(!out.iter().any(|c| c.session_id.as_deref() == Some("sid-a")));
        assert!(!ignored.contains("sid-dead"));
        assert!(ignored.contains("sid-a"));
        // 再一轮（sid-a 进程也消失）→ 记录清空
        let base2 = vec![mk("bg-1", Some("sid-bg"))];
        let out2 = super::filter_ignored(base2, &mut ignored);
        assert_eq!(out2.len(), 1);
        assert!(ignored.is_empty());
    }
}
