//! R493 — 进程 session bridge 真实执行器。
//!
//! 对齐 Node `execution-target.ts` L1360-1578
//! （`startAdapterExecutionTargetProcessSessionBridge`）：
//! - sandbox 远程 target 门控（Node：transport !== "sandbox" → null）
//! - 远端脚本 sha 门控同步（`syncProcessSessionRemoteScript`，复用 R484
//!   的 hash-skip 上传决策脚本）
//! - 启动执行（`mkdir -p stdin events` + `nohup node remote ... &` + pid）
//! - 本地 TCP server（127.0.0.1 随机端口）+ proxy 脚本（0700）
//! - 连接处理（5s 鉴权超时、token 校验、stdin/stdinEnd → 远端 stdin 文件）
//! - 事件轮询（100ms：events 目录 → socket 直写 / 缓冲，exit/error 停止）
//! - `stop`：停轮询 → destroy 连接 → server 关闭 → 补写 stdinEnd →
//!   清理 session 目录 + 本地 proxy 目录
//!
//! 设计边界：执行器与 transport 无关（runner 注入，与 paperclip bridge
//! 一致）；sandbox-only 门控保留在入口（对齐 Node）。当前 Rust 侧没有
//! sandbox provider runner，execute 接入点保持关闭；provider runner 落地
//! 后此执行器直接可用。

