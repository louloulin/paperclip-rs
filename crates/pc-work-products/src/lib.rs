//! Issue work product service。
//!
//! 对齐 Node `services/work-products.ts`：
//! - `WorkProductService`: listForIssue / getById / createForIssue / update /
//!   createManyForImport / remove
//! - `createForIssue` 在事务内把同 (company, issue, type) 下其它行的 `is_primary`
//!   置 false，再 INSERT 新行
//! - `createManyForImport`: 把每个 (issue, type) group 中"最后一个 isPrimary"作为
//!   primary，其余设为 false；在事务内 chunked INSERT

use chrono::{DateTime, Utc};
use pc_import_write_types::ImportIssueWorkProductRow;
use pc_repos::Db;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use thiserror::Error;
use uuid::Uuid;

/// Work product 类型（与 Node `IssueWorkProduct["type"]` 等价字符串）。
pub type WorkProductKind = String;
/// review_state: `none`/`pending`/`approved`/`rejected`/`changes_requested`。
pub type ReviewState = String;
/// health_status: `unknown`/`ok`/`degraded`/`down`。
pub type HealthStatus = String;

/// Issue work product（API 返回 shape，与 Node 1:1 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WorkProduct {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub issue_id: Uuid,
    pub execution_workspace_id: Option<Uuid>,
    pub runtime_service_id: Option<Uuid>,
    #[serde(rename = "type")]
    pub kind: WorkProductKind,
    pub provider: String,
    pub external_id: Option<String>,
    pub title: String,
    pub url: Option<String>,
    pub status: String,
    pub review_state: ReviewState,
    pub is_primary: bool,
    pub health_status: HealthStatus,
    pub summary: Option<String>,
    pub metadata: Option<Value>,
    pub source_trust: Option<Value>,
    pub created_by_run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Create input（不包含 issue_id / company_id）。
#[derive(Debug, Clone, Default)]
pub struct CreateWorkProductInput {
    pub project_id: Option<Uuid>,
    pub execution_workspace_id: Option<Uuid>,
    pub runtime_service_id: Option<Uuid>,
    pub kind: String,
    pub provider: String,
    pub external_id: Option<String>,
    pub title: String,
    pub url: Option<String>,
    pub status: String,
    pub review_state: Option<String>,
    pub is_primary: bool,
    pub health_status: Option<String>,
    pub summary: Option<String>,
    pub metadata: Option<Value>,
    pub source_trust: Option<Value>,
    pub created_by_run_id: Option<Uuid>,
}

/// Update patch。
#[derive(Debug, Clone, Default)]
pub struct UpdateWorkProductPatch {
    pub provider: Option<String>,
    pub external_id: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub status: Option<String>,
    pub review_state: Option<String>,
    pub is_primary: Option<bool>,
    pub health_status: Option<String>,
    pub summary: Option<String>,
    pub metadata: Option<Value>,
    pub source_trust: Option<Value>,
    pub execution_workspace_id: Option<Uuid>,
    pub runtime_service_id: Option<Uuid>,
    pub created_by_run_id: Option<Uuid>,
}

