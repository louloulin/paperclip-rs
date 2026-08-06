//! `workspace_runtime_service_state` 域（Round 271）。
//!
//! 与原 `paperclip/server/src/services/workspace-runtime.ts` 中 4 个纯函数
//! 1:1 对齐（已剥离 DB / IO 副作用）：
//! - `sanitizeRuntimeServiceBaseEnv` — 移除敏感 env 变量
//! - `readDesiredRuntimeState` — 字符串字面量校验
//! - `readConfiguredServiceStates` — 过滤有效 state 字符串
//! - `buildWorkspaceRuntimeDesiredStatePatch` — start/stop/restart 状态机
//!
//! 设计目标：高内聚低耦合。
//! - **高内聚**：4 个 helper 共同表达"service desired-state 计算"的纯逻辑。
//! - **低耦合**：输入仅是 HashMap 和 Options；不需要 db/http/runtime。
//! - 与 pc-core 内已有的 `execution_workspace_policy::*` 同样策略：
//!   调用方传 typed config，由本模块返回 typed patch。
//!
//! 与 `WorkspaceRuntimeServiceStateMap = Record<string, "running"|"stopped"|"manual">` 对齐。

use std::collections::HashMap;

// ============================================================================
// 类型 + 字符串字面量
// ============================================================================

/// `WorkspaceRuntimeDesiredState` 字符串字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredState {
    Running,
    Stopped,
    Manual,
}

impl DesiredState {
    pub fn as_str(self) -> &'static str {
        match self {
            DesiredState::Running => "running",
            DesiredState::Stopped => "stopped",
            DesiredState::Manual => "manual",
        }
    }

    /// 从任意 `value` 中读取；非合法字面量返回 `None`。
    pub fn from_value(value: &serde_json::Value) -> Option<Self> {
        let s = value.as_str()?;
        match s {
            "running" => Some(Self::Running),
            "stopped" => Some(Self::Stopped),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}

/// `serviceIndex(string) -> desiredState` 状态表（与 Node `WorkspaceRuntimeServiceStateMap` 对齐）。
pub type ServiceStatesMap = HashMap<String, DesiredState>;

/// Start / Stop / Restart 操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesiredStateAction {
    Start,
    Stop,
    Restart,
}

impl DesiredStateAction {
    /// 该 action 对应的目标 desired state（start/restart → running；stop → stopped）。
    pub fn target_state(self) -> DesiredState {
        match self {
            DesiredStateAction::Start | DesiredStateAction::Restart => DesiredState::Running,
            DesiredStateAction::Stop => DesiredState::Stopped,
        }
    }
}

// ============================================================================
// sanitizeRuntimeServiceBaseEnv（Round 271）
// ============================================================================

/// 净化 runtime service 继承的环境变量。
///
/// 与 Node `sanitizeRuntimeServiceBaseEnv(baseEnv)` 1:1 对齐：
/// - 删除所有 `PAPERCLIP_*` 键
/// - 强制删除 `DATABASE_URL`
/// - 删除 `npm_config_tailscale_auth` / `npm_config_authenticated_private`
pub fn sanitize_runtime_service_base_env(
    base_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = base_env.clone();
    let keys: Vec<String> = env
        .keys()
        .filter(|k| k.starts_with("PAPERCLIP_"))
        .cloned()
        .collect();
    for k in keys {
        env.remove(&k);
    }
    env.remove("DATABASE_URL");
    env.remove("npm_config_tailscale_auth");
    env.remove("npm_config_authenticated_private");
    env
}

// ============================================================================
// readDesiredRuntimeState / readConfiguredServiceStates（Round 271）
// ============================================================================

/// 从任意 JSON 中读取 desired_state；非合法字面量返回 None。
pub fn read_desired_runtime_state(value: Option<&serde_json::Value>) -> Option<DesiredState> {
    value.and_then(DesiredState::from_value)
}

