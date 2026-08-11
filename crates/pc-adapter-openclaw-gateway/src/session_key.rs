//! OpenClaw Gateway session-key 解析 — 对齐 Node
//! `packages/adapters/openclaw-gateway/src/server/execute.ts::resolveSessionKey`。
//!
//! 3 种策略：
//! - `fixed` — 用 config 里的 `configuredSessionKey`（缺省 `"paperclip"`）
//! - `issue` — 用 issueId（缺省 `"paperclip"`）
//! - `run`   — 用 runId
//!
//! 每个 session key 还会按 agent 名前缀（`agent:<id>:`）包装，避免多 agent 串扰。

#![allow(dead_code)]

use serde_json::Value;

use crate::constants::{
    DEFAULT_SESSION_KEY, DEFAULT_SESSION_KEY_STRATEGY, VALID_SESSION_KEY_STRATEGIES,
};

/// Session-key 策略枚举（编译期合法值检查；serde 自动识别 3 种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKeyStrategy {
    Fixed,
    Issue,
    Run,
}

impl SessionKeyStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKeyStrategy::Fixed => "fixed",
            SessionKeyStrategy::Issue => "issue",
            SessionKeyStrategy::Run => "run",
        }
    }

    /// 归一化：trim + lowercase；非合法值回落到 `"issue"`。
    pub fn from_loose(value: &str) -> Self {
        let normalized = value.trim().to_lowercase();
        match normalized.as_str() {
            "fixed" => SessionKeyStrategy::Fixed,
            "issue" => SessionKeyStrategy::Issue,
            "run" => SessionKeyStrategy::Run,
            _ => SessionKeyStrategy::Issue,
        }
    }

    /// 从 `serde_json::Value` 读取（trim/lower-aware）。
    pub fn from_value(value: Option<&Value>) -> Self {
        value
            .and_then(|v| v.as_str())
            .map(Self::from_loose)
            .unwrap_or(SessionKeyStrategy::Issue)
    }
}

impl Default for SessionKeyStrategy {
    fn default() -> Self {
        SessionKeyStrategy::Issue
    }
}

/// Session-key 构造输入。
#[derive(Debug, Clone)]
pub struct SessionKeyInput<'a> {
    pub strategy: SessionKeyStrategy,
    pub configured_session_key: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub run_id: &'a str,
    pub issue_id: Option<&'a str>,
}

/// `prefixSessionKeyForAgent` —— 在 sessionKey 前加 `agent:<id>:` 前缀，
/// 避免 sessionKey 跨 agent 复用造成的状态串扰。
pub fn prefix_session_key_for_agent(session_key: &str, agent_id: Option<&str>) -> String {
    match agent_id {
        None => session_key.to_owned(),
        Some(id) if id.is_empty() => session_key.to_owned(),
        Some(_) if session_key.starts_with("agent:") => session_key.to_owned(),
        Some(id) => format!("agent:{id}:{session_key}"),
    }
}

/// `resolveSessionKey` — 真正的解析策略逻辑。
///
/// 优先级（按策略）：
/// - `fixed`:  `configuredSessionKey`（trim 非空）否则 DEFAULT_SESSION_KEY
/// - `issue`:  `issueId`（trim 非空）否则 `configuredSessionKey`（trim 非空）否则 DEFAULT_SESSION_KEY
/// - `run`:    `runId`
///
/// 然后再 `:prefix_session_key_for_agent`（若 `agent_id` 给定）。
pub fn resolve_session_key(input: &SessionKeyInput<'_>) -> String {
    let trimmed_configured = input
        .configured_session_key
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let trimmed_issue = input.issue_id.map(str::trim).filter(|s| !s.is_empty());

    let raw: String = match input.strategy {
        SessionKeyStrategy::Fixed => trimmed_configured.unwrap_or(DEFAULT_SESSION_KEY).to_owned(),
        SessionKeyStrategy::Issue => trimmed_issue
            .or(trimmed_configured)
            .unwrap_or(DEFAULT_SESSION_KEY)
            .to_owned(),
        SessionKeyStrategy::Run => input.run_id.to_owned(),
    };
    prefix_session_key_for_agent(&raw, input.agent_id)
}

/// 决策：当前 session key 是否已 agent-prefixed。
pub fn is_agent_prefixed(session_key: &str) -> bool {
    session_key.starts_with("agent:")
}

/// Helper — 把策略序列化为 config 时的字符串（trim + lowercase 后）。
pub fn normalize_strategy_string(value: &str) -> &'static str {
    let normalized = value.trim().to_lowercase();
    match normalized.as_str() {
        "fixed" => "fixed",
        "issue" => "issue",
        "run" => "run",
        _ => DEFAULT_SESSION_KEY_STRATEGY,
    }
}

