//! Plugin capability catalogue + static operation / feature / UI-slot mappings.
//!
//! 高内聚：所有"什么是 capability / 哪些 capability 被哪些操作需要"的事实
//! 都集中在这个文件，不分散。
//!
//! 低耦合：本模块是纯静态数据 + 公开 helper，零 validator 实现依赖。
//!
//! 设计选择：用 `&'static str` 表示合法常量（PLUGIN_CAPABILITIES 等表），
//! 但业务返回类型用 `String`（无生命周期纠缠、可哈希、可序列化）。

// ============================================================================
// PluginCapability — Copy newtype
// ============================================================================

/// 单个 plugin capability。完整列表见 [`PLUGIN_CAPABILITIES`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginCapability(pub String);

impl PluginCapability {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PluginCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&'static str> for PluginCapability {
    fn from(s: &'static str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for PluginCapability {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// ============================================================================
// PLUGIN_CAPABILITIES — 全部合法 capability（与 Node 1:1）
// ============================================================================

pub const PLUGIN_CAPABILITIES: &[&str] = &[
    // Data Read
    "companies.read", "projects.read", "project.workspaces.read",
    "execution.workspaces.read", "issues.read", "issue.relations.read",
    "issue.subtree.read", "issue.comments.read", "issue.interactions.read",
    "issue.attachments.read", "approvals.read", "issue.documents.read",
    "agents.read", "goals.read", "goals.create", "goals.update",
    "activity.read", "costs.read", "issues.orchestration.read",
    "access.members.read", "access.invites.read", "authorization.grants.read",
    "authorization.policies.read", "authorization.audit.read",
    "database.namespace.read",
    // Data Write
    "issues.create", "issues.update", "issue.relations.write", "issues.checkout",
    "issues.wakeup", "issue.comments.create", "issue.comments.create_human_attributed",
    "issue.interactions.create", "issue.interactions.respond", "approvals.respond",
    "issue.documents.write", "projects.managed", "routines.managed", "skills.managed",
    "agents.pause", "agents.resume", "agents.invoke", "agents.managed",
    "access.members.write", "access.invites.write", "authorization.grants.write",
    "authorization.policies.write", "agent.sessions.create", "agent.sessions.list",
    "agent.sessions.send", "agent.sessions.close", "activity.log.write",
    "metrics.write", "telemetry.track", "database.namespace.migrate",
    "database.namespace.write", "external.objects.detect", "external.objects.read",
    "external.objects.write", "external.objects.refresh",
    // Plugin State
    "plugin.state.read", "plugin.state.write",
    // Runtime / Integration
    "events.subscribe", "events.emit", "jobs.schedule", "webhooks.receive",
    "api.routes.register", "http.outbound", "secrets.read-ref",
    "environment.drivers.register", "local.folders",
    // Agent Tools
    "agent.tools.register",
    // UI
    "instance.settings.register", "ui.sidebar.register", "ui.page.register",
    "ui.detailTab.register", "ui.dashboardWidget.register",
    "ui.commentAnnotation.register", "ui.action.register",
];

pub fn parse_capability(s: &str) -> Option<PluginCapability> {
    if PLUGIN_CAPABILITIES.iter().any(|c| *c == s) {
        Some(PluginCapability::new(s))
    } else {
        None
    }
}

pub fn is_valid_capability(s: &str) -> bool {
    parse_capability(s).is_some()
}

// ============================================================================
// UI_SLOT_CAPABILITIES — UI slot 类型 → 所需 capability
// ============================================================================

/// Plugin UI slot 类型（与 Node `PluginUiSlotType` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginUiSlotType(pub String);

impl PluginUiSlotType {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub const PAGE: &'static str = "page";
    pub const DETAIL_TAB: &'static str = "detailTab";
    pub const TASK_DETAIL_VIEW: &'static str = "taskDetailView";
    pub const DASHBOARD_WIDGET: &'static str = "dashboardWidget";
    pub const SIDEBAR: &'static str = "sidebar";
    pub const ROUTE_SIDEBAR: &'static str = "routeSidebar";
    pub const SIDEBAR_PANEL: &'static str = "sidebarPanel";
    pub const PROJECT_SIDEBAR_ITEM: &'static str = "projectSidebarItem";
    pub const GLOBAL_TOOLBAR_BUTTON: &'static str = "globalToolbarButton";
    pub const TOOLBAR_BUTTON: &'static str = "toolbarButton";
    pub const CONTEXT_MENU_ITEM: &'static str = "contextMenuItem";
    pub const COMMENT_ANNOTATION: &'static str = "commentAnnotation";
    pub const COMMENT_CONTEXT_MENU_ITEM: &'static str = "commentContextMenuItem";
    pub const SETTINGS_PAGE: &'static str = "settingsPage";
    pub const COMPANY_SETTINGS_PAGE: &'static str = "companySettingsPage";

    pub const ALL: &'static [&'static str] = &[
        Self::PAGE, Self::DETAIL_TAB, Self::TASK_DETAIL_VIEW, Self::DASHBOARD_WIDGET,
        Self::SIDEBAR, Self::ROUTE_SIDEBAR, Self::SIDEBAR_PANEL,
        Self::PROJECT_SIDEBAR_ITEM, Self::GLOBAL_TOOLBAR_BUTTON, Self::TOOLBAR_BUTTON,
        Self::CONTEXT_MENU_ITEM, Self::COMMENT_ANNOTATION,
        Self::COMMENT_CONTEXT_MENU_ITEM, Self::SETTINGS_PAGE, Self::COMPANY_SETTINGS_PAGE,
    ];
}

impl std::fmt::Display for PluginUiSlotType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// UI slot 类型 → 所需 capability（与 Node `UI_SLOT_CAPABILITIES` 1:1 对齐）。
pub fn ui_slot_capability(slot: &str) -> Option<PluginCapability> {
    let cap = match slot {
        "sidebar" | "sidebarPanel" | "projectSidebarItem" | "routeSidebar" => {
            "ui.sidebar.register"
        }
        "page" => "ui.page.register",
        "detailTab" | "taskDetailView" => "ui.detailTab.register",
        "dashboardWidget" => "ui.dashboardWidget.register",
        "globalToolbarButton" | "toolbarButton" | "contextMenuItem"
        | "commentContextMenuItem" => "ui.action.register",
        "commentAnnotation" => "ui.commentAnnotation.register",
        "settingsPage" | "companySettingsPage" => "instance.settings.register",
        _ => return None,
    };
    Some(PluginCapability::new(cap))
}

pub fn parse_ui_slot(s: &str) -> Option<PluginUiSlotType> {
    if PluginUiSlotType::ALL.iter().any(|t| *t == s) {
        Some(PluginUiSlotType::new(s))
    } else {
        None
    }
}

// ============================================================================
// LAUNCHER_PLACEMENT_CAPABILITIES — launcher placement zone → 所需 capability
// ============================================================================

/// Plugin launcher placement zone（与 Node `PluginLauncherPlacementZone` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginLauncherPlacementZone(pub String);

impl PluginLauncherPlacementZone {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub const ALL: &'static [&'static str] = &[
        "page", "detailTab", "taskDetailView", "dashboardWidget", "sidebar",
        "sidebarPanel", "projectSidebarItem", "globalToolbarButton", "toolbarButton",
        "contextMenuItem", "commentAnnotation", "commentContextMenuItem", "settingsPage",
    ];
}

