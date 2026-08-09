//! R491 — paperclip bridge 真实执行器。
//!
//! 把 R480-R485 的 bridge 决策（`sandbox_callback_bridge`）串成 Node
//! `sandbox-callback-bridge.ts` / `execution-target.ts` 的真实 I/O 编排：
//! - 远程命令执行器抽象（对齐 Node `CommandManagedRuntimeRunner`）+
//!   本地进程实现（测试 / 本地模拟）
//! - 队列客户端（对齐 Node `createCommandManagedSandboxCallbackBridgeQueueClient`）
//! - bridge asset 创建（对齐 Node `createSandboxCallbackBridgeAsset`）
//! - bridge server 启动 / 就绪 / 停止（对齐 Node
//!   `startSandboxCallbackBridgeServer`）
//! - bridge worker 轮询循环（对齐 Node `startSandboxCallbackBridgeWorker`）
//! - host API 转发 handler（对齐 Node `startAdapterExecutionTargetPaperclipBridge`
//!   内的 fetch 转发）
//! - 顶层编排 `start_adapter_execution_target_paperclip_bridge`（对齐 Node
//!   `execution-target.ts` L1719-1930，含 teardown）
//!
//! # 设计边界
//!
//! 本模块不依赖具体 SSH 实现：调用方注入 [`BridgeCommandRunner`]（真实 SSH
//! 执行器在 `pc-acpx::ssh` 后续轮次补齐）。sandbox run log tail
//! （`streamRunLogs`）执行器留待后续。

use crate::sandbox_callback_bridge as scb;
use crate::sandbox_run_log_stream::{
    create_sandbox_run_log_tail_factory, SandboxRunLogRunner, SandboxRunLogTailFactory,
    SandboxRunLogTailFactoryOptions,
};
use crate::{
    execution_target::{
        adapter_execution_target_remote_cwd, adapter_execution_target_uses_paperclip_bridge,
        bridge_handle_paths, resolve_bridge_max_body_bytes, resolve_bridge_timeout_ms,
        AdapterExecutionTarget,
    },
    sandbox_callback_bridge::{
        bridge_response_body_utf8_len_within_limit, bridge_response_body_within_limit,
        bridge_runner_failure_message, build_bridge_exec_env, build_bridge_forward_url,
        build_bridge_response_headers, build_bridge_server_stop_script,
        build_list_json_files_script, build_make_dirs_script, build_read_text_file_script,
        build_remove_script, build_rename_script, build_write_response_file_script,
        build_write_text_file_steps, create_sandbox_callback_bridge_token,
        decide_bridge_handler_response, decide_bridge_response_write,
        decide_bridge_worker_loop_action, decide_bridge_worker_should_stop_processing,
        decide_bridge_worker_stop_deadline, denied_bridge_request_response,
        get_sandbox_callback_bridge_server_source, handler_failure_bridge_response,
        invalid_bridge_request_payload_response, parse_bridge_ready_data,
        parse_bridge_request_file, parse_list_json_files_output, parse_sync_text_file_result,
        parse_write_response_file_result, pending_request_failure_bridge_response,
        preferred_shell_for_sandbox, sanitize_sandbox_callback_bridge_headers, shell_command_args,
        start_sandbox_callback_bridge_server_plan, BridgeDirectories, BridgeResponseWritePlan,
        BridgeServerStopScriptInput, SandboxCallbackBridgeRequest, SandboxCallbackBridgeResponse,
        DEFAULT_BRIDGE_POLL_INTERVAL_MS, DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS,
        DEFAULT_BRIDGE_STOP_TIMEOUT_MS, DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES,
        SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT,
    },
};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// =============================================================================
// 远程命令执行器（对齐 Node CommandManagedRuntimeRunner）
// =============================================================================

/// 远程命令执行结果。
#[derive(Debug, Clone)]
pub struct RunnerCommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

impl RunnerCommandResult {
    /// 成功判定（对齐 Node `requireSuccessfulResult`：
    /// timedOut → 失败；exitCode 非 0 → 失败）。
    #[must_use]
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// 远程命令执行输入。
#[derive(Debug, Clone)]
pub struct RunnerExecuteInput {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    /// 写入子进程 stdin 的数据（同步脚本的 base64 上传走这里）。
    pub stdin: Option<String>,
    pub timeout_ms: u64,
}

/// 远程 shell 命令执行器抽象（对齐 Node `CommandManagedRuntimeRunner`）。
#[async_trait]
pub trait BridgeCommandRunner: Send + Sync {
    async fn execute(&self, input: &RunnerExecuteInput) -> Result<RunnerCommandResult, String>;
}

/// 本地进程执行器：直接用 `command args` 在本地 spawn（测试与本地模拟）。
pub struct LocalProcessBridgeRunner;

#[async_trait]
impl BridgeCommandRunner for LocalProcessBridgeRunner {
    async fn execute(&self, input: &RunnerExecuteInput) -> Result<RunnerCommandResult, String> {
        use tokio::io::AsyncWriteExt;
        use tokio::process::Command;
        use tokio::time::{timeout, Duration};

        let mut command = Command::new(&input.command);
        command
            .args(&input.args)
            .envs(&input.env)
            .stdin(if input.stdin.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if !input.cwd.is_empty() {
            command.current_dir(&input.cwd);
        }
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        if let Some(stdin_data) = &input.stdin {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| "child stdin pipe unavailable".to_string())?;
            stdin
                .write_all(stdin_data.as_bytes())
                .await
                .map_err(|error| error.to_string())?;
            stdin.shutdown().await.map_err(|error| error.to_string())?;
        }
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "child stdout pipe unavailable".to_string())?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| "child stderr pipe unavailable".to_string())?;
        let stdout_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            let _ = stdout.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });
        let stderr_task = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf).await;
            String::from_utf8_lossy(&buf).into_owned()
        });
        let wait = async {
            child
                .wait()
                .await
                .map(|status| status.code())
                .map_err(|error| error.to_string())
        };
        let (status, timed_out) = match timeout(Duration::from_millis(input.timeout_ms), wait).await
        {
            Ok(Ok(code)) => (code, false),
            Ok(Err(error)) => return Err(error),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                (None, true)
            }
        };
        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        Ok(RunnerCommandResult {
            stdout,
            stderr,
            exit_code: status,
            timed_out,
        })
    }
}

