//! `project_workspace_runtime_config` 域（Round 267）。
//!
//! 与原 `paperclip/server/src/services/project-workspace-runtime-config.ts` 1:1 对齐：
//! - 从项目 metadata 中读取 `ProjectWorkspaceRuntimeConfig`（含 desiredState/serviceStates）
//! - 合并 patch 到 metadata（深合并 + 校验字面量）
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：纯函数 metadata 处理；零 IO，零 DB。
//! - 低耦合：仅依赖 serde_json；调用方提供 `&serde_json::Value` 即可。

use std::collections::HashMap;

use serde_json::{Map, Value};

/// `WorkspaceRuntimeDesiredState` 字符串字面量（与 Node union 1:1 对齐）。
pub type DesiredState = String;
pub const DESIRED_STATE_RUNNING: &str = "running";
pub const DESIRED_STATE_STOPPED: &str = "stopped";
pub const DESIRED_STATE_MANUAL: &str = "manual";

/// `WorkspaceRuntimeServiceStateMap`：serviceName → desiredState。
pub type ServiceStateMap = HashMap<String, String>;

/// 项目级 workspace runtime 配置（与 Node `ProjectWorkspaceRuntimeConfig` 1:1 对齐）。
///
/// `Option<Option<T>>` 用于区分：
/// - 外层 `None`  → patch 中字段**未设置**（沿用当前值）
/// - 外层 `Some(None)` → patch 中字段设置为 "清空"
/// - 外层 `Some(Some(v))` → patch 中字段设置为 `v`
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectWorkspaceRuntimeConfig {
    pub workspace_runtime: Option<Option<Map<String, Value>>>,
    pub desired_state: Option<Option<DesiredState>>,
    pub service_states: Option<Option<ServiceStateMap>>,
}

impl ProjectWorkspaceRuntimeConfig {
    pub fn is_empty(&self) -> bool {
        // 用于判断"清理所有字段后为空"的情况
        let wr = self
            .workspace_runtime
            .as_ref()
            .map(|x| x.is_none())
            .unwrap_or(true);
        let ds = self
            .desired_state
            .as_ref()
            .map(|x| x.is_none())
            .unwrap_or(true);
        let ss = self
            .service_states
            .as_ref()
            .map(|x| x.is_none())
            .unwrap_or(true);
        wr && ds && ss
    }
}

/// 从项目的 `metadata` JSON object 中读取 `runtimeConfig` 子对象，归一化为强类型 config。
///
/// 与 Node `readProjectWorkspaceRuntimeConfig(metadata)` 1:1 对齐：
/// - metadata 必须为 object；非对象 → null
/// - 只有当任何一个字段被设置过才返回非 null（避免空对象）
/// 从 metadata 读取的"已平坦化"配置（无 sentinel）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlatProjectWorkspaceRuntimeConfig {
    pub workspace_runtime: Option<Map<String, Value>>,
    pub desired_state: Option<DesiredState>,
    pub service_states: Option<ServiceStateMap>,
}

impl FlatProjectWorkspaceRuntimeConfig {
    pub fn is_empty(&self) -> bool {
        self.workspace_runtime.is_none()
            && self.desired_state.is_none()
            && self.service_states.is_none()
    }
}

impl From<FlatProjectWorkspaceRuntimeConfig> for ProjectWorkspaceRuntimeConfig {
    fn from(f: FlatProjectWorkspaceRuntimeConfig) -> Self {
        Self {
            workspace_runtime: Some(f.workspace_runtime),
            desired_state: Some(f.desired_state),
            service_states: Some(f.service_states),
        }
    }
}

pub fn read_project_workspace_runtime_config(
    metadata: Option<&Map<String, Value>>,
) -> Option<ProjectWorkspaceRuntimeConfig> {
    let raw = metadata
        .and_then(|m| m.get("runtimeConfig"))
        .and_then(|v| v.as_object())?;
    let flat = FlatProjectWorkspaceRuntimeConfig {
        workspace_runtime: clone_record(raw.get("workspaceRuntime")),
        desired_state: read_desired_state(raw.get("desiredState")),
        service_states: read_service_states(raw.get("serviceStates")),
    };
    if flat.is_empty() {
        None
    } else {
        Some(ProjectWorkspaceRuntimeConfig::from(flat))
    }
}

