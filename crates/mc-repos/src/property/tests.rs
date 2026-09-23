//! `PropertyRepo` 的纯单测 + PG 集成测试（需要真库；`MULTICA_TEST_DATABASE_URL`）。

use super::*;
use serde_json::json;

// ---------------------------------------------------------------------------
// 纯单测
// ---------------------------------------------------------------------------

#[test]
fn name_normalization_and_reserved_words() {
    assert_eq!(normalize_name("  Hello World "), "hello_world");
    assert_eq!(normalize_name("Multi  Space"), "multi__space");
    assert_eq!(validate_name("  Sprint ").as_deref(), Ok("Sprint"));
    assert_eq!(validate_name("").unwrap_err(), "name is required");
    assert_eq!(
        validate_name("bad\tname").unwrap_err(),
        "name cannot contain tabs, newlines, or control characters"
    );
    assert_eq!(
        validate_name(&"n".repeat(MAX_NAME_LEN + 1)).unwrap_err(),
        "name must be 32 characters or fewer"
    );
    // 保留名按**规范化形态**比较：`Priority` / `  due date ` 也拒。
    assert_eq!(
        validate_name("Priority").unwrap_err(),
        "\"Priority\" is reserved for a built-in issue field"
    );
    assert_eq!(
        validate_name(" Due Date ").unwrap_err(),
        "\"Due Date\" is reserved for a built-in issue field"
    );
    assert_eq!(
        validate_name("due_date").unwrap_err(),
        "\"due_date\" is reserved for a built-in issue field"
    );
    assert!(validate_name("estimate").is_ok());
}

#[test]
fn icon_validation() {
    assert_eq!(validate_icon("").as_deref(), Ok(""));
    assert_eq!(validate_icon(" sparkles ").as_deref(), Ok("sparkles"));
    assert_eq!(
        validate_icon("not-an-icon").unwrap_err(),
        "icon must be a supported icon key"
    );
    assert_eq!(
        validate_icon("a\nb").unwrap_err(),
        "icon cannot contain tabs, newlines, or control characters"
    );
    assert_eq!(
        validate_icon(&"i".repeat(MAX_ICON_LEN + 1)).unwrap_err(),
        "icon must be 32 characters or fewer"
    );
    assert_eq!(PROPERTY_ICONS.len(), 36);
}

#[test]
fn type_validation_and_predicates() {
    assert!(validate_type("multi_actor").is_ok());
    assert_eq!(
        validate_type("bool").unwrap_err(),
        "invalid type \"bool\"; valid types: text, number, select, multi_select, date, checkbox, url, actor, multi_actor"
    );
    assert!(type_has_options("select") && type_has_options("multi_select"));
    assert!(!type_has_options("text"));
    assert!(type_is_actor("actor") && type_is_actor("multi_actor"));
    assert!(!type_is_actor("text"));
}

#[test]
fn config_canonicalization() {
    // 非 select 类型：带选项 → 400。
    let with_options = PropertyConfig {
        options: vec![PropertyOption {
            id: String::new(),
            name: "A".into(),
            color: "#000000".into(),
        }],
    };
    assert_eq!(
        validate_config("text", Some(&with_options)).unwrap_err(),
        "type \"text\" does not accept options"
    );
    assert_eq!(validate_config("text", None).unwrap(), json!({}));

    // select 必须有选项。
    assert_eq!(
        validate_config("select", None).unwrap_err(),
        "select properties require at least one option"
    );

    // 缺 id ⇒ 服务端补 UUID；颜色规范化；名字 trim。
    let raw = PropertyConfig {
        options: vec![
            PropertyOption {
                id: String::new(),
                name: " Backlog ".into(),
                color: "3b82f6".into(),
            },
            PropertyOption {
                id: "11111111-1111-1111-1111-111111111111".into(),
                name: "Done".into(),
                color: "#22C55E".into(),
            },
        ],
    };
    let canonical = validate_config("select", Some(&raw)).expect("config");
    let parsed = parse_config(&canonical);
    assert_eq!(parsed.options.len(), 2);
    assert_eq!(parsed.options[0].name, "Backlog");
    assert_eq!(parsed.options[0].color, "#3b82f6");
    assert!(Uuid::parse_str(&parsed.options[0].id).is_ok());
    assert_eq!(parsed.options[1].id, "11111111-1111-1111-1111-111111111111");
    assert_eq!(parsed.options[1].color, "#22c55e");

    // 重复选项名 / 坏颜色 / 坏 id。
    let dup_name = PropertyConfig {
        options: vec![
            PropertyOption {
                id: String::new(),
                name: "A".into(),
                color: "#000000".into(),
            },
            PropertyOption {
                id: String::new(),
                name: "a".into(),
                color: "#ffffff".into(),
            },
        ],
    };
    assert_eq!(
        validate_config("select", Some(&dup_name)).unwrap_err(),
        "duplicate option name \"a\""
    );
    let bad_color = PropertyConfig {
        options: vec![PropertyOption {
            id: String::new(),
            name: "A".into(),
            color: "red".into(),
        }],
    };
    assert_eq!(
        validate_config("select", Some(&bad_color)).unwrap_err(),
        "option \"A\": color must be a 6-digit hex value like #3b82f6"
    );
    let bad_id = PropertyConfig {
        options: vec![PropertyOption {
            id: "nope".into(),
            name: "A".into(),
            color: "#000000".into(),
        }],
    };
    assert_eq!(
        validate_config("select", Some(&bad_id)).unwrap_err(),
        "option \"A\": id must be a UUID"
    );
}

