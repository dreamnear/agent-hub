//! hub HTTP 客户端（纯客户端，零 hub 改动）：地址/token 解析 + 各端点薄封装。
//! API 形状对照 crates/server/src/api/{agents,messages}.rs 的 DTO（serde camelCase）。

use serde::Deserialize;

/// 会话摘要（server AgentSummary 的客户端镜像，字段全 Option 宽松解析）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSummary {
    pub driver: String,
    pub id: String,
    pub name: Option<String>,
    pub cwd: Option<String>,
    pub kind: Option<String>,
    pub raw_state: Option<String>,
    pub group: Option<String>,
    pub tokens: Option<u64>,
    pub session_id: Option<String>,
}

/// 会话消息（server ChatMessage 的客户端镜像；只取 CLI 渲染要用的字段）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub kind: String,
    pub text: Option<String>,
    pub tool_name: Option<String>,
}

/// 分页信封（server MessagePageDto 镜像）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagePage {
    pub messages: Vec<ChatMessage>,
}

/// WS chat 推送事件（server ChatEvent 镜像）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEvent {
    pub message: ChatMessage,
}

/// hub 地址解析：`--url` > `AH_URL` env > 默认 127.0.0.1:7800。
pub fn resolve_url(override_url: Option<&str>) -> String {
    override_url
        .map(str::to_string)
        .or_else(|| std::env::var("AH_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:7800".into())
}

/// hub 客户端：HTTP（reqwest）+ WS（tokio-tungstenite）共用 base 与 token。
#[derive(Clone)]
pub struct Hub {
    pub base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Hub {
    pub fn new(base: String) -> Self {
        Self {
            token: std::env::var("AH_TOKEN").ok().filter(|t| !t.is_empty()),
            http: reqwest::Client::new(),
            base: base.trim_end_matches('/').to_string(),
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        let b = self.http.get(format!("{}{path}", self.base));
        match &self.token {
            Some(t) => b.bearer_auth(t),
            None => b,
        }
    }

    /// 会话清单；`all=1` 含已结束（对齐 GET /api/agents?all=1）。
    pub async fn agents(&self, all: bool) -> anyhow::Result<Vec<AgentSummary>> {
        let path = if all {
            "/api/agents?all=1"
        } else {
            "/api/agents"
        };
        Ok(self
            .get(path)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// 原始 JSON（--json 透传用，不经客户端结构化）。
    pub async fn agents_raw(&self, all: bool) -> anyhow::Result<serde_json::Value> {
        let path = if all {
            "/api/agents?all=1"
        } else {
            "/api/agents"
        };
        Ok(self
            .get(path)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// hub 存活探针（GET /api/drivers 200 即认为在跑；serve 幂等判定用）。
    pub async fn healthy(&self) -> bool {
        self.get("/api/drivers")
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// 发送消息（POST /api/agents/{driver}/{id}/message，body {"text": ...}）。
    pub async fn send(&self, driver: &str, id: &str, text: &str) -> anyhow::Result<()> {
        let r = self
            .http
            .post(format!(
                "{base}/api/agents/{driver}/{id}/message",
                base = self.base
            ))
            .json(&serde_json::json!({ "text": text }));
        let r = match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        };
        r.send().await?.error_for_status()?;
        Ok(())
    }

    /// 历史末页（GET .../messages/page?limit=N）。
    pub async fn page(&self, driver: &str, id: &str, limit: usize) -> anyhow::Result<MessagePage> {
        Ok(self
            .get(&format!(
                "/api/agents/{driver}/{id}/messages/page?limit={limit}"
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// WS 端点地址（http→ws / https→wss；token 走 ?token=，对齐前端 WS 惯例）。
    pub fn ws_url(&self, path: &str) -> String {
        let ws = self
            .base
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        match &self.token {
            Some(t) => format!("{ws}{path}?token={t}"),
            None => format!("{ws}{path}"),
        }
    }
}
