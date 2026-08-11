//! Policy —— 增量/全量选择 + policy 评估。
//!
//! 与 Node `chooseStatusCardUpdateKind` / `evaluateStatusCardPolicy` 1:1 对齐。

use chrono::{DateTime, Utc};

use crate::schedule::is_within_status_card_active_hours;
use crate::types::{PolicyDecision, StatusCardRefreshPolicy, UpdateKind};
use crate::types::{
    DEFAULT_DAILY_TOKEN_CAP, DEFAULT_INTERVAL_MINUTES, DEFAULT_MAX_UPDATES_PER_HOUR,
    DEFAULT_REACTIVE_DEBOUNCE_SECONDS, REACTIVE_DEBOUNCE_MAX_SECONDS,
};

/// 决定 update 类别（与 Node `chooseStatusCardUpdateKind` 1:1 对齐）。
///
/// ## 规则（任一满足即 full）
///
/// - `explicit_full` 显式标记；
/// - 当前没有 document（`has_document == false`）；
/// - change 数 > 10；
/// - configuration 改变；
/// - `restore_refresh`；
/// - `last_update_query_version != query_version`；
/// - 增量计数 ≥ 9。
///
/// 其他情况返回 incremental。
pub fn choose_status_card_update_kind(input: &ChooseStatusCardUpdateKindInput) -> UpdateKind {
    if input.explicit_full
        || !input.has_document
        || input.change_count > 10
        || input.configuration_changed
        || input.restore_refresh
        || input.last_update_query_version != Some(input.query_version)
        || input.incremental_count >= 9
    {
        UpdateKind::Full
    } else {
        UpdateKind::Incremental
    }
}

/// `choose_status_card_update_kind` 入参（与 Node 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct ChooseStatusCardUpdateKindInput {
    pub explicit_full: bool,
    pub has_document: bool,
    pub change_count: usize,
    pub query_version: i32,
    pub last_update_query_version: Option<i32>,
    pub incremental_count: i32,
    pub configuration_changed: bool,
    pub restore_refresh: bool,
}

impl Default for ChooseStatusCardUpdateKindInput {
    fn default() -> Self {
        Self {
            explicit_full: false,
            has_document: true,
            change_count: 0,
            query_version: 1,
            last_update_query_version: Some(1),
            incremental_count: 0,
            configuration_changed: false,
            restore_refresh: false,
        }
    }
}

/// 评估 policy 当前应采取的动作（与 Node `evaluateStatusCardPolicy` 1:1 对齐）。
///
/// ## 决策顺序
///
/// 1. 若 `!manual && tokens_today >= cap` → `PauseBudget`。
/// 2. 若 `!manual && !is_within_active_hours` → `PauseHours`。
/// 3. 若 `manual` → `Run`。
/// 4. 若 `policy.mode == Manual` → `Wait`。
/// 5. 若 `policy.mode == Reactive`：
///    - `updates_last_hour >= max_updates_per_hour` → `Wait`。
///    - `last_change_at + debounce_seconds > now` → `Wait` (with due_at)。
/// 6. 其他（`Interval` 等）→ `Run`。
pub fn evaluate_status_card_policy(input: &EvaluateStatusCardPolicyInput) -> PolicyDecision {
    let cap = input
        .policy
        .daily_token_cap
        .map(|c| c as u64)
        .unwrap_or(DEFAULT_DAILY_TOKEN_CAP);

    if !input.manual && input.tokens_today >= cap {
        return PolicyDecision::PauseBudget;
    }
    if !input.manual && !is_within_status_card_active_hours(&input.policy, input.now) {
        return PolicyDecision::PauseHours;
    }
    if input.manual {
        return PolicyDecision::Run;
    }

    use crate::types::RefreshMode;
    match input.policy.mode {
        RefreshMode::Manual => PolicyDecision::Wait { due_at: None },
        RefreshMode::Reactive => {
            let max_per_hour = input
                .policy
                .max_updates_per_hour
                .unwrap_or(DEFAULT_MAX_UPDATES_PER_HOUR);
            if input.updates_last_hour >= max_per_hour {
                return PolicyDecision::Wait { due_at: None };
            }
            let debounce = input
                .policy
                .debounce_seconds
                .unwrap_or(DEFAULT_REACTIVE_DEBOUNCE_SECONDS);
            let anchor = input.last_change_at.unwrap_or(input.now);
            let due_at = anchor + chrono::Duration::seconds(debounce as i64);
            if due_at > input.now {
                PolicyDecision::Wait {
                    due_at: Some(due_at),
                }
            } else {
                PolicyDecision::Run
            }
        }
        RefreshMode::Interval => PolicyDecision::Run,
    }
}

