//! R605: `RoutineService` — high-level facade for routine CRUD + trigger lifecycle.
//!
//! Aligned with `paperclip/server/src/services/routines.ts`. The Node service is
//! ~3100 lines and has deep dependencies on issues / secrets / wakeup. We scope
//! the first Rust port to **CRUD + trigger management + read-only list helpers**
//! (create, update, trigger CRUD, list runs, list revisions, restore revision).
//! Run / fire / cron tick APIs are deferred to a later round (R612+).

use std::sync::Arc;

use async_trait::async_trait;
use pc_errors::{conflict, internal, unprocessable, validation, Error, Result};
use pc_repos::routine::{
    CreateRoutineRecord, CreateRoutineTriggerRecord,
    RoutineRepo, RoutineRevisionRow, RoutineRow, RoutineRunRow, RoutineTriggerMutationResult,
    RoutineTriggerRow, UpdateRoutineRecord, UpdateRoutineTriggerRecord,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const ALLOWED_PRIORITIES: &[&str] = &["low", "medium", "high", "urgent"];
const ALLOWED_STATUSES: &[&str] = &["draft", "active", "paused", "archived"];
const ALLOWED_CONCURRENCY: &[&str] = &["allow", "skip", "queue"];
const ALLOWED_CATCHUP: &[&str] = &["skip_missed", "enqueue_missed_with_cap"];
const ALLOWED_ACTIVITY_GATE: &[&str] = &["always", "require_external_activity"];

const TRIGGER_KIND_SCHEDULE: &str = "schedule";
const TRIGGER_KIND_WEBHOOK: &str = "webhook";

// =============================================================================
// R605: routine lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RoutineHookEvent {
    Created { id: Uuid, company_id: Uuid, title: String, status: String },
    Updated { id: Uuid, company_id: Uuid, title: String, status: String },
    Archived { id: Uuid, company_id: Uuid },
    TriggerCreated { id: Uuid, routine_id: Uuid, kind: String },
    TriggerUpdated { id: Uuid, routine_id: Uuid, kind: String },
    TriggerDeleted { id: Uuid, routine_id: Uuid, kind: String },
}

// =============================================================================
// R605: RoutineHook trait
// =============================================================================

