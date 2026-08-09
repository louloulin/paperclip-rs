//! R505 — `prepare_workspace_for_ssh_execution` + `restore_workspace_from_ssh_execution`
//! end-to-end via real sshd fixture.
//!
//! 验证两个 orchestration 入口：
//! 1. **git-backed 路径**：本地建 git repo + 文件 → prepare → 远端有 `.git` +
//!    文件 + 在远端改文件 → restore → 本地文件更新
//! 2. **非 git 路径**：本地无 `.git` → prepare → 远端有文件 → 远端改文件 →
//!    restore → 本地文件更新
//!
//! 缺失 sshd/git/tar 时跳过。

mod common;
use crate::common::{init_git_repo, SshLabFixture};

use pc_acpx::git_workspace_sync::{
    prepare_workspace_for_ssh_execution, restore_workspace_from_ssh_execution,
};
use pc_acpx::ssh::SshRemoteExecutionSpec;
use std::path::{Path, PathBuf};
use std::time::Duration;


#[tokio::test(flavor = "multi_thread")]
async fn prepare_restore_roundtrip_git_backed_workspace() {
    let Some(fixture) = SshLabFixture::start("r505").await else {
        return;
    };

    // Build a git-backed local workspace.
    let local = std::env::temp_dir().join(format!(
        "paperclip-r505-git-local-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&local).expect("mkdir local");
    init_git_repo(&local);
    std::fs::write(local.join("README.md"), "# Test\n").expect("write README");
    std::fs::write(local.join("src.txt"), "alpha\n").expect("write src");
    let commit = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(&local)
        .status();
    assert!(commit.unwrap().success());
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", "initial"])
        .current_dir(&local)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status();
    assert!(commit.unwrap().success());

    let remote_dir = format!("{}/git-target", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");

    let git_backed = prepare_workspace_for_ssh_execution(
        &fixture.spec,
        &local,
        &remote_dir,
        None,
        None,
    )
    .await
    .expect("prepare should succeed");
    assert!(git_backed, "should detect git-backed workspace");

    // After prepare: remote should have README.md + src.txt + .git/.
    assert!(Path::new(&remote_dir).join("README.md").exists());
    assert!(Path::new(&remote_dir).join("src.txt").exists());
    assert!(Path::new(&remote_dir).join(".git").exists());

    // Simulate remote mutation: change src.txt on remote.
    std::fs::write(
        Path::new(&remote_dir).join("src.txt"),
        "remote-edit\n",
    )
    .expect("remote write");

    restore_workspace_from_ssh_execution(
        &fixture.spec,
        &local,
        &remote_dir,
        None,
        None,
    )
    .await
    .expect("restore should succeed");

    // After restore: local src.txt must reflect the remote edit.
    let got = std::fs::read_to_string(local.join("src.txt")).expect("read local src");
    assert_eq!(
        got, "remote-edit\n",
        "local src.txt must reflect remote edit after restore"
    );
    // Local .git must still exist (preserve).
    assert!(local.join(".git").exists(), ".git must be preserved");

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn prepare_restore_roundtrip_non_git_workspace() {
    let Some(fixture) = SshLabFixture::start("r505").await else {
        return;
    };

    let local = std::env::temp_dir().join(format!(
        "paperclip-r505-nongit-local-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&local).expect("mkdir local");
    std::fs::write(local.join("config.yaml"), "key: val\n").expect("write config");

    let remote_dir = format!("{}/nongit-target", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    // Pre-create stale file the prepare should overwrite.
    std::fs::write(
        Path::new(&remote_dir).join("config.yaml"),
        "stale: yes\n",
    )
    .expect("write stale");

    let git_backed = prepare_workspace_for_ssh_execution(
        &fixture.spec,
        &local,
        &remote_dir,
        None,
        None,
    )
    .await
    .expect("prepare should succeed");
    assert!(!git_backed, "should detect non-git workspace");

    assert!(Path::new(&remote_dir).join("config.yaml").exists());
    let r = std::fs::read_to_string(Path::new(&remote_dir).join("config.yaml"))
        .expect("read remote config");
    assert_eq!(r, "key: val\n", "remote config.yaml must be overwritten");

    // Mutate remote.
    std::fs::write(
        Path::new(&remote_dir).join("config.yaml"),
        "remote-set: 1\n",
    )
    .expect("remote edit");

    restore_workspace_from_ssh_execution(
        &fixture.spec,
        &local,
        &remote_dir,
        None,
        None,
    )
    .await
    .expect("restore should succeed");

    let got = std::fs::read_to_string(local.join("config.yaml")).expect("read local");
    assert_eq!(
        got, "remote-set: 1\n",
        "local config.yaml must reflect remote edit"
    );

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}
