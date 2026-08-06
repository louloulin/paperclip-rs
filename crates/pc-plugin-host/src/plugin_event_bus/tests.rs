//! Plugin event bus 模块内单测（涵盖 pattern / filter / bus / namespace 四组）。
//!
//! 与 Node 端 `plugin-event-bus.ts` 1:1 对齐 + Round 105 mod/ 拆分经验（tests.rs 单文件聚合）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chrono::Utc;
use serde_json::json;

use super::filter::passes_filter;
use super::*;

// ============================================================================
// Helpers
// ============================================================================

fn event(event_type: &str, company_id: &str) -> PluginEvent {
    PluginEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        occurred_at: Utc::now(),
        actor_id: None,
        actor_type: None,
        entity_id: None,
        entity_type: None,
        company_id: company_id.to_string(),
        payload: json!({}),
    }
}

fn event_with_entity(
    event_type: &str,
    entity_type: &str,
    entity_id: &str,
    company_id: &str,
) -> PluginEvent {
    PluginEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        occurred_at: Utc::now(),
        actor_id: None,
        actor_type: None,
        entity_id: Some(entity_id.to_string()),
        entity_type: Some(entity_type.to_string()),
        company_id: company_id.to_string(),
        payload: json!({}),
    }
}

fn event_with_payload(
    event_type: &str,
    company_id: &str,
    payload: serde_json::Value,
) -> PluginEvent {
    PluginEvent {
        event_id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        occurred_at: Utc::now(),
        actor_id: None,
        actor_type: None,
        entity_id: None,
        entity_type: None,
        company_id: company_id.to_string(),
        payload,
    }
}

// ============================================================================
// pattern::matches_pattern tests
// ============================================================================

#[test]
fn pattern_exact_match() {
    assert!(matches_pattern("issue.created", "issue.created"));
}

#[test]
fn pattern_exact_no_match() {
    assert!(!matches_pattern("issue.created", "issue.updated"));
}

#[test]
fn pattern_wildcard_suffix_matches() {
    assert!(matches_pattern(
        "plugin.acme.linear.sync-done",
        "plugin.acme.linear.*"
    ));
    assert!(matches_pattern("plugin.foo.bar", "plugin.foo.*"));
}

#[test]
fn pattern_wildcard_suffix_does_not_match_different_namespace() {
    assert!(!matches_pattern("plugin.other.event", "plugin.acme.*"));
}

#[test]
fn pattern_wildcard_requires_dot_prefix() {
    // "foo*" 不应识别为通配
    assert!(!matches_pattern("foobar", "foo*"));
    // "foo.*bar" 不应识别为通配（不是尾随 .*）
    assert!(!matches_pattern("foo.xbar", "foo.*bar"));
}

#[test]
fn validate_event_name_rejects_empty() {
    let r = validate_event_name("acme", "");
    assert!(matches!(r, Err(ScopedBusError::EmptyEventName { .. })));
    let r2 = validate_event_name("acme", "   ");
    assert!(matches!(r2, Err(ScopedBusError::EmptyEventName { .. })));
}

#[test]
fn validate_event_name_rejects_plugin_prefix() {
    let r = validate_event_name("acme", "plugin.foo");
    assert!(matches!(r, Err(ScopedBusError::ForbiddenPrefix { .. })));
}

#[test]
fn validate_event_name_accepts_bare_name() {
    assert!(validate_event_name("acme", "sync-done").is_ok());
    assert!(validate_event_name("acme", "issue.updated").is_ok());
}

#[test]
fn namespaced_event_type_format() {
    assert_eq!(
        namespaced_event_type("acme.linear", "sync-done"),
        "plugin.acme.linear.sync-done"
    );
}

// ============================================================================
// filter::passes_filter tests
// ============================================================================

#[test]
fn filter_none_passes_all() {
    let e = event("issue.created", "c1");
    assert!(passes_filter(&e, None));
}

#[test]
fn filter_empty_object_passes_all() {
    let e = event("issue.created", "c1");
    assert!(passes_filter(&e, Some(&EventFilter::default())));
}

#[test]
fn filter_project_id_from_entity() {
    let e = event_with_entity("project.created", "project", "p1", "c1");
    let f = EventFilter {
        project_id: Some("p1".into()),
        ..Default::default()
    };
    assert!(passes_filter(&e, Some(&f)));
}

#[test]
fn filter_project_id_mismatch() {
    let e = event_with_entity("project.created", "project", "p1", "c1");
    let f = EventFilter {
        project_id: Some("p2".into()),
        ..Default::default()
    };
    assert!(!passes_filter(&e, Some(&f)));
}

