//! Tool profile binding scope precedence and ordering.
//!
//! 对齐 Node `services/tool-profile-binding-precedence.ts`：
//! - 6 个 binding target 类型（gateway > issue > routine > agent > project > company）
//! - 同 scope 内按 priority 升序 / createdAt 升序 / profileId 字典序升序
//! - `profileIdsInBindingOrder` 按顺序去重，保留 binding 顺序

use serde::{Deserialize, Serialize};

/// Tool profile binding 的目标范围。
///
/// 对齐 Node `ToolProfileBindingTargetType`（`TOOL_PROFILE_BINDING_TARGET_TYPES`）。
/// JSON 序列化使用小写 snake tag，与 `@paperclipai/shared/constants.ts` 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolProfileBindingTargetType {
    /// 具体命名的 MCP gateway 实例；最具体，优先级最高。
    Gateway,
    /// 单个 issue。
    Issue,
    /// 单个 routine run。
    Routine,
    /// 单个 agent。
    Agent,
    /// 单个 project。
    Project,
    /// 整公司默认；最宽，优先级最低。
    Company,
}

impl ToolProfileBindingTargetType {
    /// 返回小写字符串形式，便于日志与共享类型互通。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Issue => "issue",
            Self::Routine => "routine",
            Self::Agent => "agent",
            Self::Project => "project",
            Self::Company => "company",
        }
    }
}

/// 6 个 target 的固定 scope precedence 表（数字越小越 narrow，越优先）。
///
/// 对齐 Node `TOOL_PROFILE_SCOPE_PRECEDENCE`。
pub const TOOL_PROFILE_BINDING_SCOPE_PRECEDENCE: &[(ToolProfileBindingTargetType, u8)] = &[
    (ToolProfileBindingTargetType::Gateway, 0),
    (ToolProfileBindingTargetType::Issue, 1),
    (ToolProfileBindingTargetType::Routine, 2),
    (ToolProfileBindingTargetType::Agent, 3),
    (ToolProfileBindingTargetType::Project, 4),
    (ToolProfileBindingTargetType::Company, 5),
];

/// 读取一个 target 的 scope precedence。
///
/// 对齐 Node `toolProfileBindingScopePrecedence`。
#[must_use]
pub fn tool_profile_binding_scope_precedence(target: ToolProfileBindingTargetType) -> u8 {
    match target {
        ToolProfileBindingTargetType::Gateway => 0,
        ToolProfileBindingTargetType::Issue => 1,
        ToolProfileBindingTargetType::Routine => 2,
        ToolProfileBindingTargetType::Agent => 3,
        ToolProfileBindingTargetType::Project => 4,
        ToolProfileBindingTargetType::Company => 5,
    }
}

/// 内部使用的 binding 视图：precedence 排序和去重所需的最小字段集合。
///
/// 设计：
/// - 复用 `chrono::DateTime<Utc>` 等时间源时，调用方自行 `.timestamp_millis()`；
/// - 仓储层实现 `From<&ToolProfileBindingRow>` 即可直接进入这些纯函数；
/// - 与 Node `BindingLike` 一一对应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProfileBinding {
    pub profile_id: String,
    pub target_type: ToolProfileBindingTargetType,
    pub priority: i32,
    /// `created_at` 的 epoch millis（与 Node `Date.prototype.getTime()` 对齐）。
    pub created_at_millis: i64,
}

impl ToolProfileBinding {
    /// 用 raw 字段构造一条 binding 记录。
    pub fn new(
        profile_id: impl Into<String>,
        target_type: ToolProfileBindingTargetType,
        priority: i32,
        created_at_millis: i64,
    ) -> Self {
        Self {
            profile_id: profile_id.into(),
            target_type,
            priority,
            created_at_millis,
        }
    }
}

