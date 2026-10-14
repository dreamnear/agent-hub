//! dsh RPC 信封客户端（attach-service 数据面）：上行纯 HTTP
//! `POST /api/{endpoint}`，信封 `{"type":"client-request","rpcId",..,
//! "method":"<endpoint>","payload":{"args":{…}}}` →
//! `{"type":"server-response","rpcId",..,"result":{ok,value|error}}`。
//! 端点为斜式两段（`session/list` 等，0.2.0-rc.2 typert 网关 claimsEndpoint
//! 恰好两段约束）；payload 为命名参数包裹（`args` 槽）。
//!
//! hyper-util client-legacy 复用工程既有编译面（opencode/http.rs 同款，axum re-export）。
//!
//! 认证（急修第二刀）：dsh web 可开浏览器认证（launch token → `GET /?token=`
//! 换签名 cookie → 后续请求凭 cookie）。hub 侧凭据通路 = 用户把 launch token
//! 填进 config `[dsh] token`（**敏感数据走配置，hub 绝不读 dsh 的凭据存储**）。
//! 401 → 有 token 则自动重换 cookie 重试一次；仍 401 → `AuthRejected`
//!（驱动据此走 authError 降级链，绝不上抛 500）。

use std::sync::{Arc, OnceLock, RwLock};

use anyhow::{anyhow, Context, Result};
use axum::body::Body;
use http_body_util::BodyExt;

type HttpClient =
    hyper_util::client::legacy::Client<hyper_util::client::legacy::connect::HttpConnector, Body>;

fn client() -> Arc<HttpClient> {
    static CLIENT: OnceLock<Arc<HttpClient>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            use hyper_util::rt::TokioExecutor;
            Arc::new(hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http())
        })
        .clone()
}

/// 认证被拒（401 终态，重换 cookie 后仍拒）——驱动据此打 authError 降级标记。
#[derive(Debug, thiserror::Error)]
#[error("dsh 认证被拒（401；配置 [dsh] token 后自动重试）")]
pub struct AuthRejected;

/// 认证上下文（per-driver：launch token + 换发的会话 cookie）。
/// token None = 目标实例未开认证（请求不带 cookie，401 不可能因配置产生）。
#[derive(Clone, Default)]
pub struct Auth {
    token: Arc<RwLock<Option<String>>>,
    cookie: Arc<RwLock<Option<String>>>,
}

impl Auth {
    pub fn new(token: Option<String>) -> Self {
        Self {
            token: Arc::new(RwLock::new(token)),
            cookie: Arc::new(RwLock::new(None)),
        }
    }

    /// spawn 路后置注入（stdout 捕获的 launch token；config token 优先时不覆盖）。
    pub fn set_token(&self, t: String) {
        let mut slot = self.token.write().expect("dsh token lock");
        if slot.is_none() {
            *slot = Some(t);
        }
    }

    fn token(&self) -> Option<String> {
        self.token.read().expect("dsh token lock").clone()
    }

    pub(crate) fn cookie(&self) -> Option<String> {
        self.cookie.read().expect("dsh cookie lock").clone()
    }

    fn set_cookie(&self, c: Option<String>) {
        *self.cookie.write().expect("dsh cookie lock") = c;
    }

    fn has_token(&self) -> bool {
        self.token().is_some()
    }

    /// launch token 换会话 cookie（`GET /?token=` → 303 set-cookie）。
    /// hyper-util legacy client 不跟随重定向，首响应即带 set-cookie。
    pub async fn exchange_cookie(&self, base: &str) -> Result<()> {
        let token = self
            .token()
            .ok_or_else(|| anyhow!("dsh 认证重试失败：未配置 [dsh] token"))?;
        let url = format!("{base}/?token={}", token);
        let req = axum::http::Request::builder()
            .method(axum::http::Method::GET)
            .uri(&url)
            .header("host", "127.0.0.1")
            .body(Body::empty())?;
        let resp = client()
            .request(req)
            .await
            .map_err(|e| anyhow!("dsh 登录交换失败: {e}"))?;
        let cookie = resp
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .find_map(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).to_string())
            .ok_or_else(|| anyhow!("dsh 登录交换未返回会话 cookie（token 无效或已过期？）"))?;
        self.set_cookie(Some(cookie));
        Ok(())
    }

    async fn authorized(
        &self,
        base: &str,
        endpoint: &str,
        body: serde_json::Value,
    ) -> Result<(axum::http::StatusCode, Vec<u8>)> {
        let (status, bytes) =
            post_status(base, endpoint, body.clone(), self.cookie().as_deref()).await?;
        // 401 且有 token：重换 cookie 重试一次（cookie 过期是常态，token 是长效凭据）；
        // 重换失败（token 失效）→ 终态 AuthRejected（驱动据此 authError 降级）
        if status == axum::http::StatusCode::UNAUTHORIZED && self.has_token() {
            if let Err(e) = self.exchange_cookie(base).await {
                return Err(
                    anyhow::Error::new(AuthRejected).context(format!("dsh 会话重换失败: {e}"))
                );
            }
            return post_status(base, endpoint, body, self.cookie().as_deref()).await;
        }
        Ok((status, bytes))
    }
}

