//! Instance feature catalog（与 Node
//! `packages/shared/src/feature-catalog.ts` 1:1 对齐）。
//!
//! ## 职责
//! 提供 managed-config 文档可以引用的 instance feature key 集合、tier 元数据
//! 与默认行为。所有字段都是 compile-time constant（除 `INSTANCE_FEATURE_CATALOG`
//! 使用 `LazyLock` 之外）；不含 IO、不含可变状态。
//!
//! ## 设计原则
//! - `FeatureTier` 与 `InstanceFeatureKey` 都是 `Copy + Eq + Hash + PartialOrd`
//!   便于用作 map key 与 serde 序列化。
//! - `INSTANCE_FEATURE_CATALOG` 使用 `LazyLock<HashMap>` 而非 `const HashMap`
//!   是因为 stable Rust 不支持 const-context 内的 `String::to_string()`。
//! - tier 不是 `"managed"` 的 feature key 在 managed-config 解析时会被
//!   fail-closed 拒绝（见 `managed_config::parser`）。

use std::collections::HashMap;
use std::sync::LazyLock;

// ============================================================================
// FeatureTier
// ============================================================================

/// Feature tier（与 Node `FEATURE_TIERS` 1:1 对齐）。
///
/// - `preference`：tenant 自选；cloud 不管控。
/// - `managed`：cloud 可通过 `PAPERCLIP_MANAGED_CONFIG` 管控。
/// - `floor`：被代码 pinned，managed instance 上不能放宽。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FeatureTier {
    Preference,
    Managed,
    Floor,
}

impl FeatureTier {
    /// 字符串字面量（与 Node `FEATURE_TIERS` 元素 1:1）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Managed => "managed",
            Self::Floor => "floor",
        }
    }

    /// 解析字符串字面量（与 Node 端 zod `z.enum(FEATURE_TIERS)` 等价）。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "preference" => Some(Self::Preference),
            "managed" => Some(Self::Managed),
            "floor" => Some(Self::Floor),
            _ => None,
        }
    }
}

// ============================================================================
// InstanceFeatureKey
// ============================================================================

/// Boolean flag keys of `instanceExperimentalSettingsSchema`
/// （与 Node `InstanceFeatureKey` 1:1 对齐）。
///
/// 非 boolean 字段（activation timestamps、lookback hours 等）已 by-construction
/// 排除；本 enum 是 schema 的投影，schema 修改 = 本 enum 修改。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InstanceFeatureKey {
    EnableEnvironments,
    EnableIsolatedWorkspaces,
    EnableStreamlinedLeftNavigation,
    EnableApps,
    EnablePipelines,
    EnableCases,
    EnableConferenceRoomChat,
    EnableTaskChatRedesign,
    EnableTaskWatchdogs,
    EnableIssuePlanDecompositions,
    EnableExperimentalFileViewer,
    EnableExternalObjects,
    EnableSmokeLab,
    EnableBuiltInAgents,
    EnableBetaSkills,
    EnableSummaries,
    EnableStatusCards,
    EnableDecisions,
    EnableGoalsSidebarLink,
    EnableServerInfoDebugView,
    AutoRestartDevServerWhenIdle,
    EnableIssueGraphLivenessAutoRecovery,
    EnableWorkspaceBranchReconcileForward,
    EnableWorkspaceDirtyQuarantineRepair,
    EnableOwnerInstanceAdmin,
    EnableWorktreeRunExecution,
}

