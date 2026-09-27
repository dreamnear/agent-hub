//! 多实例配置模型 + 持久化（agent-hub-multi-instance 批1 任务2）。
//! 文件 `~/.claude-view/instances.json`（env `INSTANCES_FILE` 覆盖，惯例同 notes.json）；
//! 权限强制 0600——内容含远程实例 token 与 ssh 密码。token/密码不进任何日志/tracing，
//! 但 **会经 GET /api/instances 下发给浏览器**——浏览器直连方案固有属性（信任域
//! 同 hub token），tasks.md 风险已标注；联邦网关是升级路径（本期明确不做）。

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::config::Config;

/// 实例连接模式
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstanceMode {
    Direct,
    SshTunnel,
}

/// SSH 认证方式（三选）：证书路径 / authsock / 密码
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SshAuth {
    #[default]
    KeyPath,
    Authsock,
    Password,
}

/// SSH 隧道连接参数
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfig {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub auth: SshAuth,
    pub key_path: Option<String>,
    /// 密码（可选）：仅经 sshpass -e 环境变量传给子进程，不进 argv 不落盘明文日志
    pub password: Option<String>,
}

const fn default_ssh_port() -> u16 {
    22
}

/// 一条实例配置（扁平结构便于前端表单双向绑定与序列化）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceConfig {
    pub id: String,
    pub name: String,
    pub mode: InstanceMode,
    /// direct：完整 base url（远程必须 https，本机 http 例外）
    pub url: Option<String>,
    /// direct：远程实例 token；ssh-tunnel：可选（远程如强制认证用）
    pub token: Option<String>,
    /// ssh-tunnel：连接参数
    pub ssh: Option<SshConfig>,
    /// ssh-tunnel：远程目标端口
    pub remote_port: Option<u16>,
    /// ssh-tunnel：动态分配的本地端口（隧道 start 后填充；持久化供前端连接）
    pub local_port: Option<u16>,
}

impl InstanceConfig {
    /// URL 红线校验（验收条目 4）：direct 远程（非 loopback 主机）非 https 拒绝。
    /// 本机 http（localhost/127.0.0.1/::1）放行。ssh-tunnel 不需要 url（SSH 加密内建）。
    pub fn validate(&self) -> Result<(), String> {
        match self.mode {
            InstanceMode::SshTunnel => {
                let ssh = self
                    .ssh
                    .as_ref()
                    .ok_or("ssh-tunnel 模式缺少 ssh 连接参数")?;
                if ssh.host.trim().is_empty() {
                    return Err("SSH 主机不能为空".into());
                }
                if ssh.user.trim().is_empty() {
                    return Err("SSH 用户名不能为空".into());
                }
                let rp = self.remote_port.ok_or("ssh-tunnel 模式缺少远程目标端口")?;
                if rp == 0 {
                    return Err("远程目标端口无效".into());
                }
                if ssh.auth == SshAuth::KeyPath {
                    let kp = ssh
                        .key_path
                        .as_ref()
                        .ok_or("证书路径认证需提供密钥文件路径")?;
                    if kp.trim().is_empty() {
                        return Err("证书路径认证需提供密钥文件路径".into());
                    }
                }
                Ok(())
            }
            InstanceMode::Direct => {
                let url = self.url.as_ref().ok_or("direct 模式缺少 URL")?;
                let (scheme, host) = parse_url_scheme_host(url)?;
                if is_loopback_host(&host) {
                    return Ok(()); // 本机 http 例外（允许用户配 localhost/127.0.0.1 http）
                }
                if scheme != "https" {
                    return Err(
                        "远程实例必须使用 https（frp 入口明文裸奔公网不可接受；本机可用 http）"
                            .into(),
                    );
                }
                Ok(())
            }
        }
    }