/// Hook into routine lifecycle events. Default impls are noop so callers only
/// implement what they care about.
#[async_trait]
pub trait RoutineHook: Send + Sync {
    async fn on_routine_event(&self, _event: RoutineHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopRoutineHook;
#[async_trait]
impl RoutineHook for NoopRoutineHook {}

/// Test helper that records every event the service dispatches.
#[derive(Default)]
pub struct RecordingRoutineHook {
    pub events: std::sync::Mutex<Vec<RoutineHookEvent>>,
}

#[async_trait]
impl RoutineHook for RecordingRoutineHook {
    async fn on_routine_event(&self, event: RoutineHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingRoutineHook {
    /// Take a snapshot of every event recorded so far.
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<RoutineHookEvent> {
        self.events.lock().expect("lock").clone()
    }

    /// Discard all recorded events.
    pub fn clear(&self) {
        self.events.lock().expect("lock").clear();
    }

    /// Number of events currently buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }

    /// Whether the recorder has no events buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.lock().expect("lock").is_empty()
    }
}

// =============================================================================
// R605: Public input / patch types
// =============================================================================

/// Input for `RoutineService::create`.
#[derive(Debug, Clone, Default)]
pub struct CreateRoutine {
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub goal_id: Option<Uuid>,
    pub parent_issue_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub assignee_agent_id: Option<Uuid>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub concurrency_policy: Option<String>,
    pub catch_up_policy: Option<String>,
    pub activity_gate_policy: Option<String>,
    pub activity_gate_scope: Option<String>,
    pub variables: Option<Value>,
    pub env: Option<Value>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_run_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
}

impl CreateRoutine {
    fn normalize(&self) -> Result<NormalizedCreate> {
        let title = self.title.trim().to_string();
        if title.is_empty() {
            return Err(validation("title must not be empty"));
        }
        if self.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        let priority = self.priority.clone().unwrap_or_else(|| "medium".to_string());
        if !ALLOWED_PRIORITIES.contains(&priority.as_str()) {
            return Err(validation("priority must be one of low/medium/high/urgent"));
        }
        let status = self.status.clone().unwrap_or_else(|| "active".to_string());
        if !ALLOWED_STATUSES.contains(&status.as_str()) {
            return Err(validation("status must be one of draft/active/paused/archived"));
        }
        let concurrency_policy = self
            .concurrency_policy
            .clone()
            .unwrap_or_else(|| "allow".to_string());
        if !ALLOWED_CONCURRENCY.contains(&concurrency_policy.as_str()) {
            return Err(validation("concurrencyPolicy must be one of allow/skip/queue"));
        }
        let catch_up_policy = self
            .catch_up_policy
            .clone()
            .unwrap_or_else(|| "skip_missed".to_string());
        if !ALLOWED_CATCHUP.contains(&catch_up_policy.as_str()) {
            return Err(validation("catchUpPolicy must be one of skip_missed/enqueue_missed_with_cap"));
        }
        let activity_gate_policy = self
            .activity_gate_policy
            .clone()
            .unwrap_or_else(|| "always".to_string());
        if !ALLOWED_ACTIVITY_GATE.contains(&activity_gate_policy.as_str()) {
            return Err(validation("activityGatePolicy must be one of always/require_external_activity"));
        }
        let activity_gate_scope = self
            .activity_gate_scope
            .clone()
            .unwrap_or_else(|| "company".to_string());
        let variables = self.variables.clone().unwrap_or_else(|| Value::Array(vec![]));
        let responsible_user_id = self
            .responsible_user_id
            .clone()
            .or_else(|| self.created_by_user_id.clone())
            .ok_or_else(|| unprocessable("Routine requires a responsible user"))?;
        Ok(NormalizedCreate {
            title,
            priority,
            status,
            concurrency_policy,
            catch_up_policy,
            activity_gate_policy,
            activity_gate_scope,
            variables,
            responsible_user_id,
        })
    }
}

struct NormalizedCreate {
    title: String,
    priority: String,
    status: String,
    concurrency_policy: String,
    catch_up_policy: String,
    activity_gate_policy: String,
    activity_gate_scope: String,
    variables: Value,
    responsible_user_id: String,
}

/// Partial update for a routine. Each `None` field means "leave unchanged",
/// `Some(None)` means "set to NULL" (only valid for nullable fields).
#[derive(Debug, Clone, Default)]
pub struct RoutinePatch {
    pub project_id: Option<Option<Uuid>>,
    pub folder_id: Option<Option<Uuid>>,
    pub goal_id: Option<Option<Uuid>>,
    pub parent_issue_id: Option<Option<Uuid>>,
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub assignee_agent_id: Option<Option<Uuid>>,
    pub priority: Option<String>,
    pub status: Option<String>,
    pub concurrency_policy: Option<String>,
    pub catch_up_policy: Option<String>,
    pub activity_gate_policy: Option<String>,
    pub activity_gate_scope: Option<String>,
    pub variables: Option<Value>,
    pub env: Option<Option<Value>>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub base_revision_id: Option<Uuid>,
}

impl RoutinePatch {
    fn validate(&self) -> Result<()> {
        if let Some(p) = &self.priority {
            if !ALLOWED_PRIORITIES.contains(&p.as_str()) {
                return Err(validation("priority must be one of low/medium/high/urgent"));
            }
        }
        if let Some(s) = &self.status {
            if !ALLOWED_STATUSES.contains(&s.as_str()) {
                return Err(validation("status must be one of draft/active/paused/archived"));
            }
        }
        if let Some(c) = &self.concurrency_policy {
            if !ALLOWED_CONCURRENCY.contains(&c.as_str()) {
                return Err(validation("concurrencyPolicy must be one of allow/skip/queue"));
            }
        }
        if let Some(c) = &self.catch_up_policy {
            if !ALLOWED_CATCHUP.contains(&c.as_str()) {
                return Err(validation("catchUpPolicy must be one of skip_missed/enqueue_missed_with_cap"));
            }
        }
        if let Some(a) = &self.activity_gate_policy {
            if !ALLOWED_ACTIVITY_GATE.contains(&a.as_str()) {
                return Err(validation("activityGatePolicy must be one of always/require_external_activity"));
            }
        }
        if let Some(t) = &self.title {
            if t.trim().is_empty() {
                return Err(validation("title must not be empty"));
            }
        }
        Ok(())
    }

    fn into_record(self) -> UpdateRoutineRecord {
        UpdateRoutineRecord {
            project_id: self.project_id,
            folder_id: self.folder_id,
            goal_id: self.goal_id,
            parent_issue_id: self.parent_issue_id,
            title: self.title,
            description: self.description,
            assignee_agent_id: self.assignee_agent_id,
            priority: self.priority,
            status: self.status,
            concurrency_policy: self.concurrency_policy,
            catch_up_policy: self.catch_up_policy,
            activity_gate_policy: self.activity_gate_policy,
            activity_gate_scope: self.activity_gate_scope,
            variables: self.variables,
            env: self.env,
            updated_by_agent_id: self.updated_by_agent_id,
            updated_by_user_id: self.updated_by_user_id,
            created_by_run_id: None,
        }
    }
}

/// Input for `RoutineService::create_trigger`.
#[derive(Debug, Clone, Default)]
pub struct CreateRoutineTrigger {
    pub kind: String,
    pub label: Option<String>,
    pub enabled: Option<bool>,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub signing_mode: Option<String>,
    pub replay_window_sec: Option<i32>,
    pub actor_agent_id: Option<Uuid>,
    pub actor_user_id: Option<String>,
    pub actor_run_id: Option<Uuid>,
}

impl CreateRoutineTrigger {
    fn validate(&self) -> Result<()> {
        match self.kind.as_str() {
            TRIGGER_KIND_SCHEDULE => {
                let cron = self
                    .cron_expression
                    .as_deref()
                    .ok_or_else(|| unprocessable("scheduled triggers require cronExpression"))?;
                if cron.trim().is_empty() {
                    return Err(unprocessable("scheduled triggers require cronExpression"));
                }
                // timezone default to UTC
                let _ = self.timezone.as_deref().unwrap_or("UTC");
            }
            TRIGGER_KIND_WEBHOOK => {
                // webhook triggers must NOT have cronExpression
                if self.cron_expression.is_some() {
                    return Err(unprocessable("webhook triggers must not include cronExpression"));
                }
            }
            other => {
                return Err(unprocessable(format!(
                    "trigger kind must be schedule or webhook, got {other}"
                )));
            }
        }
        Ok(())
    }

    fn into_record(self) -> CreateRoutineTriggerRecord {
        let tz = self.timezone.clone();
        CreateRoutineTriggerRecord {
            kind: self.kind.clone(),
            label: self.label,
            enabled: self.enabled.unwrap_or(true),
            cron_expression: if self.kind == TRIGGER_KIND_SCHEDULE {
                self.cron_expression.clone()
            } else {
                None
            },
            timezone: if self.kind == TRIGGER_KIND_SCHEDULE {
                Some(tz.unwrap_or_else(|| "UTC".into()))
            } else {
                None
            },
            next_run_at: None,
            public_id: None,
            secret_id: None,
            signing_mode: self.signing_mode,
            replay_window_sec: self.replay_window_sec,
            actor_agent_id: self.actor_agent_id,
            actor_user_id: self.actor_user_id,
            actor_run_id: self.actor_run_id,
        }
    }
}

/// Partial update for a trigger. `Some(None)` means "set to NULL".
#[derive(Debug, Clone, Default)]
pub struct UpdateRoutineTrigger {
    pub label: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub cron_expression: Option<Option<String>>,
    pub timezone: Option<Option<String>>,
    pub signing_mode: Option<Option<String>>,
    pub replay_window_sec: Option<Option<i32>>,
    pub actor_agent_id: Option<Uuid>,
    pub actor_user_id: Option<String>,
    pub actor_run_id: Option<Uuid>,
}

impl UpdateRoutineTrigger {
    fn validate(&self) -> Result<()> {
        if let Some(Some(cron)) = &self.cron_expression {
            if cron.trim().is_empty() {
                return Err(unprocessable("scheduled triggers require cronExpression"));
            }
        }
        if let Some(Some(tz)) = &self.timezone {
            if tz.trim().is_empty() {
                return Err(unprocessable("scheduled triggers require timezone"));
            }
        }
        Ok(())
    }

    fn into_record(self) -> UpdateRoutineTriggerRecord {
        UpdateRoutineTriggerRecord {
            label: self.label,
            enabled: self.enabled,
            cron_expression: self.cron_expression,
            timezone: self.timezone,
            next_run_at: None,
            signing_mode: self.signing_mode,
            replay_window_sec: self.replay_window_sec,
            actor_agent_id: self.actor_agent_id,
            actor_user_id: self.actor_user_id,
            actor_run_id: self.actor_run_id,
        }
    }
}

// =============================================================================
// R605: RoutineService
// =============================================================================

#[derive(Clone)]
pub struct RoutineService {
    db: pc_repos::Db,
    hooks: Vec<Arc<dyn RoutineHook>>,
}

impl RoutineService {
    #[must_use]
    pub fn new(db: pc_repos::Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    #[must_use]
    pub fn with_hooks(db: pc_repos::Db, hooks: Vec<Arc<dyn RoutineHook>>) -> Self {
        Self { db, hooks }
    }

    #[must_use]
    pub fn add_hook(mut self, hook: Arc<dyn RoutineHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    async fn dispatch(&self, event: RoutineHookEvent) -> Result<()> {
        for hook in &self.hooks {
            if let Err(e) = hook.on_routine_event(event.clone()).await {
                tracing::warn!(?event, error = %e, "routine hook failed");
            }
        }
        Ok(())
    }

    // ---- read ---------------------------------------------------------------

    pub async fn get(&self, id: Uuid) -> Result<Option<RoutineRow>> {
        RoutineRepo::new(&self.db)
            .get(id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
        project_id: Option<Uuid>,
    ) -> Result<Vec<RoutineRow>> {
        RoutineRepo::new(&self.db)
            .list_by_company_filtered(company_id, project_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn list_all(&self, limit: i64) -> Result<Vec<RoutineRow>> {
        RoutineRepo::new(&self.db)
            .list_all(limit)
            .await
            .map_err(map_sql_error)
    }

    /// Aggregate detail view (triggers + recent runs + description document +
    /// active issue) for a single routine. Mirrors Node's `getDetail`.
    pub async fn get_detail(&self, id: Uuid) -> Result<Option<RoutineDetail>> {
        let repo = RoutineRepo::new(&self.db);
        let Some(row) = repo.get(id).await.map_err(map_sql_error)? else {
            return Ok(None);
        };
        let triggers = repo
            .list_triggers(id)
            .await
            .map_err(map_sql_error)?;
        let recent_runs = repo
            .list_run_summaries(id, 25)
            .await
            .map_err(map_sql_error)?;
        let active_issue = repo
            .get_active_issue(id)
            .await
            .map_err(map_sql_error)?;
        let description_document = repo
            .get_description_document(id)
            .await
            .map_err(map_sql_error)?;
        Ok(Some(RoutineDetail {
            routine: row,
            triggers,
            recent_runs,
            active_issue,
            description_document,
        }))
    }

    // ---- write --------------------------------------------------------------

    pub async fn create(&self, input: CreateRoutine) -> Result<RoutineRow> {
        let normalized = input.normalize()?;
        let record = CreateRoutineRecord {
            company_id: input.company_id,
            project_id: input.project_id,
            folder_id: input.folder_id,
            goal_id: input.goal_id,
            parent_issue_id: input.parent_issue_id,
            title: normalized.title.clone(),
            description: input.description,
            assignee_agent_id: input.assignee_agent_id,
            priority: normalized.priority.clone(),
            status: normalized.status.clone(),
            concurrency_policy: normalized.concurrency_policy.clone(),
            catch_up_policy: normalized.catch_up_policy.clone(),
            activity_gate_policy: normalized.activity_gate_policy.clone(),
            activity_gate_scope: normalized.activity_gate_scope.clone(),
            variables: normalized.variables.clone(),
            env: input.env,
            created_by_agent_id: input.created_by_agent_id,
            created_by_user_id: input.created_by_user_id,
            responsible_user_id: Some(normalized.responsible_user_id),
            created_by_run_id: input.created_by_run_id,
        };
        let created = RoutineRepo::new(&self.db)
            .create_with_initial_revision(&record)
            .await
            .map_err(map_sql_error)?;
        self.dispatch(RoutineHookEvent::Created {
            id: created.id,
            company_id: created.company_id,
            title: created.title.clone(),
            status: created.status.clone(),
        })
        .await?;
        Ok(created)
    }

    /// Update an existing routine. Returns the updated row, or `None` if the
    /// routine was not found. Returns [`pc_errors::Error::Conflict`] if
    /// `base_revision_id` was supplied and does not match the current
    /// `latest_revision_id` (optimistic concurrency).
    pub async fn update(
        &self,
        id: Uuid,
        patch: RoutinePatch,
    ) -> Result<Option<RoutineRow>> {
        patch.validate()?;
        if let Some(want) = patch.base_revision_id {
            let repo = RoutineRepo::new(&self.db);
            let current = repo.get(id).await.map_err(map_sql_error)?;
            if let Some(cur) = current {
                if cur.latest_revision_id != Some(want) {
                    return Err(conflict_with_current(cur.latest_revision_id));
                }
            } else {
                return Ok(None);
            }
        }
        let record = patch.into_record();
        let updated = RoutineRepo::new(&self.db)
            .update_with_revision(id, &record)
            .await?;
        if let Some(ref row) = updated {
            let event = if row.status == "archived" {
                RoutineHookEvent::Archived { id: row.id, company_id: row.company_id }
            } else {
                RoutineHookEvent::Updated {
                    id: row.id,
                    company_id: row.company_id,
                    title: row.title.clone(),
                    status: row.status.clone(),
                }
            };
            self.dispatch(event).await?;
        }
        Ok(updated)
    }

    pub async fn delete(&self, id: Uuid) -> Result<bool> {
        let repo = RoutineRepo::new(&self.db);
        let company_id = match repo.get(id).await.map_err(map_sql_error)? {
            Some(row) => row.company_id,
            None => return Ok(false),
        };
        let removed = repo.delete(id).await.map_err(map_sql_error)?;
        if removed {
            self.dispatch(RoutineHookEvent::Archived { id, company_id }).await?;
        }
        Ok(removed)
    }

    // ---- triggers -----------------------------------------------------------

    pub async fn list_triggers(&self, routine_id: Uuid) -> Result<Vec<RoutineTriggerRow>> {
        RoutineRepo::new(&self.db)
            .list_triggers(routine_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn get_trigger(&self, id: Uuid) -> Result<Option<RoutineTriggerRow>> {
        RoutineRepo::new(&self.db)
            .get_trigger(id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn create_trigger(
        &self,
        routine_id: Uuid,
        input: CreateRoutineTrigger,
    ) -> Result<RoutineTriggerMutationResult> {
        input.validate()?;
        let repo = RoutineRepo::new(&self.db);
        let Some(_routine) = repo.get(routine_id).await.map_err(map_sql_error)? else {
            return Err(validation(format!("routine {routine_id} not found")));
        };
        let kind = input.kind.clone();
        let result = repo
            .create_trigger_with_revision(routine_id, &input.into_record())
            .await?;
        self.dispatch(RoutineHookEvent::TriggerCreated {
            id: result.trigger.id,
            routine_id,
            kind: kind.clone(),
        })
        .await?;
        Ok(result)
    }

    pub async fn update_trigger(
        &self,
        id: Uuid,
        patch: UpdateRoutineTrigger,
    ) -> Result<Option<RoutineTriggerMutationResult>> {
        patch.validate()?;
        let repo = RoutineRepo::new(&self.db);
        let result = repo
            .update_trigger_with_revision(id, &patch.into_record())
            .await?;
        if let Some(ref r) = result {
            self.dispatch(RoutineHookEvent::TriggerUpdated {
                id: r.trigger.id,
                routine_id: r.trigger.routine_id,
                kind: r.trigger.kind.clone(),
            })
            .await?;
        }
        Ok(result)
    }

    pub async fn delete_trigger(
        &self,
        id: Uuid,
    ) -> Result<Option<RoutineRevisionRow>> {
        let repo = RoutineRepo::new(&self.db);
        let existing = repo.get_trigger(id).await.map_err(map_sql_error)?;
        let Some(existing) = existing else {
            return Ok(None);
        };
        let revision = repo
            .delete_trigger_with_revision(
                id,
                existing.updated_by_agent_id,
                existing.updated_by_user_id.as_deref(),
                None,
            )
            .await?;
        self.dispatch(RoutineHookEvent::TriggerDeleted {
            id,
            routine_id: existing.routine_id,
            kind: existing.kind.clone(),
        })
        .await?;
        Ok(revision)
    }

    // ---- runs / revisions ---------------------------------------------------

    pub async fn list_runs(
        &self,
        routine_id: Uuid,
        limit: i64,
    ) -> Result<Vec<RoutineRunRow>> {
        RoutineRepo::new(&self.db)
            .list_runs(routine_id, limit)
            .await
            .map_err(map_sql_error)
    }

    pub async fn list_revisions(
        &self,
        routine_id: Uuid,
    ) -> Result<Vec<RoutineRevisionRow>> {
        RoutineRepo::new(&self.db)
            .list_revisions(routine_id)
            .await
            .map_err(map_sql_error)
    }

    pub async fn restore_revision(
        &self,
        routine_id: Uuid,
        revision_id: Uuid,
    ) -> Result<Option<RoutineRestoreSummary>> {
        let repo = RoutineRepo::new(&self.db);
        // restore_revision_by_id returns Result<RoutineRestoreResult> (not Option),
        // returning Err(not_found) when the routine or revision is missing.
        let result = match repo
            .restore_revision_by_id(routine_id, revision_id, None, None, None)
            .await
        {
            Ok(r) => r,
            Err(pc_errors::Error::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        self.dispatch(RoutineHookEvent::Updated {
            id: result.routine.id,
            company_id: result.routine.company_id,
            title: result.routine.title.clone(),
            status: result.routine.status.clone(),
        })
        .await?;
        Ok(Some(RoutineRestoreSummary {
            routine: result.routine,
            revision: result.revision,
            restored_from_revision_id: result.restored_from_revision_id,
            restored_from_revision_number: result.restored_from_revision_number,
        }))
    }
}

// =============================================================================
// R605: detail aggregate
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineDetail {
    #[serde(flatten)]
    pub routine: RoutineRow,
    pub triggers: Vec<RoutineTriggerRow>,
    pub recent_runs: Vec<pc_repos::routine::RoutineRunSummary>,
    pub active_issue: Option<pc_repos::routine::RoutineIssueSummary>,
    pub description_document: Option<pc_repos::routine::RoutineDescriptionDocumentRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineRestoreSummary {
    pub routine: RoutineRow,
    pub revision: RoutineRevisionRow,
    pub restored_from_revision_id: Uuid,
    pub restored_from_revision_number: i32,
}

// =============================================================================
// helpers
// =============================================================================

fn map_sql_error(error: sqlx::Error) -> Error {
    internal(format!("routine database operation failed: {error}"))
}

fn conflict_with_current(current_revision_id: Option<Uuid>) -> Error {
    conflict(format!(
        "Routine was updated by someone else (currentRevisionId={:?})",
        current_revision_id
    ))
}

// make the marker visible to clippy
#[allow(dead_code)]
const _MARKER: &str = "R605";
