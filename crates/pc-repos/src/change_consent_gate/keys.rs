use serde_json::{Map, Value};

pub const AGENT_PROFILE_CHANGE_CONSENT_FIELDS: [&str; 4] =
    ["name", "role", "title", "capabilities"];

pub fn agent_instructions_change_target_key(agent_id: impl std::fmt::Display) -> String {
    format!("agent:{agent_id}:instructions")
}

pub fn agent_profile_change_target_key(agent_id: impl std::fmt::Display) -> String {
    format!("agent:{agent_id}:profile")
}

pub fn skill_change_target_key(skill_id: impl std::fmt::Display) -> String {
    format!("skill:{skill_id}")
}

pub fn skill_slug_change_target_key(slug: &str) -> String {
    format!("skill-slug:{slug}")
}

pub fn skill_import_change_target_key(source: &str) -> String {
    format!("skill-import:{source}")
}

pub fn skills_scan_projects_change_target_key() -> &'static str {
    "skills:scan-projects"
}

pub fn touches_agent_profile_change_consent_fields(patch: &Map<String, Value>) -> bool {
    AGENT_PROFILE_CHANGE_CONSENT_FIELDS
        .iter()
        .any(|key| patch.contains_key(*key))
}

pub(super) fn legacy_target_keys(target_key: &str) -> Vec<String> {
    if let Some(agent_id) = target_key
        .strip_prefix("agent:")
        .and_then(|value| value.strip_suffix(":instructions"))
        .filter(|value| !value.is_empty())
    {
        return vec![format!("reflection-coach:agent-instructions:{agent_id}")];
    }
    if let Some(agent_id) = target_key
        .strip_prefix("agent:")
        .and_then(|value| value.strip_suffix(":profile"))
        .filter(|value| !value.is_empty())
    {
        return vec![format!("reflection-coach:agent-description:{agent_id}")];
    }
    for (prefix, legacy) in [
        ("skill:", "reflection-coach:company-skill:"),
        ("skill-slug:", "reflection-coach:company-skill-slug:"),
    ] {
        if let Some(value) = target_key.strip_prefix(prefix).filter(|value| !value.is_empty()) {
            return vec![format!("{legacy}{value}")];
        }
    }
    if let Some(source) = target_key
        .strip_prefix("skill-import:")
        .filter(|value| !value.is_empty())
    {
        return vec![
            format!("reflection-coach:company-skill-import:{source}"),
            format!("reflection-coach:company-skill-catalog:{source}"),
        ];
    }
    if target_key == skills_scan_projects_change_target_key() {
        return vec!["reflection-coach:company-skills:scan-projects".into()];
    }
    Vec::new()
}