#[derive(Debug, Error)]
pub enum WorkProductError {
    #[error("postgres error: {0}")]
    Postgres(#[from] sqlx::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// WorkProduct service。
#[derive(Clone)]
pub struct WorkProductService<'a> {
    db: &'a Db,
}

impl<'a> WorkProductService<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub fn pool(&self) -> &PgPool {
        self.db.pool()
    }

    /// `listForIssue` — 按 isPrimary desc, updatedAt desc 排序。
    pub async fn list_for_issue(
        &self,
        issue_id: Uuid,
    ) -> Result<Vec<WorkProduct>, WorkProductError> {
        let rows: Vec<WorkProductRow> = sqlx::query_as::<_, WorkProductRow>(
            "SELECT id, company_id, project_id, issue_id, execution_workspace_id, runtime_service_id, \
                    type, provider, external_id, title, url, status, review_state, is_primary, \
                    health_status, summary, metadata, created_by_run_id, created_at, updated_at, \
                    source_trust \
             FROM issue_work_products WHERE issue_id = $1 \
             ORDER BY is_primary DESC, updated_at DESC",
        )
        .bind(issue_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(row_to_work_product).collect())
    }

    pub async fn get_by_id(&self, id: Uuid) -> Result<Option<WorkProduct>, WorkProductError> {
        let row: Option<WorkProductRow> = sqlx::query_as::<_, WorkProductRow>(
            "SELECT id, company_id, project_id, issue_id, execution_workspace_id, runtime_service_id, \
                    type, provider, external_id, title, url, status, review_state, is_primary, \
                    health_status, summary, metadata, created_by_run_id, created_at, updated_at, \
                    source_trust \
             FROM issue_work_products WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(row_to_work_product))
    }

    pub async fn create_for_issue(
        &self,
        issue_id: Uuid,
        company_id: Uuid,
        data: CreateWorkProductInput,
    ) -> Result<Option<WorkProduct>, WorkProductError> {
        let mut tx = self.pool().begin().await?;
        let outcome = self
            .create_for_issue_in_tx(&mut tx, issue_id, company_id, data)
            .await?;
        tx.commit().await?;
        Ok(outcome)
    }

    pub async fn update(
        &self,
        id: Uuid,
        patch: UpdateWorkProductPatch,
    ) -> Result<Option<WorkProduct>, WorkProductError> {
        let mut tx = self.pool().begin().await?;
        let outcome = self.update_in_tx(&mut tx, id, patch).await?;
        tx.commit().await?;
        Ok(outcome)
    }

    pub async fn create_many_for_import(
        &self,
        rows: Vec<ImportIssueWorkProductRow>,
    ) -> Result<(), WorkProductError> {
        if rows.is_empty() {
            return Ok(());
        }
        // 计算每个 (issue, type) group 中最后一个 isPrimary 的索引
        let mut last_primary_index: std::collections::HashMap<(Uuid, String), usize> =
            std::collections::HashMap::new();
        for (i, row) in rows.iter().enumerate() {
            if row.is_primary {
                last_primary_index.insert((row.issue_id, row.kind.clone()), i);
            }
        }
        let mut tx = self.pool().begin().await?;
        // chunked INSERT：每行独立 execute，rows 通过 18 个 typed bind 传入。
        const CHUNK_ROWS: usize = 500;
        for chunk in rows.chunks(CHUNK_ROWS) {
            // 构造 VALUES 占位符：18 列，$1..$18 per row
            let mut sql = String::from(
                "INSERT INTO issue_work_products (\
                 company_id, project_id, issue_id, execution_workspace_id, runtime_service_id, \
                 type, provider, external_id, title, url, status, review_state, is_primary, \
                 health_status, summary, metadata, created_by_run_id, source_trust) VALUES ",
            );
            let mut counter = 0usize;
            for (i, row) in chunk.iter().enumerate() {
                counter = i;
                let is_primary = row.is_primary
                    && last_primary_index
                        .get(&(row.issue_id, row.kind.clone()))
                        .copied()
                        == Some(i);
                let offset = i * 18;
                let placeholders: Vec<String> = (0..18).map(|j| format!("${}", offset + j + 1)).collect();
                if i > 0 {
                    sql.push_str(", ");
                }
                sql.push('(');
                sql.push_str(&placeholders.join(", "));
                sql.push(')');
            }
            // 用 sqlx::query + 18 binds × chunk 行 一次性传入。
            let mut q = sqlx::query(&sql);
            let mut bind_counter = 0usize;
            for row in chunk.iter() {
                let idx = bind_counter;
                bind_counter += 1;
                let is_primary = row.is_primary
                    && last_primary_index
                        .get(&(row.issue_id, row.kind.clone()))
                        .copied()
                        == Some(idx);
                q = q.bind(row.company_id);
                q = q.bind(row.project_id);
                q = q.bind(row.issue_id);
                q = q.bind(row.execution_workspace_id);
                q = q.bind(row.runtime_service_id);
                q = q.bind(&row.kind);
                q = q.bind(&row.provider);
                q = q.bind(row.external_id.as_deref());
                q = q.bind(&row.title);
                q = q.bind(row.url.as_deref());
                q = q.bind(&row.status);
                q = q.bind(&row.review_state);
                q = q.bind(is_primary);
                q = q.bind(&row.health_status);
                q = q.bind(row.summary.as_deref());
                q = q.bind(row.metadata.as_ref().map(sqlx::types::Json));
                q = q.bind(row.created_by_run_id);
                q = q.bind(row.source_trust.as_ref().map(sqlx::types::Json));
            }
            q.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn remove(&self, id: Uuid) -> Result<Option<WorkProduct>, WorkProductError> {
        let row: Option<WorkProductRow> = sqlx::query_as::<_, WorkProductRow>(
            "DELETE FROM issue_work_products WHERE id = $1 RETURNING \
             id, company_id, project_id, issue_id, execution_workspace_id, runtime_service_id, \
             type, provider, external_id, title, url, status, review_state, is_primary, \
             health_status, summary, metadata, created_by_run_id, created_at, updated_at, \
             source_trust",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(row_to_work_product))
    }

    async fn create_for_issue_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        issue_id: Uuid,
        company_id: Uuid,
        data: CreateWorkProductInput,
    ) -> Result<Option<WorkProduct>, WorkProductError> {
        if data.is_primary {
            sqlx::query(
                "UPDATE issue_work_products SET is_primary = false, updated_at = now() \
                 WHERE company_id = $1 AND issue_id = $2 AND type = $3",
            )
            .bind(company_id)
            .bind(issue_id)
            .bind(&data.kind)
            .execute(&mut **tx)
            .await?;
        }
        let row: Option<WorkProductRow> = sqlx::query_as::<_, WorkProductRow>(
            "INSERT INTO issue_work_products \
             (company_id, project_id, issue_id, execution_workspace_id, runtime_service_id, \
              type, provider, external_id, title, url, status, review_state, is_primary, \
              health_status, summary, metadata, created_by_run_id, source_trust) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
                     COALESCE($12, 'none'), $13, COALESCE($14, 'unknown'), $15, $16, $17, $18) \
             RETURNING id, company_id, project_id, issue_id, execution_workspace_id, \
                       runtime_service_id, type, provider, external_id, title, url, status, \
                       review_state, is_primary, health_status, summary, metadata, \
                       created_by_run_id, created_at, updated_at, source_trust",
        )
        .bind(company_id)
        .bind(data.project_id)
        .bind(issue_id)
        .bind(data.execution_workspace_id)
        .bind(data.runtime_service_id)
        .bind(&data.kind)
        .bind(&data.provider)
        .bind(data.external_id.as_deref())
        .bind(&data.title)
        .bind(data.url.as_deref())
        .bind(&data.status)
        .bind(data.review_state.as_deref())
        .bind(data.is_primary)
        .bind(data.health_status.as_deref())
        .bind(data.summary.as_deref())
        .bind(data.metadata.as_ref())
        .bind(data.created_by_run_id)
        .bind(data.source_trust.as_ref())
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row.map(row_to_work_product))
    }

    async fn update_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
        patch: UpdateWorkProductPatch,
    ) -> Result<Option<WorkProduct>, WorkProductError> {
        let existing: Option<WorkProductRow> = sqlx::query_as::<_, WorkProductRow>(
            "SELECT id, company_id, project_id, issue_id, execution_workspace_id, runtime_service_id, \
                    type, provider, external_id, title, url, status, review_state, is_primary, \
                    health_status, summary, metadata, created_by_run_id, created_at, updated_at, \
                    source_trust \
             FROM issue_work_products WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
        let Some(existing) = existing else {
            return Ok(None);
        };
        if patch.is_primary == Some(true) {
            sqlx::query(
                "UPDATE issue_work_products SET is_primary = false, updated_at = now() \
                 WHERE company_id = $1 AND issue_id = $2 AND type = $3 AND id <> $4",
            )
            .bind(existing.company_id)
            .bind(existing.issue_id)
            .bind(&existing.kind)
            .bind(id)
            .execute(&mut **tx)
            .await?;
        }
        // 用单一 SQL：总是 bind 14 个可选字段（None 表示不改）。
        // id=$1, provider=$2, external_id=$3, title=$4, url=$5, status=$6, review_state=$7,
        // is_primary=$8, health_status=$9, summary=$10, metadata=$11, source_trust=$12,
        // execution_workspace_id=$13, runtime_service_id=$14, created_by_run_id=$15.
        // SET 子句用 COALESCE($N, col) 跳过 NULL。
        let row: Option<WorkProductRow> = sqlx::query_as::<_, WorkProductRow>(
            "UPDATE issue_work_products SET \
                provider = COALESCE($2, provider), \
                external_id = COALESCE($3, external_id), \
                title = COALESCE($4, title), \
                url = COALESCE($5, url), \
                status = COALESCE($6, status), \
                review_state = COALESCE($7, review_state), \
                is_primary = COALESCE($8, is_primary), \
                health_status = COALESCE($9, health_status), \
                summary = COALESCE($10, summary), \
                metadata = COALESCE($11::jsonb, metadata), \
                source_trust = COALESCE($12::jsonb, source_trust), \
                execution_workspace_id = COALESCE($13, execution_workspace_id), \
                runtime_service_id = COALESCE($14, runtime_service_id), \
                created_by_run_id = COALESCE($15, created_by_run_id), \
                updated_at = now() \
             WHERE id = $1 \
             RETURNING id, company_id, project_id, issue_id, execution_workspace_id, runtime_service_id, \
                       type, provider, external_id, title, url, status, review_state, is_primary, \
                       health_status, summary, metadata::text::jsonb AS metadata, created_by_run_id, \
                       created_at, updated_at, source_trust::text::jsonb AS source_trust",
        )
        .bind(id)
        .bind(patch.provider)
        .bind(patch.external_id)
        .bind(patch.title)
        .bind(patch.url)
        .bind(patch.status)
        .bind(patch.review_state)
        .bind(patch.is_primary)
        .bind(patch.health_status)
        .bind(patch.summary)
        .bind(patch.metadata.as_ref())
        .bind(patch.source_trust.as_ref())
        .bind(patch.execution_workspace_id)
        .bind(patch.runtime_service_id)
        .bind(patch.created_by_run_id)
        .fetch_optional(&mut **tx)
        .await?;
        Ok(row.map(row_to_work_product))
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct WorkProductRow {
    id: Uuid,
    company_id: Uuid,
    project_id: Option<Uuid>,
    issue_id: Uuid,
    execution_workspace_id: Option<Uuid>,
    runtime_service_id: Option<Uuid>,
    #[sqlx(rename = "type")]
    kind: String,
    provider: String,
    external_id: Option<String>,
    title: String,
    url: Option<String>,
    status: String,
    review_state: String,
    is_primary: bool,
    health_status: String,
    summary: Option<String>,
    metadata: Option<sqlx::types::Json<Value>>,
    created_by_run_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    source_trust: Option<sqlx::types::Json<Value>>,
}

fn row_to_work_product(row: WorkProductRow) -> WorkProduct {
    WorkProduct {
        id: row.id,
        company_id: row.company_id,
        project_id: row.project_id,
        issue_id: row.issue_id,
        execution_workspace_id: row.execution_workspace_id,
        runtime_service_id: row.runtime_service_id,
        kind: row.kind,
        provider: row.provider,
        external_id: row.external_id,
        title: row.title,
        url: row.url,
        status: row.status,
        review_state: row.review_state,
        is_primary: row.is_primary,
        health_status: row.health_status,
        summary: row.summary,
        metadata: row.metadata.map(|j| j.0),
        source_trust: row.source_trust.map(|j| j.0),
        created_by_run_id: row.created_by_run_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

/// 把 `ImportIssueWorkProductRow` 转 `CreateWorkProductInput`。
pub fn import_row_to_create_input(
    row: &ImportIssueWorkProductRow,
) -> CreateWorkProductInput {
    CreateWorkProductInput {
        project_id: row.project_id,
        execution_workspace_id: row.execution_workspace_id,
        runtime_service_id: row.runtime_service_id,
        kind: row.kind.clone(),
        provider: row.provider.clone(),
        external_id: row.external_id.clone(),
        title: row.title.clone(),
        url: row.url.clone(),
        status: row.status.clone(),
        review_state: Some(row.review_state.clone()),
        is_primary: row.is_primary,
        health_status: Some(row.health_status.clone()),
        summary: row.summary.clone(),
        metadata: row.metadata.clone(),
        source_trust: row.source_trust.clone(),
        created_by_run_id: row.created_by_run_id,
    }
}