pub(crate) async fn post_envelope(
    auth: &Auth,
    base: &str,
    path_method: &str,
    body: serde_json::Value,
) -> Result<Vec<u8>> {
    let (status, bytes) = auth.authorized(base, path_method, body).await?;
    if status == axum::http::StatusCode::UNAUTHORIZED || status == axum::http::StatusCode::FORBIDDEN
    {
        return Err(anyhow::Error::new(AuthRejected)
            .context(format!("dsh {base}/api/{path_method} 返回 {status}")));
    }
    if !status.is_success() {
        return Err(anyhow!("dsh {base}/api/{path_method} 返回 {status}"));
    }
    Ok(bytes)
}

/// POST 并返回（状态码, 响应体）——探测路径需要区分「有 HTTP 响应」（进程活着）
/// 与「连接拒绝/超时」（真未就绪）。
async fn post_status(
    base: &str,
    path_method: &str,
    body: serde_json::Value,
    cookie: Option<&str>,
) -> Result<(axum::http::StatusCode, Vec<u8>)> {
    let url = format!("{base}/api/{path_method}");
    let mut builder = axum::http::Request::builder()
        .method(axum::http::Method::POST)
        .uri(&url)
        .header("host", "127.0.0.1")
        .header("content-type", "application/json");
    if let Some(c) = cookie {
        builder = builder.header("cookie", c);
    }
    let req = builder.body(Body::from(serde_json::to_vec(&body)?))?;
    let resp = client()
        .request(req)
        .await
        .map_err(|e| anyhow!("dsh POST {url} 失败: {e}"))?;
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| anyhow!("读取 dsh 响应体失败: {e}"))?
        .to_bytes()
        .to_vec();
    Ok((status, bytes))
}

/// 线协议形态（自适应双协议，2026-10-03 用户定案「必须接受可升级」）：
/// - `V1`（≤0.1.1-rc.2）：点式端点（`session.list`）、payload 直传、
///   `session.history`/`llm.models`/`subagent.list`、健康探测 host/describe
/// - `V2`（≥0.2.0-rc.2）：斜式两段端点（`session/list`）、payload `{args:{…}}`
///   命名参数包裹、`session/page`/`session/modelCatalog`、subagent 内联主清单
///
/// 下次 dsh 升级改协议：改这张形态映射 + 新增 Wire 变体，不全量重写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wire {
    #[default]
    V2,
    V1,
}

