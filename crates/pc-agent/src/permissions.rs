//! Agent 权限标准化
//!
//! 对齐 Node `services/agent-permissions.ts`：
//! - `default_permissions_for_role(role)` —— 按角色返回默认权限
//! - `normalize_agent_permissions(permissions, role)` —— 合并入参 + 角色默认，
//!   对 `canCreateAgents` / `canCreateSkills` 做**类型校验**后回填（类型错则用默认值）
//! - 其他字段原样保留
//!
//! 设计：
//! - 公开 `AgentPermissions` 类型 = 开放 `Map<String, Value>`，但约定两个 bool 字段
//! - 纯函数无副作用，方便单测
//! - 模块化放在 `pc-agent` 而非 `pc-repos`：与 agent 业务逻辑同包，便于调用方就近引用

use serde_json::{Map, Value};

// ============================================================================
// Types
// ============================================================================

/// 标准化的 Agent 权限对象。
///
/// - 已知字段：`can_create_agents` / `can_create_skills`（布尔）
/// - 未知字段：原样保留（如 `trustPreset` / `authorizationPolicy` 等 Node 端扩展字段）
///
/// 字段命名采用 Rust snake_case 以匹配仓库其它领域模型；JSON 序列化时与 Node 同名。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPermissions {
    inner: Map<String, Value>,
}

