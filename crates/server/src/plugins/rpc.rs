//! stdio 换行分隔 JSON-RPC 2.0 通道（ADR-0006 第 3 条，技术设计 §4.1）。
//!
//! - stdin/stdout 只走协议帧；stderr 归日志（hub 采集并脱敏）；
//! - 单帧上限 4MB、单请求超时 30s（逐方法可覆盖）为护栏；
//! - hub → backend：`initialize` / `tool.invoke`（B1）；`shutdown` 通知；
//! - backend → hub：经 [`HubCallHandler`] 由 hub 代执行（权限判定在 handler 内），
//!   未知方法 → 拒绝 + 违规留痕（BR-12）；
//! - **hub 不向 backend 进程传递 hub token**。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, Mutex};

/// 单帧上限（ADR-0006 第 3 条 / 技术设计 §4.1）
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// 单请求默认超时
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// 握手超时（ADR-0006 第 2 条）
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// JSON-RPC 错误码（技术设计 §4.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// -32001 权限未声明
    Unauthorized(String),
    /// -32002 越界（已留痕）
    Violation(String),
    /// -32003 插件内部错误 / 传输层错误
    PluginError(String),
    /// -32004 超时
    Timeout,
    /// -32005 并发上限
    Busy(String),
}

impl RpcError {
    pub fn code(&self) -> i64 {
        match self {
            RpcError::Unauthorized(_) => -32001,
            RpcError::Violation(_) => -32002,
            RpcError::PluginError(_) => -32003,
            RpcError::Timeout => -32004,
            RpcError::Busy(_) => -32005,
        }
    }

    pub fn message(&self) -> String {
        match self {
            RpcError::Unauthorized(m)
            | RpcError::Violation(m)
            | RpcError::PluginError(m)
            | RpcError::Busy(m) => m.clone(),
            RpcError::Timeout => "插件请求超时".into(),
        }
    }

    /// 对外错误码名（postMessage `hub:rpc-error.code`）
    pub fn kind(&self) -> &'static str {
        match self {
            RpcError::Unauthorized(_) => "unauthorized",
            RpcError::Violation(_) => "violation",
            RpcError::Timeout => "timeout",
            RpcError::Busy(_) => "busy",
            RpcError::PluginError(_) => "plugin_error",
        }
    }
}

/// backend → hub 的调用处理器（由 `PluginManager` 实现）。
pub type HubFuture = Pin<Box<dyn Future<Output = Result<Value, RpcError>> + Send>>;

pub trait HubCallHandler: Send + Sync {
    fn handle_call(&self, plugin_id: &str, method: &str, params: Value) -> HubFuture;
}

/// 协议帧归类（纯函数，单测锚点）。
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// hub 发来的请求（有 id + method）
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    /// hub 发来的通知（有 method 无 id）
    Notification { method: String, params: Value },
    /// backend 的应答（有 id + result/error）
    Response {
        id: Value,
        result: Result<Value, (i64, String)>,
    },
    /// 无法归类（违规帧）
    Invalid(String),
}

/// 解析一行（不含末尾换行）。
pub fn parse_frame(line: &str) -> Frame {
    if line.len() > MAX_FRAME_BYTES {
        return Frame::Invalid(format!("帧超限（> {MAX_FRAME_BYTES} 字节）"));
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Frame::Invalid("非 JSON 帧".into());
    };
    if v.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Frame::Invalid("缺少 jsonrpc: \"2.0\"".into());
    }
    let id = v.get("id").cloned();
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), Some(method)) => Frame::Request { id, method, params },
        (None, Some(method)) => Frame::Notification { method, params },
        (Some(id), None) => {
            if let Some(err) = v.get("error") {
                let code = err.get("code").and_then(Value::as_i64).unwrap_or(-32003);
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("plugin error")
                    .to_string();
                Frame::Response {
                    id,
                    result: Err((code, msg)),
                }
            } else {
                Frame::Response {
                    id,
                    result: Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                }
            }
        }
        (None, None) => Frame::Invalid("既无 method 也无 id".into()),
    }
}

