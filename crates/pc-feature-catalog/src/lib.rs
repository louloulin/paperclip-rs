#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Feature catalog for cloud-managed instance experimental flags.
//!
//! R556: Direct port of `paperclip/packages/shared/src/feature-catalog.ts` (282 LOC).
//!
//! Each flag is a key in `INSTANCE_FEATURE_CATALOG`. Keys are derived from
//! the `instanceExperimentalSettingsSchema` (Node upstream). We mirror the
//! keys as a `&'static str` constant so the catalog is discoverable and the
//! Rust port doesn't depend on `zod`.

/// All known feature tiers.
pub const FEATURE_TIERS: [&str; 3] = ["preference", "managed", "floor"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureTier {
    Preference,
    Managed,
    Floor,
}

impl FeatureTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::Managed => "managed",
            Self::Floor => "floor",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "preference" => Some(Self::Preference),
            "managed" => Some(Self::Managed),
            "floor" => Some(Self::Floor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeatureCatalogEntry {
    pub title: &'static str,
    pub description: &'static str,
    pub tier: FeatureTier,
    pub cloud_default: bool,
    pub self_hosted_default: bool,
}

/// Lookup the catalog entry for a feature key. Returns `None` for unknown keys.
pub fn lookup_feature(key: &str) -> Option<&'static FeatureCatalogEntry> {
    for (k, entry) in INSTANCE_FEATURE_CATALOG {
        if *k == key {
            return Some(entry);
        }
    }
    None
}

/// All known feature keys, sorted alphabetically — mirrors
/// `Object.keys(INSTANCE_FEATURE_CATALOG).sort()`.
pub fn instance_feature_keys() -> Vec<&'static str> {
    let mut keys: Vec<&'static str> = INSTANCE_FEATURE_CATALOG.iter().map(|(k, _)| *k).collect();
    keys.sort_unstable();
    keys
}

/// Build the JSON artifact consumed by the cloud harness per release.
///
/// Mirrors `buildFeatureCatalogArtifact`.
pub fn build_feature_catalog_artifact(
    catalog_version: &str,
) -> Result<serde_json::Value, &'static str> {
    if catalog_version.trim().is_empty() {
        return Err("catalogVersion must be a non-empty string");
    }
    let mut features = serde_json::Map::new();
    for key in instance_feature_keys() {
        let entry = lookup_feature(key).expect("key present in catalog");
        features.insert(
            key.to_string(),
            serde_json::json!({ "tier": entry.tier.as_str() }),
        );
    }
    Ok(serde_json::json!({
        "catalogVersion": catalog_version,
        "features": features,
    }))
}

/// Deterministic JSON serialization of the artifact (sorted keys, 2-space indent,
/// trailing newline). Mirrors `renderFeatureCatalogArtifact`.
pub fn render_feature_catalog_artifact(catalog_version: &str) -> Result<String, &'static str> {
    let artifact = build_feature_catalog_artifact(catalog_version)?;
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&artifact).unwrap()
    ))
}

// ---------- catalog data ----------