#[test]
fn config_normalizes_to_empty_object_for_plain_types() {
    let value = validate_config("number", None).unwrap();
    assert_eq!(value.as_object().map(serde_json::Map::len), Some(0));
    // 坏 config 解析成空配置而不是 panic（同上游 parsePropertyConfig）。
    assert!(parse_config(&json!("nope")).options.is_empty());
    // 坏 config 解析成空配置而不是 panic（同上游 parsePropertyConfig）。
    assert!(parse_config(&json!("nope")).options.is_empty());
}

#[test]
fn validate_value_text_number_checkbox() {
    assert_eq!(
        validate_value("text", &json!({}), &json!("hi")).unwrap(),
        json!("hi")
    );
    assert_eq!(
        validate_value("text", &json!({}), &json!("  ")).unwrap_err(),
        "value cannot be empty (use DELETE to unset a property)"
    );
    assert_eq!(
        validate_value("text", &json!({}), &json!(5)).unwrap_err(),
        "value must be a string"
    );
    assert_eq!(
        validate_value("text", &json!({}), &JsonValue::Null).unwrap_err(),
        "value cannot be null (use DELETE to unset a property)"
    );
    assert_eq!(
        validate_value(
            "text",
            &json!({}),
            &json!("n".repeat(MAX_TEXT_VALUE_LEN + 1))
        )
        .unwrap_err(),
        "value must be 2000 characters or fewer"
    );
    // 多字节按 rune 计。
    assert!(validate_value("text", &json!({}), &json!("汉".repeat(MAX_TEXT_VALUE_LEN))).is_ok());
    // `\0` 被剥掉（Postgres 存不了）。
    assert_eq!(
        validate_value("text", &json!({}), &json!("a\0b")).unwrap(),
        json!("ab")
    );

    assert_eq!(
        validate_value("number", &json!({}), &json!(1.5)).unwrap(),
        json!(1.5)
    );
    assert_eq!(
        validate_value("number", &json!({}), &json!(3)).unwrap(),
        json!(3)
    );
    assert_eq!(
        validate_value("number", &json!({}), &json!("3")).unwrap_err(),
        "value must be a number"
    );

    assert_eq!(
        validate_value("checkbox", &json!({}), &json!(true)).unwrap(),
        json!(true)
    );
    assert_eq!(
        validate_value("checkbox", &json!({}), &json!("true")).unwrap_err(),
        "value must be true or false"
    );
}

#[test]
fn validate_value_url_and_date() {
    assert_eq!(
        validate_value("url", &json!({}), &json!(" https://a.b/c?d=1 ")).unwrap(),
        json!("https://a.b/c?d=1")
    );
    assert_eq!(
        validate_value("url", &json!({}), &json!("ftp://a.b")).unwrap_err(),
        "value must be an http(s) URL"
    );
    assert_eq!(
        validate_value("url", &json!({}), &json!("https://")).unwrap_err(),
        "value must be an http(s) URL"
    );
    assert_eq!(
        validate_value("url", &json!({}), &json!(7)).unwrap_err(),
        "value must be a URL string"
    );
    assert_eq!(
        validate_value(
            "url",
            &json!({}),
            &json!(format!("https://a.b/{}", "x".repeat(MAX_URL_VALUE_LEN)))
        )
        .unwrap_err(),
        "value must be 2048 characters or fewer"
    );

    assert_eq!(
        validate_value("date", &json!({}), &json!("2026-01-31")).unwrap(),
        json!("2026-01-31")
    );
    #[allow(clippy::unreadable_literal)] // 20260131 是日期字面量，按日期读而不是按千分位分组
    let non_string_date = json!(20260131);
    for bad in ["2026-1-31", "2026-13-01", "20260131", "2026-02-30"] {
        assert_eq!(
            validate_value("date", &json!({}), &json!(bad)).unwrap_err(),
            "value must be a date string in YYYY-MM-DD format",
            "bad date {bad:?}"
        );
    }
    assert_eq!(
        validate_value("date", &json!({}), &non_string_date).unwrap_err(),
        "value must be a date string in YYYY-MM-DD format"
    );
}

