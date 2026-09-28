//! Agent harness 自动发现（agent-hub-settings 批A 需求5/6）：扫本机 PATH 里的 coding
//! agent CLI，出可执行路径 + 版本；已配置但路径失效的条目标 `alive: false`
//! （需求6 的死亡判定，前端默认过滤掉）。
//!
//! 探测刻意轻量：`which` 等价的路径解析 + `<bin> --version`（claude/omp/codex/gemini
//! 同款，首行即版本串）。**不做 ACP initialize 握手**——每个 harness 一次子进程 spawn，
//! 代价远大于收益；真不可用会在建会话握手时暴露（见 drivers/acp）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::config::AcpAgentConfig;

/// 探测 `--version` 超时（超时不阻塞 UI，版本缺失不判死——存在即可执行才是存活判据）
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// harness 形态：CLI 类 hub 不直接驱动（走各自原生链路），ACP 类可加入 `[[acp.agents]]`
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HarnessKind {
    Cli,
    Acp,
}

impl HarnessKind {
    /// 加入 ACP 配置时的启动参数（omp 走 `acp` 子命令）
    pub fn args(self) -> Vec<String> {
        match self {
            HarnessKind::Cli => Vec::new(),
            HarnessKind::Acp => vec!["acp".into()],
        }
    }
}

pub struct KnownHarness {
    pub name: &'static str,
    pub kind: HarnessKind,
}

/// 已知 harness 清单（可扩展）：加一项即自动进探测与面板。
/// name 既是 PATH 上的可执行名，也是写入 `[[acp.agents]]` 的 agent 名。
pub const KNOWN: &[KnownHarness] = &[
    KnownHarness {
        name: "claude",
        kind: HarnessKind::Cli,
    },
    KnownHarness {
        name: "omp",
        kind: HarnessKind::Acp,
    },
    KnownHarness {
        name: "codex",
        kind: HarnessKind::Cli,
    },
    KnownHarness {
        name: "gemini",
        kind: HarnessKind::Cli,
    },
];

/// 清单内查 name 的形态。**清单外一律拒绝加入配置**——name 不取自用户输入，
/// 顺带挡掉路径穿越 / TOML 注入。
pub fn kind_of(name: &str) -> Option<HarnessKind> {
    KNOWN.iter().find(|k| k.name == name).map(|k| k.kind)
}

/// 一条 harness 清单项（前端列表单元）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessEntry {
    pub name: String,
    /// 可执行路径；已配置但失效时回显配置里的原始 command（便于排查）
    pub path: String,
    pub version: Option<String>,
    /// 存活 = 路径存在且可执行（需求6 死亡判据：文件不存在/不可执行 → 失效）
    pub alive: bool,
    pub kind: HarnessKind,
    /// 已在 `[[acp.agents]]` 里（前端据此把「加入配置」换成「已加入」）
    pub configured: bool,
}

/// 存活判据：存在 + 可执行位（unix）。GUI 环境下不可执行的文件照样是死路径。
pub fn is_alive(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    true
}

/// 继承 PATH + GUI 环境常见安装位（macOS .app 的 PATH 不含 ~/.local/bin、~/.bun/bin ——
/// 与 desktop 壳 `extended_path` 同一先例，探测必须同样补齐，否则桌面端全灭）。
pub fn extended_path(inherited: &str, home: &str) -> String {
    let candidates = [
        "/usr/local/bin",
        "/usr/local/sbin",
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        &format!("{home}/.local/bin"),
        &format!("{home}/.bun/bin"),
        &format!("{home}/.cargo/bin"),
    ];
    let mut parts: Vec<String> = inherited
        .split(':')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    for c in candidates {
        if Path::new(c).is_dir() && !parts.iter().any(|p| p == c) {
            parts.push(c.to_string());
        }
    }
    parts.join(":")
}

