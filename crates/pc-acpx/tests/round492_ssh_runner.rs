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

mod common;
use crate::common::{node_available, SshLabFixture};

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


#[tokio::test(flavor = "multi_thread")]
async fn ssh_run_command_echo_pwd_and_env() {
    let Some(fixture) = SshLabFixture::start("r492").await else {
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
    let Some(fixture) = SshLabFixture::start("r492").await else {
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
    let Some(fixture) = SshLabFixture::start("r492").await else {
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
    let Some(fixture) = SshLabFixture::start("r492").await else {
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
    let Some(fixture) = SshLabFixture::start("r492").await else {
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
    let Some(fixture) = SshLabFixture::start("r492").await else {
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
    let Some(fixture) = SshLabFixture::start("r492").await else {
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
        .run(
            &format!("find {} -type f | wc -l", shell_quote(&queue_dir)),
            SshCommandOptions::default(),
        )
        .await
        .expect("queue scan via ssh");
    assert_eq!(queue_clean.stdout.trim(), "0", "no queue files left");
}
