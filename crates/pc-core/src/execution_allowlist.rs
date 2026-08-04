//! 执行环境白名单守卫（安全关键）
//!
//! 对齐 Node `services/execution-allowlist.ts`：
//! - 在共享云实例上，强制把不可信租户 agent 限制在 Kubernetes 沙箱 provider 上
//! - 拒绝 local / in-process / ssh 等非沙箱执行路径
//! - 纯函数无副作用，便于单测；与 DB / heartbeat 解耦
//!
//! 设计：
//! - 类型 `ExecutionPolicy` / `ExecutionEnvironmentCandidate` / `ExecutionAllowlistDecision`
//! - 函数 `is_execution_forced_to_kubernetes` / `is_kubernetes_sandbox_environment` / `evaluate_execution_allowlist`
//! - 常量 `KUBERNETES_PROVIDER_KEY = "kubernetes"`

use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// 一方 Kubernetes 沙箱 provider 的 key（即 plugin driverKey）。
pub const KUBERNETES_PROVIDER_KEY: &str = "kubernetes";

// ============================================================================
// Types
// ============================================================================

/// 实例级执行策略（来自 instance general settings）。
///
/// - `execution_mode == Some("any")` 或 `None`：不限制（默认，单租户 / 本地信任行为）
/// - `execution_mode == Some("kubernetes")`：强制 Kubernetes 沙箱；拒绝 local / ssh / 非 k8s 沙箱
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    #[serde(rename = "executionMode", skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<ExecutionMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    Kubernetes,
    Any,
}

/// 候选执行环境的最小信息。
/// - `driver`：核心 `EnvironmentDriver`
/// - `provider`：沙箱 provider key（即 plugin driverKey），仅当 `driver == "sandbox"` 时相关，否则为 `None`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEnvironmentCandidate {
    pub driver: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// 白名单决策结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "allowed", rename_all = "lowercase")]
pub enum ExecutionAllowlistDecision {
    True,
    False {
        reason: String,
        denied_driver: String,
        denied_provider: Option<String>,
    },
}

impl ExecutionAllowlistDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::True)
    }
}

// ============================================================================
// Public API
// ============================================================================

/// `true` 当策略强制所有执行走 Kubernetes 沙箱。
pub fn is_execution_forced_to_kubernetes(policy: Option<&ExecutionPolicy>) -> bool {
    matches!(
        policy.and_then(|p| p.execution_mode),
        Some(ExecutionMode::Kubernetes)
    )
}

/// `true` 当候选环境是 Kubernetes 沙箱 provider（`driver == "sandbox"` 且 `provider == "kubernetes"`）。
pub fn is_kubernetes_sandbox_environment(candidate: &ExecutionEnvironmentCandidate) -> bool {
    candidate.driver == "sandbox" && candidate.provider.as_deref() == Some(KUBERNETES_PROVIDER_KEY)
}

