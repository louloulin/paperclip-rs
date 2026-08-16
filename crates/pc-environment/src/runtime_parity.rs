//! Node `environment-runtime.ts` 1:1 parity 包装 (R671)
//!
//! 对应 Node 上游 `server/src/services/environment-runtime.ts` 的两个 pure function：
//! - `buildEnvironmentLeaseContext({ persistedExecutionWorkspace })`
//! - `findReusableSandboxLeaseId({ config, leases })`
//!
//! 这两个函数在 Node 中被 environment-runtime service 用来：
//! 1. 构造 lease context（携带 execution workspace id / mode）
//! 2. 从历史 leases 中匹配可复用的 sandbox lease id
//!
//! Rust 端没有对应的纯函数包装（`pc-environment::service` 是 stateful service），
//! 这里以纯函数 + 强类型的形式提供 1:1 等价实现，便于跨 crate 复用。

use serde::{Deserialize, Serialize};

/// Execution workspace 的最小投影 —— 与 Node `Pick<ExecutionWorkspace, "id" | "mode">` 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionWorkspaceRef {
    pub id: uuid::Uuid,
    pub mode: String,
}

/// Lease context —— 与 Node `buildEnvironmentLeaseContext()` 返回值 1:1。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentLeaseContext {
    pub execution_workspace_id: Option<uuid::Uuid>,
    pub execution_workspace_mode: Option<String>,
}

/// 构造 lease context（Node 1:1）。
///
/// 当 `persistedExecutionWorkspace` 为 `None` 时，两个字段均为 `None`；
/// 否则携带 `id` / `mode`。
pub fn build_environment_lease_context(
    persisted_execution_workspace: Option<&ExecutionWorkspaceRef>,
) -> EnvironmentLeaseContext {
    match persisted_execution_workspace {
        Some(ws) => EnvironmentLeaseContext {
            execution_workspace_id: Some(ws.id),
            execution_workspace_mode: Some(ws.mode.clone()),
        },
        None => EnvironmentLeaseContext::default(),
    }
}

// -----------------------------------------------------------------------------
// Reusable sandbox lease id
// -----------------------------------------------------------------------------

/// Sandbox lease 最小投影 —— Node `Pick<EnvironmentLease, "providerLeaseId" | "metadata">`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxLeaseCandidate {
    pub provider_lease_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Sandbox config 最小投影（仅需 provider 标识字段，Node 端是 `SandboxEnvironmentConfig`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfigRef {
    pub provider: String,
    /// 配置指纹 / fingerprint（用于匹配相同配置的 lease）。
    pub fingerprint: Option<String>,
}

/// 在历史 leases 中查找可复用的 sandbox lease id（Node 1:1）。
///
/// 匹配规则：
/// 1. `provider_lease_id` 必须非空
/// 2. `metadata.provider` 等于 `config.provider`
/// 3. 若 `config.fingerprint` 给定，`metadata.fingerprint` 必须等于它
///
/// 返回第一个匹配项的 `provider_lease_id`，否则 `None`。
pub fn find_reusable_sandbox_lease_id(
    config: &SandboxConfigRef,
    leases: &[SandboxLeaseCandidate],
) -> Option<String> {
    for lease in leases {
        let Some(provider_lease_id) = lease.provider_lease_id.as_ref() else {
            continue;
        };
        let Some(metadata) = lease.metadata.as_ref() else {
            continue;
        };
        let provider_matches = metadata
            .get("provider")
            .and_then(|v| v.as_str())
            .map(|s| s == config.provider)
            .unwrap_or(false);
        if !provider_matches {
            continue;
        }
        if let Some(expected_fp) = config.fingerprint.as_ref() {
            let actual_fp = metadata
                .get("fingerprint")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if actual_fp != expected_fp {
                continue;
            }
        }
        return Some(provider_lease_id.clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r671_build_lease_context_none_yields_nulls() {
        let ctx = build_environment_lease_context(None);
        assert_eq!(ctx.execution_workspace_id, None);
        assert_eq!(ctx.execution_workspace_mode, None);
    }

    #[test]
    fn r671_build_lease_context_some_yields_id_and_mode() {
        let ws = ExecutionWorkspaceRef {
            id: uuid::Uuid::nil(),
            mode: "ephemeral".to_string(),
        };
        let ctx = build_environment_lease_context(Some(&ws));
        assert_eq!(ctx.execution_workspace_id, Some(uuid::Uuid::nil()));
        assert_eq!(ctx.execution_workspace_mode.as_deref(), Some("ephemeral"));
    }

    #[test]
    fn r671_find_reusable_no_match_returns_none() {
        let config = SandboxConfigRef {
            provider: "modal".to_string(),
            fingerprint: None,
        };
        let leases: Vec<SandboxLeaseCandidate> = vec![];
        assert!(find_reusable_sandbox_lease_id(&config, &leases).is_none());
    }

    #[test]
    fn r671_find_reusable_provider_match_returns_id() {
        let config = SandboxConfigRef {
            provider: "modal".to_string(),
            fingerprint: None,
        };
        let leases = vec![
            SandboxLeaseCandidate {
                provider_lease_id: None,
                metadata: Some(serde_json::json!({"provider": "modal"})),
            },
            SandboxLeaseCandidate {
                provider_lease_id: Some("lease-123".to_string()),
                metadata: Some(serde_json::json!({"provider": "modal"})),
            },
        ];
        assert_eq!(
            find_reusable_sandbox_lease_id(&config, &leases),
            Some("lease-123".to_string())
        );
    }

    #[test]
    fn r671_find_reusable_provider_mismatch_skipped() {
        let config = SandboxConfigRef {
            provider: "modal".to_string(),
            fingerprint: None,
        };
        let leases = vec![SandboxLeaseCandidate {
            provider_lease_id: Some("other-lease".to_string()),
            metadata: Some(serde_json::json!({"provider": "fly"})),
        }];
        assert!(find_reusable_sandbox_lease_id(&config, &leases).is_none());
    }

    #[test]
    fn r671_find_reusable_fingerprint_match_required() {
        let config = SandboxConfigRef {
            provider: "modal".to_string(),
            fingerprint: Some("fp-v1".to_string()),
        };
        // lease with same provider but different fingerprint → skip
        let leases = vec![
            SandboxLeaseCandidate {
                provider_lease_id: Some("wrong-fp".to_string()),
                metadata: Some(serde_json::json!({"provider": "modal", "fingerprint": "fp-v0"})),
            },
            SandboxLeaseCandidate {
                provider_lease_id: Some("right-fp".to_string()),
                metadata: Some(serde_json::json!({"provider": "modal", "fingerprint": "fp-v1"})),
            },
        ];
        assert_eq!(
            find_reusable_sandbox_lease_id(&config, &leases),
            Some("right-fp".to_string())
        );
    }

    #[test]
    fn r671_find_reusable_missing_metadata_skipped() {
        let config = SandboxConfigRef {
            provider: "modal".to_string(),
            fingerprint: None,
        };
        let leases = vec![SandboxLeaseCandidate {
            provider_lease_id: Some("no-meta".to_string()),
            metadata: None,
        }];
        assert!(find_reusable_sandbox_lease_id(&config, &leases).is_none());
    }
}
