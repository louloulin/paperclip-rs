//! Tests for `pc-adapter-plugin-store` against temp-dir managed home.

use pc_adapter_plugin_store::{AdapterPluginRecord, AdapterPluginStore};
use std::sync::Mutex;
use tempfile::TempDir;

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn fresh_home() -> TempDir {
    let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    TempDir::new().expect("tempdir")
}

fn make_record(kind: &str, name: &str) -> AdapterPluginRecord {
    AdapterPluginRecord {
        package_name: name.to_string(),
        local_path: None,
        version: Some("1.0.0".to_string()),
        kind: kind.to_string(),
        installed_at: "2024-01-01T00:00:00Z".to_string(),
        disabled: None,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn empty_home_returns_empty_list() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    let list = store.list().await.expect("list");
    assert!(list.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn add_and_list_single_record() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.add(make_record("claude_local", "claude-paperclip")).await.expect("add");
    let list = store.list().await.expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].kind, "claude_local");
    assert_eq!(list[0].package_name, "claude-paperclip");
}

#[tokio::test(flavor = "current_thread")]
async fn add_with_same_type_replaces_existing() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.add(make_record("claude_local", "v1")).await.expect("add v1");
    store.add(make_record("claude_local", "v2")).await.expect("add v2");
    let list = store.list().await.expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].package_name, "v2");
}

#[tokio::test(flavor = "current_thread")]
async fn add_with_different_types_appends() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.add(make_record("claude_local", "claude")).await.expect("add claude");
    store.add(make_record("codex_local", "codex")).await.expect("add codex");
    let list = store.list().await.expect("list");
    assert_eq!(list.len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn remove_existing_returns_true() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.add(make_record("claude_local", "claude")).await.expect("add");
    let removed = store.remove("claude_local").await.expect("remove");
    assert!(removed);
    assert!(store.list().await.expect("list").is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn remove_nonexistent_returns_false() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    let removed = store.remove("nonexistent").await.expect("remove");
    assert!(!removed);
}

#[tokio::test(flavor = "current_thread")]
async fn get_by_type_returns_record() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.add(make_record("claude_local", "claude")).await.expect("add");
    let got = store.get_by_type("claude_local").await.expect("get");
    assert!(got.is_some());
    assert_eq!(got.unwrap().kind, "claude_local");
}

#[tokio::test(flavor = "current_thread")]
async fn get_by_type_returns_none_for_missing() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    let got = store.get_by_type("nonexistent").await.expect("get");
    assert!(got.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn ensure_dirs_creates_directory_and_package_json() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    let dir = store.plugins_dir_path().await.expect("ensure_dirs");
    assert!(dir.exists());
    assert!(dir.join("package.json").exists());
    // package.json 应包含 "paperclip-adapter-plugins" name
    let body = tokio::fs::read_to_string(dir.join("package.json"))
        .await
        .expect("read pkg.json");
    assert!(body.contains("paperclip-adapter-plugins"));
    assert!(body.contains("private"));
}

#[tokio::test(flavor = "current_thread")]
async fn ensure_dirs_idempotent_keeps_existing_package_json() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.plugins_dir_path().await.expect("first");
    // 第二次调用不应破坏现有 package.json
    store.plugins_dir_path().await.expect("second");
    let body = tokio::fs::read_to_string(home.path().join("adapter-plugins/package.json"))
        .await
        .expect("read pkg.json");
    assert!(body.contains("paperclip-adapter-plugins"));
}

#[tokio::test(flavor = "current_thread")]
async fn settings_default_is_empty() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    let disabled = store.get_disabled_types().await.expect("disabled");
    assert!(disabled.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn set_disabled_to_true_then_check_is_disabled() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    let changed = store.set_disabled("claude_local", true).await.expect("set");
    assert!(changed);
    assert!(store.is_disabled("claude_local").await.expect("is_disabled"));
}

#[tokio::test(flavor = "current_thread")]
async fn set_disabled_to_false_removes() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.set_disabled("claude_local", true).await.expect("set true");
    let changed = store.set_disabled("claude_local", false).await.expect("set false");
    assert!(changed);
    assert!(!store.is_disabled("claude_local").await.expect("is_disabled"));
}

#[tokio::test(flavor = "current_thread")]
async fn set_disabled_to_true_when_already_returns_false() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    store.set_disabled("claude_local", true).await.expect("set");
    let changed = store.set_disabled("claude_local", true).await.expect("set");
    assert!(!changed);
}

#[tokio::test(flavor = "current_thread")]
async fn cache_invalidates_on_add() {
    let home = fresh_home();
    let store = AdapterPluginStore::new(home.path());
    // 空盘首次读：缓存为空列表
    let list1 = store.list().await.expect("list1");
    assert!(list1.is_empty());
    // store.add 写盘 + 缓存
    store.add(make_record("claude_local", "x")).await.expect("add claude");
    assert_eq!(store.list().await.expect("list cached").len(), 1);
    // 直接覆写磁盘文件（模拟另一个进程写入）；同 store 缓存命中旧值
    let raw = serde_json::to_string_pretty(&vec![make_record("codex_local", "y")]).unwrap();
    tokio::fs::write(home.path().join("adapter-plugins.json"), raw + "\n")
        .await
        .expect("write direct");
    let list2 = store.list().await.expect("list2");
    assert_eq!(list2.len(), 1, "same-store cached read sees prior value, not external write");
    assert_eq!(list2[0].kind, "claude_local");
    // 新 store 实例首次读会从磁盘加载
    let fresh = AdapterPluginStore::new(home.path());
    let list_fresh = fresh.list().await.expect("list_fresh");
    assert_eq!(list_fresh.len(), 1, "fresh store reads disk and observes external write");
    assert_eq!(list_fresh[0].kind, "codex_local");
    // add 触发写盘：store 用当前缓存列表整个覆写文件（不合并外部修改）
    store.add(make_record("codex_local_v2", "z")).await.expect("add third");
    let raw_after = tokio::fs::read_to_string(home.path().join("adapter-plugins.json"))
        .await
        .expect("read after add");
    assert!(raw_after.contains("claude_local"), "original cached entry still present");
    assert!(raw_after.contains("codex_local_v2"));
}

#[tokio::test(flavor = "current_thread")]
async fn record_serializes_camel_case_with_type_field() {
    let rec = make_record("claude_local", "claude");
    let v = serde_json::to_value(&rec).unwrap();
    assert_eq!(v["type"], "claude_local");
    assert_eq!(v["packageName"], "claude");
    assert_eq!(v["installedAt"], "2024-01-01T00:00:00Z");
    assert!(v.get("localPath").is_none() || v["localPath"].is_null());
    assert!(v.get("disabled").is_none() || v["disabled"].is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn record_with_local_path_serialized() {
    let mut rec = make_record("claude_local", "claude");
    rec.local_path = Some("/opt/claude-adapter".to_string());
    let v = serde_json::to_value(&rec).unwrap();
    assert_eq!(v["localPath"], "/opt/claude-adapter");
}
