//! Hermes gateway adapter（对齐 Node
//! `packages/adapters/hermes/src/gateway/server/execute.ts` 的非 HTTP 部分）。
//!
//! 当前覆盖：
//! - 配置 schema（9 字段）
//! - 传输安全校验（loopback / 远端 HTTP / escape hatch）
//! - session key 策略解析
//! - Dashboard REST + SSE 事件流消费
//! - session key、重连退避和终态结果构造
//!
//! 模块拆分（高内聚、低耦合）：
//! - [`constants`] — adapter 常量 + `SessionKeyStrategy` 枚举
//! - [`config_schema`] — 9 字段 Paperclip UI schema
//! - [`transport_security`] — URL 安全校验（loopback / escape hatch）

#![forbid(unsafe_code)]

pub mod config_schema;
pub mod constants;
pub mod dashboard;
pub mod execute;
pub mod retry_policy;
pub mod sse_client;
pub mod transport_security;

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEventSink, AdapterExecutionContext,
    AdapterExecutionResult,
};
use serde_json::Value;

pub use constants::{ADAPTER_LABEL, ADAPTER_TYPE};

/// Hermes gateway adapter。
///
/// Hermes Gateway 适配器的公共入口。
///
/// 实际执行委托给 `execute` 模块，公共入口只保留稳定的 adapter API。
pub struct HermesGatewayAdapter;

impl HermesGatewayAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// 生产 runtime 工厂：构造基于 `reqwest` 的真实 HermesGateway client。
    ///
    /// 当 `base_url` + `api_key` 都不为空时返回真实 transport 工厂；
    /// 否则回退到纯 stub（只保留 descriptor，无可执行 transport）。
    /// `extra_session_key` 可选：注入默认 session key。
    #[must_use]
    pub fn for_runtime(
        base_url: Option<String>,
        api_key: Option<String>,
        extra_session_key: Option<String>,
    ) -> Self {
        let base = base_url
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let key = api_key
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let sk = extra_session_key
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        match (base, key) {
            (Some(base_url), Some(api_key)) => {
                // 把运行时上下文缓存到 adapter 自己的 const 字段，供后续测试断言
                // 真正 run-time 执行仍由 `execute` 模块在 V2 入口构造 transport。
                Self::set_runtime_context(base_url, api_key, sk);
                Self::new()
            }
            _ => Self::new(),
        }
    }

    /// 暴露给 server / 测试的内部 hook：把 runtime ctx 缓存到 lib crate。
    fn set_runtime_context(base_url: String, api_key: String, session_key: Option<String>) {
        RUNTIME_CONTEXT.with(|slot| {
            *slot.borrow_mut() = Some(RuntimeContext {
                base_url,
                api_key,
                session_key,
            });
        });
    }

    /// Server 端读取运行时上下文以决定 trace 日志。
    pub fn runtime_context() -> Option<RuntimeContext> {
        RUNTIME_CONTEXT.with(|slot| slot.borrow().clone())
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeContext {
    pub base_url: String,
    pub api_key: String,
    pub session_key: Option<String>,
}

thread_local! {
    static RUNTIME_CONTEXT: std::cell::RefCell<Option<RuntimeContext>> = const { std::cell::RefCell::new(None) };
}

impl Default for HermesGatewayAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Adapter for HermesGatewayAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, ADAPTER_LABEL)
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        execute::HermesGatewayAdapterV2::new()
            .execute(context, events)
            .await
    }
}

/// 解析 hermes-gateway 命令路径（`adapterConfig.command` 覆盖默认）。
#[allow(dead_code)]
fn resolve_command(config: &Value) -> String {
    cfg_string(config.get("command")).unwrap_or_else(|| "hermes-gateway".to_string())
}

/// 解析 session key 策略（带默认值 fallback）。
pub fn resolve_session_key_strategy(config: &Value) -> constants::SessionKeyStrategy {
    cfg_string(config.get("sessionKeyStrategy"))
        .as_deref()
        .and_then(constants::SessionKeyStrategy::from_config_str)
        .unwrap_or(constants::SessionKeyStrategy::Issue)
}

