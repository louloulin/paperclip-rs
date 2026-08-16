#![forbid(unsafe_code)]

//! Routines pure helpers — 1:1 port of paperclip/server/src/services/routines.ts
//! and paperclip/packages/shared/src/routine-variables.ts.
//!
//! R713: zero-DB helpers extracted from the routines service.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const ROUTINE_DATE_REGEX: &str = r"^(\d{4})-(\d{2})-(\d{2})$";

const DAYS_IN_MONTH_COMMON: [u32; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn unprocessable(msg: impl Into<String>) -> pc_errors::Error {
    pc_errors::unprocessable(msg)
}

pub fn is_valid_routine_variable_name(name: &str) -> bool {
    if name.is_empty() { return false; }
    let first = name.chars().next().unwrap();
    if !(first.is_ascii_alphabetic()) { return false; }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn is_valid_routine_date_string(value: &str) -> bool {
    let re = match regex::Regex::new(ROUTINE_DATE_REGEX) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let caps = match re.captures(value) {
        Some(c) => c,
        None => return false,
    };
    let year: i32 = caps[1].parse().unwrap_or(0);
    let month: u32 = caps[2].parse().unwrap_or(0);
    let day: u32 = caps[3].parse().unwrap_or(0);
    if month < 1 || month > 12 { return false; }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = if month == 2 && leap { 29 } else { DAYS_IN_MONTH_COMMON[month as usize] };
    day >= 1 && day <= max_day
}

pub fn parse_boolean_variable_value(name: &str, raw: &Value) -> Result<bool, pc_errors::Error> {
    if let Some(b) = raw.as_bool() { return Ok(b); }
    if let Some(n) = raw.as_f64() {
        if n == 0.0 || n == 1.0 { return Ok(n == 1.0); }
    }
    if let Some(s) = raw.as_str() {
        let norm = s.trim().to_lowercase();
        match norm.as_str() {
            "true" | "1" | "yes" | "y" | "on" => return Ok(true),
            "false" | "0" | "no" | "n" | "off" => return Ok(false),
            _ => {}
        }
    }
    Err(unprocessable(format!("Variable \"{}\" must be a boolean", name)))
}

pub fn parse_number_variable_value(name: &str, raw: &Value) -> Result<f64, pc_errors::Error> {
    if let Some(n) = raw.as_f64() {
        if n.is_finite() { return Ok(n); }
    }
    if let Some(s) = raw.as_str() {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            if let Ok(parsed) = trimmed.parse::<f64>() {
                if parsed.is_finite() { return Ok(parsed); }
            }
        }
    }
    Err(unprocessable(format!("Variable \"{}\" must be a number", name)))
}

pub fn parse_date_variable_value(name: &str, raw: &Value) -> Result<String, pc_errors::Error> {
    let s = raw.as_str().ok_or_else(|| unprocessable(format!("Variable \"{}\" must be a YYYY-MM-DD date", name)))?;
    let trimmed = s.trim();
    if !is_valid_routine_date_string(trimmed) {
        return Err(unprocessable(format!("Variable \"{}\" must be a valid YYYY-MM-DD date", name)));
    }
    Ok(trimmed.to_string())
}

pub fn normalize_webhook_timestamp_ms(raw: &str) -> Option<f64> {
    let parsed: f64 = raw.parse().ok()?;
    if !parsed.is_finite() { return None; }
    Some(if parsed > 1e12 { parsed } else { parsed * 1000.0 })
}

pub fn is_missing_routine_variable_value(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        _ => false,
    }
}

pub fn status_requires_default_agent(status: &str) -> bool {
    status == "active"
}

pub fn normalize_draft_routine_status<'a>(status: &'a str, assignee_agent_id: Option<&str>) -> &'a str {
    if status_requires_default_agent(status) && assignee_agent_id.map(str::is_empty).unwrap_or(true) {
        return "paused";
    }
    status
}

pub fn assert_routine_can_enable(status: &str, assignee_agent_id: Option<&str>) -> Result<(), pc_errors::Error> {
    if status_requires_default_agent(status) && assignee_agent_id.map(str::is_empty).unwrap_or(true) {
        return Err(unprocessable("Default agent required"));
    }
    Ok(())
}

