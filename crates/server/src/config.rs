use std::{net::SocketAddr, path::PathBuf};

/// chat 分页配置（P6 B14）：`~/.claude-view/config.toml` 的 `[chat]` 段；
/// 文件缺失/解析失败回退默认（20/100）。page_size 限 20-30，buffer_max 不低于 page_size。
#[derive(Debug, Clone)]
pub struct ChatConfig {
    pub page_size: u32,
    pub buffer_max: u32,
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            page_size: 20,
            buffer_max: 100,
        }
    }
}

impl ChatConfig {
    fn load() -> Self {
        let path = std::env::var("AGENT_HUB_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                format!(
                    "{}/.claude-view/config.toml",
                    std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                )
                .into()
            });
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(val) = toml::from_str::<toml::Value>(&raw) else {
            tracing::warn!(path = %path.display(), "config.toml 解析失败，chat 分页用默认值");
            return Self::default();
        };
        let chat = val.get("chat");
        let int = |key: &str, fallback: u32| -> u32 {
            chat.and_then(|c| c.get(key))
                .and_then(toml::Value::as_integer)
                .map(|v| v.max(0) as u32)
                .unwrap_or(fallback)
        };
        let page_size = int("page_size", 20).clamp(20, 30);
        Self {
            page_size,
            // buffer_max 不低于 page_size：防 page_size=30/buffer_max=20 首屏即超限（r1 B-1 连带）
            buffer_max: int("buffer_max", 100).max(page_size),
        }
    }
}

/// ACP agent 条目（agent-hub-acp-omp 批1 任务1）：hub 侧声明可驱动的 ACP agent。
/// command+args 数组分离（spawn 不走 shell 拼接）；cwd/model 可空（会话创建时指定）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AcpAgentConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub model: Option<String>,
}

/// `[acp]` 配置段：agent 清单。文件缺省/解析失败回退内置 omp 条目；
/// env `AGENT_HUB_ACP_BIN` 覆盖默认 omp 命令（对齐 CLAUDE_BIN 惯例，不 pin 版本）。
#[derive(Debug, Clone)]
pub struct AcpConfig {
    pub agents: Vec<AcpAgentConfig>,
}

impl Default for AcpConfig {
    fn default() -> Self {
        let command = std::env::var("AGENT_HUB_ACP_BIN").unwrap_or_else(|_| "omp".into());
        Self {
            agents: vec![AcpAgentConfig {
                name: "omp".into(),
                command,
                args: vec!["acp".into()],
                cwd: None,
                model: None,
            }],
        }
    }
}

impl AcpConfig {
    pub fn load() -> Self {
        let path = std::env::var("AGENT_HUB_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                format!(
                    "{}/.claude-view/config.toml",
                    std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                )
                .into()
            });
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(val) = toml::from_str::<toml::Value>(&raw) else {
            tracing::warn!(path = %path.display(), "config.toml 解析失败，acp 段用默认值");
            return Self::default();
        };
        let Some(agents_val) = val.get("acp").and_then(|a| a.get("agents")) else {
            return Self::default();
        };
        // 表数组 → AcpAgentConfig；坏条目整段回退默认（配置错误不静默吞字段）
        let parsed: Option<Vec<AcpAgentConfig>> =
            serde_json::from_value(toml_to_json(agents_val)).ok();
        match parsed {
            Some(agents) if !agents.is_empty() => Self { agents },
            _ => {
                tracing::warn!(path = %path.display(), "[acp] 段无效或为空，用默认 agent 清单");
                Self::default()
            }
        }
    }
}

/// toml::Value → serde_json::Value（serde derive 解析 toml 表数组的桥）。
fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::json!(b),
        toml::Value::Datetime(d) => serde_json::json!(d.to_string()),
        toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub claude_bin: PathBuf,
    pub jobs_dir: PathBuf,
    pub poll_secs: u64,
    /// 工程列表持久化文件：`~/.claude-view/projects.json`，测试通过 `PROJECTS_FILE` 覆盖
    pub projects_file: PathBuf,
    /// `~/.claude` 根（会话 jsonl 定位 `<root>/projects/<slug>/<id>.jsonl`），env `CLAUDE_PROJECTS_DIR` 覆盖
    pub claude_root: PathBuf,
    /// 允许局域网访问（监听 0.0.0.0 并强制 token 认证）；env `AGENT_HUB_ALLOW_LAN`（"1"/"true"）
    pub allow_lan: bool,
    /// 认证 token；env `AGENT_HUB_TOKEN` 覆盖，未设则首启随机生成并持久化到 token_file
    pub token: Option<String>,
    /// token 持久化文件，env `AGENT_HUB_TOKEN_FILE` 覆盖；默认 `~/.claude-view/token`
    pub token_file: PathBuf,
    /// agents 配置目录（P3 配置管理器），env `AGENT_HUB_AGENTS_DIR` 覆盖；默认 `~/.claude/agents`
    pub agents_dir: PathBuf,
    /// chat 分页配置（P6 B14）
    pub chat: ChatConfig,
    /// ACP agent 清单（acp-omp 批1）
    pub acp: AcpConfig,
    /// ACP 会话持久化文件（批3 任务13）：`~/.claude-view/acp_sessions.json`，
    /// env `ACP_SESSIONS_FILE` 覆盖；惯例同 projects_file
    pub acp_sessions_file: PathBuf,
    /// 工程便签持久化（agent-hub-notes）：`~/.claude-view/notes.json`，env `NOTES_FILE`
    /// 覆盖；惯例同 projects_file，落盘强制 0600（内容可能含账密）
    pub notes_file: PathBuf,
    /// 打开本地目录的命令（P6 B3）；env `AGENT_HUB_DIR_OPENER` 覆盖，默认 macOS `open`
    pub dir_opener: String,
    /// 多实例配置持久化（agent-hub-multi-instance 批1 任务2）：`~/.claude-view/instances.json`，
    /// env `INSTANCES_FILE` 覆盖；惯例同 notes.json，落盘强制 0600（含远程实例 token/密码）
    pub instances_file: PathBuf,
}

