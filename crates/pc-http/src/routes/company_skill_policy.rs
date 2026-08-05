//! 公司级 skill 策略。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use pc_repos::company_skill_policy::{CompanySkillPolicyRepo, PolicyRow};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/skill-policy",
            get(get_skill_policy)
                .put(put_skill_policy)
                .delete(delete_skill_policy),
        )
        // ── Round 214: 端口化 evaluate 端点 ──
        .route(
            "/api/companies/:company_id/skill-policy/evaluate",
            post(evaluate_skill_policy),
        )
}

#[derive(Debug, Deserialize, Default)]
#[allow(dead_code)]
struct PolicyBody {
    #[serde(default)]
    default_effect: Option<String>,
    #[serde(default)]
    rules: Option<Value>,
    #[serde(default)]
    revision: Option<i32>,
}

fn policy_json(row: &PolicyRow) -> Value {
    json!({
        "companyId": row.company_id,
        "schemaVersion": row.schema_version,
        "revision": row.revision,
        "defaultEffect": row.default_effect,
        "rules": row.rules,
        "updatedAt": row.updated_at,
    })
}

fn default_policy(company_id: Uuid) -> Value {
    json!({
        "companyId": company_id,
        "schemaVersion": 1,
        "revision": 0,
        "defaultEffect": "allow",
        "rules": [],
        "updatedAt": null
    })
}

async fn read(state: &AppState, company_id: Uuid) -> ApiResult<Value> {
    match CompanySkillPolicyRepo::new(&state.db).fetch(company_id).await? {
        Some(row) => Ok(policy_json(&row)),
        None => Ok(default_policy(company_id)),
    }
}

async fn write(state: &AppState, company_id: Uuid, body: &PolicyBody) -> ApiResult<Value> {
    let default_effect = body
        .default_effect
        .clone()
        .unwrap_or_else(|| "allow".to_owned());
    let rules = body.rules.clone().unwrap_or_else(|| json!([]));
    let new_revision = body.revision.unwrap_or(0) + 1;
    CompanySkillPolicyRepo::new(&state.db)
        .upsert(company_id, new_revision, &default_effect, &rules)
        .await?;
    read(state, company_id).await
}

async fn get_skill_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    Ok(Json(read(&state, company_id).await?))
}

async fn put_skill_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<PolicyBody>,
) -> ApiResult<impl IntoResponse> {
    Ok((
        StatusCode::OK,
        Json(write(&state, company_id, &body).await?),
    ))
}

