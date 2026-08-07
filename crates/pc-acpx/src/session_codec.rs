//! `pc-acpx` session codec — pure serializer/deserializer for ACPX session
//! metadata that mirrors the Node `acpx-engine/session-codec.ts`.
//!
//! The codec accepts an arbitrary JSON value (typically a `session_params`
//! blob in `heartbeat_runs`), normalizes it into a typed `AcpxSessionParams`
//! record, and exposes a stable display id for UI surfacing. The codec is
//! conservative: it only normalizes fields whose values are well-typed strings
//! or objects; any non-string scalars are dropped silently.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

// ============================================================================
// Public types
// ============================================================================

/// ACPX session metadata.
///
/// Field shapes mirror Node `sessionCodec.deserialize` output. All fields are
/// optional — a session that only has `acpSessionId` is still usable.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpxSessionParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_session_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acpx_record_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acp_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_execution: Option<BTreeMap<String, Value>>,
}

// ============================================================================
// Codec
// ============================================================================

/// Normalize a raw `serde_json::Value` into `AcpxSessionParams`.
///
/// Returns `None` when the value is not an object, or when none of the
/// session-identity fields are present.
pub fn deserialize(raw: &Value) -> Option<AcpxSessionParams> {
    let record = raw.as_object()?;
    let runtime_session_name = read_string(record.get("runtimeSessionName"));
    let acp_session_id = read_string(record.get("acpSessionId"));
    let agent_session_id = read_string(record.get("agentSessionId"));
    let remote_execution = read_object(record.get("remoteExecution"));

    if runtime_session_name.is_none() && acp_session_id.is_none() && agent_session_id.is_none() {
        return None;
    }

    Some(AcpxSessionParams {
        runtime_session_name,
        session_key: read_string(record.get("sessionKey")),
        acpx_record_id: read_string(record.get("acpxRecordId")),
        acp_session_id,
        agent_session_id,
        agent: read_string(record.get("agent")),
        cwd: read_string(record.get("cwd")),
        mode: read_string(record.get("mode")),
        state_dir: read_string(record.get("stateDir")),
        config_fingerprint: read_string(record.get("configFingerprint")),
        workspace_id: read_string(record.get("workspaceId")),
        repo_url: read_string(record.get("repoUrl")),
        repo_ref: read_string(record.get("repoRef")),
        remote_execution,
    })
}

/// Serialize an `AcpxSessionParams` (or a free-form map) back into a
/// `serde_json::Value`. When given a free-form map, the same normalization
/// rules as `deserialize` apply.
pub fn serialize(params: Option<&AcpxSessionParams>) -> Option<Value> {
    let params = params?;
    let mut object = serde_json::Map::new();
    if let Some(name) = &params.runtime_session_name {
        object.insert("runtimeSessionName".into(), Value::String(name.clone()));
    }
    if let Some(key) = &params.session_key {
        object.insert("sessionKey".into(), Value::String(key.clone()));
    }
    if let Some(id) = &params.acpx_record_id {
        object.insert("acpxRecordId".into(), Value::String(id.clone()));
    }
    if let Some(id) = &params.acp_session_id {
        object.insert("acpSessionId".into(), Value::String(id.clone()));
    }
    if let Some(id) = &params.agent_session_id {
        object.insert("agentSessionId".into(), Value::String(id.clone()));
    }
    if let Some(agent) = &params.agent {
        object.insert("agent".into(), Value::String(agent.clone()));
    }
    if let Some(cwd) = &params.cwd {
        object.insert("cwd".into(), Value::String(cwd.clone()));
    }
    if let Some(mode) = &params.mode {
        object.insert("mode".into(), Value::String(mode.clone()));
    }
    if let Some(state_dir) = &params.state_dir {
        object.insert("stateDir".into(), Value::String(state_dir.clone()));
    }
    if let Some(fp) = &params.config_fingerprint {
        object.insert("configFingerprint".into(), Value::String(fp.clone()));
    }
    if let Some(id) = &params.workspace_id {
        object.insert("workspaceId".into(), Value::String(id.clone()));
    }
    if let Some(url) = &params.repo_url {
        object.insert("repoUrl".into(), Value::String(url.clone()));
    }
    if let Some(reference) = &params.repo_ref {
        object.insert("repoRef".into(), Value::String(reference.clone()));
    }
    if let Some(remote) = &params.remote_execution {
        let value = serde_json::to_value(remote).ok()?;
        object.insert("remoteExecution".into(), value);
    }
    Some(Value::Object(object))
}

