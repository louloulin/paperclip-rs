#![forbid(unsafe_code)]
//! Pipeline attention DB glue —— 与 Node \`listPipelineAttention\` 1:1 对齐。
//!
//! 当前 R639.2 子集：
//! - suggestions（pending_suggestion IS NOT NULL）
//! - reviews（stage.kind='review' + caller-aware reviewer filter）
//! - heads_up（drift detection）留 R639.2.2 轮次

use super::aggregation::{
    bounded_limit, AttentionCaseDisplay, AttentionCaller, AttentionPipelineRef,
    AttentionStageRef, PipelineAttention, PipelineAttentionCounts, ReviewConfig, ReviewItem,
    SuggestionActor, SuggestionItem, SuggestionPayload, PIPELINE_ATTENTION_DEFAULT_LIMIT,
    PIPELINE_ATTENTION_MAX_LIMIT,
};
use pc_core::Timestamp;
use sqlx::{PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

/// suggestion 行（pipeline_cases JOIN pipelines + pipeline_stages）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SuggestionDbRow {
    pub case_id: Uuid,
    pub case_key: String,
    pub case_title: String,
    pub case_summary: Option<String>,
    pub case_version: i32,
    pub case_terminal_kind: Option<String>,
    pub case_updated_at: Timestamp,
    pub case_created_at: Timestamp,
    pub case_pending_suggestion: Option<serde_json::Value>,
    pub pipeline_id: Uuid,
    pub pipeline_key: String,
    pub pipeline_name: String,
    pub stage_id: Uuid,
    pub stage_key: String,
    pub stage_name: String,
    pub stage_kind: String,
    pub to_stage_name: Option<String>,
    pub suggesting_agent_id: Option<String>,
    pub suggesting_agent_name: Option<String>,
}

/// review 行。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReviewDbRow {
    pub case_id: Uuid,
    pub case_key: String,
    pub case_title: String,
    pub case_summary: Option<String>,
    pub case_version: i32,
    pub case_terminal_kind: Option<String>,
    pub case_updated_at: Timestamp,
    pub case_created_at: Timestamp,
    pub pipeline_id: Uuid,
    pub pipeline_key: String,
    pub pipeline_name: String,
    pub stage_id: Uuid,
    pub stage_key: String,
    pub stage_name: String,
    pub stage_kind: String,
    pub stage_config: serde_json::Value,
    pub created_at: Timestamp,
}

/// 构建 reviewer-aware SQL 条件（user 永远 true；agent 看 requireApproval + approver.id）。
///
/// 与 Node \`reviewStageAwaitsCallerSql\` 1:1 对齐 —— SQL 端过滤以防 busy company 截断 agent 的 review feed。
pub fn review_stage_awaits_caller_sql(caller: &AttentionCaller) -> String {
    if caller.is_user() {
        return "true".to_string();
    }
    let agent_id = caller.agent_id().unwrap_or("");
    format!(
        "(coalesce(ps.config->>'reviewerKind', '') = 'any' \
         or (coalesce(ps.config->>'reviewerKind', '') <> 'human' \
             and (coalesce((ps.config->>'requireApproval')::boolean, false) = false \
                  or (ps.config->'approver'->>'kind' = 'agent' \
                      and ps.config->'approver'->>'id' = '{}'))))",
        agent_id.replace('\'', "''")
    )
}

