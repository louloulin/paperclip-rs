#![forbid(unsafe_code)]

//! `cursor_cloud` adapter: spawn-less Cursor Cloud SDK wrapper.
//!
//! 当前阶段复刻（对齐 Node `packages/adapters/cursor-cloud/src/server/execute.ts`）：
//! - 常量（`constants`）
//! - session codec（`session_codec`）
//! - JSONL 事件编解码（`event_codec`）
//! - 配置 schema（`config_schema`）
//!
//! 后续 R608+：
//! - wake env / prompt render / result builder
//! - cloud client trait + fake HTTP server
//! - 完整 execute path

pub mod cloud_client;
pub mod config_schema;
pub mod constants;
pub mod event_codec;
pub mod execute;
pub mod http_client;
pub mod prompt_render;
pub mod result_builder;
pub mod session_codec;
pub mod wake_env;

pub use config_schema::{
    describe_adapter, get_config_schema, AdapterDescriptor, ConfigField, ConfigOption, FieldType,
};
pub use constants::{
    ADAPTER_LABEL, ADAPTER_TYPE, ADAPTER_TYPE as CURSOR_CLOUD_ADAPTER_TYPE, BILLER, BILLING_TYPE,
    DEFAULT_RUNTIME_ENV_TYPE, FORBIDDEN_CONFIG_KEYS, PAPERCLIP_ENV_PREFIX, PROVIDER,
    REQUIRED_CONFIG_FIELDS, RUNTIME_ENV_TYPES,
};
pub use event_codec::{
    event_line, init_event, message_event, parse_cursor_cloud_stdout_line, result_event,
    serialize_sdk_assistant, status_event, CursorCloudEvent, SdkMessageKind,
};
pub use session_codec::{
    deserialize_session, display_id, serialize_session, session_matches, CursorCloudRepo,
    CursorCloudSession, RuntimeEnvType,
};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct AdapterReadinessReport {
    pub ready: bool,
    pub missing: Vec<&'static str>,
    pub extra: Vec<String>,
    pub checks: Vec<(String, bool)>,
}