/// The static catalog — mirror of `INSTANCE_FEATURE_CATALOG`.
/// Order does not matter for lookups; `instance_feature_keys()` returns sorted.
pub const INSTANCE_FEATURE_CATALOG: &[(&str, FeatureCatalogEntry)] = &[
    (
        "enableEnvironments",
        FeatureCatalogEntry {
            title: "Environments",
            description: "Show environment management in company settings and allow project and agent environment assignment controls.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableIsolatedWorkspaces",
        FeatureCatalogEntry {
            title: "Isolated Workspaces",
            description: "Show execution workspace controls in project configuration and allow isolated workspace behavior for task runs.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableStreamlinedLeftNavigation",
        FeatureCatalogEntry {
            title: "Streamlined Left Navigation",
            description: "Use the streamlined main sidebar navigation layout.",
            tier: FeatureTier::Preference,
            cloud_default: true,
            self_hosted_default: true,
        },
    ),
    (
        "enableApps",
        FeatureCatalogEntry {
            title: "Apps",
            description: "Show the Apps navigation and allow access to app connections, gateways, and advanced app tooling.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enablePipelines",
        FeatureCatalogEntry {
            title: "Pipelines",
            description: "Enable pipeline definitions and pipeline-driven case production surfaces.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableCases",
        FeatureCatalogEntry {
            title: "Cases",
            description: "Durable work products that tasks create and iterate on. Adds the Cases tab and the agent case API.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableConferenceRoomChat",
        FeatureCatalogEntry {
            title: "Conference Room Chat",
            description: "Add the Conference Room team chat, the live activity feed, and the redesigned onboarding; restyles task threads as chat bubbles.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableTaskChatRedesign",
        FeatureCatalogEntry {
            title: "Chat-Style Tasks",
            description: "Restyle task threads using the chat-bubble layout used by Conference Room.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableTaskWatchdogs",
        FeatureCatalogEntry {
            title: "Task Watchdogs",
            description: "Run a watchdog that detects stalled task runs and triggers a recovery flow.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableIssuePlanDecompositions",
        FeatureCatalogEntry {
            title: "Issue Plan Decompositions",
            description: "Allow issue plans to be auto-decomposed into tracked subtasks.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableExperimentalFileViewer",
        FeatureCatalogEntry {
            title: "Experimental File Viewer",
            description: "Show the experimental in-app file viewer for case attachments.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableStatusCards",
        FeatureCatalogEntry {
            title: "Status Cards",
            description: "Enable the status card dashboard surfacing per-pipeline health.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableExternalObjects",
        FeatureCatalogEntry {
            title: "External Objects",
            description: "Allow external objects to be ingested and referenced from issue text.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableSmokeLab",
        FeatureCatalogEntry {
            title: "Smoke Lab",
            description: "Enable the Smoke Lab fixtures and runs surfaces for adapter smoke tests.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableBuiltInAgents",
        FeatureCatalogEntry {
            title: "Built-In Agents",
            description: "Expose built-in agents in the company agent picker.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableBetaSkills",
        FeatureCatalogEntry {
            title: "Beta Skills",
            description: "Expose skills tagged as beta in the skills picker.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableSummaries",
        FeatureCatalogEntry {
            title: "Summaries",
            description: "Show summary slot widgets across company surfaces.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableDecisions",
        FeatureCatalogEntry {
            title: "Decisions",
            description: "Enable the decisions log and approvals surfaces.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableGoalsSidebarLink",
        FeatureCatalogEntry {
            title: "Goals Sidebar Link",
            description: "Surface the goals navigation link in the left sidebar.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableServerInfoDebugView",
        FeatureCatalogEntry {
            title: "Server Info Debug View",
            description: "Show the server info debug view in company admin pages.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "autoRestartDevServerWhenIdle",
        FeatureCatalogEntry {
            title: "Auto-Restart Dev Server When Idle",
            description: "In local development, wait for queued and running agent runs to finish, then restart the server automatically when backend changes make the current boot stale.",
            tier: FeatureTier::Preference,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableIssueGraphLivenessAutoRecovery",
        FeatureCatalogEntry {
            title: "Auto-Create Recovery Tasks",
            description: "Let the heartbeat scheduler create recovery tasks for task dependency chains found inside the configured lookback window.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
    (
        "enableWorkspaceBranchReconcileForward",
        FeatureCatalogEntry {
            title: "Workspace Branch Reconcile Forward",
            description: "Let execution workspaces reconcile a diverged recorded branch forward instead of failing branch containment.",
            tier: FeatureTier::Managed,
            cloud_default: true,
            self_hosted_default: true,
        },
    ),
    (
        "enableWorkspaceDirtyQuarantineRepair",
        FeatureCatalogEntry {
            title: "Workspace Dirty Quarantine Repair",
            description: "Let workspace runtime recovery quarantine and repair dirty execution workspaces before runs.",
            tier: FeatureTier::Managed,
            cloud_default: true,
            self_hosted_default: true,
        },
    ),
    (
        "enableOwnerInstanceAdmin",
        FeatureCatalogEntry {
            title: "Owner Instance Admin",
            description: "On cloud-managed instances, grant the stack owner instance-admin access to their own dedicated instance. Elevation is computed at the trusted-header auth boundary; no instance admin role rows are created. Inert on self-hosted instances.",
            tier: FeatureTier::Managed,
            cloud_default: true,
            self_hosted_default: false,
        },
    ),
    (
        "enableWorktreeRunExecution",
        FeatureCatalogEntry {
            title: "Worktree Run Execution",
            description: "Let the scheduler execute runs inside an isolated git-worktree preview instance for tasks created after activation.",
            tier: FeatureTier::Managed,
            cloud_default: false,
            self_hosted_default: false,
        },
    ),
];

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn feature_tier_round_trip() {
        for tier in [
            FeatureTier::Preference,
            FeatureTier::Managed,
            FeatureTier::Floor,
        ] {
            let s = tier.as_str();
            assert_eq!(FeatureTier::parse(s), Some(tier));
        }
        assert!(FeatureTier::parse("nope").is_none());
    }

    #[test]
    fn catalog_size_matches_node() {
        // Node has ~25 entries; we mirror all of them.
        assert_eq!(INSTANCE_FEATURE_CATALOG.len(), 26);
    }

    #[test]
    fn lookup_known_key() {
        let entry = lookup_feature("enableEnvironments").unwrap();
        assert_eq!(entry.title, "Environments");
        assert_eq!(entry.tier, FeatureTier::Managed);
        assert!(!entry.cloud_default);
    }

    #[test]
    fn lookup_unknown_key_returns_none() {
        assert!(lookup_feature("enableUnknownFlag").is_none());
    }

    #[test]
    fn keys_sorted() {
        let keys = instance_feature_keys();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn build_artifact_validates_version() {
        assert!(build_feature_catalog_artifact("").is_err());
        assert!(build_feature_catalog_artifact("   ").is_err());
    }

    #[test]
    fn build_artifact_contains_all_keys() {
        let artifact = build_feature_catalog_artifact("v1").unwrap();
        let features = artifact["features"].as_object().unwrap();
        for key in instance_feature_keys() {
            assert!(features.contains_key(key), "missing {key}");
            assert!(features[key]["tier"].is_string());
        }
    }

    #[test]
    fn render_artifact_is_deterministic() {
        let a = render_feature_catalog_artifact("v1").unwrap();
        let b = render_feature_catalog_artifact("v1").unwrap();
        assert_eq!(a, b);
        assert!(a.ends_with('\n'));
    }
}
