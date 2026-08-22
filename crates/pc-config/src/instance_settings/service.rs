//! Paperclip 实例 settings 服务层（service layer）。
//!
//! 1:1 对齐 Node `server/src/services/instance-settings.ts`（438 行, R846）。
//!
//! 拆分：
//! - `InstanceSettingsStore` trait：DB 读写（`get_or_create` / `update_*`），
//!   与具体 ORM 解耦。生产实现交给 `pc-repos` / `pc-db`；测试用 mock。
//! - `CompanyLister` trait：`listCompanyIds()` 的 DI 接口。
//! - `InstanceSettingsService` 结构体：业务逻辑，调用 store + 注入
//!   `ManagedInstanceConfig` + `now()` 闭包，组合出与 Node 一致的输出。
//! - `instance_settings_service(...)` 工厂：与 Node `instanceSettingsService(db, options)`
//!   1:1 镜像（签名略调，便于 Rust 类型）。
//!
//! 与 `pure.rs` 的边界：`service` 只组合 `pure` 中的纯函数；不在此模块
//! 直接做 schema 验证/schema stripping，patch 输入已经被上层
//! `pc-config-schema` 校验过。

#![forbid(unsafe_code)]

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

use super::pure::{
    apply_experimental_settings_patch, apply_managed_experimental_overlay,
    normalize_experimental_settings, normalize_general_settings,
    InstanceExperimentalSettings, InstanceGeneralSettings, ManagedExperimentalKeyMetadata,
    ManagedInstanceConfig, WorktreeRunExecutionActivationState,
    WorktreeRunExecutionSuppressedReason,
};

// =========================================================================
// Errors
// =========================================================================

/// 抽象层错误。底层错误（DB）通过 `Store` 的关联类型上传。
#[derive(Debug, Error)]
pub enum InstanceSettingsServiceError {
    /// 初始化 / 读取行失败（store 内部无法恢复时抛出）。
    #[error("failed to initialize instance settings row")]
    InitializationFailed,
    /// store 实现的内部错误（`pc-repos::RepoError` 等）。
    #[error("instance settings store error: {0}")]
    Store(String),
}

pub type InstanceSettingsResult<T> = Result<T, InstanceSettingsServiceError>;

// =========================================================================
// Public types
// =========================================================================

/// 单条 `instance_settings` 行（与 Node `instanceSettings.$inferSelect` 对齐）。
///
/// `general` / `experimental` 保持为 `serde_json::Value` 以兼容 Node 的
/// `Record<string, unknown>` 存储形态；具体形状由 normalize_* 提取。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceSettingsRow {
    pub id: String,
    pub default_environment_id: Option<String>,
    pub general: Value,
    pub experimental: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// 对齐 Node `InstanceSettings`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstanceSettings {
    pub id: String,
    pub default_environment_id: Option<String>,
    pub general: InstanceGeneralSettings,
    /// `managedConfig` 为 None 时不带 `managedKeys` 字段（与 Node 行为一致）。
    pub experimental: InstanceExperimentalSettingsWithManaged,
    pub created_at: String,
    pub updated_at: String,
}

/// 对齐 Node `InstanceExperimentalSettingsWithManaged`：
/// `managedConfig` 为 None 时直接是 `InstanceExperimentalSettings`；
/// 否则附带 `managedKeys` 字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum InstanceExperimentalSettingsWithManaged {
    Plain(InstanceExperimentalSettings),
    WithManaged {
        #[serde(flatten)]
        experimental: InstanceExperimentalSettings,
        #[serde(default, skip_serializing_if = "ManagedExperimentalKeyMetadata::is_empty")]
        managed_keys: ManagedExperimentalKeyMetadata,
    },
}

impl InstanceExperimentalSettingsWithManaged {
    pub fn plain(s: InstanceExperimentalSettings) -> Self {
        Self::Plain(s)
    }

    pub fn with_managed(
        s: InstanceExperimentalSettings,
        keys: ManagedExperimentalKeyMetadata,
    ) -> Self {
        if keys.is_empty() {
            Self::Plain(s)
        } else {
            Self::WithManaged {
                experimental: s,
                managed_keys: keys,
            }
        }
    }

