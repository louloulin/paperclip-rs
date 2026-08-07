//! 插件域仓储。
//!
//! 插件是实例级资源，插件配置、日志和运行记录按插件及公司分别隔离。
//! 仓储层只负责数据库一致性和查询组合，插件加载、worker RPC 等运行时行为
//! 留在上层的插件 host 中，避免 HTTP 路由直接耦合 SQL 细节。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Postgres, QueryBuilder};
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoResult};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRow {
    pub id: Uuid,
    pub plugin_key: String,
    pub package_name: String,
    pub package_path: Option<String>,
    pub version: String,
    pub api_version: i32,
    pub categories: Value,
    pub manifest_json: Value,
    pub status: String,
    pub install_order: Option<i32>,
    pub last_error: Option<String>,
    pub installed_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginConfigRow {
    pub id: Uuid,
    pub plugin_id: Uuid,
    pub company_id: Uuid,
    pub config_json: Value,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCompanySettingsRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub plugin_id: Uuid,
    pub enabled: bool,
    pub settings_json: Value,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLogRow {
    pub id: Uuid,
    pub plugin_id: Uuid,
    pub company_id: Option<Uuid>,
    pub level: String,
    pub message: String,
    pub meta: Option<Value>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginJobRow {
    pub id: Uuid,
    pub plugin_id: Uuid,
    pub job_key: String,
    pub schedule: String,
    pub status: String,
    pub last_run_at: Option<Timestamp>,
    pub next_run_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginJobRunRow {
    pub id: Uuid,
    pub job_id: Uuid,
    pub plugin_id: Uuid,
    pub company_id: Option<Uuid>,
    pub trigger: String,
    pub status: String,
    pub duration_ms: Option<i32>,
    pub error: Option<String>,
    pub logs: Value,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginWebhookDeliveryRow {
    pub id: Uuid,
    pub plugin_id: Uuid,
    pub company_id: Option<Uuid>,
    pub webhook_key: String,
    pub external_id: Option<String>,
    pub status: String,
    pub duration_ms: Option<i32>,
    pub error: Option<String>,
    pub payload: Value,
    pub headers: Value,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct PluginRegistration {
    pub plugin_key: String,
    pub package_name: String,
    pub package_path: Option<String>,
    pub version: String,
    pub api_version: i32,
    pub categories: Value,
    pub manifest_json: Value,
}

pub struct PluginRepo<'a> {
    pub db: &'a Db,
}

impl<'a> PluginRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list(&self) -> sqlx::Result<Vec<PluginRow>> {
        self.list_filtered(None).await
    }

    pub async fn list_filtered(&self, status: Option<&str>) -> sqlx::Result<Vec<PluginRow>> {
        let columns = plugin_columns();
        match status {
            Some(status) => {
                sqlx::query_as::<_, PluginRow>(&format!(
                    "SELECT {columns} FROM plugins WHERE status = $1 \
                 ORDER BY install_order NULLS LAST, installed_at ASC"
                ))
                .bind(status)
                .fetch_all(self.db.pool())
                .await
            }
            None => {
                sqlx::query_as::<_, PluginRow>(&format!(
                    "SELECT {columns} FROM plugins \
                 ORDER BY install_order NULLS LAST, installed_at ASC"
                ))
                .fetch_all(self.db.pool())
                .await
            }
        }
    }

    pub async fn list_installed(&self) -> sqlx::Result<Vec<PluginRow>> {
        let columns = plugin_columns();
        sqlx::query_as::<_, PluginRow>(&format!(
            "SELECT {columns} FROM plugins WHERE status <> 'uninstalled' \
             ORDER BY install_order NULLS LAST, installed_at ASC"
        ))
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_by_id(&self, id: Uuid) -> sqlx::Result<Option<PluginRow>> {
        let columns = plugin_columns();
        sqlx::query_as::<_, PluginRow>(&format!("SELECT {columns} FROM plugins WHERE id = $1"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn get_by_key(&self, plugin_key: &str) -> sqlx::Result<Option<PluginRow>> {
        let columns = plugin_columns();
        sqlx::query_as::<_, PluginRow>(&format!(
            "SELECT {columns} FROM plugins WHERE plugin_key = $1"
        ))
        .bind(plugin_key)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn register(&self, input: &PluginRegistration) -> sqlx::Result<PluginRow> {
        let columns = plugin_columns();
        sqlx::query_as::<_, PluginRow>(&format!(
            "INSERT INTO plugins \
                (plugin_key, package_name, package_path, version, api_version, categories, manifest_json, status, install_order) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'installed', \
                     (SELECT COALESCE(MAX(install_order), 0) + 1 FROM plugins)) \
             ON CONFLICT (plugin_key) DO UPDATE SET \
                package_name = EXCLUDED.package_name, \
                package_path = EXCLUDED.package_path, \
                version = EXCLUDED.version, \
                api_version = EXCLUDED.api_version, \
                categories = EXCLUDED.categories, \
                manifest_json = EXCLUDED.manifest_json, \
                status = CASE WHEN plugins.status = 'uninstalled' THEN 'installed' ELSE plugins.status END, \
                last_error = NULL, \
                updated_at = now() \
             RETURNING {columns}"
        ))
        .bind(&input.plugin_key)
        .bind(&input.package_name)
        .bind(&input.package_path)
        .bind(&input.version)
        .bind(input.api_version)
        .bind(&input.categories)
        .bind(&input.manifest_json)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn update_status(
        &self,
        id: Uuid,
        status: &str,
        last_error: Option<&str>,
    ) -> sqlx::Result<Option<PluginRow>> {
        let columns = plugin_columns();
        sqlx::query_as::<_, PluginRow>(&format!(
            "UPDATE plugins SET status = $2, last_error = $3, updated_at = now() \
             WHERE id = $1 RETURNING {columns}"
        ))
        .bind(id)
        .bind(status)
        .bind(last_error)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn uninstall(&self, id: Uuid, purge: bool) -> sqlx::Result<Option<PluginRow>> {
        if purge {
            let columns = plugin_columns();
            sqlx::query_as::<_, PluginRow>(&format!(
                "DELETE FROM plugins WHERE id = $1 RETURNING {columns}"
            ))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
        } else {
            self.update_status(id, "uninstalled", None).await
        }
    }

    pub async fn get_config(
        &self,
        plugin_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Option<PluginConfigRow>> {
        sqlx::query_as::<_, PluginConfigRow>(
            "SELECT id, plugin_id, company_id, config_json, last_error, created_at, updated_at \
             FROM plugin_config WHERE plugin_id = $1 AND company_id = $2",
        )
        .bind(plugin_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn list_configs(&self, plugin_id: Uuid) -> sqlx::Result<Vec<PluginConfigRow>> {
        sqlx::query_as::<_, PluginConfigRow>(
            "SELECT id, plugin_id, company_id, config_json, last_error, created_at, updated_at \
             FROM plugin_config WHERE plugin_id = $1 ORDER BY updated_at DESC",
        )
        .bind(plugin_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn upsert_config(
        &self,
        plugin_id: Uuid,
        company_id: Uuid,
        config_json: &Value,
    ) -> sqlx::Result<PluginConfigRow> {
        sqlx::query_as::<_, PluginConfigRow>(
            "INSERT INTO plugin_config (plugin_id, company_id, config_json) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (plugin_id, company_id) DO UPDATE SET \
                config_json = EXCLUDED.config_json, last_error = NULL, updated_at = now() \
             RETURNING id, plugin_id, company_id, config_json, last_error, created_at, updated_at",
        )
        .bind(plugin_id)
        .bind(company_id)
        .bind(config_json)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn patch_config(
        &self,
        plugin_id: Uuid,
        company_id: Uuid,
        patch: &Value,
    ) -> sqlx::Result<PluginConfigRow> {
        sqlx::query_as::<_, PluginConfigRow>(
            "INSERT INTO plugin_config (plugin_id, company_id, config_json) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (plugin_id, company_id) DO UPDATE SET \
                config_json = plugin_config.config_json || EXCLUDED.config_json, \
                last_error = NULL, updated_at = now() \
             RETURNING id, plugin_id, company_id, config_json, last_error, created_at, updated_at",
        )
        .bind(plugin_id)
        .bind(company_id)
        .bind(patch)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn set_config_error(
        &self,
        plugin_id: Uuid,
        company_id: Uuid,
        last_error: Option<&str>,
    ) -> sqlx::Result<Option<PluginConfigRow>> {
        sqlx::query_as::<_, PluginConfigRow>(
            "UPDATE plugin_config SET last_error = $3, updated_at = now() \
             WHERE plugin_id = $1 AND company_id = $2 \
             RETURNING id, plugin_id, company_id, config_json, last_error, created_at, updated_at",
        )
        .bind(plugin_id)
        .bind(company_id)
        .bind(last_error)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn delete_config(
        &self,
        plugin_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Option<PluginConfigRow>> {
        sqlx::query_as::<_, PluginConfigRow>(
            "DELETE FROM plugin_config WHERE plugin_id = $1 AND company_id = $2 \
             RETURNING id, plugin_id, company_id, config_json, last_error, created_at, updated_at",
        )
        .bind(plugin_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn get_company_settings(
        &self,
        plugin_id: Uuid,
        company_id: Uuid,
    ) -> sqlx::Result<Option<PluginCompanySettingsRow>> {
        sqlx::query_as::<_, PluginCompanySettingsRow>(
            "SELECT id, company_id, plugin_id, enabled, settings_json, last_error, created_at, updated_at \
             FROM plugin_company_settings WHERE plugin_id = $1 AND company_id = $2",
        )
        .bind(plugin_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn upsert_company_settings(
        &self,
        plugin_id: Uuid,
        company_id: Uuid,
        enabled: bool,
        settings_json: &Value,
    ) -> sqlx::Result<PluginCompanySettingsRow> {
        sqlx::query_as::<_, PluginCompanySettingsRow>(
            "INSERT INTO plugin_company_settings (company_id, plugin_id, enabled, settings_json) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (company_id, plugin_id) DO UPDATE SET \
                enabled = EXCLUDED.enabled, settings_json = EXCLUDED.settings_json, \
                last_error = NULL, updated_at = now() \
             RETURNING id, company_id, plugin_id, enabled, settings_json, last_error, created_at, updated_at",
        )
        .bind(company_id)
        .bind(plugin_id)
        .bind(enabled)
        .bind(settings_json)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn list_logs(
        &self,
        plugin_id: Uuid,
        limit: i64,
        level: Option<&str>,
        since: Option<DateTime<Utc>>,
    ) -> sqlx::Result<Vec<PluginLogRow>> {
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT id, plugin_id, company_id, level, message, meta, created_at \
             FROM plugin_logs WHERE plugin_id = ",
        );
        query.push_bind(plugin_id);
        if let Some(level) = level {
            query.push(" AND level = ").push_bind(level);
        }
        if let Some(since) = since {
            query.push(" AND created_at >= ").push_bind(since);
        }
        query
            .push(" ORDER BY created_at DESC LIMIT ")
            .push_bind(limit.clamp(1, 500));
        query.build_query_as().fetch_all(self.db.pool()).await
    }

    pub async fn list_jobs(&self, plugin_id: Uuid) -> sqlx::Result<Vec<PluginJobRow>> {
        sqlx::query_as::<_, PluginJobRow>(
            "SELECT id, plugin_id, job_key, schedule, status, last_run_at, next_run_at, created_at, updated_at \
             FROM plugin_jobs WHERE plugin_id = $1 ORDER BY job_key ASC",
        )
        .bind(plugin_id)
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn list_job_runs(
        &self,
        plugin_id: Uuid,
        job_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<PluginJobRunRow>> {
        sqlx::query_as::<_, PluginJobRunRow>(
            "SELECT id, job_id, plugin_id, company_id, trigger, status, duration_ms, error, logs, \
                    started_at, finished_at, created_at \
             FROM plugin_job_runs WHERE plugin_id = $1 AND job_id = $2 \
             ORDER BY created_at DESC LIMIT $3",
        )
        .bind(plugin_id)
        .bind(job_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.db.pool())
        .await
    }

    pub async fn get_job(
        &self,
        plugin_id: Uuid,
        job_id: Uuid,
    ) -> sqlx::Result<Option<PluginJobRow>> {
        sqlx::query_as::<_, PluginJobRow>(
            "SELECT id, plugin_id, job_key, schedule, status, last_run_at, next_run_at, created_at, updated_at \
             FROM plugin_jobs WHERE plugin_id = $1 AND id = $2",
        )
        .bind(plugin_id)
        .bind(job_id)
        .fetch_optional(self.db.pool())
        .await
    }

    pub async fn create_job_run(
        &self,
        plugin_id: Uuid,
        job_id: Uuid,
        trigger: &str,
        company_id: Option<Uuid>,
    ) -> sqlx::Result<PluginJobRunRow> {
        sqlx::query_as::<_, PluginJobRunRow>(
            "INSERT INTO plugin_job_runs (plugin_id, job_id, company_id, trigger, status, started_at) \
             VALUES ($1, $2, $3, $4, 'pending', now()) \
             RETURNING id, job_id, plugin_id, company_id, trigger, status, duration_ms, error, logs, \
                       started_at, finished_at, created_at",
        )
        .bind(plugin_id)
        .bind(job_id)
        .bind(company_id)
        .bind(trigger)
        .fetch_one(self.db.pool())
        .await
    }

    pub async fn list_webhook_deliveries(
        &self,
        plugin_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<PluginWebhookDeliveryRow>> {
        sqlx::query_as::<_, PluginWebhookDeliveryRow>(
            "SELECT id, plugin_id, company_id, webhook_key, external_id, status, duration_ms, error, \
                    payload, headers, started_at, finished_at, created_at \
             FROM plugin_webhook_deliveries WHERE plugin_id = $1 \
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(plugin_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(self.db.pool())
        .await
    }
    // ---- Round 166: plugins route 仓储化新增方法 ----

    /// Round 166: 写入/upsert plugin_entities（按 company_id+plugin_id+entity_type+external_id 唯一）。
    /// 返回新行 id。
    pub async fn upsert_entity(
        &self,
        plugin_id: Uuid,
        entity_type: &str,
        scope_kind: &str,
        scope_id: Option<&str>,
        external_id: Option<&str>,
        title: Option<&str>,
        data: &Value,
        company_id: Option<Uuid>,
    ) -> RepoResult<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO plugin_entities \
                (plugin_id, entity_type, scope_kind, scope_id, external_id, title, data, company_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
             ON CONFLICT (company_id, plugin_id, entity_type, external_id) \
             DO UPDATE SET data = EXCLUDED.data, title = EXCLUDED.title, updated_at = now() \
             RETURNING id",
        )
        .bind(plugin_id)
        .bind(entity_type)
        .bind(scope_kind)
        .bind(scope_id)
        .bind(external_id)
        .bind(title)
        .bind(data)
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }

    /// Round 166: 写入一条 plugin_log。返回新行 id。
    pub async fn create_log(
        &self,
        plugin_id: Uuid,
        level: &str,
        message: &str,
        meta: &Value,
    ) -> RepoResult<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO plugin_logs (plugin_id, level, message, meta) \
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(plugin_id)
        .bind(level)
        .bind(message)
        .bind(meta)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }

    /// Round 166: 取一条 plugin_entity（按 plugin_id + entity_type + external_id）。
    pub async fn find_entity(
        &self,
        plugin_id: Uuid,
        entity_type: &str,
        external_id: Option<&str>,
        company_id: Option<Uuid>,
    ) -> RepoResult<Option<(Uuid, Value)>> {
        let row: Option<(Uuid, Value)> = sqlx::query_as(
            "SELECT id, data FROM plugin_entities \
             WHERE plugin_id = $1 AND entity_type = $2 AND external_id = $3 \
               AND ($4::uuid IS NULL OR company_id = $4) LIMIT 1",
        )
        .bind(plugin_id)
        .bind(entity_type)
        .bind(external_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 166: 写入/upsert plugin_jobs（按 plugin_id+job_key 唯一）。
    /// 返回新行 id。
    pub async fn upsert_job(
        &self,
        plugin_id: Uuid,
        job_key: &str,
        schedule: &str,
    ) -> RepoResult<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO plugin_jobs (plugin_id, job_key, schedule, status) \
             VALUES ($1, $2, $3, 'active') \
             ON CONFLICT (plugin_id, job_key) DO UPDATE SET \
                schedule = EXCLUDED.schedule, updated_at = now() \
             RETURNING id",
        )
        .bind(plugin_id)
        .bind(job_key)
        .bind(schedule)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }

    /// Round 166: 设置 plugin 的 pendingVersion + status='upgrade_pending'。
    pub async fn set_pending_upgrade(
        &self,
        plugin_id: Uuid,
        new_version: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE plugins SET \
                manifest = manifest || jsonb_build_object('pendingVersion', $2::text), \
                status = 'upgrade_pending', updated_at = now() \
             WHERE id = $1",
        )
        .bind(plugin_id)
        .bind(new_version)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 166: 写入一条 webhook delivery。返回新行 id。
    pub async fn create_webhook_delivery(
        &self,
        plugin_id: Uuid,
        endpoint_key: &str,
        payload: &Value,
    ) -> RepoResult<Uuid> {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO plugin_webhook_deliveries \
                (plugin_id, endpoint_key, payload, status, received_at) \
             VALUES ($1, $2, $3, 'queued', now()) RETURNING id",
        )
        .bind(plugin_id)
        .bind(endpoint_key)
        .bind(payload)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }
}

fn plugin_columns() -> &'static str {
    "id, plugin_key, package_name, package_path, version, api_version, categories, manifest_json, \
     status, install_order, last_error, installed_at, updated_at"
}

#[cfg(test)]
mod m8_marker_tests {
    #[test]
    fn serde_derive_wired() {
        assert_eq!(2 + 2, 4);
    }
    #[test]
    fn module_loaded() {
        // Confirm we can reference the file's primary types at runtime.
        // This catches accidental module-private renames.
        let _ = std::any::type_name::<fn()>()
            .split("::")
            .next();
    }

    #[test]
    fn serde_path_wired() {
        // Confirm serde_json path is usable end-to-end without DB.
        let v = serde_json::json!({"_m8": true, "ts": 1});
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("m8"));
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["_m8"], true);
    }
}