/// 从 config.serviceStates 中读出过滤后的 service states 表。
///
/// 与 Node `readConfiguredServiceStates(config)` 1:1 对齐：仅保留合法字面量值。
pub fn read_configured_service_states(
    config: &serde_json::Map<String, serde_json::Value>,
) -> ServiceStatesMap {
    let raw = config.get("serviceStates").and_then(|v| v.as_object());
    let mut out = ServiceStatesMap::new();
    if let Some(obj) = raw {
        for (key, val) in obj {
            if let Some(state) = DesiredState::from_value(val) {
                out.insert(key.clone(), state);
            }
        }
    }
    out
}

// ============================================================================
// buildWorkspaceRuntimeDesiredStatePatch（Round 271）
// ============================================================================

pub struct BuildDesiredStatePatchInput<'a> {
    /// 配置：可读 `serviceStates`、`desiredState`。我们不解析 `workspaceRuntime` 列表
    /// ——调用方用别的方法（service discovery API）拿到 `configured_services` 的索引 0..n-1。
    pub configured_service_count: usize,
    pub current_desired_state: Option<DesiredState>,
    pub current_service_states: Option<&'a ServiceStatesMap>,
    pub action: DesiredStateAction,
    /// 如果指定，仅该索引 service 被目标 action 影响；其他保留。
    pub service_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildDesiredStatePatchOutput {
    pub desired_state: DesiredState,
    pub service_states: Option<ServiceStatesMap>,
}

/// 计算"对一个 run 的 runtime services"执行 start/stop/restart 后的 desired state。
///
/// 与 Node `buildWorkspaceRuntimeDesiredStatePatch(input)` 1:1 对齐：
/// - 遍历所有已配置 service 索引（0..count）
/// - 当前状态：fallback → `readDesiredRuntimeState(currentDesiredState) ?? Stopped`
///   显式索引值 → `current_service_states[key]`
/// - 应用 action：手动（manual）的 service 不被 override
/// - 如果 `serviceIndex` 指定，仅该 index 被应用，否则全部
/// - 整体 `desiredState`：是否存在 running → running；否则 manual → manual；否则 stopped
pub fn build_workspace_runtime_desired_state_patch(
    input: BuildDesiredStatePatchInput<'_>,
) -> BuildDesiredStatePatchOutput {
    let fallback_state = input.current_desired_state.unwrap_or(DesiredState::Stopped);
    let mut next: ServiceStatesMap = ServiceStatesMap::new();

    for index in 0..input.configured_service_count {
        let key = index.to_string();
        let current = input
            .current_service_states
            .and_then(|m| m.get(&key).copied())
            .unwrap_or(fallback_state);
        next.insert(key, current);
    }

    let target = input.action.target_state();
    let apply = |next: &mut ServiceStatesMap, index: usize| {
        let key = index.to_string();
        // 手动 service 跳过（operator-controlled）
        if next.get(&key).copied() == Some(DesiredState::Manual) {
            return;
        }
        next.insert(key, target);
    };

    match input.service_index {
        None | Some(_) if input.configured_service_count == 0 => {}
        Some(idx) if idx < input.configured_service_count => apply(&mut next, idx),
        _ => {}
    }
    if input.service_index.is_none() {
        for index in 0..input.configured_service_count {
            apply(&mut next, index);
        }
    }

    let any_running = next.values().any(|s| *s == DesiredState::Running);
    let any_manual = next.values().any(|s| *s == DesiredState::Manual);
    let desired_state = if any_running {
        DesiredState::Running
    } else if any_manual {
        DesiredState::Manual
    } else {
        DesiredState::Stopped
    };

    BuildDesiredStatePatchOutput {
        desired_state,
        service_states: if next.is_empty() { None } else { Some(next) },
    }
}

