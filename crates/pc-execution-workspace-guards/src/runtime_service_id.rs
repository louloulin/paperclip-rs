#![forbid(unsafe_code)]

//! Runtime service stable ID generation.
//! R705: Direct port of workspace-runtime.ts::stableStringify + stableRuntimeServiceId.

use serde::Serialize;
use sha2::{Digest, Sha256};

/// Runtime service scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeServiceScope {
    Run,
    ProjectWorkspace,
    ExecutionWorkspace,
    Agent,
}

impl RuntimeServiceScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::ProjectWorkspace => "project_workspace",
            Self::ExecutionWorkspace => "execution_workspace",
            Self::Agent => "agent",
        }
    }
}

/// Stable stringify: recursive, sorted keys.
pub fn stable_stringify<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    stringify_value(&json)
}

fn stringify_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(stringify_value).collect();
            format!("[{}]", parts.join(","))
        }
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys.iter().map(|k| {
                let key_json = serde_json::to_string(k).unwrap_or_default();
                let val_str = stringify_value(&map[*k]);
                format!("{}:{}", key_json, val_str)
            }).collect();
            format!("{{{}}}", parts.join(","))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_string()),
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeServiceIdInput {
    pub adapter_type: String,
    pub run_id: String,
    pub scope_type: RuntimeServiceScope,
    pub scope_id: Option<String>,
    pub service_name: String,
    pub report_id: Option<String>,
    pub provider_ref: Option<String>,
    pub reuse_key: Option<String>,
}

pub fn stable_runtime_service_id(input: &RuntimeServiceIdInput) -> String {
    if let Some(ref id) = input.report_id {
        return id.clone();
    }
    let payload = serde_json::json!({
        "adapterType": input.adapter_type,
        "runId": input.run_id,
        "scopeType": input.scope_type.as_str(),
        "scopeId": input.scope_id,
        "serviceName": input.service_name,
        "providerRef": input.provider_ref,
        "reuseKey": input.reuse_key,
    });
    let s = stable_stringify(&payload);
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    let hex = format!("{:x}", result);
    let truncated = hex.chars().take(32).collect::<String>();
    format!("{}-{}", input.adapter_type, truncated)
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stable_stringify_primitive() {
        assert_eq!(stable_stringify(&1i32), "1");
        assert_eq!(stable_stringify(&"hello".to_string()), "\"hello\"");
        assert_eq!(stable_stringify(&true), "true");
        assert_eq!(stable_stringify(&Option::<String>::None), "null");
    }

    #[test]
    fn stable_stringify_array() {
        assert_eq!(stable_stringify(&json!([1, 2, 3])), "[1,2,3]");
        assert_eq!(stable_stringify(&json!(["a", "b"])), "[\"a\",\"b\"]");
    }

    #[test]
    fn stable_stringify_object_sorts_keys() {
        let s1 = stable_stringify(&json!({"a": 1, "b": 2}));
        let s2 = stable_stringify(&json!({"b": 2, "a": 1}));
        assert_eq!(s1, s2);
        assert_eq!(s1, "{\"a\":1,\"b\":2}");
    }

    #[test]
    fn stable_stringify_nested() {
        let s = stable_stringify(&json!({"x": {"a": 1, "b": 2}, "y": [3, 4]}));
        assert_eq!(s, "{\"x\":{\"a\":1,\"b\":2},\"y\":[3,4]}");
    }

    #[test]
    fn stable_stringify_empty() {
        assert_eq!(stable_stringify(&json!({})), "{}");
        assert_eq!(stable_stringify(&json!([])), "[]");
    }

    fn make_input(scope: RuntimeServiceScope) -> RuntimeServiceIdInput {
        RuntimeServiceIdInput {
            adapter_type: "claude_local".into(),
            run_id: "run_123".into(),
            scope_type: scope,
            scope_id: Some("scope_1".into()),
            service_name: "dev_server".into(),
            report_id: None,
            provider_ref: Some("provider_ref_1".into()),
            reuse_key: None,
        }
    }

    #[test]
    fn stable_id_uses_report_id_when_present() {
        let mut input = make_input(RuntimeServiceScope::Run);
        input.report_id = Some("report_id_xyz".into());
        assert_eq!(stable_runtime_service_id(&input), "report_id_xyz");
    }

    #[test]
    fn stable_id_deterministic() {
        let input = make_input(RuntimeServiceScope::Run);
        let id1 = stable_runtime_service_id(&input);
        let id2 = stable_runtime_service_id(&input);
        assert_eq!(id1, id2);
    }

    #[test]
    fn stable_id_format() {
        let input = make_input(RuntimeServiceScope::Run);
        let id = stable_runtime_service_id(&input);
        assert!(id.starts_with("claude_local-"));
        let hex_part = &id["claude_local-".len()..];
        assert_eq!(hex_part.len(), 32);
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn stable_id_changes_with_scope() {
        let run = make_input(RuntimeServiceScope::Run);
        let project = make_input(RuntimeServiceScope::ProjectWorkspace);
        let execution = make_input(RuntimeServiceScope::ExecutionWorkspace);
        let agent = make_input(RuntimeServiceScope::Agent);
        assert_ne!(stable_runtime_service_id(&run), stable_runtime_service_id(&project));
        assert_ne!(stable_runtime_service_id(&run), stable_runtime_service_id(&execution));
        assert_ne!(stable_runtime_service_id(&run), stable_runtime_service_id(&agent));
    }

    #[test]
    fn stable_id_changes_with_service_name() {
        let mut a = make_input(RuntimeServiceScope::Run);
        a.service_name = "alpha".into();
        let mut b = make_input(RuntimeServiceScope::Run);
        b.service_name = "beta".into();
        assert_ne!(stable_runtime_service_id(&a), stable_runtime_service_id(&b));
    }

    #[test]
    fn scope_as_str_matches_node() {
        assert_eq!(RuntimeServiceScope::Run.as_str(), "run");
        assert_eq!(RuntimeServiceScope::ProjectWorkspace.as_str(), "project_workspace");
        assert_eq!(RuntimeServiceScope::ExecutionWorkspace.as_str(), "execution_workspace");
        assert_eq!(RuntimeServiceScope::Agent.as_str(), "agent");
    }
}
