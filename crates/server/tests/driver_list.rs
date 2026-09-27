use std::path::{Path, PathBuf};

use server::drivers::claude::ClaudeDriver;
use server::models::Group;

fn fake_bin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join("fake-claude.sh")
}

#[tokio::test]
async fn merges_jobs_detail_and_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("a1b2c3d4");
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(
        job.join("state.json"),
        r#"{"detail":"from jobs","tokens":999,"name":"jobs-name"}"#,
    )
    .unwrap();

    let driver = ClaudeDriver {
        bin: fake_bin(),
        jobs_dir: dir.path().to_path_buf(),
    };
    let list = driver.list(false).await.unwrap();
    assert_eq!(list.len(), 3);

    let a = list.iter().find(|a| a.id == "a1b2c3d4").unwrap();
    assert_eq!(a.driver, "claude");
    assert_eq!(a.group, Group::NeedsInput);
    assert_eq!(a.detail.as_deref(), Some("from jobs"));
    assert_eq!(a.tokens, Some(999));
    assert_eq!(a.name.as_deref(), Some("refactor-auth"), "CLI 优先于 jobs");
}

#[tokio::test]
async fn degrades_silently_on_bad_jobs_file() {
    let dir = tempfile::tempdir().unwrap();
    let job = dir.path().join("a1b2c3d4");
    std::fs::create_dir_all(&job).unwrap();
    std::fs::write(job.join("state.json"), "<<<half-written>>>").unwrap();

    let driver = ClaudeDriver {
        bin: fake_bin(),
        jobs_dir: dir.path().to_path_buf(),
    };
    let list = driver.list(false).await.unwrap();
    let a = list.iter().find(|a| a.id == "a1b2c3d4").unwrap();
    assert_eq!(a.detail, None, "坏 jobs 文件静默降级");
    assert_eq!(a.group, Group::NeedsInput, "分组仍来自 CLI raw_state");
}