impl std::fmt::Display for PluginLauncherPlacementZone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn launcher_placement_capability(zone: &str) -> Option<PluginCapability> {
    let cap = match zone {
        "page" => "ui.page.register",
        "detailTab" | "taskDetailView" => "ui.detailTab.register",
        "dashboardWidget" => "ui.dashboardWidget.register",
        "sidebar" | "sidebarPanel" | "projectSidebarItem" => "ui.sidebar.register",
        "globalToolbarButton" | "toolbarButton" | "contextMenuItem" => "ui.action.register",
        "commentAnnotation" => "ui.commentAnnotation.register",
        "commentContextMenuItem" => "ui.action.register",
        "settingsPage" => "instance.settings.register",
        _ => return None,
    };
    Some(PluginCapability::new(cap))
}

// ============================================================================
// FEATURE_CAPABILITIES — manifest feature 字段 → 所需 capability
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ManifestFeature {
    Tools,
    Jobs,
    Webhooks,
    Database,
    EnvironmentDrivers,
    Agents,
    Projects,
    Routines,
    ObjectReferences,
}

impl ManifestFeature {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Jobs => "jobs",
            Self::Webhooks => "webhooks",
            Self::Database => "database",
            Self::EnvironmentDrivers => "environmentDrivers",
            Self::Agents => "agents",
            Self::Projects => "projects",
            Self::Routines => "routines",
            Self::ObjectReferences => "objectReferences",
        }
    }
}

