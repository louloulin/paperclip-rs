use super::*;
use serde_json::json;

fn envelope(event: &str, payload: Value) -> WebhookEnvelope {
    WebhookEnvelope {
        event: event.to_owned(),
        event_payload: payload,
        request: WebhookRequest {
            received_at: "2026-01-01T00:00:00Z".to_owned(),
            content_type: None,
        },
    }
}

fn headers(pairs: &[(&str, &str)]) -> WebhookHeaders {
    let mut headers = WebhookHeaders::new();
    for (name, value) in pairs {
        headers.set(name, value);
    }
    headers
}

#[test]
fn unknown_headers_are_dropped() {
    let h = headers(&[("Authorization", "Bearer secret"), ("User-Agent", "gh/1.0")]);
    assert_eq!(h.user_agent.as_deref(), Some("gh/1.0"));
    assert_eq!(h.to_selected_json(), json!({"user-agent": "gh/1.0"}));
}

/// `INBOUND_HEADER_NAMES` 是 `mc-http` 折 `HeaderMap` 时用的名单，必须与 `set` 的 `match`
/// 一一对应：少一个名字 ⇒ 某个头永远收不到；多一个名字 ⇒ `set` 静默丢弃（无声丢数据）。
#[test]
fn inbound_header_names_match_the_set_arms_exactly() {
    let mut slots = std::collections::BTreeSet::new();
    for name in WebhookHeaders::INBOUND_HEADER_NAMES {
        let mut h = WebhookHeaders::new();
        h.set(name, "v");
        assert_ne!(
            h,
            WebhookHeaders::new(),
            "`{name}` 不在 `set` 的名单里（会被静默丢弃）"
        );
        assert!(
            slots.insert(format!("{h:?}")),
            "`{name}` 与另一个名字绑到同一个字段"
        );
    }
    assert_eq!(slots.len(), WebhookHeaders::INBOUND_HEADER_NAMES.len());
}

#[test]
fn signature_value_never_lands_in_selected_headers() {
    let h = headers(&[("X-Hub-Signature-256", "sha256=deadbeef")]);
    assert_eq!(
        h.to_selected_json(),
        json!({"x-hub-signature-256-present": true})
    );
}

#[test]
fn round_trips_through_selected_headers() {
    let h = headers(&[
        ("X-GitHub-Event", "issues"),
        ("X-GitHub-Delivery", "abc"),
        ("Idempotency-Key", "key-1"),
    ]);
    let selected = h.to_selected_json();
    let rebuilt = WebhookHeaders::from_selected(&selected, Some("application/json"));
    assert_eq!(rebuilt.x_github_event.as_deref(), Some("issues"));
    assert_eq!(rebuilt.x_github_delivery.as_deref(), Some("abc"));
    assert_eq!(rebuilt.idempotency_key.as_deref(), Some("key-1"));
    assert_eq!(rebuilt.content_type.as_deref(), Some("application/json"));
    // 签名头只留 present 标记 ⇒ 重建后必然为空。
    assert!(rebuilt.x_hub_signature_256.is_none());
}

