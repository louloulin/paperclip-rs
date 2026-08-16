#![forbid(unsafe_code)]

//! Tool profile pure helpers \u2014 1:1 port of paperclip/server/src/services/tool-access.ts
//!
//! R722: zero-DB helpers for profile entry matching, summary computation, and
//! pending-new-tool review derivation.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Mirror of Node `profileEntryMatchesCatalog`.
pub fn profile_entry_matches_catalog(entry: &Value, catalog_entry: &Value) -> bool {
    match entry.get("selectorType").and_then(Value::as_str) {
        Some("application") => entry.get("applicationId") == catalog_entry.get("applicationId"),
        Some("connection") => entry.get("connectionId") == catalog_entry.get("connectionId"),
        Some("catalog_entry") => entry.get("catalogEntryId") == catalog_entry.get("id"),
        Some("tool_name") => entry.get("toolName") == catalog_entry.get("toolName"),
        Some("risk_level") => entry.get("riskLevel") == catalog_entry.get("riskLevel"),
        _ => false,
    }
}

/// Mirror of Node `summarizeProfile`.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolProfileSummary {
    pub access_mode: String,         // "all_except" | "selected"
    pub allowed_tool_count: usize,
    pub allowed_application_count: usize,
    pub excluded_tool_count: usize,
    pub total_tool_count: usize,
    pub assignment_count: usize,
    pub applies_to_agent_count: usize,
    pub is_company_default: bool,
}

pub fn summarize_profile(
    profile: &Value,
    entries: &[Value],
    bindings: &[Value],
    catalog: &[Value],
    agent_ids: &[String],
) -> ToolProfileSummary {
    let default_action = profile.get("defaultAction").and_then(Value::as_str).unwrap_or("deny");
    let includes: Vec<&Value> = entries.iter().filter(|e| e.get("effect").and_then(Value::as_str) == Some("include")).collect();
    let excludes: Vec<&Value> = entries.iter().filter(|e| e.get("effect").and_then(Value::as_str) == Some("exclude")).collect();

    let mut allowed_catalog_ids = BTreeSet::new();
    let mut allowed_application_ids = BTreeSet::new();
    let mut excluded_catalog_ids = BTreeSet::new();
    for ce in catalog {
        let id_str = ce.get("id").and_then(Value::as_str).unwrap_or("");
        let excluded = excludes.iter().any(|e| profile_entry_matches_catalog(e, ce));
        if excluded {
            excluded_catalog_ids.insert(id_str.to_string());
            continue;
        }
        let included = includes.iter().any(|e| profile_entry_matches_catalog(e, ce));
        if default_action == "allow" || included {
            allowed_catalog_ids.insert(id_str.to_string());
            if let Some(app_id) = ce.get("applicationId").and_then(Value::as_str) {
                allowed_application_ids.insert(app_id.to_string());
            }
        }
    }

    let company_id = profile.get("companyId").and_then(Value::as_str).unwrap_or("");
    let is_company_default = bindings.iter().any(|b|
        b.get("targetType").and_then(Value::as_str) == Some("company")
            && b.get("targetId").and_then(Value::as_str) == Some(company_id)
    );

    let mut applies_to_agents = BTreeSet::new();
    if is_company_default {
        for aid in agent_ids { applies_to_agents.insert(aid.clone()); }
    } else {
        let company_agent_ids: BTreeSet<&str> = agent_ids.iter().map(|s| s.as_str()).collect();
        for b in bindings {
            if b.get("targetType").and_then(Value::as_str) == Some("agent") {
                if let Some(tid) = b.get("targetId").and_then(Value::as_str) {
                    if company_agent_ids.contains(tid) {
                        applies_to_agents.insert(tid.to_string());
                    }
                }
            }
        }
    }

    ToolProfileSummary {
        access_mode: if default_action == "allow" { "all_except".into() } else { "selected".into() },
        allowed_tool_count: allowed_catalog_ids.len(),
        allowed_application_count: allowed_application_ids.len(),
        excluded_tool_count: excluded_catalog_ids.len(),
        total_tool_count: catalog.len(),
        assignment_count: bindings.len(),
        applies_to_agent_count: applies_to_agents.len(),
        is_company_default,
    }
}

/// Mirror of Node `profileCoversCatalogScope`.
pub fn profile_covers_catalog_scope(
    entry: &Value,
    catalog_entry: &Value,
    catalog_by_id: &BTreeMap<String, Value>,
) -> bool {
    if entry.get("effect").and_then(Value::as_str) != Some("include") { return false; }
    match entry.get("selectorType").and_then(Value::as_str) {
        Some("application") => entry.get("applicationId") == catalog_entry.get("applicationId"),
        Some("connection") => entry.get("connectionId") == catalog_entry.get("connectionId"),
        Some("catalog_entry") => {
            let scoped_id = match entry.get("catalogEntryId").and_then(Value::as_str) {
                Some(s) => s,
                None => return false,
            };
            let scoped_entry = match catalog_by_id.get(scoped_id) {
                Some(v) => v,
                None => return false,
            };
            if scoped_entry.get("connectionId") == catalog_entry.get("connectionId") {
                return true;
            }
            let scoped_app = scoped_entry.get("applicationId").and_then(Value::as_str);
            let cat_app = catalog_entry.get("applicationId").and_then(Value::as_str);
            scoped_app.is_some() && scoped_app == cat_app
        }
        _ => false,
    }
}

