use std::{net::SocketAddr, path::PathBuf};

/// config.toml 路径：env `AGENT_HUB_CONFIG` 覆盖，默认 `~/.claude-view/config.toml`。
/// 写侧（harness 加入配置）用它定位；读侧各段仍走自己的 `load()`。
pub fn config_path() -> PathBuf {
    std::env::var("AGENT_HUB_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            format!(
                "{}/.claude-view/config.toml",
                crate::platform::home_dir_str()
            )
            .into()
        })
}

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
                    crate::platform::home_dir_str()
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

/// `[pi]` 配置段（agent-hub-pi-hub-agent）：内置 Hub 助手驱动。
/// bin 缺省走 PATH（env `AGENT_HUB_PI_BIN` 覆盖，对齐 CLAUDE_BIN 惯例）；
/// auto_start = hub 启动即拉起助手（config.toml `[pi] auto_start = false` 关闭）。
/// provider/model 不在此段——P3 走设置页持久化（pi_settings.json，变更重启生效）。
#[derive(Debug, Clone)]
pub struct PiConfig {
    pub bin: String,
    pub auto_start: bool,
}

impl Default for PiConfig {
    fn default() -> Self {
        Self {
            bin: std::env::var("AGENT_HUB_PI_BIN").unwrap_or_else(|_| "pi".into()),
            auto_start: true,
        }
    }
}

impl PiConfig {
    pub fn load() -> Self {
        let mut cfg = Self::default();
        let path = std::env::var("AGENT_HUB_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                format!(
                    "{}/.claude-view/config.toml",
                    crate::platform::home_dir_str()
                )
                .into()
            });
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return cfg;
        };
        let Ok(val) = toml::from_str::<toml::Value>(&raw) else {
            return cfg;
        };
        if let Some(pi) = val.get("pi").and_then(|p| p.as_table()) {
            if let Some(bin) = pi.get("bin").and_then(|b| b.as_str()) {
                cfg.bin = bin.into();
            }
            if let Some(auto) = pi.get("auto_start").and_then(|a| a.as_bool()) {
                cfg.auto_start = auto;
            }
        }
        cfg
    }
}

/// `[opencode]` 配置段（agent-hub-opencode-driver）：attach-service 首发驱动。
/// bin 缺省走 PATH（env `OPENCODE_BIN` 覆盖，对齐 CLAUDE_BIN/AGENT_HUB_ACP_BIN 惯例）；
/// port 首选 4097（env `OPENCODE_PORT`），被占顺延探测。
#[derive(Debug, Clone)]
pub struct OpencodeConfig {
    pub bin: String,
    pub port: u16,
}