/// 在 runner 上执行单条 shell 脚本（对齐 Node `runShell`）。
pub async fn run_shell(
    runner: &Arc<dyn BridgeCommandRunner>,
    remote_cwd: &str,
    script: &str,
    shell: &str,
    stdin: Option<String>,
    env: BTreeMap<String, String>,
    timeout_ms: u64,
) -> Result<RunnerCommandResult, String> {
    // 对齐 Node `runShell`：所有 bridge shell 执行都带
    // `SANDBOX_EXEC_CHANNEL_ENV=bridge`（start 额外携带 server env）。
    let env = build_bridge_exec_env(&env);
    runner
        .execute(&RunnerExecuteInput {
            command: shell.to_string(),
            args: shell_command_args(script),
            cwd: remote_cwd.to_string(),
            env,
            stdin,
            timeout_ms,
        })
        .await
}

/// 校验命令成功；失败时抛出 Node 同款消息（对齐
/// `requireSuccessfulResult(action, result)`）。
pub fn require_successful_result(action: &str, result: &RunnerCommandResult) -> Result<(), String> {
    if result.succeeded() {
        return Ok(());
    }
    Err(bridge_runner_failure_message(
        action,
        result.timed_out,
        result.exit_code,
        &result.stderr,
        &result.stdout,
    ))
}

// =============================================================================
// 队列客户端（对齐 Node createCommandManagedSandboxCallbackBridgeQueueClient）
// =============================================================================

/// 队列客户端抽象（对齐 Node `SandboxCallbackBridgeQueueClient`）。
#[async_trait]
pub trait BridgeQueueClient: Send + Sync {
    async fn make_dirs(&self, paths: &[String]) -> Result<(), String>;
    async fn list_json_files(&self, dir: &str) -> Result<Vec<String>, String>;
    async fn read_text_file(&self, path: &str) -> Result<String, String>;
    async fn write_text_file(&self, path: &str, body: &str) -> Result<(), String>;
    async fn write_response_file(
        &self,
        response_path: &str,
        body: &str,
        request_path: Option<&str>,
    ) -> Result<bool, String>;
    async fn rename(&self, from_path: &str, to_path: &str) -> Result<(), String>;
    async fn remove(&self, path: &str) -> Result<(), String>;
}

/// 基于 runner 的队列客户端：每个操作 = 一条 shell 脚本（R484 决策脚本）。
pub struct RunnerBridgeQueueClient {
    pub runner: Arc<dyn BridgeCommandRunner>,
    pub remote_cwd: String,
    pub timeout_ms: u64,
    pub shell: &'static str,
}

impl RunnerBridgeQueueClient {
    #[must_use]
    pub fn new(runner: Arc<dyn BridgeCommandRunner>, remote_cwd: String, timeout_ms: u64) -> Self {
        Self {
            runner,
            remote_cwd,
            timeout_ms,
            shell: preferred_shell_for_sandbox(None),
        }
    }

    async fn execute_script(&self, script: &str) -> Result<RunnerCommandResult, String> {
        run_shell(
            &self.runner,
            &self.remote_cwd,
            script,
            self.shell,
            None,
            BTreeMap::new(),
            self.timeout_ms,
        )
        .await
    }
}

#[async_trait]
impl BridgeQueueClient for RunnerBridgeQueueClient {
    async fn make_dirs(&self, paths: &[String]) -> Result<(), String> {
        if let Some(script) = build_make_dirs_script(paths) {
            let result = self.execute_script(&script).await?;
            require_successful_result("create bridge queue directories", &result)?;
        }
        Ok(())
    }

    async fn list_json_files(&self, dir: &str) -> Result<Vec<String>, String> {
        let script = build_list_json_files_script(dir);
        let result = self.execute_script(&script).await?;
        require_successful_result("list bridge queue files", &result)?;
        Ok(parse_list_json_files_output(&result.stdout))
    }

    async fn read_text_file(&self, path: &str) -> Result<String, String> {
        let script = build_read_text_file_script(path);
        let result = self.execute_script(&script).await?;
        require_successful_result("read bridge queue file", &result)?;
        scb::base64_decode_utf8(result.stdout.trim())
            .map_err(|error| format!("bridge queue file {path} is not valid base64: {error}"))
    }

    async fn write_text_file(&self, path: &str, body: &str) -> Result<(), String> {
        let steps = build_write_text_file_steps(path, body);
        for step in steps {
            let result = self.execute_script(&step.script).await?;
            require_successful_result(&step.action, &result)?;
        }
        Ok(())
    }

