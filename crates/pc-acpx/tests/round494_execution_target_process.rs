//! R494 — `run_adapter_execution_target_process` 三分支真实端到端验证。
//!
//! 对齐 Node `execution-target.ts` L570-630：
//! - local 分支：streaming spawn + 捕获 stdout/stderr + timeout/grace/kill
//! - ssh 分支：真实 sshd fixture + build_ssh_spawn_target（spawn 远程 shell
//!   命令），streaming on_log
//! - sandbox 分支：LocalProcessBridgeRunner + 真实 node 远端 child，
//!   run log tail 真实流式推送（对齐 `runLogTail.start` → onLog
//!   incremental chunks）
//!
//! 缺失 sshd / node 时跳过真实部分。

mod common;
use crate::common::{node_available, SshLabFixture};

use pc_acpx::bridge_executor::{BridgeCommandRunner, LocalProcessBridgeRunner};
use pc_acpx::execution_target::AdapterExecutionTarget;
use pc_acpx::execution_target_process::{
    execute_command_for_target, run_adapter_execution_target_process,
    RunAdapterExecutionTargetProcessOptions,
};
use pc_acpx::sandbox_run_log_stream::{
    create_sandbox_run_log_tail_factory, SandboxRunLogRunner, SandboxRunLogTailFactoryOptions,
    SandboxRunLogTickInput, SandboxRunLogTickResult,
};
use pc_acpx::ssh::{
    run_ssh_command, shell_quote, SshCommandOptions, SshConnectionConfig,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};


#[tokio::test(flavor = "multi_thread")]
async fn local_branch_echo_captures_stdout_and_emits_on_log() {
    let chunks = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let chunks_for_log = Arc::clone(&chunks);
    let on_log: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |stream, chunk| {
        chunks_for_log.lock().expect("lock").push((stream.to_string(), chunk.to_string()));
    });
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let env = BTreeMap::new();
    let result = run_adapter_execution_target_process(
        "run-494-local-echo",
        Some(&AdapterExecutionTarget::Local(Default::default())),
        "sh",
        &["-c".to_string(), "printf 'hello-stream\\n'".to_string()],
        &RunAdapterExecutionTargetProcessOptions {
            cwd: "",
            env: &env,
            stdin: None,
            timeout_sec: 0.0,
            grace_sec: 1.0,
            on_log: Some(on_log),
            run_log_tail: None,
            runner,
            kill_flag: None,
            stdout_cap_bytes: None,
            stderr_cap_bytes: None,
        },
    )
    .await
    .expect("local process succeeds");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert_eq!(result.stdout, "hello-stream\n");
    let observed = chunks.lock().expect("lock").clone();
    assert!(
        observed.iter().any(|(s, c)| s == "stdout" && c.contains("hello-stream")),
        "on_log received stdout chunk; got {:?}",
        observed
    );
}

// ---------------------------------------------------------------------------
// 2. local 分支：超时 → timed_out + SIGTERM 升级 SIGKILL
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn local_branch_timeout_triggers_sigterm_and_sigkill() {
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let env = BTreeMap::new();
    let start = Instant::now();
    let result = run_adapter_execution_target_process(
        "run-494-local-timeout",
        Some(&AdapterExecutionTarget::Local(Default::default())),
        "sh",
        &["-c".to_string(), "sleep 5; echo late".to_string()],
        &RunAdapterExecutionTargetProcessOptions {
            cwd: "",
            env: &env,
            stdin: None,
            timeout_sec: 0.3,
            grace_sec: 0.2,
            on_log: None,
            run_log_tail: None,
            runner,
            kill_flag: None,
            stdout_cap_bytes: None,
            stderr_cap_bytes: None,
        },
    )
    .await
    .expect("process completes (with kill)");
    let elapsed = start.elapsed();
    assert!(result.timed_out, "timed_out flag set");
    // 信号路径：SIGTERM 触发后再 SIGKILL（grace 后）。label 是
    // "SIGTERM" 或 "SIGKILL"（取决于在 SIGKILL 之前是否已读到）。
    let label = result.signal.clone().unwrap_or_default();
    assert!(label == "SIGTERM" || label == "SIGKILL", "got signal label {label}");
    assert!(elapsed < Duration::from_secs(3), "process returned quickly (got {elapsed:?})");
}