/// `evaluate_status_card_policy` 入参。
#[derive(Debug, Clone)]
pub struct EvaluateStatusCardPolicyInput<'a> {
    pub policy: &'a StatusCardRefreshPolicy,
    pub now: DateTime<Utc>,
    pub last_change_at: Option<DateTime<Utc>>,
    pub updates_last_hour: u32,
    pub tokens_today: u64,
    pub manual: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ActiveHours, RefreshMode};

    #[test]
    fn r676_choose_update_kind_default_returns_incremental() {
        let input = ChooseStatusCardUpdateKindInput {
            has_document: true,
            change_count: 2,
            query_version: 3,
            last_update_query_version: Some(3),
            incremental_count: 2,
            ..Default::default()
        };
        assert_eq!(
            choose_status_card_update_kind(&input),
            UpdateKind::Incremental
        );
    }

    #[test]
    fn r676_choose_update_kind_too_many_changes_returns_full() {
        let input = ChooseStatusCardUpdateKindInput {
            change_count: 11,
            ..Default::default()
        };
        assert_eq!(choose_status_card_update_kind(&input), UpdateKind::Full);
    }

    #[test]
    fn r676_choose_update_kind_query_version_mismatch_returns_full() {
        let input = ChooseStatusCardUpdateKindInput {
            query_version: 4,
            last_update_query_version: Some(3),
            ..Default::default()
        };
        assert_eq!(choose_status_card_update_kind(&input), UpdateKind::Full);
    }

    #[test]
    fn r676_choose_update_kind_incremental_count_9_returns_full() {
        let input = ChooseStatusCardUpdateKindInput {
            incremental_count: 9,
            ..Default::default()
        };
        assert_eq!(choose_status_card_update_kind(&input), UpdateKind::Full);
    }

    #[test]
    fn r676_choose_update_kind_no_document_returns_full() {
        let input = ChooseStatusCardUpdateKindInput {
            has_document: false,
            ..Default::default()
        };
        assert_eq!(choose_status_card_update_kind(&input), UpdateKind::Full);
    }

    #[test]
    fn r676_choose_update_kind_configuration_changed_returns_full() {
        let input = ChooseStatusCardUpdateKindInput {
            configuration_changed: true,
            ..Default::default()
        };
        assert_eq!(choose_status_card_update_kind(&input), UpdateKind::Full);
    }

    #[test]
    fn r676_choose_update_kind_explicit_full_returns_full() {
        let input = ChooseStatusCardUpdateKindInput {
            explicit_full: true,
            ..Default::default()
        };
        assert_eq!(choose_status_card_update_kind(&input), UpdateKind::Full);
    }

    #[test]
    fn r676_choose_update_kind_restore_refresh_returns_full() {
        let input = ChooseStatusCardUpdateKindInput {
            restore_refresh: true,
            ..Default::default()
        };
        assert_eq!(choose_status_card_update_kind(&input), UpdateKind::Full);
    }

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn reactive_policy() -> StatusCardRefreshPolicy {
        StatusCardRefreshPolicy {
            mode: RefreshMode::Reactive,
            interval_minutes: None,
            debounce_seconds: Some(60),
            max_updates_per_hour: Some(6),
            triggers: Default::default(),
            active_hours: None,
            daily_token_cap: None,
        }
    }

    #[test]
    fn r676_evaluate_policy_reactive_within_debounce_returns_wait() {
        let policy = reactive_policy();
        let now = dt("2026-07-23T14:00:30Z");
        let input = EvaluateStatusCardPolicyInput {
            policy: &policy,
            now,
            last_change_at: Some(dt("2026-07-23T14:00:00Z")),
            updates_last_hour: 0,
            tokens_today: 0,
            manual: false,
        };
        match evaluate_status_card_policy(&input) {
            PolicyDecision::Wait { due_at } => {
                assert!(due_at.is_some());
            }
            _ => panic!("expected Wait"),
        }
    }

    #[test]
    fn r676_evaluate_policy_reactive_max_updates_returns_wait() {
        let policy = reactive_policy();
        let now = dt("2026-07-23T14:00:30Z");
        let input = EvaluateStatusCardPolicyInput {
            policy: &policy,
            now,
            last_change_at: Some(dt("2026-07-23T13:59:00Z")),
            updates_last_hour: 6,
            tokens_today: 0,
            manual: false,
        };
        assert!(matches!(
            evaluate_status_card_policy(&input),
            PolicyDecision::Wait { .. }
        ));
    }

    #[test]
    fn r676_evaluate_policy_tokens_over_cap_returns_pause_budget() {
        let policy = reactive_policy();
        let now = dt("2026-07-23T14:00:30Z");
        let input = EvaluateStatusCardPolicyInput {
            policy: &policy,
            now,
            last_change_at: Some(dt("2026-07-23T13:59:00Z")),
            updates_last_hour: 0,
            tokens_today: 100_000,
            manual: false,
        };
        assert!(matches!(
            evaluate_status_card_policy(&input),
            PolicyDecision::PauseBudget
        ));
    }

    #[test]
    fn r676_evaluate_policy_manual_override_returns_run() {
        let policy = reactive_policy();
        let now = dt("2026-07-23T14:00:30Z");
        let input = EvaluateStatusCardPolicyInput {
            policy: &policy,
            now,
            last_change_at: None,
            updates_last_hour: 99,
            tokens_today: 999_999,
            manual: true,
        };
        assert!(matches!(
            evaluate_status_card_policy(&input),
            PolicyDecision::Run
        ));
    }

    #[test]
    fn r676_evaluate_policy_outside_active_hours_returns_pause_hours() {
        let policy = StatusCardRefreshPolicy {
            mode: RefreshMode::Interval,
            interval_minutes: Some(15),
            triggers: Default::default(),
            active_hours: Some(ActiveHours {
                start: "09:00".to_string(),
                end: "17:00".to_string(),
                timezone: "UTC".to_string(),
            }),
            ..StatusCardRefreshPolicy::default_manual()
        };
        let input = EvaluateStatusCardPolicyInput {
            policy: &policy,
            now: dt("2026-07-23T18:00:00Z"),
            last_change_at: None,
            updates_last_hour: 0,
            tokens_today: 0,
            manual: false,
        };
        assert!(matches!(
            evaluate_status_card_policy(&input),
            PolicyDecision::PauseHours
        ));
    }

    #[test]
    fn r676_evaluate_policy_manual_mode_returns_wait() {
        let policy = StatusCardRefreshPolicy::default_manual();
        let now = dt("2026-07-23T14:00:00Z");
        let input = EvaluateStatusCardPolicyInput {
            policy: &policy,
            now,
            last_change_at: None,
            updates_last_hour: 0,
            tokens_today: 0,
            manual: false,
        };
        assert!(matches!(
            evaluate_status_card_policy(&input),
            PolicyDecision::Wait { .. }
        ));
    }

    #[test]
    fn r676_evaluate_policy_reactive_debounce_passed_returns_run() {
        let policy = reactive_policy();
        let now = dt("2026-07-23T14:01:30Z"); // 1.5 min after last change
        let input = EvaluateStatusCardPolicyInput {
            policy: &policy,
            now,
            last_change_at: Some(dt("2026-07-23T14:00:00Z")),
            updates_last_hour: 0,
            tokens_today: 0,
            manual: false,
        };
        assert!(matches!(
            evaluate_status_card_policy(&input),
            PolicyDecision::Run
        ));
    }
}
