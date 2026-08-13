#![forbid(unsafe_code)]
//! Pipeline case events + children summary DB glue - 1:1 with Node pipelines-aggregation.ts

use super::aggregation::{
    bounded_limit, payload_string, stage_automation_from_config, AutomationContext, AutomationIssue,
    AutomationRoutine, CaseChildrenRollup, CompanyCaseEventAgent, CompanyCaseEventCase,
    CompanyCaseEventItem, CompanyCaseEventPipeline, CompanyCaseEventStage, CompanyCaseEventsPage,
    COMPANY_CASE_EVENTS_DEFAULT_LIMIT, COMPANY_CASE_EVENTS_MAX_LIMIT,
    CaseChildGroup, CaseChildNode, CaseChildPipeline, CaseChildStage, CaseChildrenTree,
    CASE_CHILDREN_TREE_MAX_DEPTH, CASE_CHILDREN_TREE_MAX_NODES,
};
use pc_core::Timestamp;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CaseEventDbRow {
    pub event_id: Uuid,
    pub event_company_id: Uuid,
    pub event_case_id: Uuid,
    pub event_type: String,
    pub event_actor_type: String,
    pub event_actor_user_id: Option<String>,
    pub event_actor_agent_id: Option<Uuid>,
    pub event_run_id: Option<Uuid>,
    pub event_from_stage_id: Option<Uuid>,
    pub event_to_stage_id: Option<Uuid>,
    pub event_payload: serde_json::Value,
    pub event_created_at: Timestamp,
    pub event_updated_at: Timestamp,
    pub case_id: Uuid,
    pub case_key: String,
    pub case_title: String,
    pub case_terminal_kind: Option<String>,
    pub pipeline_id: Uuid,
    pub pipeline_key: String,
    pub pipeline_name: String,
    pub from_stage_id: Option<Uuid>,
    pub from_stage_key: Option<String>,
    pub from_stage_name: Option<String>,
    pub from_stage_kind: Option<String>,
    pub to_stage_id: Option<Uuid>,
    pub to_stage_key: Option<String>,
    pub to_stage_name: Option<String>,
    pub to_stage_kind: Option<String>,
    pub actor_agent_id: Option<Uuid>,
    pub actor_agent_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoutineRefRow { pub id: Uuid, pub title: String }

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IssueRefRow { pub id: Uuid, pub identifier: Option<String>, pub title: String, pub status: String }

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StageConfigRow { pub pipeline_id: Uuid, pub id: Uuid, pub config: serde_json::Value }

pub async fn list_company_case_events(
    pool: &PgPool, company_id: Uuid, types: &[String], limit: Option<i64>, offset: Option<i64>,
) -> sqlx::Result<Vec<CaseEventDbRow>> {
    let bounded = bounded_limit(limit, COMPANY_CASE_EVENTS_DEFAULT_LIMIT, COMPANY_CASE_EVENTS_MAX_LIMIT);
    let off = offset.unwrap_or(0).max(0);
    if !types.is_empty() {
        sqlx::query_as::<_, CaseEventDbRow>(
            "SELECT pce.id AS event_id, pce.company_id AS event_company_id, pce.case_id AS event_case_id, pce.type AS event_type, pce.actor_type AS event_actor_type, pce.actor_user_id AS event_actor_user_id, pce.actor_agent_id AS event_actor_agent_id, pce.run_id AS event_run_id, pce.from_stage_id AS event_from_stage_id, pce.to_stage_id AS event_to_stage_id, pce.payload AS event_payload, pce.created_at AS event_created_at, pce.updated_at AS event_updated_at, pc.id AS case_id, pc.case_key AS case_key, pc.title AS case_title, pc.terminal_kind AS case_terminal_kind, p.id AS pipeline_id, p.key AS pipeline_key, p.name AS pipeline_name, fs.id AS from_stage_id, fs.key AS from_stage_key, fs.name AS from_stage_name, fs.kind AS from_stage_kind, ts.id AS to_stage_id, ts.key AS to_stage_key, ts.name AS to_stage_name, ts.kind AS to_stage_kind, aa.id AS actor_agent_id, aa.name AS actor_agent_name FROM pipeline_case_events pce INNER JOIN pipeline_cases pc ON pc.id = pce.case_id INNER JOIN pipelines p ON p.id = pc.pipeline_id LEFT JOIN pipeline_stages fs ON fs.id = pce.from_stage_id LEFT JOIN pipeline_stages ts ON ts.id = pce.to_stage_id LEFT JOIN agents aa ON aa.id = pce.actor_agent_id WHERE pce.company_id = $1 AND pc.company_id = $1 AND pce.type = ANY($2) ORDER BY pce.created_at DESC, pce.id DESC LIMIT $3 OFFSET $4",
        ).bind(company_id).bind(types).bind(bounded + 1).bind(off).fetch_all(pool).await
    } else {
        sqlx::query_as::<_, CaseEventDbRow>(
            "SELECT pce.id AS event_id, pce.company_id AS event_company_id, pce.case_id AS event_case_id, pce.type AS event_type, pce.actor_type AS event_actor_type, pce.actor_user_id AS event_actor_user_id, pce.actor_agent_id AS event_actor_agent_id, pce.run_id AS event_run_id, pce.from_stage_id AS event_from_stage_id, pce.to_stage_id AS event_to_stage_id, pce.payload AS event_payload, pce.created_at AS event_created_at, pce.updated_at AS event_updated_at, pc.id AS case_id, pc.case_key AS case_key, pc.title AS case_title, pc.terminal_kind AS case_terminal_kind, p.id AS pipeline_id, p.key AS pipeline_key, p.name AS pipeline_name, fs.id AS from_stage_id, fs.key AS from_stage_key, fs.name AS from_stage_name, fs.kind AS from_stage_kind, ts.id AS to_stage_id, ts.key AS to_stage_key, ts.name AS to_stage_name, ts.kind AS to_stage_kind, aa.id AS actor_agent_id, aa.name AS actor_agent_name FROM pipeline_case_events pce INNER JOIN pipeline_cases pc ON pc.id = pce.case_id INNER JOIN pipelines p ON p.id = pc.pipeline_id LEFT JOIN pipeline_stages fs ON fs.id = pce.from_stage_id LEFT JOIN pipeline_stages ts ON ts.id = pce.to_stage_id LEFT JOIN agents aa ON aa.id = pce.actor_agent_id WHERE pce.company_id = $1 AND pc.company_id = $1 ORDER BY pce.created_at DESC, pce.id DESC LIMIT $2 OFFSET $3",
        ).bind(company_id).bind(bounded + 1).bind(off).fetch_all(pool).await
    }
}

pub async fn lookup_routines_by_ids(pool: &PgPool, company_id: Uuid, ids: &[Uuid]) -> sqlx::Result<Vec<RoutineRefRow>> {
    if ids.is_empty() { return Ok(Vec::new()); }
    sqlx::query_as::<_, RoutineRefRow>("SELECT id, title FROM routines WHERE company_id = $1 AND id = ANY($2)")
        .bind(company_id).bind(ids).fetch_all(pool).await
}

pub async fn lookup_issues_by_ids(pool: &PgPool, company_id: Uuid, ids: &[Uuid]) -> sqlx::Result<Vec<IssueRefRow>> {
    if ids.is_empty() { return Ok(Vec::new()); }
    sqlx::query_as::<_, IssueRefRow>("SELECT id, identifier, title, status FROM issues WHERE company_id = $1 AND id = ANY($2)")
        .bind(company_id).bind(ids).fetch_all(pool).await
}

pub async fn lookup_stages_by_pipeline_ids(pool: &PgPool, pipeline_ids: &[Uuid]) -> sqlx::Result<Vec<StageConfigRow>> {
    if pipeline_ids.is_empty() { return Ok(Vec::new()); }
    sqlx::query_as::<_, StageConfigRow>("SELECT pipeline_id, id, config FROM pipeline_stages WHERE pipeline_id = ANY($1)")
        .bind(pipeline_ids).fetch_all(pool).await
}

pub fn build_company_case_event_item(
    row: CaseEventDbRow,
    routines_by_id: &std::collections::HashMap<Uuid, RoutineRefRow>,
    issues_by_id: &std::collections::HashMap<Uuid, IssueRefRow>,
    stages_by_automation_id: &std::collections::HashMap<String, Uuid>,
    stages_by_routine_id: &std::collections::HashMap<String, Uuid>,
) -> CompanyCaseEventItem {
    let from_stage = row.from_stage_id.map(|id| CompanyCaseEventStage {
        id: id.to_string(), key: row.from_stage_key.clone().unwrap_or_default(),
        name: row.from_stage_name.clone().unwrap_or_default(), kind: row.from_stage_kind.clone().unwrap_or_default(),
    });
    let to_stage = row.to_stage_id.map(|id| CompanyCaseEventStage {
        id: id.to_string(), key: row.to_stage_key.clone().unwrap_or_default(),
        name: row.to_stage_name.clone().unwrap_or_default(), kind: row.to_stage_kind.clone().unwrap_or_default(),
    });
    let actor_agent = row.actor_agent_id.map(|id| CompanyCaseEventAgent {
        id: id.to_string(), name: row.actor_agent_name.clone().unwrap_or_default(),
    });
    let automation = if row.event_type == "automation_executed" || row.event_type == "automation_failed" {
        let routine_id = payload_string(&row.event_payload, "routineId");
        let issue_id = payload_string(&row.event_payload, "issueId");
        let automation_id = payload_string(&row.event_payload, "automationId");
        let _stage_id = automation_id.as_ref().and_then(|aid| stages_by_automation_id.get(aid))
            .or_else(|| routine_id.as_ref().and_then(|rid| stages_by_routine_id.get(rid)));
        let routine_ref = routine_id.as_ref().and_then(|rid| Uuid::parse_str(rid).ok())
            .and_then(|rid| routines_by_id.get(&rid));
        let issue_ref = issue_id.as_ref().and_then(|iid| Uuid::parse_str(iid).ok())
            .and_then(|iid| issues_by_id.get(&iid));
        Some(AutomationContext {
            routine: routine_ref.map(|r| AutomationRoutine { id: r.id.to_string(), title: r.title.clone() }),
            issue: issue_ref.map(|i| AutomationIssue {
                id: i.id.to_string(), identifier: i.identifier.clone(),
                title: i.title.clone(), status: i.status.clone(),
            }),
        })
    } else { None };
    CompanyCaseEventItem {
        id: row.event_id.to_string(),
        company_id: row.event_company_id.to_string(),
        case_id: row.event_case_id.to_string(),
        type_: row.event_type,
        actor_type: row.event_actor_type,
        actor_user_id: row.event_actor_user_id,
        actor_agent_id: row.event_actor_agent_id.map(|id| id.to_string()),
        run_id: row.event_run_id.map(|id| id.to_string()),
        from_stage_id: row.event_from_stage_id.map(|id| id.to_string()),
        to_stage_id: row.event_to_stage_id.map(|id| id.to_string()),
        payload: row.event_payload,
        created_at: row.event_created_at.as_datetime().to_rfc3339(),
        updated_at: row.event_updated_at.as_datetime().to_rfc3339(),
        case: CompanyCaseEventCase { id: row.case_id.to_string(), case_key: row.case_key, title: row.case_title, terminal_kind: row.case_terminal_kind },
        pipeline: CompanyCaseEventPipeline { id: row.pipeline_id.to_string(), key: row.pipeline_key, name: row.pipeline_name },
        from_stage, to_stage, actor_agent, automation,
    }
}

pub async fn list_company_case_events_page(
    pool: &PgPool, company_id: Uuid, types: &[String], limit: Option<i64>, offset: Option<i64>,
) -> sqlx::Result<CompanyCaseEventsPage> {
    let bounded = bounded_limit(limit, COMPANY_CASE_EVENTS_DEFAULT_LIMIT, COMPANY_CASE_EVENTS_MAX_LIMIT);
    let off = offset.unwrap_or(0).max(0);
    let rows = list_company_case_events(pool, company_id, types, limit, offset).await?;
    let has_more = rows.len() as i64 > bounded;
    let page_rows: Vec<CaseEventDbRow> = if has_more { rows.into_iter().take(bounded as usize).collect() } else { rows };
    let automation_rows: Vec<&CaseEventDbRow> = page_rows.iter()
        .filter(|r| r.event_type == "automation_executed" || r.event_type == "automation_failed").collect();
    let routine_ids: Vec<Uuid> = automation_rows.iter()
        .filter_map(|r| payload_string(&r.event_payload, "routineId"))
        .filter_map(|s| Uuid::parse_str(&s).ok())
        .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let issue_ids: Vec<Uuid> = automation_rows.iter()
        .filter_map(|r| payload_string(&r.event_payload, "issueId"))
        .filter_map(|s| Uuid::parse_str(&s).ok())
        .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let automation_pipeline_ids: Vec<Uuid> = automation_rows.iter().map(|r| r.pipeline_id)
        .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let routine_fut = lookup_routines_by_ids(pool, company_id, &routine_ids);
    let issue_fut = lookup_issues_by_ids(pool, company_id, &issue_ids);
    let stage_fut = lookup_stages_by_pipeline_ids(pool, &automation_pipeline_ids);
    let (routine_rows, issue_rows, stage_rows) = tokio::join!(routine_fut, issue_fut, stage_fut);
    let routine_rows = routine_rows?;
    let issue_rows = issue_rows?;
    let stage_rows = stage_rows?;
    let routines_by_id: std::collections::HashMap<Uuid, RoutineRefRow> = routine_rows.into_iter().map(|r| (r.id, r)).collect();
    let issues_by_id: std::collections::HashMap<Uuid, IssueRefRow> = issue_rows.into_iter().map(|r| (r.id, r)).collect();
    let mut stages_by_automation_id: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    let mut stages_by_routine_id: std::collections::HashMap<String, Uuid> = std::collections::HashMap::new();
    for stage in stage_rows {
        if let Some(automation) = stage_automation_from_config(&stage.id.to_string(), &stage.config) {
            stages_by_automation_id.insert(automation.id, stage.id);
            stages_by_routine_id.insert(automation.routine_id, stage.id);
        }
    }
    let items: Vec<CompanyCaseEventItem> = page_rows.into_iter().map(|r| {
        build_company_case_event_item(r, &routines_by_id, &issues_by_id, &stages_by_automation_id, &stages_by_routine_id)
    }).collect();
    let total = items.len();
    Ok(CompanyCaseEventsPage { items, limit: bounded, offset: off, has_more, total })
}

pub async fn get_direct_children_summary(
    pool: &PgPool, company_id: Uuid, case_id: Uuid,
) -> sqlx::Result<CaseChildrenRollup> {
    let row: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT count(*)::int8 AS total, count(*) FILTER (WHERE terminal_kind = 'done')::int8 AS done, count(*) FILTER (WHERE terminal_kind = 'cancelled')::int8 AS dropped, count(*) FILTER (WHERE terminal_kind IS NULL)::int8 AS in_motion FROM pipeline_cases WHERE company_id = $1 AND parent_case_id = $2 AND hidden_from_board_at IS NULL",
    ).bind(company_id).bind(case_id).fetch_optional(pool).await?;
    Ok(match row {
        Some((total, done, dropped, in_motion)) => CaseChildrenRollup { total, done, dropped, in_motion },
        None => CaseChildrenRollup::default(),
    })
}


