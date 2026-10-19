//! 能力侧进程监督器（ADR-0006）：每插件一进程 sidecar。
//!
//! 职责边界 = `spawn` / 进程组管理 / RPC 通道 / 崩溃与故障态处理；
//! 业务逻辑全部在插件侧。**懒启动**（首次 RPC 才 spawn）、空闲回收 300s、
//! 握手超时 10s、进程组终止（防孤儿）。
//!
//! 工程既有 kill 纪律：只杀自己 spawn 的 PID；按进程组整组终止，不做批量杀。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::process::Command;

use super::credentials::CredentialsStore;
use super::manifest::BackendDecl;
use super::registry::InstalledPlugin;
use super::rpc::{HubCallHandler, RpcChannel, RpcError, HANDSHAKE_TIMEOUT};

/// 默认空闲回收秒数（ADR-0006 拍板结论 1）
pub const DEFAULT_IDLE_SECS: u64 = 300;
/// 停用/卸载时的优雅退出等待上限（技术设计 §6.3）
pub const STOP_GRACE: Duration = Duration::from_secs(5);

/// 一个已启动的 backend 进程。
pub struct RunningBackend {
    pub plugin_id: String,
    pub pid: Option<u32>,
    pub channel: Arc<RpcChannel>,
    pub started_at: Instant,
    pub idle_secs: u64,
    pub always_on: bool,
    child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
}

impl std::fmt::Debug for RunningBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningBackend")
            .field("plugin_id", &self.plugin_id)
            .field("pid", &self.pid)
            .field("alive", &self.channel.is_alive())
            .finish()
    }
}

