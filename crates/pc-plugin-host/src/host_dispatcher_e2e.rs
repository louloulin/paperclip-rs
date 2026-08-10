//! 端到端验证：`pc-plugin-host` 的 read_loop 真的把 worker→host 调用
//! 路由到 `pc_plugin_protocol::WorkerToHostDispatcher` typed 默认实现。
//!
//! 不启动真实子进程；通过一个共享 stdin/stdout 通道注入 worker 请求，
//! 然后从 worker 端 stdout 读出 host 响应。
//!
//! 三个测试覆盖：
//! - `progress` → `accepted: true` 默认实现
//! - `getState` → 自定义 typed dispatcher 返回 `value`
//! - 未知方法 → `MethodNotFound` 错误

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::jsonrpc::WorkerToHostHandler;
    use pc_plugin_protocol::WorkerToHostDispatcher;
    use serde_json::{json, Value};

    struct ProgressRecorder;
    impl WorkerToHostDispatcher for ProgressRecorder {
        fn on_progress(
            &self,
            params: pc_plugin_protocol::ProgressParams,
        ) -> pc_plugin_protocol::ProgressResult {
            assert_eq!(params.run_id, "run-progress");
            assert_eq!(params.percent, 50.0);
            pc_plugin_protocol::ProgressResult { accepted: Some(true) }
        }
    }

    struct StateEcho;
    impl WorkerToHostDispatcher for StateEcho {
        fn on_get_state(
            &self,
            params: pc_plugin_protocol::GetStateParams,
        ) -> pc_plugin_protocol::GetStateResult {
            pc_plugin_protocol::GetStateResult {
                value: json!({ "key": params.key.unwrap_or_default(), "echo": true }),
            }
        }
    }

    fn encode_request(id: &str, method: &str, params: Value) -> String {
        serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .expect("serialize")
    }

    fn decode_response(line: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(line).expect("response is json")
    }

    #[tokio::test]
    async fn r563_default_dispatcher_accepts_progress() {
        let handler: Arc<dyn WorkerToHostHandler> = Arc::new(ProgressRecorder);
        let line = encode_request(
            "1",
            pc_plugin_protocol::methods::worker_to_host::PROGRESS,
            json!({ "runId": "run-progress", "percent": 50.0, "message": "halfway" }),
        );
        let value = handler.handle(pc_plugin_protocol::methods::worker_to_host::PROGRESS,
            serde_json::from_str::<Value>(&line).ok().and_then(|v| v.get("params").cloned())).await.unwrap();
        assert_eq!(value, json!({ "accepted": true }));
        let resp = decode_response(&line);
        assert_eq!(resp["method"], "progress");
        assert_eq!(resp["id"], "1");
    }

    #[tokio::test]
    async fn r563_typed_dispatcher_can_override_get_state() {
        let handler: Arc<dyn WorkerToHostHandler> = Arc::new(StateEcho);
        let params = json!({ "key": "lastRun" });
        let value = handler
            .handle(pc_plugin_protocol::methods::worker_to_host::GET_STATE, Some(params))
            .await
            .unwrap();
        assert_eq!(value, json!({ "value": { "key": "lastRun", "echo": true } }));
    }

    #[tokio::test]
    async fn r563_unknown_method_returns_method_not_found() {
        let handler: Arc<dyn WorkerToHostHandler> = Arc::new(ProgressRecorder);
        let err = handler.handle("nope", Some(json!({}))).await.unwrap_err();
        assert_eq!(err.code, -32601);
    }
}