// ---------------------------------------------------------------------------
// 3. local 分支：kill_flag 触发终止
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn local_branch_kill_flag_terminates_long_running_process() {
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let env = BTreeMap::new();
    let kill_flag = Arc::new(AtomicBool::new(false));
    let flag_for_killer = Arc::clone(&kill_flag);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        flag_for_killer.store(true, Ordering::SeqCst);
    });
    let result = run_adapter_execution_target_process(
        "run-494-local-killflag",
        Some(&AdapterExecutionTarget::Local(Default::default())),
        "sh",
        &["-c".to_string(), "sleep 30".to_string()],
        &RunAdapterExecutionTargetProcessOptions {
            cwd: "",
            env: &env,
            stdin: None,
            timeout_sec: 0.0,
            grace_sec: 1.0,
            on_log: None,
            run_log_tail: None,
            runner,
            kill_flag: Some(kill_flag),
            stdout_cap_bytes: None,
            stderr_cap_bytes: None,
        },
    )
    .await
    .expect("process completes (with kill)");
    assert!(result.timed_out, "kill_flag sets timed_out");
    assert_eq!(result.signal.as_deref(), Some("SIGTERM"));
}

// ---------------------------------------------------------------------------
// 4. sandbox 分支：run log tail 真实流式推送（node child 写日志）
// ---------------------------------------------------------------------------

/// 本地沙箱 fixture + 远端 child（在 /tmp/<uuid>/child.mjs）。
struct SandboxFixture {
    root_dir: PathBuf,
    child_path: PathBuf,
    target: AdapterExecutionTarget,
}

impl SandboxFixture {
    fn new(child_source: &str) -> Self {
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-runlog-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root_dir).expect("root dir");
        let child_path = root_dir.join("child.mjs");
        std::fs::write(&child_path, child_source).expect("child script");
        let target_json = serde_json::json!({
            "kind": "remote",
            "transport": "sandbox",
            "providerKey": "local-test",
            "remoteCwd": root_dir.to_string_lossy(),
            "streamRunLogs": true,
            "timeoutMs": 30_000,
        });
        let target = pc_acpx::execution_target::parse_adapter_execution_target(&target_json)
            .expect("valid sandbox target");
        Self { root_dir, child_path, target }
    }
}

impl Drop for SandboxFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root_dir);
    }
}

const STREAMING_CHILD: &str = r#"// 写到 <logs_dir> 下与 tail marker 配套的 stdout.log
import fs from "node:fs";
const logsDir = process.env.PAPERCLIP_RUN_LOGS_DIR;
if (!logsDir) { console.error("missing PAPERCLIP_RUN_LOGS_DIR"); process.exit(2); }
fs.mkdirSync(logsDir, { recursive: true });
fs.writeFileSync(`${logsDir}/run-1-stdout.log`, "");
fs.writeFileSync(`${logsDir}/run-1-stderr.log`, "");
fs.writeFileSync(`${logsDir}/run-1-status`, "");
// 直接打开 stdout.log 追加，模拟 child 正在产生的输出。
const out = fs.openSync(`${logsDir}/run-1-stdout.log`, "a");
for (let i = 0; i < 5; i++) {
  fs.writeSync(out, `line-${i}\n`);
  // 让 host tick 有机会读
  await new Promise((r) => setTimeout(r, 50));
}
fs.closeSync(out);
process.exit(0);
"#;

#[tokio::test(flavor = "multi_thread")]
async fn sandbox_branch_run_log_tail_streams_incremental_output() {
    if !node_available() {
        eprintln!("SKIP: node not available");
        return;
    }
    let sandbox = SandboxFixture::new(STREAMING_CHILD);
    // 把 logs_dir 注入 launch env（child 期望）。
    let logs_dir = sandbox.root_dir.join("logs");
    let mut env = BTreeMap::new();
    env.insert(
        "PAPERCLIP_RUN_LOGS_DIR".to_string(),
        logs_dir.to_string_lossy().into_owned(),
    );
    env.insert(
        "HOME".to_string(),
        sandbox.root_dir.to_string_lossy().into_owned(),
    );

    let runner: Arc<dyn SandboxRunLogRunner> = Arc::new(TickRunner);
    let factory = create_sandbox_run_log_tail_factory(SandboxRunLogTailFactoryOptions {
        runner: runner.clone(),
        remote_cwd: sandbox.root_dir.to_string_lossy().to_string(),
        logs_dir: logs_dir.to_string_lossy().to_string(),
        shell_command: None,
        poll_interval_ms: Some(50),
        max_chunk_bytes_per_tick: None,
        tick_timeout_ms: Some(5_000),
        max_consecutive_failures: None,
    });

    // shell 命令：node + child；process_session_bridge 风格 sh -lc "exec node ..."
    let shell = format!(
        "{} {}",
        std::env::var("NODE").unwrap_or_else(|_| "node".to_string()),
        sandbox.child_path.display()
    );
    let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
    let chunks_for_log = Arc::clone(&chunks);
    let on_log: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |stream, chunk| {
        if stream == "stdout" {
            chunks_for_log.lock().expect("lock").push(chunk.to_string());
        }
    });

    let process_runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalSandboxRunner);
    let result = run_adapter_execution_target_process(
        "run-494-sandbox-tail",
        Some(&sandbox.target),
        "sh",
        &["-lc".to_string(), format!("exec {shell}")],
        &RunAdapterExecutionTargetProcessOptions {
            cwd: &sandbox.root_dir.to_string_lossy(),
            env: &env,
            stdin: None,
            timeout_sec: 10.0,
            grace_sec: 1.0,
            on_log: Some(on_log),
            run_log_tail: Some(Arc::new(factory)),
            runner: process_runner,
            kill_flag: None,
            stdout_cap_bytes: None,
            stderr_cap_bytes: None,
        },
    )
    .await
    .expect("sandbox process succeeds");
    assert_eq!(result.exit_code, Some(0));
    // 5 行输出应该通过 tail 流式到达 on_log（在 exit 之前）。
    let observed = chunks.lock().expect("lock").join("");
    assert!(
        observed.contains("line-0") && observed.contains("line-4"),
        "tail streamed child output; got {observed:?}"
    );
}

