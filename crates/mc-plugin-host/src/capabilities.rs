//! 宿主能力裁决：manifest 的声明 × 宿主开放的能力 ⇒ 能不能跑。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`pkg/plugincontract/capabilities.go` 的 `Capabilities`(16) 与
//!   `ErrCapabilityUnavailable`(61)。
//! - **语义**：能力是**宿主**的开关（不是插件声明的结果）。插件只声明它想要什么；
//!   宿主在这里裁决「本部署是否提供」。裁决失败不能崩、不能静默跳过 —— 返回
//!   [`CapabilityUnavailable`]，由 route 层映射成明确的降级码（`plugin_disabled` /
//!   `plugin_surfaces_not_configured`）。
//! - **与 scope 的分工**：`capabilities` 判「这类能力本部署有没有」，[`crate::scope`] 判
//!   「这个安装被授予了没有」。两者都在 M6-1（唯一实现点）；route 层不要自己写判定。
//! - **本仓约定**：能力集合用封闭枚举 + `ALL`（照 `mc-core` 的既有风格），**不要**用裸字符串
//!   比较散落在各 route 文件里；枚举的 `as_str()` 就是 manifest 里的 wire 字面量。
//! - **不做什么**：不做 feature flag（部署级开关走 `mc-feature-flags`，是另一套东西，别混）。
//!
//! **状态：M6-1 已落地。**
//!
//! ## 本文件同时是 scope 词表的第二入口
//!
//! `mc-core::plugin::PluginScope` 的注释把取值集合的权威点写成「`mc-plugin-host::capabilities`」，
//! 而实现点在 [`crate::scope`]（该文件的头注解释了为什么单独成文件）。两个路径都必须能取到
//! 同一个常量，所以这里 `pub use` 转发 —— **定义仍然只有一处**。

use std::collections::BTreeSet;
use std::fmt;

use crate::manifest::Manifest;

pub use crate::scope::{
    FIXED_SCOPES, SCOPE_AGENTS_READ, SCOPE_COMMENTS_READ, SCOPE_COMMENTS_WRITE, SCOPE_ISSUES_READ,
    SCOPE_ISSUES_WRITE, SCOPE_MEMBERS_READ, SCOPE_NET_PREFIX, SCOPE_STORAGE_USER,
    SCOPE_STORAGE_WORKSPACE, SCOPE_TASKS_READ, SCOPE_TASKS_WRITE,
};

/// 面（iframe）类型。上游 `SurfaceIssuePanel` / `SurfaceSidebarPanel` / `SurfaceModal`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SurfaceType {
    /// `issue_panel`：挂在 issue 详情页的固定位置。
    IssuePanel,
    /// `sidebar_panel`：**宿主没有位置可渲染** ⇒ 默认关闭（见 [`HOST_CAPABILITIES`]）。
    SidebarPanel,
    /// `modal`：由 manual hook 打开。
    Modal,
}

impl SurfaceType {
    /// 全部取值（顺序即上游声明的顺序）。
    pub const ALL: &'static [Self] = &[Self::IssuePanel, Self::SidebarPanel, Self::Modal];

    /// wire 字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IssuePanel => "issue_panel",
            Self::SidebarPanel => "sidebar_panel",
            Self::Modal => "modal",
        }
    }

    /// 解析 wire 字面量（未知值 ⇒ `None`，由 `manifest` 报「unsupported」）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
    }
}

impl fmt::Display for SurfaceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// hook 的触发者。上游 `TriggerUI` / `TriggerManual` / `TriggerAgent` / `TriggerEvent` /
/// `TriggerSchedule`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookTrigger {
    /// `ui`：宿主界面里的按钮。
    Ui,
    /// `manual`：用户手动执行。
    Manual,
    /// `agent`：以 MCP 工具形态提供给 agent，由 agent 决定。
    Agent,
    /// `event`：产品事件推来的（异步）。
    Event,
    /// `schedule`：宿主按 cron 计划调（异步）。
    Schedule,
}

impl HookTrigger {
    /// 全部取值。
    pub const ALL: &'static [Self] = &[
        Self::Ui,
        Self::Manual,
        Self::Agent,
        Self::Event,
        Self::Schedule,
    ];

    /// wire 字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Manual => "manual",
            Self::Agent => "agent",
            Self::Event => "event",
            Self::Schedule => "schedule",
        }
    }

    /// 解析 wire 字面量。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
    }
}

