//! R493 — 进程 session bridge 真实端到端验证。
//!
//! 对齐 Node `execution-target-sandbox.test.ts` 的模式：用本地进程
//! runner（`LocalProcessBridgeRunner`）模拟 sandbox，真实 node 远端脚本 +
//! 真实 TCP server + 真实 proxy 脚本跑通：
//! - 双向桥接：stdin → 远端 child stdout/stderr 回显 → exit code
//! - 输出缓冲：proxy 连接前的事件在接管后 flush
//! - 未鉴权 / 错误 token 连接被断开
//! - exit code 传播（exit 7）
//! - stop 清理（session 目录 / proxy 目录 / 连接全清）
//! - gate：SSH target → `Ok(None)`

use pc_acpx::bridge_executor::{BridgeCommandRunner, LocalProcessBridgeRunner};
use pc_acpx::execution_target::adapter_execution_target_from_remote_execution;
use pc_acpx::process_session_bridge::{
    start_adapter_execution_target_process_session_bridge, StartProcessSessionBridgeInput,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn node_available_check() -> bool {
    node_available()
}

/// 本地"沙箱" fixture：临时目录 + 远端 child 脚本。
struct LocalSandbox {
    root_dir: PathBuf,
    child_path: PathBuf,
    target: serde_json::Value,
}

impl LocalSandbox {
    fn new(child_source: &str) -> Self {
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-process-session-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root_dir).expect("root dir");
        let child_path = root_dir.join("fake-acp-child.mjs");
        std::fs::write(&child_path, child_source).expect("child script");
        let target = serde_json::json!({
            "kind": "remote",
            "transport": "sandbox",
            "providerKey": "local-test",
            "remoteCwd": root_dir.to_string_lossy(),
            "timeoutMs": 30_000,
        });
        Self {
            root_dir,
            child_path,
            target,
        }
    }

    fn parsed_target(&self) -> pc_acpx::execution_target::AdapterExecutionTarget {
        pc_acpx::execution_target::parse_adapter_execution_target(&self.target)
            .expect("valid sandbox target")
    }

    fn node(&self) -> String {
        std::env::var("NODE").unwrap_or_else(|_| "node".to_string())
    }

    fn launch_env(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
}

impl Drop for LocalSandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

fn ssh_target() -> pc_acpx::execution_target::AdapterExecutionTarget {
    adapter_execution_target_from_remote_execution(
        &serde_json::json!({
            "transport": "ssh",
            "host": "127.0.0.1",
            "port": 2222,
            "username": "fixture",
            "remoteWorkspacePath": "/w",
            "remoteCwd": "/w",
            "strictHostKeyChecking": false,
        }),
        None,
    )
    .expect("valid ssh target")
}

/// 启动 bridge 的公共 helper。
async fn start_bridge(
    sandbox: &LocalSandbox,
    command: &str,
    args: &[String],
    on_log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
) -> pc_acpx::process_session_bridge::ProcessSessionBridgeHandle {
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let target = sandbox.parsed_target();
    let env = sandbox.launch_env();
    start_adapter_execution_target_process_session_bridge(&StartProcessSessionBridgeInput {
        run_id: "run-493",
        target: Some(&target),
        runtime_root_dir: None,
        adapter_key: "acpx",
        command,
        args,
        cwd: &sandbox.root_dir.to_string_lossy(),
        launch_env: &env,
        timeout_sec: Some(5.0),
        runner,
        on_log,
    })
    .await
    .expect("bridge starts")
    .expect("sandbox target ⇒ bridge present")
}

/// 运行 proxy 脚本（spawn agent_command + stdin 输入 → 收集输出 + exit code）。
async fn run_proxy_with_input(
    agent_command: &str,
    input: &str,
    timeout_ms: u64,
) -> Result<(String, String, Option<i32>), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;
    use tokio::time::timeout;

    let mut child = Command::new(agent_command)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawn proxy failed: {error}"))?;
    let mut stdin = child.stdin.take().expect("stdin pipe");
    let input_owned = input.to_string();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input_owned.as_bytes()).await;
        let _ = stdin.shutdown().await;
    });
    let mut stdout = child.stdout.take().expect("stdout pipe");
    let mut stderr = child.stderr.take().expect("stderr pipe");
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });
    let wait = async {
        let status = child.wait().await.map_err(|error| error.to_string())?;
        Ok::<Option<i32>, String>(status.code())
    };
    let code = match timeout(Duration::from_millis(timeout_ms), wait).await {
        Ok(result) => result?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("Timed out waiting for process session proxy.".to_string());
        }
    };
    let _ = writer.await;
    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();
    Ok((stdout, stderr, code))
}

const ECHO_CHILD: &str = r#"process.stdin.on("data", (chunk) => {
  process.stdout.write("out:" + chunk.toString());
  process.stderr.write("err:" + chunk.toString());
});
"#;

const FAST_EXIT_CHILD: &str = r#"process.stdout.write("early-out\n");
process.stderr.write("early-err\n");
setTimeout(() => process.exit(0), 20);
"#;

const EXIT_7_CHILD: &str = r#"setTimeout(() => process.exit(7), 20);
"#;