    async fn write_response_file(
        &self,
        response_path: &str,
        body: &str,
        request_path: Option<&str>,
    ) -> Result<bool, String> {
        let script = build_write_response_file_script(response_path, request_path);
        let result = self.execute_with_stdin(&script, body.to_string()).await?;
        require_successful_result("write bridge response file", &result)?;
        parse_write_response_file_result(&result.stdout)
    }

    async fn rename(&self, from_path: &str, to_path: &str) -> Result<(), String> {
        let script = build_rename_script(from_path, to_path);
        let result = self.execute_script(&script).await?;
        require_successful_result("rename bridge queue file", &result)
    }

    async fn remove(&self, path: &str) -> Result<(), String> {
        let script = build_remove_script(path);
        let result = self.execute_script(&script).await?;
        require_successful_result("remove bridge queue file", &result)
    }
}

impl RunnerBridgeQueueClient {
    async fn execute_with_stdin(
        &self,
        script: &str,
        stdin: String,
    ) -> Result<RunnerCommandResult, String> {
        self.runner
            .execute(&RunnerExecuteInput {
                command: self.shell.to_string(),
                args: shell_command_args(script),
                cwd: self.remote_cwd.clone(),
                env: BTreeMap::new(),
                stdin: Some(stdin),
                timeout_ms: self.timeout_ms,
            })
            .await
    }
}

// =============================================================================
// bridge asset（对齐 Node createSandboxCallbackBridgeAsset）
// =============================================================================

/// bridge server asset：本地临时目录 + entrypoint 源码文件。
pub struct BridgeAsset {
    pub local_dir: PathBuf,
    pub entrypoint: PathBuf,
}

impl BridgeAsset {
    /// 创建 asset（mkdtemp + 写 entrypoint 源码）。
    pub fn create(source: &str) -> std::io::Result<Self> {
        let local_dir =
            std::env::temp_dir().join(format!("paperclip-bridge-asset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&local_dir)?;
        let entrypoint = local_dir.join(SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT);
        std::fs::write(&entrypoint, source)?;
        Ok(Self {
            local_dir,
            entrypoint,
        })
    }

    /// 清理 asset 目录（对齐 Node `cleanup`；失败静默）。
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.local_dir);
    }
}

// =============================================================================
// bridge server 启动 / 就绪 / 停止（对齐 Node startSandboxCallbackBridgeServer）
// =============================================================================

/// [`start_bridge_server`] 输入（对齐 Node 同名函数参数）。
pub struct StartBridgeServerInput<'a> {
    pub runner: Arc<dyn BridgeCommandRunner>,
    pub remote_cwd: &'a str,
    pub asset_remote_dir: &'a str,
    pub queue_dir: &'a str,
    pub bridge_token: &'a str,
    pub bridge_asset: Option<&'a BridgeAsset>,
    pub host: Option<&'a str>,
    pub port: Option<u16>,
    pub poll_interval_ms: Option<u64>,
    pub response_timeout_ms: Option<u64>,
    pub timeout_ms: Option<u64>,
    pub node_command: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub max_queue_depth: Option<u64>,
    pub max_body_bytes: Option<u64>,
}

/// 已启动的 bridge server（对齐 Node `StartedSandboxCallbackBridgeServer`）。
pub struct StartedBridgeServer {
    pub base_url: String,
    pub host: String,
    pub port: u16,
    pub pid: u32,
    pub directories: BridgeDirectories,
    pub queue_dir: String,
    pub remote_cwd: String,
    pub timeout_ms: u64,
    pub shell: &'static str,
    runner: Arc<dyn BridgeCommandRunner>,
    stop_script: String,
}

impl StartedBridgeServer {
    /// 停止 server（对齐 Node `stop`：kill pid、等待退出、清理 pid/ready 文件）。
    pub async fn stop(&self) -> Result<(), String> {
        let result = run_shell(
            &self.runner,
            &self.remote_cwd,
            &self.stop_script,
            self.shell,
            None,
            BTreeMap::new(),
            self.timeout_ms,
        )
        .await?;
        if result.timed_out {
            return Err(bridge_runner_failure_message(
                "stop sandbox callback bridge",
                true,
                result.exit_code,
                &result.stderr,
                &result.stdout,
            ));
        }
        Ok(())
    }
}

