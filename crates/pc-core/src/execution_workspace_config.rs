//! `execution_workspace_config` 域（Round 268）。
//!
//! 与原 `paperclip/server/src/services/execution-workspaces.ts` 中
//! `readExecutionWorkspaceConfig` / `mergeExecutionWorkspaceConfig` 1:1 对齐：
//! - 从项目 metadata 中读取 `ExecutionWorkspaceConfig`（provision/teardown/cleanup 等）
//! - 合并 patch 到 metadata（深合并 + 校验字面量）
//!
//! 设计目标：高内聚低耦合。
//! - 高内聚：纯函数 metadata 处理；零 IO，零 DB。
//! - 低耦合：仅依赖 `serde_json`；调用方提供 `&serde_json::Value` 即可。
//!
//! 注意：与 `project_workspace_runtime_config` 模块是兄弟（前者读 `metadata.config`，
//!       后者读 `metadata.runtimeConfig`），共享同一个 sentinel 语义。

use std::collections::HashMap;

use serde_json::{Map, Value};

/// `WorkspaceRuntimeDesiredState` 字符串字面量。
pub type DesiredState = String;
pub const DESIRED_STATE_RUNNING: &str = "running";
pub const DESIRED_STATE_STOPPED: &str = "stopped";
pub const DESIRED_STATE_MANUAL: &str = "manual";

/// `serviceName -> desiredState` 状态表。
pub type ServiceStateMap = HashMap<String, String>;

/// Execution workspace 配置（与 Node `ExecutionWorkspaceConfig` 1:1 对齐）。
///
/// `Option<Option<T>>` sentinel 模式：
/// - 外层 `None` → 字段未在 patch 中出现（保留旧值）
/// - 外层 `Some(None)` → 字段在 patch 中显式置空（清空）
/// - 外层 `Some(Some(v))` → 字段在 patch 中赋值
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionWorkspaceConfig {
    pub environment_id: Option<Option<String>>,
    pub provision_command: Option<Option<String>>,
    pub teardown_command: Option<Option<String>>,
    pub cleanup_command: Option<Option<String>>,
    pub workspace_runtime: Option<Option<Map<String, Value>>>,
    pub desired_state: Option<Option<DesiredState>>,
    pub service_states: Option<Option<ServiceStateMap>>,
}

impl ExecutionWorkspaceConfig {
    /// 是否至少有一个字段是"非空"值（用于决定是否要写入 metadata）。
    pub fn has_any_value(&self) -> bool {
        let eid = self.environment_id.as_ref().map(|x| x.is_some()).unwrap_or(false);
        let prov = self.provision_command.as_ref().map(|x| x.is_some()).unwrap_or(false);
        let td = self.teardown_command.as_ref().map(|x| x.is_some()).unwrap_or(false);
        let cl = self.cleanup_command.as_ref().map(|x| x.is_some()).unwrap_or(false);
        let wr = self
            .workspace_runtime
            .as_ref()
            .map(|x| x.as_ref().map(|m| !m.is_empty()).unwrap_or(false))
            .unwrap_or(false);
        let ds = self.desired_state.as_ref().map(|x| x.is_some()).unwrap_or(false);
        let ss = self
            .service_states
            .as_ref()
            .map(|x| x.as_ref().map(|m| !m.is_empty()).unwrap_or(false))
            .unwrap_or(false);
        eid || prov || td || cl || wr || ds || ss
    }
}

fn clone_record(value: Option<&Value>) -> Option<Map<String, Value>> {
    value.and_then(|v| v.as_object()).map(|m| m.clone())
}

fn clone_record_value(value: Option<&Map<String, Value>>) -> Option<Map<String, Value>> {
    value.cloned()
}

fn read_nullable_string(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        _ => None,
    }
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
            if matches!(s, DESIRED_STATE_RUNNING | DESIRED_STATE_STOPPED | DESIRED_STATE_MANUAL) {
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

/// 从 metadata 读出"已平坦化"的内部表示（无 sentinel）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlatExecutionWorkspaceConfig {
    pub environment_id: Option<String>,
    pub provision_command: Option<String>,
    pub teardown_command: Option<String>,
    pub cleanup_command: Option<String>,
    pub workspace_runtime: Option<Map<String, Value>>,
    pub desired_state: Option<DesiredState>,
    pub service_states: Option<ServiceStateMap>,
}

