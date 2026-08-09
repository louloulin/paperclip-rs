//! R492 — claude adapter 真实 bridge 启动接入验证。
//!
//! 用真实 sshd fixture 验证 `start_claude_execution_bridge`（R492 接入
//! claude execute 的 bridge 启动路径）：
//! - SSH target → 真实启动完整 bridge（SSH runner + node server + worker），
//!   adapter key 为 `claude`，teardown 清理
//! - sandbox target → `Ok(None)`（无 provider runner，保持 env-only）
//! - 本地 / 缺 target → `Ok(None)`
//!
//! sshd / node 缺失时跳过真实部分。

use pc_acpx::ssh::{run_ssh_command, shell_quote, SshCommandOptions, SshConnectionConfig};
use pc_adapter_claude_local::claude_remote_workspace::start_claude_execution_bridge;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
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

/// 真实 sshd fixture（与 pc-adapter-codex-local round492 相同模式）。
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
            std::env::temp_dir().join(format!("paperclip-claude-ssh-lab-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root_dir.join("workspace")).ok()?;
        let username = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
        let client_key = root_dir.join("client_key");
        let host_key = root_dir.join("host_key");
        let authorized_keys = root_dir.join("authorized_keys");
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
             UseDNS no\n\
             Subsystem sftp internal-sftp\n",
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
            .spawn()
            .ok()?;
        let pid = child.id().unwrap_or(0);
        drop(child);
        let config = SshConnectionConfig {
            host: "127.0.0.1".to_string(),
            port,
            username,
            remote_workspace_path: root_dir.join("workspace").to_string_lossy().to_string(),
            private_key: Some(std::fs::read_to_string(&client_key).unwrap_or_default()),
            known_hosts: Some(known_hosts_entry),
            strict_host_key_checking: true,
        };
        let fixture = Self {
            config,
            root_dir,
            pid,
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let ready =
                run_ssh_command(&fixture.config, "echo ready", &SshCommandOptions::default())
                    .await
                    .map(|ok| ok.stdout.trim() == "ready")
                    .unwrap_or(false);
            if ready {
                return Some(fixture);
            }
            if std::time::Instant::now() > deadline {
                eprintln!("sshd fixture failed to become ready");
                return None;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

impl Drop for SshLabFixture {
    fn drop(&mut self) {
        if self.pid != 0 {
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
        }
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

fn base_env() -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PAPERCLIP_RUN_ID".to_string(), "run-492".to_string());
    env.insert("PAPERCLIP_API_KEY".to_string(), "host-token".to_string());
    env.insert(
        "PAPERCLIP_API_URL".to_string(),
        "http://host:3100".to_string(),
    );
    env
}

fn ssh_target_value(fixture: &SshLabFixture) -> serde_json::Value {
    serde_json::json!({
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

#[tokio::test(flavor = "multi_thread")]
async fn ssh_target_starts_real_bridge_with_claude_adapter_key() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    if !command_available("node") {
        eprintln!("SKIP: node not available");
        return;
    }
    let (echo_url, _echo_task) = spawn_echo_server().await;
    let env = base_env();
    let target = ssh_target_value(&fixture);
    let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let logs_for_hook = Arc::clone(&logs);
    let bridge = start_claude_execution_bridge(
        "run-492",
        &env,
        Some(&target),
        Some(60.0),
        Some(Arc::new(move |line: &str| {
            logs_for_hook
                .lock()
                .expect("log lock")
                .push(line.to_string());
        })),
    )
    .await
    .expect("bridge starts")
    .expect("ssh target ⇒ bridge present");

    // 启动日志用 claude adapter key。
    let log_lines = logs.lock().expect("log lock").clone();
    assert!(log_lines.iter().any(|line| {
        line.contains("[paperclip] Starting sandbox callback bridge for claude in")
            && line.contains(".paperclip-runtime/claude/paperclip-bridge.")
    }));

    // env 4 键 + 转发可达。
    assert_eq!(bridge.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
    let base_url = bridge.env["PAPERCLIP_API_URL"].clone();
    let bridge_token = bridge.env["PAPERCLIP_API_KEY"].clone();
    let queue_dir = bridge.env["PAPERCLIP_BRIDGE_QUEUE_DIR"].clone();
    let (status, _, body) = http_request(
        &format!("{base_url}/api/agents/me"),
        "GET",
        Some(&bridge_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(
        parsed["path"],
        serde_json::Value::String("/api/agents/me".to_string())
    );

    // teardown：经 SSH 验证队列无残留。
    bridge.stop().await;
    let queue_clean = run_ssh_command(
        &fixture.config,
        &format!("find {} -type f | wc -l", shell_quote(&queue_dir)),
        &SshCommandOptions::default(),
    )
    .await
    .expect("queue scan via ssh");
    assert_eq!(queue_clean.stdout.trim(), "0", "no queue files left");
}

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_local_and_missing_target_return_none() {
    let env = base_env();
    let sandbox = serde_json::json!({
        "transport": "sandbox",
        "providerKey": "e2b",
        "remoteCwd": "/sandbox/workspace",
        "streamRunLogs": true,
    });
    let bridge = start_claude_execution_bridge("run-492", &env, Some(&sandbox), None, None)
        .await
        .expect("sandbox no error");
    assert!(bridge.is_none(), "sandbox keeps env-only merge");

    let local = serde_json::json!({ "kind": "local" });
    let bridge = start_claude_execution_bridge("run-492", &env, Some(&local), None, None)
        .await
        .expect("local no error");
    assert!(bridge.is_none());

    let bridge = start_claude_execution_bridge("run-492", &env, None, None, None)
        .await
        .expect("no target no error");
    assert!(bridge.is_none());
}

/// 极简 HTTP/1.1 echo 服务器。
async fn spawn_echo_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind echo server");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut head = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    let Ok(n) = socket.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    head.extend_from_slice(&tmp[..n]);
                    if head.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if head.len() > 64 * 1024 {
                        return;
                    }
                }
                let head_text = String::from_utf8_lossy(&head);
                let mut lines = head_text.split("\r\n");
                let request_line = lines.next().unwrap_or("");
                let method = request_line.split(' ').next().unwrap_or("GET").to_string();
                let path = request_line
                    .split(' ')
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let mut content_length = 0usize;
                for line in lines {
                    if let Some((key, value)) = line.split_once(':') {
                        if key.trim().eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let body_start = head_text
                    .find("\r\n\r\n")
                    .map(|i| i + 4)
                    .unwrap_or(head.len());
                let mut body = head[body_start.min(head.len())..].to_vec();
                while body.len() < content_length {
                    let Ok(n) = socket.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    body.extend_from_slice(&tmp[..n]);
                }
                let body_text = String::from_utf8_lossy(&body[..content_length.min(body.len())]);
                let response_body = format!(
                    "{{\"ok\":true,\"method\":\"{method}\",\"path\":\"{path}\",\"body\":{}}}",
                    serde_json::to_string(&body_text).expect("json body")
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://{addr}"), handle)
}

async fn http_request(
    url: &str,
    method: &str,
    bearer: Option<&str>,
    content_type: Option<&str>,
    body: Option<&str>,
) -> (u16, BTreeMap<String, String>, String) {
    let mut builder = reqwest::Client::new()
        .request(
            reqwest::Method::from_bytes(method.as_bytes()).expect("method"),
            url,
        )
        .timeout(Duration::from_secs(15));
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if let Some(ct) = content_type {
        builder = builder.header("content-type", ct);
    }
    if let Some(body) = body {
        builder = builder.body(body.to_string());
    }
    let response = builder.send().await.expect("request succeeds");
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = response.text().await.unwrap_or_default();
    (status, headers, body)
}