/// 读取"已存在"配置的内部使用版本（无 sentinel）。
pub(crate) fn read_flat(
    metadata: Option<&Map<String, Value>>,
) -> FlatProjectWorkspaceRuntimeConfig {
    match read_project_workspace_runtime_config(metadata) {
        Some(cfg) => FlatProjectWorkspaceRuntimeConfig {
            workspace_runtime: cfg.workspace_runtime.unwrap_or(None),
            desired_state: cfg.desired_state.unwrap_or(None),
            service_states: cfg.service_states.unwrap_or(None),
        },
        None => FlatProjectWorkspaceRuntimeConfig::default(),
    }
}

/// 把 `patch` 合并到 `metadata`，返回新的 metadata object。删除空 `runtimeConfig`。
///
/// 与 Node `mergeProjectWorkspaceRuntimeConfig(metadata, patch)` 1:1 对齐：
/// - patch=null → 删除 runtimeConfig（如果 metadata 变空，整体返回 null）
/// - patch 部分字段缺失 → 保留现有值
/// - patch 部分字段 = None → 视为 "不改"（不要用 None 表示删除）
pub fn merge_project_workspace_runtime_config(
    metadata: Option<&Map<String, Value>>,
    patch: Option<&ProjectWorkspaceRuntimeConfig>,
) -> Option<Map<String, Value>> {
    let mut next_metadata: Map<String, Value> = match metadata {
        Some(m) => m.clone(),
        None => Map::new(),
    };

    if patch.is_none() {
        next_metadata.remove("runtimeConfig");
        return if next_metadata.is_empty() {
            None
        } else {
            Some(next_metadata)
        };
    }

    let current = read_project_workspace_runtime_config(metadata);
    let current_flat = read_flat(metadata);
    let _ = current;

    let patch_cfg = patch.expect("non-null branch already checked");
    // patch_cfg.* 是 sentinel: Option<Option<T>>。
    // Some(v) 表示用户显式设置字段（值 = v）；None 表示字段未变（保留 current_flat）。
    let next_config = ProjectWorkspaceRuntimeConfig {
        workspace_runtime: match &patch_cfg.workspace_runtime {
            Some(v) => Some(v.clone()),
            None => Some(current_flat.workspace_runtime),
        },
        desired_state: match &patch_cfg.desired_state {
            Some(v) => Some(v.clone()),
            None => Some(current_flat.desired_state),
        },
        service_states: match &patch_cfg.service_states {
            Some(v) => Some(v.clone()),
            None => Some(current_flat.service_states),
        },
    };

    if next_config.is_empty() {
        next_metadata.remove("runtimeConfig");
    } else {
        let mut runtime_config_obj = Map::new();
        if let Some(Some(wr)) = next_config.workspace_runtime {
            runtime_config_obj.insert("workspaceRuntime".to_string(), Value::Object(wr));
        }
        if let Some(Some(ds)) = next_config.desired_state {
            runtime_config_obj.insert("desiredState".to_string(), Value::String(ds));
        }
        if let Some(Some(ss)) = next_config.service_states {
            let obj: Map<String, Value> =
                ss.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
            runtime_config_obj.insert("serviceStates".to_string(), Value::Object(obj));
        }
        next_metadata.insert(
            "runtimeConfig".to_string(),
            Value::Object(runtime_config_obj),
        );
    }

    if next_metadata.is_empty() {
        None
    } else {
        Some(next_metadata)
    }
}

fn clone_record(value: Option<&Value>) -> Option<Map<String, Value>> {
    value.and_then(|v| v.as_object()).map(|m| m.clone())
}

fn clone_record_value(value: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    value.cloned()
}

fn read_desired_state(value: Option<&Value>) -> Option<DesiredState> {
    let s = value.and_then(|v| v.as_str())?;
    match s {
        DESIRED_STATE_RUNNING | DESIRED_STATE_STOPPED | DESIRED_STATE_MANUAL => Some(s.to_string()),
        _ => None,
    }
}