    /// 该实例当前可达的 base url（不含路径尾斜杠）：
    /// direct → 配置 url；ssh-tunnel → http://127.0.0.1:{local_port}
    pub fn base_url(&self) -> Result<String, String> {
        match self.mode {
            InstanceMode::Direct => Ok(self.url.clone().ok_or("direct 实例缺少 url")?),
            InstanceMode::SshTunnel => {
                let lp = self
                    .local_port
                    .ok_or("ssh-tunnel 实例尚未分配本地端口（先 start 隧道）")?;
                Ok(format!("http://127.0.0.1:{lp}"))
            }
        }
    }
}

// 轻量 scheme://host 解析（避免引入 url 依赖）：仅取 scheme 与 host 判 loopback/https。
fn parse_url_scheme_host(s: &str) -> Result<(String, String), String> {
    let (scheme, rest) = s
        .split_once("://")
        .ok_or_else(|| "URL 格式无效".to_string())?;
    if scheme.is_empty() || rest.is_empty() {
        return Err("URL 格式无效".into());
    }
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_string();
    if host.is_empty() {
        return Err("URL 缺少主机名".into());
    }
    Ok((scheme.to_lowercase(), host.to_lowercase()))
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0:0:0:0:0:0:0:1")
}

/// 进程内实例配置写互斥（并发 load-modify-write 防丢更新；对齐 notes io_lock 惯例）。
fn io_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Debug, Clone)]
pub struct InstancesStore {
    pub file: PathBuf,
}

impl InstancesStore {
    pub fn new(cfg: &Config) -> Self {
        Self {
            file: cfg.instances_file.clone(),
        }
    }