/// 决策：当前配置是否满足 cursor_cloud 的最低运行要求。
///
/// 必备：
/// - `CURSOR_API_KEY`（来自 `envBindings` 解析后映射）
/// - `repoUrl`（config 或 workspace.repoUrl 兜底）
///
/// **当前实现只校验静态字段**（不真正连云），所以 `ready` 仅意味着配置齐全；
/// HTTP 层校验留给后续 round（云端 `auth check` via fake server）。
pub fn evaluate_readiness(
    env_map: &Value,
    config: &Value,
    workspace: &Value,
) -> AdapterReadinessReport {
    let mut missing: Vec<&'static str> = Vec::new();
    let mut checks: Vec<(String, bool)> = Vec::new();

    let api_key = env_map
        .get("CURSOR_API_KEY")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let api_key_ok = api_key.is_some();
    checks.push(("env.CURSOR_API_KEY".to_owned(), api_key_ok));
    if api_key.is_none() {
        missing.push("CURSOR_API_KEY");
    }

    let cfg_repo = config
        .get("repoUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let ws_repo = workspace
        .get("repoUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let repo_url = cfg_repo.or(ws_repo);
    let repo_ok = repo_url.is_some();
    checks.push(("config.repoUrl or workspace.repoUrl".to_owned(), repo_ok));
    if repo_url.is_none() {
        missing.push("repoUrl");
    }

    let env_type_raw = config
        .get("runtimeEnvType")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_RUNTIME_ENV_TYPE);
    let normalized = RuntimeEnvType::from_loose(env_type_raw);
    let needs_name = !matches!(normalized, RuntimeEnvType::Cloud);
    let env_name = config
        .get("runtimeEnvName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let env_name_ok = !needs_name || env_name.is_some();
    checks.push((
        "config.runtimeEnvName when required".to_owned(),
        env_name_ok,
    ));
    if !env_name_ok {
        missing.push("runtimeEnvName");
    }

    // 可选 —— `model` 仅在用户明确指定时传入 SDK。
    let model_present = config
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    checks.push(("config.model (optional)".to_owned(), true));
    let mut extra = Vec::new();
    if model_present {
        extra.push("model".to_owned());
    }

    let ready = api_key_ok && repo_ok && env_name_ok;
    AdapterReadinessReport {
        ready,
        missing,
        extra,
        checks,
    }
}

/// Document — JSON-serializable readiness summary (供 UI / onMeta 报告)。
pub fn readiness_to_value(report: &AdapterReadinessReport) -> Value {
    serde_json::json!({
        "ready": report.ready,
        "missing": report.missing,
        "checks": report.checks.iter().map(|(k, ok)| {
            serde_json::json!({"key": k, "ok": ok})
        }).collect::<Vec<_>>(),
        "extras": report.extra,
    })
}

/// 一句错误摘要（Paperclip UI 顶层 alert 用）。
pub fn readiness_error_message(report: &AdapterReadinessReport) -> Option<String> {
    if report.ready {
        return None;
    }
    Some(format!(
        "cursor_cloud is not ready: missing fields [{}]",
        report.missing.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn readiness_full_pass() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({"repoUrl": "https://github.com/a/b"});
        let ws = json!({});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(r.ready, "report = {:?}", r);
        assert!(r.missing.is_empty());
    }

    #[test]
    fn readiness_missing_api_key() {
        let env = json!({});
        let cfg = json!({"repoUrl": "https://github.com/a/b"});
        let ws = json!({});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(!r.ready);
        assert!(r.missing.contains(&"CURSOR_API_KEY"));
    }

    #[test]
    fn readiness_falls_back_to_workspace_repo_url() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({});
        let ws = json!({"repoUrl": "https://github.com/a/b"});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(r.ready, "report = {:?}", r);
    }

    #[test]
    fn readiness_missing_repo_url() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({});
        let ws = json!({});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(!r.ready);
        assert!(r.missing.contains(&"repoUrl"));
    }

    #[test]
    fn readiness_pool_requires_env_name() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({"repoUrl": "https://github.com/a/b", "runtimeEnvType": "pool"});
        let ws = json!({});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(!r.ready);
        assert!(r.missing.contains(&"runtimeEnvName"));
    }

    #[test]
    fn readiness_pool_with_name_passes() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({
            "repoUrl": "https://github.com/a/b",
            "runtimeEnvType": "pool",
            "runtimeEnvName": "pool-1"
        });
        let ws = json!({});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(r.ready);
    }

    #[test]
    fn readiness_machine_default_requires_name_when_normalized() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({"repoUrl": "https://github.com/a/b", "runtimeEnvType": "Machine"});
        let ws = json!({});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(!r.ready);
        assert!(r.missing.contains(&"runtimeEnvName"));
    }

    #[test]
    fn readiness_extra_lists_model_when_present() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({"repoUrl": "https://github.com/a/b", "model": "gpt-4"});
        let ws = json!({});
        let r = evaluate_readiness(&env, &cfg, &ws);
        assert!(r.ready);
        assert!(r.extra.contains(&"model".to_owned()));
    }

    #[test]
    fn readiness_error_message_returns_none_when_ready() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({"repoUrl": "https://github.com/a/b"});
        let r = evaluate_readiness(&env, &cfg, &json!({}));
        assert!(readiness_error_message(&r).is_none());
    }

    #[test]
    fn readiness_error_message_lists_missing() {
        let env = json!({});
        let cfg = json!({});
        let r = evaluate_readiness(&env, &cfg, &json!({}));
        let m = readiness_error_message(&r).unwrap();
        assert!(m.contains("CURSOR_API_KEY"));
        assert!(m.contains("repoUrl"));
    }

    #[test]
    fn readiness_to_value_serializes_to_json() {
        let env = json!({"CURSOR_API_KEY": "ck-1"});
        let cfg = json!({"repoUrl": "https://github.com/a/b"});
        let r = evaluate_readiness(&env, &cfg, &json!({}));
        let v = readiness_to_value(&r);
        assert_eq!(v["ready"], serde_json::json!(true));
        assert!(v["missing"].is_array());
        assert!(v["checks"].is_array());
    }
}

/// Public alias for the runtime cloud client.
pub use crate::execute::CursorCloudAdapter;
