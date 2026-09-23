//! workspace 级 wakeup 列表与摘要（上游 `db/queries/workspace_wakeup.sql:1` +
//! `wakeup.sql:22` 的 `ListWorkspaceWakeupSummaryRows`）。
//!
//! 这两条是**整屏 SQL**（上游把分页、计数、筛选选项、可见性掩码、最近 run 投影都收进一条 CTE），
//! 所以本文件的做法是**逐字搬运**：SQL 文本与上游一一对应，只把 sqlc 的 `@name` 换成 `$n`，
//! 不重排 CTE、不改别名。返回值是**已经成形的 JSON**（上游 `jsonb_build_object` 的结果直接写回
//! 响应体，handler 只做 `writeJSON(200, json.RawMessage(result))`）⇒ 本仓也用
//! [`serde_json::Value`] 直通，避免在 Rust 里重建一层会漂移的 DTO。
//!
//! | 上游 query | 行 | 本文件 |
//! | --- | ---: | --- |
//! | `ListWorkspaceWakeups` | `workspace_wakeup.sql:1` | [`list_workspace_wakeups`] |
//! | `ListWorkspaceWakeupSummaryRows` | `wakeup.sql:22` | [`list_workspace_wakeup_summaries`] |
//!
//! # 视觉口径（勿"顺手修正"）
//!
//! - `scope` 几何：`(enabled AND NOT issue_closed) OR active_runs>0` ⇒ `active`；
//!   `NOT issue_closed AND disabled_at IS NOT NULL` ⇒ `disabled`；其余 `ended`。
//!   即「已停用但 issue 还开着」= `disabled`，而「issue 关了」一律 `ended`（不看 enabled）。
//! - `can_manage = created_by = 调用者 OR 调用者是 admin`：**权限标记**，不是过滤条件。
//! - `task` 投影优先取活跃 run（`running/waiting_local_directory/dispatched` 排前面），
//!   没有活跃 run 才退到最近一条历史 run。
//! - 摘要每 issue **最多 3 条预览**（`rank<=3`）但 `active_count`/`event_count` 是**全量计数**。

use serde_json::Value as JsonValue;
use sqlx::PgPool;
use uuid::Uuid;

use super::{map_wakeup_err, WakeupSummaryRow};
use crate::Result;

/// `ListWorkspaceWakeups` 的入参（对应上游 `db.ListWorkspaceWakeupsParams`）。
#[derive(Debug, Clone)]
pub struct WorkspaceWakeupQuery {
    /// 目标 workspace。
    pub workspace_id: Uuid,
    /// 调用者可见的 agent 集合（掩码用）。
    pub agent_ids: Vec<Uuid>,
    /// 「发起人」成员 id（管理标记用，非成员时 `None`）。
    pub member_id: Option<Uuid>,
    /// 调用者是否 owner/admin。
    pub is_admin: bool,
    /// `active | all | disabled | ended`（handler 已归一化，默认 `active`）。
    pub scope: String,
    /// `all | event | at | recurring`（handler 已归一化，默认 `all`）。
    pub kind: String,
    /// agent 过滤（空串 = 不过滤）。
    pub agent_id: String,
    /// 搜索串（已 trim，空串 = 不过滤）。
    pub search: String,
    /// 页大小（1..=100，handler 已校验）。
    pub page_limit: i32,
    /// 偏移（0..=1000000，handler 已校验）。
    pub page_offset: i32,
}

/// `ListWorkspaceWakeups`：`items` / `total` / `counts` / `agents` 一次成型。
pub async fn list_workspace_wakeups(pool: &PgPool, q: &WorkspaceWakeupQuery) -> Result<JsonValue> {
    sqlx::query_scalar::<_, JsonValue>(WORKSPACE_WAKEUPS_SQL)
        .bind(q.workspace_id)
        .bind(&q.agent_ids)
        .bind(q.member_id)
        .bind(q.is_admin)
        .bind(&q.scope)
        .bind(&q.kind)
        .bind(&q.agent_id)
        .bind(&q.search)
        .bind(q.page_limit)
        .bind(q.page_offset)
        .fetch_one(pool)
        .await
        .map_err(map_wakeup_err)
}

/// `ListWorkspaceWakeupSummaryRows`：每 issue 最多 3 条预览 + 全量计数。
pub async fn list_workspace_wakeup_summaries(
    pool: &PgPool,
    workspace_id: Uuid,
    agent_ids: &[Uuid],
) -> Result<Vec<WakeupSummaryRow>> {
    sqlx::query_as::<_, WakeupSummaryRow>(SUMMARIES_SQL)
        .bind(workspace_id)
        .bind(agent_ids)
        .fetch_all(pool)
        .await
        .map_err(map_wakeup_err)
}

