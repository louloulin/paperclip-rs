//! `manifest.rs` 的测试面（拆出以守门 ⑩）。
//!
//! 写入规则与 `manifest.rs` / `manifest/rules.rs` 同片（M6-1）。**不要**在别的片里改这里：
//! 门 ⑩ 把「一个大文件被多片改」当成 R7 的并发冲突源，测试面单独成文件正是为了让
//! M6-5 的 `preview` 只读它、不写它。
//!
//! 本文件能看见 `super::` 的私有项（子模块规则），但看不见 `manifest::rules` 的私有项
//! —— 所以 `rules.rs` 里被这里调用的条目是 `pub(crate)`。

use super::rules::*;
use super::*;

const MINIMAL: &str = r#"{
      "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
      "version": "1.2.3", "author": {"name": "Example"},
      "scopes": ["issues:read", "net:example.com"],
      "contributes": {
        "hooks": [{"key": "on_issue", "name": "On issue", "description": "d",
          "triggers": ["event"], "events": ["issue.created"],
          "transport": {"type": "http", "url": "https://example.com/hook"}}]
      }
    }"#;

fn parse(raw: &str) -> Result<Manifest, ManifestError> {
    parse_manifest(raw.as_bytes())
}

fn invalid_message(raw: &str) -> String {
    match parse(raw) {
        Err(ManifestError::Invalid(message)) => message,
        other => panic!("expected invalid, got {other:?}"),
    }
}

#[test]
fn minimal_manifest_parses() {
    let manifest = parse(MINIMAL).expect("parses");
    assert_eq!(manifest.manifest_version, 1);
    assert_eq!(manifest.key, "com.example.demo");
    assert_eq!(manifest.contributes.hooks.len(), 1);
    assert_eq!(manifest.contributes.hooks[0].transport.kind, "http");
    assert!(manifest.config.is_empty());
    assert_eq!(MANIFEST_FILENAME, "multica.plugin.json");
}

#[test]
fn empty_and_oversized_and_trailing_are_rejected_by_json_code() {
    assert_eq!(
        parse_manifest(b"").unwrap_err().code(),
        "plugin_manifest_invalid_json"
    );
    assert_eq!(
        parse_manifest(&vec![b'{'; MAX_MANIFEST_SIZE + 1])
            .unwrap_err()
            .code(),
        "plugin_manifest_invalid_json"
    );
    // 尾部多余内容（上游 rejectTrailingJSON）
    let trailing = format!("{MINIMAL}{MINIMAL}");
    assert_eq!(
        parse(&trailing).unwrap_err().code(),
        "plugin_manifest_invalid_json"
    );
    // 未知字段**不**报错（本仓前向兼容政策）
    let with_unknown = MINIMAL.replace("\"name\": \"Demo\"", "\"name\": \"Demo\", \"future\": 1");
    assert!(parse(&with_unknown).is_ok());
    // manifest_version 不是 1（且**不**做迁移）
    let v2 = MINIMAL.replace("\"manifest_version\": 1", "\"manifest_version\": 2");
    assert_eq!(invalid_message(&v2), "manifest_version must be 1");
}

#[test]
fn identity_rules_match_upstream_messages() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "\"name\": \"Demo\"",
            "\"name\": \"\"",
            "name must be non-empty without surrounding whitespace",
        ),
        (
            "\"name\": \"Demo\"",
            "\"name\": \" Demo\"",
            "name must be non-empty without surrounding whitespace",
        ),
        (
            "\"name\": \"Demo\"",
            "\"name\": \"De\\nmo\"",
            "name must be single-line",
        ),
        (
            "\"version\": \"1.2.3\"",
            "\"version\": \"1.2\"",
            "version must be semantic versioning, got \"1.2\"",
        ),
        (
            "\"version\": \"1.2.3\"",
            "\"version\": \"01.2.3\"",
            "version must be semantic versioning, got \"01.2.3\"",
        ),
        (
            "\"key\": \"com.example.demo\"",
            "\"key\": \"demo\"",
            "key must use a reverse-DNS namespace",
        ),
        (
            "\"key\": \"com.example.demo\"",
            "\"key\": \"com.Example.demo\"",
            "key contains invalid segment \"Example\"",
        ),
        (
            "\"key\": \"com.example.demo\"",
            "\"key\": \"com.ex--ample.demo\"",
            "key contains invalid segment \"ex--ample\"",
        ),
        (
            "\"author\": {\"name\": \"Example\"}",
            "\"author\": {\"name\": \"Example\", \"url\": \"http://example.com\"}",
            "author.url must be a plain HTTPS URL",
        ),
        (
            "\"author\": {\"name\": \"Example\"}",
            "\"author\": {\"name\": \"Example\", \"url\": \"https://user@example.com\"}",
            "author.url must be a plain HTTPS URL",
        ),
        (
            "\"author\": {\"name\": \"Example\"}",
            "\"author\": {\"name\": \"Example\", \"url\": \"https://example.com#frag\"}",
            "author.url must be a plain HTTPS URL",
        ),
    ];
    for (from, to, expected) in cases {
        let raw = MINIMAL.replace(from, to);
        assert_eq!(invalid_message(&raw), *expected, "case {to}");
    }
}

