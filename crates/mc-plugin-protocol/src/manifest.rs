//! Plugin manifest v1。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifestV1 {
    pub manifest_version: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    #[serde(default)]
    pub hooks: Vec<String>,
    #[serde(default)]
    pub actions: Vec<PluginAction>,
    #[serde(default)]
    pub config_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginAction {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip() {
        let m = PluginManifestV1 {
            manifest_version: "v1".into(),
            name: "x".into(),
            version: "0.1.0".into(),
            entry: "node index.js".into(),
            hooks: vec!["on_comment".into()],
            actions: vec![],
            config_schema: None,
            permissions: vec!["read_issues".into()],
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: PluginManifestV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "x");
        assert_eq!(back.hooks, vec!["on_comment".to_string()]);
    }
}