pub const WORKSPACE_BRANCH_ROUTINE_VARIABLE: &str = "workspaceBranch";

pub fn extract_routine_variable_names(templates: &[Option<&str>]) -> Vec<String> {
    let re = regex::Regex::new(r"\{\{\s*([A-Za-z](?:\\\\_|[A-Za-z0-9_])*)\s*\}\}").unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for t in templates.iter().flatten() {
        for cap in re.captures_iter(t) {
            seen.insert(cap[1].replace("\\\\_", "_"));
        }
    }
    seen.into_iter().collect()
}

pub fn routine_uses_workspace_branch(
    variables: &[Value],
    title: Option<&str>,
    description: Option<&str>,
) -> bool {
    let in_variables = variables.iter().any(|v| {
        v.get("name").and_then(Value::as_str) == Some(WORKSPACE_BRANCH_ROUTINE_VARIABLE)
    });
    if in_variables { return true; }
    extract_routine_variable_names(&[title, description]).iter()
        .any(|n| n == WORKSPACE_BRANCH_ROUTINE_VARIABLE)
}

pub fn normalize_routine_dispatch_fingerprint_value(value: Value) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::Bool(b) => Value::Bool(b),
        Value::Number(n) => Value::Number(n.clone()),
        Value::String(s) => Value::String(s.clone()),
        Value::Array(arr) => Value::Array(arr.into_iter().map(normalize_routine_dispatch_fingerprint_value).collect()),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::new();
            for k in keys {
                out.insert(k.clone(), normalize_routine_dispatch_fingerprint_value(map[k].clone()));
            }
            Value::Object(out)
        }
    }
}

pub fn create_routine_env_fingerprint(env: Option<&Value>) -> String {
    let normalized = normalize_routine_dispatch_fingerprint_value(env.cloned().unwrap_or(Value::Null));
    let canonical = serde_json::to_string(&normalized).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone)]
pub struct RoutineDispatchFingerprintInput {
    pub payload: Option<Value>,
    pub project_id: Option<String>,
    pub project_workspace_id: Option<String>,
    pub assignee_agent_id: Option<String>,
    pub routine_revision_id: Option<String>,
    pub routine_env_fingerprint: Option<String>,
    pub execution_workspace_id: Option<String>,
    pub execution_workspace_preference: Option<String>,
    pub execution_workspace_settings: Option<Value>,
    pub title: String,
    pub description: Option<String>,
}

impl RoutineDispatchFingerprintInput {
    fn to_canonical_value(&self) -> Value {
        let mut keys = BTreeMap::<String, Value>::new();
        keys.insert("payload".into(), self.payload.clone().map(|v| normalize_routine_dispatch_fingerprint_value(v)).unwrap_or(Value::Null));
        keys.insert("projectId".into(), self.project_id.clone().map(Value::String).unwrap_or(Value::Null));
        keys.insert("projectWorkspaceId".into(), self.project_workspace_id.clone().map(Value::String).unwrap_or(Value::Null));
        keys.insert("assigneeAgentId".into(), self.assignee_agent_id.clone().map(Value::String).unwrap_or(Value::Null));
        keys.insert("routineRevisionId".into(), self.routine_revision_id.clone().map(Value::String).unwrap_or(Value::Null));
        keys.insert("routineEnvFingerprint".into(), self.routine_env_fingerprint.clone().map(Value::String).unwrap_or(Value::Null));
        keys.insert("executionWorkspaceId".into(), self.execution_workspace_id.clone().map(Value::String).unwrap_or(Value::Null));
        keys.insert("executionWorkspacePreference".into(), self.execution_workspace_preference.clone().map(Value::String).unwrap_or(Value::Null));
        keys.insert("executionWorkspaceSettings".into(), self.execution_workspace_settings.clone().map(|v| normalize_routine_dispatch_fingerprint_value(v)).unwrap_or(Value::Null));
        keys.insert("title".into(), Value::String(self.title.clone()));
        keys.insert("description".into(), self.description.clone().map(Value::String).unwrap_or(Value::Null));
        let mut obj = serde_json::Map::new();
        for (k, v) in keys { obj.insert(k, v); }
        Value::Object(obj)
    }
}

