//! R649: worktree run execution 自动调度资格。
//!
//! 与 Node `services/routines.ts::getAutomaticRoutineDispatchEligibility` +
//! `services/instance-settings.ts::resolveWorktreeRunExecutionActivation` 1:1 对齐。
//!
//! 语义:
//! - 非 worktree 运行时 → 永远 eligible（不抑制）。
//! - worktree 运行时 → 看 DB experimental flag + activation timestamp +
//!   activation instance id；任何 mismatch → suppressed。
//! - eligible 时再 cutoff 检查：`routine.created_at >= cutoff` 才放行（防止已存在
//!   的 routine 在 flag 打开后被偷偷激活）。

use chrono::{DateTime, Utc};
use pc_repos::routine::RoutineRow;
use pc_repos::settings::{SettingsRepo, WorktreeRunExecutionActivation};
use serde::{Deserialize, Serialize};

/// Routine 自动调度资格（含可观察的 reason）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticRoutineDispatchEligibility {
    pub eligible: bool,
    pub reason: AutomaticRoutineSuppressionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticRoutineSuppressionReason {
    /// 非 worktree runtime, 总是放行。
    NotWorktreeRuntime,
    /// 工作流是常规运行时。
    Eligible,
    /// DB flag 未启用 / 没 cutoff / instance id mismatch / settings 读取失败。
    FlagDisabled,
    MissingCutoff,
    MissingInstanceId,
    InstanceIdMismatch,
    SettingsReadError,
    /// worktree runtime + activation + instance match，但 routine 在 cutoff 之前
    /// 已存在（不应该被新策略覆盖）。
    PreCutoffRoutine,
}

impl AutomaticRoutineDispatchEligibility {
    pub fn eligible() -> Self {
        Self {
            eligible: true,
            reason: AutomaticRoutineSuppressionReason::Eligible,
        }
    }

    pub fn suppressed(reason: AutomaticRoutineSuppressionReason) -> Self {
        Self {
            eligible: false,
            reason,
        }
    }

    /// 是否因为 worktree execution cutoff 被抑制。
    pub fn is_worktree_suppressed(&self) -> bool {
        matches!(
            self.reason,
            AutomaticRoutineSuppressionReason::FlagDisabled
                | AutomaticRoutineSuppressionReason::MissingCutoff
                | AutomaticRoutineSuppressionReason::MissingInstanceId
                | AutomaticRoutineSuppressionReason::InstanceIdMismatch
                | AutomaticRoutineSuppressionReason::SettingsReadError
                | AutomaticRoutineSuppressionReason::PreCutoffRoutine
        )
    }
}

/// 把 `WorktreeRunExecutionActivation` 投影到 suppression reason。
fn reason_from_activation(
    activation: &WorktreeRunExecutionActivation,
) -> AutomaticRoutineSuppressionReason {
    if activation.armed {
        return AutomaticRoutineSuppressionReason::Eligible;
    }
    match activation.reason {
        Some("flag_disabled") => AutomaticRoutineSuppressionReason::FlagDisabled,
        Some("missing_cutoff") => AutomaticRoutineSuppressionReason::MissingCutoff,
        Some("missing_instance_id") => AutomaticRoutineSuppressionReason::MissingInstanceId,
        Some("instance_id_mismatch") => AutomaticRoutineSuppressionReason::InstanceIdMismatch,
        Some("settings_read_error") => AutomaticRoutineSuppressionReason::SettingsReadError,
        _ => AutomaticRoutineSuppressionReason::FlagDisabled,
    }
}

/// env 解析 helper（与 Node `isTruthyRuntimeEnvValue` 1:1）。
pub fn is_truthy_runtime_env_value(value: Option<&str>) -> bool {
    match value {
        Some("1") | Some("true") | Some("yes") | Some("on") => true,
        Some(v) => v.eq_ignore_ascii_case("true")
            || v.eq_ignore_ascii_case("yes")
            || v.eq_ignore_ascii_case("on")
            || v == "1",
        None => false,
    }
}

/// Runtime instance id 解析 helper（与 Node `getRuntimeInstanceId` 对齐）。
pub fn runtime_instance_id(env: &std::collections::HashMap<String, String>) -> Option<String> {
    env.get("PAPERCLIP_INSTANCE_ID")
        .cloned()
        .or_else(|| env.get("PAPERCLIP_RUNTIME_INSTANCE_ID").cloned())
}

/// 把 DB row 的 `created_at` 转成 `DateTime<Utc>`，便于 cutoff 比较。
pub fn routine_created_at(routine: &RoutineRow) -> DateTime<Utc> {
    routine.created_at.as_datetime()
}

