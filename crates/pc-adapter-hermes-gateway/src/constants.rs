//! Hermes gateway adapter 常量（对齐 Node
//! `packages/adapters/hermes/src/gateway/shared/constants.ts`）。

#![allow(dead_code)]

/// Adapter 类型标识。
pub const ADAPTER_TYPE: &str = "hermes_gateway";
/// Adapter UI 标签。
pub const ADAPTER_LABEL: &str = "Hermes Gateway";

/// 默认 timeout (秒)。
pub const DEFAULT_TIMEOUT_SEC: u64 = 600;

/// SSE 事件流断开后重连延迟 (毫秒)。
pub const DEFAULT_EVENT_RECONNECT_MS: u64 = 2_000;

/// 轮询间隔 (毫秒)。
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

/// SIGTERM → SIGKILL 之间宽限 (毫秒)。
pub const STOP_GRACE_MS: u64 = 10_000;

/// Session key 策略（对齐 Node `SessionKeyStrategy`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKeyStrategy {
    /// Issue-scoped — 默认。防止跨任务 memory bleed。
    Issue,
    /// Agent-scoped — 同一 agent 跨 issue 共享 session。
    Agent,
    /// Run-scoped — 每次 run 一个独立 session。
    Run,
    /// 完全无 session。
    None,
}

impl SessionKeyStrategy {
    /// 从字符串解析（与 Node `cfgString(config.sessionKeyStrategy)` 等价）。
    pub fn from_config_str(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "issue" => Some(Self::Issue),
            "agent" => Some(Self::Agent),
            "run" => Some(Self::Run),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Agent => "agent",
            Self::Run => "run",
            Self::None => "none",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_session_key_strategy_handles_known_values() {
        assert_eq!(
            SessionKeyStrategy::from_config_str("issue"),
            Some(SessionKeyStrategy::Issue)
        );
        assert_eq!(
            SessionKeyStrategy::from_config_str("AGENT"),
            Some(SessionKeyStrategy::Agent)
        );
        assert_eq!(
            SessionKeyStrategy::from_config_str("Run"),
            Some(SessionKeyStrategy::Run)
        );
        assert_eq!(
            SessionKeyStrategy::from_config_str("none"),
            Some(SessionKeyStrategy::None)
        );
        assert_eq!(SessionKeyStrategy::from_config_str(""), None);
        assert_eq!(SessionKeyStrategy::from_config_str("unknown"), None);
    }
}
