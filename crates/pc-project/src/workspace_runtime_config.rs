#![forbid(unsafe_code)]
//! 从 project metadata 读取 / 合并 `ProjectWorkspaceRuntimeConfig`（原 `pc-project-workspace-runtime-config` 已下沉）。
//! `ProjectWorkspaceRuntimeConfig`。
//!
//! 对应 Node `server/src/services/project-workspace-runtime-config.ts`（72 行）。
//!
//! 设计目标：1:1 复刻
//! - `readProjectWorkspaceRuntimeConfig(metadata)` —— 从 `metadata.runtimeConfig`
//!   读取 3 个字段：`workspaceRuntime`、`desiredState`、`serviceStates`
//! - `mergeProjectWorkspaceRuntimeConfig(metadata, patch)` —— 合并现有 metadata
//!   与 patch；当 3 个字段全为 null 时从 metadata 中删除 `runtimeConfig`
//!
//! 关键点：
//! - `desiredState` / `serviceStates` 的枚举值是 `running` | `stopped` | `manual`
//! - `workspaceRuntime` 是任意 Record<string, unknown>
//! - `merge` 中 patch === `Some(null)` → 删除 runtimeConfig
//! - `metadata === null` → 当作 `{}`

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// desiredState / service state 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeState {
    Running,
    Stopped,
    Manual,
}

impl RuntimeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Manual => "manual",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// ProjectWorkspaceRuntimeConfig —— 与 Node 1:1 对齐。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkspaceRuntimeConfig {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace_runtime: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub desired_state: Option<RuntimeState>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub service_states: Option<BTreeMap<String, RuntimeState>>,
}

fn is_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

fn clone_record(value: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    value.and_then(|v| if is_record(v) { Some(v.clone()) } else { None })
}

fn read_desired_state(value: Option<&serde_json::Value>) -> Option<RuntimeState> {
    let s = value?.as_str()?;
    RuntimeState::from_str(s)
}

fn read_service_states(value: Option<&serde_json::Value>) -> Option<BTreeMap<String, RuntimeState>> {
    let obj = value?.as_object()?;
    let mut result = BTreeMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            if let Some(state) = RuntimeState::from_str(s) {
                result.insert(k.clone(), state);
            }
        }
    }
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// 从 metadata 中读取 ProjectWorkspaceRuntimeConfig。
///
/// 与 Node `readProjectWorkspaceRuntimeConfig` 1:1 对齐。
pub fn read_project_workspace_runtime_config(
    metadata: Option<&serde_json::Value>,
) -> Option<ProjectWorkspaceRuntimeConfig> {
    let raw = metadata
        .and_then(|m| m.as_object())
        .and_then(|obj| obj.get("runtimeConfig"));
    let raw = raw?;

    let config = ProjectWorkspaceRuntimeConfig {
        workspace_runtime: clone_record(raw.get("workspaceRuntime")),
        desired_state: read_desired_state(raw.get("desiredState")),
        service_states: read_service_states(raw.get("serviceStates")),
    };

    let has_any = config.workspace_runtime.is_some()
        || config.desired_state.is_some()
        || config.service_states.is_some();
    if has_any {
        Some(config)
    } else {
        None
    }
}