// ---------------------------------------------------------------------------


#[tokio::test(flavor = "multi_thread")]
async fn ssh_branch_runs_remote_command_via_spawn_target() {
    let Some(fixture) = SshLabFixture::start("r494").await else {
        return;
    };
    let ssh_target_json = serde_json::json!({
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
    });
    let target: AdapterExecutionTarget =
        pc_acpx::execution_target::parse_adapter_execution_target(&ssh_target_json)
            .expect("ssh target");
    let chunks = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let chunks_for_log = Arc::clone(&chunks);
    let on_log: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |stream, chunk| {
        chunks_for_log
            .lock()
            .expect("lock")
            .push((stream.to_string(), chunk.to_string()));
    });
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(LocalProcessBridgeRunner);
    let env = BTreeMap::new();
    let remote_marker = "ssh-r494-ok";
    let result = run_adapter_execution_target_process(
        "run-494-ssh",
        Some(&target),
        "sh",
        &["-c".to_string(), format!("echo {remote_marker}")],
        &RunAdapterExecutionTargetProcessOptions {
            cwd: "",
            env: &env,
            stdin: None,
            timeout_sec: 8.0,
            grace_sec: 1.0,
            on_log: Some(on_log),
            run_log_tail: None,
            runner,
            kill_flag: None,
            stdout_cap_bytes: None,
            stderr_cap_bytes: None,
        },
    )
    .await
    .expect("ssh branch succeeds");
    assert_eq!(result.exit_code, Some(0));
    assert!(
        result.stdout.contains(remote_marker),
        "ssh stdout contains marker; got {:?}",
        result.stdout
    );
    let observed = chunks.lock().expect("lock").clone();
    assert!(
        observed
            .iter()
            .any(|(s, c)| s == "stdout" && c.contains(remote_marker)),
        "on_log received remote stdout chunk; got {:?}",
        observed
    );
}

use std::sync::Mutex;

// ---------------------------------------------------------------------------
// 6. execute_command_for_target: local target → 本地 spawn（走本地分支）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn execute_command_for_target_local_dispatches_locally() {
    let local_target = serde_json::json!({ "kind": "local" });
    let env = BTreeMap::new();
    let result = execute_command_for_target(
        "sh",
        &["-c".to_string(), "printf 'local-dispatch\n'".to_string()],
        None,
        0.0,
        1.0,
        &env,
        "",
        Some(&local_target),
        None,
        None,
    )
    .await
    .expect("local dispatch succeeds");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);
    assert_eq!(result.stdout, "local-dispatch
");
}

// ---------------------------------------------------------------------------
// 7. execute_command_for_target: ssh target → ssh 分支（真实 sshd）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn execute_command_for_target_ssh_dispatches_remotely() {
    let Some(fixture) = SshLabFixture::start("r494").await else {
        return;
    };
    let target = serde_json::json!({
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
    });
    let env = BTreeMap::new();
    let remote_marker = "exec-cmd-target-ok";
    let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
    let chunks_for_log = Arc::clone(&chunks);
    let on_log: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |stream, chunk| {
        if stream == "stdout" {
            chunks_for_log.lock().expect("lock").push(chunk.to_string());
        }
    });
    let result = execute_command_for_target(
        "sh",
        &["-c".to_string(), format!("echo {remote_marker}")],
        None,
        8.0,
        1.0,
        &env,
        "",
        Some(&target),
        Some(on_log),
        None,
    )
    .await
    .expect("ssh dispatch succeeds");
    assert_eq!(result.exit_code, Some(0));
    assert!(
        result.stdout.contains(remote_marker),
        "remote stdout contains marker; got {:?}",
        result.stdout
    );
    let observed = chunks.lock().expect("lock").join("");
    assert!(observed.contains(remote_marker), "on_log streamed remote output");
}

