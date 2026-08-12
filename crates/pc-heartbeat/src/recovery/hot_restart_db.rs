#![forbid(unsafe_code)]
//! 热重启的 DB I/O glue。
//!
//! 负责：
//! - 在旧 server 退出前从 heartbeat_runs 投影 running runs + adapter_type，
//!   构造 shutdown snapshot 并通过 pc-hot-restart 写入实例 + 共享 marker。
//! - 在新 server 启动时按 snapshot 投影仍存在的 runs，按进程存活状况分类，
//!   并把 adoption 元数据写回 result_json。

use pc_core::Timestamp;
use pc_hot_restart::{
    read_hot_restart_intent, remove_hot_restart_intent, write_hot_restart_intent,
    write_hot_restart_report, write_hot_restart_shutdown_snapshot, HotRestartError,
    HotRestartIntent, HotRestartIntentInput, HotRestartPaths, HotRestartReport,
    ShutdownSignal,
};
use pc_repos::Db;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;
use uuid::Uuid;

use crate::recovery::hot_restart::{
    classify_adoption_candidate, decide_prepare_shutdown, payload_to_object,
    resolve_missing_run_ids, run_to_intent_run, AdoptionCandidate, AdoptionFacts,
    PrepareShutdownDecision,
};

/// prepare_shutdown + write_snapshot 的组合入口。
pub async fn prepare_shutdown_and_snapshot(
    db: &Db,
    paths: &HotRestartPaths,
    current_pid: i32,
    signal: ShutdownSignal,
    captured_at: Option<String>,
    server_version: Option<String>,
) -> Result<PrepareShutdownOutcome, HotRestartError> {
    let decision = match read_hot_restart_intent(paths).await {
        Ok(Some(intent)) => decide_prepare_shutdown(Some(&intent), current_pid),
        Ok(None) => decide_prepare_shutdown(None, current_pid),
        Err(_error) => PrepareShutdownDecision::ReadError { skip_drain: false },
    };
    if !matches!(decision, PrepareShutdownDecision::HotRestart { .. }) {
        return Ok(PrepareShutdownOutcome {
            decision,
            active_run_ids: Vec::new(),
            intent: None,
        });
    }
    let mut intent = read_hot_restart_intent(paths)
        .await?
        .expect("hot_restart decision requires intent");
    if intent.previous_server_version.is_none() {
        intent.previous_server_version = server_version;
    }
    let runs = pc_repos::heartbeat::HeartbeatRepo::new(db)
        .list_running_with_adapter()
        .await
        .map_err(|error| HotRestartError::Io(std::io::Error::other(format!("heartbeat list: {error}"))))?;
    let active_runs: Vec<_> = runs.iter().map(run_to_intent_run).collect();
    let active_run_ids: Vec<String> = active_runs.iter().map(|run| run.run_id.clone()).collect();
    // 不覆盖 intent.preflight_active_run_ids：reconcile 阶段用它去 DB 找已 finalized 的 run
    // （Node prepareHotRestartShutdown 也不会覆盖）
    let updated = write_hot_restart_shutdown_snapshot(
        paths,
        &intent,
        signal,
        active_runs,
        captured_at,
    )
    .await?;
    let active_run_ids_from_snapshot: Vec<String> = updated
        .shutdown_snapshot
        .as_ref()
        .map(|snapshot| snapshot.active_runs.iter().map(|run| run.run_id.clone()).collect())
        .unwrap_or_default();
    Ok(PrepareShutdownOutcome {
        decision: PrepareShutdownDecision::HotRestart {
            skip_drain: true,
            active_run_ids,
        },
        active_run_ids: active_run_ids_from_snapshot,
        intent: Some(updated),
    })
}

/// prepare_shutdown 的返回结构。
#[derive(Debug, Clone)]
pub struct PrepareShutdownOutcome {
    pub decision: PrepareShutdownDecision,
    pub active_run_ids: Vec<String>,
    pub intent: Option<HotRestartIntent>,
}

/// reconcile 阶段的 outcome，携带每 run 的最终分类。
#[derive(Debug, Clone)]
pub struct ReconcileOutcome {
    pub report: HotRestartReport,
    pub adopted: Vec<AdoptionCandidate>,
    pub lost: Vec<AdoptionCandidate>,
    pub finalized_while_down: Vec<AdoptionCandidate>,
    pub skipped: Vec<AdoptionCandidate>,
    pub finalized_while_down_missing: Vec<String>,
}