pub fn feature_capability(feature: ManifestFeature) -> PluginCapability {
    let cap = match feature {
        ManifestFeature::Tools => "agent.tools.register",
        ManifestFeature::Jobs => "jobs.schedule",
        ManifestFeature::Webhooks => "webhooks.receive",
        ManifestFeature::Database => "database.namespace.migrate",
        ManifestFeature::EnvironmentDrivers => "environment.drivers.register",
        ManifestFeature::Agents => "agents.managed",
        ManifestFeature::Projects => "projects.managed",
        ManifestFeature::Routines => "routines.managed",
        ManifestFeature::ObjectReferences => "external.objects.detect",
    };
    PluginCapability::new(cap)
}

// ============================================================================
// OPERATION_CAPABILITIES — runtime operation → 所需 capability
// ============================================================================

/// 单个 runtime operation → 所需 capability 列表（与 Node `OPERATION_CAPABILITIES` 1:1 对齐）。
///
/// 未知 operation 返回 `&[]` —— `check_operation` 把空列表视为 "deny by default"。
///
/// 实现：用 OnceLock + HashMap 懒加载；首次调用构建，之后 O(1) 查找。
/// 表项数 ~85，懒加载开销可忽略；查找热路径零分配。
pub fn operation_capabilities(operation: &str) -> Vec<PluginCapability> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<std::collections::HashMap<&'static str, Vec<PluginCapability>>> =
        OnceLock::new();
    let map = CACHE.get_or_init(|| {
        let entries: &[(&'static str, &[&'static str])] = build_table();
        entries
            .iter()
            .map(|(op, caps)| {
                (
                    *op,
                    caps.iter()
                        .map(|c| PluginCapability::new(*c))
                        .collect(),
                )
            })
            .collect()
    });
    map.get(operation).cloned().unwrap_or_default()
}

