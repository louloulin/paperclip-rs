//! 单个 plugin worker 的 handle。

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;
use tracing::{debug, info, warn};
use uuid::Uuid;

use pc_plugin_protocol::{
    ConfigChangedParams, ExecuteToolParams, GetDataParams, HandleApiRequestParams,
    InitializeParams, InitializeResult, JsonRpcError, OnEventParams, PerformActionParams,
    PluginHealthDiagnostics, RunJobParams, ToolResult,
};

use crate::jsonrpc::JsonRpcStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    Starting,
    Ready,
    Busy,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Clone)]
pub struct WorkerOptions {
    pub plugin_id: Uuid,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub plugin_version: String,
    pub manifest_version: String,
    pub instance_id: Uuid,
    pub init_timeout: Duration,
}

pub struct WorkerHandle {
    pub plugin_id: Uuid,
    pub options: WorkerOptions,
    pub state: Arc<Mutex<WorkerState>>,
    stream: Arc<Mutex<Option<JsonRpcStream>>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl WorkerHandle {
    pub fn new(options: WorkerOptions) -> Self {
        Self {
            plugin_id: options.plugin_id,
            options,
            state: Arc::new(Mutex::new(WorkerState::Starting)),
            stream: Arc::new(Mutex::new(None)),
            child: Arc::new(Mutex::new(None)),
        }
    }

    /// Spawn the worker process and perform initialize handshake.
    pub async fn start(&self) -> Result<InitializeResult, String> {
        let mut command = Command::new(&self.options.command);
        command
            .args(&self.options.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(cwd) = &self.options.cwd {
            command.current_dir(cwd);
        }
        for (k, v) in &self.options.env {
            command.env(k, v);
        }

        let mut child = command
            .spawn()
            .map_err(|e| format!("failed to spawn worker `{}`: {e}", self.options.command))?;

        let stream = JsonRpcStream::new(&mut child)
            .map_err(|e| format!("failed to open JSON-RPC stream: {e}"))?;

        // Send initialize call
        let params = InitializeParams {
            plugin_id: self.options.plugin_id,
            plugin_version: self.options.plugin_version.clone(),
            manifest_version: self.options.manifest_version.clone(),
            instance_id: self.options.instance_id,
            runtime_config: Value::Null,
        };

        let init_future = stream.call::<_, InitializeResult>("initialize", params);
        let init_result = match timeout(self.options.init_timeout, init_future).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                let _ = child.kill().await;
                self.set_state(WorkerState::Failed).await;
                return Err(format!("initialize call failed: {e:?}"));
            }
            Err(_) => {
                let _ = child.kill().await;
                self.set_state(WorkerState::Failed).await;
                return Err("initialize call timed out".into());
            }
        };

        *self.stream.lock().await = Some(stream);
        *self.child.lock().await = Some(child);
        self.set_state(WorkerState::Ready).await;
        info!(plugin_id = %self.plugin_id, "plugin worker ready");
        Ok(init_result)
    }

    /// Helper: get the stream or return the not-ready error.
    async fn get_stream(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<JsonRpcStream>>, JsonRpcError> {
        let guard = self.stream.lock().await;
        if guard.is_none() {
            return Err(JsonRpcError::new(
                -32603,
                "worker not ready: stream not initialized".to_string(),
            ));
        }
        Ok(guard)
    }

    pub async fn health(&self) -> Result<PluginHealthDiagnostics, JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream
            .call::<_, PluginHealthDiagnostics>("health", serde_json::json!({}))
            .await
    }

