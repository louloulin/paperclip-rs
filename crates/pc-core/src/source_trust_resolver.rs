//! Source trust metadata 分类与编辑（含 resolver trait）。
//!
//! 对应 Node `server/src/services/source-trust.ts`（173 行）1:1 复刻。
//! （原 `pc-source-trust` crate 已下沉到 `pc-core::source_trust_resolver`）。
//!
//! 提供 [`SourceTrustResolver`] trait 抽象 IO；纯规则部分见 [`crate::source_trust`]。

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::trust_preset_resolver::{
    resolve_core_trust_preset, PolicySource, ResolveCoreTrustPresetInput, TrustPresetDenyReason,
    TrustPresetResolution, DEFAULT_TRUST_PRESET, LOW_TRUST_REVIEW_PRESET,
};

// ============================================================================
// Constants
// ============================================================================

/// Quarantined low-trust body 占位符（与 Node `LOW_TRUST_QUARANTINED_BODY` 1:1 对齐）。
pub const LOW_TRUST_QUARANTINED_BODY: &str =
    "[Quarantined low-trust output omitted from higher-trust agent context. A trusted reviewer can inspect and promote a sanitized artifact.]";

// ============================================================================
// DTO
// ============================================================================

/// Source trust metadata（与 Node `SourceTrustMetadata` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceTrustMetadata {
    pub preset: String,
    pub disposition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_from: Option<PromotedFrom>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_by_actor_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_by_actor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promoted_at: Option<String>,
}

impl SourceTrustMetadata {
    /// 构造标准的 source trust（无 promotedFrom 等）。
    pub fn standard() -> Self {
        Self {
            preset: DEFAULT_TRUST_PRESET.to_string(),
            disposition: "approved".to_string(),
            source_issue_id: None,
            source_run_id: None,
            source_agent_id: None,
            promoted_from: None,
            promoted_by_actor_type: None,
            promoted_by_actor_id: None,
            promoted_at: None,
        }
    }
}

/// Promoted from artifact（与 Node `promotedFrom` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotedFrom {
    pub artifact_kind: String,
    pub artifact_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
}

// ============================================================================
// Actor / issue context
// ============================================================================

/// Actor 上下文（与 Node `SourceTrustActor` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct SourceTrustActor {
    pub actor_type: String, // "agent" | "user"
    pub actor_id: String,
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
}

/// Issue 上下文（与 Node `SourceTrustIssueContext` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct SourceTrustIssueContext {
    pub id: String,
    pub company_id: String,
    pub project_id: Option<String>,
    pub execution_policy: Option<serde_json::Value>,
}

// ============================================================================
// Errors
// ============================================================================

/// Source trust 服务错误。
#[derive(Debug, Error)]
pub enum SourceTrustError {
    #[error("forbidden: {0}")]
    Forbidden(String),
}

pub type SourceTrustResult<T> = std::result::Result<T, SourceTrustError>;

// ============================================================================
// Pure helpers
// ============================================================================

/// 检查 source trust 是否为低信任 quarantined 状态（与 Node `isLowTrustQuarantined` 1:1 对齐）。
pub fn is_low_trust_quarantined(source_trust: Option<&SourceTrustMetadata>) -> bool {
    let Some(st) = source_trust else { return false };
    st.preset == LOW_TRUST_REVIEW_PRESET && st.disposition == "quarantined"
}

/// Redact body for higher trust context（与 Node `redactQuarantinedBodyForHigherTrust` 1:1 对齐）。
pub fn redact_quarantined_body_for_higher_trust<T>(value: T) -> T
where
    T: RedactableBody,
{
    if !is_low_trust_quarantined(value.source_trust()) {
        return value;
    }
    value.with_replaced_body(LOW_TRUST_QUARANTINED_BODY.to_string())
}

/// Trait for types that have a body + sourceTrust（用于 redact）。
pub trait RedactableBody {
    fn body(&self) -> Option<&str>;
    fn source_trust(&self) -> Option<&SourceTrustMetadata>;
    fn with_replaced_body(self, body: String) -> Self;
}

