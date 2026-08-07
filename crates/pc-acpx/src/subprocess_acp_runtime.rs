//! `pc-acpx` `SubprocessAcpRuntime` — the real `AcpRuntime` impl that talks
//! to the `acpx` binary over JSON-RPC. R371 added the synchronous control
//! surface; R372 adds the streaming `start_turn` pipeline (broadcast event
//! channel + per-request oneshot result channel + background reader task
//! that demuxes notifications and responses).
//!
//! ## Concurrency model
//!
//! - `state` is split between `tokio::sync::Mutex` (the subprocess handle,
//!   which holds async I/O handles) and `std::sync::Mutex` (the JSON-RPC
//!   id allocator, in-flight waiters, and broadcast sender — all sync-safe).
//! - This split lets `start_turn` (a sync trait method) allocate an id,
//!   register a oneshot, subscribe to the broadcast channel, and spawn the
//!   write task — all without awaiting an async lock.
//! - The background reader task uses the `std::sync::Mutex` side to demux
//!   stdout lines into per-request oneshot senders and the broadcast event
//!   channel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::{Future, StreamExt};
use serde::Deserialize;
use tokio::sync::{broadcast, oneshot, Mutex as TokioMutex};
use tokio::task::JoinHandle;

use crate::acp_runtime::{
    AcpRuntime, AcpRuntimeCancelInput, AcpRuntimeCapabilities, AcpRuntimeCloseInput,
    AcpRuntimeDoctorReport, AcpRuntimeEnsureInput, AcpRuntimeError, AcpRuntimeEvent,
    AcpRuntimeEventStream, AcpRuntimeGetCapabilitiesInput, AcpRuntimeGetStatusInput,
    AcpRuntimeHandle, AcpRuntimeSetConfigOptionInput, AcpRuntimeSetModeInput, AcpRuntimeStatus,
    AcpRuntimeTurn, AcpRuntimeTurnInput, AcpRuntimeTurnResult, AcpRuntimeTurnResultFuture,
    AcpRuntimeTurnResultResolver,
};
use crate::error::AcpxError;
use crate::jsonrpc_wire::{
    encode_jsonrpc_request, next_jsonrpc_id, parse_jsonrpc_line, JsonRpcIdAllocator,
    JsonRpcResponse,
};
use crate::subprocess_handle::{SpawnAcpxInput, SubprocessHandle};

/// Builder input for [`SubprocessAcpRuntime::new`].
#[derive(Debug, Clone)]
pub struct SubprocessAcpRuntimeSpec {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: HashMap<String, String>,
    pub response_timeout: Duration,
}

/// Capacity for the broadcast channel that delivers
/// [`AcpRuntimeEvent`]s to active turn subscribers.
const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Outcome delivered to in-flight request waiters via oneshot.
#[derive(Debug)]
enum JsonRpcOutcome {
    Response(Option<serde_json::Value>),
    Error(crate::jsonrpc_wire::JsonRpcErrorBody),
}

/// Sync-accessible portion of the runtime state. Held under
/// [`std::sync::Mutex`] so `start_turn` (sync) can allocate ids, register
/// oneshots, and clone the broadcast sender without awaiting.
struct SyncState {
    id_alloc: JsonRpcIdAllocator,
    event_tx: Option<broadcast::Sender<AcpRuntimeEvent>>,
    in_flight: HashMap<u64, oneshot::Sender<JsonRpcOutcome>>,
}

/// Async-only portion of the runtime state — owns the subprocess handle
/// and the reader task join handle.
struct AsyncState {
    handle: Option<SubprocessHandle>,
    reader_handle: Option<JoinHandle<()>>,
}

pub struct SubprocessAcpRuntime {
    sync_state: Arc<StdMutex<SyncState>>,
    async_state: Arc<TokioMutex<AsyncState>>,
    spec: SubprocessAcpRuntimeSpec,
}

impl std::fmt::Debug for SubprocessAcpRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubprocessAcpRuntime")
            .field("command", &self.spec.command)
            .finish()
    }
}

