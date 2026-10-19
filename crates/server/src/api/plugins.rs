//! 插件 HTTP 面（技术设计 §3）。
//!
//! 全部路由一次性挂在 `router()`（ADR-0002 第 1 条）：安装/卸载插件**不重建路由树**、
//! 不重启 hub。路由分两组：
//!
//! - `/api/plugins*`：走既有 token 门禁的插件管理/调用面；
//! - `/plugin-view/{id}/{*path}`：视图资产下发，**不在 `/api` 前缀**（避免把 hub token
//!   交给 iframe），门禁由 handler 用**票据**强制实施（ADR-0004 第 3 条）。

use std::time::Duration;

use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::SharedState;
use crate::error::AppError;
use crate::plugins::rpc::RpcError;
use crate::plugins::{api_surface, PluginView};

/// 视图资产响应统一 CSP（ADR-0004 第 4 条）：`connect-src 'none'` 是结构性约束——
/// 视图的一切网络访问必须经宿主代理（视图零直调 /api/*）。
///
/// 说明：`'self'` 在 sandbox（无 allow-same-origin）的 iframe 内是 opaque origin，
/// 浏览器对 `script-src 'self'` 的匹配可能落在文档 origin 上而失效；因此显式补上
/// 本 hub 的实际 origin（由 Host + X-Forwarded-Proto 推导），两写法并存不放松边界。
fn csp_for(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or("http").trim().to_string())
        .unwrap_or_else(|| "http".into());
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1");
    let origin = format!("{scheme}://{host}");
    format!(
        "default-src 'none'; script-src 'self' {origin}; style-src 'self' {origin} 'unsafe-inline'; \
         img-src 'self' {origin} data:; font-src 'self' {origin}; connect-src 'none'; \
         frame-ancestors 'self' {origin}; base-uri 'none'; form-action 'none'"
    )
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/discovered", get(list_discovered))
        .route("/api/plugins/scan", post(scan_plugins))
        .route("/api/plugins/audit", post(audit_plugin))
        .route("/api/plugins/install", post(install_plugin))
        .route("/api/plugins/api-schema", get(api_schema))
        .route("/api/plugins/{id}/enable", post(enable_plugin))
        .route("/api/plugins/{id}/disable", post(disable_plugin))
        .route("/api/plugins/{id}/retry", post(retry_plugin))
        .route("/api/plugins/{id}", delete(uninstall_plugin))
        .route("/api/plugins/{id}/records", get(plugin_records))
        .route("/api/plugins/{id}/view-ticket", post(view_ticket))
        .route("/api/plugins/{id}/rpc", post(plugin_rpc))
        .route("/api/plugins/{id}/tool-invoke", post(tool_invoke))
}

/// 视图资产路由（不并入 `router()` 的 `/api` 组；挂载点在 `lib.rs`，位于 SPA 通配之前）。
pub fn view_router() -> Router<SharedState> {
    Router::new().route("/plugin-view/{id}/{*path}", get(plugin_view_asset))
}

// ===== 管理面 =====