/// Mirror of Node `pendingNewToolsForProfile` (subset without DateTime arithmetic).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PendingNewToolItem {
    pub catalog_entry_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<String>,
}

pub fn pending_new_tools_for_profile(
    profile: &Value,
    entries: &[Value],
    catalog: &[Value],
    applications_by_id: &BTreeMap<String, Value>,
    connections_by_id: &BTreeMap<String, Value>,
    watermark: Option<DateTime<Utc>>,
) -> Vec<PendingNewToolItem> {
    if profile.get("status").and_then(Value::as_str) != Some("active")
        || profile.get("defaultAction").and_then(Value::as_str) != Some("deny") {
        return Vec::new();
    }
    let catalog_by_id: BTreeMap<String, &Value> = catalog.iter()
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(|s| (s.to_string(), c)))
        .collect();
    let scoped: Vec<&Value> = entries.iter()
        .filter(|e| e.get("effect").and_then(Value::as_str) == Some("include")
            && matches!(e.get("selectorType").and_then(Value::as_str),
                Some("application") | Some("connection") | Some("catalog_entry")))
        .collect();
    if scoped.is_empty() { return Vec::new(); }

    catalog.iter()
        .filter(|ce| matches!(ce.get("status").and_then(Value::as_str), Some("active") | Some("quarantined")))
        .filter(|ce| {
            watermark.map_or(true, |w| {
                ce.get("firstSeenAt").and_then(Value::as_str)
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|d| d.with_timezone(&Utc) > w)
                    .unwrap_or(false)
            })
        })
        .filter(|ce| scoped.iter().any(|e| {
            let owned = catalog_by_id.iter().map(|(k, v)| (k.clone(), (*v).clone())).collect();
            profile_covers_catalog_scope(e, ce, &owned)
        }))
        .filter(|ce| !entries.iter().any(|e| profile_entry_matches_catalog(e, ce)))
        .map(|ce| {
            let app_id = ce.get("applicationId").and_then(Value::as_str).map(String::from);
            let conn_id = ce.get("connectionId").and_then(Value::as_str).map(String::from);
            PendingNewToolItem {
                catalog_entry_id: ce.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                application_id: app_id.clone(),
                application_name: app_id.as_ref().and_then(|id| applications_by_id.get(id))
                    .and_then(|a| a.get("name").and_then(Value::as_str))
                    .map(String::from),
                connection_id: conn_id.clone(),
                connection_name: conn_id.as_ref().and_then(|id| connections_by_id.get(id))
                    .and_then(|c| c.get("name").and_then(Value::as_str))
                    .map(String::from),
                tool_name: ce.get("toolName").and_then(Value::as_str).map(String::from),
                title: ce.get("title").and_then(Value::as_str).map(String::from),
                description: ce.get("description").and_then(Value::as_str).map(String::from),
                risk_level: ce.get("riskLevel").and_then(Value::as_str).map(String::from),
                first_seen_at: ce.get("firstSeenAt").and_then(Value::as_str).map(String::from),
            }
        })
        .collect()
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn entry(selector_type: &str, effect: &str, key: &str, value: &str) -> Value {
        let mut v = json!({"selectorType": selector_type, "effect": effect});
        match selector_type {
            "application" => v["applicationId"] = json!(value),
            "connection" => v["connectionId"] = json!(value),
            "catalog_entry" => v["catalogEntryId"] = json!(value),
            "tool_name" => v["toolName"] = json!(value),
            "risk_level" => v["riskLevel"] = json!(value),
            _ => {}
        }
        let _ = key;
        v
    }

    #[test]
    fn profile_entry_matches_by_selector() {
        let ce = json!({"id": "c1", "applicationId": "a1", "toolName": "t1", "riskLevel": "low"});
        assert!(profile_entry_matches_catalog(&entry("application", "include", "", "a1"), &ce));
        assert!(!profile_entry_matches_catalog(&entry("application", "include", "", "a2"), &ce));
        assert!(profile_entry_matches_catalog(&entry("tool_name", "include", "", "t1"), &ce));
        assert!(profile_entry_matches_catalog(&entry("risk_level", "include", "", "low"), &ce));
        assert!(!profile_entry_matches_catalog(&entry("unknown", "include", "", "x"), &ce));
    }

    #[test]
    fn summarize_profile_default_allow_counts_all() {
        let profile = json!({"companyId": "co1", "defaultAction": "allow"});
        let entries = vec![entry("tool_name", "exclude", "", "t1")];
        let bindings = vec![json!({"targetType": "company", "targetId": "co1"})];
        let catalog = vec![
            json!({"id": "c1", "applicationId": "a1"}),
            json!({"id": "c2", "applicationId": "a2"}),
            json!({"id": "c3", "toolName": "t1"}),
        ];
        let summary = summarize_profile(&profile, &entries, &bindings, &catalog, &vec!["agent1".into()]);
        assert_eq!(summary.access_mode, "all_except");
        assert_eq!(summary.allowed_tool_count, 2);
        assert_eq!(summary.allowed_application_count, 2);
        assert_eq!(summary.excluded_tool_count, 1);
        assert!(summary.is_company_default);
        assert_eq!(summary.applies_to_agent_count, 1);
    }

    #[test]
    fn summarize_profile_default_deny_counts_includes() {
        let profile = json!({"companyId": "co1", "defaultAction": "deny"});
        let entries = vec![entry("application", "include", "", "a1")];
        let bindings = vec![json!({"targetType": "agent", "targetId": "agent1"})];
        let catalog = vec![
            json!({"id": "c1", "applicationId": "a1"}),
            json!({"id": "c2", "applicationId": "a2"}),
        ];
        let summary = summarize_profile(&profile, &entries, &bindings, &catalog, &vec!["agent1".into(), "agent2".into()]);
        assert_eq!(summary.access_mode, "selected");
        assert_eq!(summary.allowed_tool_count, 1);
        assert_eq!(summary.allowed_application_count, 1);
        assert!(!summary.is_company_default);
        assert_eq!(summary.applies_to_agent_count, 1);
    }

    #[test]
    fn profile_covers_catalog_scope_application() {
        let entry = json!({"effect": "include", "selectorType": "application", "applicationId": "a1"});
        let ce = json!({"id": "c1", "applicationId": "a1"});
        let catalog_by_id = BTreeMap::new();
        assert!(profile_covers_catalog_scope(&entry, &ce, &catalog_by_id));
    }

    #[test]
    fn profile_covers_catalog_scope_catalog_entry() {
        let entry = json!({"effect": "include", "selectorType": "catalog_entry", "catalogEntryId": "c1"});
        let ce = json!({"id": "c2", "applicationId": "a1"});
        let mut catalog_by_id = BTreeMap::new();
        catalog_by_id.insert("c1".to_string(), json!({"id": "c1", "applicationId": "a1"}));
        assert!(profile_covers_catalog_scope(&entry, &ce, &catalog_by_id));
    }

    #[test]
    fn profile_covers_catalog_scope_exclude_rejected() {
        let entry = json!({"effect": "exclude", "selectorType": "application", "applicationId": "a1"});
        let ce = json!({"id": "c1", "applicationId": "a1"});
        assert!(!profile_covers_catalog_scope(&entry, &ce, &BTreeMap::new()));
    }

    #[test]
    fn pending_new_tools_respects_watermark() {
        let profile = json!({"status": "active", "defaultAction": "deny"});
        let entries = vec![entry("catalog_entry", "include", "", "c1")];
        let watermark = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let catalog = vec![
            json!({"id": "c1", "applicationId": "a1", "status": "active", "firstSeenAt": "2025-01-15T00:00:00Z", "toolName": "known"}),
            json!({"id": "c2", "applicationId": "a1", "status": "active", "firstSeenAt": "2025-08-01T00:00:00Z", "toolName": "newtool", "riskLevel": "low"}),
        ];
        let out = pending_new_tools_for_profile(&profile, &entries, &catalog, &BTreeMap::new(), &BTreeMap::new(), Some(watermark));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].catalog_entry_id, "c2");
    }

    #[test]
    fn pending_new_tools_excludes_when_already_covered() {
        let profile = json!({"status": "active", "defaultAction": "deny"});
        let entries = vec![entry("tool_name", "include", "", "known-tool")];
        let watermark = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap();
        let catalog = vec![json!({
            "id": "c1",
            "applicationId": "a1",
            "status": "active",
            "firstSeenAt": "2025-08-01T00:00:00Z",
            "toolName": "known-tool",
            "riskLevel": "low",
        })];
        let out = pending_new_tools_for_profile(&profile, &entries, &catalog, &BTreeMap::new(), &BTreeMap::new(), Some(watermark));
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn pending_new_tools_empty_when_not_deny() {
        let profile = json!({"status": "active", "defaultAction": "allow"});
        let entries = vec![entry("application", "include", "", "a1")];
        let catalog = vec![json!({"id": "c1", "applicationId": "a1", "status": "active"})];
        let out = pending_new_tools_for_profile(&profile, &entries, &catalog, &BTreeMap::new(), &BTreeMap::new(), None);
        assert!(out.is_empty());
    }
}