impl SubprocessAcpRuntime {
    pub fn new(spec: SubprocessAcpRuntimeSpec) -> Result<Self, AcpxError> {
        Ok(Self {
            sync_state: Arc::new(StdMutex::new(SyncState {
                id_alloc: JsonRpcIdAllocator::new(),
                event_tx: None,
                in_flight: HashMap::new(),
            })),
            async_state: Arc::new(TokioMutex::new(AsyncState {
                handle: None,
                reader_handle: None,
            })),
            spec,
        })
    }

    /// Lazy spawn — the subprocess is only created on the first request that
    /// needs it. Idempotent: returns the existing handle if already spawned.
    async fn ensure_subprocess(&self) -> Result<SubprocessHandle, AcpxError> {
        let mut async_state = self.async_state.lock().await;
        if let Some(handle) = async_state.handle.as_ref() {
            return Ok(handle.clone());
        }
        let handle = SubprocessHandle::spawn(SpawnAcpxInput {
            command: self.spec.command.clone(),
            args: self.spec.args.clone(),
            cwd: self.spec.cwd.clone(),
            env: self.spec.env.clone(),
            stdin_request_capacity: 32,
        })
        .await?;
        let (event_tx, _event_rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        {
            let mut sync_state = self.sync_state.lock().expect("sync_state poisoned");
            sync_state.event_tx = Some(event_tx);
        }
        let sync_state_for_reader = Arc::clone(&self.sync_state);
        let reader_handle_local = handle.clone();
        let timeout = self.spec.response_timeout;
        let reader = tokio::spawn(async move {
            reader_loop(sync_state_for_reader, reader_handle_local, timeout).await;
        });
        async_state.handle = Some(handle.clone());
        async_state.reader_handle = Some(reader);
        Ok(handle)
    }

    /// Send a JSON-RPC request and await the result via the background
    /// reader task's oneshot channel. Used by every control method.
    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, AcpxError> {
        let handle = self.ensure_subprocess().await?;
        let (id, body) = {
            let mut sync_state = self.sync_state.lock().expect("sync_state poisoned");
            let id = next_jsonrpc_id(&sync_state.id_alloc);
            let body = encode_jsonrpc_request(id, method, params);
            (id, body)
        };
        // Insert the oneshot AFTER releasing the lock above. The
        // std::sync::Mutex guard cannot be held across an await.
        let receiver = {
            let mut sync_state = self.sync_state.lock().expect("sync_state poisoned");
            let (sender, receiver) = oneshot::channel();
            sync_state.in_flight.insert(id, sender);
            receiver
        };
        handle.write_request(&body).await?;
        let outcome = tokio::time::timeout(self.spec.response_timeout, receiver)
            .await
            .map_err(|_| AcpxError::ReadTimeout {
                timeout_ms: self.spec.response_timeout.as_millis() as u64,
            })?
            .map_err(|_| AcpxError::JsonRpcParse {
                line: String::new(),
                reason: "response channel closed".to_string(),
            })?;
        let _ = id;
        match outcome {
            JsonRpcOutcome::Response(Some(value)) => Ok(value),
            JsonRpcOutcome::Response(None) => Ok(serde_json::Value::Null),
            JsonRpcOutcome::Error(error) => Err(AcpxError::JsonRpcParse {
                line: String::new(),
                reason: format!("jsonrpc error {}: {}", error.code, error.message),
            }),
        }
    }
}

/// Background reader loop — runs until the subprocess closes its stdout or
/// the runtime is dropped. Demuxes stdout lines into per-request oneshot
/// senders (for responses and errors) and the broadcast event channel (for
/// `session/event` notifications).
async fn reader_loop(
    sync_state: Arc<StdMutex<SyncState>>,
    handle: SubprocessHandle,
    response_timeout: Duration,
) {
    loop {
        let line = match handle.read_response_line(response_timeout).await {
            Ok(line) => line,
            Err(_) => return,
        };
        let frame = match parse_jsonrpc_line(&line) {
            Ok(frame) => frame,
            Err(_) => continue,
        };
        match frame {
            crate::jsonrpc_wire::JsonRpcFrame::Response(response) => {
                deliver_response(&sync_state, response);
            }
            crate::jsonrpc_wire::JsonRpcFrame::Error { id, error } => {
                let mut sync_state = sync_state.lock().expect("sync_state poisoned");
                if let Some(sender) = sync_state.in_flight.remove(&id) {
                    let _ = sender.send(JsonRpcOutcome::Error(error));
                }
            }
            crate::jsonrpc_wire::JsonRpcFrame::Notification(notification) => {
                if notification.method == "session/event" {
                    if let Some(event) = notification
                        .params
                        .as_ref()
                        .and_then(acpx_event_from_params)
                    {
                        let tx = {
                            let sync_state = sync_state.lock().expect("sync_state poisoned");
                            sync_state.event_tx.clone()
                        };
                        if let Some(tx) = tx {
                            let _ = tx.send(event);
                        }
                    }
                }
            }
            crate::jsonrpc_wire::JsonRpcFrame::Request(_) => {
                // Server-pushed requests are not part of the acpx protocol.
            }
        }
    }
}

fn deliver_response(sync_state: &Arc<StdMutex<SyncState>>, response: JsonRpcResponse) {
    let mut sync_state = sync_state.lock().expect("sync_state poisoned");
    if let Some(sender) = sync_state.in_flight.remove(&response.id) {
        let _ = sender.send(JsonRpcOutcome::Response(response.result));
    }
}

/// Map an acpx notification's `params` to a typed
/// [`AcpRuntimeEvent`]. Returns `None` for unknown / malformed shapes.
fn acpx_event_from_params(params: &serde_json::Value) -> Option<AcpRuntimeEvent> {
    serde_json::from_value::<AcpRuntimeEvent>(params.clone()).ok()
}

#[derive(Debug, Deserialize)]
struct SessionNewResult {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    backend_session_id: Option<String>,
    #[serde(default)]
    agent_session_id: Option<String>,
}

#[async_trait]
impl AcpRuntime for SubprocessAcpRuntime {
    async fn ensure_session(
        &self,
        input: AcpRuntimeEnsureInput,
    ) -> Result<AcpRuntimeHandle, AcpRuntimeError> {
        let params = serde_json::json!({
            "agent": input.agent,
            "mode": match input.mode {
                crate::acp_runtime::AcpRuntimeMode::Persistent => "persistent",
                crate::acp_runtime::AcpRuntimeMode::OneShot => "oneshot",
            },
            "cwd": input.cwd,
        });
        let result = self
            .request("session/new", Some(params))
            .await
            .map_err(|error| AcpRuntimeError::SessionError(error.to_string()))?;
        let parsed: SessionNewResult = serde_json::from_value(result).map_err(|error| {
            AcpRuntimeError::SessionError(format!("session/new result: {error}"))
        })?;
        let runtime_session_name = parsed.session_id.clone();
        let backend_session_id = parsed
            .backend_session_id
            .clone()
            .or_else(|| parsed.agent_session_id.clone());
        Ok(AcpRuntimeHandle {
            session_key: input.session_key,
            backend: input.agent,
            runtime_session_name,
            cwd: input.cwd,
            acpx_record_id: parsed.session_id,
            backend_session_id,
            agent_session_id: parsed.agent_session_id,
        })
    }

