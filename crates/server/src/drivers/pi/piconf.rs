//! pi 原生配置读写（agent-hub-pi-hub-agent P10）：`~/.pi/agent/` 下 settings.json
//! 与 models.json 的投影读写。
//!
//! 格式事实（0.73.1 实测 + docs/models.md、docs/settings.md）：
//! - settings.json：`defaultProvider` / `defaultModel` / `defaultThinkingLevel`
//!   （off/minimal/low/medium/high/xhigh）——读写走键级 merge，**其他键保留**
//! - models.json：`{"providers": {"<name>": {baseUrl, api, apiKey, models:[…]}}}`；
//!   **自定义 provider 的 ak 存条目内 apiKey**（auth.json 是 OAuth 凭据形态
//!   refresh/access/expires，不是 ak 存放地——需求字面按实测修正）
//! - 兼容红线：已有手工条目（含其他 providers）保留；留空字段不写不覆盖
//! - 脱敏：ak 读取即掩码（`mask_key`），明文只在写路径瞬时存在；日志零明文
//!
//! 权限：两文件写后强制 0600（models.json 条目含 apiKey，同 auth.json 敏感级）。

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// pi agent 配置目录（`~/.pi/agent`；测试经 PI_AGENT_DIR 隔离）。
pub struct PiAgentConf {
    pub dir: std::path::PathBuf,
}

/// settings.json 投影（P10 关心三键；其余键 merge 保留）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSettings {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub default_thinking_level: Option<String>,
}

/// models.json `providers.<name>` 条目投影（ak 掩码态）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    pub base_url: Option<String>,
    /// 协议四选一：anthropic-messages / openai-completions / openai-responses /
    /// google-generative-ai（docs/models.md Supported APIs 全集）
    pub api: Option<String>,
    /// 掩码态（`mask_key` 产物）；明文不出读路径
    pub api_key_masked: Option<String>,
    /// 条目内模型 id 清单（保序）
    pub models: Vec<String>,
}

/// 掩码规则：≤8 字符全掩码 `••••`；>8 取前 2 + `…` + 后 4。None/空 → None。
pub fn mask_key(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() <= 8 {
        Some("••••".into())
    } else {
        let head: String = chars[..2].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        Some(format!("{head}…{tail}"))
    }
}

