//! Recovery model profile hint 注入与 scrub 工具
//!
//! 对齐 Node `services/recovery/model-profile-hint.ts`：
//! - 常量 `RECOVERY_MODEL_PROFILE_KEY = "cheap"`
//! - 常量 `STATUS_ONLY_RECOVERY_GUARD_CONTEXT` —— status_only 模式的 4 个 guard 字段
//! - 常量 `RECOVERY_MODEL_PROFILE_HINT_KEYS` —— 6 个 hint key 列表
//! - 类型 `RecoveryModelProfileWorkClass`
//! - 函数 `scrub_recovery_model_profile_hints(input)` —— 移除所有 hint key
//! - 函数 `with_recovery_model_profile_hint(input, work_class)` —— 按 work class 注入
//! - 函数 `recovery_assignee_adapter_overrides(work_class)` —— 返回 adapter 覆盖
//!
//! 设计：
//! - 用 `serde_json::Value` 而非泛型，因为 Node 端的对象结构是动态的
//! - work_class 决定是否注入 status_only guard 字段 + `modelProfile: "cheap"`
//! - 6 个 hint key 统一管理：`scrub` 和 `with` 都引用同一列表

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ============================================================================
// Constants
// ============================================================================

/// Status-only 恢复模式使用的模型 profile key。
pub const RECOVERY_MODEL_PROFILE_KEY: &str = "cheap";

/// Status-only 模式的 guard context（4 个字段）。
///
/// 通过 OnceLock 延迟构造，因为 Value::String 需要运行时分配 String。
pub fn status_only_recovery_guard_context() -> &'static [(&'static str, Value)] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<(&'static str, Value)>> = OnceLock::new();
    CACHE.get_or_init(|| {
        vec![
            ("recoveryIntent", Value::String("status_only".to_string())),
            ("allowDeliverableWork", Value::Bool(false)),
            ("allowDocumentUpdates", Value::Bool(false)),
            ("resumeRequiresNormalModel", Value::Bool(true)),
        ]
    })
}

/// 所有 hint key 的统一列表（scrub 与 with 共享）。
pub const RECOVERY_MODEL_PROFILE_HINT_KEYS: &[&str] = &[
    "modelProfile",
    "paperclipModelProfile",
    "recoveryIntent",
    "allowDeliverableWork",
    "allowDocumentUpdates",
    "resumeRequiresNormalModel",
];

// ============================================================================
// Types
// ============================================================================

/// 恢复任务的 work class。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryModelProfileWorkClass {
    StatusOnly,
    NormalModel,
}

impl RecoveryModelProfileWorkClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StatusOnly => "status_only",
            Self::NormalModel => "normal_model",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "status_only" => Some(Self::StatusOnly),
            "normal_model" => Some(Self::NormalModel),
            _ => None,
        }
    }
}

/// Adapter 覆盖对象（仅返回 modelProfile 一项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAssigneeAdapterOverrides {
    pub model_profile: String,
}

// ============================================================================
// Public API
// ============================================================================

/// 从对象中移除所有 6 个 hint key，返回新对象（不修改入参）。
///
/// 对齐 Node `scrubRecoveryModelProfileHints`。
pub fn scrub_recovery_model_profile_hints(input: &Map<String, Value>) -> Map<String, Value> {
    let mut output = input.clone();
    for key in RECOVERY_MODEL_PROFILE_HINT_KEYS {
        output.remove(*key);
    }
    output
}

/// 按 work_class 注入 model profile hints：
///
/// - `normal_model` → 等价于 `scrub_recovery_model_profile_hints(input)`
/// - `status_only` → scrub 之后，再注入 STATUS_ONLY_GUARD_CONTEXT 4 个字段 + `modelProfile: "cheap"`
///
/// 对齐 Node `withRecoveryModelProfileHint`。
pub fn with_recovery_model_profile_hint(
    input: &Map<String, Value>,
    work_class: RecoveryModelProfileWorkClass,
) -> Map<String, Value> {
    let mut output = scrub_recovery_model_profile_hints(input);
    if matches!(work_class, RecoveryModelProfileWorkClass::StatusOnly) {
        for (k, v) in status_only_recovery_guard_context() {
            output.insert((*k).to_string(), v.clone());
        }
        output.insert(
            "modelProfile".to_string(),
            Value::String(RECOVERY_MODEL_PROFILE_KEY.to_string()),
        );
    }
    output
}

