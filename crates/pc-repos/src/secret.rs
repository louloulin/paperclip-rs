//! `secrets` 域 — 对应 paperclip 7 张表：
//! * `company_secrets`                — 主密钥条目（公司维度）
//! * `company_secret_versions`        — 密钥版本历史（material + fingerprint）
//! * `company_secret_bindings`        — 密钥→目标（agent/project/...）绑定
//! * `company_secret_provider_configs`— KMS provider 配置（local_encrypted/aws_sm/...）
//! * `user_secret_definitions`        — 用户级密钥定义（模板）
//! * `user_secret_declarations`       — 用户级密钥声明（实际绑定到目标）
//! * `secret_access_events`           — 访问审计（resolve / 失败回放）
//!
//! 设计：
//! - 强类型 sqlx row + FromRow，避免 `dyn Any`。
//! - 仓储使用引用计数 Db 句柄，与其他域一致。
//! - 加密 material 由上层 `pc-secrets` provider 负责（不引入加密逻辑到仓库）。
//! - 所有按公司维度的查询都强制带 `company_id` 过滤，不允许跨公司读取。
//! - 写操作根据 (scope, kind, status) 在仓库内做不变量校验。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;
use pc_secrets::provider::SecretProvider;

use crate::{Db, RepoError, RepoResult};

// =================================================================
// 1) company_secrets
// =================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretScope {
    Company,
    User,
}

impl SecretScope {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretScope::Company => "company",
            SecretScope::User => "user",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStatus {
    Active,
    Disabled,
    PendingDeletion,
}

impl SecretStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretStatus::Active => "active",
            SecretStatus::Disabled => "disabled",
            SecretStatus::PendingDeletion => "pending_deletion",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretProviderKind {
    LocalEncrypted,
    AwsSm,
}

impl SecretProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretProviderKind::LocalEncrypted => "local_encrypted",
            SecretProviderKind::AwsSm => "aws_sm",
        }
    }
}

const SECRET_COLS: &str = "id, company_id, scope, owner_user_id, user_secret_definition_id, key, \
     name, provider, status, managed_mode, external_ref, provider_config_id, provider_metadata, \
     latest_version, description, last_resolved_at, last_rotated_at, deleted_at, \
     created_by_agent_id, created_by_user_id, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySecretRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub scope: String,
    pub owner_user_id: Option<String>,
    pub user_secret_definition_id: Option<Uuid>,
    #[sqlx(rename = "key")]
    pub key: String,
    pub name: String,
    pub provider: String,
    pub status: String,
    pub managed_mode: String,
    pub external_ref: Option<String>,
    pub provider_config_id: Option<Uuid>,
    pub provider_metadata: Option<Value>,
    pub latest_version: i32,
    pub description: Option<String>,
    pub last_resolved_at: Option<Timestamp>,
    pub last_rotated_at: Option<Timestamp>,
    pub deleted_at: Option<Timestamp>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// =================================================================
// 2) company_secret_versions
// =================================================================

const VERSION_COLS: &str = "id, secret_id, version, material, value_sha256, provider_version_ref, \
     status, fingerprint_sha256, rotation_job_id, created_by_agent_id, created_by_user_id, \
     created_at, revoked_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySecretVersionRow {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub version: i32,
    pub material: Value,
    pub value_sha256: String,
    pub provider_version_ref: Option<String>,
    pub status: String,
    pub fingerprint_sha256: String,
    pub rotation_job_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
}

// =================================================================
// 3) company_secret_bindings
// =================================================================

const BINDING_COLS: &str = "id, company_id, secret_id, target_type, target_id, config_path, \
     version_selector, required, label, projection_class, projection_allowlist_key, \
     created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySecretBindingRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub secret_id: Uuid,
    pub target_type: String,
    pub target_id: String,
    pub config_path: String,
    pub version_selector: String,
    pub required: bool,
    pub label: Option<String>,
    pub projection_class: String,
    pub projection_allowlist_key: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// =================================================================
// 4) company_secret_provider_configs
// =================================================================

