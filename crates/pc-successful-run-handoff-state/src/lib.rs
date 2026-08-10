#![forbid(unsafe_code)]
//! `pc-successful-run-handoff-state` —— successful run handoff 状态 hydration。
//!
//! 对应 Node `server/src/services/successful-run-handoff-state.ts`（128 行）。
//!
//! 设计目标：1:1 复刻
//! - [`SuccessfulRunHandoffState`] —— typed DTO（与 shared `SuccessfulRunHandoffState` 1:1 对齐）
//! - [`SuccessfulRunHandoffStateKind`] —— 三种 kind：required / resolved / escalated
//! - [`hydrate_successful_run_handoff_liveness`] —— 给 state map 注入 liveness
//! - [`resolve_required_successful_run_handoff_on_valid_path`] —— 检查最新 handoff activity
//!
//! 与 Node 的差异：
//! - DB 通过 [`HandoffDataSource`] trait 注入（测试用 fake）
//! - [`HandoffActivityWriter`] trait 抽象 activity log 写入（测试用 fake）

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// Live run statuses（与 Node `SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES` 1:1 对齐）。
pub const SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES: &[&str] =
    &["queued", "running", "scheduled_retry"];

/// Live wake statuses（与 Node `SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES` 1:1 对齐）。
pub const SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES: &[&str] =
    &["queued", "deferred_issue_execution", "claimed"];

// ============================================================================
// DTO
// ============================================================================

/// Handoff state kind（与 Node `SuccessfulRunHandoffStateKind` 1:1 对齐）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessfulRunHandoffStateKind {
    #[default]
    Required,
    Resolved,
    Escalated,
}

/// Successful run handoff state（与 Node `SuccessfulRunHandoffState` 1:1 对齐）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuccessfulRunHandoffState {
    pub state: SuccessfulRunHandoffStateKind,
    pub required: bool,
    pub has_live_continuation: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_run_id: Option<String>,
    pub source_run_id: Option<String>,
    pub corrective_run_id: Option<String>,
    pub assignee_agent_id: Option<String>,
    pub detected_progress_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
}

// ============================================================================
// Data source trait
// ============================================================================

/// 抽象 DB 数据源（与 Node 端 heartbeat_runs + agent_wakeup_requests 1:1 对齐）。
#[async_trait]
pub trait HandoffDataSource: Send + Sync {
    /// 查询 live runs：返回 `(run_id, issue_id)` 列表
    async fn live_runs(
        &self,
        company_id: &str,
        live_statuses: &[&str],
        required_issue_ids: &[String],
    ) -> Vec<(String, String)>;

    /// 查询 live wakes：返回 issue_id 列表
    async fn live_wakes(
        &self,
        company_id: &str,
        live_statuses: &[&str],
        required_issue_ids: &[String],
    ) -> Vec<String>;

    /// 查询 latest handoff activity
    async fn latest_handoff_activity(
        &self,
        company_id: &str,
        issue_id: &str,
        actions: &[&str],
    ) -> Option<HandoffActivityRow>;

    /// 写 handoff resolved activity
    async fn write_handoff_resolved_activity(
        &self,
        record: HandoffActivityWrite,
    ) -> Result<(), String>;
}

/// Handoff activity 行（与 Node `activityLog` 查询字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct HandoffActivityRow {
    pub action: String,
    pub run_id: Option<String>,
    pub details: Option<serde_json::Value>,
}

/// Handoff activity 写入（与 Node `logActivity` 输入 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct HandoffActivityWrite {
    pub company_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub agent_id: Option<String>,
    pub run_id: Option<String>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub details: serde_json::Value,
}

// ============================================================================
// Pure helpers
// ============================================================================