/// reconcile_adoption 的完整 DB + 文件 IO 实现。
#[allow(clippy::too_many_arguments)]
pub async fn reconcile_adoption(
    db: &Db,
    paths: &HotRestartPaths,
    now: Timestamp,
    new_server_pid: i32,
    new_server_version: &str,
    previous_server_version: Option<String>,
) -> Result<Option<ReconcileOutcome>, HotRestartError> {
    let Some(intent) = read_hot_restart_intent(paths).await? else {
        return Ok(None);
    };
    let snapshot_run_ids: Vec<String> = intent
        .shutdown_snapshot
        .as_ref()
        .map(|snapshot| snapshot.active_runs.iter().map(|run| run.run_id.clone()).collect())
        .unwrap_or_default();
    let missing_run_ids = resolve_missing_run_ids(&intent);
    let snapshot_run_ids: Vec<String> = intent
        .shutdown_snapshot
        .as_ref()
        .map(|snapshot| snapshot.active_runs.iter().map(|run| run.run_id.clone()).collect())
        .unwrap_or_default();
    let all_run_ids: std::collections::HashSet<String> = snapshot_run_ids
        .iter()
        .cloned()
        .chain(missing_run_ids.iter().cloned())
        .collect();
    let repo = pc_repos::heartbeat::HeartbeatRepo::new(db);
    let mut current_rows: std::collections::HashMap<String, pc_repos::heartbeat::HeartbeatRow> =
        std::collections::HashMap::new();
    for run_id_str in &all_run_ids {
        if let Ok(uuid) = Uuid::parse_str(run_id_str) {
            if let Some(row) = repo.get(uuid).await.map_err(map_sql)? {
                current_rows.insert(row.id.to_string(), row);
            }
        }
    }
    let mut candidates: Vec<AdoptionCandidate> = Vec::new();
    let mut finalized_while_down_missing: Vec<String> = Vec::new();
    for run_id in &missing_run_ids {
        match current_rows.get(run_id) {
            None => finalized_while_down_missing.push(run_id.clone()),
            Some(row) => {
                let adapter_type = lookup_agent_adapter_type(db, row.agent_id).await.unwrap_or_else(|| "unknown".into());
                let candidate = build_candidate_from_row(row, adapter_type);
                let facts = facts_for_row(row, false, false);
                candidates.push(classify_adoption_candidate(
                    candidate,
                    facts,
                    intent.drain_required,
                ));
            }
        }
    }
    for snapshot_run in intent
        .shutdown_snapshot
        .as_ref()
        .map(|snapshot| snapshot.active_runs.clone())
        .unwrap_or_default()
    {
        let row = current_rows.remove(&snapshot_run.run_id);
        let row = match row {
            Some(row) => row,
            None => {
                candidates.push(classify_adoption_candidate(
                    snapshot_run.clone(),
                    AdoptionFacts {
                        run_id: Uuid::nil(),
                        run_status: "missing".into(),
                        adapter_type: snapshot_run.adapter_type.clone(),
                        process_pid: snapshot_run.process_pid,
                        process_group_id: snapshot_run.process_group_id,
                        process_pid_alive: false,
                        process_group_alive: false,
                    },
                    intent.drain_required,
                ));
                continue;
            }
        };
        let facts = build_facts(&snapshot_run, &row).await;
        candidates.push(classify_adoption_candidate(
            snapshot_run,
            facts,
            intent.drain_required,
        ));
    }
    let completed_at = now.as_datetime().to_rfc3339();
    let report = crate::recovery::hot_restart::build_report(
        &intent,
        previous_server_version.clone(),
        new_server_pid,
        new_server_version,
        &completed_at,
        candidates,
        missing_run_ids,
        finalized_while_down_missing.clone(),
    );
    let mut adopted = Vec::new();
    let mut lost = Vec::new();
    let mut finalized_while_down = Vec::new();
    let mut skipped = Vec::new();
    for run in &report.runs {
        let run_id = Uuid::parse_str(&run.run.run_id).map_err(|error| HotRestartError::Io(std::io::Error::other(format!("uuid: {error}"))))?;
        let company_id = Uuid::parse_str(&run.run.company_id).map_err(|error| HotRestartError::Io(std::io::Error::other(format!("uuid: {error}"))))?;
        let facts = AdoptionFacts {
            run_id,
            run_status: run.run.status.clone(),
            adapter_type: run.run.adapter_type.clone(),
            process_pid: run.run.process_pid,
            process_group_id: run.run.process_group_id,
            process_pid_alive: false,
            process_group_alive: false,
        };
        let candidate = AdoptionCandidate {
            run_id,
            company_id,
            agent_id: Uuid::parse_str(&run.run.agent_id).unwrap_or(Uuid::nil()),
            adapter_type: run.run.adapter_type.clone(),
            run_status: run.run.status.clone(),
            process_pid: run.run.process_pid,
            process_group_id: run.run.process_group_id,
            classification: parse_classification(&run.classification),
            reason: run.reason.clone(),
        };
        if candidate.classification == pc_hot_restart::HotRestartRunClassification::Adopted {
            repo.merge_adoption_result_json(
                company_id,
                run_id,
                intent.previous_server_pid,
                new_server_pid,
                previous_server_version.as_deref(),
                new_server_version,
                candidate.process_pid,
                candidate.process_group_id,
                now,
            )
            .await
            .map_err(map_sql)?;
            adopted.push(candidate);
        } else if candidate.classification == pc_hot_restart::HotRestartRunClassification::Lost {
            lost.push(candidate);
        } else if candidate.classification == pc_hot_restart::types::HotRestartRunClassification::FinalizedWhileDown {
            finalized_while_down.push(candidate);
        } else {
            skipped.push(candidate);
        }
    }
    write_hot_restart_report(paths, &report).await?;
    remove_hot_restart_intent(paths, Some(&intent)).await?;
    Ok(Some(ReconcileOutcome {
        report,
        adopted,
        lost,
        finalized_while_down,
        skipped,
        finalized_while_down_missing,
    }))
}