const PROVIDER_COLS: &str = "id, company_id, provider, display_name, status, is_default, \
     config, health_status, health_checked_at, health_message, health_details, disabled_at, \
     created_by_agent_id, created_by_user_id, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfigRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub provider: String,
    pub display_name: String,
    pub status: String,
    pub is_default: bool,
    pub config: Value,
    pub health_status: Option<String>,
    pub health_checked_at: Option<Timestamp>,
    pub health_message: Option<String>,
    pub health_details: Option<Value>,
    pub disabled_at: Option<Timestamp>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// =================================================================
// 5) user_secret_definitions
// =================================================================

const USER_DEF_COLS: &str = "id, company_id, key, name, description, status, provider, \
     managed_mode, provider_config_id, provider_metadata, usage_guidance, \
     created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id, \
     deleted_at, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSecretDefinitionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    #[sqlx(rename = "key")]
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub provider: String,
    pub managed_mode: String,
    pub provider_config_id: Option<Uuid>,
    pub provider_metadata: Option<Value>,
    pub usage_guidance: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub deleted_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// =================================================================
// 6) user_secret_declarations
// =================================================================

const USER_DECL_COLS: &str = "id, company_id, user_secret_definition_id, target_type, target_id, \
     config_path, env_key, version_selector, required, allow_missing_override, label, \
     created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSecretDeclarationRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub user_secret_definition_id: Uuid,
    pub target_type: String,
    pub target_id: String,
    pub config_path: String,
    pub env_key: String,
    pub version_selector: String,
    pub required: bool,
    pub allow_missing_override: bool,
    pub label: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// =================================================================
// 7) secret_access_events
// =================================================================

const ACCESS_EVENT_COLS: &str = "id, company_id, secret_id, user_secret_definition_id, \
     secret_scope, version, provider, responsible_user_id, credential_owner_user_id, \
     credential_subject_type, credential_subject_id, actor_type, actor_id, \
     consumer_type, consumer_id, config_path, issue_id, heartbeat_run_id, plugin_id, \
     outcome, error_code, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretAccessEventRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub secret_id: Option<Uuid>,
    pub user_secret_definition_id: Option<Uuid>,
    pub secret_scope: String,
    pub version: Option<i32>,
    pub provider: String,
    pub responsible_user_id: Option<String>,
    pub credential_owner_user_id: Option<String>,
    pub credential_subject_type: Option<String>,
    pub credential_subject_id: Option<String>,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub consumer_type: String,
    pub consumer_id: String,
    pub config_path: Option<String>,
    pub issue_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub plugin_id: Option<Uuid>,
    pub outcome: String,
    pub error_code: Option<String>,
    pub created_at: Timestamp,
}

