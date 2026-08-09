//! R502 — `sync_directory_to_ssh` end-to-end via real sshd fixture.
//!
//! 用真实 sshd 验证 tar-based directory sync：本地建一个有多个文件的目录，
//! 调用 `sync_directory_to_ssh` 后远端应出现同名 + 同内容文件。缺失 sshd/tar
//! 时跳过。

use pc_acpx::git_workspace_sync::sync_directory_to_ssh;
use pc_acpx::ssh::SshRemoteExecutionSpec;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

fn command_available(command: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {command}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn allocate_loopback_port() -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

struct SshLabFixture {
    spec: SshRemoteExecutionSpec,
    root_dir: PathBuf,
    pid: u32,
}

impl SshLabFixture {
    async fn start() -> Option<Self> {
        if !command_available("ssh")
            || !command_available("sshd")
            || !command_available("ssh-keygen")
            || !command_available("tar")
        {
            eprintln!("SKIP: ssh/sshd/ssh-keygen/tar unavailable");
            return None;
        }
        let port = allocate_loopback_port()?;
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-r502-sync-{}",
            uuid::Uuid::new_v4()
        ));
        let remote_workspace_path = root_dir.join("workspace");
        std::fs::create_dir_all(&remote_workspace_path).ok()?;
        let username = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
        let client_key = root_dir.join("client_key");
        let host_key = root_dir.join("host_key");
        let authorized_keys = root_dir.join("authorized_keys");
        let known_hosts_path = root_dir.join("known_hosts");
        let sshd_config_path = root_dir.join("sshd_config");
        let sshd_log_path = root_dir.join("sshd.log");
        let sshd_pid_path = root_dir.join("sshd.pid");

        let gen = |args: &[&str]| {
            std::process::Command::new("ssh-keygen")
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !gen(&[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            client_key.to_str().unwrap(),
        ]) || !gen(&[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-f",
            host_key.to_str().unwrap(),
        ])
        {
            return None;
        }
        let _ = std::fs::copy(client_key.with_extension("pub"), &authorized_keys);
        let host_public_key = std::process::Command::new("ssh-keygen")
            .args(["-y", "-f", host_key.to_str().unwrap()])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let known_hosts_entry = pc_acpx::ssh::build_known_hosts_entry(
            pc_acpx::ssh::KnownHostsEntryInput {
                host: "127.0.0.1".to_string(),
                port,
                public_key: host_public_key,
            },
        );
        let _ = std::fs::write(&known_hosts_path, format!("{known_hosts_entry}\n"));
        let config_text = format!(
            "Port {port}\n\
             ListenAddress 127.0.0.1\n\
             HostKey {}\n\
             PidFile {}\n\
             AuthorizedKeysFile {}\n\
             PasswordAuthentication no\n\
             ChallengeResponseAuthentication no\n\
             KbdInteractiveAuthentication no\n\
             PubkeyAuthentication yes\n\
             PermitRootLogin no\n\
             UsePAM no\n\
             StrictModes no\n\
             AllowUsers {username}\n\
             LogLevel VERBOSE\n\
             PrintMotd no\n\
             UseDNS no\n",
            host_key.display(),
            sshd_pid_path.display(),
            authorized_keys.display(),
        );
        let _ = std::fs::write(&sshd_config_path, &config_text);
        let child = tokio::process::Command::new("sshd")
            .args([
                "-D",
                "-f",
                sshd_config_path.to_str().unwrap(),
                "-E",
                sshd_log_path.to_str().unwrap(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let pid = child.id().unwrap_or(0);
        let config = pc_acpx::ssh::SshConnectionConfig {
            host: "127.0.0.1".to_string(),
            port,
            username: username.clone(),
            private_key: Some(std::fs::read_to_string(&client_key).ok()?),
            known_hosts: Some(std::fs::read_to_string(&known_hosts_path).ok()?),
            strict_host_key_checking: true,
            remote_workspace_path: remote_workspace_path.to_string_lossy().into_owned(),
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            let probe = pc_acpx::ssh::run_ssh_command(
                &config,
                "echo probe",
                &pc_acpx::ssh::SshCommandOptions {
                    env: std::collections::BTreeMap::new(),
                    stdin: None,
                    timeout_ms: 1_500,
                    max_buffer: 64 * 1024,
                },
            )
            .await;
            if probe.is_ok() {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if !ready {
            let _ = tokio::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            return None;
        }
        let spec = SshRemoteExecutionSpec::from_parts(config, remote_workspace_path.to_string_lossy().into_owned());
        Some(Self {
            spec,
            root_dir,
            pid,
        })
    }
}

impl Drop for SshLabFixture {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &self.pid.to_string()])
            .status();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let alive = std::process::Command::new("kill")
                .args(["-0", &self.pid.to_string()])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_directory_to_ssh_pipes_tar_through_ssh_to_remote_extract() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let local = std::env::temp_dir().join(format!(
        "paperclip-r502-local-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&local).expect("mkdir local");
    std::fs::write(local.join("file1.txt"), "alpha\n").expect("write f1");
    std::fs::write(local.join("file2.txt"), "beta\n").expect("write f2");
    std::fs::create_dir_all(local.join("subdir")).expect("mkdir subdir");
    std::fs::write(local.join("subdir").join("nested.txt"), "gamma\n").expect("write nested");

    let remote_dir = format!("{}/remote-target", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");
    // Pre-create a stale file the sync should overwrite.
    std::fs::write(std::path::Path::new(&remote_dir).join("file1.txt"), "stale\n").expect("write stale");

    sync_directory_to_ssh(&fixture.spec, &local, &remote_dir, None, false, None)
        .await
        .expect("sync should succeed");

    // Verify remote contents.
    let r1 = std::fs::read_to_string(std::path::Path::new(&remote_dir).join("file1.txt"))
        .expect("read r1");
    assert_eq!(r1, "alpha\n", "file1.txt must be overwritten by tar extract");

    let r2 = std::fs::read_to_string(std::path::Path::new(&remote_dir).join("file2.txt"))
        .expect("read r2");
    assert_eq!(r2, "beta\n");

    let rn = std::fs::read_to_string(
        std::path::Path::new(&remote_dir).join("subdir").join("nested.txt"),
    )
    .expect("read nested");
    assert_eq!(rn, "gamma\n");

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_directory_to_ssh_respects_exclude() {
    let Some(fixture) = SshLabFixture::start().await else {
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
    sync_directory_to_ssh(&fixture.spec, &local, &remote_dir, Some(&exclude), false, None)
        .await
        .expect("sync should succeed");

    // keep.txt must be there.
    assert!(
        std::path::Path::new(&remote_dir).join("keep.txt").exists(),
        "keep.txt must be synced"
    );
    // node_modules must NOT be there.
    assert!(
        !std::path::Path::new(&remote_dir).join("node_modules").exists(),
        "node_modules must be excluded by --exclude"
    );

    let _ = std::fs::remove_dir_all(&local);
    let _ = std::fs::remove_dir_all(&remote_dir);
}