#[test]
fn version_length_and_description_carriage_return() {
    let long_version = format!("\"version\": \"1.2.3+{}\"", "a".repeat(MAX_VERSION_LENGTH));
    assert_eq!(
        invalid_message(&MINIMAL.replace("\"version\": \"1.2.3\"", &long_version)),
        format!("version exceeds {MAX_VERSION_LENGTH} bytes")
    );
    let cr = MINIMAL.replace(
        "\"name\": \"Demo\"",
        "\"name\": \"Demo\", \"description\": \"a\\rb\"",
    );
    assert_eq!(
        invalid_message(&cr),
        "description must not contain carriage returns"
    );
}

#[test]
fn semver_matcher_covers_upstream_regex() {
    for ok in [
        "0.0.0",
        "1.2.3",
        "1.2.3-0.3.7",
        "1.2.3-alpha.beta.1",
        "1.2.3+build.1",
        "1.2.3-rc.1+build.2",
        "10.20.30",
    ] {
        assert!(is_semver(ok), "{ok}");
    }
    for bad in [
        "", "1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "1.2.03", "1.2.3-", "1.2.3+", "v1.2.3",
        "1.2.3-+", "1.2.3-á",
    ] {
        assert!(!is_semver(bad), "{bad}");
    }
}

#[test]
fn scopes_are_validated_as_a_closed_set() {
    let raw = MINIMAL.replace(
        "\"issues:read\", \"net:example.com\"",
        "\"issues:read\", \"bogus\"",
    );
    assert_eq!(
        invalid_message(&raw),
        "scopes[1]: unsupported scope \"bogus\""
    );
    let dup = MINIMAL.replace(
        "\"issues:read\", \"net:example.com\"",
        "\"issues:read\", \"issues:read\"",
    );
    assert_eq!(
        invalid_message(&dup),
        "scopes contains duplicate value \"issues:read\""
    );
    let empty = MINIMAL.replace(
        "\"scopes\": [\"issues:read\", \"net:example.com\"],",
        "\"scopes\": [],",
    );
    assert_eq!(invalid_message(&empty), "scopes must not be empty");
}

#[test]
fn config_schema_keeps_declaration_order_and_rejects_duplicates() {
    let raw = MINIMAL.replace(
        "\"contributes\": {",
        r#""config": {
              "zeta": {"type": "string", "label": "Z", "multiline": true},
              "alpha": {"type": "enum", "label": "A", "options": ["x", "y"]},
              "token": {"type": "secret", "label": "Token", "required": true}
            },
            "contributes": {"#,
    );
    let manifest = parse(&raw).expect("parses");
    let keys: Vec<&str> = manifest
        .config
        .fields
        .iter()
        .map(|field| field.key.as_str())
        .collect();
    assert_eq!(keys, vec!["zeta", "alpha", "token"]);
    assert!(manifest
        .config
        .field("alpha")
        .is_some_and(|field| field.kind == "enum"));
    // 序列化仍然是对象，且键序保住（`serde_json::Map` 默认排序，因此这里手写了 Serialize）
    let encoded = serde_json::to_string(&manifest.config).expect("encode");
    assert_eq!(
        encoded,
        r#"{"zeta":{"type":"string","label":"Z","multiline":true},"alpha":{"type":"enum","label":"A","options":["x","y"]},"token":{"type":"secret","label":"Token","required":true}}"#
    );
    // 往返
    let decoded: ConfigSchema = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, manifest.config);
    // 重复键由 visitor 拦下（`serde_json` 默认会静默取最后一个）
    let duplicate = r#"{"a":{"type":"string","label":"A"},"a":{"type":"string","label":"A"}}"#;
    assert!(serde_json::from_str::<ConfigSchema>(duplicate).is_err());
    // 非对象
    assert!(serde_json::from_str::<ConfigSchema>("[]").is_err());
}