async fn delete_skill_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    CompanySkillPolicyRepo::new(&state.db).delete(company_id).await?;
    Ok((
        StatusCode::OK,
        Json(json!({ "deleted": true, "companyId": company_id })),
    ))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct EvaluateBody {
    action: String,
    #[serde(default)]
    resource: Value,
    #[serde(default)]
    principal: Option<EvaluatePrincipal>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct EvaluatePrincipal {
    agent_id: Uuid,
}

/// Round 197: 评估 skill policy。
///
/// 简化版规则匹配：
/// 1. 若无策略（materialized=false），默认 allow
/// 2. 按规则优先级 + id 排序后逐条匹配
///    - action 必须匹配 rule.actions
///    - subject 匹配（agent_id / agent_role / all）
///    - resource 匹配（skill_id / skill_key / source_type / all）
/// 3. 匹配中 → 按 rule.effect 决定 allow/deny
/// 4. 未匹配中 → 按 default_effect
async fn evaluate_skill_policy(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<EvaluateBody>,
) -> ApiResult<Json<Value>> {
    let repo = CompanySkillPolicyRepo::new(&state.db);
    let policy = match repo.fetch(company_id).await? {
        Some(p) => p,
        None => {
            return Ok(Json(json!({
                "allowed": true,
                "action": body.action,
                "reason": "no_policy_default",
                "policyRevision": 0,
                "matchedRuleId": serde_json::Value::Null,
                "remediation": serde_json::Value::Null,
            })));
        }
    };

    // Parse rules array
    let rules = policy.rules.as_array().cloned().unwrap_or_default();
    let mut sorted: Vec<Value> = rules;
    sorted.sort_by(|a, b| {
        let ap = a.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
        let bp = b.get("priority").and_then(|v| v.as_i64()).unwrap_or(0);
        ap.cmp(&bp).then_with(|| {
            let ai = a.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let bi = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
            ai.cmp(bi)
        })
    });

    // Resolve principal
    let principal = if let Some(p) = body.principal.as_ref() {
        json!({"kind": "agent", "agentId": p.agent_id})
    } else {
        // Default principal from auth context
        json!({"kind": "anonymous"})
    };

    let resource = body.resource.clone();

    // Try each rule in order
    for rule in sorted.iter() {
        if !rule_action_matches(rule, &body.action) {
            continue;
        }
        if !subject_matches(rule, &principal) {
            continue;
        }
        if !resource_matches(rule, &resource) {
            continue;
        }
        let effect = rule.get("effect").and_then(|v| v.as_str()).unwrap_or("allow");
        let rule_id = rule.get("id").and_then(|v| v.as_str()).map(String::from);
        return Ok(Json(json!({
            "allowed": effect == "allow",
            "action": body.action,
            "reason": "explicit_rule",
            "policyRevision": policy.revision,
            "matchedRuleId": rule_id,
            "remediation": serde_json::Value::Null,
        })));
    }

    // Default effect
    let allowed = policy.default_effect == "allow";
    Ok(Json(json!({
        "allowed": allowed,
        "action": body.action,
        "reason": "policy_default",
        "policyRevision": policy.revision,
        "matchedRuleId": serde_json::Value::Null,
        "remediation": if allowed { serde_json::Value::Null } else { serde_json::Value::String("Adjust policy rules to allow this action".into()) },
    })))
}

fn rule_action_matches(rule: &Value, action: &str) -> bool {
    rule.get("actions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .any(|a| a.as_str().map(|s| s == action).unwrap_or(false))
        })
        .unwrap_or(false)
}

fn subject_matches(rule: &Value, principal: &Value) -> bool {
    let subject = match rule.get("subject") {
        Some(s) => s,
        None => return true,
    };
    // Match "all" → any principal
    if subject.get("kind").and_then(|v| v.as_str()) == Some("all") {
        return true;
    }
    // Match agent_id
    if let (Some(target_id), Some(agent_id)) = (
        subject.get("agentId").and_then(|v| v.as_str()),
        principal.get("agentId").and_then(|v| v.as_str()),
    ) {
        if target_id == agent_id {
            return true;
        }
    }
    // Match role
    if let (Some(role), Some(actual_role)) = (
        subject.get("role").and_then(|v| v.as_str()),
        principal.get("role").and_then(|v| v.as_str()),
    ) {
        if role == actual_role {
            return true;
        }
    }
    false
}