impl Default for OpencodeConfig {
    fn default() -> Self {
        Self {
            bin: std::env::var("OPENCODE_BIN").unwrap_or_else(|_| "opencode".into()),
            port: std::env::var("OPENCODE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4097),
        }
    }
}

/// `[dsh]` 配置段（agent-hub-dsh-driver）：attach-service 第二实现。
/// 结构与 OpencodeConfig 同款；port 首选 13100（env `DSH_PORT`），bin 缺省走 PATH
///（env `DSH_BIN` 覆盖）。
///
/// **单实例不变式（方案 A，2026-10-04 用户拍板）**：`port` 是唯一 dsh web 实例，
/// DSH_HOME 恒为真实 `~/.dsh`（hub 全托管 = 把用户真实会话收进视野）。收编与
/// spawn 都锚定同一 (port, home)；同 home 出现第二个 host = 用户自行破坏，hub
/// 只收编或降级，绝不另起实例。跨 host 活体会话互不可解析（page/follow
/// session/not-found，2026-10-03 实测），锚定是防拓扑裂开的第一道闸。
#[derive(Debug, Clone)]
pub struct DshConfig {
    pub bin: String,
    pub port: u16,
    /// 自动拉起（默认 **false**——2026-10-02 端口风暴事故定案）：false 时只收编
    /// 配置端口上已监听的实例；true 才允许 hub 主动 spawn
    pub auto_start: bool,
    /// dsh web 浏览器认证的 launch token（**用户自己填**——hub 不读 dsh 凭据存储；
    /// None = 目标实例未开认证）。敏感值仅内存与 config 文件，不落日志
    pub token: Option<String>,
}

impl Default for DshConfig {
    fn default() -> Self {
        Self {
            bin: std::env::var("DSH_BIN").unwrap_or_else(|_| "dsh".into()),
            port: std::env::var("DSH_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(13100),
            auto_start: std::env::var("DSH_AUTO_START")
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            token: std::env::var("DSH_TOKEN")
                .ok()
                .filter(|v| !v.is_empty())
                .map(normalize_launch_token),
        }
    }
}

/// 用户粘贴形态归一：完整登录 URL（`http://…?token=<t>`）或裸 token 皆可。
fn normalize_launch_token(v: String) -> String {
    match v.split("token=").nth(1) {
        Some(t) if !t.is_empty() => t.split('&').next().unwrap_or(t).to_string(),
        _ => v,
    }
}

impl DshConfig {
    fn load() -> Self {
        let path = std::env::var("AGENT_HUB_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                format!(
                    "{}/.claude-view/config.toml",
                    crate::platform::home_dir_str()
                )
                .into()
            });
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(val) = toml::from_str::<toml::Value>(&raw) else {
            tracing::warn!(path = %path.display(), "config.toml 解析失败，dsh 段用默认值");
            return Self::default();
        };
        let section = val.get("dsh");
        let bin = section
            .and_then(|o| o.get("bin"))
            .and_then(toml::Value::as_str)
            .map(str::to_string);
        let port = section
            .and_then(|o| o.get("port"))
            .and_then(toml::Value::as_integer)
            .map(|v| v as u16);
        let auto_start = section
            .and_then(|o| o.get("auto_start"))
            .and_then(toml::Value::as_bool);
        let d = Self::default();
        // env 覆盖 config.toml（惯例：env > 文件 > 默认）
        Self {
            bin: std::env::var("DSH_BIN").unwrap_or_else(|_| bin.unwrap_or(d.bin)),
            port: std::env::var("DSH_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(port)
                .unwrap_or(d.port),
            auto_start: std::env::var("DSH_AUTO_START")
                .ok()
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .or(auto_start)
                .unwrap_or(d.auto_start),
            token: std::env::var("DSH_TOKEN")
                .ok()
                .filter(|v| !v.is_empty())
                .map(normalize_launch_token)
                .or_else(|| {
                    section
                        .and_then(|o| o.get("token"))
                        .and_then(toml::Value::as_str)
                        .map(|v| normalize_launch_token(v.to_string()))
                })
                .or(d.token),
        }
    }
}

/// `[omp]` 配置段（agent-hub-omp-session-host 阶段 A）：文件事实源驱动。
/// 纯只读扫 `~/.omp/agent/sessions`（绝不写）；无 spawn 语义（omp 本体无外连服务）。
#[derive(Debug, Clone)]
pub struct OmpConfig {
    pub sessions_dir: PathBuf,
}

impl Default for OmpConfig {
    fn default() -> Self {
        Self {
            sessions_dir: std::env::var("OMP_SESSIONS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!("{}/.omp/agent/sessions", crate::platform::home_dir_str()).into()
                }),
        }
    }
}

impl OmpConfig {
    fn load() -> Self {
        let path = std::env::var("AGENT_HUB_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                format!(
                    "{}/.claude-view/config.toml",
                    crate::platform::home_dir_str()
                )
                .into()
            });
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(val) = toml::from_str::<toml::Value>(&raw) else {
            return Self::default();
        };
        let dir = val
            .get("omp")
            .and_then(|o| o.get("sessions_dir"))
            .and_then(toml::Value::as_str)
            .map(PathBuf::from);
        let d = Self::default();
        Self {
            sessions_dir: std::env::var("OMP_SESSIONS_DIR")
                .ok()
                .map(PathBuf::from)
                .or(dir)
                .unwrap_or(d.sessions_dir),
        }
    }
}

