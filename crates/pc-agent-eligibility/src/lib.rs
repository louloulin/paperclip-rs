#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Agent work-eligibility + org-chain health.
//!
//! R544: Direct port of `paperclip/packages/shared/src/agent-eligibility.ts`
//! (245 LOC). All public API is deterministic, total, and free of I/O — the
//! only inputs are the candidate `agent` plus the list of every other agent
//! in the same company (so the chain walk can resolve `reports_to` ids).
//!
//! 设计原则:
//! - **Pure functions** — every public API is a pure transformation over
//!   the supplied agent list. No database calls, no global state, no time.
//! - **Strong types** — `AgentStatus` enum (`Active | Idle | Running | Paused
//! | Error | Terminated | PendingApproval | Other(String)`) replaces the
//!   loose `AgentStatus | string` TS union. `AgentEligibilityLifecycleReason`
//!   + `AgentOrgChainInvalidReason` make the closed sets explicit.
//! - **Cycle-safe traversal** — the chain walker uses a `seen` set so a
//!   cycle (A → B → A) terminates after recording the first repeated node as
//!   an `invalid_ancestor` of kind `Cycle`.
//! - **Cross-company isolation** — a `reports_to` pointing at an agent in a
//!   different `company_id` (or at an id absent from the supplied list) is
//!   reported as `MissingManager` rather than silently terminating the walk.
//!
//! 设计 vs Node 上游:
//! - `AgentStatus` becomes a Rust enum (not a free `string`), but we preserve
//!   the upstream "unknown statuses are allowed" semantics via
//!   `Other(String)` so callers passing arbitrary DB strings never panic.
//! - `agent.status` accepts `&AgentStatus` rather than `&str` (status is now
//!   parsed at the type level); this pushes string-to-enum conversion to
//!   the caller (typically the DB row), where it's cheap and total.
//! - `assignabilityReason` / `invokabilityReason` use a flat `match` instead
//!   of nested ternaries; the precedence is preserved exactly.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

// ============================================================================
// AgentStatus
// ============================================================================

/// Known agent status values. Mirrors the string literals in
/// `paperclip/packages/shared/src/constants.ts` AgentStatus enum.
///
/// `Other(String)` captures arbitrary / future status strings without
/// panicking — every public function treats unknown statuses conservatively
/// (always `UnknownStatus` reason, never assignable / invokable).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Active,
    Idle,
    Running,
    Paused,
    Error,
    Terminated,
    PendingApproval,
    /// Any status not enumerated above. Preserves the upstream "unknown
    /// statuses are allowed" semantics without making `AgentStatus`
    /// non-exhaustive.
    #[serde(untagged)]
    Other(String),
}

impl AgentStatus {
    /// Construct an `AgentStatus` from a raw database string.
    pub fn from_db(value: &str) -> Self {
        match value {
            "active" => Self::Active,
            "idle" => Self::Idle,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "error" => Self::Error,
            "terminated" => Self::Terminated,
            "pending_approval" => Self::PendingApproval,
            other => Self::Other(other.to_string()),
        }
    }

    /// Lower-case wire form.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Error => "error",
            Self::Terminated => "terminated",
            Self::PendingApproval => "pending_approval",
            Self::Other(s) => s.as_str(),
        }
    }
}

// ============================================================================
// Lifecycle + chain enums
// ============================================================================

/// Why an agent is (or isn't) eligible to be assigned work / invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEligibilityLifecycleReason {
    Eligible,
    Terminated,
    PendingApproval,
    Paused,
    InvalidOrgChain,
    UnknownStatus,
}

/// Relation of an entry in `full_chain` to the queried agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrgChainRelation {
    Subject,
    Ancestor,
}

/// Why an org chain was flagged as invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrgChainInvalidReason {
    Healthy,
    TerminatedAncestor,
    MissingManager,
    Cycle,
}

/// Top-level org-chain health verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrgChainHealthStatus {
    Healthy,
    InvalidOrgChain,
}

// ============================================================================
// Data shapes
// ============================================================================

/// Minimal agent view required for eligibility computation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEligibilityAgent {
    pub id: String,
    pub company_id: String,
    pub name: String,
    pub status: AgentStatus,
    pub reports_to: Option<String>,
}

/// One step in `AgentOrgChainHealth::full_chain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOrgChainEntry {
    pub id: String,
    pub company_id: String,
    pub name: String,
    pub status: AgentStatus,
    pub reports_to: Option<String>,
    pub depth: u32,
    pub relation: AgentOrgChainRelation,
}