fn resource_matches(rule: &Value, resource: &Value) -> bool {
    let selector = match rule.get("resources") {
        Some(r) => r,
        None => return true,
    };
    if selector.is_null() {
        return true;
    }
    // skill_id
    if let (Some(target_sid), Some(actual_sid)) = (
        selector.get("skillId").and_then(|v| v.as_str()),
        resource.get("skillId").and_then(|v| v.as_str()),
    ) {
        if target_sid != actual_sid {
            return false;
        }
    }
    // skill_key
    if let (Some(target_sk), Some(actual_sk)) = (
        selector.get("skillKey").and_then(|v| v.as_str()),
        resource.get("skillKey").and_then(|v| v.as_str()),
    ) {
        if target_sk != actual_sk {
            return false;
        }
    }
    // source_type
    if let (Some(target_st), Some(actual_st)) = (
        selector.get("sourceType").and_then(|v| v.as_str()),
        resource.get("sourceType").and_then(|v| v.as_str()),
    ) {
        if target_st != actual_st {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod round214_tests {
    //! Round 214: 为 evaluate 端点使用的纯函数提供单元测试。
    //!
    //! 这些函数（rule_action_matches、subject_matches、resource_matches）
    //! 不依赖 db / auth，是纯 JSON 逻辑，最适合用单元测试覆盖。
    use super::*;
    use serde_json::json;

    fn rule_with_actions(actions: &[&str]) -> Value {
        json!({ "actions": actions, "id": "r1" })
    }

    #[test]
    fn rule_action_matches_when_in_list() {
        let r = rule_with_actions(&["skill:install", "skill:run"]);
        assert!(rule_action_matches(&r, "skill:install"));
        assert!(rule_action_matches(&r, "skill:run"));
        assert!(!rule_action_matches(&r, "skill:delete"));
    }

    #[test]
    fn rule_action_matches_returns_false_when_no_actions() {
        // 防御性: 缺少 actions 字段 → 不匹配任何 action
        let r = json!({ "id": "r1" });
        assert!(!rule_action_matches(&r, "skill:install"));
    }

    #[test]
    fn subject_matches_all_passes_any_principal() {
        let r = json!({ "subject": { "kind": "all" } });
        let p = json!({ "kind": "agent", "agentId": "x" });
        assert!(subject_matches(&r, &p));
    }

    #[test]
    fn subject_matches_agent_id_specific() {
        let r = json!({ "subject": { "kind": "agent", "agentId": "abc" } });
        assert!(subject_matches(&r, &json!({ "kind": "agent", "agentId": "abc" })));
        assert!(!subject_matches(&r, &json!({ "kind": "agent", "agentId": "xyz" })));
        assert!(!subject_matches(&r, &json!({ "kind": "role", "role": "agent" })));
    }

    #[test]
    fn subject_matches_role_specific() {
        let r = json!({ "subject": { "kind": "role", "role": "admin" } });
        assert!(subject_matches(&r, &json!({ "kind": "role", "role": "admin" })));
        assert!(!subject_matches(&r, &json!({ "kind": "role", "role": "member" })));
    }

    #[test]
    fn subject_matches_no_subject_field_means_match_all() {
        // 规则没有 subject → 匹配任何 principal
        let r = json!({ "actions": ["x"] });
        assert!(subject_matches(&r, &json!({ "kind": "agent", "agentId": "any" })));
    }

    #[test]
    fn resource_matches_no_selector_means_match_all() {
        let r = json!({ "actions": ["x"] });
        let res = json!({ "skillId": "s1" });
        assert!(resource_matches(&r, &res));
    }

    #[test]
    fn resource_matches_null_selector_means_match_all() {
        let r = json!({ "resources": null });
        let res = json!({ "skillId": "s1", "skillKey": "k1" });
        assert!(resource_matches(&r, &res));
    }

    #[test]
    fn resource_matches_skill_id_specific() {
        let r = json!({ "resources": { "skillId": "s1" } });
        assert!(resource_matches(&r, &json!({ "skillId": "s1" })));
        assert!(!resource_matches(&r, &json!({ "skillId": "s2" })));
    }

    #[test]
    fn resource_matches_multiple_selectors_and_logic() {
        let r = json!({ "resources": { "skillKey": "k1", "sourceType": "bundled" } });
        assert!(resource_matches(&r, &json!({ "skillKey": "k1", "sourceType": "bundled" })));
        assert!(!resource_matches(&r, &json!({ "skillKey": "k1", "sourceType": "external" })));
        assert!(!resource_matches(&r, &json!({ "skillKey": "k2", "sourceType": "bundled" })));
    }

    #[test]
    fn resource_matches_passes_when_only_some_fields_present() {
        // selector 只指定 skillId, resource 额外有 sourceType → 通过
        let r = json!({ "resources": { "skillId": "s1" } });
        assert!(resource_matches(
            &r,
            &json!({ "skillId": "s1", "sourceType": "bundled" })
        ));
    }
}
