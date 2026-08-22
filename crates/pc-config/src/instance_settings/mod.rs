//! Paperclip 实例 settings 模块。
//!
//! 1:1 对齐 Node `server/src/services/instance-settings.ts`（438 行）。
//! 拆分：
//! - `pure`  —— 纯函数（normalizers, overlay, patch, activation）；
//!              不引入 IO / clock 副作用（clock 通过 `now` 注入）。
//! - `service` —— DI 层 + 业务逻辑；DB 读写通过
//!                `InstanceSettingsStore` trait 注入，`CompanyLister` trait
//!                暴露 `listCompanyIds()`。
//!
//! 生产实现：`pc-repos::instance_settings_repo` +
//!           `pc-repos::companies_repo`（不在本 crate）。
//! 测试实现：见 `service::tests` 中的 `MemStore` / `MemCompanies`。

#![forbid(unsafe_code)]

pub mod pure;
pub mod service;

// Re-export the surface from `pure` for backward compatibility with the
// pre-R752 public API of `pc_config::instance_settings`.
pub use pure::{
    apply_experimental_settings_patch, apply_managed_experimental_overlay,
    get_runtime_instance_id, is_truthy_runtime_env_value,
    normalize_experimental_settings, normalize_general_settings,
    resolve_worktree_run_execution_activation, strip_server_managed_experimental_patch_fields,
    suppress_worktree_run_execution, InstanceGeneralSettings, InstanceExperimentalSettings,
    ManagedExperimentalKeyMetadata, ManagedExperimentalOverlayResult, ManagedInstanceConfig,
    ManagedSettingMetadata, WorktreeRunExecutionActivationState,
    WorktreeRunExecutionSuppressedReason, DEFAULT_BACKUP_RETENTION, DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE,
    DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS, DEFAULT_SINGLETON_KEY,
    PAPERCLIP_CLOUD_MANAGED_BY,
};

// Re-export the service-layer surface.
pub use service::{
    instance_settings_service, resolve_worktree_run_execution_activation_state, CompanyLister,
    InstanceExperimentalSettingsWithManaged, InstanceSettings, InstanceSettingsRow,
    InstanceSettingsService, InstanceSettingsServiceError, InstanceSettingsServiceOptions,
    InstanceSettingsStore, PatchInstanceExperimentalSettings, PatchInstanceGeneralSettings,
    PatchInstanceSettings,
};