#![forbid(unsafe_code)]
//! Tool profile binding scope 优先级 + 排序（原 `pc-tool-profile-binding-precedence` 已下沉）。
//!
//! 对应 Node `server/src/services/tool-profile-binding-precedence.ts`（50 行）。
//!
//! 设计目标：1:1 复刻 scope precedence 排序、`narrowestScopeBindings` 排序规则。
//!
//! Scope 优先级（数值越小越优先）：
//! - gateway: 0 （具体 MCP endpoint 实例，最窄）
//! - issue:   1
//! - routine: 2
//! - agent:   3
//! - project: 4
//! - company: 5 （最广）

/// Tool profile binding 的 target 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProfileBindingTargetType {
    Gateway,
    Issue,
    Routine,
    Agent,
    Project,
    Company,
}

impl ToolProfileBindingTargetType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Issue => "issue",
            Self::Routine => "routine",
            Self::Agent => "agent",
            Self::Project => "project",
            Self::Company => "company",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "gateway" => Some(Self::Gateway),
            "issue" => Some(Self::Issue),
            "routine" => Some(Self::Routine),
            "agent" => Some(Self::Agent),
            "project" => Some(Self::Project),
            "company" => Some(Self::Company),
            _ => None,
        }
    }
}

/// scope precedence 查找表 —— 与 Node `TOOL_PROFILE_SCOPE_PRECEDENCE` 1:1。
pub fn tool_profile_binding_scope_precedence(target: ToolProfileBindingTargetType) -> i32 {
    match target {
        ToolProfileBindingTargetType::Gateway => 0,
        ToolProfileBindingTargetType::Issue => 1,
        ToolProfileBindingTargetType::Routine => 2,
        ToolProfileBindingTargetType::Agent => 3,
        ToolProfileBindingTargetType::Project => 4,
        ToolProfileBindingTargetType::Company => 5,
    }
}

/// binding 的最小 trait 形状。
///
/// 业务层的 binding 通常有更多字段（status、metadata 等），但排序只依赖这 4 项。
pub trait BindingLike {
    fn profile_id(&self) -> &str;
    fn target_type(&self) -> ToolProfileBindingTargetType;
    fn priority(&self) -> i32;
    fn created_at(&self) -> chrono::DateTime<chrono::Utc>;
}

/// 把任意 binding 适配到 `BindingLike` 的辅助函数。
pub fn created_at_millis(value: &chrono::DateTime<chrono::Utc>) -> i64 {
    value.timestamp_millis()
}

/// 计算"最窄 scope"的 bindings。
///
/// 1. 找出所有 binding 中 precedence 最小的（即最窄 scope）
/// 2. 在最窄 scope 内，按 (priority asc, createdAt asc, profileId asc) 排序
pub fn narrowest_scope_bindings<B: BindingLike>(bindings: &[B]) -> Vec<&B> {
    if bindings.is_empty() {
        return Vec::new();
    }
    let winning_scope = bindings
        .iter()
        .map(|b| tool_profile_binding_scope_precedence(b.target_type()))
        .min()
        .unwrap();
    let mut narrow: Vec<&B> = bindings
        .iter()
        .filter(|b| tool_profile_binding_scope_precedence(b.target_type()) == winning_scope)
        .collect();
    narrow.sort_by(|a, b| {
        a.priority()
            .cmp(&b.priority())
            .then(created_at_millis(&a.created_at()).cmp(&created_at_millis(&b.created_at())))
            .then(a.profile_id().cmp(b.profile_id()))
    });
    narrow
}