fn read_service_states(value: Option<&Value>) -> Option<ServiceStateMap> {
    let obj = value.and_then(|v| v.as_object())?;
    let mut out = ServiceStateMap::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            if matches!(
                s,
                DESIRED_STATE_RUNNING | DESIRED_STATE_STOPPED | DESIRED_STATE_MANUAL
            ) {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map_from(entries: &[(&str, Value)]) -> Map<String, Value> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn read_returns_null_when_metadata_missing() {
        assert_eq!(read_project_workspace_runtime_config(None), None);
        let empty = Map::new();
        assert_eq!(read_project_workspace_runtime_config(Some(&empty)), None);
    }

    #[test]
    fn read_filters_non_literal_desired_state() {
        let m = map_from(&[(
            "runtimeConfig",
            json!({"desiredState": "garbage", "serviceStates": {"a": "running"}}),
        )]);
        let cfg = read_project_workspace_runtime_config(Some(&m)).unwrap();
        // Node: 不可识别的 desiredState → null，但 cfg 仍存在 (因为 serviceStates)
        // 我们 cfg.desired_state 是 Some(None)（字段在 metadata 中但被规范化为 None）
        assert_eq!(cfg.desired_state, Some(None));
        assert!(cfg.service_states.is_some());
    }

    #[test]
    fn read_filters_non_literal_service_states() {
        let m = map_from(&[(
            "runtimeConfig",
            json!({"serviceStates": {"good": "running", "bad": "garbage"}}),
        )]);
        let cfg = read_project_workspace_runtime_config(Some(&m)).unwrap();
        let states = cfg.service_states.unwrap().unwrap();
        assert_eq!(states.len(), 1);
        assert_eq!(states.get("good").map(String::as_str), Some("running"));
        assert!(!states.contains_key("bad"));
    }

    #[test]
    fn read_returns_null_when_runtime_config_empty() {
        let m = map_from(&[("runtimeConfig", json!({}))]);
        assert_eq!(read_project_workspace_runtime_config(Some(&m)), None);
    }

    #[test]
    fn read_full_payload_roundtrip() {
        let m = map_from(&[(
            "runtimeConfig",
            json!({
                "workspaceRuntime": {"k": 1},
                "desiredState": "running",
                "serviceStates": {"a": "stopped", "b": "manual"}
            }),
        )]);
        let cfg = read_project_workspace_runtime_config(Some(&m)).unwrap();
        assert!(cfg.workspace_runtime.is_some());
        assert_eq!(
            cfg.desired_state.as_ref().map(|x| x.as_deref()),
            Some(Some("running"))
        );
        assert_eq!(
            cfg.service_states
                .as_ref()
                .and_then(|x| x.as_ref())
                .map(|m| m.len()),
            Some(2)
        );
    }

    #[test]
    fn merge_with_null_patch_drops_runtime_config() {
        let m = map_from(&[
            ("runtimeConfig", json!({"desiredState": "running"})),
            ("extra", json!("keep")),
        ]);
        let next = merge_project_workspace_runtime_config(Some(&m), None).unwrap();
        assert!(!next.contains_key("runtimeConfig"));
        assert_eq!(next.get("extra").and_then(|v| v.as_str()), Some("keep"));
    }

    #[test]
    fn merge_with_null_patch_returns_null_when_empty() {
        let m = map_from(&[("runtimeConfig", json!({"desiredState": "running"}))]);
        let next = merge_project_workspace_runtime_config(Some(&m), None);
        assert!(next.is_none());
    }

    #[test]
    fn merge_overrides_fields() {
        let m = map_from(&[(
            "runtimeConfig",
            json!({"desiredState": "stopped", "workspaceRuntime": {"old": 1}}),
        )]);
        let patch = ProjectWorkspaceRuntimeConfig {
            workspace_runtime: None,
            desired_state: Some(Some(DESIRED_STATE_RUNNING.to_string())),
            service_states: None,
        };
        let next = merge_project_workspace_runtime_config(Some(&m), Some(&patch)).unwrap();
        let rc = next
            .get("runtimeConfig")
            .and_then(|v| v.as_object())
            .unwrap();
        assert_eq!(
            rc.get("desiredState").and_then(|v| v.as_str()),
            Some("running")
        );
        // workspaceRuntime 保留旧值（因为 patch.workspace_runtime == None 表示不改）
        assert!(rc.get("workspaceRuntime").is_some());
    }

    #[test]
    fn merge_drops_runtime_config_when_results_empty() {
        let m = Map::new();
        let patch = ProjectWorkspaceRuntimeConfig {
            workspace_runtime: None,
            desired_state: None,
            service_states: None,
        };
        let next = merge_project_workspace_runtime_config(Some(&m), Some(&patch));
        assert!(next.is_none());
    }
}