/// 决定候选环境是否可在给定策略下执行。
///
/// - 策略非 kubernetes → 全部允许
/// - 策略 kubernetes 且候选是 k8s 沙箱 → 允许
/// - 策略 kubernetes 且候选非 k8s 沙箱 → 拒绝（含详细原因）
pub fn evaluate_execution_allowlist(
    policy: Option<&ExecutionPolicy>,
    candidate: &ExecutionEnvironmentCandidate,
) -> ExecutionAllowlistDecision {
    if !is_execution_forced_to_kubernetes(policy) {
        return ExecutionAllowlistDecision::True;
    }

    if is_kubernetes_sandbox_environment(candidate) {
        return ExecutionAllowlistDecision::True;
    }

    let provider = candidate.provider.clone();
    let target = if candidate.driver == "sandbox" {
        format!(
            "sandbox provider \"{}\"",
            provider.as_deref().unwrap_or("(none)")
        )
    } else {
        format!("\"{}\" driver", candidate.driver)
    };

    ExecutionAllowlistDecision::False {
        reason: format!(
            "Instance execution policy requires the Kubernetes sandbox provider \
             (executionMode=kubernetes), but the resolved environment uses the {target}. \
             Untrusted execution on a non-Kubernetes environment is refused."
        ),
        denied_driver: candidate.driver.clone(),
        denied_provider: provider,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(mode: Option<ExecutionMode>) -> Option<ExecutionPolicy> {
        mode.map(|execution_mode| ExecutionPolicy {
            execution_mode: Some(execution_mode),
        })
    }

    fn k8s_candidate() -> ExecutionEnvironmentCandidate {
        ExecutionEnvironmentCandidate {
            driver: "sandbox".to_string(),
            provider: Some(KUBERNETES_PROVIDER_KEY.to_string()),
        }
    }

    fn local_candidate() -> ExecutionEnvironmentCandidate {
        ExecutionEnvironmentCandidate {
            driver: "local".to_string(),
            provider: None,
        }
    }

    // -----------------------------------------------------------------------
    // is_execution_forced_to_kubernetes
    // -----------------------------------------------------------------------

    #[test]
    fn forced_to_kubernetes_only_when_explicit() {
        assert!(!is_execution_forced_to_kubernetes(None));
        assert!(!is_execution_forced_to_kubernetes(policy(None).as_ref()));
        assert!(!is_execution_forced_to_kubernetes(
            policy(Some(ExecutionMode::Any)).as_ref()
        ));
        assert!(is_execution_forced_to_kubernetes(
            policy(Some(ExecutionMode::Kubernetes)).as_ref()
        ));
    }

    // -----------------------------------------------------------------------
    // is_kubernetes_sandbox_environment
    // -----------------------------------------------------------------------

    #[test]
    fn k8s_sandbox_requires_both_driver_and_provider() {
        // 必须 driver=sandbox
        let mut c = k8s_candidate();
        c.driver = "local".to_string();
        assert!(!is_kubernetes_sandbox_environment(&c));

        // 必须 provider=kubernetes
        let mut c = k8s_candidate();
        c.provider = Some("docker".to_string());
        assert!(!is_kubernetes_sandbox_environment(&c));

        // provider 缺失 → false
        let mut c = k8s_candidate();
        c.provider = None;
        assert!(!is_kubernetes_sandbox_environment(&c));

        // 完全匹配
        assert!(is_kubernetes_sandbox_environment(&k8s_candidate()));
    }

    // -----------------------------------------------------------------------
    // evaluate_execution_allowlist: any policy
    // -----------------------------------------------------------------------

    #[test]
    fn any_policy_allows_everything() {
        assert!(evaluate_execution_allowlist(None, &local_candidate()).is_allowed());
        assert!(evaluate_execution_allowlist(
            policy(Some(ExecutionMode::Any)).as_ref(),
            &local_candidate()
        )
        .is_allowed());
        assert!(evaluate_execution_allowlist(
            policy(Some(ExecutionMode::Any)).as_ref(),
            &k8s_candidate()
        )
        .is_allowed());
    }

    // -----------------------------------------------------------------------
    // evaluate_execution_allowlist: kubernetes policy
    // -----------------------------------------------------------------------

    #[test]
    fn kubernetes_policy_allows_k8s_sandbox() {
        let d = evaluate_execution_allowlist(
            policy(Some(ExecutionMode::Kubernetes)).as_ref(),
            &k8s_candidate(),
        );
        assert!(d.is_allowed());
    }

    #[test]
    fn kubernetes_policy_denies_local() {
        let d = evaluate_execution_allowlist(
            policy(Some(ExecutionMode::Kubernetes)).as_ref(),
            &local_candidate(),
        );
        assert!(!d.is_allowed());
        match d {
            ExecutionAllowlistDecision::False {
                reason,
                denied_driver,
                denied_provider,
            } => {
                assert_eq!(denied_driver, "local");
                assert_eq!(denied_provider, None);
                assert!(reason.contains("Kubernetes"));
                assert!(reason.contains("local"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    #[test]
    fn kubernetes_policy_denies_non_k8s_sandbox() {
        let candidate = ExecutionEnvironmentCandidate {
            driver: "sandbox".to_string(),
            provider: Some("docker".to_string()),
        };
        let d = evaluate_execution_allowlist(
            policy(Some(ExecutionMode::Kubernetes)).as_ref(),
            &candidate,
        );
        assert!(!d.is_allowed());
        match d {
            ExecutionAllowlistDecision::False {
                reason,
                denied_driver,
                denied_provider,
            } => {
                assert_eq!(denied_driver, "sandbox");
                assert_eq!(denied_provider.as_deref(), Some("docker"));
                assert!(reason.contains("sandbox provider"));
                assert!(reason.contains("docker"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    #[test]
    fn kubernetes_policy_denies_sandbox_without_provider() {
        let candidate = ExecutionEnvironmentCandidate {
            driver: "sandbox".to_string(),
            provider: None,
        };
        let d = evaluate_execution_allowlist(
            policy(Some(ExecutionMode::Kubernetes)).as_ref(),
            &candidate,
        );
        assert!(!d.is_allowed());
        match d {
            ExecutionAllowlistDecision::False {
                reason,
                denied_provider,
                ..
            } => {
                assert_eq!(denied_provider, None);
                assert!(reason.contains("(none)"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    #[test]
    fn kubernetes_policy_denies_ssh() {
        let candidate = ExecutionEnvironmentCandidate {
            driver: "ssh".to_string(),
            provider: None,
        };
        let d = evaluate_execution_allowlist(
            policy(Some(ExecutionMode::Kubernetes)).as_ref(),
            &candidate,
        );
        assert!(!d.is_allowed());
        match d {
            ExecutionAllowlistDecision::False { reason, .. } => {
                assert!(reason.contains("ssh"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    // -----------------------------------------------------------------------
    // Serde round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn policy_serde_round_trip() {
        let original = ExecutionPolicy {
            execution_mode: Some(ExecutionMode::Kubernetes),
        };
        let json = serde_json::to_string(&original).unwrap();
        let back: ExecutionPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(original, back);
    }

    #[test]
    fn decision_serde_round_trip() {
        let allowed = ExecutionAllowlistDecision::True;
        let json = serde_json::to_string(&allowed).unwrap();
        let back: ExecutionAllowlistDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(allowed, back);

        let denied = ExecutionAllowlistDecision::False {
            reason: "test".to_string(),
            denied_driver: "local".to_string(),
            denied_provider: None,
        };
        let json = serde_json::to_string(&denied).unwrap();
        let back: ExecutionAllowlistDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(denied, back);
    }

    // -----------------------------------------------------------------------
    // Edge case: default policy
    // -----------------------------------------------------------------------

    #[test]
    fn default_policy_is_any() {
        assert_eq!(
            ExecutionPolicy::default(),
            ExecutionPolicy {
                execution_mode: None
            }
        );
        assert!(!is_execution_forced_to_kubernetes(Some(
            &ExecutionPolicy::default()
        )));
    }
}