#[test]
fn filter_project_id_from_payload_when_entity_type_differs() {
    let e = event_with_payload("issue.created", "c1", json!({"projectId": "p1"}));
    let f = EventFilter {
        project_id: Some("p1".into()),
        ..Default::default()
    };
    assert!(passes_filter(&e, Some(&f)));
}

#[test]
fn filter_company_id_from_payload() {
    let e = event_with_payload("issue.created", "c1", json!({"companyId": "c1"}));
    let f = EventFilter {
        company_id: Some("c1".into()),
        ..Default::default()
    };
    assert!(passes_filter(&e, Some(&f)));
}

#[test]
fn filter_company_id_mismatch() {
    let e = event_with_payload("issue.created", "c1", json!({"companyId": "c1"}));
    let f = EventFilter {
        company_id: Some("c2".into()),
        ..Default::default()
    };
    assert!(!passes_filter(&e, Some(&f)));
}

#[test]
fn filter_agent_id_from_entity() {
    let e = event_with_entity("agent.created", "agent", "a1", "c1");
    let f = EventFilter {
        agent_id: Some("a1".into()),
        ..Default::default()
    };
    assert!(passes_filter(&e, Some(&f)));
}

#[test]
fn filter_agent_id_from_payload() {
    let e = event_with_payload("agent.run.started", "c1", json!({"agentId": "a1"}));
    let f = EventFilter {
        agent_id: Some("a1".into()),
        ..Default::default()
    };
    assert!(passes_filter(&e, Some(&f)));
}

#[test]
fn filter_multiple_fields_anded() {
    let e = event_with_payload(
        "issue.created",
        "c1",
        json!({"projectId": "p1", "companyId": "c1", "agentId": "a1"}),
    );
    let f = EventFilter {
        project_id: Some("p1".into()),
        company_id: Some("c1".into()),
        agent_id: Some("a1".into()),
    };
    assert!(passes_filter(&e, Some(&f)));

    // 任何一个 mismatch → false
    let mut f2 = f.clone();
    f2.project_id = Some("p2".into());
    assert!(!passes_filter(&e, Some(&f2)));
}

// ============================================================================
// bus::emit tests
// ============================================================================

#[tokio::test]
async fn bus_emit_no_subscribers_is_noop() {
    let bus = PluginEventBus::new();
    let r = bus.emit(event("issue.created", "c1")).await;
    assert!(r.errors.is_empty());
}

#[tokio::test]
async fn bus_emit_calls_matching_handler() {
    let bus = PluginEventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();

    let scoped = bus.for_plugin("acme");
    scoped
        .subscribe(
            "issue.created",
            FilterOrHandler::Handler(move |_e: PluginEvent| {
                let c = counter_clone.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                }
            }),
            None,
        )
        .unwrap();

    let r = bus.emit(event("issue.created", "c1")).await;
    assert!(r.errors.is_empty());
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bus_emit_pattern_wildcard_matches_multiple_events() {
    let bus = PluginEventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let c1 = counter.clone();
    let c2 = counter.clone();

    let scoped = bus.for_plugin("acme");
    scoped
        .subscribe(
            "issue.*",
            FilterOrHandler::Handler(move |_e: PluginEvent| {
                let c = c1.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                }
            }),
            None,
        )
        .unwrap();
    scoped
        .subscribe(
            "issue.created",
            FilterOrHandler::Handler(move |_e: PluginEvent| {
                let c = c2.clone();
                async move {
                    c.fetch_add(10, Ordering::SeqCst);
                }
            }),
            None,
        )
        .unwrap();

    bus.emit(event("issue.created", "c1")).await;
    bus.emit(event("issue.updated", "c1")).await;
    bus.emit(event("issue.deleted", "c1")).await;

    // issue.created matches both "issue.*" and "issue.created" → +11
    // issue.updated/deleted match "issue.*" only → +1 each
    // Total: 11 + 1 + 1 = 13
    assert_eq!(counter.load(Ordering::SeqCst), 13);
}