/// Sanitize quarantined comment for higher trust（与 Node `sanitizeQuarantinedCommentForHigherTrust` 1:1 对齐）。
///
/// 替换 body 为 `LOW_TRUST_QUARANTINED_BODY`，并清空 presentation / metadata。
pub fn sanitize_quarantined_comment_for_higher_trust<T>(comment: T) -> T
where
    T: SanitizableComment,
{
    if !is_low_trust_quarantined(comment.source_trust()) {
        return comment;
    }
    comment.sanitize(LOW_TRUST_QUARANTINED_BODY.to_string())
}

/// Trait for types that have body + presentation + metadata + sourceTrust。
pub trait SanitizableComment {
    fn source_trust(&self) -> Option<&SourceTrustMetadata>;
    fn sanitize(self, body: String) -> Self;
}

/// Build low-trust source trust（与 Node `buildLowTrustSourceTrust` 1:1 对齐）。
pub fn build_low_trust_source_trust(input: LowTrustSourceTrustInput) -> SourceTrustMetadata {
    SourceTrustMetadata {
        preset: LOW_TRUST_REVIEW_PRESET.to_string(),
        disposition: "quarantined".to_string(),
        source_issue_id: Some(input.issue_id),
        source_run_id: input.run_id,
        source_agent_id: input.agent_id,
        promoted_from: None,
        promoted_by_actor_type: None,
        promoted_by_actor_id: None,
        promoted_at: None,
    }
}

/// Input for `build_low_trust_source_trust`。
#[derive(Debug, Clone, Default)]
pub struct LowTrustSourceTrustInput {
    pub issue_id: String,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
}

/// Build promoted source trust（与 Node `buildPromotedSourceTrust` 1:1 对齐）。
pub fn build_promoted_source_trust(input: PromotedSourceTrustInput) -> SourceTrustMetadata {
    let source_issue_id = input.source_issue_id;
    SourceTrustMetadata {
        preset: LOW_TRUST_REVIEW_PRESET.to_string(),
        disposition: "promoted".to_string(),
        source_issue_id: Some(source_issue_id.clone()),
        source_run_id: None,
        source_agent_id: None,
        promoted_from: Some(PromotedFrom {
            artifact_kind: input.source_artifact_kind,
            artifact_id: input.source_artifact_id,
            issue_id: Some(source_issue_id),
        }),
        promoted_by_actor_type: Some(input.promoted_by_actor_type),
        promoted_by_actor_id: Some(input.promoted_by_actor_id),
        promoted_at: Some(input.promoted_at.unwrap_or_else(Utc::now).to_rfc3339()),
    }
}

/// Input for `build_promoted_source_trust`。
#[derive(Debug, Clone, Default)]
pub struct PromotedSourceTrustInput {
    pub source_issue_id: String,
    pub source_artifact_kind: String,
    pub source_artifact_id: String,
    pub promoted_by_actor_type: String,
    pub promoted_by_actor_id: String,
    pub promoted_at: Option<DateTime<Utc>>,
}

// ============================================================================
// Data source trait
// ============================================================================

/// 抽象 DB 查询（与 Node 端 agent / project / run 查询 1:1 对齐）。
#[async_trait]
pub trait SourceTrustResolver: Send + Sync {
    /// 查询 agent（按 id + companyId）
    async fn find_agent(&self, company_id: &str, agent_id: &str) -> Option<AgentTrustProjection>;

    /// 查询 project（按 id + companyId）
    async fn find_project(
        &self,
        company_id: &str,
        project_id: &str,
    ) -> Option<ProjectTrustProjection>;

    /// 查询 run（按 id + companyId）
    async fn find_run(&self, company_id: &str, run_id: &str) -> Option<RunTrustProjection>;
}

/// Agent 投影（与 Node 端 agent 查询字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct AgentTrustProjection {
    pub company_id: Option<String>,
    pub permissions: Option<serde_json::Value>,
}

/// Project 投影（与 Node 端 project 查询字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ProjectTrustProjection {
    pub company_id: Option<String>,
    pub execution_workspace_policy: Option<serde_json::Value>,
}