/// 合并 metadata 与 patch，返回新的 metadata。
///
/// 与 Node `mergeProjectWorkspaceRuntimeConfig` 1:1 对齐：
/// - `patch = None` → 删除 `runtimeConfig` 字段
/// - 否则按字段合并：patch 中存在的字段用 patch 值，否则保留 current
pub fn merge_project_workspace_runtime_config(
    metadata: Option<&serde_json::Value>,
    patch: Option<&ProjectWorkspaceRuntimeConfig>,
) -> Option<serde_json::Value> {
    let mut next_metadata = metadata
        .and_then(|m| if is_record(m) { Some(m.clone()) } else { None })
        .unwrap_or_else(|| serde_json::json!({}));

    let current: ProjectWorkspaceRuntimeConfig =
        read_project_workspace_runtime_config(Some(&next_metadata)).unwrap_or_default();

    if patch.is_none() {
        if let Some(obj) = next_metadata.as_object_mut() {
            obj.remove("runtimeConfig");
        }
        if next_metadata.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            return None;
        }
        return Some(next_metadata);
    }

    let patch = patch.unwrap();
    let next_config = ProjectWorkspaceRuntimeConfig {
        workspace_runtime: if patch.workspace_runtime.is_some() {
            clone_record(patch.workspace_runtime.as_ref())
        } else {
            current.workspace_runtime
        },
        desired_state: if patch.desired_state.is_some() {
            patch.desired_state
        } else {
            current.desired_state
        },
        service_states: if patch.service_states.is_some() {
            patch.service_states.clone()
        } else {
            current.service_states
        },
    };

    let next_has_any = next_config.workspace_runtime.is_some()
        || next_config.desired_state.is_some()
        || next_config.service_states.is_some();

    if next_has_any {
        if let Some(obj) = next_metadata.as_object_mut() {
            obj.insert(
                "runtimeConfig".to_string(),
                serde_json::to_value(&next_config).unwrap(),
            );
        }
        Some(next_metadata)
    } else {
        if let Some(obj) = next_metadata.as_object_mut() {
            obj.remove("runtimeConfig");
        }
        if next_metadata.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            None
        } else {
            Some(next_metadata)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn r706_read_null_metadata_returns_null() {
        assert!(read_project_workspace_runtime_config(None).is_none());
    }

    #[test]
    fn r706_read_non_object_metadata_returns_null() {
        assert!(read_project_workspace_runtime_config(Some(&json!("string"))).is_none());
        assert!(read_project_workspace_runtime_config(Some(&json!(42))).is_none());
    }

    #[test]
    fn r706_read_metadata_without_runtime_config_returns_null() {
        assert!(
            read_project_workspace_runtime_config(Some(&json!({"foo": "bar"}))).is_none()
        );
    }

    #[test]
    fn r706_read_runtime_config_with_only_empty_returns_null() {
        // runtimeConfig 存在但三个字段都是非合法值 → 返回 None
        let m = json!({"runtimeConfig": {"foo": "bar"}});
        assert!(read_project_workspace_runtime_config(Some(&m)).is_none());
    }

    #[test]
    fn r706_read_desired_state_running() {
        let m = json!({"runtimeConfig": {"desiredState": "running"}});
        let c = read_project_workspace_runtime_config(Some(&m)).unwrap();
        assert_eq!(c.desired_state, Some(RuntimeState::Running));
        assert!(c.workspace_runtime.is_none());
        assert!(c.service_states.is_none());
    }

    #[test]
    fn r706_read_desired_state_invalid_returns_none() {
        let m = json!({"runtimeConfig": {"desiredState": "invalid"}});
        // 所有 3 字段都是 None → 返回 None（与 Node 一致）
        assert!(read_project_workspace_runtime_config(Some(&m)).is_none());
    }

    #[test]
    fn r706_read_workspace_runtime_record() {
        let m = json!({"runtimeConfig": {"workspaceRuntime": {"foo": "bar"}}});
        let c = read_project_workspace_runtime_config(Some(&m)).unwrap();
        assert_eq!(c.workspace_runtime.as_ref().unwrap()["foo"], "bar");
    }

    #[test]
    fn r706_read_workspace_runtime_non_object_returns_none() {
        let m = json!({"runtimeConfig": {"workspaceRuntime": "string"}});
        assert!(read_project_workspace_runtime_config(Some(&m)).is_none());
    }

    #[test]
    fn r706_read_service_states_filters_invalid() {
        let m = json!({
            "runtimeConfig": {
                "serviceStates": {
                    "svc1": "running",
                    "svc2": "invalid",
                    "svc3": "stopped"
                }
            }
        });
        let c = read_project_workspace_runtime_config(Some(&m)).unwrap();
        let ss = c.service_states.unwrap();
        assert_eq!(ss.len(), 2);
        assert_eq!(ss["svc1"], RuntimeState::Running);
        assert_eq!(ss["svc3"], RuntimeState::Stopped);
    }

    #[test]
    fn r706_read_service_states_all_invalid_returns_null() {
        let m = json!({
            "runtimeConfig": {
                "serviceStates": {"svc1": "invalid", "svc2": 42}
            }
        });
        assert!(read_project_workspace_runtime_config(Some(&m)).is_none());
    }

    #[test]
    fn r706_read_full_config() {
        let m = json!({
            "runtimeConfig": {
                "workspaceRuntime": {"k": "v"},
                "desiredState": "manual",
                "serviceStates": {"svc1": "running"}
            }
        });
        let c = read_project_workspace_runtime_config(Some(&m)).unwrap();
        assert_eq!(c.workspace_runtime.as_ref().unwrap()["k"], "v");
        assert_eq!(c.desired_state, Some(RuntimeState::Manual));
        assert_eq!(
            c.service_states.as_ref().unwrap()["svc1"],
            RuntimeState::Running
        );
    }

    #[test]
    fn r706_merge_with_null_patch_removes_runtime_config() {
        let m = json!({
            "runtimeConfig": {"desiredState": "running"},
            "other": "value"
        });
        let result = merge_project_workspace_runtime_config(Some(&m), None).unwrap();
        assert!(result.get("runtimeConfig").is_none());
        assert_eq!(result["other"], "value");
    }

    #[test]
    fn r706_merge_with_null_patch_clears_empty_metadata() {
        let m = json!({"runtimeConfig": {"desiredState": "running"}});
        assert!(merge_project_workspace_runtime_config(Some(&m), None).is_none());
    }

    #[test]
    fn r706_merge_preserves_other_fields() {
        let m = json!({
            "runtimeConfig": {"desiredState": "running"},
            "unrelated": "value"
        });
        let patch = ProjectWorkspaceRuntimeConfig {
            workspace_runtime: None,
            desired_state: Some(RuntimeState::Stopped),
            service_states: None,
        };
        let result = merge_project_workspace_runtime_config(Some(&m), Some(&patch)).unwrap();
        assert_eq!(result["runtimeConfig"]["desiredState"], "stopped");
        assert_eq!(result["unrelated"], "value");
    }

    #[test]
    fn r706_merge_with_null_metadata() {
        let patch = ProjectWorkspaceRuntimeConfig {
            workspace_runtime: Some(json!({"k": "v"})),
            desired_state: Some(RuntimeState::Running),
            service_states: None,
        };
        let result = merge_project_workspace_runtime_config(None, Some(&patch)).unwrap();
        assert_eq!(result["runtimeConfig"]["desiredState"], "running");
        assert_eq!(result["runtimeConfig"]["workspaceRuntime"]["k"], "v");
    }

    #[test]
    fn r706_merge_patch_overrides_current() {
        let m = json!({
            "runtimeConfig": {
                "desiredState": "running",
                "serviceStates": {"svc1": "running"}
            }
        });
        let patch = ProjectWorkspaceRuntimeConfig {
            workspace_runtime: None,
            desired_state: Some(RuntimeState::Stopped),
            service_states: None,
        };
        let result = merge_project_workspace_runtime_config(Some(&m), Some(&patch)).unwrap();
        assert_eq!(result["runtimeConfig"]["desiredState"], "stopped");
        // serviceStates 保留原值
        assert_eq!(result["runtimeConfig"]["serviceStates"]["svc1"], "running");
    }

    #[test]
    fn r706_merge_all_clear_removes_field() {
        let m = json!({"runtimeConfig": {"desiredState": "running"}});
        let patch = ProjectWorkspaceRuntimeConfig::default();
        let result = merge_project_workspace_runtime_config(Some(&m), Some(&patch));
        // patch 全 null + current 是 desiredState=running → 合并后还是 running 保留
        assert_eq!(result.unwrap()["runtimeConfig"]["desiredState"], "running");
    }

    #[test]
    fn r706_runtime_state_round_trip() {
        for s in [RuntimeState::Running, RuntimeState::Stopped, RuntimeState::Manual] {
            assert_eq!(RuntimeState::from_str(s.as_str()), Some(s));
        }
        assert_eq!(RuntimeState::from_str("unknown"), None);
    }
}