#[tokio::test]
async fn bus_emit_with_filter_only_delivers_matching_events() {
    let bus = PluginEventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();

    let scoped = bus.for_plugin("acme");
    let filter = EventFilter {
        project_id: Some("p1".into()),
        ..Default::default()
    };
    scoped
        .subscribe(
            "issue.created",
            FilterOrHandler::Filter(filter),
            Some(move |_e: PluginEvent| {
                let c = c.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                }
            }),
        )
        .unwrap();

    // matching
    bus.emit(event_with_payload(
        "issue.created",
        "c1",
        json!({"projectId": "p1"}),
    ))
    .await;
    // mismatch
    bus.emit(event_with_payload(
        "issue.created",
        "c1",
        json!({"projectId": "p2"}),
    ))
    .await;

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn bus_per_plugin_isolation() {
    let bus = PluginEventBus::new();
    let counter_a = Arc::new(AtomicUsize::new(0));
    let counter_b = Arc::new(AtomicUsize::new(0));
    let ca = counter_a.clone();
    let cb = counter_b.clone();

    let a = bus.for_plugin("plugin-a");
    a.subscribe(
        "issue.*",
        FilterOrHandler::Handler(move |_e: PluginEvent| {
            let c = ca.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }),
        None,
    )
    .unwrap();

    let b = bus.for_plugin("plugin-b");
    b.subscribe(
        "issue.*",
        FilterOrHandler::Handler(move |_e: PluginEvent| {
            let c = cb.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }),
        None,
    )
    .unwrap();

    bus.emit(event("issue.created", "c1")).await;

    assert_eq!(counter_a.load(Ordering::SeqCst), 1);
    assert_eq!(counter_b.load(Ordering::SeqCst), 1);

    // Clear only plugin-a
    a.clear();
    assert_eq!(bus.subscription_count("plugin-a"), 0);
    assert_eq!(bus.subscription_count("plugin-b"), 1);
}

#[tokio::test]
async fn bus_clear_plugin_removes_all_subs() {
    let bus = PluginEventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();

    let a = bus.for_plugin("acme");
    a.subscribe(
        "issue.*",
        FilterOrHandler::Handler(move |_e: PluginEvent| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }),
        None,
    )
    .unwrap();

    bus.emit(event("issue.created", "c1")).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);

    a.clear();
    bus.emit(event("issue.created", "c1")).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1); // unchanged
}

// ============================================================================
// ScopedPluginEventBus tests
// ============================================================================

#[tokio::test]
async fn scoped_emit_auto_namespaces() {
    let bus = PluginEventBus::new();
    let received_type = Arc::new(std::sync::Mutex::new(String::new()));
    let rt = received_type.clone();

    let a = bus.for_plugin("acme.linear");
    a.subscribe(
        "plugin.acme.linear.*",
        FilterOrHandler::Handler(move |e: PluginEvent| {
            let r = rt.clone();
            async move {
                *r.lock().unwrap() = e.event_type.clone();
            }
        }),
        None,
    )
    .unwrap();

    let r = a
        .emit("sync-done", "c1", json!({"ok": true}))
        .await
        .unwrap();
    assert!(r.errors.is_empty());
    assert_eq!(
        *received_type.lock().unwrap(),
        "plugin.acme.linear.sync-done"
    );
}

#[tokio::test]
async fn scoped_emit_rejects_plugin_prefix() {
    let bus = PluginEventBus::new();
    let a = bus.for_plugin("acme");
    let r = a.emit("plugin.foo", "c1", json!({})).await;
    assert!(matches!(r, Err(ScopedBusError::ForbiddenPrefix { .. })));
}

#[tokio::test]
async fn scoped_emit_rejects_empty_name() {
    let bus = PluginEventBus::new();
    let a = bus.for_plugin("acme");
    let r = a.emit("", "c1", json!({})).await;
    assert!(matches!(r, Err(ScopedBusError::EmptyEventName { .. })));
    let r2 = a.emit("   ", "c1", json!({})).await;
    assert!(matches!(r2, Err(ScopedBusError::EmptyEventName { .. })));
}

#[tokio::test]
async fn scoped_emit_rejects_empty_company_id() {
    let bus = PluginEventBus::new();
    let a = bus.for_plugin("acme");
    let r = a.emit("sync-done", "", json!({})).await;
    assert!(matches!(r, Err(ScopedBusError::EmptyCompanyId { .. })));
}

#[tokio::test]
async fn scoped_subscribe_requires_handler_when_filter_given() {
    let bus = PluginEventBus::new();
    let a = bus.for_plugin("acme");
    let r = a.subscribe(
        "issue.*",
        FilterOrHandler::Filter(EventFilter::default()),
        None::<fn(PluginEvent) -> std::future::Ready<()>>,
    );
    assert!(matches!(r, Err(ScopedBusError::MissingHandlerWithFilter)));
}

// ============================================================================
// FilterOrHandler ergonomics
// ============================================================================

#[tokio::test]
async fn filter_or_handler_handler_path_works() {
    let bus = PluginEventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();

    let a = bus.for_plugin("acme");
    let r = a.subscribe(
        "issue.created",
        FilterOrHandler::Handler(move |_e: PluginEvent| {
            let c = c.clone();
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }),
        None,
    );
    assert!(r.is_ok());

    bus.emit(event("issue.created", "c1")).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