/// Helper — 列出所有合法策略字符串。
pub fn known_strategies() -> &'static [&'static str] {
    VALID_SESSION_KEY_STRATEGIES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(
        strategy: SessionKeyStrategy,
        configured: Option<&'a str>,
        agent: Option<&'a str>,
        run: &'a str,
        issue: Option<&'a str>,
    ) -> SessionKeyInput<'a> {
        SessionKeyInput {
            strategy,
            configured_session_key: configured,
            agent_id: agent,
            run_id: run,
            issue_id: issue,
        }
    }

    #[test]
    fn strategy_from_loose_normalizes_case_and_unknown() {
        assert_eq!(
            SessionKeyStrategy::from_loose("FIXED"),
            SessionKeyStrategy::Fixed
        );
        assert_eq!(
            SessionKeyStrategy::from_loose(" Run "),
            SessionKeyStrategy::Run
        );
        assert_eq!(
            SessionKeyStrategy::from_loose("garbage"),
            SessionKeyStrategy::Issue
        );
        assert_eq!(
            SessionKeyStrategy::from_loose(""),
            SessionKeyStrategy::Issue
        );
    }

    #[test]
    fn strategy_default_is_issue() {
        assert_eq!(SessionKeyStrategy::default(), SessionKeyStrategy::Issue);
    }

    #[test]
    fn strategy_as_str_returns_canonical_name() {
        assert_eq!(SessionKeyStrategy::Fixed.as_str(), "fixed");
        assert_eq!(SessionKeyStrategy::Issue.as_str(), "issue");
        assert_eq!(SessionKeyStrategy::Run.as_str(), "run");
    }

    #[test]
    fn prefix_session_key_for_agent_skips_when_no_agent() {
        assert_eq!(prefix_session_key_for_agent("paperclip", None), "paperclip");
        assert_eq!(
            prefix_session_key_for_agent("paperclip", Some("")),
            "paperclip"
        );
    }

    #[test]
    fn prefix_session_key_for_agent_skips_when_already_prefixed() {
        assert_eq!(
            prefix_session_key_for_agent("agent:a-1:foo", Some("a-1")),
            "agent:a-1:foo"
        );
    }

    #[test]
    fn prefix_session_key_for_agent_adds_prefix() {
        assert_eq!(
            prefix_session_key_for_agent("paperclip", Some("a-1")),
            "agent:a-1:paperclip"
        );
    }

    #[test]
    fn resolve_fixed_prefers_configured_session_key() {
        let key = resolve_session_key(&input(
            SessionKeyStrategy::Fixed,
            Some("my-thread"),
            Some("a-1"),
            "run-1",
            Some("iss-1"),
        ));
        assert_eq!(key, "agent:a-1:my-thread");
    }

    #[test]
    fn resolve_fixed_uses_default_when_no_config() {
        let key = resolve_session_key(&input(
            SessionKeyStrategy::Fixed,
            None,
            Some("a-1"),
            "run-1",
            None,
        ));
        assert_eq!(key, "agent:a-1:paperclip");
    }

    #[test]
    fn resolve_issue_prefers_issue_id() {
        let key = resolve_session_key(&input(
            SessionKeyStrategy::Issue,
            Some("cfg"),
            Some("a-1"),
            "run-1",
            Some("iss-9"),
        ));
        assert_eq!(key, "agent:a-1:iss-9");
    }

    #[test]
    fn resolve_issue_falls_back_to_configured_then_default() {
        let key = resolve_session_key(&input(
            SessionKeyStrategy::Issue,
            Some("cfg"),
            Some("a-1"),
            "run-1",
            None,
        ));
        assert_eq!(key, "agent:a-1:cfg");
        let key2 = resolve_session_key(&input(
            SessionKeyStrategy::Issue,
            None,
            Some("a-1"),
            "run-1",
            None,
        ));
        assert_eq!(key2, "agent:a-1:paperclip");
    }

    #[test]
    fn resolve_run_uses_run_id_only() {
        let key = resolve_session_key(&input(
            SessionKeyStrategy::Run,
            Some("cfg"),
            Some("a-1"),
            "run-99",
            Some("iss-9"),
        ));
        assert_eq!(key, "agent:a-1:run-99");
    }

    #[test]
    fn resolve_without_agent_skips_prefix() {
        let key = resolve_session_key(&input(
            SessionKeyStrategy::Issue,
            Some("cfg"),
            None,
            "run-1",
            None,
        ));
        assert_eq!(key, "cfg");
    }

    #[test]
    fn is_agent_prefixed_true_when_starting_with_agent() {
        assert!(is_agent_prefixed("agent:a-1:foo"));
        assert!(!is_agent_prefixed("a-1:foo"));
        assert!(!is_agent_prefixed(""));
    }

    #[test]
    fn normalize_strategy_string_returns_canonical() {
        assert_eq!(normalize_strategy_string("fixed"), "fixed");
        assert_eq!(normalize_strategy_string("  Issue "), "issue");
        assert_eq!(
            normalize_strategy_string("anything"),
            DEFAULT_SESSION_KEY_STRATEGY
        );
    }

    #[test]
    fn known_strategies_lists_three() {
        let s = known_strategies();
        assert_eq!(s.len(), 3);
        assert!(s.contains(&"fixed"));
        assert!(s.contains(&"issue"));
        assert!(s.contains(&"run"));
    }
}
