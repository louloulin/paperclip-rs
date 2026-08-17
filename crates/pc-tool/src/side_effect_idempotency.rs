#![forbid(unsafe_code)]

//! Side effect idempotency key + audit outcome + risk rank.
//! R708: Direct port of tool-access-policy.ts::sideEffectIdempotencyKey + auditOutcome + riskRank.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Tool access decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolAccessDecision {
    Allow,
    RequireApproval,
    DeferRuntime,
    Deny,
}

/// Audit outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditOutcome {
    Pending,
    Success,
    Denied,
    Timeout,
}

impl AuditOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Denied => "denied",
            Self::Timeout => "timeout",
        }
    }
}

/// Risk rank: read=1, write=2, destructive=3, critical=4, unknown=0.
pub fn risk_rank(level: &str) -> u32 {
    match level {
        "read" | "low" => 1,
        "write" | "medium" => 2,
        "destructive" | "high" => 3,
        "critical" => 4,
        _ => 0,
    }
}

/// Map decision to audit outcome.
/// Node auditOutcome(decision) 1:1 parity.
pub fn audit_outcome(decision: ToolAccessDecision) -> AuditOutcome {
    match decision {
        ToolAccessDecision::Allow => AuditOutcome::Success,
        ToolAccessDecision::RequireApproval => AuditOutcome::Pending,
        ToolAccessDecision::DeferRuntime => AuditOutcome::Timeout,
        ToolAccessDecision::Deny => AuditOutcome::Denied,
    }
}

/// Idempotency key context.
#[derive(Debug, Clone, Default)]
pub struct IdempotencyContext {
    pub company_id: Option<String>,
    pub run_id: Option<String>,
    pub issue_id: Option<String>,
    pub application_id: Option<String>,
    pub connection_id: Option<String>,
    pub catalog_entry_id: Option<String>,
    pub tool_name: Option<String>,
}

