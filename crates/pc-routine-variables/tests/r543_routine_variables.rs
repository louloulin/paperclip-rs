//! R543 — pc-routine-variables 综合测试集。
//!
//! 覆盖：
//! 1. `extract_routine_variable_names` — 单模板 / 多模板 / 去重 / 顺序保留 / markdown 转义
//! 2. `sync_routine_variables_with_template` — 保留已有元数据 / 推断 Date 类型 / 过滤 builtin
//! 3. `is_routine_date_variable_name` — 大写 Date 后缀 / 排除 length == 4
//! 4. `is_valid_routine_date_string` — YYYY-MM-DD 解析 / 闰年 / 月份范围 / 日范围
//! 5. `is_valid_routine_variable_name` — 首字母 / 字符集
//! 6. `interpolate_routine_template` — 简单替换 / 缺失保留 / 多种值类型 / markdown 转义
//! 7. `stringify_routine_variable_value` — string/number/bool/null/object
//! 8. `builtin_values_at` — date / timestamp 格式
//! 9. `is_builtin_routine_variable` — date / timestamp / 任意
//! 10. `RoutineTemplateInput` — From 实现
//! 11. `RoutineVariable` `serde` — JSON 往返与 Node 上游字段命名兼容

use std::collections::BTreeMap;

use chrono::{TimeZone, Utc};
use pc_routine_variables::{
    builtin_values_at, extract_routine_variable_names, interpolate_routine_template,
    is_builtin_routine_variable, is_routine_date_variable_name, is_valid_routine_date_string,
    is_valid_routine_variable_name, stringify_routine_variable_value,
    sync_routine_variables_with_template, RoutineVariable, RoutineVariableType,
};
use serde_json::{json, Value};

// ============================================================================
// `extract_routine_variable_names`
// ============================================================================

#[test]
fn r543_extract_first_appearance_order() {
    let names = extract_routine_variable_names("Review {{repo}} and {{priority}} for {{repo}}");
    assert_eq!(names, vec!["repo", "priority"]);
}

#[test]
fn r543_extract_dedupes_across_title_and_description() {
    let names = extract_routine_variable_names(vec![
        "Triage {{repo}}",
        "Review {{repo}} for {{priority}} bugs",
    ]);
    assert_eq!(names, vec!["repo", "priority"]);
}

#[test]
fn r543_extract_handles_markdown_escaped_underscores() {
    let names = extract_routine_variable_names("Issue {{pr\\_url}} review {{pr_url}}");
    assert_eq!(names, vec!["pr_url"]);
}

#[test]
fn r543_extract_skips_null_and_empty_fragments() {
    let names = extract_routine_variable_names(vec![None, Some(""), Some("Hi {{name}}")]);
    assert_eq!(names, vec!["name"]);
}

#[test]
fn r543_extract_returns_empty_when_no_placeholders() {
    let names = extract_routine_variable_names("Just a plain title");
    assert!(names.is_empty());
}

#[test]
fn r543_extract_tolerates_whitespace_inside_braces() {
    let names = extract_routine_variable_names("Use {{   spaced_in  }} and {{tight}}");
    assert_eq!(names, vec!["spaced_in", "tight"]);
}

#[test]
fn r543_extract_ignores_malformed_placeholders() {
    // {{ }} (no name), {{1abc}} (must start with letter)
    let names = extract_routine_variable_names("Skip {{ }} and {{1abc}} pass {{valid}}");
    assert_eq!(names, vec!["valid"]);
}

// ============================================================================
// `sync_routine_variables_with_template`
// ============================================================================

#[test]
fn r543_sync_preserves_existing_metadata() {
    let existing = vec![
        RoutineVariable {
            name: "repo".into(),
            label: Some("Repository".into()),
            r#type: RoutineVariableType::Text,
            default_value: Some(json!("paperclip")),
            required: true,
            options: vec![],
        },
        RoutineVariable {
            name: "startDate".into(),
            label: Some("Start".into()),
            r#type: RoutineVariableType::Text,
            default_value: Some(json!("soon")),
            required: false,
            options: vec![],
        },
    ];
    let synced = sync_routine_variables_with_template(
        vec!["Triage {{repo}}", "Review {{repo}} and {{startDate}}"],
        Some(&existing),
    );
    assert_eq!(synced.len(), 2);
    assert_eq!(synced[0].label.as_deref(), Some("Repository"));
    assert_eq!(
        synced[0].default_value,
        Some(Value::String("paperclip".into()))
    );
    assert_eq!(synced[1].label.as_deref(), Some("Start"));
}

