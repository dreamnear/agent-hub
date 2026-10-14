use std::path::{Path, PathBuf};
use std::time::Duration;

use server::drivers::claude::attach::{attach, validate_id};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn fake_bin() -> PathBuf {
    fixture_dir().join("fake-claude.sh")
}

#[test]
fn rejects_bad_ids() {
    assert!(validate_id("abc123").is_ok());
    assert!(validate_id("a1b2c3d4-0987").is_ok());
    assert!(validate_id("").is_err());
    assert!(validate_id("../escape").is_err());
    assert!(validate_id("a/b").is_err());
    assert!(validate_id("a b").is_err());
    assert!(validate_id("$(cmd)").is_err());
}

#[tokio::test]
async fn attach_write_kill_roundtrip() {
    let mut sess = attach(&fake_bin(), "abc123")
        .await
        .expect("attach spawn 应成功");
    // 等 PTY 内子进程（bash → cat）就绪
    tokio::time::sleep(Duration::from_millis(400)).await;

    sess.write_stdin("hello-attach\r").expect("写 stdin 应成功");
    // cat 从 PTY 读到后落盘有短暂延迟
    tokio::time::sleep(Duration::from_millis(400)).await;

    let f = fixture_dir().join("attach-stdin-abc123.txt");
    let content = std::fs::read_to_string(&f).expect("attach stdin 文件应存在");
    assert!(
        content.contains("hello-attach"),
        "透传内容应到 fake stdin，got: {content:?}"
    );

    sess.kill().expect("kill 应成功");
    // 进程组 SIGKILL 后 cat 停止写——再写一段验证文件不再变化
    let before = std::fs::read_to_string(&f).unwrap();
    let _ = sess.write_stdin("AFTER-KILL");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = std::fs::read_to_string(&f).unwrap();
    assert_eq!(before, after, "kill 后子进程应已结束，stdin 不再被消费");

    std::fs::remove_file(&f).ok();
}