impl fmt::Display for HookTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// hook 的传输。上游 `TransportHTTP` / `TransportMCP`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HookTransport {
    /// `http`：调一个 manifest 声明的端点。
    Http,
    /// `mcp`：采纳一个外部 MCP server 的工具（有采纳步骤，见 `plugin_mcp_transport.go`）。
    Mcp,
}

impl HookTransport {
    /// 全部取值。
    pub const ALL: &'static [Self] = &[Self::Http, Self::Mcp];

    /// wire 字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Mcp => "mcp",
        }
    }

    /// 解析 wire 字面量。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
    }
}

impl fmt::Display for HookTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 静态贡献的类型。上游只有 `ResourceSkill`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ResourceType {
    /// `skill`：`SKILL.md` 写进既有 skill 表（安装时物化、卸载时删除）。
    Skill,
}

impl ResourceType {
    /// 全部取值。
    pub const ALL: &'static [Self] = &[Self::Skill];

    /// wire 字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
        }
    }

    /// 解析 wire 字面量。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|kind| kind.as_str() == raw)
    }
}

impl fmt::Display for ResourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 本宿主 build **实际**能跑的能力（上游 `Capabilities`）。
///
/// 用 `&'static [T]` 而不是 `HashSet`：集合是编译期常量、判定只有十几个元素、且能进 `const`。
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    /// 能渲染的面类型。
    pub surface_types: &'static [SurfaceType],
    /// 能触发的 hook 触发者。
    pub hook_triggers: &'static [HookTrigger],
    /// 能走的 hook 传输。
    pub hook_transports: &'static [HookTransport],
    /// 能物化的资源类型。
    pub resource_types: &'static [ResourceType],
}

impl Capabilities {
    /// 本部署是否提供这个面类型。
    #[must_use]
    pub fn supports_surface(&self, kind: SurfaceType) -> bool {
        self.surface_types.contains(&kind)
    }

    /// 本部署是否提供这个 hook 触发者。
    #[must_use]
    pub fn supports_trigger(&self, kind: HookTrigger) -> bool {
        self.hook_triggers.contains(&kind)
    }

    /// 本部署是否提供这个 hook 传输。
    #[must_use]
    pub fn supports_transport(&self, kind: HookTransport) -> bool {
        self.hook_transports.contains(&kind)
    }

    /// 本部署是否提供这个资源类型。
    #[must_use]
    pub fn supports_resource(&self, kind: ResourceType) -> bool {
        self.resource_types.contains(&kind)
    }
}

/// 已发货的能力集合（上游 `HostCapabilities`）。
///
/// - `sidebar_panel` **故意关闭**：宿主没有它的渲染位置；把不能渲染的面打开等于让插件
///   「装上了但永远不出现」，而这正是这道闸门要防的。
/// - `agent` 触发打开：它不是宿主驱动的调用点 —— hook 以 MCP 工具形态交给 agent 自行决定。
/// - `mcp` 传输打开：它采纳外部 server 的工具，因此带一个按 schema 摘要固定工具的采纳步骤
///   （**装插件不是那次授权，采纳工具才是**）。
pub const HOST_CAPABILITIES: Capabilities = Capabilities {
    surface_types: &[SurfaceType::IssuePanel, SurfaceType::Modal],
    hook_triggers: &[
        HookTrigger::Ui,
        HookTrigger::Manual,
        HookTrigger::Event,
        HookTrigger::Agent,
        HookTrigger::Schedule,
    ],
    hook_transports: &[HookTransport::Http, HookTransport::Mcp],
    resource_types: &[ResourceType::Skill],
};

/// 宿主跑不了的声明项（上游 `ErrCapabilityUnavailable`）。
///
/// `missing` 是**去重且排序**后的 `"<类目> <值>"` 列表，例如 `"hook trigger agent"` ——
/// 一次报全，管理员不必逐个安装去发现缺口。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("this Multica version does not support: {}", .missing.join(", "))]
pub struct CapabilityUnavailable {
    /// 缺失项（已去重排序）。
    pub missing: Vec<String>,
}

impl CapabilityUnavailable {
    /// 稳定错误码（route 层映射 409/422 时用）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        "plugin_capability_unavailable"
    }
}

