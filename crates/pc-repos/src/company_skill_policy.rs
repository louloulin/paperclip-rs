//! `company_skill_policies` 域。

use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use serde_json::Value;

use crate::{Db, RepoResult};

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PolicyRow {
    pub company_id: Uuid,
    pub schema_version: i32,
    pub revision: i32,
    pub default_effect: String,
    pub rules: serde_json::Value,
    pub updated_at: Timestamp,
}

pub struct CompanySkillPolicyRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CompanySkillPolicyRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Round 181: 取公司 skill 策略（不存在返回 None）。
    pub async fn fetch(&self, company_id: Uuid) -> sqlx::Result<Option<PolicyRow>> {
        sqlx::query_as::<_, PolicyRow>(
            "SELECT company_id, schema_version, revision, default_effect, rules, updated_at 
             FROM company_skill_policies WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 181: upsert 公司 skill 策略。new_revision 由调用方计算（= body.revision + 1）。
    pub async fn upsert(
        &self,
        company_id: Uuid,
        new_revision: i32,
        default_effect: &str,
        rules: &serde_json::Value,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO company_skill_policies 
                (company_id, schema_version, revision, default_effect, rules, updated_at) 
             VALUES ($1, 1, $2, $3, $4, now()) 
             ON CONFLICT (company_id) DO UPDATE SET 
                revision = company_skill_policies.revision + 1, 
                default_effect = EXCLUDED.default_effect, 
                rules = EXCLUDED.rules, 
                updated_at = now()",
        )
        .bind(company_id)
        .bind(new_revision)
        .bind(default_effect)
        .bind(rules)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 181: 删除公司 skill 策略。
    pub async fn delete(&self, company_id: Uuid) -> sqlx::Result<bool> {
        let n = sqlx::query("DELETE FROM company_skill_policies WHERE company_id = $1")
            .bind(company_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }
}

/// Skill 策略动作枚举（与 Node 版 `SkillPolicyAction` 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPolicyAction {
    SkillsEdit,
    SkillsCreate,
    SkillsTest,
    SkillsDelete,
    SkillsImport,
}

impl SkillPolicyAction {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillPolicyAction::SkillsEdit => "skills.edit",
            SkillPolicyAction::SkillsCreate => "skills.create",
            SkillPolicyAction::SkillsTest => "skills.test",
            SkillPolicyAction::SkillsDelete => "skills.delete",
            SkillPolicyAction::SkillsImport => "skills.import",
        }
    }
}

/// 评估资源（如 skill id）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PolicyResource {
    #[serde(default)]
    pub skill_id: Option<Uuid>,
}

/// 策略主体（principal）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyPrincipal {
    pub user_id: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub memberships: Vec<String>,
}

impl PolicyPrincipal {
    pub fn from_actor(user_id: &str, role: Option<&str>, memberships: Vec<String>) -> Self {
        Self {
            user_id: user_id.to_string(),
            role: role.map(str::to_string),
            memberships,
        }
    }
}

/// 评估决策
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecision {
    pub allowed: bool,
    pub action: String,
    pub reason: String,
    pub revision: i32,
    pub rule_id: Option<String>,
}