impl AgentPermissions {
    pub fn can_create_agents(&self) -> bool {
        self.inner
            .get("canCreateAgents")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn can_create_skills(&self) -> bool {
        self.inner
            .get("canCreateSkills")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn as_object(&self) -> &Map<String, Value> {
        &self.inner
    }

    pub fn into_object(self) -> Map<String, Value> {
        self.inner
    }

    pub fn to_value(&self) -> Value {
        Value::Object(self.inner.clone())
    }
}

impl From<Map<String, Value>> for AgentPermissions {
    fn from(inner: Map<String, Value>) -> Self {
        Self { inner }
    }
}

impl From<AgentPermissions> for Value {
    fn from(p: AgentPermissions) -> Self {
        Value::Object(p.into_object())
    }
}

// ============================================================================
// Public API
// ============================================================================

/// 按角色返回默认权限。
///
/// - `ceo`（大小写不敏感、去前后空格）：`canCreateAgents=true`
/// - 其他角色：`canCreateAgents=false`
/// - 所有角色：`canCreateSkills=true`
pub fn default_permissions_for_role(role: &str) -> AgentPermissions {
    let can_create_agents = role.trim().eq_ignore_ascii_case("ceo");
    let mut inner = Map::new();
    inner.insert("canCreateAgents".into(), Value::Bool(can_create_agents));
    inner.insert("canCreateSkills".into(), Value::Bool(true));
    AgentPermissions { inner }
}

/// 标准化 Agent 权限：
///
/// 1. 若 `permissions` 不是对象（含 `null` / 数组），返回 role 默认值
/// 2. 保留入参对象中所有原字段
/// 3. 对 `canCreateAgents` 做类型校验：若存在且为 bool → 使用；否则用 role 默认
/// 4. 对 `canCreateSkills` 做同样的类型校验
///
/// 行为对齐 Node `normalizeAgentPermissions`：
/// - `preserved = { ...record }` —— 保留所有原字段
/// - `canCreateAgents: typeof record.canCreateAgents === "boolean" ? record.canCreateAgents : defaults.canCreateAgents`
pub fn normalize_agent_permissions(permissions: Value, role: &str) -> AgentPermissions {
    let defaults = default_permissions_for_role(role);

    // 步骤 1：非对象 → 直接返回 role 默认
    let Some(record) = permissions.as_object() else {
        return defaults;
    };

    // 步骤 2：保留原字段
    let mut inner = record.clone();

    // 步骤 3 & 4：类型校验后回填（注意：覆盖而非保留，以对齐 Node 的"类型错则用默认值"语义）
    inner.insert(
        "canCreateAgents".into(),
        record
            .get("canCreateAgents")
            .and_then(Value::as_bool)
            .map(Value::Bool)
            .unwrap_or_else(|| defaults.can_create_agents_value()),
    );
    inner.insert(
        "canCreateSkills".into(),
        record
            .get("canCreateSkills")
            .and_then(Value::as_bool)
            .map(Value::Bool)
            .unwrap_or_else(|| defaults.can_create_skills_value()),
    );

    AgentPermissions { inner }
}

impl AgentPermissions {
    fn can_create_agents_value(&self) -> Value {
        self.inner
            .get("canCreateAgents")
            .cloned()
            .unwrap_or(Value::Bool(false))
    }

    fn can_create_skills_value(&self) -> Value {
        self.inner
            .get("canCreateSkills")
            .cloned()
            .unwrap_or(Value::Bool(true))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_for_ceo_can_create_agents_and_skills() {
        let p = default_permissions_for_role("ceo");
        assert!(p.can_create_agents());
        assert!(p.can_create_skills());
    }

    #[test]
    fn default_for_ceo_case_insensitive() {
        for role in ["CEO", "Ceo", "  cEO  "] {
            let p = default_permissions_for_role(role);
            assert!(
                p.can_create_agents(),
                "role {role:?} should grant canCreateAgents"
            );
        }
    }

    #[test]
    fn default_for_non_ceo_cannot_create_agents_but_can_skills() {
        for role in ["worker", "manager", "admin", ""] {
            let p = default_permissions_for_role(role);
            assert!(
                !p.can_create_agents(),
                "role {role:?} should NOT grant canCreateAgents"
            );
            assert!(
                p.can_create_skills(),
                "role {role:?} should grant canCreateSkills"
            );
        }
    }

    #[test]
    fn normalize_null_returns_defaults() {
        let p = normalize_agent_permissions(Value::Null, "ceo");
        assert!(p.can_create_agents());
        assert!(p.can_create_skills());
    }

    #[test]
    fn normalize_non_object_returns_defaults() {
        let p = normalize_agent_permissions(json!("just a string"), "ceo");
        assert!(p.can_create_agents());

        let p = normalize_agent_permissions(json!([1, 2, 3]), "ceo");
        assert!(p.can_create_agents());

        let p = normalize_agent_permissions(json!(42), "ceo");
        assert!(p.can_create_agents());
    }

    #[test]
    fn normalize_preserves_explicit_bool() {
        let input = json!({
            "canCreateAgents": false,
            "canCreateSkills": false,
        });
        let p = normalize_agent_permissions(input, "ceo");
        assert!(!p.can_create_agents(), "explicit false must override ceo default");
        assert!(!p.can_create_skills(), "explicit false must override default true");
    }

    #[test]
    fn normalize_overrides_wrong_type_with_default() {
        // canCreateAgents is null → use default
        let input = json!({
            "canCreateAgents": null,
            "canCreateSkills": "not a bool",
        });
        let p = normalize_agent_permissions(input, "ceo");
        assert!(p.can_create_agents(), "null must fall back to ceo default");
        assert!(p.can_create_skills(), "non-bool string must fall back to default true");
    }

    #[test]
    fn normalize_preserves_extra_fields() {
        let input = json!({
            "canCreateAgents": true,
            "canCreateSkills": false,
            "trustPreset": "standard",
            "authorizationPolicy": { "mode": "allow" },
            "customField": 42,
        });
        let p = normalize_agent_permissions(input, "worker");
        let obj = p.as_object();
        assert_eq!(obj.get("trustPreset"), Some(&json!("standard")));
        assert_eq!(obj.get("authorizationPolicy"), Some(&json!({ "mode": "allow" })));
        assert_eq!(obj.get("customField"), Some(&json!(42)));
    }

    #[test]
    fn normalize_missing_fields_uses_role_default() {
        let input = json!({}); // empty object
        let p = normalize_agent_permissions(input, "ceo");
        assert!(p.can_create_agents());
        assert!(p.can_create_skills());
    }

    #[test]
    fn normalize_missing_fields_for_worker() {
        let input = json!({});
        let p = normalize_agent_permissions(input, "worker");
        assert!(!p.can_create_agents());
        assert!(p.can_create_skills());
    }

    #[test]
    fn round_trip_via_value() {
        let p = default_permissions_for_role("ceo");
        let v: Value = p.clone().into();
        let back = normalize_agent_permissions(v, "ceo");
        assert_eq!(p, back);
    }

    #[test]
    fn can_create_helpers_safe_for_missing_keys() {
        // Direct construction bypassing default fn
        let p = AgentPermissions {
            inner: Map::new(),
        };
        assert!(!p.can_create_agents()); // no key → false
        assert!(!p.can_create_skills()); // no key → false
    }

    #[test]
    fn from_map_constructor() {
        let mut m = Map::new();
        m.insert("canCreateAgents".into(), Value::Bool(true));
        m.insert("canCreateSkills".into(), Value::Bool(false));
        let p: AgentPermissions = m.into();
        assert!(p.can_create_agents());
        assert!(!p.can_create_skills());
    }
}