/// 从 JSON record 提取 source run id（与 Node 1:1 对齐）。
///
/// 优先级：`sourceRunId` → `source_run_id` → `resumeFromRunId`
pub fn extract_source_run_id(details: &serde_json::Value, fallback: Option<&str>) -> Option<String> {
    let obj = details.as_object()?;
    for key in ["sourceRunId", "source_run_id", "resumeFromRunId"] {
        if let Some(v) = obj.get(key).and_then(|x| x.as_str()) {
            let t = v.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    fallback.map(|s| s.to_string())
}

// ============================================================================
// Hydrate liveness
// ============================================================================

/// 给 state map 注入 liveness（与 Node `hydrateSuccessfulRunHandoffLiveness` 1:1 对齐）。
pub async fn hydrate_successful_run_handoff_liveness(
    data: &dyn HandoffDataSource,
    company_id: &str,
    mut states: HashMap<String, SuccessfulRunHandoffState>,
) -> HashMap<String, SuccessfulRunHandoffState> {
    // 收集所有 "required" 状态的 issue_id
    let required_issue_ids: Vec<String> = states
        .iter()
        .filter(|(_, s)| s.state == SuccessfulRunHandoffStateKind::Required)
        .map(|(id, _)| id.clone())
        .collect();

    if required_issue_ids.is_empty() {
        return states;
    }

    // 并行查 live runs + live wakes
    let (active_runs, active_wakes) = tokio::join!(
        data.live_runs(
            company_id,
            SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES,
            &required_issue_ids,
        ),
        data.live_wakes(
            company_id,
            SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES,
            &required_issue_ids,
        ),
    );

    // 第一条 live run per issue
    let mut live_run_by_issue: HashMap<String, String> = HashMap::new();
    for (run_id, issue_id) in active_runs {
        live_run_by_issue.entry(issue_id).or_insert(run_id);
    }

    let live_wake_issue_ids: HashSet<String> = active_wakes.into_iter().collect();

    // 更新 states
    for issue_id in &required_issue_ids {
        if let Some(state) = states.get(issue_id) {
            let live_run_id = live_run_by_issue.get(issue_id).cloned();
            let has_live_continuation =
                live_run_id.is_some() || live_wake_issue_ids.contains(issue_id);

            let mut new_state = state.clone();
            new_state.has_live_continuation = has_live_continuation;
            if let Some(rid) = live_run_id {
                new_state.live_run_id = Some(rid);
            }
            states.insert(issue_id.clone(), new_state);
        }
    }

    states
}

// ============================================================================
// Resolve on valid path
// ============================================================================

/// Resolve required handoff on valid path（与 Node `resolveRequiredSuccessfulRunHandoffOnValidPath` 1:1 对齐）。
pub async fn resolve_required_successful_run_handoff_on_valid_path(
    data: &dyn HandoffDataSource,
    input: ResolveRequiredInput,
) -> bool {
    let actions = [
        "issue.successful_run_handoff_required",
        "issue.successful_run_handoff_resolved",
        "issue.successful_run_handoff_escalated",
    ];
    let latest = data
        .latest_handoff_activity(&input.company_id, &input.issue_id, &actions)
        .await;

    let Some(latest) = latest else {
        return false;
    };
    if latest.action != "issue.successful_run_handoff_required" {
        return false;
    }

    let details = latest
        .details
        .as_ref()
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let source_run_id = extract_source_run_id(
        &serde_json::Value::Object(details),
        latest.run_id.as_deref(),
    );

    let details_json = serde_json::json!({
        "label": "Successful run handoff continuation confirmed",
        "sourceRunId": source_run_id,
        "resolvedByRunId": input.run_id,
        "resolvedBySkipReason": input.skip_reason,
        "issue": {
            "id": input.issue_id,
            "identifier": input.issue_identifier,
        }
    });

    let record = HandoffActivityWrite {
        company_id: input.company_id,
        actor_type: "system".to_string(),
        actor_id: "heartbeat".to_string(),
        agent_id: Some(input.agent_id),
        run_id: Some(input.run_id),
        action: "issue.successful_run_handoff_resolved".to_string(),
        entity_type: "issue".to_string(),
        entity_id: input.issue_id,
        details: details_json,
    };
    if let Err(err) = data.write_handoff_resolved_activity(record).await {
        tracing::warn!(err = %err, "failed to write handoff resolved activity");
    }
    true
}

/// Resolve input（与 Node `input` 参数 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct ResolveRequiredInput {
    pub company_id: String,
    pub issue_id: String,
    pub issue_identifier: Option<String>,
    pub agent_id: String,
    pub run_id: String,
    pub skip_reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // ----- constants -----

    #[test]
    fn r719_constants_match_node() {
        assert_eq!(
            SUCCESSFUL_RUN_HANDOFF_LIVE_RUN_STATUSES,
            &["queued", "running", "scheduled_retry"]
        );
        assert_eq!(
            SUCCESSFUL_RUN_HANDOFF_LIVE_WAKE_STATUSES,
            &["queued", "deferred_issue_execution", "claimed"]
        );
    }

    // ----- extract_source_run_id -----

    #[test]
    fn r719_extract_source_run_id_camel() {
        let v = serde_json::json!({"sourceRunId": "  r-1  "});
        assert_eq!(extract_source_run_id(&v, None).as_deref(), Some("r-1"));
    }

    #[test]
    fn r719_extract_source_run_id_snake() {
        let v = serde_json::json!({"source_run_id": "r-2"});
        assert_eq!(extract_source_run_id(&v, None).as_deref(), Some("r-2"));
    }

    #[test]
    fn r719_extract_source_run_id_resume() {
        let v = serde_json::json!({"resumeFromRunId": "r-3"});
        assert_eq!(extract_source_run_id(&v, None).as_deref(), Some("r-3"));
    }

    #[test]
    fn r719_extract_source_run_id_priority() {
        // sourceRunId 优先
        let v = serde_json::json!({
            "sourceRunId": "r-camel",
            "source_run_id": "r-snake",
            "resumeFromRunId": "r-resume"
        });
        assert_eq!(extract_source_run_id(&v, None).as_deref(), Some("r-camel"));
    }

    #[test]
    fn r719_extract_source_run_id_fallback() {
        let v = serde_json::json!({});
        assert_eq!(
            extract_source_run_id(&v, Some("r-fb")).as_deref(),
            Some("r-fb")
        );
    }

    #[test]
    fn r719_extract_source_run_id_no_match() {
        let v = serde_json::json!({"other": "x"});
        assert_eq!(extract_source_run_id(&v, None), None);
    }

    // ----- hydrate -----

    #[derive(Default, Clone)]
    struct FakeData {
        runs: Arc<Mutex<Vec<(String, String)>>>, // (run_id, issue_id)
        wakes: Arc<Mutex<Vec<String>>>,
        activities: Arc<Mutex<Option<HandoffActivityRow>>>,
        writes: Arc<Mutex<Vec<HandoffActivityWrite>>>,
    }

    #[async_trait]
    impl HandoffDataSource for FakeData {
        async fn live_runs(
            &self,
            _: &str,
            _: &[&str],
            _: &[String],
        ) -> Vec<(String, String)> {
            self.runs.lock().await.clone()
        }
        async fn live_wakes(&self, _: &str, _: &[&str], _: &[String]) -> Vec<String> {
            self.wakes.lock().await.clone()
        }
        async fn latest_handoff_activity(
            &self,
            _: &str,
            _: &str,
            _: &[&str],
        ) -> Option<HandoffActivityRow> {
            self.activities.lock().await.clone()
        }
        async fn write_handoff_resolved_activity(
            &self,
            record: HandoffActivityWrite,
        ) -> Result<(), String> {
            self.writes.lock().await.push(record);
            Ok(())
        }
    }

    #[tokio::test]
    async fn r719_hydrate_no_required_states_is_noop() {
        let data = FakeData::default();
        let mut states = HashMap::new();
        states.insert(
            "i-1".into(),
            SuccessfulRunHandoffState {
                state: SuccessfulRunHandoffStateKind::Resolved,
                required: false,
                ..Default::default()
            },
        );
        let r = hydrate_successful_run_handoff_liveness(&data, "co-1", states).await;
        assert!(r.get("i-1").unwrap().live_run_id.is_none());
        assert!(!r.get("i-1").unwrap().has_live_continuation);
    }

    #[tokio::test]
    async fn r719_hydrate_injects_live_run() {
        let data = FakeData::default();
        data.runs.lock().await.push(("r-1".into(), "i-1".into()));
        let mut states = HashMap::new();
        states.insert(
            "i-1".into(),
            SuccessfulRunHandoffState {
                state: SuccessfulRunHandoffStateKind::Required,
                required: true,
                ..Default::default()
            },
        );
        let r = hydrate_successful_run_handoff_liveness(&data, "co-1", states).await;
        let s = r.get("i-1").unwrap();
        assert_eq!(s.live_run_id.as_deref(), Some("r-1"));
        assert!(s.has_live_continuation);
    }

    #[tokio::test]
    async fn r719_hydrate_injects_live_wake() {
        let data = FakeData::default();
        data.wakes.lock().await.push("i-1".into());
        let mut states = HashMap::new();
        states.insert(
            "i-1".into(),
            SuccessfulRunHandoffState {
                state: SuccessfulRunHandoffStateKind::Required,
                required: true,
                ..Default::default()
            },
        );
        let r = hydrate_successful_run_handoff_liveness(&data, "co-1", states).await;
        let s = r.get("i-1").unwrap();
        assert!(s.has_live_continuation);
        assert!(s.live_run_id.is_none());
    }

    #[tokio::test]
    async fn r719_hydrate_picks_first_run() {
        let data = FakeData::default();
        data.runs
            .lock()
            .await
            .push(("r-1".into(), "i-1".into()));
        data.runs
            .lock()
            .await
            .push(("r-2".into(), "i-1".into()));
        let mut states = HashMap::new();
        states.insert(
            "i-1".into(),
            SuccessfulRunHandoffState {
                state: SuccessfulRunHandoffStateKind::Required,
                required: true,
                ..Default::default()
            },
        );
        let r = hydrate_successful_run_handoff_liveness(&data, "co-1", states).await;
        // 第一个 run 保留
        assert_eq!(r.get("i-1").unwrap().live_run_id.as_deref(), Some("r-1"));
    }

    // ----- resolve on valid path -----

    #[tokio::test]
    async fn r719_resolve_no_activity_returns_false() {
        let data = FakeData::default();
        let r = resolve_required_successful_run_handoff_on_valid_path(
            &data,
            ResolveRequiredInput {
                company_id: "co-1".into(),
                issue_id: "i-1".into(),
                ..Default::default()
            },
        )
        .await;
        assert!(!r);
        assert!(data.writes.lock().await.is_empty());
    }

    #[tokio::test]
    async fn r719_resolve_latest_is_resolved_returns_false() {
        let data = FakeData::default();
        *data.activities.lock().await = Some(HandoffActivityRow {
            action: "issue.successful_run_handoff_resolved".into(),
            run_id: None,
            details: None,
        });
        let r = resolve_required_successful_run_handoff_on_valid_path(
            &data,
            ResolveRequiredInput {
                company_id: "co-1".into(),
                issue_id: "i-1".into(),
                ..Default::default()
            },
        )
        .await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r719_resolve_required_with_source_run_id() {
        let data = FakeData::default();
        *data.activities.lock().await = Some(HandoffActivityRow {
            action: "issue.successful_run_handoff_required".into(),
            run_id: Some("r-source".into()),
            details: Some(serde_json::json!({
                "sourceRunId": "r-explicit"
            })),
        });
        let r = resolve_required_successful_run_handoff_on_valid_path(
            &data,
            ResolveRequiredInput {
                company_id: "co-1".into(),
                issue_id: "i-1".into(),
                issue_identifier: Some("PAP-1".into()),
                agent_id: "a-1".into(),
                run_id: "r-current".into(),
                skip_reason: "no_progress".into(),
            },
        )
        .await;
        assert!(r);
        let writes = data.writes.lock().await;
        assert_eq!(writes.len(), 1);
        let w = &writes[0];
        assert_eq!(w.action, "issue.successful_run_handoff_resolved");
        assert_eq!(w.entity_type, "issue");
        assert_eq!(w.entity_id, "i-1");
        assert_eq!(w.run_id.as_deref(), Some("r-current"));
        assert_eq!(w.agent_id.as_deref(), Some("a-1"));
        assert_eq!(w.actor_type, "system");
        assert_eq!(w.actor_id, "heartbeat");
        let details = &w.details;
        assert_eq!(
            details.get("sourceRunId").and_then(|v| v.as_str()),
            Some("r-explicit")
        );
        assert_eq!(
            details.get("resolvedByRunId").and_then(|v| v.as_str()),
            Some("r-current")
        );
        assert_eq!(
            details
                .get("resolvedBySkipReason")
                .and_then(|v| v.as_str()),
            Some("no_progress")
        );
    }

    #[tokio::test]
    async fn r719_resolve_required_falls_back_to_activity_run_id() {
        let data = FakeData::default();
        *data.activities.lock().await = Some(HandoffActivityRow {
            action: "issue.successful_run_handoff_required".into(),
            run_id: Some("r-from-row".into()),
            details: Some(serde_json::json!({})),
        });
        let r = resolve_required_successful_run_handoff_on_valid_path(
            &data,
            ResolveRequiredInput {
                company_id: "co-1".into(),
                issue_id: "i-1".into(),
                run_id: "r-current".into(),
                agent_id: "a-1".into(),
                ..Default::default()
            },
        )
        .await;
        assert!(r);
        let writes = data.writes.lock().await;
        let details = &writes[0].details;
        assert_eq!(
            details.get("sourceRunId").and_then(|v| v.as_str()),
            Some("r-from-row")
        );
    }

    // ----- send/sync -----

    #[test]
    fn r719_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SuccessfulRunHandoffState>();
        assert_send_sync::<SuccessfulRunHandoffStateKind>();
        assert_send_sync::<HandoffDataSourceBox>();
    }
}

pub type HandoffDataSourceBox = Box<dyn HandoffDataSource>;
