//! 摘要槽位读取、生成任务和 revision 写入。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use pc_repos::issue::IssueRepo;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/summary-slots/:scope_kind/:slot_key",
            get(get_slot).put(write_slot),
        )
        .route(
            "/api/companies/:company_id/summary-slots/:scope_kind/:slot_key/revisions",
            get(list_revisions),
        )
        .route(
            "/api/companies/:company_id/summary-slots/:scope_kind/:slot_key/generate",
            post(generate_slot),
        )
}

#[derive(Debug, Deserialize)]
struct ScopeQuery {
    scope_id: Option<Uuid>,
}

#[derive(Debug, FromRow)]
struct SlotRow {
    id: Uuid,
    company_id: Uuid,
    scope_kind: String,
    scope_id: Option<Uuid>,
    slot_key: String,
    document_id: Option<Uuid>,
    status: String,
    failure_reason: Option<String>,
    generating_issue_id: Option<Uuid>,
    last_generated_at: Option<pc_core::Timestamp>,
    last_generated_by_agent_id: Option<Uuid>,
    last_model: Option<String>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

#[derive(Debug, FromRow)]
struct DocumentView {
    id: Uuid,
    company_id: Uuid,
    title: Option<String>,
    format: String,
    latest_body: String,
    latest_revision_id: Option<Uuid>,
    latest_revision_number: i32,
    created_by_agent_id: Option<Uuid>,
    created_by_user_id: Option<String>,
    updated_by_agent_id: Option<Uuid>,
    updated_by_user_id: Option<String>,
    created_at: pc_core::Timestamp,
    updated_at: pc_core::Timestamp,
}

#[derive(Debug, FromRow)]
struct RevisionView {
    id: Uuid,
    company_id: Uuid,
    document_id: Uuid,
    revision_number: i32,
    title: Option<String>,
    format: String,
    body: String,
    change_summary: Option<String>,
    created_by_agent_id: Option<Uuid>,
    created_by_user_id: Option<String>,
    created_by_run_id: Option<Uuid>,
    created_at: pc_core::Timestamp,
}

#[derive(Debug, FromRow)]
struct IssueView {
    id: Uuid,
    identifier: Option<String>,
    title: String,
    status: String,
    assignee_agent_id: Option<Uuid>,
}

fn slot_json(row: &SlotRow) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "scopeKind": row.scope_kind,
        "scopeId": row.scope_id,
        "slotKey": row.slot_key,
        "documentId": row.document_id,
        "status": row.status,
        "failureReason": row.failure_reason,
        "generatingIssueId": row.generating_issue_id,
        "lastGeneratedAt": row.last_generated_at,
        "lastGeneratedByAgentId": row.last_generated_by_agent_id,
        "lastModel": row.last_model,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at
    })
}

fn document_json(row: &DocumentView) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "title": row.title,
        "format": row.format,
        "body": row.latest_body,
        "latestRevisionId": row.latest_revision_id,
        "latestRevisionNumber": row.latest_revision_number,
        "createdByAgentId": row.created_by_agent_id,
        "createdByUserId": row.created_by_user_id,
        "updatedByAgentId": row.updated_by_agent_id,
        "updatedByUserId": row.updated_by_user_id,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at
    })
}

fn revision_json(row: &RevisionView) -> Value {
    json!({
        "id": row.id,
        "companyId": row.company_id,
        "documentId": row.document_id,
        "revisionNumber": row.revision_number,
        "title": row.title,
        "format": row.format,
        "body": row.body,
        "changeSummary": row.change_summary,
        "createdByAgentId": row.created_by_agent_id,
        "createdByUserId": row.created_by_user_id,
        "createdByRunId": row.created_by_run_id,
        "createdAt": row.created_at
    })
}

async fn find_slot(
    state: &AppState,
    company_id: Uuid,
    scope_kind: &str,
    slot_key: &str,
    scope_id: Option<Uuid>,
) -> sqlx::Result<Option<SlotRow>> {
    sqlx::query_as::<_, SlotRow>(
        "SELECT id, company_id, scope_kind, scope_id, slot_key, document_id, status, failure_reason, \
                generating_issue_id, last_generated_at, last_generated_by_agent_id, last_model, created_at, updated_at \
         FROM summary_slots WHERE company_id = $1 AND scope_kind = $2 AND slot_key = $3 \
           AND scope_id IS NOT DISTINCT FROM $4",
    )
    .bind(company_id)
    .bind(scope_kind)
    .bind(slot_key)
    .bind(scope_id)
    .fetch_optional(state.db.pool())
    .await
}