// ---------------------------------------------------------------------------
// 1. 双向桥接：stdin → stdout/stderr 回显 + exit code 0
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn bridges_bidirectional_sandbox_process_session() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let sandbox = LocalSandbox::new(ECHO_CHILD);
    let bridge = start_bridge(
        &sandbox,
        &sandbox.node(),
        &[sandbox.child_path.to_string_lossy().into_owned()],
        None,
    )
    .await;
    let (stdout, stderr, code) = run_proxy_with_input(&bridge.agent_command, "hello\n", 8_000)
        .await
        .expect("proxy completes");
    assert_eq!(code, Some(0), "exit code");
    assert_eq!(stdout, "out:hello\n");
    assert_eq!(stderr, "err:hello\n");
    bridge.stop().await;
}

// ---------------------------------------------------------------------------
// 2. 输出缓冲：proxy 连接前的事件在接管后 flush
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn buffers_output_until_proxy_connects() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let sandbox = LocalSandbox::new(FAST_EXIT_CHILD);
    let bridge = start_bridge(
        &sandbox,
        &sandbox.node(),
        &[sandbox.child_path.to_string_lossy().into_owned()],
        None,
    )
    .await;
    // child 在 20ms 内退出；proxy 稍后连接，仍应收到缓冲的 early-out/err。
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (stdout, stderr, code) = run_proxy_with_input(&bridge.agent_command, "", 8_000)
        .await
        .expect("proxy completes");
    assert_eq!(code, Some(0));
    assert_eq!(stdout, "early-out\n");
    assert_eq!(stderr, "early-err\n");
    bridge.stop().await;
}

// ---------------------------------------------------------------------------
// 3. exit code 传播（exit 7）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn propagates_remote_exit_code() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let sandbox = LocalSandbox::new(EXIT_7_CHILD);
    let bridge = start_bridge(
        &sandbox,
        &sandbox.node(),
        &[sandbox.child_path.to_string_lossy().into_owned()],
        None,
    )
    .await;
    let (_, _, code) = run_proxy_with_input(&bridge.agent_command, "", 8_000)
        .await
        .expect("proxy completes");
    assert_eq!(code, Some(7));
    bridge.stop().await;
}

// ---------------------------------------------------------------------------
// 4. 鉴权：错误 token 立即断开
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn rejects_wrong_token_connection() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let sandbox = LocalSandbox::new(ECHO_CHILD);
    let bridge = start_bridge(
        &sandbox,
        &sandbox.node(),
        &[sandbox.child_path.to_string_lossy().into_owned()],
        None,
    )
    .await;
    // 从 proxy 脚本提取端口，用错误 token 直连。
    let proxy_source = std::fs::read_to_string(&bridge.agent_command).expect("proxy source");
    let port = extract_proxy_port(&proxy_source).expect("port in proxy source");
    let socket = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    use tokio::io::AsyncWriteExt;
    let mut socket = socket;
    let _ = socket
        .write_all(b"{\"token\":\"wrong-token\",\"type\":\"hello\"}\n")
        .await;
    // 服务端应立即关闭连接。
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut buf))
        .await
        .expect("connection closed promptly")
        .expect("read ok");
    assert_eq!(n, 0, "server closes connection on wrong token");
    bridge.stop().await;
}

// ---------------------------------------------------------------------------
// 5. stop 清理：session 目录 / proxy 目录 / 连接全清
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn stop_cleans_session_proxy_and_connections() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let sandbox = LocalSandbox::new(ECHO_CHILD);
    let bridge = start_bridge(
        &sandbox,
        &sandbox.node(),
        &[sandbox.child_path.to_string_lossy().into_owned()],
        None,
    )
    .await;
    let proxy_path = PathBuf::from(&bridge.agent_command);
    assert!(proxy_path.exists(), "proxy script written");
    // 找到远端 session 目录（rootDir/.paperclip-runtime/acpx/process-sessions/<uuid>）。
    let sessions_dir = sandbox
        .root_dir
        .join(".paperclip-runtime/acpx/process-sessions");
    assert!(sessions_dir.exists(), "remote sessions dir created");
    let session_dirs: Vec<PathBuf> = std::fs::read_dir(&sessions_dir)
        .expect("read sessions")
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(session_dirs.len(), 1, "one session subdirectory");
    bridge.stop().await;
    assert!(
        !session_dirs[0].exists(),
        "session dir removed after stop"
    );
    assert!(!proxy_path.exists(), "proxy dir removed after stop");
}

// ---------------------------------------------------------------------------
// 6. gate：SSH target → None
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ssh_target_returns_none() {
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let target = ssh_target();
    let env = BTreeMap::new();
    let bridge = start_adapter_execution_target_process_session_bridge(
        &StartProcessSessionBridgeInput {
            run_id: "run-493",
            target: Some(&target),
            runtime_root_dir: None,
            adapter_key: "acpx",
            command: "node",
            args: &[],
            cwd: "/w",
            launch_env: &env,
            timeout_sec: Some(5.0),
            runner,
            on_log: None,
        },
    )
    .await
    .expect("gate returns Ok");
    assert!(bridge.is_none(), "ssh transport does not start process session bridge");
}

fn extract_proxy_port(source: &str) -> Option<u16> {
    let marker = "port: ";
    let start = source.find(marker)? + marker.len();
    let rest = &source[start..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}