/// Synthetic record inserted into `invalid_ancestors` for `cycle` /
/// `missing_manager` cases (the parent agent might not exist).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInvalidOrgChainAncestor {
    pub id: String,
    pub name: String,
    /// `terminated` (real terminated parent), `missing` (parent id absent),
    /// or `cycle` (parent already in chain).
    pub status: AgentStatus,
}

/// Result of walking the reporting chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOrgChainHealth {
    pub status: AgentOrgChainHealthStatus,
    pub reason: AgentOrgChainInvalidReason,
    pub full_chain: Vec<AgentOrgChainEntry>,
    pub first_invalid_ancestor: Option<AgentInvalidOrgChainAncestor>,
    pub invalid_ancestors: Vec<AgentInvalidOrgChainAncestor>,
    pub repair_guidance: Option<String>,
}

/// Combined eligibility verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkEligibility {
    pub assignable: bool,
    pub invokable: bool,
    pub assignability_reason: AgentEligibilityLifecycleReason,
    pub invokability_reason: AgentEligibilityLifecycleReason,
    pub org_chain_health: AgentOrgChainHealth,
}

/// Bundled input to every public API in this crate. Keeps call sites tidy
/// without forcing tuple-of-positional-args.
#[derive(Debug, Clone)]
pub struct EligibilityInput<'a> {
    pub agent: &'a AgentEligibilityAgent,
    pub agents: &'a [AgentEligibilityAgent],
}

// ============================================================================
// Internal status sets (mirror Node constants)
// ============================================================================

/// Statuses that are *ever* candidates for assignment (subject to
/// `NON_ASSIGNABLE_AGENT_STATUSES`).
const ASSIGNABLE_STATUSES: &[AgentStatus] = &[
    AgentStatus::Active,
    AgentStatus::Paused,
    AgentStatus::Idle,
    AgentStatus::Running,
    AgentStatus::Error,
];

/// Statuses that *disqualify* a candidate from being assignable.
const NON_ASSIGNABLE_STATUSES: &[AgentStatus] =
    &[AgentStatus::Terminated, AgentStatus::PendingApproval];

/// Statuses that are *ever* candidates for invocation.
const INVOKABLE_STATUSES: &[AgentStatus] = &[
    AgentStatus::Active,
    AgentStatus::Idle,
    AgentStatus::Running,
    AgentStatus::Error,
];

/// Statuses that *disqualify* a candidate from being invokable.
const NON_INVOKABLE_STATUSES: &[AgentStatus] = &[
    AgentStatus::Terminated,
    AgentStatus::PendingApproval,
    AgentStatus::Paused,
];

// ============================================================================
// Public API — status predicates
// ============================================================================

/// Returns `true` if `status` is in [`ASSIGNABLE_STATUSES`] AND not in
/// [`NON_ASSIGNABLE_STATUSES`].
pub fn is_agent_status_assignable_to_work(status: &AgentStatus) -> bool {
    ASSIGNABLE_STATUSES.contains(status) && !NON_ASSIGNABLE_STATUSES.contains(status)
}

/// Returns `true` if `status` is in [`INVOKABLE_STATUSES`] AND not in
/// [`NON_INVOKABLE_STATUSES`].
pub fn is_agent_status_invokable(status: &AgentStatus) -> bool {
    INVOKABLE_STATUSES.contains(status) && !NON_INVOKABLE_STATUSES.contains(status)
}

// ============================================================================
// Public API — chain walk
// ============================================================================