/// 编码一帧（补换行）。
pub fn encode_frame(v: &Value) -> String {
    let mut s = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

/// 活着的 stdio 通道。
pub struct RpcChannel {
    plugin_id: String,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    last_activity: Arc<StdMutex<std::time::Instant>>,
}

impl std::fmt::Debug for RpcChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpcChannel")
            .field("plugin_id", &self.plugin_id)
            .field("alive", &self.is_alive())
            .finish()
    }
}

impl RpcChannel {
    /// 从子进程管道建立通道，并启动读循环 / stderr 采集。
    pub fn start(
        plugin_id: String,
        stdin: ChildStdin,
        stdout: ChildStdout,
        stderr: Option<ChildStderr>,
        handler: Arc<dyn HubCallHandler>,
        scrub: Arc<dyn Fn(String) -> String + Send + Sync>,
    ) -> Arc<Self> {
        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let last_activity = Arc::new(StdMutex::new(std::time::Instant::now()));

        let channel = Arc::new(Self {
            plugin_id: plugin_id.clone(),
            stdin: stdin.clone(),
            pending: pending.clone(),
            next_id: AtomicU64::new(1),
            alive: alive.clone(),
            last_activity: last_activity.clone(),
        });

        // 读循环：应答 → pending；请求 → handler；非法帧 → 丢弃 + warn（违规留痕由 handler 侧做）
        {
            let pending = pending.clone();
            let alive = alive.clone();
            let last_activity = last_activity.clone();
            let handler = handler.clone();
            let write_stdin = stdin.clone();
            let pid = plugin_id.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if let Ok(mut t) = last_activity.lock() {
                                *t = std::time::Instant::now();
                            }
                            match parse_frame(&line) {
                                Frame::Response { id, result } => {
                                    let Some(n) = id.as_u64() else {
                                        tracing::warn!(plugin = %pid, "backend 应答 id 非整数，丢弃");
                                        continue;
                                    };
                                    let tx = pending.lock().ok().and_then(|mut m| m.remove(&n));
                                    if let Some(tx) = tx {
                                        let _ = tx.send(result.map_err(|(code, msg)| {
                                            RpcError::PluginError(format!("{msg} (code {code})"))
                                        }));
                                    }
                                }
                                Frame::Request { id, method, params } => {
                                    let handler = handler.clone();
                                    let write_stdin = write_stdin.clone();
                                    let pid = pid.clone();
                                    tokio::spawn(async move {
                                        let res = handler.handle_call(&pid, &method, params).await;
                                        let payload = match res {
                                            Ok(result) => json!({
                                                "jsonrpc": "2.0", "id": id, "result": result
                                            }),
                                            Err(e) => json!({
                                                "jsonrpc": "2.0", "id": id,
                                                "error": {"code": e.code(), "message": e.message()}
                                            }),
                                        };
                                        let mut w = write_stdin.lock().await;
                                        let _ =
                                            w.write_all(encode_frame(&payload).as_bytes()).await;
                                        let _ = w.flush().await;
                                    });
                                }
                                Frame::Notification { method, params } => {
                                    // B1：只记录；未知通知不构成违规（无法回执）
                                    tracing::debug!(plugin = %pid, method = %method, params = ?params, "backend notification");
                                }
                                Frame::Invalid(why) => {
                                    tracing::warn!(plugin = %pid, why = %why, "backend 非法帧，丢弃");
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(plugin = %pid, error = %e, "backend stdout 读取失败");
                            break;
                        }
                    }
                }
                alive.store(false, Ordering::SeqCst);
                // 进程已死：所有在途请求立即失败，不挂住调用方（BR-07：会话不挂起）
                if let Ok(mut map) = pending.lock() {
                    for (_, tx) in map.drain() {
                        let _ = tx.send(Err(RpcError::PluginError("backend 进程已退出".into())));
                    }
                }
            });
        }

        // stderr：日志（脱敏后落 tracing；限速由 tracing 侧过滤承担）
        if let Some(stderr) = stderr {
            let pid = plugin_id;
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let scrubbed = scrub(line);
                    tracing::info!(plugin = %pid, "plugin backend stderr: {scrubbed}");
                }
            });
        }

        channel
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn last_activity(&self) -> std::time::Instant {
        self.last_activity
            .lock()
            .map(|t| *t)
            .unwrap_or_else(|e| *e.into_inner())
    }

    /// 发请求并等应答（超时/断连都给明确错误，不断连）。
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        if !self.is_alive() {
            return Err(RpcError::PluginError("backend 未运行".into()));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut map) = self.pending.lock() {
            map.insert(id, tx);
        }
        let payload = json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params
        });
        {
            let mut w = self.stdin.lock().await;
            if let Err(e) = w.write_all(encode_frame(&payload).as_bytes()).await {
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&id);
                }
                return Err(RpcError::PluginError(format!(
                    "写入 backend stdin 失败：{e}"
                )));
            }
            let _ = w.flush().await;
        }
        if let Ok(mut t) = self.last_activity.lock() {
            *t = std::time::Instant::now();
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(RpcError::PluginError("backend 通道已关闭".into())),
            Err(_) => {
                if let Ok(mut map) = self.pending.lock() {
                    map.remove(&id);
                }
                Err(RpcError::Timeout)
            }
        }
    }

    /// 发通知（无应答），例如 `shutdown`。
    pub async fn notify(&self, method: &str, params: Value) {
        let payload = json!({"jsonrpc": "2.0", "method": method, "params": params});
        let mut w = self.stdin.lock().await;
        let _ = w.write_all(encode_frame(&payload).as_bytes()).await;
        let _ = w.flush().await;
    }

    /// 关闭写端（子进程读到 EOF 可自行退出）。
    pub async fn close_stdin(&self) {
        let mut w = self.stdin.lock().await;
        let _ = w.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_response_and_notification() {
        let f = parse_frame(
            r#"{"jsonrpc":"2.0","id":3,"method":"tool.invoke","params":{"toolId":"x"}}"#,
        );
        assert!(matches!(
            f,
            Frame::Request {
                id: Value::Number(_),
                ..
            }
        ));

        let f = parse_frame(r#"{"jsonrpc":"2.0","id":3,"result":{"ok":true}}"#);
        match f {
            Frame::Response { result: Ok(v), .. } => assert_eq!(v["ok"], json!(true)),
            other => panic!("unexpected {other:?}"),
        }

        let f = parse_frame(r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32001,"message":"nope"}}"#);
        match f {
            Frame::Response {
                result: Err((c, m)),
                ..
            } => {
                assert_eq!(c, -32001);
                assert_eq!(m, "nope");
            }
            other => panic!("unexpected {other:?}"),
        }

        let f = parse_frame(r#"{"jsonrpc":"2.0","method":"shutdown"}"#);
        assert!(matches!(f, Frame::Notification { .. }));
    }

    #[test]
    fn invalid_frames_are_rejected() {
        assert!(matches!(parse_frame("not json"), Frame::Invalid(_)));
        assert!(matches!(parse_frame(r#"{"id":1}"#), Frame::Invalid(_)));
        assert!(matches!(
            parse_frame(r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#),
            Frame::Invalid(_)
        ));
        let huge = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"x","params":"{}"}}"#,
            "a".repeat(MAX_FRAME_BYTES)
        );
        assert!(matches!(parse_frame(&huge), Frame::Invalid(_)));
    }

    #[test]
    fn error_codes_match_contract() {
        assert_eq!(RpcError::Unauthorized("x".into()).code(), -32001);
        assert_eq!(RpcError::Violation("x".into()).code(), -32002);
        assert_eq!(RpcError::PluginError("x".into()).code(), -32003);
        assert_eq!(RpcError::Timeout.code(), -32004);
        assert_eq!(RpcError::Busy("x".into()).code(), -32005);
        assert_eq!(RpcError::Unauthorized("x".into()).kind(), "unauthorized");
    }

    #[test]
    fn encode_appends_newline() {
        let s = encode_frame(&json!({"jsonrpc":"2.0","method":"shutdown"}));
        assert!(s.ends_with('\n'));
        assert_eq!(s.trim_end(), r#"{"jsonrpc":"2.0","method":"shutdown"}"#);
    }
}
