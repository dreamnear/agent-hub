//! nodelink 线协议契约（agent-hub-gateway-node-p1 任务 1）：节点↔网关双方共享的
//! **唯一事实源**——控制帧 + HTTP-over-mux 帧 + 常量 + 凭据哈希工具。
//! 设计权威：`docs/requirements/agent-hub-gateway-node/proposal.md` §3/§4。
//!
//! 帧走单条 WS 的 JSON 文本帧（http body 经 base64 编码进 JSON）。帧类型只在本
//! 模块定义，client/gateway/registry 全部引用此处常量与类型，禁复制（完成判据）。

use std::time::Duration;

pub mod client;
pub mod gateway;
pub mod node_config;
pub mod registry;

/// 协议版本（P1 只携带不做协商窗；P3 再议 N-1 容忍窗）
pub const PROTOCOL_VERSION: u32 = 1;
/// 单帧上限 32MB：覆盖上传 8MB×1.37 base64 膨胀 + 富余；超限 413 回注（P1 边界）
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
/// tick 心跳间隔（秒）：对齐 OpenClaw hello-ok.policy.tickIntervalMs 默认 15s
pub const TICK_SECS: u64 = 15;
/// 静默判死倍数：无入站帧超过 2×tick 即判死重连（tick watchdog 语义）
pub const DEAD_AFTER_TICKS: u32 = 2;
/// 重连退避基数（秒）
pub const BACKOFF_INITIAL_SECS: u64 = 1;
/// 重连退避上限（秒）
pub const BACKOFF_MAX_SECS: u64 = 60;

/// 指数退避序列（秒）：1,2,4,… 封顶 [`BACKOFF_MAX_SECS`]；attempt 从 1 计。
/// 纯函数（单测锚点：退避序列可观测）。
pub fn backoff_secs(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(7);
    BACKOFF_INITIAL_SECS
        .saturating_mul(1u64 << shift)
        .min(BACKOFF_MAX_SECS)
}

/// 静默判死窗口（= [`DEAD_AFTER_TICKS`] × [`TICK_SECS`]）
pub fn dead_after() -> Duration {
    Duration::from_secs(TICK_SECS * u64::from(DEAD_AFTER_TICKS))
}

// ===== 凭据哈希工具（registry/client 共用；任务 1 完成判据）=====

/// sha256 十六进制小写。node token / enrollment code 的唯一落盘形态（网关零明文）。
pub fn sha256_hex(input: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(input.as_bytes());
    format!("{:x}", h.finalize())
}

/// 常量时间字节比较（等长 XOR 累积；长度不等直接 false——哈希为定长，长度不泄密）。
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 字符串便捷封装
pub fn constant_time_eq_str(a: &str, b: &str) -> bool {
    constant_time_eq(a.as_bytes(), b.as_bytes())
}

/// 日志脱敏：凭据（node token / enroll code）进日志的唯一合法形态——保留首 4 字符
/// + 长度提示。短凭据（≤4 字符）全掩码。
pub fn mask_secret(s: &str) -> String {
    let n = s.chars().count();
    let head = s.get(..4.min(s.len())).unwrap_or("");
    if n <= 4 {
        "*".repeat(n)
    } else {
        format!("{head}…({n} chars)")
    }
}

// ===== 帧类型 =====