#[derive(Default)]
pub struct Supervisor {
    runs: StdMutex<HashMap<String, Arc<RunningBackend>>>,
    /// 启动串行化（同插件并发 RPC 只 spawn 一次）
    start_locks: StdMutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor")
            .field("running", &self.runs.lock().map(|m| m.len()).unwrap_or(0))
            .finish()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_running(&self, plugin_id: &str) -> bool {
        self.runs
            .lock()
            .map(|m| {
                m.get(plugin_id)
                    .map(|r| r.channel.is_alive())
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// 运行中的 backend（含已死但未回收的，供监视任务判定）。
    pub fn snapshot(&self) -> Vec<Arc<RunningBackend>> {
        self.runs
            .lock()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// 移除运行记录（不杀进程；用于崩溃回收 / 已死进程清理）。
    pub fn forget(&self, plugin_id: &str) -> Option<Arc<RunningBackend>> {
        self.runs.lock().ok()?.remove(plugin_id)
    }

    /// 懒启动：已运行且存活直接复用；否则 spawn + `initialize` 握手。
    pub async fn ensure_started(
        &self,
        plugin: &InstalledPlugin,
        creds: &CredentialsStore,
        handler: Arc<dyn HubCallHandler>,
        scrub: Arc<dyn Fn(String) -> String + Send + Sync>,
    ) -> Result<Arc<RunningBackend>, String> {
        let id = plugin.entry.id.clone();
        // 快路径
        if let Some(run) = self.runs.lock().ok().and_then(|m| m.get(&id).cloned()) {
            if run.channel.is_alive() {
                return Ok(run);
            }
            self.forget(&id);
        }
        let lock = {
            let mut map = self.start_locks.lock().unwrap_or_else(|e| e.into_inner());
            map.entry(id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        // 双重检查（并发等待者只有一个真正 spawn）
        if let Some(run) = self.runs.lock().ok().and_then(|m| m.get(&id).cloned()) {
            if run.channel.is_alive() {
                return Ok(run);
            }
            self.forget(&id);
        }

        let backend = plugin
            .manifest
            .backend
            .clone()
            .ok_or_else(|| "插件未声明 backend（无能力侧进程）".to_string())?;
        let run = spawn_backend(
            &plugin.plugin_dir,
            &id,
            &plugin.entry.version,
            &backend,
            creds,
            handler,
            scrub,
        )
        .await?;
        self.runs
            .lock()
            .map_err(|_| "supervisor 状态锁中毒".to_string())?
            .insert(id, run.clone());
        Ok(run)
    }

    /// 转发 `tool.invoke` 到插件 backend（普通调用；权限判定在调用方已完成）。
    pub async fn invoke_tool(
        &self,
        plugin: &InstalledPlugin,
        creds: &CredentialsStore,
        handler: Arc<dyn HubCallHandler>,
        scrub: Arc<dyn Fn(String) -> String + Send + Sync>,
        tool_id: &str,
        args: Value,
        call_id: &str,
        session_id: Option<&str>,
    ) -> Result<Value, RpcError> {
        let run = self
            .ensure_started(plugin, creds, handler, scrub)
            .await
            .map_err(RpcError::PluginError)?;
        let params = json!({
            "toolId": tool_id,
            "args": args,
            "callId": call_id,
            "sessionId": session_id,
        });
        run.channel
            .request("tool.invoke", params, super::rpc::DEFAULT_TIMEOUT)
            .await
    }

    /// 优雅停：`shutdown` 通知 → 等 5s → 进程组 SIGTERM → 再等 1s → SIGKILL。
    pub async fn stop(&self, plugin_id: &str) {
        let Some(run) = self.forget(plugin_id) else {
            return;
        };
        run.channel.notify("shutdown", json!({})).await;
        run.channel.close_stdin().await;
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if !run.channel.is_alive() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if let Some(pid) = run.pid {
            terminate_group(pid, false);
            tokio::time::sleep(Duration::from_millis(300)).await;
            terminate_group(pid, true);
        }
        let mut child = run.child.lock().await;
        let _ = child.start_kill();
        // 回收僵尸（wait 必须有人调用）
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }

    /// 空闲回收：返回本轮被回收的插件 id。
    pub async fn reap_idle(&self) -> Vec<String> {
        let now = Instant::now();
        let idle: Vec<String> = self
            .snapshot()
            .into_iter()
            .filter(|r| !r.always_on)
            .filter(|r| {
                now.duration_since(r.channel.last_activity()) >= Duration::from_secs(r.idle_secs)
            })
            .map(|r| r.plugin_id.clone())
            .collect();
        for id in &idle {
            tracing::info!(plugin = %id, "空闲回收：终止插件 backend");
            self.stop(id).await;
        }
        idle
    }

    /// hub 退出：全部回收（不遗留孤儿进程）。
    pub async fn shutdown_all(&self) {
        let ids: Vec<String> = self
            .snapshot()
            .into_iter()
            .map(|r| r.plugin_id.clone())
            .collect();
        for id in ids {
            self.stop(&id).await;
        }
    }
}

/// spawn + 握手（失败即返回可定位错因）。
async fn spawn_backend(
    plugin_dir: &Path,
    plugin_id: &str,
    plugin_version: &str,
    backend: &BackendDecl,
    creds: &CredentialsStore,
    handler: Arc<dyn HubCallHandler>,
    scrub: Arc<dyn Fn(String) -> String + Send + Sync>,
) -> Result<Arc<RunningBackend>, String> {
    let program = resolve_program(plugin_dir, &backend.command)?;
    let cwd = match &backend.cwd {
        Some(c) => plugin_dir.join(c),
        None => plugin_dir.to_path_buf(),
    };
    let mut cmd = Command::new(&program);
    cmd.args(&backend.args)
        .current_dir(&cwd)
        // stdio 只走协议帧；stderr 归日志
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // 凭据注入路径之二：启动 env 的 $cred:<name> 占位由 hub 侧替换（ADR-0007 第 4 条）
    for (k, v) in &backend.env {
        let value = substitute_credentials(v, creds)?;
        cmd.env(k, value);
    }
    // 进程组：整组终止防孤儿（ADR-0006 第 2 条）。pgid = 子进程 pid，
    // stop 时对负 pid 发信号即整组回收。
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("backend-spawn-failed: 启动 {} 失败：{e}", program.display()))?;
    let pid = child.id();
    let stdin = child.stdin.take().ok_or("backend stdin 不可用")?;
    let stdout = child.stdout.take().ok_or("backend stdout 不可用")?;
    let stderr = child.stderr.take();
    let child = Arc::new(tokio::sync::Mutex::new(child));

    let channel = RpcChannel::start(plugin_id.to_string(), stdin, stdout, stderr, handler, scrub);

    // 握手：超时 = 装载失败（handshake-timeout）
    let init_params = json!({
        "apiVersion": super::api_surface::PLUGIN_API_VERSION.to_string(),
        "pluginId": plugin_id,
        "pluginVersion": plugin_version,
    });
    match channel
        .request("initialize", init_params, HANDSHAKE_TIMEOUT)
        .await
    {
        Ok(v) => {
            if v.get("ok").and_then(Value::as_bool) == Some(false) {
                let _ = terminate_child(&child).await;
                return Err(format!("handshake-failed: backend 拒绝 initialize：{v}"));
            }
        }
        Err(RpcError::Timeout) => {
            let _ = terminate_child(&child).await;
            return Err("handshake-timeout: 10s 内未回应 initialize".into());
        }
        Err(e) => {
            let _ = terminate_child(&child).await;
            return Err(format!("handshake-failed: {}", e.message()));
        }
    }

    let idle_secs = backend
        .idle_timeout_secs
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_IDLE_SECS);
    Ok(Arc::new(RunningBackend {
        plugin_id: plugin_id.to_string(),
        pid,
        channel,
        started_at: Instant::now(),
        idle_secs,
        always_on: backend.always_on.unwrap_or(false),
        child,
    }))
}

async fn terminate_child(child: &Arc<tokio::sync::Mutex<tokio::process::Child>>) -> Option<i32> {
    let mut c = child.lock().await;
    let pid = c.id().map(|p| p as i32);
    let _ = c.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), c.wait()).await;
    pid
}

/// 命令解析：含路径分隔符 → 相对插件根（禁止逃出，manifest 层已校验）；否则走 PATH。
fn resolve_program(plugin_dir: &Path, command: &str) -> Result<PathBuf, String> {
    if command.contains('/') || command.contains('\\') {
        let p = plugin_dir.join(command);
        if !p.is_file() {
            return Err(format!("backend-spawn-failed: 入口不存在 {}", p.display()));
        }
        Ok(p)
    } else {
        Ok(PathBuf::from(command))
    }
}

/// `$cred:<name>` 占位替换（未配置 → 明确失败，不静默注入空值）。
fn substitute_credentials(template: &str, creds: &CredentialsStore) -> Result<String, String> {
    let mut out = template.to_string();
    while let Some(start) = out.find("$cred:") {
        let rest = &out[start + 6..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'))
            .unwrap_or(rest.len());
        let name = &rest[..end];
        let Some(value) = creds.get(name) else {
            return Err(format!(
                "backend-spawn-failed: 凭据 {name} 未配置（manifest 以 $cred:{name} 引用）"
            ));
        };
        out.replace_range(start..start + 6 + end, &value);
    }
    Ok(out)
}

/// 进程组终止（unix：负 pid 整组；windows：仅子进程）。
fn terminate_group(pid: u32, hard: bool) {
    #[cfg(unix)]
    {
        let sig = if hard { libc::SIGKILL } else { libc::SIGTERM };
        // safety: 只对本进程 spawn 的进程组发信号（pgid = 子进程 pid，spawn 时 process_group(0)）
        unsafe {
            libc::kill(-(pid as i32), sig);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, hard);
    }
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::manifest;
    use crate::plugins::registry::{now_rfc3339, PluginEntry, PluginSource, PluginState};
    use std::fs;

    struct NoopHandler;
    impl HubCallHandler for NoopHandler {
        fn handle_call(
            &self,
            _plugin_id: &str,
            _method: &str,
            _params: Value,
        ) -> super::super::rpc::HubFuture {
            Box::pin(async { Ok(json!({"ok": true})) })
        }
    }

    fn creds() -> CredentialsStore {
        CredentialsStore::load(std::env::temp_dir().join("hub-test-no-such-credentials.json"))
    }

    fn scrub() -> Arc<dyn Fn(String) -> String + Send + Sync> {
        Arc::new(|s: String| s)
    }

    /// 写一个 POSIX sh 版 JSON-RPC backend（只回 initialize / tool.invoke）。
    fn write_sh_backend(dir: &Path, id: &str, hang: bool) -> InstalledPlugin {
        fs::create_dir_all(dir.join("backend")).unwrap();
        let script = if hang {
            "#!/bin/sh\nexec sleep 600\n".to_string()
        } else {
            r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *initialize*) printf '{"jsonrpc":"2.0","id":%s,"result":{"ok":true}}\n' "$id";;
    *tool.invoke*) printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id";;
  esac
done
"#
            .to_string()
        };
        fs::write(dir.join("backend/main.sh"), script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                dir.join("backend/main.sh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let raw = format!(
            r#"{{"schemaVersion":1,"id":"{id}","name":"T","version":"0.1.0","hub":{{"apiVersion":"1"}},
                "backend":{{"command":"backend/main.sh"}}}}"#
        );
        let manifest = manifest::parse(&raw).unwrap().manifest;
        InstalledPlugin {
            entry: PluginEntry {
                id: id.into(),
                version: "0.1.0".into(),
                content_hash: "sha256:x".into(),
                permissions_hash: "sha256:y".into(),
                source: PluginSource::drop_in(),
                installed_at: now_rfc3339(),
                enabled: true,
                state: PluginState::Ready,
                last_fault: None,
            },
            manifest,
            plugin_dir: dir.to_path_buf(),
            records: vec![],
        }
    }

