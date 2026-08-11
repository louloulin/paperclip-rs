//! Service 实现 —— IssueThreadInteractionService。
//!
//! 设计：
//! - 接收 `&Db` 引用（IssueRepo 不 Clone → 每次创建）
//! - 提供顶层公开函数 + Service 封装
//! - Idempotency：create 时若提供 idempotency_key 且已存在，返回 existing
//! - 状态流转：accept / reject / cancel / withdraw / respond / submit_verdicts
//! - Hook 集成

use std::sync::Arc;

use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use pc_repos::{issue::IssueThreadInteractionRow, Db};

use super::hook::{IssueThreadInteractionHook, NoopIssueThreadInteractionHook};
use super::types::{
    ContinuationPolicy, CreateIssueThreadInteractionInput, InteractionActor,
    InteractionResolution, InteractionStatus, IssueThreadInteractionError,
    IssueThreadInteractionInfo, IssueThreadInteractionResult, ResolveInteractionInput,
    SubmitVerdictsInput, INTERACTION_KINDS, INTERACTION_TERMINAL_STATUSES,
};

// ============================================================================
// 顶层公开函数（与 Node service 1:1 对齐）
// ============================================================================

/// List interactions for an issue (与 Node `listInteractions` 1:1 对齐).
pub async fn list_interactions(db: &Db, issue_id: Uuid) -> sqlx::Result<Vec<IssueThreadInteractionRow>> {
    let repo = pc_repos::issue::IssueRepo::new(db);
    repo.list_interactions(issue_id).await
}