/// 启动 bridge server（对齐 Node `startSandboxCallbackBridgeServer` L947-1094）：
/// asset 同步 → 启动脚本 → 就绪轮询 → ready.json 解析。
pub async fn start_bridge_server(
    input: &StartBridgeServerInput<'_>,
) -> Result<StartedBridgeServer, String> {
    let timeout_ms =
        scb::normalize_timeout_ms(input.timeout_ms, DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS);
    let shell = preferred_shell_for_sandbox(input.shell);
    let plan = start_sandbox_callback_bridge_server_plan(&scb::StartBridgeServerPlanInput {
        queue_dir: input.queue_dir.to_string(),
        bridge_token: input.bridge_token.to_string(),
        asset_remote_dir: input.asset_remote_dir.to_string(),
        bridge_asset_source: input
            .bridge_asset
            .map(|asset| std::fs::read_to_string(&asset.entrypoint).unwrap_or_default()),
        host: input.host.map(str::to_string),
        port: input.port,
        poll_interval_ms: input.poll_interval_ms,
        response_timeout_ms: input.response_timeout_ms,
        max_queue_depth: input.max_queue_depth,
        max_body_bytes: input.max_body_bytes,
        timeout_ms: Some(timeout_ms),
        shell_command: input.shell.map(str::to_string),
        node_command: input.node_command.map(str::to_string),
    });

    // 1. 同步 entrypoint（sha 门控 + base64 上传）。
    if let Some(sync) = &plan.entrypoint_sync {
        let source = input
            .bridge_asset
            .map(|asset| std::fs::read_to_string(&asset.entrypoint).unwrap_or_default())
            .unwrap_or_default();
        let result = run_shell(
            &input.runner,
            input.remote_cwd,
            &sync.uploaded_decision_script,
            shell,
            Some(scb::base64_encode_utf8(&source)),
            BTreeMap::new(),
            timeout_ms,
        )
        .await?;
        require_successful_result(&sync.action, &result)?;
        parse_sync_text_file_result(&result.stdout, &sync.label)?;
    }

    // 2. 启动脚本（mkdir / 清 ready+pid / nohup node / pid 文件 / 输出 pid）。
    let start_result = run_shell(
        &input.runner,
        input.remote_cwd,
        &plan.start_script,
        shell,
        None,
        plan.env.clone(),
        timeout_ms,
    )
    .await?;
    require_successful_result("start sandbox callback bridge", &start_result)?;

    // 3. 就绪轮询（200 × 0.05s；失败时输出远端日志）。
    let ready_result = run_shell(
        &input.runner,
        input.remote_cwd,
        &plan.ready_script,
        shell,
        None,
        BTreeMap::new(),
        timeout_ms,
    )
    .await?;
    require_successful_result("wait for sandbox callback bridge readiness", &ready_result)?;

    // 4. 解析 ready.json（host/port/baseUrl/pid）。
    let ready = parse_bridge_ready_data(ready_result.stdout.trim())?;
    if ready.port > u16::MAX as u64 {
        return Err("Sandbox callback bridge reported an invalid listening port.".to_string());
    }
    let stop_script = build_bridge_server_stop_script(&BridgeServerStopScriptInput {
        pid_file: plan.directories.pid_file.clone(),
        ready_file: plan.directories.ready_file.clone(),
    });
    Ok(StartedBridgeServer {
        base_url: ready.base_url,
        host: ready.host,
        port: ready.port as u16,
        pid: ready.pid as u32,
        directories: plan.directories,
        queue_dir: input.queue_dir.to_string(),
        remote_cwd: input.remote_cwd.to_string(),
        timeout_ms,
        shell,
        runner: input.runner.clone(),
        stop_script,
    })
}

// =============================================================================
// bridge worker（对齐 Node startSandboxCallbackBridgeWorker）
// =============================================================================

/// handler 成功结果。
#[derive(Debug, Clone)]
pub struct BridgeHandlerResult {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

/// 请求 handler：把 sandbox 请求转发给 host API 并返回响应。
pub type BridgeHandleRequestFn = Arc<
    dyn Fn(
            SandboxCallbackBridgeRequest,
        ) -> Pin<Box<dyn Future<Output = Result<BridgeHandlerResult, String>> + Send>>
        + Send
        + Sync,
>;

/// 授权函数：返回 `Some(reason)` 表示拒绝。
pub type BridgeAuthorizeFn =
    Arc<dyn Fn(&SandboxCallbackBridgeRequest) -> Option<String> + Send + Sync>;

/// [`start_bridge_worker`] 输入。
pub struct StartBridgeWorkerInput {
    pub client: Arc<dyn BridgeQueueClient>,
    pub queue_dir: String,
    pub poll_interval_ms: Option<u64>,
    pub max_body_bytes: Option<u64>,
    pub authorize: Option<BridgeAuthorizeFn>,
    pub handle_request: BridgeHandleRequestFn,
}

/// worker 句柄（对齐 Node `SandboxCallbackBridgeWorkerHandle`）。
pub struct BridgeWorkerHandle {
    inner: Arc<WorkerInner>,
}

struct WorkerInner {
    stopping: AtomicBool,
    stop_deadline_ms: Mutex<Option<u64>>,
    settled: tokio::sync::Notify,
    join: Mutex<Option<tokio::task::JoinHandle<()>>>,
    client: Arc<dyn BridgeQueueClient>,
    queue_dir: String,
}

impl BridgeWorkerHandle {
    /// 停止 worker（对齐 Node `stop`）：置 stopping + deadline，
    /// 等待循环退出（drain 超时），然后给未决请求补写 503。
    pub async fn stop(&self) {
        let now = scb::now_unix_ms();
        self.inner.stopping.store(true, Ordering::SeqCst);
        let deadline = decide_bridge_worker_stop_deadline(now, None);
        *self
            .inner
            .stop_deadline_ms
            .lock()
            .expect("stop deadline lock") = Some(deadline);
        let drain = DEFAULT_BRIDGE_STOP_TIMEOUT_MS;
        let join = self.inner.join.lock().expect("join lock").take();
        if let Some(join) = join {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(drain), join).await;
        }
        self.inner.settled.notify_waiters();
        let message = "Bridge worker stopped before request could be handled.";
        let _ = fail_pending_requests(&self.inner.client, &self.inner.queue_dir, message).await;
    }
}