const WORKSPACE_WAKEUPS_SQL: &str = "\
WITH base AS MATERIALIZED ( \
 SELECT w.id,w.issue_id,i.title AS issue_title,ws.issue_prefix||'-'||i.number AS issue_identifier, \
  w.agent_id,a.name AS agent_name,w.kind,w.mode,w.event_types,w.filter_actor_type, \
 (CASE WHEN actor_agent.id IS NOT NULL OR actor_member.user_id IS NOT NULL THEN w.filter_actor_id END)::uuid AS filter_actor_id, \
 COALESCE(actor_agent.name,actor_user.name,'')::text AS filter_actor_name, \
  CASE WHEN source.id IS NOT NULL THEN w.filter_agent_id END AS filter_agent_id, \
  source.name AS filter_agent_name, \
  CASE WHEN EXISTS(SELECT 1 FROM agent_task_queue ft JOIN agent fa ON fa.id=ft.agent_id AND fa.workspace_id=w.workspace_id \
   WHERE ft.id=w.filter_task_id AND ft.issue_id=w.issue_id AND fa.id=ANY($2::uuid[])) THEN w.filter_task_id END AS filter_task_id, \
  w.interval_seconds,w.cron_expression,w.timezone, \
  w.next_fire_at,w.enabled,w.revision,w.disabled_at,w.last_task_id,w.last_error,w.created_at, \
  (i.status IN ('done','cancelled') OR EXISTS(SELECT 1 FROM issue_status s WHERE s.workspace_id=i.workspace_id AND s.key=i.status AND s.category IN ('done','closed'))) AS issue_closed, \
  COALESCE((w.created_by=$3::uuid OR $4::boolean),false) AS can_manage, \
  COALESCE(r.active_runs,0)::int AS active_runs \
 FROM issue_wakeup w \
 JOIN workspace ws ON ws.id=w.workspace_id \
 JOIN issue i ON i.id=w.issue_id AND i.workspace_id=w.workspace_id \
 JOIN agent a ON a.id=w.agent_id AND a.workspace_id=w.workspace_id \
 LEFT JOIN agent actor_agent ON w.filter_actor_type='agent' AND actor_agent.id=w.filter_actor_id AND actor_agent.workspace_id=w.workspace_id AND actor_agent.id=ANY($2::uuid[]) \
LEFT JOIN member actor_member ON w.filter_actor_type='member' AND actor_member.user_id=w.filter_actor_id AND actor_member.workspace_id=w.workspace_id \
LEFT JOIN \"user\" actor_user ON actor_user.id=actor_member.user_id \
 LEFT JOIN agent source ON source.id=w.filter_agent_id AND source.workspace_id=w.workspace_id AND source.id=ANY($2::uuid[]) \
 LEFT JOIN LATERAL ( \
  SELECT count(*) AS active_runs FROM agent_task_queue t \
  WHERE t.context->>'wakeup_id'=w.id::text AND t.issue_id=w.issue_id AND t.agent_id=w.agent_id \
   AND t.status IN ('queued','deferred','dispatched','running','waiting_local_directory') \
 ) r ON true \
 WHERE w.workspace_id=$1 \
), classified AS ( \
 SELECT *,CASE WHEN (enabled AND NOT issue_closed) OR active_runs>0 THEN 'active' \
  WHEN NOT issue_closed AND disabled_at IS NOT NULL THEN 'disabled' ELSE 'ended' END AS scope \
 FROM base \
), filtered AS ( \
 SELECT * FROM classified WHERE ($5::text='all' OR scope=$5) \
  AND ($6::text='all' OR ($6='event' AND kind='event') OR ($6='at' AND kind='at') OR ($6='recurring' AND kind IN ('every','cron'))) \
  AND ($7::text='' OR agent_id::text=$7) \
  AND ($8::text='' OR strpos(lower(issue_title||' '||issue_identifier||' '||agent_name),lower($8))>0) \
), page AS ( \
 SELECT * FROM filtered ORDER BY created_at DESC,id DESC LIMIT $9::int OFFSET $10::int \
), details AS ( \
 SELECT p.*,r.status AS last_task_status, \
  CASE WHEN r.id IS NOT NULL THEN jsonb_build_object( \
   'id',r.id,'agent_id',r.agent_id,'runtime_id',r.runtime_id,'issue_id',r.issue_id,'wakeup_id',p.id, \
   'status',r.status,'priority',r.priority,'created_at',r.created_at,'started_at',r.started_at, \
   'dispatched_at',r.dispatched_at,'completed_at',r.completed_at \
  ) END AS task \
 FROM page p \
 LEFT JOIN LATERAL ( \
  SELECT candidate.* FROM ( \
   (SELECT t.id,t.agent_id,t.runtime_id,t.issue_id,t.status,t.priority,t.created_at,t.started_at,t.dispatched_at,t.completed_at,1 AS active \
    FROM agent_task_queue t WHERE t.context->>'wakeup_id'=p.id::text AND t.issue_id=p.issue_id AND t.agent_id=p.agent_id \
     AND t.status IN ('queued','deferred','dispatched','running','waiting_local_directory') \
    ORDER BY (t.status IN ('running','waiting_local_directory','dispatched')) DESC,t.created_at DESC,t.id DESC LIMIT 1) \
   UNION ALL \
   (SELECT t.id,t.agent_id,t.runtime_id,t.issue_id,t.status,t.priority,t.created_at,t.started_at,t.dispatched_at,t.completed_at,0 AS active \
    FROM agent_task_queue t WHERE t.context->>'wakeup_id'=p.id::text AND t.issue_id=p.issue_id AND t.agent_id=p.agent_id \
     AND t.status NOT IN ('queued','deferred','dispatched','running','waiting_local_directory') \
    ORDER BY t.created_at DESC,t.id DESC LIMIT 1) \
  ) candidate ORDER BY active DESC LIMIT 1 \
 ) r ON true \
) \
SELECT jsonb_build_object( \
 'items',COALESCE((SELECT jsonb_agg(to_jsonb(details)-'created_at'-'scope' ORDER BY created_at DESC,id DESC) FROM details),'[]'::jsonb), \
 'total',(SELECT count(*) FROM filtered), \
 'counts',jsonb_build_object('all',(SELECT count(*) FROM classified),'active',(SELECT count(*) FROM classified WHERE scope='active'), \
  'disabled',(SELECT count(*) FROM classified WHERE scope='disabled'),'ended',(SELECT count(*) FROM classified WHERE scope='ended')), \
 'agents',COALESCE((SELECT jsonb_agg(x ORDER BY x.name,x.id) FROM (SELECT DISTINCT agent_id AS id,agent_name AS name FROM base) x),'[]'::jsonb) \
) AS result";

