//! node 形态身份文件 + N6 传输红线（agent-hub-gateway-node-p1 任务 2）。
//!
//! - node.json（`~/.claude-view/node.json`，env `AGENT_HUB_NODE_FILE` 覆盖）：
//!   {gatewayUrl, nodeId, nodeName, token, allowInsecure}，0600 原子落盘（含
//!   node token——节点完全控制权凭据，同 instances.json 纪律）；nodeId 首启
//!   UUIDv4 生成后不变。enroll code 是一次性凭据，**不落盘**（只随当次泵参数）。
//! - N6 红线（proposal 红线 3）：gateway_url 非 loopback 且非 wss 且未显式
//!   `--allow-insecure` → 拒绝。校验在两处生效：启动入口 + [`NodeConfig::save`]
//!   （落盘前再验，防旁路写入绕过 argv 校验）。

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::platform;

/// node.json 内容（camelCase 对齐 instances.json 惯例）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeConfig {
    pub gateway_url: String,
    /// 首启生成（UUIDv4），此后不变——网关审批与 token 绑定的身份键
    pub node_id: String,
    /// 审批 UI 展示名（默认 hostname）
    pub node_name: String,
    /// 配对后签发的长效凭据（hello-ok 首发，0600 落盘；None = 未配对）
    #[serde(default)]
    pub token: Option<String>,
    /// N6 豁免标志（--allow-insecure 显式给定后随身份持久化，重启沿用同一取舍）
    #[serde(default)]
    pub allow_insecure: bool,
}

/// N6 传输红线校验（纯函数，四态单测锚点）：
/// 1. scheme 必须 ws/wss（WS 拨号器不认 http/https，早失败带引导）；
/// 2. loopback 主机 → 放行（隔离测试/本机网关）；
/// 3. 非 loopback 且 wss → 放行（frp 证书路径）；
/// 4. 非 loopback 且明文 ws → 仅 `allow_insecure` 豁免，否则拒绝带明确文案。
pub fn validate_gateway_url(url: &str, allow_insecure: bool) -> Result<(), String> {
    let (scheme, host) = crate::instances::parse_url_scheme_host(url)?;
    if scheme != "ws" && scheme != "wss" {
        return Err(format!(
            "gateway 地址需为 ws:// 或 wss://（收到 {scheme}://）；本机网关用 \
             ws://127.0.0.1:端口，跨公网用 wss://（frp 证书已就绪）"
        ));
    }
    if crate::instances::is_loopback_host(&host) {
        return Ok(());
    }
    if scheme == "wss" {
        return Ok(());
    }
    if allow_insecure {
        return Ok(());
    }
    Err(
        "gateway 地址非 loopback 且非 wss：明文 ws 跨网传输被拒绝——node token \
         等于节点完全控制权，不允许明文裸奔。改用 wss://，或测试/LAN 场景显式加 \
         --allow-insecure 豁免"
            .into(),
    )
}

/// node_name 缺省：hostname 命令输出；不可得回退 "node"（仅启动期一次同步调用）。
pub fn default_node_name() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "node".into())
}

impl NodeConfig {
    /// 读取既有 node.json（缺失/坏 JSON → None，由调用方决定初始化）。
    pub fn load(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str(&raw) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "node.json 解析失败，按未初始化处理");
                None
            }
        }
    }

    /// 原子落盘（0600 临时文件 → rename，同 instances.json 惯例）。保存前执行
    /// N6 校验（红线第二处），拒绝明文跨网配置经任何路径落盘。
    pub async fn save(&self, path: &Path) -> Result<()> {
        validate_gateway_url(&self.gateway_url, self.allow_insecure).map_err(anyhow::Error::msg)?;
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        let tmp = path.with_extension("json.tmp");
        let mut f = platform::private_write_options()
            .open(&tmp)
            .await
            .with_context(|| format!("node.json 临时文件创建失败: {}", tmp.display()))?;
        use tokio::io::AsyncWriteExt;
        f.write_all(raw.as_bytes()).await?;
        f.sync_all().await?;
        tokio::fs::rename(&tmp, path).await?;
        platform::harden_0600(path);
        Ok(())
    }
}

/// node 形态最小 argv 解析（不引 clap）：`--node` `--gateway <url>` `--enroll <code>`
/// `--node-name <name>` `--allow-insecure`。未知参数忽略（hub 二进制同体复用）；
/// 取值参数后缺值时按 None 处理。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeArgs {
    pub node: bool,
    pub gateway: Option<String>,
    pub enroll: Option<String>,
    pub node_name: Option<String>,
    pub allow_insecure: bool,
}