/// 裁决：manifest 声明的每一项贡献，本部署能不能跑（上游 `Manifest.CheckCapabilities`）。
///
/// 只看**取值**是否能被宿主支持；manifest 自身的形态问题（未声明的触发者等）在
/// [`crate::manifest::parse_manifest`] 就已经拒了。
pub fn check_capabilities(
    manifest: &Manifest,
    host: &Capabilities,
) -> Result<(), CapabilityUnavailable> {
    let mut missing = BTreeSet::new();
    for surface in &manifest.contributes.surfaces {
        let Some(kind) = SurfaceType::parse(&surface.kind) else {
            continue;
        };
        if !host.supports_surface(kind) {
            missing.insert(format!("surface {}", kind.as_str()));
        }
    }
    for hook in &manifest.contributes.hooks {
        for trigger in &hook.triggers {
            let Some(kind) = HookTrigger::parse(trigger) else {
                continue;
            };
            if !host.supports_trigger(kind) {
                missing.insert(format!("hook trigger {}", kind.as_str()));
            }
        }
        let Some(kind) = HookTransport::parse(&hook.transport.kind) else {
            continue;
        };
        if !host.supports_transport(kind) {
            missing.insert(format!("hook transport {}", kind.as_str()));
        }
    }
    for resource in &manifest.contributes.resources {
        let Some(kind) = ResourceType::parse(&resource.kind) else {
            continue;
        };
        if !host.supports_resource(kind) {
            missing.insert(format!("resource {}", kind.as_str()));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    Err(CapabilityUnavailable {
        missing: missing.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_from(raw: &str) -> Manifest {
        crate::manifest::parse_manifest(raw.as_bytes()).expect("manifest should parse")
    }

    #[test]
    fn host_switches_are_the_upstream_ones() {
        assert!(HOST_CAPABILITIES.supports_surface(SurfaceType::IssuePanel));
        assert!(HOST_CAPABILITIES.supports_surface(SurfaceType::Modal));
        assert!(!HOST_CAPABILITIES.supports_surface(SurfaceType::SidebarPanel));
        for trigger in HookTrigger::ALL {
            assert!(HOST_CAPABILITIES.supports_trigger(*trigger), "{trigger}");
        }
        for transport in HookTransport::ALL {
            assert!(
                HOST_CAPABILITIES.supports_transport(*transport),
                "{transport}"
            );
        }
        assert!(HOST_CAPABILITIES.supports_resource(ResourceType::Skill));
        // 词表与 capabilities.go 逐条对齐
        assert_eq!(SurfaceType::ALL.len(), 3);
        assert_eq!(HookTrigger::ALL.len(), 5);
        assert_eq!(HookTransport::ALL.len(), 2);
        assert_eq!(ResourceType::ALL.len(), 1);
        assert!(SurfaceType::parse("nope").is_none());
    }

    #[test]
    fn declared_and_supported_passes() {
        let manifest = manifest_from(
            r#"{
              "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
              "version": "1.0.0", "author": {"name": "Example"},
              "scopes": ["issues:read", "net:example.com"],
              "contributes": {
                "surfaces": [{"key": "panel", "type": "issue_panel", "name": "Panel", "entry": "panel.js"}],
                "hooks": [{"key": "on_issue", "name": "On issue", "description": "d",
                  "triggers": ["event"], "events": ["issue.created"],
                  "transport": {"type": "http", "url": "https://example.com/hook"}}]
              }
            }"#,
        );
        assert!(check_capabilities(&manifest, &HOST_CAPABILITIES).is_ok());
    }

    #[test]
    fn unsupported_contributions_are_reported_all_at_once_and_sorted() {
        let manifest = manifest_from(
            r#"{
              "manifest_version": 1, "key": "com.example.demo", "name": "Demo",
              "version": "1.0.0", "author": {"name": "Example"},
              "scopes": ["issues:read", "tasks:read", "net:example.com"],
              "contributes": {
                "surfaces": [{"key": "side", "type": "sidebar_panel", "name": "Side", "entry": "side.js"}],
                "hooks": [{"key": "on_task", "name": "On task", "description": "d",
                  "triggers": ["event", "ui"], "events": ["task.started"],
                  "transport": {"type": "http", "url": "https://example.com/hook"}}]
              }
            }"#,
        );
        let host = Capabilities {
            surface_types: &[SurfaceType::IssuePanel],
            hook_triggers: &[HookTrigger::Manual],
            hook_transports: &[HookTransport::Http],
            resource_types: &[],
        };
        let err = check_capabilities(&manifest, &host).expect_err("sidebar/ui/event unsupported");
        assert_eq!(
            err.missing,
            vec![
                "hook trigger event".to_owned(),
                "hook trigger ui".to_owned(),
                "surface sidebar_panel".to_owned()
            ]
        );
        assert_eq!(err.code(), "plugin_capability_unavailable");
        assert_eq!(
            err.to_string(),
            "this Multica version does not support: hook trigger event, hook trigger ui, surface sidebar_panel"
        );
    }
}
