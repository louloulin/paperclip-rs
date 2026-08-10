#![forbid(unsafe_code)]
//! `pc-issue-goal-fallback` —— 解析 issue 的 goal id。
//!
//! 对应 Node `server/src/services/issue-goal-fallback.ts`（56 行）。
//!
//! 设计目标：1:1 复刻
//! - `resolveIssueGoalId({projectId, goalId, projectGoalId, defaultGoalId})` ——
//!   优先级：`goalId` > (有 project 时) `projectGoalId` > `defaultGoalId`
//! - `resolveNextIssueGoalId(...)` —— 把 "current vs next" 上下文做组合：
//!   - 显式 `goalId` → 直接用它；否则 fallback
//!   - 没有 currentGoalId → nextFallback
//!   - currentGoalId === currentFallback → nextFallback
//!   - 否则保留 currentGoalId

/// 输入 —— 与 Node 入参 1:1 对齐。
#[derive(Debug, Clone, Default)]
pub struct ResolveGoalInput {
    pub project_id: Option<String>,
    pub goal_id: Option<String>,
    pub project_goal_id: Option<String>,
    pub default_goal_id: Option<String>,
}

/// 解析 issue 的 goal id。
///
/// 与 Node `resolveIssueGoalId` 1:1 对齐。
pub fn resolve_issue_goal_id(input: ResolveGoalInput) -> Option<String> {
    if input.goal_id.is_some() {
        return input.goal_id;
    }
    if input.project_id.is_some() {
        return input.project_goal_id;
    }
    input.default_goal_id
}

/// "current vs next" 上下文 —— 用于跨项目/goal 迁移时复用 current goal。
#[derive(Debug, Clone, Default)]
pub struct ResolveNextGoalInput {
    pub current_project_id: Option<String>,
    pub current_goal_id: Option<String>,
    pub current_project_goal_id: Option<String>,
    pub project_id: Option<String>,
    pub goal_id: Option<String>,
    pub project_goal_id: Option<String>,
    pub default_goal_id: Option<String>,
}