/// List interactions for company + issue (with company scoping).
pub async fn list_interactions_for_company(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Vec<IssueThreadInteractionRow>> {
    let repo = pc_repos::issue::IssueRepo::new(db);
    repo.list_interactions_for_company(company_id, issue_id).await
}

/// List pending interactions for attention queue.
pub async fn list_pending_interactions_attention(
    db: &Db,
    company_id: Uuid,
) -> sqlx::Result<Vec<IssueThreadInteractionRow>> {
    let repo = pc_repos::issue::IssueRepo::new(db);
    repo.list_pending_interactions_attention(company_id).await
}

/// Get a single interaction by ID.
pub async fn get_interaction(
    db: &Db,
    id: Uuid,
) -> sqlx::Result<Option<IssueThreadInteractionRow>> {
    let repo = pc_repos::issue::IssueRepo::new(db);
    repo.get_interaction(id).await
}

/// Get interaction by idempotency key (returns first match).
pub async fn get_idempotent_interaction(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    idempotency_key: &str,
) -> sqlx::Result<Option<IssueThreadInteractionRow>> {
    let row = sqlx::query_as::<_, IssueThreadInteractionRow>(
        "SELECT id, company_id, issue_id, kind, status, continuation_policy, \
                source_comment_id, source_run_id, title, summary, \
                created_by_agent_id, created_by_user_id, \
                resolved_by_agent_id, resolved_by_user_id, \
                payload, result, resolved_at, created_at, updated_at \
         FROM issue_thread_interactions \
         WHERE company_id = $1 AND issue_id = $2 AND idempotency_key = $3 \
         LIMIT 1",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(idempotency_key)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

/// Create a new interaction. Supports idempotency via `idempotency_key`.
///
/// If a row exists with same (company_id, issue_id, idempotency_key),
/// returns the existing row instead of creating a duplicate.
///
/// 与 Node `createIssueThreadInteraction` 1:1 对齐.
pub async fn create_interaction(
    db: &Db,
    input: CreateIssueThreadInteractionInput,
) -> IssueThreadInteractionResult<IssueThreadInteractionRow> {
    // Validate kind
    if !INTERACTION_KINDS.contains(&input.kind.as_str()) {
        return Err(IssueThreadInteractionError::InvalidInput(format!(
            "invalid kind: {}",
            input.kind
        )));
    }

    // Validate payload is object
    if !input.payload.is_object() {
        return Err(IssueThreadInteractionError::InvalidInput(
            "payload must be an object".to_string(),
        ));
    }

    // Validate actor: exactly one of agent_id or user_id
    if input.created_by_agent_id.is_some() && input.created_by_user_id.is_some() {
        return Err(IssueThreadInteractionError::InvalidInput(
            "cannot specify both created_by_agent_id and created_by_user_id".to_string(),
        ));
    }

    // Check idempotency
    if let Some(key) = &input.idempotency_key {
        if let Some(existing) = get_idempotent_interaction(db, input.company_id, input.issue_id, key).await? {
            return Ok(existing);
        }
    }

    // Insert with idempotency_key
    let row = sqlx::query_as::<_, IssueThreadInteractionRow>(
        "INSERT INTO issue_thread_interactions \
            (company_id, issue_id, kind, continuation_policy, title, summary, \
             payload, source_comment_id, source_run_id, \
             created_by_agent_id, created_by_user_id, idempotency_key) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
         RETURNING id, company_id, issue_id, kind, status, continuation_policy, \
                source_comment_id, source_run_id, title, summary, \
                created_by_agent_id, created_by_user_id, \
                resolved_by_agent_id, resolved_by_user_id, \
                payload, result, resolved_at, created_at, updated_at",
    )
    .bind(input.company_id)
    .bind(input.issue_id)
    .bind(&input.kind)
    .bind(input.continuation_policy.as_str())
    .bind(&input.title)
    .bind(&input.summary)
    .bind(&input.payload)
    .bind(input.source_comment_id)
    .bind(input.source_run_id)
    .bind(input.created_by_agent_id)
    .bind(input.created_by_user_id.as_deref())
    .bind(input.idempotency_key.as_deref())
    .fetch_one(db.pool())
    .await
    .map_err(|e| {
        // Handle unique constraint violation for idempotency_key
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint().is_some() {
                return IssueThreadInteractionError::Conflict(
                    "interaction with this idempotency_key already exists".to_string(),
                );
            }
        }
        IssueThreadInteractionError::from(e)
    })?;

    Ok(row)
}

/// Resolve an interaction to a new status.
///
/// 与 Node `resolveIssueThreadInteraction` 1:1 对齐.
pub async fn resolve_interaction(
    db: &Db,
    input: ResolveInteractionInput,
) -> IssueThreadInteractionResult<InteractionResolution> {
    let repo = pc_repos::issue::IssueRepo::new(db);

    // Verify exists
    let current = repo
        .get_interaction(input.interaction_id)
        .await?
        .ok_or_else(|| IssueThreadInteractionError::NotFound(format!("interaction {}", input.interaction_id)))?;

    // Verify status is pending
    if current.status != "pending" {
        return Err(IssueThreadInteractionError::Conflict(format!(
            "interaction has already been resolved (status={})",
            current.status
        )));
    }

    // Validate new_status is terminal
    let new_status_str = input.new_status.as_str();
    if !INTERACTION_TERMINAL_STATUSES.contains(&new_status_str) {
        return Err(IssueThreadInteractionError::InvalidInput(format!(
            "invalid terminal status: {}",
            new_status_str
        )));
    }

    // Resolve actor → user_id or agent_id
    let resolved_by_user_id = if input.resolved_by_actor.actor_type == "user" {
        input.resolved_by_actor.actor_id.clone()
    } else {
        None
    };
    let resolved_by_agent_id = if input.resolved_by_actor.actor_type == "agent" {
        input.resolved_by_actor
            .actor_id
            .as_ref()
            .and_then(|s| Uuid::parse_str(s).ok())
    } else {
        None
    };

    let row = sqlx::query_as::<_, IssueThreadInteractionRow>(
        "UPDATE issue_thread_interactions SET \
            status = $2, result = COALESCE($3, result), \
            resolved_by_user_id = COALESCE($4, resolved_by_user_id), \
            resolved_by_agent_id = COALESCE($5, resolved_by_agent_id), \
            resolved_at = CASE WHEN $2 IN ('accepted','rejected','cancelled','withdrawn','answered','responded','done') THEN now() ELSE resolved_at END, \
            updated_at = now() \
         WHERE id = $1 \
         RETURNING id, company_id, issue_id, kind, status, continuation_policy, \
                source_comment_id, source_run_id, title, summary, \
                created_by_agent_id, created_by_user_id, \
                resolved_by_agent_id, resolved_by_user_id, \
                payload, result, resolved_at, created_at, updated_at",
    )
    .bind(input.interaction_id)
    .bind(new_status_str)
    .bind(input.result.as_ref())
    .bind(resolved_by_user_id.as_deref())
    .bind(resolved_by_agent_id)
    .fetch_optional(db.pool())
    .await?
    .ok_or_else(|| IssueThreadInteractionError::NotFound(format!("interaction {}", input.interaction_id)))?;

    Ok(InteractionResolution {
        interaction: row,
        continuation_issue_id: None,
    })
}

/// Accept interaction helper.
pub async fn accept_interaction(
    db: &Db,
    interaction_id: Uuid,
    result: Option<Value>,
    actor: InteractionActor,
) -> IssueThreadInteractionResult<InteractionResolution> {
    resolve_interaction(
        db,
        ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Accepted,
            result,
            resolved_by_actor: actor,
        },
    )
    .await
}

/// Reject interaction helper.
pub async fn reject_interaction(
    db: &Db,
    interaction_id: Uuid,
    result: Option<Value>,
    actor: InteractionActor,
) -> IssueThreadInteractionResult<InteractionResolution> {
    resolve_interaction(
        db,
        ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Rejected,
            result,
            resolved_by_actor: actor,
        },
    )
    .await
}

