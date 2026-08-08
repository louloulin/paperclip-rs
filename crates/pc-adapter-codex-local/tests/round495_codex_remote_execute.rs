//! R495 — codex adapter 真实 sshd 远端 CLI 执行接入验证。
//!
//! 用真实 sshd fixture 验证 `execute_codex_with_monitor`（R495 接入
//! `pc_acpx::execution_target_process::execute_command_for_target`）的 SSH
//! 分支：把 `execution_target` 设为 ssh json，跑 `/bin/echo` 占位命令，
//! 验证远端 stdout 正确回流到 `StreamingProcessExecution.stdout` 与
//! `AdapterEvent::stdout` 事件 sink。
//!
//! 注：此测试不启动 node bridge（已由 round492 覆盖）；只验证 helper 经 SSH
//! 分支把 stdout 拉回 codex 适配器的执行结果路径。
//!
//! sshd 缺失时跳过真实部分。

use pc_acpx::ssh::SshConnectionConfig;
use pc_adapter_api::{
    AdapterEventSink, AdapterExecutionContext,
};
use pc_adapter_codex_local::{execute_codex_with_monitor, CodexExecArgs};
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
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-r495-codex-ssh-{}",
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
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        if !gen(&["-q", "-t", "ed25519", "-N", "", "-f", client_key.to_str().unwrap()])
            || !gen(&["-q", "-t", "ed25519", "-N", "", "-f", host_key.to_str().unwrap()])
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
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let pid = child.id().unwrap_or(0);
        // 等 sshd 就绪（轮询 ssh connect）。
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
            let probe = pc_acpx::ssh::run_ssh_command(
                &config,
                "echo probe",
                &pc_acpx::ssh::SshCommandOptions {
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

/// 端到端：codex adapter 完整路径走 SSH 分支，远端 stdout 回流。
#[tokio::test(flavor = "multi_thread")]
async fn codex_execute_codex_with_monitor_dispatches_to_ssh_and_returns_remote_stdout() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let mut context = AdapterExecutionContext::new(
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4(),
        "",
    );
    context.execution_target = Some(ssh_target_json(&fixture));

    let marker = "r495-codex-ssh-marker";
    let built = CodexExecArgs {
        args: vec![
            "-c".to_owned(),
            format!("printf '{marker}\\n'"),
        ],
        model: "gpt-5.6-sol".to_owned(),
        fast_mode_requested: false,
        fast_mode_applied: false,
        fast_mode_ignored_reason: None,
    };

    let (sink, mut rx) = AdapterEventSink::channel(16);
    let captured_stdout: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_log = Arc::clone(&captured_stdout);
    // Drain events in background; capture stdout chunks.
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let pc_adapter_api::AdapterEvent::Output { stream, text, .. } = event {
                if matches!(stream, pc_adapter_api::OutputStream::Stdout) {
                    captured_for_log.lock().expect("lock").push(text);
                }
            }
        }
    });

    let (execution, monitor_outcome) = execute_codex_with_monitor(
        "/bin/sh",
        &built,
        &context,
        sink,
        None,
    )
    .await
    .expect("execute should succeed via SSH");

    // monitor 未启用，应为 None
    assert!(monitor_outcome.is_none(), "monitor must not fire when disabled");
    // 退出码 0
    assert_eq!(execution.result.exit_code, Some(0));
    // stdout 含远端 marker
    assert!(
        execution.stdout.contains(marker),
        "execution.stdout must contain marker; got {:?}",
        execution.stdout
    );
    // 事件 sink 也应收到 marker
    let observed = captured_stdout.lock().expect("lock").join("");
    assert!(
        observed.contains(marker),
        "event sink must stream remote stdout; got {:?}",
        observed
    );
}