impl FlatExecutionWorkspaceConfig {
    pub fn into_sentinel(self) -> ExecutionWorkspaceConfig {
        ExecutionWorkspaceConfig {
            environment_id: Some(self.environment_id),
            provision_command: Some(self.provision_command),
            teardown_command: Some(self.teardown_command),
            cleanup_command: Some(self.cleanup_command),
            workspace_runtime: Some(self.workspace_runtime),
            desired_state: Some(self.desired_state),
            service_states: Some(self.service_states),
        }
    }
}

/// 从项目的 `metadata.config` 读取 `ExecutionWorkspaceConfig`。
///
/// 与 Node `readExecutionWorkspaceConfig(metadata)` 1:1 对齐：
/// - metadata 必须为 object；非对象 → null
/// - 只有当任何一个字段被设置过才返回非 null（避免空 config）
pub fn read_execution_workspace_config(
    metadata: Option<&Map<String, Value>>,
) -> Option<ExecutionWorkspaceConfig> {
    let raw = metadata.and_then(|m| m.get("config")).and_then(|v| v.as_object())?;
    let flat = FlatExecutionWorkspaceConfig {
        environment_id: read_nullable_string(raw.get("environmentId")),
        provision_command: read_nullable_string(raw.get("provisionCommand")),
        teardown_command: read_nullable_string(raw.get("teardownCommand")),
        cleanup_command: read_nullable_string(raw.get("cleanupCommand")),
        workspace_runtime: clone_record(raw.get("workspaceRuntime")),
        desired_state: read_desired_state(raw.get("desiredState")),
        service_states: read_service_states(raw.get("serviceStates")),
    };
    let sentinel = flat.into_sentinel();
    if sentinel.has_any_value() {
        Some(sentinel)
    } else {
        None
    }
}