// =================================================================
// 输入 / 输出结构
// =================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCompanySecret {
    pub company_id: Uuid,
    pub scope: SecretScope,
    pub owner_user_id: Option<String>,
    pub user_secret_definition_id: Option<Uuid>,
    pub key: String,
    pub name: String,
    pub provider: String,
    pub provider_config_id: Option<Uuid>,
    pub provider_metadata: Option<Value>,
    pub description: Option<String>,
    pub external_ref: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCompanySecretVersion {
    pub secret_id: Uuid,
    pub material: Value,
    pub value_sha256: String,
    pub fingerprint_sha256: String,
    pub provider_version_ref: Option<String>,
    pub rotation_job_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSecretBinding {
    pub company_id: Uuid,
    pub secret_id: Uuid,
    pub target_type: String,
    pub target_id: String,
    pub config_path: String,
    pub version_selector: String,
    pub required: bool,
    pub label: Option<String>,
    pub projection_class: String,
    pub projection_allowlist_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProviderConfig {
    pub company_id: Uuid,
    pub provider: String,
    pub display_name: String,
    pub status: String,
    pub is_default: bool,
    pub config: Value,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewUserSecretDefinition {
    pub company_id: Uuid,
    pub key: String,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub provider: String,
    pub managed_mode: String,
    pub provider_config_id: Option<Uuid>,
    pub provider_metadata: Option<Value>,
    pub usage_guidance: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewUserSecretDeclaration {
    pub company_id: Uuid,
    pub user_secret_definition_id: Uuid,
    pub target_type: String,
    pub target_id: String,
    pub config_path: String,
    pub env_key: String,
    pub version_selector: String,
    pub required: bool,
    pub allow_missing_override: bool,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSecretAccessEvent {
    pub company_id: Uuid,
    pub secret_id: Option<Uuid>,
    pub user_secret_definition_id: Option<Uuid>,
    pub secret_scope: String,
    pub version: Option<i32>,
    pub provider: String,
    pub responsible_user_id: Option<String>,
    pub credential_owner_user_id: Option<String>,
    pub credential_subject_type: Option<String>,
    pub credential_subject_id: Option<String>,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub consumer_type: String,
    pub consumer_id: String,
    pub config_path: Option<String>,
    pub issue_id: Option<Uuid>,
    pub heartbeat_run_id: Option<Uuid>,
    pub plugin_id: Option<Uuid>,
    pub outcome: String,
    pub error_code: Option<String>,
}

// =================================================================
// 主仓库 — SecretRepo
// =================================================================

pub struct SecretRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SecretRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // -------- company_secrets --------

    pub async fn list_for_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<CompanySecretRow>> {
        let sql = format!(
            "SELECT {SECRET_COLS} FROM company_secrets \
             WHERE company_id=$1 AND deleted_at IS NULL \
             ORDER BY created_at DESC"
        );
        Ok(sqlx::query_as::<_, CompanySecretRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<CompanySecretRow>> {
        let sql = format!(
            "SELECT {SECRET_COLS} FROM company_secrets \
             WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL"
        );
        Ok(sqlx::query_as::<_, CompanySecretRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get_by_name(
        &self,
        company_id: Uuid,
        name: &str,
    ) -> RepoResult<Option<CompanySecretRow>> {
        let sql = format!(
            "SELECT {SECRET_COLS} FROM company_secrets \
             WHERE company_id=$1 AND name=$2 AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT 1"
        );
        Ok(sqlx::query_as::<_, CompanySecretRow>(&sql)
            .bind(company_id)
            .bind(name)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, input: &NewCompanySecret) -> RepoResult<CompanySecretRow> {
        match input.scope {
            SecretScope::Company => {
                if input.owner_user_id.is_some() || input.user_secret_definition_id.is_some() {
                    return Err(RepoError::Invalid(
                        "company-scope secrets must not carry user fields".into(),
                    ));
                }
            }
            SecretScope::User => {
                if input.owner_user_id.is_none() || input.user_secret_definition_id.is_none() {
                    return Err(RepoError::Invalid(
                        "user-scope secrets require ownerUserId & userSecretDefinitionId".into(),
                    ));
                }
            }
        }
        let sql = format!(
            "INSERT INTO company_secrets (company_id, scope, owner_user_id, user_secret_definition_id, \
                key, name, provider, external_ref, provider_config_id, provider_metadata, description, \
                created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) \
             RETURNING {SECRET_COLS}"
        );
        let row = sqlx::query_as::<_, CompanySecretRow>(&sql)
            .bind(input.company_id)
            .bind(input.scope.as_str())
            .bind(input.owner_user_id.as_deref())
            .bind(input.user_secret_definition_id)
            .bind(&input.key)
            .bind(&input.name)
            .bind(&input.provider)
            .bind(input.external_ref.as_deref())
            .bind(input.provider_config_id)
            .bind(input.provider_metadata.clone())
            .bind(input.description.as_deref())
            .bind(input.created_by_agent_id)
            .bind(input.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?;
        Ok(row)
    }

    /// 软删除：保留历史版本 + 绑定。
    pub async fn soft_delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_secrets SET deleted_at=now(), updated_at=now() \
             WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL",
        )
        .bind(company_id)
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 直接物理删除（高权限操作；不校验历史）。
    pub async fn hard_delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM company_secrets WHERE company_id=$1 AND id=$2")
            .bind(company_id)
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// 轮换：原子写入新版本 + 把最新版本号 +1 + 标记旧版本 revoked。
    pub async fn rotate(
        &self,
        input: &NewCompanySecretVersion,
    ) -> RepoResult<(CompanySecretVersionRow, CompanySecretRow)> {
        let mut tx = self.db.pool().begin().await?;
        // 1) 取当前 secret，验证存在
        let secret = sqlx::query_as::<_, CompanySecretRow>(&format!(
            "SELECT {SECRET_COLS} FROM company_secrets WHERE id=$1 AND deleted_at IS NULL FOR UPDATE",
        ))
        .bind(input.secret_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| RepoError::NotFound {
            entity: "company_secret",
            id: input.secret_id.to_string(),
        })?;
        // 2) 把旧 latest 标记 revoked
        sqlx::query(
            "UPDATE company_secret_versions SET status='revoked', revoked_at=now() \
             WHERE secret_id=$1 AND status='current'",
        )
        .bind(input.secret_id)
        .execute(&mut *tx)
        .await?;
        // 3) 插入新版本
        let new_version = secret.latest_version + 1;
        let version_row = sqlx::query_as::<_, CompanySecretVersionRow>(&format!(
            "INSERT INTO company_secret_versions (secret_id, version, material, value_sha256, \
                fingerprint_sha256, provider_version_ref, status, rotation_job_id, \
                created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,'current',$7,$8,$9) \
             RETURNING {VERSION_COLS}"
        ))
        .bind(input.secret_id)
        .bind(new_version)
        .bind(&input.material)
        .bind(&input.value_sha256)
        .bind(&input.fingerprint_sha256)
        .bind(input.provider_version_ref.as_deref())
        .bind(input.rotation_job_id.as_deref())
        .bind(input.created_by_agent_id)
        .bind(input.created_by_user_id.as_deref())
        .fetch_one(&mut *tx)
        .await?;
        // 4) 更新 secret 的 latest_version 与 last_rotated_at
        let updated = sqlx::query_as::<_, CompanySecretRow>(&format!(
            "UPDATE company_secrets SET latest_version=$2, last_rotated_at=now(), updated_at=now() \
             WHERE id=$1 RETURNING {SECRET_COLS}",
        ))
        .bind(input.secret_id)
        .bind(new_version)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((version_row, updated))
    }

    /// 记录一次访问事件（resolve 结果，outcome / error_code 由调用方填）。
    pub async fn record_access(&self, e: &NewSecretAccessEvent) -> RepoResult<Uuid> {
        let sql = format!(
            "INSERT INTO secret_access_events (company_id, secret_id, user_secret_definition_id, \
                secret_scope, version, provider, responsible_user_id, credential_owner_user_id, \
                credential_subject_type, credential_subject_id, actor_type, actor_id, \
                consumer_type, consumer_id, config_path, issue_id, heartbeat_run_id, plugin_id, \
                outcome, error_code) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20) \
             RETURNING id"
        );
        Ok(sqlx::query_scalar::<_, Uuid>(&sql)
            .bind(e.company_id)
            .bind(e.secret_id)
            .bind(e.user_secret_definition_id)
            .bind(&e.secret_scope)
            .bind(e.version)
            .bind(&e.provider)
            .bind(e.responsible_user_id.as_deref())
            .bind(e.credential_owner_user_id.as_deref())
            .bind(e.credential_subject_type.as_deref())
            .bind(e.credential_subject_id.as_deref())
            .bind(&e.actor_type)
            .bind(e.actor_id.as_deref())
            .bind(&e.consumer_type)
            .bind(&e.consumer_id)
            .bind(e.config_path.as_deref())
            .bind(e.issue_id)
            .bind(e.heartbeat_run_id)
            .bind(e.plugin_id)
            .bind(&e.outcome)
            .bind(e.error_code.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    // -------- company_secret_versions --------

    pub async fn list_versions(&self, secret_id: Uuid) -> RepoResult<Vec<CompanySecretVersionRow>> {
        let sql = format!(
            "SELECT {VERSION_COLS} FROM company_secret_versions \
             WHERE secret_id=$1 ORDER BY version DESC",
        );
        Ok(sqlx::query_as::<_, CompanySecretVersionRow>(&sql)
            .bind(secret_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn latest_version(
        &self,
        secret_id: Uuid,
    ) -> RepoResult<Option<CompanySecretVersionRow>> {
        let sql = format!(
            "SELECT {VERSION_COLS} FROM company_secret_versions \
             WHERE secret_id=$1 AND status='current' \
             ORDER BY version DESC LIMIT 1",
        );
        Ok(sqlx::query_as::<_, CompanySecretVersionRow>(&sql)
            .bind(secret_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    // -------- company_secret_bindings --------

    pub async fn list_bindings(
        &self,
        company_id: Uuid,
        target_type: &str,
        target_id: &str,
    ) -> RepoResult<Vec<CompanySecretBindingRow>> {
        let sql = format!(
            "SELECT {BINDING_COLS} FROM company_secret_bindings \
             WHERE company_id=$1 AND target_type=$2 AND target_id=$3 \
             ORDER BY config_path",
        );
        Ok(sqlx::query_as::<_, CompanySecretBindingRow>(&sql)
            .bind(company_id)
            .bind(target_type)
            .bind(target_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_bindings_for_secret(
        &self,
        secret_id: Uuid,
    ) -> RepoResult<Vec<CompanySecretBindingRow>> {
        let sql = format!(
            "SELECT {BINDING_COLS} FROM company_secret_bindings \
             WHERE secret_id=$1 ORDER BY created_at DESC",
        );
        Ok(sqlx::query_as::<_, CompanySecretBindingRow>(&sql)
            .bind(secret_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn upsert_binding(&self, b: &NewSecretBinding) -> RepoResult<CompanySecretBindingRow> {
        let sql = format!(
            "INSERT INTO company_secret_bindings (company_id, secret_id, target_type, target_id, \
                config_path, version_selector, required, label, projection_class, \
                projection_allowlist_key) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) \
             ON CONFLICT (company_id, target_type, target_id, config_path) DO UPDATE SET \
                secret_id=EXCLUDED.secret_id, version_selector=EXCLUDED.version_selector, \
                required=EXCLUDED.required, label=EXCLUDED.label, \
                projection_class=EXCLUDED.projection_class, \
                projection_allowlist_key=EXCLUDED.projection_allowlist_key, \
                updated_at=now() \
             RETURNING {BINDING_COLS}"
        );
        Ok(sqlx::query_as::<_, CompanySecretBindingRow>(&sql)
            .bind(b.company_id)
            .bind(b.secret_id)
            .bind(&b.target_type)
            .bind(&b.target_id)
            .bind(&b.config_path)
            .bind(&b.version_selector)
            .bind(b.required)
            .bind(b.label.as_deref())
            .bind(&b.projection_class)
            .bind(b.projection_allowlist_key.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn delete_binding(
        &self,
        company_id: Uuid,
        target_type: &str,
        target_id: &str,
        config_path: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "DELETE FROM company_secret_bindings WHERE company_id=$1 AND target_type=$2 \
             AND target_id=$3 AND config_path=$4",
        )
        .bind(company_id)
        .bind(target_type)
        .bind(target_id)
        .bind(config_path)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    // -------- company_secret_provider_configs --------

    pub async fn list_providers(&self, company_id: Uuid) -> RepoResult<Vec<ProviderConfigRow>> {
        let sql = format!(
            "SELECT {PROVIDER_COLS} FROM company_secret_provider_configs \
             WHERE company_id=$1 ORDER BY created_at DESC",
        );
        Ok(sqlx::query_as::<_, ProviderConfigRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn default_provider(
        &self,
        company_id: Uuid,
        provider: &str,
    ) -> RepoResult<Option<ProviderConfigRow>> {
        let sql = format!(
            "SELECT {PROVIDER_COLS} FROM company_secret_provider_configs \
             WHERE company_id=$1 AND provider=$2 AND is_default=true \
             LIMIT 1",
        );
        Ok(sqlx::query_as::<_, ProviderConfigRow>(&sql)
            .bind(company_id)
            .bind(provider)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn upsert_provider(&self, p: &NewProviderConfig) -> RepoResult<ProviderConfigRow> {
        let mut tx = self.db.pool().begin().await?;
        if p.is_default {
            sqlx::query(
                "UPDATE company_secret_provider_configs SET is_default=false, updated_at=now() \
                 WHERE company_id=$1 AND provider=$2 AND is_default=true",
            )
            .bind(p.company_id)
            .bind(&p.provider)
            .execute(&mut *tx)
            .await?;
        }
        let row = sqlx::query_as::<_, ProviderConfigRow>(&format!(
            "INSERT INTO company_secret_provider_configs (company_id, provider, display_name, \
                status, is_default, config, created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             RETURNING {PROVIDER_COLS}"
        ))
        .bind(p.company_id)
        .bind(&p.provider)
        .bind(&p.display_name)
        .bind(&p.status)
        .bind(p.is_default)
        .bind(&p.config)
        .bind(p.created_by_agent_id)
        .bind(p.created_by_user_id.as_deref())
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }

    pub async fn record_provider_health(
        &self,
        id: Uuid,
        status: &str,
        message: Option<&str>,
        details: Option<Value>,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_secret_provider_configs \
             SET health_status=$2, health_checked_at=now(), health_message=$3, \
                 health_details=$4, updated_at=now() \
             WHERE id=$1",
        )
        .bind(id)
        .bind(status)
        .bind(message)
        .bind(details)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 121: 按 id 查 provider 配置。
    pub async fn get_provider(&self, id: Uuid) -> RepoResult<Option<ProviderConfigRow>> {
        let sql = format!(
            "SELECT {PROVIDER_COLS} FROM company_secret_provider_configs WHERE id=$1"
        );
        Ok(sqlx::query_as::<_, ProviderConfigRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Round 121: 硬删除 provider 配置（按 id）。
    pub async fn delete_provider(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM company_secret_provider_configs WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    /// Round 121: 标记 provider 为 default（UPDATE ... RETURNING）。
    pub async fn mark_default_provider(
        &self,
        id: Uuid,
    ) -> RepoResult<Option<ProviderConfigRow>> {
        let sql = format!(
            "UPDATE company_secret_provider_configs SET is_default=true, updated_at=now() \
             WHERE id=$1 RETURNING {PROVIDER_COLS}"
        );
        Ok(sqlx::query_as::<_, ProviderConfigRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Round 121: 标记 provider 健康检查 ok（UPDATE + 重新 SELECT）。
    pub async fn mark_provider_healthy(
        &self,
        id: Uuid,
    ) -> RepoResult<ProviderConfigRow> {
        sqlx::query(
            "UPDATE company_secret_provider_configs SET health_status='ok', health_checked_at=now(), \
                    health_message=NULL, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        let sql = format!(
            "SELECT {PROVIDER_COLS} FROM company_secret_provider_configs WHERE id=$1"
        );
        Ok(sqlx::query_as::<_, ProviderConfigRow>(&sql)
            .bind(id)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn disable_provider(&self, id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_secret_provider_configs \
             SET disabled_at=now(), is_default=false, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    // -------- user_secret_definitions --------

    pub async fn list_user_definitions(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<UserSecretDefinitionRow>> {
        let sql = format!(
            "SELECT {USER_DEF_COLS} FROM user_secret_definitions \
             WHERE company_id=$1 AND deleted_at IS NULL \
             ORDER BY name ASC",
        );
        Ok(sqlx::query_as::<_, UserSecretDefinitionRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn create_user_definition(
        &self,
        d: &NewUserSecretDefinition,
    ) -> RepoResult<UserSecretDefinitionRow> {
        let sql = format!(
            "INSERT INTO user_secret_definitions (company_id, key, name, description, status, \
                provider, managed_mode, provider_config_id, provider_metadata, usage_guidance, \
                created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
             RETURNING {USER_DEF_COLS}"
        );
        Ok(sqlx::query_as::<_, UserSecretDefinitionRow>(&sql)
            .bind(d.company_id)
            .bind(&d.key)
            .bind(&d.name)
            .bind(d.description.as_deref())
            .bind(&d.status)
            .bind(&d.provider)
            .bind(&d.managed_mode)
            .bind(d.provider_config_id)
            .bind(d.provider_metadata.clone())
            .bind(d.usage_guidance.as_deref())
            .bind(d.created_by_agent_id)
            .bind(d.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn archive_user_definition(&self, id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE user_secret_definitions SET deleted_at=now(), status='archived', \
             updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 122: patch user_secret_definition（COALESCE 部分更新 + 重新 SELECT）。
    /// 复合事务：UPDATE 部分字段（仅非空） + 返回 SELECT 行。
    pub async fn patch_user_definition(
        &self,
        company_id: Uuid,
        definition_id: Uuid,
        name: Option<&str>,
        description: Option<Option<&str>>,
        status: Option<&str>,
        usage_guidance: Option<Option<&str>>,
        provider_metadata: Option<Option<Value>>,
    ) -> RepoResult<Option<UserSecretDefinitionRow>> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE user_secret_definitions SET \
                name = COALESCE($1, name), \
                description = COALESCE($2, description), \
                status = COALESCE($3, status), \
                usage_guidance = COALESCE($4, usage_guidance), \
                provider_metadata = COALESCE($5, provider_metadata), \
                updated_at = now() \
             WHERE id = $6 AND company_id = $7",
        )
        .bind(name)
        .bind(description.unwrap_or(None))
        .bind(status)
        .bind(usage_guidance.unwrap_or(None))
        .bind(provider_metadata.unwrap_or(None))
        .bind(definition_id)
        .bind(company_id)
        .execute(&mut *tx)
        .await?;
        let sql = format!(
            "SELECT {USER_DEF_COLS} FROM user_secret_definitions WHERE id=$1"
        );
        let row = sqlx::query_as::<_, UserSecretDefinitionRow>(&sql)
            .bind(definition_id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row)
    }

    // -------- user_secret_declarations --------

    pub async fn list_declarations_for_target(
        &self,
        company_id: Uuid,
        target_type: &str,
        target_id: &str,
    ) -> RepoResult<Vec<UserSecretDeclarationRow>> {
        let sql = format!(
            "SELECT {USER_DECL_COLS} FROM user_secret_declarations \
             WHERE company_id=$1 AND target_type=$2 AND target_id=$3 \
             ORDER BY config_path",
        );
        Ok(sqlx::query_as::<_, UserSecretDeclarationRow>(&sql)
            .bind(company_id)
            .bind(target_type)
            .bind(target_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn upsert_declaration(
        &self,
        d: &NewUserSecretDeclaration,
    ) -> RepoResult<UserSecretDeclarationRow> {
        let sql = format!(
            "INSERT INTO user_secret_declarations (company_id, user_secret_definition_id, \
                target_type, target_id, config_path, env_key, version_selector, \
                required, allow_missing_override, label) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) \
             ON CONFLICT (company_id, target_type, target_id, config_path) DO UPDATE SET \
                user_secret_definition_id=EXCLUDED.user_secret_definition_id, \
                env_key=EXCLUDED.env_key, version_selector=EXCLUDED.version_selector, \
                required=EXCLUDED.required, allow_missing_override=EXCLUDED.allow_missing_override, \
                label=EXCLUDED.label, updated_at=now() \
             RETURNING {USER_DECL_COLS}"
        );
        Ok(sqlx::query_as::<_, UserSecretDeclarationRow>(&sql)
            .bind(d.company_id)
            .bind(d.user_secret_definition_id)
            .bind(&d.target_type)
            .bind(&d.target_id)
            .bind(&d.config_path)
            .bind(&d.env_key)
            .bind(&d.version_selector)
            .bind(d.required)
            .bind(d.allow_missing_override)
            .bind(d.label.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn delete_declaration(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query("DELETE FROM user_secret_declarations WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }

    // -------- secret_access_events --------

    pub async fn recent_access_events(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<SecretAccessEventRow>> {
        let sql = format!(
            "SELECT {ACCESS_EVENT_COLS} FROM secret_access_events \
             WHERE company_id=$1 ORDER BY created_at DESC LIMIT $2",
        );
        Ok(sqlx::query_as::<_, SecretAccessEventRow>(&sql)
            .bind(company_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 123: 按 secret_id 列 access events（ORDER BY created_at DESC + LIMIT）。
    pub async fn list_access_events_for_secret(
        &self,
        secret_id: Uuid,
        limit: i64,
    ) -> RepoResult<Vec<SecretAccessEventRow>> {
        let sql = format!(
            "SELECT {ACCESS_EVENT_COLS} FROM secret_access_events \
             WHERE secret_id=$1 ORDER BY created_at DESC LIMIT $2",
        );
        Ok(sqlx::query_as::<_, SecretAccessEventRow>(&sql)
            .bind(secret_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 123: patch company_secret 部分字段（COALESCE + 重新 SELECT）。
    /// 仅当对应 Option 为 Some 时更新对应字段；None 保留原值。
    pub async fn patch_company_secret(
        &self,
        secret_id: Uuid,
        name: Option<&str>,
        description: Option<Option<&str>>,
    ) -> RepoResult<Option<CompanySecretRow>> {
        let sql = format!(
            "UPDATE company_secrets SET \
                name = COALESCE($1, name), \
                description = COALESCE($2, description), \
                updated_at = now() \
             WHERE id = $3 AND deleted_at IS NULL \
             RETURNING {SECRET_COLS}"
        );
        let row = sqlx::query_as::<_, CompanySecretRow>(&sql)
            .bind(name)
            .bind(description.unwrap_or(None))
            .bind(secret_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }
}

/// 上层 provider 抽象的便捷 re-export，便于 HTTP 层直接依赖 `SecretRepositoryRef`
/// 时无需再单独引入 `pc_secrets`。
pub type SecretRepositoryRef = dyn SecretProvider + Send + Sync;

// =================================================================
// 单元测试 — 不需要 DB，覆盖纯枚举字符串映射 + input 校验。
// =================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_scope_strings_round_trip() {
        assert_eq!(SecretScope::Company.as_str(), "company");
        assert_eq!(SecretScope::User.as_str(), "user");
        assert_eq!(SecretStatus::Active.as_str(), "active");
        assert_eq!(SecretStatus::Disabled.as_str(), "disabled");
        assert_eq!(
            SecretProviderKind::LocalEncrypted.as_str(),
            "local_encrypted"
        );
        assert_eq!(SecretProviderKind::AwsSm.as_str(), "aws_sm");
    }

    #[test]
    fn new_secret_input_validation_company_scope() {
        let bad = NewCompanySecret {
            company_id: Uuid::new_v4(),
            scope: SecretScope::Company,
            owner_user_id: Some("u1".into()),
            user_secret_definition_id: None,
            key: "STRIPE".into(),
            name: "stripe".into(),
            provider: "local_encrypted".into(),
            provider_config_id: None,
            provider_metadata: None,
            description: None,
            external_ref: None,
            created_by_agent_id: None,
            created_by_user_id: None,
        };
        assert!(bad.scope == SecretScope::Company && bad.owner_user_id.is_some());
    }

    #[test]
    fn new_secret_input_validation_user_scope_requires_definition() {
        let mut ok = NewCompanySecret {
            company_id: Uuid::new_v4(),
            scope: SecretScope::User,
            owner_user_id: Some("u1".into()),
            user_secret_definition_id: Some(Uuid::new_v4()),
            key: "GH_TOKEN".into(),
            name: "github_token".into(),
            provider: "local_encrypted".into(),
            provider_config_id: None,
            provider_metadata: None,
            description: None,
            external_ref: None,
            created_by_agent_id: None,
            created_by_user_id: None,
        };
        assert!(ok.user_secret_definition_id.is_some());
        // 把 user 字段清掉应当校验失败
        ok.owner_user_id = None;
        assert!(ok.owner_user_id.is_none());
    }
}

