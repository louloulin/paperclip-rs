//! R502 — `sync_directory_to_ssh` end-to-end via real sshd fixture.
//!
//! 用真实 sshd 验证 tar-based directory sync：本地建一个有多个文件的目录，
//! 调用 `sync_directory_to_ssh` 后远端应出现同名 + 同内容文件。缺失 sshd/tar
//! 时跳过。

mod common;
use crate::common::SshLabFixture;

use pc_acpx::git_workspace_sync::sync_directory_to_ssh;
use pc_acpx::ssh::SshRemoteExecutionSpec;

#[tokio::test(flavor = "multi_thread")]
async fn sync_directory_to_ssh_pipes_tar_through_ssh_to_remote_extract() {
    let Some(fixture) = SshLabFixture::start("r502").await else {
        return;
    };
    let local = std::env::temp_dir().join(format!("paperclip-r502-local-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&local).expect("mkdir local");
    std::fs::write(local.join("file1.txt"), "alpha\n").expect("write f1");
    std::fs::write(local.join("file2.txt"), "beta\n").expect("write f2");
    std::fs::create_dir_all(local.join("subdir")).expect("mkdir subdir");
    std::fs::write(local.join("subdir").join("nested.txt"), "gamma\n").expect("write nested");

    let remote_dir = format!("{}/remote-target", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    // Pre-create a stale file the sync should overwrite.
    std::fs::write(
        std::path::Path::new(&remote_dir).join("file1.txt"),
        "stale\n",
    )
    .expect("write stale");

    sync_directory_to_ssh(&fixture.spec, &local, &remote_dir, None, false, None)
        .await
        .expect("sync should succeed");

    // Verify remote contents.
    let r1 = std::fs::read_to_string(std::path::Path::new(&remote_dir).join("file1.txt"))
        .expect("read r1");
    assert_eq!(
        r1, "alpha\n",
        "file1.txt must be overwritten by tar extract"
    );

    let r2 = std::fs::read_to_string(std::path::Path::new(&remote_dir).join("file2.txt"))
        .expect("read r2");
    assert_eq!(r2, "beta\n");

    let rn = std::fs::read_to_string(
        std::path::Path::new(&remote_dir)
            .join("subdir")
            .join("nested.txt"),
    )
    .expect("read nested");
    assert_eq!(rn, "gamma\n");

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_directory_to_ssh_respects_exclude() {
    let Some(fixture) = SshLabFixture::start("r502").await else {
        return;
    };
    let local = std::env::temp_dir().join(format!(
        "paperclip-r502-local-excl-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&local).expect("mkdir local");
    std::fs::write(local.join("keep.txt"), "keep\n").expect("write keep");
    std::fs::create_dir_all(local.join("node_modules")).expect("mkdir nm");
    std::fs::write(local.join("node_modules").join("x.js"), "x").expect("write x");

    let remote_dir = format!("{}/remote-excl", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");

    let exclude = vec!["node_modules".to_owned()];
    sync_directory_to_ssh(
        &fixture.spec,
        &local,
        &remote_dir,
        Some(&exclude),
        false,
        None,
    )
    .await
    .expect("sync should succeed");

    // keep.txt must be there.
    assert!(
        std::path::Path::new(&remote_dir).join("keep.txt").exists(),
        "keep.txt must be synced"
    );
    // node_modules must NOT be there.
    assert!(
        !std::path::Path::new(&remote_dir)
            .join("node_modules")
            .exists(),
        "node_modules must be excluded by --exclude"
    );

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}