fn search_dirs() -> Vec<PathBuf> {
    let path = extended_path(
        &std::env::var("PATH").unwrap_or_default(),
        &std::env::var("HOME").unwrap_or_default(),
    );
    path.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// `cmd` 解析成实际路径：含分隔符/绝对路径按原样判存活，否则在搜索路径里找。
fn resolve(dirs: &[PathBuf], cmd: &str) -> Option<PathBuf> {
    let p = Path::new(cmd);
    if p.is_absolute() || cmd.contains('/') {
        return is_alive(p).then(|| p.to_path_buf());
    }
    dirs.iter().map(|d| d.join(cmd)).find(|c| is_alive(c))
}

/// 版本探测：`<bin> --version` 取首个非空行。超时不阻塞 UI。
async fn probe_version(path: &Path) -> Option<String> {
    let out = tokio::time::timeout(VERSION_TIMEOUT, async {
        tokio::process::Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .ok()
    })
    .await
    .ok()??;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(String::from)
}

/// 探测全清单 + 合并当前已配置的 ACP agent（搜索路径 = 本机 PATH）
pub async fn discover(configured: &[AcpAgentConfig]) -> Vec<HarnessEntry> {
    discover_in(&search_dirs(), configured).await
}

/// 同上，但搜索路径由调用方给定（单测自造目录用）
pub async fn discover_in(dirs: &[PathBuf], configured: &[AcpAgentConfig]) -> Vec<HarnessEntry> {
    let known: Vec<HarnessEntry> = futures::future::join_all(KNOWN.iter().map(|k| async move {
        let cmd = configured
            .iter()
            .find(|a| a.name == k.name)
            .map(|a| a.command.clone());
        let found = resolve(dirs, cmd.as_deref().unwrap_or(k.name));
        let version = match &found {
            Some(p) => probe_version(p).await,
            None => None,
        };
        let alive = found.is_some();
        HarnessEntry {
            name: k.name.to_string(),
            path: found
                .map(|p| p.to_string_lossy().into_owned())
                .or(cmd.clone())
                .unwrap_or_else(|| k.name.to_string()),
            alive,
            version,
            kind: k.kind,
            configured: cmd.is_some(),
        }
    }))
    .await;

    // 清单外的自定义 ACP agent：不在探测范围，只做存活判定
    let mut out = known;
    for a in configured {
        if out.iter().any(|e| e.name == a.name) {
            continue;
        }
        out.push(HarnessEntry {
            name: a.name.clone(),
            path: a.command.clone(),
            alive: resolve(dirs, &a.command).is_some(),
            version: None,
            kind: HarnessKind::Acp,
            configured: true,
        });
    }
    out
}

/// 把 agent 追加进 config.toml 的 `[[acp.agents]]`（保留其他段）。
/// `current` = 当前生效清单：配置里还没有 acp 段时先按它落盘再追加，否则新建的
/// config.toml 会让 `AcpConfig::load` 的内置默认（omp）静默消失。
/// 已存在但解析不了的配置**不覆盖**（拒绝写入，不丢用户配置）。
pub fn append_agent(
    config_path: &Path,
    agent: &AcpAgentConfig,
    current: &[AcpAgentConfig],
) -> Result<(), String> {
    let mut doc: toml::Table = match std::fs::read_to_string(config_path) {
        Ok(raw) => {
            toml::from_str(&raw).map_err(|e| format!("config.toml 解析失败，拒绝写入: {e}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
        Err(e) => return Err(format!("读取 config.toml 失败: {e}")),
    };
    let acp = doc
        .entry("acp")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let acp = acp.as_table_mut().ok_or("config.toml 的 [acp] 段不是表")?;
    let agents = acp
        .entry("agents")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let arr = agents
        .as_array_mut()
        .ok_or("config.toml 的 acp.agents 不是数组")?;
    if arr.is_empty() {
        arr.extend(current.iter().map(agent_table));
    }
    arr.push(agent_table(agent));

    if let Some(dir) = config_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    std::fs::write(config_path, doc.to_string()).map_err(|e| format!("写回 config.toml 失败: {e}"))
}

/// 单条 agent 的 TOML 表（手写而非 `toml::to_value`：Option::None 在 toml 序列化里报错，
/// 而 cwd/model 缺省即 None——解析侧 serde 对 Option 字段可缺省，两边语义对齐）
fn agent_table(a: &AcpAgentConfig) -> toml::Value {
    let mut t = toml::Table::new();
    t.insert("name".into(), toml::Value::String(a.name.clone()));
    t.insert("command".into(), toml::Value::String(a.command.clone()));
    t.insert(
        "args".into(),
        toml::Value::Array(
            a.args
                .iter()
                .map(|s| toml::Value::String(s.clone()))
                .collect(),
        ),
    );
    toml::Value::Table(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// env var 全局态 + cargo test 并行 → 合并单测试顺序执行，防跨测试竞态
    fn agent(name: &str, command: &str, args: &[&str]) -> AcpAgentConfig {
        AcpAgentConfig {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: None,
            model: None,
        }
    }

    #[test]
    fn extended_path_appends_existing_dirs_and_dedupes() {
        let out = extended_path("/usr/bin:/opt/homebrew/bin", "/home/test");
        let parts: Vec<&str> = out.split(':').collect();
        assert_eq!(parts[0], "/usr/bin"); // 继承 PATH 保序在前
        assert_eq!(
            parts.iter().filter(|p| **p == "/opt/homebrew/bin").count(),
            1
        );
        assert!(parts.contains(&"/usr/local/bin"));
        assert!(!parts.iter().any(|p| p.contains(".cargo"))); // /home/test/.cargo/bin 不存在
    }

    #[test]
    fn is_alive_rejects_missing_and_non_executable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_alive(&dir.path().join("nope"))); // 不存在
        let ok = dir.path().join("ok.sh");
        std::fs::write(&ok, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!is_alive(&ok)); // 存在但无执行位 = 死路径
            std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(is_alive(&ok));
    }

    #[test]
    fn resolve_finds_bare_name_in_dirs_and_keeps_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fakecli");
        std::fs::write(&bin, "#!/bin/sh\necho 1.2.3\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dirs = vec![PathBuf::from("/nonexistent-dir"), dir.path().to_path_buf()];
        assert_eq!(resolve(&dirs, "fakecli"), Some(bin.clone()));
        assert_eq!(resolve(&dirs, "missing"), None);
        assert_eq!(resolve(&dirs, bin.to_str().unwrap()), Some(bin));
    }

    #[tokio::test]
    async fn probe_version_reads_first_line_and_survives_junk() {
        let dir = tempfile::tempdir().unwrap();
        let sh = dir.path().join("v.sh");
        // 首行空 → 取首个非空行；多行 → 只取首行
        std::fs::write(&sh, "#!/bin/sh\n\necho \"1.2.3 (Fake)\"\necho noise\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sh, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(probe_version(&sh).await.as_deref(), Some("1.2.3 (Fake)"));
        // 非可执行文件探不到版本 → None（不 panic、不判死）
        assert_eq!(probe_version(&dir.path().join("none")).await, None);
    }

    #[tokio::test]
    async fn discover_marks_configured_dead_entry_not_alive() {
        // 自造搜索目录：只放一个可执行假 harness，其余清单项自然探不到
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("omp");
        std::fs::write(&bin, "#!/bin/sh\necho omp/18.0.11\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dirs = vec![dir.path().to_path_buf()];

        // 未配置的可执行 harness → alive + 版本 + 清单全覆盖
        let out = discover_in(&dirs, &[]).await;
        let omp = out.iter().find(|e| e.name == "omp").expect("omp in list");
        assert!(omp.alive);
        assert_eq!(omp.version.as_deref(), Some("omp/18.0.11"));
        assert_eq!(omp.path, bin.to_string_lossy());
        assert!(!omp.configured);
        assert_eq!(out.len(), KNOWN.len());

        // 已配置的 omp 指到失效路径 → alive:false（需求6 死亡过滤的服务端判据）
        let dead = agent("omp", "/nope/omp", &["acp"]);
        let out = discover_in(&dirs, std::slice::from_ref(&dead)).await;
        let omp = out.iter().find(|e| e.name == "omp").expect("omp in list");
        assert!(!omp.alive);
        assert_eq!(omp.path, "/nope/omp"); // 回显配置原文便于排查
        assert!(omp.configured);
        assert_eq!(omp.kind, HarnessKind::Acp);
    }

    #[test]
    fn append_agent_preserves_other_sections_and_seeds_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested/config.toml");
        let read =
            || -> toml::Table { toml::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap() };
        let names = |t: &toml::Table| -> Vec<String> {
            t["acp"]["agents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["name"].as_str().unwrap().to_string())
                .collect()
        };

        // 场景 1：文件已有 [chat] 段、无 acp 段 → 先按当前生效清单落盘再追加，其他段不动
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "[chat]\npage_size = 25\n").unwrap();
        append_agent(
            &file,
            &agent("gemini", "/opt/gemini", &[]),
            &[agent("omp", "omp", &["acp"])],
        )
        .unwrap();
        let doc = read();
        assert_eq!(doc["chat"]["page_size"].as_integer(), Some(25));
        assert_eq!(names(&doc), vec!["omp".to_string(), "gemini".to_string()]);
        assert_eq!(doc["acp"]["agents"][1]["args"].as_array().unwrap().len(), 0);

        // 场景 2：已有 acp 段 → 只追加，不重复播种
        append_agent(&file, &agent("omp2", "/opt/omp2", &["acp"]), &[]).unwrap();
        let doc = read();
        assert_eq!(doc["chat"]["page_size"].as_integer(), Some(25));
        assert_eq!(
            names(&doc),
            vec!["omp".to_string(), "gemini".to_string(), "omp2".to_string()]
        );

        // 场景 3：坏 toml 拒绝写入（不丢用户配置）
        std::fs::write(&file, "not [ valid toml").unwrap();
        assert!(append_agent(&file, &agent("x", "/opt/x", &[]), &[]).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "not [ valid toml");
    }

    #[test]
    fn kind_of_only_knows_list() {
        assert_eq!(kind_of("omp"), Some(HarnessKind::Acp));
        assert_eq!(kind_of("claude"), Some(HarnessKind::Cli));
        assert_eq!(kind_of("../../etc/passwd"), None);
        assert_eq!(HarnessKind::Acp.args(), vec!["acp".to_string()]);
    }
}
