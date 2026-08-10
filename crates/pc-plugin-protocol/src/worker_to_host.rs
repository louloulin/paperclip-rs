//! Worker → Host typed JSON-RPC method schemas.
//!
//! 与原 `@paperclipai/plugin-sdk` 的 `protocol.ts` 中 worker → host 方法
//! 一一对应。每个方法给出 typed `Params` / `Result` 结构体，以及一个
//! 可复用的 typed dispatcher `dispatch_worker_to_host_request`。
//!
//! 当前模块只覆盖 schema 与解析/编码逻辑；真正路由由
//! `pc_plugin_host::jsonrpc::TypedWorkerToHostHandler` 解释。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::envelope::{JsonRpcError, JsonRpcErrorCode};
use crate::methods::worker_to_host;

/// JSON-RPC 错误：worker 传入的 params 不能被反序列化为目标类型时
/// 返回 `-32602 Invalid params`，对齐 JSON-RPC 2.0 规范。
fn invalid_params(message: impl Into<String>) -> JsonRpcError {
    JsonRpcError::new(JsonRpcErrorCode::InvalidParams.as_i32(), message)
}

/// 工具：从 `Value` 提取 typed `Params`，失败时返回 invalid params 错误。
pub fn parse_params<T: for<'de> Deserialize<'de>>(
    params: Option<Value>,
) -> Result<T, JsonRpcError> {
    let value = params.unwrap_or(Value::Null);
    serde_json::from_value::<T>(value).map_err(|err| {
        invalid_params(format!("invalid params for worker → host request: {err}"))
    })
}

/// 当目标类型 `T` 的所有字段都是 `Option`（例如 `GetStateParams`）时，
/// worker 调用 `method()` 不传 params / 传 `null` 也应被接受。
/// 返回一个空 `Object` 喂给 serde，使 typed 解析成功。
#[must_use]
pub fn params_or_empty_object(params: Option<Value>) -> Value {
    match params {
        Some(value) if !value.is_null() => value,
        _ => Value::Object(serde_json::Map::new()),
    }
}

/// progress 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProgressParams {
    pub run_id: String,
    pub percent: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
}

/// progress 方法结果：worker 通知 host，host 不需要回传业务值。
/// 这里用 `{}` 占位，保证 typed 调用一致。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProgressResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
}

/// log 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LogParams {
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub fields: Value,
}

/// log 方法结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct LogResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
}

/// emitEvent 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EmitEventParams {
    pub event: String,
    pub resource: String,
    pub resource_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct EmitEventResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered: Option<bool>,
}

/// getState 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetStateParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// getState 方法结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GetStateResult {
    #[serde(default)]
    pub value: Value,
}

/// setState 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SetStateParams {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetStateResult {
    pub stored: bool,
}

/// dataQuery 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DataQueryParams {
    pub key: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DataQueryResult {
    #[serde(default)]
    pub rows: Value,
}

/// dataMutate 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DataMutateParams {
    pub key: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DataMutateResult {
    #[serde(default)]
    pub result: Value,
}

/// toolInvoke 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvokeParams {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvokeResult {
    #[serde(default)]
    pub result: Value,
}

/// activityLog 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ActivityLogParams {
    pub kind: String,
    pub message: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityLogResult {
    pub accepted: bool,
}

/// notify 方法参数（通用通知）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NotifyParams {
    pub topic: String,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct NotifyResult {
    pub accepted: bool,
}