impl OpencodeConfig {
    fn load() -> Self {
        let path = std::env::var("AGENT_HUB_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                format!(
                    "{}/.claude-view/config.toml",
                    crate::platform::home_dir_str()
                )
                .into()
            });
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(val) = toml::from_str::<toml::Value>(&raw) else {
            tracing::warn!(path = %path.display(), "config.toml 解析失败，opencode 段用默认值");
            return Self::default();
        };
        let section = val.get("opencode");
        let bin = section
            .and_then(|o| o.get("bin"))
            .and_then(toml::Value::as_str)
            .map(str::to_string);
        let port = section
            .and_then(|o| o.get("port"))
            .and_then(toml::Value::as_integer)
            .map(|v| v as u16);
        let d = Self::default();
        // env 覆盖 config.toml（惯例：env > 文件 > 默认）
        Self {
            bin: std::env::var("OPENCODE_BIN").unwrap_or_else(|_| bin.unwrap_or(d.bin)),
            port: std::env::var("OPENCODE_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .or(port)
                .unwrap_or(d.port),
        }
    }
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
                    crate::platform::home_dir_str()
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
    /// node 形态身份文件（gateway-node P1 任务2）：`~/.claude-view/node.json`，
    /// env `AGENT_HUB_NODE_FILE` 覆盖；惯例同 instances.json（0600，含 node token）
    pub node_file: PathBuf,
    /// 节点注册表（gateway-node P1 任务4）：`~/.claude-view/nodes.json`，
    /// env `AGENT_HUB_NODES_FILE` 覆盖；惯例同 instances.json（0600，只含 token 哈希）
    pub nodes_file: PathBuf,
    /// opencode 驱动配置（agent-hub-opencode-driver，attach-service 首发）
    pub opencode: OpencodeConfig,
    /// opencode serve 运行态落盘文件：`~/.claude-view/opencode_serve.json`，
    /// env `OPENCODE_SERVE_FILE` 覆盖（端口/pid 记录，hub 重启后重新发现）
    pub opencode_serve_file: PathBuf,
    /// dsh 驱动配置（agent-hub-dsh-driver，attach-service 第二实现）
    pub dsh: DshConfig,
    /// omp 文件事实源驱动配置（agent-hub-omp-session-host 阶段 A）
    pub omp: OmpConfig,
    /// dsh serve 运行态落盘文件：`~/.claude-view/dsh_serve.json`，env `DSH_SERVE_FILE` 覆盖
    pub dsh_serve_file: PathBuf,
    /// 定时任务持久化目录（agent-hub-daemon-capabilities）：`~/.claude-view/tasks`
    /// （tasks.json + runs/<taskId>.jsonl），env `AGENT_HUB_TASKS_DIR` 覆盖；惯例同 projects_file
    pub tasks_dir: PathBuf,
    /// interactive「从列表移除」忽略清单文件（R8）：`~/.claude-view/ignored_interactive.json`，
    /// env `AGENT_HUB_IGNORED_INTERACTIVE_FILE` 覆盖；惯例同 notes_file
    pub ignored_interactive_file: PathBuf,
    /// pi 驱动配置（agent-hub-pi-hub-agent）
    pub pi: PiConfig,
    /// P3 pi 助手设置持久化：`~/.claude-view/pi_settings.json`，
    /// env `PI_SETTINGS_FILE` 覆盖（provider/model，设置页读写）
    pub pi_settings_file: PathBuf,
    /// P8 extension 注入文件：`~/.claude-view/pi/hub-assistant.ts`（hub 生成写入，
    /// spawn 时 `-e` 加载）；env `PI_EXTENSION_FILE` 覆盖
    pub pi_extension_file: PathBuf,
    /// pi 会话存储目录（`--session-dir`）：`~/.claude-view/pi/sessions`，
    /// env `PI_SESSION_DIR` 覆盖
    pub pi_session_dir: PathBuf,
    /// 插件根目录（ADR-0001 第 4/5 条 drop-in 位）：`~/.claude-view/plugins`，
    /// env `AGENT_HUB_PLUGINS_DIR` 覆盖；只扫一层深、不跟随符号链接
    pub plugins_dir: PathBuf,
    /// 插件注册表持久态：`~/.claude-view/plugins.json`，env `AGENT_HUB_PLUGINS_FILE`
    /// 覆盖；落盘强制 0600（含权限面 hash 与故障摘要）
    pub plugin_state_file: PathBuf,
    /// 插件凭据代持文件（ADR-0007 第 4 条）：`~/.claude-view/plugin_credentials.json`，
    /// env `AGENT_HUB_PLUGIN_CREDENTIALS_FILE` 覆盖；0600，值零回显
    pub plugin_credentials_file: PathBuf,
    /// 插件审计留档目录：`~/.claude-view/plugin_audit/<id>-<hash>.json`，
    /// env `AGENT_HUB_PLUGIN_AUDIT_DIR` 覆盖
    pub plugin_audit_dir: PathBuf,
    /// 插件自持数据目录：`~/.claude-view/plugin_data/<id>/`，
    /// env `AGENT_HUB_PLUGIN_DATA_DIR` 覆盖（卸载默认保留，ADR-0007 拍板结论 5）
    pub plugin_data_dir: PathBuf,
    /// 周期扫描间隔秒数（ADR-0002 第 2 条）：`[plugins] scan_interval_secs`，
    /// env `AGENT_HUB_PLUGIN_SCAN_SECS` 覆盖；0 = 关闭（退化为启动扫描 + 手动刷新）
    pub plugin_scan_secs: u64,
    /// transient 插件目录（ADR-0001 第 4 条）：`--plugin-dir` 可重复或 env
    /// `AGENT_HUB_PLUGIN_DIR`（多值 `:` 分隔）；不写注册表持久层
    pub plugin_transient_dirs: Vec<PathBuf>,
    /// MCP 聚合代理会话级 config 目录（B2）：`~/.claude-view/plugin_mcp`，
    /// env `AGENT_HUB_PLUGIN_MCP_DIR` 覆盖；每会话一 JSON（不写 `~/.claude`）
    pub plugin_mcp_dir: PathBuf,
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
                .unwrap_or_else(|_| format!("{}/.claude/jobs", crate::platform::home_dir_str()))
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
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            claude_root: std::env::var("CLAUDE_PROJECTS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| format!("{}/.claude", crate::platform::home_dir_str()).into()),
            allow_lan: matches!(
                std::env::var("AGENT_HUB_ALLOW_LAN").as_deref(),
                Ok("1") | Ok("true")
            ),
            node_file: std::env::var("AGENT_HUB_NODE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!("{}/.claude-view/node.json", crate::platform::home_dir_str()).into()
                }),
            nodes_file: std::env::var("AGENT_HUB_NODES_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/nodes.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            token: std::env::var("AGENT_HUB_TOKEN").ok(),
            agents_dir: std::env::var("AGENT_HUB_AGENTS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!("{}/.claude/agents", crate::platform::home_dir_str()).into()
                }),
            token_file: std::env::var("AGENT_HUB_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!("{}/.claude-view/token", crate::platform::home_dir_str()).into()
                }),
            chat: ChatConfig::load(),
            acp: AcpConfig::load(),
            acp_sessions_file: std::env::var("ACP_SESSIONS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/acp_sessions.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            notes_file: std::env::var("NOTES_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/notes.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            // interactive「从列表移除」忽略清单（R8）：按 sessionId 持久过滤
            // 前台会话条目；底层进程消失后条目自然死亡，清单懒清理（惯例同 notes）
            ignored_interactive_file: std::env::var("AGENT_HUB_IGNORED_INTERACTIVE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/ignored_interactive.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            dir_opener: std::env::var("AGENT_HUB_DIR_OPENER").unwrap_or_else(|_| "open".into()),
            instances_file: std::env::var("INSTANCES_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/instances.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            opencode: OpencodeConfig::load(),
            opencode_serve_file: std::env::var("OPENCODE_SERVE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/opencode_serve.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            dsh: DshConfig::load(),
            omp: OmpConfig::load(),
            dsh_serve_file: std::env::var("DSH_SERVE_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/dsh_serve.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            tasks_dir: std::env::var("AGENT_HUB_TASKS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!("{}/.claude-view/tasks", crate::platform::home_dir_str()).into()
                }),
            pi: PiConfig::load(),
            pi_settings_file: std::env::var("PI_SETTINGS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/pi_settings.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            pi_extension_file: std::env::var("PI_EXTENSION_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/pi/hub-assistant.ts",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            pi_session_dir: std::env::var("PI_SESSION_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/pi/sessions",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            plugins_dir: std::env::var("AGENT_HUB_PLUGINS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!("{}/.claude-view/plugins", crate::platform::home_dir_str()).into()
                }),
            plugin_state_file: std::env::var("AGENT_HUB_PLUGINS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/plugins.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            plugin_credentials_file: std::env::var("AGENT_HUB_PLUGIN_CREDENTIALS_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/plugin_credentials.json",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            plugin_audit_dir: std::env::var("AGENT_HUB_PLUGIN_AUDIT_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/plugin_audit",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            plugin_data_dir: std::env::var("AGENT_HUB_PLUGIN_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/plugin_data",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
            plugin_scan_secs: PluginScanConfig::load(),
            plugin_transient_dirs: std::env::var("AGENT_HUB_PLUGIN_DIR")
                .ok()
                .map(|raw| {
                    raw.split(':')
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default(),
            plugin_mcp_dir: std::env::var("AGENT_HUB_PLUGIN_MCP_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    format!(
                        "{}/.claude-view/plugin_mcp",
                        crate::platform::home_dir_str()
                    )
                    .into()
                }),
        }
    }
}

