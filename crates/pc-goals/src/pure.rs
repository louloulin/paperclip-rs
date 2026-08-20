#![forbid(unsafe_code)]

//! Goal pure helpers — 1:1 port of paperclip/server/src/services/goals.ts
//!
//! R725: zero-DB helpers for default-company-goal selection and validation.
//! Service-layer callers (e.g. `GoalService`) compose these without touching SQL.

use pc_repos::goal::{GoalLevel, GoalStatus, GoalRow};

/// Allowed status transitions: `planned -> active -> completed | cancelled`.
/// `blocked` is a side state reachable from `active` and returns to `active`.
/// Terminal states (`completed` / `cancelled`) are sticky.
pub fn is_allowed_status_transition(from: GoalStatus, to: GoalStatus) -> bool {
    use GoalStatus::*;
    match (from, to) {
        (a, b) if a == b => true,
        (Planned, Active) => true,
        (Active, Completed) => true,
        (Active, Cancelled) => true,
        (Planned, Cancelled) => true,
        (Active, Blocked) => true,
        (Blocked, Active) => true,
        (Blocked, Cancelled) => true,
        _ => false,
    }
}

/// Validate that a goal title is acceptable (non-empty, max length).
pub fn is_valid_goal_title(title: &str, max_len: usize) -> bool {
    let trimmed = title.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= max_len
}

/// Validate that a goal level is allowed (mission / company / team / project / task).
pub fn is_valid_goal_level(level: GoalLevel) -> bool {
    matches!(
        level,
        GoalLevel::Mission
            | GoalLevel::Company
            | GoalLevel::Team
            | GoalLevel::Project
            | GoalLevel::Task
    )
}

/// Check whether a parent_id belongs to the same company as the child goal.
///
/// Node parity: validate that `parent_id.company_id == self.company_id`.
pub fn parent_id_matches_company(goal_company_id: uuid::Uuid, parent_company_id: uuid::Uuid) -> bool {
    goal_company_id == parent_company_id
}

/// Default company goal selection (Node `getDefaultCompanyGoal` priority):
/// 1. Active root (`status = active`, `parent_id IS NULL`)
/// 2. Any root (regardless of status)
/// 3. Any company-level goal (regardless of parent)
///
/// Goals must be ordered by `created_at` ascending within each tier.
/// Tier is the first non-empty list (short-circuit semantics).
pub fn select_default_company_goal<'a>(
    active_roots: &'a [GoalRow],
    any_roots: &'a [GoalRow],
    any_company_level: &'a [GoalRow],
) -> Option<&'a GoalRow> {
    if !active_roots.is_empty() {
        active_roots.first()
    } else if !any_roots.is_empty() {
        any_roots.first()
    } else {
        any_company_level.first()
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use pc_repos::goal::{GoalLevel, GoalStatus};
    use uuid::Uuid;

    fn row(level: GoalLevel, status: GoalStatus, parent_id: Option<Uuid>) -> GoalRow {
        GoalRow {
            id: Uuid::new_v4(),
            company_id: Uuid::nil(),
            title: "test".into(),
            description: None,
            level: level.as_str().to_string(),
            status: status.as_str().to_string(),
            parent_id,
            owner_agent_id: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        }
    }

    #[test]
    fn status_transition_allowed() {
        assert!(is_allowed_status_transition(GoalStatus::Planned, GoalStatus::Active));
        assert!(is_allowed_status_transition(GoalStatus::Active, GoalStatus::Completed));
        assert!(is_allowed_status_transition(GoalStatus::Active, GoalStatus::Cancelled));
        assert!(is_allowed_status_transition(GoalStatus::Active, GoalStatus::Blocked));
        assert!(is_allowed_status_transition(GoalStatus::Blocked, GoalStatus::Active));
        assert!(is_allowed_status_transition(GoalStatus::Planned, GoalStatus::Cancelled));
    }

    #[test]
    fn status_transition_disallowed() {
        // Terminal sticky: completed / cancelled cannot go back
        assert!(!is_allowed_status_transition(GoalStatus::Completed, GoalStatus::Active));
        assert!(!is_allowed_status_transition(GoalStatus::Cancelled, GoalStatus::Active));
        // Skip planned -> completed
        assert!(!is_allowed_status_transition(GoalStatus::Planned, GoalStatus::Completed));
        // Random
        assert!(!is_allowed_status_transition(GoalStatus::Blocked, GoalStatus::Completed));
    }

    #[test]
    fn title_validation() {
        assert!(is_valid_goal_title("hello", 64));
        assert!(is_valid_goal_title("  hello  ", 64));
        assert!(!is_valid_goal_title("", 64));
        assert!(!is_valid_goal_title("   ", 64));
        assert!(!is_valid_goal_title(&"a".repeat(65), 64));
        assert!(is_valid_goal_title(&"a".repeat(64), 64));
    }

    #[test]
    fn level_validation_all_levels_allowed() {
        for level in [
            GoalLevel::Mission,
            GoalLevel::Company,
            GoalLevel::Team,
            GoalLevel::Project,
            GoalLevel::Task,
        ] {
            assert!(is_valid_goal_level(level));
        }
    }

    #[test]
    fn parent_id_matches_company_test() {
        let c = Uuid::new_v4();
        assert!(parent_id_matches_company(c, c));
        assert!(!parent_id_matches_company(c, Uuid::new_v4()));
    }

    #[test]
    fn select_default_prefers_active_root() {
        let active = row(GoalLevel::Company, GoalStatus::Active, None);
        let inactive_root = row(GoalLevel::Company, GoalStatus::Completed, None);
        let company_level = row(GoalLevel::Company, GoalStatus::Active, Some(Uuid::new_v4()));
        let active_list = [active.clone()];
        let inactive_list = [inactive_root];
        let company_list = [company_level];
        let r = select_default_company_goal(&active_list, &inactive_list, &company_list);
        assert_eq!(r.map(|g| g.id), Some(active.id));
    }

    #[test]
    fn select_default_falls_back_to_any_root() {
        let inactive_root = row(GoalLevel::Company, GoalStatus::Completed, None);
        let company_level = row(GoalLevel::Company, GoalStatus::Active, Some(Uuid::new_v4()));
        let inactive_list = [inactive_root.clone()];
        let company_list = [company_level];
        let r = select_default_company_goal(&[], &inactive_list, &company_list);
        assert_eq!(r.map(|g| g.id), Some(inactive_root.id));
    }

    #[test]
    fn select_default_falls_back_to_company_level() {
        let company_level = row(GoalLevel::Company, GoalStatus::Active, Some(Uuid::new_v4()));
        let company_list = [company_level.clone()];
        let r = select_default_company_goal(&[], &[], &company_list);
        assert_eq!(r.map(|g| g.id), Some(company_level.id));
    }

    #[test]
    fn select_default_none_when_empty() {
        assert!(select_default_company_goal(&[], &[], &[]).is_none());
    }
}