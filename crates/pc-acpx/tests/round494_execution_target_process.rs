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

use pc_acpx::bridge_executor::{BridgeCommandRunner, LocalProcessBridgeRunner};
use pc_acpx::execution_target::AdapterExecutionTarget;
use pc_acpx::execution_target_process::{
    run_adapter_execution_target_process, RunAdapterExecutionTargetProcessOptions,
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

fn command_available(command: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {command}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn node_available() -> bool {
    command_available("node")
}

fn allocate_loopback_port() -> Option<u16> {
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// `BridgeCommandRunner`-shaped adapter that runs via the shell helper
/// (so the runner.execute inside the sandbox branch handles `sh -c`
/// scripts the same way the real provider runner would).
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

/// `SandboxRunLogRunner` impl that delegates to the shell command via
/// `LocalProcessBridgeRunner`-equivalent tokio spawn — captures stdout
/// for the tail parser to consume.
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

// ---------------------------------------------------------------------------
// 1. local 分支：echo + 退出码 + stdout 捕获 + on_log streaming
// ---------------------------------------------------------------------------

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
// 5. ssh 分支：真实 sshd fixture + build_ssh_spawn_target（spawn 远端
//    shell 命令），streaming on_log
// ---------------------------------------------------------------------------

struct SshLabFixture {
    config: SshConnectionConfig,
    remote_workspace_path: PathBuf,
}

impl SshLabFixture {
    async fn start() -> Option<Self> {
        if !command_available("ssh") || !command_available("sshd") || !command_available("ssh-keygen") {
            eprintln!("SKIP: ssh/sshd/ssh-keygen unavailable");
            return None;
        }
        let port = allocate_loopback_port()?;
        let root_dir = std::env::temp_dir().join(format!(
            "paperclip-r494-ssh-{}",
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
            .kill_on_drop(true)
            .spawn()
            .ok()?;
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
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ready = false;
        while Instant::now() < deadline {
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
            let _ = child.kill().await;
            return None;
        }
        Some(Self {
            config,
            remote_workspace_path,
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ssh_branch_runs_remote_command_via_spawn_target() {
    let Some(fixture) = SshLabFixture::start().await else {
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
