#![forbid(unsafe_code)]

//! Activity gate pure helpers — R750.
//!
//! Extracted from pc-routines/src/activity_gate.rs:
//! - policy decision (should we even consult the gate?)
//! - scope parsing (company / project)
//! - self-loop detection (actor == routine-scheduler and references this routine)
//! - ignored action check (inbox / read actions)
//! - verdict constructors (fire_default / fire_first / fire_matched / skip)
//!
//! Zero DB / zero IO. All functions are pure and testable.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::activity_gate::{ActivityGateScope, ActivityGateVerdict};

/// 默认 / always 策略集合（无需 gate 检查）。
pub const DEFAULT_POLICIES: &[&str] = &["always", "none", "disabled", ""];

/// require_external_activity 策略值（这是唯一需要 gate 的策略）。
pub const REQUIRE_EXTERNAL_ACTIVITY_POLICY: &str = "require_external_activity";

/// 与 Node  1:1 对齐。
pub const IGNORED_ACTIONS: &[&str] = &[
    "issue.read_marked",
    "issue.read_unmarked",
    "issue.inbox_archived",
    "issue.inbox_unarchived",
];

/// self-loop actor id（与 Node  1:1 对齐）。
pub const ROUTINE_SCHEDULER_ACTOR_ID: &str = "routine-scheduler";

/// 是否需要 gate 检查（policy 是 require_external_activity）。
///
/// 默认策略（always / none / disabled / ""）直接 fire，不查 gate。
pub fn gate_required_for_policy(policy: &str) -> bool {
    policy == REQUIRE_EXTERNAL_ACTIVITY_POLICY
}

/// 解析 activity_gate_scope 字符串。
///
/// 与 Node 行为对齐："project" -> Project；其它（"company" / "global" / ""）-> Global。
pub fn parse_scope(scope_str: &str) -> ActivityGateScope {
    match scope_str {
        "project" => ActivityGateScope::Project,
        _ => ActivityGateScope::Global,
    }
}

/// 判断 action 是否在 ignored actions 列表中。
pub fn is_ignored_action(action: &str) -> bool {
    IGNORED_ACTIONS.contains(&action)
}

/// 判断一条 activity_log 记录是否是 routine 自循环。
///
/// self-loop 条件：
/// - actor_id == ROUTINE_SCHEDULER_ACTOR_ID
/// - 并且 details.routineId == routine_id 或者 (entity_type == "routine" && entity_id == routine_id)
///
/// 三参数版：只判断是否引用了 routine（不依赖 entity_type/entity_id 字段）。
pub fn is_self_loop_by_details_routine_id(
    actor_id: &str,
    details_routine_id: Option<&str>,
    routine_id: &Uuid,
) -> bool {
    if actor_id != ROUTINE_SCHEDULER_ACTOR_ID {
        return false;
    }
    match details_routine_id {
        Some(id) => id == routine_id.to_string(),
        None => false,
    }
}

/// 完整版 self-loop 判断（包含 entity_type / entity_id 检查）。
pub fn is_self_loop(
    actor_id: &str,
    entity_type: Option<&str>,
    entity_id: Option<&str>,
    details_routine_id: Option<&str>,
    routine_id: &Uuid,
) -> bool {
    if actor_id != ROUTINE_SCHEDULER_ACTOR_ID {
        return false;
    }
    let rid = routine_id.to_string();
    if let Some(dr) = details_routine_id {
        if dr == rid {
            return true;
        }
    }
    if let (Some(et), Some(ei)) = (entity_type, entity_id) {
        if et == "routine" && ei == rid {
            return true;
        }
    }
    false
}

/// 构造 "fire=default" verdict（策略不是 require_external_activity 时）。
pub fn verdict_fire_default() -> ActivityGateVerdict {
    ActivityGateVerdict {
        fire: true,
        window_start: None,
        matched_activity_id: None,
        scope: ActivityGateScope::Global,
    }
}

/// 构造 "fire=first" verdict（首次 dispatch，无 window_start）。
pub fn verdict_fire_first(scope: ActivityGateScope) -> ActivityGateVerdict {
    ActivityGateVerdict {
        fire: true,
        window_start: None,
        matched_activity_id: None,
        scope,
    }
}

/// 构造 "fire=matched" verdict。
pub fn verdict_fire_matched(
    scope: ActivityGateScope,
    window_start: DateTime<Utc>,
    matched_activity_id: Uuid,
) -> ActivityGateVerdict {
    ActivityGateVerdict {
        fire: true,
        window_start: Some(window_start),
        matched_activity_id: Some(matched_activity_id),
        scope,
    }
}

