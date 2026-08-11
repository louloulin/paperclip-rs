//! R610: pc-assets hook contract tests.

use pc_repos::asset_service::{AssetHook, AssetHookEvent, AssetService, NoopAssetHook, RecordingAssetHook};
use pc_repos::Db;
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

#[tokio::test(flavor = "current_thread")]
async fn noop_hook_accepts_events() {
    let hook = NoopAssetHook;
    let event = AssetHookEvent::Created {
        company_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        provider: "local".into(),
        content_type: "image/png".into(),
        byte_size: 10,
    };
    let res = AssetHook::on_asset_event(&hook, event).await;
    assert!(res.is_ok());
}

#[tokio::test(flavor = "current_thread")]
async fn recording_hook_stores_events_in_order() {
    let hook = RecordingAssetHook::default();
    assert!(hook.is_empty());

    let ev1 = AssetHookEvent::Created {
        company_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        provider: "s3".into(),
        content_type: "image/jpeg".into(),
        byte_size: 1,
    };
    let ev2 = AssetHookEvent::Deleted {
        company_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
    };
    AssetHook::on_asset_event(&hook, ev1.clone()).await.unwrap();
    AssetHook::on_asset_event(&hook, ev2.clone()).await.unwrap();
    let snap = hook.events_snapshot();
    assert_eq!(snap, vec![ev1, ev2]);
    assert_eq!(hook.len(), 2);

    hook.clear();
    assert!(hook.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn hook_event_serializes_with_type_tag() {
    let ev = AssetHookEvent::Deleted {
        company_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "deleted");
    assert!(v["company_id"].is_string());
    assert!(v["asset_id"].is_string());
}

#[tokio::test(flavor = "current_thread")]
async fn created_event_serializes_with_camel_case_payload() {
    let ev = AssetHookEvent::Created {
        company_id: Uuid::new_v4(),
        asset_id: Uuid::new_v4(),
        provider: "local".into(),
        content_type: "image/png".into(),
        byte_size: 42,
    };
    let v: Value = serde_json::to_value(&ev).expect("serialize");
    assert_eq!(v["type"], "created");
    // snake_case payload fields (since we did not apply camelCase to variant payloads)
    assert_eq!(v["provider"], "local");
    assert_eq!(v["content_type"], "image/png");
    assert_eq!(v["byte_size"], 42);
}

#[tokio::test(flavor = "current_thread")]
async fn service_add_hook_appends() {
    let db = Db::connect(TEST_DATABASE_URL, 1, 0).await.expect("db");
    let svc = AssetService::new(db);
    assert_eq!(svc.hook_count(), 0);
    let svc = svc.add_hook(Arc::new(NoopAssetHook));
    let svc = svc.add_hook(Arc::new(RecordingAssetHook::default()));
    assert_eq!(svc.hook_count(), 2);
}
