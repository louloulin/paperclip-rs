//! R606: `FolderService` — high-level facade for folder CRUD + hierarchy +
//! slug + lifecycle hooks.
//!
//! Aligned with `paperclip/server/src/services/folders.ts`. The Node service is
//! ~620 lines and includes `folderService`, `normalizeFolderSlug`,
//! `buildFolderViews`, advisory locks for concurrent mutations, and
//! `ensureMyFolder` / `ensureBundledFolder` helpers.
//!
//! The Rust port focuses on **CRUD + hierarchy + slug + hook events** for the
//! first cut. Advisory locks / bundled-folder reconciliation / personal-folder
//! ensure routines are deferred to R607+ since they overlap with the dedicated
//! `pc-repos/folder/personal.rs` module.

use std::sync::Arc;

use async_trait::async_trait;
use pc_errors::{conflict, forbidden, internal, unprocessable, validation, Error, Result};
use pc_repos::folder::slug::normalize_folder_slug;
use pc_repos::folder::{CountsQuery, FolderKind, FolderRepo, FolderRow, FolderView, NewFolder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Maximum folder nesting depth (mirrors `MAX_FOLDER_DEPTH` in
/// `pc-repos/src/folder/slug.rs`).
pub const MAX_FOLDER_DEPTH: i32 = 4;

/// Reserved root-level skill folder slugs (system-managed).
const RESERVED_ROOT_SLUGS: &[&str] = &["bundled", "my", "projects"];

// =============================================================================
// R606: folder lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FolderHookEvent {
    Created {
        id: Uuid,
        company_id: Uuid,
        kind: String,
        parent_id: Option<Uuid>,
        path: String,
    },
    Updated {
        id: Uuid,
        company_id: Uuid,
        kind: String,
        path: String,
    },
    Moved {
        id: Uuid,
        company_id: Uuid,
        old_parent_id: Option<Uuid>,
        new_parent_id: Option<Uuid>,
    },
    Deleted {
        id: Uuid,
        company_id: Uuid,
        kind: String,
    },
}

// =============================================================================
// R606: FolderHook trait
// =============================================================================

#[async_trait]
pub trait FolderHook: Send + Sync {
    async fn on_folder_event(&self, _event: FolderHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopFolderHook;
#[async_trait]
impl FolderHook for NoopFolderHook {}

#[derive(Default)]
pub struct RecordingFolderHook {
    pub events: std::sync::Mutex<Vec<FolderHookEvent>>,
}

#[async_trait]
impl FolderHook for RecordingFolderHook {
    async fn on_folder_event(&self, event: FolderHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingFolderHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<FolderHookEvent> {
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
// R606: Public input / patch types
// =============================================================================

/// Input for `FolderService::create`.
#[derive(Debug, Clone)]
pub struct CreateFolder {
    pub company_id: Uuid,
    pub kind: FolderKind,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: Option<String>,
    pub color: Option<String>,
    pub system_key: Option<String>,
    pub position: Option<i32>,
}

impl CreateFolder {
    fn normalize(&self) -> Result<NormalizedCreate> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(validation("folder name must not be empty"));
        }
        if self.company_id.is_nil() {
            return Err(validation("companyId is required"));
        }
        let slug_input = self.slug.clone().unwrap_or_else(|| name.clone());
        let slug = normalize_folder_slug(&slug_input);
        if slug.is_empty() {
            return Err(validation("folder slug must not be empty"));
        }
        // reserved root slugs are system-managed
        if self.kind == FolderKind::Skill
            && self.parent_id.is_none()
            && RESERVED_ROOT_SLUGS.contains(&slug.as_str())
        {
            return Err(forbidden(
                "skill reserved root slugs (bundled/my/projects) are system-managed",
            ));
        }
        let color = self
            .color
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        Ok(NormalizedCreate {
            name,
            slug,
            color,
            position: self.position.unwrap_or(0),
        })
    }
}

struct NormalizedCreate {
    name: String,
    slug: String,
    color: Option<String>,
    position: i32,
}

/// Partial update for a folder.
#[derive(Debug, Clone, Default)]
pub struct FolderPatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub color: Option<String>,
    pub position: Option<i32>,
    pub parent_id: Option<Option<Uuid>>,
}

impl FolderPatch {
    fn validate(&self) -> Result<()> {
        if let Some(n) = &self.name {
            if n.trim().is_empty() {
                return Err(validation("folder name must not be empty"));
            }
        }
        if let Some(s) = &self.slug {
            let normalized = normalize_folder_slug(s);
            if normalized.is_empty() {
                return Err(validation("folder slug must not be empty"));
            }
        }
        Ok(())
    }

    /// Convert to repo patch with normalization applied to slug.
    fn into_repo_patch(self) -> pc_repos::folder::FolderPatch {
        pc_repos::folder::FolderPatch {
            name: self.name.map(|n| n.trim().to_string()),
            slug: self.slug.map(|s| normalize_folder_slug(&s)),
            color: self.color.as_ref().map(|c| {
                let trimmed = c.trim();
                if trimmed.is_empty() {
                    String::new()
                } else {
                    trimmed.to_owned()
                }
            }),
            position: self.position,
            parent_id: self.parent_id,
        }
    }
}

// =============================================================================
// R606: FolderService
// =============================================================================

#[derive(Clone)]
pub struct FolderService {
    db: pc_repos::Db,
    hooks: Vec<Arc<dyn FolderHook>>,
}

impl FolderService {
    #[must_use]
    pub fn new(db: pc_repos::Db) -> Self {
        Self {
            db,
            hooks: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_hooks(db: pc_repos::Db, hooks: Vec<Arc<dyn FolderHook>>) -> Self {
        Self { db, hooks }
    }

    #[must_use]
    pub fn add_hook(mut self, hook: Arc<dyn FolderHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    async fn dispatch(&self, event: FolderHookEvent) -> Result<()> {
        for hook in &self.hooks {
            if let Err(e) = hook.on_folder_event(event.clone()).await {
                tracing::warn!(?event, error = %e, "folder hook failed");
            }
        }
        Ok(())
    }

    // ---- read ---------------------------------------------------------------

    /// List all folders of a given kind for a company, enriched with item
    /// counts and aggregated `allCount` / `unfiledCount`.
    pub async fn list_with_counts(
        &self,
        company_id: Uuid,
        kind: FolderKind,
    ) -> Result<pc_repos::folder::FolderListResult> {
        CountsQuery::new(&self.db)
            .list_with_counts(company_id, kind)
            .await
            .map_err(map_repo_error)
    }

    /// Fetch a single folder by id (company-scoped) and return its view
    /// (with path / depth / itemCount).
    pub async fn get(&self, company_id: Uuid, folder_id: Uuid) -> Result<Option<FolderView>> {
        let repo = FolderRepo::new(&self.db);
        let row = repo
            .get(company_id, folder_id)
            .await
            .map_err(map_repo_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let kind = parse_kind(&row.kind)?;
        let all = repo
            .list_by_kind(company_id, kind)
            .await
            .map_err(map_repo_error)?;
        let views = pc_repos::folder::view::build_folder_views(&all).map_err(map_repo_error)?;
        let counts = CountsQuery::new(&self.db)
            .list_with_counts(company_id, kind)
            .await
            .map_err(map_repo_error)?;
        let view = views.get(&folder_id).cloned().ok_or_else(|| {
            internal("build_folder_views returned no entry for the fetched folder")
        })?;
        // hydrate item_count from the counts result
        let hydrated = FolderView {
            item_count: counts
                .folders
                .iter()
                .find(|f| f.id == folder_id)
                .map(|f| f.item_count)
                .unwrap_or(0),
            ..view
        };
        Ok(Some(hydrated))
    }

    // ---- write --------------------------------------------------------------

    pub async fn create(&self, input: CreateFolder) -> Result<FolderRow> {
        let normalized = input.normalize()?;

        // depth check — refuse if parent depth + 1 > MAX
        if let Some(parent_id) = input.parent_id {
            let repo = FolderRepo::new(&self.db);
            let parent = repo
                .get(input.company_id, parent_id)
                .await
                .map_err(map_repo_error)?
                .ok_or_else(|| validation(format!("parent folder {parent_id} not found")))?;
            if parent.kind != input.kind.as_str() {
                return Err(validation(
                    "parent folder kind does not match new folder kind",
                ));
            }
            if parent.system_key.is_some() {
                return Err(forbidden("system-managed folders cannot have children"));
            }
            // compute parent depth
            let all = repo
                .list_by_kind(input.company_id, input.kind)
                .await
                .map_err(map_repo_error)?;
            let parent_view = pc_repos::folder::view::build_folder_views(&all)
                .map_err(map_repo_error)?
                .get(&parent_id)
                .cloned()
                .ok_or_else(|| internal("parent not in kind views"))?;
            if parent_view.depth + 1 > MAX_FOLDER_DEPTH {
                return Err(unprocessable(format!(
                    "folder depth would exceed maximum {MAX_FOLDER_DEPTH}"
                )));
            }
        }

        // slug uniqueness per parent
        let repo = FolderRepo::new(&self.db);
        let conflict_row = repo
            .find_by_slug(input.company_id, input.kind, &normalized.slug)
            .await
            .map_err(map_repo_error)?;
        if conflict_row.is_some() && input.parent_id.is_none() {
            return Err(conflict(format!(
                "folder slug '{}' already exists at root for this kind",
                normalized.slug
            )));
        }

        let next_position = if let Some(p) = input.position {
            p
        } else {
            // compute max(position) + 1 under parent
            let max: i32 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM folders                  WHERE company_id=$1 AND kind=$2 AND                  (($3::uuid IS NULL AND parent_id IS NULL) OR parent_id=$3)",
            )
            .bind(input.company_id)
            .bind(input.kind.as_str())
            .bind(input.parent_id)
            .fetch_one(self.db.pool())
            .await
            .map_err(|e| internal(format!("compute next position: {e}")))?;
            max
        };

        let new_folder = NewFolder {
            company_id: input.company_id,
            kind: input.kind,
            parent_id: input.parent_id,
            name: normalized.name.clone(),
            slug: normalized.slug.clone(),
            system_key: input.system_key.clone(),
            color: normalized.color.clone(),
            position: next_position,
        };
        let created = FolderRepo::new(&self.db)
            .create(&new_folder)
            .await
            .map_err(map_repo_error)?;

        // build path via views
        let all = repo
            .list_by_kind(created.company_id, parse_kind(&created.kind)?)
            .await
            .map_err(map_repo_error)?;
        let views = pc_repos::folder::view::build_folder_views(&all).map_err(map_repo_error)?;
        let path = views
            .get(&created.id)
            .map(|v| v.path.clone())
            .unwrap_or_else(|| created.slug.clone());

        self.dispatch(FolderHookEvent::Created {
            id: created.id,
            company_id: created.company_id,
            kind: created.kind.clone(),
            parent_id: created.parent_id,
            path,
        })
        .await?;
        Ok(created)
    }

    pub async fn update(
        &self,
        company_id: Uuid,
        folder_id: Uuid,
        patch: FolderPatch,
    ) -> Result<Option<FolderRow>> {
        patch.validate()?;
        let repo = FolderRepo::new(&self.db);
        let existing = repo
            .get(company_id, folder_id)
            .await
            .map_err(map_repo_error)?
            .ok_or_else(|| validation(format!("folder {folder_id} not found")))?;
        if existing.system_key.is_some() {
            return Err(forbidden("system-managed folders cannot be changed"));
        }
        if let Some(parent) = patch.parent_id.flatten() {
            let parent_row = repo
                .get(company_id, parent)
                .await
                .map_err(map_repo_error)?
                .ok_or_else(|| validation(format!("new parent {parent} not found")))?;
            if parent_row.kind != existing.kind {
                return Err(validation("new parent kind does not match folder kind"));
            }
        }

        let repo_patch = patch.into_repo_patch();
        let updated = repo
            .patch(company_id, folder_id, &repo_patch)
            .await
            .map_err(map_repo_error)?;
        eprintln!("DEBUG: patch OK");

        let Some(updated) = updated else {
            return Ok(None);
        };

        let kind = parse_kind(&updated.kind)?;
        let all = repo
            .list_by_kind(company_id, kind)
            .await
            .map_err(map_repo_error)?;
        let views = pc_repos::folder::view::build_folder_views(&all).map_err(map_repo_error)?;
        let path = views
            .get(&updated.id)
            .map(|v| v.path.clone())
            .unwrap_or_else(|| updated.slug.clone());

        let event = if repo_patch.parent_id.is_some() {
            FolderHookEvent::Moved {
                id: updated.id,
                company_id: updated.company_id,
                old_parent_id: existing.parent_id,
                new_parent_id: updated.parent_id,
            }
        } else {
            FolderHookEvent::Updated {
                id: updated.id,
                company_id: updated.company_id,
                kind: updated.kind.clone(),
                path,
            }
        };
        self.dispatch(event).await?;
        Ok(Some(updated))
    }

    /// R800: 删除一个 folder (returns FolderRow; RepoError::NotFound on miss).
    pub async fn delete(&self, company_id: Uuid, folder_id: Uuid) -> Result<FolderRow> {
        let repo = FolderRepo::new(&self.db);
        let existing = repo
            .get(company_id, folder_id)
            .await
            .map_err(map_repo_error)?
            .ok_or_else(|| validation(format!("folder {folder_id} not found")))?;
        if existing.system_key.is_some() {
            return Err(forbidden("system-managed folders cannot be deleted"));
        }
        // R800: delete returns FolderRow directly; RepoError::NotFound on miss
        let row = repo
            .delete(company_id, folder_id)
            .await
            .map_err(map_repo_error)?;
        self.dispatch(FolderHookEvent::Deleted {
            id: folder_id,
            company_id,
            kind: existing.kind.clone(),
        })
        .await?;
        Ok(row)
    }
}

// =============================================================================
// helpers
// =============================================================================

fn parse_kind(s: &str) -> Result<FolderKind> {
    FolderKind::parse(s).ok_or_else(|| internal(format!("unknown folder kind in row: {s}")))
}

fn map_repo_error(error: pc_repos::RepoError) -> Error {
    match error {
        pc_repos::RepoError::Sql(e) => internal(format!("folder database operation failed: {e}")),
        pc_repos::RepoError::Invalid(msg) => unprocessable(msg),
        pc_repos::RepoError::NotFound { entity, id } => {
            pc_errors::not_found(format!("{entity} {id}"))
        }
        pc_repos::RepoError::Json(e) => internal(format!("folder json decode failed: {e}")),
        pc_repos::RepoError::Core(e) => internal(format!("folder core invariant: {e}")),
    }
}

/// Helper for tests that want raw `Value` from a hook event.
#[allow(dead_code)]
pub(crate) fn event_to_value(event: &FolderHookEvent) -> Value {
    serde_json::to_value(event).unwrap_or(Value::Null)
}