/// 按 binding 顺序去重提取 profile ids。
///
/// 与 Node `profileIdsInBindingOrder` 1:1 对齐：第一次出现的 profile_id 保留，后续重复跳过。
pub fn profile_ids_in_binding_order<B>(bindings: &[B]) -> Vec<String>
where
    B: AsRef<str>,
{
    let mut seen = std::collections::HashSet::new();
    let mut ordered = Vec::new();
    for b in bindings {
        let id = b.as_ref();
        if seen.insert(id.to_string()) {
            ordered.push(id.to_string());
        }
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[derive(Debug, Clone, PartialEq)]
    struct TestBinding {
        profile_id: String,
        target_type: ToolProfileBindingTargetType,
        priority: i32,
        created_at: chrono::DateTime<chrono::Utc>,
    }

    impl BindingLike for TestBinding {
        fn profile_id(&self) -> &str {
            &self.profile_id
        }
        fn target_type(&self) -> ToolProfileBindingTargetType {
            self.target_type
        }
        fn priority(&self) -> i32 {
            self.priority
        }
        fn created_at(&self) -> chrono::DateTime<chrono::Utc> {
            self.created_at
        }
    }

    impl AsRef<str> for TestBinding {
        fn as_ref(&self) -> &str {
            &self.profile_id
        }
    }

    fn dt(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
    }

    #[test]
    fn r695_precedence_values_match_node() {
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
    fn r695_as_str_round_trip() {
        for t in [
            ToolProfileBindingTargetType::Gateway,
            ToolProfileBindingTargetType::Issue,
            ToolProfileBindingTargetType::Routine,
            ToolProfileBindingTargetType::Agent,
            ToolProfileBindingTargetType::Project,
            ToolProfileBindingTargetType::Company,
        ] {
            assert_eq!(ToolProfileBindingTargetType::from_str(t.as_str()), Some(t));
        }
        assert_eq!(ToolProfileBindingTargetType::from_str("unknown"), None);
    }

    #[test]
    fn r695_narrowest_scope_returns_only_min_precedence() {
        let bindings = vec![
            TestBinding {
                profile_id: "p-agent".into(),
                target_type: ToolProfileBindingTargetType::Agent,
                priority: 0,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
            TestBinding {
                profile_id: "p-gateway".into(),
                target_type: ToolProfileBindingTargetType::Gateway,
                priority: 5,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
            TestBinding {
                profile_id: "p-issue".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 5,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
        ];
        let narrow = narrowest_scope_bindings(&bindings);
        assert_eq!(narrow.len(), 1);
        assert_eq!(narrow[0].profile_id, "p-gateway");
    }

    #[test]
    fn r695_narrowest_scope_sorts_by_priority_then_created_at_then_profile_id() {
        let bindings = vec![
            // 同一 scope (issue)
            TestBinding {
                profile_id: "z".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 1,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
            TestBinding {
                profile_id: "a".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 1,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
            TestBinding {
                profile_id: "m".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 0,
                created_at: dt("2024-02-01T00:00:00Z"),
            },
        ];
        let narrow = narrowest_scope_bindings(&bindings);
        let order: Vec<&str> = narrow.iter().map(|b| b.profile_id.as_str()).collect();
        // priority=0 优先 → m，然后 priority=1 按 profileId 字母序 → a, z
        assert_eq!(order, vec!["m", "a", "z"]);
    }

    #[test]
    fn r695_narrowest_scope_sorts_by_created_at_when_priority_equal() {
        let bindings = vec![
            TestBinding {
                profile_id: "later".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 0,
                created_at: dt("2024-02-01T00:00:00Z"),
            },
            TestBinding {
                profile_id: "earlier".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 0,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
        ];
        let narrow = narrowest_scope_bindings(&bindings);
        let order: Vec<&str> = narrow.iter().map(|b| b.profile_id.as_str()).collect();
        assert_eq!(order, vec!["earlier", "later"]);
    }

    #[test]
    fn r695_narrowest_scope_empty_input() {
        let narrow: Vec<&TestBinding> = narrowest_scope_bindings(&[]);
        assert!(narrow.is_empty());
    }

    #[test]
    fn r695_profile_ids_in_binding_order_dedupes() {
        let bindings = vec![
            TestBinding {
                profile_id: "a".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 0,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
            TestBinding {
                profile_id: "b".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 0,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
            TestBinding {
                profile_id: "a".into(),
                target_type: ToolProfileBindingTargetType::Issue,
                priority: 0,
                created_at: dt("2024-01-01T00:00:00Z"),
            },
        ];
        let ids = profile_ids_in_binding_order(&bindings);
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn r695_profile_ids_empty() {
        let empty: Vec<TestBinding> = Vec::new();
        let ids = profile_ids_in_binding_order(&empty);
        assert!(ids.is_empty());
    }

    #[test]
    fn r695_created_at_millis() {
        let t = dt("2024-01-02T03:04:05Z");
        assert_eq!(created_at_millis(&t), t.timestamp_millis());
    }
}
