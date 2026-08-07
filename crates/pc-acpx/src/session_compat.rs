//! `pc-acpx` session compatibility helpers — mirrors Node `uniqueSorted`
//! and `isCompatibleSession` from `acpx-engine/execute.ts`. The runtime
//! decides whether a persisted ACPX session record can be reused against
//! the runtime we are about to launch: fingerprint, session key, agent,
//! mode, cwd, and the (possibly empty) remote execution identity must all
//! match.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::hash::stable_json;

/// Deduplicate, sort, and drop empty entries. Mirrors Node `uniqueSorted`.
///
/// Accepts any iterable of `Option<String>` so callers can pipe through
/// JSON-derived `Option` values without an extra `unwrap`.
pub fn unique_sorted<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = Option<String>>,
{
    let mut kept: Vec<String> = values
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .collect();
    kept.sort();
    kept.dedup();
    kept
}

/// Minimal view of a `PreparedRuntime` used for session compatibility and
/// session-config-option derivation. The full `PreparedRuntime` is too heavy
/// (it carries skill identity, MCP identity, and bridge state) for the
/// equality check; we only need the values that affect session routing and
/// runtime overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpxPreparedRuntimeLite {
    pub fingerprint: String,
    pub session_key: String,
    pub acpx_agent: String,
    pub mode: String,
    pub cwd: String,
    pub remote_execution_identity: Option<serde_json::Value>,
    /// Requested model name (e.g. `gpt-5`). Empty/None means "do not set".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// Requested reasoning effort (e.g. `high`, `medium`). Empty/None means
    /// "do not set".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_thinking_effort: Option<String>,
    /// Whether fast mode was requested.
    #[serde(default)]
    pub fast_mode: bool,
}

impl AcpxPreparedRuntimeLite {
    /// Build a fresh lite view from the fields the equality check requires.
    pub fn new(
        fingerprint: impl Into<String>,
        session_key: impl Into<String>,
        acpx_agent: impl Into<String>,
        mode: impl Into<String>,
        cwd: impl Into<String>,
        remote_execution_identity: Option<serde_json::Value>,
    ) -> Self {
        Self {
            fingerprint: fingerprint.into(),
            session_key: session_key.into(),
            acpx_agent: acpx_agent.into(),
            mode: mode.into(),
            cwd: cwd.into(),
            remote_execution_identity,
            requested_model: None,
            requested_thinking_effort: None,
            fast_mode: false,
        }
    }

    pub fn with_overrides(
        mut self,
        model: Option<String>,
        effort: Option<String>,
        fast_mode: bool,
    ) -> Self {
        self.requested_model = model;
        self.requested_thinking_effort = effort;
        self.fast_mode = fast_mode;
        self
    }
}

/// Decide whether a serialized ACPX session can be replayed against the
/// runtime we are about to launch. Mirrors Node `isCompatibleSession`.
pub fn is_compatible_session(
    params: &HashMap<String, String>,
    runtime: &AcpxPreparedRuntimeLite,
) -> bool {
    if params
        .get("configFingerprint")
        .map(String::as_str)
        .unwrap_or("")
        != runtime.fingerprint
    {
        return false;
    }
    if params.get("sessionKey").map(String::as_str).unwrap_or("") != runtime.session_key {
        return false;
    }
    if params.get("agent").map(String::as_str).unwrap_or("") != runtime.acpx_agent {
        return false;
    }
    if params.get("mode").map(String::as_str).unwrap_or("") != runtime.mode {
        return false;
    }
    let saved_cwd = params.get("cwd").map(String::as_str).unwrap_or("");
    if saved_cwd.is_empty() {
        return false;
    }
    if !paths_equal(saved_cwd, &runtime.cwd) {
        return false;
    }
    let saved_remote = params
        .get("remoteExecution")
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let current_remote = runtime
        .remote_execution_identity
        .clone()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    stable_json(&saved_remote) == stable_json(&current_remote)
}

/// Decide whether the raw persisted `sessionParams` JSON can be resumed.
///
/// The Node executor receives an untyped object, not a `HashMap<String,
/// String>`. Keeping this projection here avoids lossy conversions in the
/// executor (in particular for the nested `remoteExecution` identity) and
/// makes the compatibility gate usable by every execution target.
pub fn is_compatible_session_value(
    params: &serde_json::Value,
    runtime: &AcpxPreparedRuntimeLite,
) -> bool {
    let Some(record) = params.as_object() else {
        return false;
    };

    let read_string = |key: &str| {
        record
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };

    if read_string("configFingerprint") != Some(runtime.fingerprint.as_str()) {
        return false;
    }
    if read_string("sessionKey") != Some(runtime.session_key.as_str()) {
        return false;
    }
    if read_string("agent") != Some(runtime.acpx_agent.as_str()) {
        return false;
    }
    if read_string("mode") != Some(runtime.mode.as_str()) {
        return false;
    }
    let Some(saved_cwd) = read_string("cwd") else {
        return false;
    };
    if !paths_equal(saved_cwd, &runtime.cwd) {
        return false;
    }

    let saved_remote = record
        .get("remoteExecution")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let current_remote = runtime
        .remote_execution_identity
        .clone()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    stable_json(&saved_remote) == stable_json(&current_remote)
}

/// Extract the ACP backend session id used for a resume request.
///
/// Compatibility and identity are intentionally separate: a record may be
/// compatible while only carrying a runtime/cache identity, in which case a
/// warm handle can still be reused but no `resumeSessionId` is sent.
pub fn resume_session_id(params: &serde_json::Value) -> Option<String> {
    params
        .as_object()
        .and_then(|record| record.get("acpSessionId"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn paths_equal(a: &str, b: &str) -> bool {
    let pa = Path::new(a);
    let pb = Path::new(b);
    match (pa.canonicalize(), pb.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => {
            let abs_a = if pa.is_absolute() {
                pa.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| Path::new(".").to_path_buf())
                    .join(pa)
            };
            let abs_b = if pb.is_absolute() {
                pb.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| Path::new(".").to_path_buf())
                    .join(pb)
            };
            abs_a == abs_b
        }
    }
}