/// Cancel interaction helper.
pub async fn cancel_interaction(
    db: &Db,
    interaction_id: Uuid,
    actor: InteractionActor,
) -> IssueThreadInteractionResult<InteractionResolution> {
    resolve_interaction(
        db,
        ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Cancelled,
            result: None,
            resolved_by_actor: actor,
        },
    )
    .await
}

/// Withdraw interaction helper.
pub async fn withdraw_interaction(
    db: &Db,
    interaction_id: Uuid,
    actor: InteractionActor,
) -> IssueThreadInteractionResult<InteractionResolution> {
    resolve_interaction(
        db,
        ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Withdrawn,
            result: None,
            resolved_by_actor: actor,
        },
    )
    .await
}

/// Respond interaction helper (for ask_user_questions type).
pub async fn respond_interaction(
    db: &Db,
    interaction_id: Uuid,
    result: Value,
    actor: InteractionActor,
) -> IssueThreadInteractionResult<InteractionResolution> {
    resolve_interaction(
        db,
        ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Answered,
            result: Some(result),
            resolved_by_actor: actor,
        },
    )
    .await
}

/// Submit verdicts helper (for request_item_verdicts type).
pub async fn submit_verdicts(
    db: &Db,
    input: SubmitVerdictsInput,
) -> IssueThreadInteractionResult<InteractionResolution> {
    resolve_interaction(
        db,
        ResolveInteractionInput {
            interaction_id: input.interaction_id,
            new_status: InteractionStatus::Responded,
            result: Some(input.verdicts),
            resolved_by_actor: input.resolved_by_actor,
        },
    )
    .await
}

/// Issue thread interaction service —— 封装 + Hook.
pub struct IssueThreadInteractionService {
    hook: Arc<dyn IssueThreadInteractionHook>,
}

impl std::fmt::Debug for IssueThreadInteractionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueThreadInteractionService").finish()
    }
}