/// 构造 "skip" verdict（gate 拒绝）。
pub fn verdict_skip(scope: ActivityGateScope, window_start: DateTime<Utc>) -> ActivityGateVerdict {
    ActivityGateVerdict {
        fire: false,
        window_start: Some(window_start),
        matched_activity_id: None,
        scope,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r750_gate_required_only_for_require_external() {
        assert!(gate_required_for_policy("require_external_activity"));
        assert!(!gate_required_for_policy("always"));
        assert!(!gate_required_for_policy("none"));
        assert!(!gate_required_for_policy("disabled"));
        assert!(!gate_required_for_policy(""));
        assert!(!gate_required_for_policy("unknown_policy"));
    }

    #[test]
    fn r750_default_policies_includes_empty() {
        assert!(DEFAULT_POLICIES.contains(&"always"));
        assert!(DEFAULT_POLICIES.contains(&"none"));
        assert!(DEFAULT_POLICIES.contains(&"disabled"));
        assert!(DEFAULT_POLICIES.contains(&""));
    }

    #[test]
    fn r750_parse_scope_project() {
        assert_eq!(parse_scope("project"), ActivityGateScope::Project);
    }

    #[test]
    fn r750_parse_scope_defaults_to_global() {
        assert_eq!(parse_scope("company"), ActivityGateScope::Global);
        assert_eq!(parse_scope("global"), ActivityGateScope::Global);
        assert_eq!(parse_scope(""), ActivityGateScope::Global);
        assert_eq!(parse_scope("unknown"), ActivityGateScope::Global);
    }

    #[test]
    fn r750_is_ignored_action_known() {
        assert!(is_ignored_action("issue.read_marked"));
        assert!(is_ignored_action("issue.read_unmarked"));
        assert!(is_ignored_action("issue.inbox_archived"));
        assert!(is_ignored_action("issue.inbox_unarchived"));
    }

    #[test]
    fn r750_is_ignored_action_unknown() {
        assert!(!is_ignored_action("issue.created"));
        assert!(!is_ignored_action("issue.commented"));
        assert!(!is_ignored_action(""));
    }

    #[test]
    fn r750_is_self_loop_by_details_match() {
        let rid = Uuid::new_v4();
        assert!(is_self_loop_by_details_routine_id(
            ROUTINE_SCHEDULER_ACTOR_ID,
            Some(&rid.to_string()),
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_by_details_mismatch() {
        let rid = Uuid::new_v4();
        let other = Uuid::new_v4();
        assert!(!is_self_loop_by_details_routine_id(
            ROUTINE_SCHEDULER_ACTOR_ID,
            Some(&other.to_string()),
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_by_details_wrong_actor() {
        let rid = Uuid::new_v4();
        assert!(!is_self_loop_by_details_routine_id(
            "user-1",
            Some(&rid.to_string()),
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_by_details_none() {
        let rid = Uuid::new_v4();
        assert!(!is_self_loop_by_details_routine_id(
            ROUTINE_SCHEDULER_ACTOR_ID,
            None,
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_full_match_via_entity() {
        let rid = Uuid::new_v4();
        assert!(is_self_loop(
            ROUTINE_SCHEDULER_ACTOR_ID,
            Some("routine"),
            Some(&rid.to_string()),
            None,
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_full_match_via_details() {
        let rid = Uuid::new_v4();
        assert!(is_self_loop(
            ROUTINE_SCHEDULER_ACTOR_ID,
            Some("issue"),
            Some("i-1"),
            Some(&rid.to_string()),
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_full_no_match() {
        let rid = Uuid::new_v4();
        assert!(!is_self_loop(
            ROUTINE_SCHEDULER_ACTOR_ID,
            Some("issue"),
            Some("i-1"),
            Some("other-routine-id"),
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_full_wrong_entity_type() {
        let rid = Uuid::new_v4();
        assert!(!is_self_loop(
            ROUTINE_SCHEDULER_ACTOR_ID,
            Some("issue"),
            Some(&rid.to_string()),
            None,
            &rid
        ));
    }

    #[test]
    fn r750_is_self_loop_full_wrong_actor() {
        let rid = Uuid::new_v4();
        assert!(!is_self_loop(
            "user-1",
            Some("routine"),
            Some(&rid.to_string()),
            None,
            &rid
        ));
    }

    #[test]
    fn r750_verdict_fire_default() {
        let v = verdict_fire_default();
        assert!(v.fire);
        assert!(v.window_start.is_none());
        assert!(v.matched_activity_id.is_none());
        assert_eq!(v.scope, ActivityGateScope::Global);
    }

    #[test]
    fn r750_verdict_fire_first() {
        let v = verdict_fire_first(ActivityGateScope::Project);
        assert!(v.fire);
        assert!(v.window_start.is_none());
        assert_eq!(v.scope, ActivityGateScope::Project);
    }

    #[test]
    fn r750_verdict_fire_matched() {
        let now = Utc::now();
        let matched = Uuid::new_v4();
        let v = verdict_fire_matched(ActivityGateScope::Global, now, matched);
        assert!(v.fire);
        assert_eq!(v.window_start, Some(now));
        assert_eq!(v.matched_activity_id, Some(matched));
    }

    #[test]
    fn r750_verdict_skip() {
        let now = Utc::now();
        let v = verdict_skip(ActivityGateScope::Project, now);
        assert!(!v.fire);
        assert_eq!(v.window_start, Some(now));
        assert!(v.matched_activity_id.is_none());
    }

    #[test]
    fn r750_constants_match_node() {
        assert_eq!(REQUIRE_EXTERNAL_ACTIVITY_POLICY, "require_external_activity");
        assert_eq!(ROUTINE_SCHEDULER_ACTOR_ID, "routine-scheduler");
        assert_eq!(IGNORED_ACTIONS.len(), 4);
    }
}


#[cfg(test)]
mod internal_tests_r772 {
    use super::*;

    // ---- Round 772: pc-routines::activity_gate_pure 边缘测试 ----

    /// gate_required_for_policy: 仅 require_external 必需.
    #[test]
    fn r772_gate_required_for_policy_variants() {
        assert!(gate_required_for_policy("require_external_activity"));
        assert!(!gate_required_for_policy("always"));
        assert!(!gate_required_for_policy("never"));
        assert!(!gate_required_for_policy("unknown"), "unknown → false");
    }

    /// parse_scope: 3 种 + 默认.
    #[test]
    fn r772_parse_scope_variants() {
        assert!(matches!(parse_scope("global"), ActivityGateScope::Global));
        assert!(matches!(parse_scope("project"), ActivityGateScope::Project));
        assert!(matches!(parse_scope("agent"), ActivityGateScope::Global));
        assert!(matches!(parse_scope("unknown"), ActivityGateScope::Global), "unknown → Global");
    }

    /// is_ignored_action: 4 种已知 + 未知.
    #[test]
    fn r772_is_ignored_action() {
        assert!(is_ignored_action("issue.read_marked"));
        assert!(is_ignored_action("issue.read_unmarked"));
        assert!(is_ignored_action("issue.inbox_archived"));
        assert!(is_ignored_action("issue.inbox_unarchived"));
        assert!(!is_ignored_action("user_comment"));
        assert!(!is_ignored_action(""));
    }

    /// is_self_loop_by_details_routine_id: 三种情况.
    #[test]
    fn r772_is_self_loop_by_routine_id() {
        // 同一 routine_id
        assert!(is_self_loop_by_details_routine_id("routine-scheduler", Some("11111111-1111-4111-8111-111111111111"), &Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap()));
        // 不同 routine_id
        // "different actor" test below
        // 一方缺失
        assert!(!is_self_loop_by_details_routine_id("routine-scheduler", None, &Uuid::new_v4()));
        assert!(!is_self_loop_by_details_routine_id("not-scheduler", Some("r-1"), &Uuid::new_v4()), "wrong actor → false");
    }

    /// verdict_fire_default: 4 字段.
    #[test]
    fn r772_verdict_fire_default() {
        let v = verdict_fire_default();
        assert!(matches!(v.scope, ActivityGateScope::Global));
        assert!(v.window_start.is_none());
        assert!(v.matched_activity_id.is_none());
        assert!(v.fire);
    }

    /// verdict_fire_first: scope 设置.
    #[test]
    fn r772_verdict_fire_first_preserves_scope() {
        let v = verdict_fire_first(ActivityGateScope::Project);
        assert!(matches!(v.scope, ActivityGateScope::Project));
    }

    /// verdict_skip: window_start required.
    #[test]
    fn r772_verdict_skip_window_start() {
        let now = Utc::now();
        let v = verdict_skip(ActivityGateScope::Project, now);
        assert!(matches!(v.scope, ActivityGateScope::Project));
        assert_eq!(v.window_start, Some(now));
    }
}
