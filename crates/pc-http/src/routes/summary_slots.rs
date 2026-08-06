//! 摘要槽位读取、生成任务和 revision 写入。

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use pc_repos::document::{DocumentRepo, DocumentRevisionRow};
use pc_repos::issue::IssueRepo;
use pc_repos::summary::{SummaryRepo, SummarySlotRow};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::document::DocumentRow;

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
#[allow(dead_code)]
struct ScopeQuery {
    scope_id: Option<Uuid>,
}

fn slot_json(row: &SummarySlotRow) -> Value {
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

fn document_json(row: &DocumentRow) -> Value {
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

fn revision_json(row: &DocumentRevisionRow) -> Value {
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
) -> sqlx::Result<Option<SummarySlotRow>> {
    SummaryRepo::new(&state.db)
        .find_by_scope_str(company_id, scope_kind, slot_key, scope_id)
        .await
        .map_err(|e| {
            sqlx::Error::Decode(Box::new(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            )))
        })
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
        Some(document_id) => DocumentRepo::new(&state.db)
            .get_in_company(company_id, document_id)
            .await?
            .map(|row| document_json(&row)),
        None => None,
    };
    let generating_issue = match slot.generating_issue_id {
        Some(issue_id) => IssueRepo::new(&state.db)
            .get(issue_id)
            .await?
            .filter(|row| row.company_id == company_id)
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
        Some(document_id) => DocumentRepo::new(&state.db)
            .list_revisions_in_company(company_id, document_id, 20)
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
) -> ApiResult<SummarySlotRow> {
    if let Some(slot) = find_slot(state, company_id, scope_kind, slot_key, scope_id).await? {
        return Ok(slot);
    }
    Ok(SummaryRepo::new(&state.db)
        .insert_idle(company_id, scope_kind, scope_id, slot_key)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?)
}

async fn check_base_revision(
    state: &AppState,
    company_id: Uuid,
    slot: &SummarySlotRow,
    base_revision_id: Uuid,
) -> ApiResult<()> {
    let Some(document_id) = slot.document_id else {
        return Ok(());
    };
    if document_id == Uuid::nil() {
        return Ok(());
    }
    let latest_id = DocumentRepo::new(&state.db)
        .latest_revision_id_in_company(company_id, document_id)
        .await?;
    if latest_id.is_some_and(|id| id != base_revision_id) {
        return Err(ApiError::BadRequest(
            "summary was updated by someone else".to_owned(),
        ));
    }
    Ok(())
}

async fn upsert_document(
    state: &AppState,
    company_id: Uuid,
    slot: &SummarySlotRow,
    body: &WriteBody,
    now: chrono::DateTime<Utc>,
) -> ApiResult<DocumentRow> {
    let doc_repo = DocumentRepo::new(&state.db);
    if let Some(document_id) = slot.document_id {
        Ok(doc_repo
            .write_body(
                company_id,
                document_id,
                body.title.as_deref(),
                &body.markdown,
                now,
            )
            .await?)
    } else {
        Ok(doc_repo
            .create_markdown(company_id, body.title.as_deref(), &body.markdown, now)
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
) -> ApiResult<DocumentRevisionRow> {
    Ok(DocumentRepo::new(&state.db)
        .insert_revision_full(
            company_id,
            document_id,
            revision_number,
            body.title.as_deref(),
            &body.markdown,
            body.change_summary.as_deref(),
            now,
        )
        .await?)
}

async fn link_revision(
    state: &AppState,
    document_id: Uuid,
    revision_id: Uuid,
    revision_number: i32,
) -> ApiResult<DocumentRow> {
    Ok(DocumentRepo::new(&state.db)
        .set_latest_revision(document_id, revision_id, revision_number)
        .await?)
}

async fn mark_slot_written(
    state: &AppState,
    slot_id: Uuid,
    document_id: Uuid,
    model: Option<&str>,
    now: chrono::DateTime<Utc>,
) -> ApiResult<SummarySlotRow> {
    Ok(SummaryRepo::new(&state.db)
        .mark_slot_written(slot_id, document_id, now, model)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?)
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
            let issue = match slot.generating_issue_id {
                Some(issue_id) => IssueRepo::new(&state.db).get(issue_id).await?,
                None => None,
            };
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
    let repo = SummaryRepo::new(&state.db);
    let slot = if let Some(slot) = existing {
        repo.update_to_generating(slot.id, issue.id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
    } else {
        repo.insert_generating(company_id, &scope_kind, scope_id, &slot_key, issue.id)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?
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
