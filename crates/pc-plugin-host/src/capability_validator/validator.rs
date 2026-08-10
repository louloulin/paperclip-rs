//! Plugin capability validator trait + 默认实现。
//!
//! 高内聚：本模块是 validator 的全部逻辑核心 —— 所有 `has_*` / `check_*` /
//! `assert_*` / `validate_*` 方法都在这里。
//!
//! 低耦合：
//! - [`PluginCapabilityValidator`] 是 trait，可注入 mock / 改写
//! - 不依赖 DB / 网络 / IO
//! - 日志通过 `tracing` crate
//! - manifest 通过 [`PluginManifestV1View`] trait 传入

use std::collections::HashSet;

use tracing::{debug, warn};

use super::capabilities::{
    feature_capability, launcher_placement_capability, operation_capabilities,
    parse_ui_slot, ui_slot_capability, ManifestFeature, PluginCapability,
};
use super::error::ForbiddenError;
use super::manifest::PluginManifestV1View;
use super::result::CapabilityCheckResult;

// ============================================================================
// Validator trait
// ============================================================================

pub trait PluginCapabilityValidator {
    fn has_capability(&self, manifest: &dyn PluginManifestV1View, capability: &str) -> bool;

    fn has_all_capabilities(
        &self,
        manifest: &dyn PluginManifestV1View,
        capabilities: &[&str],
    ) -> CapabilityCheckResult;

    fn has_any_capability(
        &self,
        manifest: &dyn PluginManifestV1View,
        capabilities: &[&str],
    ) -> bool;

    fn check_operation(
        &self,
        manifest: &dyn PluginManifestV1View,
        operation: &str,
    ) -> CapabilityCheckResult;

    fn assert_operation(
        &self,
        manifest: &dyn PluginManifestV1View,
        operation: &str,
    ) -> Result<(), ForbiddenError>;

    fn assert_capability(
        &self,
        manifest: &dyn PluginManifestV1View,
        capability: &str,
    ) -> Result<(), ForbiddenError>;

    fn check_ui_slot(
        &self,
        manifest: &dyn PluginManifestV1View,
        slot_type: &str,
    ) -> CapabilityCheckResult;

    fn validate_manifest_capabilities(
        &self,
        manifest: &dyn PluginManifestV1View,
    ) -> CapabilityCheckResult;

    fn get_required_capabilities(&self, operation: &str) -> Vec<PluginCapability>;

    fn get_ui_slot_capability(&self, slot_type: &str) -> Option<PluginCapability>;
}

// ============================================================================
// Default validator
// ============================================================================

#[derive(Debug, Default, Clone)]
pub struct DefaultPluginCapabilityValidator;

impl DefaultPluginCapabilityValidator {
    pub const fn new() -> Self {
        Self
    }
}

impl PluginCapabilityValidator for DefaultPluginCapabilityValidator {
    fn has_capability(&self, manifest: &dyn PluginManifestV1View, capability: &str) -> bool {
        manifest
            .capabilities()
            .iter()
            .any(|c| c.as_str() == capability)
    }

    fn has_all_capabilities(
        &self,
        manifest: &dyn PluginManifestV1View,
        capabilities: &[&str],
    ) -> CapabilityCheckResult {
        let declared = declared_set(manifest);
        let missing: Vec<PluginCapability> = capabilities
            .iter()
            .filter(|c| !declared.contains(**c))
            .map(|c| PluginCapability::new(*c))
            .collect();
        CapabilityCheckResult {
            allowed: missing.is_empty(),
            missing,
            operation: None,
            plugin_id: Some(manifest.id().to_string()),
        }
    }

    fn has_any_capability(
        &self,
        manifest: &dyn PluginManifestV1View,
        capabilities: &[&str],
    ) -> bool {
        let declared = declared_set(manifest);
        capabilities.iter().any(|c| declared.contains(*c))
    }

    fn check_operation(
        &self,
        manifest: &dyn PluginManifestV1View,
        operation: &str,
    ) -> CapabilityCheckResult {
        let required = operation_capabilities(operation);
        if required.is_empty() {
            warn!(
                plugin_id = manifest.id(),
                operation,
                "capability check for unknown operation \u{2013} rejecting by default"
            );
            return CapabilityCheckResult {
                allowed: false,
                missing: Vec::new(),
                operation: Some(operation.to_string()),
                plugin_id: Some(manifest.id().to_string()),
            };
        }
        let declared = declared_set(manifest);
        let missing: Vec<PluginCapability> = required
            .into_iter()
            .filter(|cap| !declared.contains(cap.as_str()))
            .collect();
        if !missing.is_empty() {
            debug!(
                plugin_id = manifest.id(),
                operation,
                missing = ?missing.iter().map(PluginCapability::as_str).collect::<Vec<_>>(),
                "capability check failed"
            );
        }
        CapabilityCheckResult {
            allowed: missing.is_empty(),
            missing,
            operation: Some(operation.to_string()),
            plugin_id: Some(manifest.id().to_string()),
        }
    }

    fn assert_operation(
        &self,
        manifest: &dyn PluginManifestV1View,
        operation: &str,
    ) -> Result<(), ForbiddenError> {
        let result = self.check_operation(manifest, operation);
        if result.allowed {
            return Ok(());
        }
        let message = if result.missing.is_empty() {
            format!(
                "Plugin '{}' attempted unknown operation '{}'",
                manifest.id(),
                operation
            )
        } else {
            build_forbidden_message(manifest, operation, &result.missing)
        };
        Err(ForbiddenError::new(message))
    }