// ============================================================================
// 单元测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map_from(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn desired_state_strings_round_trip() {
        assert_eq!(DesiredState::Running.as_str(), "running");
        assert_eq!(DesiredState::Stopped.as_str(), "stopped");
        assert_eq!(DesiredState::Manual.as_str(), "manual");
    }

    #[test]
    fn desired_state_action_target_state() {
        assert_eq!(
            DesiredStateAction::Start.target_state(),
            DesiredState::Running
        );
        assert_eq!(
            DesiredStateAction::Restart.target_state(),
            DesiredState::Running
        );
        assert_eq!(
            DesiredStateAction::Stop.target_state(),
            DesiredState::Stopped
        );
    }

    #[test]
    fn desired_state_from_value_filters_non_literal() {
        assert_eq!(
            DesiredState::from_value(&json!("running")),
            Some(DesiredState::Running)
        );
        assert_eq!(DesiredState::from_value(&json!("RUBBISH")), None);
        assert_eq!(DesiredState::from_value(&json!(123)), None);
        assert_eq!(DesiredState::from_value(&json!(null)), None);
    }

    #[test]
    fn sanitize_strips_paperclip_keys() {
        let env = map_from(&[
            ("PAPERCLIP_HOME", "/x"),
            ("PAPERCLIP_FOO", "bar"),
            ("PATH", "/usr/bin"),
            ("HOME", "/home/u"),
        ]);
        let out = sanitize_runtime_service_base_env(&env);
        assert!(!out.contains_key("PAPERCLIP_HOME"));
        assert!(!out.contains_key("PAPERCLIP_FOO"));
        assert_eq!(out.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(out.get("HOME").map(String::as_str), Some("/home/u"));
    }

    #[test]
    fn sanitize_strips_database_url() {
        let env = map_from(&[("DATABASE_URL", "postgres://x")]);
        let out = sanitize_runtime_service_base_env(&env);
        assert!(!out.contains_key("DATABASE_URL"));
    }

    #[test]
    fn sanitize_strips_npm_tailscale_auth_keys() {
        let env = map_from(&[
            ("npm_config_tailscale_auth", "x"),
            ("npm_config_authenticated_private", "y"),
        ]);
        let out = sanitize_runtime_service_base_env(&env);
        assert!(!out.contains_key("npm_config_tailscale_auth"));
        assert!(!out.contains_key("npm_config_authenticated_private"));
    }

    #[test]
    fn read_desired_runtime_state_filters_invalid() {
        assert_eq!(read_desired_runtime_state(None), None);
        assert_eq!(
            read_desired_runtime_state(Some(&json!("stopped"))),
            Some(DesiredState::Stopped)
        );
        assert_eq!(read_desired_runtime_state(Some(&json!("nope"))), None);
    }

    #[test]
    fn read_configured_service_states_filters_invalid() {
        let cfg = json!({
            "serviceStates": {
                "0": "running",
                "1": "STOPPED", // 大小写敏感：被过滤
                "2": "manual",
                "3": "garbage"
            }
        })
        .as_object()
        .unwrap()
        .clone();
        let states = read_configured_service_states(&cfg);
        assert_eq!(states.len(), 2);
        assert_eq!(states.get("0").copied(), Some(DesiredState::Running));
        assert_eq!(states.get("2").copied(), Some(DesiredState::Manual));
        assert!(!states.contains_key("1"));
        assert!(!states.contains_key("3"));
    }

    #[test]
    fn build_patch_no_services_returns_stopped() {
        let out = build_workspace_runtime_desired_state_patch(BuildDesiredStatePatchInput {
            configured_service_count: 0,
            current_desired_state: None,
            current_service_states: None,
            action: DesiredStateAction::Start,
            service_index: None,
        });
        assert_eq!(out.desired_state, DesiredState::Stopped);
        assert!(out.service_states.is_none());
    }

    #[test]
    fn build_patch_start_all() {
        let mut cur = ServiceStatesMap::new();
        cur.insert("0".into(), DesiredState::Stopped);
        cur.insert("1".into(), DesiredState::Stopped);
        let out = build_workspace_runtime_desired_state_patch(BuildDesiredStatePatchInput {
            configured_service_count: 2,
            current_desired_state: Some(DesiredState::Stopped),
            current_service_states: Some(&cur),
            action: DesiredStateAction::Start,
            service_index: None,
        });
        assert_eq!(out.desired_state, DesiredState::Running);
        let s = out.service_states.unwrap();
        assert_eq!(s.get("0").copied(), Some(DesiredState::Running));
        assert_eq!(s.get("1").copied(), Some(DesiredState::Running));
    }

    #[test]
    fn build_patch_stop_all_keeps_manual() {
        let mut cur = ServiceStatesMap::new();
        cur.insert("0".into(), DesiredState::Running);
        cur.insert("1".into(), DesiredState::Manual);
        let out = build_workspace_runtime_desired_state_patch(BuildDesiredStatePatchInput {
            configured_service_count: 2,
            current_desired_state: Some(DesiredState::Stopped),
            current_service_states: Some(&cur),
            action: DesiredStateAction::Stop,
            service_index: None,
        });
        let s = out.service_states.unwrap();
        assert_eq!(s.get("0").copied(), Some(DesiredState::Stopped));
        // manual 保留
        assert_eq!(s.get("1").copied(), Some(DesiredState::Manual));
        // 整体：存在 manual → manual
        assert_eq!(out.desired_state, DesiredState::Manual);
    }

    #[test]
    fn build_patch_only_target_index_changed() {
        let mut cur = ServiceStatesMap::new();
        cur.insert("0".into(), DesiredState::Running);
        cur.insert("1".into(), DesiredState::Running);
        let out = build_workspace_runtime_desired_state_patch(BuildDesiredStatePatchInput {
            configured_service_count: 2,
            current_desired_state: Some(DesiredState::Stopped),
            current_service_states: Some(&cur),
            action: DesiredStateAction::Stop,
            service_index: Some(0),
        });
        let s = out.service_states.unwrap();
        assert_eq!(s.get("0").copied(), Some(DesiredState::Stopped));
        // 其他 service 不变
        assert_eq!(s.get("1").copied(), Some(DesiredState::Running));
    }

    #[test]
    fn build_patch_target_index_out_of_range_keeps_all() {
        let mut cur = ServiceStatesMap::new();
        cur.insert("0".into(), DesiredState::Running);
        let out = build_workspace_runtime_desired_state_patch(BuildDesiredStatePatchInput {
            configured_service_count: 1,
            current_desired_state: Some(DesiredState::Running),
            current_service_states: Some(&cur),
            action: DesiredStateAction::Stop,
            service_index: Some(99),
        });
        let s = out.service_states.unwrap();
        // 越界：不应用
        assert_eq!(s.get("0").copied(), Some(DesiredState::Running));
    }

    #[test]
    fn build_patch_target_index_for_manual_service_is_skipped() {
        let mut cur = ServiceStatesMap::new();
        cur.insert("0".into(), DesiredState::Manual);
        let out = build_workspace_runtime_desired_state_patch(BuildDesiredStatePatchInput {
            configured_service_count: 1,
            current_desired_state: Some(DesiredState::Stopped),
            current_service_states: Some(&cur),
            action: DesiredStateAction::Start,
            service_index: Some(0),
        });
        let s = out.service_states.unwrap();
        // manual 即使 explicit 也不 override
        assert_eq!(s.get("0").copied(), Some(DesiredState::Manual));
    }

    #[test]
    fn build_patch_fallback_state_used_for_missing_service_states() {
        // 没有 current_service_states：所有 service 用 fallback = currentDesiredState(Stopped)
        let out = build_workspace_runtime_desired_state_patch(BuildDesiredStatePatchInput {
            configured_service_count: 3,
            current_desired_state: Some(DesiredState::Stopped),
            current_service_states: None,
            action: DesiredStateAction::Start,
            service_index: None,
        });
        let s = out.service_states.unwrap();
        assert_eq!(s.get("0").copied(), Some(DesiredState::Running));
        assert_eq!(s.get("1").copied(), Some(DesiredState::Running));
        assert_eq!(s.get("2").copied(), Some(DesiredState::Running));
        assert_eq!(out.desired_state, DesiredState::Running);
    }
}