impl NodeArgs {
    pub fn parse<I: Iterator<Item = String>>(args: I) -> Self {
        let mut out = Self::default();
        let mut it = args.peekable();
        while let Some(a) = it.next() {
            let value_of = |it: &mut std::iter::Peekable<I>| -> Option<String> {
                it.peek().filter(|v| !v.starts_with("--")).cloned()
            };
            match a.as_str() {
                "--node" => out.node = true,
                "--allow-insecure" => out.allow_insecure = true,
                "--gateway" => {
                    if let Some(v) = value_of(&mut it) {
                        it.next();
                        out.gateway = Some(v);
                    }
                }
                "--enroll" => {
                    if let Some(v) = value_of(&mut it) {
                        it.next();
                        out.enroll = Some(v);
                    }
                }
                "--node-name" => {
                    if let Some(v) = value_of(&mut it) {
                        it.next();
                        out.node_name = Some(v);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// N6 四态：loopback ws 豁免 / wss 放行 / 明文非 loopback 拒 / allow-insecure 豁免
    #[test]
    fn n6_gateway_url_four_states() {
        // loopback（127.0.0.1 / localhost）明文 ws 放行（口径同 instances 直连红线；
        // 方括号 IPv6 字面量属共享解析器的既有边界，此处不扩大）
        validate_gateway_url("ws://127.0.0.1:7901", false).unwrap();
        validate_gateway_url("ws://localhost:7901", false).unwrap();
        // 非 loopback wss 放行
        validate_gateway_url("wss://hub.example.com", false).unwrap();
        // 非 loopback 明文 ws 无豁免 → 拒（文案引导 wss / --allow-insecure）
        let e = validate_gateway_url("ws://192.168.1.5:7901", false).unwrap_err();
        assert!(
            e.contains("wss") && e.contains("--allow-insecure"),
            "文案: {e}"
        );
        // 同地址显式豁免 → 放行
        validate_gateway_url("ws://192.168.1.5:7901", true).unwrap();
    }

    /// N6 附加：scheme 门（http/https 及坏 URL 早失败带引导，不进拨号器才报错）
    #[test]
    fn n6_scheme_gate() {
        let e = validate_gateway_url("https://hub.example.com", false).unwrap_err();
        assert!(e.contains("ws://"), "文案引导 ws(s)://: {e}");
        assert!(validate_gateway_url("not-a-url", false).is_err());
        // 空 host
        assert!(validate_gateway_url("ws://", false).is_err());
    }

    /// node.json 落盘 0600（镜像 instances saved_file_is_0600）+ 往返一致
    #[tokio::test]
    async fn node_json_is_0600_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("node.json");
        let cfg = NodeConfig {
            gateway_url: "wss://hub.example.com".into(),
            node_id: "n-1".into(),
            node_name: "lab".into(),
            token: Some("tok-1".into()),
            allow_insecure: false,
        };
        cfg.save(&file).await.unwrap();
        let mode = platform::file_mode(&file).unwrap();
        assert_eq!(mode, 0o600, "含 node token 的身份文件必须 0600");
        assert_eq!(NodeConfig::load(&file), Some(cfg.clone()));
        // 二次写入仍 0600
        let mut cfg2 = cfg.clone();
        cfg2.token = None;
        cfg2.save(&file).await.unwrap();
        assert_eq!(platform::file_mode(&file).unwrap(), 0o600);
        assert_eq!(NodeConfig::load(&file).unwrap().token, None);
    }

    /// 红线第二处：save 落盘前 N6 校验——明文跨网配置经任何路径拒绝落盘
    #[tokio::test]
    async fn save_rejects_insecure_cross_network() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("node.json");
        let cfg = NodeConfig {
            gateway_url: "ws://192.168.1.5:7901".into(),
            node_id: "n-1".into(),
            node_name: "lab".into(),
            token: None,
            allow_insecure: false,
        };
        let err = cfg.save(&file).await.unwrap_err();
        assert!(err.to_string().contains("--allow-insecure"), "文案: {err}");
        assert!(!file.exists(), "被拒配置不得落盘");
    }

    /// load 容错：缺失 → None；坏 JSON → None（warn 不炸）
    #[test]
    fn load_missing_or_bad_gives_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(NodeConfig::load(&dir.path().join("nope.json")), None);
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "not json").unwrap();
        assert_eq!(NodeConfig::load(&bad), None);
    }

    /// argv 解析：旗标/取值/缺值/未知参数忽略
    #[test]
    fn node_args_parse() {
        let a = NodeArgs::parse(
            [
                "--node",
                "--gateway",
                "ws://127.0.0.1:7901",
                "--enroll",
                "enr-1",
                "--node-name",
                "lab",
                "--allow-insecure",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        assert!(a.node && a.allow_insecure);
        assert_eq!(a.gateway.as_deref(), Some("ws://127.0.0.1:7901"));
        assert_eq!(a.enroll.as_deref(), Some("enr-1"));
        assert_eq!(a.node_name.as_deref(), Some("lab"));

        // 取值参数缺值（尾随 --gateway）→ None 不 panic；未知参数忽略
        let b = NodeArgs::parse(
            ["--node", "--gateway", "--other"]
                .iter()
                .map(|s| s.to_string()),
        );
        assert!(b.node);
        assert_eq!(b.gateway, None, "--gateway 后跟旗标视为缺值");
        assert_eq!(
            NodeArgs::parse(std::iter::empty()),
            NodeArgs::default(),
            "空 argv = 非节点形态"
        );
        // 值本身以 -- 开头不吞（防御 URL 恰似旗标的歧义：按缺值处理）
        let c = NodeArgs::parse(["--gateway"].iter().map(|s| s.to_string()));
        assert_eq!(c.gateway, None);
    }
}
