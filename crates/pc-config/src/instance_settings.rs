//! Paperclip 实例 settings 纯函数（pure helpers）。
//!
//! 1:1 对齐 Node `server/src/services/instance-settings.ts`（438 行, R752）。
//! 仅 port 纯函数部分；DB-touching 的 `instanceSettingsService` /
//! `resolveWorktreeRunExecutionActivationState` 不在本模块。
//!
//! 端口语义严格匹配：相同的入参/出参形状，相同的真值集合，相同的
//! server-managed 字段在 patch 中剥离后合并。`serde_json::Value` 作为
//! 边界类型以便与上层 DB / HTTP 层互操作。

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

// =========================================================================
// Constants
// =========================================================================

/// 反馈数据分享偏好默认值（对齐 Node `DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE`）。
pub const DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE: &str = "anonymous";

/// 备份保留默认值（对齐 Node `DEFAULT_BACKUP_RETENTION`）。
pub const DEFAULT_BACKUP_RETENTION: &str = "30d";

/// Issue graph liveness 自动恢复回溯小时数默认值
/// （对齐 Node `DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS`）。
pub const DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS: u32 = 24;

/// Cloud managed-config 的 `managedBy` 元数据值
/// （对齐 Node `PAPERCLIP_CLOUD_MANAGED_BY`）。
pub const PAPERCLIP_CLOUD_MANAGED_BY: &str = "paperclip-cloud";

/// Truthy runtime env values（对齐 Node `TRUTHY_RUNTIME_ENV_VALUES`）。
const TRUTHY_RUNTIME_ENV_VALUES: &[&str] = &["1", "true", "yes", "on"];

// =========================================================================
// Types
// =========================================================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeRunExecutionSuppressedReason {
    NotWorktreeRuntime,
    FlagDisabled,
    MissingCutoff,
    MissingInstanceId,
    InstanceIdMismatch,
    SettingsReadError,
}