    #[tokio::test]
    async fn lazy_start_handshake_and_tool_invoke() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_sh_backend(dir.path(), "sh-plugin", false);
        let sup = Supervisor::new();
        assert!(!sup.is_running("sh-plugin"), "懒启动：未调用前不 spawn");
        let run = sup
            .ensure_started(&plugin, &creds(), Arc::new(NoopHandler), scrub())
            .await
            .expect("handshake ok");
        assert!(run.pid.is_some());
        assert!(sup.is_running("sh-plugin"));
        let res = sup
            .invoke_tool(
                &plugin,
                &creds(),
                Arc::new(NoopHandler),
                scrub(),
                "ping",
                json!({}),
                "c1",
                None,
            )
            .await
            .unwrap();
        assert_eq!(res["content"][0]["text"], json!("pong"));
        sup.stop("sh-plugin").await;
        assert!(!sup.is_running("sh-plugin"));
    }

    #[tokio::test]
    async fn handshake_timeout_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_sh_backend(dir.path(), "hang-plugin", true);
        let sup = Supervisor::new();
        let t0 = Instant::now();
        let err = sup
            .ensure_started(&plugin, &creds(), Arc::new(NoopHandler), scrub())
            .await
            .unwrap_err();
        assert!(err.starts_with("handshake-timeout"), "实际：{err}");
        // 握手超时上限 10s（不无限挂）
        assert!(t0.elapsed() < Duration::from_secs(15));
        assert!(!sup.is_running("hang-plugin"));
    }

    #[test]
    fn credential_env_substitution() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plugin_credentials.json");
        std::fs::write(
            &file,
            r#"{"demo_pat":"fake-credential-value-for-test"}"#,
        )
        .unwrap();
        let creds = CredentialsStore::load(file);
        let out = substitute_credentials("Bearer $cred:demo_pat!", &creds).unwrap();
        assert_eq!(out, "Bearer fake-credential-value-for-test!");
        // 未配置的凭据 → 明确失败（不静默注入空值）
        let err = substitute_credentials("$cred:missing_one", &creds).unwrap_err();
        assert!(err.contains("未配置"), "{err}");
    }

    #[tokio::test]
    async fn killed_backend_is_observed_dead() {
        let dir = tempfile::tempdir().unwrap();
        let plugin = write_sh_backend(dir.path(), "kill-plugin", false);
        let sup = Supervisor::new();
        let run = sup
            .ensure_started(&plugin, &creds(), Arc::new(NoopHandler), scrub())
            .await
            .unwrap();
        let pid = run.pid.unwrap();
        terminate_group(pid, true);
        // 读循环感知 EOF → alive=false
        let deadline = Instant::now() + Duration::from_secs(5);
        while run.channel.is_alive() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!run.channel.is_alive(), "进程被杀后通道应感知退出");
        sup.forget("kill-plugin");
    }
}