/// 返回 recovery 模式下 assignee adapter 的覆盖项。
///
/// 对齐 Node `recoveryAssigneeAdapterOverrides`（仅用于 status_only）。
pub fn recovery_assignee_adapter_overrides(
    _work_class: RecoveryModelProfileWorkClass,
) -> RecoveryAssigneeAdapterOverrides {
    // Per Node: this function is only called for status_only, but the impl is identical for both.
    RecoveryAssigneeAdapterOverrides {
        model_profile: RECOVERY_MODEL_PROFILE_KEY.to_string(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty() -> Map<String, Value> {
        Map::new()
    }

    fn sample() -> Map<String, Value> {
        let v = json!({
            "issueId": "is1",
            "taskKey": "tk1",
            "modelProfile": "expensive", // 残留 hint
            "allowDeliverableWork": true, // 残留 hint
        });
        v.as_object().unwrap().clone()
    }

    // -----------------------------------------------------------------------
    // work_class round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn work_class_as_str() {
        assert_eq!(
            RecoveryModelProfileWorkClass::StatusOnly.as_str(),
            "status_only"
        );
        assert_eq!(
            RecoveryModelProfileWorkClass::NormalModel.as_str(),
            "normal_model"
        );
    }

    #[test]
    fn work_class_from_str() {
        assert_eq!(
            RecoveryModelProfileWorkClass::from_str("status_only"),
            Some(RecoveryModelProfileWorkClass::StatusOnly)
        );
        assert_eq!(
            RecoveryModelProfileWorkClass::from_str("normal_model"),
            Some(RecoveryModelProfileWorkClass::NormalModel)
        );
        assert_eq!(RecoveryModelProfileWorkClass::from_str("unknown"), None);
    }

    // -----------------------------------------------------------------------
    // scrub
    // -----------------------------------------------------------------------

    #[test]
    fn scrub_removes_all_hint_keys() {
        let out = scrub_recovery_model_profile_hints(&sample());
        let obj = out;
        assert!(!obj.contains_key("modelProfile"));
        assert!(!obj.contains_key("allowDeliverableWork"));
        // 非 hint key 保留
        assert_eq!(obj.get("issueId"), Some(&json!("is1")));
        assert_eq!(obj.get("taskKey"), Some(&json!("tk1")));
    }

    #[test]
    fn scrub_on_empty_object() {
        let out = scrub_recovery_model_profile_hints(&empty());
        assert!(out.is_empty());
    }

    #[test]
    fn scrub_does_not_mutate_input() {
        let input = sample();
        let _out = scrub_recovery_model_profile_hints(&input);
        // input still has hints
        assert!(input.contains_key("modelProfile"));
    }

    // -----------------------------------------------------------------------
    // with_recovery_model_profile_hint
    // -----------------------------------------------------------------------

    #[test]
    fn with_normal_model_just_scrubs() {
        let out =
            with_recovery_model_profile_hint(&sample(), RecoveryModelProfileWorkClass::NormalModel);
        // 不应注入 modelProfile / guard 字段
        assert!(!out.contains_key("modelProfile"));
        assert!(!out.contains_key("recoveryIntent"));
        assert!(!out.contains_key("allowDeliverableWork"));
        // 应保留 issueId / taskKey
        assert_eq!(out.get("issueId"), Some(&json!("is1")));
    }

    #[test]
    fn with_status_only_injects_guards_and_profile() {
        let out =
            with_recovery_model_profile_hint(&sample(), RecoveryModelProfileWorkClass::StatusOnly);
        // guard context
        assert_eq!(out.get("recoveryIntent"), Some(&json!("status_only")));
        assert_eq!(out.get("allowDeliverableWork"), Some(&json!(false)));
        assert_eq!(out.get("allowDocumentUpdates"), Some(&json!(false)));
        assert_eq!(out.get("resumeRequiresNormalModel"), Some(&json!(true)));
        // model profile
        assert_eq!(out.get("modelProfile"), Some(&json!("cheap")));
        // 原始非 hint 字段保留
        assert_eq!(out.get("issueId"), Some(&json!("is1")));
        assert_eq!(out.get("taskKey"), Some(&json!("tk1")));
    }

    #[test]
    fn with_status_only_overrides_existing_hints() {
        let mut input = sample();
        input.insert("allowDeliverableWork".to_string(), json!(true));
        let out =
            with_recovery_model_profile_hint(&input, RecoveryModelProfileWorkClass::StatusOnly);
        // 被 status_only guard 覆盖为 false
        assert_eq!(out.get("allowDeliverableWork"), Some(&json!(false)));
    }

    #[test]
    fn with_normal_model_clears_existing_status_only_hints() {
        let mut input = empty();
        input.insert("recoveryIntent".to_string(), json!("status_only"));
        input.insert("allowDeliverableWork".to_string(), json!(false));
        let out =
            with_recovery_model_profile_hint(&input, RecoveryModelProfileWorkClass::NormalModel);
        assert!(!out.contains_key("recoveryIntent"));
        assert!(!out.contains_key("allowDeliverableWork"));
    }

    // -----------------------------------------------------------------------
    // recovery_assignee_adapter_overrides
    // -----------------------------------------------------------------------

    #[test]
    fn adapter_overrides_returns_cheap_profile() {
        let ov = recovery_assignee_adapter_overrides(RecoveryModelProfileWorkClass::StatusOnly);
        assert_eq!(ov.model_profile, "cheap");
    }

    #[test]
    fn adapter_overrides_returns_cheap_profile_even_for_normal() {
        // Per Node: this function is only called for status_only, but we accept normal too
        let ov = recovery_assignee_adapter_overrides(RecoveryModelProfileWorkClass::NormalModel);
        assert_eq!(ov.model_profile, "cheap");
    }

    // -----------------------------------------------------------------------
    // Constants consistency
    // -----------------------------------------------------------------------

    #[test]
    fn status_only_guard_context_has_four_keys() {
        assert_eq!(status_only_recovery_guard_context().len(), 4);
    }

    #[test]
    fn hint_keys_include_all_guard_keys() {
        for (k, _) in status_only_recovery_guard_context() {
            assert!(
                RECOVERY_MODEL_PROFILE_HINT_KEYS.contains(k),
                "{k} should be in hint keys"
            );
        }
    }

    #[test]
    fn hint_keys_include_model_profile() {
        assert!(RECOVERY_MODEL_PROFILE_HINT_KEYS.contains(&"modelProfile"));
        assert!(RECOVERY_MODEL_PROFILE_HINT_KEYS.contains(&"paperclipModelProfile"));
    }

    // -----------------------------------------------------------------------
    // Serde round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn work_class_serde_via_value() {
        let v = serde_json::to_value(RecoveryModelProfileWorkClass::StatusOnly).unwrap();
        assert_eq!(v, json!("status_only"));
        let v = serde_json::to_value(RecoveryModelProfileWorkClass::NormalModel).unwrap();
        assert_eq!(v, json!("normal_model"));
    }
}