const SUMMARIES_SQL: &str = "\
WITH ranked AS ( \
 SELECT w.issue_id,w.id,w.agent_id,a.name AS agent_name,w.kind,w.mode,w.event_types,w.filter_actor_type, \
 (CASE WHEN actor_agent.id IS NOT NULL OR actor_member.user_id IS NOT NULL THEN w.filter_actor_id END)::uuid AS filter_actor_id, \
 COALESCE(actor_agent.name,actor_user.name,'')::text AS filter_actor_name, \
  (CASE WHEN EXISTS(SELECT 1 FROM agent_task_queue ft JOIN agent fa ON fa.id=ft.agent_id AND fa.workspace_id=w.workspace_id \
   WHERE ft.id=w.filter_task_id AND ft.issue_id=w.issue_id AND fa.id=ANY($2::uuid[])) THEN w.filter_task_id END)::uuid AS filter_task_id, \
  source.name AS filter_agent_name,w.interval_seconds,w.cron_expression,w.timezone,w.next_fire_at, \
  count(*) OVER(PARTITION BY w.issue_id) AS active_count, \
  count(*) FILTER(WHERE w.kind='event') OVER(PARTITION BY w.issue_id) AS event_count, \
  row_number() OVER(PARTITION BY w.issue_id ORDER BY w.next_fire_at NULLS LAST,w.created_at,w.id) AS rank \
 FROM issue_wakeup w \
 JOIN issue i ON i.id=w.issue_id AND i.workspace_id=w.workspace_id \
 JOIN agent a ON a.id=w.agent_id AND a.workspace_id=w.workspace_id \
 LEFT JOIN agent actor_agent ON w.filter_actor_type='agent' AND actor_agent.id=w.filter_actor_id AND actor_agent.workspace_id=w.workspace_id AND actor_agent.id=ANY($2::uuid[]) \
LEFT JOIN member actor_member ON w.filter_actor_type='member' AND actor_member.user_id=w.filter_actor_id AND actor_member.workspace_id=w.workspace_id \
LEFT JOIN \"user\" actor_user ON actor_user.id=actor_member.user_id \
LEFT JOIN agent source ON source.id=w.filter_agent_id AND source.workspace_id=w.workspace_id AND source.id=ANY($2::uuid[]) \
 WHERE w.workspace_id=$1 AND w.enabled \
  AND i.status NOT IN ('done','cancelled') \
  AND NOT EXISTS(SELECT 1 FROM issue_status s WHERE s.workspace_id=i.workspace_id AND s.key=i.status AND s.category IN ('done','closed')) \
) \
SELECT issue_id,id,agent_id,agent_name,kind,mode,event_types,filter_actor_type,filter_actor_id,filter_actor_name,filter_task_id,filter_agent_name,interval_seconds,cron_expression,timezone,next_fire_at,active_count,event_count \
FROM ranked WHERE rank<=3 ORDER BY issue_id,rank";
