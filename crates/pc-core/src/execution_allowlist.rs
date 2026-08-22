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
// Tests — mirror paperclip/server/src/services/execution-allowlist.test.ts 1:1
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Fixtures — mirror Node top-level fixtures (localEnv, kubernetesEnv,
    // fakeSandboxEnv, sshEnv).
    // -----------------------------------------------------------------------

    fn local_env() -> ExecutionEnvironmentCandidate {
        ExecutionEnvironmentCandidate {
            driver: "local".to_string(),
            provider: None,
        }
    }

    fn kubernetes_env() -> ExecutionEnvironmentCandidate {
        ExecutionEnvironmentCandidate {
            driver: "sandbox".to_string(),
            provider: Some(KUBERNETES_PROVIDER_KEY.to_string()),
        }
    }

    fn fake_sandbox_env() -> ExecutionEnvironmentCandidate {
        ExecutionEnvironmentCandidate {
            driver: "sandbox".to_string(),
            provider: Some("fake".to_string()),
        }
    }

    fn ssh_env() -> ExecutionEnvironmentCandidate {
        ExecutionEnvironmentCandidate {
            driver: "ssh".to_string(),
            provider: None,
        }
    }

    fn sandbox_no_provider_env() -> ExecutionEnvironmentCandidate {
        ExecutionEnvironmentCandidate {
            driver: "sandbox".to_string(),
            provider: None,
        }
    }

    fn any_policy() -> Option<ExecutionPolicy> {
        Some(ExecutionPolicy {
            execution_mode: Some(ExecutionMode::Any),
        })
    }

    fn kubernetes_policy() -> Option<ExecutionPolicy> {
        Some(ExecutionPolicy {
            execution_mode: Some(ExecutionMode::Kubernetes),
        })
    }

    fn empty_policy() -> Option<ExecutionPolicy> {
        Some(ExecutionPolicy::default())
    }

    fn policy_with_undefined_mode() -> Option<ExecutionPolicy> {
        Some(ExecutionPolicy {
            execution_mode: None,
        })
    }

    // -----------------------------------------------------------------------
    // describe('executionMode "any" (unrestricted, default)')
    // -----------------------------------------------------------------------

    /// Node: `it("allows the local environment")`
    #[test]
    fn any_mode_allows_local_environment() {
        let result = evaluate_execution_allowlist(any_policy().as_ref(), &local_env());
        assert!(result.is_allowed());
    }

    /// Node: `it("allows the kubernetes sandbox environment")`
    #[test]
    fn any_mode_allows_kubernetes_sandbox() {
        let result = evaluate_execution_allowlist(any_policy().as_ref(), &kubernetes_env());
        assert!(result.is_allowed());
    }

    /// Node: `it("allows a non-kubernetes sandbox environment")`
    #[test]
    fn any_mode_allows_non_kubernetes_sandbox() {
        let result = evaluate_execution_allowlist(any_policy().as_ref(), &fake_sandbox_env());
        assert!(result.is_allowed());
    }

    /// Node: `it("treats absent executionMode as unrestricted")` — both `{}`
    /// and `executionMode: undefined` collapse to the default policy.
    #[test]
    fn absent_execution_mode_is_unrestricted() {
        assert!(evaluate_execution_allowlist(empty_policy().as_ref(), &local_env()).is_allowed());
        assert!(
            evaluate_execution_allowlist(policy_with_undefined_mode().as_ref(), &local_env())
                .is_allowed()
        );
    }

    /// No policy at all → unrestricted.
    #[test]
    fn missing_policy_is_unrestricted() {
        assert!(evaluate_execution_allowlist(None, &local_env()).is_allowed());
        assert!(evaluate_execution_allowlist(None, &kubernetes_env()).is_allowed());
        assert!(evaluate_execution_allowlist(None, &fake_sandbox_env()).is_allowed());
    }

    // -----------------------------------------------------------------------
    // describe('executionMode "kubernetes" (forced sandbox)')
    // -----------------------------------------------------------------------

    /// Node: `it("allows ONLY a kubernetes sandbox_provider environment")`
    #[test]
    fn kubernetes_mode_allows_only_k8s_sandbox() {
        let result = evaluate_execution_allowlist(kubernetes_policy().as_ref(), &kubernetes_env());
        assert!(result.is_allowed());
    }

    /// Node: `it("DENIES the local environment")` with reason/k8s regex + driver.
    #[test]
    fn kubernetes_mode_denies_local_environment() {
        let result = evaluate_execution_allowlist(kubernetes_policy().as_ref(), &local_env());
        assert!(!result.is_allowed());
        match result {
            ExecutionAllowlistDecision::False {
                reason,
                denied_driver,
                ..
            } => {
                // Node: `expect(result.reason).toMatch(/kubernetes/i)`
                assert!(reason.contains("kubernetes") || reason.contains("Kubernetes"));
                // Node: `expect(result.deniedDriver).toBe("local")`
                assert_eq!(denied_driver, "local");
            }
            _ => panic!("expected deny decision"),
        }
    }

    /// Node: `it("DENIES an ssh environment")`
    #[test]
    fn kubernetes_mode_denies_ssh_environment() {
        let result = evaluate_execution_allowlist(kubernetes_policy().as_ref(), &ssh_env());
        assert!(!result.is_allowed());
    }

    /// Node: `it("DENIES a non-kubernetes sandbox provider (e.g. fake)")` with
    /// `deniedProvider` assertion.
    #[test]
    fn kubernetes_mode_denies_non_kubernetes_sandbox_provider() {
        let result = evaluate_execution_allowlist(kubernetes_policy().as_ref(), &fake_sandbox_env());
        assert!(!result.is_allowed());
        match result {
            ExecutionAllowlistDecision::False {
                denied_provider, ..
            } => {
                assert_eq!(denied_provider.as_deref(), Some("fake"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    /// Node: `it("DENIES a sandbox driver with no provider")`
    #[test]
    fn kubernetes_mode_denies_sandbox_without_provider() {
        let result = evaluate_execution_allowlist(
            kubernetes_policy().as_ref(),
            &sandbox_no_provider_env(),
        );
        assert!(!result.is_allowed());
    }

    // -----------------------------------------------------------------------
    // describe("isExecutionForcedToKubernetes helper")
    // -----------------------------------------------------------------------

    /// Node: `it("reflects the policy")` — three sequential assertions.
    #[test]
    fn is_execution_forced_to_kubernetes_reflects_policy() {
        assert!(is_execution_forced_to_kubernetes(kubernetes_policy().as_ref()));
        assert!(!is_execution_forced_to_kubernetes(any_policy().as_ref()));
        assert!(!is_execution_forced_to_kubernetes(empty_policy().as_ref()));
    }

    /// Helper tolerates a fully-absent policy without panicking.
    #[test]
    fn is_execution_forced_to_kubernetes_none_policy() {
        assert!(!is_execution_forced_to_kubernetes(None));
    }

    // -----------------------------------------------------------------------
    // is_kubernetes_sandbox_environment — guard the type-level predicate
    // directly (Node does not cover this, but the predicate is part of the
    // public surface).
    // -----------------------------------------------------------------------

    #[test]
    fn kubernetes_sandbox_predicate_requires_both_driver_and_provider() {
        let mut c = kubernetes_env();
        c.driver = "local".to_string();
        assert!(!is_kubernetes_sandbox_environment(&c));

        let mut c = kubernetes_env();
        c.provider = Some("docker".to_string());
        assert!(!is_kubernetes_sandbox_environment(&c));

        let mut c = kubernetes_env();
        c.provider = None;
        assert!(!is_kubernetes_sandbox_environment(&c));

        assert!(is_kubernetes_sandbox_environment(&kubernetes_env()));
    }

    // -----------------------------------------------------------------------
    // Decision payload invariants — exercise fields the Node suite doesn't
    // touch directly but that downstream consumers rely on.
    // -----------------------------------------------------------------------

    #[test]
    fn deny_decision_carries_denied_driver_and_provider_for_sandbox_driver() {
        let d = evaluate_execution_allowlist(kubernetes_policy().as_ref(), &fake_sandbox_env());
        match d {
            ExecutionAllowlistDecision::False {
                denied_driver,
                denied_provider,
                reason,
            } => {
                assert_eq!(denied_driver, "sandbox");
                assert_eq!(denied_provider.as_deref(), Some("fake"));
                assert!(reason.contains("sandbox provider"));
                assert!(reason.contains("fake"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    #[test]
    fn deny_decision_reports_none_for_provider_when_missing() {
        let d = evaluate_execution_allowlist(
            kubernetes_policy().as_ref(),
            &sandbox_no_provider_env(),
        );
        match d {
            ExecutionAllowlistDecision::False {
                denied_provider,
                reason,
                ..
            } => {
                assert_eq!(denied_provider, None);
                assert!(reason.contains("(none)"));
            }
            _ => panic!("expected deny decision"),
        }
    }

    #[test]
    fn deny_decision_for_ssh_names_driver_in_reason() {
        let d = evaluate_execution_allowlist(kubernetes_policy().as_ref(), &ssh_env());
        match d {
            ExecutionAllowlistDecision::False {
                denied_driver,
                reason,
                ..
            } => {
                assert_eq!(denied_driver, "ssh");
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
        assert!(!is_execution_forced_to_kubernetes(Some(&ExecutionPolicy::default())));
    }
}
