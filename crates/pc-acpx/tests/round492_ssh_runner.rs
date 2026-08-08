//! R492 — SSH runner 真实端到端验证（真实 sshd fixture）。
//!
//! 用真实 `sshd`（本机 /usr/sbin/sshd，随机 loopback 端口 + 生成密钥）
//! 验证 `pc-acpx::ssh` 的 SSH 执行器：
//! - `run_ssh_command`：echo / env 注入 / stdin 管道 / 超时 kill
//! - `SshCommandManagedRuntimeRunner`（BridgeCommandRunner 实现）：
//!   sh -c 脚本 + export 前缀、退出码传播、非 shell 命令
//! - bridge 全链路：用 SSH runner 启动
//!   `start_adapter_execution_target_paperclip_bridge` → 真实 node bridge
//!   server → 代理转发 echo 服务器 → teardown 后队列/pid/ready 清理
//!
//! sshd / ssh-keygen 缺失时跳过（`SshLabFixture::start` 返回 None）。

use pc_acpx::bridge_executor::{
    start_adapter_execution_target_paperclip_bridge, BridgeCommandRunner, StartAdapterBridgeInput,
};
use pc_acpx::execution_target::{adapter_execution_target_from_remote_execution, AdapterExecutionTarget};
use pc_acpx::ssh::{
    run_ssh_command, shell_quote, SshCommandManagedRuntimeRunner, SshCommandOptions,
    SshConnectionConfig, SshRemoteExecutionSpec,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// 随机 loopback 端口（对齐 Node `allocateLoopbackPort`：
/// 先 bind :0 拿空闲端口再释放，测试内小竞态可接受）。
fn allocate_loopback_port() -> Option<u16> {
    use std::io::Read;
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    // 触发一次 socket 初始化（保持与 Node 行为一致的无害探测）。
    let _ = std::io::Cursor::new(Vec::new()).read(&mut []).ok();
    Some(port)
}

/// 真实 sshd fixture（对齐 Node `startSshEnvLabFixture`）：
/// 生成 client/host ed25519 密钥、known_hosts、authorized_keys，
/// 随机端口启动 `sshd -D`，就绪后通过真实 ssh 往返验证。
struct SshLabFixture {
    config: SshConnectionConfig,
    child: Option<tokio::process::Child>,
    root_dir: PathBuf,
    pid: u32,
}

impl SshLabFixture {
    /// 启动 fixture；sshd/ssh-keygen 缺失时返回 None（测试跳过）。
    async fn start() -> Option<Self> {
        if !command_available("ssh") || !command_available("sshd") || !command_available("ssh-keygen") {
            eprintln!("SKIP: ssh/sshd/ssh-keygen unavailable");
            return None;
        }
        let port = allocate_loopback_port()?;
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-ssh-lab-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root_dir.join("workspace")).ok()?;
        let username = std::env::var("USER").unwrap_or_else(|_| "root".to_string());
        let client_key = root_dir.join("client_key");
        let host_key = root_dir.join("host_key");
        let authorized_keys = root_dir.join("authorized_keys");
        let known_hosts_path = root_dir.join("known_hosts");
        let sshd_config_path = root_dir.join("sshd_config");
        let sshd_log_path = root_dir.join("sshd.log");
        let sshd_pid_path = root_dir.join("sshd.pid");

        if !run_sync(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-f", client_key.to_str().unwrap()],
        ) || !run_sync(
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-f", host_key.to_str().unwrap()],
        ) {
            return None;
        }
        let _ = std::fs::copy(client_key.with_extension("pub"), &authorized_keys);
        let host_public_key = run_sync_output(
            "ssh-keygen",
            &["-y", "-f", host_key.to_str().unwrap()],
        )
        .trim()
        .to_string();
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
             UseDNS no\n\
             Subsystem sftp internal-sftp\n",
            host_key.display(),
            sshd_pid_path.display(),
            authorized_keys.display(),
        );
        let _ = std::fs::write(&sshd_config_path, &config_text);
        let mut child = tokio::process::Command::new("sshd")
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
        let config = SshConnectionConfig {
            host: "127.0.0.1".to_string(),
            port,
            username,
            remote_workspace_path: root_dir.join("workspace").to_string_lossy().to_string(),
            private_key: Some(
                std::fs::read_to_string(&client_key).unwrap_or_default(),
            ),
            known_hosts: Some(known_hosts_entry),
            strict_host_key_checking: true,
        };
        let fixture = Self {
            config,
            child: Some(child),
            root_dir,
            pid,
        };
        // 就绪轮询：真实 ssh 往返 `echo ready`，最长 10s（对齐 Node
        // `waitForCondition` 10s / 250ms）。
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let result = fixture
                .run("echo ready")
                .await;
            if matches!(result, Ok(ok) if ok.stdout.trim() == "ready") {
                return Some(fixture);
            }
            if std::time::Instant::now() > deadline {
                eprintln!("sshd fixture failed to become ready");
                return None;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// 通过 fixture 跑一条远程命令。
    async fn run(&self, remote_command: &str) -> Result<pc_acpx::ssh::SshCommandResult, pc_acpx::ssh::SshCommandError> {
        run_ssh_command(
            &self.config,
            remote_command,
            &SshCommandOptions {
                timeout_ms: 15_000,
                ..SshCommandOptions::default()
            },
        )
        .await
    }

    /// 组装 SSH runner（default cwd = fixture workspace）。
    fn runner(&self) -> SshCommandManagedRuntimeRunner {
        SshCommandManagedRuntimeRunner::new(
            self.spec(),
            None,
            None,
        )
    }

    fn spec(&self) -> SshRemoteExecutionSpec {
        SshRemoteExecutionSpec::from_parts(
            self.config.clone(),
            self.config.remote_workspace_path.clone(),
        )
    }

    /// fixture 的 execution target（transport ssh）。
    fn target(&self) -> AdapterExecutionTarget {
        let value = serde_json::json!({
            "transport": "ssh",
            "host": self.config.host,
            "port": self.config.port,
            "username": self.config.username,
            "remoteWorkspacePath": self.config.remote_workspace_path,
            "remoteCwd": self.config.remote_workspace_path,
            "privateKey": self.config.private_key,
            "knownHosts": self.config.known_hosts,
            "strictHostKeyChecking": self.config.strict_host_key_checking,
        });
        adapter_execution_target_from_remote_execution(&value, None)
            .expect("valid ssh execution target")
    }
}

impl Drop for SshLabFixture {
    fn drop(&mut self) {
        if self.pid != 0 {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &self.pid.to_string()])
                .status();
            // 等待 sshd 退出（最多 2s），避免 PID 残留。
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
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

fn run_sync(command: &str, args: &[&str]) -> bool {
    std::process::Command::new(command)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run_sync_output(command: &str, args: &[&str]) -> String {
    let output = std::process::Command::new(command)
        .args(args)
        .output()
        .unwrap_or_else(|_| std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// 1. run_ssh_command 真实往返
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ssh_run_command_echo_pwd_and_env() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    // 1. echo + 注入 env（env 走 `exec env K=V sh -c`）。
    let mut env = BTreeMap::new();
    env.insert("R492_MARKER".to_string(), "hello-ssh".to_string());
    let result = run_ssh_command(
        &fixture.config,
        "printf '%s/%s' \"$R492_MARKER\" \"$(pwd)\"",
        &SshCommandOptions {
            env,
            ..SshCommandOptions::default()
        },
    )
    .await
    .expect("remote command succeeds");
    assert_eq!(
        result.stdout,
        format!("hello-ssh/{}", fixture.config.remote_workspace_path)
    );
    assert!(result.stderr.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_run_command_stdin_round_trip() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let payload = "stdin-payload-492\nline2";
    let result = run_ssh_command(
        &fixture.config,
        "cat",
        &SshCommandOptions {
            stdin: Some(payload.to_string()),
            ..SshCommandOptions::default()
        },
    )
    .await
    .expect("remote cat succeeds");
    assert_eq!(result.stdout, payload);
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_run_command_timeout_kills_remote() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let started = std::time::Instant::now();
    let error = run_ssh_command(
        &fixture.config,
        "sleep 30",
        &SshCommandOptions {
            timeout_ms: 1_500,
            ..SshCommandOptions::default()
        },
    )
    .await
    .expect_err("sleep must time out");
    assert!(error.timed_out, "error: {error:?}");
    assert!(error.exit_code != Some(0));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "timeout should not wait for the full sleep"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_run_command_max_buffer_overflow() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let error = run_ssh_command(
        &fixture.config,
        "yes x | head -c 65536",
        &SshCommandOptions {
            max_buffer: 4 * 1024,
            ..SshCommandOptions::default()
        },
    )
    .await
    .expect_err("output must exceed maxBuffer");
    assert!(
        error.message.contains("exceeded maxBuffer"),
        "message: {}",
        error.message
    );
}

// ---------------------------------------------------------------------------
// 2. SshCommandManagedRuntimeRunner（BridgeCommandRunner）语义
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ssh_runner_executes_sh_c_with_export_prefix() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(fixture.runner());
    let mut env = BTreeMap::new();
    env.insert("R492_INJECTED".to_string(), "exported-value".to_string());
    let result = runner
        .execute(&pc_acpx::bridge_executor::RunnerExecuteInput {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "printf '%s' \"$R492_INJECTED\"".to_string()],
            cwd: String::new(),
            env,
            stdin: None,
            timeout_ms: 15_000,
        })
        .await
        .expect("runner execute resolves");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert_eq!(result.stdout, "exported-value");
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_runner_propagates_exit_code_and_cwd() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(fixture.runner());
    // 1. 非 shell 命令（`exec <cmd>` 路径）：pwd 输出 remote_cwd。
    let result = runner
        .execute(&pc_acpx::bridge_executor::RunnerExecuteInput {
            command: "pwd".to_string(),
            args: vec![],
            cwd: String::new(),
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 15_000,
        })
        .await
        .expect("pwd resolves");
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.trim(), fixture.config.remote_workspace_path);
    // 2. 失败命令 → exit_code 传播。
    let result = runner
        .execute(&pc_acpx::bridge_executor::RunnerExecuteInput {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 7".to_string()],
            cwd: String::new(),
            env: BTreeMap::new(),
            stdin: None,
            timeout_ms: 15_000,
        })
        .await
        .expect("exit 7 resolves (Err is not surfaced)");
    assert_eq!(result.exit_code, Some(7));
    assert!(!result.timed_out);
    // 3. stdin 管道（对齐 bridge base64 上传路径）。
    let result = runner
        .execute(&pc_acpx::bridge_executor::RunnerExecuteInput {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "cat".to_string()],
            cwd: String::new(),
            env: BTreeMap::new(),
            stdin: Some("base64-payload".to_string()),
            timeout_ms: 15_000,
        })
        .await
        .expect("cat resolves");
    assert_eq!(result.stdout, "base64-payload");
}

// ---------------------------------------------------------------------------
// 3. bridge 全链路（SSH runner + 真实 sshd + 真实 node bridge server）
// ---------------------------------------------------------------------------

/// 极简 HTTP/1.1 echo 服务器（与 round491 相同的转发目标）。
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
                let body_start = head_text.find("\r\n\r\n").map(|i| i + 4).unwrap_or(head.len());
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
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"round492\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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
        .request(reqwest::Method::from_bytes(method.as_bytes()).expect("method"), url)
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
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let body = response.text().await.unwrap_or_default();
    (status, headers, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_bridge_full_round_trip_with_real_sshd() {
    let Some(fixture) = SshLabFixture::start().await else {
        return;
    };
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let (echo_url, _echo_task) = spawn_echo_server().await;
    let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let logs_for_hook = Arc::clone(&logs);
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(fixture.runner());
    let target = fixture.target();
    let bridge = start_adapter_execution_target_paperclip_bridge(&StartAdapterBridgeInput {
        run_id: "run-492",
        target: Some(&target),
        runtime_root_dir: None,
        adapter_key: "codex",
        timeout_sec: Some(60.0),
        host_api_token: Some("host-token"),
        host_api_url: Some(&echo_url),
        runner: runner.clone(),
        on_log: Some(Arc::new(move |line: &str| {
            logs_for_hook
                .lock()
                .expect("log lock")
                .push(line.to_string());
        })),
    })
    .await
    .expect("bridge starts")
    .expect("remote target ⇒ bridge present");
    let log_lines = logs.lock().expect("log lock").clone();
    assert!(log_lines.iter().any(|line| {
        line.contains("[paperclip] Starting sandbox callback bridge for codex in")
            && line.contains(".paperclip-runtime/codex/paperclip-bridge.")
    }));

    // 1. env 4 键 + bridge server 可达（经 SSH 启动的真实 node）。
    assert_eq!(bridge.env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
    let queue_dir = bridge.env["PAPERCLIP_BRIDGE_QUEUE_DIR"].clone();
    let base_url = bridge.env["PAPERCLIP_API_URL"].clone();
    let bridge_token = bridge.env["PAPERCLIP_API_KEY"].clone();
    assert_eq!(bridge.server.base_url, base_url);
    assert!(queue_dir.ends_with("/.paperclip-runtime/codex/paperclip-bridge/queue"));
    // entrypoint 已通过 SSH 同步到远端（同一台机器，直接验证文件）。
    let entrypoint_path = format!(
        "{}/.paperclip-runtime/codex/paperclip-bridge/server/{}",
        fixture.config.remote_workspace_path,
        pc_acpx::sandbox_callback_bridge::SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT
    );
    assert!(
        Path::new(&entrypoint_path).exists(),
        "entrypoint synced via ssh: {entrypoint_path}"
    );

    // 2. 代理 POST：200 + body 回显 + etag 透传（worker 经 SSH 轮询队列，
    // server 经 SSH 启动的 node 转发到 echo server）。
    let (status, headers, body) = http_request(
        &format!("{base_url}/api/issues/issue-1/comments?q=1"),
        "POST",
        Some(&bridge_token),
        Some("application/json"),
        Some(r#"{"hello":"ssh"}"#),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(
        headers.get("etag").map(String::as_str),
        Some("\"round492\"")
    );
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json body");
    assert_eq!(parsed["ok"], serde_json::Value::Bool(true));
    assert_eq!(parsed["method"], serde_json::Value::String("POST".to_string()));
    assert_eq!(
        parsed["path"],
        serde_json::Value::String("/api/issues/issue-1/comments".to_string())
    );
    let echoed_body: serde_json::Value =
        serde_json::from_str(parsed["body"].as_str().expect("echo body string"))
            .expect("echo body json");
    assert_eq!(
        echoed_body["hello"],
        serde_json::Value::String("ssh".to_string())
    );

    // 3. 错误 token → 401。
    let (status, _, body) = http_request(
        &format!("{base_url}/api/issues/issue-1/comments"),
        "POST",
        Some("wrong-token"),
        Some("application/json"),
        Some("{}"),
    )
    .await;
    assert_eq!(status, 401);
    assert!(body.contains("Invalid bridge token."));

    // 4. GET 转发（无 body）。
    let (status, _, body) = http_request(
        &format!("{base_url}/api/agents/me"),
        "GET",
        Some(&bridge_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["method"], serde_json::Value::String("GET".to_string()));
    assert_eq!(parsed["path"], serde_json::Value::String("/api/agents/me".to_string()));

    // 5. teardown：server pid/ready 清理、队列无残留（经 SSH 验证）。
    let pid_file = format!("{queue_dir}/server.pid");
    assert!(Path::new(&pid_file).exists());
    bridge.stop().await;
    assert!(!Path::new(&pid_file).exists());
    assert!(!Path::new(&format!("{queue_dir}/ready.json")).exists());
    let queue_clean = fixture
        .run(&format!("find {} -type f | wc -l", shell_quote(&queue_dir)))
        .await
        .expect("queue scan via ssh");
    assert_eq!(queue_clean.stdout.trim(), "0", "no queue files left");
}
