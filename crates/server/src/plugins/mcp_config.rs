//! 会话级 MCP config 生成（B2，claude --mcp-config 注入）。
//!
//! 不写用户 `~/.claude`（`--mcp-config` 为单次调用级开关，纯加法；
//! `--strict-mcp-config` 不使用——保持用户自有 MCP 不受影响）。
//! 文件落 `~/.claude-view/plugin_mcp/<session>.json`（Config::plugin_mcp_dir，
//! env `AGENT_HUB_PLUGIN_MCP_DIR` 覆盖），每会话一文件；会话结束由驱动层回收。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// 生成 --mcp-config 所需的 JSON 值（单 server: hub-plugins）。
/// `hub_url`：hub loopback（env `AGENT_HUB_LOOPBACK` 注入代理进程）——高危
/// 审批落单/轮询用（auth 对 loopback 直通免 token，与 P8 extension 同款；
/// 不下发 hub token，凭据零回显红线不破）。
pub fn mcp_config_value(hub_bin: &Path, hub_url: &str, session_id: &str) -> Value {
    json!({
        "mcpServers": {
            "hub-plugins": {
                "command": hub_bin.to_string_lossy().to_string(),
                "args": ["mcp-proxy", "--session", session_id],
                "env": { "AGENT_HUB_LOOPBACK": hub_url }
            }
        }
    })
}

/// 会话级 config 文件路径：`<mcp_dir>/<session_id>.json`。
pub fn config_path(mcp_dir: &Path, session_id: &str) -> PathBuf {
    mcp_dir.join(format!("{}.json", session_id))
}

/// 同步写 config（原子：tmp + rename；父目录不存在则建）。
/// 返回写入路径。
pub fn write_config_sync(
    mcp_dir: &Path,
    hub_bin: &Path,
    hub_url: &str,
    session_id: &str,
) -> anyhow::Result<PathBuf> {
    let path = config_path(mcp_dir, session_id);
    write_config_at_sync(&path, hub_bin, hub_url, session_id)?;
    Ok(path)
}

/// 同步写 config 到**指定路径**（F-6，preview 反馈轮）：文件名允许与 session_id
/// 无关——start 期真实会话 id 未知，占位文件（hub-<uuid>.json）先写、claude
/// daemon 拿到 id 后**原地重写内容**。daemon 对 --mcp-config 的读取时点不受
/// hub 控制（多会话/长生命周期下明显滞后），删除另写会让晚读的 daemon 落空；
/// tmp+rename 原子替换保证读侧要么完整旧内容要么完整新内容。
pub fn write_config_at_sync(
    path: &Path,
    hub_bin: &Path,
    hub_url: &str,
    session_id: &str,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.tmp"));
    let raw = serde_json::to_string_pretty(&mcp_config_value(hub_bin, hub_url, session_id))?;
    std::fs::write(&tmp, raw)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 异步写（驱动层便捷）。
pub async fn write_config(
    mcp_dir: PathBuf,
    hub_bin: PathBuf,
    hub_url: String,
    session_id: String,
) -> anyhow::Result<PathBuf> {
    tokio::task::spawn_blocking(move || {
        write_config_sync(&mcp_dir, &hub_bin, &hub_url, &session_id)
    })
    .await
    .map_err(|e| anyhow::anyhow!("mcp config spawn_blocking: {e}"))?
}

/// 回收会话级 config（不存在忽略）。
pub fn remove_config_sync(mcp_dir: &Path, session_id: &str) {
    let path = config_path(mcp_dir, session_id);
    let _ = std::fs::remove_file(path);
}

/// 当前 hub 二进制路径（回退字面量 `server`，保证 JSON 仍合法）。
pub fn hub_bin_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("server"))
}

/// 供测试：判断是否需要注入（有启用插件才注入，零回归——无插件时行为与引入前一致）。
pub fn needs_injection(enabled_count: usize) -> bool {
    enabled_count > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn config_value_shape() {
        let v = mcp_config_value(
            Path::new("/tmp/server"),
            "http://127.0.0.1:7909",
            "sess-123",
        );
        assert_eq!(
            v["mcpServers"]["hub-plugins"]["command"],
            json!("/tmp/server")
        );
        assert_eq!(
            v["mcpServers"]["hub-plugins"]["args"],
            json!(["mcp-proxy", "--session", "sess-123"])
        );
        // 高危审批通道（B2 批3）：loopback env 注入（免 token，不下发凭据）
        assert_eq!(
            v["mcpServers"]["hub-plugins"]["env"]["AGENT_HUB_LOOPBACK"],
            json!("http://127.0.0.1:7909")
        );
    }

    #[test]
    fn write_and_remove_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mcp_dir = dir.path().join("plugin_mcp");
        let hub = Path::new("/tmp/hub-bin");
        let sid = "test-sess-abc";
        let path = write_config_sync(&mcp_dir, hub, "http://127.0.0.1:7909", sid).unwrap();
        assert!(path.is_file());
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["mcpServers"]["hub-plugins"]["args"][2], json!(sid));
        // 原子无残留 tmp
        assert!(!mcp_dir.join(format!(".{sid}.tmp")).exists());
        remove_config_sync(&mcp_dir, sid);
        assert!(!path.exists());
        // 重复删除不报错
        remove_config_sync(&mcp_dir, sid);
    }

    #[test]
    fn needs_injection_gate() {
        assert!(!needs_injection(0));
        assert!(needs_injection(1));
    }

    #[test]
    fn write_at_keeps_placeholder_name_with_real_session() {
        // F-6：占位路径原地重写——文件名保持 hub-<uuid>.json，内容 session 换真实 id
        let dir = TempDir::new().unwrap();
        let mcp_dir = dir.path().join("plugin_mcp");
        let hub = Path::new("/tmp/hub-bin");
        let placeholder = write_config_sync(&mcp_dir, hub, "http://127.0.0.1:7909", "hub-abc")
            .unwrap();
        write_config_at_sync(&placeholder, hub, "http://127.0.0.1:7909", "real-sess-1").unwrap();
        assert!(placeholder.is_file(), "占位文件必须原地保留（不删另写）");
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(&placeholder).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["hub-plugins"]["args"][2], json!("real-sess-1"));
        // 原子无残留 tmp
        assert!(!mcp_dir.join(".hub-abc.json.tmp").exists());
    }
}
