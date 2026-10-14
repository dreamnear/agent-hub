//! 工程列表持久化：`projects.json` → `Vec<String>` 绝对路径。

use std::path::PathBuf;

use crate::config::Config;

#[derive(Debug, Clone)]
pub struct ProjectsStore {
    pub file: PathBuf,
}

impl ProjectsStore {
    pub fn new(cfg: &Config) -> Self {
        Self {
            file: cfg.projects_file.clone(),
        }
    }

    pub async fn load(&self) -> Vec<String> {
        match tokio::fs::read_to_string(&self.file).await {
            Ok(raw) => serde_json::from_str(&raw).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn save(&self, paths: &[String]) -> anyhow::Result<()> {
        if let Some(dir) = self.file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(&self.file, serde_json::to_string_pretty(paths)?).await?;
        Ok(())
    }
}

/// 每项校验：不含 `..`、存在且是目录。
pub async fn validate_paths(paths: &[String]) -> Result<(), crate::error::AppError> {
    for p in paths {
        if p.contains("..") {
            return Err(crate::error::AppError::bad(format!(
                "路径不允许包含 ..: {p}"
            )));
        }
        let is_dir = tokio::fs::metadata(p)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if !is_dir {
            return Err(crate::error::AppError::bad(format!(
                "路径不存在或不是目录: {p}"
            )));
        }
    }
    Ok(())
}