fn build_candidate_from_row(
    row: &pc_repos::heartbeat::HeartbeatRow,
    adapter_type: String,
) -> pc_hot_restart::HotRestartIntentRun {
    pc_hot_restart::HotRestartIntentRun {
        run_id: row.id.to_string(),
        company_id: row.company_id.to_string(),
        agent_id: row.agent_id.to_string(),
        adapter_type,
        status: row.status.clone(),
        process_pid: row.process_pid,
        process_group_id: row.process_group_id,
        issue_id: row
            .context_snapshot
            .as_ref()
            .and_then(|value| value.get("issueId"))
            .and_then(|value| value.as_str())
            .map(ToOwned::to_owned),
    }
}

async fn build_facts(
    snapshot_run: &pc_hot_restart::HotRestartIntentRun,
    row: &pc_repos::heartbeat::HeartbeatRow,
) -> AdoptionFacts {
    let process_pid = row.process_pid.or(snapshot_run.process_pid);
    let process_group_id = row.process_group_id.or(snapshot_run.process_group_id);
    let process_pid_alive = match process_pid {
        Some(pid) if pid > 0 => process_alive(pid).await,
        _ => false,
    };
    let process_group_alive = match process_group_id {
        Some(gid) if gid > 0 => process_group_alive(gid).await,
        _ => false,
    };
    AdoptionFacts {
        run_id: row.id,
        run_status: row.status.clone(),
        adapter_type: snapshot_run.adapter_type.clone(),
        process_pid,
        process_group_id,
        process_pid_alive,
        process_group_alive,
    }
}

async fn lookup_agent_adapter_type(db: &Db, agent_id: Uuid) -> Option<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT adapter_type FROM agents WHERE id = $1")
        .bind(agent_id)
        .fetch_optional(db.pool())
        .await
        .ok()
        .flatten();
    row.map(|(adapter_type,)| adapter_type)
}

fn parse_classification(value: &str) -> pc_hot_restart::types::HotRestartRunClassification {
    match value {
        "adopted" => pc_hot_restart::types::HotRestartRunClassification::Adopted,
        "finalized_while_down" => pc_hot_restart::types::HotRestartRunClassification::FinalizedWhileDown,
        "lost" => pc_hot_restart::types::HotRestartRunClassification::Lost,
        _ => pc_hot_restart::types::HotRestartRunClassification::Skipped,
    }
}

fn map_sql(error: sqlx::Error) -> HotRestartError {
    HotRestartError::Io(std::io::Error::other(format!("sqlx: {error}")))
}

/// 构造缺失行的 AdoptionFacts（missing_run_ids 分支专用，无 row 数据时硬编码为非存活）。
fn facts_for_row(row: &pc_repos::heartbeat::HeartbeatRow, process_pid_alive: bool, process_group_alive: bool) -> AdoptionFacts {
    AdoptionFacts {
        run_id: row.id,
        run_status: row.status.clone(),
        adapter_type: String::new(),
        process_pid: row.process_pid,
        process_group_id: row.process_group_id,
        process_pid_alive,
        process_group_alive,
    }
}

async fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .await
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let _ = pid;
        false
    }
}

async fn process_group_alive(gid: i32) -> bool {
    if gid <= 0 {
        return false;
    }
    #[cfg(unix)]
    {
    Command::new("kill")
        .args(["-0", &format!("-{gid}")])
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        let _ = gid;
        false
    }
}

/// 写一个测试用 intent（不依赖 OS 进程探测）。
pub async fn write_test_intent(
    paths: &HotRestartPaths,
    input: HotRestartIntentInput,
) -> Result<HotRestartIntent, HotRestartError> {
    write_hot_restart_intent(paths, input).await
}

#[allow(dead_code)]
fn _unused_path_helper(_path: &Path) -> Duration {
    Duration::from_secs(0)
}

#[allow(dead_code)]
fn _payload_sink() -> Value {
    payload_to_object(None)
}