pub fn create_routine_dispatch_fingerprint(input: &RoutineDispatchFingerprintInput) -> String {
    let canonical = input.to_canonical_value();
    let normalized = normalize_routine_dispatch_fingerprint_value(canonical);
    let json = serde_json::to_string(&normalized).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(json.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedRoutineIssueTemplate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_visibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_code: Option<String>,
}

pub fn read_managed_routine_issue_template(defaults_json: Option<&Value>) -> Option<ManagedRoutineIssueTemplate> {
    let v = defaults_json?;
    let obj = v.get("issueTemplate")?;
    if !obj.is_object() { return None; }
    let surface_visibility = obj.get("surfaceVisibility")
        .and_then(Value::as_str)
        .map(|s| s.to_string());
    let origin_id = obj.get("originId")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let billing_code = obj.get("billingCode")
        .and_then(Value::as_str)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(ManagedRoutineIssueTemplate { surface_visibility, origin_id, billing_code })
}

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn name_validation() {
        assert!(is_valid_routine_variable_name("foo"));
        assert!(is_valid_routine_variable_name("foo_bar"));
        assert!(is_valid_routine_variable_name("Foo123"));
        assert!(!is_valid_routine_variable_name(""));
        assert!(!is_valid_routine_variable_name("1foo"));
        assert!(!is_valid_routine_variable_name("foo-bar"));
    }

    #[test]
    fn date_validity_calendar() {
        assert!(is_valid_routine_date_string("2025-01-15"));
        assert!(is_valid_routine_date_string("2024-02-29"));
        assert!(!is_valid_routine_date_string("2025-02-29"));
        assert!(!is_valid_routine_date_string("2025-13-01"));
        assert!(!is_valid_routine_date_string("2025-00-15"));
        assert!(!is_valid_routine_date_string("2025-04-31"));
        assert!(!is_valid_routine_date_string("not-a-date"));
        assert!(!is_valid_routine_date_string("2025-01-15x"));
    }

    #[test]
    fn boolean_parse_string_variants() {
        for s in ["true","1","yes","y","on","TRUE","Yes"] {
            assert_eq!(parse_boolean_variable_value("k", &json!(s)).unwrap(), true);
        }
        for s in ["false","0","no","n","off","FALSE","No"] {
            assert_eq!(parse_boolean_variable_value("k", &json!(s)).unwrap(), false);
        }
        assert_eq!(parse_boolean_variable_value("k", &json!(true)).unwrap(), true);
        assert_eq!(parse_boolean_variable_value("k", &json!(1)).unwrap(), true);
        assert_eq!(parse_boolean_variable_value("k", &json!(0)).unwrap(), false);
        assert!(parse_boolean_variable_value("k", &json!("maybe")).is_err());
    }

    #[test]
    fn number_parse_string_and_num() {
        assert_eq!(parse_number_variable_value("k", &json!(42)).unwrap(), 42.0);
        assert_eq!(parse_number_variable_value("k", &json!("3.14")).unwrap(), 3.14);
        assert_eq!(parse_number_variable_value("k", &json!("-7")).unwrap(), -7.0);
        assert!(parse_number_variable_value("k", &json!("abc")).is_err());
        assert!(parse_number_variable_value("k", &json!("")).is_err());
    }

    #[test]
    fn date_parse_valid_only() {
        assert_eq!(parse_date_variable_value("k", &json!("2025-01-15")).unwrap(), "2025-01-15");
        assert!(parse_date_variable_value("k", &json!("2025-13-01")).is_err());
        assert!(parse_date_variable_value("k", &json!(123)).is_err());
    }

    #[test]
    fn webhook_timestamp_normalize() {
        assert_eq!(normalize_webhook_timestamp_ms("1700000000"), Some(1_700_000_000_000.0));
        assert_eq!(normalize_webhook_timestamp_ms("1700000000000"), Some(1_700_000_000_000.0));
        assert_eq!(normalize_webhook_timestamp_ms("not-a-number"), None);
    }

    #[test]
    fn missing_value_detect() {
        assert!(is_missing_routine_variable_value(None));
        assert!(is_missing_routine_variable_value(Some(&Value::Null)));
        assert!(is_missing_routine_variable_value(Some(&json!(""))));
        assert!(is_missing_routine_variable_value(Some(&json!("   "))));
        assert!(!is_missing_routine_variable_value(Some(&json!("ok"))));
        assert!(!is_missing_routine_variable_value(Some(&json!(0))));
    }

    #[test]
    fn draft_status_normalize() {
        assert!(status_requires_default_agent("active"));
        assert!(!status_requires_default_agent("draft"));
        assert_eq!(normalize_draft_routine_status("active", None), "paused");
        assert_eq!(normalize_draft_routine_status("active", Some("")), "paused");
        assert_eq!(normalize_draft_routine_status("active", Some("agent-1")), "active");
        assert_eq!(normalize_draft_routine_status("draft", None), "draft");
    }

    #[test]
    fn enable_assertion() {
        assert!(assert_routine_can_enable("active", Some("agent-1")).is_ok());
        assert!(assert_routine_can_enable("active", None).is_err());
        assert!(assert_routine_can_enable("draft", None).is_ok());
    }

    #[test]
    fn workspace_branch_in_variables() {
        let vars = vec![json!({"name": "workspaceBranch", "type": "text"}), json!({"name": "x", "type": "text"})];
        assert!(routine_uses_workspace_branch(&vars, None, None));
    }

    #[test]
    fn workspace_branch_in_template() {
        let vars: Vec<Value> = vec![];
        let title = Some("Deploy {{workspaceBranch}}");
        assert!(routine_uses_workspace_branch(&vars, title, None));
    }

    #[test]
    fn fingerprint_normalize_orders_keys() {
        let v = json!({"b": 1, "a": 2, "nested": {"y": 1, "x": 2}});
        let norm = normalize_routine_dispatch_fingerprint_value(v);
        let expected = json!({"a": 2, "b": 1, "nested": {"x": 2, "y": 1}});
        assert_eq!(norm, expected);
    }

    #[test]
    fn env_fingerprint_stable() {
        let env = json!({"NODE_ENV": "prod", "PORT": "3000"});
        let fp1 = create_routine_env_fingerprint(Some(&env));
        let fp2 = create_routine_env_fingerprint(Some(&env));
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64);
    }

    #[test]
    fn dispatch_fingerprint_changes_with_input() {
        let input1 = RoutineDispatchFingerprintInput {
            payload: None, project_id: Some("p1".into()), project_workspace_id: None,
            assignee_agent_id: Some("a1".into()), routine_revision_id: Some("r1".into()),
            routine_env_fingerprint: None, execution_workspace_id: None,
            execution_workspace_preference: None, execution_workspace_settings: None,
            title: "T".into(), description: None,
        };
        let mut input2 = input1.clone();
        input2.title = "T-changed".into();
        assert_ne!(create_routine_dispatch_fingerprint(&input1), create_routine_dispatch_fingerprint(&input2));
    }

    #[test]
    fn managed_issue_template_extract() {
        let defaults = json!({"issueTemplate": {"surfaceVisibility": "public", "originId": "  abc  ", "billingCode": "B1"}});
        let t = read_managed_routine_issue_template(Some(&defaults)).unwrap();
        assert_eq!(t.surface_visibility.as_deref(), Some("public"));
        assert_eq!(t.origin_id.as_deref(), Some("abc"));
        assert_eq!(t.billing_code.as_deref(), Some("B1"));
    }

    #[test]
    fn managed_issue_template_missing_returns_none() {
        assert!(read_managed_routine_issue_template(None).is_none());
        assert!(read_managed_routine_issue_template(Some(&json!({}))).is_none());
        assert!(read_managed_routine_issue_template(Some(&json!("not-an-object"))).is_none());
    }


