//! 插件 metadata 注册表。
//!
//! 与原 `server/src/services/plugin-registry.ts` 中纯 metadata 路径等价。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use pc_plugin_protocol::manifest::PaperclipPluginManifestV1;

/// 插件 metadata 条目。
#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub plugin_id: Uuid,
    pub plugin_key: String,
    pub manifest: PaperclipPluginManifestV1,
    pub install_order: i32,
    pub status: PluginStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStatus {
    Installed,
    Ready,
    Error,
    Disabled,
    Uninstalled,
}

impl PluginStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Ready => "ready",
            Self::Error => "error",
            Self::Disabled => "disabled",
            Self::Uninstalled => "uninstalled",
        }
    }
}

/// 进程内 plugin metadata 注册表。
#[derive(Default)]
pub struct PluginRegistry {
    by_id: Arc<RwLock<HashMap<Uuid, PluginEntry>>>,
    by_key: Arc<RwLock<HashMap<String, Uuid>>>,
}

impl PluginRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, entry: PluginEntry) -> Result<(), String> {
        let mut by_id = self.by_id.write().expect("plugin registry poisoned");
        let mut by_key = self.by_key.write().expect("plugin registry poisoned");
        if by_id.contains_key(&entry.plugin_id) {
            return Err(format!("plugin already registered: {}", entry.plugin_key));
        }
        if by_key.contains_key(&entry.plugin_key) {
            return Err(format!(
                "plugin key already registered: {}",
                entry.plugin_key
            ));
        }
        by_key.insert(entry.plugin_key.clone(), entry.plugin_id);
        by_id.insert(entry.plugin_id, entry);
        Ok(())
    }

    pub fn unregister(&self, plugin_id: &Uuid) -> Option<PluginEntry> {
        let mut by_id = self.by_id.write().expect("plugin registry poisoned");
        let removed = by_id.remove(plugin_id);
        if let Some(ref entry) = removed {
            let mut by_key = self.by_key.write().expect("plugin registry poisoned");
            by_key.remove(&entry.plugin_key);
        }
        removed
    }

    #[must_use]
    pub fn get_by_id(&self, plugin_id: &Uuid) -> Option<PluginEntry> {
        let by_id = self.by_id.read().expect("plugin registry poisoned");
        by_id.get(plugin_id).cloned()
    }

    #[must_use]
    pub fn get_by_key(&self, key: &str) -> Option<PluginEntry> {
        let by_id = self.by_id.read().expect("plugin registry poisoned");
        let by_key = self.by_key.read().expect("plugin registry poisoned");
        by_key.get(key).and_then(|id| by_id.get(id)).cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<PluginEntry> {
        let by_id = self.by_id.read().expect("plugin registry poisoned");
        let mut entries: Vec<PluginEntry> = by_id.values().cloned().collect();
        entries.sort_by_key(|e| e.install_order);
        entries
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let by_id = self.by_id.read().expect("plugin registry poisoned");
        by_id.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn update_status(&self, plugin_id: &Uuid, status: PluginStatus) -> bool {
        let mut by_id = self.by_id.write().expect("plugin registry poisoned");
        if let Some(entry) = by_id.get_mut(plugin_id) {
            entry.status = status;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(key: &str) -> PluginEntry {
        PluginEntry {
            plugin_id: Uuid::new_v4(),
            plugin_key: key.into(),
            manifest: PaperclipPluginManifestV1 {
                id: key.into(),
                version: "1.0.0".into(),
                manifest_version: "v1".into(),
                label: "Test".into(),
                description: String::new(),
                author: None,
                entry: "dist/worker.js".into(),
                capabilities: vec![],
                config_schema: serde_json::Value::Null,
                ui_contributions: vec![],
                metadata: serde_json::Value::Null,
                local_folders: vec![],
            },
            install_order: 0,
            status: PluginStatus::Installed,
        }
    }

    #[test]
    fn register_and_lookup() {
        let reg = PluginRegistry::new();
        let entry = test_entry("test.plugin");
        let id = entry.plugin_id;
        reg.register(entry).unwrap();
        assert!(reg.get_by_id(&id).is_some());
        assert!(reg.get_by_key("test.plugin").is_some());
    }

    #[test]
    fn unregister_removes_both_indices() {
        let reg = PluginRegistry::new();
        let entry = test_entry("test.plugin");
        let id = entry.plugin_id;
        reg.register(entry).unwrap();
        let removed = reg.unregister(&id);
        assert!(removed.is_some());
        assert!(reg.get_by_id(&id).is_none());
        assert!(reg.get_by_key("test.plugin").is_none());
    }

    #[test]
    fn duplicate_key_rejected() {
        let reg = PluginRegistry::new();
        reg.register(test_entry("test.plugin")).unwrap();
        let result = reg.register(test_entry("test.plugin"));
        assert!(result.is_err());
    }

    #[test]
    fn list_sorted_by_install_order() {
        let reg = PluginRegistry::new();
        let mut a = test_entry("a");
        a.install_order = 5;
        let mut b = test_entry("b");
        b.install_order = 1;
        let mut c = test_entry("c");
        c.install_order = 3;
        reg.register(a).unwrap();
        reg.register(b).unwrap();
        reg.register(c).unwrap();

        let keys: Vec<_> = reg.list().into_iter().map(|e| e.plugin_key).collect();
        assert_eq!(keys, vec!["b", "c", "a"]);
    }

    #[test]
    fn update_status() {
        let reg = PluginRegistry::new();
        let entry = test_entry("test.plugin");
        let id = entry.plugin_id;
        reg.register(entry).unwrap();
        assert!(reg.update_status(&id, PluginStatus::Ready));
        assert_eq!(reg.get_by_id(&id).unwrap().status, PluginStatus::Ready);
    }

    #[test]
    fn len_and_is_empty() {
        let reg = PluginRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        reg.register(test_entry("a")).unwrap();
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_empty());
    }
}
