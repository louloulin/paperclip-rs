//! R493 — codex adapter 真实 process session bridge 启动接入验证。
//!
//! 用本地进程 runner（`LocalProcessBridgeRunner`）+ 临时目录模拟 sandbox
//! target，真实 node 远端脚本 + 真实 TCP server + 真实 proxy 脚本跑通
//! `start_codex_process_session_bridge`（R493 接入 codex execute 的
//! process session bridge 启动路径）：
//! - sandbox target + 无 runner → `Ok(None)`（Rust 现状）
//! - SSH target + runner → `Ok(None)`（Node gate）
//! - sandbox target + runner → 真实启动 bridge：proxy 双向桥接
//!   （stdin → 远端 child 回显）+ stop 清理
//!
//! node 缺失时跳过真实部分。

use pc_acpx::bridge_executor::{BridgeCommandRunner, LocalProcessBridgeRunner};
use pc_adapter_codex_local::codex_bridge_env::start_codex_process_session_bridge;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
            "paperclip-codex-process-session-{}",
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

    fn node(&self) -> String {
        std::env::var("NODE").unwrap_or_else(|_| "node".to_string())
    }

    /// agentCommandShell（Node `configuredCommand` 语义）：`node <child>`。
    fn agent_command_shell(&self) -> String {
        format!("{} {}", self.node(), self.child_path.display())
    }
}

impl Drop for LocalSandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

fn ssh_target_value() -> serde_json::Value {
    serde_json::json!({
        "kind": "remote",
        "transport": "ssh",
        "host": "127.0.0.1",
        "port": 2222,
        "username": "fixture",
        "remoteWorkspacePath": "/w",
        "remoteCwd": "/w",
        "strictHostKeyChecking": false,
    })
}

const ECHO_CHILD: &str = r#"process.stdin.on("data", (chunk) => {
  process.stdout.write("out:" + chunk.toString());
  process.stderr.write("err:" + chunk.toString());
});
"#;

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

// ---------------------------------------------------------------------------
// 1. sandbox target + 无 runner → Ok(None)（Rust 现状，execute 默认路径）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_target_without_runner_returns_none() {
    let sandbox = LocalSandbox::new(ECHO_CHILD);
    let env = BTreeMap::new();
    let bridge = start_codex_process_session_bridge(
        "run-493",
        Some(&sandbox.target),
        None,
        "codex",
        &sandbox.agent_command_shell(),
        &sandbox.root_dir.to_string_lossy(),
        &env,
        Some(5.0),
        None,
        None,
    )
    .await
    .expect("gate returns Ok");
    assert!(
        bridge.is_none(),
        "no provider runner ⇒ no process session bridge"
    );
}

// ---------------------------------------------------------------------------
// 2. SSH target + runner → Ok(None)（Node gate：仅 remote + sandbox）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn ssh_target_with_runner_returns_none() {
    let target = ssh_target_value();
    let env = BTreeMap::new();
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let bridge = start_codex_process_session_bridge(
        "run-493",
        Some(&target),
        None,
        "codex",
        "codex-acp",
        "/w",
        &env,
        Some(5.0),
        Some(runner),
        None,
    )
    .await
    .expect("gate returns Ok");
    assert!(
        bridge.is_none(),
        "ssh transport ⇒ no process session bridge"
    );
}

// ---------------------------------------------------------------------------
// 3. sandbox target + runner → 真实启动 bridge，双向桥接 + stop 清理
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_target_starts_real_bridge_via_local_runner() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let sandbox = LocalSandbox::new(ECHO_CHILD);
    // launch env 里把 HOME 指到沙箱目录：`sh -lc`（Node 忠实 launch 形状）
    // 以登录 shell 启动，避免宿主 ~/.profile 噪音混入远端 child stderr。
    let mut env = BTreeMap::new();
    env.insert(
        "HOME".to_string(),
        sandbox.root_dir.to_string_lossy().into_owned(),
    );
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let bridge = start_codex_process_session_bridge(
        "run-493",
        Some(&sandbox.target),
        None,
        "codex",
        &sandbox.agent_command_shell(),
        &sandbox.root_dir.to_string_lossy(),
        &env,
        Some(8.0),
        Some(runner),
        None,
    )
    .await
    .expect("bridge starts")
    .expect("sandbox target + runner ⇒ bridge present");
    let proxy_path = PathBuf::from(&bridge.agent_command);
    assert!(proxy_path.exists(), "proxy script written");
    let (stdout, stderr, code) = run_proxy_with_input(&bridge.agent_command, "hello\n", 8_000)
        .await
        .expect("proxy completes");
    assert_eq!(code, Some(0), "exit code");
    assert_eq!(stdout, "out:hello\n");
    assert_eq!(stderr, "err:hello\n");
    bridge.stop().await;
    assert!(!proxy_path.exists(), "proxy dir removed after stop");
}