/// 同一 scope 内部的稳定排序键。
///
/// Node 原版：`(a.priority - b.priority) || (createdAtMillis(a.createdAt) - createdAtMillis(b.createdAt)) || a.profileId.localeCompare(b.profileId)`。
fn binding_secondary_order(
    left: &ToolProfileBinding,
    right: &ToolProfileBinding,
) -> std::cmp::Ordering {
    left.priority
        .cmp(&right.priority)
        .then(left.created_at_millis.cmp(&right.created_at_millis))
        .then(left.profile_id.cmp(&right.profile_id))
}

/// 选取所有 binding 中 scope 最 narrow（precedence 最小）的子集，并在同 scope 内按 Node 排序键稳定排序。
///
/// - 空输入直接返回 `Vec::new()`；
/// - 不修改入参，返回新分配的 `Vec<&ToolProfileBinding>`；
/// - 与 Node `narrowestScopeBindings` 行为一致。
#[must_use]
pub fn narrowest_scope_bindings(bindings: &[ToolProfileBinding]) -> Vec<&ToolProfileBinding> {
    if bindings.is_empty() {
        return Vec::new();
    }
    let winning_scope = bindings
        .iter()
        .map(|binding| tool_profile_binding_scope_precedence(binding.target_type))
        .min()
        .expect("non-empty iterator yields a min");
    let mut filtered: Vec<&ToolProfileBinding> = bindings
        .iter()
        .filter(|binding| {
            tool_profile_binding_scope_precedence(binding.target_type) == winning_scope
        })
        .collect();
    filtered.sort_by(|left, right| binding_secondary_order(left, right));
    filtered
}

