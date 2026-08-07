//! `pc-acpx` adapter skill context — lightweight value type shared by
//! concrete adapter crates when they implement `listXxxSkills` /
//! `syncXxxSkills`.
//!
//! Rust port of Node `packages/adapter-utils/src/types.ts` L278-283
//! (`AdapterSkillContext`). The `listSkills` / `syncSkills` methods
//! themselves are NOT part of the `Adapter` trait in
//! `pc-adapter-api::Adapter` (which only carries `execute`); each
//! adapter crate exposes a `list_*_skills` / `sync_*_skills` function
//! directly and server-side wiring picks them up by adapter type.
//!
//! This keeps `pc-acpx` free of `pc-adapter-api` (no circular
//! dependency) while still letting every adapter crate share the same
//! context shape.

use serde_json::Value;

/// Input shape for every `list_*_skills` / `sync_*_skills` adapter
/// call. Mirrors Node `AdapterSkillContext` (L278-283).
///
/// - `agent_id` / `company_id` identify the Paperclip-owned entity
///   the run belongs to.
/// - `adapter_type` echoes the registered `AdapterDescriptor.adapter_type`
///   so callers can switch on it without reaching for the descriptor.
/// - `config` is the raw adapter config object (may carry `env`,
///   `paperclipRuntimeSkills`, `paperclipSkillSync`, etc.).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSkillContext {
    pub agent_id: String,
    pub company_id: String,
    pub adapter_type: String,
    pub config: Value,
}

impl AdapterSkillContext {
    /// Convenience constructor that accepts borrowed string slices
    /// (server-side often has `&str` before materialising the struct).
    pub fn new(
        agent_id: impl Into<String>,
        company_id: impl Into<String>,
        adapter_type: impl Into<String>,
        config: Value,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            company_id: company_id.into(),
            adapter_type: adapter_type.into(),
            config,
        }
    }

    /// Look up a nested value inside `config` by walking a dotted
    /// key path. Mirrors the small `getConfigPath` style helper
    /// adapters use in Node (e.g. `config.env.HOME`). Returns `None`
    /// when any segment is missing or the value is not a JSON object
    /// at an intermediate step.
    pub fn lookup_path(&self, path: &str) -> Option<Value> {
        let mut current: &Value = &self.config;
        for segment in path.split('.') {
            current = current.as_object()?.get(segment)?;
        }
        Some(current.clone())
    }

    /// Borrow the inner `env` block as a JSON object, treating any
    /// non-object value (including absence) as empty. Most adapters
    /// use this to resolve a `HOME` override.
    pub fn env_object(&self) -> &serde_json::Map<String, Value> {
        self.config
            .get("env")
            .and_then(Value::as_object)
            .unwrap_or_else(|| {
                // Return a reference to an empty map owned by `serde_json`.
                // The trick: serde_json::Map::new() can't be returned by
                // reference, so we lazily build a thread-local via OnceLock.
                static EMPTY: std::sync::OnceLock<serde_json::Map<String, Value>> =
                    std::sync::OnceLock::new();
                EMPTY.get_or_init(serde_json::Map::new)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_new_round_trips_fields() {
        let ctx = AdapterSkillContext::new(
            "agent-1",
            "company-1",
            "claude_local",
            json!({ "env": { "HOME": "/tmp" } }),
        );
        assert_eq!(ctx.agent_id, "agent-1");
        assert_eq!(ctx.company_id, "company-1");
        assert_eq!(ctx.adapter_type, "claude_local");
        assert_eq!(ctx.config, json!({ "env": { "HOME": "/tmp" } }));
    }

    #[test]
    fn lookup_path_walks_dotted_keys() {
        let ctx = AdapterSkillContext::new(
            "a",
            "c",
            "t",
            json!({ "env": { "HOME": "/home/x" }, "paperclipRuntimeSkills": [] }),
        );
        assert_eq!(ctx.lookup_path("env.HOME"), Some(json!("/home/x")));
        assert_eq!(ctx.lookup_path("paperclipRuntimeSkills"), Some(json!([])));
        assert_eq!(ctx.lookup_path("env.MISSING"), None);
        assert_eq!(ctx.lookup_path("missing.path.here"), None);
    }

    #[test]
    fn env_object_returns_empty_when_no_env() {
        let ctx = AdapterSkillContext::new("a", "c", "t", json!({}));
        assert!(ctx.env_object().is_empty());
    }

    #[test]
    fn env_object_returns_empty_when_env_is_scalar() {
        let ctx = AdapterSkillContext::new("a", "c", "t", json!({ "env": "not-an-object" }));
        assert!(ctx.env_object().is_empty());
    }

    #[test]
    fn env_object_returns_block_when_object() {
        let ctx = AdapterSkillContext::new("a", "c", "t", json!({ "env": { "HOME": "/h" } }));
        assert_eq!(ctx.env_object().get("HOME"), Some(&json!("/h")));
    }
}