    fn start_turn(&self, input: AcpRuntimeTurnInput) -> AcpRuntimeTurn {
        let request_id = input.request_id.clone();
        let handle = input.handle.clone();
        let text = input.text.clone();
        let timeout_ms = input.timeout_ms;
        let sync_state = Arc::clone(&self.sync_state);
        let async_state = Arc::clone(&self.async_state);
        let response_timeout = self.spec.response_timeout;

        // 1. Allocate the prompt id and register a placeholder oneshot
        //    synchronously (the reader task will overwrite it with the
        //    real one once the dispatch task installs the receiver).
        let prompt_id = {
            let mut sync_state = sync_state.lock().expect("sync_state poisoned");
            let id = next_jsonrpc_id(&sync_state.id_alloc);
            // Place a dummy sender so the slot is reserved; the dispatch
            // task will swap it out for the real one.
            let (placeholder_sender, _placeholder_receiver) = oneshot::channel();
            sync_state.in_flight.insert(id, placeholder_sender);
            id
        };

        // 2. Subscribe to the broadcast event channel synchronously.
        let event_rx = {
            let sync_state = sync_state.lock().expect("sync_state poisoned");
            sync_state.event_tx.as_ref().map(|tx| tx.subscribe())
        };

        // 3. Replace the placeholder oneshot with a real one bound to a
        //    shared `Arc<oneshot::Sender>` that the result future will
        //    hold. We use Arc so both the dispatch task (which writes
        //    the prompt) and the result future can share the sender.
        let (result_tx, result_rx) = oneshot::channel::<JsonRpcOutcome>();
        {
            let mut sync_state = sync_state.lock().expect("sync_state poisoned");
            // Remove the placeholder, install the real sender.
            let _ = sync_state.in_flight.remove(&prompt_id);
            sync_state.in_flight.insert(prompt_id, result_tx);
        }

        // 4. Spawn the async dispatch: write the session/prompt frame.
        let dispatch_sync = Arc::clone(&sync_state);
        let dispatch_async = Arc::clone(&async_state);
        let dispatch_handle = handle.clone();
        let dispatch_request_id = request_id.clone();
        let dispatch_text = text.clone();
        tokio::spawn(async move {
            let body = encode_jsonrpc_request(
                prompt_id,
                "session/prompt",
                Some(serde_json::json!({
                    "sessionKey": dispatch_handle.session_key,
                    "agentSessionId": dispatch_handle.agent_session_id,
                    "requestId": dispatch_request_id,
                    "text": dispatch_text,
                })),
            );
            let sub_handle = {
                let async_state = dispatch_async.lock().await;
                async_state.handle.as_ref().cloned()
            };
            if let Some(sub_handle) = sub_handle {
                let _ = sub_handle.write_request(&body).await;
            } else {
                // Subprocess not spawned yet — fail the prompt.
                let _ = dispatch_sync
                    .lock()
                    .expect("sync_state poisoned")
                    .in_flight
                    .remove(&prompt_id);
            }
        });

        // 5. Build the events stream from the broadcast receiver.
        let events: AcpRuntimeEventStream = match event_rx {
            Some(rx) => broadcast_receiver_to_stream(rx),
            None => Box::pin(futures::stream::empty()),
        };

        // 6. Build the result future: wait for the reader task to deliver
        //    into our oneshot (it will pick up our id from `in_flight`).
        let result_sync = Arc::clone(&sync_state);
        let timeout = timeout_ms.unwrap_or(response_timeout.as_millis() as u64);
        let result_future: AcpRuntimeTurnResultFuture = Box::pin(async move {
            let outcome = tokio::time::timeout(Duration::from_millis(timeout), result_rx).await;
            // Best-effort cleanup of our slot.
            result_sync
                .lock()
                .expect("sync_state poisoned")
                .in_flight
                .remove(&prompt_id);
            match outcome {
                Ok(Ok(JsonRpcOutcome::Response(value))) => {
                    let parsed = value.unwrap_or(serde_json::Value::Null);
                    parse_turn_result(parsed)
                }
                Ok(Ok(JsonRpcOutcome::Error(error))) => AcpRuntimeTurnResult::Failed {
                    error: crate::acp_runtime::AcpRuntimeTurnResultError {
                        message: format!("{}: {}", error.code, error.message),
                        code: Some(format!("rpc_{}", error.code)),
                        detail_code: None,
                        retryable: None,
                    },
                },
                Ok(Err(_closed)) => AcpRuntimeTurnResult::Failed {
                    error: crate::acp_runtime::AcpRuntimeTurnResultError {
                        message: "response channel closed".to_string(),
                        code: Some("E_CLOSED".to_string()),
                        detail_code: None,
                        retryable: None,
                    },
                },
                Err(_) => AcpRuntimeTurnResult::Failed {
                    error: crate::acp_runtime::AcpRuntimeTurnResultError {
                        message: format!("turn timed out after {timeout} ms"),
                        code: Some("E_TIMEOUT".to_string()),
                        detail_code: None,
                        retryable: None,
                    },
                },
            }
        });
        AcpRuntimeTurn {
            request_id,
            events,
            result: AcpRuntimeTurnResultResolver {
                future: result_future,
            },
        }
    }