// =============================================================================
// Routine variable definition (Node sanity helpers)
// =============================================================================

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoutineVariable {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub kind: String,           // "text" | "boolean" | "number" | "date" | "select"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<RoutineVariableValue>,
    pub required: bool,
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(untagged)]
pub enum RoutineVariableValue {
    Text(String),
    Number(f64),
    Boolean(bool),
    Date(String),
    Null,
}

pub fn stringify_routine_variable_value(raw: &Value) -> String {
    match raw {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub fn sanitize_routine_variable_inputs(
    variables: Option<&[RoutineVariable]>,
) -> Vec<RoutineVariable> {
    variables.map(|v| v.to_vec()).unwrap_or_default()
    // In Node the function fills defaults; in Rust callers must supply complete RoutineVariable values.
}

pub fn assert_routine_variable_definitions(variables: &[RoutineVariable]) -> Result<(), pc_errors::Error> {
    for v in variables {
        if let Some(default) = &v.default_value {
            // Validate the default by attempting to parse it through the same path
            let _ = validate_routine_variable_default(v, default)?;
        }
        if v.kind == "select" && v.options.is_empty() {
            return Err(unprocessable(format!(
                "Variable \"{}\" must define at least one option",
                v.name
            )));
        }
    }
    Ok(())
}

fn validate_routine_variable_default(
    v: &RoutineVariable,
    default: &RoutineVariableValue,
) -> Result<(), pc_errors::Error> {
    use RoutineVariableValue::*;
    match (&v.kind[..], default) {
        ("select", Text(s)) => {
            if !v.options.iter().any(|o| o == s) {
                return Err(unprocessable(format!(
                    "Variable \"{}\" default must match one of: {}",
                    v.name, v.options.join(", ")
                )));
            }
        }
        ("date", Text(s)) => {
            if !is_valid_routine_date_string(s) {
                return Err(unprocessable(format!(
                    "Variable \"{}\" default must be a valid YYYY-MM-DD date",
                    v.name
                )));
            }
        }
        ("boolean", Boolean(_)) | ("number", Number(_)) | ("text", Text(_)) | ("text", Null) => {}
        _ => {
            return Err(unprocessable(format!(
                "Variable \"{}\" default type does not match declared kind \"{}\"",
                v.name, v.kind
            )));
        }
    }
    Ok(())
}

pub fn assert_schedule_compatible_variables(variables: &[RoutineVariable]) -> Result<(), pc_errors::Error> {
    for v in variables {
        if v.default_value.is_none() && v.required {
            return Err(unprocessable(format!(
                "Scheduled variables must have a defaultValue; missing for \"{}\"",
                v.name
            )));
        }
    }
    Ok(())
}

// =============================================================================
// Variable collection / resolution / merging (Node parity)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutineSource { Schedule, Manual, Api, Webhook }

impl RoutineSource {
    fn as_str(&self) -> &'static str {
        match self {
            RoutineSource::Schedule => "schedule",
            RoutineSource::Manual => "manual",
            RoutineSource::Api => "api",
            RoutineSource::Webhook => "webhook",
        }
    }
}

pub fn collect_provided_routine_variables(
    source: RoutineSource,
    payload: Option<&Value>,
    variables: Option<&Value>,
) -> Value {
    let nested_variables = payload
        .and_then(|p| p.get("variables"))
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let mut provided = serde_json::Map::new();
    if source == RoutineSource::Webhook {
        if let Some(p) = payload {
            if let Some(p_obj) = p.as_object() {
                for (k, v) in p_obj {
                    provided.insert(k.clone(), v.clone());
                }
            }
        }
    }
    if let Some(n) = nested_variables.as_object() {
        for (k, v) in n {
            provided.insert(k.clone(), v.clone());
        }
    }
    if let Some(v) = variables {
        if let Some(v_obj) = v.as_object() {
            for (k, v2) in v_obj {
                provided.insert(k.clone(), v2.clone());
            }
        }
    }
    provided.remove("variables");
    Value::Object(provided)
}

pub fn resolve_routine_variable_values(
    variables: &[RoutineVariable],
    input: ResolveRoutineVariablesInput<'_>,
) -> Result<std::collections::BTreeMap<String, RoutineVariableValue>, pc_errors::Error> {
    use RoutineVariableValue::*;
    let mut resolved: std::collections::BTreeMap<String, RoutineVariableValue> = std::collections::BTreeMap::new();
    if variables.is_empty() { return Ok(resolved); }

    let provided = collect_provided_routine_variables(input.source, input.payload, input.variables);
    let provided_obj = provided.as_object();
    let mut missing: Vec<String> = Vec::new();

    for v in variables {
        // Workspace-derived automatic values are authoritative.
        let candidate_value = if let Some(av) = input.automatic_variables {
            av.get(&v.name).cloned()
        } else { None };
        let candidate_value = match candidate_value {
            Some(c) => Some(c),
            None => provided_obj.and_then(|p| p.get(&v.name).cloned()),
        };
        let candidate_value = match candidate_value {
            Some(c) => Some(c),
            None => v.default_value.as_ref().map(|d| value_from_default(d)),
        };

        let normalized: Option<RoutineVariableValue> = match (&v.kind[..], &candidate_value) {
            ("boolean", Some(Value::Bool(b))) => Some(Boolean(*b)),
            ("number", Some(Value::Number(n))) => n.as_f64().map(Number),
            ("number", Some(Value::String(s))) => s.trim().parse::<f64>().ok().map(Number),
            ("date", Some(Value::String(s))) if is_valid_routine_date_string(s.trim()) => Some(Date(s.trim().to_string())),
            ("select", Some(Value::String(s))) => {
                if v.options.iter().any(|o| o == s) { Some(Text(s.clone())) }
                else {
                    return Err(unprocessable(format!(
                        "Variable \"{}\" must match one of: {}",
                        v.name, v.options.join(", ")
                    )));
                }
            }
            ("text", Some(Value::String(s))) => Some(Text(s.clone())),
            (_, Some(Value::Null)) => None,
            (_, None) => None,
            (kind, Some(_)) => {
                return Err(unprocessable(format!(
                    "Variable \"{}\" must be a {}",
                    v.name, kind
                )));
            }
        };

        let is_missing = match &normalized {
            None => true,
            Some(Text(s)) => s.trim().is_empty(),
            Some(Null) => true,
            _ => false,
        };
        if is_missing {
            if v.required { missing.push(v.name.clone()); }
            continue;
        }
        if let Some(n) = normalized {
            resolved.insert(v.name.clone(), n);
        }
    }

    if !missing.is_empty() {
        return Err(unprocessable(format!(
            "Missing routine variables: {}",
            missing.join(", ")
        )));
    }
    Ok(resolved)
}

fn value_from_default(d: &RoutineVariableValue) -> Value {
    use RoutineVariableValue::*;
    match d {
        Text(s) => Value::String(s.clone()),
        Number(n) => serde_json::Number::from_f64(*n).map(Value::Number).unwrap_or(Value::Null),
        Boolean(b) => Value::Bool(*b),
        Date(s) => Value::String(s.clone()),
        Null => Value::Null,
    }
}

#[derive(Debug, Clone)]
pub struct ResolveRoutineVariablesInput<'a> {
    pub source: RoutineSource,
    pub payload: Option<&'a Value>,
    pub variables: Option<&'a Value>,
    pub automatic_variables: Option<&'a std::collections::BTreeMap<String, Value>>,
}