/// 启动 worker：创建队列目录 + 后台轮询循环（对齐 Node loop）。
pub async fn start_bridge_worker(
    input: StartBridgeWorkerInput,
) -> Result<BridgeWorkerHandle, String> {
    let poll_interval_ms =
        scb::normalize_timeout_ms(input.poll_interval_ms, DEFAULT_BRIDGE_POLL_INTERVAL_MS);
    let max_body_bytes = scb::normalize_timeout_ms(
        input.max_body_bytes,
        DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES,
    );
    let directories = scb::sandbox_callback_bridge_directories(&input.queue_dir);
    let queue_directories = vec![
        directories.root_dir.clone(),
        directories.requests_dir.clone(),
        directories.responses_dir.clone(),
        directories.logs_dir.clone(),
    ];
    input.client.make_dirs(&queue_directories).await?;

    let authorize = input
        .authorize
        .unwrap_or_else(|| default_authorize_route_allowlist());
    let inner = Arc::new(WorkerInner {
        stopping: AtomicBool::new(false),
        stop_deadline_ms: Mutex::new(None),
        settled: tokio::sync::Notify::new(),
        join: Mutex::new(None),
        client: input.client.clone(),
        queue_dir: input.queue_dir.clone(),
    });
    let worker_inner = Arc::clone(&inner);
    let requests_dir = directories.requests_dir.clone();
    let responses_dir = directories.responses_dir.clone();
    let join = tokio::spawn(async move {
        let loop_result: Result<(), String> = async {
            loop {
                let file_names = worker_inner.client.list_json_files(&requests_dir).await?;
                let stopping = worker_inner.stopping.load(Ordering::SeqCst);
                match decide_bridge_worker_loop_action(file_names.len(), stopping) {
                    scb::BridgeWorkerLoopAction::Stop => break,
                    scb::BridgeWorkerLoopAction::Sleep => {
                        tokio::time::sleep(std::time::Duration::from_millis(poll_interval_ms))
                            .await;
                        continue;
                    }
                    scb::BridgeWorkerLoopAction::Process => {}
                }
                for file_name in file_names {
                    let stopping = worker_inner.stopping.load(Ordering::SeqCst);
                    let deadline = *worker_inner.stop_deadline_ms.lock().expect("deadline lock");
                    let now = scb::now_unix_ms();
                    if decide_bridge_worker_should_stop_processing(
                        stopping,
                        now,
                        deadline.unwrap_or(u64::MAX),
                    ) {
                        break;
                    }
                    let request_path = scb::posix_join(&requests_dir, &file_name);
                    let response_path = scb::posix_join(&responses_dir, &file_name);
                    let outcome = process_request_file(
                        &worker_inner.client,
                        &request_path,
                        &response_path,
                        &file_name,
                        &authorize,
                        &input.handle_request,
                        max_body_bytes,
                    )
                    .await;
                    if let Err(error) = outcome {
                        let message = format!("Sandbox callback bridge worker failed: {error}");
                        eprintln!("[paperclip] {message}");
                        let _ = fail_pending_requests(
                            &worker_inner.client,
                            &worker_inner.queue_dir,
                            &message,
                        )
                        .await;
                        break;
                    }
                }
                let stopping = worker_inner.stopping.load(Ordering::SeqCst);
                let deadline = *worker_inner.stop_deadline_ms.lock().expect("deadline lock");
                let now = scb::now_unix_ms();
                if decide_bridge_worker_should_stop_processing(
                    stopping,
                    now,
                    deadline.unwrap_or(u64::MAX),
                ) {
                    break;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = loop_result {
            let message = format!("Sandbox callback bridge worker failed: {error}");
            eprintln!("[paperclip] {message}");
            let _ = fail_pending_requests(&worker_inner.client, &worker_inner.queue_dir, &message)
                .await;
        }
        worker_inner.settled.notify_waiters();
    });
    *inner.join.lock().expect("join lock") = Some(join);
    Ok(BridgeWorkerHandle { inner })
}

/// 默认授权：路由 allowlist（对齐 Node
/// `authorizeSandboxCallbackBridgeRequestWithRoutes`）。
fn default_authorize_route_allowlist() -> BridgeAuthorizeFn {
    Arc::new(|request: &SandboxCallbackBridgeRequest| {
        scb::authorize_sandbox_callback_bridge_request_with_routes(
            &request.method,
            &request.path,
            None,
        )
        .err()
    })
}

/// 处理单个请求文件（对齐 Node `processRequestFile`）：
/// 解析失败 → 400；授权拒绝 → 403；handler 抛错 → 502；成功 → 响应文件。
async fn process_request_file(
    client: &Arc<dyn BridgeQueueClient>,
    request_path: &str,
    response_path: &str,
    file_name: &str,
    authorize: &BridgeAuthorizeFn,
    handle_request: &BridgeHandleRequestFn,
    max_body_bytes: u64,
) -> Result<(), String> {
    let raw = client.read_text_file(request_path).await?;
    let completed_at = scb::now_rfc3339();
    let parsed = parse_bridge_request_file(&raw);
    let request = match parsed {
        Ok(request) => request,
        Err(_) => {
            let request_id = scb::bridge_request_id_from_file_name(file_name)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            write_bridge_response(
                client,
                request_path,
                response_path,
                &invalid_bridge_request_payload_response(request_id, completed_at),
                true,
            )
            .await?;
            client.remove(request_path).await?;
            return Ok(());
        }
    };

    if let Some(denial_reason) = authorize(&request) {
        write_bridge_response(
            client,
            request_path,
            response_path,
            &denied_bridge_request_response(request.id.clone(), &denial_reason, scb::now_rfc3339()),
            true,
        )
        .await?;
        client.remove(request_path).await?;
        return Ok(());
    }

    let outcome = match handle_request(request.clone()).await {
        Ok(result) => decide_bridge_handler_response(
            request.id.clone(),
            result.status,
            &result.headers,
            &result.body,
            max_body_bytes,
            scb::now_rfc3339(),
        ),
        Err(error) => Err(error),
    };
    match outcome {
        Ok(response) => {
            write_bridge_response(client, request_path, response_path, &response, true).await?;
        }
        Err(error) => {
            eprintln!(
                "[paperclip] sandbox callback bridge handler failed for {}: {error}",
                request.id
            );
            write_bridge_response(
                client,
                request_path,
                response_path,
                &handler_failure_bridge_response(request.id.clone(), &error, scb::now_rfc3339()),
                true,
            )
            .await?;
        }
    }
    client.remove(request_path).await?;
    Ok(())
}

/// 写响应文件（对齐 Node `writeBridgeResponse`：优先 writeResponseFile
/// 直写，否则 temp + rename）。
async fn write_bridge_response(
    client: &Arc<dyn BridgeQueueClient>,
    request_path: &str,
    response_path: &str,
    response: &SandboxCallbackBridgeResponse,
    require_request_path: bool,
) -> Result<(), String> {
    match decide_bridge_response_write(
        response_path,
        Some(request_path),
        true,
        require_request_path,
        response,
    ) {
        BridgeResponseWritePlan::Direct {
            response_path,
            request_path,
            body,
        } => {
            client
                .write_response_file(&response_path, &body, request_path.as_deref())
                .await?;
        }
        BridgeResponseWritePlan::ViaTemp {
            temp_path,
            response_path,
            body,
        } => {
            client.write_text_file(&temp_path, &body).await?;
            client.rename(&temp_path, &response_path).await?;
        }
    }
    Ok(())
}

/// 给未决请求补写 503（对齐 Node `failPendingRequests`）。
async fn fail_pending_requests(
    client: &Arc<dyn BridgeQueueClient>,
    queue_dir: &str,
    message: &str,
) -> Result<(), String> {
    let directories = scb::sandbox_callback_bridge_directories(queue_dir);
    let file_names = client.list_json_files(&directories.requests_dir).await?;
    for file_name in file_names {
        let request_path = scb::posix_join(&directories.requests_dir, &file_name);
        let response_path = scb::posix_join(&directories.responses_dir, &file_name);
        let request_id = scb::bridge_request_id_from_file_name(&file_name)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let request_id = match client.read_text_file(&request_path).await {
            Ok(raw) => parse_bridge_request_file(&raw)
                .ok()
                .map(|request| request.id)
                .unwrap_or(request_id),
            Err(_) => request_id,
        };
        let _ = client.remove(&request_path).await;
        let response = pending_request_failure_bridge_response(
            request_id.clone(),
            message,
            scb::now_rfc3339(),
        );
        if let Err(error) =
            write_bridge_response(client, &request_path, &response_path, &response, false).await
        {
            eprintln!(
                "[paperclip] sandbox callback bridge failed to abort pending request {request_id}: {error}"
            );
        }
        let _ = client.remove(&request_path).await;
    }
    Ok(())
}

// =============================================================================
// host API 转发 handler（对齐 Node startAdapterExecutionTargetPaperclipBridge
// 内的 fetch 转发）
// =============================================================================

/// 把 sandbox bridge 请求转发到 host Paperclip API 的 handler。
pub struct BridgeForwardHandler {
    pub host_api_url: String,
    pub host_api_token: String,
    pub run_id: String,
    pub max_body_bytes: u64,
    client: reqwest::Client,
}

impl BridgeForwardHandler {
    #[must_use]
    pub fn new(
        host_api_url: impl Into<String>,
        host_api_token: impl Into<String>,
        run_id: impl Into<String>,
        max_body_bytes: u64,
    ) -> Self {
        Self {
            host_api_url: host_api_url.into(),
            host_api_token: host_api_token.into(),
            run_id: run_id.into(),
            max_body_bytes,
            client: reqwest::Client::new(),
        }
    }

    /// 转发单个请求（对齐 Node L1802-1834：
    /// 30s 超时、授权头 + x-paperclip-run-id、GET/HEAD 无 body、
    /// 响应 headers 透传 + body 限额）。
    pub async fn handle(
        &self,
        request: SandboxCallbackBridgeRequest,
    ) -> Result<BridgeHandlerResult, String> {
        let url = build_bridge_forward_url(&self.host_api_url, &request.path, &request.query);
        let method = if request.method.trim().is_empty() {
            "GET"
        } else {
            request.method.trim()
        };
        let mut builder = self
            .client
            .request(
                reqwest::Method::from_bytes(method.to_uppercase().as_bytes())
                    .map_err(|error| format!("invalid bridge request method: {error}"))?,
                &url,
            )
            .timeout(std::time::Duration::from_secs(30));
        for (key, value) in sanitize_sandbox_callback_bridge_headers(&request.headers, None) {
            if !value.trim().is_empty() {
                builder = builder.header(&key, &value);
            }
        }
        builder = builder.header("authorization", format!("Bearer {}", self.host_api_token));
        builder = builder.header("x-paperclip-run-id", &self.run_id);
        if method != "GET" && method != "HEAD" {
            builder = builder.body(request.body);
        }
        let response = builder
            .send()
            .await
            .map_err(|error| format!("bridge forward request failed: {error}"))?;
        let status = response.status().as_u16();
        let headers: BTreeMap<String, String> = response
            .headers()
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let passthrough = build_bridge_response_headers(&headers);
        let content_length = headers
            .get("content-length")
            .and_then(|value| value.parse::<u64>().ok());
        bridge_response_body_within_limit(content_length, self.max_body_bytes)?;
        let body = response
            .text()
            .await
            .map_err(|error| format!("bridge forward response read failed: {error}"))?;
        bridge_response_body_utf8_len_within_limit(&body, self.max_body_bytes)?;
        Ok(BridgeHandlerResult {
            status,
            headers: passthrough,
            body,
        })
    }
}

/// 把 handler 包装成 worker 需要的 `BridgeHandleRequestFn`。
#[must_use]
pub fn wrap_forward_handler(handler: Arc<BridgeForwardHandler>) -> BridgeHandleRequestFn {
    Arc::new(move |request: SandboxCallbackBridgeRequest| {
        let handler = Arc::clone(&handler);
        Box::pin(async move { handler.handle(request).await })
    })
}

// =============================================================================
// 顶层编排（对齐 Node execution-target.ts L1719-1930）
// =============================================================================

/// [`start_adapter_execution_target_paperclip_bridge`] 输入。
pub struct StartAdapterBridgeInput<'a> {
    pub run_id: &'a str,
    pub target: Option<&'a AdapterExecutionTarget>,
    pub runtime_root_dir: Option<&'a str>,
    pub adapter_key: &'a str,
    pub timeout_sec: Option<f64>,
    pub host_api_token: Option<&'a str>,
    pub host_api_url: Option<&'a str>,
    pub runner: Arc<dyn BridgeCommandRunner>,
    /// 启动日志回调（`[paperclip] Starting sandbox callback bridge ...`）。
    pub on_log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

/// 已启动的 bridge（env + server + worker + asset，供 teardown）。
pub struct StartedAdapterBridge {
    pub env: BTreeMap<String, String>,
    pub server: StartedBridgeServer,
    pub worker: BridgeWorkerHandle,
    pub bridge_runtime_dir: String,
    pub has_run_log_tail: bool,
    /// Sandbox-only: run log tail factory for streaming CLI output during
    /// the run (mirrors Node `AdapterExecutionTargetPaperclipBridgeHandle.runLogTail`).
    pub run_log_tail: Option<Arc<SandboxRunLogTailFactory>>,
    asset_dir: PathBuf,
}

impl StartedAdapterBridge {
    /// 停止 bridge（对齐 Node handle.stop：
    /// 先停 server，再停 worker + 清理 asset；全部 settle）。
    pub async fn stop(&self) {
        let _ = self.server.stop().await;
        self.worker.stop().await;
        let _ = std::fs::remove_dir_all(&self.asset_dir);
    }
}

/// 启动 paperclip bridge（对齐 Node
/// `startAdapterExecutionTargetPaperclipBridge` L1719-1930）：
/// 非远程 → `Ok(None)`；token 缺失 → Err；否则 asset → worker → server →
/// env 组装。
pub async fn start_adapter_execution_target_paperclip_bridge(
    input: &StartAdapterBridgeInput<'_>,
) -> Result<Option<StartedAdapterBridge>, String> {
    if !adapter_execution_target_uses_paperclip_bridge(input.target) {
        return Ok(None);
    }
    let target = input
        .target
        .ok_or("paperclip bridge requires a remote execution target")?;
    let host_api_token = input
        .host_api_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            "Sandbox bridge mode requires a host-side Paperclip API token.".to_string()
        })?;
    let remote_cwd = adapter_execution_target_remote_cwd(Some(target), "");
    let runtime_root_dir = match input
        .runtime_root_dir
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(dir) => dir.to_string(),
        None => format!("{remote_cwd}/.paperclip-runtime/{}", input.adapter_key),
    };
    let paths = bridge_handle_paths(&runtime_root_dir);
    let bridge_token = create_sandbox_callback_bridge_token(None);
    let max_body_bytes = resolve_bridge_max_body_bytes(None);
    let host_api_url = input
        .host_api_url
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var("PAPERCLIP_RUNTIME_API_URL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            std::env::var("PAPERCLIP_API_URL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "http://localhost:3100".to_string());
    let timeout_ms = resolve_bridge_timeout_ms(input.timeout_sec, Some(target))
        .unwrap_or(DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS);

    if let Some(on_log) = &input.on_log {
        on_log(&format!(
            "[paperclip] Starting sandbox callback bridge for {} in {}.\n",
            input.adapter_key, paths.bridge_runtime_dir
        ));
    }

    let source = get_sandbox_callback_bridge_server_source();
    let asset = BridgeAsset::create(&source)
        .map_err(|error| format!("create sandbox callback bridge asset failed: {error}"))?;

    let client: Arc<dyn BridgeQueueClient> = Arc::new(RunnerBridgeQueueClient::new(
        input.runner.clone(),
        remote_cwd.clone(),
        timeout_ms,
    ));
    let forward_handler = Arc::new(BridgeForwardHandler::new(
        host_api_url.clone(),
        host_api_token.to_string(),
        input.run_id.to_string(),
        max_body_bytes,
    ));
    let worker = match start_bridge_worker(StartBridgeWorkerInput {
        client: client.clone(),
        queue_dir: paths.queue_dir.clone(),
        poll_interval_ms: None,
        max_body_bytes: Some(max_body_bytes),
        authorize: None,
        handle_request: wrap_forward_handler(forward_handler),
    })
    .await
    {
        Ok(worker) => worker,
        Err(error) => {
            asset.cleanup();
            return Err(error);
        }
    };

    let server = match start_bridge_server(&StartBridgeServerInput {
        runner: input.runner.clone(),
        remote_cwd: &remote_cwd,
        asset_remote_dir: &paths.asset_remote_dir,
        queue_dir: &paths.queue_dir,
        bridge_token: &bridge_token,
        bridge_asset: Some(&asset),
        host: None,
        port: None,
        poll_interval_ms: None,
        response_timeout_ms: None,
        timeout_ms: Some(timeout_ms),
        node_command: None,
        shell: None,
        max_queue_depth: None,
        max_body_bytes: Some(max_body_bytes),
    })
    .await
    {
        Ok(server) => server,
        Err(error) => {
            worker.stop().await;
            asset.cleanup();
            return Err(error);
        }
    };

    let mut env = BTreeMap::new();
    env.insert("PAPERCLIP_API_URL".to_string(), server.base_url.clone());
    env.insert("PAPERCLIP_API_KEY".to_string(), bridge_token);
    env.insert(
        "PAPERCLIP_API_BRIDGE_MODE".to_string(),
        "queue_v1".to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_QUEUE_DIR".to_string(),
        paths.queue_dir.clone(),
    );
    let has_run_log_tail = matches!(
        target,
        AdapterExecutionTarget::Remote(
            crate::execution_target::AdapterRemoteExecutionTarget::Sandbox(sandbox)
        ) if sandbox.stream_run_logs != Some(false)
    );
    let run_log_tail = if let AdapterExecutionTarget::Remote(
        crate::execution_target::AdapterRemoteExecutionTarget::Sandbox(sandbox),
    ) = target
    {
        if sandbox.stream_run_logs != Some(false) {
            // Mirror Node L1848-1866: create the tail factory from the
            // sandbox runner + logs dir + shell, then emit the enabled
            // log line through on_log.
            let logs_dir = format!("{}/logs", paths.queue_dir);
            let shell_command = if sandbox.shell_command.as_deref() == Some("bash") {
                Some("bash")
            } else {
                None
            };
            // Adapt any `BridgeCommandRunner` to the `SandboxRunLogRunner`
            // shape via [`pc_acpx::sandbox_run_log_stream::adapt_bridge_runner`]
            // (avoiding Arc trait-object coercion between independent traits).
            let tail_runner =
                crate::sandbox_run_log_stream::adapt_bridge_runner(Arc::clone(&input.runner));
            let factory = create_sandbox_run_log_tail_factory(SandboxRunLogTailFactoryOptions {
                runner: tail_runner,
                remote_cwd: sandbox.remote_cwd.clone(),
                logs_dir,
                shell_command,
                poll_interval_ms: None,
                max_chunk_bytes_per_tick: None,
                tick_timeout_ms: None,
                max_consecutive_failures: None,
            });
            if let Some(on_log) = &input.on_log {
                on_log("[paperclip] Sandbox run log streaming enabled for this run.\n");
            }
            Some(Arc::new(factory))
        } else {
            None
        }
    } else {
        None
    };
    Ok(Some(StartedAdapterBridge {
        env,
        server,
        worker,
        bridge_runtime_dir: paths.bridge_runtime_dir,
        has_run_log_tail,
        run_log_tail,
        asset_dir: asset.local_dir,
    }))
}

