//! 分发面（B4，ADR-0008 §2/§3）：git 仓库直装 + `marketplace.json` 索引市场。
//!
//! - git 直装：clone（临时目录）→ 定位 plugin.json → 审计 → 落盘插件目录 →
//!   走既有「新发现/更新待确认」确认流（可审计态 = 落盘即进扫描面，安装仍须
//!   用户显式确认）。hub 不代持 git 凭据（v1 不支持私有仓库认证，ADR-0008 拍板 5）。
//! - 索引市场：用户显式添加索引源（git 仓库或本地文件，内含 marketplace.json）；
//!   hub 只做检索/浏览/安装触发，不自建 registry、不托管插件代码。
//! - 明确不做（ADR-0008 §4）：归档包、签名、自动更新、插件互依赖。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::audit::{self, AuditReport};
use super::PluginManager;

/// clone 超时（首个主动出站行为的护栏；ADR-0008 §2「网络行为需定义超时」）
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// `git --version` 探测超时
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// git 直装请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInstallRequest {
    /// 仓库 URL（https:// 或本地路径；可带 `#<ref>` 指定 tag/branch/commit）
    pub repo_url: String,
    /// 仓库内子目录（插件在仓库根时省略；禁 `..` / 绝对路径）
    #[serde(default)]
    pub sub_path: Option<String>,
}

/// git 直装结果：插件已落盘进扫描面，等待既有确认流（new / update / audit_failed）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInstallOutcome {
    pub id: String,
    pub path: String,
    /// `new` | `update` | `already_installed` | `audit_failed`
    pub status: String,
    pub report: AuditReport,
    /// 指定过的 ref（原样回显，追溯用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

/// URL 中的凭据脱敏（git 错误信息可能带 `https://user:token@host` 形态；
/// 密钥零回显红线——除 credentials.scrub 外再做形态级打码）。
fn redact_url_credentials(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("://") {
        let scheme_start = rest[..pos].rfind(|c: char| c.is_whitespace() || c == '"').map(|p| p + 1).unwrap_or(0);
        let prefix = &rest[scheme_start..pos];
        if prefix.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
            let after = &rest[pos + 3..];
            if let Some(at) = after.find('@') {
                let host_start = after[..at].find('/').unwrap_or(at);
                if host_start == at {
                    // `://user:pass@host` 形态（@ 先于首个 /）
                    out.push_str(&rest[..pos + 3]);
                    out.push_str("<redacted>@");
                    rest = &after[at + 1..];
                    continue;
                }
            }
        }
        out.push_str(&rest[..pos + 3]);
        rest = &rest[pos + 3..];
    }
    out.push_str(rest);
    out
}

/// 解析 `url#ref`（ADR-0008 §2：# 指定 tag / branch / commit；缺省 = 默认分支 head）。
fn split_git_ref(repo_url: &str) -> (String, Option<String>) {
    match repo_url.rsplit_once('#') {
        Some((url, git_ref)) if !git_ref.trim().is_empty() => {
            (url.trim().to_string(), Some(git_ref.trim().to_string()))
        }
        _ => (repo_url.trim().to_string(), None),
    }
}

/// git URL/源合法性：v1 只接受 https:// 与本机路径（ADR-0008 拍板 5：不支持
/// 私有仓库认证；ssh 形态依赖 agent/密钥链，同样不进 v1 面）。
pub fn valid_git_source(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if url.starts_with("http://") {
        // 明网明传仅本机路径例外之外的显式拒绝：防误把 token 明文送去网络
        return Err("http:// 明文远端不受支持（请用 https:// 或本机路径）".into());
    }
    if url.contains("://") {
        return Err(format!(
            "不支持的源形态 {url:?}（v1 支持 https:// 与本机路径；ssh/私有认证不进 v1）"
        ));
    }
    let p = Path::new(url);
    if !p.is_absolute() {
        return Err(format!(
            "本机源必须是绝对路径（当前 {url:?}）；相对路径无法定位仓库"
        ));
    }
    Ok(())
}