pub fn merge_routine_run_payload(
    payload: Option<&Value>,
    variables: &std::collections::BTreeMap<String, RoutineVariableValue>,
) -> Value {
    if variables.is_empty() { return payload.cloned().unwrap_or(Value::Null); }
    let mut out = payload.cloned().unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if !out.is_object() {
        out = Value::Object(serde_json::Map::new());
    }
    let obj = out.as_object_mut().unwrap();
    let existing_variables = obj.get("variables")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let mut merged = serde_json::Map::new();
    for (k, v) in existing_variables {
        merged.insert(k, v);
    }
    for (k, v) in variables {
        merged.insert(k.clone(), value_from_default(v));
    }
    obj.insert("variables".to_string(), Value::Object(merged));
    out
}

// =============================================================================
// R720 tests
// =============================================================================
#[cfg(test)]
mod r720_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn v_text(name: &str, kind: &str, required: bool) -> RoutineVariable {
        RoutineVariable {
            name: name.into(),
            label: None,
            kind: kind.into(),
            default_value: None,
            required,
            options: vec![],
        }
    }

    #[test]
    fn stringify_routine_value_basic() {
        assert_eq!(stringify_routine_variable_value(&json!("hi")), "hi");
        assert_eq!(stringify_routine_variable_value(&json!(42)), "42");
        assert_eq!(stringify_routine_variable_value(&json!(true)), "true");
        assert_eq!(stringify_routine_variable_value(&json!(null)), "");
    }

    #[test]
    fn assert_select_requires_options() {
        let mut v = v_text("x", "select", true);
        v.options.clear();
        assert!(assert_routine_variable_definitions(std::slice::from_ref(&v)).is_err());
        v.options.push("a".into());
        assert!(assert_routine_variable_definitions(std::slice::from_ref(&v)).is_ok());
    }

    #[test]
    fn collect_provided_webhook_includes_payload_keys() {
        let payload = json!({"foo": 1, "variables": {"bar": 2}});
        let variables = json!({"baz": 3});
        let out = collect_provided_routine_variables(RoutineSource::Webhook, Some(&payload), Some(&variables));
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("foo"), Some(&json!(1)));
        assert_eq!(obj.get("bar"), Some(&json!(2)));
        assert_eq!(obj.get("baz"), Some(&json!(3)));
        assert!(obj.get("variables").is_none());
    }

    #[test]
    fn collect_provided_manual_excludes_payload() {
        let payload = json!({"foo": 1});
        let variables = json!({"baz": 3});
        let out = collect_provided_routine_variables(RoutineSource::Manual, Some(&payload), Some(&variables));
        let obj = out.as_object().unwrap();
        assert!(obj.get("foo").is_none());
        assert_eq!(obj.get("baz"), Some(&json!(3)));
    }

    #[test]
    fn resolve_required_missing_errors() {
        let vars = vec![v_text("env", "text", true)];
        let input = ResolveRoutineVariablesInput {
            source: RoutineSource::Manual,
            payload: None, variables: None, automatic_variables: None,
        };
        assert!(resolve_routine_variable_values(&vars, input).is_err());
    }

    #[test]
    fn resolve_with_default_succeeds() {
        let mut v = v_text("env", "text", true);
        v.default_value = Some(RoutineVariableValue::Text("prod".into()));
        let input = ResolveRoutineVariablesInput {
            source: RoutineSource::Manual,
            payload: None, variables: None, automatic_variables: None,
        };
        let out = resolve_routine_variable_values(&[v], input).unwrap();
        assert_eq!(out.get("env"), Some(&RoutineVariableValue::Text("prod".into())));
    }

    #[test]
    fn resolve_automatic_overrides_provided() {
        let v = v_text("branch", "text", true);
        let provided = json!({"branch": "from-provided"});
        let mut auto = BTreeMap::new();
        auto.insert("branch".to_string(), json!("from-auto"));
        let input = ResolveRoutineVariablesInput {
            source: RoutineSource::Webhook,
            payload: None, variables: Some(&provided), automatic_variables: Some(&auto),
        };
        let out = resolve_routine_variable_values(&[v], input).unwrap();
        assert_eq!(out.get("branch"), Some(&RoutineVariableValue::Text("from-auto".into())));
    }

    #[test]
    fn resolve_select_validates_options() {
        let mut v = v_text("color", "select", true);
        v.options = vec!["red".into(), "blue".into()];
        let provided = json!({"color": "green"});
        let input = ResolveRoutineVariablesInput {
            source: RoutineSource::Manual,
            payload: None, variables: Some(&provided), automatic_variables: None,
        };
        assert!(resolve_routine_variable_values(&[v], input).is_err());
    }

    #[test]
    fn merge_routine_run_payload_creates_variables() {
        let mut vars = BTreeMap::new();
        vars.insert("k".to_string(), RoutineVariableValue::Text("v".into()));
        let out = merge_routine_run_payload(None, &vars);
        assert_eq!(out, json!({"variables": {"k": "v"}}));
    }

    #[test]
    fn merge_routine_run_payload_merges_existing() {
        let mut vars = BTreeMap::new();
        vars.insert("new".to_string(), RoutineVariableValue::Text("v2".into()));
        let existing = json!({"foo": 1, "variables": {"old": "v0"}});
        let out = merge_routine_run_payload(Some(&existing), &vars);
        assert_eq!(out.get("foo"), Some(&json!(1)));
        let merged = out.get("variables").unwrap().as_object().unwrap();
        assert_eq!(merged.get("old"), Some(&json!("v0")));
        assert_eq!(merged.get("new"), Some(&json!("v2")));
    }

    #[test]
    fn merge_no_variables_returns_original() {
        let mut vars = BTreeMap::new();
        let payload = json!({"x": 1});
        let out = merge_routine_run_payload(Some(&payload), &vars);
        assert_eq!(out, json!({"x": 1}));
    }
}
}