    pub fn settings(&self) -> &InstanceExperimentalSettings {
        match self {
            Self::Plain(s) => s,
            Self::WithManaged { experimental, .. } => experimental,
        }
    }
}

/// 对齐 Node `PatchInstanceSettings`。
/// `None` 表示字段缺失（保留原值）；`Some(None)` 表示显式置 null；
/// `Some(Some(v))` 表示设置。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchInstanceSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_environment_id: Option<Option<String>>,
}

/// 对齐 Node `PatchInstanceGeneralSettings`：partial subset of
/// `InstanceGeneralSettings`。未出现的字段保留原值。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PatchInstanceGeneralSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub censor_username_in_logs: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyboard_shortcuts: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback_data_sharing_preference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_retention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<Option<String>>,
}

/// 对齐 Node `PatchInstanceExperimentalSettings`：partial subset of
/// `InstanceExperimentalSettings`，作为 `serde_json::Value` 透传
/// （caller 端已用 zod schema 校验过）。理由：实验性 flag 字段多达 30+，
/// 全字段枚举会在每次新增 flag 时改动 Rust 结构体；用 raw map 与
/// Node `Record<string, unknown>` 行为一致。Server-managed 字段
/// （`worktreeRunExecutionActivatedAt` /
/// `worktreeRunExecutionActivationInstanceId`）在
/// `apply_experimental_settings_patch` 内部被剥离。
pub type PatchInstanceExperimentalSettings = Map<String, Value>;

/// Service 选项（对齐 Node `InstanceSettingsServiceOptions`）。
#[derive(Clone, Default)]
pub struct InstanceSettingsServiceOptions {
    pub runtime_env: Option<HashMap<String, String>>,
    pub now: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

// =========================================================================
// DI traits
// =========================================================================

/// 抽象 DB 访问层（`get_or_create` / `update_*`）。
///
/// 生产实现交给 `pc-repos::instance_settings_repo`；测试用 mock。
/// 关联类型 `Error` 让 store 上传自己的错误类型（如 `sqlx::Error` /
/// `pc_repos::RepoError`），service 端用 `Into<String>` 收敛。
#[async_trait]
pub trait InstanceSettingsStore: Send + Sync {
    type Error: std::fmt::Display + Send + Sync + 'static;

    /// 读取或初始化单例行。
    /// Node 行为：`select` 失败 → 抛；找到直接返回；找不到则
    /// `insert ... onConflictDoUpdate` upsert；upsert 返回空再二次 select；
    /// 都拿不到抛 "Failed to initialize instance settings row"。
    async fn get_or_create(&self) -> Result<InstanceSettingsRow, Self::Error>;

    /// 仅更新 `default_environment_id`，并 bump `updated_at`。
    /// `default_environment_id = None` 表示显式置 null。
    async fn update_default_environment(
        &self,
        id: &str,
        default_environment_id: Option<&str>,
    ) -> Result<InstanceSettingsRow, Self::Error>;

    /// 整体替换 `general` JSON。
    async fn update_general(
        &self,
        id: &str,
        general: &Value,
    ) -> Result<InstanceSettingsRow, Self::Error>;

