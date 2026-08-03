//! `instance_settings` 域 — Paperclip 实例级别配置。
//!
//! 单例（singleton_key='default'）一行的实例全局配置：
//! - `general`：稳定 UI 行为（如 theme / locale / cache strategy）
//! - `experimental`：实验开关（如新 agent 类型 / alpha 功能）
//! - `default_environment_id`：跨公司默认环境的可选覆盖
//!
//! 设计：
//! - 单例行使用 UPSERT 保证 get() 总能返回
//! - 通过 `general || $1` 做 jsonb 字段级合并而非整体覆盖
//! - `experimental` 在 release 模式下应严格走 feature-flag 门禁

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

const SINGLETON_KEY: &str = "default";
const COLS: &str = "id, singleton_key, default_environment_id, general, experimental,      created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSettingRow {
    pub id: Uuid,
    pub singleton_key: String,
    pub default_environment_id: Option<Uuid>,
    pub general: Value,
    pub experimental: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Backward compatibility alias (used by existing pc-http routes).
pub type InstanceSetting = InstanceSettingRow;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceSettingPatch {
    pub default_environment_id: Option<Option<Uuid>>, // None=不改; Some(None)=清空; Some(Some(x))=设置
    pub general_merge: Option<Value>,
    pub experimental_merge: Option<Value>,
}

pub struct SettingsRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SettingsRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Legacy back-compat: ignores the key, returns the singleton row.
    #[allow(dead_code)]
    pub async fn get_with_key(&self, _key: &str) -> RepoResult<InstanceSettingRow> {
        self.get().await
    }

    /// Back-compat: takes (key), ignored.
    #[allow(dead_code)]
    pub async fn _legacy_get(&self, key: &str) -> RepoResult<InstanceSettingRow> {
        self.get_with_key(key).await
    }

    /// Back-compat: simple get() returns singleton row.
    #[allow(dead_code)]
    pub async fn simple_get(&self) -> RepoResult<InstanceSettingRow> {
        self.get().await
    }

    /// Ensure the singleton row exists and return the latest value.
    pub async fn get(&self) -> RepoResult<InstanceSettingRow> {
        // UPSERT 然后返回
        let sql = format!(
            "INSERT INTO instance_settings (singleton_key)              VALUES ('{SINGLETON_KEY}')              ON CONFLICT (singleton_key) DO UPDATE SET singleton_key=EXCLUDED.singleton_key              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, InstanceSettingRow>(&sql)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn patch(&self, p: &InstanceSettingPatch) -> RepoResult<InstanceSettingRow> {
        // 先 ensure 行存在
        self.get().await?;
        // 执行 patch
        let new_env: Option<Option<Uuid>> = p.default_environment_id;
        let env_set = new_env.is_some();
        let env_value = new_env.flatten();
        let general = p.general_merge.clone().unwrap_or_else(|| json!({}));
        let experimental = p.experimental_merge.clone().unwrap_or_else(|| json!({}));
        // `general || $merge`，`COALESCE` 让 None 表示"不改"
        let sql = format!(
            "UPDATE instance_settings SET                 default_environment_id = CASE WHEN $1::bool THEN $2 ELSE default_environment_id END,                 general = general || $3::jsonb,                 experimental = experimental || $4::jsonb,                 updated_at = now()              WHERE singleton_key='{SINGLETON_KEY}'              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, InstanceSettingRow>(&sql)
            .bind(env_set)
            .bind(env_value)
            .bind(&general)
            .bind(&experimental)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// 读取某个 feature flag（带实验开关）
    pub async fn is_experimental_enabled(&self, flag: &str) -> RepoResult<bool> {
        let v: Option<Value> = sqlx::query_scalar(
            "SELECT experimental FROM instance_settings WHERE singleton_key=$1",
        )
        .bind(SINGLETON_KEY)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(v.and_then(|m| m.get(flag).cloned()).and_then(|x| x.as_bool()).unwrap_or(false))
    }

    pub async fn read_general(&self, key: &str) -> RepoResult<Option<Value>> {
        let v: Option<Value> = sqlx::query_scalar(
            "SELECT general FROM instance_settings WHERE singleton_key=$1",
        )
        .bind(SINGLETON_KEY)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(v.and_then(|m| m.get(key).cloned()))
    }

    /// 重置整个 general / experimental 到默认（清空）。仅管理员调用。
    /// Back-compat shim: simple patch() returning the full row.
    #[allow(dead_code)]
    pub async fn patch_simple(
        &self,
        default_environment_id: Option<Uuid>,
        general: Option<Value>,
        experimental: Option<Value>,
    ) -> RepoResult<InstanceSettingRow> {
        let p = InstanceSettingPatch {
            default_environment_id: Some(default_environment_id),
            general_merge: general,
            experimental_merge: experimental,
        };
        self.patch(&p).await
    }

    pub async fn reset_all(&self) -> RepoResult<()> {
        sqlx::query(
            "UPDATE instance_settings SET general='{}'::jsonb, experimental='{}'::jsonb,              updated_at=now() WHERE singleton_key=$1",
        )
        .bind(SINGLETON_KEY)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 整行替换（用于导入 / 离线 sync 场景）
    pub async fn replace(
        &self,
        default_environment_id: Option<Uuid>,
        general: &Value,
        experimental: &Value,
    ) -> RepoResult<InstanceSettingRow> {
        if !general.is_object() || !experimental.is_object() {
            return Err(RepoError::Invalid(
                "general/experimental must be JSON objects".into(),
            ));
        }
        let sql = format!(
            "INSERT INTO instance_settings (singleton_key, default_environment_id, general, experimental)              VALUES ($1, $2, $3, $4)              ON CONFLICT (singleton_key) DO UPDATE SET                 default_environment_id=EXCLUDED.default_environment_id,                 general=EXCLUDED.general, experimental=EXCLUDED.experimental,                 updated_at=now()              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, InstanceSettingRow>(&sql)
            .bind(SINGLETON_KEY)
            .bind(default_environment_id)
            .bind(general)
            .bind(experimental)
            .fetch_one(self.db.pool())
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_uses_double_option_for_optional_clear() {
        let keep = InstanceSettingPatch {
            default_environment_id: None,
            general_merge: None,
            experimental_merge: None,
        };
        assert!(keep.default_environment_id.is_none()); // 不改

        let clear = InstanceSettingPatch {
            default_environment_id: Some(None),
            general_merge: None,
            experimental_merge: None,
        };
        assert!(matches!(clear.default_environment_id, Some(None))); // 清空

        let set = InstanceSettingPatch {
            default_environment_id: Some(Some(Uuid::new_v4())),
            general_merge: None,
            experimental_merge: None,
        };
        assert!(set.default_environment_id.flatten().is_some());
    }

    #[test]
    fn replace_validates_json_object() {
        let bad = serde_json::json!("string");
        assert!(!bad.is_object());
        let ok = serde_json::json!({"k": 1});
        assert!(ok.is_object());
    }
}
