//! Issue goal fallback resolution (1:1 port of Node `server/src/services/issue-goal-fallback.ts`, 56 行).
//!
//! 单一职责：在 issue 上写 / 改 goal 时，按 fallback 链解析最终 goal_id。
//!
//! 两个公开函数：
//! - `resolve_issue_goal_id` —— 单点解析：传入 goalId / projectId / projectGoalId / defaultGoalId，
//!   返回最终 goalId（可能 `null`）
//! - `resolve_next_issue_goal_id` —— 状态迁移解析：当前与目标各 4 个字段，
//!   按 Node 端算法选出 next goalId
//!
//! 不持有任何状态；不依赖 IO。

/// `MaybeId` 与 Node `string | null | undefined` 1:1 对齐。
pub type MaybeId = Option<String>;

/// Issue goal 解析输入。
///
/// 与 Node `resolveIssueGoalId` 入参 1:1 对齐。
#[derive(Debug, Clone, Default)]
pub struct ResolveIssueGoalIdInput {
    pub project_id: MaybeId,
    pub goal_id: MaybeId,
    pub project_goal_id: MaybeId,
    pub default_goal_id: MaybeId,
}

/// 单点解析 issue goal_id。
///
/// 行为（与 Node `resolveIssueGoalId` 1:1 对齐）：
/// 1. 若 `goal_id` 非空 → 返回 `goal_id`
/// 2. 否则若 `project_id` 非空 → 返回 `project_goal_id ?? null`
/// 3. 否则返回 `default_goal_id ?? null`
#[must_use]
pub fn resolve_issue_goal_id(input: ResolveIssueGoalIdInput) -> Option<String> {
    if let Some(gid) = input.goal_id {
        return Some(gid);
    }
    if input.project_id.is_some() {
        return input.project_goal_id;
    }
    input.default_goal_id
}

/// Issue goal 状态迁移解析输入（current + next）。
///
/// 与 Node `resolveNextIssueGoalId` 入参 1:1 对齐（current 4 字段 + next 4 字段）。
#[derive(Debug, Clone, Default)]
pub struct ResolveNextIssueGoalIdInput {
    // current
    pub current_project_id: MaybeId,
    pub current_goal_id: MaybeId,
    pub current_project_goal_id: MaybeId,
    // next（可选；`None` 视为未提供，fallback 到 current）
    pub project_id: MaybeId,
    /// `Option<Option<String>>` 三态：
    /// - `None` —— 未提供（对应 Node `undefined`）
    /// - `Some(None)` —— 显式 null（对应 Node `null`）
    /// - `Some(Some(s))` —— 显式字符串
    pub goal_id: Option<Option<String>>,
    pub project_goal_id: MaybeId,
    // 默认（必填字段语义）
    pub default_goal_id: MaybeId,
}

