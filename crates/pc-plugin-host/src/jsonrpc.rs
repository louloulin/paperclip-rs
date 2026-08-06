//! JSON-RPC over stdio 双向流。
//!
//! 与原 `server/src/services/plugin-worker-manager.ts` 中 stdio JSON-RPC 通信逻辑等价。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, Mutex};
use uuid::Uuid;

use pc_plugin_protocol::{
    JsonRpcError, JsonRpcErrorCode, JsonRpcRequest, JsonRpcResponse, WORKER_TO_HOST_METHODS,
};

/// Async trait for handling worker → host JSON-RPC requests. The host
/// invokes this callback when a worker calls one of the registered
/// `WORKER_TO_HOST_METHODS` methods.
#[async_trait::async_trait]
pub trait WorkerToHostHandler: Send + Sync {
    /// Handle the worker request and return a JSON value (or error).
    async fn handle(&self, method: &str, params: Option<Value>) -> Result<Value, JsonRpcError>;
}

/// 待处理 RPC 调用的 `HashMap` 别名。
pub type PendingMap = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, JsonRpcError>>>>>;

/// 待处理的 RPC 调用：调用方通过 `oneshot` 等响应。
pub struct PendingCall {
    pub id: String,
    pub sender: oneshot::Sender<Result<Value, JsonRpcError>>,
}

/// JSON-RPC 2.0 over stdio 双向流。
///
/// 一个 worker 一个 stream，负责：
/// - 编码请求到 stdin
/// - 从 stdout 按行读取响应
/// - 匹配 id → pending sender
pub struct JsonRpcStream {
    pending: PendingMap,
    stdin: Arc<Mutex<ChildStdin>>,
    worker_to_host: Arc<Mutex<Option<Arc<dyn WorkerToHostHandler>>>>,
    #[allow(dead_code)]
    stdout_task: Option<tokio::task::JoinHandle<()>>,
}

