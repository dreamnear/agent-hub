//! 凭据代持（ADR-0007 第 4 条 / BR-10/11）。
//!
//! - 值只存 hub 侧 `plugin_credentials.json`（0600）；
//! - 注入路径仅两条：backend 启动 env 的 `$cred:<name>` 占位替换、`hub.credentials.use`
//!   的应答载荷；
//! - 一切发往日志/错误响应/审计产物的字符串先过 [`CredentialsStore::scrub`]。
//!
//! 零回显纪律：本模块的 `Debug` 实现不输出值；对外只暴露键名与「已配置」态。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub name: String,
    pub configured: bool,
}

pub struct CredentialsStore {
    file: PathBuf,
    values: RwLock<BTreeMap<String, String>>,
}

impl std::fmt::Debug for CredentialsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 值零回显：只打文件名与键数量
        let n = self.values.read().map(|v| v.len()).unwrap_or(0);
        f.debug_struct("CredentialsStore")
            .field("file", &self.file)
            .field("names", &n)
            .finish()
    }
}

impl CredentialsStore {
    /// 从文件加载（缺失 = 空表；解析失败 = 空表 + warn，不回显内容）。
    pub fn load(file: PathBuf) -> Self {
        let values = match std::fs::read_to_string(&file) {
            Ok(raw) => match serde_json::from_str::<BTreeMap<String, String>>(&raw) {
                Ok(map) => map,
                Err(e) => {
                    // 只打错误类别，不回显文件内容（零回显铁律）
                    tracing::warn!(file = %file.display(), error = %e, "plugin_credentials.json 解析失败，按空表处理");
                    BTreeMap::new()
                }
            },
            Err(_) => BTreeMap::new(),
        };
        Self {
            file,
            values: RwLock::new(values),
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// 取凭据值（仅内部注入路径调用；调用方负责不回显）。
    pub fn get(&self, name: &str) -> Option<String> {
        self.values.read().ok()?.get(name).cloned()
    }

    pub fn is_configured(&self, name: &str) -> bool {
        self.values
            .read()
            .map(|v| v.contains_key(name))
            .unwrap_or(false)
    }

    /// 声明清单 → UI 状态（只给名与已配置态，值零回显）。
    pub fn statuses(&self, names: &[String]) -> Vec<CredentialStatus> {
        names
            .iter()
            .map(|n| CredentialStatus {
                name: n.clone(),
                configured: self.is_configured(n),
            })
            .collect()
    }

    /// 设置 / 覆盖一个凭据值并原子落盘（0600）。
    /// 值只在 hub 侧落盘与注入路径出现；调用方（HTTP handler）**不得**把它写进响应或日志。
    pub fn set(&self, name: &str, value: &str) -> std::io::Result<()> {
        let mut guard = self.values.write().unwrap_or_else(|e| e.into_inner());
        guard.insert(name.to_string(), value.to_string());
        Self::persist(&self.file, &guard)
    }

    /// 清除一个凭据值；返回是否确实存在过（不存在也视为成功，幂等）。
    pub fn remove(&self, name: &str) -> std::io::Result<bool> {
        let mut guard = self.values.write().unwrap_or_else(|e| e.into_inner());
        let removed = guard.remove(name).is_some();
        if removed {
            Self::persist(&self.file, &guard)?;
        }
        Ok(removed)
    }

    /// 原子落盘（tmp + rename）+ 强制 0600（同 instances.json 惯例）。
    fn persist(file: &Path, values: &BTreeMap<String, String>) -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let raw = serde_json::to_string_pretty(values)?;
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, raw)?;
        std::fs::rename(&tmp, file)?;
        crate::platform::harden_0600(file);
        Ok(())
    }

    /// 所有已配置凭据的值被替换为 `<redacted>`。
    /// 落盘故障摘要、错误响应、日志输出前必过本函数。
    pub fn scrub(&self, s: &str) -> String {
        let Ok(values) = self.values.read() else {
            return s.to_string();
        };
        let mut out = s.to_string();
        for v in values.values() {
            // 空值不做替换（否则会把空串位置全打上标记）
            if v.len() >= 3 && out.contains(v) {
                out = out.replace(v, "<redacted>");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(pairs: &[(&str, &str)]) -> CredentialsStore {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plugin_credentials.json");
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        std::fs::write(&file, serde_json::to_string(&map).unwrap()).unwrap();
        CredentialsStore::load(file)
    }

    #[test]
    fn statuses_never_expose_values() {
        let s = store_with(&[("gitlab_pat", "super-secret-token")]);
        let st = s.statuses(&["gitlab_pat".into(), "other".into()]);
        assert_eq!(st[0].name, "gitlab_pat");
        assert!(st[0].configured);
        assert!(!st[1].configured);
        let json = serde_json::to_string(&st).unwrap();
        assert!(!json.contains("super-secret-token"));
    }

    #[test]
    fn scrub_redacts_configured_values() {
        let s = store_with(&[("gitlab_pat", "super-secret-token")]);
        let out = s.scrub("request failed with token super-secret-token (401)");
        assert!(out.contains("<redacted>"));
        assert!(!out.contains("super-secret-token"));
    }

    #[test]
    fn debug_does_not_leak_values() {
        let s = store_with(&[("gitlab_pat", "super-secret-token")]);
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("super-secret-token"));
    }

    #[test]
    fn set_and_remove_round_trip_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plugin_credentials.json");
        let s = CredentialsStore::load(file.clone());
        s.set("gitlab_pat", "super-secret-token").unwrap();
        assert!(s.is_configured("gitlab_pat"));
        assert_eq!(s.get("gitlab_pat").as_deref(), Some("super-secret-token"));
        // 重新加载（同文件）可读回：落盘真实生效
        let s2 = CredentialsStore::load(file.clone());
        assert_eq!(s2.get("gitlab_pat").as_deref(), Some("super-secret-token"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "凭据文件必须 0600");
        }
        // 覆盖写
        s.set("gitlab_pat", "second-secret-value").unwrap();
        assert_eq!(s.get("gitlab_pat").as_deref(), Some("second-secret-value"));
        // 清除（幂等）
        assert!(s.remove("gitlab_pat").unwrap());
        assert!(!s.is_configured("gitlab_pat"));
        assert!(!s.remove("gitlab_pat").unwrap());
        assert!(CredentialsStore::load(file).get("gitlab_pat").is_none());
    }

    #[test]
    fn missing_file_is_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let s = CredentialsStore::load(dir.path().join("nope.json"));
        assert!(!s.is_configured("gitlab_pat"));
        assert_eq!(s.scrub("nothing"), "nothing");
    }
}
