use std::path::Path;

use server::drivers::claude::jobs::{parse_state, parse_timeline, read_job_state};

fn fixture_path(parts: &[&str]) -> std::path::PathBuf {
    let mut p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jobs");
    for part in parts {
        p = p.join(part);
    }
    p
}

#[test]
fn parse_state_extracts_fields() {
    let raw = std::fs::read_to_string(fixture_path(&["a1b2c3d4", "state.json"])).unwrap();
    let s = parse_state(&raw).expect("should parse");
    assert_eq!(s.state.as_deref(), Some("blocked"));
    assert_eq!(
        s.detail.as_deref(),
        Some("Waiting for permission to run Bash command")
    );
    assert_eq!(s.tokens, Some(45231));
    assert_eq!(s.name.as_deref(), Some("refactor-auth"));
    assert_eq!(
        s.session_id,
        Some("3f9a2b7c-1111-2222-3333-444455556666".into())
    );
    assert_eq!(s.cwd.as_deref(), Some("/Users/demo/proj-alpha"));
    assert_eq!(s.updated_at.as_deref(), Some("2026-09-16T12:00:00Z"));
}

#[test]
fn parse_state_rejects_bad_json() {
    assert!(parse_state("<<<half-written>>>").is_none());
}

#[test]
fn parse_timeline_skips_bad_lines() {
    let raw = std::fs::read_to_string(fixture_path(&["a1b2c3d4", "timeline.jsonl"])).unwrap();
    let events = parse_timeline(&raw);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].state.as_deref(), Some("starting"));
    assert_eq!(events[1].state.as_deref(), Some("blocked"));
}

#[tokio::test]
async fn read_job_state_hits_disk() {
    let jobs_dir = std::env::temp_dir().join(format!("agent-hub-jobs-test-{}", std::process::id()));
    let job_dir = jobs_dir.join("xyz789");
    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(
        job_dir.join("state.json"),
        r#"{"state":"running","tokens":10}"#,
    )
    .unwrap();

    let hit = read_job_state(&jobs_dir, "xyz789").await;
    assert_eq!(hit.unwrap().tokens, Some(10));
    assert!(read_job_state(&jobs_dir, "missing").await.is_none());

    std::fs::remove_dir_all(&jobs_dir).ok();
}
