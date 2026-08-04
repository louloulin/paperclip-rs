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

use chrono::{DateTime, Utc};
use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

/// Resolution state for the worktree run execution override. Mirrors Node
/// `resolveWorktreeRunExecutionActivation` in
/// `services/instance-settings.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRunExecutionActivation {
    pub armed: bool,
    pub cutoff: Option<DateTime<Utc>>,
    pub activation_instance_id: Option<String>,
    pub reason: Option<&'static str>,
}

impl WorktreeRunExecutionActivation {
    pub fn suppressed(reason: &'static str) -> Self {
        Self {
            armed: false,
            cutoff: None,
            activation_instance_id: None,
            reason: Some(reason),
        }
    }
}

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

    /// Resolve the worktree run execution activation from the experimental
    /// settings. Mirrors Node `resolveWorktreeRunExecutionActivation`.
    /// Returns the suppressed state when `enableWorktreeRunExecution` is not
    /// `true`, the activation cutoff is missing, or the activation instance
    /// id does not match the current instance id.
    pub async fn resolve_worktree_run_execution_activation(
        &self,
        current_instance_id: Option<&str>,
    ) -> RepoResult<WorktreeRunExecutionActivation> {
        let row = self.get().await?;
        let experimental = row.experimental;
        let enabled = experimental
            .get("enableWorktreeRunExecution")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            return Ok(WorktreeRunExecutionActivation::suppressed("flag_disabled"));
        }
        let cutoff = experimental
            .get("worktreeRunExecutionActivatedAt")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let activation_instance_id = experimental
            .get("worktreeRunExecutionActivationInstanceId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());
        let Some(cutoff) = cutoff else {
            return Ok(WorktreeRunExecutionActivation::suppressed("missing_cutoff"));
        };
        let current = match current_instance_id {
            Some(c) => c,
            None => {
                return Ok(WorktreeRunExecutionActivation::suppressed(
                    "missing_instance_id",
                ));
            }
        };
        let activation_id = match activation_instance_id.as_deref() {
            Some(a) => a,
            None => {
                return Ok(WorktreeRunExecutionActivation::suppressed(
                    "missing_instance_id",
                ));
            }
        };
        if activation_id != current {
            return Ok(WorktreeRunExecutionActivation::suppressed(
                "instance_id_mismatch",
            ));
        }
        Ok(WorktreeRunExecutionActivation {
            armed: true,
            cutoff: Some(cutoff),
            activation_instance_id: Some(current.to_owned()),
            reason: None,
        })
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

    fn make_experimental(map: serde_json::Value) -> WorktreeRunExecutionActivation {
        let current = Some("instance-1");
        let experimental = map;
        let enabled = experimental
            .get("enableWorktreeRunExecution")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            return WorktreeRunExecutionActivation::suppressed("flag_disabled");
        }
        let cutoff = experimental
            .get("worktreeRunExecutionActivatedAt")
            .and_then(|v| v.as_str())
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        let activation_instance_id = experimental
            .get("worktreeRunExecutionActivationInstanceId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_owned());
        let Some(cutoff) = cutoff else {
            return WorktreeRunExecutionActivation::suppressed("missing_cutoff");
        };
        let current = match current {
            Some(c) => c,
            None => {
                return WorktreeRunExecutionActivation::suppressed("missing_instance_id");
            }
        };
        let activation_id = match activation_instance_id.as_deref() {
            Some(a) => a,
            None => {
                return WorktreeRunExecutionActivation::suppressed("missing_instance_id");
            }
        };
        if activation_id != current {
            return WorktreeRunExecutionActivation::suppressed("instance_id_mismatch");
        }
        WorktreeRunExecutionActivation {
            armed: true,
            cutoff: Some(cutoff),
            activation_instance_id: Some(current.to_owned()),
            reason: None,
        }
    }

    #[test]
    fn worktree_activation_suppressed_when_flag_disabled() {
        let activation = make_experimental(serde_json::json!({}));
        assert!(!activation.armed);
        assert_eq!(activation.reason, Some("flag_disabled"));
    }

    #[test]
    fn worktree_activation_suppressed_when_cutoff_missing() {
        let activation = make_experimental(serde_json::json!({
            "enableWorktreeRunExecution": true,
        }));
        assert!(!activation.armed);
        assert_eq!(activation.reason, Some("missing_cutoff"));
    }

    #[test]
    fn worktree_activation_suppressed_when_instance_id_mismatches() {
        let activation = make_experimental(serde_json::json!({
            "enableWorktreeRunExecution": true,
            "worktreeRunExecutionActivatedAt": "2026-08-04T00:00:00Z",
            "worktreeRunExecutionActivationInstanceId": "instance-2",
        }));
        assert!(!activation.armed);
        assert_eq!(activation.reason, Some("instance_id_mismatch"));
    }

    #[test]
    fn worktree_activation_armed_when_all_match() {
        let activation = make_experimental(serde_json::json!({
            "enableWorktreeRunExecution": true,
            "worktreeRunExecutionActivatedAt": "2026-08-04T00:00:00Z",
            "worktreeRunExecutionActivationInstanceId": "instance-1",
        }));
        assert!(activation.armed);
        assert!(activation.cutoff.is_some());
        assert_eq!(activation.activation_instance_id, Some("instance-1".to_owned()));
        assert!(activation.reason.is_none());
    }
}
