#![forbid(unsafe_code)]
//! 热重启决策函数（无 I/O）。
//!
//! 对应 Node services/heartbeat.ts 中的 prepareHotRestartShutdown 与
//! reconcileHotRestartAdoption 决策部分。所有 I/O（读 intent/
//! 写 snapshot/读 DB/写 report）都由 hot_restart_db.rs 和 pc-hot-restart 完成。


use pc_hot_restart::{
    find_missing_hot_restart_snapshot_run_ids, should_honor_hot_restart_intent_for_process,
    HotRestartIntent, HotRestartIntentRun, HotRestartReport, HotRestartReportRun,
};
use pc_repos::heartbeat::RunningHeartbeatWithAdapterRow;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

pub use pc_hot_restart::ShutdownSignal;
/// 与 Node `HotRestartReportRun.classification` 1:1 的分类枚举。
pub use pc_hot_restart::HotRestartRunClassification as RunClassification;


/// 与 Node SESSIONED_LOCAL_ADAPTERS 对齐的本地子进程 adapter 列表。
pub const SESSIONED_LOCAL_ADAPTERS: &[&str] = &[
    "claude_local",
    "codex_local",
    "cursor",
    "gemini_local",
    "hermes_local",
    "opencode_local",
    "pi_local",
];

pub fn is_tracked_local_child_process_adapter(adapter_type: &str) -> bool {
    SESSIONED_LOCAL_ADAPTERS.contains(&adapter_type)
}

/// 从 DB 行构造 hot-restart snapshot run。
pub fn run_to_intent_run(run: &RunningHeartbeatWithAdapterRow) -> HotRestartIntentRun {
    HotRestartIntentRun {
        run_id: run.run.id.to_string(),
        company_id: run.run.company_id.to_string(),
        agent_id: run.run.agent_id.to_string(),
        adapter_type: run.adapter_type.clone(),
        status: run.run.status.clone(),
        process_pid: run.run.process_pid,
        process_group_id: run.run.process_group_id,
        issue_id: run
            .run
            .context_snapshot
            .as_ref()
            .and_then(|value| value.get("issueId"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
    }
}

/// prepare_shutdown 的决策结果，与 Node 函数返回值 1:1。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode")]
pub enum PrepareShutdownDecision {
    #[serde(rename = "not_requested")]
    NotRequested { skip_drain: bool },
    #[serde(rename = "drain_required")]
    DrainRequired { skip_drain: bool },
    #[serde(rename = "pid_mismatch")]
    PidMismatch { skip_drain: bool, expected_pid: i32, current_pid: i32 },
    #[serde(rename = "hot_restart")]
    HotRestart { skip_drain: bool, active_run_ids: Vec<String> },
    #[serde(rename = "read_error")]
    ReadError { skip_drain: bool },
}

impl PrepareShutdownDecision {
    pub fn skip_drain(&self) -> bool {
        matches!(self, Self::HotRestart { skip_drain: true, .. })
    }
}

/// 判断旧 server 是否需要为 hot-restart 跳过 normal drain。
///
/// 与 Node `prepareHotRestartShutdown` 1:1：
/// - 无 intent → NotRequested
/// - intent.drainRequired → DrainRequired
/// - PID 不匹配 → PidMismatch
/// - 其余 → HotRestart（active_run_ids 由调用方在查询 DB 后填入）
pub fn decide_prepare_shutdown(intent: Option<&HotRestartIntent>, current_pid: i32) -> PrepareShutdownDecision {
    let Some(intent) = intent else {
        return PrepareShutdownDecision::NotRequested { skip_drain: false };
    };
    if intent.drain_required {
        return PrepareShutdownDecision::DrainRequired { skip_drain: false };
    };
    if !should_honor_hot_restart_intent_for_process(intent, current_pid) {
        return PrepareShutdownDecision::PidMismatch {
            skip_drain: false,
            expected_pid: intent.previous_server_pid,
            current_pid,
        };
    }
    PrepareShutdownDecision::HotRestart {
        skip_drain: true,
        active_run_ids: Vec::new(),
    }
}

/// reconcile 阶段被分类的 run。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionCandidate {
    pub run_id: Uuid,
    pub company_id: Uuid,
    pub agent_id: Uuid,
    pub adapter_type: String,
    pub run_status: String,
    pub process_pid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub classification: RunClassification,
    pub reason: String,
}

/// 一个 run 在 reconcile 阶段的事实，DB 与进程探针的合并。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptionFacts {
    pub run_id: Uuid,
    pub run_status: String,
    pub adapter_type: String,
    pub process_pid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub process_pid_alive: bool,
    pub process_group_alive: bool,
}