    fn assert_capability(
        &self,
        manifest: &dyn PluginManifestV1View,
        capability: &str,
    ) -> Result<(), ForbiddenError> {
        if self.has_capability(manifest, capability) {
            return Ok(());
        }
        Err(ForbiddenError::new(format!(
            "Plugin '{}' lacks required capability '{}'",
            manifest.id(),
            capability
        )))
    }

    fn check_ui_slot(
        &self,
        manifest: &dyn PluginManifestV1View,
        slot_type: &str,
    ) -> CapabilityCheckResult {
        if parse_ui_slot(slot_type).is_none() {
            return CapabilityCheckResult {
                allowed: false,
                missing: Vec::new(),
                operation: Some(format!("ui.{}.register", slot_type)),
                plugin_id: Some(manifest.id().to_string()),
            };
        }
        let required = ui_slot_capability(slot_type);
        match required {
            None => CapabilityCheckResult {
                allowed: false,
                missing: Vec::new(),
                operation: Some(format!("ui.{}.register", slot_type)),
                plugin_id: Some(manifest.id().to_string()),
            },
            Some(cap) => {
                let has = self.has_capability(manifest, cap.as_str());
                let mut missing = Vec::new();
                if !has {
                    missing.push(cap);
                }
                CapabilityCheckResult {
                    allowed: has,
                    missing,
                    operation: Some(format!("ui.{}.register", slot_type)),
                    plugin_id: Some(manifest.id().to_string()),
                }
            }
        }
    }

    fn validate_manifest_capabilities(
        &self,
        manifest: &dyn PluginManifestV1View,
    ) -> CapabilityCheckResult {
        let declared = declared_set(manifest);
        let mut all_missing: Vec<PluginCapability> = Vec::new();

        // Feature declarations → required capabilities
        let feature_presence: [(ManifestFeature, bool); 9] = [
            (ManifestFeature::Tools, !manifest.tools().is_empty()),
            (ManifestFeature::Jobs, !manifest.jobs().is_empty()),
            (ManifestFeature::Webhooks, !manifest.webhooks().is_empty()),
            (ManifestFeature::Database, manifest.has_database()),
            (ManifestFeature::EnvironmentDrivers, !manifest.environment_drivers().is_empty()),
            (ManifestFeature::Agents, !manifest.agents().is_empty()),
            (ManifestFeature::Projects, !manifest.projects().is_empty()),
            (ManifestFeature::Routines, !manifest.routines().is_empty()),
            (ManifestFeature::ObjectReferences, !manifest.object_references().is_empty()),
        ];
        for (feature, present) in feature_presence {
            if present {
                let cap = feature_capability(feature);
                if !declared.contains(cap.as_str()) && !contains_cap(&all_missing, cap.as_str()) {
                    all_missing.push(cap);
                }
            }
        }

        // objectReferences → also require external.objects.read
        if !manifest.object_references().is_empty() {
            let cap = PluginCapability::new("external.objects.read");
            if !declared.contains(cap.as_str()) && !contains_cap(&all_missing, cap.as_str()) {
                all_missing.push(cap);
            }
        }

        // UI slots → required capabilities
        for slot_str in manifest.ui_slots() {
            if let Some(cap) = ui_slot_capability(slot_str) {
                if !declared.contains(cap.as_str()) && !contains_cap(&all_missing, cap.as_str()) {
                    all_missing.push(cap);
                }
            }
        }

        // Launchers (top-level + ui.launchers) → required capabilities
        for zone_str in manifest
            .launchers()
            .iter()
            .chain(manifest.ui_launchers().iter())
        {
            if let Some(cap) = launcher_placement_capability(zone_str) {
                if !declared.contains(cap.as_str()) && !contains_cap(&all_missing, cap.as_str()) {
                    all_missing.push(cap);
                }
            }
        }

        let allowed = all_missing.is_empty();
        CapabilityCheckResult {
            allowed,
            missing: all_missing,
            operation: None,
            plugin_id: Some(manifest.id().to_string()),
        }
    }

    fn get_required_capabilities(&self, operation: &str) -> Vec<PluginCapability> {
        operation_capabilities(operation)
    }

    fn get_ui_slot_capability(&self, slot_type: &str) -> Option<PluginCapability> {
        if parse_ui_slot(slot_type).is_none() {
            return None;
        }
        ui_slot_capability(slot_type)
    }
}

// ============================================================================
// Factory + helpers
// ============================================================================

pub fn plugin_capability_validator() -> DefaultPluginCapabilityValidator {
    DefaultPluginCapabilityValidator::new()
}

fn declared_set(manifest: &dyn PluginManifestV1View) -> HashSet<&str> {
    manifest.capabilities().iter().map(String::as_str).collect()
}

fn contains_cap(list: &[PluginCapability], s: &str) -> bool {
    list.iter().any(|c| c.as_str() == s)
}

fn build_forbidden_message(
    manifest: &dyn PluginManifestV1View,
    operation: &str,
    missing: &[PluginCapability],
) -> String {
    let missing_list = missing
        .iter()
        .map(PluginCapability::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Plugin '{}' is not allowed to perform '{}'. Missing required capabilities: {}",
        manifest.id(),
        operation,
        missing_list
    )
}