/// Walk the agent's `reports_to` chain and classify the resulting org graph.
///
/// `agents` is the full agent roster for the company. `agent` must be a
/// member of `agents` (else its `reports_to` chain can't be resolved beyond
/// the self-entry). The walk is bounded by the `seen` set so cycles
/// terminate after one recorded invalid ancestor.
pub fn get_agent_org_chain_health(input: &EligibilityInput<'_>) -> AgentOrgChainHealth {
    let by_id: HashMap<&str, &AgentEligibilityAgent> =
        input.agents.iter().map(|a| (a.id.as_str(), a)).collect();

    let mut full_chain: Vec<AgentOrgChainEntry> = Vec::with_capacity(input.agents.len().min(16));
    full_chain.push(chain_entry(input.agent, 0, AgentOrgChainRelation::Subject));

    let mut invalid_ancestors: Vec<AgentInvalidOrgChainAncestor> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(input.agent.id.clone());

    let mut current = input.agent;
    let mut depth: u32 = 1;

    while let Some(reports_to_id) = current.reports_to.as_deref() {
        if seen.contains(reports_to_id) {
            // Cycle — record the offending id and stop.
            let cycle_agent = by_id.get(reports_to_id).copied();
            let invalid = AgentInvalidOrgChainAncestor {
                id: reports_to_id.to_string(),
                name: cycle_agent.map_or_else(|| reports_to_id.to_string(), |a| a.name.clone()),
                status: AgentStatus::Other("cycle".to_string()),
            };
            full_chain.push(AgentOrgChainEntry {
                id: invalid.id.clone(),
                company_id: input.agent.company_id.clone(),
                name: invalid.name.clone(),
                status: invalid.status.clone(),
                reports_to: cycle_agent.and_then(|a| a.reports_to.clone()),
                depth,
                relation: AgentOrgChainRelation::Ancestor,
            });
            invalid_ancestors.push(invalid);
            break;
        }
        seen.insert(reports_to_id.to_string());

        let parent = match by_id.get(reports_to_id) {
            Some(parent) if parent.company_id == input.agent.company_id => *parent,
            Some(_) | None => {
                // Either the id is missing entirely, or it belongs to a
                // different company. Both cases are "missing manager".
                let invalid = AgentInvalidOrgChainAncestor {
                    id: reports_to_id.to_string(),
                    name: reports_to_id.to_string(),
                    status: AgentStatus::Other("missing".to_string()),
                };
                full_chain.push(AgentOrgChainEntry {
                    id: invalid.id.clone(),
                    company_id: input.agent.company_id.clone(),
                    name: invalid.name.clone(),
                    status: invalid.status.clone(),
                    reports_to: None,
                    depth,
                    relation: AgentOrgChainRelation::Ancestor,
                });
                invalid_ancestors.push(invalid);
                break;
            }
        };

        full_chain.push(chain_entry(parent, depth, AgentOrgChainRelation::Ancestor));
        if parent.status == AgentStatus::Terminated {
            invalid_ancestors.push(invalid_ancestor(parent));
        }

        current = parent;
        depth += 1;
    }

    let first_invalid_ancestor = invalid_ancestors.first().cloned();
    let (status, reason) = classify_org_chain_health(first_invalid_ancestor.as_ref());
    let repair_guidance = first_invalid_ancestor
        .as_ref()
        .map(|ancestor| build_repair_guidance(input.agent, ancestor));

    AgentOrgChainHealth {
        status,
        reason,
        full_chain,
        first_invalid_ancestor,
        invalid_ancestors,
        repair_guidance,
    }
}

fn classify_org_chain_health(
    first_invalid_ancestor: Option<&AgentInvalidOrgChainAncestor>,
) -> (AgentOrgChainHealthStatus, AgentOrgChainInvalidReason) {
    let Some(ancestor) = first_invalid_ancestor else {
        return (
            AgentOrgChainHealthStatus::Healthy,
            AgentOrgChainInvalidReason::Healthy,
        );
    };
    let reason = if ancestor.status == AgentStatus::Other("missing".to_string()) {
        AgentOrgChainInvalidReason::MissingManager
    } else if ancestor.status == AgentStatus::Other("cycle".to_string()) {
        AgentOrgChainInvalidReason::Cycle
    } else {
        AgentOrgChainInvalidReason::TerminatedAncestor
    };
    (AgentOrgChainHealthStatus::InvalidOrgChain, reason)
}

// ============================================================================
// Public API — work eligibility
// ============================================================================

/// Combined eligibility verdict. Combines status checks with org-chain
/// health to produce final `assignable` / `invokable` booleans plus their
/// human-readable reasons.
pub fn get_agent_work_eligibility(input: &EligibilityInput<'_>) -> AgentWorkEligibility {
    let org_chain_health = get_agent_org_chain_health(input);
    let assignability_reason =
        compute_lifecycle_reason(input.agent, &org_chain_health, LifecycleAxis::Assignable);
    let invokability_reason =
        compute_lifecycle_reason(input.agent, &org_chain_health, LifecycleAxis::Invokable);
    AgentWorkEligibility {
        assignable: assignability_reason == AgentEligibilityLifecycleReason::Eligible,
        invokable: invokability_reason == AgentEligibilityLifecycleReason::Eligible,
        assignability_reason,
        invokability_reason,
        org_chain_health,
    }
}

#[derive(Debug, Clone, Copy)]
enum LifecycleAxis {
    Assignable,
    Invokable,
}

