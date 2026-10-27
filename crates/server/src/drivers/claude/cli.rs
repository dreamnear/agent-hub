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
    /// 会话启动期注入：`--mcp-config <file>`（B2，claude 实测可行）。为纯加法
    ///（不用 `--strict-mcp-config`），文件由 hub 在会话侧生成（不写 `~/.claude`）。
    pub mcp_config: Option<String>,
}

/// 启动参数构造（纯函数便于单测）：`--bg` 打头 → `--resume`（恢复）→
/// `--name` → `--model` → `--effort` → prompt 尾随（空 = 不追加）。
/// 当 `mcp_config` 提供时追加 `--mcp-config <file>`（位置在其它开关之后、prompt 之前）。
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
    if let Some(cfg) = &req.mcp_config {
        // 等号连写（agent-hub#48）：CLI 2.1.309 的空格分隔形态会把尾随的位置
        // 参数 prompt 再吞成该开关的第二个值（按 cwd 相对路径解析 → "Invalid
        // MCP configuration: MCP config file not found: <cwd>/<prompt>"，init 即
        // 崩、daemon 无限 claimed-spare→crashed 重试）。等号连写实测正常；
        // prompt 保持尾随位置参数不动（避免 prompt 以 `-` 开头被误解析的边界）。
        args.push(format!("--mcp-config={cfg}"));
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

/// `claude --bg` 成功行的结构锚点（claude 2.1.300 实测，2026-10-15）。
const BG_MARKER: &str = "backgrounded";

/// 整行即单一 token（无分隔符）——旧 CLI 纯 id 行形态。
fn is_single_token(line: &str) -> bool {
    !line.contains([' ', '\t', '\u{b7}'])
}

/// 按 `·`/空白切 token，取**首个**短 hex id：id 恒紧跟 `backgrounded ·`，
/// 名字/提示语即便含 hex 形 token 也在 id 之后，故首个命中即 id。
fn first_hex_token(s: &str) -> Option<&str> {
    s.split(['\u{b7}', ' ', '\t']).find(|t| is_hex_id(t))
}

/// 从 `claude --bg` stdout 首行提取任务 id（agent-hub#10 修复）。
///
/// 输出形态（claude 2.1.300 实测，2026-10-15 字节级）：
/// - 带名：`backgrounded \xc2\xb7 <id> \xc2\xb7 <name>`（+ 4 行提示尾巴）
/// - 不带名：`backgrounded \xc2\xb7 <id>`
/// - legacy：整行即纯 id（旧 CLI / fixture）
///
/// **按结构锚定，禁位置切片**：旧实现取**末** token（`--name X` 时末 token 是
/// 名字非 hex）→ 回退整行 → 含空格整行被当 id（下游 `/message` http 000）。
/// 现按 `backgrounded` 标记锚定其后首个短 hex token；无标记行则在整行找首个
/// 短 hex token；整行单一 token 原样返回（legacy 纯 id，任意字符集/长度）。
///
/// 取不到 id **明确失败**：返回空串 → `start_bg` bail 报错。绝不回退整行把
/// 不可用 id 静默交给下游（#10 根因：坏 id 只会伪装成难查的下游错误）。
pub fn parse_bg_id(line: &str) -> String {
    let clean = strip_ansi(line);
    let line = clean.trim();
    if line.is_empty() {
        return String::new();
    }
    // 锚点一：`backgrounded` 标记之后（当前 CLI 形态；标记后仍无短 hex = 格式
    // 变更 → 明确失败，不做整行兜底）
    if let Some(rest) = line.find(BG_MARKER).map(|p| &line[p + BG_MARKER.len()..]) {
        return first_hex_token(rest).unwrap_or_default().to_string();
    }
    // 锚点二：无标记行的首个短 hex token
    if let Some(tok) = first_hex_token(line) {
        return tok.to_string();
    }
    // 兜底：整行即单一 token（旧 CLI 纯 id）→ 原样；多 token 却取不到 id → 失败
    if is_single_token(line) {
        return line.to_string();
    }
    String::new()
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
            mcp_config: None,
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

    /// 回归锁（agent-hub#48）：`--mcp-config` 必须走 `=` 连写——CLI 2.1.309 的
    /// 空格分隔形态会把尾随位置参数 prompt 吞成该开关的第二个值（init 即崩）。
    /// prompt 恒为尾随位置参数。
    #[test]
    fn mcp_config_uses_eq_form_prompt_trails() {
        let mut r = req("hello", None, None);
        r.mcp_config = Some("/tmp/plugin_mcp/sess.json".into());
        assert_eq!(
            build_start_args(&r),
            vec![
                "--bg".to_string(),
                "--mcp-config=/tmp/plugin_mcp/sess.json".to_string(),
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

    /// agent-hub#10 主修复：`claude --bg --name X` 实测输出（claude 2.1.300，
    /// 2026-10-15 实测字节 `backgrounded \xc2\xb7 <id> \xc2\xb7 <name>`）——
    /// 末 token 是**名字**而非 hex，旧「取末 token，否则回退整行」逻辑把整行
    /// 当 id（下游 /message 拿含空格 id，http 000）。
    #[test]
    fn parses_id_from_named_backgrounded_line() {
        assert_eq!(
            parse_bg_id("backgrounded \u{b7} 96bad8a7 \u{b7} hub-test-fs-23837"),
            "96bad8a7"
        );
        // 名字恰为 hex 形（旧逻辑取末 token 会直接返回名字本身）
        assert_eq!(
            parse_bg_id("backgrounded \u{b7} 0ad5b1ac \u{b7} dead"),
            "0ad5b1ac"
        );
        assert_eq!(
            parse_bg_id("backgrounded \u{b7} 0ad5b1ac \u{b7} 123"),
            "0ad5b1ac"
        );
        // 数字开头 / 全数字名字（hexdigit 子集）
        assert_eq!(
            parse_bg_id("backgrounded \u{b7} 12ab34cd \u{b7} 42"),
            "12ab34cd"
        );
    }

    /// 结构锚定而非位置切片：ANSI 色码、额外 token、尾随提示语、顺序变化、
    /// 无 `backgrounded` 标记的变体都只认「首个短 hex token」。
    #[test]
    fn anchors_on_structure_not_token_position() {
        // 带名 + ANSI 色码（真机两特征叠加）
        assert_eq!(
            parse_bg_id(
                "\x1b[36mbackgrounded\x1b[39m \u{b7} \x1b[36m92d38ecf\x1b[39m \u{b7} \x1b[2mhub-test-x\x1b[22m"
            ),
            "92d38ecf"
        );
        // 尾随 (idle — …) 提示语
        assert_eq!(
            parse_bg_id(
                "backgrounded \u{b7} 96bad8a7 \u{b7} hub-test-x (idle \u{2014} send a prompt to start)"
            ),
            "96bad8a7"
        );
        // 额外 token 混入（提示词回声等）
        assert_eq!(
            parse_bg_id("backgrounded \u{b7} 96bad8a7 \u{b7} hub-test-x \u{b7} say ok"),
            "96bad8a7"
        );
        // 无标记变体：id 位置变化仍按 hex 判据命中
        assert_eq!(parse_bg_id("note: started as abc12345"), "abc12345");
        assert_eq!(parse_bg_id("backgrounded \u{b7} ab12cd34"), "ab12cd34");
    }

    /// 解析失败必须明确（空串 → `start_bg` bail 报错），禁回退整行——含空格的
    /// 整行是不可用 id，静默交给下游只会变成 http 000 一类难查错误（#10 根因）。
    /// 例外：整行即单一 token 的旧 CLI 纯 id 形态原样保留（任意字符集/长度）。
    #[test]
    fn refuses_unusable_id_instead_of_falling_back_to_whole_line() {
        assert_eq!(parse_bg_id("job created: x9y"), "");
        assert_eq!(
            parse_bg_id("backgrounded \u{b7} not-hex \u{b7} also-not"),
            ""
        );
        assert_eq!(parse_bg_id("backgrounded"), "");
        assert_eq!(parse_bg_id(""), "");
        assert_eq!(parse_bg_id("   "), "");
        // 单 token 行：整行即 id（旧 CLI 形态 / 非 hex 长 id）
        assert_eq!(parse_bg_id("job-9f8e7d6c5b4a3928"), "job-9f8e7d6c5b4a3928");
        assert_eq!(
            parse_bg_id("  job-9f8e7d6c5b4a3928  "),
            "job-9f8e7d6c5b4a3928"
        );
    }

    /// #10 端到端（函数级）：真实 `claude --bg --name X` 全量 stdout（claude
    /// 2.1.300，2026-10-15 实测 5 行）→ `start_bg` 所用的 extract_bg_id 返回
    /// 短 hex id；不带名形态同批实测。（带名/不带名的 id 均须可直接用于
    /// logs/attach/stop/rm。）
    #[test]
    fn extract_id_from_real_named_start_output() {
        let out = concat!(
            "backgrounded \u{b7} 96bad8a7 \u{b7} hub-test-fs-23837\n",
            "  claude agents             list sessions\n",
            "  claude attach 96bad8a7    open in this terminal\n",
            "  claude logs 96bad8a7      show recent output\n",
            "  claude stop 96bad8a7      stop this session\n",
        );
        assert_eq!(extract_bg_id(out), "96bad8a7");
        assert_eq!(
            extract_bg_id("backgrounded \u{b7} 96bad8a7\n  claude agents ...\n"),
            "96bad8a7"
        );
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