fn build_table() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        // Data Read
        ("companies.list", &["companies.read"]),
        ("companies.get", &["companies.read"]),
        ("projects.list", &["projects.read"]),
        ("projects.get", &["projects.read"]),
        ("project.workspaces.list", &["project.workspaces.read"]),
        ("project.workspaces.get", &["project.workspaces.read"]),
        ("execution.workspaces.get", &["execution.workspaces.read"]),
        ("issues.list", &["issues.read"]),
        ("issues.get", &["issues.read"]),
        ("issue.comments.read", &["issue.comments.read"]),
        ("issue.comments.list", &["issue.comments.read"]),
        ("issue.relations.read", &["issue.relations.read"]),
        ("issue.relations.list", &["issue.relations.read"]),
        ("issue.subtree.read", &["issue.subtree.read"]),
        ("issue.subtree.list", &["issue.subtree.read"]),
        ("issue.interactions.read", &["issue.interactions.read"]),
        ("issue.interactions.list", &["issue.interactions.read"]),
        ("issue.attachments.read", &["issue.attachments.read"]),
        ("issue.attachments.list", &["issue.attachments.read"]),
        ("issue.attachments.download", &["issue.attachments.read"]),
        ("issue.documents.read", &["issue.documents.read"]),
        ("issue.documents.list", &["issue.documents.read"]),
        ("approvals.read", &["approvals.read"]),
        ("approvals.list", &["approvals.read"]),
        ("agents.read", &["agents.read"]),
        ("agents.list", &["agents.read"]),
        ("goals.read", &["goals.read"]),
        ("goals.list", &["goals.read"]),
        ("activity.read", &["activity.read"]),
        ("activity.list", &["activity.read"]),
        ("costs.read", &["costs.read"]),
        ("issues.orchestration.read", &["issues.orchestration.read"]),
        ("access.members.read", &["access.members.read"]),
        ("access.members.list", &["access.members.read"]),
        ("access.invites.read", &["access.invites.read"]),
        ("access.invites.list", &["access.invites.read"]),
        ("authorization.grants.read", &["authorization.grants.read"]),
        ("authorization.policies.read", &["authorization.policies.read"]),
        ("authorization.audit.read", &["authorization.audit.read"]),
        ("database.namespace.read", &["database.namespace.read"]),
        // Data Write
        ("issues.create", &["issues.create"]),
        ("issues.update", &["issues.update"]),
        ("issues.checkout", &["issues.checkout"]),
        ("issues.wakeup", &["issues.wakeup"]),
        ("issue.comments.create", &["issue.comments.create"]),
        ("issue.comments.create_human_attributed", &["issue.comments.create_human_attributed"]),
        ("issue.interactions.create", &["issue.interactions.create"]),
        ("issue.interactions.respond", &["issue.interactions.respond"]),
        ("approvals.respond", &["approvals.respond"]),
        ("issue.documents.write", &["issue.documents.write"]),
        ("goals.create", &["goals.create"]),
        ("goals.update", &["goals.update"]),
        // Managed
        ("projects.managed.get", &["projects.managed"]),
        ("projects.managed.reconcile", &["projects.managed"]),
        ("projects.managed.reset", &["projects.managed"]),
        ("routines.managed.get", &["routines.managed"]),
        ("routines.managed.reconcile", &["routines.managed"]),
        ("routines.managed.reset", &["routines.managed"]),
        ("agents.managed", &["agents.managed"]),
        ("agents.managed.list", &["agents.managed"]),
        ("agents.managed.create", &["agents.managed"]),
        ("agents.managed.update", &["agents.managed"]),
        ("access.members.write", &["access.members.write"]),
        ("access.invites.write", &["access.invites.write"]),
        ("authorization.grants.write", &["authorization.grants.write"]),
        ("authorization.policies.write", &["authorization.policies.write"]),
        // Agent control
        ("agents.invoke", &["agents.invoke"]),
        ("agents.pause", &["agents.pause", "agents.resume"]),
        ("agents.resume", &["agents.resume"]),
        // Agent sessions
        ("agent.sessions.create", &["agent.sessions.create"]),
        ("agent.sessions.list", &["agent.sessions.list"]),
        ("agent.sessions.send", &["agent.sessions.send"]),
        ("agent.sessions.close", &["agent.sessions.close"]),
        // Database
        ("database.namespace.migrate", &["database.namespace.migrate"]),
        ("database.namespace.write", &["database.namespace.write"]),
        // External objects
        ("external.objects.detect", &["external.objects.detect"]),
        ("external.objects.read", &["external.objects.read"]),
        ("external.objects.write", &["external.objects.write"]),
        ("external.objects.refresh", &["external.objects.refresh"]),
        // Plugin state
        ("plugin.state.read", &["plugin.state.read"]),
        ("plugin.state.write", &["plugin.state.write"]),
        // Runtime / Integration
        ("events.subscribe", &["events.subscribe"]),
        ("events.emit", &["events.emit"]),
        ("jobs.schedule", &["jobs.schedule"]),
        ("jobs.run", &["jobs.schedule"]),
        ("webhooks.receive", &["webhooks.receive"]),
        ("api.routes.register", &["api.routes.register"]),
        ("http.outbound", &["http.outbound"]),
        ("secrets.read-ref", &["secrets.read-ref"]),
        ("environment.drivers.register", &["environment.drivers.register"]),
        ("environment.drivers.list", &["environment.drivers.register"]),
        ("local.folders.read", &["local.folders"]),
        ("local.folders.list", &["local.folders"]),
        ("local.folders.write", &["local.folders"]),
        // Agent tools
        ("agent.tools.register", &["agent.tools.register"]),
        // Telemetry / metrics / activity
        ("activity.log.write", &["activity.log.write"]),
        ("metrics.write", &["metrics.write"]),
        ("telemetry.track", &["telemetry.track"]),
        ("skills.managed", &["skills.managed"]),
        // UI
        ("ui.sidebar.register", &["ui.sidebar.register"]),
        ("ui.page.register", &["ui.page.register"]),
        ("ui.detailTab.register", &["ui.detailTab.register"]),
        ("ui.dashboardWidget.register", &["ui.dashboardWidget.register"]),
        ("ui.commentAnnotation.register", &["ui.commentAnnotation.register"]),
        ("ui.action.register", &["ui.action.register"]),
        ("instance.settings.register", &["instance.settings.register"]),
    ]
}
