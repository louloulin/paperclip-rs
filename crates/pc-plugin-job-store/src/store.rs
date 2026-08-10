//! PluginJobStore — service 层封装。
//!
//! 高内聚：本模块是 Node `pluginJobStore()` factory 返回的对象的 1:1 Rust 复刻。
//! 所有公共方法都是 Node 公开 API 的对应实现。
//!
//! 低耦合：
//! - 持有 `pc_db::Db` + `pc_repos::PluginRepo`，不直接接触 sqlx
//! - 所有公开方法返回 `PluginJobStoreResult<T>` 或具体 row 类型
//! - 业务编排（syncJobDeclarations）保持在 service 层，DB 层只负责基础 CRUD

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use pc_db::Db;
use pc_repos::plugin::{PluginJobRow, PluginJobRunRow, PluginRepo};

use crate::declaration::{CompleteJobRunInput, CreateJobRunInput, PluginJobDeclaration};
use crate::errors::{PluginJobStoreError, PluginJobStoreResult};
use crate::types::{JobDefinitionStatus, JobRunStatus};

// ============================================================================
// PluginJobStore
// ============================================================================

/// PluginJobStore —— 1:1 对齐 Node `pluginJobStore(db)` 返回对象。
///
/// 设计要点：
/// - cheap clone：`Db` 内部是 `Arc<PgPool>`，所以 service 也 cheap clone
/// - 持有 `Db` 而非 `PluginRepo` —— 每次访问时构造（cheap, 仅一个引用）
#[derive(Clone)]
pub struct PluginJobStore {
    db: Db,
}