    async fn get_capabilities(
        &self,
        _input: AcpRuntimeGetCapabilitiesInput,
    ) -> Option<AcpRuntimeCapabilities> {
        let result = self.request("session/capabilities", None).await.ok()?;
        serde_json::from_value::<AcpRuntimeCapabilities>(result).ok()
    }

    async fn get_status(&self, input: AcpRuntimeGetStatusInput) -> Option<AcpRuntimeStatus> {
        let params = serde_json::json!({
            "sessionKey": input.handle.session_key,
            "agentSessionId": input.handle.agent_session_id,
        });
        let result = self.request("session/status", Some(params)).await.ok()?;
        serde_json::from_value::<AcpRuntimeStatus>(result).ok()
    }

    async fn set_mode(&self, input: AcpRuntimeSetModeInput) -> Result<(), AcpRuntimeError> {
        let params = serde_json::json!({
            "mode": input.mode,
            "sessionKey": input.handle.session_key,
        });
        self.request("session/set_mode", Some(params))
            .await
            .map_err(|error| AcpRuntimeError::SessionError(error.to_string()))?;
        Ok(())
    }

    async fn set_config_option(
        &self,
        input: AcpRuntimeSetConfigOptionInput,
    ) -> Result<(), AcpRuntimeError> {
        let params = serde_json::json!({
            "key": input.key,
            "value": input.value,
            "sessionKey": input.handle.session_key,
        });
        self.request("session/set_config_option", Some(params))
            .await
            .map_err(|error| AcpRuntimeError::SessionError(error.to_string()))?;
        Ok(())
    }