impl Default for IssueThreadInteractionService {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueThreadInteractionService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(NoopIssueThreadInteractionHook),
        }
    }

    pub fn with_hook(hook: Arc<dyn IssueThreadInteractionHook>) -> Self {
        Self { hook }
    }

    pub fn hook(&self) -> Arc<dyn IssueThreadInteractionHook> {
        self.hook.clone()
    }

    /// Create interaction (hook 集成 + idempotency).
    pub async fn create(
        &self,
        db: &Db,
        input: CreateIssueThreadInteractionInput,
    ) -> IssueThreadInteractionResult<IssueThreadInteractionRow> {
        self.hook.before_create(&input);
        // Idempotency check
        if let Some(key) = &input.idempotency_key {
            if let Some(existing) = get_idempotent_interaction(db, input.company_id, input.issue_id, key).await? {
                self.hook.on_conflict(input.issue_id, &input.kind, key);
                return Ok(existing);
            }
        }

        let result = create_interaction(db, input.clone()).await?;
        self.hook.after_create(result.id, &result.kind);
        Ok(result)
    }

    /// List for issue (pass-through).
    pub async fn list_for_issue(
        &self,
        db: &Db,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueThreadInteractionRow>> {
        list_interactions(db, issue_id).await
    }

    /// List for company + issue.
    pub async fn list_for_company(
        &self,
        db: &Db,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueThreadInteractionRow>> {
        list_interactions_for_company(db, company_id, issue_id).await
    }

    /// List pending for attention queue.
    pub async fn list_pending(
        &self,
        db: &Db,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<IssueThreadInteractionRow>> {
        list_pending_interactions_attention(db, company_id).await
    }

    /// Get by ID.
    pub async fn get(
        &self,
        db: &Db,
        id: Uuid,
    ) -> sqlx::Result<Option<IssueThreadInteractionRow>> {
        get_interaction(db, id).await
    }

    /// Get by idempotency key.
    pub async fn get_idempotent(
        &self,
        db: &Db,
        company_id: Uuid,
        issue_id: Uuid,
        idempotency_key: &str,
    ) -> sqlx::Result<Option<IssueThreadInteractionRow>> {
        get_idempotent_interaction(db, company_id, issue_id, idempotency_key).await
    }

    /// Convert Row to DTO.
    pub fn to_info(&self, row: IssueThreadInteractionRow) -> IssueThreadInteractionInfo {
        IssueThreadInteractionInfo::from(row)
    }

    /// Accept (hook 集成).
    pub async fn accept(
        &self,
        db: &Db,
        interaction_id: Uuid,
        result: Option<Value>,
        actor: InteractionActor,
    ) -> IssueThreadInteractionResult<InteractionResolution> {
        let input = ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Accepted,
            result,
            resolved_by_actor: actor,
        };
        self.hook.before_resolve(&input);
        let resolution = resolve_interaction(db, input).await?;
        self.hook.after_resolve(&resolution);
        Ok(resolution)
    }

    /// Reject (hook 集成).
    pub async fn reject(
        &self,
        db: &Db,
        interaction_id: Uuid,
        result: Option<Value>,
        actor: InteractionActor,
    ) -> IssueThreadInteractionResult<InteractionResolution> {
        let input = ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Rejected,
            result,
            resolved_by_actor: actor,
        };
        self.hook.before_resolve(&input);
        let resolution = resolve_interaction(db, input).await?;
        self.hook.after_resolve(&resolution);
        Ok(resolution)
    }

    /// Cancel (hook 集成).
    pub async fn cancel(
        &self,
        db: &Db,
        interaction_id: Uuid,
        actor: InteractionActor,
    ) -> IssueThreadInteractionResult<InteractionResolution> {
        let input = ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Cancelled,
            result: None,
            resolved_by_actor: actor,
        };
        self.hook.before_resolve(&input);
        let resolution = resolve_interaction(db, input).await?;
        self.hook.after_resolve(&resolution);
        Ok(resolution)
    }

    /// Withdraw (hook 集成).
    pub async fn withdraw(
        &self,
        db: &Db,
        interaction_id: Uuid,
        actor: InteractionActor,
    ) -> IssueThreadInteractionResult<InteractionResolution> {
        let input = ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Withdrawn,
            result: None,
            resolved_by_actor: actor,
        };
        self.hook.before_resolve(&input);
        let resolution = resolve_interaction(db, input).await?;
        self.hook.after_resolve(&resolution);
        Ok(resolution)
    }

    /// Respond (ask_user_questions type).
    pub async fn respond(
        &self,
        db: &Db,
        interaction_id: Uuid,
        result: Value,
        actor: InteractionActor,
    ) -> IssueThreadInteractionResult<InteractionResolution> {
        let input = ResolveInteractionInput {
            interaction_id,
            new_status: InteractionStatus::Answered,
            result: Some(result),
            resolved_by_actor: actor,
        };
        self.hook.before_resolve(&input);
        let resolution = resolve_interaction(db, input).await?;
        self.hook.after_resolve(&resolution);
        Ok(resolution)
    }

    /// Submit verdicts (request_item_verdicts type).
    pub async fn submit_verdicts(
        &self,
        db: &Db,
        interaction_id: Uuid,
        verdicts: Value,
        actor: InteractionActor,
    ) -> IssueThreadInteractionResult<InteractionResolution> {
        let input = SubmitVerdictsInput {
            interaction_id,
            verdicts,
            resolved_by_actor: actor,
        };
        let resolve_input = ResolveInteractionInput {
            interaction_id: input.interaction_id,
            new_status: InteractionStatus::Responded,
            result: Some(input.verdicts),
            resolved_by_actor: input.resolved_by_actor,
        };
        self.hook.before_resolve(&resolve_input);
        let resolution = resolve_interaction(db, resolve_input).await?;
        self.hook.after_resolve(&resolution);
        Ok(resolution)
    }
}

// Suppress unused imports
#[allow(dead_code)]
fn _unused_continuation(_p: ContinuationPolicy) {}
