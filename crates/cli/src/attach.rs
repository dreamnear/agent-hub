//! `ah attach <name>`：按名定位会话 → 终端交互循环（raw mode）。
//! 接收走 WS `/ws/chat/{sessionId}`（hub 已有 per-session 推送房间，CLI 直接订阅，
//! 免 1s 轮询——派发方「二选一注明理由」：WS 为零轮询、与网页端同一事件流，
//! 天然满足需求决策 4 的多端回流一致）；发送走 POST `/api/agents/{driver}/{id}/message`。
//! 退出约定：Ctrl-C / Ctrl-D；stdout 非 tty（管道）时退化为按行读取，便于自动化验证。

use std::io::{Read, Write};

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::api::{AgentSummary, ChatMessage, Hub};

/// 名匹配失败形态：多候选（列出让用户选）或无命中。
#[derive(Debug)]
pub enum ResolveError {
    Ambiguous(Vec<AgentSummary>),
    NotFound,
}

/// 按名定位：name 精确 → id 精确 → name/id 唯一前缀（大小写不敏感）。
pub fn resolve(agents: &[AgentSummary], query: &str) -> Result<AgentSummary, ResolveError> {
    let q = query.to_lowercase();
    let same = |a: &AgentSummary| a.id == query || a.name.as_deref() == Some(query);
    if let Some(a) = agents.iter().find(|a| same(a)) {
        return Ok(a.clone());
    }
    let prefixed: Vec<AgentSummary> = agents
        .iter()
        .filter(|a| {
            a.id.to_lowercase().starts_with(&q)
                || a.name
                    .as_deref()
                    .map(|n| n.to_lowercase().starts_with(&q))
                    .unwrap_or(false)
        })
        .cloned()
        .collect();
    match prefixed.len() {
        1 => Ok(prefixed.into_iter().next().unwrap()),
        0 => Err(ResolveError::NotFound),
        _ => Err(ResolveError::Ambiguous(prefixed)),
    }
}

/// ChatEvent → 终端行（纯函数便于测试）：user/assistant 文本、tool_use 名单行；
/// tool_result/thinking/空块不渲染（首版保真度：发送 + 增量渲染，不做 per-driver 特化）。
pub fn render_event(m: &ChatMessage) -> Option<String> {
    let text = m.text.as_deref().unwrap_or("").trim_end();
    match m.kind.as_str() {
        "user" if !text.is_empty() => Some(format!("你 ▸ {text}")),
        "assistant" if !text.is_empty() => Some(format!("AI ▸ {text}")),
        "tool_use" => Some(format!(
            "  [工具] {}",
            m.tool_name.as_deref().unwrap_or("?")
        )),
        _ => None,
    }
}

/// WS 房间键：优先 sessionId（前端 /ws/chat/{sessionId} 同键），缺省回退 id。
fn room_of(a: &AgentSummary) -> String {
    a.session_id.clone().unwrap_or_else(|| a.id.clone())
}

