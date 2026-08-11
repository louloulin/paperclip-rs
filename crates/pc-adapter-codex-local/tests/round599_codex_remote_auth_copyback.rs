//! R599 — Codex 远端 auth copy-back 真实 SSH 端到端验证。
//!
//! 验证 `codex_remote_home::read_remote_codex_auth`（SSH `cat` 远端
//! `auth.json`） + `auth_copyback::copy_back_codex_auth`（生产决策器
//! `CodexAuthMergeDecider`）+ 决策谓词 `decide_codex_auth_merge_from_paths`
//! 真实串联工作的能力：
//! 1. 在远端 sandbox home 写入一份「更新 + 同 account」的 subscription auth.json
//! 2. 在 host 端写入一份「更旧 + 同 account」的 subscription auth.json
//! 3. 通过真实 sshd fixture 把 sandbox 凭据 `cat` 拉回 host
//! 4. 用生产决策器执行 `copy_back_codex_auth`，验证 host auth.json 被替换
//! 5. 验证反向场景：sandbox 凭据更旧 → 保留 host
//! 6. 验证 `KeptHost` 路径：sandbox 没有 auth.json → 不写 host
//!
//! sshd 缺失时跳过真实部分。

use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
use pc_acpx::ssh::{run_ssh_command, shell_quote, SshCommandOptions, SshConnectionConfig};
use pc_adapter_codex_local::auth_copyback::{
    copy_back_codex_auth, CodexAuthMergeDecider, CopyBackCodexAuthInput, CopyBackCodexAuthOutcome,
};
use pc_adapter_codex_local::codex_remote_home::read_remote_codex_auth;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