/// 双形态请求描述（v1 = 点式端点 + 直 payload；v2 = 斜式端点 + args 槽内容）。
#[derive(Debug, Clone)]
pub struct WireReq {
    pub v1: (&'static str, serde_json::Value),
    pub v2: (&'static str, serde_json::Value),
}

impl WireReq {
    pub fn new(
        v1: (&'static str, serde_json::Value),
        v2: (&'static str, serde_json::Value),
    ) -> Self {
        Self { v1, v2 }
    }

    fn parts(&self, wire: Wire) -> (&'static str, serde_json::Value) {
        match wire {
            Wire::V1 => (self.v1.0, self.v1.1.clone()),
            Wire::V2 => (self.v2.0, self.v2.1.clone()),
        }
    }
}

/// 能力探测（serve 实例建立后调用一次）：斜式探活成功 → V2；404（仅点式网关
/// 认端点）→ V1；其余（401/403 认证、5xx）按最新形态 V2 兜底。
pub async fn probe_wire(auth: &Auth, base: &str) -> Wire {
    let payload = serde_json::json!({
        "type": "client-request",
        "rpcId": format!("hub-{}", rpc_uuid()),
        "method": "session/list",
        "payload": {"args": {"_request": {}}},
    });
    match post_status(base, "session/list", payload, auth.cookie().as_deref()).await {
        Ok((status, _)) if status == axum::http::StatusCode::NOT_FOUND => Wire::V1,
        _ => Wire::V2,
    }
}

/// 单调用（unary）：按形态组信封 → POST（带认证重试）→ 解包 server-response →
/// rpcId 回声校验 → result.ok 判定 → value 反序列化。
pub async fn call<T: serde::de::DeserializeOwned>(
    auth: &Auth,
    base: &str,
    wire: Wire,
    req: &WireReq,
) -> Result<T> {
    let (method, payload) = req.parts(wire);
    let rpc_id = format!("hub-{}", rpc_uuid());
    let envelope = serde_json::json!({
        "type": "client-request",
        "rpcId": rpc_id,
        "method": method,
        "payload": payload,
    });
    let bytes = post_envelope(auth, base, method, envelope).await?;
    let resp: serde_json::Value =
        serde_json::from_slice(&bytes).with_context(|| format!("解析 dsh {method} 响应失败"))?;
    if resp["type"].as_str() != Some("server-response") {
        return Err(anyhow!("dsh {method} 响应非 server-response 信封"));
    }
    if resp["rpcId"].as_str() != Some(rpc_id.as_str()) {
        return Err(anyhow!("dsh {method} rpcId 不匹配（信封回声校验失败）"));
    }
    let result = resp["result"]
        .as_object()
        .ok_or_else(|| anyhow!("dsh {method} 响应缺 result 槽: {}", preview(&bytes)))?;
    if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let err = result.get("error");
        return Err(anyhow!(
            "dsh {method} 失败: code={} message={}",
            err.and_then(|e| e.get("code"))
                .and_then(|c| c.as_str())
                .unwrap_or("?"),
            err.and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("?")
        ));
    }
    serde_json::from_value(
        result
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .with_context(|| format!("解析 dsh {method} value 失败"))
}

/// 审批应答（/api/respond）：client-response 载体 + mux 帧回执的 rpcId。
/// value = `{sessionId, approvalId, outcome:"allowed-once"|"rejected"}`。
pub async fn respond(
    auth: &Auth,
    base: &str,
    rpc_id: &str,
    value: serde_json::Value,
) -> Result<()> {
    let envelope = serde_json::json!({
        "type": "client-response",
        "rpcId": rpc_id,
        "result": {"ok": true, "value": value},
    });
    let bytes = post_envelope(auth, base, "respond", envelope).await?;
    // 响应宽松：2xx 即受理（宿主 pending 表路由，无回执 value 语义）
    let _ = bytes;
    Ok(())
}

/// 存活探测：**任意 HTTP 响应 = 进程活着**（含 401/403——认证门卫行为说明 dsh
/// 在跑；生产事故 2026-10-02：401 被当未就绪 → 换端口风暴拉起 11 个实例）。
/// 连接拒绝/超时 = false。探测不带 cookie（认证与否不影响存活判定）。
pub async fn alive(base: &str) -> bool {
    let payload = serde_json::json!({
        "type": "client-request",
        "rpcId": "hub-alive-probe",
        "method": "host.describe",
        "payload": {},
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        post_status(base, "host.describe", payload, None).await
    })
    .await
    .unwrap_or(Err(anyhow!("探测超时")))
    .is_ok()
}

fn preview(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).chars().take(200).collect()
}

/// rpcId/requestId 铸造。**必须真 UUID**（隔离真 dsh 实测 2026-10-03）：非 UUID
/// 形状的 requestId 被 dsh 编解码静默吞掉——prompt 返回 accepted:true 却零落盘
/// （SessionRequestId 品牌 codec 校验失败走幂等假阳性）。浏览器同款 randomUUID。
pub(crate) fn rpc_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 信封回环：起内存 axum 路由按线协议回 server-response，验 ok/value/rpcId；
    /// ok:false 报错携带 code+message；rpcId 不匹配报错
    #[tokio::test]
    async fn call_roundtrip_error_and_echo_paths() {
        let app = axum::Router::new().route(
            "/api/echo",
            axum::routing::post(|req: axum::extract::Json<serde_json::Value>| async move {
                assert_eq!(req.0["type"], "client-request");
                assert_eq!(req.0["method"], "echo");
                let rpc_id = req.0["rpcId"].as_str().unwrap().to_string();
                let mode = req.0["payload"]["args"]["mode"]
                    .as_str()
                    .unwrap()
                    .to_string();
                let body = match mode.as_str() {
                    "ok" => serde_json::json!({"type":"server-response","rpcId":rpc_id,
                        "result":{"ok":true,"value":{"n":42}}}),
                    "err" => serde_json::json!({"type":"server-response","rpcId":rpc_id,
                        "result":{"ok":false,"error":{"code":"boom","message":"bad thing"}}}),
                    _ => serde_json::json!({"type":"server-response","rpcId":"mismatch",
                        "result":{"ok":true,"value":1}}),
                };
                axum::Json(body)
            }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://127.0.0.1:{port}");
        let auth = Auth::default();
        // args 槽内容（0.2 payload = {args}；本 fake 直回不校验包裹）
        let mk = |mode: &str| {
            WireReq::new(
                ("echo", serde_json::json!({"mode": mode})),
                ("echo", serde_json::json!({"args": {"mode": mode}})),
            )
        };

        let v: serde_json::Value = call(&auth, &base, Wire::V2, &mk("ok")).await.unwrap();
        assert_eq!(v["n"], 42);

        let e = call::<serde_json::Value>(&auth, &base, Wire::V2, &mk("err"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("boom") && e.to_string().contains("bad thing"));

        let e = call::<serde_json::Value>(&auth, &base, Wire::V2, &mk("echo"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("rpcId 不匹配"), "{e}");
    }

    /// 存活探测：401（认证门卫）= 活；连接拒绝 = 死
    #[tokio::test]
    async fn alive_true_for_auth_and_false_for_closed() {
        let app = axum::Router::new().route(
            "/api/session/list",
            axum::routing::post(|| async { axum::http::StatusCode::UNAUTHORIZED }),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            alive(&format!("http://127.0.0.1:{port}")).await,
            "401 必须判活"
        );
        assert!(!alive("http://127.0.0.1:65098").await, "连接拒绝判死");
    }

    /// 认证矩阵核心：token 换 cookie → 带 cookie 的调用 200；无/错 cookie → 401
    /// → AuthRejected（驱动据此降级）
    #[tokio::test]
    async fn auth_exchange_and_rejection_paths() {
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::get(|uri: axum::http::Uri| async move {
                    use axum::response::IntoResponse;
                    let _ = &uri;
                    // 登录校验 token 查询参数（wrong token 拿不到会话 cookie）
                    let valid = uri
                        .query()
                        .map(|q| q.contains("token=good-token"))
                        .unwrap_or(false);
                    if valid {
                        (
                            axum::http::StatusCode::SEE_OTHER,
                            [("set-cookie", "dsh-session=good1; Path=/")],
                        )
                            .into_response()
                    } else {
                        axum::http::StatusCode::UNAUTHORIZED.into_response()
                    }
                }),
            )
            .route(
                "/api/session/list",
                axum::routing::post(
                    |(headers, req): (
                        axum::http::HeaderMap,
                        axum::extract::Json<serde_json::Value>,
                    )| async move {
                        use axum::response::IntoResponse;
                        let authed = headers
                            .get(axum::http::header::COOKIE)
                            .and_then(|v| v.to_str().ok())
                            .map(|c| c.contains("dsh-session=good1"))
                            .unwrap_or(false);
                        if !authed {
                            return axum::http::StatusCode::UNAUTHORIZED.into_response();
                        }
                        // 回显 rpcId（客户端做信封回声校验）
                        axum::Json(serde_json::json!({
                            "type": "server-response", "rpcId": req.0["rpcId"],
                            "result": {"ok": true, "value": {"items": []}}}))
                        .into_response()
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let base = format!("http://127.0.0.1:{port}");

        // 无凭据 + 认证实例：401 → AuthRejected（不 panic 不死循环）
        let anon = Auth::default();
        let e = call::<serde_json::Value>(
            &anon,
            &base,
            Wire::V2,
            &WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            ),
        )
        .await
        .unwrap_err();
        assert!(
            e.downcast_ref::<AuthRejected>().is_some(),
            "无凭据 401 应为 AuthRejected: {e}"
        );

        // 有凭据：自动换 cookie → 重试 → 200
        let authed = Auth::new(Some("good-token".into()));
        let v: serde_json::Value = call(
            &authed,
            &base,
            Wire::V2,
            &WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            ),
        )
        .await
        .unwrap();
        assert!(
            v["items"].is_array(),
            "带 token 的调用应自动换 cookie 通过: {v:?}"
        );

        // 错 token：登录 401 → 换不到 cookie → 调用 401 → AuthRejected（重试一次后终态）
        let bad = Auth::new(Some("wrong".into()));
        let e = call::<serde_json::Value>(
            &bad,
            &base,
            Wire::V2,
            &WireReq::new(
                ("", serde_json::Value::Null),
                (
                    "session/list",
                    serde_json::json!({"args": {"_request": {}}}),
                ),
            ),
        )
        .await
        .unwrap_err();
        assert!(
            e.downcast_ref::<AuthRejected>().is_some(),
            "错 token 终态仍 AuthRejected: {e}"
        );
    }
}