/// Worker → host 调用的统一 typed dispatcher。
///
/// 引入这个 trait 是为了把 `pc-plugin-host` 端的 `Value → String match`
/// 路由转换成 typed params，避免方法名/参数对不上时只在运行期才发现。
/// 任何实现都可以 `match` `method` 后转 typed `Params`。
pub trait WorkerToHostHandler: Send + Sync {
    /// 处理一个 typed worker → host 方法。
    fn handle(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, JsonRpcError>;
}

impl<T> WorkerToHostHandler for T
where
    T: WorkerToHostDispatcher + Send + Sync,
{
    fn handle(&self, method: &str, params: Option<Value>) -> Result<Value, JsonRpcError> {
        match method {
            worker_to_host::PROGRESS => {
                let p = parse_params::<ProgressParams>(params)?;
                let r = WorkerToHostDispatcher::on_progress(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::LOG => {
                let p = parse_params::<LogParams>(params)?;
                let r = WorkerToHostDispatcher::on_log(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::EMIT_EVENT => {
                let p = parse_params::<EmitEventParams>(params)?;
                let r = WorkerToHostDispatcher::on_emit_event(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::GET_STATE => {
                let p = parse_params::<GetStateParams>(Some(params_or_empty_object(params)))?;
                let r = WorkerToHostDispatcher::on_get_state(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::SET_STATE => {
                let p = parse_params::<SetStateParams>(params)?;
                let r = WorkerToHostDispatcher::on_set_state(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::DATA_QUERY => {
                let p = parse_params::<DataQueryParams>(params)?;
                let r = WorkerToHostDispatcher::on_data_query(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::DATA_MUTATE => {
                let p = parse_params::<DataMutateParams>(params)?;
                let r = WorkerToHostDispatcher::on_data_mutate(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::TOOL_INVOKE => {
                let p = parse_params::<ToolInvokeParams>(params)?;
                let r = WorkerToHostDispatcher::on_tool_invoke(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::ACTIVITY_LOG => {
                let p = parse_params::<ActivityLogParams>(params)?;
                let r = WorkerToHostDispatcher::on_activity_log(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            worker_to_host::NOTIFY => {
                let p = parse_params::<NotifyParams>(params)?;
                let r = WorkerToHostDispatcher::on_notify(self, p);
                serde_json::to_value(r).map_err(JsonRpcError::from)
            }
            _ => Err(JsonRpcError::new(
                JsonRpcErrorCode::MethodNotFound.as_i32(),
                format!("worker → host method `{method}` is not registered"),
            )),
        }
    }
}

impl From<serde_json::Error> for JsonRpcError {
    fn from(err: serde_json::Error) -> Self {
        JsonRpcError::new(
            JsonRpcErrorCode::InternalError.as_i32(),
            format!("serialize worker → host response: {err}"),
        )
    }
}

/// Worker → host typed handler 入口，每个方法一个 default impl，
/// 避免破坏性升级。
#[allow(unused_variables)]
pub trait WorkerToHostDispatcher: Send + Sync {
    fn on_progress(&self, params: ProgressParams) -> ProgressResult {
        ProgressResult::default()
    }
    fn on_log(&self, params: LogParams) -> LogResult {
        LogResult::default()
    }
    fn on_emit_event(&self, params: EmitEventParams) -> EmitEventResult {
        EmitEventResult::default()
    }
    fn on_get_state(&self, params: GetStateParams) -> GetStateResult {
        GetStateResult::default()
    }
    fn on_set_state(&self, params: SetStateParams) -> SetStateResult {
        SetStateResult { stored: true }
    }
    fn on_data_query(&self, params: DataQueryParams) -> DataQueryResult {
        DataQueryResult::default()
    }
    fn on_data_mutate(&self, params: DataMutateParams) -> DataMutateResult {
        DataMutateResult::default()
    }
    fn on_tool_invoke(&self, params: ToolInvokeParams) -> ToolInvokeResult {
        ToolInvokeResult::default()
    }
    fn on_activity_log(&self, params: ActivityLogParams) -> ActivityLogResult {
        ActivityLogResult { accepted: true }
    }
    fn on_notify(&self, params: NotifyParams) -> NotifyResult {
        NotifyResult { accepted: true }
    }
}

/// 暴露给外部的 typed 调度 helper：给定 `method` + `params` + handler
/// 派发到对应 typed 入口，等价于 `WorkerToHostHandler::handle` 但 typed。
pub fn dispatch_worker_to_host_request<H: WorkerToHostDispatcher>(
    method: &str,
    params: Option<Value>,
    handler: &H,
) -> Result<Value, JsonRpcError> {
    WorkerToHostHandler::handle(handler, method, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::methods::worker_to_host as m;
    use serde_json::json;
    use std::sync::Mutex;

    /// 收集所有 10 个方法调用的测试 handler。
    #[derive(Default)]
    struct CollectingHandler {
        progress: Mutex<Option<ProgressParams>>,
        log: Mutex<Option<LogParams>>,
        emit_event: Mutex<Option<EmitEventParams>>,
        get_state: Mutex<Option<GetStateParams>>,
        set_state: Mutex<Option<SetStateParams>>,
        data_query: Mutex<Option<DataQueryParams>>,
        data_mutate: Mutex<Option<DataMutateParams>>,
        tool_invoke: Mutex<Option<ToolInvokeParams>>,
        activity_log: Mutex<Option<ActivityLogParams>>,
        notify: Mutex<Option<NotifyParams>>,
    }

    impl WorkerToHostDispatcher for CollectingHandler {
        fn on_progress(&self, params: ProgressParams) -> ProgressResult {
            *self.progress.lock().unwrap() = Some(params);
            ProgressResult { accepted: Some(true) }
        }
        fn on_log(&self, params: LogParams) -> LogResult {
            *self.log.lock().unwrap() = Some(params);
            LogResult { accepted: Some(true) }
        }
        fn on_emit_event(&self, params: EmitEventParams) -> EmitEventResult {
            *self.emit_event.lock().unwrap() = Some(params);
            EmitEventResult { delivered: Some(true) }
        }
        fn on_get_state(&self, params: GetStateParams) -> GetStateResult {
            *self.get_state.lock().unwrap() = Some(params);
            GetStateResult { value: json!({"k":"v"}) }
        }
        fn on_set_state(&self, params: SetStateParams) -> SetStateResult {
            *self.set_state.lock().unwrap() = Some(params);
            SetStateResult { stored: true }
        }
        fn on_data_query(&self, params: DataQueryParams) -> DataQueryResult {
            *self.data_query.lock().unwrap() = Some(params);
            DataQueryResult { rows: json!([1, 2, 3]) }
        }
        fn on_data_mutate(&self, params: DataMutateParams) -> DataMutateResult {
            *self.data_mutate.lock().unwrap() = Some(params);
            DataMutateResult { result: json!({"ok": true}) }
        }
        fn on_tool_invoke(&self, params: ToolInvokeParams) -> ToolInvokeResult {
            *self.tool_invoke.lock().unwrap() = Some(params);
            ToolInvokeResult { result: json!("done") }
        }
        fn on_activity_log(&self, params: ActivityLogParams) -> ActivityLogResult {
            *self.activity_log.lock().unwrap() = Some(params);
            ActivityLogResult { accepted: true }
        }
        fn on_notify(&self, params: NotifyParams) -> NotifyResult {
            *self.notify.lock().unwrap() = Some(params);
            NotifyResult { accepted: true }
        }
    }

    #[test]
    fn r563_dispatch_progress_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({
            "runId": "run-1",
            "percent": 42.0,
            "message": "working"
        });
        let value = dispatch_worker_to_host_request(m::PROGRESS, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "accepted": true }));
        let captured = h.progress.lock().unwrap().clone().unwrap();
        assert_eq!(captured.run_id, "run-1");
        assert_eq!(captured.percent, 42.0);
        assert_eq!(captured.message.as_deref(), Some("working"));
    }

    #[test]
    fn r563_dispatch_log_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "level": "info", "message": "hi", "fields": {"k": 1} });
        let value = dispatch_worker_to_host_request(m::LOG, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "accepted": true }));
        let captured = h.log.lock().unwrap().clone().unwrap();
        assert_eq!(captured.level, "info");
        assert_eq!(captured.message, "hi");
        assert_eq!(captured.fields, json!({"k": 1}));
    }

    #[test]
    fn r563_dispatch_emit_event_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({
            "event": "issue.created",
            "resource": "issue",
            "resourceId": "00000000-0000-0000-0000-000000000001",
            "data": {"k": "v"}
        });
        let value = dispatch_worker_to_host_request(m::EMIT_EVENT, Some(params.clone()), &h).unwrap();
        assert_eq!(value, json!({ "delivered": true }));
        let captured = h.emit_event.lock().unwrap().clone().unwrap();
        assert_eq!(captured.event, "issue.created");
        assert_eq!(captured.resource_id, "00000000-0000-0000-0000-000000000001");
    }

    #[test]
    fn r563_dispatch_get_state_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "key": "resume" });
        let value = dispatch_worker_to_host_request(m::GET_STATE, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "value": { "k": "v" } }));
        let captured = h.get_state.lock().unwrap().clone().unwrap();
        assert_eq!(captured.key.as_deref(), Some("resume"));
    }

    #[test]
    fn r563_dispatch_set_state_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "key": "counter", "value": 7 });
        let value = dispatch_worker_to_host_request(m::SET_STATE, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "stored": true }));
        let captured = h.set_state.lock().unwrap().clone().unwrap();
        assert_eq!(captured.key, "counter");
        assert_eq!(captured.value, json!(7));
    }

    #[test]
    fn r563_dispatch_data_query_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "key": "issues", "params": {"status": "open"} });
        let value = dispatch_worker_to_host_request(m::DATA_QUERY, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "rows": [1, 2, 3] }));
        let captured = h.data_query.lock().unwrap().clone().unwrap();
        assert_eq!(captured.key, "issues");
        assert_eq!(captured.params, json!({"status": "open"}));
    }

    #[test]
    fn r563_dispatch_data_mutate_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "key": "issues", "params": {"id": 1} });
        let value = dispatch_worker_to_host_request(m::DATA_MUTATE, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "result": { "ok": true } }));
        let captured = h.data_mutate.lock().unwrap().clone().unwrap();
        assert_eq!(captured.key, "issues");
    }

    #[test]
    fn r563_dispatch_tool_invoke_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "tool": "shell", "args": ["ls"] });
        let value = dispatch_worker_to_host_request(m::TOOL_INVOKE, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "result": "done" }));
        let captured = h.tool_invoke.lock().unwrap().clone().unwrap();
        assert_eq!(captured.tool, "shell");
        assert_eq!(captured.args, json!(["ls"]));
    }

    #[test]
    fn r563_dispatch_activity_log_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "kind": "deploy", "message": "done", "data": {"x": 1} });
        let value = dispatch_worker_to_host_request(m::ACTIVITY_LOG, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "accepted": true }));
        let captured = h.activity_log.lock().unwrap().clone().unwrap();
        assert_eq!(captured.kind, "deploy");
    }

    #[test]
    fn r563_dispatch_notify_routes_to_typed_handler() {
        let h = CollectingHandler::default();
        let params = json!({ "topic": "agent.paused", "data": {"reason": "user"} });
        let value = dispatch_worker_to_host_request(m::NOTIFY, Some(params), &h).unwrap();
        assert_eq!(value, json!({ "accepted": true }));
        let captured = h.notify.lock().unwrap().clone().unwrap();
        assert_eq!(captured.topic, "agent.paused");
    }

    #[test]
    fn r563_unknown_method_returns_method_not_found() {
        let h = CollectingHandler::default();
        let err = dispatch_worker_to_host_request("nope", Some(json!({})), &h).unwrap_err();
        assert_eq!(err.code, JsonRpcErrorCode::MethodNotFound.as_i32());
    }

    #[test]
    fn r563_invalid_params_returns_invalid_params_error() {
        let h = CollectingHandler::default();
        let err = dispatch_worker_to_host_request(m::PROGRESS, Some(json!("not-an-object")), &h).unwrap_err();
        assert_eq!(err.code, JsonRpcErrorCode::InvalidParams.as_i32());
    }

    #[test]
    fn r563_missing_params_defaults_to_null_for_optional() {
        let h = CollectingHandler::default();
        // get_state 全部字段为 Option，缺省视为 null 也能解析
        let value = dispatch_worker_to_host_request(m::GET_STATE, None, &h).unwrap();
        assert_eq!(value, json!({ "value": { "k": "v" } }));
    }
}