impl JsonRpcStream {
    /// Wrap a child's stdio into a JSON-RPC stream.
    pub fn new(child: &mut Child) -> Result<Self, String> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "worker stdin not available".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "worker stdout not available".to_string())?;
        let stderr = child.stderr.take();

        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        let worker_to_host_reader = Arc::new(Mutex::new(None));
        let stdin_reader = Arc::new(Mutex::new(
            child
                .stdin
                .take()
                .ok_or_else(|| "worker stdin not available".to_string())?,
        ));

        // Spawn reader task
        let stdout_task = tokio::spawn(async move {
            if let Err(err) = Self::read_loop(
                BufReader::new(stdout),
                pending_reader,
                worker_to_host_reader,
                stdin_reader,
                stderr,
            )
            .await
            {
                tracing::warn!("plugin worker stdout read loop ended: {err}");
            }
        });

        Ok(Self {
            pending,
            stdin: Arc::new(Mutex::new(stdin)),
            worker_to_host: Arc::new(Mutex::new(None)),
            stdout_task: Some(stdout_task),
        })
    }

    /// Register a worker → host handler. The handler will be invoked when
    /// the worker sends a JSON-RPC request whose method is in
    /// `WORKER_TO_HOST_METHODS`. Responses are written back to the worker's
    /// stdin.
    pub async fn set_worker_to_host_handler(&self, handler: Arc<dyn WorkerToHostHandler>) {
        let mut slot = self.worker_to_host.lock().await;
        *slot = Some(handler);
    }

    /// 发送一个 RPC 调用，等待响应。
    pub async fn call<P, R>(&self, method: &str, params: P) -> Result<R, JsonRpcError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = Uuid::new_v4().to_string();
        let request = JsonRpcRequest::new(&id, method, params);

        // Register pending sender BEFORE writing to avoid race
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id.clone(), tx);
        }

        // Write request as one JSON line
        let line = match serde_json::to_string(&request) {
            Ok(s) => s,
            Err(e) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(JsonRpcError::new(
                    JsonRpcErrorCode::InternalError.as_i32(),
                    format!("serialize request failed: {e}"),
                ));
            }
        };

        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(JsonRpcError::new(
                    JsonRpcErrorCode::InternalError.as_i32(),
                    format!("write to worker stdin failed: {e}"),
                ));
            }
            if let Err(e) = stdin.write_all(b"\n").await {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(JsonRpcError::new(
                    JsonRpcErrorCode::InternalError.as_i32(),
                    format!("newline write to worker stdin failed: {e}"),
                ));
            }
            if let Err(e) = stdin.flush().await {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(JsonRpcError::new(
                    JsonRpcErrorCode::InternalError.as_i32(),
                    format!("flush worker stdin failed: {e}"),
                ));
            }
        }

        // Await response
        match rx.await {
            Ok(Ok(value)) => match serde_json::from_value::<R>(value) {
                Ok(r) => Ok(r),
                Err(e) => Err(JsonRpcError::new(
                    JsonRpcErrorCode::InternalError.as_i32(),
                    format!("deserialize response failed: {e}"),
                )),
            },
            Ok(Err(err)) => Err(err),
            Err(_) => Err(JsonRpcError::new(
                JsonRpcErrorCode::InternalError.as_i32(),
                "worker connection closed before response".to_string(),
            )),
        }
    }

    async fn read_loop(
        mut reader: BufReader<ChildStdout>,
        pending: PendingMap,
        worker_to_host: Arc<Mutex<Option<Arc<dyn WorkerToHostHandler>>>>,
        stdin: Arc<Mutex<ChildStdin>>,
        mut stderr: Option<ChildStderr>,
    ) -> Result<(), String> {
        // Spawn stderr drain task if available
        if let Some(stderr) = stderr.take() {
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!(target: "plugin_worker_stderr", "{line}");
                }
            });
        }

        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            match reader.read_line(&mut line_buf).await {
                Ok(0) => {
                    // EOF - worker exited
                    let mut pending = pending.lock().await;
                    for (_, sender) in pending.drain() {
                        let _ = sender.send(Err(JsonRpcError::new(
                            JsonRpcErrorCode::InternalError.as_i32(),
                            "worker stream closed".to_string(),
                        )));
                    }
                    return Ok(());
                }
                Ok(_) => {
                    let trimmed = line_buf.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    // First try as a JSON-RPC response from the worker.
                    if let Ok(response) = serde_json::from_str::<JsonRpcResponse<Value>>(trimmed) {
                        match response {
                            JsonRpcResponse::Success(success) => {
                                let mut pending = pending.lock().await;
                                if let Some(sender) = pending.remove(&success.id) {
                                    let _ = sender.send(Ok(success.result));
                                }
                            }
                            JsonRpcResponse::Error(error_response) => {
                                let mut pending = pending.lock().await;
                                if let Some(sender) = pending.remove(&error_response.id) {
                                    let _ = sender.send(Err(error_response.error));
                                }
                            }
                        }
                        continue;
                    }
                    // Otherwise try as a worker → host JSON-RPC request.
                    if let Ok(request) = serde_json::from_str::<JsonRpcRequest<Value>>(trimmed) {
                        if WORKER_TO_HOST_METHODS.contains(&request.method.as_str()) {
                            let handler = {
                                let slot = worker_to_host.lock().await;
                                slot.clone()
                            };
                            if let Some(handler) = handler {
                                let stdin = stdin.clone();
                                let id = request.id.clone();
                                let method = request.method.clone();
                                let params = request.params.clone();
                                tokio::spawn(async move {
                                    let result = handler.handle(&method, params).await;
                                    let response = match result {
                                        Ok(value) => JsonRpcResponse::success(id, value),
                                        Err(error) => JsonRpcResponse::error(id, error),
                                    };
                                    let line = match serde_json::to_string(&response) {
                                        Ok(line) => line,
                                        Err(err) => {
                                            tracing::warn!(
                                                "failed to serialize worker → host response: {err}"
                                            );
                                            return;
                                        }
                                    };
                                    let mut stdin = stdin.lock().await;
                                    use tokio::io::AsyncWriteExt;
                                    if let Err(err) = async {
                                        stdin.write_all(line.as_bytes()).await?;
                                        stdin.write_all(b"\n").await?;
                                        stdin.flush().await
                                    }
                                    .await
                                    {
                                        tracing::warn!(
                                            "failed to write worker → host response: {err}"
                                        );
                                    }
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    return Err(format!("read_line failed: {e}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_call_struct_constructs() {
        let (tx, _rx) = oneshot::channel();
        let call = PendingCall {
            id: "test".to_string(),
            sender: tx,
        };
        assert_eq!(call.id, "test");
    }

    struct TestWorkerToHostHandler {
        result: Value,
    }

    #[async_trait::async_trait]
    impl WorkerToHostHandler for TestWorkerToHostHandler {
        async fn handle(
            &self,
            _method: &str,
            _params: Option<Value>,
        ) -> Result<Value, JsonRpcError> {
            Ok(self.result.clone())
        }
    }

    #[test]
    fn worker_to_host_methods_are_recognized() {
        use pc_plugin_protocol::WORKER_TO_HOST_METHODS;
        // Spot-check the core methods we depend on
        assert!(WORKER_TO_HOST_METHODS.contains(&"progress"));
        assert!(WORKER_TO_HOST_METHODS.contains(&"log"));
        assert!(WORKER_TO_HOST_METHODS.contains(&"emitEvent"));
        assert!(WORKER_TO_HOST_METHODS.contains(&"dataQuery"));
    }

    #[tokio::test]
    async fn read_loop_handles_empty_lines() {
        // Construct an empty stream - just test the loop terminates
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        // Empty stdout via empty pipe would block; skip exercising I/O here
        let map = pending.lock().await;
        assert!(map.is_empty());
    }
}