    /// 全量读入（文件缺失/坏 JSON → 空列表，对齐 projects.json 容错惯例）。
    pub async fn load(&self) -> Vec<InstanceConfig> {
        match tokio::fs::read_to_string(&self.file).await {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn get(&self, id: &str) -> Option<InstanceConfig> {
        self.load().await.into_iter().find(|c| c.id == id)
    }

    /// 原子落盘整文件（0600 临时文件 → rename）；与 notes.rs set 同构。
    async fn save(&self, instances: &[InstanceConfig]) -> anyhow::Result<()> {
        let _guard = io_lock().lock().await;
        if let Some(dir) = self.file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let raw = serde_json::to_string_pretty(instances)?;
        let tmp = self.file.with_extension("json.tmp");
        let mut opts = tokio::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut f = opts.open(&tmp).await?;
        use tokio::io::AsyncWriteExt;
        f.write_all(raw.as_bytes()).await?;
        f.sync_all().await?;
        tokio::fs::rename(&tmp, &self.file).await?;
        Ok(())
    }

    /// 追加/覆写一条实例（id 已存在则替换；否则保留顺序追加）。
    pub async fn upsert(&self, cfg: &InstanceConfig) -> anyhow::Result<()> {
        let mut list = self.load().await;
        if let Some(existing) = list.iter_mut().find(|c| c.id == cfg.id) {
            *existing = cfg.clone();
        } else {
            list.push(cfg.clone());
        }
        self.save(&list).await
    }

    /// 删除一条实例；返回被删条目（隧道管理器据此 stop）。
    pub async fn delete(&self, id: &str) -> anyhow::Result<Option<InstanceConfig>> {
        let mut list = self.load().await;
        let Some(idx) = list.iter().position(|c| c.id == id) else {
            return Ok(None);
        };
        let removed = list.remove(idx);
        self.save(&list).await?;
        Ok(Some(removed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(mode: InstanceMode, url: &str) -> InstanceConfig {
        InstanceConfig {
            id: "i1".into(),
            name: "x".into(),
            mode,
            url: Some(url.into()),
            token: Some("t".into()),
            ssh: None,
            remote_port: None,
            local_port: None,
        }
    }

    /// 验收红线：http 远程 direct 拒绝（含明确文案）；https 远程 / 本机 http 放行
    #[test]
    fn direct_url_redline() {
        let e = direct(InstanceMode::Direct, "http://192.168.1.5:7800")
            .validate()
            .unwrap_err();
        assert!(e.contains("https"), "拒绝文案应含 https 引导: {e}");
        direct(InstanceMode::Direct, "https://hub.example.com")
            .validate()
            .unwrap();
        // 本机 http 例外
        direct(InstanceMode::Direct, "http://localhost:7800")
            .validate()
            .unwrap();
        direct(InstanceMode::Direct, "http://127.0.0.1:7800")
            .validate()
            .unwrap();
        // 无 scheme / 坏 url
        direct(InstanceMode::Direct, "not-a-url")
            .validate()
            .unwrap_err();
    }

    /// ssh-tunnel：缺参校验、key-path 认证必填证书路径
    #[test]
    fn ssh_validation() {
        let mk = |auth: SshAuth, key: Option<&str>, rp: Option<u16>| InstanceConfig {
            id: "i1".into(),
            name: "x".into(),
            mode: InstanceMode::SshTunnel,
            url: None,
            token: None,
            ssh: Some(SshConfig {
                host: "example.com".into(),
                port: 22,
                user: "u".into(),
                auth,
                key_path: key.map(String::from),
                password: None,
            }),
            remote_port: rp,
            local_port: None,
        };
        mk(SshAuth::Authsock, None, Some(22)).validate().unwrap();
        // key-path 认证缺证书路径 → 拒绝
        let e = mk(SshAuth::KeyPath, None, Some(22)).validate().unwrap_err();
        assert!(e.contains("密钥文件路径"));
        mk(SshAuth::KeyPath, Some("/x/id_rsa"), Some(22))
            .validate()
            .unwrap();
        // 缺 remote_port → 拒绝
        mk(SshAuth::Authsock, None, None).validate().unwrap_err();
    }

    #[tokio::test]
    async fn store_upsert_delete_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstancesStore {
            file: dir.path().join("instances.json"),
        };
        let mut c = direct(InstanceMode::Direct, "https://a.example.com");
        c.id = "a".into();
        store.upsert(&c).await.unwrap();
        let mut d = direct(InstanceMode::Direct, "http://127.0.0.1:7800");
        d.id = "b".into();
        store.upsert(&d).await.unwrap();
        assert_eq!(store.load().await.len(), 2);
        assert_eq!(
            store.get("a").await.unwrap().url.as_deref(),
            Some("https://a.example.com")
        );

        // id 已存在 → 覆写不新增
        let mut c2 = c.clone();
        c2.name = "renamed".into();
        store.upsert(&c2).await.unwrap();
        assert_eq!(store.load().await.len(), 2);

        // 删除
        let removed = store.delete("a").await.unwrap();
        assert!(removed.is_some());
        assert_eq!(store.load().await.len(), 1);
        assert!(store.get("a").await.is_none());
    }

    /// 验收红线：落盘 0600（含 token/密码）
    #[cfg(unix)]
    #[tokio::test]
    async fn saved_file_is_0600() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstancesStore {
            file: dir.path().join("instances.json"),
        };
        store
            .upsert(&direct(InstanceMode::Direct, "https://a.example.com"))
            .await
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&store.file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // 二次写入仍 0600
        store
            .upsert(&direct(InstanceMode::Direct, "https://b.example.com"))
            .await
            .unwrap();
        let mode = std::fs::metadata(&store.file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// base_url 语义：direct 回配置 url；ssh-tunnel 回 localhost:local_port；未分配本地端口报错
    #[test]
    fn base_url_semantics() {
        let d = direct(InstanceMode::Direct, "https://a.example.com");
        assert_eq!(d.base_url().unwrap(), "https://a.example.com");
        let mut s = InstanceConfig {
            id: "i1".into(),
            name: "x".into(),
            mode: InstanceMode::SshTunnel,
            url: None,
            token: None,
            ssh: None,
            remote_port: Some(8080),
            local_port: None,
        };
        assert!(s.base_url().is_err(), "未分配本地端口应报错");
        s.local_port = Some(44123);
        assert_eq!(s.base_url().unwrap(), "http://127.0.0.1:44123");
    }
}