/// 计算一次 automatic dispatch 资格。同步纯函数（DB/env 已预解析）。
pub fn evaluate_automatic_dispatch_eligibility(
    in_worktree: bool,
    activation: &WorktreeRunExecutionActivation,
    routine_created_at: DateTime<Utc>,
) -> AutomaticRoutineDispatchEligibility {
    if !in_worktree {
        return AutomaticRoutineDispatchEligibility {
            eligible: true,
            reason: AutomaticRoutineSuppressionReason::NotWorktreeRuntime,
        };
    }
    if !activation.armed {
        return AutomaticRoutineDispatchEligibility::suppressed(reason_from_activation(
            activation,
        ));
    }
    // cutoff 必须 <= routine.created_at 才允许（防止旧 routine 被新策略劫持）
    match activation.cutoff {
        Some(cutoff) if routine_created_at < cutoff => AutomaticRoutineDispatchEligibility::suppressed(
            AutomaticRoutineSuppressionReason::PreCutoffRoutine,
        ),
        _ => AutomaticRoutineDispatchEligibility::eligible(),
    }
}

/// DB-backed 解析：env + DB experimental settings + routine.created_at 综合判断。
pub async fn resolve_automatic_dispatch_eligibility(
    db: &pc_repos::Db,
    env: &std::collections::HashMap<String, String>,
    current_instance_id: Option<&str>,
    routine: &RoutineRow,
) -> AutomaticRoutineDispatchEligibility {
    let in_worktree =
        is_truthy_runtime_env_value(env.get("PAPERCLIP_IN_WORKTREE").map(String::as_str));
    if !in_worktree {
        return AutomaticRoutineDispatchEligibility::eligible();
    }
    let activation = match SettingsRepo::new(db)
        .resolve_worktree_run_execution_activation(current_instance_id)
        .await
    {
        Ok(activation) => activation,
        Err(_) => {
            return AutomaticRoutineDispatchEligibility::suppressed(
                AutomaticRoutineSuppressionReason::SettingsReadError,
            )
        }
    };
    evaluate_automatic_dispatch_eligibility(in_worktree, &activation, routine_created_at(routine))
}

#[allow(dead_code)]
const _MARKER: &str = "R649";

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn activation_armed(cutoff: DateTime<Utc>) -> WorktreeRunExecutionActivation {
        WorktreeRunExecutionActivation {
            armed: true,
            cutoff: Some(cutoff),
            activation_instance_id: Some("instance-1".into()),
            reason: None,
        }
    }

    fn activation_suppressed(reason: &'static str) -> WorktreeRunExecutionActivation {
        WorktreeRunExecutionActivation {
            armed: false,
            cutoff: None,
            activation_instance_id: None,
            reason: Some(reason),
        }
    }

    #[test]
    fn not_worktree_runtime_is_always_eligible() {
        let activation = activation_suppressed("flag_disabled");
        let eligibility = evaluate_automatic_dispatch_eligibility(
            false,
            &activation,
            Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap(),
        );
        assert!(eligibility.eligible);
        assert_eq!(
            eligibility.reason,
            AutomaticRoutineSuppressionReason::NotWorktreeRuntime
        );
    }

    #[test]
    fn worktree_runtime_with_disabled_flag_is_suppressed() {
        let activation = activation_suppressed("flag_disabled");
        let created_at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let eligibility = evaluate_automatic_dispatch_eligibility(true, &activation, created_at);
        assert!(!eligibility.eligible);
        assert_eq!(
            eligibility.reason,
            AutomaticRoutineSuppressionReason::FlagDisabled
        );
    }

    #[test]
    fn worktree_runtime_armed_with_post_cutoff_routine_is_eligible() {
        let cutoff = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let activation = activation_armed(cutoff);
        let created_at = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let eligibility = evaluate_automatic_dispatch_eligibility(true, &activation, created_at);
        assert!(eligibility.eligible);
        assert_eq!(
            eligibility.reason,
            AutomaticRoutineSuppressionReason::Eligible
        );
    }

    #[test]
    fn worktree_runtime_armed_with_pre_cutoff_routine_is_suppressed() {
        let cutoff = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let activation = activation_armed(cutoff);
        let created_at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let eligibility = evaluate_automatic_dispatch_eligibility(true, &activation, created_at);
        assert!(!eligibility.eligible);
        assert_eq!(
            eligibility.reason,
            AutomaticRoutineSuppressionReason::PreCutoffRoutine
        );
    }

    #[test]
    fn truthy_runtime_env_value_matches_node_semantics() {
        let cases = [("1", true), ("true", true), ("yes", true), ("on", true)];
        for (val, expected) in cases {
            assert_eq!(
                is_truthy_runtime_env_value(Some(val)),
                expected,
                "val={val}"
            );
        }
        let negatives = ["0", "false", "no", "off", ""];
        for val in negatives {
            assert!(!is_truthy_runtime_env_value(Some(val)), "val={val}");
        }
        assert!(!is_truthy_runtime_env_value(None));
    }
}
