#![forbid(unsafe_code)]

//! Tool access selector matching.
//! R706: Direct port of tool-access-policy.ts::selectorMatches.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Tool access context — what we are matching against.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAccessContext {
    pub actor_type: Option<String>,
    pub agent_id: Option<String>,
    pub project_id: Option<String>,
    pub routine_id: Option<String>,
    pub issue_id: Option<String>,
    pub gateway_id: Option<String>,
    pub application_id: Option<String>,
    pub connection_id: Option<String>,
    pub catalog_entry_id: Option<String>,
    pub application_key: Option<String>,
    pub provider_type: Option<String>,
    pub tool_name: Option<String>,
    pub upstream_tool_name: Option<String>,
    pub risk_level: Option<String>,
}

/// Tool access selector — what we are matching.
/// 单数字段 (e.g. agent_id) 精确匹配，复数字段 (e.g. agent_ids) 是 array 包含检查。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAccessSelector {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routine_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_entry_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<String>,
    #[serde(default)]
    pub actor_types: Vec<String>,
    #[serde(default)]
    pub agent_ids: Vec<String>,
    #[serde(default)]
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub routine_ids: Vec<String>,
    #[serde(default)]
    pub issue_ids: Vec<String>,
    #[serde(default)]
    pub gateway_ids: Vec<String>,
    #[serde(default)]
    pub application_ids: Vec<String>,
    #[serde(default)]
    pub connection_ids: Vec<String>,
    #[serde(default)]
    pub catalog_entry_ids: Vec<String>,
    #[serde(default)]
    pub application_keys: Vec<String>,
    #[serde(default)]
    pub provider_types: Vec<String>,
    #[serde(default)]
    pub tool_names: Vec<String>,
    #[serde(default)]
    pub risk_levels: Vec<String>,
}

fn match_single(single: &Option<String>, many: &[String], actual: &Option<String>) -> bool {
    if let Some(s) = single {
        if actual.as_deref() != Some(s.as_str()) { return false; }
    }
    if !many.is_empty() {
        match actual {
            Some(a) => {
                let set: HashSet<&str> = many.iter().map(|s| s.as_str()).collect();
                if !set.contains(a.as_str()) { return false; }
            }
            None => return false,
        }
    }
    true
}

fn match_any_tool_name(single: &Option<String>, many: &[String], ctx: &ToolAccessContext) -> bool {
    let actuals: Vec<&str> = [ctx.tool_name.as_deref(), ctx.upstream_tool_name.as_deref()]
        .into_iter().flatten().collect();
    if let Some(s) = single {
        if !actuals.contains(&s.as_str()) { return false; }
    }
    if !many.is_empty() {
        let set: HashSet<&str> = many.iter().map(|s| s.as_str()).collect();
        if !actuals.iter().any(|a| set.contains(a)) { return false; }
    }
    true
}

