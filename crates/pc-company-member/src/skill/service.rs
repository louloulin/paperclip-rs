use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    skill::{CompanySkillRow, NewCompanySkill, SkillRepo, SkillSharingScope},
    Db,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum CompanySkillHookEvent {
    Created {
        company_id: Uuid,
        skill_id: Uuid,
        key: String,
    },
    SoftDeleted {
        company_id: Uuid,
        skill_id: Uuid,
    },
    Forked {
        company_id: Uuid,
        skill_id: Uuid,
        from_skill_id: Uuid,
        from_company_id: Uuid,
    },
    SharingChanged {
        company_id: Uuid,
        skill_id: Uuid,
        sharing_scope: String,
    },
    Starred {
        company_id: Uuid,
        skill_id: Uuid,
        user_id: String,
    },
    Unstarred {
        company_id: Uuid,
        skill_id: Uuid,
        user_id: String,
    },
}

#[async_trait]
pub trait CompanySkillHook: Send + Sync {
    async fn on_company_skill_event(&self, _event: CompanySkillHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopCompanySkillHook;
#[async_trait]
impl CompanySkillHook for NoopCompanySkillHook {}

#[derive(Default)]
pub struct RecordingCompanySkillHook {
    pub events: std::sync::Mutex<Vec<CompanySkillHookEvent>>,
}
impl RecordingCompanySkillHook {
    pub fn events_snapshot(&self) -> Vec<CompanySkillHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("mutex").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[async_trait]
impl CompanySkillHook for RecordingCompanySkillHook {
    async fn on_company_skill_event(&self, e: CompanySkillHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CompanySkillError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("skill not found: {0}")]
    NotFound(Uuid),
    #[error("skill already soft-deleted")]
    AlreadyDeleted,
    #[error("skill already exists (key/slug conflict)")]
    Conflict,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for CompanySkillError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type SkillResult<T> = std::result::Result<T, CompanySkillError>;

#[derive(Clone)]
pub struct CompanySkillService {
    db: Db,
    hooks: Vec<Arc<dyn CompanySkillHook>>,
}

impl CompanySkillService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn CompanySkillHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn CompanySkillHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> SkillRepo<'_> {
        SkillRepo::new(&self.db)
    }
    async fn dispatch(&self, e: CompanySkillHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_company_skill_event(e.clone()).await {
                tracing::warn!(?err, "company skill hook failed");
            }
        }
    }
    fn require_non_nil(id: Uuid, field: &str) -> SkillResult<()> {
        if id.is_nil() {
            Err(CompanySkillError::Validation(format!(
                "{field} is required"
            )))
        } else {
            Ok(())
        }
    }

    // ---- Read paths ----
    pub async fn list_for_company(&self, company_id: Uuid) -> SkillResult<Vec<CompanySkillRow>> {
        Self::require_non_nil(company_id, "companyId")?;
        Ok(self.repo().list_for_company(company_id).await?)
    }

    pub async fn list_categories(&self, company_id: Uuid) -> SkillResult<Vec<String>> {
        Self::require_non_nil(company_id, "companyId")?;
        Ok(self.repo().list_categories(company_id).await?)
    }

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> SkillResult<Option<CompanySkillRow>> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(id, "skillId")?;
        Ok(self.repo().get(company_id, id).await?)
    }

    pub async fn get_by_slug(
        &self,
        company_id: Uuid,
        slug: &str,
    ) -> SkillResult<Option<CompanySkillRow>> {
        Self::require_non_nil(company_id, "companyId")?;
        if slug.trim().is_empty() {
            return Err(CompanySkillError::Validation("slug is required".into()));
        }
        Ok(self.repo().get_by_slug(company_id, slug).await?)
    }

    pub async fn list_versions(
        &self,
        skill_id: Uuid,
    ) -> SkillResult<Vec<pc_repos::skill::CompanySkillVersionRow>> {
        Self::require_non_nil(skill_id, "skillId")?;
        Ok(self.repo().list_versions(skill_id).await?)
    }