/// `[plugins]` 配置段（ADR-0002 第 2 条）：周期扫描间隔（秒）。
/// 文件缺失/解析失败回退默认 10s；0 = 关闭周期扫描（启动扫描 + 手动刷新仍可用）。
struct PluginScanConfig;

impl PluginScanConfig {
    fn load() -> u64 {
        if let Ok(raw) = std::env::var("AGENT_HUB_PLUGIN_SCAN_SECS") {
            if let Ok(v) = raw.parse::<u64>() {
                return v;
            }
        }
        let path = config_path();
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return 10;
        };
        let Ok(val) = toml::from_str::<toml::Value>(&raw) else {
            return 10;
        };
        val.get("plugins")
            .and_then(|p| p.get("scan_interval_secs"))
            .and_then(toml::Value::as_integer)
            .map(|v| v.max(0) as u64)
            .unwrap_or(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// env var 是进程级全局态：本模块多个场景测试都改 AGENT_HUB_CONFIG，
    /// cargo test 默认并行，两个测试互踩会导致对方读到错文件（CI 实测翻车：
    /// chat 场景 2 期望 clamp 30 实得 20）。共享锁串行化全部动 env 的测试。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// env var 全局态 + cargo test 并行 → 三场景合并单测试顺序执行，防跨测试竞态
    #[test]
    fn chat_config_load_scenarios() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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

    /// env var 全局态 + cargo test 并行 → 五场景合并单测顺序执行
    #[test]
    fn opencode_config_load_scenarios() {
        let _guard = ENV_LOCK.lock().unwrap();
        // 场景 1：无 config.toml / 无 env → 默认 opencode@4097
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("AGENT_HUB_CONFIG", dir.path().join("none.toml"));
        std::env::remove_var("OPENCODE_BIN");
        std::env::remove_var("OPENCODE_PORT");
        let cfg = OpencodeConfig::load();
        assert_eq!(cfg.bin, "opencode");
        assert_eq!(cfg.port, 4097);

        // 场景 2：[opencode] 段生效
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "[opencode]\nbin = \"/opt/oc\"\nport = 5099\n").unwrap();
        std::env::set_var("AGENT_HUB_CONFIG", &file);
        let cfg = OpencodeConfig::load();
        assert_eq!(cfg.bin, "/opt/oc");
        assert_eq!(cfg.port, 5099);

        // 场景 3：env 覆盖文件（惯例 env > 文件 > 默认）
        std::env::set_var("OPENCODE_BIN", "/env/oc");
        std::env::set_var("OPENCODE_PORT", "6099");
        let cfg = OpencodeConfig::load();
        assert_eq!(cfg.bin, "/env/oc");
        assert_eq!(cfg.port, 6099);

        // 场景 4：坏 toml 回退默认
        std::env::remove_var("OPENCODE_BIN");
        std::env::remove_var("OPENCODE_PORT");
        std::fs::write(&file, "not [ valid toml").unwrap();
        let cfg = OpencodeConfig::load();
        assert_eq!(cfg.bin, "opencode");
        assert_eq!(cfg.port, 4097);

        std::env::remove_var("AGENT_HUB_CONFIG");
    }
}
