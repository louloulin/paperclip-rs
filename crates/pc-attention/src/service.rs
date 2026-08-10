//! R632: AttentionService — 跨 repo 聚合 attention feed。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// R632: Attention item severity 等级。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AttentionSeverity {
    Critical,
    High,
    #[default]
    Medium,
    Low,
    Info,
}

/// R632: Attention item 类型 — 描述数据来源。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AttentionItemKind {
    AgentError,
    ApprovalPending,
    BudgetIncident,
    DecisionOpen,
    HeartbeatFailed,
    IssueBlocked,
    IssueProductivityReview,
    IssueReview,
    IssuePendingInteraction,
    JoinRequestPending,
    PipelineAttention,
    ToolError,
}

impl AttentionItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AttentionItemKind::AgentError => "agent_error",
            AttentionItemKind::ApprovalPending => "approval_pending",
            AttentionItemKind::BudgetIncident => "budget_incident",
            AttentionItemKind::DecisionOpen => "decision_open",
            AttentionItemKind::HeartbeatFailed => "heartbeat_failed",
            AttentionItemKind::IssueBlocked => "issue_blocked",
            AttentionItemKind::IssueProductivityReview => "issue_productivity_review",
            AttentionItemKind::IssueReview => "issue_review",
            AttentionItemKind::IssuePendingInteraction => "issue_pending_interaction",
            AttentionItemKind::JoinRequestPending => "join_request_pending",
            AttentionItemKind::PipelineAttention => "pipeline_attention",
            AttentionItemKind::ToolError => "tool_error",
        }
    }
}

/// R632: 统一的 attention item。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionItem {
    pub kind: AttentionItemKind,
    pub subject_id: Uuid,
    pub company_id: Uuid,
    pub severity: AttentionSeverity,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// R632: 按 kind 的计数聚合。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttentionCounts {
    pub agent_error: usize,
    pub approval_pending: usize,
    pub budget_incident: usize,
    pub decision_open: usize,
    pub heartbeat_failed: usize,
    pub issue_blocked: usize,
    pub issue_productivity_review: usize,
    pub issue_review: usize,
    pub issue_pending_interaction: usize,
    pub join_request_pending: usize,
    pub pipeline_attention: usize,
    pub tool_error: usize,
}

impl AttentionCounts {
    pub fn total(&self) -> usize {
        self.agent_error
            + self.approval_pending
            + self.budget_incident
            + self.decision_open
            + self.heartbeat_failed
            + self.issue_blocked
            + self.issue_productivity_review
            + self.issue_review
            + self.issue_pending_interaction
            + self.join_request_pending
            + self.pipeline_attention
            + self.tool_error
    }
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }
}

#[derive(Debug, Error)]
pub enum AttentionError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}
impl From<pc_repos::RepoError> for AttentionError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Db(sqlx::Error::Decode(format!("{e}").into()))
    }
}
pub type AttentionResult<T> = Result<T, AttentionError>;

/// R632: AttentionService — 跨 repo 的 attention 聚合入口。
#[derive(Clone)]
pub struct AttentionService {
    db: pc_repos::Db,
}

impl AttentionService {
    pub fn new(db: pc_repos::Db) -> Self {
        Self { db }
    }

    /// R632: 列出 company 的所有 attention item（按 severity 排序，limit 截断）。
    pub async fn list_for_company(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> AttentionResult<Vec<AttentionItem>> {
        if company_id.is_nil() {
            return Err(AttentionError::Validation(
                "companyId is required".into(),
            ));
        }
        let limit = limit.clamp(1, 500);
        let mut all = Vec::new();

        // 1. AgentError
        let agent_repo = pc_repos::agent::AgentRepo::new(&self.db);
        for row in agent_repo.list_error_attention(company_id).await? {
            all.push(AttentionItem {
                kind: AttentionItemKind::AgentError,
                subject_id: row.id,
                company_id,
                severity: AttentionSeverity::High,
                title: format!("Agent error: {}", row.name),
                description: row.error_reason,
                created_at: row.updated_at,
            });
        }

        // 2. ApprovalPending
        let approval_repo = pc_repos::approval::ApprovalRepo::new(&self.db);
        for row in approval_repo.list_pending_attention(company_id).await? {
            all.push(AttentionItem {
                kind: AttentionItemKind::ApprovalPending,
                subject_id: row.id,
                company_id,
                severity: AttentionSeverity::Medium,
                title: format!("Approval pending: {}", row.title),
                description: None,
                created_at: row.created_at,
            });
        }

        // 3. BudgetIncident
        let budget_repo = pc_repos::budget::BudgetRepo::new(&self.db);
        for row in budget_repo.list_open_attention(company_id).await? {
            all.push(AttentionItem {
                kind: AttentionItemKind::BudgetIncident,
                subject_id: row.id,
                company_id,
                severity: AttentionSeverity::High,
                title: format!("Budget incident: {}", row.title),
                description: None,
                created_at: row.created_at,
            });
        }

        // 4. DecisionOpen
        let decision_repo = pc_repos::decision::DecisionRepo::new(&self.db);
        if let Ok(rows) = decision_repo.list_open_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::DecisionOpen,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::Medium,
                    title: format!("Decision open: {}", row.title),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }

        // 5. HeartbeatFailed
        let heartbeat_repo = pc_repos::heartbeat::HeartbeatRepo::new(&self.db);
        if let Ok(rows) = heartbeat_repo.list_failed_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::HeartbeatFailed,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::High,
                    title: format!("Heartbeat run failed: {}", row.id),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }

