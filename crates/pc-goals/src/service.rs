//! R607: `GoalService` — high-level facade for goal CRUD + hierarchy +
//! status state machine + lifecycle hooks.
//!
//! Aligned with `paperclip/server/src/services/goals.ts`. The Node service is
//! ~80 lines and exposes 6 methods: list / getById / getDefaultCompanyGoal /
//! create / update / remove.
//!
//! The Rust port focuses on **CRUD + hierarchy + status state machine +
//! hook events** for the first cut. Back-compat shims (`create_simple` /
//! `get_id` / `delete_one`) and bulk operations are deferred to R608+.

use std::sync::Arc;

use async_trait::async_trait;
use pc_errors::{conflict, internal, unprocessable, validation, Error, Result};
use pc_repos::goal::{
    GoalLevel, GoalPatch as RepoGoalPatch, GoalRepo, GoalRow, GoalStatus, NewGoal,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// =============================================================================
// R607: goal lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GoalHookEvent {
    Created {
        id: Uuid,
        company_id: Uuid,
        title: String,
        level: String,
        status: String,
        parent_id: Option<Uuid>,
    },
    Updated {
        id: Uuid,
        company_id: Uuid,
        title: String,
        level: String,
        status: String,
    },
    ParentChanged {
        id: Uuid,
        company_id: Uuid,
        old_parent_id: Option<Uuid>,
        new_parent_id: Option<Uuid>,
    },
    StatusChanged {
        id: Uuid,
        company_id: Uuid,
        old_status: String,
        new_status: String,
    },
    Deleted {
        id: Uuid,
        company_id: Uuid,
    },
}

// =============================================================================
// R607: GoalHook trait
// =============================================================================