/// 按 binding 出现的顺序收集 `profileId`，去重保留首次出现。
///
/// - 不要求 binding 已先经过 `narrowest_scope_bindings`；
/// - 复刻 Node `profileIdsInBindingOrder` 的实现。
#[must_use]
pub fn profile_ids_in_binding_order(bindings: &[ToolProfileBinding]) -> Vec<String> {
    let mut ordered: Vec<String> = Vec::with_capacity(bindings.len());
    for binding in bindings {
        if !ordered
            .iter()
            .any(|existing| existing == &binding.profile_id)
        {
            ordered.push(binding.profile_id.clone());
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(
        profile_id: &str,
        target: ToolProfileBindingTargetType,
        priority: i32,
        created_at_millis: i64,
    ) -> ToolProfileBinding {
        ToolProfileBinding::new(profile_id, target, priority, created_at_millis)
    }

    fn profile_ids(bindings: &[&ToolProfileBinding]) -> Vec<String> {
        bindings
            .iter()
            .map(|binding| binding.profile_id.clone())
            .collect()
    }

    #[test]
    fn scope_precedence_values_match_node() {
        assert_eq!(
            TOOL_PROFILE_BINDING_SCOPE_PRECEDENCE,
            &[
                (ToolProfileBindingTargetType::Gateway, 0),
                (ToolProfileBindingTargetType::Issue, 1),
                (ToolProfileBindingTargetType::Routine, 2),
                (ToolProfileBindingTargetType::Agent, 3),
                (ToolProfileBindingTargetType::Project, 4),
                (ToolProfileBindingTargetType::Company, 5),
            ]
        );
        assert_eq!(
            tool_profile_binding_scope_precedence(ToolProfileBindingTargetType::Gateway),
            0
        );
        assert_eq!(
            tool_profile_binding_scope_precedence(ToolProfileBindingTargetType::Issue),
            1
        );
        assert_eq!(
            tool_profile_binding_scope_precedence(ToolProfileBindingTargetType::Routine),
            2
        );
        assert_eq!(
            tool_profile_binding_scope_precedence(ToolProfileBindingTargetType::Agent),
            3
        );
        assert_eq!(
            tool_profile_binding_scope_precedence(ToolProfileBindingTargetType::Project),
            4
        );
        assert_eq!(
            tool_profile_binding_scope_precedence(ToolProfileBindingTargetType::Company),
            5
        );
    }

    #[test]
    fn target_type_serializes_lowercase() {
        for (target, expected) in [
            (ToolProfileBindingTargetType::Gateway, "\"gateway\""),
            (ToolProfileBindingTargetType::Issue, "\"issue\""),
            (ToolProfileBindingTargetType::Routine, "\"routine\""),
            (ToolProfileBindingTargetType::Agent, "\"agent\""),
            (ToolProfileBindingTargetType::Project, "\"project\""),
            (ToolProfileBindingTargetType::Company, "\"company\""),
        ] {
            assert_eq!(serde_json::to_string(&target).unwrap(), expected);
        }
        for (target, expected) in [
            (ToolProfileBindingTargetType::Gateway, "gateway"),
            (ToolProfileBindingTargetType::Company, "company"),
        ] {
            assert_eq!(target.as_str(), expected);
        }
    }

    #[test]
    fn narrowest_scope_bindings_returns_empty_for_empty_input() {
        let result = narrowest_scope_bindings(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn narrowest_scope_bindings_picks_narrowest_scope_across_all_types() {
        let bindings = vec![
            binding(
                "company-wide",
                ToolProfileBindingTargetType::Company,
                100,
                1_000,
            ),
            binding(
                "project-x",
                ToolProfileBindingTargetType::Project,
                50,
                2_000,
            ),
            binding("agent-y", ToolProfileBindingTargetType::Agent, 10, 3_000),
            binding("gateway-z", ToolProfileBindingTargetType::Gateway, 0, 4_000),
        ];
        let result = narrowest_scope_bindings(&bindings);
        assert_eq!(profile_ids(&result), vec!["gateway-z"]);
    }

    #[test]
    fn narrowest_scope_bindings_filters_out_broader_scopes() {
        let bindings = vec![
            binding(
                "company-wide",
                ToolProfileBindingTargetType::Company,
                0,
                1_000,
            ),
            binding("project-x", ToolProfileBindingTargetType::Project, 0, 2_000),
            binding("issue-1", ToolProfileBindingTargetType::Issue, 0, 3_000),
            binding("issue-2", ToolProfileBindingTargetType::Issue, 0, 4_000),
        ];
        let result = narrowest_scope_bindings(&bindings);
        assert_eq!(profile_ids(&result), vec!["issue-1", "issue-2"]);
    }

    #[test]
    fn narrowest_scope_bindings_sorts_by_priority_ascending() {
        let bindings = vec![
            binding("low", ToolProfileBindingTargetType::Agent, 90, 5_000),
            binding("high", ToolProfileBindingTargetType::Agent, 10, 9_000),
            binding("mid", ToolProfileBindingTargetType::Agent, 50, 6_000),
        ];
        let result = narrowest_scope_bindings(&bindings);
        assert_eq!(profile_ids(&result), vec!["high", "mid", "low"]);
    }

    #[test]
    fn narrowest_scope_bindings_sorts_by_created_at_when_priority_ties() {
        let bindings = vec![
            binding("late", ToolProfileBindingTargetType::Agent, 5, 9_000),
            binding("early", ToolProfileBindingTargetType::Agent, 5, 1_000),
            binding("mid", ToolProfileBindingTargetType::Agent, 5, 5_000),
        ];
        let result = narrowest_scope_bindings(&bindings);
        assert_eq!(profile_ids(&result), vec!["early", "mid", "late"]);
    }

    #[test]
    fn narrowest_scope_bindings_sorts_by_profile_id_when_priority_and_created_at_tie() {
        let bindings = vec![
            binding("zeta", ToolProfileBindingTargetType::Agent, 5, 1_000),
            binding("alpha", ToolProfileBindingTargetType::Agent, 5, 1_000),
            binding("mike", ToolProfileBindingTargetType::Agent, 5, 1_000),
        ];
        let result = narrowest_scope_bindings(&bindings);
        assert_eq!(profile_ids(&result), vec!["alpha", "mike", "zeta"]);
    }

    #[test]
    fn narrowest_scope_bindings_combines_all_three_sort_keys() {
        let bindings = vec![
            binding("zeta", ToolProfileBindingTargetType::Agent, 5, 1_000),
            binding("alpha", ToolProfileBindingTargetType::Agent, 5, 1_000),
            binding("late-mid", ToolProfileBindingTargetType::Agent, 5, 9_000),
            binding("high-late", ToolProfileBindingTargetType::Agent, 10, 9_000),
            binding("high-early", ToolProfileBindingTargetType::Agent, 10, 1_000),
        ];
        let result = narrowest_scope_bindings(&bindings);
        assert_eq!(
            profile_ids(&result),
            vec!["alpha", "zeta", "late-mid", "high-early", "high-late"]
        );
    }

    #[test]
    fn narrowest_scope_bindings_does_not_mutate_input() {
        let original = vec![
            binding("zeta", ToolProfileBindingTargetType::Agent, 5, 1_000),
            binding("alpha", ToolProfileBindingTargetType::Agent, 5, 1_000),
        ];
        let snapshot = original.clone();
        let _ = narrowest_scope_bindings(&original);
        assert_eq!(original, snapshot);
    }

    #[test]
    fn profile_ids_in_binding_order_preserves_first_occurrence() {
        let bindings = vec![
            binding("a", ToolProfileBindingTargetType::Agent, 1, 1_000),
            binding("b", ToolProfileBindingTargetType::Issue, 2, 2_000),
            binding("c", ToolProfileBindingTargetType::Company, 3, 3_000),
        ];
        let result = profile_ids_in_binding_order(&bindings);
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn profile_ids_in_binding_order_dedupes_repeats() {
        let bindings = vec![
            binding("a", ToolProfileBindingTargetType::Agent, 1, 1_000),
            binding("b", ToolProfileBindingTargetType::Issue, 2, 2_000),
            binding("a", ToolProfileBindingTargetType::Company, 3, 3_000),
            binding("c", ToolProfileBindingTargetType::Project, 4, 4_000),
            binding("b", ToolProfileBindingTargetType::Routine, 5, 5_000),
        ];
        let result = profile_ids_in_binding_order(&bindings);
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn profile_ids_in_binding_order_returns_empty_for_empty_input() {
        assert!(profile_ids_in_binding_order(&[]).is_empty());
    }

    #[test]
    fn narrowest_then_profile_ids_matches_node_pipeline() {
        let bindings = vec![
            binding(
                "agent-shared",
                ToolProfileBindingTargetType::Agent,
                5,
                1_000,
            ),
            binding(
                "company-wide",
                ToolProfileBindingTargetType::Company,
                0,
                1_000,
            ),
            binding("issue-1", ToolProfileBindingTargetType::Issue, 0, 9_000),
            binding("issue-2", ToolProfileBindingTargetType::Issue, 0, 1_000),
            binding(
                "issue-shared",
                ToolProfileBindingTargetType::Issue,
                0,
                1_000,
            ),
        ];
        let narrowest = narrowest_scope_bindings(&bindings);
        let owned: Vec<ToolProfileBinding> = narrowest.iter().copied().cloned().collect();
        let profile_ids = profile_ids_in_binding_order(&owned);
        assert_eq!(profile_ids, vec!["issue-2", "issue-shared", "issue-1"]);
    }

    #[test]
    fn binding_struct_round_trips_field_order() {
        let binding = binding(
            "profile-7",
            ToolProfileBindingTargetType::Routine,
            42,
            1_700_000_000_000,
        );
        assert_eq!(binding.profile_id, "profile-7");
        assert_eq!(binding.target_type, ToolProfileBindingTargetType::Routine);
        assert_eq!(binding.priority, 42);
        assert_eq!(binding.created_at_millis, 1_700_000_000_000);
    }
}