/// list_suggestions —— projection of pending_suggestion IS NOT NULL cases.
pub async fn list_suggestions(
    pool: &PgPool,
    company_id: Uuid,
    limit: i64,
) -> sqlx::Result<Vec<SuggestionDbRow>> {
    sqlx::query_as::<_, SuggestionDbRow>(
        "SELECT pc.id AS case_id, pc.case_key, pc.title AS case_title, pc.summary AS case_summary, \
                pc.version AS case_version, pc.terminal_kind AS case_terminal_kind, \
                pc.updated_at AS case_updated_at, pc.created_at AS case_created_at, \
                pc.pending_suggestion AS case_pending_suggestion, \
                p.id AS pipeline_id, p.key AS pipeline_key, p.name AS pipeline_name, \
                ps.id AS stage_id, ps.key AS stage_key, ps.name AS stage_name, ps.kind AS stage_kind, \
                to_stage.name AS to_stage_name, \
                sa.id::text AS suggesting_agent_id, sa.name AS suggesting_agent_name \
         FROM pipeline_cases pc \
         INNER JOIN pipelines p ON p.id = pc.pipeline_id \
         INNER JOIN pipeline_stages ps ON ps.id = pc.stage_id \
         LEFT JOIN pipeline_stages to_stage ON to_stage.pipeline_id = pc.pipeline_id \
             AND to_stage.key = pc.pending_suggestion->>'toStageKey' \
         LEFT JOIN agents sa ON sa.id::text = pc.pending_suggestion->>'suggestedByAgentId' \
         WHERE pc.company_id = $1 \
           AND p.company_id = $1 \
           AND pc.terminal_kind IS NULL \
           AND pc.pending_suggestion IS NOT NULL \
         ORDER BY pc.updated_at DESC \
         LIMIT $2"
    )
    .bind(company_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// list_reviews —— projection of stage.kind='review' + caller filter.
pub async fn list_reviews(
    pool: &PgPool,
    company_id: Uuid,
    caller: &AttentionCaller,
    limit: i64,
) -> sqlx::Result<Vec<ReviewDbRow>> {
    let caller_clause = review_stage_awaits_caller_sql(caller);
    let sql = format!(
        "SELECT pc.id AS case_id, pc.case_key, pc.title AS case_title, pc.summary AS case_summary, \
                pc.version AS case_version, pc.terminal_kind AS case_terminal_kind, \
                pc.updated_at AS case_updated_at, pc.created_at AS case_created_at, \
                p.id AS pipeline_id, p.key AS pipeline_key, p.name AS pipeline_name, \
                ps.id AS stage_id, ps.key AS stage_key, ps.name AS stage_name, ps.kind AS stage_kind, \
                ps.config AS stage_config, pc.created_at \
         FROM pipeline_cases pc \
         INNER JOIN pipelines p ON p.id = pc.pipeline_id \
         INNER JOIN pipeline_stages ps ON ps.id = pc.stage_id \
         WHERE pc.company_id = $1 \
           AND p.company_id = $1 \
           AND ps.kind = 'review' \
           AND pc.terminal_kind IS NULL \
           AND {caller_clause} \
         ORDER BY pc.created_at ASC \
         LIMIT $2"
    );
    sqlx::query_as::<_, ReviewDbRow>(&sql)
        .bind(company_id)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// 把 SuggestionDbRow 转 SuggestionItem。
pub fn suggestion_row_to_item(row: SuggestionDbRow) -> Option<SuggestionItem> {
    let stage_key = row.stage_key.clone();
    let stage_name = row.stage_name.clone();
    let stage_kind = row.stage_kind.clone();
    let pending = row.case_pending_suggestion.as_ref()?;
    let suggestion_id = pending
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}:suggestion", row.case_id));
    let to_stage_key = pending
        .get("toStageKey")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let rationale = pending
        .get("rationale")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let confidence = pending.get("confidence").and_then(|v| v.as_f64());
    let created_at = pending
        .get("createdAt")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| row.case_created_at.as_datetime().to_rfc3339());
    let suggested_by = match (&row.suggesting_agent_id, &row.suggesting_agent_name) {
        (Some(id), Some(name)) => Some(SuggestionActor {
            agent_id: id.clone(),
            agent_name: name.clone(),
        }),
        _ => None,
    };
    Some(SuggestionItem {
        case: AttentionCaseDisplay {
            id: row.case_id.to_string(),
            case_key: row.case_key,
            title: row.case_title,
            summary: row.case_summary,
            version: row.case_version,
            terminal_kind: row.case_terminal_kind,
            updated_at: row.case_updated_at.as_datetime().to_rfc3339(),
            created_at: row.case_created_at.as_datetime().to_rfc3339(),
            pipeline: AttentionPipelineRef {
                id: row.pipeline_id.to_string(),
                key: row.pipeline_key,
                name: row.pipeline_name,
            },
            stage: AttentionStageRef {
                id: row.stage_id.to_string(),
                key: stage_key.clone(),
                name: stage_name.clone(),
                kind: stage_kind,
            },
        },
        suggestion: SuggestionPayload {
            id: suggestion_id,
            from_stage_key: stage_key,
            from_stage_name: stage_name,
            to_stage_key,
            to_stage_name: row.to_stage_name,
            rationale,
            confidence,
            created_at,
            suggested_by,
        },
    })
}

/// 把 ReviewDbRow 转 ReviewItem。
pub fn review_row_to_item(row: ReviewDbRow) -> ReviewItem {
    let stage_key = row.stage_key.clone();
    let stage_name = row.stage_name.clone();
    let stage_kind = row.stage_kind.clone();
    let cfg = row
.stage_config
.as_object()
.cloned()
.unwrap_or_default();
    let s = |k: &str| cfg.get(k).and_then(|v| v.as_str()).map(|x| x.to_string());
    let b = |k: &str, d: bool| cfg.get(k).and_then(|v| v.as_bool()).unwrap_or(d);
    let reviewer_kind = s("reviewerKind").unwrap_or_else(|| {
        if !b("requireApproval", false) {
            "any".to_string()
        } else {
            "human".to_string()
        }
    });
    ReviewItem {
        case: AttentionCaseDisplay {
            id: row.case_id.to_string(),
            case_key: row.case_key,
            title: row.case_title,
            summary: row.case_summary,
            version: row.case_version,
            terminal_kind: row.case_terminal_kind,
            updated_at: row.case_updated_at.as_datetime().to_rfc3339(),
            created_at: row.case_created_at.as_datetime().to_rfc3339(),
            pipeline: AttentionPipelineRef {
                id: row.pipeline_id.to_string(),
                key: row.pipeline_key,
                name: row.pipeline_name,
            },
            stage: AttentionStageRef {
                id: row.stage_id.to_string(),
                key: stage_key,
                name: stage_name,
                kind: stage_kind,
            },
        },
        review: ReviewConfig {
            expected_version: row.case_version,
            approve_to_stage_key: s("approveToStageKey"),
            reject_to_stage_key: s("rejectToStageKey"),
            request_changes_to_stage_key: s("requestChangesToStageKey"),
            require_reject_reason: b("requireRejectReason", true),
            require_request_changes_reason: b("requireRequestChangesReason", true),
            reviewer_kind,
        },
    }
}

/// list_pipeline_attention 主入口（suggestions + reviews；drift 留 R639.2.2）。
pub async fn list_pipeline_attention(
    pool: &PgPool,
    company_id: Uuid,
    caller: &AttentionCaller,
    limit: Option<i64>,
) -> sqlx::Result<PipelineAttention> {
    let bounded = bounded_limit(limit, PIPELINE_ATTENTION_DEFAULT_LIMIT, PIPELINE_ATTENTION_MAX_LIMIT);
    let suggestion_rows = list_suggestions(pool, company_id, bounded).await?;
    let review_rows = list_reviews(pool, company_id, caller, bounded).await?;
    let suggestions: Vec<SuggestionItem> = suggestion_rows
        .into_iter()
        .filter_map(suggestion_row_to_item)
        .collect();
    let reviews: Vec<ReviewItem> = review_rows.into_iter().map(review_row_to_item).collect();
    let counts = PipelineAttentionCounts {
        suggestions: suggestions.len(),
        reviews: reviews.len(),
    };
    Ok(PipelineAttention {
        suggestions,
        reviews,
        counts,
    })
}

/// 把 SQL identifier 安全 quote（防 caller.agent_id 含特殊字符）。
#[allow(dead_code)]
fn quote_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

#[allow(dead_code)]
fn _unused_query_builder_marker(_qb: &mut QueryBuilder<'_, Postgres>) {}