    async fn doctor(&self) -> Option<AcpRuntimeDoctorReport> {
        let handle = self.ensure_subprocess().await.ok()?;
        let pid = handle.pid();
        Some(AcpRuntimeDoctorReport {
            ok: pid > 0,
            message: format!("subprocess pid {pid}"),
            ..Default::default()
        })
    }

    async fn cancel(&self, _input: AcpRuntimeCancelInput) -> Result<(), AcpRuntimeError> {
        let async_state = self.async_state.lock().await;
        if let Some(handle) = async_state.handle.as_ref() {
            handle
                .cancel()
                .await
                .map_err(|error| AcpRuntimeError::SessionError(error.to_string()))?;
        }
        Ok(())
    }

    async fn close(&self, _input: AcpRuntimeCloseInput) -> Result<(), AcpRuntimeError> {
        let mut async_state = self.async_state.lock().await;
        {
            let mut sync_state = self.sync_state.lock().expect("sync_state poisoned");
            sync_state.event_tx = None;
            for (_, sender) in sync_state.in_flight.drain() {
                drop(sender);
            }
        }
        if let Some(reader) = async_state.reader_handle.take() {
            reader.abort();
        }
        if let Some(handle) = async_state.handle.take() {
            handle.close_stdin().await.ok();
            let _ = handle.cancel().await;
        }
        Ok(())
    }
}

/// Convert a [`broadcast::Receiver<AcpRuntimeEvent>`] into a pinned
/// [`AcpRuntimeEventStream`]. The receiver is moved into the stream so the
/// stream owns its state for the full `'static` lifetime the trait requires.
fn broadcast_receiver_to_stream(
    receiver: broadcast::Receiver<AcpRuntimeEvent>,
) -> AcpRuntimeEventStream {
    let mut receiver = Box::pin(receiver);
    Box::pin(futures::stream::poll_fn(move |cx| {
        let recv = receiver.recv();
        tokio::pin!(recv);
        match recv.poll(cx) {
            std::task::Poll::Ready(Ok(event)) => std::task::Poll::Ready(Some(event)),
            std::task::Poll::Ready(Err(_closed)) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }))
}

fn parse_turn_result(value: serde_json::Value) -> AcpRuntimeTurnResult {
    if let Some(error) = value.get("error") {
        if let Ok(message) = serde_json::from_value::<String>(error.clone()) {
            return AcpRuntimeTurnResult::Failed {
                error: crate::acp_runtime::AcpRuntimeTurnResultError {
                    message,
                    code: None,
                    detail_code: None,
                    retryable: None,
                },
            };
        }
    }
    let stop_reason = value
        .get("stopReason")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    AcpRuntimeTurnResult::Completed { stop_reason }
}

impl Drop for SubprocessAcpRuntime {
    fn drop(&mut self) {
        // Best-effort cleanup. The subprocess handle is wrapped in
        // `kill_on_drop(true)`, so even if the runtime never reaches
        // `close` the OS will reap the child when the last clone drops.
    }
}