use crate::bridge_executor::{
    require_successful_result, run_shell, BridgeCommandRunner, BridgeQueueClient,
    RunnerBridgeQueueClient,
};
use crate::execution_target::{
    build_proxy_stop_stdin_end_write, build_proxy_stdin_write, decide_proxy_connection_message,
    decide_proxy_poll_should_stop, decide_remote_event_delivery, parse_proxy_message_line,
    get_process_session_remote_source, process_session_listen_port_or_error,
    proxy_error_message_line, start_adapter_execution_target_process_session_bridge_plan,
    sync_process_session_remote_script_plan, AdapterExecutionTarget,
    AdapterRemoteExecutionTarget, PROCESS_SESSION_AUTH_TIMEOUT_MS,
    PROCESS_SESSION_PROXY_SCRIPT,
};
use crate::sandbox_callback_bridge::{
    base64_encode_utf8, parse_sync_text_file_result, preferred_shell_for_sandbox,
    DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// 进程 session bridge 的共享状态（连接任务 / 轮询任务 / stop 共用）。
struct ProcessSessionBridgeState {
    client: Arc<dyn BridgeQueueClient>,
    stdin_dir: String,
    session_dir: String,
    events_dir: String,
    proxy_token: String,
    stdin_seq: AtomicU64,
    stopping: AtomicBool,
    /// 活跃（已鉴权）连接的写半部：输出事件从这里下发。
    write_half: Mutex<Option<tokio::net::tcp::OwnedWriteHalf>>,
    /// 已鉴权连接被接管前的缓冲事件行（含未写出的 exit/error 行）。
    pending: std::sync::Mutex<Vec<String>>,
    /// stop 时唤醒所有连接任务退出（对齐 Node stop 的 liveSockets destroy）。
    shutdown_notify: tokio::sync::Notify,
    proxy_dir: PathBuf,
}

impl ProcessSessionBridgeState {
    /// 投递一个远端事件（对齐 Node `deliverRemoteEvent`）：
    /// 有活跃 socket → 直写；否则入缓冲；exit/error 停止后续轮询。
    async fn deliver(&self, event: &serde_json::Value) -> bool {
        let event_type = event.get("type").and_then(|t| t.as_str());
        let has_socket = self.write_half.lock().await.is_some();
        let line = crate::execution_target::json_line(event);
        match decide_remote_event_delivery(has_socket, event_type) {
            crate::execution_target::RemoteEventDeliveryDecision::WriteToSocket { action } => {
                let mut guard = self.write_half.lock().await;
                if let Some(write_half) = guard.as_mut() {
                    let _ = write_half.write_all(line.as_bytes()).await;
                    let _ = write_half.flush().await;
                    if !matches!(
                        action,
                        crate::execution_target::RemoteEventSocketAction::Write
                    ) {
                        let _ = write_half.shutdown().await;
                        return true;
                    }
                    return false;
                }
                // socket 刚断开：退回缓冲（对齐 writeRemoteEventToSocket
                // 返回 false 时 Node 仍 push pending 的行为）。
                self.pending.lock().expect("pending lock").push(line);
                decide_proxy_poll_should_stop(event_type)
            }
            crate::execution_target::RemoteEventDeliveryDecision::QueuePending { stop_loop } => {
                self.pending.lock().expect("pending lock").push(line);
                stop_loop
            }
        }
    }

    /// flush 缓冲事件到已接管 socket（对齐 Node `flushPendingRemoteEvents`）。
    async fn flush_pending(&self) {
        let mut guard = self.write_half.lock().await;
        let Some(write_half) = guard.as_mut() else {
            return;
        };
        let lines: Vec<String> = std::mem::take(&mut *self.pending.lock().expect("pending lock"));
        for line in &lines {
            let _ = write_half.write_all(line.as_bytes()).await;
            let _ = write_half.flush().await;
            // 缓冲里的 exit/error 行：结束 socket（对齐 writeRemoteEventToSocket）。
            if line.contains("\"type\":\"exit\"") || line.contains("\"type\":\"error\"") {
                let _ = write_half.shutdown().await;
                break;
            }
        }
    }
}

/// 已启动的进程 session bridge handle（对齐 Node
/// `AdapterExecutionTargetProcessSessionBridgeHandle`：`{ agentCommand, stop }`）。
pub struct ProcessSessionBridgeHandle {
    pub agent_command: String,
    state: Arc<ProcessSessionBridgeState>,
    server_task: tokio::task::JoinHandle<()>,
    poll_task: tokio::task::JoinHandle<()>,
}

impl ProcessSessionBridgeHandle {
    /// 停止 bridge（对齐 Node handle.stop）：
    /// stopping → 停轮询 + server → destroy 连接 → 补写 stdinEnd →
    /// 移除远端 session 目录 → 清理本地 proxy 目录。全部 best-effort。
    pub async fn stop(&self) {
        self.state.stopping.store(true, Ordering::SeqCst);
        self.poll_task.abort();
        self.server_task.abort();
        // 唤醒所有连接任务退出（对齐 Node stop 的 liveSockets destroy）。
        self.state.shutdown_notify.notify_waiters();
        if let Some(write_half) = self.state.write_half.lock().await.as_mut() {
            let _ = write_half.shutdown().await;
        }
        // 补写 stdinEnd（对齐 Node `${stdinSeq + 1}.json`）。
        let seq = self.state.stdin_seq.load(Ordering::SeqCst);
        let write = build_proxy_stop_stdin_end_write(seq);
        let _ = self
            .state
            .client
            .write_text_file(
                &format!("{}/{}", self.state.stdin_dir, write.file_name),
                &write.body,
            )
            .await;
        let _ = self.state.client.remove(&self.state.session_dir).await;
        let _ = std::fs::remove_dir_all(&self.state.proxy_dir);
    }
}

/// [`start_adapter_execution_target_process_session_bridge`] 输入
/// （对齐 Node input：target / runtimeRootDir / adapterKey / command /
/// args / cwd / launch env / timeoutSec / runner / onLog）。
pub struct StartProcessSessionBridgeInput<'a> {
    pub run_id: &'a str,
    pub target: Option<&'a AdapterExecutionTarget>,
    pub runtime_root_dir: Option<&'a str>,
    pub adapter_key: &'a str,
    pub command: &'a str,
    pub args: &'a [String],
    pub cwd: &'a str,
    /// 启动 env（对齐 Node 的 launch env；Rust 侧调用方在 paperclip
    /// bridge env 合并完成后传入，等价于 Node 的 env thunk 求值结果）。
    pub launch_env: &'a BTreeMap<String, String>,
    pub timeout_sec: Option<f64>,
    pub runner: Arc<dyn BridgeCommandRunner>,
    pub on_log: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

/// 启动进程 session bridge（对齐 Node
/// `startAdapterExecutionTargetProcessSessionBridge` L1360-1578）。
///
/// 仅 sandbox 远程 target 返回 `Ok(Some(...))`（Node gate：
/// `kind !== "remote" || transport !== "sandbox"` → null）。
pub async fn start_adapter_execution_target_process_session_bridge(
    input: &StartProcessSessionBridgeInput<'_>,
) -> Result<Option<ProcessSessionBridgeHandle>, String> {
    if !matches!(
        input.target,
        Some(AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(_)))
    ) {
        return Ok(None);
    }
    let target = input
        .target
        .ok_or("process session bridge requires a remote sandbox execution target")?;
    let sandbox = match target {
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(s)) => s,
        _ => unreachable!("gate above"),
    };
    let remote_cwd = sandbox.remote_cwd.trim_end_matches('/').to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let plan = start_adapter_execution_target_process_session_bridge_plan(
        &session_id,
        Some(target),
        input.runtime_root_dir,
        input.adapter_key,
        input.command,
        input.args,
        input.cwd,
        input.launch_env,
        input.timeout_sec,
    )
    .expect("sandbox target ⇒ plan present");
    let timeout_ms = plan
        .timeout_ms
        .unwrap_or(DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS);
    let shell = preferred_shell_for_sandbox(None);

    // 1. 远端脚本 sha 门控同步（对齐 Node syncProcessSessionRemoteScript：
    // 内容 hash 相同则跳过 base64 上传）。
    let sync_plan = sync_process_session_remote_script_plan(
        &plan.bridge_runtime_dir,
        &plan.remote_script_path,
    );
    let sync_result = run_shell(
        &input.runner,
        &remote_cwd,
        &sync_plan.uploaded_decision_script,
        shell,
        Some(base64_encode_utf8(&get_process_session_remote_source())),
        BTreeMap::new(),
        timeout_ms,
    )
    .await?;
    require_successful_result(&sync_plan.action, &sync_result)?;
    parse_sync_text_file_result(&sync_result.stdout, &sync_plan.label)?;

    // 2. 启动日志 + 启动执行（mkdir + nohup node + pid）。
    if let Some(on_log) = &input.on_log {
        on_log(&format!(
            "[paperclip] Starting ACP process session bridge in sandbox ({}).\n",
            sandbox.provider_key.as_deref().unwrap_or("provider")
        ));
    }
    let start_result = run_shell(
        &input.runner,
        &remote_cwd,
        &plan.start_script,
        shell,
        None,
        BTreeMap::new(),
        timeout_ms,
    )
    .await?;
    if start_result.timed_out || start_result.exit_code != Some(0) {
        let detail = if !start_result.stderr.trim().is_empty() {
            start_result.stderr.trim().to_string()
        } else {
            start_result.stdout.trim().to_string()
        };
        return Err(format!(
            "Failed to start sandbox ACP process session bridge: {detail}"
        ));
    }

    // 3. 本地 TCP server（127.0.0.1 随机端口）。
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("process session bridge listen failed: {error}"))?;
    let port = process_session_listen_port_or_error(
        Some(listener.local_addr().map_err(|error| error.to_string())?.port()),
    )?;

    // 4. proxy 脚本（本地 mkdtemp + 0700，对齐 writeProcessSessionProxyScript）。
    let proxy_dir = std::env::temp_dir().join(format!(
        "paperclip-process-session-proxy-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&proxy_dir)
        .map_err(|error| format!("create proxy dir failed: {error}"))?;
    let proxy_path = proxy_dir.join(PROCESS_SESSION_PROXY_SCRIPT);
    {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let write_result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&proxy_path)?;
            file.write_all(
                crate::execution_target::get_process_session_proxy_source(
                    port,
                    &plan.proxy_token,
                )
                .as_bytes(),
            )?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_dir_all(&proxy_dir);
            return Err(format!("write process session proxy script failed: {error}"));
        }
        let _ = std::fs::set_permissions(&proxy_path, std::fs::Permissions::from_mode(0o700));
    }

    // 5. 队列客户端（stdin 写 / events 读都走 runner）。
    let client: Arc<dyn BridgeQueueClient> = Arc::new(RunnerBridgeQueueClient::new(
        input.runner.clone(),
        remote_cwd.clone(),
        timeout_ms,
    ));
    let state = Arc::new(ProcessSessionBridgeState {
        client: client.clone(),
        stdin_dir: plan.stdin_dir.clone(),
        session_dir: plan.session_dir.clone(),
        events_dir: plan.events_dir.clone(),
        proxy_token: plan.proxy_token.clone(),
        stdin_seq: AtomicU64::new(0),
        stopping: AtomicBool::new(false),
        write_half: Mutex::new(None),
        pending: std::sync::Mutex::new(Vec::new()),
        shutdown_notify: tokio::sync::Notify::new(),
        proxy_dir: proxy_dir.clone(),
    });

    // 6. server 任务：接受连接 → 连接处理。
    let server_state = Arc::clone(&state);
    let server_task = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            let connection_state = Arc::clone(&server_state);
            tokio::spawn(async move {
                handle_connection(connection_state, socket).await;
            });
        }
    });

    // 7. 轮询任务：100ms 读 events 目录 → 投递。
    let poll_state = Arc::clone(&state);
    let poll_on_log = input.on_log.clone();
    let poll_task = tokio::spawn(async move {
        let mut stop_loop = false;
        while !poll_state.stopping.load(Ordering::SeqCst) && !stop_loop {
            tokio::time::sleep(Duration::from_millis(
                crate::execution_target::PROXY_POLL_INTERVAL_MS,
            ))
            .await;
            match poll_events(&poll_state).await {
                Ok(stop) => stop_loop = stop,
                Err(message) => {
                    if let Some(on_log) = &poll_on_log {
                        on_log(&format!(
                            "[paperclip] ACP process session bridge poll failed: {message}\n"
                        ));
                    }
                    let _ = poll_state
                        .deliver(&serde_json::json!({ "type": "error", "message": message }))
                        .await;
                    stop_loop = true;
                }
            }
        }
    });

    Ok(Some(ProcessSessionBridgeHandle {
        agent_command: proxy_path.to_string_lossy().into_owned(),
        state,
        server_task,
        poll_task,
    }))
}

