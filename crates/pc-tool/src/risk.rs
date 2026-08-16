#![forbid(unsafe_code)]

//! Tool risk classification — pure functions.
//! R701: Direct port of tool-access.ts::classifyRisk.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolRiskLevel {
    Read,
    Write,
    Destructive,
}

impl ToolRiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Destructive => "destructive",
        }
    }
}

pub fn verb_matches(name: &str, pattern: &str) -> bool {
    let name_lower = name.to_lowercase();
    for verb in pattern.split('|') {
        let verb = verb.trim();
        if verb.is_empty() { continue; }
        if name_lower.contains(verb) { return true; }
    }
    false
}

pub const DESTRUCTIVE_VERBS: &str = "delete|remove|destroy|unpublish";
pub const WRITE_VERBS: &str = "create|update|write|set|send|publish|post|mutate|mark|archive";

pub fn classify_risk(tool: &McpToolDescriptor) -> ToolRiskLevel {
    let annotations = tool.annotations.clone().unwrap_or_default();
    if annotations.destructive_hint == Some(true) || annotations.destructive == Some(true) {
        return ToolRiskLevel::Destructive;
    }
    if annotations.read_only_hint == Some(false) || annotations.write_hint == Some(true) {
        return ToolRiskLevel::Write;
    }
    if verb_matches(&tool.name, DESTRUCTIVE_VERBS) {
        return ToolRiskLevel::Destructive;
    }
    if verb_matches(&tool.name, WRITE_VERBS) {
        return ToolRiskLevel::Write;
    }
    ToolRiskLevel::Read
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    fn tool(name: &str) -> McpToolDescriptor {
        McpToolDescriptor { name: name.to_string(), title: None, description: None, input_schema: None, annotations: None }
    }

    fn tool_with_annotations(name: &str, ann: McpToolAnnotations) -> McpToolDescriptor {
        McpToolDescriptor { name: name.to_string(), title: None, description: None, input_schema: None, annotations: Some(ann) }
    }

    #[test]
    fn destructive_annotation_wins() {
        let t = tool_with_annotations("foo", McpToolAnnotations { destructive_hint: Some(true), ..Default::default() });
        assert_eq!(classify_risk(&t), ToolRiskLevel::Destructive);
    }

    #[test]
    fn destructive_legacy_alias_wins() {
        let t = tool_with_annotations("foo", McpToolAnnotations { destructive: Some(true), ..Default::default() });
        assert_eq!(classify_risk(&t), ToolRiskLevel::Destructive);
    }

    #[test]
    fn write_hint_annotation() {
        let t = tool_with_annotations("foo", McpToolAnnotations { write_hint: Some(true), ..Default::default() });
        assert_eq!(classify_risk(&t), ToolRiskLevel::Write);
    }

    #[test]
    fn not_read_only_annotation() {
        let t = tool_with_annotations("foo", McpToolAnnotations { read_only_hint: Some(false), ..Default::default() });
        assert_eq!(classify_risk(&t), ToolRiskLevel::Write);
    }

    #[test]
    fn destructive_verb_in_name() {
        assert_eq!(classify_risk(&tool("delete_user")), ToolRiskLevel::Destructive);
        assert_eq!(classify_risk(&tool("remove_member")), ToolRiskLevel::Destructive);
        assert_eq!(classify_risk(&tool("destroy_cache")), ToolRiskLevel::Destructive);
        assert_eq!(classify_risk(&tool("unpublish_doc")), ToolRiskLevel::Destructive);
    }

    #[test]
    fn write_verb_in_name() {
        assert_eq!(classify_risk(&tool("create_issue")), ToolRiskLevel::Write);
        assert_eq!(classify_risk(&tool("update_record")), ToolRiskLevel::Write);
        assert_eq!(classify_risk(&tool("send_email")), ToolRiskLevel::Write);
        assert_eq!(classify_risk(&tool("publish_post")), ToolRiskLevel::Write);
        assert_eq!(classify_risk(&tool("archive_item")), ToolRiskLevel::Write);
    }

    #[test]
    fn read_fallback() {
        assert_eq!(classify_risk(&tool("list_things")), ToolRiskLevel::Read);
        assert_eq!(classify_risk(&tool("get_data")), ToolRiskLevel::Read);
        assert_eq!(classify_risk(&tool("search")), ToolRiskLevel::Read);
    }

    #[test]
    fn annotation_priority_over_verb() {
        let t = tool_with_annotations("get_user", McpToolAnnotations { write_hint: Some(true), ..Default::default() });
        assert_eq!(classify_risk(&t), ToolRiskLevel::Write);
    }

    #[test]
    fn verb_matches_basic() {
        assert!(verb_matches("delete_user", "delete"));
        assert!(!verb_matches("get_user", "delete"));
        assert!(verb_matches("create_x", "create|update"));
        assert!(!verb_matches("list_x", "create|update"));
    }

    #[test]
    fn verb_matches_handles_empty_pattern() {
        assert!(!verb_matches("foo", ""));
        assert!(!verb_matches("foo", "  "));
        assert!(!verb_matches("foo", "|"));
    }

    #[test]
    fn verb_matches_case_insensitive() {
        assert!(verb_matches("DELETE_user", "delete"));
        assert!(verb_matches("Create_Issue", "create"));
    }
}