        // 6-8. Issue attentions
        let issue_repo = pc_repos::issue::IssueRepo::new(&self.db);
        if let Ok(rows) = issue_repo.list_blocked_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::IssueBlocked,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::High,
                    title: format!("Issue blocked: {}", row.title),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }
        if let Ok(rows) = issue_repo.list_productivity_review_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::IssueProductivityReview,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::Low,
                    title: format!("Productivity review: {}", row.title),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }
        if let Ok(rows) = issue_repo.list_review_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::IssueReview,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::Medium,
                    title: format!("Issue in review: {}", row.title),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }
        if let Ok(rows) = issue_repo.list_pending_interactions_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::IssuePendingInteraction,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::Medium,
                    title: format!("Issue pending interaction: {}", row.title),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }

        // 9. JoinRequestPending
        let jr_repo = pc_repos::join_request::JoinRequestRepo::new(&self.db);
        if let Ok(rows) = jr_repo.list_pending_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::JoinRequestPending,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::Low,
                    title: format!("Join request pending: {}", row.email),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }

        // 10. PipelineAttention
        let pipeline_repo = pc_repos::pipeline::PipelineRepo::new(&self.db);
        if let Ok(rows) = pipeline_repo.list_attention_pipelines(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::PipelineAttention,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::Medium,
                    title: format!("Pipeline attention: {}", row.name),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }

        // 11. ToolError
        let tool_repo = pc_repos::tool::ToolRepo::new(&self.db);
        if let Ok(rows) = tool_repo.list_apps_attention(company_id).await {
            for row in rows {
                all.push(AttentionItem {
                    kind: AttentionItemKind::ToolError,
                    subject_id: row.id,
                    company_id,
                    severity: AttentionSeverity::Medium,
                    title: format!("Tool error: {}", row.name),
                    description: None,
                    created_at: row.created_at,
                });
            }
        }

        // Sort by severity then created_at desc
        all.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        all.truncate(limit as usize);
        Ok(all)
    }

    /// R632: 列出按 kind 过滤的 attention item。
    pub async fn list_by_kind(
        &self,
        company_id: Uuid,
        kind: AttentionItemKind,
        limit: i64,
    ) -> AttentionResult<Vec<AttentionItem>> {
        let all = self.list_for_company(company_id, 500).await?;
        Ok(all
            .into_iter()
            .filter(|i| i.kind == kind)
            .take(limit.clamp(1, 500) as usize)
            .collect())
    }

    /// R632: 按 kind 统计 attention 数量。
    pub async fn counts_for_company(
        &self,
        company_id: Uuid,
    ) -> AttentionResult<AttentionCounts> {
        let all = self.list_for_company(company_id, 500).await?;
        let mut counts = AttentionCounts::default();
        for item in all {
            match item.kind {
                AttentionItemKind::AgentError => counts.agent_error += 1,
                AttentionItemKind::ApprovalPending => counts.approval_pending += 1,
                AttentionItemKind::BudgetIncident => counts.budget_incident += 1,
                AttentionItemKind::DecisionOpen => counts.decision_open += 1,
                AttentionItemKind::HeartbeatFailed => counts.heartbeat_failed += 1,
                AttentionItemKind::IssueBlocked => counts.issue_blocked += 1,
                AttentionItemKind::IssueProductivityReview => {
                    counts.issue_productivity_review += 1
                }
                AttentionItemKind::IssueReview => counts.issue_review += 1,
                AttentionItemKind::IssuePendingInteraction => {
                    counts.issue_pending_interaction += 1
                }
                AttentionItemKind::JoinRequestPending => counts.join_request_pending += 1,
                AttentionItemKind::PipelineAttention => counts.pipeline_attention += 1,
                AttentionItemKind::ToolError => counts.tool_error += 1,
            }
        }
        Ok(counts)
    }

    /// R632: 列出所有 supported kinds。
    pub fn supported_kinds() -> Vec<AttentionItemKind> {
        vec![
            AttentionItemKind::AgentError,
            AttentionItemKind::ApprovalPending,
            AttentionItemKind::BudgetIncident,
            AttentionItemKind::DecisionOpen,
            AttentionItemKind::HeartbeatFailed,
            AttentionItemKind::IssueBlocked,
            AttentionItemKind::IssueProductivityReview,
            AttentionItemKind::IssueReview,
            AttentionItemKind::IssuePendingInteraction,
            AttentionItemKind::JoinRequestPending,
            AttentionItemKind::PipelineAttention,
            AttentionItemKind::ToolError,
        ]
    }
}