#[test]
fn r543_sync_defaults_capital_date_to_date_type() {
    let synced = sync_routine_variables_with_template("Compare {{startDate}} to {{endDate}}", None);
    assert_eq!(synced.len(), 2);
    assert_eq!(synced[0].name, "startDate");
    assert_eq!(synced[0].r#type, RoutineVariableType::Date);
    assert_eq!(synced[1].name, "endDate");
    assert_eq!(synced[1].r#type, RoutineVariableType::Date);
}

#[test]
fn r543_sync_filters_out_builtin_variable_names() {
    let synced = sync_routine_variables_with_template(
        "Today is {{date}} at {{timestamp}} for {{repo}}",
        None,
    );
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0].name, "repo");
}

#[test]
fn r543_sync_drops_orphan_existing_variables() {
    let existing = vec![RoutineVariable {
        name: "stale".into(),
        label: None,
        r#type: RoutineVariableType::Text,
        default_value: None,
        required: true,
        options: vec![],
    }];
    let synced = sync_routine_variables_with_template("Only {{fresh}} here", Some(&existing));
    assert_eq!(synced.len(), 1);
    assert_eq!(synced[0].name, "fresh");
}

// ============================================================================
// `is_routine_date_variable_name`
// ============================================================================

#[test]
fn r543_is_routine_date_variable_name_acceptance_matrix() {
    assert!(is_routine_date_variable_name("startDate"));
    assert!(is_routine_date_variable_name("endDate"));
    assert!(is_routine_date_variable_name("fooDate"));
    assert!(!is_routine_date_variable_name("date")); // length == 4
    assert!(!is_routine_date_variable_name("startdate")); // lowercase d
    assert!(!is_routine_date_variable_name("candidate")); // doesn't end with Date
    assert!(!is_routine_date_variable_name("Date")); // exactly "Date"
}

#[test]
fn r543_is_routine_date_variable_name_rejects_invalid_name() {
    assert!(!is_routine_date_variable_name("1Date")); // must start with letter
    assert!(!is_routine_date_variable_name("start-Date")); // '-' breaks grammar
}

// ============================================================================
// `is_valid_routine_date_string`
// ============================================================================

#[test]
fn r543_is_valid_routine_date_string_handles_leap_years() {
    assert!(is_valid_routine_date_string("2024-02-29"));
    assert!(!is_valid_routine_date_string("2024-02-30"));
    assert!(!is_valid_routine_date_string("2023-02-29"));
    assert!(!is_valid_routine_date_string("2024-13-01"));
    assert!(!is_valid_routine_date_string("2024-1-01"));
}

#[test]
fn r543_is_valid_routine_date_string_month_day_ranges() {
    assert!(!is_valid_routine_date_string("2024-00-01"));
    assert!(!is_valid_routine_date_string("2024-12-00"));
    assert!(!is_valid_routine_date_string("2024-12-32"));
    assert!(!is_valid_routine_date_string("2024-04-31")); // April has 30 days
    assert!(is_valid_routine_date_string("2024-04-30"));
    assert!(is_valid_routine_date_string("2024-01-31"));
    assert!(is_valid_routine_date_string("2024-03-31"));
    assert!(!is_valid_routine_date_string("2024-15-01")); // month > 12
}

#[test]
fn r543_is_valid_routine_date_string_century_leap_rules() {
    // Century years: divisible by 400 → leap, otherwise not.
    assert!(is_valid_routine_date_string("2000-02-29"));
    assert!(!is_valid_routine_date_string("1900-02-29"));
    assert!(is_valid_routine_date_string("2400-02-29"));
}

