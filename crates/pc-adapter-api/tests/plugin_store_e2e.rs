//! R743 regression: 验证 `pc-adapter-api::plugin_store`（原 pc-adapter-plugin-store）。
//!
//! 覆盖 JSON 文件存储 + 内存缓存关键路径：
//! - ensure_dirs / add / list / get_by_type / remove
//! - settings: get_disabled_types / set_disabled / is_disabled

use pc_adapter_api::plugin_store::{AdapterPluginRecord, AdapterPluginStore};
use tempfile::TempDir;

fn make_record(kind: &str, package_name: &str) -> AdapterPluginRecord {
    AdapterPluginRecord {
        package_name: package_name.to_string(),
        local_path: None,
        version: Some("1.0.0".to_string()),
        kind: kind.to_string(),
        installed_at: "2026-08-11T00:00:00Z".to_string(),
        disabled: None,
    }
}

#[tokio::test]
async fn add_and_list_round_trip() {
    let dir = TempDir::new().expect("tempdir");
    let store = AdapterPluginStore::new(dir.path().to_path_buf());

    store.add(make_record("claude-local", "droid-pkg-a")).await.expect("add a");
    store.add(make_record("codex-local", "droid-pkg-b")).await.expect("add b");

    let list = store.list().await.expect("list");
    assert_eq!(list.len(), 2);

    let a = store.get_by_type("claude-local").await.expect("get").expect("exists");
    assert_eq!(a.package_name, "droid-pkg-a");
    assert_eq!(a.version.as_deref(), Some("1.0.0"));

    let removed = store.remove("claude-local").await.expect("remove");
    assert!(removed);
    let list2 = store.list().await.expect("list2");
    assert_eq!(list2.len(), 1);

    let _ = dir.close();
}

#[tokio::test]
async fn add_replaces_same_kind() {
    let dir = TempDir::new().expect("tempdir");
    let store = AdapterPluginStore::new(dir.path().to_path_buf());

    store.add(make_record("claude-local", "old")).await.expect("add old");
    store.add(make_record("claude-local", "new")).await.expect("add new");

    let list = store.list().await.expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].package_name, "new");
    let _ = dir.close();
}

#[tokio::test]
async fn disabled_toggle_persists_to_disk() {
    let dir = TempDir::new().expect("tempdir");
    let store = AdapterPluginStore::new(dir.path().to_path_buf());

    assert!(!store.is_disabled("claude-local").await.expect("is_disabled"));
    assert_eq!(store.get_disabled_types().await.expect("get"), Vec::<String>::new());

    let changed = store.set_disabled("claude-local", true).await.expect("set");
    assert!(changed);
    assert!(store.is_disabled("claude-local").await.expect("is_disabled 2"));
    let types = store.get_disabled_types().await.expect("get2");
    assert_eq!(types, vec!["claude-local".to_string()]);

    let changed2 = store.set_disabled("claude-local", true).await.expect("set2");
    assert!(!changed2);

    let changed3 = store.set_disabled("claude-local", false).await.expect("set3");
    assert!(changed3);
    assert!(!store.is_disabled("claude-local").await.expect("is_disabled 3"));

    let settings_path = dir.path().join("adapter-settings.json");
    assert!(settings_path.exists(), "settings file should exist");
    let _ = dir.close();
}

#[tokio::test]
async fn ensure_dirs_creates_plugin_dir() {
    let dir = TempDir::new().expect("tempdir");
    let store = AdapterPluginStore::new(dir.path().to_path_buf());

    let path = store.ensure_dirs().await.expect("ensure_dirs");
    assert!(path.exists());
    assert!(path.ends_with("adapter-plugins"));

    store.add(make_record("codex-local", "pkg")).await.expect("add");
    assert!(path.exists());
    let _ = dir.close();
}

#[tokio::test]
async fn empty_dir_yields_empty_list() {
    let dir = TempDir::new().expect("tempdir");
    let store = AdapterPluginStore::new(dir.path().to_path_buf());

    let list = store.list().await.expect("list");
    assert!(list.is_empty());
    let _ = dir.close();
}
