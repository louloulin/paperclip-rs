//! R498 — git workspace import/export via real sshd fixture.
//!
//! 端到端验证 `pc_acpx::git_workspace_sync` 的 `import_git_workspace_to_ssh`
//! + `export_git_workspace_from_ssh`：用真实 sshd + 真实 git repo + 真实
//! `git bundle` 传输 + 远端 `git init` / `fetch` / `checkout` / `bundle create`。
//!
//! 缺失 sshd / git / ssh-keygen 时跳过。

use pc_acpx::git_workspace_sync::{
    export_git_workspace_from_ssh, import_git_workspace_to_ssh, read_git_workspace_snapshot,
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
        {
            eprintln!("SKIP: ssh/sshd/ssh-keygen unavailable");
            return None;
        }
        let port = allocate_loopback_port()?;
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-r498-git-sync-{}",
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
        // Wait for sshd to be ready by polling `echo probe`.
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

async fn init_local_repo_with_commit(name: &str, message: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "paperclip-r498-local-{name}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).expect("create dir");
    let dir_str = dir.to_string_lossy().to_string();
    pc_acpx::git_workspace_sync::run_local_git(&dir_str, &["init", "-q"], None, None)
        .await
        .expect("git init");
    pc_acpx::git_workspace_sync::run_local_git(
        &dir_str,
        &["config", "user.email", "test@example.com"],
        None,
        None,
    )
    .await
    .expect("git config email");
    pc_acpx::git_workspace_sync::run_local_git(
        &dir_str,
        &["config", "user.name", "Test"],
        None,
        None,
    )
    .await
    .expect("git config name");
    std::fs::write(dir.join("hello.txt"), "hello from local\n").expect("write hello");
    pc_acpx::git_workspace_sync::run_local_git(&dir_str, &["add", "hello.txt"], None, None)
        .await
        .expect("git add");
    pc_acpx::git_workspace_sync::run_local_git(
        &dir_str,
        &["commit", "-q", "-m", message],
        None,
        None,
    )
    .await
    .expect("git commit");
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn import_git_workspace_to_ssh_runs_remote_git_init_and_checkout() {
    if !command_available("git") {
        eprintln!("SKIP: git unavailable");
        return;
    }
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let local = init_local_repo_with_commit("import", "init commit").await;
    let snapshot = read_git_workspace_snapshot(&local.to_string_lossy())
        .await
        .expect("snapshot")
        .expect("Some");
    let remote_dir = format!("{}/remote-workspace", fixture.root_dir.display());
    std::fs::create_dir_all(&remote_dir).expect("mkdir remote");

    import_git_workspace_to_ssh(&fixture.spec, &local, &remote_dir, &snapshot)
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
    let Some(fixture) = SshLabFixture::start().await else {
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
        export_git_workspace_from_ssh(&fixture.spec, &remote_dir, &local, true)
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
