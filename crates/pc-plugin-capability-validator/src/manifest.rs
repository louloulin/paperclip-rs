//! Plugin manifest 的 validator 视角子集。
//!
//! 高内聚：本模块只描述 validator 需要读取的 manifest 字段，**不**复制整个
//! `@paperclipai/shared` 的 `PaperclipPluginManifestV1`（那是 plugin-protocol 的职责）。
//!
//! 低耦合：[`PluginManifestV1View`] 是 trait —— 调用方可以传入任意 manifest 实现
//! （共享 crate 的 struct、JSON view、或测试 stub），无需把整个 manifest 类型
//! 拉进本 crate 的依赖图。
//!
//! 字段命名与 Node `PaperclipPluginManifestV1` 1:1 对齐（camelCase JSON）。
//!
//! 注：UI slot / launcher zone 在 view trait 中用 `String` 暴露，validator
//! 内部用 `parse_ui_slot` / `launcher_placement_capability` 解析。

// ============================================================================
// View trait
// ============================================================================

/// Validator 看到的 manifest 视图。
///
/// 设计要点：trait 抽象让 validator 不耦合具体的 manifest 类型。调用方传
/// 入任何实现本 trait 的 view，validator 就只看到这些方法，不关心其它字段。
pub trait PluginManifestV1View {
    /// Plugin id（用于日志 / forbidden 错误信息）。
    fn id(&self) -> &str;

    /// Manifest 中声明的 capability 字符串列表（**未校验** 是否合法）。
    fn capabilities(&self) -> &[String];

    /// 声明的 tools（feature 列表）。
    fn tools(&self) -> &[serde_json::Value] {
        &[]
    }
    /// 声明的 scheduled jobs。
    fn jobs(&self) -> &[serde_json::Value] {
        &[]
    }
    /// 声明的 webhooks。
    fn webhooks(&self) -> &[serde_json::Value] {
        &[]
    }
    /// 是否声明了 database namespace（布尔 / 配置对象皆可）。
    fn has_database(&self) -> bool {
        false
    }
    /// 声明的 environment drivers。
    fn environment_drivers(&self) -> &[serde_json::Value] {
        &[]
    }
    /// 声明的 agents。
    fn agents(&self) -> &[serde_json::Value] {
        &[]
    }
    /// 声明的 projects。
    fn projects(&self) -> &[serde_json::Value] {
        &[]
    }
    /// 声明的 routines。
    fn routines(&self) -> &[serde_json::Value] {
        &[]
    }
    /// 声明的 object references。
    fn object_references(&self) -> &[serde_json::Value] {
        &[]
    }
    /// UI slots（来自 `manifest.ui.slots`），每项是 `slot.type` 字符串。
    fn ui_slots(&self) -> &[String] {
        &[]
    }
    /// Top-level launchers（来自 `manifest.launchers`），每项是 `launcher.placementZone` 字符串。
    fn launchers(&self) -> &[String] {
        &[]
    }
    /// UI launchers（来自 `manifest.ui.launchers`）。
    fn ui_launchers(&self) -> &[String] {
        &[]
    }
}

// ============================================================================
// JSON manifest view — 默认实现
// ============================================================================

/// 默认的 JSON 视图：直接从 `serde_json::Value` 读取所有字段。
///
/// 适用于没有专门的 manifest struct 但已有 JSON 的场景（如从 DB 取出的 `manifestJson` 列）。
#[derive(Debug, Clone, Default)]
pub struct JsonManifestView {
    pub id: String,
    pub capabilities: Vec<String>,
    pub tools: Vec<serde_json::Value>,
    pub jobs: Vec<serde_json::Value>,
    pub webhooks: Vec<serde_json::Value>,
    pub database: Option<serde_json::Value>,
    pub environment_drivers: Vec<serde_json::Value>,
    pub agents: Vec<serde_json::Value>,
    pub projects: Vec<serde_json::Value>,
    pub routines: Vec<serde_json::Value>,
    pub object_references: Vec<serde_json::Value>,
    pub ui_slots: Vec<String>,
    pub launchers: Vec<String>,
    pub ui_launchers: Vec<String>,
}

impl JsonManifestView {
    /// 从 `serde_json::Value` 构造 view。缺失字段视为空。
    pub fn from_value(value: &serde_json::Value) -> Self {
        let ui = value.get("ui");
        Self {
            id: string_at(value, "id"),
            capabilities: string_array_at(value, "capabilities"),
            tools: json_array_at(value, "tools"),
            jobs: json_array_at(value, "jobs"),
            webhooks: json_array_at(value, "webhooks"),
            database: value.get("database").cloned(),
            environment_drivers: json_array_at(value, "environmentDrivers"),
            agents: json_array_at(value, "agents"),
            projects: json_array_at(value, "projects"),
            routines: json_array_at(value, "routines"),
            object_references: json_array_at(value, "objectReferences"),
            ui_slots: ui
                .and_then(|u| u.get("slots"))
                .map(extract_slot_types)
                .unwrap_or_default(),
            launchers: extract_launcher_zones(value.get("launchers")),
            ui_launchers: extract_launcher_zones(ui.and_then(|u| u.get("launchers"))),
        }
    }
}

fn string_at(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn string_array_at(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .map(string_array_from_value)
        .unwrap_or_default()
}

fn string_array_from_value(v: &serde_json::Value) -> Vec<String> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn json_array_at(value: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn extract_slot_types(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("type").and_then(|t| t.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn extract_launcher_zones(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| {
                    l.get("placementZone")
                        .and_then(|z| z.as_str())
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default()
}

impl PluginManifestV1View for JsonManifestView {
    fn id(&self) -> &str {
        &self.id
    }
    fn capabilities(&self) -> &[String] {
        &self.capabilities
    }
    fn tools(&self) -> &[serde_json::Value] {
        &self.tools
    }
    fn jobs(&self) -> &[serde_json::Value] {
        &self.jobs
    }
    fn webhooks(&self) -> &[serde_json::Value] {
        &self.webhooks
    }
    fn has_database(&self) -> bool {
        self.database.is_some()
    }
    fn environment_drivers(&self) -> &[serde_json::Value] {
        &self.environment_drivers
    }
    fn agents(&self) -> &[serde_json::Value] {
        &self.agents
    }
    fn projects(&self) -> &[serde_json::Value] {
        &self.projects
    }
    fn routines(&self) -> &[serde_json::Value] {
        &self.routines
    }
    fn object_references(&self) -> &[serde_json::Value] {
        &self.object_references
    }
    fn ui_slots(&self) -> &[String] {
        &self.ui_slots
    }
    fn launchers(&self) -> &[String] {
        &self.launchers
    }
    fn ui_launchers(&self) -> &[String] {
        &self.ui_launchers
    }
}