    pub async fn validate_config(&self, config: Value) -> Result<Value, JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream
            .call::<_, Value>("validateConfig", serde_json::json!({ "config": config }))
            .await
    }

    pub async fn config_changed(&self, params: ConfigChangedParams) -> Result<(), JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream.call::<_, ()>("configChanged", params).await
    }

    pub async fn on_event(&self, params: OnEventParams) -> Result<(), JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream.call::<_, ()>("onEvent", params).await
    }

    pub async fn run_job(&self, params: RunJobParams) -> Result<(), JsonRpcError> {
        self.set_state(WorkerState::Busy).await;
        let res = {
            let guard = self.get_stream().await?;
            let stream = guard.as_ref().expect("checked above");
            stream.call::<_, ()>("runJob", params).await
        };
        self.set_state(WorkerState::Ready).await;
        res
    }

    pub async fn handle_api_request(
        &self,
        params: HandleApiRequestParams,
    ) -> Result<Value, JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream.call::<_, Value>("handleApiRequest", params).await
    }

    pub async fn get_data(&self, params: GetDataParams) -> Result<Value, JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream.call::<_, Value>("getData", params).await
    }

    pub async fn perform_action(&self, params: PerformActionParams) -> Result<Value, JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream.call::<_, Value>("performAction", params).await
    }

    pub async fn execute_tool(
        &self,
        params: ExecuteToolParams,
    ) -> Result<ToolResult, JsonRpcError> {
        let guard = self.get_stream().await?;
        let stream = guard.as_ref().expect("checked above");
        stream.call::<_, ToolResult>("executeTool", params).await
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        self.set_state(WorkerState::Stopping).await;

        // Try graceful shutdown RPC
        let shutdown_result = timeout(Duration::from_secs(5), async {
            let guard = self.stream.lock().await;
            if let Some(stream) = guard.as_ref() {
                stream
                    .call::<_, ()>("shutdown", serde_json::json!({}))
                    .await
            } else {
                Ok(())
            }
        })
        .await;

        match shutdown_result {
            Ok(Ok(())) => {
                debug!(plugin_id = %self.plugin_id, "worker shutdown ok");
            }
            Ok(Err(e)) => {
                warn!("worker graceful shutdown failed: {e:?}, will kill");
            }
            Err(_) => {
                warn!("worker shutdown timed out, will kill");
            }
        }

        // Drop stream and kill child
        *self.stream.lock().await = None;
        if let Some(mut child) = self.child.lock().await.take() {
            if let Err(e) = child.kill().await {
                warn!("failed to kill worker child: {e}");
            }
            let _ = child.wait().await;
        }
        self.set_state(WorkerState::Stopped).await;
        Ok(())
    }

    pub fn is_alive(&self) -> bool {
        match self.state.try_lock() {
            Ok(s) => matches!(*s, WorkerState::Ready | WorkerState::Busy),
            Err(_) => false,
        }
    }

    async fn set_state(&self, new_state: WorkerState) {
        let mut state = self.state.lock().await;
        *state = new_state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_options() -> WorkerOptions {
        WorkerOptions {
            plugin_id: Uuid::new_v4(),
            command: "/bin/echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: vec![],
            plugin_version: "1.0.0".into(),
            manifest_version: "v1".into(),
            instance_id: Uuid::new_v4(),
            init_timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn handle_starts_in_starting_state() {
        let h = WorkerHandle::new(test_options());
        let state = *h.state.lock().await;
        assert_eq!(state, WorkerState::Starting);
    }

    #[tokio::test]
    async fn handle_with_echo_process_fails_initialize() {
        let h = WorkerHandle::new(test_options());
        let result = h.start().await;
        assert!(result.is_err());
        let state = *h.state.lock().await;
        assert_eq!(state, WorkerState::Failed);
    }

    #[tokio::test]
    async fn is_alive_false_when_not_ready() {
        let h = WorkerHandle::new(test_options());
        assert!(!h.is_alive());
    }

    #[tokio::test]
    async fn shutdown_is_safe_even_when_never_started() {
        let h = WorkerHandle::new(test_options());
        let r = h.shutdown().await;
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn health_before_start_returns_error() {
        let h = WorkerHandle::new(test_options());
        let r = h.health().await;
        assert!(r.is_err());
    }
}
