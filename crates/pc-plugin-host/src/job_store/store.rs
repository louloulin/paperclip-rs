//! PluginJobStore —— service 层封装。
//!
//! 与原 `crates/pc-plugin-job-store/src/store.rs` 等价。

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use pc_core::Timestamp;
use pc_db::Db;
use pc_repos::plugin::{PluginJobRow, PluginJobRunRow, PluginRepo};
use uuid::Uuid;

use super::declaration::{CompleteJobRunInput, CreateJobRunInput, PluginJobDeclaration};
use super::errors::{PluginJobStoreError, PluginJobStoreResult};
use super::types::{JobDefinitionStatus, JobRunStatus};

/// PluginJobStore —— 1:1 对齐 Node `pluginJobStore(db)`。
#[derive(Clone)]
pub struct PluginJobStore {
    db: Db,
}

impl PluginJobStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    fn repo(&self) -> PluginRepo<'_> {
        PluginRepo::new(&self.db)
    }

    async fn assert_plugin_exists(&self, plugin_id: Uuid) -> PluginJobStoreResult<()> {
        let exists = self.repo().assert_plugin_exists(plugin_id).await?;
        if !exists {
            return Err(PluginJobStoreError::PluginNotFound(plugin_id.to_string()));
        }
        Ok(())
    }

    // ========================================================================
    // Job declarations
    // ========================================================================

    pub async fn sync_job_declarations(
        &self,
        plugin_id: Uuid,
        declarations: &[PluginJobDeclaration],
    ) -> PluginJobStoreResult<()> {
        self.assert_plugin_exists(plugin_id).await?;

        let existing = self.repo().list_jobs(plugin_id).await?;

        let mut declared_keys: HashSet<String> = HashSet::new();
        for decl in declarations {
            declared_keys.insert(decl.job_key.clone());
            let schedule = decl.schedule_or_empty().to_string();
            if let Some(existing_row) = existing.iter().find(|r| r.job_key == decl.job_key) {
                let mut updates: Vec<&str> = Vec::new();
                if existing_row.schedule != schedule {
                    updates.push("schedule_changed");
                }
                if existing_row.status == "paused" {
                    updates.push("resume_from_paused");
                }
                if !updates.is_empty() {
                    if updates.contains(&"schedule_changed") {
                        self.repo()
                            .upsert_job(plugin_id, &decl.job_key, &schedule)
                            .await?;
                    }
                    if updates.contains(&"resume_from_paused") {
                        self.repo()
                            .update_job_status(
                                existing_row.id,
                                JobDefinitionStatus::Active.as_str(),
                            )
                            .await?;
                    }
                }
            } else {
                self.repo()
                    .upsert_job(plugin_id, &decl.job_key, &schedule)
                    .await?;
            }
        }

        for row in &existing {
            if !declared_keys.contains(&row.job_key) && row.status != "paused" {
                self.repo()
                    .update_job_status(row.id, JobDefinitionStatus::Paused.as_str())
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn list_jobs(
        &self,
        plugin_id: Uuid,
        status: Option<JobDefinitionStatus>,
    ) -> PluginJobStoreResult<Vec<PluginJobRow>> {
        let s = status.map(|s| s.as_str());
        Ok(self.repo().list_jobs_filtered(plugin_id, s).await?)
    }

    pub async fn get_job_by_key(
        &self,
        plugin_id: Uuid,
        job_key: &str,
    ) -> PluginJobStoreResult<Option<PluginJobRow>> {
        Ok(self.repo().get_job_by_key(plugin_id, job_key).await?)
    }

    pub async fn get_job_by_id(&self, job_id: Uuid) -> PluginJobStoreResult<Option<PluginJobRow>> {
        Ok(self.repo().get_job_by_id(job_id).await?)
    }

    pub async fn get_job_by_id_for_plugin(
        &self,
        plugin_id: Uuid,
        job_id: Uuid,
    ) -> PluginJobStoreResult<Option<PluginJobRow>> {
        Ok(self
            .repo()
            .get_job_by_id_for_plugin(plugin_id, job_id)
            .await?)
    }

    pub async fn update_job_status(
        &self,
        job_id: Uuid,
        status: JobDefinitionStatus,
    ) -> PluginJobStoreResult<()> {
        self.repo()
            .update_job_status(job_id, status.as_str())
            .await?;
        Ok(())
    }

    pub async fn update_run_timestamps(
        &self,
        job_id: Uuid,
        last_run_at: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
    ) -> PluginJobStoreResult<()> {
        self.repo()
            .update_run_timestamps(
                job_id,
                Timestamp::from_dt(last_run_at),
                next_run_at.map(Timestamp::from_dt),
            )
            .await?;
        Ok(())
    }

    pub async fn delete_all_jobs(&self, plugin_id: Uuid) -> PluginJobStoreResult<u64> {
        Ok(self.repo().delete_all_jobs(plugin_id).await?)
    }

    // ========================================================================
    // Job runs
    // ========================================================================

    pub async fn create_run(
        &self,
        input: CreateJobRunInput,
    ) -> PluginJobStoreResult<PluginJobRunRow> {
        let plugin_id = parse_uuid(&input.plugin_id, "plugin_id")?;
        let job_id = parse_uuid(&input.job_id, "job_id")?;
        Ok(self
            .repo()
            .create_queued_run(plugin_id, job_id, input.trigger.as_str(), None)
            .await?)
    }

    pub async fn mark_running(&self, run_id: Uuid) -> PluginJobStoreResult<()> {
        self.repo().mark_run_running(run_id).await?;
        Ok(())
    }

    pub async fn complete_run(
        &self,
        run_id: Uuid,
        input: CompleteJobRunInput,
    ) -> PluginJobStoreResult<()> {
        self.repo()
            .complete_run(
                run_id,
                input.status.as_str(),
                input.error.as_deref(),
                input.duration_ms,
            )
            .await?;
        Ok(())
    }

    pub async fn get_run_by_id(
        &self,
        run_id: Uuid,
    ) -> PluginJobStoreResult<Option<PluginJobRunRow>> {
        Ok(self.repo().get_run_by_id(run_id).await?)
    }

    pub async fn list_runs_by_job(
        &self,
        job_id: Uuid,
        limit: i64,
    ) -> PluginJobStoreResult<Vec<PluginJobRunRow>> {
        Ok(self.repo().list_runs_by_job(job_id, limit).await?)
    }

    pub async fn list_runs_by_plugin(
        &self,
        plugin_id: Uuid,
        status: Option<JobRunStatus>,
        limit: i64,
    ) -> PluginJobStoreResult<Vec<PluginJobRunRow>> {
        let s = status.map(|s| s.as_str());
        Ok(self.repo().list_runs_by_plugin(plugin_id, s, limit).await?)
    }
}

pub fn plugin_job_store(db: Db) -> PluginJobStore {
    PluginJobStore::new(db)
}

fn parse_uuid(s: &str, field: &str) -> PluginJobStoreResult<Uuid> {
    Uuid::parse_str(s)
        .map_err(|_| PluginJobStoreError::PluginNotFound(format!("invalid uuid for {field}: {s}")))
}