/// SHA-256 side effect idempotency key.
/// Node sideEffectIdempotencyKey(ctx, argsHash) 1:1 parity.
pub fn side_effect_idempotency_key(ctx: &IdempotencyContext, arguments_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ctx.company_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(ctx.run_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(ctx.issue_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(ctx.application_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(ctx.connection_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(ctx.catalog_entry_id.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(ctx.tool_name.as_deref().unwrap_or("").as_bytes());
    hasher.update(b"|");
    hasher.update(arguments_hash.as_bytes());
    let result = hasher.finalize();
    format!("side_effect:{}", format!("{:x}", result))
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn ctx() -> IdempotencyContext {
        IdempotencyContext {
            company_id: Some("c-1".into()),
            run_id: Some("r-1".into()),
            issue_id: Some("i-1".into()),
            application_id: Some("app-1".into()),
            connection_id: Some("conn-1".into()),
            catalog_entry_id: Some("cat-1".into()),
            tool_name: Some("foo".into()),
        }
    }

    #[test]
    fn idempotency_key_deterministic() {
        let k1 = side_effect_idempotency_key(&ctx(), "h1");
        let k2 = side_effect_idempotency_key(&ctx(), "h1");
        assert_eq!(k1, k2);
    }

    #[test]
    fn idempotency_key_changes_with_args_hash() {
        let k1 = side_effect_idempotency_key(&ctx(), "h1");
        let k2 = side_effect_idempotency_key(&ctx(), "h2");
        assert_ne!(k1, k2);
    }

    #[test]
    fn idempotency_key_changes_with_context() {
        let mut c1 = ctx();
        c1.tool_name = Some("foo".into());
        let mut c2 = ctx();
        c2.tool_name = Some("bar".into());
        assert_ne!(
            side_effect_idempotency_key(&c1, "h"),
            side_effect_idempotency_key(&c2, "h"),
        );
    }

    #[test]
    fn idempotency_key_starts_with_side_effect_prefix() {
        let k = side_effect_idempotency_key(&ctx(), "h");
        assert!(k.starts_with("side_effect:"));
        let hex = &k["side_effect:".len()..];
        assert_eq!(hex.len(), 64);
    }

    #[test]
    fn idempotency_key_handles_none_fields() {
        let c = IdempotencyContext::default();
        let k = side_effect_idempotency_key(&c, "h");
        assert!(k.starts_with("side_effect:"));
    }

    #[test]
    fn risk_rank_known_levels() {
        assert_eq!(risk_rank("read"), 1);
        assert_eq!(risk_rank("low"), 1);
        assert_eq!(risk_rank("write"), 2);
        assert_eq!(risk_rank("medium"), 2);
        assert_eq!(risk_rank("destructive"), 3);
        assert_eq!(risk_rank("high"), 3);
        assert_eq!(risk_rank("critical"), 4);
    }

    #[test]
    fn risk_rank_unknown_returns_zero() {
        assert_eq!(risk_rank("unknown"), 0);
        assert_eq!(risk_rank(""), 0);
    }

    #[test]
    fn risk_rank_ordering() {
        assert!(risk_rank("read") < risk_rank("write"));
        assert!(risk_rank("write") < risk_rank("destructive"));
        assert!(risk_rank("destructive") < risk_rank("critical"));
    }

    #[test]
    fn audit_outcome_allow_success() {
        assert_eq!(audit_outcome(ToolAccessDecision::Allow), AuditOutcome::Success);
    }

    #[test]
    fn audit_outcome_require_approval_pending() {
        assert_eq!(audit_outcome(ToolAccessDecision::RequireApproval), AuditOutcome::Pending);
    }

    #[test]
    fn audit_outcome_defer_runtime_timeout() {
        assert_eq!(audit_outcome(ToolAccessDecision::DeferRuntime), AuditOutcome::Timeout);
    }

    #[test]
    fn audit_outcome_deny_denied() {
        assert_eq!(audit_outcome(ToolAccessDecision::Deny), AuditOutcome::Denied);
    }

    #[test]
    fn audit_outcome_serde_camel_case() {
        assert_eq!(AuditOutcome::Success.as_str(), "success");
        assert_eq!(AuditOutcome::Denied.as_str(), "denied");
        let j = serde_json::to_string(&AuditOutcome::Timeout).unwrap();
        assert_eq!(j, "\"timeout\"");
    }

    #[test]
    fn decision_serde_snake_case() {
        let j = serde_json::to_string(&ToolAccessDecision::RequireApproval).unwrap();
        assert_eq!(j, "\"require_approval\"");
        let j = serde_json::to_string(&ToolAccessDecision::DeferRuntime).unwrap();
        assert_eq!(j, "\"defer_runtime\"");
    }

    // ---- Round 767: pc-tool side_effect_idempotency 集成测试 ----

    /// risk_rank: 5 档 mapping (read=1, write=2, destructive=3, critical=4, unknown=0)。
    #[test]
    fn r767_risk_rank_mapping() {
        assert_eq!(risk_rank("read"), 1);
        assert_eq!(risk_rank("low"), 1);
        assert_eq!(risk_rank("write"), 2);
        assert_eq!(risk_rank("medium"), 2);
        assert_eq!(risk_rank("destructive"), 3);
        assert_eq!(risk_rank("high"), 3);
        assert_eq!(risk_rank("critical"), 4);
        assert_eq!(risk_rank("unknown"), 0);
        assert_eq!(risk_rank(""), 0);
    }

    /// audit_outcome: 4 个决策 → 4 个 outcome。
    #[test]
    fn r767_audit_outcome_decision_mapping() {
        use super::*;
        assert!(matches!(audit_outcome(ToolAccessDecision::Allow), AuditOutcome::Success));
        assert!(matches!(audit_outcome(ToolAccessDecision::RequireApproval), AuditOutcome::Pending));
        assert!(matches!(audit_outcome(ToolAccessDecision::DeferRuntime), AuditOutcome::Timeout));
        assert!(matches!(audit_outcome(ToolAccessDecision::Deny), AuditOutcome::Denied));
    }

    /// side_effect_idempotency_key: 同输入 → 同 hash (format: "side_effect:<sha256-hex>")。
    #[test]
    fn r767_idempotency_key_deterministic() {
        let ctx = IdempotencyContext {
            company_id: Some("c1".into()),
            run_id: Some("r1".into()),
            issue_id: Some("i1".into()),
            application_id: Some("a1".into()),
            connection_id: Some("conn1".into()),
            catalog_entry_id: Some("cat1".into()),
            tool_name: Some("t1".into()),
        };
        let k1 = side_effect_idempotency_key(&ctx, "args-hash-1");
        let k2 = side_effect_idempotency_key(&ctx, "args-hash-1");
        assert_eq!(k1, k2, "same inputs should produce same key");
        assert!(k1.starts_with("side_effect:"));
        assert_eq!(k1.len(), "side_effect:".len() + 64, "side_effect: prefix + SHA-256 hex");

        // 不同 arguments_hash → 不同 key
        let k3 = side_effect_idempotency_key(&ctx, "args-hash-2");
        assert_ne!(k1, k3);

        // 缺字段也能计算
        let empty = IdempotencyContext::default();
        let k4 = side_effect_idempotency_key(&empty, "");
        assert!(k4.starts_with("side_effect:"));
    }
}