#[test]
fn config_field_rules_match_upstream_messages() {
    let with_config = |body: &str| {
        parse(&MINIMAL.replace(
            "\"contributes\": {",
            &format!("\"config\": {body}, \"contributes\": {{"),
        ))
    };
    assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"a\": {\"type\": \"string\", \"label\": \"A\", \"options\": [\"x\"]}}, \"contributes\": {",
            )),
            "config.a.options is only valid for enum fields"
        );
    assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"a\": {\"type\": \"number\", \"label\": \"A\", \"multiline\": true}}, \"contributes\": {",
            )),
            "config.a.multiline is only valid for string fields"
        );
    assert_eq!(
        invalid_message(&MINIMAL.replace(
            "\"contributes\": {",
            "\"config\": {\"a\": {\"type\": \"enum\", \"label\": \"A\"}}, \"contributes\": {",
        )),
        "config.a.options must not be empty for enum fields"
    );
    assert_eq!(
        invalid_message(&MINIMAL.replace(
            "\"contributes\": {",
            "\"config\": {\"a\": {\"type\": \"date\", \"label\": \"A\"}}, \"contributes\": {",
        )),
        "config.a.type is unsupported: \"date\""
    );
    assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"contributes\": {",
                "\"config\": {\"Bad Key\": {\"type\": \"string\", \"label\": \"A\"}}, \"contributes\": {",
            )),
            "config contains invalid field name \"Bad Key\""
        );
    assert!(with_config(r#"{"a": {"type": "bool", "label": "A"}}"#).is_ok());
}

#[test]
fn surface_rules() {
    let build = |surfaces: &str| {
        format!(
            r#"{{
                  "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
                  "version": "1.2.3", "author": {{"name": "Example"}},
                  "scopes": ["issues:read", "net:example.com"],
                  "contributes": {{"surfaces": [{surfaces}]}}
                }}"#
        )
    };
    let surface_error = |surface: &str| match parse(&build(surface)) {
        Err(ManifestError::Invalid(message)) => message,
        other => panic!("expected invalid for {surface}, got {other:?}"),
    };
    assert!(parse(&build(
            r#"{"key": "panel", "type": "issue_panel", "name": "Panel", "entry": "panel.js", "platforms": ["web"]}"#
        ))
        .is_ok());
    let cases: &[(&str, &str)] = &[
            (
                r#"{"key": "panel", "type": "sidebar_panel", "name": "", "entry": "p.js"}"#,
                "contributes.surfaces[0].name must be non-empty without surrounding whitespace",
            ),
            (
                r#"{"key": "panel", "type": "floating", "name": "P", "entry": "p.js"}"#,
                "contributes.surfaces[0].type is unsupported: \"floating\"",
            ),
            (
                r#"{"key": "panel", "type": "modal", "name": "P", "entry": "p.html"}"#,
                "contributes.surfaces[0].entry must be a .js or .mjs script; the host renders the surface document itself",
            ),
            (
                r#"{"key": "panel", "type": "modal", "name": "P", "entry": "../p.js"}"#,
                "contributes.surfaces[0].entry must not contain path traversal",
            ),
            (
                r#"{"key": "panel", "type": "modal", "name": "P", "entry": "p.js", "platforms": ["ios"]}"#,
                "contributes.surfaces[0].platforms contains unsupported platform \"ios\"",
            ),
            (
                r#"{"key": "panel", "type": "issue_panel", "name": "P", "entry": "panel.js", "platforms": ["web", "web"]}"#,
                "contributes.surfaces[0].platforms contains duplicate platform \"web\"",
            ),
            (
                r#"{"key": "panel", "type": "issue_panel", "name": "P", "entry": "panel.js"}, {"key": "panel", "type": "modal", "name": "P2", "entry": "p2.js"}"#,
                "duplicate surface key \"panel\"",
            ),
        ];
    for (surface, expected) in cases {
        assert_eq!(surface_error(surface), *expected, "{surface}");
    }
}

#[test]
fn hook_rules() {
    let build = |hooks: &str| {
        format!(
            r#"{{
                  "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
                  "version": "1.2.3", "author": {{"name": "Example"}},
                  "scopes": ["issues:read", "net:example.com"],
                  "contributes": {{"hooks": [{hooks}]}}
                }}"#
        )
    };
    let hook_error = |hook: &str| match parse(&build(hook)) {
        Err(ManifestError::Invalid(message)) => message,
        other => panic!("expected invalid for {hook}, got {other:?}"),
    };
    assert!(parse(&build(
            r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#
        ))
        .is_ok());
    let cases: &[(&str, &str)] = &[
            (
                r#"{"key": "k", "name": "K", "description": "  ", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].description must be non-empty and at most 2000 bytes",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": [], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].triggers must not be empty",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "events": ["issue.created"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].events requires the event trigger",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "schedule": {"cron": "* * * * *", "timezone": "UTC"}, "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].schedule requires the schedule trigger",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["event"], "events": ["comment.created"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].events subscribes to \"comment.created\", which delivers content requiring the comments:read scope",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://api.example.com/h"}}"#,
                "contributes.hooks[0].transport.url host \"api.example.com\" is not covered by a net: scope",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "smtp", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].transport.type is unsupported: \"smtp\"",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}, "timeout_ms": 99}"#,
                "contributes.hooks[0].timeout_ms must be between 100 and 30000",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "input_schema": {"type": "array"}, "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "contributes.hooks[0].input_schema.type must be object",
            ),
            (
                r#"{"key": "k", "name": "K", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}, {"key": "k", "name": "K2", "description": "d", "triggers": ["ui"], "transport": {"type": "http", "url": "https://example.com/h"}}"#,
                "duplicate hook key \"k\"",
            ),
        ];
    for (hook, expected) in cases {
        assert_eq!(hook_error(hook), *expected, "{hook}");
    }
}

