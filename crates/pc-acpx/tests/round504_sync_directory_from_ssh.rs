//! R504 — `sync_directory_from_ssh` end-to-end via real sshd fixture.

mod common;
use crate::common::SshLabFixture;

use pc_acpx::git_workspace_sync::sync_directory_from_ssh;
use pc_acpx::ssh::SshRemoteExecutionSpec;


#[tokio::test(flavor = "multi_thread")]
async fn sync_directory_from_ssh_pipes_tar_through_ssh_to_local_extract() {
    let Some(fixture) = SshLabFixture::start("r504").await else { return; };
    let remote_dir = std::env::temp_dir().join(format!("paperclip-r504-remote-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    std::fs::write(remote_dir.join("file1.txt"), "alpha\n").expect("write f1");
    std::fs::write(remote_dir.join("file2.txt"), "beta\n").expect("write f2");
    std::fs::create_dir_all(remote_dir.join("subdir")).expect("mkdir subdir");
    std::fs::write(remote_dir.join("subdir").join("nested.txt"), "gamma\n").expect("write nested");

    let local = std::env::temp_dir().join(format!("paperclip-r504-local-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&local).expect("mkdir local");
    std::fs::write(local.join("file1.txt"), "stale\n").expect("write stale");

    let remote_str = remote_dir.to_string_lossy().into_owned();
    sync_directory_from_ssh(&fixture.spec, &remote_str, &local, None, None, None)
        .await
        .expect("sync should succeed");

    let l1 = std::fs::read_to_string(local.join("file1.txt")).expect("read f1");
    assert_eq!(l1, "alpha\n");
    let l2 = std::fs::read_to_string(local.join("file2.txt")).expect("read f2");
    assert_eq!(l2, "beta\n");
    let ln = std::fs::read_to_string(local.join("subdir").join("nested.txt")).expect("read nested");
    assert_eq!(ln, "gamma\n");

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_directory_from_ssh_preserves_local_entries() {
    let Some(fixture) = SshLabFixture::start("r504").await else { return; };
    let remote_dir = std::env::temp_dir().join(format!("paperclip-r504-remote-pres-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    std::fs::write(remote_dir.join("keep.txt"), "remote\n").expect("write keep");

    let local = std::env::temp_dir().join(format!("paperclip-r504-local-pres-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&local).expect("mkdir local");
    std::fs::write(local.join("user.env"), "SECRET=hi\n").expect("write user.env");
    std::fs::write(local.join("stale.txt"), "stale\n").expect("write stale");

    let remote_str = remote_dir.to_string_lossy().into_owned();
    let preserve = vec!["user.env".to_owned()];
    sync_directory_from_ssh(&fixture.spec, &remote_str, &local, None, Some(&preserve), None)
        .await
        .expect("sync should succeed");

    let keep_contents = std::fs::read_to_string(local.join("keep.txt")).expect("read keep");
    assert_eq!(keep_contents, "remote\n");
    let preserved = std::fs::read_to_string(local.join("user.env")).expect("read env");
    assert_eq!(preserved, "SECRET=hi\n");
    assert!(!local.join("stale.txt").exists());

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_directory_from_ssh_respects_exclude() {
    let Some(fixture) = SshLabFixture::start("r504").await else { return; };
    let remote_dir = std::env::temp_dir().join(format!("paperclip-r504-remote-excl-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    std::fs::write(remote_dir.join("keep.txt"), "keep\n").expect("write keep");
    std::fs::create_dir_all(remote_dir.join("node_modules")).expect("mkdir nm");
    std::fs::write(remote_dir.join("node_modules").join("x.js"), "x").expect("write x");

    let local = std::env::temp_dir().join(format!("paperclip-r504-local-excl-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&local).expect("mkdir local");

    let remote_str = remote_dir.to_string_lossy().into_owned();
    let exclude = vec!["node_modules".to_owned()];
    sync_directory_from_ssh(&fixture.spec, &remote_str, &local, Some(&exclude), None, None)
        .await
        .expect("sync should succeed");

    assert!(local.join("keep.txt").exists());
    assert!(!local.join("node_modules").exists());

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}