// ---------------------------------------------------------------------------
// 8. execute_command_for_target: sandbox target → 回退本地（无 provider runner）
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn execute_command_for_target_sandbox_falls_back_to_local() {
    let sandbox_target = serde_json::json!({
        "kind": "remote",
        "transport": "sandbox",
        "providerKey": "local-test",
        "remoteCwd": "/sandbox/workspace",
        "timeoutMs": 30_000,
    });
    let env = BTreeMap::new();
    let chunks = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let chunks_for_log = Arc::clone(&chunks);
    let on_log: Arc<dyn Fn(&str, &str) + Send + Sync> = Arc::new(move |stream, chunk| {
        chunks_for_log
            .lock()
            .expect("lock")
            .push((stream.to_string(), chunk.to_string()));
    });
    let result = execute_command_for_target(
        "sh",
        &["-c".to_string(), "printf 'sandbox-fallback\n'".to_string()],
        None,
        0.0,
        1.0,
        &env,
        "",
        Some(&sandbox_target),
        Some(on_log),
        None,
    )
    .await
    .expect("sandbox fallback succeeds");
    assert_eq!(result.exit_code, Some(0));
    let observed = chunks.lock().expect("lock").clone();
    let stdout_text: String = observed
        .iter()
        .filter(|(s, _)| s == "stdout")
        .map(|(_, c)| c.clone())
        .collect();
    assert!(
        stdout_text.contains("sandbox-fallback"),
        "fallback ran the local command; got {stdout_text:?}"
    );
    assert!(
        stdout_text.contains("sandbox provider runner not implemented"),
        "fallback emits the note; got {stdout_text:?}"
    );
}

// ---------------------------------------------------------------------------
// 9. execute_command_for_target: kill_flag 集成 → timed_out + SIGTERM
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn execute_command_for_target_respects_kill_flag() {
    let kill_flag = Arc::new(AtomicBool::new(false));
    let flag_for_killer = Arc::clone(&kill_flag);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        flag_for_killer.store(true, Ordering::SeqCst);
    });
    let env = BTreeMap::new();
    let result = execute_command_for_target(
        "sh",
        &["-c".to_string(), "sleep 30".to_string()],
        None,
        0.0,
        1.0,
        &env,
        "",
        None, // no target → local dispatch
        None,
        Some(kill_flag),
    )
    .await
    .expect("kill_flag dispatch succeeds");
    assert!(result.timed_out, "kill_flag triggers timed_out");
    assert_eq!(result.signal.as_deref(), Some("SIGTERM"));
}


// ---------------------------------------------------------------------------
// LocalSandboxRunner: BridgeCommandRunner that delegates to LocalProcessBridgeRunner
// ---------------------------------------------------------------------------
struct LocalSandboxRunner;

#[async_trait::async_trait]
impl BridgeCommandRunner for LocalSandboxRunner {
    async fn execute(
        &self,
        input: &pc_acpx::bridge_executor::RunnerExecuteInput,
    ) -> Result<pc_acpx::bridge_executor::RunnerCommandResult, String> {
        LocalProcessBridgeRunner.execute(input).await
    }
}

// ---------------------------------------------------------------------------
// TickRunner: SandboxRunLogRunner that spawns a tokio child to capture stdout
// ---------------------------------------------------------------------------
struct TickRunner;

#[async_trait::async_trait]
impl SandboxRunLogRunner for TickRunner {
    async fn execute(
        &self,
        input: SandboxRunLogTickInput,
    ) -> Result<SandboxRunLogTickResult, String> {
        let mut cmd = tokio::process::Command::new(&input.command);
        cmd.args(&input.args)
            .envs(input.env.iter())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if !input.cwd.is_empty() {
            cmd.current_dir(&input.cwd);
        }
        let output = tokio::time::timeout(
            Duration::from_millis(input.timeout_ms),
            cmd.output(),
        )
        .await
        .map_err(|_| "tick timeout".to_string())?
        .map_err(|error| error.to_string())?;
        Ok(SandboxRunLogTickResult {
            exit_code: output.status.code(),
            timed_out: !output.status.success() && output.stdout.is_empty(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}