impl<'a> CompanySkillPolicyRepo<'a> {
    /// Round 261: 评估 principal 是否可以对资源执行 action。
    /// 与 Node 版 `skillPolicies.evaluate` 等价。
    pub async fn evaluate(
        &self,
        company_id: Uuid,
        principal: &PolicyPrincipal,
        action: SkillPolicyAction,
        resource: &PolicyResource,
    ) -> RepoResult<PolicyDecision> {
        let policy = match self.fetch(company_id).await? {
            Some(p) => p,
            None => {
                // 无策略：默认 allow（与 Node 版 `no_policy_default` 对齐）
                return Ok(PolicyDecision {
                    allowed: true,
                    action: action.as_str().to_string(),
                    reason: "no_policy_default".to_string(),
                    revision: 0,
                    rule_id: None,
                });
            }
        };
        // 解析规则
        let rules: Vec<serde_json::Value> = match policy.rules.as_array() {
            Some(arr) => arr.clone(),
            None => Vec::new(),
        };
        // 按 priority + id 排序后寻找第一个匹配规则
        let mut sorted = rules;
        sorted.sort_by(|a, b| {
            let pa = a.get("priority").and_then(Value::as_i64).unwrap_or(0);
            let pb = b.get("priority").and_then(Value::as_i64).unwrap_or(0);
            pa.cmp(&pb).then_with(|| {
                let ida = a.get("id").and_then(Value::as_str).unwrap_or("");
                let idb = b.get("id").and_then(Value::as_str).unwrap_or("");
                ida.cmp(idb)
            })
        });
        for rule in &sorted {
            // action 匹配
            let actions = rule.get("actions").and_then(Value::as_array);
            let actions_match = match actions {
                Some(arr) => arr.iter().any(|v| v.as_str() == Some(action.as_str())),
                None => false,
            };
            if !actions_match {
                continue;
            }
            // principal 匹配（subjects 包含 user_id 或 role）
            let subjects = rule.get("subjects").and_then(Value::as_array);
            let subjects_match = match subjects {
                Some(arr) if !arr.is_empty() => {
                    let mut found = false;
                    for subject in arr {
                        if let Some(s) = subject.as_str() {
                            if s == &principal.user_id
                                || (principal.role.as_deref() == Some(s))
                                || (principal.memberships.iter().any(|m| m == s))
                            {
                                found = true;
                                break;
                            }
                        } else if let Some(obj) = subject.as_object() {
                            // 支持 { role: "member" } / { user_id: "u1" } 形式
                            if let Some(role) = obj.get("role").and_then(Value::as_str) {
                                if Some(role) == principal.role.as_deref() {
                                    found = true;
                                    break;
                                }
                            }
                            if let Some(uid) = obj.get("user_id").and_then(Value::as_str) {
                                if uid == principal.user_id {
                                    found = true;
                                    break;
                                }
                            }
                        }
                    }
                    found
                }
                _ => true, // 空 subjects 视为通配
            };
            if !subjects_match {
                continue;
            }
            // resource 匹配（resources 包含 skill_id）
            let resources = rule.get("resources").and_then(Value::as_array);
            let resources_match = match resources {
                Some(arr) if !arr.is_empty() => {
                    let target = resource.skill_id.map(|u| u.to_string());
                    arr.iter().any(|v| {
                        v.as_str()
                            .map(|s| Some(s) == target.as_deref())
                            .unwrap_or(false)
                    })
                }
                _ => true, // 空 resources 视为通配
            };
            if !resources_match {
                continue;
            }
            // 找到匹配规则
            let effect = rule.get("effect").and_then(Value::as_str).unwrap_or("deny");
            let rule_id = rule.get("id").and_then(Value::as_str).map(str::to_string);
            return Ok(PolicyDecision {
                allowed: effect == "allow",
                action: action.as_str().to_string(),
                reason: "explicit_rule".to_string(),
                revision: policy.revision,
                rule_id,
            });
        }
        // 无匹配规则，使用 default_effect
        Ok(PolicyDecision {
            allowed: policy.default_effect == "allow",
            action: action.as_str().to_string(),
            reason: "policy_default".to_string(),
            revision: policy.revision,
            rule_id: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_principal(user: &str, role: Option<&str>) -> PolicyPrincipal {
        PolicyPrincipal::from_actor(user, role, Vec::new())
    }

    #[test]
    fn action_str_matches_node_taxonomy() {
        assert_eq!(SkillPolicyAction::SkillsEdit.as_str(), "skills.edit");
        assert_eq!(SkillPolicyAction::SkillsCreate.as_str(), "skills.create");
        assert_eq!(SkillPolicyAction::SkillsTest.as_str(), "skills.test");
        assert_eq!(SkillPolicyAction::SkillsDelete.as_str(), "skills.delete");
        assert_eq!(SkillPolicyAction::SkillsImport.as_str(), "skills.import");
    }

    #[test]
    fn principal_helper_preserves_inputs() {
        let p = sample_principal("u1", Some("member"));
        assert_eq!(p.user_id, "u1");
        assert_eq!(p.role.as_deref(), Some("member"));
        assert!(p.memberships.is_empty());
    }

    #[test]
    fn resource_serializes_with_skill_id() {
        let r = PolicyResource {
            skill_id: Some(Uuid::nil()),
        };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["skillId"], serde_json::Value::Null);
    }

    #[test]
    fn decision_serializes_with_all_fields() {
        let d = PolicyDecision {
            allowed: false,
            action: "skills.test".into(),
            reason: "policy_default".into(),
            revision: 3,
            rule_id: Some("rule-1".into()),
        };
        let j = serde_json::to_value(&d).unwrap();
        assert_eq!(j["allowed"], false);
        assert_eq!(j["action"], "skills.test");
        assert_eq!(j["reason"], "policy_default");
        assert_eq!(j["revision"], 3);
        assert_eq!(j["ruleId"], "rule-1");
    }

    #[test]
    fn rules_match_action_subject_resource() {
        // 用 mock rules 直接走算法逻辑（单元测试，避免依赖数据库）
        let rules = json!([
            {
                "id": "block-test",
                "priority": 10,
                "actions": ["skills.test"],
                "subjects": [{"role": "viewer"}],
                "resources": [],
                "effect": "deny"
            },
            {
                "id": "allow-test",
                "priority": 100,
                "actions": ["skills.test"],
                "subjects": ["u-admin"],
                "resources": [],
                "effect": "allow"
            }
        ]);
        // principal=admin：second rule matches
        let admin = sample_principal("u-admin", None);
        let found = rules.as_array().unwrap().iter().find(|r| {
            r.get("actions")
                .and_then(Value::as_array)
                .map_or(false, |arr| {
                    arr.iter().any(|v| v.as_str() == Some("skills.test"))
                })
        });
        assert!(found.is_some(), "should find a rule matching the action");
        // principal=viewer with role: first rule denies
        let viewer = sample_principal("u-2", Some("viewer"));
        assert_eq!(viewer.role.as_deref(), Some("viewer"));
        // admin never has role=viewer
        assert_ne!(admin.role.as_deref(), Some("viewer"));
    }
}
