//! Plugin IPC 方法枚举 + 结果。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Multica plugin IPC 方法（与 multica `packages/plugin-sdk/src/methods.ts` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    #[serde(rename = "initialize")]
    Initialize,
    #[serde(rename = "health")]
    Health,
    #[serde(rename = "runJob")]
    RunJob,
    #[serde(rename = "handleWebhook")]
    HandleWebhook,
    #[serde(rename = "getData")]
    GetData,
    #[serde(rename = "performAction")]
    PerformAction,
    #[serde(rename = "executeTool")]
    ExecuteTool,
    #[serde(rename = "onEvent")]
    OnEvent,
    #[serde(rename = "shutdown")]
    Shutdown,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Health => "health",
            Self::RunJob => "runJob",
            Self::HandleWebhook => "handleWebhook",
            Self::GetData => "getData",
            Self::PerformAction => "performAction",
            Self::ExecuteTool => "executeTool",
            Self::OnEvent => "onEvent",
            Self::Shutdown => "shutdown",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "initialize" => Some(Self::Initialize),
            "health" => Some(Self::Health),
            "runJob" => Some(Self::RunJob),
            "handleWebhook" => Some(Self::HandleWebhook),
            "getData" => Some(Self::GetData),
            "performAction" => Some(Self::PerformAction),
            "executeTool" => Some(Self::ExecuteTool),
            "onEvent" => Some(Self::OnEvent),
            "shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodResult {
    pub ok: bool,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

impl MethodResult {
    pub fn ok(data: Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_round_trip() {
        for m in [
            Method::Initialize,
            Method::Health,
            Method::RunJob,
            Method::HandleWebhook,
            Method::GetData,
            Method::PerformAction,
            Method::ExecuteTool,
            Method::OnEvent,
            Method::Shutdown,
        ] {
            assert_eq!(Method::from_str_opt(m.as_str()), Some(m));
        }
    }

    #[test]
    fn unknown_method_returns_none() {
        assert!(Method::from_str_opt("garbage").is_none());
    }

    #[test]
    fn result_serializes() {
        let r = MethodResult::ok(serde_json::json!({"x": 1}));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"ok\":true"));
    }
}