/// Best human-readable session identifier. Priority:
/// `runtimeSessionName` → `acpSessionId` → `agentSessionId`.
pub fn get_display_id(params: Option<&AcpxSessionParams>) -> Option<String> {
    let params = params?;
    params
        .runtime_session_name
        .clone()
        .or_else(|| params.acp_session_id.clone())
        .or_else(|| params.agent_session_id.clone())
}

// ============================================================================
// Helpers
// ============================================================================

fn read_string(value: Option<&Value>) -> Option<String> {
    let value = value?;
    let string = value.as_str()?;
    let trimmed = string.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn read_object(value: Option<&Value>) -> Option<BTreeMap<String, Value>> {
    let obj = value?.as_object()?;
    let mut map = BTreeMap::new();
    for (key, value) in obj {
        map.insert(key.clone(), value.clone());
    }
    Some(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_drops_empty_objects() {
        let value = serde_json::json!({});
        assert_eq!(deserialize(&value), None);
        let arr = serde_json::json!([]);
        assert_eq!(deserialize(&arr), None);
    }

    #[test]
    fn deserialize_keeps_only_known_keys() {
        let value = serde_json::json!({
            "acpSessionId": "acp-1",
            "agent": "claude",
            "unknownField": "ignored",
        });
        let params = deserialize(&value).expect("params");
        assert_eq!(params.acp_session_id.as_deref(), Some("acp-1"));
        assert_eq!(params.agent.as_deref(), Some("claude"));
    }

    #[test]
    fn deserialize_requires_a_session_identity() {
        let value = serde_json::json!({ "agent": "claude" });
        assert!(deserialize(&value).is_none());
    }

    #[test]
    fn deserialize_round_trips_via_serialize() {
        let raw = serde_json::json!({
            "runtimeSessionName": "rs",
            "sessionKey": "sk",
            "acpxRecordId": "ar",
            "acpSessionId": "acp-1",
            "agentSessionId": "as-1",
            "agent": "claude",
            "cwd": "/tmp",
            "mode": "persistent",
            "stateDir": "/state",
            "configFingerprint": "abc",
            "workspaceId": "ws-1",
            "repoUrl": "https://example.com/r.git",
            "repoRef": "main",
            "remoteExecution": { "transport": "ssh" },
        });
        let params = deserialize(&raw).expect("params");
        let serialized = serialize(Some(&params)).expect("serialized");
        assert_eq!(serialized, raw);
    }

    #[test]
    fn get_display_id_prefers_runtime_session_name() {
        let params = AcpxSessionParams {
            runtime_session_name: Some("rs".into()),
            acp_session_id: Some("acp-1".into()),
            agent_session_id: Some("as-1".into()),
            ..Default::default()
        };
        assert_eq!(get_display_id(Some(&params)).as_deref(), Some("rs"));

        let params = AcpxSessionParams {
            acp_session_id: Some("acp-1".into()),
            agent_session_id: Some("as-1".into()),
            ..Default::default()
        };
        assert_eq!(get_display_id(Some(&params)).as_deref(), Some("acp-1"));

        let params = AcpxSessionParams {
            agent_session_id: Some("as-1".into()),
            ..Default::default()
        };
        assert_eq!(get_display_id(Some(&params)).as_deref(), Some("as-1"));

        assert_eq!(get_display_id(None), None);
    }

    #[test]
    fn read_string_trims_whitespace() {
        assert_eq!(
            read_string(Some(&Value::String("  abc  ".into()))).as_deref(),
            Some("abc")
        );
        assert!(read_string(Some(&Value::String("   ".into()))).is_none());
        assert!(read_string(Some(&Value::Null)).is_none());
        assert!(read_string(Some(&Value::from(42))).is_none());
    }
}