/// 状态迁移解析 next issue goal_id。
///
/// 行为（与 Node `resolveNextIssueGoalId` 1:1 对齐）：
/// 1. `projectId` 缺省回退到 `currentProjectId`；`projectGoalId` 缺省回退：
///    - 若 `projectId` 存在 → `currentProjectGoalId`
///    - 否则 → `None`
/// 2. 若 `goalId` 显式提供 → 返回 `goalId ?? resolveFallbackGoalId(projectId, projectGoalId)`
/// 3. 否则若 `currentGoalId` 不存在 → 返回 next fallback
/// 4. 否则若 `currentGoalId === currentFallbackGoalId` → 返回 next fallback
/// 5. 否则返回 `currentGoalId`
#[must_use]
pub fn resolve_next_issue_goal_id(input: ResolveNextIssueGoalIdInput) -> Option<String> {
    let project_id = input
        .project_id
        .clone()
        .or_else(|| input.current_project_id.clone());
    let project_goal_id = if input.project_goal_id.is_some() {
        input.project_goal_id.clone()
    } else if project_id.is_some() {
        input.current_project_goal_id.clone()
    } else {
        None
    };

    // Use a helper that takes default as a parameter to avoid capturing `input`
    // by reference (we may have moved some fields out of `input` above).
    fn fallback_goal_id(
        target_project_id: MaybeId,
        target_project_goal_id: MaybeId,
        default_goal_id: MaybeId,
    ) -> Option<String> {
        if target_project_id.is_some() {
            target_project_goal_id
        } else {
            default_goal_id
        }
    }

    if input.goal_id.is_some() {
        // Node: `input.goalId !== undefined` → 走显式分支
        // 然后 `input.goalId ?? resolveFallbackGoalId(projectId, projectGoalId)`
        // unwrap outer Some 得到内层 Option<String>，再 or_else 走 fallback
        return input.goal_id.unwrap().or_else(|| {
            fallback_goal_id(
                project_id.clone(),
                project_goal_id.clone(),
                input.default_goal_id.clone(),
            )
        });
    }

    let current_fallback_goal_id = fallback_goal_id(
        input.current_project_id.clone(),
        input.current_project_goal_id.clone(),
        input.default_goal_id.clone(),
    );
    let next_fallback_goal_id = fallback_goal_id(
        project_id.clone(),
        project_goal_id.clone(),
        input.default_goal_id.clone(),
    );

    if input.current_goal_id.is_none() {
        return next_fallback_goal_id;
    }
    if input.current_goal_id == current_fallback_goal_id {
        return next_fallback_goal_id;
    }
    input.current_goal_id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> MaybeId {
        Some(v.to_string())
    }

    // ---- resolve_issue_goal_id ----

    #[test]
    fn resolve_issue_goal_id_returns_explicit_goal_id() {
        let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
            project_id: s("p1"),
            goal_id: s("g-explicit"),
            project_goal_id: s("pg1"),
            default_goal_id: s("d1"),
        });
        assert_eq!(out, Some("g-explicit".to_string()));
    }

    #[test]
    fn resolve_issue_goal_id_uses_project_goal_when_no_goal_id() {
        let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
            project_id: s("p1"),
            goal_id: None,
            project_goal_id: s("pg1"),
            default_goal_id: s("d1"),
        });
        assert_eq!(out, Some("pg1".to_string()));
    }

    #[test]
    fn resolve_issue_goal_id_returns_null_project_goal_when_project_no_goal() {
        let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
            project_id: s("p1"),
            goal_id: None,
            project_goal_id: None,
            default_goal_id: s("d1"),
        });
        assert_eq!(out, None);
    }

    #[test]
    fn resolve_issue_goal_id_uses_default_when_no_project() {
        let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
            project_id: None,
            goal_id: None,
            project_goal_id: s("pg1"),
            default_goal_id: s("d1"),
        });
        assert_eq!(out, Some("d1".to_string()));
    }

    #[test]
    fn resolve_issue_goal_id_returns_none_when_nothing() {
        let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: None,
        });
        assert_eq!(out, None);
    }

    #[test]
    fn resolve_issue_goal_id_goal_id_beats_project_and_default() {
        // goalId 优先级最高
        let out = resolve_issue_goal_id(ResolveIssueGoalIdInput {
            project_id: None,
            goal_id: s("g-explicit"),
            project_goal_id: None,
            default_goal_id: None,
        });
        assert_eq!(out, Some("g-explicit".to_string()));
    }

    // ---- resolve_next_issue_goal_id ----

    #[test]
    fn resolve_next_explicit_goal_id_wins() {
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: s("cp"),
            current_goal_id: s("cg"),
            current_project_goal_id: s("cpg"),
            project_id: Some("p".into()),
            goal_id: Some(Some("g".into())),
            project_goal_id: Some("pg".into()),
            default_goal_id: s("d"),
        });
        assert_eq!(out, Some("g".to_string()));
    }

    #[test]
    fn resolve_next_explicit_goal_id_can_be_null_falls_back() {
        // Node: `input.goalId ?? resolveFallbackGoalId(projectId, projectGoalId)`
        // 显式 null（Some(None)）走 fallback
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: s("cp"),
            current_goal_id: s("cg"),
            current_project_goal_id: s("cpg"),
            project_id: Some("p".into()),
            goal_id: Some(None), // 显式提供但为 null → 走 fallback
            project_goal_id: Some("pg".into()),
            default_goal_id: s("d"),
        });
        // project_id 存在 → fallback 返回 project_goal_id
        assert_eq!(out, Some("pg".to_string()));
    }

    #[test]
    fn resolve_next_no_current_goal_returns_next_fallback() {
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: s("cp"),
            current_goal_id: None,
            current_project_goal_id: s("cpg"),
            project_id: Some("p".into()),
            goal_id: None, // 未提供 → 走状态迁移分支
            project_goal_id: Some("pg".into()),
            default_goal_id: s("d"),
        });
        // next fallback: projectId 存在 → projectGoalId
        assert_eq!(out, Some("pg".to_string()));
    }

    #[test]
    fn resolve_next_current_goal_equals_current_fallback_returns_next_fallback() {
        // currentGoalId === currentFallbackGoalId 时返回 nextFallback
        // currentFallbackGoalId: currentProjectId 存在 → currentProjectGoalId = "cpg"
        // nextFallbackGoalId: projectId 存在 → projectGoalId = "pg-new"
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: s("cp"),
            current_goal_id: s("cpg"), // == currentFallbackGoalId
            current_project_goal_id: s("cpg"),
            project_id: Some("p".into()),
            goal_id: None,
            project_goal_id: Some("pg-new".into()),
            default_goal_id: s("d"),
        });
        assert_eq!(out, Some("pg-new".to_string()));
    }

    #[test]
    fn resolve_next_current_goal_differs_from_fallback_keeps_current() {
        // currentGoalId !== currentFallbackGoalId → 返回 currentGoalId
        // currentFallbackGoalId: "cpg"
        // currentGoalId: "user-pinned" ≠ "cpg"
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: s("cp"),
            current_goal_id: s("user-pinned"),
            current_project_goal_id: s("cpg"),
            project_id: Some("p".into()),
            goal_id: None,
            project_goal_id: Some("pg-new".into()),
            default_goal_id: s("d"),
        });
        assert_eq!(out, Some("user-pinned".to_string()));
    }

    #[test]
    fn resolve_next_project_id_omitted_falls_back_to_current() {
        // project_id 为 None → 用 current_project_id
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: s("cp"),
            current_goal_id: None,
            current_project_goal_id: s("cpg"),
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: s("d"),
        });
        // projectId = currentProjectId = "cp"，projectGoalId = currentProjectGoalId = "cpg"
        // next fallback: projectId 存在 → projectGoalId
        assert_eq!(out, Some("cpg".to_string()));
    }

    #[test]
    fn resolve_next_no_project_uses_default_goal_id() {
        // 都没有 projectId → fallback 走 defaultGoalId
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: None,
            current_goal_id: None,
            current_project_goal_id: None,
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: s("d"),
        });
        assert_eq!(out, Some("d".to_string()));
    }

    #[test]
    fn resolve_next_project_goal_id_omitted_when_no_project_uses_null() {
        // projectGoalId 未提供且 projectId 也未提供 → projectGoalId = None
        // 与 Node `projectId ? input.currentProjectGoalId : null` 1:1 对齐
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: s("cp"),
            current_goal_id: None,
            current_project_goal_id: s("cpg"),
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: None,
        });
        // projectId = currentProjectId = "cp"，projectGoalId = currentProjectGoalId = "cpg"
        assert_eq!(out, Some("cpg".to_string()));
    }

    #[test]
    fn resolve_next_project_goal_id_omitted_no_current_project_yields_null_fallback() {
        // current_project_id 是 None，project_id 是 None，project_goal_id 未提供
        // → projectGoalId = null，next fallback = defaultGoalId ?? null
        let out = resolve_next_issue_goal_id(ResolveNextIssueGoalIdInput {
            current_project_id: None,
            current_goal_id: None,
            current_project_goal_id: Some("cpg".into()),
            project_id: None,
            goal_id: None,
            project_goal_id: None,
            default_goal_id: None,
        });
        assert_eq!(out, None);
    }
}