/// 决定一个 run 的最终分类。
pub fn classify_adoption_candidate(
    candidate: HotRestartIntentRun,
    facts: AdoptionFacts,
    intent_drain_required: bool,
) -> AdoptionCandidate {
    if facts.run_status != "running" {
        let status_label = facts.run_status.clone();
        return build_candidate(
            candidate,
            facts,
            RunClassification::FinalizedWhileDown,
            format!("run_status_{}", status_label),
        );
    }
    if intent_drain_required {
        return build_candidate(candidate, facts, RunClassification::Skipped, "drain_required".into());
    }
    if !is_tracked_local_child_process_adapter(&facts.adapter_type) {
        return build_candidate(
            candidate,
            facts,
            RunClassification::Skipped,
            "adapter_not_local_child_process".into(),
        );
    }
    if facts.process_pid.is_none() && facts.process_group_id.is_none() {
        return build_candidate(
            candidate,
            facts,
            RunClassification::Lost,
            "missing_process_metadata".into(),
        );
    }
    if !facts.process_pid_alive && !facts.process_group_alive {
        return build_candidate(
            candidate,
            facts,
            RunClassification::Lost,
            "process_not_alive".into(),
        );
    }
    let reason = if facts.process_pid_alive {
        "process_pid_alive"
    } else {
        "process_group_alive"
    };
    build_candidate(candidate, facts, RunClassification::Adopted, reason.into())
}

fn build_candidate(
    candidate: HotRestartIntentRun,
    facts: AdoptionFacts,
    classification: RunClassification,
    reason: String,
) -> AdoptionCandidate {
    AdoptionCandidate {
        run_id: Uuid::parse_str(&candidate.run_id).unwrap_or(Uuid::nil()),
        company_id: Uuid::parse_str(&candidate.company_id).unwrap_or(Uuid::nil()),
        agent_id: Uuid::parse_str(&candidate.agent_id).unwrap_or(Uuid::nil()),
        adapter_type: candidate.adapter_type,
        run_status: candidate.status,
        process_pid: candidate.process_pid,
        process_group_id: candidate.process_group_id,
        classification,
        reason,
    }
}

/// 装配最终 report。
#[allow(clippy::too_many_arguments)]
pub fn build_report(
    intent: &HotRestartIntent,
    previous_server_version: Option<String>,
    new_server_pid: i32,
    new_server_version: &str,
    completed_at: &str,
    candidates: Vec<AdoptionCandidate>,
    missing_snapshot_run_ids: Vec<String>,
    finalized_while_down_missing_ids: Vec<String>,
) -> HotRestartReport {
    let mut adopted_run_ids = Vec::new();
    let mut finalized_while_down_run_ids = Vec::new();
    let mut lost_run_ids = Vec::new();
    let mut skipped_run_ids = Vec::new();
    let mut runs = Vec::new();
    for candidate in candidates {
        match candidate.classification {
            RunClassification::Adopted => adopted_run_ids.push(candidate.run_id.to_string()),
            RunClassification::FinalizedWhileDown => finalized_while_down_run_ids.push(candidate.run_id.to_string()),
            RunClassification::Lost => lost_run_ids.push(candidate.run_id.to_string()),
            RunClassification::Skipped => skipped_run_ids.push(candidate.run_id.to_string()),
        }
        runs.push(HotRestartReportRun {
            run: HotRestartIntentRun {
                run_id: candidate.run_id.to_string(),
                company_id: candidate.company_id.to_string(),
                agent_id: candidate.agent_id.to_string(),
                adapter_type: candidate.adapter_type,
                status: candidate.run_status,
                process_pid: candidate.process_pid,
                process_group_id: candidate.process_group_id,
                issue_id: None,
            },
            classification: candidate.classification.as_str().to_owned(),
            reason: candidate.reason,
        });
    }
    finalized_while_down_run_ids.extend(finalized_while_down_missing_ids);
    let mut finalized_while_down_for_missing = missing_snapshot_run_ids.clone();
    finalized_while_down_for_missing.retain(|id| {
        !adopted_run_ids.contains(id) && !skipped_run_ids.contains(id) && !lost_run_ids.contains(id)
    });
    finalized_while_down_run_ids.extend(finalized_while_down_for_missing);
    HotRestartReport {
        version: 1,
        requested_at: intent.requested_at.clone(),
        completed_at: completed_at.to_owned(),
        drain_required: intent.drain_required,
        previous_server_pid: intent.previous_server_pid,
        new_server_pid,
        previous_server_version,
        new_server_version: new_server_version.to_owned(),
        adopted_run_ids,
        finalized_while_down_run_ids,
        lost_run_ids,
        skipped_run_ids,
        runs,
    }
}

/// 推导 intent 中被 preflight 标记但未进入 snapshot 的 run id。
pub fn resolve_missing_run_ids(intent: &HotRestartIntent) -> Vec<String> {
    find_missing_hot_restart_snapshot_run_ids(intent)
}