#[test]
fn bom_and_scalars_are_rejected() {
    assert_eq!(
        normalize_webhook_payload(b"\xef\xbb\xbf", &WebhookHeaders::new()),
        Err("empty body".to_owned())
    );
    assert_eq!(
        normalize_webhook_payload(br#""hello""#, &WebhookHeaders::new()),
        Err("body must be a JSON object or array".to_owned())
    );
    assert!(normalize_webhook_payload(b"{ not json", &WebhookHeaders::new()).is_err());
}

#[test]
fn bom_is_stripped_before_parsing() {
    let env = normalize_webhook_payload(b"\xef\xbb\xbf{\"hello\":1}", &WebhookHeaders::new())
        .expect("BOM-prefixed object");
    assert_eq!(env.event, DEFAULT_EVENT);
    assert_eq!(env.event_payload, json!({"hello": 1}));
}

#[test]
fn caller_supplied_envelope_wins() {
    let env = normalize_webhook_payload(
        br#"{"event":"custom.ping","eventPayload":{"a":1}}"#,
        &WebhookHeaders::new(),
    )
    .expect("envelope");
    assert_eq!(env.event, "custom.ping");
    assert_eq!(env.event_payload, json!({"a": 1}));
}

#[test]
fn event_without_payload_falls_back_to_whole_body() {
    let env = normalize_webhook_payload(br#"{"event":"custom.ping"}"#, &WebhookHeaders::new())
        .expect("envelope");
    assert_eq!(env.event, "custom.ping");
    assert_eq!(env.event_payload, json!({"event": "custom.ping"}));
}

#[test]
fn infer_event_prefers_headers_then_body() {
    assert_eq!(
        normalize_webhook_payload(
            br#"{"action":"opened"}"#,
            &headers(&[("X-GitHub-Event", "issues")])
        )
        .expect("normalize")
        .event,
        "github.issues.opened"
    );
    assert_eq!(
        normalize_webhook_payload(
            br#"{"action":"opened"}"#,
            &headers(&[
                ("X-GitHub-Event", "issues"),
                ("X-Gitlab-Event", "Push Hook")
            ])
        )
        .expect("normalize")
        .event,
        "github.issues.opened"
    );
    assert_eq!(
        normalize_webhook_payload(br#"{"type":"build"}"#, &WebhookHeaders::new())
            .expect("normalize")
            .event,
        "build"
    );
    assert_eq!(
        normalize_webhook_payload(br"{}", &WebhookHeaders::new())
            .expect("normalize")
            .event,
        DEFAULT_EVENT
    );
}

#[test]
fn content_type_is_trimmed_at_the_first_semicolon() {
    assert_eq!(
        normalize_webhook_payload(
            br#"{"a":1}"#,
            &headers(&[("Content-Type", "application/json; charset=utf-8")])
        )
        .expect("normalize")
        .request
        .content_type
        .as_deref(),
        Some("application/json")
    );
    assert_eq!(
        normalize_webhook_payload(br#"{"a":1}"#, &headers(&[("Content-Type", "  ")]))
            .expect("normalize")
            .request
            .content_type,
        // 上游只在分号分支 trim ⇒ 无分号的 "  " 原样留下（非空 ⇒ 出现在信封里）。
        Some("  ".to_owned())
    );
    // 完全空串 ⇒ `omitempty` ⇒ `None`。
    assert_eq!(
        normalize_webhook_payload(br#"{"a":1}"#, &headers(&[("Content-Type", "")]))
            .expect("normalize")
            .request
            .content_type,
        None
    );
}

#[test]
fn dedupe_key_precedence_follows_provider() {
    let h = headers(&[("X-GitHub-Delivery", " d1 "), ("Idempotency-Key", "k1")]);
    assert_eq!(
        extract_dedupe_key("github", &h),
        (Some("d1".to_owned()), Some("x-github-delivery".to_owned()))
    );
    assert_eq!(
        extract_dedupe_key("generic", &h),
        (Some("k1".to_owned()), Some("idempotency-key".to_owned()))
    );
    let only_github = headers(&[("X-GitHub-Delivery", "d1")]);
    assert_eq!(
        extract_dedupe_key("generic", &only_github),
        (Some("d1".to_owned()), Some("x-github-delivery".to_owned()))
    );
    assert_eq!(
        extract_dedupe_key("generic", &WebhookHeaders::new()),
        (None, None)
    );
}

#[test]
fn split_event_handles_qualified_and_bare_names() {
    assert_eq!(
        split_webhook_event("github.workflow_run.completed"),
        (
            "github".to_owned(),
            "workflow_run".to_owned(),
            "completed".to_owned()
        )
    );
    assert_eq!(
        split_webhook_event("github.push"),
        ("github".to_owned(), "push".to_owned(), String::new())
    );
    assert_eq!(
        split_webhook_event("issues"),
        (String::new(), "issues".to_owned(), String::new())
    );
    assert_eq!(
        split_webhook_event("issues.opened"),
        (String::new(), "issues".to_owned(), "opened".to_owned())
    );
    assert_eq!(
        split_webhook_event(""),
        (String::new(), String::new(), String::new())
    );
}

#[test]
fn scope_without_filters_allows_everything() {
    let env = envelope("anything", json!({}));
    assert!(event_allowed_by_trigger_scope(None, &env));
    assert!(event_allowed_by_trigger_scope(Some(&Value::Null), &env));
    assert!(event_allowed_by_trigger_scope(Some(&json!([])), &env));
}

#[test]
fn scope_matches_event_name_without_action_restriction() {
    let filters = json!([{"event": "issues"}]);
    assert!(event_allowed_by_trigger_scope(
        Some(&filters),
        &envelope("github.issues.opened", json!({}))
    ));
    assert!(!event_allowed_by_trigger_scope(
        Some(&filters),
        &envelope("github.push", json!({}))
    ));
}

#[test]
fn scope_matches_action_from_event_suffix_or_payload() {
    let filters = json!([{"event": "workflow_run", "actions": ["completed"]}]);
    assert!(event_allowed_by_trigger_scope(
        Some(&filters),
        &envelope("github.workflow_run.completed", json!({}))
    ));
    assert!(!event_allowed_by_trigger_scope(
        Some(&filters),
        &envelope("github.workflow_run.started", json!({}))
    ));
    // 事件名里不带动作时，从 body 字段取。
    assert!(event_allowed_by_trigger_scope(
        Some(&filters),
        &envelope("github.workflow_run", json!({"conclusion": "completed"}))
    ));
}

#[test]
fn scope_keeps_scanning_same_named_rows() {
    // 第一行同名但不匹配，第二行匹配 ⇒ 必须放行（不短路）。
    let filters = json!([
        {"event": "workflow_run", "actions": ["started"]},
        {"event": "workflow_run", "actions": ["completed"]},
    ]);
    assert!(event_allowed_by_trigger_scope(
        Some(&filters),
        &envelope("github.workflow_run.completed", json!({}))
    ));
    assert_eq!(
        matching_filter_rows(Some(&filters), "github.workflow_run.completed"),
        2
    );
}

#[test]
fn malformed_filters_fail_closed() {
    assert!(!event_allowed_by_trigger_scope(
        Some(&json!([{"event": 7}])),
        &envelope("issues", json!({}))
    ));
    assert!(!event_allowed_by_trigger_scope(
        Some(&json!({"event": "issues"})),
        &envelope("issues", json!({}))
    ));
}

#[test]
fn action_candidates_are_ordered_and_deduped() {
    let candidates = webhook_action_candidates(
        " completed ",
        &json!({"action": "completed", "status": "queued"}),
    );
    assert_eq!(
        candidates,
        vec!["completed".to_owned(), "queued".to_owned()]
    );
}

#[test]
fn token_shape_assertion_matches_minted_tokens() {
    let token = format!(
        "{}abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ",
        super::super::WEBHOOK_TOKEN_PREFIX
    );
    assert_eq!(token.len(), 47);
    assert!(looks_like_webhook_token(&token));
    assert!(!looks_like_webhook_token("awt_short"));
    assert!(!looks_like_webhook_token(&token.replace("awt_", "xwt_")));
}
