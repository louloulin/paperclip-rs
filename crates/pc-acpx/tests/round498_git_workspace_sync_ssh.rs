//! R498 — git workspace import/export via real sshd fixture.
//!
//! 端到端验证 `pc_acpx::git_workspace_sync` 的 `import_git_workspace_to_ssh`
//! + `export_git_workspace_from_ssh`：用真实 sshd + 真实 git repo + 真实
//! `git bundle` 传输 + 远端 `git init` / `fetch` / `checkout` / `bundle create`。
//!
//! 缺失 sshd / git / ssh-keygen 时跳过。

mod common;
use crate::common::{command_available, init_local_repo_with_commit, SshLabFixture};

use pc_acpx::git_workspace_sync::{
    export_git_workspace_from_ssh, import_git_workspace_to_ssh, read_git_workspace_snapshot,
};
use pc_acpx::ssh::SshRemoteExecutionSpec;
use std::path::{Path, PathBuf};
use std::time::Duration;


#[tokio::test(flavor = "multi_thread")]
async fn import_git_workspace_to_ssh_runs_remote_git_init_and_checkout() {
    if !command_available("git") {
        eprintln!("SKIP: git unavailable");
        return;
    }
    let Some(fixture) = SshLabFixture::start("r498").await else {
        return;
    };
    let Some(local) = init_local_repo_with_commit("import", "init commit").await else {
        return;
    };
    let snapshot = read_git_workspace_snapshot(&local.to_string_lossy())
        .await
        .expect("snapshot")
        .expect("Some");
    let remote_dir = format!("{}/remote-workspace", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");

    import_git_workspace_to_ssh(&fixture.spec, &local, &remote_dir, &snapshot, None)
        .await
        .expect("import should succeed");

    // Verify remote: .git created + hello.txt checked out.
    assert!(
        std::path::Path::new(&remote_dir).join(".git").exists(),
        "remote .git must exist after import"
    );
    let remote_hello = std::path::Path::new(&remote_dir).join("hello.txt");
    assert!(
        remote_hello.exists(),
        "remote hello.txt must be checked out"
    );
    let content = std::fs::read_to_string(&remote_hello).expect("read remote hello");
    assert!(
        content.contains("hello from local"),
        "remote hello.txt must contain local content; got: {content}"
    );

    let _ = std::fs::remove_dir_all(&local);
}

#[tokio::test(flavor = "multi_thread")]
async fn export_git_workspace_from_ssh_runs_remote_bundle_create_and_local_reset() {
    if !command_available("git") {
        eprintln!("SKIP: git unavailable");
        return;
    }
    let Some(fixture) = SshLabFixture::start("r498").await else {
        return;
    };
    // Set up remote with a git repo + commit.
    let remote_dir = format!("{}/remote-export", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    let remote_str = remote_dir.clone();
    pc_acpx::git_workspace_sync::run_local_git(&remote_str, &["init", "-q"], None, None)
        .await
        .expect("remote git init");
    pc_acpx::git_workspace_sync::run_local_git(
        &remote_str,
        &["config", "user.email", "test@example.com"],
        None,
        None,
    )
    .await
    .expect("remote git config email");
    pc_acpx::git_workspace_sync::run_local_git(
        &remote_str,
        &["config", "user.name", "Test"],
        None,
        None,
    )
    .await
    .expect("remote git config name");
    std::fs::write(std::path::Path::new(&remote_dir).join("world.txt"), "world from remote\n")
        .expect("write world");
    pc_acpx::git_workspace_sync::run_local_git(&remote_str, &["add", "world.txt"], None, None)
        .await
        .expect("remote git add");
    pc_acpx::git_workspace_sync::run_local_git(
        &remote_str,
        &["commit", "-q", "-m", "remote init"],
        None,
        None,
    )
    .await
    .expect("remote git commit");

    // Set up local empty repo.
    let local = std::env::temp_dir().join(format!(
        "paperclip-r498-local-export-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&local).expect("mkdir local");
    let local_str = local.to_string_lossy().to_string();
    pc_acpx::git_workspace_sync::run_local_git(&local_str, &["init", "-q"], None, None)
        .await
        .expect("local git init");

    let imported_head =
        export_git_workspace_from_ssh(&fixture.spec, &remote_dir, &local, true, None)
            .await
            .expect("export should succeed");

    // Verify imported head SHA matches remote HEAD.
    let remote_head = pc_acpx::git_workspace_sync::run_local_git(
        &remote_str,
        &["rev-parse", "HEAD"],
        Some(5_000),
        None,
    )
    .await
    .expect("remote rev-parse");
    assert_eq!(imported_head, remote_head.stdout.trim());

    // Verify local working tree has world.txt.
    let local_world = local.join("world.txt");
    assert!(
        local_world.exists(),
        "local world.txt must exist after export"
    );
    let content = std::fs::read_to_string(&local_world).expect("read world");
    assert!(content.contains("world from remote"));

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}