#[test]
fn r543_is_valid_routine_date_string_rejects_garbage() {
    assert!(!is_valid_routine_date_string(""));
    assert!(!is_valid_routine_date_string("2024/02/29"));
    assert!(!is_valid_routine_date_string("2024-02-29T00:00:00"));
    assert!(!is_valid_routine_date_string("not-a-date"));
}

// ============================================================================
// `is_valid_routine_variable_name`
// ============================================================================

#[test]
fn r543_is_valid_routine_variable_name_grammar() {
    assert!(is_valid_routine_variable_name("repo"));
    assert!(is_valid_routine_variable_name("PR_URL"));
    assert!(is_valid_routine_variable_name("v1_2_3"));
    assert!(!is_valid_routine_variable_name("1repo")); // must start with letter
    assert!(!is_valid_routine_variable_name("repo-name")); // '-' not allowed
    assert!(!is_valid_routine_variable_name("")); // empty
    assert!(!is_valid_routine_variable_name("repo name")); // space
}

// ============================================================================
// `interpolate_routine_template`
// ============================================================================

#[test]
fn r543_interpolate_replaces_provided_values() {
    let mut values = BTreeMap::new();
    values.insert("repo".to_string(), json!("paperclip"));
    values.insert("priority".to_string(), json!("high"));
    let out = interpolate_routine_template(Some("Review {{repo}} for {{priority}}"), Some(&values));
    assert_eq!(out.as_deref(), Some("Review paperclip for high"));
}

#[test]
fn r543_interpolate_preserves_missing_placeholders() {
    let values = BTreeMap::new();
    let out = interpolate_routine_template(Some("Hello {{name}} and {{uncle}}"), Some(&values));
    assert_eq!(out.as_deref(), Some("Hello {{name}} and {{uncle}}"));
}

#[test]
fn r543_interpolate_returns_template_when_no_values() {
    let out = interpolate_routine_template(Some("Raw {{x}}"), None);
    assert_eq!(out.as_deref(), Some("Raw {{x}}"));
}

#[test]
fn r543_interpolate_returns_none_for_none_template() {
    let out = interpolate_routine_template(None, None);
    assert_eq!(out, None);
}

#[test]
fn r543_interpolate_handles_various_value_types() {
    let mut values = BTreeMap::new();
    values.insert("count".to_string(), json!(42));
    values.insert("flag".to_string(), json!(true));
    values.insert("ratio".to_string(), json!(0.5));
    values.insert("absent".to_string(), Value::Null);
    values.insert("list".to_string(), json!([1, 2, 3]));
    let out = interpolate_routine_template(
        Some("c={{count}} f={{flag}} r={{ratio}} n={{absent}} l={{list}}"),
        Some(&values),
    );
    assert_eq!(out.as_deref(), Some("c=42 f=true r=0.5 n= l=[1,2,3]"));
}

#[test]
fn r543_interpolate_unescapes_markdown_underscore_placeholder() {
    let mut values = BTreeMap::new();
    values.insert("pr_url".to_string(), json!("https://example.com"));
    let out = interpolate_routine_template(Some("See {{pr\\_url}} for details"), Some(&values));
    assert_eq!(out.as_deref(), Some("See https://example.com for details"));
}

#[test]
fn r543_interpolate_preserves_trailing_text_after_placeholder() {
    let mut values = BTreeMap::new();
    values.insert("name".to_string(), json!("alice"));
    let out = interpolate_routine_template(Some("Hi {{name}}, welcome!"), Some(&values));
    assert_eq!(out.as_deref(), Some("Hi alice, welcome!"));
}

// ============================================================================
// `stringify_routine_variable_value`
// ============================================================================

#[test]
fn r543_stringify_handles_all_value_kinds() {
    assert_eq!(stringify_routine_variable_value(&json!("abc")), "abc");
    assert_eq!(stringify_routine_variable_value(&json!(42)), "42");
    assert_eq!(stringify_routine_variable_value(&json!(true)), "true");
    assert_eq!(stringify_routine_variable_value(&json!(false)), "false");
    assert_eq!(stringify_routine_variable_value(&Value::Null), "");
    assert_eq!(
        stringify_routine_variable_value(&json!({"a": 1})),
        "{\"a\":1}"
    );
}