    /// 整体替换 `experimental` JSON。
    async fn update_experimental(
        &self,
        id: &str,
        experimental: &Value,
    ) -> Result<InstanceSettingsRow, Self::Error>;
}

/// 抽象 company 表读取（仅 `listCompanyIds()` 一项）。
#[async_trait]
pub trait CompanyLister: Send + Sync {
    type Error: std::fmt::Display + Send + Sync + 'static;
    async fn list_company_ids(&self) -> Result<Vec<String>, Self::Error>;
}

// =========================================================================
// Service
// =========================================================================

/// `InstanceSettingsService`：业务逻辑容器。构造时注入 store / lister /
/// managed config / clock options。所有方法接收 `&self`。
pub struct InstanceSettingsService<S: InstanceSettingsStore, C: CompanyLister> {
    store: Arc<S>,
    companies: Arc<C>,
    managed_config: Option<ManagedInstanceConfig>,
    options: InstanceSettingsServiceOptions,
}

impl<S: InstanceSettingsStore, C: CompanyLister> InstanceSettingsService<S, C> {
    fn to_instance_settings(&self, row: InstanceSettingsRow) -> InstanceSettings {
        let normalized = normalize_experimental_settings(Some(&row.experimental));
        let overlay = apply_managed_experimental_overlay(&normalized, self.managed_config.as_ref());
        let experimental = if self.managed_config.is_some() {
            InstanceExperimentalSettingsWithManaged::with_managed(
                overlay.experimental,
                overlay.managed_keys,
            )
        } else {
            // Node 注释：Self-hosted responses stay byte-identical: no
            // managedKeys field at all.
            InstanceExperimentalSettingsWithManaged::plain(overlay.experimental)
        };
        InstanceSettings {
            id: row.id,
            default_environment_id: row.default_environment_id,
            general: normalize_general_settings(Some(&row.general)),
            experimental,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }

    fn now_iso(&self) -> String {
        match &self.options.now {
            Some(f) => f(),
            None => super::pure::now_iso_for_test(),
        }
    }

    fn runtime_env(&self) -> HashMap<String, String> {
        match &self.options.runtime_env {
            Some(env) => env.clone(),
            None => std::env::vars().collect(),
        }
    }

    /// Get full instance settings (get-or-create on first read).
    pub async fn get(&self) -> InstanceSettingsResult<InstanceSettings> {
        let row = self
            .store
            .get_or_create()
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        Ok(self.to_instance_settings(row))
    }

    /// Patch `defaultEnvironmentId` only. Node 行为：仅在 patch 中显式带
    /// `defaultEnvironmentId` 时才写入；用 `null` 显式清空；未带保留原值。
    pub async fn update(
        &self,
        patch: PatchInstanceSettings,
    ) -> InstanceSettingsResult<InstanceSettings> {
        let current = self
            .store
            .get_or_create()
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        let row = if let Some(env) = patch.default_environment_id {
            self.store
                .update_default_environment(&current.id, env.as_deref())
                .await
                .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?
        } else {
            current
        };
        Ok(self.to_instance_settings(row))
    }

    pub async fn get_general(&self) -> InstanceSettingsResult<InstanceGeneralSettings> {
        let row = self
            .store
            .get_or_create()
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        Ok(normalize_general_settings(Some(&row.general)))
    }

    pub async fn get_experimental(
        &self,
    ) -> InstanceSettingsResult<InstanceExperimentalSettingsWithManaged> {
        let row = self
            .store
            .get_or_create()
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        let normalized = normalize_experimental_settings(Some(&row.experimental));
        let overlay = apply_managed_experimental_overlay(&normalized, self.managed_config.as_ref());
        Ok(if self.managed_config.is_some() {
            InstanceExperimentalSettingsWithManaged::with_managed(
                overlay.experimental,
                overlay.managed_keys,
            )
        } else {
            InstanceExperimentalSettingsWithManaged::plain(overlay.experimental)
        })
    }

    pub async fn update_general(
        &self,
        patch: PatchInstanceGeneralSettings,
    ) -> InstanceSettingsResult<InstanceSettings> {
        let current = self
            .store
            .get_or_create()
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        let mut merged = normalize_general_settings(Some(&current.general));
        // Node: shallow-merge patch over current (only keys present override).
        if let Some(v) = patch.censor_username_in_logs {
            merged.censor_username_in_logs = v;
        }
        if let Some(v) = patch.keyboard_shortcuts {
            merged.keyboard_shortcuts = v;
        }
        if let Some(v) = patch.feedback_data_sharing_preference {
            merged.feedback_data_sharing_preference = v;
        }
        if let Some(v) = patch.backup_retention {
            merged.backup_retention = v;
        }
        if let Some(v) = patch.execution_mode {
            // Some(None) => explicit null; Some(Some(s)) => set; None => keep.
            merged.execution_mode = v;
        }
        let general_json =
            serde_json::to_value(&merged).map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        let row = self
            .store
            .update_general(&current.id, &general_json)
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        Ok(self.to_instance_settings(row))
    }

    pub async fn update_experimental(
        &self,
        patch: PatchInstanceExperimentalSettings,
    ) -> InstanceSettingsResult<InstanceSettings> {
        let current = self
            .store
            .get_or_create()
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        let runtime_env = self.runtime_env();
        let next = apply_experimental_settings_patch(
            &current.experimental,
            &patch,
            Some(&runtime_env),
            Some(&|| self.now_iso()),
        );
        let experimental_json = serde_json::to_value(&next)
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        let row = self
            .store
            .update_experimental(&current.id, &experimental_json)
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))?;
        Ok(self.to_instance_settings(row))
    }

