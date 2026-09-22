//! Plugin 领域类型（multica 自有 plugin-sdk）。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// Plugin 主体。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plugin {
    pub id: Id,
    pub workspace_id: Option<Id>, // None = global
    pub plugin_key: String,
    pub display_name: String,
    pub version: String,
    pub status: String, // ready / installing / uninstalling / failed
    pub install_order: i32,
    pub manifest: serde_json::Value,
    pub package_path: Option<String>,
    pub config_revision: i64,
    pub secret_revision: i64,
    pub via_attribution: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// Plugin manifest v1。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifestV1 {
    pub manifest_version: String, // "v1"
    pub name: String,
    pub version: String,
    pub entry: String, // command to spawn
    pub hooks: Vec<String>,
    pub actions: Vec<PluginAction>,
    pub config_schema: Option<serde_json::Value>,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAction {
    pub name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_v1_serializes() {
        let m = PluginManifestV1 {
            manifest_version: "v1".into(),
            name: "test".into(),
            version: "0.1.0".into(),
            entry: "node ./index.js".into(),
            hooks: vec!["on_comment".into()],
            actions: vec![],
            config_schema: None,
            permissions: vec![],
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"v1\""));
    }
}
