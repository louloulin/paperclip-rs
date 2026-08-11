use std::sync::Arc;

use pc_plugin_state_store::{
    plugin_state_store, ListPluginStateFilter, PluginStateScopeKind, PluginStateStoreService,
    RecordingStateStoreHook, ScopeOptions, SetPluginStateInput, StateStoreHookEvent,
};
use pc_repos::Db;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

const URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";
static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn setup() -> (Db, PgPool) {
    (
        Db::connect(URL, 4, 1).await.unwrap(),
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(URL)
            .await
            .unwrap(),
    )
}

async fn insert_plugin(p: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO plugins (id, plugin_key, package_name, version, manifest_json, status) \
         VALUES ($1, $2, 'test-pkg', '0.0.1', '{\"id\":\"test\"}'::jsonb, 'installed')",
    )
    .bind(id)
    .bind(format!("pss-{}", Uuid::new_v4().simple()))
    .execute(p)
    .await
    .unwrap();
    id
}

async fn cleanup(p: &PgPool, plugin_id: Uuid) {
    let _ = sqlx::query("DELETE FROM plugin_state WHERE plugin_id = $1")
        .bind(plugin_id)
        .execute(p)
        .await;
    let _ = sqlx::query("DELETE FROM plugins WHERE id = $1")
        .bind(plugin_id)
        .execute(p)
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn set_get_delete_lifecycle_via_service() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let plugin_id = insert_plugin(&p).await;

    let recorder = Arc::new(RecordingStateStoreHook::default());
    let svc = PluginStateStoreService::with_dependencies(
        db.clone(),
        Arc::new(pc_plugin_state_store::AllowAllCapabilities),
        vec![recorder.clone() as Arc<dyn pc_plugin_state_store::StateStoreHook>],
    );

    // set
    svc.set(
        plugin_id,
        SetPluginStateInput {
            scope_kind: PluginStateScopeKind::Company,
            scope_id: Some("company-1".into()),
            namespace: Some("ns1".into()),
            state_key: "key1".into(),
            value: json!({"hello": "world"}),
        },
    )
    .await
    .unwrap();

    // get → hit
    let v = svc
        .get(
            plugin_id,
            PluginStateScopeKind::Company,
            "key1",
            ScopeOptions {
                scope_id: Some("company-1".into()),
                namespace: Some("ns1".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(v, Some(json!({"hello": "world"})));

    // get miss
    let miss = svc
        .get(
            plugin_id,
            PluginStateScopeKind::Company,
            "missing",
            ScopeOptions {
                scope_id: Some("company-1".into()),
                namespace: Some("ns1".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(miss, None);

    // list
    let listed = svc
        .list(
            plugin_id,
            ListPluginStateFilter {
                namespace: Some("ns1".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);

    // delete
    svc.delete(
        plugin_id,
        PluginStateScopeKind::Company,
        "key1",
        ScopeOptions {
            scope_id: Some("company-1".into()),
            namespace: Some("ns1".into()),
        },
    )
    .await
    .unwrap();

    // get → miss
    let v2 = svc
        .get(
            plugin_id,
            PluginStateScopeKind::Company,
            "key1",
            ScopeOptions {
                scope_id: Some("company-1".into()),
                namespace: Some("ns1".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(v2, None);

    // hook events captured
    let events = recorder.events_snapshot_async().await;
    assert!(events
        .iter()
        .any(|e| matches!(e, StateStoreHookEvent::SetWritten { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, StateStoreHookEvent::GetHit { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, StateStoreHookEvent::GetMiss { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, StateStoreHookEvent::DeleteRemoved { .. })));
    assert!(events
        .iter()
        .any(|e| matches!(e, StateStoreHookEvent::Listed { .. })));

    cleanup(&p, plugin_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn upsert_overwrites_existing_value() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let plugin_id = insert_plugin(&p).await;

    let store = plugin_state_store(&db);
    store
        .set(
            plugin_id,
            SetPluginStateInput {
                scope_kind: PluginStateScopeKind::Instance,
                scope_id: None,
                namespace: None,
                state_key: "k".into(),
                value: json!(1),
            },
        )
        .await
        .unwrap();

    store
        .set(
            plugin_id,
            SetPluginStateInput {
                scope_kind: PluginStateScopeKind::Instance,
                scope_id: None,
                namespace: None,
                state_key: "k".into(),
                value: json!(2),
            },
        )
        .await
        .unwrap();

    let v = store
        .get(
            plugin_id,
            PluginStateScopeKind::Instance,
            "k",
            ScopeOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(v, Some(json!(2)));

    cleanup(&p, plugin_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn delete_all_removes_everything() {
    let _g = LOCK.lock().await;
    let (db, p) = setup().await;
    let plugin_id = insert_plugin(&p).await;

    let store = plugin_state_store(&db);
    for i in 0..3 {
        store
            .set(
                plugin_id,
                SetPluginStateInput {
                    scope_kind: PluginStateScopeKind::Instance,
                    scope_id: None,
                    namespace: None,
                    state_key: format!("k{i}"),
                    value: json!(i),
                },
            )
            .await
            .unwrap();
    }
    let before = store
        .list(plugin_id, ListPluginStateFilter::default())
        .await
        .unwrap();
    assert_eq!(before.len(), 3);

    store.delete_all(plugin_id).await.unwrap();

    let after = store
        .list(plugin_id, ListPluginStateFilter::default())
        .await
        .unwrap();
    assert_eq!(after.len(), 0);

    cleanup(&p, plugin_id).await;
}

#[tokio::test(flavor = "current_thread")]
async fn set_with_nonexistent_plugin_returns_error() {
    let _g = LOCK.lock().await;
    let (db, _p) = setup().await;
    let store = plugin_state_store(&db);
    let res = store
        .set(
            Uuid::new_v4(),
            SetPluginStateInput {
                scope_kind: PluginStateScopeKind::Instance,
                scope_id: None,
                namespace: None,
                state_key: "k".into(),
                value: json!(1),
            },
        )
        .await;
    assert!(res.is_err());
}
