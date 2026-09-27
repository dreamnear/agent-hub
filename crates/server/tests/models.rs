use server::models::{map_group, AgentSummary, Group};

#[test]
fn map_group_table() {
    assert_eq!(map_group("blocked"), Group::NeedsInput);
    assert_eq!(map_group("working"), Group::Working);
    assert_eq!(map_group("running"), Group::Working);
    assert_eq!(map_group("active"), Group::Working);
    assert_eq!(map_group("exited"), Group::Completed);
    assert_eq!(map_group("weird"), Group::Other);
}

#[test]
fn agent_summary_serializes_camel_case() {
    let a = AgentSummary {
        driver: "claude".into(),
        id: "abc".into(),
        name: None,
        cwd: None,
        kind: None,
        raw_state: Some("blocked".into()),
        group: Group::NeedsInput,
        detail: None,
        tokens: None,
        started_at: Some(1),
        session_id: None,
    };
    let json = serde_json::to_string(&a).unwrap();
    assert!(json.contains("\"group\":\"needs_input\""), "got: {json}");
    assert!(json.contains("\"startedAt\":1"), "got: {json}");
    assert!(json.contains("\"sessionId\""), "got: {json}");
    assert!(json.contains("\"rawState\""), "got: {json}");
}
