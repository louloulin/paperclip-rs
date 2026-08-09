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

use pc_acpx::git_workspace_sync::{
    prepare_workspace_for_ssh_execution, restore_workspace_from_ssh_execution,
};
use pc_acpx::ssh::SshRemoteExecutionSpec;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
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
            || !command_available("git")
        {
            eprintln!("SKIP: ssh/sshd/ssh-keygen/tar/git unavailable");
            return None;
        }
        let port = allocate_loopback_port()?;
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-r505-orch-{}",
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

fn init_git_repo(local: &Path) {
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
async fn prepare_restore_roundtrip_git_backed_workspace() {
    let Some(fixture) = SshLabFixture::start().await else {
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
    let Some(fixture) = SshLabFixture::start().await else {
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