// =============================================================================
// R492 — adapter 执行流程接入（选 runner 启动 bridge）
// =============================================================================

/// [`start_adapter_execution_bridge_for_target`] 输入。
pub struct StartAdapterBridgeForTargetInput<'a> {
    pub run_id: &'a str,
    pub target: Option<&'a AdapterExecutionTarget>,
    pub runtime_root_dir: Option<&'a str>,
    pub adapter_key: &'a str,
    pub timeout_sec: Option<f64>,
    pub host_api_token: Option<&'a str>,
    pub host_api_url: Option<&'a str>,
    pub on_log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

/// 为 adapter execution target 启动真实 bridge（对齐 Node codex/claude
/// execute.ts 的 `startAdapterExecutionTargetPaperclipBridge` 接入点）：
///
/// - 本地 / 非 bridge target → `Ok(None)`
/// - 远程 SSH target → 用真实 SSH runner（[`crate::ssh`]）启动完整
///   bridge（asset → worker → server → env），返回
///   [`StartedAdapterBridge`] 供调用方 teardown
/// - 远程 Sandbox target → `Ok(None)`：provider runner 未在 Rust 侧实现，
///   调用方保持 R490 的 env-only 合并（bridge env 4 键仍注入子进程，
///   但无真实 server/worker）
pub async fn start_adapter_execution_bridge_for_target(
    input: &StartAdapterBridgeForTargetInput<'_>,
) -> Result<Option<StartedAdapterBridge>, String> {
    let Some(target) = input.target else {
        return Ok(None);
    };
    if !adapter_execution_target_uses_paperclip_bridge(Some(target)) {
        return Ok(None);
    }
    let Some(ssh) = target.as_ssh() else {
        // Sandbox provider runner 未实现：保持 env-only 合并。
        return Ok(None);
    };
    let runner: Arc<dyn BridgeCommandRunner> = Arc::new(
        crate::ssh::SshCommandManagedRuntimeRunner::new(ssh.spec.clone(), None, None),
    );
    start_adapter_execution_target_paperclip_bridge(&StartAdapterBridgeInput {
        run_id: input.run_id,
        target: Some(target),
        runtime_root_dir: input.runtime_root_dir,
        adapter_key: input.adapter_key,
        timeout_sec: input.timeout_sec,
        host_api_token: input.host_api_token,
        host_api_url: input.host_api_url,
        runner,
        on_log: input.on_log.clone(),
    })
    .await
}
