//! RPC 方法参数与结果类型。
//!
//! 与原 `@paperclipai/plugin-sdk` 的 `protocol.ts` 中方法参数类型对应。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Plugin 事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEvent {
    pub event: String,
    pub resource: String,
    pub resource_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// initialize 方法参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub plugin_id: Uuid,
    pub plugin_version: String,
    pub manifest_version: String,
    pub instance_id: Uuid,
    pub runtime_config: Value,
}

/// initialize 方法结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub capabilities: Vec<String>,
    pub manifest_version: String,
}

/// 插件 health 信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHealthDiagnostics {
    pub status: String,
    #[serde(default)]
    pub checks: Vec<PluginHealthCheckEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHealthCheckEntry {
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// runJob 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunJobParams {
    pub job_key: String,
    pub run_id: Uuid,
    pub context: PluginJobContext,
}

/// 作业运行上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginJobContext {
    pub company_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_run_id: Option<Uuid>,
    pub config: Value,
    #[serde(default)]
    pub secrets: Value,
    #[serde(default)]
    pub metadata: Value,
}

/// onEvent 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnEventParams {
    pub event: PluginEvent,
}

/// configChanged 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChangedParams {
    pub config: Value,
}

/// validateConfig 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateConfigParams {
    pub config: Value,
}

/// getData 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetDataParams {
    pub key: String,
    #[serde(default)]
    pub params: Value,
    pub company_id: Uuid,
}

/// performAction 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformActionParams {
    pub action: String,
    #[serde(default)]
    pub params: Value,
    pub company_id: Uuid,
}

/// executeTool 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteToolParams {
    pub tool: String,
    pub args: Value,
    pub context: Value,
}

/// Tool 执行结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

/// handleApiRequest 参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandleApiRequestParams {
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    #[serde(default)]
    pub headers: Value,
    pub company_id: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_params_round_trip() {
        let p = InitializeParams {
            plugin_id: Uuid::new_v4(),
            plugin_version: "1.0.0".into(),
            manifest_version: "v1".into(),
            instance_id: Uuid::new_v4(),
            runtime_config: Value::Null,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: InitializeParams = serde_json::from_str(&json).unwrap();
        assert_eq!(back.plugin_version, "1.0.0");
    }

    #[test]
    fn tool_result_serialization_skips_none() {
        let r = ToolResult {
            ok: true,
            output: Some(Value::String("done".into())),
            error: None,
            metadata: Value::Null,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"output\":\"done\""));
        assert!(!json.contains("error"));
    }

    #[test]
    fn event_with_minimal_fields() {
        let ev = PluginEvent {
            event: "issue.created".into(),
            resource: "issue".into(),
            resource_id: Uuid::new_v4(),
            company_id: None,
            actor: None,
            data: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"event\":\"issue.created\""));
        assert!(!json.contains("companyId"));
        assert!(!json.contains("data"));
    }
}