#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SubtreeRow {
    pub id: Uuid,
    pub parent_case_id: Option<Uuid>,
    pub pipeline_id: Uuid,
    pub stage_id: Uuid,
    pub case_key: String,
    pub title: String,
    pub terminal_kind: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub depth: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PipelineRefRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StageRefRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub kind: String,
}

pub async fn fetch_case_subtree(
    pool: &PgPool, company_id: Uuid, case_id: Uuid,
) -> sqlx::Result<Vec<SubtreeRow>> {
    sqlx::query_as::<_, SubtreeRow>(
        "WITH RECURSIVE subtree AS ( \
         SELECT id, parent_case_id, pipeline_id, stage_id, case_key, title, terminal_kind, created_at, updated_at, 0 AS depth \
         FROM pipeline_cases \
         WHERE company_id = $1 AND id = $2 \
         UNION ALL \
         SELECT child.id, child.parent_case_id, child.pipeline_id, child.stage_id, child.case_key, child.title, child.terminal_kind, child.created_at, child.updated_at, parent.depth + 1 \
         FROM pipeline_cases child \
         JOIN subtree parent ON child.parent_case_id = parent.id \
         WHERE child.company_id = $1 AND child.hidden_from_board_at IS NULL AND parent.depth < $3 \
         ) \
         SELECT id, parent_case_id, pipeline_id, stage_id, case_key, title, terminal_kind, created_at, updated_at, depth \
         FROM subtree \
         ORDER BY depth ASC, created_at ASC \
         LIMIT $4",
    ).bind(company_id).bind(case_id).bind(CASE_CHILDREN_TREE_MAX_DEPTH as i32).bind(CASE_CHILDREN_TREE_MAX_NODES + 1).fetch_all(pool).await
}

