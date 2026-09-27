//! 工程便签持久化（agent-hub-notes）：归一化 cwd → Markdown 内容的 JSON 映射。
//! 文件 `~/.claude-view/notes.json`（env `NOTES_FILE` 覆盖，惯例同 projects.json）；
//! 权限强制 0600——内容可能含服务端口、测试账密。内容不进任何日志/tracing。

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Config;

/// cwd 字符串级归一化：trim + 去空分量 + 统一根 `/`。
/// 只做字符串归一（不消解 symlink/大小写——过度工程）：路径不同即视为不同工程，
/// Claude Code 会话与 ACP 会话传同一路径即共享同一张便签。
pub fn normalize_cwd(input: &str) -> String {
    let parts: Vec<&str> = input.trim().split('/').filter(|s| !s.is_empty()).collect();
    if parts.is_empty() {
        return "/".into();
    }
    format!("/{}", parts.join("/"))
}

/// `..` 校验（GET/PUT 共用，ocr-review 中：两端口径必须一致）：路径段精确匹配，
/// `/my..project` 这类含双点的合法名不误伤。API 层调用；含 `..` 的路径一律 400。
pub fn has_parent_segment(path: &str) -> bool {
    path.split('/').any(|s| s == "..")
}

/// 进程内便签写互斥（ocr-review 中：load-modify-write 并发会丢失更新）。
// ponytail: 全局单锁——单用户本地 hub 并发写极低频；多实例/多进程才需要文件锁
fn io_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Debug, Clone)]
pub struct NotesStore {
    pub file: PathBuf,
}

impl NotesStore {
    pub fn new(cfg: &Config) -> Self {
        Self {
            file: cfg.notes_file.clone(),
        }
    }

    /// 全量读入（文件缺失/坏 JSON → 空 map，对齐 projects.json 容错惯例）。
    pub async fn load(&self) -> HashMap<String, String> {
        match tokio::fs::read_to_string(&self.file).await {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => HashMap::new(),
        }
    }

    pub async fn get(&self, cwd: &str) -> Option<String> {
        let key = normalize_cwd(cwd);
        self.load().await.get(&key).cloned()
    }

    /// 写入一条便签并整文件落盘；空内容 = 删除该工程条目（不留空壳）。
    /// ocr-review 中：进程内互斥串行化 load-modify-write（防并发 PUT 丢失更新）；
    /// 原子落盘——0600 建临时文件后 rename 替换（无截断半截文件、无世界可读窗口）。
    pub async fn set(&self, cwd: &str, content: &str) -> anyhow::Result<()> {
        let key = normalize_cwd(cwd);
        let _guard = io_lock().lock().await;
        let mut notes = self.load().await;
        if content.is_empty() {
            notes.remove(&key);
        } else {
            notes.insert(key, content.to_string());
        }
        if let Some(dir) = self.file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        let raw = serde_json::to_string_pretty(&notes)?;
        // 0600 建临时文件（rename 保留 mode，替换后正式文件同为 0600）
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_cwd_variants() {
        assert_eq!(normalize_cwd("/repo/main"), "/repo/main");
        assert_eq!(normalize_cwd("/repo/main/"), "/repo/main", "尾斜杠归一");
        assert_eq!(
            normalize_cwd("//repo//main//"),
            "/repo/main",
            "重复斜杠归一"
        );
        assert_eq!(normalize_cwd("  /repo/main  "), "/repo/main", "首尾空白");
        assert_eq!(normalize_cwd("/"), "/");
        assert_eq!(normalize_cwd(""), "/");
        // 含 `..` 不消解——调用方（API 层）负责拒绝
        assert_eq!(normalize_cwd("/repo/../main"), "/repo/../main");
    }

    /// ocr-review 中：`..` 校验为路径段精确匹配——`/my..project` 这类含双点
    /// 的合法名不误伤
    #[test]
    fn parent_segment_is_component_match() {
        assert!(super::has_parent_segment("/repo/../main"));
        assert!(super::has_parent_segment(".."));
        assert!(super::has_parent_segment("/a/b/.."));
        assert!(!super::has_parent_segment("/repo/main"));
        assert!(!super::has_parent_segment("/my..project/data"));
        assert!(!super::has_parent_segment("/a..b"));
    }

    /// ocr-review 中：原子落盘——写临时文件后 rename，成功后无 .tmp 残留
    /// （崩溃时最坏丢一次写，不留下半截 JSON）
    #[tokio::test]
    async fn set_leaves_no_tmp_residue() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.json");
        let store = NotesStore { file: file.clone() };
        store.set("/repo/main", "a").await.unwrap();
        store.set("/repo/other", "b").await.unwrap();
        assert!(file.exists(), "正式文件在");
        let residue: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(residue.is_empty(), "不应残留临时文件: {residue:?}");
        assert_eq!(store.get("/repo/main").await.as_deref(), Some("a"));
    }

    #[tokio::test]
    async fn store_roundtrip_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = NotesStore {
            file: dir.path().join("notes.json"),
        };
        assert_eq!(store.get("/repo/main").await, None, "无文件 → None");

        store.set("/repo/main", "端口: 8080\n").await.unwrap();
        assert_eq!(
            store.get("/repo/main").await.as_deref(),
            Some("端口: 8080\n")
        );
        // ocr-review 中：归一化收进 Store 内部——尾斜杠变体在存储层直接命中同一键，
        // 不再依赖调用方先归一化（防遗漏归一化产生重复键）
        assert_eq!(
            store.get("/repo/main/").await.as_deref(),
            Some("端口: 8080\n"),
            "变体路径命中同一键（Store 内统一归一化）"
        );

        // 覆写
        store.set("/repo/main", "端口: 9090").await.unwrap();
        assert_eq!(store.get("/repo/main").await.as_deref(), Some("端口: 9090"));

        // 两条并存（同一文件多工程）
        store.set("/repo/other", "x").await.unwrap();
        assert_eq!(store.load().await.len(), 2);

        // 空内容 = 删除
        store.set("/repo/main", "").await.unwrap();
        assert_eq!(store.get("/repo/main").await, None);
        assert_eq!(store.load().await.len(), 1);
    }

    /// 验收红线：落盘文件权限 0600（内容可能含账密）
    #[cfg(unix)]
    #[tokio::test]
    async fn saved_file_has_0600_perms() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.json");
        let store = NotesStore { file: file.clone() };
        store.set("/repo/main", "secret").await.unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "便签文件必须 0600");
        // 二次写入（文件已存在）权限仍 0600
        store.set("/repo/main", "secret2").await.unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