impl PiAgentConf {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }

    fn settings_file(&self) -> std::path::PathBuf {
        self.dir.join("settings.json")
    }

    /// models.json 路径（model-key-bug 存量自检等写前校验共用）。
    pub(crate) fn models_file(&self) -> std::path::PathBuf {
        self.dir.join("models.json")
    }

    /// 读 JSON 对象文件；缺失 → 空对象（不视为错误——首配场景）；坏 JSON → Err
    ///（配置坏值要显性报错，静默空对象会诱发「以为写上了」）。
    async fn read_obj(file: &std::path::Path) -> Result<Map<String, Value>> {
        let raw = match tokio::fs::read_to_string(file).await {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Map::new());
            }
            Err(e) => return Err(e).with_context(|| format!("读取 {} 失败", file.display())),
        };
        let v: Value = serde_json::from_str(&raw)
            .with_context(|| format!("{} 不是合法 JSON", file.display()))?;
        match v {
            Value::Object(m) => Ok(m),
            other => Err(anyhow::anyhow!(
                "{} 顶层应为对象，实为 {}",
                file.display(),
                other
            )),
        }
    }

    /// 原子写对象 + 0600（临时文件 + rename；敏感文件不留半截与宽权限窗口）。
    async fn write_obj(&self, file: &std::path::Path, obj: &Map<String, Value>) -> Result<()> {
        if let Some(dir) = file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let raw = serde_json::to_string_pretty(obj)?;
        let tmp = file.with_extension("json.tmp");
        tokio::fs::write(&tmp, raw).await?;
        // 0600 硬化（权限归驱动层）
        crate::platform::harden_0600_async(&tmp).await?;
        tokio::fs::rename(&tmp, file).await?;
        Ok(())
    }

    /// 读 settings.json 投影（缺失键 → None）。
    pub async fn read_settings(&self) -> Result<NativeSettings> {
        let m = Self::read_obj(&self.settings_file()).await?;
        let get = |k: &str| {
            m.get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Ok(NativeSettings {
            default_provider: get("defaultProvider"),
            default_model: get("defaultModel"),
            default_thinking_level: get("defaultThinkingLevel"),
        })
    }

    /// 键级 merge 写 settings.json：只写 Some 键，其他既有键（含 pi 内部键）原样保留。
    /// None = 清除该键（写 null 等价删除——设置页「留空用 pi 默认」语义）。
    pub async fn write_settings(&self, patch: &NativeSettings) -> Result<()> {
        let file = self.settings_file();
        let mut m = Self::read_obj(&file).await?;
        let apply = |m: &mut Map<String, Value>, key: &str, v: &Option<String>| {
            match v {
                Some(s) => m.insert(key.into(), Value::String(s.clone())),
                None => m.remove(key),
            };
        };
        apply(&mut m, "defaultProvider", &patch.default_provider);
        apply(&mut m, "defaultModel", &patch.default_model);
        apply(
            &mut m,
            "defaultThinkingLevel",
            &patch.default_thinking_level,
        );
        self.write_obj(&file, &m).await
    }

    /// 读 providers.<name> 条目投影（条目/文件缺失 → 空 None 形态）。
    pub async fn read_provider(&self, name: &str) -> Result<ProviderEntry> {
        let m = Self::read_obj(&self.models_file()).await?;
        let entry = m
            .get("providers")
            .and_then(Value::as_object)
            .and_then(|p| p.get(name))
            .and_then(Value::as_object);
        let Some(entry) = entry else {
            return Ok(ProviderEntry::default());
        };
        let get = |k: &str| {
            entry
                .get(k)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        Ok(ProviderEntry {
            base_url: get("baseUrl"),
            api: get("api"),
            api_key_masked: mask_key(entry.get("apiKey").and_then(Value::as_str)),
            models: entry
                .get("models")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// 写 providers.<name> 条目（merge 语义，兼容红线所在）：
    /// - 只动目标 `name` 键；**其他 providers 条目原样保留**（lmapi 等手工条目）
    /// - `api_key: None/空` → **保留条目既有 apiKey**（留空不覆盖）；Some → 覆盖
    /// - `base_url/api/models` Some 才写；条目内既有未知字段（headers/compat 等）
    ///   原样保留
    /// - models Some(ids) → 重写条目 models 数组为最小条目 `{"id": …}`（pi 最小
    ///   形态合法；手工条目的多字段模型不被本路径触碰——只动经 hub 写的条目）
    pub async fn write_provider(
        &self,
        name: &str,
        base_url: Option<&str>,
        api: Option<&str>,
        api_key: Option<&str>,
        models: Option<&[String]>,
    ) -> Result<()> {
        let file = self.models_file();
        let mut root = Self::read_obj(&file).await?;
        let providers = root
            .entry("providers")
            .or_insert_with(|| Value::Object(Map::new()));
        if !providers.is_object() {
            anyhow::bail!("models.json 的 providers 段不是对象，拒绝覆盖（防破坏手工配置）");
        }
        let map = providers.as_object_mut().expect("checked above");
        // 既有条目（可能手工）：clone 出来改，未提及字段原样保留
        let mut entry: Map<String, Value> = map
            .get(name)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(v) = base_url {
            entry.insert("baseUrl".into(), Value::String(v.into()));
        }
        if let Some(v) = api {
            entry.insert("api".into(), Value::String(v.into()));
        }
        match api_key.map(str::trim).filter(|s| !s.is_empty()) {
            Some(k) => {
                entry.insert("apiKey".into(), Value::String(k.into()));
            }
            None => {
                if !entry.contains_key("apiKey") {
                    // model-key-bug 回修：新条目未给 key 不再放空串占位——pi schema
                    // 对 apiKey 有 minLength 1 约束，空串会让**整个 models.json 拒载**
                    // （全部自定义条目失效，No API key found for the selected model）。
                    // 新条目无 key = 写进去即不可用 → 调用方写前拦截（见
                    // find_empty_api_key_entries 与 PUT 校验链）。
                    let _ = entry;
                }
            }
        }
        if let Some(ids) = models {
            let arr: Vec<Value> = ids
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(|id| {
                    Value::Object({
                        let mut m = Map::new();
                        m.insert("id".into(), Value::String(id.trim().into()));
                        m
                    })
                })
                .collect();
            entry.insert("models".into(), Value::Array(arr));
        }
        map.insert(name.into(), Value::Object(entry));
        self.write_obj(&file, &root).await
    }
}

// ---- model-key-bug：Model ∉ 自定义条目清单 = 死配置，写前拦截 ----

/// 校验「提交后条目形态」的 models 数组是否收纳 model（pi 按 provider 条目的
/// models 数组解析模型归属，无归属则 key 解析不到 → No API key found）。
/// 放行面：model 形如 `provider/id`（pi 显式跨条目引用语义）；条目不存在
/// （None——内建 provider 走 auth.json/内建注册表，非本缺陷面）。错误信息给
/// 两条修正路径（加入清单 / 改用清单内 id）。
pub fn validate_model_in_entry(
    provider: &str,
    model: &str,
    entry_models: Option<&[String]>,
) -> Result<(), String> {
    if model.contains('/') {
        return Ok(());
    }
    let Some(models) = entry_models else {
        return Ok(());
    };
    if models.iter().any(|m| m == model) {
        return Ok(());
    }
    Err(format!(
        "Model「{model}」不在 provider「{provider}」的模型清单内（pi 按条目清单解析模型归属，无归属则 key 解析不到）。请把 {model} 加入模型清单，或将 Model 改为清单内已有 id（当前清单：{models:?}）"
    ))
}

/// 扫描 models.json 中 apiKey 为空串的条目名（model-key-bug 存量自检）。
/// pi schema 对 apiKey 有 minLength 1：任一条目空串 = **整个 models.json 拒载**
/// （全部自定义条目失效）。命中即返回名单——PUT 写前拦截（写入即不可用），
/// 存量修复交用户手动（D3：手工条目零改写）。文件缺失/坏 JSON → 空名单
/// （坏 JSON 由 read_obj 各自报错，此处不重复报）。
pub async fn find_empty_api_key_entries(file: &std::path::Path) -> Vec<String> {
    let raw = match tokio::fs::read_to_string(file).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    let Some(providers) = v.get("providers").and_then(Value::as_object) else {
        return Vec::new();
    };
    providers
        .iter()
        .filter(|(_, entry)| {
            entry
                .get("apiKey")
                .and_then(Value::as_str)
                .is_some_and(str::is_empty)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod model_key_tests {
    use super::*;

    /// 死组合拦截（用户实机同款：m-x3 ∉ 清单）。
    #[test]
    fn dead_combo_rejected() {
        let models = vec!["claude-opus-5".into(), "claude-sonnet-5".into()];
        let err = validate_model_in_entry("prov-x3", "m-x3", Some(&models)).unwrap_err();
        assert!(err.contains("m-x3") && err.contains("prov-x3"));
        assert!(err.contains("加入模型清单"), "错误信息应给修正路径");
        // 合法组合放行
        assert!(validate_model_in_entry("prov-x3", "claude-opus-5", Some(&models)).is_ok());
    }

    /// 放行面：provider/id 显式形态；条目不存在（内建 provider 面）。
    #[test]
    fn passthrough_faces() {
        assert!(validate_model_in_entry("prov-x3", "other/id-x", None).is_ok());
        assert!(validate_model_in_entry("prov-x3", "m-x3", None).is_ok());
        assert!(validate_model_in_entry("anthropic", "claude-opus-5", None).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf(dir: &std::path::Path) -> PiAgentConf {
        PiAgentConf::new(dir.to_path_buf())
    }

    /// settings 三键 roundtrip；非本单键保留（pi 内部键不被清）。
    #[tokio::test]
    async fn settings_merge_preserves_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let c = conf(dir.path());
        std::fs::write(
            c.settings_file(),
            r#"{"defaultProvider":"anthropic","defaultModel":"m1","hideThinkingBlock":true}"#,
        )
        .unwrap();
        c.write_settings(&NativeSettings {
            default_provider: Some("lmapi".into()),
            default_model: None, // 清除
            default_thinking_level: Some("high".into()),
        })
        .await
        .unwrap();
        let raw: Value =
            serde_json::from_str(&std::fs::read_to_string(c.settings_file()).unwrap()).unwrap();
        assert_eq!(raw["defaultProvider"], "lmapi");
        assert_eq!(raw["defaultThinkingLevel"], "high");
        assert_eq!(raw["defaultModel"], Value::Null);
        assert!(raw.get("defaultModel").is_none(), "None 语义 = 移除键");
        assert_eq!(raw["hideThinkingBlock"], true, "非本单键保留");
    }

    /// 坏 JSON 显性报错（不静默空对象——防「以为写上了」）。
    #[tokio::test]
    async fn bad_json_is_explicit_error() {
        let dir = tempfile::tempdir().unwrap();
        let c = conf(dir.path());
        std::fs::write(c.settings_file(), "not json{").unwrap();
        assert!(c.read_settings().await.is_err());
    }

    /// models.json 写条目：其他 providers 保留 + 未提及字段保留 + ak 留空不覆盖。
    #[tokio::test]
    async fn provider_write_merge_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let c = conf(dir.path());
        std::fs::write(
            c.models_file(),
            r#"{"providers":{
                "lmapi":{"baseUrl":"https://x","api":"anthropic-messages","apiKey":"ah-secret","models":[{"id":"m0"}]},
                "keepme":{"baseUrl":"https://keep","api":"openai-completions","apiKey":"k","models":[{"id":"k0"}]}
            }}"#,
        )
        .unwrap();
        // 写 lmapi：不给 ak（保留 ah-secret）、不给 models（保留既有数组）
        c.write_provider(
            "lmapi",
            Some("https://new"),
            Some("anthropic-messages"),
            None,
            None,
        )
        .await
        .unwrap();
        let raw: Value =
            serde_json::from_str(&std::fs::read_to_string(c.models_file()).unwrap()).unwrap();
        let lm = &raw["providers"]["lmapi"];
        assert_eq!(lm["baseUrl"], "https://new");
        assert_eq!(lm["apiKey"], "ah-secret", "留空 ak 不得覆盖");
        assert_eq!(lm["models"][0]["id"], "m0", "未提及 models 保留");
        assert_eq!(
            raw["providers"]["keepme"]["baseUrl"], "https://keep",
            "其他条目保留"
        );
    }

    /// 新条目 + models 重写为最小形态 + 掩码读取。
    #[tokio::test]
    async fn provider_new_entry_models_rewrite_and_mask() {
        let dir = tempfile::tempdir().unwrap();
        let c = conf(dir.path());
        c.write_provider(
            "my-relay",
            Some("https://relay.example/v1"),
            Some("openai-completions"),
            Some("sk-verysecretkey123"),
            Some(&["gpt-x".into(), "gpt-y".into()]),
        )
        .await
        .unwrap();
        let e = c.read_provider("my-relay").await.unwrap();
        assert_eq!(e.base_url.as_deref(), Some("https://relay.example/v1"));
        assert_eq!(e.api.as_deref(), Some("openai-completions"));
        assert_eq!(e.models, vec!["gpt-x", "gpt-y"]);
        let masked = e.api_key_masked.unwrap();
        assert!(
            !masked.contains("verysecret"),
            "掩码不得含中段明文: {masked}"
        );
        assert!(masked.starts_with("sk") && masked.ends_with("y123"));
        // 0600 权限（unix）
        #[cfg(unix)]
        {
            let mode = crate::platform::file_mode(&c.models_file()).unwrap();
            assert_eq!(mode, 0o600, "models.json 应 0600");
        }
    }

    /// 掩码规则边界。
    #[test]
    fn mask_key_rules() {
        assert_eq!(mask_key(None), None);
        assert_eq!(mask_key(Some("  ")), None);
        assert_eq!(mask_key(Some("short")), Some("••••".to_string()));
        let m = mask_key(Some("ah-0123456789abcdef")).unwrap();
        assert!(m.starts_with("ah") && m.ends_with("cdef") && m.contains('…'));
    }
}

// ---- P10 增量/P11：hub 侧助手 flags（pi_settings.json 复活只存 flags；provider/
// model 已切原生配置，旧键残留不读不写）----
//
// 结构简单到不值得独立文件；随 settings_file（hub 数据目录）走。

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flags {
    /// 允许助手经 hub_session_send 注入会话（默认关——授权双闸之一）
    #[serde(default)]
    pub allow_session_inject: bool,
    /// P13：允许助手控制定时任务（启停/手动触发；默认关——与 inject 同款
    /// 服务端闸，防 prompt injection 诱导写操作；hub_tasks_list 只读不设闸）
    #[serde(default)]
    pub allow_task_control: bool,
    /// P11：助手工作目录（None = 默认 $HOME/agent-hub）——pi 进程 cwd，
    /// 助手的文件操作根（与 extension 工具的 hub API 调用无耦合）
    #[serde(default)]
    pub work_dir: Option<String>,
}

/// 读 flags 文件（缺失/坏值 → 默认全关）。
pub async fn read_flags(file: &std::path::Path) -> Flags {
    match tokio::fs::read_to_string(file).await {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
        Err(_) => Flags::default(),
    }
}

/// P11：解析工作目录（None/空 → 默认 $HOME/agent-hub）+ 启动前校验：
/// mkdir -p、指向文件/父不可建 → Err 明示原因（验收：非法路径报错不启动）。
pub async fn resolve_work_dir(raw: Option<&str>) -> Result<std::path::PathBuf> {
    let dir = match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => std::path::PathBuf::from(s),
        None => crate::platform::home_dir_opt()
            .context("HOME 环境变量未设置")?
            .join("agent-hub"),
    };
    if dir.exists() && !dir.is_dir() {
        anyhow::bail!("工作目录路径已存在且不是目录: {}", dir.display());
    }
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("工作目录创建失败: {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod p11_tests {
    use super::*;

    /// 工作目录：默认路径创建；非法路径（指向文件）Err 明示。
    #[tokio::test]
    async fn work_dir_resolution_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        // 显式路径：不存在 → 创建
        let wd = resolve_work_dir(Some(dir.path().join("ws/deep").to_str().unwrap()))
            .await
            .unwrap();
        assert!(wd.is_dir());
        // 指向文件 → Err
        let f = dir.path().join("afile");
        std::fs::write(&f, "x").unwrap();
        let err = resolve_work_dir(Some(f.to_str().unwrap()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("不是目录"));
        // 空白 → 默认 $HOME/agent-hub（env HOME 指向 tempdir 时）
        std::env::set_var("HOME", dir.path());
        let wd = resolve_work_dir(Some("  ")).await.unwrap();
        assert!(wd.ends_with("agent-hub"));
        assert!(wd.is_dir());
    }
}