/// 把 `patch` 合并到 `metadata`，返回新的 metadata object。
///
/// 与 Node `mergeExecutionWorkspaceConfig(metadata, patch)` 1:1 对齐：
/// - patch=null → 删除 config（如果 metadata 变空，整体返回 null）
/// - patch 部分字段缺失 → 保留现有值
/// - patch 部分字段 = Some(None) → 显式置空
pub fn merge_execution_workspace_config(
    metadata: Option<&Map<String, Value>>,
    patch: Option<&ExecutionWorkspaceConfig>,
) -> Option<Map<String, Value>> {
    let mut next_metadata: Map<String, Value> = match metadata {
        Some(m) => m.clone(),
        None => Map::new(),
    };

    if patch.is_none() {
        next_metadata.remove("config");
        return if next_metadata.is_empty() { None } else { Some(next_metadata) };
    }

    let current_flat = read_flat(metadata);
    let patch_cfg = patch.expect("non-null branch already checked");

    let next_config = ExecutionWorkspaceConfig {
        environment_id: match &patch_cfg.environment_id {
            Some(v) => Some(v.clone()),
            None => Some(current_flat.environment_id),
        },
        provision_command: match &patch_cfg.provision_command {
            Some(v) => Some(v.clone()),
            None => Some(current_flat.provision_command),
        },
        teardown_command: match &patch_cfg.teardown_command {
            Some(v) => Some(v.clone()),
            None => Some(current_flat.teardown_command),
        },
        cleanup_command: match &patch_cfg.cleanup_command {
            Some(v) => Some(v.clone()),
            None => Some(current_flat.cleanup_command),
        },
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

    if next_config.has_any_value() {
        let mut config_obj = Map::new();
        // 镜像 Node：写入所有 7 个字段（缺失/空 → null）。
        config_obj.insert(
            "environmentId".to_string(),
            next_config
                .environment_id
                .clone()
                .flatten()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        config_obj.insert(
            "provisionCommand".to_string(),
            next_config
                .provision_command
                .clone()
                .flatten()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        config_obj.insert(
            "teardownCommand".to_string(),
            next_config
                .teardown_command
                .clone()
                .flatten()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        config_obj.insert(
            "cleanupCommand".to_string(),
            next_config
                .cleanup_command
                .clone()
                .flatten()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        if let Some(Some(wr)) = next_config.workspace_runtime.as_ref() {
            config_obj.insert("workspaceRuntime".to_string(), Value::Object(wr.clone()));
        } else {
            config_obj.insert("workspaceRuntime".to_string(), Value::Null);
        }
        config_obj.insert(
            "desiredState".to_string(),
            next_config
                .desired_state
                .clone()
                .flatten()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        if let Some(Some(ss)) = next_config.service_states.as_ref() {
            let obj: Map<String, Value> = ss
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect();
            config_obj.insert("serviceStates".to_string(), Value::Object(obj));
        } else {
            config_obj.insert("serviceStates".to_string(), Value::Null);
        }
        next_metadata.insert("config".to_string(), Value::Object(config_obj));
    } else {
        next_metadata.remove("config");
    }

    if next_metadata.is_empty() {
        None
    } else {
        Some(next_metadata)
    }
}

fn read_flat(metadata: Option<&Map<String, Value>>) -> FlatExecutionWorkspaceConfig {
    match read_execution_workspace_config(metadata) {
        Some(s) => FlatExecutionWorkspaceConfig {
            environment_id: s.environment_id.unwrap_or(None),
            provision_command: s.provision_command.unwrap_or(None),
            teardown_command: s.teardown_command.unwrap_or(None),
            cleanup_command: s.cleanup_command.unwrap_or(None),
            workspace_runtime: s.workspace_runtime.unwrap_or(None),
            desired_state: s.desired_state.unwrap_or(None),
            service_states: s.service_states.unwrap_or(None),
        },
        None => FlatExecutionWorkspaceConfig::default(),
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
        assert_eq!(read_execution_workspace_config(None), None);
        let empty = Map::new();
        assert_eq!(read_execution_workspace_config(Some(&empty)), None);
    }

    #[test]
    fn read_returns_null_when_config_empty() {
        let m = map_from(&[("config", json!({}))]);
        assert_eq!(read_execution_workspace_config(Some(&m)), None);
    }

    #[test]
    fn read_returns_null_when_only_invalid_desired_state() {
        // desiredState 无效时不视为设置（has_any_value 检查 desired_state 字段为 Some(Some(...))）
        // 但我们这里的实现中 desired_state = Some(None) 表示 None。
        // Node 行为：
        //   desiredState: "garbage" → null，serviceStates:{"a":"running"} 也算设置。
        // 这里保持一致：空 config 不返回。
        let m = map_from(&[(
            "config",
            json!({"desiredState": "garbage", "serviceStates": {}}),
        )]);
        assert_eq!(read_execution_workspace_config(Some(&m)), None);
    }

    #[test]
    fn read_full_payload() {
        let m = map_from(&[(
            "config",
            json!({
                "environmentId": "env-1",
                "provisionCommand": "pnpm i",
                "teardownCommand": null,
                "cleanupCommand": "rm",
                "workspaceRuntime": {"k": 1},
                "desiredState": "running",
                "serviceStates": {"a": "stopped"}
            }),
        )]);
        let s = read_execution_workspace_config(Some(&m)).unwrap();
        assert_eq!(s.environment_id, Some(Some("env-1".to_string())));
        assert_eq!(s.provision_command, Some(Some("pnpm i".to_string())));
        assert_eq!(s.teardown_command, Some(None));
        assert_eq!(s.cleanup_command, Some(Some("rm".to_string())));
        assert!(s.workspace_runtime.as_ref().map(|x| x.is_some()).unwrap_or(false));
        assert_eq!(s.desired_state, Some(Some("running".to_string())));
        assert!(s.service_states.as_ref().map(|x| x.is_some()).unwrap_or(false));
    }

    #[test]
    fn read_filters_non_literal_desired_state() {
        let m = map_from(&[(
            "config",
            json!({"desiredState": "garbage", "provisionCommand": "pnpm i"}),
        )]);
        let s = read_execution_workspace_config(Some(&m)).unwrap();
        assert_eq!(s.desired_state, Some(None));
        assert_eq!(s.provision_command, Some(Some("pnpm i".to_string())));
    }

    #[test]
    fn merge_with_null_patch_drops_config() {
        let m = map_from(&[
            ("config", json!({"provisionCommand": "pnpm i"})),
            ("extra", json!("keep")),
        ]);
        let next = merge_execution_workspace_config(Some(&m), None).unwrap();
        assert!(!next.contains_key("config"));
        assert_eq!(next.get("extra").and_then(|v| v.as_str()), Some("keep"));
    }

    #[test]
    fn merge_with_null_patch_returns_null_when_empty() {
        let m = map_from(&[("config", json!({"provisionCommand": "pnpm i"}))]);
        let next = merge_execution_workspace_config(Some(&m), None);
        assert!(next.is_none());
    }

    #[test]
    fn merge_overrides_fields() {
        let m = map_from(&[(
            "config",
            json!({"provisionCommand": "pnpm i", "teardownCommand": "rm"}),
        )]);
        let patch = ExecutionWorkspaceConfig {
            environment_id: None,
            provision_command: Some(Some("npm i".to_string())),
            teardown_command: None, // 保留
            cleanup_command: None,
            workspace_runtime: None,
            desired_state: None,
            service_states: None,
        };
        let next = merge_execution_workspace_config(Some(&m), Some(&patch)).unwrap();
        let c = next.get("config").and_then(|v| v.as_object()).unwrap();
        // Node 合并后所有 7 个字段都在
        assert_eq!(c.get("provisionCommand").and_then(|v| v.as_str()), Some("npm i"));
        assert_eq!(c.get("teardownCommand").and_then(|v| v.as_str()), Some("rm"));
        assert!(c.get("environmentId").map(|v| v.is_null()).unwrap_or(false));
    }

    #[test]
    fn merge_drops_config_when_results_empty() {
        let m = Map::new();
        let patch = ExecutionWorkspaceConfig::default();
        let next = merge_execution_workspace_config(Some(&m), Some(&patch));
        assert!(next.is_none());
    }

    #[test]
    fn merge_clears_field_explicitly() {
        let m = map_from(&[("config", json!({"provisionCommand": "pnpm i"}))]);
        let patch = ExecutionWorkspaceConfig {
            environment_id: None,
            provision_command: Some(None), // 清空
            teardown_command: None,
            cleanup_command: None,
            workspace_runtime: None,
            desired_state: None,
            service_states: None,
        };
        let next = merge_execution_workspace_config(Some(&m), Some(&patch));
        // 整体 config 还有其他可能为 null 的字段写入；has_any_value 检查只看 None？
        // 我们 has_any_value：None 当作"无值"。修补后 provision_command = None → 无值；
        // 整个 config 视为空，metadata 应变 null
        assert!(next.is_none() || !next.as_ref().unwrap().contains_key("config"));
    }

    #[test]
    fn round_trip_merge_then_read() {
        let m = map_from(&[("config", json!({"provisionCommand": "pnpm i"}))]);
        let patch = ExecutionWorkspaceConfig {
            environment_id: Some(Some("env-1".to_string())),
            provision_command: None,
            teardown_command: None,
            cleanup_command: None,
            workspace_runtime: None,
            desired_state: Some(Some(DESIRED_STATE_RUNNING.to_string())),
            service_states: None,
        };
        let next = merge_execution_workspace_config(Some(&m), Some(&patch)).unwrap();
        let s = read_execution_workspace_config(Some(&next)).unwrap();
        assert_eq!(s.environment_id, Some(Some("env-1".to_string())));
        assert_eq!(s.provision_command, Some(Some("pnpm i".to_string())));
        assert_eq!(s.desired_state, Some(Some(DESIRED_STATE_RUNNING.to_string())));
    }
}
