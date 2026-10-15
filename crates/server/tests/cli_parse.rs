use std::path::Path;

use server::drivers::claude::cli::{
    agents_json, logs, parse_agents_json, remove, start_bg, stop, StartReq,
};

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn parses_fixture_entries() {
    let raw = std::fs::read_to_string(fixture_path("agents.json")).unwrap();
    let parsed = parse_agents_json(&raw);
    // --all 全量 fixture：3 原始条目 + done-alive(f0a1b2c3) + livejob1（send 守卫用）
    assert_eq!(parsed.len(), 5);
    let first = &parsed[0];
    assert_eq!(first.id, "a1b2c3d4");
    assert_eq!(first.cwd.as_deref(), Some("/Users/demo/proj-alpha"));
    assert_eq!(first.kind.as_deref(), Some("background"));
    assert_eq!(first.started_at, Some(1758000000));
    assert_eq!(
        first.session_id.as_deref(),
        Some("3f9a2b7c-1111-2222-3333-444455556666")
    );
    assert_eq!(first.name.as_deref(), Some("refactor-auth"));
    assert_eq!(first.state.as_deref(), Some("blocked"));
    assert_eq!(parsed[1].kind.as_deref(), Some("interactive"));
    assert_eq!(parsed[1].name, None);
}

#[tokio::test]
async fn fake_cli_integration() {
    let fake = fixture_path("fake-claude.sh");
    let cwd = std::env::temp_dir().join("agent-hub-cli-test-cwd");
    std::fs::create_dir_all(&cwd).unwrap();

    // 缺省口径 = 活跃清单（CLI 真实语义：活 daemon 才列，agents-active.json 4 条）
    let list = agents_json(&fake, false).await.unwrap();
    assert_eq!(list.len(), 4);
    assert!(
        list.iter()
            .any(|a| a.id == "f0a1b2c3" && a.state.as_deref() == Some("done")),
        "活 done 会话在缺省清单内（done ≠ 退出的反例锚点）"
    );
    // --all = 全量（含 daemon 已死条目）
    let all = agents_json(&fake, true).await.unwrap();
    assert_eq!(all.len(), 5);
    assert!(all
        .iter()
        .any(|a| a.id == "c9d0e1f2" && a.state.as_deref() == Some("exited")));

    let id = start_bg(
        &fake,
        &cwd,
        &StartReq {
            prompt: "hi".into(),
            name: Some("test".into()),
            model: None,
            effort: None,
            resume: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(id, "abc123");

    assert!(logs(&fake, "abc123").await.is_ok());
    assert!(stop(&fake, "abc123").await.is_ok());
    assert!(remove(&fake, "abc123").await.is_ok());
}

/// agent-hub#10 端到端（函数级）：mock CLI 回放真实 `claude --bg` stdout 形态，
/// start_bg 返回的 id 必须能直接驱动 logs/stop/rm——返回整行或返回名字时
/// mock CLI 会非零退出（模拟真实下游「id 不可用」）。三形态逐条覆盖：
/// 带名（#10 症状）/ 不带名 / resume（#11 路径不回归）。
#[tokio::test]
async fn bg_start_parses_usable_id_for_every_output_shape() {
    let fake = fixture_path("fake-claude-bg.sh");
    let cwd = std::env::temp_dir().join("agent-hub-cli-test-cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let req = |name: Option<&str>, resume: Option<&str>| StartReq {
        prompt: "say ok".into(),
        name: name.map(Into::into),
        model: None,
        effort: None,
        resume: resume.map(Into::into),
    };

    // 用例非空证：污染 id（整行 / 名字）确实会被 mock 拒绝，红了才算数
    assert!(logs(&fake, "backgrounded · 96bad8a7 · hub-test-x")
        .await
        .is_err());
    assert!(logs(&fake, "hub-test-x").await.is_err());

    // 带名（#10 症状形态）：`backgrounded · <id> · <name>`
    let named = start_bg(&fake, &cwd, &req(Some("hub-test-bgid-probe"), None))
        .await
        .unwrap();
    assert_eq!(named, "96bad8a7");
    assert!(logs(&fake, &named).await.is_ok());
    assert!(stop(&fake, &named).await.is_ok());
    assert!(remove(&fake, &named).await.is_ok());

    // 不带名：`backgrounded · <id>`
    let plain = start_bg(&fake, &cwd, &req(None, None)).await.unwrap();
    assert_eq!(plain, "96bad8a7");
    assert!(logs(&fake, &plain).await.is_ok());

    // resume（#11 路径）：note 首行 + backgrounded 行在后
    let resumed = start_bg(
        &fake,
        &cwd,
        &req(None, Some("15c9e382-1658-4703-bdcb-2d236739943f")),
    )
    .await
    .unwrap();
    assert_eq!(resumed, "e5b57ea3");
    assert!(logs(&fake, &resumed).await.is_ok());
}