pub(super) fn select_config() -> JsonValue {
    json!({"options": [
        {"id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "name": "Low", "color": "#22c55e"},
        {"id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "name": "High", "color": "#ef4444"}
    ]})
}

#[test]
fn validate_value_select_and_multi_select() {
    let config = select_config();
    let low = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
    let high = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";

    assert_eq!(
        validate_value("select", &config, &json!(low)).unwrap(),
        json!(low)
    );
    assert_eq!(
        validate_value("select", &config, &json!(7)).unwrap_err(),
        "value must be one of the option ids: aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa (Low), bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb (High)"
    );
    assert_eq!(
        validate_value("select", &config, &json!("nope")).unwrap_err(),
        "value must be one of the option ids: aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa (Low), bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb (High)"
    );
    assert_eq!(
        validate_value("select", &json!({}), &json!("nope")).unwrap_err(),
        "value must be one of the option ids: "
    );

    // multi_select：去重 + 按定义顺序稳定排序。
    assert_eq!(
        validate_value("multi_select", &config, &json!([high, low, high])).unwrap(),
        json!([low, high])
    );
    assert_eq!(
        validate_value("multi_select", &config, &json!([])).unwrap_err(),
        "value must be a non-empty array of option ids: aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa (Low), bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb (High)"
    );
    assert_eq!(
        validate_value("multi_select", &config, &json!([low, "zzz"])).unwrap_err(),
        "unknown option id \"zzz\"; valid option ids: aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa (Low), bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb (High)"
    );
    assert_eq!(
        validate_value("multi_select", &config, &json!("nope")).unwrap_err(),
        "value must be a non-empty array of option ids: aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa (Low), bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb (High)"
    );
}

#[test]
fn validate_value_actor_and_multi_actor() {
    let member = "11111111-2222-3333-4444-555555555555";
    let reference = format!("member:{member}");
    assert_eq!(
        validate_value("actor", &json!({}), &json!(reference)).unwrap(),
        json!(reference)
    );
    // 大写 UUID 规范化成小写。
    let upper = format!("member:{}", member.to_uppercase());
    assert_eq!(
        validate_value("actor", &json!({}), &json!(upper)).unwrap(),
        json!(reference)
    );
    assert_eq!(
        validate_value(
            "actor",
            &json!({}),
            &json!("agent:11111111-2222-3333-4444-555555555555")
        )
        .unwrap_err(),
        "unknown actor kind \"agent\"; valid kinds: member"
    );
    assert_eq!(
        validate_value("actor", &json!({}), &json!("nocolon")).unwrap_err(),
        "value must look like \"<kind>:<uuid>\" where kind is one of: member"
    );
    assert_eq!(
        validate_value("actor", &json!({}), &json!("member:not-a-uuid")).unwrap_err(),
        "actor id in \"member:not-a-uuid\" must be a UUID"
    );

    let other = "99999999-2222-3333-4444-555555555555";
    let multi = json!([reference, format!("member:{other}"), reference]);
    let validated = validate_value("multi_actor", &json!({}), &multi).expect("multi_actor");
    assert_eq!(
        validated,
        json!([reference, format!("member:{other}")]),
        "去重保持首次出现顺序（multi_actor 不按选项顺序重排）"
    );
    assert_eq!(
        validate_value("multi_actor", &json!({}), &json!([])).unwrap_err(),
        "value must be a non-empty array of actor references"
    );
    let too_many: Vec<String> = (1..=(MAX_ACTOR_VALUES + 1))
        .map(|i| format!("member:{i:08}-2222-3333-4444-555555555555"))
        .collect();
    assert_eq!(
        validate_value("multi_actor", &json!({}), &json!(too_many)).unwrap_err(),
        "value cannot list more than 20 actors"
    );
    assert_eq!(
        actor_refs_in_value("multi_actor", &validated),
        vec![reference.clone(), format!("member:{other}")]
    );
    assert!(actor_refs_in_value("text", &validated).is_empty());
    assert_eq!(
        validate_value("bogus", &json!({}), &json!("x")).unwrap_err(),
        "unsupported property type \"bogus\""
    );
}

#[test]
fn bag_helpers() {
    let bag = json!({"a": 1});
    let merged = merge_bag(&bag, "b", &json!("two"));
    assert_eq!(merged, json!({"a": 1, "b": "two"}));
    assert_eq!(merge_bag(&json!(null), "b", &json!(2)), json!({"b": 2}));
    assert!(!bag_exceeds_limit(&json!({"a": "x"})));
    assert!(bag_exceeds_limit(
        &json!({"a": "x".repeat(MAX_PROPERTIES_BAG_BYTES)})
    ));
}

#[test]
fn describe_options_in_use_lists_names_and_is_sorted() {
    let config = select_config();
    let rows = vec![
        ("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".to_string(), 2_i64),
        ("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string(), 1_i64),
    ];
    assert_eq!(
        describe_options_in_use(&config, &rows),
        "cannot remove options still in use: \"High\" (2 issues), \"Low\" (1 issues); clear or change those values first"
    );
}

#[test]
fn removed_option_ids_is_ordered_by_existing_config() {
    let existing = select_config();
    let next = json!({"options": [
        {"id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "name": "High", "color": "#ef4444"}
    ]});
    assert_eq!(
        removed_option_ids(&existing, &next),
        vec!["aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".to_string()]
    );
    assert!(removed_option_ids(&existing, &existing).is_empty());
}
