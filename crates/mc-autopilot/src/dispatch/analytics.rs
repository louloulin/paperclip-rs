//! 派发/运行的分析位（analytics）。
//!
//! - **写者**：M5-4。
//! - **上游**：`service/autopilot.go` 的分析 92 行。
//! - **归属说明**：`docs/44` §1.2 已判定 M5 的 "analytics" 落在 quota/usage（M5-1）与调度 job
//!   （M5-8）上 ⇒ **不建 `mc-analytics` crate**；本文件只放派发路径上必须记的那几个计数/时间戳。
//!
//! # 本地形态（偏差已登记）
//!
//! 上游这 92 行全是 PostHog（`obsmetrics.RecordEvent` + `analytics.AutopilotRun*`），本地**没有**
//! 分析平面：既没有 `mc-analytics` crate，也没有事件出口（`docs/44` §1.2 / §8）。
//! 于是这里落成 **`tracing` 结构化事件**（`target = "mc_autopilot::dispatch::analytics"`），
//! 字段名与上游 PostHog 属性对齐，供将来的分析接线直接复用；`error_type` 这类上游的私有分类
//! （`autopilotErrorType`）**不猜**，只带原始 reason。
//!
//! 上游逐个逐字对应的函数：
//! `captureAutopilotRunStarted`1589 / `captureAutopilotRunCompleted`1604 /
//! `captureAutopilotRunFailed`1620 / `captureIssueCreatedFromAutopilot`1570 /
//! `autopilotRunDurationMS`（`completed_at - triggered_at`，负值归 0）。

use mc_core::autopilot::RunSource;
use mc_repos::autopilot::{AutopilotRow, AutopilotRunRow};

/// 事件字段：run 的时长（ms）。无效组合（还没终态 / 时钟回拨）归 0，与上游一致。
#[must_use]
pub(crate) fn run_duration_ms(run: &AutopilotRunRow) -> i64 {
    let Some(completed_at) = run.completed_at else {
        return 0;
    };
    let delta = completed_at - run.triggered_at;
    if delta.num_milliseconds() < 0 {
        0
    } else {
        delta.num_milliseconds()
    }
}

/// `captureAutopilotRunStarted`：run 建出来、副作用开工前记一次。
pub(crate) fn run_started(autopilot: &AutopilotRow, run: &AutopilotRunRow, source: RunSource) {
    tracing::info!(
        target: "mc_autopilot::dispatch::analytics",
        event = "autopilot_run_started",
        workspace_id = %autopilot.workspace_id,
        autopilot_id = %autopilot.id,
        run_id = %run.id,
        trigger_source = source.as_str(),
        execution_mode = %autopilot.execution_mode,
        assignee_type = %autopilot.assignee_type,
        "autopilot run started"
    );
}

/// `captureAutopilotRunCompleted`。
pub(crate) fn run_completed(autopilot: &AutopilotRow, run: &AutopilotRunRow) {
    tracing::info!(
        target: "mc_autopilot::dispatch::analytics",
        event = "autopilot_run_completed",
        workspace_id = %autopilot.workspace_id,
        autopilot_id = %autopilot.id,
        run_id = %run.id,
        trigger_source = %run.source,
        duration_ms = run_duration_ms(run),
        "autopilot run completed"
    );
}

/// `captureIssueCreatedFromAutopilot`：create_issue 线建出 issue 后记一次
/// （上游把执行 agent（squad leader）作为 `agent_id`，好让 per-agent 计数与 daemon 上报对齐）。
pub(crate) fn issue_created_from_autopilot(
    autopilot: &AutopilotRow,
    run: &AutopilotRunRow,
    issue_id: uuid::Uuid,
    leader_id: uuid::Uuid,
) {
    tracing::info!(
        target: "mc_autopilot::dispatch::analytics",
        event = "issue_created",
        source = "autopilot",
        workspace_id = %autopilot.workspace_id,
        autopilot_id = %autopilot.id,
        run_id = %run.id,
        issue_id = %issue_id,
        agent_id = %leader_id,
        "issue created from autopilot"
    );
}

/// `captureAutopilotRunFailed`：`reason` 为空时上游写 `"unknown"`。
pub(crate) fn run_failed(
    autopilot: &AutopilotRow,
    run: &AutopilotRunRow,
    source: RunSource,
    reason: &str,
) {
    let reason = if reason.is_empty() { "unknown" } else { reason };
    tracing::warn!(
        target: "mc_autopilot::dispatch::analytics",
        event = "autopilot_run_failed",
        workspace_id = %autopilot.workspace_id,
        autopilot_id = %autopilot.id,
        run_id = %run.id,
        trigger_source = source.as_str(),
        reason,
        duration_ms = run_duration_ms(run),
        "autopilot run failed"
    );
}

/// 跳过（上游没有独立的 analytics 事件：`recordSkippedRun` 只发 WS + `last_run_at`，
/// 跳过率由 `reason_code` 聚合）。本地记一条结构化日志便于运维排查。
pub(crate) fn run_skipped(
    autopilot: &AutopilotRow,
    run: &AutopilotRunRow,
    source: RunSource,
    reason: &str,
) {
    tracing::info!(
        target: "mc_autopilot::dispatch::analytics",
        event = "autopilot_run_skipped",
        workspace_id = %autopilot.workspace_id,
        autopilot_id = %autopilot.id,
        run_id = %run.id,
        trigger_source = source.as_str(),
        reason,
        "autopilot run skipped"
    );
}