/// ref 是否是 commit sha（7–40 hex → 全量 clone + checkout；其余按 branch/tag 走浅克隆重定向）。
fn is_commit_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 运行 git 命令（超时 + 输出收集 + 失败清理由调用方负责）。
async fn run_git(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args).stdin(std::process::Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "git 命令超时".to_string())?
        .map_err(|e| format!("git 启动失败：{e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = if stderr.trim().is_empty() {
            String::from_utf8_lossy(&out.stdout).into_owned()
        } else {
            stderr.into_owned()
        };
        return Err(redact_url_credentials(combined.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// git 二进制探测（ADR-0008 后果节：明确错因而非半途失败）。
pub async fn probe_git() -> Result<String, String> {
    tokio::time::timeout(GIT_PROBE_TIMEOUT, async {
        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("--version").stdin(std::process::Stdio::null());
        let out = cmd
            .output()
            .await
            .map_err(|_| "git 未安装或不在 PATH（git 直装依赖本机 git）".to_string())?;
        if !out.status.success() {
            return Err("git --version 失败（本机 git 不可用）".into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    })
    .await
    .unwrap_or_else(|_| Err("git 探测超时".into()))
}

/// 在仓库工作副本内定位插件根：`<root>/<sub_path>/plugin.json` 或 `<root>/plugin.json`。
fn locate_plugin_root(work: &Path, sub_path: Option<&str>) -> Result<PathBuf, String> {
    let base = match sub_path {
        Some(sp) => {
            let p = Path::new(sp);
            if p.is_absolute() || sp.contains("..") {
                return Err(format!("subPath 不合法：{sp:?}（禁绝对路径与 ..）"));
            }
            work.join(p)
        }
        None => work.to_path_buf(),
    };
    if base.join("plugin.json").is_file() {
        return Ok(base);
    }
    Err(format!(
        "未在 {} 找到 plugin.json（确认仓库/子目录是 hub 插件形态）",
        base.display()
    ))
}

/// git 直装主流程：clone → 定位 → 审计 → 落盘 → 扫描进确认面。
pub async fn git_install(
    manager: &Arc<PluginManager>,
    req: &GitInstallRequest,
) -> Result<GitInstallOutcome, String> {
    probe_git().await?;
    let (url, git_ref) = split_git_ref(&req.repo_url);
    valid_git_source(&url)?;
    let work = std::env::temp_dir().join(format!("hub-plugin-install-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&work);
    // 失败/ panic 两路都清理临时目录（首个人站行为的卫生纪律）
    let result = git_install_inner(manager, &url, git_ref.clone(), req.sub_path.as_deref(), &work).await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

async fn git_install_inner(
    manager: &Arc<PluginManager>,
    url: &str,
    git_ref: Option<String>,
    sub_path: Option<&str>,
    work: &Path,
) -> Result<GitInstallOutcome, String> {
    match git_ref.as_deref() {
        Some(r) if is_commit_sha(r) => {
            run_git(&["clone", "--quiet", url, work.to_string_lossy().as_ref()], None).await?;
            run_git(&["checkout", "--quiet", r], Some(work)).await?;
        }
        Some(r) => {
            run_git(
                &[
                    "clone",
                    "--quiet",
                    "--depth",
                    "1",
                    "--single-branch",
                    "--branch",
                    r,
                    url,
                    work.to_string_lossy().as_ref(),
                ],
                None,
            )
            .await?;
        }
        None => {
            run_git(
                &["clone", "--quiet", "--depth", "1", url, work.to_string_lossy().as_ref()],
                None,
            )
            .await?;
        }
    }
    let plugin_root = locate_plugin_root(work, sub_path)?;
    let report = audit::audit_dir(&plugin_root, &manager.credentials);
    if report.plugin_id.is_empty() {
        let first = report
            .errors
            .first()
            .map(|e| format!("{}：{}", e.field, e.message))
            .unwrap_or_else(|| "manifest 不可解析".into());
        return Err(format!("git 直装校验失败：{first}"));
    }
    let id = report.plugin_id.clone();
    // 落盘：plugins/<id>/（已存在时替换内容——旧版已由安装快照保底，替换后
    // 扫描把它呈递为「更新待确认」，确认与否都走 BR-13 流）
    let live = manager.paths.plugins_dir.join(&id);
    std::fs::create_dir_all(&live).map_err(|e| format!("插件目录创建失败：{e}"))?;
    let current_hash = audit::content_hash(&live);
    let already = current_hash == report.content_hash && live.join("plugin.json").is_file();
    if !already {
        std::fs::remove_dir_all(&live).map_err(|e| format!("旧目录清理失败：{e}"))?;
        super::copy_dir_recursive(&plugin_root, &live)
            .map_err(|e| format!("落盘失败：{e}"))?;
    }
    manager.scan_now();
    let status = if !report.errors.is_empty() {
        "audit_failed"
    } else if already {
        "already_installed"
    } else if manager.registry.contains(&id) {
        "update"
    } else {
        "new"
    };
    Ok(GitInstallOutcome {
        id,
        path: live.to_string_lossy().into_owned(),
        status: status.into(),
        report,
        git_ref,
    })
}

// ===== 索引市场（ADR-0008 §3）=====

/// marketplace.json 单条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MarketEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub repo_url: String,
    #[serde(default)]
    pub sub_path: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// marketplace.json 文档（仓库/文件内）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketIndex {
    #[serde(default)]
    pub name: Option<String>,
    pub plugins: Vec<MarketEntry>,
}

/// 一个索引源的呈现态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSourceView {
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub plugin_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 索引源清单持久态（`plugin_marketplaces.json`；用户显式添加，无官方目录）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketSources {
    #[serde(default = "default_market_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub sources: Vec<String>,
}

fn default_market_schema() -> u32 {
    1
}

impl MarketSources {
    pub fn load(file: &Path) -> Self {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, file: &Path) -> Result<(), String> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let raw = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(file, raw).map_err(|e| e.to_string())?;
        crate::platform::harden_0600(file);
        Ok(())
    }
}

/// 拉取一个索引源（本地文件路径或 git 仓库；返回索引名 + 条目）。
pub async fn fetch_market_source(
    manager: &Arc<PluginManager>,
    source: &str,
) -> Result<(Option<String>, Vec<MarketEntry>), String> {
    // 本地文件形态：<path>/marketplace.json 或 <path>（直接指向 json 文件）
    let as_dir_json = Path::new(source).join("marketplace.json");
    let as_file = Path::new(source);
    if as_file.is_file() {
        return parse_market_source(manager, &std::fs::read_to_string(as_file).map_err(|e| e.to_string())?);
    }
    if as_dir_json.is_file() {
        return parse_market_source(
            manager,
            &std::fs::read_to_string(&as_dir_json).map_err(|e| e.to_string())?,
        );
    }
    // git 仓库形态：浅克隆到临时目录读 marketplace.json
    probe_git().await?;
    valid_git_source(source)?;
    let work = std::env::temp_dir().join(format!("hub-market-{}", uuid::Uuid::new_v4()));
    let _ = std::fs::create_dir_all(&work);
    let result = async {
        run_git(
            &[
                "clone",
                "--quiet",
                "--depth",
                "1",
                source,
                work.to_string_lossy().as_ref(),
            ],
            None,
        )
        .await?;
        let index_file = work.join("marketplace.json");
        let raw = std::fs::read_to_string(&index_file)
            .map_err(|_| "仓库根缺 marketplace.json（索引源必须是含索引的仓库或本地文件）".to_string())?;
        parse_market_source(manager, &raw)
    }
    .await;
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn parse_market_source(
    manager: &Arc<PluginManager>,
    raw: &str,
) -> Result<(Option<String>, Vec<MarketEntry>), String> {
    let index: MarketIndex = serde_json::from_str(raw)
        .map_err(|e| format!("marketplace.json 解析失败：{e}"))?;
    // id 合法性预检（与 manifest id 规则一致；不合法条目被丢弃 + warn）
    let mut entries = Vec::new();
    for e in index.plugins {
        if super::manifest::valid_plugin_id(&e.id) {
            entries.push(e);
        } else {
            tracing::warn!(entry = %e.id, "marketplace 条目 id 不合法，已忽略");
        }
    }
    let _ = manager;
    Ok((index.name, entries))
}

/// 索引条目 → git 直装请求。
impl From<&MarketEntry> for GitInstallRequest {
    fn from(e: &MarketEntry) -> Self {
        let url = match &e.git_ref {
            Some(r) => format!("{}#{}", e.repo_url, r),
            None => e.repo_url.clone(),
        };
        Self {
            repo_url: url,
            sub_path: e.sub_path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_git_ref_handles_fragment() {
        let (url, r) = split_git_ref("https://example.com/repo.git#v0.2.0");
        assert_eq!(url, "https://example.com/repo.git");
        assert_eq!(r.as_deref(), Some("v0.2.0"));
        let (url, r) = split_git_ref("https://example.com/repo.git");
        assert_eq!(url, "https://example.com/repo.git");
        assert!(r.is_none());
        // 井尾空 ref 视为无 ref
        let (_, r) = split_git_ref("https://example.com/repo.git#");
        assert!(r.is_none());
    }

    #[test]
    fn git_source_validation() {
        assert!(valid_git_source("https://example.com/r.git").is_ok());
        assert!(valid_git_source("/tmp/local-repo").is_ok());
        assert!(valid_git_source("http://example.com/r.git").is_err(), "明文 http 拒绝");
        assert!(valid_git_source("git@github.com:a/b.git").is_err(), "ssh 不进 v1");
        assert!(valid_git_source("ssh://git@host/r").is_err());
        assert!(valid_git_source("relative/path").is_err(), "相对路径无法定位");
    }

    #[test]
    fn commit_sha_detection() {
        assert!(is_commit_sha("abcdef1234567"));
        assert!(is_commit_sha(&"a".repeat(40)));
        assert!(!is_commit_sha("v1.0.0"));
        assert!(!is_commit_sha("abc"));
        assert!(!is_commit_sha("xyz123"));
    }

    #[test]
    fn url_credentials_are_redacted() {
        let s = redact_url_credentials(
            "fatal: unable to access 'https://user:secret-token@example.com/r.git/'",
        );
        assert!(!s.contains("secret-token"), "{s}");
        assert!(s.contains("<redacted>@example.com"), "{s}");
        // 无凭据 URL 原样保留
        let s2 = redact_url_credentials("clone https://example.com/r.git done");
        assert_eq!(s2, "clone https://example.com/r.git done");
    }

    #[test]
    fn market_entry_to_install_request() {
        let e = MarketEntry {
            id: "demo".into(),
            name: "Demo".into(),
            description: None,
            repo_url: "https://example.com/r.git".into(),
            sub_path: Some("plugins/demo".into()),
            git_ref: Some("v1".into()),
            tags: vec![],
        };
        let req: GitInstallRequest = (&e).into();
        assert_eq!(req.repo_url, "https://example.com/r.git#v1");
        assert_eq!(req.sub_path.as_deref(), Some("plugins/demo"));
    }

    #[tokio::test]
    async fn git_install_end_to_end_from_local_repo() {
        // 本地 git 仓库 fixture：init → 提交一个最小插件 → 直装
        let repo = tempfile::tempdir().unwrap();
        let run = |args: &[&str], cwd: &Path| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q"], repo.path());
        let plugin_dir = repo.path().join("my-plugin");
        std::fs::create_dir_all(plugin_dir.join("backend")).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"schemaVersion":1,"id":"git-installed","name":"GitP","version":"0.1.0","hub":{"apiVersion":"1"},
                "contributes":{"tools":[{"id":"ping","name":"Ping","risk":"read","impl":{"kind":"backend","ref":"ping"}}]},
                "backend":{"command":"node","args":["backend/main.js"]}}"#,
        )
        .unwrap();
        std::fs::write(plugin_dir.join("backend/main.js"), "// demo\n").unwrap();
        run(&["add", "."], repo.path());
        run(&["-c", "user.email=t@example.com", "-c", "user.name=t", "commit", "-qm", "init"], repo.path());
        // 走 valid 校验 + 定位逻辑（不 spawn 完整 manager——网络路径单测覆盖解析层，
        // 全链路集成在 plugin_api 的 update/git 测试面）
        let (url, git_ref) = split_git_ref(&format!("{}#master", repo.path().display()));
        assert!(git_ref.is_some());
        valid_git_source(&url).expect("local path valid");
        assert!(locate_plugin_root(repo.path(), Some("my-plugin")).is_ok());
        assert!(locate_plugin_root(repo.path(), Some("../escape")).is_err());
    }
}