// ============================================================================
// `builtin_values_at`
// ============================================================================

#[test]
fn r543_builtin_values_at_emits_iso_date_and_human_timestamp() {
    let instant = Utc.with_ymd_and_hms(2026, 4, 28, 12, 17, 0).unwrap();
    let values = builtin_values_at(instant);
    assert_eq!(values.get("date").map(String::as_str), Some("2026-04-28"));
    let ts = values.get("timestamp").map(String::as_str).unwrap();
    assert!(ts.starts_with("April 28, 2026 at 12:17 PM UTC"), "got {ts}");
}

#[test]
fn r543_builtin_values_at_handles_morning_12am_and_12pm() {
    // 00:30 UTC → "12:30 AM UTC"
    let morning = Utc.with_ymd_and_hms(2026, 1, 1, 0, 30, 0).unwrap();
    let ts = builtin_values_at(morning).remove("timestamp").unwrap();
    assert_eq!(ts, "January 1, 2026 at 12:30 AM UTC");

    // 12:30 UTC → "12:30 PM UTC"
    let noon = Utc.with_ymd_and_hms(2026, 1, 1, 12, 30, 0).unwrap();
    let ts = builtin_values_at(noon).remove("timestamp").unwrap();
    assert_eq!(ts, "January 1, 2026 at 12:30 PM UTC");

    // 13:30 UTC → "1:30 PM UTC"
    let afternoon = Utc.with_ymd_and_hms(2026, 1, 1, 13, 30, 0).unwrap();
    let ts = builtin_values_at(afternoon).remove("timestamp").unwrap();
    assert_eq!(ts, "January 1, 2026 at 1:30 PM UTC");
}

// ============================================================================
// `is_builtin_routine_variable`
// ============================================================================

#[test]
fn r543_is_builtin_routine_variable_returns_expected_flags() {
    assert!(is_builtin_routine_variable("date"));
    assert!(is_builtin_routine_variable("timestamp"));
    assert!(!is_builtin_routine_variable("repo"));
    assert!(!is_builtin_routine_variable("Date")); // case sensitive
    assert!(!is_builtin_routine_variable(""));
}

// ============================================================================
// `RoutineTemplateInput` + `serde`
// ============================================================================

#[test]
fn r543_template_input_from_implementations_skip_empty() {
    use pc_routine_variables::RoutineTemplateInput;
    let from_str: RoutineTemplateInput = "Hello {{x}}".into();
    let from_opt: RoutineTemplateInput = Some("Hello {{x}}").into();
    let from_vec: RoutineTemplateInput = vec![Some("Hello {{x}}"), None, Some("")].into();
    assert_eq!(from_str.fragments(), &["Hello {{x}}"]);
    assert_eq!(from_opt.fragments(), &["Hello {{x}}"]);
    assert_eq!(from_vec.fragments(), &["Hello {{x}}"]);
}

#[test]
fn r543_routine_variable_serializes_camel_case() {
    let variable = RoutineVariable {
        name: "repo".into(),
        label: Some("Repository".into()),
        r#type: RoutineVariableType::Text,
        default_value: Some(json!("paperclip")),
        required: true,
        options: vec![],
    };
    let json_str = serde_json::to_string(&variable).unwrap();
    // "defaultValue" not "default_value"; "type" matches Node.
    assert!(json_str.contains("\"defaultValue\""));
    assert!(json_str.contains("\"type\":\"text\""));
    assert!(json_str.contains("\"name\":\"repo\""));
}

#[test]
fn r543_routine_variable_deserializes_camel_case_from_node_shape() {
    let payload = json!({
        "name": "startDate",
        "label": "Start",
        "type": "date",
        "defaultValue": "2026-01-01",
        "required": true,
        "options": [],
    });
    let variable: RoutineVariable = serde_json::from_value(payload).unwrap();
    assert_eq!(variable.name, "startDate");
    assert_eq!(variable.r#type, RoutineVariableType::Date);
    assert_eq!(variable.default_value, Some(json!("2026-01-01")));
}