    pub async fn count_stars(&self, company_id: Uuid, skill_id: Uuid) -> SkillResult<i64> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(skill_id, "skillId")?;
        Ok(self.repo().count_stars(company_id, skill_id).await?)
    }

    // ---- Write paths ----
    pub async fn create(&self, input: NewCompanySkill) -> SkillResult<CompanySkillRow> {
        Self::require_non_nil(input.company_id, "companyId")?;
        if input.key.trim().is_empty() {
            return Err(CompanySkillError::Validation("key is required".into()));
        }
        if input.slug.trim().is_empty() {
            return Err(CompanySkillError::Validation("slug is required".into()));
        }
        if input.name.trim().is_empty() {
            return Err(CompanySkillError::Validation("name is required".into()));
        }
        if input.markdown.is_empty() {
            return Err(CompanySkillError::Validation(
                "markdown must not be empty".into(),
            ));
        }
        if self
            .repo()
            .get_by_slug(input.company_id, &input.slug)
            .await?
            .is_some()
        {
            return Err(CompanySkillError::Conflict);
        }
        let row = self.repo().create(&input).await?;
        self.dispatch(CompanySkillHookEvent::Created {
            company_id: row.company_id,
            skill_id: row.id,
            key: row.key.clone(),
        })
        .await;
        Ok(row)
    }

    pub async fn soft_delete(&self, company_id: Uuid, id: Uuid) -> SkillResult<bool> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(id, "skillId")?;
        let row = self
            .repo()
            .get(company_id, id)
            .await?
            .ok_or(CompanySkillError::NotFound(id))?;
        if row.deleted_at.is_some() {
            return Err(CompanySkillError::AlreadyDeleted);
        }
        let ok = self.repo().soft_delete(company_id, id).await?;
        if ok {
            self.dispatch(CompanySkillHookEvent::SoftDeleted {
                company_id,
                skill_id: id,
            })
            .await;
        }
        Ok(ok)
    }

    pub async fn archive(&self, company_id: Uuid, id: Uuid) -> SkillResult<bool> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(id, "skillId")?;
        Ok(self.repo().archive(company_id, id).await?)
    }

    pub async fn set_sharing_scope(
        &self,
        company_id: Uuid,
        id: Uuid,
        scope: SkillSharingScope,
    ) -> SkillResult<CompanySkillRow> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(id, "skillId")?;
        let _ = self
            .repo()
            .get(company_id, id)
            .await?
            .ok_or(CompanySkillError::NotFound(id))?;
        // Use the raw update_status to switch sharing scope, treating it as a metadata change.
        let scope_str = scope.as_str();
        sqlx::query(
            "UPDATE company_skills SET sharing_scope = $1, updated_at = now() WHERE company_id = $2 AND id = $3",
        )
        .bind(scope_str)
        .bind(company_id)
        .bind(id)
        .execute(self.db.pool())
        .await?;
        let row = self
            .repo()
            .get(company_id, id)
            .await?
            .ok_or(CompanySkillError::NotFound(id))?;
        self.dispatch(CompanySkillHookEvent::SharingChanged {
            company_id,
            skill_id: id,
            sharing_scope: scope_str.to_string(),
        })
        .await;
        Ok(row)
    }

    pub async fn fork(
        &self,
        target_company_id: Uuid,
        source_company_id: Uuid,
        source_skill_id: Uuid,
        new_key: &str,
        new_slug: &str,
        new_name: &str,
        created_by_user_id: Option<&str>,
    ) -> SkillResult<CompanySkillRow> {
        Self::require_non_nil(target_company_id, "targetCompanyId")?;
        Self::require_non_nil(source_company_id, "sourceCompanyId")?;
        Self::require_non_nil(source_skill_id, "sourceSkillId")?;
        if new_key.trim().is_empty() || new_slug.trim().is_empty() || new_name.trim().is_empty() {
            return Err(CompanySkillError::Validation(
                "key, slug, and name are required for fork".into(),
            ));
        }
        let _ = new_key;
        let _ = new_slug; // repo generates fork-specific key/slug
        if let Some(s) = created_by_user_id {
            if s.trim().is_empty() {
                return Err(CompanySkillError::Validation(
                    "userId must not be empty".into(),
                ));
            }
        }
        let new_id = Uuid::new_v4();
        self.repo()
            .fork_from_skill(target_company_id, source_skill_id, new_id, new_name)
            .await?;
        // Update creator on the new row
        if let Some(uid) = created_by_user_id {
            sqlx::query(
                "UPDATE company_skills SET created_by_user_id=$1, updated_at=now() WHERE id=$2",
            )
            .bind(uid)
            .bind(new_id)
            .execute(self.db.pool())
            .await?;
        }
        let row = self
            .repo()
            .get(target_company_id, new_id)
            .await?
            .ok_or(CompanySkillError::NotFound(new_id))?;
        self.dispatch(CompanySkillHookEvent::Forked {
            company_id: target_company_id,
            skill_id: row.id,
            from_skill_id: source_skill_id,
            from_company_id: row.forked_from_company_id.unwrap_or(source_company_id),
        })
        .await;
        Ok(row)
    }

    pub async fn star(&self, company_id: Uuid, skill_id: Uuid, user_id: &str) -> SkillResult<()> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(skill_id, "skillId")?;
        if user_id.trim().is_empty() {
            return Err(CompanySkillError::Validation("userId is required".into()));
        }
        self.repo()
            .star(company_id, skill_id, None, Some(user_id))
            .await?;
        self.dispatch(CompanySkillHookEvent::Starred {
            company_id,
            skill_id,
            user_id: user_id.to_string(),
        })
        .await;
        Ok(())
    }

    pub async fn unstar(&self, company_id: Uuid, skill_id: Uuid, user_id: &str) -> SkillResult<()> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(skill_id, "skillId")?;
        if user_id.trim().is_empty() {
            return Err(CompanySkillError::Validation("userId is required".into()));
        }
        self.repo()
            .unstar(company_id, skill_id, None, Some(user_id))
            .await?;
        self.dispatch(CompanySkillHookEvent::Unstarred {
            company_id,
            skill_id,
            user_id: user_id.to_string(),
        })
        .await;
        Ok(())
    }

    pub async fn rename(
        &self,
        company_id: Uuid,
        id: Uuid,
        new_name: &str,
    ) -> SkillResult<CompanySkillRow> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(id, "skillId")?;
        if new_name.trim().is_empty() {
            return Err(CompanySkillError::Validation(
                "name must not be empty".into(),
            ));
        }
        if !self.repo().rename_skill(company_id, id, new_name).await? {
            return Err(CompanySkillError::NotFound(id));
        }
        let row = self
            .repo()
            .get(company_id, id)
            .await?
            .ok_or(CompanySkillError::NotFound(id))?;
        Ok(row)
    }

    pub async fn get_config(&self, company_id: Uuid, skill_id: Uuid) -> SkillResult<Option<Value>> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(skill_id, "skillId")?;
        Ok(self.repo().get_config(company_id, skill_id).await?)
    }
}