/// 解析 "next" issue 的 goal id。
///
/// 与 Node `resolveNextIssueGoalId` 1:1 对齐。
pub fn resolve_next_issue_goal_id(input: ResolveNextGoalInput) -> Option<String> {
    let project_id = match input.project_id {
        Some(p) => Some(p),
        None => input.current_project_id.clone(),
    };
    let project_goal_id = match input.project_goal_id {
        Some(pg) => Some(pg),
        None => {
            if project_id.is_some() {
                input.current_project_goal_id.clone()
            } else {
                None
            }
        }
    };

    let resolve_fallback = |target_project_id: Option<String>, target_project_goal_id: Option<String>| {
        if target_project_id.is_some() {
            target_project_goal_id
        } else {
            input.default_goal_id.clone()
        }
    };

    if input.goal_id.is_some() {
        // explicit override; fallback if null
        if input.goal_id.is_some() {
            return input.goal_id.clone().or_else(|| resolve_fallback(project_id.clone(), project_goal_id.clone()));
        }
    }

    let current_fallback = resolve_fallback(input.current_project_id.clone(), input.current_project_goal_id.clone());
    let next_fallback = resolve_fallback(project_id.clone(), project_goal_id.clone());

    if input.current_goal_id.is_none() {
        return next_fallback;
    }
    if input.current_goal_id == current_fallback {
        return next_fallback;
    }
    input.current_goal_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r704_resolve_uses_explicit_goal_id() {
        let r = resolve_issue_goal_id(ResolveGoalInput {
            project_id: Some("p".into()),
            goal_id: Some("g".into()),
            project_goal_id: Some("pg".into()),
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("g"));
    }

    #[test]
    fn r704_resolve_uses_project_goal_when_no_goal_id() {
        let r = resolve_issue_goal_id(ResolveGoalInput {
            project_id: Some("p".into()),
            goal_id: None,
            project_goal_id: Some("pg".into()),
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("pg"));
    }

    #[test]
    fn r704_resolve_uses_default_when_no_project() {
        let r = resolve_issue_goal_id(ResolveGoalInput {
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("d"));
    }

    #[test]
    fn r704_resolve_project_without_project_goal_returns_null() {
        let r = resolve_issue_goal_id(ResolveGoalInput {
            project_id: Some("p".into()),
            goal_id: None,
            project_goal_id: None,
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r, None);
    }

    #[test]
    fn r704_resolve_no_project_no_goal_returns_default() {
        let r = resolve_issue_goal_id(ResolveGoalInput {
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("d"));
    }

    #[test]
    fn r704_next_explicit_goal_wins() {
        let r = resolve_next_issue_goal_id(ResolveNextGoalInput {
            current_project_id: Some("cp".into()),
            current_goal_id: Some("cg".into()),
            current_project_goal_id: Some("cpg".into()),
            project_id: Some("np".into()),
            goal_id: Some("ng".into()),
            project_goal_id: Some("npg".into()),
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("ng"));
    }

    #[test]
    fn r704_next_explicit_null_goal_falls_back() {
        // 当 goal_id 是 Some(None) 不可能（类型上）；这里 goal_id = None 表示 "无 override"
        // 走 no current goal 分支
        let r = resolve_next_issue_goal_id(ResolveNextGoalInput {
            current_project_id: Some("cp".into()),
            current_goal_id: None,
            current_project_goal_id: Some("cpg".into()),
            project_id: Some("np".into()),
            goal_id: None,
            project_goal_id: Some("npg".into()),
            default_goal_id: Some("d".into()),
        });
        // 没有 current goal → next fallback = projectGoalId
        assert_eq!(r.as_deref(), Some("npg"));
    }

    #[test]
    fn r704_next_no_current_no_explicit_uses_next_fallback() {
        let r = resolve_next_issue_goal_id(ResolveNextGoalInput {
            current_project_id: None,
            current_goal_id: None,
            current_project_goal_id: None,
            project_id: Some("np".into()),
            goal_id: None,
            project_goal_id: Some("npg".into()),
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("npg"));
    }

    #[test]
    fn r704_next_current_equals_current_fallback_uses_next_fallback() {
        let r = resolve_next_issue_goal_id(ResolveNextGoalInput {
            current_project_id: Some("cp".into()),
            current_goal_id: Some("cpg".into()),  // === current fallback (project_goal)
            current_project_goal_id: Some("cpg".into()),
            project_id: Some("np".into()),
            goal_id: None,
            project_goal_id: Some("npg".into()),
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("npg"));
    }

    #[test]
    fn r704_next_current_differs_from_fallback_keeps_current() {
        let r = resolve_next_issue_goal_id(ResolveNextGoalInput {
            current_project_id: Some("cp".into()),
            current_goal_id: Some("manual-goal".into()),  // not fallback
            current_project_goal_id: Some("cpg".into()),
            project_id: Some("np".into()),
            goal_id: None,
            project_goal_id: Some("npg".into()),
            default_goal_id: Some("d".into()),
        });
        assert_eq!(r.as_deref(), Some("manual-goal"));
    }

    #[test]
    fn r704_next_no_project_anywhere_uses_default() {
        let r = resolve_next_issue_goal_id(ResolveNextGoalInput {
            current_project_id: None,
            current_goal_id: Some("cg".into()),
            current_project_goal_id: None,
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: Some("d".into()),
        });
        // current_fallback = default, current_goal != current_fallback → keep current
        assert_eq!(r.as_deref(), Some("cg"));
    }

    #[test]
    fn r704_next_project_id_undefined_keeps_current_project() {
        let r = resolve_next_issue_goal_id(ResolveNextGoalInput {
            current_project_id: Some("cp".into()),
            current_goal_id: Some("cg".into()),
            current_project_goal_id: Some("cpg".into()),
            project_id: None, // undefined → keep current
            goal_id: None,
            project_goal_id: None, // undefined → fall back to currentProjectGoalId
            default_goal_id: Some("d".into()),
        });
        // current_fallback = cpg (project_goal)
        // current_goal != current_fallback → keep current
        assert_eq!(r.as_deref(), Some("cg"));
    }
}
