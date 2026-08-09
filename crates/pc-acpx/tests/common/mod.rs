//! Shared sshd fixture for integration tests.
//!
//! Many integration tests need a real sshd running on a loopback port.
//! Instead of duplicating ~150 lines of boilerplate per test file, every
//! test that needs a sshd fixture should:
//!
//! ```ignore
//! mod common;
//! use common::SshLabFixture;
//!
//! let Some(fixture) = SshLabFixture::start("my_test_name").await else {
//!     return;
//! };
//! ```
//!
//! Mirrors the previous per-file `SshLabFixture` shape exactly — `start()`
//! returns `Option<Self>`, callers `return` on `None` (which signals
//! missing prerequisites, not a test failure).

use pc_acpx::bridge_executor::BridgeCommandRunner;
use pc_acpx::execution_target::AdapterExecutionTarget;
use pc_acpx::ssh::{
    run_ssh_command, SshCommandManagedRuntimeRunner, SshCommandOptions,
    SshConnectionConfig, SshRemoteExecutionSpec,
};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

/// Try to find an executable on PATH.
pub fn command_available(command: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {command}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Allocate a free loopback port and return its number.
pub fn allocate_loopback_port() -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// A running loopback sshd + matching `SshRemoteExecutionSpec`. Dropping
/// the fixture terminates the sshd child and removes the temp directory.

/// Try to detect whether `node` is on PATH. Tests that exercise the
/// real bridge server (which spawns `node`) gate on this and bail
/// early with `SKIP` when the binary is missing.
pub fn node_available() -> bool {
    command_available("node")
}


/// Run `git init -q` in the given directory. Returns true if the
/// command succeeded. Used by tests that already manage their own
/// commit messages and file seeds.
pub fn init_git_repo(dir: &std::path::Path) -> bool {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Create a fresh local git repo with one commit, returning its
/// root path. Returns `None` if `git` is not on PATH (callers should
/// `return` from the test in that case to mark it skipped).
pub async fn init_local_repo_with_commit(label: &str, message: &str) -> Option<PathBuf> {
    if !command_available("git") {
        eprintln!("SKIP: git unavailable");
        return None;
    }
    let root_dir = std::env::temp_dir().join(format!(
        "paperclip-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root_dir).ok()?;
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&root_dir)
            .env("GIT_AUTHOR_NAME", "paperclip-test")
            .env("GIT_AUTHOR_EMAIL", "test@paperclip.local")
            .env("GIT_COMMITTER_NAME", "paperclip-test")
            .env("GIT_COMMITTER_EMAIL", "test@paperclip.local")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if !run(&["init", "-q"]) {
        return None;
    }
    // Seed at least one file so the snapshot has content.
    std::fs::write(root_dir.join("README.md"), format!("# {label}\n")).ok()?;
    if !run(&["add", "README.md"]) || !run(&["commit", "-q", "-m", message]) {
        return None;
    }
    Some(root_dir)
}

pub struct SshLabFixture {
    /// Convenience handle to the underlying [`SshConnectionConfig`]
    /// (drops `remote_cwd`). Test code frequently references
    /// `fixture.config.host` etc., so the field lives alongside
    /// [`Self::spec`] for ergonomics.
    pub config: SshConnectionConfig,
    pub spec: SshRemoteExecutionSpec,
    pub root_dir: PathBuf,
    pub pid: u32,
}

impl SshLabFixture {
    /// Start a real sshd on a loopback port. Returns `None` if the required
    /// tools (`ssh`, `sshd`, `ssh-keygen`) are missing — callers should
    /// `return` from the test in that case to mark it skipped, not failed.
    ///
    /// `name` is used to namespace the temp directory so concurrent test
    /// runs and `round*` identifiers don't collide.
    pub async fn start(name: &str) -> Option<Self> {
        if !command_available("ssh")
            || !command_available("sshd")
            || !command_available("ssh-keygen")
        {
            eprintln!("SKIP: ssh/sshd/ssh-keygen unavailable");
            return None;
        }
        let port = allocate_loopback_port()?;
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-{}-{}",
            name,
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
        ]) {
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
        let spec = SshRemoteExecutionSpec::from_parts(
            config,
            remote_workspace_path.to_string_lossy().into_owned(),
        );
        let config = spec.as_connection_config();
        Some(Self {
            config,
            spec,
            root_dir,
            pid,
        })
    }
}


impl SshLabFixture {
    /// Borrow the underlying `SshConnectionConfig` (drops `remote_cwd`).
    /// Used by callers that only need connection-level fields.
    #[must_use]
    pub fn config(&self) -> SshConnectionConfig {
        self.spec.as_connection_config()
    }

    /// Build an `SshCommandManagedRuntimeRunner` bound to this fixture.
    /// Returns the underlying concrete type so callers can use
    /// `Arc::new(fixture.runner())` to coerce into `Arc<dyn BridgeCommandRunner>`.
    #[must_use]
    pub fn runner(&self) -> SshCommandManagedRuntimeRunner {
        SshCommandManagedRuntimeRunner::new(self.spec.clone(), None, None)
    }

    /// Build a sandbox-shaped `AdapterExecutionTarget` for this fixture.
    /// Mirrors the shape used by bridge/start tests that require a target.
    #[must_use]
    pub fn target(&self) -> AdapterExecutionTarget {
        // The integration tests only need a target that names the same SSH
        // host/port the fixture already exposes; the helper reuses the
        // canonical SSH constructor so type/identity stay in sync.
        AdapterExecutionTarget::from_remote_execution_ssh(self.spec.clone())
    }

    /// Convenience: invoke [`run_ssh_command`] against this fixture.
    pub async fn run(
        &self,
        script: &str,
        options: SshCommandOptions,
    ) -> Result<pc_acpx::ssh::SshCommandResult, String> {
        run_ssh_command(&self.spec.as_connection_config(), script, &options)
            .await
            .map_err(|error| error.message)
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