impl Config {
    pub fn load() -> Self {
        Self {
            bind_addr: std::env::var("AGENT_HUB_BIND")
                .unwrap_or_else(|_| "127.0.0.1:7800".into())
                .parse()
                .expect("bad AGENT_HUB_BIND"),
            claude_bin: std::env::var("CLAUDE_BIN")
                .unwrap_or_else(|_| "claude".into())
                .into(),
            jobs_dir: std::env::var("JOBS_DIR")
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude/jobs",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                })
                .into(),
            poll_secs: std::env::var("POLL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10),
            projects_file: std::env::var("PROJECTS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/projects.json",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                    .into()
                }),
            claude_root: std::env::var("CLAUDE_PROJECTS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                    .into()
                }),
            allow_lan: matches!(
                std::env::var("AGENT_HUB_ALLOW_LAN").as_deref(),
                Ok("1") | Ok("true")
            ),
            token: std::env::var("AGENT_HUB_TOKEN").ok(),
            agents_dir: std::env::var("AGENT_HUB_AGENTS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude/agents",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                    .into()
                }),
            token_file: std::env::var("AGENT_HUB_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/token",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                    .into()
                }),
            chat: ChatConfig::load(),
            acp: AcpConfig::load(),
            acp_sessions_file: std::env::var("ACP_SESSIONS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/acp_sessions.json",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                    .into()
                }),
            notes_file: std::env::var("NOTES_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/notes.json",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                    .into()
                }),
            dir_opener: std::env::var("AGENT_HUB_DIR_OPENER").unwrap_or_else(|_| "open".into()),
            instances_file: std::env::var("INSTANCES_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/instances.json",
                        std::env::var("HOME").expect("HOME 环境变量未设置，无法定位数据目录")
                    )
                    .into()
                }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// env var 全局态 + cargo test 并行 → 三场景合并单测试顺序执行，防跨测试竞态
    #[test]
    fn chat_config_load_scenarios() {
        // 场景 1：无 config.toml → 默认 20/100（B14 验收）
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("AGENT_HUB_CONFIG", dir.path().join("none.toml"));
        let cfg = ChatConfig::load();
        assert_eq!(cfg.page_size, 20);
        assert_eq!(cfg.buffer_max, 100);

        // 场景 2：[chat] 合法段生效；page_size 越界 clamp 到 20-30；buffer_max 不低于 page_size
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[chat]\npage_size = 99\nbuffer_max = 5\n").unwrap();
        std::env::set_var("AGENT_HUB_CONFIG", &file);
        let cfg = ChatConfig::load();
        assert_eq!(cfg.page_size, 30);
        assert_eq!(cfg.buffer_max, 30);

        // 场景 3：坏 toml 回退默认
        std::fs::write(&file, "not [ valid toml").unwrap();
        let cfg = ChatConfig::load();
        assert_eq!(cfg.page_size, 20);
        assert_eq!(cfg.buffer_max, 100);

        std::env::remove_var("AGENT_HUB_CONFIG");
    }

    /// env var 全局态 + cargo test 并行 → 四场景合并单测顺序执行，防跨测试竞态
    #[test]
    fn acp_config_load_scenarios() {
        // 场景 1：无 config.toml → 内置 omp 条目（任务 1 验收：缺省回退）
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("AGENT_HUB_CONFIG", dir.path().join("none.toml"));
        std::env::remove_var("AGENT_HUB_ACP_BIN");
        let cfg = AcpConfig::load();
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.agents[0].name, "omp");
        assert_eq!(cfg.agents[0].args, vec!["acp".to_string()]);

        // 场景 2：显式 [[acp.agents]] 覆盖 → 按配置解析（含 cwd/model 可选字段）
        let file = dir.path().join("config.toml");
        std::fs::write(
            &file,
            "[[acp.agents]]\nname = \"x\"\ncommand = \"/bin/echo\"\nargs = [\"a\", \"b\"]\ncwd = \"/tmp\"\nmodel = \"m1\"\n",
        )
        .unwrap();
        std::env::set_var("AGENT_HUB_CONFIG", &file);
        let cfg = AcpConfig::load();
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.agents[0].name, "x");
        assert_eq!(cfg.agents[0].command, "/bin/echo");
        assert_eq!(cfg.agents[0].args, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(cfg.agents[0].cwd.as_deref(), Some("/tmp"));
        assert_eq!(cfg.agents[0].model.as_deref(), Some("m1"));

        // 场景 3：坏 [acp] 段（条目缺 name）→ 回退默认
        std::fs::write(&file, "[[acp.agents]]\ncommand = \"/bin/echo\"\n").unwrap();
        let cfg = AcpConfig::load();
        assert_eq!(cfg.agents[0].name, "omp");

        // 场景 4：AGENT_HUB_ACP_BIN 覆盖默认 omp 命令（对齐 CLAUDE_BIN 惯例）
        std::env::remove_var("AGENT_HUB_CONFIG");
        std::env::set_var("AGENT_HUB_ACP_BIN", "/opt/fake-acp");
        let cfg = AcpConfig::load();
        assert_eq!(cfg.agents[0].command, "/opt/fake-acp");
        assert_eq!(cfg.agents[0].name, "omp");

        std::env::remove_var("AGENT_HUB_ACP_BIN");
    }
}
