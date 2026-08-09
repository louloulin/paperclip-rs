//! R496 — Claude adapter 真实 sshd 远端 CLI 执行接入验证。
//!
//! 用真实 sshd fixture 验证 [`ClaudeLocalAdapter::execute`]（R496 接入
//! `pc_acpx::execution_target_process::execute_command_for_target` 的主执行
//! 路径）的 SSH 分支：把 `execution_target` 设为 ssh json，跑 `/bin/echo`
//! 占位命令，验证远端 stdout 正确回流到 `AdapterExecutionResult`（通过
//! `parse_claude_stream_json` 解析后 session_id / model / error_message
//! 字段）。
//!
//! 注：此测试不启动 node bridge（已由 round492 覆盖）；只验证 helper 经
//! SSH 分支把 stdout 拉回 Claude 主路径。
//!
//! sshd 缺失时跳过真实部分。

use pc_acpx::ssh::SshConnectionConfig;
use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};
use pc_adapter_claude_local::ClaudeLocalAdapter;
use std::collections::BTreeMap;
use std::path::PathBuf;
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
            "paperclip-r496-claude-ssh-{}",
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

fn make_context(fixture: &SshLabFixture) -> AdapterExecutionContext {
    // `command` 指向 `/bin/echo`，adapter_config 让 build_claude_exec_args 不展开
    // 任何 CLI 子命令之外的副作用（model / chrome / effort 等留空）。
    let mut ctx = AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "");
    ctx.adapter_config = serde_json::json!({
        "command": "/bin/echo",
        // 极简 args：echo "r496-claude-ssh-marker"
    });
    ctx.execution_target = Some(ssh_target_json(fixture));
    ctx
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_execute_dispatches_to_ssh_and_parses_remote_stdout() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let context = make_context(&fixture);
    let (sink, _rx) = AdapterEventSink::channel(16);

    let result = ClaudeLocalAdapter::new()
        .execute(context, sink)
        .await
        .expect("Claude execute must succeed via SSH");

    // 退出码 0（远端 echo 成功）
    assert_eq!(result.exit_code, Some(0));
    // provider 由 execute() 显式填入
    assert_eq!(result.provider.as_deref(), Some("claude_local"));
    // billing_type 必须被填充
    assert!(
        result.billing_type.is_some(),
        "billing_type must be populated; got {:?}",
        result.billing_type
    );
    // result_json 已构造（merged 含 sawProtocolEvent 等）
    let merged = result
        .result_json
        .clone()
        .expect("result_json must be Some after execute");
    assert_eq!(
        merged.get("sawProtocolEvent").and_then(|v| v.as_bool()),
        Some(true)
    );
}