struct SshLabFixture {
    config: SshConnectionConfig,
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
        let root_dir =
            std::env::temp_dir().join(format!("paperclip-r599-codex-ssh-{}", uuid::Uuid::new_v4()));
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
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
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
        let known_hosts_entry =
            pc_acpx::ssh::build_known_hosts_entry(pc_acpx::ssh::KnownHostsEntryInput {
                host: "127.0.0.1".to_string(),
                port,
                public_key: host_public_key,
            });
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
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let pid = child.id().unwrap_or(0);
        let config = SshConnectionConfig {
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
            let probe = run_ssh_command(
                &config,
                "echo probe",
                &SshCommandOptions {
                    env: BTreeMap::new(),
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
        Some(Self {
            config,
            root_dir,
            pid,
        })
    }

    async fn write_remote_file(&self, path: &str, content: &str) {
        let quoted = shell_quote(content);
        let cmd = format!("mkdir -p $(dirname {path}) && printf %s {quoted} > {path}");
        run_ssh_command(
            &self.config,
            &cmd,
            &SshCommandOptions {
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: 5_000,
                max_buffer: 64 * 1024,
            },
        )
        .await
        .expect("write remote file");
    }

    async fn remove_remote_file(&self, path: &str) {
        let _ = run_ssh_command(
            &self.config,
            &format!("rm -f {path}"),
            &SshCommandOptions {
                env: BTreeMap::new(),
                stdin: None,
                timeout_ms: 5_000,
                max_buffer: 64 * 1024,
            },
        )
        .await;
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

fn ssh_target_json(fixture: &SshLabFixture) -> serde_json::Value {
    serde_json::json!({
        "kind": "remote",
        "transport": "ssh",
        "host": fixture.config.host,
        "port": fixture.config.port,
        "username": fixture.config.username,
        "remoteWorkspacePath": fixture.config.remote_workspace_path,
        "remoteCwd": fixture.config.remote_workspace_path,
        "privateKey": fixture.config.private_key,
        "knownHosts": fixture.config.known_hosts,
        "strictHostKeyChecking": fixture.config.strict_host_key_checking,
    })
}

fn subscription_auth_json(account_id: &str, last_refresh_ms: i64) -> String {
    format!(
        "{{\"tokens\":{{\"account_id\":\"{account_id}\",\"access_token\":\"REDACTED\"}},\"last_refresh\":\"{last_refresh_ms}\"}}"
    )
}

fn silent_log() -> pc_adapter_codex_local::auth_copyback::LogFn {
    Arc::new(|_line: String| Box::pin(async {}))
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_remote_auth_copy_back_over_ssh_installs_newer_credential() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let remote_home = format!(
        "{}/.paperclip-runtime/codex/home",
        fixture.config.remote_workspace_path
    );
    let remote_auth = format!("{remote_home}/auth.json");

    fixture
        .write_remote_file(
            &remote_auth,
            &subscription_auth_json("acct-1", 2_000_000_000_000),
        )
        .await;

    let host_dir = fixture.root_dir.join("host-codex-home");
    std::fs::create_dir_all(&host_dir).expect("host dir");
    let host_auth_path = host_dir.join("auth.json");
    std::fs::write(
        &host_auth_path,
        subscription_auth_json("acct-1", 1_000_000_000_000).as_bytes(),
    )
    .expect("write host auth");

    let target = adapter_execution_target_from_remote_execution(&ssh_target_json(&fixture), None)
        .expect("parse SSH target");

    let sandbox_bytes = read_remote_codex_auth(&target, &remote_home)
        .await
        .expect("read_remote_codex_auth should succeed");
    assert!(
        sandbox_bytes
            .windows(b"acct-1".len())
            .any(|w| w == b"acct-1"),
        "sandbox bytes must contain expected account id; got {}",
        String::from_utf8_lossy(&sandbox_bytes)
    );

    let bytes_cell = Arc::new(Mutex::new(Some(sandbox_bytes)));
    let bytes_for_read = Arc::clone(&bytes_cell);
    let read_sandbox: pc_adapter_codex_local::auth_copyback::ReadSandboxAuthFn =
        Arc::new(move || {
            let cell = Arc::clone(&bytes_for_read);
            Box::pin(async move {
                cell.lock()
                    .expect("bytes lock")
                    .take()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "absent"))
            })
        });
    let log_lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let log_for_cb: pc_adapter_codex_local::auth_copyback::LogFn = {
        let sink = Arc::clone(&log_lines);
        Arc::new(move |line: String| {
            let sink = Arc::clone(&sink);
            Box::pin(async move {
                sink.lock().expect("log lock").push(line);
            })
        })
    };
    let outcome = copy_back_codex_auth(
        CopyBackCodexAuthInput {
            read_sandbox_auth: read_sandbox,
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: log_for_cb,
        },
        Box::new(CodexAuthMergeDecider),
    )
    .await
    .expect("copy_back_codex_auth");

    assert_eq!(outcome, CopyBackCodexAuthOutcome::Copied);

    let installed = std::fs::read(&host_auth_path).expect("read installed host auth");
    let installed_str = String::from_utf8_lossy(&installed);
    assert!(
        installed_str.contains("\"last_refresh\":\"2000000000000\""),
        "host auth.json should have new timestamp; got {installed_str}"
    );
    assert!(
        installed_str.contains("acct-1"),
        "host auth.json should still carry same account id"
    );

    let log_text = log_lines.lock().expect("log lock").join("\n");
    assert!(
        log_text.contains("sandbox credential is strictly newer"),
        "log should indicate copied; got {log_text}"
    );

    let leftover: Vec<_> = std::fs::read_dir(&host_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(
        leftover.is_empty(),
        "no .tmp files should linger after copy-back; got {leftover:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_remote_auth_copy_back_over_ssh_keeps_host_when_sandbox_older() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let remote_home = format!(
        "{}/.paperclip-runtime/codex/home",
        fixture.config.remote_workspace_path
    );
    let remote_auth = format!("{remote_home}/auth.json");

    fixture
        .write_remote_file(
            &remote_auth,
            &subscription_auth_json("acct-1", 1_000_000_000_000),
        )
        .await;

    let host_dir = fixture.root_dir.join("host-codex-home");
    std::fs::create_dir_all(&host_dir).expect("host dir");
    let host_auth_path = host_dir.join("auth.json");
    let host_bytes = subscription_auth_json("acct-1", 2_000_000_000_000);
    std::fs::write(&host_auth_path, host_bytes.as_bytes()).expect("write host auth");

    let target = adapter_execution_target_from_remote_execution(&ssh_target_json(&fixture), None)
        .expect("parse SSH target");
    let sandbox_bytes = read_remote_codex_auth(&target, &remote_home)
        .await
        .expect("read sandbox bytes");

    let bytes_cell = Arc::new(Mutex::new(Some(sandbox_bytes)));
    let bytes_for_read = Arc::clone(&bytes_cell);
    let read_sandbox: pc_adapter_codex_local::auth_copyback::ReadSandboxAuthFn =
        Arc::new(move || {
            let cell = Arc::clone(&bytes_for_read);
            Box::pin(async move {
                cell.lock()
                    .expect("bytes lock")
                    .take()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "absent"))
            })
        });
    let outcome = copy_back_codex_auth(
        CopyBackCodexAuthInput {
            read_sandbox_auth: read_sandbox,
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: silent_log(),
        },
        Box::new(CodexAuthMergeDecider),
    )
    .await
    .expect("copy_back_codex_auth");

    assert_eq!(outcome, CopyBackCodexAuthOutcome::KeptHost);
    let preserved = std::fs::read(&host_auth_path).expect("read preserved host auth");
    let preserved_str = String::from_utf8_lossy(&preserved);
    assert!(
        preserved_str.contains("\"last_refresh\":\"2000000000000\""),
        "host auth.json should still be the newer one; got {preserved_str}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_remote_auth_copy_back_over_ssh_keeps_host_when_sandbox_absent() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let remote_home = format!(
        "{}/.paperclip-runtime/codex/home",
        fixture.config.remote_workspace_path
    );
    let remote_auth = format!("{remote_home}/auth.json");
    fixture.remove_remote_file(&remote_auth).await;

    let host_dir = fixture.root_dir.join("host-codex-home");
    std::fs::create_dir_all(&host_dir).expect("host dir");
    let host_auth_path = host_dir.join("auth.json");
    let host_bytes = subscription_auth_json("acct-1", 2_000_000_000_000);
    std::fs::write(&host_auth_path, host_bytes.as_bytes()).expect("write host auth");

    let target = adapter_execution_target_from_remote_execution(&ssh_target_json(&fixture), None)
        .expect("parse SSH target");
    let sandbox_result = read_remote_codex_auth(&target, &remote_home).await;
    assert!(
        matches!(sandbox_result.as_ref(), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
        "absent remote auth.json should yield NotFound; got {sandbox_result:?}"
    );

    let read_sandbox: pc_adapter_codex_local::auth_copyback::ReadSandboxAuthFn = Arc::new(|| {
        Box::pin(async {
            Err::<Vec<u8>, std::io::Error>(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "absent",
            ))
        })
    });
    let outcome = copy_back_codex_auth(
        CopyBackCodexAuthInput {
            read_sandbox_auth: read_sandbox,
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: silent_log(),
        },
        Box::new(CodexAuthMergeDecider),
    )
    .await
    .expect("copy_back_codex_auth on absent sandbox");

    assert_eq!(outcome, CopyBackCodexAuthOutcome::KeptHost);
    let preserved = std::fs::read(&host_auth_path).expect("read preserved host auth");
    assert_eq!(
        preserved,
        host_bytes.as_bytes(),
        "host auth.json must remain unchanged when sandbox absent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn codex_remote_auth_copy_back_over_ssh_keeps_host_on_account_mismatch() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let remote_home = format!(
        "{}/.paperclip-runtime/codex/home",
        fixture.config.remote_workspace_path
    );
    let remote_auth = format!("{remote_home}/auth.json");
    fixture
        .write_remote_file(
            &remote_auth,
            &subscription_auth_json("acct-sandbox", 5_000_000_000_000),
        )
        .await;

    let host_dir = fixture.root_dir.join("host-codex-home");
    std::fs::create_dir_all(&host_dir).expect("host dir");
    let host_auth_path = host_dir.join("auth.json");
    std::fs::write(
        &host_auth_path,
        subscription_auth_json("acct-host", 1_000_000_000_000).as_bytes(),
    )
    .expect("write host auth");

    let target = adapter_execution_target_from_remote_execution(&ssh_target_json(&fixture), None)
        .expect("parse SSH target");
    let sandbox_bytes = read_remote_codex_auth(&target, &remote_home)
        .await
        .expect("read sandbox bytes");

    let bytes_cell = Arc::new(Mutex::new(Some(sandbox_bytes)));
    let bytes_for_read = Arc::clone(&bytes_cell);
    let read_sandbox: pc_adapter_codex_local::auth_copyback::ReadSandboxAuthFn =
        Arc::new(move || {
            let cell = Arc::clone(&bytes_for_read);
            Box::pin(async move {
                cell.lock()
                    .expect("bytes lock")
                    .take()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "absent"))
            })
        });
    let outcome = copy_back_codex_auth(
        CopyBackCodexAuthInput {
            read_sandbox_auth: read_sandbox,
            host_auth_path: host_auth_path.to_string_lossy().to_string(),
            log: silent_log(),
        },
        Box::new(CodexAuthMergeDecider),
    )
    .await
    .expect("copy_back_codex_auth");

    assert_eq!(outcome, CopyBackCodexAuthOutcome::KeptHost);
    let preserved = std::fs::read(&host_auth_path).expect("read preserved host auth");
    let preserved_str = String::from_utf8_lossy(&preserved);
    assert!(
        preserved_str.contains("acct-host"),
        "host auth.json should keep original account id; got {preserved_str}"
    );
    assert!(
        !preserved_str.contains("acct-sandbox"),
        "host auth.json must not adopt sandbox account id"
    );
}