#[test]
fn schedule_rules_cover_inline_timezone_and_cadence() {
    let with_cron = |cron_expr: &str, timezone: &str| {
        parse(&MINIMAL.replace(
                r#""triggers": ["event"], "events": ["issue.created"],"#,
                &format!(
                    r#""triggers": ["schedule"], "schedule": {{"cron": "{cron_expr}", "timezone": "{timezone}"}},"#
                ),
            ))
    };
    assert!(with_cron("0 * * * *", "UTC").is_ok());
    assert!(with_cron("*/5 * * * *", "America/Argentina/Buenos_Aires").is_ok());
    assert_eq!(
            invalid_message(&MINIMAL.replace(
                r#""triggers": ["event"], "events": ["issue.created"],"#,
                r#""triggers": ["schedule"], "schedule": {"cron": "TZ=UTC 0 * * * *", "timezone": "UTC"},"#,
            )),
            "contributes.hooks[0].schedule.cron must not contain an inline timezone"
        );
    let too_frequent = match with_cron("* * * * *", "UTC") {
        Err(ManifestError::Invalid(message)) => message,
        other => panic!("expected invalid, got {other:?}"),
    };
    assert_eq!(
        too_frequent,
        "contributes.hooks[0].schedule.cron must not run more often than every five minutes"
    );
    let bad_timezone = match with_cron("0 * * * *", "Not a zone!") {
        Err(ManifestError::Invalid(message)) => message,
        other => panic!("expected invalid, got {other:?}"),
    };
    assert_eq!(
        bad_timezone,
        "contributes.hooks[0].schedule.timezone is invalid: \"Not a zone!\""
    );
    assert!(is_timezone_name("UTC"));
    assert!(is_timezone_name("Etc/GMT+8"));
    assert!(!is_timezone_name(""));
    assert!(!is_timezone_name("a/b/c d"));
}

#[test]
fn resource_rules() {
    let with_resource = |resource: &str| {
        parse(&MINIMAL.replace(
            "\"hooks\": [",
            &format!("\"resources\": [{resource}], \"hooks\": ["),
        ))
    };
    assert!(
        with_resource(r#"{"type": "skill", "key": "demo", "entry": "skills/demo/SKILL.md"}"#)
            .is_ok()
    );
    assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"hooks\": [",
                "\"resources\": [{\"type\": \"skill\", \"key\": \"demo\", \"entry\": \"skills/other/SKILL.md\"}], \"hooks\": [",
            )),
            "contributes.resources[0].entry must be \"skills/demo/SKILL.md\""
        );
    assert_eq!(
            invalid_message(&MINIMAL.replace(
                "\"hooks\": [",
                "\"resources\": [{\"type\": \"widget\", \"key\": \"demo\", \"entry\": \"skills/demo/SKILL.md\"}], \"hooks\": [",
            )),
            "contributes.resources[0].type is unsupported: \"widget\""
        );
}

#[test]
fn contributes_must_not_be_empty() {
    let empty = r#"{
          "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
          "version": "1.0.0", "author": {"name": "Example"},
          "scopes": ["issues:read"], "contributes": {}
        }"#;
    assert_eq!(
        invalid_message(empty),
        "contributes must declare at least one surface, hook, or resource"
    );
}

#[test]
fn https_url_parsing_matches_go_trimming() {
    assert!(parse_https_host("u", "https://example.com/x?y=1").is_ok());
    assert!(parse_https_host("u", " HTTPS://EXAMPLE.com ").is_ok());
    assert!(parse_https_host("u", "https://[::1]:8443/x").is_ok());
    // 上游 `url.Parse(strings.TrimSpace(value))` ⇒ 首尾空白被吃掉是**接受**。
    assert!(parse_https_host("u", "https://example.com\n").is_ok());
    for bad in [
        "http://example.com",
        "https://",
        "https://:8443",
        "https://user@example.com",
        "https://example.com/#f",
        "https://exa mple.com",
        "example.com",
    ] {
        assert!(parse_https_host("u", bad).is_err(), "{bad}");
    }
    assert_eq!(normalise_host("Example.COM."), "example.com");
    assert_eq!(
        parse_https_host("u", "https://EXAMPLE.com./x").unwrap(),
        "EXAMPLE.com."
    );
}