/// 把 Option<Value> 收敛为对象（保护性 helper）。
pub fn payload_to_object(value: Option<Value>) -> Value {
    value.unwrap_or(Value::Object(Default::default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(drain: bool) -> HotRestartIntent {
        HotRestartIntent {
            version: 1,
            requested_at: "2026-08-12T00:00:00.000Z".into(),
            previous_server_pid: 99,
            previous_server_identity: Some("boot".into()),
            previous_server_started_at: Some("2026-08-12T00:00:00.000Z".into()),
            previous_server_version: Some("old".into()),
            drain_required: drain,
            requested_by_run_id: None,
            preflight_active_run_ids: vec!["run-a".into()],
            shutdown_snapshot: None,
        }
    }

    #[test]
    fn prepare_returns_not_requested_without_intent() {
        assert!(matches!(
            decide_prepare_shutdown(None, 1),
            PrepareShutdownDecision::NotRequested { .. }
        ));
    }

    #[test]
    fn prepare_returns_drain_required_when_drain_flag() {
        let intent = intent(true);
        assert!(matches!(
            decide_prepare_shutdown(Some(&intent), 99),
            PrepareShutdownDecision::DrainRequired { .. }
        ));
    }

    #[test]
    fn prepare_returns_pid_mismatch_when_current_differs() {
        let intent = intent(false);
        assert!(matches!(
            decide_prepare_shutdown(Some(&intent), 7),
            PrepareShutdownDecision::PidMismatch { .. }
        ));
    }

    #[test]
    fn classify_lost_when_metadata_missing() {
        let candidate = HotRestartIntentRun {
            run_id: Uuid::new_v4().to_string(),
            company_id: Uuid::new_v4().to_string(),
            agent_id: Uuid::new_v4().to_string(),
            adapter_type: "codex_local".into(),
            status: "running".into(),
            process_pid: None,
            process_group_id: None,
            issue_id: None,
        };
        let facts = AdoptionFacts {
            run_id: Uuid::nil(),
            run_status: "running".into(),
            adapter_type: "codex_local".into(),
            process_pid: None,
            process_group_id: None,
            process_pid_alive: false,
            process_group_alive: false,
        };
        let decision = classify_adoption_candidate(candidate, facts, false);
        assert_eq!(decision.classification, RunClassification::Lost);
        assert_eq!(decision.reason, "missing_process_metadata");
    }

    #[test]
    fn classify_adopted_when_pid_alive() {
        let candidate = HotRestartIntentRun {
            run_id: Uuid::new_v4().to_string(),
            company_id: Uuid::new_v4().to_string(),
            agent_id: Uuid::new_v4().to_string(),
            adapter_type: "codex_local".into(),
            status: "running".into(),
            process_pid: Some(123),
            process_group_id: None,
            issue_id: None,
        };
        let facts = AdoptionFacts {
            run_id: Uuid::nil(),
            run_status: "running".into(),
            adapter_type: "codex_local".into(),
            process_pid: Some(123),
            process_group_id: None,
            process_pid_alive: true,
            process_group_alive: false,
        };
        let decision = classify_adoption_candidate(candidate, facts, false);
        assert_eq!(decision.classification, RunClassification::Adopted);
        assert_eq!(decision.reason, "process_pid_alive");
    }

    #[test]
    fn classify_skipped_when_drain_required() {
        let candidate = HotRestartIntentRun {
            run_id: Uuid::new_v4().to_string(),
            company_id: Uuid::new_v4().to_string(),
            agent_id: Uuid::new_v4().to_string(),
            adapter_type: "codex_local".into(),
            status: "running".into(),
            process_pid: Some(1),
            process_group_id: None,
            issue_id: None,
        };
        let facts = AdoptionFacts {
            run_id: Uuid::nil(),
            run_status: "running".into(),
            adapter_type: "codex_local".into(),
            process_pid: Some(1),
            process_group_id: None,
            process_pid_alive: true,
            process_group_alive: false,
        };
        let decision = classify_adoption_candidate(candidate, facts, true);
        assert_eq!(decision.classification, RunClassification::Skipped);
        assert_eq!(decision.reason, "drain_required");
    }

    #[test]
    fn classify_finalized_when_status_not_running() {
        let candidate = HotRestartIntentRun {
            run_id: Uuid::new_v4().to_string(),
            company_id: Uuid::new_v4().to_string(),
            agent_id: Uuid::new_v4().to_string(),
            adapter_type: "codex_local".into(),
            status: "failed".into(),
            process_pid: Some(1),
            process_group_id: None,
            issue_id: None,
        };
        let facts = AdoptionFacts {
            run_id: Uuid::nil(),
            run_status: "failed".into(),
            adapter_type: "codex_local".into(),
            process_pid: Some(1),
            process_group_id: None,
            process_pid_alive: true,
            process_group_alive: false,
        };
        let decision = classify_adoption_candidate(candidate, facts, false);
        assert_eq!(decision.classification, RunClassification::FinalizedWhileDown);
        assert_eq!(decision.reason, "run_status_failed");
    }
}