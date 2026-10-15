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