/// 节点↔网关线协议帧（单 JSON 文本帧；serde 宽松 = 默认忽略未知字段，前向兼容
/// P2/P3 加字段不炸旧端）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    /// 节点→网关：握手（首帧）。token 与 enroll 二选一：已配对节点带 token；
    /// 首次入网带 enrollment code（均可能为 None=缺凭据，网关显式拒绝）。
    Hello {
        protocol: u32,
        node_id: String,
        node_name: String,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        enroll: Option<String>,
        platform: String,
        #[serde(default)]
        drivers: Vec<String>,
        #[serde(default)]
        hub_version: String,
        /// host stats presence（P3 任务 3）：旧节点（P2-）不带 → None 如实缺失
        /// （serde default = N-1 窗口的字段级兼容面）
        #[serde(default)]
        stats: Option<HostStats>,
    },
    /// 网关→节点：握手通过。instance_id = 该节点挂载的实例 id（P1 = node_id）；
    /// node_token 仅审批握手响应携带（首发长效凭据，节点 0600 落盘）。
    HelloOk {
        instance_id: String,
        #[serde(default = "default_max_frame_bytes")]
        max_frame_bytes: usize,
        #[serde(default = "default_tick_secs")]
        tick_secs: u64,
        #[serde(default)]
        node_token: Option<String>,
    },
    /// 网关→节点：enroll code 有效但节点未审批——客户端退避重试等待（PAIRING_REQUIRED 语义）
    PairingRequired,
    /// 网关→节点：终态拒绝（enroll 无效/过期/已消费，或节点被操作者拒绝）——客户端停泵等人工
    Rejected { reason: String },
    /// 网关→节点：终态拒绝（node token 错误/缺失/被撤销）——客户端停泵等人工
    NodeTokenRejected { reason: String },
    /// 网关→节点：HTTP 请求注入（HTTP-over-mux；body base64。回放面属 B3 任务 8）
    HttpReq {
        req_id: String,
        method: String,
        path: String,
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        headers: Vec<(String, String)>,
        /// base64（空 body = 空串）
        #[serde(default)]
        body: String,
    },
    /// 节点→网关：HTTP 回放结果（reqId 对账；status + headers + body base64）
    HttpResp {
        req_id: String,
        status: u16,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        body: String,
    },
    /// 网关→节点：WS-over-mux 开流（P2 任务 1）。path 含类型前缀（ws/chat/…、
    /// ws/terminal…），query 为浏览器原始 query（token 剥除由代理面完成）。
    WsOpen {
        stream_id: String,
        path: String,
        #[serde(default)]
        query: Option<String>,
    },
    /// 流数据（双向；seq 每流单调递增，网关侧按 seq 去重防断线重放——W3）。
    /// text 与 data 二选一：Text 帧走 text（免 base64），Binary 帧走 data（base64）。
    /// 高频帧：None 不上网（skip 序列化）。
    WsData {
        stream_id: String,
        seq: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data: Option<String>,
    },
    /// 收流（任一侧关断/失败；reason 仅诊断用）。
    WsClose {
        stream_id: String,
        #[serde(default)]
        reason: Option<String>,
    },
    /// 节点→网关：HubEvent::Tick 透传（P2 任务 2 事件回传）——网关转投自身
    /// events 广播面（前端 ws/events 收 tick 即重拉 agents，节点实例实时化）
    NodeTick,
    /// 网关→节点：token 轮换（P3 任务 M2）——新长效凭据仅经该节点在线连接
    /// 回显（proposal §4：轮换不落响应体）；节点 0600 落盘后旧 token 立即失效
    TokenRotated { token: String },
    /// 节点→网关：轮换回执（sweep S1，帧只增）——persist 结果回传（token 回带
    /// 供网关对账代际，防上一轮迟到 ack 顶替新一轮）。NAK → 网关回滚旧哈希 +
    /// 前端「轮换失败已回滚」。旧网关收本帧走「意外帧忽略」，双向 N-1 兼容。
    TokenRotatedAck { token: String, ok: bool },
}

/// host stats presence（P3 任务 3）：节点 announce 随 hello 上报（N-1 窗口：
/// 旧节点不带 → Option None 如实缺失，不崩不猜）
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStats {
    pub os: String,
    pub arch: String,
    /// 节点 server 版本（CARGO_PKG_VERSION）
    pub version: String,
    /// host 开机时长（秒；跨平台取法归 platform 层）
    pub uptime_secs: u64,
}

fn default_max_frame_bytes() -> usize {
    MAX_FRAME_BYTES
}

fn default_tick_secs() -> u64 {
    TICK_SECS
}

// ===== HTTP-over-mux 共享转换（任务 8；节点重放与网关重建共用，禁两处漂移）=====

use base64::Engine as _;