pub fn selector_matches(selector: &ToolAccessSelector, ctx: &ToolAccessContext) -> bool {
    if !match_single(&selector.actor_type, &selector.actor_types, &ctx.actor_type) { return false; }
    if !match_single(&selector.agent_id, &selector.agent_ids, &ctx.agent_id) { return false; }
    if !match_single(&selector.project_id, &selector.project_ids, &ctx.project_id) { return false; }
    if !match_single(&selector.routine_id, &selector.routine_ids, &ctx.routine_id) { return false; }
    if !match_single(&selector.issue_id, &selector.issue_ids, &ctx.issue_id) { return false; }
    if !match_single(&selector.gateway_id, &selector.gateway_ids, &ctx.gateway_id) { return false; }
    if !match_single(&selector.application_id, &selector.application_ids, &ctx.application_id) { return false; }
    if !match_single(&selector.connection_id, &selector.connection_ids, &ctx.connection_id) { return false; }
    if !match_single(&selector.catalog_entry_id, &selector.catalog_entry_ids, &ctx.catalog_entry_id) { return false; }
    if !match_single(&selector.application_key, &selector.application_keys, &ctx.application_key) { return false; }
    if !match_single(&selector.provider_type, &selector.provider_types, &ctx.provider_type) { return false; }
    if !match_any_tool_name(&selector.tool_name, &selector.tool_names, ctx) { return false; }
    if !match_single(&selector.risk_level, &selector.risk_levels, &ctx.risk_level) { return false; }
    true
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn empty_ctx() -> ToolAccessContext { ToolAccessContext::default() }

    #[test]
    fn empty_selector_matches_anything() {
        let s = ToolAccessSelector::default();
        let c = empty_ctx();
        assert!(selector_matches(&s, &c));
    }

    #[test]
    fn single_field_must_match() {
        let mut s = ToolAccessSelector::default();
        s.agent_id = Some("a-1".into());
        let mut c = empty_ctx();
        c.agent_id = Some("a-1".into());
        assert!(selector_matches(&s, &c));
        c.agent_id = Some("a-2".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn single_mismatch_blocks() {
        let mut s = ToolAccessSelector::default();
        s.actor_type = Some("agent".into());
        let mut c = empty_ctx();
        c.actor_type = Some("user".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn many_field_includes() {
        let mut s = ToolAccessSelector::default();
        s.agent_ids = vec!["a-1".into(), "a-2".into()];
        let mut c = empty_ctx();
        c.agent_id = Some("a-2".into());
        assert!(selector_matches(&s, &c));
        c.agent_id = Some("a-3".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn many_empty_ctx_fails_when_many_set() {
        let mut s = ToolAccessSelector::default();
        s.agent_ids = vec!["a-1".into()];
        let c = empty_ctx();
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn many_set_with_no_actual_value_blocks() {
        let mut s = ToolAccessSelector::default();
        s.project_ids = vec!["p-1".into()];
        let c = empty_ctx();
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn single_and_many_combined() {
        let mut s = ToolAccessSelector::default();
        s.actor_type = Some("agent".into());
        s.agent_ids = vec!["a-1".into()];
        let mut c = empty_ctx();
        c.actor_type = Some("agent".into());
        c.agent_id = Some("a-1".into());
        assert!(selector_matches(&s, &c));
        c.agent_id = Some("a-2".into());
        assert!(!selector_matches(&s, &c));
        c.agent_id = Some("a-1".into());
        c.actor_type = Some("user".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn tool_name_match_any() {
        let mut s = ToolAccessSelector::default();
        s.tool_name = Some("foo".into());
        let mut c = empty_ctx();
        c.tool_name = Some("foo".into());
        assert!(selector_matches(&s, &c));
        c.tool_name = Some("bar".into());
        c.upstream_tool_name = Some("foo".into());
        assert!(selector_matches(&s, &c));
        c.tool_name = Some("baz".into());
        c.upstream_tool_name = Some("baz".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn tool_names_many_any() {
        let mut s = ToolAccessSelector::default();
        s.tool_names = vec!["foo".into(), "bar".into()];
        let mut c = empty_ctx();
        c.tool_name = Some("bar".into());
        assert!(selector_matches(&s, &c));
        c.tool_name = Some("baz".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn risk_level_match() {
        let mut s = ToolAccessSelector::default();
        s.risk_level = Some("write".into());
        let mut c = empty_ctx();
        c.risk_level = Some("write".into());
        assert!(selector_matches(&s, &c));
        c.risk_level = Some("destructive".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn all_fields_combined() {
        let mut s = ToolAccessSelector::default();
        s.actor_type = Some("agent".into());
        s.agent_ids = vec!["a-1".into()];
        s.risk_levels = vec!["write".into()];
        s.application_id = Some("app-1".into());
        s.tool_names = vec!["foo".into()];
        let mut c = empty_ctx();
        c.actor_type = Some("agent".into());
        c.agent_id = Some("a-1".into());
        c.risk_level = Some("write".into());
        c.application_id = Some("app-1".into());
        c.tool_name = Some("foo".into());
        assert!(selector_matches(&s, &c));
        c.tool_name = Some("bar".into());
        assert!(!selector_matches(&s, &c));
    }

    #[test]
    fn selector_serde_camel_case() {
        let mut s = ToolAccessSelector::default();
        s.actor_type = Some("agent".into());
        s.agent_ids = vec!["a-1".into()];
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("actorType"));
        assert!(j.contains("agentIds"));
    }
}