#[async_trait]
pub trait GoalHook: Send + Sync {
    async fn on_goal_event(&self, _event: GoalHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopGoalHook;
#[async_trait]
impl GoalHook for NoopGoalHook {}

#[derive(Default)]
pub struct RecordingGoalHook {
    pub events: std::sync::Mutex<Vec<GoalHookEvent>>,
}

#[async_trait]
impl GoalHook for RecordingGoalHook {
    async fn on_goal_event(&self, event: GoalHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingGoalHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<GoalHookEvent> {
        self.events.lock().expect("lock").clone()
    }

    pub fn clear(&self) {
        self.events.lock().expect("lock").clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.lock().expect("lock").is_empty()
    }
}

// =============================================================================
// R607: Public input / patch types
// =============================================================================

/// Input for `GoalService::create`.
#[derive(Debug, Clone)]
pub struct CreateGoal {
    pub company_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub level: GoalLevel,
    pub status: GoalStatus,
    pub parent_id: Option<Uuid>,
    pub owner_agent_id: Option<Uuid>,
}

impl CreateGoal {
    fn normalize(&self) -> Result<NormalizedCreate> {
        let title = self.title.trim().to_string();
        if title.is_empty() {
            return Err(validation("goal title must not be empty"));
        }
        if self.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        Ok(NormalizedCreate {
            title,
            description: self.description.as_deref().map(str::trim).map(str::to_owned),
        })
    }
}

struct NormalizedCreate {
    title: String,
    description: Option<String>,
}

/// Partial update for a goal. Re-exported from `pc_repos::goal::GoalPatch`.
/// Service-level helpers [`validate_goal_patch`] / [`normalize_goal_patch`]
/// provide normalization before passing to the repo.
pub type GoalPatch = RepoGoalPatch;

fn validate_goal_patch(p: &GoalPatch) -> Result<()> {
    if let Some(t) = &p.title {
        if t.trim().is_empty() {
            return Err(validation("goal title must not be empty"));
        }
    }
    Ok(())
}

fn normalize_goal_patch(p: GoalPatch) -> GoalPatch {
    GoalPatch {
        title: p.title.map(|t| t.trim().to_string()),
        description: match p.description.as_deref().map(str::trim) {
            Some(t) if !t.is_empty() => Some(t.to_string()),
            _ => None,
        },
        ..p
    }
}

// =============================================================================
// R607: GoalService
// =============================================================================

#[derive(Clone)]
pub struct GoalService {
    db: pc_repos::Db,
    hooks: Vec<Arc<dyn GoalHook>>,
}

impl GoalService {
    #[must_use]
    pub fn new(db: pc_repos::Db) -> Self {
        Self {
            db,
            hooks: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_hooks(db: pc_repos::Db, hooks: Vec<Arc<dyn GoalHook>>) -> Self {
        Self { db, hooks }
    }

    #[must_use]
    pub fn add_hook(mut self, hook: Arc<dyn GoalHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    async fn dispatch(&self, event: GoalHookEvent) -> Result<()> {
        for hook in &self.hooks {
            if let Err(e) = hook.on_goal_event(event.clone()).await {
                tracing::warn!(?event, error = %e, "goal hook failed");
            }
        }
        Ok(())
    }

    // ---- read ---------------------------------------------------------------

    pub async fn list_by_company(&self, company_id: Uuid) -> Result<Vec<GoalRow>> {
        GoalRepo::new(&self.db)
            .list_by_company(company_id)
            .await
            .map_err(map_repo_error)
    }

    pub async fn list_roots(&self, company_id: Uuid) -> Result<Vec<GoalRow>> {
        GoalRepo::new(&self.db)
            .list_roots(company_id)
            .await
            .map_err(map_repo_error)
    }

    pub async fn list_children(&self, parent_id: Uuid) -> Result<Vec<GoalRow>> {
        GoalRepo::new(&self.db)
            .list_children(parent_id)
            .await
            .map_err(map_repo_error)
    }

    pub async fn get(&self, company_id: Uuid, goal_id: Uuid) -> Result<Option<GoalRow>> {
        GoalRepo::new(&self.db)
            .get(company_id, goal_id)
            .await
            .map_err(map_repo_error)
    }

    pub async fn ancestors(&self, goal_id: Uuid) -> Result<Vec<GoalRow>> {
        GoalRepo::new(&self.db)
            .ancestors(goal_id)
            .await
            .map_err(map_repo_error)
    }

    pub async fn descendants(&self, goal_id: Uuid) -> Result<Vec<GoalRow>> {
        GoalRepo::new(&self.db)
            .descendants(goal_id)
            .await
            .map_err(map_repo_error)
    }

    pub async fn count_by_status(
        &self,
        company_id: Uuid,
        status: GoalStatus,
    ) -> Result<i64> {
        GoalRepo::new(&self.db)
            .count_by_status(company_id, status)
            .await
            .map_err(map_repo_error)
    }

    /// Aligned with Node `getDefaultCompanyGoal`:
    /// 1. active company-level root goal
    /// 2. any company-level root goal
    /// 3. any company-level goal (non-root fallback)
    pub async fn get_default_company_goal(
        &self,
        company_id: Uuid,
    ) -> Result<Option<GoalRow>> {
        let repo = GoalRepo::new(&self.db);
        let rows = repo
            .list_roots(company_id)
            .await
            .map_err(map_repo_error)?;
        if let Some(active) = rows
            .iter()
            .find(|g| g.level == "company" && g.status == "active")
        {
            return Ok(Some(active.clone()));
        }
        if let Some(any) = rows.iter().find(|g| g.level == "company") {
            return Ok(Some(any.clone()));
        }
        // fallback: any company-level goal (any parent)
        let all = repo
            .list_by_company(company_id)
            .await
            .map_err(map_repo_error)?;
        Ok(all.into_iter().find(|g| g.level == "company"))
    }

    // ---- write --------------------------------------------------------------

    pub async fn create(&self, input: CreateGoal) -> Result<GoalRow> {
        let normalized = input.normalize()?;
        if let Some(parent_id) = input.parent_id {
            let repo = GoalRepo::new(&self.db);
            let parent = repo
                .get(input.company_id, parent_id)
                .await
                .map_err(map_repo_error)?
                .ok_or_else(|| validation(format!("parent goal {parent_id} not found")))?;
            // hierarchy must respect level ordering — but this is permissive
            // (any level can have any level as parent) to align with Node.
            if parse_status(&parent.status).is_terminal() {
                return Err(unprocessable(
                    "cannot attach goal to a terminal parent (completed/cancelled)",
                ));
            }
        }
        let new_goal = NewGoal {
            company_id: input.company_id,
            title: normalized.title.clone(),
            description: normalized.description.clone(),
            level: input.level,
            status: input.status,
            parent_id: input.parent_id,
            owner_agent_id: input.owner_agent_id,
        };
        let created = GoalRepo::new(&self.db)
            .create(&new_goal)
            .await
            .map_err(map_repo_error)?;

        self.dispatch(GoalHookEvent::Created {
            id: created.id,
            company_id: created.company_id,
            title: created.title.clone(),
            level: created.level.clone(),
            status: created.status.clone(),
            parent_id: created.parent_id,
        })
        .await?;
        Ok(created)
    }

    pub async fn update(
        &self,
        company_id: Uuid,
        goal_id: Uuid,
        patch: GoalPatch,
    ) -> Result<Option<GoalRow>> {
        validate_goal_patch(&patch)?;
        let repo = GoalRepo::new(&self.db);
        let existing = repo
            .get(company_id, goal_id)
            .await
            .map_err(map_repo_error)?
            .ok_or_else(|| validation(format!("goal {goal_id} not found")))?;

        // terminal status is sticky
        if let Some(new_status) = patch.status {
            if parse_status(&existing.status).is_terminal()
                && new_status != parse_status(&existing.status)
            {
                return Err(conflict(format!(
                    "goal is in terminal state {} and cannot transition",
                    existing.status
                )));
            }
        }

        // if parent_id is being changed, ensure new parent exists in same company
        if let Some(new_parent) = patch.parent_id.flatten() {
            let parent = repo
                .get(company_id, new_parent)
                .await
                .map_err(map_repo_error)?
                .ok_or_else(|| validation(format!("new parent {new_parent} not found")))?;
            if parse_status(&parent.status).is_terminal() {
                return Err(unprocessable(
                    "cannot re-parent under a terminal parent (completed/cancelled)",
                ));
            }
        }

        let patch = normalize_goal_patch(patch);
        let repo_patch = patch;
        let updated = repo
            .patch(company_id, goal_id, &repo_patch)
            .await
            .map_err(map_repo_error)?;

        let Some(updated) = updated else {
            return Ok(None);
        };

        // dispatch event(s) — if parent changed, emit ParentChanged; otherwise
        // if status changed emit StatusChanged; if either or both changed,
        // also emit Updated.
        let parent_changed = repo_patch.parent_id.is_some()
            && existing.parent_id != updated.parent_id;
        let status_changed =
            repo_patch.status.is_some() && existing.status != updated.status;

        if parent_changed {
            self.dispatch(GoalHookEvent::ParentChanged {
                id: updated.id,
                company_id: updated.company_id,
                old_parent_id: existing.parent_id,
                new_parent_id: updated.parent_id,
            })
            .await?;
        }
        if status_changed {
            self.dispatch(GoalHookEvent::StatusChanged {
                id: updated.id,
                company_id: updated.company_id,
                old_status: existing.status.clone(),
                new_status: updated.status.clone(),
            })
            .await?;
        }
        // always emit Updated (callers can dedupe by id if they care)
        self.dispatch(GoalHookEvent::Updated {
            id: updated.id,
            company_id: updated.company_id,
            title: updated.title.clone(),
            level: updated.level.clone(),
            status: updated.status.clone(),
        })
        .await?;
        Ok(Some(updated))
    }

    pub async fn delete(&self, company_id: Uuid, goal_id: Uuid) -> Result<bool> {
        let repo = GoalRepo::new(&self.db);
        let existing = match repo.get(company_id, goal_id).await.map_err(map_repo_error)? {
            Some(row) => row,
            None => return Ok(false),
        };
        let removed = repo
            .delete(company_id, goal_id)
            .await
            .map_err(map_repo_error)?;
        if removed {
            self.dispatch(GoalHookEvent::Deleted {
                id: goal_id,
                company_id,
            })
            .await?;
        }
        Ok(removed)
    }
}

// =============================================================================
// helpers
// =============================================================================

fn parse_status(s: &str) -> GoalStatus {
    GoalStatus::parse(s).unwrap_or(GoalStatus::Planned)
}

fn map_repo_error(error: pc_repos::RepoError) -> Error {
    match error {
        pc_repos::RepoError::Sql(e) => internal(format!("goal database operation failed: {e}")),
        pc_repos::RepoError::Invalid(msg) => unprocessable(msg),
        pc_repos::RepoError::NotFound { entity, id } => {
            pc_errors::not_found(format!("{entity} {id}"))
        }
        pc_repos::RepoError::Json(e) => internal(format!("goal json decode failed: {e}")),
        pc_repos::RepoError::Core(e) => internal(format!("goal core invariant: {e}")),
    }
}