pub async fn lookup_pipelines_by_ids(pool: &PgPool, company_id: Uuid, ids: &[Uuid]) -> sqlx::Result<Vec<PipelineRefRow>> {
    if ids.is_empty() { return Ok(Vec::new()); }
    sqlx::query_as::<_, PipelineRefRow>("SELECT id, key, name FROM pipelines WHERE company_id = $1 AND id = ANY($2)")
        .bind(company_id).bind(ids).fetch_all(pool).await
}

pub async fn lookup_stages_by_ids(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<StageRefRow>> {
    if ids.is_empty() { return Ok(Vec::new()); }
    sqlx::query_as::<_, StageRefRow>("SELECT id, key, name, kind FROM pipeline_stages WHERE id = ANY($1)")
        .bind(ids).fetch_all(pool).await
}

pub fn build_case_children_tree(
    rows: Vec<SubtreeRow>,
    pipeline_by_id: &std::collections::HashMap<Uuid, PipelineRefRow>,
    stage_by_id: &std::collections::HashMap<Uuid, StageRefRow>,
    root_case_id: Uuid,
    truncated: bool,
) -> Option<CaseChildrenTree> {
    if rows.is_empty() { return None; }
    let mut child_rows_by_parent: std::collections::HashMap<Uuid, Vec<SubtreeRow>> = std::collections::HashMap::new();
    for row in rows.clone() {
        if row.id != root_case_id {
            if let Some(parent_id) = row.parent_case_id {
                child_rows_by_parent.entry(parent_id).or_default().push(row);
            }
        }
    }
    fn build(
        row: &SubtreeRow,
        children_map: &std::collections::HashMap<Uuid, Vec<SubtreeRow>>,
        pipeline_by_id: &std::collections::HashMap<Uuid, PipelineRefRow>,
        stage_by_id: &std::collections::HashMap<Uuid, StageRefRow>,
    ) -> CaseChildNode {
        let mut child_rows = children_map.get(&row.id).cloned().unwrap_or_default();
        child_rows.sort_by_key(|r| r.created_at.as_datetime().to_rfc3339());
        let children: Vec<CaseChildNode> = child_rows.iter().map(|r| build(r, children_map, pipeline_by_id, stage_by_id)).collect();
        let mut rollup = CaseChildrenRollup { total: 0, done: 0, dropped: 0, in_motion: 0 };
        for child in &children {
            rollup.total += 1 + child.rollup.total;
            rollup.done += if child.terminal_kind.as_deref() == Some("done") { 1 } else { 0 } + child.rollup.done;
            rollup.dropped += if child.terminal_kind.as_deref() == Some("cancelled") { 1 } else { 0 } + child.rollup.dropped;
            rollup.in_motion += if child.terminal_kind.is_none() { 1 } else { 0 } + child.rollup.in_motion;
        }
        let pipeline = pipeline_by_id.get(&row.pipeline_id).cloned().unwrap_or(PipelineRefRow {
            id: row.pipeline_id, key: String::new(), name: String::new(),
        });
        let pipeline_id_str = pipeline.id.to_string(); let mut groups_map: std::collections::BTreeMap<String, CaseChildGroup> = std::collections::BTreeMap::new();
        for child in &children {
            let entry = groups_map.entry(child.pipeline.id.clone()).or_insert_with(|| CaseChildGroup {
                pipeline: child.pipeline.clone(),
                cases: Vec::new(),
            });
            entry.cases.push(child.clone());
        }
        let mut child_groups: Vec<CaseChildGroup> = groups_map.into_values().collect();
        child_groups.sort_by(|a, b| {
            if a.pipeline.id == pipeline_id_str && b.pipeline.id != pipeline_id_str { return std::cmp::Ordering::Less; }
            if b.pipeline.id == pipeline_id_str && a.pipeline.id != pipeline_id_str { return std::cmp::Ordering::Greater; }
            a.pipeline.name.cmp(&b.pipeline.name)
        });
        let stage = stage_by_id.get(&row.stage_id).cloned().unwrap_or(StageRefRow {
            id: row.stage_id, key: String::new(), name: String::new(), kind: String::new(),
        });
        CaseChildNode {
            id: row.id.to_string(),
            case_key: row.case_key.clone(),
            title: row.title.clone(),
            terminal_kind: row.terminal_kind.clone(),
            created_at: row.created_at.as_datetime().to_rfc3339(),
            updated_at: row.updated_at.as_datetime().to_rfc3339(),
            pipeline: CaseChildPipeline {
                id: pipeline.id.to_string(),
                key: pipeline.key.clone(),
                name: pipeline.name.clone(),
            },
            stage: CaseChildStage {
                id: stage.id.to_string(),
                key: stage.key.clone(),
                name: stage.name.clone(),
                kind: stage.kind.clone(),
            },
            rollup,
            child_groups,
        }
    }
    let root_row = rows.iter().find(|r| r.id == root_case_id)?;
    let root = build(root_row, &child_rows_by_parent, pipeline_by_id, stage_by_id);
    Some(CaseChildrenTree {
        rollup: root.rollup.clone(),
        child_groups: root.child_groups.clone(),
        case: root,
        truncated,
        total_nodes: rows.len(),
    })
}

pub async fn get_case_children_tree(
    pool: &PgPool, company_id: Uuid, case_id: Uuid,
) -> sqlx::Result<Option<CaseChildrenTree>> {
    let rows = fetch_case_subtree(pool, company_id, case_id).await?;
    if rows.is_empty() { return Ok(None); }
    let truncated = rows.len() as i64 > CASE_CHILDREN_TREE_MAX_NODES;
    let bounded: Vec<SubtreeRow> = if truncated { rows.into_iter().take(CASE_CHILDREN_TREE_MAX_NODES as usize).collect() } else { rows };
    let pipeline_ids: Vec<Uuid> = bounded.iter().map(|r| r.pipeline_id).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let stage_ids: Vec<Uuid> = bounded.iter().map(|r| r.stage_id).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let (pipeline_rows, stage_rows) = tokio::join!(
        lookup_pipelines_by_ids(pool, company_id, &pipeline_ids),
        lookup_stages_by_ids(pool, &stage_ids),
    );
    let pipeline_rows = pipeline_rows?;
    let stage_rows = stage_rows?;
    let pipeline_by_id: std::collections::HashMap<Uuid, PipelineRefRow> = pipeline_rows.into_iter().map(|p| (p.id, p)).collect();
    let stage_by_id: std::collections::HashMap<Uuid, StageRefRow> = stage_rows.into_iter().map(|s| (s.id, s)).collect();
    Ok(build_case_children_tree(bounded, &pipeline_by_id, &stage_by_id, case_id, truncated))
}


#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DescendantActiveWorkCountRow {
    pub root_id: Uuid,
    pub count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PipelineDescendantActiveWorkCountRow {
    pub pipeline_id: Uuid,
    pub count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PipelineConnectionRow {
    pub parent_pipeline_id: Uuid,
    pub child_pipeline_id: Uuid,
}

pub async fn load_descendant_active_work_counts_for_cases(
    pool: &PgPool, company_id: Uuid, case_ids: &[Uuid],
) -> sqlx::Result<Vec<DescendantActiveWorkCountRow>> {
    let unique: Vec<Uuid> = {
        let mut seen = std::collections::BTreeSet::new();
        let mut v = Vec::new();
        for id in case_ids {
            if seen.insert(*id) {
                v.push(*id);
            }
        }
        v
    };
    if unique.is_empty() { return Ok(Vec::new()); }
    sqlx::query_as::<_, DescendantActiveWorkCountRow>(
        "WITH RECURSIVE roots(root_id) AS ( \
             SELECT * FROM UNNEST($2::uuid[]) AS t(root_id) \
         ), \
         subtree(root_id, id, depth) AS ( \
             SELECT roots.root_id, roots.root_id, 0 FROM roots \
             JOIN pipeline_cases root_case ON root_case.id = roots.root_id AND root_case.company_id = $1 \
             UNION ALL \
             SELECT subtree.root_id, child.id, subtree.depth + 1 \
             FROM pipeline_cases child \
             JOIN subtree ON child.parent_case_id = subtree.id \
             WHERE child.company_id = $1 AND child.hidden_from_board_at IS NULL AND subtree.depth < $3 \
         ) \
         SELECT subtree.root_id, COUNT(DISTINCT subtree.id)::int8 AS count \
         FROM subtree \
         JOIN pipeline_case_issue_links link ON link.company_id = $1 AND link.case_id = subtree.id AND link.role IN ('work', 'automation') \
         JOIN issues issue ON issue.id = link.issue_id AND issue.company_id = $1 AND issue.status = 'in_progress' AND issue.hidden_at IS NULL \
         JOIN agents agent ON agent.id = issue.assignee_agent_id \
         WHERE subtree.depth > 0 \
         GROUP BY subtree.root_id",
    )
    .bind(company_id)
    .bind(&unique)
    .bind(CASE_CHILDREN_TREE_MAX_DEPTH as i32)
    .fetch_all(pool)
    .await
}

pub async fn load_pipeline_descendant_active_work_counts(
    pool: &PgPool, company_id: Uuid, pipeline_ids: &[Uuid],
) -> sqlx::Result<Vec<PipelineDescendantActiveWorkCountRow>> {
    let unique: Vec<Uuid> = {
        let mut seen = std::collections::BTreeSet::new();
        let mut v = Vec::new();
        for id in pipeline_ids {
            if seen.insert(*id) {
                v.push(*id);
            }
        }
        v
    };
    if unique.is_empty() { return Ok(Vec::new()); }
    sqlx::query_as::<_, PipelineDescendantActiveWorkCountRow>(
        "WITH RECURSIVE target_pipelines(pipeline_id) AS ( \
             SELECT * FROM UNNEST($2::uuid[]) AS t(pipeline_id) \
         ), \
         roots(root_pipeline_id, root_case_id) AS ( \
             SELECT target_pipelines.pipeline_id, root_case.id \
             FROM target_pipelines \
             JOIN pipeline_cases root_case ON root_case.pipeline_id = target_pipelines.pipeline_id AND root_case.company_id = $1 \
         ), \
         subtree(root_pipeline_id, root_case_id, id, depth) AS ( \
             SELECT roots.root_pipeline_id, roots.root_case_id, roots.root_case_id, 0 FROM roots \
             UNION ALL \
             SELECT subtree.root_pipeline_id, subtree.root_case_id, child.id, subtree.depth + 1 \
             FROM pipeline_cases child \
             JOIN subtree ON child.parent_case_id = subtree.id \
             WHERE child.company_id = $1 AND child.hidden_from_board_at IS NULL AND subtree.depth < $3 \
         ) \
         SELECT subtree.root_pipeline_id AS pipeline_id, COUNT(DISTINCT subtree.id)::int8 AS count \
         FROM subtree \
         JOIN pipeline_case_issue_links link ON link.company_id = $1 AND link.case_id = subtree.id AND link.role IN ('work', 'automation') \
         JOIN issues issue ON issue.id = link.issue_id AND issue.company_id = $1 AND issue.status = 'in_progress' AND issue.hidden_at IS NULL \
         JOIN agents agent ON agent.id = issue.assignee_agent_id \
         WHERE subtree.depth > 0 \
         GROUP BY subtree.root_pipeline_id",
    )
    .bind(company_id)
    .bind(&unique)
    .bind(CASE_CHILDREN_TREE_MAX_DEPTH as i32)
    .fetch_all(pool)
    .await
}

pub async fn load_pipeline_connections(
    pool: &PgPool, company_id: Uuid,
) -> sqlx::Result<Vec<PipelineConnectionRow>> {
    sqlx::query_as::<_, PipelineConnectionRow>(
        "SELECT DISTINCT parent_case.pipeline_id AS parent_pipeline_id, child.pipeline_id AS child_pipeline_id \
         FROM pipeline_cases child \
         INNER JOIN pipeline_cases parent_case ON child.parent_case_id = parent_case.id \
         WHERE child.company_id = $1 \
           AND parent_case.company_id = $1 \
           AND child.pipeline_id != parent_case.pipeline_id",
    )
    .bind(company_id)
    .fetch_all(pool)
    .await
}