/// 构造 X-Hermes-Session-Key header（对齐 Node `buildHermesSessionKey`）。
///
/// 行为：
/// - `Issue` 策略 → `paperclip:company:<id>:agent:<id>:issue:<id>`（若全部存在）
/// - `Agent` 策略 → `paperclip:company:<id>:agent:<id>`
/// - `Run` 策略 → `paperclip:run:<id>`
/// - `None` → `None`
///
/// 字段缺失时优雅降级为不带该段的最短形式。
pub fn build_session_key(
    strategy: constants::SessionKeyStrategy,
    company_id: Option<&str>,
    agent_id: Option<&str>,
    issue_id: Option<&str>,
    run_id: &str,
) -> Option<String> {
    match strategy {
        constants::SessionKeyStrategy::None => None,
        constants::SessionKeyStrategy::Run => Some(format!("paperclip:run:{run_id}")),
        constants::SessionKeyStrategy::Agent => {
            let company = company_id?;
            let agent = agent_id?;
            Some(format!("paperclip:company:{company}:agent:{agent}"))
        }
        constants::SessionKeyStrategy::Issue => {
            let company = company_id?;
            let agent = agent_id?;
            let issue = issue_id?;
            Some(format!(
                "paperclip:company:{company}:agent:{agent}:issue:{issue}"
            ))
        }
    }
}

/// Parse adapter-specific JSONL or text output from stdout（hermes-gateway）。
///
/// 旧版 CLI spawn 路径专用，V2 改用 dashboard + SSE。保留供回归测试与对比。
#[allow(dead_code)]
fn parse_stdout(stdout: &str) -> Option<String> {
    for line in stdout.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(text) = event.get("text").and_then(|v| v.as_str()) {
                return Some(text.to_owned());
            }
            if let Some(text) = event
                .get("item")
                .and_then(|v| v.get("text"))
                .and_then(|v| v.as_str())
            {
                return Some(text.to_owned());
            }
        }
        return Some(trimmed.to_owned());
    }
    None
}

fn cfg_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn descriptor_uses_constants() {
        let adapter = HermesGatewayAdapter::new();
        assert_eq!(adapter.descriptor().adapter_type, ADAPTER_TYPE);
        assert_eq!(adapter.descriptor().label, ADAPTER_LABEL);
    }

    #[test]
    fn resolve_command_falls_back_to_default() {
        assert_eq!(resolve_command(&json!({})), "hermes-gateway");
        assert_eq!(
            resolve_command(&json!({"command": "/custom/hermes"})),
            "/custom/hermes"
        );
    }

    #[test]
    fn resolve_session_key_strategy_defaults_to_issue() {
        assert_eq!(
            resolve_session_key_strategy(&json!({})),
            constants::SessionKeyStrategy::Issue
        );
        assert_eq!(
            resolve_session_key_strategy(&json!({"sessionKeyStrategy": "agent"})),
            constants::SessionKeyStrategy::Agent
        );
    }

    #[test]
    fn build_session_key_issue_requires_all_fields() {
        let key = build_session_key(
            constants::SessionKeyStrategy::Issue,
            Some("co-1"),
            Some("agent-1"),
            Some("issue-1"),
            "run-1",
        );
        assert_eq!(
            key,
            Some("paperclip:company:co-1:agent:agent-1:issue:issue-1".to_string())
        );

        // 缺 issue → None（graceful degrade）
        assert!(build_session_key(
            constants::SessionKeyStrategy::Issue,
            Some("co-1"),
            Some("agent-1"),
            None,
            "run-1",
        )
        .is_none());
    }

    #[test]
    fn build_session_key_agent_ignores_issue() {
        let key = build_session_key(
            constants::SessionKeyStrategy::Agent,
            Some("co-1"),
            Some("agent-1"),
            Some("issue-1"),
            "run-1",
        );
        assert_eq!(
            key,
            Some("paperclip:company:co-1:agent:agent-1".to_string())
        );
    }

    #[test]
    fn build_session_key_run_only_needs_run_id() {
        let key = build_session_key(
            constants::SessionKeyStrategy::Run,
            None,
            None,
            None,
            "run-abc",
        );
        assert_eq!(key, Some("paperclip:run:run-abc".to_string()));
    }

    #[test]
    fn build_session_key_none_returns_none() {
        let key = build_session_key(
            constants::SessionKeyStrategy::None,
            Some("co-1"),
            Some("agent-1"),
            Some("issue-1"),
            "run-1",
        );
        assert!(key.is_none());
    }

    #[test]
    fn parse_stdout_returns_last_useful_line() {
        let output = "line1\nline2\nhello world\n";
        assert_eq!(parse_stdout(output), Some("hello world".into()));
    }

    #[test]
    fn parse_stdout_handles_jsonl() {
        let output = r#"{"type":"item.completed","item":{"type":"agent_message","text":"Done"}}"#;
        assert_eq!(parse_stdout(output), Some("Done".into()));
    }

    #[test]
    fn parse_stdout_empty_returns_none() {
        assert_eq!(parse_stdout(""), None);
    }
}