impl InstanceFeatureKey {
    /// string literal name（与 Node `Object.keys(INSTANCE_FEATURE_CATALOG)` 1:1）。
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::EnableEnvironments => "enableEnvironments",
            Self::EnableIsolatedWorkspaces => "enableIsolatedWorkspaces",
            Self::EnableStreamlinedLeftNavigation => "enableStreamlinedLeftNavigation",
            Self::EnableApps => "enableApps",
            Self::EnablePipelines => "enablePipelines",
            Self::EnableCases => "enableCases",
            Self::EnableConferenceRoomChat => "enableConferenceRoomChat",
            Self::EnableTaskChatRedesign => "enableTaskChatRedesign",
            Self::EnableTaskWatchdogs => "enableTaskWatchdogs",
            Self::EnableIssuePlanDecompositions => "enableIssuePlanDecompositions",
            Self::EnableExperimentalFileViewer => "enableExperimentalFileViewer",
            Self::EnableExternalObjects => "enableExternalObjects",
            Self::EnableSmokeLab => "enableSmokeLab",
            Self::EnableBuiltInAgents => "enableBuiltInAgents",
            Self::EnableBetaSkills => "enableBetaSkills",
            Self::EnableSummaries => "enableSummaries",
            Self::EnableStatusCards => "enableStatusCards",
            Self::EnableDecisions => "enableDecisions",
            Self::EnableGoalsSidebarLink => "enableGoalsSidebarLink",
            Self::EnableServerInfoDebugView => "enableServerInfoDebugView",
            Self::AutoRestartDevServerWhenIdle => "autoRestartDevServerWhenIdle",
            Self::EnableIssueGraphLivenessAutoRecovery => "enableIssueGraphLivenessAutoRecovery",
            Self::EnableWorkspaceBranchReconcileForward => "enableWorkspaceBranchReconcileForward",
            Self::EnableWorkspaceDirtyQuarantineRepair => "enableWorkspaceDirtyQuarantineRepair",
            Self::EnableOwnerInstanceAdmin => "enableOwnerInstanceAdmin",
            Self::EnableWorktreeRunExecution => "enableWorktreeRunExecution",
        }
    }

    /// 解析字符串字面量（与 Node 端 `Record<InstanceFeatureKey, ...>` 等价）。
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "enableEnvironments" => Self::EnableEnvironments,
            "enableIsolatedWorkspaces" => Self::EnableIsolatedWorkspaces,
            "enableStreamlinedLeftNavigation" => Self::EnableStreamlinedLeftNavigation,
            "enableApps" => Self::EnableApps,
            "enablePipelines" => Self::EnablePipelines,
            "enableCases" => Self::EnableCases,
            "enableConferenceRoomChat" => Self::EnableConferenceRoomChat,
            "enableTaskChatRedesign" => Self::EnableTaskChatRedesign,
            "enableTaskWatchdogs" => Self::EnableTaskWatchdogs,
            "enableIssuePlanDecompositions" => Self::EnableIssuePlanDecompositions,
            "enableExperimentalFileViewer" => Self::EnableExperimentalFileViewer,
            "enableExternalObjects" => Self::EnableExternalObjects,
            "enableSmokeLab" => Self::EnableSmokeLab,
            "enableBuiltInAgents" => Self::EnableBuiltInAgents,
            "enableBetaSkills" => Self::EnableBetaSkills,
            "enableSummaries" => Self::EnableSummaries,
            "enableStatusCards" => Self::EnableStatusCards,
            "enableDecisions" => Self::EnableDecisions,
            "enableGoalsSidebarLink" => Self::EnableGoalsSidebarLink,
            "enableServerInfoDebugView" => Self::EnableServerInfoDebugView,
            "autoRestartDevServerWhenIdle" => Self::AutoRestartDevServerWhenIdle,
            "enableIssueGraphLivenessAutoRecovery" => Self::EnableIssueGraphLivenessAutoRecovery,
            "enableWorkspaceBranchReconcileForward" => Self::EnableWorkspaceBranchReconcileForward,
            "enableWorkspaceDirtyQuarantineRepair" => Self::EnableWorkspaceDirtyQuarantineRepair,
            "enableOwnerInstanceAdmin" => Self::EnableOwnerInstanceAdmin,
            "enableWorktreeRunExecution" => Self::EnableWorktreeRunExecution,
            _ => return None,
        })
    }
}

// ============================================================================
// FeatureCatalogEntry
// ============================================================================

/// 单个 feature 的 catalog 元数据（与 Node `FeatureCatalogEntry` 1:1 对齐）。
#[derive(Debug, Clone, Copy)]
pub struct FeatureCatalogEntry {
    pub title: &'static str,
    pub description: &'static str,
    pub tier: FeatureTier,
    pub cloud_default: bool,
    pub self_hosted_default: bool,
}

// ============================================================================
// INSTANCE_FEATURE_CATALOG
// ============================================================================