/// 单连接处理（对齐 Node connection handler L1479-1565）：
/// - 5s 鉴权超时（未鉴权空闲连接 destroy）
/// - 每行 JSON 解析失败 → destroy
/// - token 不匹配 / 已有活跃 socket 抢占 → destroy
/// - 首次鉴权成功 → 接管为活跃 socket + flush 缓冲事件
/// - `stdin` / `stdinEnd` → 写远端 stdin 文件（`{seq:012}.json`）
async fn handle_connection(state: Arc<ProcessSessionBridgeState>, socket: TcpStream) {
    let (mut read_half, write_half) = socket.into_split();
    // 本连接是否已接管为活跃 socket（Authenticate 时 write_half 移入 state）。
    let active_marker = Arc::new(AtomicBool::new(false));
    let mut authenticated = false;
    let mut buffer = String::new();
    let mut buf = [0u8; 8192];
    let mut my_write_half = Some(write_half);
    loop {
        // 未鉴权连接受 5s 超时约束（对齐 Node authTimer）。
        let read_result = if !authenticated {
            match tokio::time::timeout(
                Duration::from_millis(PROCESS_SESSION_AUTH_TIMEOUT_MS),
                read_half.read(&mut buf),
            )
            .await
            {
                Err(_) => break,
                Ok(result) => result,
            }
        } else {
            tokio::select! {
                result = read_half.read(&mut buf) => result,
                _ = state.shutdown_notify.notified() => break,
            }
        };
        let Ok(n) = read_result else {
            break;
        };
        if n == 0 {
            break;
        }
        buffer.push_str(&String::from_utf8_lossy(&buf[..n]));
        while let Some(idx) = buffer.find('\n') {
            let line = buffer[..idx].trim_end_matches('\r').trim().to_string();
            buffer.drain(..=idx);
            if line.is_empty() {
                continue;
            }
            let message = match parse_proxy_message_line(&line) {
                Ok(value) => value,
                Err(_) => {
                    // 对齐 Node：坏 JSON → destroy。
                    my_write_half.take();
                    break;
                }
            };
            let message_token = message.get("token").and_then(|t| t.as_str());
            let has_active_socket = state.write_half.lock().await.is_some();
            let decision = decide_proxy_connection_message(
                message_token,
                &state.proxy_token,
                authenticated,
                has_active_socket,
            );
            match decision {
                crate::execution_target::ProxyConnectionDecision::Reject => {
                    // 对齐 Node：token 不匹配 / 抢占 → destroy。
                    my_write_half.take();
                    break;
                }
                crate::execution_target::ProxyConnectionDecision::Authenticate => {
                    authenticated = true;
                    active_marker.store(true, Ordering::SeqCst);
                    let taken = my_write_half.take().expect("write half present");
                    *state.write_half.lock().await = Some(taken);
                    state.flush_pending().await;
                }
                crate::execution_target::ProxyConnectionDecision::Proceed => {}
            }
            // stdin / stdinEnd → 远端 stdin 文件。
            let message_type = message.get("type").and_then(|t| t.as_str());
            let data = message.get("data").and_then(|d| d.as_str());
            if let Some(write) = build_proxy_stdin_write(
                state.stdin_seq.fetch_add(1, Ordering::SeqCst) + 1,
                message_type,
                data,
            ) {
                if let Err(error) = state
                    .client
                    .write_text_file(
                        &format!("{}/{}", state.stdin_dir, write.file_name),
                        &write.body,
                    )
                    .await
                {
                    // 对齐 Node catch：写回 error 行并 destroy。
                    if let Some(mut guard) = state.write_half.lock().await.as_mut() {
                        let line = proxy_error_message_line(&error);
                        let _ = guard.write_all(line.as_bytes()).await;
                        let _ = guard.shutdown().await;
                    }
                    break;
                }
            }
        }
    }
    // 退出：活跃 socket 已被移入 state 时清空引用（对齐 Node socket close
    // 后不再写入）；未移入的写半部随 drop 关闭连接。
    if active_marker.load(Ordering::SeqCst) {
        *state.write_half.lock().await = None;
    }
}

/// 单轮事件轮询（对齐 Node `poll`）：
/// list events → 逐文件读 + remove → 投递；exit/error 后停止。
async fn poll_events(state: &ProcessSessionBridgeState) -> Result<bool, String> {
    let names = state.client.list_json_files(&state.events_dir).await?;
    let mut stop_loop = false;
    for name in names {
        if state.stopping.load(Ordering::SeqCst) {
            return Ok(true);
        }
        let file_path = format!("{}/{}", state.events_dir, name);
        let body = state.client.read_text_file(&file_path).await?;
        let _ = state.client.remove(&file_path).await;
        let event: serde_json::Value = serde_json::from_str(body.trim())
            .map_err(|error| format!("invalid event file {name}: {error}"))?;
        stop_loop = state.deliver(&event).await;
        if stop_loop {
            break;
        }
    }
    Ok(stop_loop)
}
