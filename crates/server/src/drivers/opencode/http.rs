//! opencode HTTP 客户端（attach-service 数据面）：极简 JSON GET/POST + SSE 流，
//! 复用工程既有 `hyper-util` client-legacy（api/proxy.rs 同款，axum re-export 类型，
//! 零新增编译面）。
//!
//! 只做 opencode server 的事实源访问：会话清单 / 消息 / 发消息 / abort / 健康检查 /
//! 事件流。**不直连 SQLite**（需求范围外），一切走 HTTP API。

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use axum::body::Body;
use http_body_util::BodyExt;

type HttpClient =
    hyper_util::client::legacy::Client<hyper_util::client::legacy::connect::HttpConnector, Body>;

fn client() -> &'static HttpClient {
    static CLIENT: OnceLock<HttpClient> = OnceLock::new();
    CLIENT.get_or_init(|| {
        use hyper_util::rt::TokioExecutor;
        hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http()
    })
}

fn build(method: axum::http::Method, url: &str) -> Result<axum::http::Request<Body>> {
    axum::http::Request::builder()
        .method(method)
        .uri(url)
        .header("host", host_of(url))
        .header("content-type", "application/json")
        .body(Body::empty())
        .map_err(Into::into)
}

async fn body_of(resp: hyper::Response<hyper::body::Incoming>) -> Result<Vec<u8>> {
    let bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| anyhow!("读取响应体失败: {e}"))?
        .to_bytes();
    Ok(bytes.to_vec())
}

/// 健康探活：状态码 2xx 即健康（opencode `GET /global/health`）。
pub async fn get_ok(url: &str) -> bool {
    let Ok(req) = build(axum::http::Method::GET, url) else {
        return false;
    };
    matches!(
        client().request(req).await,
        Ok(resp) if resp.status().is_success()
    )
}

/// GET → 解析 JSON。
pub async fn get_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T> {
    let req = build(axum::http::Method::GET, url)?;
    let resp = client()
        .request(req)
        .await
        .map_err(|e| anyhow!("opencode GET {url} 失败: {e}"))?;
    let status = resp.status();
    let bytes = body_of(resp).await?;
    check(status, url, &bytes)?;
    serde_json::from_slice(&bytes).map_err(|e| anyhow!("解析 opencode 响应失败: {e}"))
}

/// POST JSON body → 解析 JSON 响应（空体回 `Null`）。
pub async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    url: &str,
    body: &B,
) -> Result<T> {
    let raw = serde_json::to_vec(body)?;
    let mut req = build(axum::http::Method::POST, url)?;
    *req.body_mut() = Body::from(raw);
    let resp = client()
        .request(req)
        .await
        .map_err(|e| anyhow!("opencode POST {url} 失败: {e}"))?;
    let status = resp.status();
    let bytes = body_of(resp).await?;
    check(status, url, &bytes)?;
    if bytes.is_empty() {
        return serde_json::from_str("null").map_err(|e| anyhow!("解析空响应失败: {e}"));
    }
    serde_json::from_slice(&bytes).map_err(|e| anyhow!("解析 opencode 响应失败: {e}"))
}

/// DELETE → 2xx 即 Ok（会话删除；404 走 check 报错）。
pub async fn delete_ok(url: &str) -> Result<()> {
    let req = build(axum::http::Method::DELETE, url)?;
    let resp = client()
        .request(req)
        .await
        .map_err(|e| anyhow!("opencode DELETE {url} 失败: {e}"))?;
    let status = resp.status();
    let bytes = body_of(resp).await?;
    check(status, url, &bytes)?;
    Ok(())
}

/// SSE 订阅：连接 `/event`，逐事件把 `data:` 载荷发进 channel；连接断开 sender
/// 落地（接收端自然结束 → 调用方重连）。事件边界 = 空行（SSE 规范）。
pub async fn sse_subscribe(url: &str) -> Result<tokio::sync::mpsc::UnboundedReceiver<String>> {
    let req = axum::http::Request::builder()
        .method(axum::http::Method::GET)
        .uri(url)
        .header("host", host_of(url))
        .header("accept", "text/event-stream")
        .body(Body::empty())?;
    let resp = client()
        .request(req)
        .await
        .map_err(|e| anyhow!("opencode SSE 连接失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("opencode SSE 返回 {}", resp.status()));
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut body = resp.into_body();
        let mut buf = String::new();
        // 断流 / 读错（None/Err）即退出：sender drop → 接收端重连
        while let Some(Ok(frame)) = body.frame().await {
            let Ok(chunk) = frame.into_data() else { break };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find("\n\n") {
                let event: String = buf.drain(..pos + 2).collect();
                if let Some(data) = event
                    .lines()
                    .find_map(|l| l.strip_prefix("data: "))
                    .map(str::to_string)
                {
                    if tx.send(data).is_err() {
                        return; // 接收端已弃（重连/停机）
                    }
                }
            }
            // 防半事件残留膨胀（1MB 上限，超出丢弃重来）
            if buf.len() > 1_000_000 {
                buf.clear();
            }
        }
    });
    Ok(rx)
}

fn check(status: axum::http::StatusCode, url: &str, bytes: &[u8]) -> Result<()> {
    if !status.is_success() {
        return Err(anyhow!("opencode {url} 返回 {status}: {}", preview(bytes)));
    }
    Ok(())
}

/// 错误响应体预览（截 200 字符，防超长刷日志）。
fn preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(200).collect()
}

/// host 头提取（hyper 需要显式 Host；URL 已是绝对 http://，取 authority 段）。
fn host_of(url: &str) -> String {
    url.trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("127.0.0.1")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_extracts_authority() {
        assert_eq!(host_of("http://127.0.0.1:4097/session"), "127.0.0.1:4097");
        assert_eq!(host_of("http://127.0.0.1:4097"), "127.0.0.1:4097");
    }

    #[tokio::test]
    async fn get_json_unreachable_errors() {
        let r: Result<serde_json::Value> = get_json("http://127.0.0.1:65009/nope").await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn get_ok_false_for_unreachable() {
        assert!(!get_ok("http://127.0.0.1:65009/global/health").await);
    }

    /// 404 语义：不存在的会话 → 明确 Err 含 "404"（调用方据此映射 NotFound）
    #[tokio::test]
    async fn get_error_carries_status_text() {
        let r: Result<serde_json::Value> = get_json("http://127.0.0.1:65009/nope").await;
        let msg = r.err().unwrap().to_string();
        assert!(msg.contains("opencode"), "错误应带上下文: {msg}");
    }
}
