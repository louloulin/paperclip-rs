//! 插件 manifest 类型。
//!
//! 与原 `@paperclipai/shared` 的 `PaperclipPluginManifestV1` 等价。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// 插件 manifest 格式版本。
pub const PLUGIN_MANIFEST_VERSION: &str = "v1";

/// Manifest 作者。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifestAuthor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Capability 类型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginManifestCapabilityKind {
    Jobs,
    Events,
    Data,
    Actions,
    Tools,
    Webhooks,
    Ui,
    ExternalObjects,
    Environments,
    Access,
}

/// Manifest capability 声明。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifestCapability {
    pub kind: PluginManifestCapabilityKind,
    #[serde(default)]
    pub requires: Vec<String>,
}

/// UI contribution（侧栏/面板/bundle 入口）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifestUiContribution {
    pub kind: String,
    pub entry: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

/// Plugin 声明的本地文件夹（公司范围内的文件系统根）。
///
/// Mirrors `@paperclipai/shared` `PluginLocalFolderDeclaration`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLocalFolderDeclaration {
    /// 稳定标识符，在插件内唯一。
    pub folder_key: String,
    /// 显示名称。
    pub display_name: String,
    /// 可选的描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 访问级别，默认为 `readWrite`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<PluginLocalFolderAccess>,
    /// 必需子目录（相对路径）。
    #[serde(default)]
    pub required_directories: Vec<String>,
    /// 必需文件（相对路径）。
    #[serde(default)]
    pub required_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PluginLocalFolderAccess {
    Read,
    #[default]
    ReadWrite,
}

/// Paperclip 插件 manifest v1。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaperclipPluginManifestV1 {
    pub id: String,
    pub version: String,
    pub manifest_version: String,
    pub label: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<PluginManifestAuthor>,
    pub entry: String,
    #[serde(default)]
    pub capabilities: Vec<PluginManifestCapability>,
    #[serde(default)]
    pub config_schema: Value,
    #[serde(default)]
    pub ui_contributions: Vec<PluginManifestUiContribution>,
    #[serde(default)]
    pub metadata: Value,
    /// 插件声明的本地文件夹列表。
    #[serde(default)]
    pub local_folders: Vec<PluginLocalFolderDeclaration>,
}

impl PaperclipPluginManifestV1 {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() {
            return Err("plugin manifest: id is empty".into());
        }
        if self.version.is_empty() {
            return Err("plugin manifest: version is empty".into());
        }
        if self.manifest_version != PLUGIN_MANIFEST_VERSION {
            return Err(format!(
                "plugin manifest: unsupported manifest_version {} (expected {})",
                self.manifest_version, PLUGIN_MANIFEST_VERSION
            ));
        }
        if self.entry.is_empty() {
            return Err("plugin manifest: entry is empty".into());
        }
        Ok(())
    }

    pub fn has_capability(&self, kind: &PluginManifestCapabilityKind) -> bool {
        self.capabilities.iter().any(|c| &c.kind == kind)
    }
}

/// 在数据库中存储 manifest 元数据。
#[derive(Debug, Clone)]
pub struct StoredManifest {
    pub plugin_id: Uuid,
    pub plugin_key: String,
    pub package_name: String,
    pub package_path: String,
    pub version: String,
    pub api_version: String,
    pub categories: Vec<String>,
    pub manifest: PaperclipPluginManifestV1,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_validates_ok() {
        let m = PaperclipPluginManifestV1 {
            id: "test.plugin".into(),
            version: "1.0.0".into(),
            manifest_version: "v1".into(),
            label: "Test Plugin".into(),
            description: "For tests".into(),
            author: None,
            entry: "dist/worker.js".into(),
            capabilities: vec![],
            config_schema: Value::Null,
            ui_contributions: vec![],
            metadata: Value::Null,
            local_folders: vec![],
        };
        assert!(m.validate().is_ok());
    }

    #[test]
    fn manifest_rejects_empty_id() {
        let m = PaperclipPluginManifestV1 {
            id: String::new(),
            version: "1.0.0".into(),
            manifest_version: "v1".into(),
            label: "Test".into(),
            description: String::new(),
            author: None,
            entry: "x".into(),
            capabilities: vec![],
            config_schema: Value::Null,
            ui_contributions: vec![],
            metadata: Value::Null,
            local_folders: vec![],
        };
        assert!(m.validate().is_err());
    }

    #[test]
    fn manifest_rejects_unsupported_version() {
        let m = PaperclipPluginManifestV1 {
            id: "x".into(),
            version: "1.0.0".into(),
            manifest_version: "v99".into(),
            label: "Test".into(),
            description: String::new(),
            author: None,
            entry: "x".into(),
            capabilities: vec![],
            config_schema: Value::Null,
            ui_contributions: vec![],
            metadata: Value::Null,
            local_folders: vec![],
        };
        let err = m.validate().unwrap_err();
        assert!(err.contains("unsupported manifest_version"));
    }

    #[test]
    fn has_capability_works() {
        let m = PaperclipPluginManifestV1 {
            id: "x".into(),
            version: "1.0.0".into(),
            manifest_version: "v1".into(),
            label: "Test".into(),
            description: String::new(),
            author: None,
            entry: "x".into(),
            capabilities: vec![PluginManifestCapability {
                kind: PluginManifestCapabilityKind::Jobs,
                requires: vec![],
            }],
            config_schema: Value::Null,
            ui_contributions: vec![],
            metadata: Value::Null,
            local_folders: vec![],
        };
        assert!(m.has_capability(&PluginManifestCapabilityKind::Jobs));
        assert!(!m.has_capability(&PluginManifestCapabilityKind::Tools));
    }
}