fn compute_lifecycle_reason(
    agent: &AgentEligibilityAgent,
    health: &AgentOrgChainHealth,
    axis: LifecycleAxis,
) -> AgentEligibilityLifecycleReason {
    let status_ok = match axis {
        LifecycleAxis::Assignable => is_agent_status_assignable_to_work(&agent.status),
        LifecycleAxis::Invokable => is_agent_status_invokable(&agent.status),
    };
    if !status_ok {
        return status_reason(agent, axis);
    }
    if health.status == AgentOrgChainHealthStatus::InvalidOrgChain {
        return AgentEligibilityLifecycleReason::InvalidOrgChain;
    }
    AgentEligibilityLifecycleReason::Eligible
}

fn status_reason(
    agent: &AgentEligibilityAgent,
    axis: LifecycleAxis,
) -> AgentEligibilityLifecycleReason {
    // Reproduce the precedence: explicit status > generic unknown.
    // Mirrors the nested ternaries in the upstream `getAgentWorkEligibility`.
    match axis {
        LifecycleAxis::Assignable => match agent.status {
            AgentStatus::Terminated => AgentEligibilityLifecycleReason::Terminated,
            AgentStatus::PendingApproval => AgentEligibilityLifecycleReason::PendingApproval,
            _ => AgentEligibilityLifecycleReason::UnknownStatus,
        },
        LifecycleAxis::Invokable => match agent.status {
            AgentStatus::Terminated => AgentEligibilityLifecycleReason::Terminated,
            AgentStatus::PendingApproval => AgentEligibilityLifecycleReason::PendingApproval,
            AgentStatus::Paused => AgentEligibilityLifecycleReason::Paused,
            _ => AgentEligibilityLifecycleReason::UnknownStatus,
        },
    }
}

/// Convenience wrapper around [`get_agent_work_eligibility`].
pub fn is_agent_assignable_to_work(input: &EligibilityInput<'_>) -> bool {
    get_agent_work_eligibility(input).assignable
}

/// Convenience wrapper around [`get_agent_work_eligibility`].
pub fn is_agent_invokable(input: &EligibilityInput<'_>) -> bool {
    get_agent_work_eligibility(input).invokable
}

// ============================================================================
// Internal helpers
// ============================================================================

fn chain_entry(
    agent: &AgentEligibilityAgent,
    depth: u32,
    relation: AgentOrgChainRelation,
) -> AgentOrgChainEntry {
    AgentOrgChainEntry {
        id: agent.id.clone(),
        company_id: agent.company_id.clone(),
        name: agent.name.clone(),
        status: agent.status.clone(),
        reports_to: agent.reports_to.clone(),
        depth,
        relation,
    }
}

fn invalid_ancestor(agent: &AgentEligibilityAgent) -> AgentInvalidOrgChainAncestor {
    AgentInvalidOrgChainAncestor {
        id: agent.id.clone(),
        name: agent.name.clone(),
        status: agent.status.clone(),
    }
}

fn build_repair_guidance(
    agent: &AgentEligibilityAgent,
    first_invalid_ancestor: &AgentInvalidOrgChainAncestor,
) -> String {
    let missing = AgentStatus::Other("missing".to_string());
    let cycle = AgentStatus::Other("cycle".to_string());
    if first_invalid_ancestor.status == missing {
        format!(
            "{} reports to missing manager {}. Reassign {} or the nearest affected ancestor under an active manager/root, or explicitly pause or terminate the invalid subtree before assigning work or starting runs.",
            agent.name,
            first_invalid_ancestor.id,
            agent.name,
        )
    } else if first_invalid_ancestor.status == cycle {
        format!(
            "{} has a cycle in its reporting chain at {}. Break the cycle by assigning one affected agent to an active manager/root, or explicitly pause or terminate the invalid subtree before assigning work or starting runs.",
            agent.name,
            first_invalid_ancestor.name,
        )
    } else {
        format!(
            "{} reports through terminated ancestor {}. Reassign {} or the nearest affected ancestor under an active manager/root, or explicitly pause or terminate the invalid subtree before assigning work or starting runs.",
            agent.name,
            first_invalid_ancestor.name,
            agent.name,
        )
    }
}

// ============================================================================
// Internal unit tests
// ============================================================================

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn agent_status_from_db_round_trips() {
        for s in [
            "active",
            "idle",
            "running",
            "paused",
            "error",
            "terminated",
            "pending_approval",
        ] {
            let parsed = AgentStatus::from_db(s);
            assert_eq!(parsed.as_str(), s, "round-trip for {s}");
        }
        assert_eq!(
            AgentStatus::from_db("custom_thing").as_str(),
            "custom_thing"
        );
    }
}