    pub async fn list_company_ids(&self) -> InstanceSettingsResult<Vec<String>> {
        self.companies
            .list_company_ids()
            .await
            .map_err(|e| InstanceSettingsServiceError::Store(e.to_string()))
    }
}

// =========================================================================
// Factory
// =========================================================================

/// 对齐 Node `instanceSettingsService(db, options)`。
/// 调用方负责提供 `Arc<dyn InstanceSettingsStore>` / `Arc<dyn CompanyLister>`
/// 与具体的 store/lister 实现（生产中由 `pc-repos` 提供；测试用 mock）。
pub fn instance_settings_service<S, C>(
    store: Arc<S>,
    companies: Arc<C>,
    managed_config: Option<ManagedInstanceConfig>,
    options: InstanceSettingsServiceOptions,
) -> InstanceSettingsService<S, C>
where
    S: InstanceSettingsStore + 'static,
    C: CompanyLister + 'static,
{
    InstanceSettingsService {
        store,
        companies,
        managed_config,
        options,
    }
}

// =========================================================================
// Async env resolver
// =========================================================================

/// 对齐 Node `resolveWorktreeRunExecutionActivationState(options)`。
/// 步骤：
/// 1. 若 `PAPERCLIP_IN_WORKTREE` 非 truthy → `not_worktree_runtime`。
/// 2. 调 `get_experimental` 读实验设置；IO 异常 → `settings_read_error`。
/// 3. 调纯函数 `resolve_worktree_run_execution_activation` 决策。
pub async fn resolve_worktree_run_execution_activation_state<S, C>(
    service: &InstanceSettingsService<S, C>,
) -> WorktreeRunExecutionActivationState
where
    S: InstanceSettingsStore + 'static,
    C: CompanyLister + 'static,
{
    let runtime_env = service.runtime_env();
    let in_worktree = super::pure::is_truthy_runtime_env_value(
        runtime_env.get("PAPERCLIP_IN_WORKTREE").map(String::as_str),
    );
    if !in_worktree {
        return super::pure::suppress_worktree_run_execution(
            WorktreeRunExecutionSuppressedReason::NotWorktreeRuntime,
            None,
        );
    }
    match service.get_experimental().await {
        Ok(view) => super::pure::resolve_worktree_run_execution_activation(
            view.settings(),
            super::pure::get_runtime_instance_id(&runtime_env).as_deref(),
        ),
        Err(_) => super::pure::suppress_worktree_run_execution(
            WorktreeRunExecutionSuppressedReason::SettingsReadError,
            None,
        ),
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    // ---- In-memory store mock ----

    #[derive(Default)]
    struct MemStore {
        row: Mutex<Option<InstanceSettingsRow>>,
    }

    impl MemStore {
        fn with_row(row: InstanceSettingsRow) -> Self {
            Self {
                row: Mutex::new(Some(row)),
            }
        }
    }

    #[async_trait]
    impl InstanceSettingsStore for MemStore {
        type Error = String;

        async fn get_or_create(&self) -> Result<InstanceSettingsRow, Self::Error> {
            let mut guard = self.row.lock().unwrap();
            if let Some(r) = guard.as_ref() {
                return Ok(r.clone());
            }
            // Simulate Node insert.
            let r = InstanceSettingsRow {
                id: "row-1".into(),
                default_environment_id: None,
                general: json!({}).into(),
                experimental: json!({}).into(),
                created_at: "2024-01-01T00:00:00.000Z".into(),
                updated_at: "2024-01-01T00:00:00.000Z".into(),
            };
            *guard = Some(r.clone());
            Ok(r)
        }

        async fn update_default_environment(
            &self,
            id: &str,
            default_environment_id: Option<&str>,
        ) -> Result<InstanceSettingsRow, Self::Error> {
            let mut guard = self.row.lock().unwrap();
            let row = guard.as_mut().ok_or("no row")?;
            assert_eq!(row.id, id);
            row.default_environment_id = default_environment_id.map(str::to_string);
            row.updated_at = "2024-01-02T00:00:00.000Z".into();
            Ok(row.clone())
        }

        async fn update_general(
            &self,
            id: &str,
            general: &Value,
        ) -> Result<InstanceSettingsRow, Self::Error> {
            let mut guard = self.row.lock().unwrap();
            let row = guard.as_mut().ok_or("no row")?;
            assert_eq!(row.id, id);
            row.general = general.clone();
            row.updated_at = "2024-01-02T00:00:00.000Z".into();
            Ok(row.clone())
        }

        async fn update_experimental(
            &self,
            id: &str,
            experimental: &Value,
        ) -> Result<InstanceSettingsRow, Self::Error> {
            let mut guard = self.row.lock().unwrap();
            let row = guard.as_mut().ok_or("no row")?;
            assert_eq!(row.id, id);
            row.experimental = experimental.clone();
            row.updated_at = "2024-01-02T00:00:00.000Z".into();
            Ok(row.clone())
        }
    }

    struct MemCompanies(Vec<String>);
    #[async_trait]
    impl CompanyLister for MemCompanies {
        type Error = String;
        async fn list_company_ids(&self) -> Result<Vec<String>, Self::Error> {
            Ok(self.0.clone())
        }
    }

    fn sample_row() -> InstanceSettingsRow {
        InstanceSettingsRow {
            id: "row-1".into(),
            default_environment_id: None,
            general: json!({"censorUsernameInLogs": true}).into(),
            experimental: json!({"enableApps": true}).into(),
            created_at: "2024-01-01T00:00:00.000Z".into(),
            updated_at: "2024-01-01T00:00:00.000Z".into(),
        }
    }

    fn build_service() -> InstanceSettingsService<MemStore, MemCompanies> {
        instance_settings_service(
            Arc::new(MemStore::with_row(sample_row())),
            Arc::new(MemCompanies(vec!["co-a".into(), "co-b".into()])),
            None,
            InstanceSettingsServiceOptions::default(),
        )
    }

    // ---- get / list ----

    #[tokio::test]
    async fn get_initializes_and_normalizes() {
        let svc = build_service();
        let view = svc.get().await.unwrap();
        assert_eq!(view.id, "row-1");
        assert!(view.general.censor_username_in_logs);
        assert!(view.experimental.settings().enable_apps);
        // No managed config => experimental stays a Plain variant.
        assert!(matches!(
            view.experimental,
            InstanceExperimentalSettingsWithManaged::Plain(_)
        ));
    }

    #[tokio::test]
    async fn get_general_returns_normalized() {
        let svc = build_service();
        let g = svc.get_general().await.unwrap();
        assert!(g.censor_username_in_logs);
    }

    #[tokio::test]
    async fn get_experimental_without_managed_returns_plain() {
        let svc = build_service();
        let view = svc.get_experimental().await.unwrap();
        assert!(view.settings().enable_apps);
        assert!(matches!(
            view,
            InstanceExperimentalSettingsWithManaged::Plain(_)
        ));
    }

    #[tokio::test]
    async fn list_company_ids_returns_all() {
        let svc = build_service();
        let ids = svc.list_company_ids().await.unwrap();
        assert_eq!(ids, vec!["co-a".to_string(), "co-b".to_string()]);
    }

    // ---- update (defaultEnvironmentId) ----

    #[tokio::test]
    async fn update_sets_default_environment_id() {
        let svc = build_service();
        let updated = svc
            .update(PatchInstanceSettings {
                default_environment_id: Some(Some("env-42".into())),
            })
            .await
            .unwrap();
        assert_eq!(updated.default_environment_id.as_deref(), Some("env-42"));
    }

    #[tokio::test]
    async fn update_clears_default_environment_id_on_explicit_null() {
        let svc = build_service();
        let updated = svc
            .update(PatchInstanceSettings {
                default_environment_id: Some(None),
            })
            .await
            .unwrap();
        assert_eq!(updated.default_environment_id, None);
    }

    #[tokio::test]
    async fn update_without_field_keeps_existing() {
        let svc = build_service();
        // Pre-set env
        svc.update(PatchInstanceSettings {
            default_environment_id: Some(Some("env-pre".into())),
        })
        .await
        .unwrap();
        // No field in patch
        let updated = svc.update(PatchInstanceSettings::default()).await.unwrap();
        assert_eq!(updated.default_environment_id.as_deref(), Some("env-pre"));
    }

    // ---- update_general ----

    #[tokio::test]
    async fn update_general_merges_patch() {
        let svc = build_service();
        let updated = svc
            .update_general(PatchInstanceGeneralSettings {
                keyboard_shortcuts: Some(true),
                ..Default::default()
            })
            .await
            .unwrap();
        // prior censor stays true
        assert!(updated.general.censor_username_in_logs);
        assert!(updated.general.keyboard_shortcuts);
    }

    #[tokio::test]
    async fn update_general_clears_execution_mode_on_explicit_null() {
        let svc = build_service();
        svc.update_general(PatchInstanceGeneralSettings {
            execution_mode: Some(Some("restricted".into())),
            ..Default::default()
        })
        .await
        .unwrap();
        let cleared = svc
            .update_general(PatchInstanceGeneralSettings {
                execution_mode: Some(None),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(cleared.general.execution_mode, None);
    }

    // ---- update_experimental ----

    #[tokio::test]
    async fn update_experimental_applies_patch() {
        let svc = build_service();
        let mut patch = Map::new();
        patch.insert("enablePipelines".into(), Value::Bool(true));
        let updated = svc.update_experimental(patch).await.unwrap();
        assert!(updated.experimental.settings().enable_apps); // preserved
        assert!(updated.experimental.settings().enable_pipelines); // new
    }

    #[tokio::test]
    async fn update_experimental_strips_server_managed_fields() {
        let svc = build_service();
        let mut patch = Map::new();
        patch.insert("enableApps".into(), Value::Bool(false));
        patch.insert(
            "worktreeRunExecutionActivatedAt".into(),
            Value::String("tampered".into()),
        );
        let updated = svc.update_experimental(patch).await.unwrap();
        assert!(!updated.experimental.settings().enable_apps);
        assert_eq!(
            updated.experimental.settings().worktree_run_execution_activated_at,
            None
        );
    }

    #[tokio::test]
    async fn update_experimental_stamps_first_enable_in_worktree() {
        let store = Arc::new(MemStore::with_row(sample_row()));
        let companies = Arc::new(MemCompanies(vec![]));
        let mut env = HashMap::new();
        env.insert("PAPERCLIP_IN_WORKTREE".into(), "1".into());
        env.insert("PAPERCLIP_INSTANCE_ID".into(), "inst-x".into());
        let opts = InstanceSettingsServiceOptions {
            runtime_env: Some(env),
            ..Default::default()
        };
        let svc = instance_settings_service(store, companies, None, opts);
        let mut patch = Map::new();
        patch.insert("enableWorktreeRunExecution".into(), Value::Bool(true));
        let updated = svc.update_experimental(patch).await.unwrap();
        assert!(updated.experimental.settings().enable_worktree_run_execution);
        assert!(updated
            .experimental
            .settings()
            .worktree_run_execution_activated_at
            .is_some());
        assert_eq!(
            updated
                .experimental
                .settings()
                .worktree_run_execution_activation_instance_id
                .as_deref(),
            Some("inst-x")
        );
    }

    // ---- managed overlay ----

    #[tokio::test]
    async fn get_with_managed_overlay_overrides_and_attaches_metadata() {
        let store = Arc::new(MemStore::with_row(sample_row()));
        let companies = Arc::new(MemCompanies(vec![]));
        let mut features = HashMap::new();
        features.insert("enableApps".into(), false);
        let managed = ManagedInstanceConfig { features };
        let svc = instance_settings_service(store, companies, Some(managed), Default::default());
        let view = svc.get().await.unwrap();
        // Overlay forced enableApps=false despite row true.
        assert!(!view.experimental.settings().enable_apps);
        match view.experimental {
            InstanceExperimentalSettingsWithManaged::WithManaged { managed_keys, .. } => {
                assert!(managed_keys.contains_key("enableApps"));
            }
            _ => panic!("expected WithManaged"),
        }
    }

    #[tokio::test]
    async fn get_without_managed_returns_plain() {
        let svc = build_service();
        let view = svc.get().await.unwrap();
        assert!(matches!(
            view.experimental,
            InstanceExperimentalSettingsWithManaged::Plain(_)
        ));
    }

    // ---- async env resolver ----

    #[tokio::test]
    async fn resolver_short_circuits_outside_worktree() {
        let svc = build_service();
        let state = resolve_worktree_run_execution_activation_state(&svc).await;
        match state {
            WorktreeRunExecutionActivationState::Suppressed { reason, .. } => {
                assert_eq!(reason, WorktreeRunExecutionSuppressedReason::NotWorktreeRuntime);
            }
            _ => panic!("expected Suppressed"),
        }
    }

    #[tokio::test]
    async fn resolver_in_worktree_with_matching_id_arms() {
        let mut row = sample_row();
        row.experimental = json!({
            "enableWorktreeRunExecution": true,
            "worktreeRunExecutionActivatedAt": "2024-05-01T00:00:00.000Z",
            "worktreeRunExecutionActivationInstanceId": "inst-x"
        })
        .into();
        let store = Arc::new(MemStore::with_row(row));
        let companies = Arc::new(MemCompanies(vec![]));
        let mut env = HashMap::new();
        env.insert("PAPERCLIP_IN_WORKTREE".into(), "1".into());
        env.insert("PAPERCLIP_INSTANCE_ID".into(), "inst-x".into());
        let svc = instance_settings_service(
            store,
            companies,
            None,
            InstanceSettingsServiceOptions {
                runtime_env: Some(env),
                ..Default::default()
            },
        );
        let state = resolve_worktree_run_execution_activation_state(&svc).await;
        match state {
            WorktreeRunExecutionActivationState::Armed {
                cutoff,
                activation_instance_id,
                ..
            } => {
                assert_eq!(cutoff, "2024-05-01T00:00:00.000Z");
                assert_eq!(activation_instance_id, "inst-x");
            }
            _ => panic!("expected Armed"),
        }
    }

    #[tokio::test]
    async fn resolver_returns_settings_read_error_on_store_failure() {
        // Build a store that errors on get_or_create.
        struct FlakeyStore;
        #[async_trait]
        impl InstanceSettingsStore for FlakeyStore {
            type Error = String;
            async fn get_or_create(&self) -> Result<InstanceSettingsRow, Self::Error> {
                Err("boom".into())
            }
            async fn update_default_environment(
                &self,
                _: &str,
                _: Option<&str>,
            ) -> Result<InstanceSettingsRow, Self::Error> {
                Err("boom".into())
            }
            async fn update_general(
                &self,
                _: &str,
                _: &Value,
            ) -> Result<InstanceSettingsRow, Self::Error> {
                Err("boom".into())
            }
            async fn update_experimental(
                &self,
                _: &str,
                _: &Value,
            ) -> Result<InstanceSettingsRow, Self::Error> {
                Err("boom".into())
            }
        }
        let mut env = HashMap::new();
        env.insert("PAPERCLIP_IN_WORKTREE".into(), "1".into());
        let svc = instance_settings_service(
            Arc::new(FlakeyStore),
            Arc::new(MemCompanies(vec![])),
            None,
            InstanceSettingsServiceOptions {
                runtime_env: Some(env),
                ..Default::default()
            },
        );
        let state = resolve_worktree_run_execution_activation_state(&svc).await;
        match state {
            WorktreeRunExecutionActivationState::Suppressed { reason, .. } => {
                assert_eq!(
                    reason,
                    WorktreeRunExecutionSuppressedReason::SettingsReadError
                );
            }
            _ => panic!("expected Suppressed"),
        }
    }
}