/// 对齐 Node `WorktreeRunExecutionActivationState` discriminated union。
/// `Armed` 对应 `{armed: true, cutoff, activationInstanceId, reason: null}`，
/// `Suppressed` 对应 `{armed: false, cutoff: null, activationInstanceId, reason}`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "armed", rename_all = "camelCase")]
pub enum WorktreeRunExecutionActivationState {
    #[serde(rename = "true")]
    Armed {
        cutoff: String,
        activation_instance_id: String,
        #[serde(skip_deserializing)]
        reason: Option<()>,
    },
    #[serde(rename = "false")]
    Suppressed {
        cutoff: Option<String>,
        activation_instance_id: Option<String>,
        reason: WorktreeRunExecutionSuppressedReason,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstanceGeneralSettings {
    #[serde(default)]
    pub censor_username_in_logs: bool,
    #[serde(default)]
    pub keyboard_shortcuts: bool,
    #[serde(default = "default_feedback_pref")]
    pub feedback_data_sharing_preference: String,
    #[serde(default = "default_backup_retention")]
    pub backup_retention: String,
    /// Absent => unrestricted; only carry through an explicit policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_mode: Option<String>,
}

fn default_feedback_pref() -> String {
    DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE.to_string()
}

fn default_backup_retention() -> String {
    DEFAULT_BACKUP_RETENTION.to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstanceExperimentalSettings {
    #[serde(default)]
    pub enable_environments: bool,
    #[serde(default)]
    pub enable_isolated_workspaces: bool,
    #[serde(default = "default_true")]
    pub enable_streamlined_left_navigation: bool,
    #[serde(default)]
    pub enable_apps: bool,
    #[serde(default)]
    pub enable_pipelines: bool,
    #[serde(default)]
    pub enable_cases: bool,
    #[serde(default)]
    pub enable_conference_room_chat: bool,
    #[serde(default)]
    pub enable_task_chat_redesign: bool,
    #[serde(default)]
    pub enable_issue_plan_decompositions: bool,
    #[serde(default)]
    pub enable_experimental_file_viewer: bool,
    #[serde(default)]
    pub enable_task_watchdogs: bool,
    #[serde(default)]
    pub enable_external_objects: bool,
    #[serde(default)]
    pub enable_smoke_lab: bool,
    #[serde(default)]
    pub enable_built_in_agents: bool,
    #[serde(default)]
    pub enable_beta_skills: bool,
    #[serde(default)]
    pub enable_summaries: bool,
    #[serde(default)]
    pub enable_status_cards: bool,
    #[serde(default)]
    pub enable_decisions: bool,
    #[serde(default)]
    pub enable_goals_sidebar_link: bool,
    #[serde(default)]
    pub enable_server_info_debug_view: bool,
    #[serde(default)]
    pub auto_restart_dev_server_when_idle: bool,
    #[serde(default)]
    pub enable_issue_graph_liveness_auto_recovery: bool,
    #[serde(default = "default_true")]
    pub enable_workspace_branch_reconcile_forward: bool,
    #[serde(default = "default_true")]
    pub enable_workspace_dirty_quarantine_repair: bool,
    #[serde(default)]
    pub enable_owner_instance_admin: bool,
    #[serde(default)]
    pub enable_worktree_run_execution: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_run_execution_activated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_run_execution_activation_instance_id: Option<String>,
    #[serde(default = "default_lookback_hours")]
    pub issue_graph_liveness_auto_recovery_lookback_hours: u32,
}

fn default_true() -> bool {
    true
}

fn default_lookback_hours() -> u32 {
    DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS
}

/// Managed overlay key → metadata. 对齐 Node `ManagedExperimentalKeyMetadata`。
pub type ManagedExperimentalKeyMetadata = HashMap<String, ManagedSettingMetadata>;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedSettingMetadata {
    #[serde(default)]
    pub managed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_by: Option<String>,
}

/// Managed instance config 的最小子集（仅对齐 overlay 路径所需字段）。
/// 对齐 Node `ManagedInstanceConfig` 的 `features` map。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedInstanceConfig {
    #[serde(default)]
    pub features: HashMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedExperimentalOverlayResult {
    pub experimental: InstanceExperimentalSettings,
    pub managed_keys: ManagedExperimentalKeyMetadata,
}

// =========================================================================
// Runtime env helpers
// =========================================================================

/// 对齐 Node `isTruthyRuntimeEnvValue`。
/// `None` 或非 truthy 字符串返回 false；大小写不敏感，trim 后匹配。
pub fn is_truthy_runtime_env_value(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(v) => {
            let lower = v.trim().to_lowercase();
            TRUTHY_RUNTIME_ENV_VALUES.iter().any(|t| *t == lower)
        }
    }
}

/// 对齐 Node `getRuntimeInstanceId`。trim 后空字符串视为缺失。
pub fn get_runtime_instance_id(env: &HashMap<String, String>) -> Option<String> {
    env.get("PAPERCLIP_INSTANCE_ID")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

// =========================================================================
// Patch helpers
// =========================================================================

/// 对齐 Node `stripServerManagedExperimentalPatchFields`。
/// 从 patch map 中删除 server-managed 字段，保留其他字段原样。
pub fn strip_server_managed_experimental_patch_fields(
    patch: &Map<String, Value>,
) -> Map<String, Value> {
    let mut out = Map::with_capacity(patch.len());
    for (k, v) in patch.iter() {
        if k == "worktreeRunExecutionActivatedAt"
            || k == "worktreeRunExecutionActivationInstanceId"
        {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    out
}

/// 对齐 Node `applyExperimentalSettingsPatch`。
/// 合并当前设置 + patch（剥离 server-managed 字段），并在
/// `enableWorktreeRunExecution` 首次开启时打上激活戳。
pub fn apply_experimental_settings_patch(
    current: &Value,
    patch: &Map<String, Value>,
    runtime_env: Option<&HashMap<String, String>>,
    now: Option<&dyn Fn() -> String>,
) -> InstanceExperimentalSettings {
    let previous = normalize_experimental_settings(Some(current));
    let patchable = strip_server_managed_experimental_patch_fields(patch);

    let mut merged = serde_json::Map::new();
    if let Value::Object(prev_map) = current_to_object(&previous) {
        for (k, v) in prev_map.iter() {
            merged.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in patchable.iter() {
        merged.insert(k.clone(), v.clone());
    }

    let next = normalize_experimental_settings(Some(&Value::Object(merged)));

    let has_patch = patchable.contains_key("enableWorktreeRunExecution");
    if !has_patch {
        return next;
    }
    if !next.enable_worktree_run_execution {
        return InstanceExperimentalSettings {
            worktree_run_execution_activated_at: None,
            worktree_run_execution_activation_instance_id: None,
            ..next
        };
    }
    if previous.enable_worktree_run_execution {
        return next;
    }

    let env_owned: HashMap<String, String> = runtime_env
        .cloned()
        .unwrap_or_else(|| std::env::vars().collect());
    if !is_truthy_runtime_env_value(env_owned.get("PAPERCLIP_IN_WORKTREE").map(|s| s.as_str())) {
        return next;
    }

    let stamped_at = now.map(|f| f()).unwrap_or_else(now_iso);
    let stamped_instance = get_runtime_instance_id(&env_owned);

    InstanceExperimentalSettings {
        worktree_run_execution_activated_at: Some(stamped_at),
        worktree_run_execution_activation_instance_id: stamped_instance,
        ..next
    }
}

// =========================================================================
// Activation state
// =========================================================================

/// 对齐 Node `suppressWorktreeRunExecution`。导出以便测试组合调用。
pub fn suppress_worktree_run_execution(
    reason: WorktreeRunExecutionSuppressedReason,
    activation_instance_id: Option<String>,
) -> WorktreeRunExecutionActivationState {
    WorktreeRunExecutionActivationState::Suppressed {
        cutoff: None,
        activation_instance_id,
        reason,
    }
}

/// 对齐 Node `resolveWorktreeRunExecutionActivation`。
/// 纯决策；不做 env lookup，不打 IO。
pub fn resolve_worktree_run_execution_activation(
    experimental: &InstanceExperimentalSettings,
    current_instance_id: Option<&str>,
) -> WorktreeRunExecutionActivationState {
    if !experimental.enable_worktree_run_execution {
        return suppress_worktree_run_execution(
            WorktreeRunExecutionSuppressedReason::FlagDisabled,
            experimental
                .worktree_run_execution_activation_instance_id
                .clone(),
        );
    }
    let Some(cutoff) = experimental.worktree_run_execution_activated_at.clone() else {
        return suppress_worktree_run_execution(
            WorktreeRunExecutionSuppressedReason::MissingCutoff,
            experimental
                .worktree_run_execution_activation_instance_id
                .clone(),
        );
    };
    let Some(current_id) = current_instance_id else {
        return suppress_worktree_run_execution(
            WorktreeRunExecutionSuppressedReason::MissingInstanceId,
            experimental
                .worktree_run_execution_activation_instance_id
                .clone(),
        );
    };
    let prior = experimental
        .worktree_run_execution_activation_instance_id
        .as_deref();
    if prior != Some(current_id) {
        return suppress_worktree_run_execution(
            WorktreeRunExecutionSuppressedReason::InstanceIdMismatch,
            experimental
                .worktree_run_execution_activation_instance_id
                .clone(),
        );
    }
    WorktreeRunExecutionActivationState::Armed {
        cutoff,
        activation_instance_id: current_id.to_string(),
        reason: None,
    }
}

// =========================================================================
// Normalizers
// =========================================================================

/// 对齐 Node `normalizeGeneralSettings`。
/// `None` 或非对象视为 `{}`；schema 拒绝的输入返回完全默认值。
pub fn normalize_general_settings(raw: Option<&Value>) -> InstanceGeneralSettings {
    let value = raw.unwrap_or(&Value::Null);
    let parsed: Option<RawGeneralSettings> = match value {
        Value::Null => Some(RawGeneralSettings::default()),
        v => serde_json::from_value(v.clone()).ok(),
    };
    match parsed {
        Some(p) => InstanceGeneralSettings {
            censor_username_in_logs: p.censor_username_in_logs.unwrap_or(false),
            keyboard_shortcuts: p.keyboard_shortcuts.unwrap_or(false),
            feedback_data_sharing_preference: p
                .feedback_data_sharing_preference
                .unwrap_or_else(|| DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE.to_string()),
            backup_retention: p
                .backup_retention
                .unwrap_or_else(|| DEFAULT_BACKUP_RETENTION.to_string()),
            execution_mode: p.execution_mode,
        },
        None => InstanceGeneralSettings {
            censor_username_in_logs: false,
            keyboard_shortcuts: false,
            feedback_data_sharing_preference: DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE.to_string(),
            backup_retention: DEFAULT_BACKUP_RETENTION.to_string(),
            execution_mode: None,
        },
    }
}

/// 对齐 Node `normalizeExperimentalSettings`。
/// `None` 或非对象视为 `{}`；schema 拒绝的输入返回完全默认值。
pub fn normalize_experimental_settings(raw: Option<&Value>) -> InstanceExperimentalSettings {
    let value = raw.unwrap_or(&Value::Null);
    let parsed: Option<RawExperimentalSettings> = match value {
        Value::Null => Some(RawExperimentalSettings::default()),
        v => serde_json::from_value(v.clone()).ok(),
    };

    let fallback = || InstanceExperimentalSettings {
        enable_environments: false,
        enable_isolated_workspaces: false,
        enable_streamlined_left_navigation: true,
        enable_apps: false,
        enable_pipelines: false,
        enable_cases: false,
        enable_conference_room_chat: false,
        enable_task_chat_redesign: false,
        enable_issue_plan_decompositions: false,
        enable_experimental_file_viewer: false,
        enable_task_watchdogs: false,
        enable_external_objects: false,
        enable_smoke_lab: false,
        enable_built_in_agents: false,
        enable_beta_skills: false,
        enable_summaries: false,
        enable_status_cards: false,
        enable_decisions: false,
        enable_goals_sidebar_link: false,
        enable_server_info_debug_view: false,
        auto_restart_dev_server_when_idle: false,
        enable_issue_graph_liveness_auto_recovery: false,
        enable_workspace_branch_reconcile_forward: true,
        enable_workspace_dirty_quarantine_repair: true,
        enable_owner_instance_admin: false,
        enable_worktree_run_execution: false,
        worktree_run_execution_activated_at: None,
        worktree_run_execution_activation_instance_id: None,
        issue_graph_liveness_auto_recovery_lookback_hours:
            DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS,
    };

    match parsed {
        Some(p) => InstanceExperimentalSettings {
            enable_environments: p.enable_environments.unwrap_or(false),
            enable_isolated_workspaces: p.enable_isolated_workspaces.unwrap_or(false),
            enable_streamlined_left_navigation: p
                .enable_streamlined_left_navigation
                .unwrap_or(true),
            enable_apps: p.enable_apps.unwrap_or(false),
            enable_pipelines: p.enable_pipelines.unwrap_or(false),
            enable_cases: p.enable_cases.unwrap_or(false),
            enable_conference_room_chat: p.enable_conference_room_chat.unwrap_or(false),
            enable_task_chat_redesign: p.enable_task_chat_redesign.unwrap_or(false),
            enable_issue_plan_decompositions: p
                .enable_issue_plan_decompositions
                .unwrap_or(false),
            enable_experimental_file_viewer: p.enable_experimental_file_viewer.unwrap_or(false),
            enable_task_watchdogs: p.enable_task_watchdogs.unwrap_or(false),
            enable_external_objects: p.enable_external_objects.unwrap_or(false),
            enable_smoke_lab: p.enable_smoke_lab.unwrap_or(false),
            enable_built_in_agents: p.enable_built_in_agents.unwrap_or(false),
            enable_beta_skills: p.enable_beta_skills.unwrap_or(false),
            enable_summaries: p.enable_summaries.unwrap_or(false),
            enable_status_cards: p.enable_status_cards.unwrap_or(false),
            enable_decisions: p.enable_decisions.unwrap_or(false),
            enable_goals_sidebar_link: p.enable_goals_sidebar_link.unwrap_or(false),
            enable_server_info_debug_view: p.enable_server_info_debug_view.unwrap_or(false),
            auto_restart_dev_server_when_idle: p.auto_restart_dev_server_when_idle.unwrap_or(false),
            enable_issue_graph_liveness_auto_recovery: p
                .enable_issue_graph_liveness_auto_recovery
                .unwrap_or(false),
            enable_workspace_branch_reconcile_forward: p
                .enable_workspace_branch_reconcile_forward
                .unwrap_or(true),
            enable_workspace_dirty_quarantine_repair: p
                .enable_workspace_dirty_quarantine_repair
                .unwrap_or(true),
            enable_owner_instance_admin: p.enable_owner_instance_admin.unwrap_or(false),
            enable_worktree_run_execution: p.enable_worktree_run_execution.unwrap_or(false),
            worktree_run_execution_activated_at: p.worktree_run_execution_activated_at,
            worktree_run_execution_activation_instance_id: p
                .worktree_run_execution_activation_instance_id,
            issue_graph_liveness_auto_recovery_lookback_hours: p
                .issue_graph_liveness_auto_recovery_lookback_hours
                .unwrap_or(DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS),
        },
        None => fallback(),
    }
}

// =========================================================================
// Managed overlay
// =========================================================================

/// 对齐 Node `applyManagedExperimentalOverlay`。
/// `managedConfig=None` 时不变；否则 overlay 每个 feature key，
/// 并填充 `managed_keys[key] = {managed: true, managedBy}`。
pub fn apply_managed_experimental_overlay(
    experimental: &InstanceExperimentalSettings,
    managed_config: Option<&ManagedInstanceConfig>,
) -> ManagedExperimentalOverlayResult {
    let managed_config = match managed_config {
        Some(c) => c,
        None => {
            return ManagedExperimentalOverlayResult {
                experimental: experimental.clone(),
                managed_keys: ManagedExperimentalKeyMetadata::new(),
            };
        }
    };

    let mut next = experimental.clone();
    let mut managed_keys = ManagedExperimentalKeyMetadata::new();
    for (key, value) in managed_config.features.iter() {
        set_feature_flag(&mut next, key, *value);
        managed_keys.insert(
            key.clone(),
            ManagedSettingMetadata {
                managed: true,
                managed_by: Some(PAPERCLIP_CLOUD_MANAGED_BY.to_string()),
            },
        );
    }
    ManagedExperimentalOverlayResult {
        experimental: next,
        managed_keys,
    }
}

// =========================================================================
// Internals
// =========================================================================

#[derive(Default, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGeneralSettings {
    #[serde(default)]
    censor_username_in_logs: Option<bool>,
    #[serde(default)]
    keyboard_shortcuts: Option<bool>,
    #[serde(default)]
    feedback_data_sharing_preference: Option<String>,
    #[serde(default)]
    backup_retention: Option<String>,
    #[serde(default)]
    execution_mode: Option<String>,
}

#[derive(Default, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct RawExperimentalSettings {
    #[serde(default)]
    enable_environments: Option<bool>,
    #[serde(default)]
    enable_isolated_workspaces: Option<bool>,
    #[serde(default)]
    enable_streamlined_left_navigation: Option<bool>,
    #[serde(default)]
    enable_apps: Option<bool>,
    #[serde(default)]
    enable_pipelines: Option<bool>,
    #[serde(default)]
    enable_cases: Option<bool>,
    #[serde(default)]
    enable_conference_room_chat: Option<bool>,
    #[serde(default)]
    enable_task_chat_redesign: Option<bool>,
    #[serde(default)]
    enable_issue_plan_decompositions: Option<bool>,
    #[serde(default)]
    enable_experimental_file_viewer: Option<bool>,
    #[serde(default)]
    enable_task_watchdogs: Option<bool>,
    #[serde(default)]
    enable_external_objects: Option<bool>,
    #[serde(default)]
    enable_smoke_lab: Option<bool>,
    #[serde(default)]
    enable_built_in_agents: Option<bool>,
    #[serde(default)]
    enable_beta_skills: Option<bool>,
    #[serde(default)]
    enable_summaries: Option<bool>,
    #[serde(default)]
    enable_status_cards: Option<bool>,
    #[serde(default)]
    enable_decisions: Option<bool>,
    #[serde(default)]
    enable_goals_sidebar_link: Option<bool>,
    #[serde(default)]
    enable_server_info_debug_view: Option<bool>,
    #[serde(default)]
    auto_restart_dev_server_when_idle: Option<bool>,
    #[serde(default)]
    enable_issue_graph_liveness_auto_recovery: Option<bool>,
    #[serde(default)]
    enable_workspace_branch_reconcile_forward: Option<bool>,
    #[serde(default)]
    enable_workspace_dirty_quarantine_repair: Option<bool>,
    #[serde(default)]
    enable_owner_instance_admin: Option<bool>,
    #[serde(default)]
    enable_worktree_run_execution: Option<bool>,
    #[serde(default)]
    worktree_run_execution_activated_at: Option<String>,
    #[serde(default)]
    worktree_run_execution_activation_instance_id: Option<String>,
    #[serde(default)]
    issue_graph_liveness_auto_recovery_lookback_hours: Option<u32>,
}

fn current_to_object(settings: &InstanceExperimentalSettings) -> Value {
    serde_json::to_value(settings).unwrap_or(Value::Null)
}

fn now_iso() -> String {
    // 不引入 chrono；保持 pc-config 零外部时间依赖。
    // 形如 `2024-05-01T12:34:56.789Z` 的简化 UTC ISO8601。
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso8601_utc(secs)
}

fn format_iso8601_utc(unix_secs: u64) -> String {
    // Compact UTC formatter without chrono. Valid for our activation stamp use;
    // matches `new Date().toISOString()` shape `YYYY-MM-DDTHH:MM:SS.fffZ`.
    let secs = unix_secs % 60;
    let mins = (unix_secs / 60) % 60;
    let hours = (unix_secs / 3600) % 24;
    let total_days = unix_secs / 86_400;
    let (y, m, d) = days_to_ymd(total_days as i64);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        y, m, d, hours, mins, secs
    )
}

fn days_to_ymd(days_since_epoch: i64) -> (i64, u32, u32) {
    // Civil-from-days algorithm (Howard Hinnant). Returns (year, month, day).
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn set_feature_flag(target: &mut InstanceExperimentalSettings, key: &str, value: bool) {
    // Mirror Node: write into the experimental struct via camelCase key
    // matching. Keep this exhaustive so adding a feature to the Node
    // managed-config list shows up as a missing-field here at compile time.
    match key {
        "enableEnvironments" => target.enable_environments = value,
        "enableIsolatedWorkspaces" => target.enable_isolated_workspaces = value,
        "enableStreamlinedLeftNavigation" => target.enable_streamlined_left_navigation = value,
        "enableApps" => target.enable_apps = value,
        "enablePipelines" => target.enable_pipelines = value,
        "enableCases" => target.enable_cases = value,
        "enableConferenceRoomChat" => target.enable_conference_room_chat = value,
        "enableTaskChatRedesign" => target.enable_task_chat_redesign = value,
        "enableIssuePlanDecompositions" => target.enable_issue_plan_decompositions = value,
        "enableExperimentalFileViewer" => target.enable_experimental_file_viewer = value,
        "enableTaskWatchdogs" => target.enable_task_watchdogs = value,
        "enableExternalObjects" => target.enable_external_objects = value,
        "enableSmokeLab" => target.enable_smoke_lab = value,
        "enableBuiltInAgents" => target.enable_built_in_agents = value,
        "enableBetaSkills" => target.enable_beta_skills = value,
        "enableSummaries" => target.enable_summaries = value,
        "enableStatusCards" => target.enable_status_cards = value,
        "enableDecisions" => target.enable_decisions = value,
        "enableGoalsSidebarLink" => target.enable_goals_sidebar_link = value,
        "enableServerInfoDebugView" => target.enable_server_info_debug_view = value,
        "autoRestartDevServerWhenIdle" => target.auto_restart_dev_server_when_idle = value,
        "enableIssueGraphLivenessAutoRecovery" => {
            target.enable_issue_graph_liveness_auto_recovery = value
        }
        "enableWorkspaceBranchReconcileForward" => {
            target.enable_workspace_branch_reconcile_forward = value
        }
        "enableWorkspaceDirtyQuarantineRepair" => {
            target.enable_workspace_dirty_quarantine_repair = value
        }
        "enableOwnerInstanceAdmin" => target.enable_owner_instance_admin = value,
        "enableWorktreeRunExecution" => target.enable_worktree_run_execution = value,
        _ => {
            // Unknown managed key: silently ignored, matching Node schema-strip behavior.
        }
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn patch(kvs: Vec<(&str, Value)>) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, v) in kvs {
            m.insert(k.to_string(), v);
        }
        m
    }

    fn env_with(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ---- isTruthyRuntimeEnvValue ----

    #[test]
    fn is_truthy_recognizes_canonical_values() {
        for v in ["1", "true", "yes", "on"] {
            assert!(is_truthy_runtime_env_value(Some(v)), "expected true: {v}");
        }
    }

    #[test]
    fn is_truthy_is_case_insensitive_with_trim() {
        for v in [" TRUE ", "On", "YES", "  1  "] {
            assert!(is_truthy_runtime_env_value(Some(v)), "expected true: {v}");
        }
    }

    #[test]
    fn is_truthy_rejects_non_truthy() {
        for v in ["0", "false", "no", "off", "", "truthy", "  "] {
            assert!(!is_truthy_runtime_env_value(Some(v)), "expected false: {v}");
        }
    }

    #[test]
    fn is_truthy_rejects_none() {
        assert!(!is_truthy_runtime_env_value(None));
    }

    // ---- getRuntimeInstanceId ----

    #[test]
    fn runtime_instance_id_returns_trimmed_when_present() {
        let env = env_with(&[("PAPERCLIP_INSTANCE_ID", "  instance-a  ")]);
        assert_eq!(get_runtime_instance_id(&env).as_deref(), Some("instance-a"));
    }

    #[test]
    fn runtime_instance_id_missing_returns_none() {
        let env = env_with(&[("OTHER", "x")]);
        assert_eq!(get_runtime_instance_id(&env), None);
    }

    #[test]
    fn runtime_instance_id_blank_returns_none() {
        let env = env_with(&[("PAPERCLIP_INSTANCE_ID", "   ")]);
        assert_eq!(get_runtime_instance_id(&env), None);
    }

    // ---- stripServerManagedExperimentalPatchFields ----

    #[test]
    fn strip_drops_server_managed_fields_only() {
        let mut m = Map::new();
        m.insert(
            "worktreeRunExecutionActivatedAt".to_string(),
            Value::String("2024-01-01T00:00:00.000Z".into()),
        );
        m.insert(
            "worktreeRunExecutionActivationInstanceId".to_string(),
            Value::String("inst-x".into()),
        );
        m.insert("enableApps".to_string(), Value::Bool(true));
        m.insert("enablePipelines".to_string(), Value::Bool(false));

        let out = strip_server_managed_experimental_patch_fields(&m);
        assert_eq!(out.len(), 2);
        assert!(out.contains_key("enableApps"));
        assert!(out.contains_key("enablePipelines"));
        assert!(!out.contains_key("worktreeRunExecutionActivatedAt"));
        assert!(!out.contains_key("worktreeRunExecutionActivationInstanceId"));
    }

    #[test]
    fn strip_preserves_empty_patch() {
        let m = Map::new();
        assert!(strip_server_managed_experimental_patch_fields(&m).is_empty());
    }

    // ---- applyExperimentalSettingsPatch ----

    #[test]
    fn apply_patch_without_worktree_returns_merged() {
        let current = json!({"enableApps": true});
        let p = patch(vec![("enablePipelines", Value::Bool(true))]);
        let next = apply_experimental_settings_patch(&current, &p, None, None);
        assert!(next.enable_apps);
        assert!(next.enable_pipelines);
    }

    #[test]
    fn apply_patch_clears_activation_when_disabling() {
        let current = json!({
            "enableWorktreeRunExecution": true,
            "worktreeRunExecutionActivatedAt": "2024-01-01T00:00:00.000Z",
            "worktreeRunExecutionActivationInstanceId": "old-inst"
        });
        let p = patch(vec![("enableWorktreeRunExecution", Value::Bool(false))]);
        let next = apply_experimental_settings_patch(&current, &p, None, None);
        assert!(!next.enable_worktree_run_execution);
        assert_eq!(next.worktree_run_execution_activated_at, None);
        assert_eq!(next.worktree_run_execution_activation_instance_id, None);
    }

    #[test]
    fn apply_patch_stamps_on_first_enable_in_worktree() {
        let current = json!({"enableWorktreeRunExecution": false});
        let p = patch(vec![("enableWorktreeRunExecution", Value::Bool(true))]);
        let env = env_with(&[
            ("PAPERCLIP_IN_WORKTREE", "1"),
            ("PAPERCLIP_INSTANCE_ID", "inst-77"),
        ]);
        let next = apply_experimental_settings_patch(&current, &p, Some(&env), None);
        assert!(next.enable_worktree_run_execution);
        assert!(next.worktree_run_execution_activated_at.is_some());
        assert_eq!(
            next.worktree_run_execution_activation_instance_id.as_deref(),
            Some("inst-77")
        );
    }

    #[test]
    fn apply_patch_does_not_stamp_outside_worktree() {
        let current = json!({"enableWorktreeRunExecution": false});
        let p = patch(vec![("enableWorktreeRunExecution", Value::Bool(true))]);
        let env = env_with(&[("PAPERCLIP_INSTANCE_ID", "inst-77")]);
        let next = apply_experimental_settings_patch(&current, &p, Some(&env), None);
        assert!(next.enable_worktree_run_execution);
        assert_eq!(next.worktree_run_execution_activated_at, None);
        assert_eq!(next.worktree_run_execution_activation_instance_id, None);
    }

    #[test]
    fn apply_patch_does_not_stamp_when_already_enabled() {
        let existing_ts = "2024-05-01T00:00:00.000Z";
        let current = json!({
            "enableWorktreeRunExecution": true,
            "worktreeRunExecutionActivatedAt": existing_ts,
            "worktreeRunExecutionActivationInstanceId": "orig"
        });
        let p = patch(vec![("enableWorktreeRunExecution", Value::Bool(true))]);
        let env = env_with(&[("PAPERCLIP_IN_WORKTREE", "1")]);
        let next = apply_experimental_settings_patch(&current, &p, Some(&env), None);
        assert_eq!(
            next.worktree_run_execution_activated_at.as_deref(),
            Some(existing_ts)
        );
        assert_eq!(
            next.worktree_run_execution_activation_instance_id.as_deref(),
            Some("orig")
        );
    }

    #[test]
    fn apply_patch_uses_injected_now() {
        let current = json!({"enableWorktreeRunExecution": false});
        let p = patch(vec![("enableWorktreeRunExecution", Value::Bool(true))]);
        let env = env_with(&[("PAPERCLIP_IN_WORKTREE", "yes")]);
        let next = apply_experimental_settings_patch(
            &current,
            &p,
            Some(&env),
            Some(&|| "2030-12-31T23:59:59.999Z".to_string()),
        );
        assert_eq!(
            next.worktree_run_execution_activated_at.as_deref(),
            Some("2030-12-31T23:59:59.999Z")
        );
    }

    #[test]
    fn apply_patch_strips_server_fields_in_input() {
        let current = json!({"enableApps": true});
        let mut p = patch(vec![("enableApps", Value::Bool(false))]);
        // server-managed fields must not be honored.
        p.insert(
            "worktreeRunExecutionActivatedAt".to_string(),
            Value::String("tampered".into()),
        );
        p.insert(
            "worktreeRunExecutionActivationInstanceId".to_string(),
            Value::String("tampered".into()),
        );
        let next = apply_experimental_settings_patch(&current, &p, None, None);
        assert!(!next.enable_apps);
        assert_eq!(next.worktree_run_execution_activated_at, None);
        assert_eq!(next.worktree_run_execution_activation_instance_id, None);
    }

    // ---- normalizeGeneralSettings ----

    #[test]
    fn normalize_general_defaults_when_input_is_null() {
        let s = normalize_general_settings(None);
        assert_eq!(s.censor_username_in_logs, false);
        assert_eq!(s.keyboard_shortcuts, false);
        assert_eq!(
            s.feedback_data_sharing_preference,
            DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE
        );
        assert_eq!(s.backup_retention, DEFAULT_BACKUP_RETENTION);
        assert_eq!(s.execution_mode, None);
    }

    #[test]
    fn normalize_general_partial_input_uses_defaults() {
        let raw = json!({"censorUsernameInLogs": true});
        let s = normalize_general_settings(Some(&raw));
        assert!(s.censor_username_in_logs);
        assert_eq!(s.keyboard_shortcuts, false);
        assert_eq!(
            s.feedback_data_sharing_preference,
            DEFAULT_FEEDBACK_DATA_SHARING_PREFERENCE
        );
    }

    #[test]
    fn normalize_general_keeps_execution_mode_only_when_present() {
        let with_mode = json!({"executionMode": "restricted"});
        assert_eq!(
            normalize_general_settings(Some(&with_mode)).execution_mode,
            Some("restricted".into())
        );
        let without_mode = json!({"censorUsernameInLogs": true});
        assert_eq!(
            normalize_general_settings(Some(&without_mode)).execution_mode,
            None
        );
    }

    #[test]
    fn normalize_general_invalid_value_falls_back() {
        let s = normalize_general_settings(Some(&json!("not an object")));
        assert_eq!(s.censor_username_in_logs, false);
        assert_eq!(s.execution_mode, None);
    }

    // ---- normalizeExperimentalSettings ----

    #[test]
    fn normalize_experimental_defaults_on_null() {
        let s = normalize_experimental_settings(None);
        // Node defaults: most flags `false`, three `true`.
        assert!(!s.enable_environments);
        assert!(!s.enable_apps);
        assert!(s.enable_streamlined_left_navigation);
        assert!(s.enable_workspace_branch_reconcile_forward);
        assert!(s.enable_workspace_dirty_quarantine_repair);
        assert_eq!(
            s.issue_graph_liveness_auto_recovery_lookback_hours,
            DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS
        );
    }

    #[test]
    fn normalize_experimental_explicit_override() {
        let raw = json!({
            "enableEnvironments": false,
            "enableApps": true,
            "enableStreamlinedLeftNavigation": false,
        });
        let s = normalize_experimental_settings(Some(&raw));
        assert_eq!(s.enable_environments, false);
        assert!(s.enable_apps);
        assert_eq!(s.enable_streamlined_left_navigation, false);
        // unspecified → node default (true)
        assert!(s.enable_workspace_branch_reconcile_forward);
    }

    #[test]
    fn normalize_experimental_invalid_input_falls_back() {
        let s = normalize_experimental_settings(Some(&json!("not an object")));
        // Default branch returns `false` for everything except the three
        // fields the Node default branch sets to `true`.
        assert!(!s.enable_apps);
        assert!(s.enable_streamlined_left_navigation);
        assert!(s.enable_workspace_branch_reconcile_forward);
        assert!(s.enable_workspace_dirty_quarantine_repair);
        assert_eq!(
            s.issue_graph_liveness_auto_recovery_lookback_hours,
            DEFAULT_ISSUE_GRAPH_LIVENESS_AUTO_RECOVERY_LOOKBACK_HOURS
        );
    }

    // ---- resolveWorktreeRunExecutionActivation ----

    #[test]
    fn activation_armed_when_flag_and_id_match() {
        let s = InstanceExperimentalSettings {
            enable_worktree_run_execution: true,
            worktree_run_execution_activated_at: Some("2024-01-01T00:00:00.000Z".into()),
            worktree_run_execution_activation_instance_id: Some("inst-x".into()),
            ..Default::default()
        };
        let state = resolve_worktree_run_execution_activation(&s, Some("inst-x"));
        match state {
            WorktreeRunExecutionActivationState::Armed {
                cutoff,
                activation_instance_id,
                ..
            } => {
                assert_eq!(cutoff, "2024-01-01T00:00:00.000Z");
                assert_eq!(activation_instance_id, "inst-x");
            }
            _ => panic!("expected Armed"),
        }
    }

    #[test]
    fn activation_flag_disabled_reports_flag_disabled_with_prior_id() {
        let s = InstanceExperimentalSettings {
            enable_worktree_run_execution: false,
            worktree_run_execution_activation_instance_id: Some("prior".into()),
            ..Default::default()
        };
        let state = resolve_worktree_run_execution_activation(&s, Some("inst-x"));
        match state {
            WorktreeRunExecutionActivationState::Suppressed {
                activation_instance_id,
                reason,
                ..
            } => {
                assert_eq!(
                    activation_instance_id.as_deref(),
                    Some("prior"),
                    "Node forwards the prior activation instance id even when suppressed"
                );
                assert_eq!(reason, WorktreeRunExecutionSuppressedReason::FlagDisabled);
            }
            _ => panic!("expected Suppressed"),
        }
    }

    #[test]
    fn activation_missing_cutoff_reports_missing_cutoff() {
        let s = InstanceExperimentalSettings {
            enable_worktree_run_execution: true,
            ..Default::default()
        };
        let state = resolve_worktree_run_execution_activation(&s, Some("inst-x"));
        match state {
            WorktreeRunExecutionActivationState::Suppressed { reason, .. } => {
                assert_eq!(reason, WorktreeRunExecutionSuppressedReason::MissingCutoff);
            }
            _ => panic!("expected Suppressed"),
        }
    }

    #[test]
    fn activation_missing_current_id_reports_missing_instance_id() {
        let s = InstanceExperimentalSettings {
            enable_worktree_run_execution: true,
            worktree_run_execution_activated_at: Some("2024-01-01T00:00:00.000Z".into()),
            worktree_run_execution_activation_instance_id: Some("inst-x".into()),
            ..Default::default()
        };
        let state = resolve_worktree_run_execution_activation(&s, None);
        match state {
            WorktreeRunExecutionActivationState::Suppressed { reason, .. } => {
                assert_eq!(
                    reason,
                    WorktreeRunExecutionSuppressedReason::MissingInstanceId
                );
            }
            _ => panic!("expected Suppressed"),
        }
    }

    #[test]
    fn activation_mismatch_reports_instance_id_mismatch() {
        let s = InstanceExperimentalSettings {
            enable_worktree_run_execution: true,
            worktree_run_execution_activated_at: Some("2024-01-01T00:00:00.000Z".into()),
            worktree_run_execution_activation_instance_id: Some("inst-other".into()),
            ..Default::default()
        };
        let state = resolve_worktree_run_execution_activation(&s, Some("inst-x"));
        match state {
            WorktreeRunExecutionActivationState::Suppressed { reason, .. } => {
                assert_eq!(
                    reason,
                    WorktreeRunExecutionSuppressedReason::InstanceIdMismatch
                );
            }
            _ => panic!("expected Suppressed"),
        }
    }

    // ---- applyManagedExperimentalOverlay ----

    #[test]
    fn overlay_none_returns_unchanged() {
        let s = InstanceExperimentalSettings {
            enable_apps: true,
            ..Default::default()
        };
        let r = apply_managed_experimental_overlay(&s, None);
        assert!(r.managed_keys.is_empty());
        assert_eq!(r.experimental.enable_apps, true);
    }

    #[test]
    fn overlay_overrides_known_keys_and_records_metadata() {
        let s = InstanceExperimentalSettings {
            enable_apps: false,
            enable_pipelines: true,
            ..Default::default()
        };
        let mut features = HashMap::new();
        features.insert("enableApps".to_string(), true);
        let cfg = ManagedInstanceConfig { features };
        let r = apply_managed_experimental_overlay(&s, Some(&cfg));
        assert!(r.experimental.enable_apps);
        // Other field untouched.
        assert!(r.experimental.enable_pipelines);
        let entry = r.managed_keys.get("enableApps").expect("managed key");
        assert!(entry.managed);
        assert_eq!(entry.managed_by.as_deref(), Some(PAPERCLIP_CLOUD_MANAGED_BY));
    }

    #[test]
    fn overlay_records_metadata_for_all_managed_keys_even_unknown() {
        // Node records metadata for every entry in managedConfig.features,
        // even keys that don't map onto InstanceExperimentalSettings (the
        // Rust port keeps the `set_feature_flag` no-op for unknown keys,
        // but metadata insertion must still occur — matches Node 1:1).
        let s = InstanceExperimentalSettings::default();
        let mut features = HashMap::new();
        features.insert("unknownFlag".to_string(), true);
        let cfg = ManagedInstanceConfig { features };
        let r = apply_managed_experimental_overlay(&s, Some(&cfg));
        assert!(r.managed_keys.contains_key("unknownFlag"));
        let entry = &r.managed_keys["unknownFlag"];
        assert!(entry.managed);
        assert_eq!(entry.managed_by.as_deref(), Some(PAPERCLIP_CLOUD_MANAGED_BY));
    }

    // ---- format_iso8601_utc ----

    #[test]
    fn format_iso8601_utc_zero_epoch() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn format_iso8601_utc_known_instant() {
        // 1700000000 seconds -> 2023-11-14T22:13:20Z
        assert_eq!(format_iso8601_utc(1_700_000_000), "2023-11-14T22:13:20.000Z");
    }
}