/// hop-by-hop 头（RFC 9110）——代理注入与重放两侧都不透传（proxy.rs 同款清单）。
/// content-length 端到端不在列：节点重算后随帧头透传（网关重建按帧头还原）。
pub fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// HttpReq 帧 → axum Request（节点本地重放入口）。body base64 解码；非法 base64
/// / 非法 header 名返回 Err（调用方回 400 形态 http_resp）。
pub fn frame_to_request(frame: Frame) -> Result<axum::http::Request<axum::body::Body>, String> {
    let Frame::HttpReq {
        method,
        path,
        query,
        headers,
        body,
        ..
    } = frame
    else {
        return Err("帧不是 http_req".into());
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .map_err(|e| format!("req body base64 解码失败: {e}"))?;
    let method: axum::http::Method = method
        .parse()
        .map_err(|_| format!("非法 method: {method}"))?;
    let uri: axum::http::Uri = match query {
        Some(q) if !q.is_empty() => format!("/{path}?{q}"),
        _ => format!("/{path}"),
    }
    .parse()
    .map_err(|e| format!("非法 path/query: {e}"))?;
    let mut builder = axum::http::Request::builder().method(method).uri(uri);
    for (k, v) in &headers {
        if is_hop_by_hop(k) {
            continue;
        }
        let name: axum::http::HeaderName = k.parse().map_err(|_| format!("非法 header 名: {k}"))?;
        let value: axum::http::HeaderValue =
            v.parse().map_err(|_| format!("非法 header 值: {k}"))?;
        builder = builder.header(name, value);
    }
    builder
        .body(axum::body::Body::from(bytes))
        .map_err(|e| format!("请求构建失败: {e}"))
}

/// 回放结果 → HttpResp 帧（content-length 以实际 body 重算——替换帧头里的旧值）。
pub fn response_to_frame(
    req_id: &str,
    status: axum::http::StatusCode,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Frame {
    let kept: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(k, _)| k != "content-length" && !is_hop_by_hop(k))
        .chain(std::iter::once((
            "content-length".to_string(),
            body.len().to_string(),
        )))
        .collect();
    Frame::HttpResp {
        req_id: req_id.to_string(),
        status: status.as_u16(),
        headers: kept,
        body: base64::engine::general_purpose::STANDARD.encode(&body),
    }
}

/// HttpResp 帧 → axum Response（网关回注浏览器；content-length 重算兜底）。
pub fn response_from_frame(
    frame: &Frame,
) -> Result<axum::http::Response<axum::body::Body>, String> {
    let Frame::HttpResp {
        status,
        headers,
        body,
        ..
    } = frame
    else {
        return Err("帧不是 http_resp".into());
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body.as_bytes())
        .map_err(|e| format!("resp body base64 解码失败: {e}"))?;
    let mut builder = axum::http::Response::builder().status(
        axum::http::StatusCode::from_u16(*status).map_err(|_| format!("非法 status: {status}"))?,
    );
    for (k, v) in headers {
        if is_hop_by_hop(k) {
            continue;
        }
        builder = builder.header(k.as_str(), v.as_str());
    }
    builder
        .body(axum::body::Body::from(bytes))
        .map_err(|e| format!("响应构建失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 控制帧 serde 往返稳定（hello 双凭据形态 / hello-ok 首发 token / 终态帧）
    #[test]
    fn control_frames_roundtrip() {
        let hello = Frame::Hello {
            protocol: PROTOCOL_VERSION,
            node_id: "n-1".into(),
            node_name: "lab-mac".into(),
            token: Some("tok-1".into()),
            enroll: Some("enr-1".into()),
            platform: "macos-aarch64".into(),
            drivers: vec!["claude".into(), "dsh".into()],
            hub_version: "0.1.0".into(),
            stats: None,
        };
        let raw = serde_json::to_string(&hello).unwrap();
        assert!(raw.contains("\"type\":\"hello\""), "tag 序列化: {raw}");
        assert_eq!(serde_json::from_str::<Frame>(&raw).unwrap(), hello);

        let ok = Frame::HelloOk {
            instance_id: "n-1".into(),
            max_frame_bytes: MAX_FRAME_BYTES,
            tick_secs: TICK_SECS,
            node_token: Some("tok-new".into()),
        };
        assert_eq!(
            serde_json::from_str::<Frame>(&serde_json::to_string(&ok).unwrap()).unwrap(),
            ok
        );

        for (f, tag) in [
            (Frame::PairingRequired, "pairing_required"),
            (
                Frame::Rejected {
                    reason: "expired".into(),
                },
                "rejected",
            ),
            (
                Frame::NodeTokenRejected {
                    reason: "bad token".into(),
                },
                "node_token_rejected",
            ),
        ] {
            let raw = serde_json::to_string(&f).unwrap();
            assert!(raw.contains(tag), "{tag} 帧名: {raw}");
            assert_eq!(serde_json::from_str::<Frame>(&raw).unwrap(), f);
        }
    }

    /// WS-over-mux 流帧（P2）：WsOpen/WsData(text+binary)/WsClose 往返稳定；
    /// text 与 data 互斥语义在往返中保留（None 不塌陷为空串）。
    #[test]
    fn ws_mux_frames_roundtrip() {
        let open = Frame::WsOpen {
            stream_id: "s-1".into(),
            path: "ws/chat/a1b2".into(),
            query: Some("token=x".into()),
        };
        let raw = serde_json::to_string(&open).unwrap();
        assert!(raw.contains("\"type\":\"ws_open\""), "tag: {raw}");
        assert_eq!(serde_json::from_str::<Frame>(&raw).unwrap(), open);

        for f in [
            Frame::WsData {
                stream_id: "s-1".into(),
                seq: 7,
                text: Some("hello".into()),
                data: None,
            },
            Frame::WsData {
                stream_id: "s-1".into(),
                seq: 8,
                text: None,
                data: Some(base64::engine::general_purpose::STANDARD.encode([0u8, 255, 16])),
            },
            Frame::WsClose {
                stream_id: "s-1".into(),
                reason: Some("peer close".into()),
            },
        ] {
            let raw = serde_json::to_string(&f).unwrap();
            assert_eq!(serde_json::from_str::<Frame>(&raw).unwrap(), f);
        }

        // text 帧不得落入 data（免 base64 语义锚）
        let t = serde_json::to_string(&Frame::WsData {
            stream_id: "s".into(),
            seq: 1,
            text: Some("纯文本".into()),
            data: None,
        })
        .unwrap();
        assert!(
            t.contains("\"text\":\"纯文本\"") && !t.contains("\"data\""),
            "text/binary 互斥: {t}"
        );
    }

    /// HTTP-over-mux 帧往返：headers 键值对 + body base64 保真
    #[test]
    fn http_frames_roundtrip() {
        let req = Frame::HttpReq {
            req_id: "r-1".into(),
            method: "POST".into(),
            path: "api/agents/claude/a1/message".into(),
            query: Some("all=1".into()),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("authorization".into(), "Bearer x".into()),
            ],
            body: "eyJ0ZXh0IjoiaGkifQ==".into(),
        };
        let raw = serde_json::to_string(&req).unwrap();
        assert!(raw.contains("\"type\":\"http_req\""), "tag 序列化: {raw}");
        assert_eq!(serde_json::from_str::<Frame>(&raw).unwrap(), req);

        let resp = Frame::HttpResp {
            req_id: "r-1".into(),
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: "W10=".into(),
        };
        assert_eq!(
            serde_json::from_str::<Frame>(&serde_json::to_string(&resp).unwrap()).unwrap(),
            resp
        );
    }

    /// 未知字段容忍（前向兼容：新端加字段不炸旧端）
    #[test]
    fn unknown_fields_tolerated() {
        let raw = r#"{"type":"hello_ok","instance_id":"n1","extra_future":{"a":1}}"#;
        let f: Frame = serde_json::from_str(raw).unwrap();
        match f {
            Frame::HelloOk {
                instance_id,
                max_frame_bytes,
                tick_secs,
                node_token,
            } => {
                assert_eq!(instance_id, "n1");
                assert_eq!(max_frame_bytes, MAX_FRAME_BYTES, "缺省回落常量");
                assert_eq!(tick_secs, TICK_SECS);
                assert_eq!(node_token, None);
            }
            other => panic!("应为 hello_ok: {other:?}"),
        }
    }

    /// 退避序列：1,2,4,8,16,32,60,60（1<<6=64 已封顶；大 attempt 饱和不溢出）
    #[test]
    fn backoff_sequence_and_saturation() {
        let seq: Vec<u64> = (1..=8).map(backoff_secs).collect();
        assert_eq!(seq, vec![1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff_secs(0), 1, "attempt 下界钳 1");
        assert_eq!(backoff_secs(u32::MAX), BACKOFF_MAX_SECS, "饱和不溢出");
        assert_eq!(dead_after(), Duration::from_secs(30), "2×15s 判死窗");
    }

    /// sha256 已知向量 + 十六进制小写
    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(""), sha256_hex(""), "确定性");
        assert_ne!(sha256_hex("a"), sha256_hex("b"));
    }

    /// 常量时间比较：等/不等/不等长（不等长必须 false 且不 panic）
    #[test]
    fn constant_time_eq_semantics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"), "不等长 false");
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq_str("x", "x"));
    }

    /// 日志脱敏：完整凭据不出现，保留首 4 字符 + 长度提示；短凭据全掩码
    #[test]
    fn mask_secret_hides_value() {
        let m = mask_secret("tok-abcdef12");
        assert!(m.starts_with("tok-…"), "首 4 字符保留: {m}");
        assert!(m.contains("(12 chars)"));
        assert!(!m.contains("abcdef12"), "完整值不得出现: {m}");
        assert_eq!(mask_secret("ab"), "**");
        assert_eq!(mask_secret(""), "");
    }

    /// HTTP-over-mux 转换往返：frame→request→（模拟回放）→response_to_frame→
    /// response_from_frame；body 字节与 content-type 保真，content-length 重算
    #[test]
    fn http_frame_conversion_roundtrip() {
        let req = Frame::HttpReq {
            req_id: "r-1".into(),
            method: "POST".into(),
            path: "api/agents/claude/a1/message".into(),
            query: Some("all=1".into()),
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("authorization".into(), "Bearer browser-tok".into()),
                ("transfer-encoding".into(), "chunked".into()), // hop-by-hop 应被剥
            ],
            body: base64::engine::general_purpose::STANDARD.encode(r#"{"text":"hi"}"#),
        };
        let request = frame_to_request(req).unwrap();
        assert_eq!(request.method(), "POST");
        assert_eq!(request.uri().path(), "/api/agents/claude/a1/message");
        assert_eq!(request.uri().query(), Some("all=1"));
        assert_eq!(
            request.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert!(
            request.headers().get("transfer-encoding").is_none(),
            "hop-by-hop 不得透传"
        );
        // body 保真（消费请求体）
        let bytes =
            futures::executor::block_on(axum::body::to_bytes(request.into_body(), usize::MAX))
                .unwrap();
        assert_eq!(&bytes[..], br#"{"text":"hi"}"#);

        // 回放结果 → 帧 → axum Response
        let resp_frame = response_to_frame(
            "r-1",
            axum::http::StatusCode::OK,
            vec![
                ("content-type".into(), "application/json".into()),
                ("content-length".into(), "999".into()), // 旧值应被重算覆盖
            ],
            br#"[{"id":"a"}]"#.to_vec(),
        );
        let resp = response_from_frame(&resp_frame).unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(resp.headers().get("content-length").unwrap(), "12");
    }

    /// 非法输入分类：坏 base64 / 坏 method / 非 http_req 帧都 Err（调用方回 400 形态）
    #[test]
    fn http_frame_conversion_rejects_garbage() {
        let bad_b64 = Frame::HttpReq {
            req_id: "r".into(),
            method: "GET".into(),
            path: "api/x".into(),
            query: None,
            headers: vec![],
            body: "!!not-base64!!".into(),
        };
        assert!(frame_to_request(bad_b64).is_err());
        let bad_method = Frame::HttpReq {
            req_id: "r".into(),
            method: "BAD METHOD".into(), // 含空格：非法 token（BREW 类扩展 method 合法）
            path: "api/x".into(),
            query: None,
            headers: vec![],
            body: String::new(),
        };
        assert!(frame_to_request(bad_method).is_err());
        assert!(frame_to_request(Frame::PairingRequired).is_err());
        assert!(response_from_frame(&Frame::PairingRequired).is_err());
    }
}
