//! R650: routine activity gate 评估。
//!
//! 与 Node `services/routines.ts::evaluateActivityGate` 1:1 对齐（核心语义）。
//!
//! 语义：当 `routine.activity_gate_policy == "require_external_activity"` 时，
//! routine 在被自动调度之前必须看到"外部活动"（即：自上次 routine run
//! 之后 activity_log 中有用户/agent 留下的"有意义"操作）。
//!
//! 排除：
//! - `issue.read_marked` / `issue.read_unmarked` / `issue.inbox_archived` /
//!   `issue.inbox_unarchived`（用户只是 inbox 互动，没有真实工作）
//! - `routine-scheduler` 自己产生的活动（防止自循环）
//!
//! 注意：完整 Node 实现的 project scope SQL 非常复杂（10+ 个 entity 子查询），
//! 本轮先实现 `global` scope（company-wide）的核心语义。`project` scope 留作
//! 后续轮次（M21 收口之后）。

use chrono::{DateTime, Utc};
use pc_repos::routine::RoutineRow;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Activity gate 评估结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityGateVerdict {
    /// 是否应该 fire。
    pub fire: bool,
    /// 窗口起点（即上次 routine run 触发时间；首次为 null）。
    pub window_start: Option<DateTime<Utc>>,
    /// 匹配到的活动 ID（仅在 fire=true 时有意义）。
    pub matched_activity_id: Option<Uuid>,
    /// 评估时使用的 scope（global/project）。
    pub scope: ActivityGateScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityGateScope {
    /// 全公司范围活动（默认）。
    Global,
    /// 项目范围活动。
    Project,
}

/// 与 Node `ACTIVITY_GATE_IGNORED_ACTIONS` 1:1 对齐。
const ACTIVITY_GATE_IGNORED_ACTIONS: &[&str] = &[
    "issue.read_marked",
    "issue.read_unmarked",
    "issue.inbox_archived",
    "issue.inbox_unarchived",
];

const ROUTINE_SCHEDULER_ACTOR_ID: &str = "routine-scheduler";

/// 决策入口：根据 routine 的 activity_gate_policy 决定要不要 fire。
///
/// - `require_external_activity`: 返回 [`ActivityGateVerdict`]，
///   调用方需检查 `fire` 字段。
/// - `always` 或其他值: 直接 fire（默认行为，不需 gate）。
/// - `none`/`disabled`: 等价于 `always`。
pub async fn evaluate_activity_gate(
    pool: &PgPool,
    routine: &RoutineRow,
    now: DateTime<Utc>,
) -> ActivityGateVerdict {
    let policy = routine.activity_gate_policy.as_str();
    if policy != "require_external_activity" {
        // 默认 always。
        return ActivityGateVerdict {
            fire: true,
            window_start: None,
            matched_activity_id: None,
            scope: ActivityGateScope::Global,
        };
    }

    let scope = match routine.activity_gate_scope.as_str() {
        "project" => ActivityGateScope::Project,
        _ => ActivityGateScope::Global,
    };

    let window_start = last_dispatched_triggered_at(pool, routine).await;
    let Some(window_start) = window_start else {
        // 从来没有 dispatch 过 → fire=true（首次）。
        return ActivityGateVerdict {
            fire: true,
            window_start: None,
            matched_activity_id: None,
            scope,
        };
    };

    let matched = find_external_activity(
        pool,
        routine,
        window_start,
        now,
        scope,
    )
    .await;
    match matched {
        Some(id) => ActivityGateVerdict {
            fire: true,
            window_start: Some(window_start),
            matched_activity_id: Some(id),
            scope,
        },
        None => ActivityGateVerdict {
            fire: false,
            window_start: Some(window_start),
            matched_activity_id: None,
            scope,
        },
    }
}

/// 查找 routine 上次 non-skipped / non-coalesced 的 run 触发时间。
///
/// 与 Node `lastDispatchedRun` 1:1：status 不在 ('skipped', 'coalesced') 之列。
async fn last_dispatched_triggered_at(pool: &PgPool, routine: &RoutineRow) -> Option<DateTime<Utc>> {
    sqlx::query_as::<_, (DateTime<Utc>,)>(
        r#"SELECT triggered_at FROM routine_runs
         WHERE company_id = $1 AND routine_id = $2
           AND status NOT IN ('skipped', 'coalesced')
         ORDER BY triggered_at DESC, id DESC
         LIMIT 1"#,
    )
    .bind(routine.company_id)
    .bind(routine.id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .map(|(ts,)| ts)
}

/// 在 [window_start, now] 区间内查找"外部活动"。
///
/// 排除：
/// - `ACTIVITY_GATE_IGNORED_ACTIONS`
/// - 自循环：`actor_id = 'routine-scheduler'` 且 details 中含相同 routineId
///   或 entity_type='routine' 且 entity_id=routine.id
///
/// 注意：完整 Node 实现包含 project scope 的 10+ 子查询；
/// 本轮先实现 global scope 的核心语义。project scope 留作后续轮次。


async fn find_external_activity(
    pool: &PgPool,
    routine: &RoutineRow,
    window_start: DateTime<Utc>,
    now: DateTime<Utc>,
    scope: ActivityGateScope,
) -> Option<Uuid> {
    // R654: project scope needs routine.project_id; missing -> never fire.
    if matches!(scope, ActivityGateScope::Project) && routine.project_id.is_none() {
        return None;
    }

    let row: Option<(Uuid,)> = match scope {
        ActivityGateScope::Global => {
sqlx::query_as(
                r#"SELECT id FROM activity_log
                 WHERE company_id = $1
                   AND created_at > $2 AND created_at <= $3
                   AND action <> ALL($4::text[])
                   AND NOT (
                     actor_id = 'routine-scheduler'
                     AND (
                       details ->> 'routineId' = $5
                       OR (entity_type = 'routine' AND entity_id = $5)
                     )
                   )
                 ORDER BY created_at ASC, id ASC
                 LIMIT 1"#,
            )
            .bind(routine.company_id)
            .bind(window_start)
            .bind(now)
            .bind(ACTIVITY_GATE_IGNORED_ACTIONS)
            .bind(routine.id.to_string())
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
        }
        ActivityGateScope::Project => {
            // 6 OR EXISTS sub-clauses mirror Node routines.ts:1241-1290.
            let project_id = routine.project_id.expect("pre-check");
            sqlx::query_as(
                r#"SELECT id FROM activity_log
                 WHERE company_id = $1
                   AND created_at > $2 AND created_at <= $3
                   AND action <> ALL($4::text[])
                   AND NOT (
                     actor_id = 'routine-scheduler'
                     AND (
                       details ->> 'routineId' = $5
                       OR (entity_type = 'routine' AND entity_id = $5)
                     )
                   )
                   AND (
                     (entity_type = 'project' AND entity_id = $6)
                     OR (details ->> 'projectId') = $6
                     OR EXISTS (
                       SELECT 1 FROM issues activity_issue
                       WHERE activity_issue.company_id = $1
                         AND activity_issue.project_id = $7
                         AND activity_issue.id::text = entity_id
                         AND entity_type = 'issue'
                     )
                     OR EXISTS (
                       SELECT 1 FROM heartbeat_runs activity_run
                       INNER JOIN issues run_issue
                         ON run_issue.company_id = $1
                         AND run_issue.id::text = activity_run.context_snapshot ->> 'issueId'
                       WHERE activity_run.company_id = $1
                         AND activity_run.id = activity_log.run_id
                         AND run_issue.project_id = $7
                     )
                     OR EXISTS (
                       SELECT 1 FROM routines activity_routine
                       WHERE activity_routine.company_id = $1
                         AND activity_routine.project_id = $7
                         AND activity_routine.id::text = entity_id
                         AND entity_type = 'routine'
                     )
                     OR EXISTS (
                       SELECT 1 FROM routine_runs activity_routine_run
                       INNER JOIN routines activity_routine
                         ON activity_routine.company_id = $1
                         AND activity_routine.id = activity_routine_run.routine_id
                       WHERE activity_routine_run.company_id = $1
                         AND activity_routine_run.id::text = entity_id
                         AND activity_routine.project_id = $7
                         AND entity_type = 'routine_run'
                     )
                   )
                 ORDER BY created_at ASC, id ASC
                 LIMIT 1"#,
            )
            .bind(routine.company_id)
            .bind(window_start)
            .bind(now)
            .bind(ACTIVITY_GATE_IGNORED_ACTIONS)
            .bind(routine.id.to_string())
            .bind(project_id.to_string())
            .bind(project_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten()
        }
    };
    row.map(|(id,)| id)
}