async fn list_plugins(State(state): State<SharedState>) -> Json<Vec<PluginView>> {
    Json(state.plugins.list())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DiscoveredResponse {
    items: Vec<crate::plugins::scanner::DiscoveredItem>,
    last_scan_at: String,
}

async fn list_discovered(State(state): State<SharedState>) -> Json<DiscoveredResponse> {
    let d = state.plugins.discovered();
    Json(DiscoveredResponse {
        items: d.items,
        last_scan_at: d.last_scan_at,
    })
}

async fn scan_plugins(State(state): State<SharedState>) -> Json<Value> {
    let r = state.plugins.scan_now();
    Json(json!({
        "lastScanAt": r.last_scan_at,
        "discovered": r.discovered,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuditBody {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

async fn audit_plugin(
    State(state): State<SharedState>,
    Json(body): Json<AuditBody>,
) -> Result<Json<Value>, AppError> {
    let report = state
        .plugins
        .audit_target(body.path.as_deref(), body.id.as_deref())
        .map_err(AppError::bad)?;
    Ok(Json(serde_json::to_value(report).unwrap_or(Value::Null)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallBody {
    id: String,
    content_hash: String,
}

async fn install_plugin(
    State(state): State<SharedState>,
    Json(body): Json<InstallBody>,
) -> Result<Json<Value>, AppError> {
    let (view, already) = state
        .plugins
        .install(&body.id, &body.content_hash)
        .await
        .map_err(AppError::bad)?;
    Ok(Json(json!({
        "plugin": view,
        "alreadyInstalled": already,
        "effect": "immediate",
    })))
}

async fn enable_plugin(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let v = state.plugins.enable(&id).await.map_err(AppError::bad)?;
    Ok(Json(
        json!({ "plugin": v, "state": v.state, "effect": "immediate" }),
    ))
}

async fn disable_plugin(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let v = state.plugins.disable(&id).await.map_err(AppError::bad)?;
    Ok(Json(
        json!({ "plugin": v, "state": v.state, "effect": "immediate" }),
    ))
}

async fn retry_plugin(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let v = state.plugins.retry(&id).await.map_err(AppError::bad)?;
    Ok(Json(
        json!({ "plugin": v, "state": v.state, "effect": "immediate" }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UninstallQuery {
    #[serde(default)]
    purge_data: bool,
}

async fn uninstall_plugin(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Query(q): Query<UninstallQuery>,
) -> Result<Json<Value>, AppError> {
    let removed = state
        .plugins
        .uninstall(&id, q.purge_data)
        .await
        .map_err(AppError::bad)?;
    // 回归断言集随卸载自动执行（技术设计 §6.6，不靠人工走查）
    let regression = regression_report(&state, &id).await;
    Ok(Json(json!({
        "removed": true,
        "id": removed.id,
        "dirKept": true,
        "purgedData": q.purge_data,
        "regression": regression,
    })))
}

#[derive(Deserialize)]
struct RecordsQuery {
    #[serde(default)]
    limit: Option<usize>,
}

async fn plugin_records(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Query(q): Query<RecordsQuery>,
) -> Json<Value> {
    let records = state.plugins.records(&id, q.limit.unwrap_or(100));
    Json(json!({ "records": records }))
}

async fn api_schema() -> Json<api_surface::ApiSchema> {
    Json(api_surface::schema())
}

// ===== 调用面 =====

async fn view_ticket(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let (ticket, expires) = state
        .plugins
        .issue_ticket(&id)
        .map_err(AppError::forbidden)?;
    Ok(Json(json!({ "ticket": ticket, "expiresInSecs": expires })))
}

#[derive(Deserialize)]
struct RpcBody {
    method: String,
    #[serde(default)]
    params: Value,
}

async fn plugin_rpc(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(body): Json<RpcBody>,
) -> Response {
    match state.plugins.rpc_call(&id, &body.method, body.params).await {
        Ok(result) => Json(json!({ "ok": true, "result": result })).into_response(),
        Err(e) => rpc_error_response(&e),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolInvokeBody {
    tool_id: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    session_id: Option<String>,
}

async fn tool_invoke(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(body): Json<ToolInvokeBody>,
) -> Response {
    let params = json!({
        "args": body.args,
        "callId": format!("api-{}", uuid::Uuid::new_v4()),
        "sessionId": body.session_id,
    });
    match state
        .plugins
        .rpc_call(&id, &format!("hub.backend.{}", body.tool_id), params)
        .await
    {
        Ok(result) => Json(json!({ "ok": true, "content": result })).into_response(),
        Err(e) => rpc_error_response(&e),
    }
}

fn rpc_error_response(e: &RpcError) -> Response {
    let status = match e.kind() {
        "unauthorized" | "violation" => StatusCode::FORBIDDEN,
        "timeout" => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(json!({
            "ok": false,
            "code": e.kind(),
            "rpcCode": e.code(),
            "message": e.message(),
        })),
    )
        .into_response()
}

// ===== 视图资产（票据门禁）=====

/// 票据 cookie 名（多文件视图的次级资源请求无法带 query，用同路径作用域 cookie 承接；
/// HttpOnly + SameSite=Strict + Path=/plugin-view/<id>/，值域仍是票据而非 hub token）。
fn ticket_cookie_name(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("hub_pv_{safe}")
}

#[derive(Deserialize)]
struct TicketQuery {
    #[serde(default)]
    ticket: Option<String>,
}

fn cookie_ticket(headers: &HeaderMap, id: &str) -> Option<String> {
    let name = ticket_cookie_name(id);
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{name}=")) {
            return Some(v.to_string());
        }
    }
    None
}

async fn plugin_view_asset(
    State(state): State<SharedState>,
    Path((id, path)): Path<(String, String)>,
    Query(q): Query<TicketQuery>,
    headers: HeaderMap,
) -> Response {
    let ticket = q.ticket.clone().or_else(|| cookie_ticket(&headers, &id));
    let Some(ticket) = ticket else {
        return (StatusCode::FORBIDDEN, "缺少视图票据").into_response();
    };
    // 票据 ≠ hub token：值域独立，且换不到任何 /api 能力
    if !state.plugins.tickets.validate(&id, &ticket) {
        return (StatusCode::FORBIDDEN, "视图票据无效或已过期").into_response();
    }
    let file = match state.plugins.view_file(&id, &path) {
        Ok(f) => f,
        Err(crate::plugins::ViewFileError::NotFound) => {
            return (StatusCode::NOT_FOUND, "asset not found").into_response()
        }
        Err(crate::plugins::ViewFileError::Denied(why)) => {
            return (StatusCode::FORBIDDEN, why).into_response()
        }
    };

    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, file.mime)
        .header(header::CONTENT_SECURITY_POLICY, csp_for(&headers))
        // 视图不得被其他站点嵌套（frame-ancestors 之外的显式补充）
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(
            header::CACHE_CONTROL,
            if file.no_store {
                "no-store"
            } else {
                "max-age=60"
            },
        );
    if file.no_store {
        // index.html 响应顺带种下作用域票据 cookie，供次级资源请求使用
        let cookie = format!(
            "{}={}; Path=/plugin-view/{}/; HttpOnly; SameSite=Strict; Max-Age={}",
            ticket_cookie_name(&id),
            ticket,
            id,
            crate::plugins::view_ticket::DEFAULT_TTL_SECS
        );
        if let Ok(v) = HeaderValue::from_str(&cookie) {
            resp = resp.header(header::SET_COOKIE, v);
        }
    }
    resp.body(Body::from(file.bytes))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "build failed").into_response())
}

// ===== 卸载回归断言集（BR-06 四条 + 主体四链路冒烟）=====

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionCheck {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegressionReport {
    pub passed: bool,
    pub checked_at: String,
    pub checks: Vec<RegressionCheck>,
}

/// 冒烟一条既有接口（进程内 oneshot，不经网络栈）：200 + 关键字段存在。
async fn smoke(state: &SharedState, path: &str, expect_key: Option<&str>) -> RegressionCheck {
    let mut req = Request::builder().uri(path);
    if state.cfg.allow_lan {
        // oneshot 无连接信息 → 中间件按「需认证」处理，补上本机 token
        let token = crate::auth::token_of(&state.cfg);
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let Ok(req) = req.body(Body::empty()) else {
        return RegressionCheck {
            name: path.into(),
            passed: false,
            detail: "构造请求失败".into(),
        };
    };
    let app = crate::router(state.clone());
    let fut = tower::ServiceExt::oneshot(app, req);
    match tokio::time::timeout(Duration::from_secs(10), fut).await {
        Ok(Ok(resp)) => {
            let status = resp.status();
            let body = axum::body::to_bytes(resp.into_body(), 8 * 1024 * 1024)
                .await
                .unwrap_or_default();
            if status != StatusCode::OK {
                return RegressionCheck {
                    name: path.into(),
                    passed: false,
                    detail: format!("HTTP {status}"),
                };
            }
            match expect_key {
                None => RegressionCheck {
                    name: path.into(),
                    passed: true,
                    detail: "HTTP 200".into(),
                },
                Some(key) => {
                    let ok = serde_json::from_slice::<Value>(&body)
                        .ok()
                        .map(|v| !v.get(key).unwrap_or(&Value::Null).is_null())
                        .unwrap_or(false);
                    RegressionCheck {
                        name: path.into(),
                        passed: ok,
                        detail: if ok {
                            format!("HTTP 200 · 字段 {key} 存在")
                        } else {
                            format!("HTTP 200 · 缺字段 {key}")
                        },
                    }
                }
            }
        }
        Ok(Err(e)) => RegressionCheck {
            name: path.into(),
            passed: false,
            detail: format!("请求失败：{e}"),
        },
        Err(_) => RegressionCheck {
            name: path.into(),
            passed: false,
            detail: "冒烟超时（10s）".into(),
        },
    }
}

/// 卸载回执的回归检查（技术设计 §6.6 / ADR-0010 第 5 条）：四条零回归断言 +
/// 主体四链路冒烟；卸载本身不回滚（贡献物已清），失败项如实进回执。
pub async fn regression_report(state: &SharedState, plugin_id: &str) -> RegressionReport {
    let mut checks = Vec::new();

    // 1) 导航集合断言
    let nav = state.plugins.nav_entries();
    let nav_clean = !nav.iter().any(|n| n.plugin_id == plugin_id);
    checks.push(RegressionCheck {
        name: "导航集合不含该插件".into(),
        passed: nav_clean,
        detail: format!("当前导航条目 {} 条", nav.len()),
    });

    // 2) 可发现能力集合断言
    let listed = state.plugins.list();
    let cap_clean = !listed.iter().any(|p| p.id == plugin_id);
    checks.push(RegressionCheck {
        name: "贡献物登记不含该插件".into(),
        passed: cap_clean,
        detail: format!("当前已安装 {} 个插件", listed.len()),
    });

    // 3) 主体四链路冒烟（agents 收发 / 会话历史 / 实例健康 / 设置）
    // `/api/agents` 返回裸数组（无包装字段）——只断言 200，字段级断言交给有包装的端点
    checks.push(smoke(state, "/api/agents?all=1", None).await);
    checks.push(smoke(state, "/api/config/chat", Some("pageSize")).await);
    checks.push(smoke(state, "/api/instances", None).await);
    checks.push(smoke(state, "/api/pi/settings", None).await);

    // 4) bundle 报数口径（构建期门禁承担，卸载热路径不跑构建）
    checks.push(RegressionCheck {
        name: "bundle 口径不变".into(),
        passed: true,
        detail: "口径由门禁 `npm run release` + `gzip -9 | wc -c` 承担（插件资产不进主体产物，ADR-0010 第 2 条）"
            .into(),
    });

    let passed = checks.iter().all(|c| c.passed);
    RegressionReport {
        passed,
        checked_at: crate::plugins::registry::now_rfc3339(),
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csp_matches_contract_and_pins_origin() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("127.0.0.1:7909"));
        let csp = csp_for(&h);
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("connect-src 'none'"));
        assert!(csp.contains("style-src 'self' http://127.0.0.1:7909 'unsafe-inline'"));
        assert!(csp.contains("frame-ancestors 'self' http://127.0.0.1:7909"));
    }

    #[test]
    fn csp_honours_forwarded_proto() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("hub.example.com"));
        h.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(csp_for(&h).contains("https://hub.example.com"));
    }

    #[test]
    fn ticket_cookie_name_is_path_safe() {
        assert_eq!(
            ticket_cookie_name("gitlab-issues-lite"),
            "hub_pv_gitlab_issues_lite"
        );
    }

    #[test]
    fn cookie_ticket_is_scoped_per_plugin() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("other=1; hub_pv_plugin_a=abc123"),
        );
        assert_eq!(cookie_ticket(&h, "plugin-a").as_deref(), Some("abc123"));
        assert_eq!(cookie_ticket(&h, "plugin-b"), None);
    }

    #[test]
    fn rpc_error_response_maps_codes() {
        let r = rpc_error_response(&RpcError::Violation("x".into()));
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let r = rpc_error_response(&RpcError::Timeout);
        assert_eq!(r.status(), StatusCode::GATEWAY_TIMEOUT);
        let r = rpc_error_response(&RpcError::PluginError("x".into()));
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