/// Instance feature 正表（与 Node `INSTANCE_FEATURE_CATALOG` 1:1 对齐）。
///
/// 使用 `LazyLock<HashMap>` 是因为 stable Rust 不支持 const HashMap 构造
/// 与 `String::to_string()` 在 const 上下文。
pub static INSTANCE_FEATURE_CATALOG: LazyLock<HashMap<InstanceFeatureKey, FeatureCatalogEntry>> =
    LazyLock::new(|| {
        let mut m: HashMap<InstanceFeatureKey, FeatureCatalogEntry> = HashMap::new();
        m.insert(
            InstanceFeatureKey::EnableEnvironments,
            FeatureCatalogEntry {
                title: "Environments",
                description:
                    "Show environment management in company settings and allow project and agent environment assignment controls.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableIsolatedWorkspaces,
            FeatureCatalogEntry {
                title: "Isolated Workspaces",
                description:
                    "Show execution workspace controls in project configuration and allow isolated workspace behavior for task runs.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableStreamlinedLeftNavigation,
            FeatureCatalogEntry {
                title: "Streamlined Left Navigation",
                description: "Use the streamlined main sidebar navigation layout.",
                tier: FeatureTier::Preference,
                cloud_default: true,
                self_hosted_default: true,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableApps,
            FeatureCatalogEntry {
                title: "Apps",
                description:
                    "Show the Apps navigation and allow access to app connections, gateways, and advanced app tooling.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnablePipelines,
            FeatureCatalogEntry {
                title: "Pipelines",
                description:
                    "Enable pipeline definitions and pipeline-driven case production surfaces.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableCases,
            FeatureCatalogEntry {
                title: "Cases",
                description:
                    "Durable work products that tasks create and iterate on. Adds the Cases tab and the agent case API.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableConferenceRoomChat,
            FeatureCatalogEntry {
                title: "Conference Room Chat",
                description:
                    "Add the Conference Room team chat, the live activity feed, and the redesigned onboarding; restyles task threads as chat bubbles.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableTaskChatRedesign,
            FeatureCatalogEntry {
                title: "Chat-Style Tasks",
                description:
                    "Reimagines the task detail page as a live conversation with your agents: chat bubbles for people and agents, streaming activity — thinking, tool calls, diffs — that folds into a one-line summary when a turn finishes, inline plan/question/permission cards, a three-mode composer (Agent · Plan · Ask), and a resizable Properties · Plan · Artifacts pane.",
                tier: FeatureTier::Preference,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableTaskWatchdogs,
            FeatureCatalogEntry {
                title: "Task Watchdogs",
                description:
                    "Show task detail controls for configuring watchdog agents that verify stopped task subtrees and restore live paths when work should continue.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableIssuePlanDecompositions,
            FeatureCatalogEntry {
                title: "Task Plan Decomposition Panel",
                description: "Show accepted-plan decomposition history on task detail pages.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableExperimentalFileViewer,
            FeatureCatalogEntry {
                title: "Experimental File Viewer",
                description:
                    "Show task detail controls for browsing and previewing workspace files relative to a task.",
                tier: FeatureTier::Preference,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableStatusCards,
            FeatureCatalogEntry {
                title: "Status Cards",
                description:
                    "Enable the experimental shared status-card board, update engine, and gated API.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableExternalObjects,
            FeatureCatalogEntry {
                title: "External Objects",
                description:
                    "Show the External Objects tab and surface external object attachments in task and case views.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableSmokeLab,
            FeatureCatalogEntry {
                title: "Smoke Lab",
                description:
                    "Add the Smoke Lab tab and dashboard card for exercising integration paths against deterministic local fixtures. Private deployments only.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableBuiltInAgents,
            FeatureCatalogEntry {
                title: "Built-in Agents",
                description:
                    "Show Paperclip-managed built-in agent surfaces, including roster badges, the Built-in agents tab, and setup controls.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableBetaSkills,
            FeatureCatalogEntry {
                title: "Beta skills",
                description: "Allow agents to pin beta releases of the Paperclip core skill.",
                tier: FeatureTier::Preference,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableSummaries,
            FeatureCatalogEntry {
                title: "Summaries",
                description:
                    "Show Summarizer-generated status slots on project and workspace pages, with on-demand refresh and revision history.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableDecisions,
            FeatureCatalogEntry {
                title: "Decisions",
                description:
                    "Show the Decisions item in the main sidebar — the attention home that surfaces tasks awaiting input.",
                tier: FeatureTier::Preference,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableGoalsSidebarLink,
            FeatureCatalogEntry {
                title: "Goals Sidebar Link",
                description: "Restore the Goals item in the main sidebar while the goals surface is being evaluated.",
                tier: FeatureTier::Preference,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableServerInfoDebugView,
            FeatureCatalogEntry {
                title: "Server Info Debug View",
                description:
                    "Show a Server section in the account drawer with the current server restart time and running commit.",
                tier: FeatureTier::Preference,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::AutoRestartDevServerWhenIdle,
            FeatureCatalogEntry {
                title: "Auto-Restart Dev Server When Idle",
                description:
                    "In local development, wait for queued and running agent runs to finish, then restart the server automatically when backend changes make the current boot stale.",
                tier: FeatureTier::Preference,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableIssueGraphLivenessAutoRecovery,
            FeatureCatalogEntry {
                title: "Auto-Create Recovery Tasks",
                description:
                    "Let the heartbeat scheduler create recovery tasks for task dependency chains found inside the configured lookback window.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableWorkspaceBranchReconcileForward,
            FeatureCatalogEntry {
                title: "Workspace Branch Reconcile Forward",
                description:
                    "Let execution workspaces reconcile a diverged recorded branch forward instead of failing branch containment.",
                tier: FeatureTier::Managed,
                cloud_default: true,
                self_hosted_default: true,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableWorkspaceDirtyQuarantineRepair,
            FeatureCatalogEntry {
                title: "Workspace Dirty Quarantine Repair",
                description:
                    "Let workspace runtime recovery quarantine and repair dirty execution workspaces before runs.",
                tier: FeatureTier::Managed,
                cloud_default: true,
                self_hosted_default: true,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableOwnerInstanceAdmin,
            FeatureCatalogEntry {
                title: "Owner Instance Admin",
                description:
                    "On cloud-managed instances, grant the stack owner instance-admin access to their own dedicated instance. Elevation is computed at the trusted-header auth boundary; no instance admin role rows are created. Inert on self-hosted instances.",
                tier: FeatureTier::Managed,
                cloud_default: true,
                self_hosted_default: false,
            },
        );
        m.insert(
            InstanceFeatureKey::EnableWorktreeRunExecution,
            FeatureCatalogEntry {
                title: "Worktree Run Execution",
                description:
                    "Let the scheduler execute runs inside an isolated git-worktree preview instance for tasks created after activation.",
                tier: FeatureTier::Managed,
                cloud_default: false,
                self_hosted_default: false,
            },
        );
        m
    });

/// 全部 feature key 的列表（与 Node `INSTANCE_FEATURE_KEYS` 1:1）。
pub static INSTANCE_FEATURE_KEYS: LazyLock<Vec<InstanceFeatureKey>> = LazyLock::new(|| {
    let mut keys: Vec<InstanceFeatureKey> = INSTANCE_FEATURE_CATALOG.keys().copied().collect();
    keys.sort();
    keys
});

// ============================================================================
// Helpers
// ============================================================================

/// 查 feature key 的 tier；未知 key 返回 `None`（与 Node
/// `INSTANCE_FEATURE_CATALOG[key].tier` 等价 + 防御性 None）。
pub fn tier_of(key: InstanceFeatureKey) -> Option<FeatureTier> {
    INSTANCE_FEATURE_CATALOG.get(&key).map(|e| e.tier)
}

/// 是否是 tier `"managed"` 的 feature key（managed-config 解析时使用）。
pub fn is_managed(key: InstanceFeatureKey) -> bool {
    matches!(tier_of(key), Some(FeatureTier::Managed))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_tier_round_trip() {
        for tier in [
            FeatureTier::Preference,
            FeatureTier::Managed,
            FeatureTier::Floor,
        ] {
            assert_eq!(FeatureTier::parse(tier.as_str()), Some(tier));
        }
    }

    #[test]
    fn feature_tier_parse_unknown() {
        assert_eq!(FeatureTier::parse("bogus"), None);
    }

    #[test]
    fn feature_tier_strings_match_node() {
        assert_eq!(FeatureTier::Preference.as_str(), "preference");
        assert_eq!(FeatureTier::Managed.as_str(), "managed");
        assert_eq!(FeatureTier::Floor.as_str(), "floor");
    }

    #[test]
    fn instance_feature_key_round_trip() {
        for key in INSTANCE_FEATURE_KEYS.iter() {
            assert_eq!(InstanceFeatureKey::parse(key.as_str()), Some(*key));
        }
    }

    #[test]
    fn instance_feature_key_parse_unknown() {
        assert_eq!(InstanceFeatureKey::parse("notAKey"), None);
        assert_eq!(InstanceFeatureKey::parse(""), None);
    }

    #[test]
    fn catalog_has_26_entries() {
        assert_eq!(INSTANCE_FEATURE_CATALOG.len(), 26);
    }

    #[test]
    fn managed_tier_features_match_node() {
        let expected_managed: &[InstanceFeatureKey] = &[
            InstanceFeatureKey::EnableEnvironments,
            InstanceFeatureKey::EnableIsolatedWorkspaces,
            InstanceFeatureKey::EnableApps,
            InstanceFeatureKey::EnablePipelines,
            InstanceFeatureKey::EnableCases,
            InstanceFeatureKey::EnableConferenceRoomChat,
            InstanceFeatureKey::EnableTaskWatchdogs,
            InstanceFeatureKey::EnableIssuePlanDecompositions,
            InstanceFeatureKey::EnableStatusCards,
            InstanceFeatureKey::EnableExternalObjects,
            InstanceFeatureKey::EnableSmokeLab,
            InstanceFeatureKey::EnableBuiltInAgents,
            InstanceFeatureKey::EnableSummaries,
            InstanceFeatureKey::EnableIssueGraphLivenessAutoRecovery,
            InstanceFeatureKey::EnableWorkspaceBranchReconcileForward,
            InstanceFeatureKey::EnableWorkspaceDirtyQuarantineRepair,
            InstanceFeatureKey::EnableOwnerInstanceAdmin,
            InstanceFeatureKey::EnableWorktreeRunExecution,
        ];
        let managed_keys: Vec<InstanceFeatureKey> = expected_managed.to_vec();
        for key in managed_keys {
            assert!(is_managed(key), "expected {:?} to be tier managed", key);
        }
    }

    #[test]
    fn preference_tier_features_match_node() {
        let expected: &[InstanceFeatureKey] = &[
            InstanceFeatureKey::EnableStreamlinedLeftNavigation,
            InstanceFeatureKey::EnableTaskChatRedesign,
            InstanceFeatureKey::EnableExperimentalFileViewer,
            InstanceFeatureKey::EnableBetaSkills,
            InstanceFeatureKey::EnableDecisions,
            InstanceFeatureKey::EnableGoalsSidebarLink,
            InstanceFeatureKey::EnableServerInfoDebugView,
            InstanceFeatureKey::AutoRestartDevServerWhenIdle,
        ];
        for key in expected {
            assert_eq!(
                tier_of(*key),
                Some(FeatureTier::Preference),
                "expected {:?} to be tier preference",
                key
            );
        }
    }

    #[test]
    fn keys_sorted_alphabetically() {
        let keys = &INSTANCE_FEATURE_KEYS;
        for window in keys.windows(2) {
            assert!(
                window[0] <= window[1],
                "{:?} should come before {:?}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn cloud_and_self_hosted_defaults_for_workspace_branch_reconcile() {
        let entry = INSTANCE_FEATURE_CATALOG
            .get(&InstanceFeatureKey::EnableWorkspaceBranchReconcileForward)
            .expect("entry must exist");
        assert!(entry.cloud_default);
        assert!(entry.self_hosted_default);
    }

    #[test]
    fn owner_instance_admin_is_managed_with_asymmetric_default() {
        let entry = INSTANCE_FEATURE_CATALOG
            .get(&InstanceFeatureKey::EnableOwnerInstanceAdmin)
            .expect("entry must exist");
        assert_eq!(entry.tier, FeatureTier::Managed);
        assert!(entry.cloud_default);
        assert!(!entry.self_hosted_default);
    }
}