impl PluginJobStore {
    /// 工厂函数（与 Node `pluginJobStore(db)` 1:1 对齐）。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    fn repo(&self) -> PluginRepo<'_> {
        PluginRepo::new(&self.db)
    }

    /// 检查 plugin 是否存在；不存在抛 `PluginNotFound`。
    async fn assert_plugin_exists(&self, plugin_id: Uuid) -> PluginJobStoreResult<()> {
        let exists = self.repo().assert_plugin_exists(plugin_id).await?;
        if !exists {
            return Err(PluginJobStoreError::PluginNotFound(plugin_id.to_string()));
        }
        Ok(())
    }

    // ========================================================================
    // Job declarations (`plugin_jobs`)
    // ========================================================================

    /// 把 manifest 声明的 jobs 同步到 `plugin_jobs` 表。
    ///
    /// 行为与 Node 1:1：
    /// - **新 job** —— 插入，`status="active"`
    /// - **已存在 job** —— 若 `schedule` 变了则更新；若之前是 `paused` 则重新激活
    /// - **manifest 中消失的 job** —— 标 `paused`（历史保留）
    ///
    /// 用 `(plugin_id, job_key)` 唯一约束做 conflict resolution。
    pub async fn sync_job_declarations(
        &self,
        plugin_id: Uuid,
        declarations: &[PluginJobDeclaration],
    ) -> PluginJobStoreResult<()> {
        self.assert_plugin_exists(plugin_id).await?;

        // 1. 读取现有 jobs
        let existing = self.repo().list_jobs(plugin_id).await?;

        // 2. upsert 每个 declared job
        let mut declared_keys: HashSet<String> = HashSet::new();
        for decl in declarations {
            declared_keys.insert(decl.job_key.clone());
            let schedule = decl.schedule_or_empty().to_string();
            if let Some(existing_row) = existing.iter().find(|r| r.job_key == decl.job_key) {
                // 已存在：根据差异更新
                let mut updates: Vec<&str> = Vec::new();
                if existing_row.schedule != schedule {
                    updates.push("schedule_changed");
                }
                if existing_row.status == "paused" {
                    updates.push("resume_from_paused");
                }
                if !updates.is_empty() {
                    // schedule_changed → 用 upsert_job（update schedule + updated_at）
                    // resume_from_paused → update_job_status('active')
                    if updates.contains(&"schedule_changed") {
                        self.repo()
                            .upsert_job(plugin_id, &decl.job_key, &schedule)
                            .await?;
                    }
                    if updates.contains(&"resume_from_paused") {
                        self.repo()
                            .update_job_status(existing_row.id, JobDefinitionStatus::Active.as_str())
                            .await?;
                    }
                }
            } else {
                // 新声明
                self.repo()
                    .upsert_job(plugin_id, &decl.job_key, &schedule)
                    .await?;
            }
        }

        // 3. 把 manifest 中**消失**的 job 标 paused
        for row in &existing {
            if !declared_keys.contains(&row.job_key) && row.status != "paused" {
                self.repo()
                    .update_job_status(row.id, JobDefinitionStatus::Paused.as_str())
                    .await?;
            }
        }
        Ok(())
    }

    /// 列出 plugin 的所有 jobs（可选 status 过滤）。
    pub async fn list_jobs(
        &self,
        plugin_id: Uuid,
        status: Option<JobDefinitionStatus>,
    ) -> PluginJobStoreResult<Vec<PluginJobRow>> {
        let s = status.map(|s| s.as_str());
        Ok(self.repo().list_jobs_filtered(plugin_id, s).await?)
    }

    /// 通过 composite key `(plugin_id, job_key)` 取 job。
    pub async fn get_job_by_key(
        &self,
        plugin_id: Uuid,
        job_key: &str,
    ) -> PluginJobStoreResult<Option<PluginJobRow>> {
        Ok(self.repo().get_job_by_key(plugin_id, job_key).await?)
    }

    /// 通过主键取 job（plugin 不限制）。
    pub async fn get_job_by_id(
        &self,
        job_id: Uuid,
    ) -> PluginJobStoreResult<Option<PluginJobRow>> {
        Ok(self.repo().get_job_by_id(job_id).await?)
    }

    /// 通过主键取 job（限定 plugin 防越权）。
    pub async fn get_job_by_id_for_plugin(
        &self,
        plugin_id: Uuid,
        job_id: Uuid,
    ) -> PluginJobStoreResult<Option<PluginJobRow>> {
        Ok(self.repo().get_job_by_id_for_plugin(plugin_id, job_id).await?)
    }

    /// 更新 job 状态。
    pub async fn update_job_status(
        &self,
        job_id: Uuid,
        status: JobDefinitionStatus,
    ) -> PluginJobStoreResult<()> {
        self.repo().update_job_status(job_id, status.as_str()).await?;
        Ok(())
    }

    /// 推进 `last_run_at` / `next_run_at`。
    pub async fn update_run_timestamps(
        &self,
        job_id: Uuid,
        last_run_at: DateTime<Utc>,
        next_run_at: Option<DateTime<Utc>>,
    ) -> PluginJobStoreResult<()> {
        use pc_core::Timestamp;
        self.repo()
            .update_run_timestamps(
                job_id,
                Timestamp::from_dt(last_run_at),
                next_run_at.map(Timestamp::from_dt),
            )
            .await?;
        Ok(())
    }

    /// 删除 plugin 的所有 jobs（CASCADE 删除 runs）。
    pub async fn delete_all_jobs(&self, plugin_id: Uuid) -> PluginJobStoreResult<u64> {
        Ok(self.repo().delete_all_jobs(plugin_id).await?)
    }

    // ========================================================================
    // Job runs (`plugin_job_runs`)
    // ========================================================================

    /// 创建一个 status=`queued` 的 run 记录（在派发 RPC 前调用）。
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

    /// 标记 run 进入 `running` 并设 `started_at = now()`。
    pub async fn mark_running(&self, run_id: Uuid) -> PluginJobStoreResult<()> {
        self.repo().mark_run_running(run_id).await?;
        Ok(())
    }

    /// 完成 run：写入最终 status / error / duration_ms / finished_at。
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

    /// 取 run（按主键）。
    pub async fn get_run_by_id(
        &self,
        run_id: Uuid,
    ) -> PluginJobStoreResult<Option<PluginJobRunRow>> {
        Ok(self.repo().get_run_by_id(run_id).await?)
    }

    /// 列出 job 的 runs（按 created_at desc）。
    pub async fn list_runs_by_job(
        &self,
        job_id: Uuid,
        limit: i64,
    ) -> PluginJobStoreResult<Vec<PluginJobRunRow>> {
        Ok(self.repo().list_runs_by_job(job_id, limit).await?)
    }

    /// 列出 plugin 的 runs（可选 status 过滤）。
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

// ============================================================================
// Factory + helpers
// ============================================================================

/// 工厂函数（与 Node `pluginJobStore(db)` 1:1 对齐）。
pub fn plugin_job_store(db: Db) -> PluginJobStore {
    PluginJobStore::new(db)
}

fn parse_uuid(s: &str, field: &str) -> PluginJobStoreResult<Uuid> {
    Uuid::parse_str(s).map_err(|_| {
        PluginJobStoreError::PluginNotFound(format!("invalid uuid for {field}: {s}"))
    })
}