/// 交互循环入口。返回前一定恢复终端态（RawGuard drop 兜底）。
pub async fn run(hub: &Hub, agent: &AgentSummary) -> anyhow::Result<()> {
    let room = room_of(agent);
    println!(
        "attach: {}（{} 驱动，{}）——输入回车发送，Ctrl-C/Ctrl-D 退出",
        agent.name.as_deref().unwrap_or(&agent.id),
        agent.driver,
        agent.cwd.as_deref().unwrap_or("-")
    );

    // 首屏历史（末 30 条），随后增量走 WS
    let page = hub.page(&agent.driver, &agent.id, 30).await?;
    for line in page.messages.iter().filter_map(render_event) {
        println!("{line}");
    }

    let raw = RawGuard::enable(); // 非 tty（管道/重定向）自动退化为按行读
    let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || read_stdin(ctl_tx, raw.active));

    let ws_url = hub.ws_url(&format!("/ws/chat/{room}"));
    loop {
        let ws = connect(&ws_url).await?;
        let (mut sink, mut stream) = ws.split();
        loop {
            tokio::select! {
                ctl = ctl_rx.recv() => match ctl {
                    Some(Ctl::Line(l)) => {
                        if l.trim().is_empty() {
                            continue;
                        }
                        if let Err(e) = hub.send(&agent.driver, &agent.id, &l).await {
                            println!("\r[发送失败] {e:#}");
                        }
                    }
                    _ => {
                        let _ = sink.close().await;
                        return Ok(());
                    }
                },
                msg = stream.next() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(ev) = serde_json::from_str::<crate::api::ChatEvent>(&t) {
                            if let Some(line) = render_event(&ev.message) {
                                println!("\r{line}");
                            }
                        } // 坏帧忽略（对齐前端语义）
                    }
                    Some(Ok(_)) => {} // ping/pong/binary 忽略
                    Some(Err(_)) | None => {
                        println!("\r[连接断开，2s 后重连]");
                        break;
                    }
                },
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn connect(
    url: &str,
) -> anyhow::Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let (ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .with_context(|| format!("WS 连接失败: {url}"))?;
    Ok(ws)
}

enum Ctl {
    Line(String),
    Exit,
}

/// stdin 读线程：raw mode 下逐字节整形为行/退出信号。`echo`=tty 时逐键回显
///（按原始字节写，终端按 UTF-8 流解码，多字节安全）；非 tty（管道验证）不回显。
/// 发送的行不本地 echo——回流由 WS 承担（与网页端同一事件流，防双份）。
fn read_stdin(tx: tokio::sync::mpsc::UnboundedSender<Ctl>, echo: bool) {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    let mut input = std::io::stdin();
    let mut out = std::io::stdout();
    macro_rules! emit {
        ($b:expr) => {
            if echo {
                let _ = out.write_all($b);
                let _ = out.flush();
            }
        };
    }
    loop {
        match input.read(&mut byte) {
            Ok(0) | Err(_) => break, // EOF / 读失败 → 退出
            Ok(_) => match byte[0] {
                // Ctrl-C / Ctrl-D：退出约定
                3 | 4 => {
                    let _ = tx.send(Ctl::Exit);
                    return;
                }
                b'\r' | b'\n' => {
                    emit!(b"\r\n");
                    let line = String::from_utf8_lossy(&buf).to_string();
                    buf.clear();
                    if tx.send(Ctl::Line(line)).is_err() {
                        return;
                    }
                }
                0x7f => {
                    // backspace：终端回抹
                    if buf.pop().is_some() {
                        emit!(b"\x08 \x08");
                    }
                }
                0x15 => {
                    // Ctrl-U 清行
                    buf.clear();
                    emit!(b"\r\x1b[K");
                }
                c if c < 0x20 => {} // 其余控制键忽略
                c => {
                    buf.push(c);
                    emit!(&[c]);
                }
            },
        }
    }
    let _ = tx.send(Ctl::Exit);
}

/// raw mode 守护：构造即进入，drop 即恢复（含 panic 路径）；stdin 非 tty 时为 no-op。
struct RawGuard {
    active: bool,
    orig: libc::termios,
}

impl RawGuard {
    fn enable() -> Self {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            // SAFETY: fd 0 termios 读取；非 tty 返回 ENOTTY，退化为无 raw mode
            let is_tty = libc::isatty(0) == 1 && libc::tcgetattr(0, &mut t) == 0;
            if !is_tty {
                return Self {
                    active: false,
                    orig: t,
                };
            }
            let orig = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Self { active: true, orig }
        }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.active {
            // SAFETY: 恢复 enable 时保存的原始终端态
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.orig) };
        }
    }
}

// —— 手动验证点（真机 raw mode，自动化打不到）：实机终端 attach 真实 claude 测试会话，
// 逐字回显/backspace/Ctrl-U/回车发送/Ctrl-C 退出后终端态恢复。本 feature 交付前已
// 在隔离实例 7905 上用自开测试会话走通（见交付报告）。 ——

#[cfg(test)]
mod tests {
    use super::*;

    fn a(id: &str, name: Option<&str>) -> AgentSummary {
        serde_json::from_value(serde_json::json!({
            "driver": "claude", "id": id, "name": name, "cwd": null,
            "kind": null, "rawState": null, "group": "working",
            "detail": null, "tokens": null, "startedAt": null, "sessionId": null,
        }))
        .unwrap()
    }

    #[test]
    fn resolve_exact_name_then_id_then_prefix() {
        let agents = vec![a("job-1", Some("日报")), a("job-2", Some("dec-staging"))];
        // name 精确
        assert_eq!(resolve(&agents, "日报").unwrap().id, "job-1");
        // id 精确（同名不存在时）
        assert_eq!(resolve(&agents, "job-2").unwrap().id, "job-2");
        // 唯一前缀（大小写不敏感）
        assert_eq!(resolve(&agents, "DEC").unwrap().id, "job-2");
        // 无命中
        assert!(matches!(
            resolve(&agents, "xx"),
            Err(ResolveError::NotFound)
        ));
    }

    #[test]
    fn resolve_prefix_ambiguity_lists_candidates() {
        let agents = vec![a("job-1", Some("dec-a")), a("job-2", Some("dec-b"))];
        match resolve(&agents, "dec") {
            Err(ResolveError::Ambiguous(v)) => assert_eq!(v.len(), 2),
            other => panic!("应歧义，实得 {other:?}"),
        }
    }

    #[test]
    fn render_event_kinds() {
        let m = |kind: &str, text: Option<&str>, tool: Option<&str>| -> ChatMessage {
            serde_json::from_value(serde_json::json!({
                "kind": kind, "text": text, "toolName": tool,
            }))
            .unwrap()
        };
        assert_eq!(
            render_event(&m("user", Some("你好"), None)).unwrap(),
            "你 ▸ 你好"
        );
        assert_eq!(
            render_event(&m("assistant", Some("回复"), None)).unwrap(),
            "AI ▸ 回复"
        );
        assert_eq!(
            render_event(&m("tool_use", None, Some("Bash"))).unwrap(),
            "  [工具] Bash"
        );
        // tool_result / thinking / 空文本不渲染
        assert!(render_event(&m("tool_result", Some("ok"), None)).is_none());
        assert!(render_event(&m("thinking", Some("..."), None)).is_none());
        assert!(render_event(&m("assistant", Some(""), None)).is_none());
    }
}