/// Run 投影（与 Node 端 run 查询字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct RunTrustProjection {
    pub company_id: Option<String>,
    pub agent_id: Option<String>,
    pub context_snapshot: Option<serde_json::Value>,
}

// ============================================================================
// Resolve actor trust
// ============================================================================

/// Resolve actor source trust for an issue（与 Node `resolveActorSourceTrustForIssue` 1:1 对齐）。
pub async fn resolve_actor_source_trust_for_issue(
    resolver: &dyn SourceTrustResolver,
    issue: &SourceTrustIssueContext,
    actor: &SourceTrustActor,
) -> SourceTrustResult<Option<SourceTrustMetadata>> {
    // 非 agent actor → 没有任何 source trust
    if actor.actor_type != "agent" || actor.agent_id.is_none() {
        return Ok(None);
    }
    let agent_id = actor.agent_id.as_ref().unwrap();

    // 并行查 agent / project / run
    let (agent, project, run) = tokio::join!(
        resolver.find_agent(&issue.company_id, agent_id),
        async {
            if let Some(pid) = &issue.project_id {
                resolver.find_project(&issue.company_id, pid).await
            } else {
                None
            }
        },
        async {
            if let Some(rid) = &actor.run_id {
                resolver.find_run(&issue.company_id, rid).await
            } else {
                None
            }
        },
    );

    // 如果 run 被声明但不存在或 agent 不匹配 → fail-closed: tag as quarantined
    if actor.run_id.is_some() {
        let rid = actor.run_id.as_ref().unwrap();
        match &run {
            None => {
                return Ok(Some(build_low_trust_source_trust(
                    LowTrustSourceTrustInput {
                        issue_id: issue.id.clone(),
                        run_id: Some(rid.clone()),
                        agent_id: Some(agent_id.clone()),
                    },
                )));
            }
            Some(r) => {
                if r.agent_id.as_deref() != Some(agent_id.as_str()) {
                    return Ok(Some(build_low_trust_source_trust(
                        LowTrustSourceTrustInput {
                            issue_id: issue.id.clone(),
                            run_id: Some(rid.clone()),
                            agent_id: Some(agent_id.clone()),
                        },
                    )));
                }
            }
        }
    }

    // 构造 PolicySource 并 resolve
    let agent_source = agent.map(|a| PolicySource {
        company_id: a.company_id,
        permissions: a.permissions,
        ..Default::default()
    });
    let project_source = project.map(|p| PolicySource {
        company_id: p.company_id,
        execution_workspace_policy: p.execution_workspace_policy,
        ..Default::default()
    });
    let issue_source = Some(PolicySource {
        company_id: Some(issue.company_id.clone()),
        execution_policy: issue.execution_policy.clone(),
        ..Default::default()
    });
    let run_source = run.as_ref().map(|r| {
        // 从 run.contextSnapshot 中提取 executionPolicy
        let run_execution_policy = r
            .context_snapshot
            .as_ref()
            .and_then(|snap| snap.get("executionPolicy"))
            .cloned();
        PolicySource {
            company_id: r.company_id.clone(),
            execution_policy: run_execution_policy,
            ..Default::default()
        }
    });

    let resolution = resolve_core_trust_preset(&ResolveCoreTrustPresetInput {
        company_id: issue.company_id.clone(),
        agent: agent_source,
        project: project_source,
        issue: issue_source,
        run: run_source,
    });

    match resolution {
        TrustPresetResolution::Denied { reason, detail, .. } => {
            // 一些 deny 是 "policy source 缺失" 等良性场景；这里只对 forbidden 类抛出
            // 与 Node 一致：denied → throw forbidden
            Err(SourceTrustError::Forbidden(format!(
                "{:?}: {}",
                reason, detail
            )))
        }
        TrustPresetResolution::Standard { .. } => Ok(None),
        TrustPresetResolution::LowTrustReview { .. } => Ok(Some(build_low_trust_source_trust(
            LowTrustSourceTrustInput {
                issue_id: issue.id.clone(),
                run_id: actor.run_id.clone(),
                agent_id: Some(agent_id.clone()),
            },
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- constants -----

    #[test]
    fn r717_quarantined_body_constant() {
        assert!(LOW_TRUST_QUARANTINED_BODY.starts_with("[Quarantined low-trust"));
    }

    // ----- is_low_trust_quarantined -----

    #[test]
    fn r717_is_quarantined_none() {
        assert!(!is_low_trust_quarantined(None));
    }

    #[test]
    fn r717_is_quarantined_standard() {
        let m = SourceTrustMetadata::standard();
        assert!(!is_low_trust_quarantined(Some(&m)));
    }

    #[test]
    fn r717_is_quarantined_low_trust_promoted() {
        let m = SourceTrustMetadata {
            preset: LOW_TRUST_REVIEW_PRESET.to_string(),
            disposition: "promoted".to_string(),
            ..SourceTrustMetadata::standard()
        };
        assert!(!is_low_trust_quarantined(Some(&m)));
    }

    #[test]
    fn r717_is_quarantined_low_trust_quarantined() {
        let m = SourceTrustMetadata {
            preset: LOW_TRUST_REVIEW_PRESET.to_string(),
            disposition: "quarantined".to_string(),
            ..SourceTrustMetadata::standard()
        };
        assert!(is_low_trust_quarantined(Some(&m)));
    }

    // ----- build_low_trust_source_trust -----

    #[test]
    fn r717_build_low_trust_minimal() {
        let m = build_low_trust_source_trust(LowTrustSourceTrustInput {
            issue_id: "i-1".into(),
            ..Default::default()
        });
        assert_eq!(m.preset, LOW_TRUST_REVIEW_PRESET);
        assert_eq!(m.disposition, "quarantined");
        assert_eq!(m.source_issue_id.as_deref(), Some("i-1"));
        assert!(m.source_run_id.is_none());
        assert!(m.source_agent_id.is_none());
    }

    #[test]
    fn r717_build_low_trust_with_run_agent() {
        let m = build_low_trust_source_trust(LowTrustSourceTrustInput {
            issue_id: "i-1".into(),
            run_id: Some("r-1".into()),
            agent_id: Some("a-1".into()),
        });
        assert_eq!(m.source_run_id.as_deref(), Some("r-1"));
        assert_eq!(m.source_agent_id.as_deref(), Some("a-1"));
    }

    // ----- build_promoted_source_trust -----

    #[test]
    fn r717_build_promoted_minimal() {
        let m = build_promoted_source_trust(PromotedSourceTrustInput {
            source_issue_id: "i-1".into(),
            source_artifact_kind: "comment".into(),
            source_artifact_id: "c-1".into(),
            promoted_by_actor_type: "user".into(),
            promoted_by_actor_id: "u-1".into(),
            promoted_at: None,
        });
        assert_eq!(m.preset, LOW_TRUST_REVIEW_PRESET);
        assert_eq!(m.disposition, "promoted");
        assert_eq!(m.source_issue_id.as_deref(), Some("i-1"));
        let pf = m.promoted_from.unwrap();
        assert_eq!(pf.artifact_kind, "comment");
        assert_eq!(pf.artifact_id, "c-1");
        assert_eq!(m.promoted_by_actor_type.as_deref(), Some("user"));
        assert_eq!(m.promoted_by_actor_id.as_deref(), Some("u-1"));
        assert!(m.promoted_at.is_some()); // 默认 Utc::now
    }

    // ----- redact_quarantined_body_for_higher_trust -----

    #[derive(Debug, Clone, PartialEq)]
    struct FakeComment {
        body: String,
        source_trust: Option<SourceTrustMetadata>,
    }

    impl RedactableBody for FakeComment {
        fn body(&self) -> Option<&str> {
            Some(&self.body)
        }
        fn source_trust(&self) -> Option<&SourceTrustMetadata> {
            self.source_trust.as_ref()
        }
        fn with_replaced_body(self, body: String) -> Self {
            Self { body, ..self }
        }
    }

    #[test]
    fn r717_redact_keeps_standard_body() {
        let c = FakeComment {
            body: "hello".into(),
            source_trust: Some(SourceTrustMetadata::standard()),
        };
        let redacted = redact_quarantined_body_for_higher_trust(c);
        assert_eq!(redacted.body, "hello");
    }

    #[test]
    fn r717_redact_replaces_quarantined_body() {
        let c = FakeComment {
            body: "secret".into(),
            source_trust: Some(SourceTrustMetadata {
                preset: LOW_TRUST_REVIEW_PRESET.into(),
                disposition: "quarantined".into(),
                ..SourceTrustMetadata::standard()
            }),
        };
        let redacted = redact_quarantined_body_for_higher_trust(c);
        assert_eq!(redacted.body, LOW_TRUST_QUARANTINED_BODY);
    }

    #[test]
    fn r717_redact_no_source_trust_keeps_body() {
        let c = FakeComment {
            body: "hello".into(),
            source_trust: None,
        };
        let redacted = redact_quarantined_body_for_higher_trust(c);
        assert_eq!(redacted.body, "hello");
    }

    // ----- sanitize_quarantined_comment_for_higher_trust -----

    #[derive(Debug, Clone, PartialEq)]
    struct FullComment {
        body: String,
        presentation: Option<serde_json::Value>,
        metadata: Option<serde_json::Value>,
        source_trust: Option<SourceTrustMetadata>,
    }

    impl SanitizableComment for FullComment {
        fn source_trust(&self) -> Option<&SourceTrustMetadata> {
            self.source_trust.as_ref()
        }
        fn sanitize(self, body: String) -> Self {
            Self {
                body,
                presentation: None,
                metadata: None,
                ..self
            }
        }
    }

    #[test]
    fn r717_sanitize_replaces_quarantined_clears_extras() {
        let c = FullComment {
            body: "secret".into(),
            presentation: Some(serde_json::json!({"kind": "code"})),
            metadata: Some(serde_json::json!({"trace": "x"})),
            source_trust: Some(SourceTrustMetadata {
                preset: LOW_TRUST_REVIEW_PRESET.into(),
                disposition: "quarantined".into(),
                ..SourceTrustMetadata::standard()
            }),
        };
        let s = sanitize_quarantined_comment_for_higher_trust(c);
        assert_eq!(s.body, LOW_TRUST_QUARANTINED_BODY);
        assert!(s.presentation.is_none());
        assert!(s.metadata.is_none());
    }

    #[test]
    fn r717_sanitize_keeps_standard() {
        let c = FullComment {
            body: "hello".into(),
            presentation: Some(serde_json::json!({"kind": "code"})),
            metadata: Some(serde_json::json!({"k": "v"})),
            source_trust: Some(SourceTrustMetadata::standard()),
        };
        let s = sanitize_quarantined_comment_for_higher_trust(c);
        assert_eq!(s.body, "hello");
        assert!(s.presentation.is_some());
        assert!(s.metadata.is_some());
    }

    // ----- resolve_actor_source_trust_for_issue (using fake resolver) -----

    struct FakeResolver {
        agent: Option<AgentTrustProjection>,
        project: Option<ProjectTrustProjection>,
        run: Option<RunTrustProjection>,
    }

    #[async_trait]
    impl SourceTrustResolver for FakeResolver {
        async fn find_agent(&self, _: &str, _: &str) -> Option<AgentTrustProjection> {
            self.agent.clone()
        }
        async fn find_project(&self, _: &str, _: &str) -> Option<ProjectTrustProjection> {
            self.project.clone()
        }
        async fn find_run(&self, _: &str, _: &str) -> Option<RunTrustProjection> {
            self.run.clone()
        }
    }

    #[tokio::test]
    async fn r717_resolve_non_agent_returns_none() {
        let r = FakeResolver {
            agent: None,
            project: None,
            run: None,
        };
        let actor = SourceTrustActor {
            actor_type: "user".into(),
            actor_id: "u-1".into(),
            ..Default::default()
        };
        let issue = SourceTrustIssueContext {
            id: "i-1".into(),
            company_id: "co-1".into(),
            ..Default::default()
        };
        let m = resolve_actor_source_trust_for_issue(&r, &issue, &actor)
            .await
            .unwrap();
        assert!(m.is_none());
    }

    #[tokio::test]
    async fn r717_resolve_standard_returns_none() {
        // agent with no low trust marker
        let r = FakeResolver {
            agent: Some(AgentTrustProjection {
                company_id: Some("co-1".into()),
                permissions: Some(serde_json::json!({})),
            }),
            project: None,
            run: None,
        };
        let actor = SourceTrustActor {
            actor_type: "agent".into(),
            actor_id: "u-1".into(),
            agent_id: Some("a-1".into()),
            run_id: None,
        };
        let issue = SourceTrustIssueContext {
            id: "i-1".into(),
            company_id: "co-1".into(),
            ..Default::default()
        };
        let m = resolve_actor_source_trust_for_issue(&r, &issue, &actor)
            .await
            .unwrap();
        assert!(m.is_none());
    }

    #[tokio::test]
    async fn r717_resolve_low_trust_via_agent_policy() {
        let r = FakeResolver {
            agent: Some(AgentTrustProjection {
                company_id: Some("co-1".into()),
                permissions: Some(serde_json::json!({
                    "trustPreset": "low_trust_review",
                    "authorizationPolicy": {
                        "trustBoundary": {
                            "mode": "low_trust_review",
                            "rootIssueId": "123e4567-e89b-12d3-a456-426614174000"
                        }
                    }
                })),
            }),
            project: None,
            run: None,
        };
        let actor = SourceTrustActor {
            actor_type: "agent".into(),
            actor_id: "a-1".into(),
            agent_id: Some("a-1".into()),
            run_id: None,
        };
        let issue = SourceTrustIssueContext {
            id: "i-1".into(),
            company_id: "co-1".into(),
            ..Default::default()
        };
        let m = resolve_actor_source_trust_for_issue(&r, &issue, &actor)
            .await
            .unwrap();
        assert!(m.is_some());
        let m = m.unwrap();
        assert_eq!(m.disposition, "quarantined");
    }

    #[tokio::test]
    async fn r717_resolve_run_mismatch_fails_closed() {
        // run 存在但 agent 不匹配 → fail-closed quarantined
        let r = FakeResolver {
            agent: Some(AgentTrustProjection {
                company_id: Some("co-1".into()),
                permissions: Some(serde_json::json!({})),
            }),
            project: None,
            run: Some(RunTrustProjection {
                company_id: Some("co-1".into()),
                agent_id: Some("different-agent".into()),
                context_snapshot: None,
            }),
        };
        let actor = SourceTrustActor {
            actor_type: "agent".into(),
            actor_id: "a-1".into(),
            agent_id: Some("a-1".into()),
            run_id: Some("r-1".into()),
        };
        let issue = SourceTrustIssueContext {
            id: "i-1".into(),
            company_id: "co-1".into(),
            ..Default::default()
        };
        let m = resolve_actor_source_trust_for_issue(&r, &issue, &actor)
            .await
            .unwrap();
        assert!(m.is_some());
        assert_eq!(m.unwrap().disposition, "quarantined");
    }

    #[tokio::test]
    async fn r717_resolve_run_not_found_fails_closed() {
        let r = FakeResolver {
            agent: Some(AgentTrustProjection {
                company_id: Some("co-1".into()),
                permissions: Some(serde_json::json!({})),
            }),
            project: None,
            run: None, // run not found
        };
        let actor = SourceTrustActor {
            actor_type: "agent".into(),
            actor_id: "a-1".into(),
            agent_id: Some("a-1".into()),
            run_id: Some("r-1".into()),
        };
        let issue = SourceTrustIssueContext {
            id: "i-1".into(),
            company_id: "co-1".into(),
            ..Default::default()
        };
        let m = resolve_actor_source_trust_for_issue(&r, &issue, &actor)
            .await
            .unwrap();
        assert!(m.is_some());
    }

    // ----- send/sync -----

    #[test]
    fn r717_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SourceTrustMetadata>();
        assert_send_sync::<SourceTrustActor>();
        assert_send_sync::<SourceTrustIssueContext>();
    }
}
