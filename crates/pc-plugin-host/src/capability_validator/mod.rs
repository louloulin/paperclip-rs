//! Plugin capability 校验器（原 `pc-plugin-capability-validator` 已下沉为本 crate 子模块）。
//!
//! 1:1 对齐 Node `server/src/services/plugin-capability-validator.ts` (525 行)。
//!
//! ## 职责
//! 1. **Install-time validation** —— `validate_manifest_capabilities` 检查 manifest
//!    中声明的 feature（tools / jobs / webhooks / UI slots / launchers / ...）是否
//!    对应了所需的 capability。
//! 2. **Runtime gating** —— `check_operation` / `assert_operation` 在 worker→host
//!    bridge 调用时被调用，最小权限访问；未知 operation **默认拒绝** (fail-closed)。
//!
//! ## 模块拆分（高内聚低耦合）
//! - [`capabilities`] —— 静态 capability catalogue + 所有 capability 映射表（数据）
//! - [`manifest`] —— Validator 视角的 manifest 视图 trait + JSON 默认实现（解耦）
//! - [`result`] —— `CapabilityCheckResult` 类型
//! - [`error`] —— `ForbiddenError`（assert_* 失败时抛）
//! - [`validator`] —— `PluginCapabilityValidator` trait + 默认实现 + 工厂函数
//!
//! ## 零外部依赖
//! 不依赖 DB / 网络 / IO / host 系统；只依赖 `serde` + `serde_json` + `thiserror`
//! + `tracing`。可被 server / cli / test / 其他 host 子系统任意复用。

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod error;
pub mod manifest;
pub mod result;
pub mod validator;

// ============================================================================
// Public re-exports
// ============================================================================

pub use capabilities::{
    feature_capability, is_valid_capability, launcher_placement_capability, operation_capabilities,
    parse_capability, parse_ui_slot, ui_slot_capability, ManifestFeature, PluginCapability,
    PluginLauncherPlacementZone, PluginUiSlotType, PLUGIN_CAPABILITIES,
};

pub use error::ForbiddenError;

pub use manifest::{JsonManifestView, PluginManifestV1View};

pub use result::CapabilityCheckResult;

pub use validator::{
    plugin_capability_validator, DefaultPluginCapabilityValidator, PluginCapabilityValidator,
};