async fn get_slot(
    State(state): State<AppState>,
    Path((company_id, scope_kind, slot_key)): Path<(Uuid, String, String)>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<Value>> {
    let slot = find_slot(&state, company_id, &scope_kind, &slot_key, query.scope_id).await?;
    let Some(slot) = slot else {
        return Ok(Json(
            json!({ "slot": null, "document": null, "generatingIssue": null }),
        ));
    };
    let document = match slot.document_id {
        Some(document_id) => sqlx::query_as::<_, DocumentView>(
            "SELECT id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, \
                    created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id, created_at, updated_at \
             FROM documents WHERE id = $1 AND company_id = $2",
        )
        .bind(document_id)
        .bind(company_id)
        .fetch_optional(state.db.pool())
        .await?
        .map(|row| document_json(&row)),
        None => None,
    };
    let generating_issue = match slot.generating_issue_id {
        Some(issue_id) => sqlx::query_as::<_, IssueView>(
            "SELECT id, identifier, title, status, assignee_agent_id FROM issues WHERE id = $1 AND company_id = $2",
        )
        .bind(issue_id)
        .bind(company_id)
        .fetch_optional(state.db.pool())
        .await?
        .map(|row| {
            json!({ "id": row.id, "identifier": row.identifier, "title": row.title, "status": row.status, "assigneeAgentId": row.assignee_agent_id })
        }),
        None => None,
    };
    Ok(Json(json!({
        "slot": slot_json(&slot),
        "document": document,
        "generatingIssue": generating_issue
    })))
}

async fn list_revisions(
    State(state): State<AppState>,
    Path((company_id, scope_kind, slot_key)): Path<(Uuid, String, String)>,
    Query(query): Query<ScopeQuery>,
) -> ApiResult<Json<Value>> {
    let slot = find_slot(&state, company_id, &scope_kind, &slot_key, query.scope_id).await?;
    let Some(slot) = slot else {
        return Ok(Json(json!({ "slot": null, "revisions": [] })));
    };
    let revisions = match slot.document_id {
        Some(document_id) => sqlx::query_as::<_, RevisionView>(
            "SELECT id, company_id, document_id, revision_number, title, format, body, change_summary, \
                    created_by_agent_id, created_by_user_id, created_by_run_id, created_at \
             FROM document_revisions WHERE company_id = $1 AND document_id = $2 \
             ORDER BY revision_number DESC LIMIT 20",
        )
        .bind(company_id)
        .bind(document_id)
        .fetch_all(state.db.pool())
        .await?
        .iter()
        .map(revision_json)
        .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    Ok(Json(
        json!({ "slot": slot_json(&slot), "revisions": revisions }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteBody {
    scope_id: Option<Uuid>,
    markdown: String,
    title: Option<String>,
    change_summary: Option<String>,
    base_revision_id: Option<Uuid>,
    generation_issue_id: Option<Uuid>,
    model: Option<String>,
}

async fn ensure_summary_slot(
    state: &AppState,
    company_id: Uuid,
    scope_kind: &str,
    slot_key: &str,
    scope_id: Option<Uuid>,
) -> ApiResult<SlotRow> {
    if let Some(slot) = find_slot(state, company_id, scope_kind, slot_key, scope_id).await? {
        return Ok(slot);
    }
    Ok(sqlx::query_as::<_, SlotRow>(
        "INSERT INTO summary_slots (company_id, scope_kind, scope_id, slot_key, status) \
         VALUES ($1,$2,$3,$4,'idle') RETURNING id, company_id, scope_kind, scope_id, slot_key, document_id, status, failure_reason, \
         generating_issue_id, last_generated_at, last_generated_by_agent_id, last_model, created_at, updated_at",
    )
    .bind(company_id)
    .bind(scope_kind)
    .bind(scope_id)
    .bind(slot_key)
    .fetch_one(state.db.pool())
    .await?)
}

async fn check_base_revision(
    state: &AppState,
    company_id: Uuid,
    slot: &SlotRow,
    base_revision_id: Uuid,
) -> ApiResult<()> {
    let Some(document_id) = slot.document_id else {
        return Ok(());
    };
    if document_id == Uuid::nil() {
        return Ok(());
    }
    let latest: Option<(Uuid,)> = sqlx::query_as(
        "SELECT latest_revision_id FROM documents WHERE id = $1 AND company_id = $2",
    )
    .bind(document_id)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await?;
    if latest
        .map(|(id,)| id)
        .is_some_and(|id| id != base_revision_id)
    {
        return Err(ApiError::BadRequest(
            "summary was updated by someone else".to_owned(),
        ));
    }
    Ok(())
}

async fn upsert_document(
    state: &AppState,
    company_id: Uuid,
    slot: &SlotRow,
    body: &WriteBody,
    now: chrono::DateTime<Utc>,
) -> ApiResult<DocumentView> {
    if let Some(document_id) = slot.document_id {
        Ok(sqlx::query_as::<_, DocumentView>(
            "UPDATE documents SET title = $2, latest_body = $3, latest_revision_number = latest_revision_number + 1, \
             updated_by_agent_id = NULL, updated_at = $4 WHERE id = $1 AND company_id = $5 \
             RETURNING id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, \
             created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id, created_at, updated_at",
        )
        .bind(document_id)
        .bind(body.title.as_deref())
        .bind(&body.markdown)
        .bind(now)
        .bind(company_id)
        .fetch_one(state.db.pool())
        .await?)
    } else {
        Ok(sqlx::query_as::<_, DocumentView>(
            "INSERT INTO documents (company_id, title, format, latest_body, created_at, updated_at) \
             VALUES ($1,$2,'markdown',$3,$4,$4) RETURNING id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, \
             created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id, created_at, updated_at",
        )
        .bind(company_id)
        .bind(body.title.as_deref())
        .bind(&body.markdown)
        .bind(now)
        .fetch_one(state.db.pool())
        .await?)
    }
}

async fn insert_revision(
    state: &AppState,
    company_id: Uuid,
    document_id: Uuid,
    revision_number: i32,
    body: &WriteBody,
    now: chrono::DateTime<Utc>,
) -> ApiResult<RevisionView> {
    Ok(sqlx::query_as::<_, RevisionView>(
        "INSERT INTO document_revisions (company_id, document_id, revision_number, title, format, body, change_summary, created_at) \
         VALUES ($1,$2,$3,$4,'markdown',$5,$6,$7) RETURNING id, company_id, document_id, revision_number, title, format, body, change_summary, \
         created_by_agent_id, created_by_user_id, created_by_run_id, created_at",
    )
    .bind(company_id)
    .bind(document_id)
    .bind(revision_number)
    .bind(body.title.as_deref())
    .bind(&body.markdown)
    .bind(body.change_summary.as_deref())
    .bind(now)
    .fetch_one(state.db.pool())
    .await?)
}

async fn link_revision(
    state: &AppState,
    document_id: Uuid,
    revision_id: Uuid,
    revision_number: i32,
) -> ApiResult<DocumentView> {
    Ok(sqlx::query_as::<_, DocumentView>(
        "UPDATE documents SET latest_revision_id = $2, latest_revision_number = $3 WHERE id = $1 \
         RETURNING id, company_id, title, format, latest_body, latest_revision_id, latest_revision_number, \
         created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id, created_at, updated_at",
    )
    .bind(document_id)
    .bind(revision_id)
    .bind(revision_number)
    .fetch_one(state.db.pool())
    .await?)
}

async fn mark_slot_written(
    state: &AppState,
    slot_id: Uuid,
    document_id: Uuid,
    model: Option<&str>,
    now: chrono::DateTime<Utc>,
) -> ApiResult<SlotRow> {
    Ok(sqlx::query_as::<_, SlotRow>(
        "UPDATE summary_slots SET document_id=$2, status='idle', failure_reason=NULL, generating_issue_id=NULL, \
         last_generated_at=$3, last_model=$4, updated_at=$3 WHERE id=$1 RETURNING id, company_id, scope_kind, scope_id, slot_key, document_id, status, failure_reason, \
         generating_issue_id, last_generated_at, last_generated_by_agent_id, last_model, created_at, updated_at",
    )
    .bind(slot_id)
    .bind(document_id)
    .bind(now)
    .bind(model)
    .fetch_one(state.db.pool())
    .await?)
}

async fn write_slot(
    State(state): State<AppState>,
    Path((company_id, scope_kind, slot_key)): Path<(Uuid, String, String)>,
    Query(query): Query<ScopeQuery>,
    Json(body): Json<WriteBody>,
) -> ApiResult<Json<Value>> {
    if body.markdown.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "markdown must not be empty".to_owned(),
        ));
    }
    let scope_id = body.scope_id.or(query.scope_id);
    let slot = ensure_summary_slot(&state, company_id, &scope_kind, &slot_key, scope_id).await?;
    if let Some(base_revision_id) = body.base_revision_id {
        check_base_revision(&state, company_id, &slot, base_revision_id).await?;
    }
    let now = Utc::now();
    let document = upsert_document(&state, company_id, &slot, &body, now).await?;
    let revision_number = document.latest_revision_number;
    let revision =
        insert_revision(&state, company_id, document.id, revision_number, &body, now).await?;
    let document = link_revision(&state, document.id, revision.id, revision_number).await?;
    let slot = mark_slot_written(&state, slot.id, document.id, body.model.as_deref(), now).await?;
    let _ = body.generation_issue_id;
    Ok(Json(
        json!({ "slot": slot_json(&slot), "document": document_json(&document), "revision": revision_json(&revision) }),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GenerateBody {
    scope_id: Option<Uuid>,
}

async fn generate_slot(
    State(state): State<AppState>,
    Path((company_id, scope_kind, slot_key)): Path<(Uuid, String, String)>,
    Query(query): Query<ScopeQuery>,
    Json(body): Json<Option<GenerateBody>>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let scope_id = body.and_then(|value| value.scope_id).or(query.scope_id);
    let existing = find_slot(&state, company_id, &scope_kind, &slot_key, scope_id).await?;
    if let Some(slot) = existing.as_ref() {
        if slot.status == "generating" {
            let issue = sqlx::query_as::<_, IssueView>(
                "SELECT id, identifier, title, status, assignee_agent_id FROM issues WHERE id = $1",
            )
            .bind(slot.generating_issue_id)
            .fetch_optional(state.db.pool())
            .await?;
            return Ok((
                StatusCode::OK,
                Json(json!({
                    "slot": slot_json(slot),
                    "generatingIssue": issue.map(|row| json!({ "id": row.id, "identifier": row.identifier, "title": row.title, "status": row.status, "assigneeAgentId": row.assignee_agent_id })),
                    "alreadyGenerating": true
                })),
            ));
        }
    }
    let issue = IssueRepo::new(&state.db)
        .create(
            company_id,
            &format!("Generate {scope_kind} {slot_key} summary"),
            Some("Summary generation task created by the Rust summary-slot API."),
            "medium",
            None,
        )
        .await?;
    let slot = if let Some(slot) = existing {
        sqlx::query_as::<_, SlotRow>(
            "UPDATE summary_slots SET status='generating', generating_issue_id=$2, updated_at=now() WHERE id=$1 \
             RETURNING id, company_id, scope_kind, scope_id, slot_key, document_id, status, failure_reason, generating_issue_id, \
             last_generated_at, last_generated_by_agent_id, last_model, created_at, updated_at",
        )
        .bind(slot.id)
        .bind(issue.id)
        .fetch_one(state.db.pool())
        .await?
    } else {
        sqlx::query_as::<_, SlotRow>(
            "INSERT INTO summary_slots (company_id, scope_kind, scope_id, slot_key, status, generating_issue_id) \
             VALUES ($1,$2,$3,$4,'generating',$5) RETURNING id, company_id, scope_kind, scope_id, slot_key, document_id, status, failure_reason, generating_issue_id, \
             last_generated_at, last_generated_by_agent_id, last_model, created_at, updated_at",
        )
        .bind(company_id)
        .bind(&scope_kind)
        .bind(scope_id)
        .bind(&slot_key)
        .bind(issue.id)
        .fetch_one(state.db.pool())
        .await?
    };
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "slot": slot_json(&slot),
            "generatingIssue": { "id": issue.id, "identifier": issue.identifier, "title": issue.title, "status": issue.status, "assigneeAgentId": issue.assignee_agent_id },
            "alreadyGenerating": false
        })),
    ))
}
