//! R504 — `stream_local_file_to_ssh` progress sink end-to-end via real sshd.
//!
//! 验证：当调用方传入 `progress` sink 时，bundle 推送过程中 sink 收到
//! 至少一个 `[paperclip] Importing git history to ssh: ... MB` 行 + 终态 100% 行。

mod common;
use crate::common::SshLabFixture;

use pc_acpx::git_workspace_sync::{import_git_workspace_to_ssh, read_git_workspace_snapshot};
use pc_acpx::runtime_progress::RuntimeProgressSink;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

fn init_git_repo(local: &std::path::Path) {
    use std::process::Command;
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(local)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    assert!(run(&["init", "--initial-branch=main"]));
    assert!(run(&["config", "user.email", "test@example.com"]));
    assert!(run(&["config", "user.name", "Test"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn import_git_bundle_progress_sink_receives_lines() {
    let Some(fixture) = SshLabFixture::start("r504-progress").await else {
        return;
    };

    let local = std::env::temp_dir().join(format!(
        "paperclip-r504-progress-local-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&local).expect("mkdir local");
    init_git_repo(&local);
    std::fs::write(local.join("file.txt"), "x".repeat(2048)).expect("write");
    let commit = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&local)
        .status();
    assert!(commit.unwrap().success());
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&local)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status();
    assert!(commit.unwrap().success());

    let snapshot = read_git_workspace_snapshot(&local.to_string_lossy())
        .await
        .expect("snapshot ok")
        .expect("snapshot is Some");

    let remote_dir = format!("{}/progress-target", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");

    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_sink = Arc::clone(&captured);
    let sink: RuntimeProgressSink = Arc::new(move |line: String| {
        captured_for_sink.lock().unwrap().push(line);
    });

    import_git_workspace_to_ssh(&fixture.spec, &local, &remote_dir, &snapshot, Some(&sink))
        .await
        .expect("import should succeed");

    let lines = captured.lock().unwrap();
    assert!(
        !lines.is_empty(),
        "progress sink should receive at least one line"
    );
    assert!(
        lines.iter().any(|l| l.contains("Importing git history")),
        "expected at least one line with 'Importing git history', got: {:?}",
        *lines
    );
    assert!(
        lines.iter().any(|l| l.contains("100%")),
        "expected terminal 100% line, got: {:?}",
        *lines
    );

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}
