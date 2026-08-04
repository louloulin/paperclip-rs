//! `/api/cases*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_core::Timestamp;
use pc_realtime::LiveEvent;
use pc_repos::case::CaseRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/cases", get(list).post(create))
        .route("/api/cases/:case_id", get(get_one).patch(update).delete(remove))
        .route(
            "/api/companies/:company_id/cases",
            get(list_company_cases).post(create_company_case),
        )
        .route("/api/cases/:case_id/events", get(list_case_events))
        .route("/api/cases/:case_id/links", post(create_case_link))
        .route(
            "/api/cases/:case_id/documents",
            get(list_case_documents).put(upsert_case_document),
        )
        .route("/api/cases/:case_id/documents/:key", get(get_case_document))
        .route(
            "/api/cases/:case_id/documents/:key/lock",
            post(lock_case_document),
        )
        .route(
            "/api/cases/:case_id/documents/:key/unlock",
            post(unlock_case_document),
        )
        .route(
            "/api/cases/:case_id/documents/:key/annotations",
            get(list_case_annotations),
        )
        // ── Round 22: case annotations / revisions / attachments / issue case-links ──
        .route(
            "/api/cases/:case_id/documents/:key/annotations/threads",
            get(list_case_annotation_threads),
        )
        .route(
            "/api/cases/:case_id/documents/:key/annotations/threads",
            post(create_case_annotation_thread),
        )
        .route(
            "/api/cases/:case_id/documents/:key/annotations/threads/:thread_id",
            get(get_case_annotation_thread),
        )
        .route(
            "/api/cases/:case_id/documents/:key/annotations/threads/:thread_id",
            patch(patch_case_annotation_thread),
        )
        .route(
            "/api/cases/:case_id/documents/:key/annotations/threads/:thread_id/comments",
            post(add_case_annotation_comment),
        )
        .route(
            "/api/cases/:case_id/documents/:key",
            delete(delete_case_document),
        )
        .route(
            "/api/cases/:case_id/documents/:key/revisions",
            get(list_case_document_revisions),
        )
        .route(
            "/api/cases/:case_id/documents/:key/revisions/:revision_id/restore",
            post(restore_case_document_revision),
        )
        .route(
            "/api/cases/:case_id/attachments",
            post(create_case_attachment),
        )
        .route(
            "/api/issues/:issue_id/cases",
            get(list_issue_cases),
        )
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
}

async fn list(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let rows = match q.company_id {
        Some(cid) => CaseRepo::new(&state.db).list_by_company(cid).await?,
        None => CaseRepo::new(&state.db).list_all(200).await?,
    };
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn get_one(State(state): State<AppState>, Path(case_id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    case_type: String,
    title: String,
    #[serde(default)]
    project_id: Option<Uuid>,
    #[serde(default)]
    summary: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = CaseRepo::new(&state.db)
        .create(
            body.company_id,
            &body.case_type,
            &body.title,
            body.project_id,
            body.summary.as_deref(),
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("case.created", "case", row.id).with_company(row.company_id));
    let response = serde_json::json!({
            "id": row.id, "company_id": row.company_id, "title": row.title,
            "case_type": row.case_type, "status": row.status, "identifier": row.identifier
        });
    Ok((StatusCode::CREATED, Json(response)))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

async fn update(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let row = CaseRepo::new(&state.db)
        .update(
            case_id,
            body.title.as_deref(),
            body.summary.as_deref(),
            body.status.as_deref(),
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    state
        .realtime
        .publish(LiveEvent::new("case.updated", "case", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn remove(State(state): State<AppState>, Path(case_id): Path<Uuid>) -> ApiResult<StatusCode> {
    let ok = CaseRepo::new(&state.db).delete(case_id).await?;
    if ok {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("case {case_id}")))
    }
}


// ============== Sub-resource handlers ==============

async fn list_company_cases(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = CaseRepo::new(&state.db)
        .list_by_company(company_id)
        .await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

async fn create_company_case(
    State(state): State<AppState>,
    Path(company_id): Path<Uuid>,
    Json(body): Json<CreateBody>,
) -> ApiResult<Json<Value>> {
    if body.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title must not be empty".into()));
    }
    let row = CaseRepo::new(&state.db)
        .create(
            company_id,
            &body.case_type,
            &body.title,
            body.project_id,
            body.summary.as_deref(),
        )
        .await?;
    state
        .realtime
        .publish(LiveEvent::new("case.created", "case", row.id).with_company(row.company_id));
    Ok(Json(serde_json::to_value(row).unwrap_or_default()))
}

async fn list_case_events(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<(Uuid, String, String, Option<String>, Option<Uuid>, Option<Uuid>, Value, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at          FROM case_events WHERE case_id = $1 ORDER BY created_at DESC LIMIT $2",
    )
    .bind(case_id)
    .bind(limit)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at)| {
            json!({
                "id": id,
                "kind": kind,
                "actorType": actor_type,
                "actorUserId": actor_user_id,
                "actorAgentId": actor_agent_id,
                "runId": run_id,
                "payload": payload,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn create_case_link(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<CreateCaseLinkBody>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let role = body.role.unwrap_or_else(|| "reference".to_string());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO case_issue_links (company_id, case_id, issue_id, role)          VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(body.issue_id)
    .bind(&role)
    .fetch_one(state.db.pool())
    .await?;
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)          VALUES ($1, $2, 'issue_linked', 'user', jsonb_build_object('issueId',$3::text,'role',$4::text))",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(body.issue_id.to_string())
    .bind(&role)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.issue_linked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"issueId": body.issue_id, "role": role})),
    );
    Ok(Json(json!({ "id": id, "caseId": case_id, "issueId": body.issue_id, "role": role })))
}

async fn list_case_documents(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, String, Option<Timestamp>)> = sqlx::query_as(
        "SELECT id, document_id, key, created_at FROM case_documents          WHERE case_id = $1 ORDER BY created_at DESC",
    )
    .bind(case_id)
    .fetch_all(state.db.pool())
    .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, document_id, key, created_at)| {
            json!({"id": id, "documentId": document_id, "key": key, "createdAt": created_at})
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn upsert_case_document(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<UpsertCaseDocumentBody>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO case_documents (company_id, case_id, document_id, key)          VALUES ($1, $2, $3, $4)          ON CONFLICT (case_id, key) DO UPDATE SET document_id = EXCLUDED.document_id, updated_at = now()          RETURNING id",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(body.document_id)
    .bind(&body.key)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.document.upserted", "case", case_id)
            .with_company(case_row.company_id),
    );
    Ok(Json(json!({"id": id, "caseId": case_id, "key": body.key, "documentId": body.document_id})))
}

async fn get_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, document_id FROM case_documents WHERE case_id = $1 AND key = $2",
    )
    .bind(case_id)
    .bind(&key)
    .fetch_optional(state.db.pool())
    .await?;
    let (id, document_id) = row.ok_or_else(|| ApiError::NotFound(format!("case document {key}")))?;
    Ok(Json(json!({"id": id, "caseId": case_id, "key": key, "documentId": document_id})))
}

async fn lock_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let _: Uuid = sqlx::query_scalar(
        "UPDATE case_documents SET updated_at = now() WHERE case_id = $1 AND key = $2 RETURNING id",
    )
    .bind(case_id)
    .bind(&key)
    .fetch_one(state.db.pool())
    .await?;
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)          VALUES ($1, $2, 'document_locked', 'user', jsonb_build_object('key',$3::text))",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(&key)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.document.locked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"key": key})),
    );
    Ok(Json(json!({"locked": true, "caseId": case_id, "key": key})))
}

async fn unlock_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)          VALUES ($1, $2, 'document_unlocked', 'user', jsonb_build_object('key',$3::text))",
    )
    .bind(case_row.company_id)
    .bind(case_id)
    .bind(&key)
    .execute(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.document.unlocked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"key": key})),
    );
    Ok(Json(json!({"unlocked": true, "caseId": case_id, "key": key})))
}

async fn list_case_annotations(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    // Mirrors Node `/cases/:id/documents/:key/annotations`. Annotations live
    // in the `document_annotations` table; we filter by case-bound document
    // and key. Empty array when no rows exist (UI tolerates this).
    let rows: Vec<(Uuid, String, Option<String>, Value)> = sqlx::query_as(
        "SELECT id, kind, thread_id, payload FROM document_annotations          WHERE document_id IN (SELECT document_id FROM case_documents WHERE case_id = $1 AND key = $2)          ORDER BY created_at DESC LIMIT 200",
    )
    .bind(case_id)
    .bind(&key)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, thread_id, payload)| {
            json!({"id": id, "kind": kind, "threadId": thread_id, "payload": payload})
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CreateCaseLinkBody {
    issue_id: Uuid,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpsertCaseDocumentBody {
    document_id: Uuid,
    key: String,
}

// ============== Round 22: case annotations / revisions / attachments / issue case-links ==============

// ── Helpers ──────────────────────────────────────────────────

async fn resolve_case_document_id(
    state: &AppState,
    case_id: Uuid,
    key: &str,
) -> ApiResult<(Uuid, Uuid)> {
    // Returns (company_id, document_id) for the (case, key) pair.
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT company_id, document_id FROM case_documents WHERE case_id = $1 AND key = $2",
    )
    .bind(case_id)
    .bind(key)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    row.ok_or_else(|| ApiError::NotFound(format!("case document {case_id}:{key}")))
}

async fn ensure_case_exists(state: &AppState, case_id: Uuid) -> ApiResult<Uuid> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM cases WHERE id = $1")
        .bind(case_id)
        .fetch_optional(state.db.pool())
        .await
        .ok()
        .flatten();
    row.map(|(c,)| c)
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))
}

// ── Case annotation threads ──────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAnnotationThreadsQuery {
    status: Option<String>,
    include_comments: Option<bool>,
}

async fn list_case_annotation_threads(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
    axum::extract::Query(q): axum::extract::Query<ListAnnotationThreadsQuery>,
) -> ApiResult<Json<Value>> {
    let company_id = ensure_case_exists(&state, case_id).await?;
    let (status_filter, include_comments) = (q.status, q.include_comments.unwrap_or(false));
    let mut sql = String::from(
        "SELECT id, company_id, case_id, document_id, document_key, status, anchor_state, \
                original_revision_id, original_revision_number, current_revision_id, current_revision_number, \
                selected_text, prefix_text, suffix_text, normalized_start, normalized_end, \
                markdown_start, markdown_end, anchor_confidence, anchor_selector, \
                resolved_at, resolved_by_user_id, resolved_by_agent_id, \
                created_by_user_id, created_by_agent_id, created_at, updated_at \
         FROM document_annotation_threads WHERE case_id = $1 AND document_key = $2",
    );
    if let Some(s) = status_filter.as_deref() {
        if s == "open" || s == "resolved" {
            sql.push_str(&format!(" AND status = '{}'", s));
        } else if s != "all" {
            // ignore unknown filter
        }
    }
    sql.push_str(" ORDER BY created_at DESC LIMIT 200");
    let rows: Vec<(
        Uuid, Uuid, Uuid, Uuid, String, String, String,
        Option<Uuid>, i32, Option<Uuid>, i32,
        String, String, String, i32, i32, i32, i32,
        String, Value,
        Option<chrono::DateTime<chrono::Utc>>>, Option<String>, Option<Uuid>,
        Option<String>, Option<Uuid>,
        chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(&sql)
        .bind(case_id)
        .bind(&key)
        .fetch_all(state.db.pool())
        .await
        .unwrap_or_default();
    let mut items: Vec<Value> = rows
        .into_iter()
        .map(|(id, _cid, _case_id, _doc_id, document_key, status, anchor_state,
                orig_rev_id, orig_rev_no, curr_rev_id, curr_rev_no,
                selected_text, prefix_text, suffix_text, norm_start, norm_end, md_start, md_end,
                anchor_confidence, anchor_selector,
                resolved_at, resolved_by_user_id, resolved_by_agent_id,
                created_by_user_id, created_by_agent_id,
                created_at, updated_at)| {
            json!({
                "id": id,
                "documentKey": document_key,
                "status": status,
                "anchorState": anchor_state,
                "originalRevisionId": orig_rev_id,
                "originalRevisionNumber": orig_rev_no,
                "currentRevisionId": curr_rev_id,
                "currentRevisionNumber": curr_rev_no,
                "selectedText": selected_text,
                "prefixText": prefix_text,
                "suffixText": suffix_text,
                "normalizedStart": norm_start,
                "normalizedEnd": norm_end,
                "markdownStart": md_start,
                "markdownEnd": md_end,
                "anchorConfidence": anchor_confidence,
                "anchorSelector": anchor_selector,
                "resolvedAt": resolved_at,
                "resolvedByUserId": resolved_by_user_id,
                "resolvedByAgentId": resolved_by_agent_id,
                "createdByUserId": created_by_user_id,
                "createdByAgentId": created_by_agent_id,
                "createdAt": created_at,
                "updatedAt": updated_at,
            })
        })
        .collect();

    if include_comments {
        // Load comments for each thread and inline
        let thread_ids: Vec<Uuid> = items
            .iter()
            .map(|v| v.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()))
            .flatten()
            .collect();
        if !thread_ids.is_empty() {
            let comments: Vec<(Uuid, Uuid, String, String, Option<Uuid>, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
                "SELECT id, thread_id, body, author_type, author_agent_id, author_user_id, created_at \
                 FROM document_annotation_comments \
                 WHERE company_id = $1 AND case_id = $2 AND thread_id = ANY($3::uuid[]) \
                 ORDER BY created_at ASC",
            )
            .bind(company_id)
            .bind(case_id)
            .bind(&thread_ids)
            .fetch_all(state.db.pool())
            .await
            .unwrap_or_default();
            for t in items.iter_mut() {
                let tid = t.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok());
                let cs: Vec<Value> = comments
                    .iter()
                    .filter(|c| Some(c.1) == tid)
                    .map(|(id, _tid, body, author_type, author_agent_id, author_user_id, created_at)| {
                        json!({
                            "id": id,
                            "body": body,
                            "authorType": author_type,
                            "authorAgentId": author_agent_id,
                            "authorUserId": author_user_id,
                            "createdAt": created_at,
                        })
                    })
                    .collect();
                t["comments"] = json!(cs);
            }
        }
    }
    Ok(Json(json!({
        "caseId": case_id,
        "documentKey": key,
        "threads": items,
        "items": items,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCaseAnnotationThreadBody {
    selected_text: String,
    prefix_text: Option<String>,
    suffix_text: Option<String>,
    normalized_start: Option<i32>,
    normalized_end: Option<i32>,
    markdown_start: Option<i32>,
    markdown_end: Option<i32>,
    anchor_confidence: Option<String>,
    anchor_selector: Option<Value>,
    body: Option<String>,
    revision_number: Option<i32>,
    document_id: Option<Uuid>,
    status: Option<String>,
}

async fn create_case_annotation_thread(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
    Json(body): Json<CreateCaseAnnotationThreadBody>,
) -> ApiResult<impl IntoResponse> {
    if body.selected_text.is_empty() {
        return Err(ApiError::BadRequest("selectedText is required".into()));
    }
    let company_id = ensure_case_exists(&state, case_id).await?;
    let (doc_company_id, document_id) = resolve_case_document_id(&state, case_id, &key).await?;
    if doc_company_id != company_id {
        return Err(ApiError::BadRequest("case/document company mismatch".into()));
    }
    let norm_start = body.normalized_start.unwrap_or(0);
    let norm_end = body.normalized_end.unwrap_or(body.selected_text.len() as i32);
    let md_start = body.markdown_start.unwrap_or(0);
    let md_end = body.markdown_end.unwrap_or(body.selected_text.len() as i32);
    let confidence = body.anchor_confidence.unwrap_or_else(|| "exact".to_owned());
    let selector = body.anchor_selector.clone().unwrap_or_else(|| json!({}));
    let revision_number = body.revision_number.unwrap_or(1);
    let thread_id: Uuid = sqlx::query_scalar(
        "INSERT INTO document_annotation_threads (company_id, case_id, document_id, document_key, status, anchor_state, original_revision_number, current_revision_number, selected_text, prefix_text, suffix_text, normalized_start, normalized_end, markdown_start, markdown_end, anchor_confidence, anchor_selector) \
         VALUES ($1, $2, $3, $4, COALESCE($5, 'open'), 'active', $6, $6, $7, COALESCE($8, ''), COALESCE($9, ''), $10, $11, $12, $13, $14, $15) RETURNING id",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(document_id)
    .bind(&key)
    .bind(body.status.as_deref())
    .bind(revision_number)
    .bind(&body.selected_text)
    .bind(body.prefix_text.as_deref().unwrap_or(""))
    .bind(body.suffix_text.as_deref().unwrap_or(""))
    .bind(norm_start)
    .bind(norm_end)
    .bind(md_start)
    .bind(md_end)
    .bind(&confidence)
    .bind(&selector)
    .fetch_one(state.db.pool())
    .await?;

    if let Some(initial_body) = body.body.as_deref() {
        if !initial_body.is_empty() {
            sqlx::query(
                "INSERT INTO document_annotation_comments (company_id, case_id, thread_id, document_id, body, author_type) \
                 VALUES ($1, $2, $3, $4, $5, 'user')",
            )
            .bind(company_id)
            .bind(case_id)
            .bind(thread_id)
            .bind(document_id)
            .bind(initial_body)
            .execute(state.db.pool())
            .await?;
        }
    }

    state.realtime.publish(
        LiveEvent::new("case.annotation.created", "case_annotation", thread_id)
            .with_company(company_id)
            .with_data(json!({"caseId": case_id, "documentKey": key})),
    );

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": thread_id,
            "caseId": case_id,
            "documentKey": key,
            "status": body.status.unwrap_or_else(|| "open".to_owned()),
            "selectedText": body.selected_text,
            "anchorConfidence": confidence,
            "anchorSelector": selector,
        })),
    ))
}

async fn get_case_annotation_thread(
    State(state): State<AppState>,
    Path((case_id, key, thread_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let company_id = ensure_case_exists(&state, case_id).await?;
    let row: Option<(
        Uuid, Uuid, String, String, String, i32, i32,
        String, Value,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<Uuid>, Option<String>,
        chrono::DateTime<chrono::Utc>,
    )> = sqlx::query_as(
        "SELECT id, document_id, document_key, status, anchor_confidence, normalized_start, normalized_end, selected_text, anchor_selector, resolved_at, resolved_by_agent_id, resolved_by_user_id, created_at \
         FROM document_annotation_threads WHERE id = $1 AND case_id = $2 AND document_key = $3",
    )
    .bind(thread_id)
    .bind(case_id)
    .bind(&key)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (id, document_id, document_key, status, anchor_confidence, normalized_start, normalized_end, selected_text, anchor_selector, resolved_at, resolved_by_agent_id, resolved_by_user_id, created_at) = row
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;

    let comments: Vec<(Uuid, String, String, Option<Uuid>, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, body, author_type, author_agent_id, author_user_id, created_at \
         FROM document_annotation_comments \
         WHERE company_id = $1 AND case_id = $2 AND thread_id = $3 \
         ORDER BY created_at ASC",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(thread_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let comment_items: Vec<Value> = comments
        .into_iter()
        .map(|(id, body, author_type, author_agent_id, author_user_id, created_at)| {
            json!({
                "id": id,
                "body": body,
                "authorType": author_type,
                "authorAgentId": author_agent_id,
                "authorUserId": author_user_id,
                "createdAt": created_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "id": id,
        "caseId": case_id,
        "documentId": document_id,
        "documentKey": document_key,
        "status": status,
        "anchorConfidence": anchor_confidence,
        "normalizedStart": normalized_start,
        "normalizedEnd": normalized_end,
        "selectedText": selected_text,
        "anchorSelector": anchor_selector,
        "resolvedAt": resolved_at,
        "resolvedByAgentId": resolved_by_agent_id,
        "resolvedByUserId": resolved_by_user_id,
        "createdAt": created_at,
        "comments": comment_items,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchCaseAnnotationThreadBody {
    status: Option<String>,
    anchor_selector: Option<Value>,
    anchor_state: Option<String>,
    current_revision_id: Option<Uuid>,
    current_revision_number: Option<i32>,
}

async fn patch_case_annotation_thread(
    State(state): State<AppState>,
    Path((case_id, key, thread_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<PatchCaseAnnotationThreadBody>,
) -> ApiResult<Json<Value>> {
    let company_id = ensure_case_exists(&state, case_id).await?;
    // Validate status if provided
    if let Some(s) = body.status.as_deref() {
        if !matches!(s, "open" | "resolved" | "outdated") {
            return Err(ApiError::BadRequest(format!("invalid status '{s}'")));
        }
    }
    let affected = sqlx::query(
        "UPDATE document_annotation_threads SET \
            status = COALESCE($1, status), \
            anchor_selector = COALESCE($2, anchor_selector), \
            anchor_state = COALESCE($3, anchor_state), \
            current_revision_id = COALESCE($4, current_revision_id), \
            current_revision_number = COALESCE($5, current_revision_number), \
            resolved_at = CASE WHEN $1 = 'resolved' THEN now() WHEN $1 IN ('open', 'outdated') THEN NULL ELSE resolved_at END, \
            updated_at = now() \
         WHERE id = $6 AND case_id = $7 AND document_key = $8",
    )
    .bind(body.status.as_deref())
    .bind(body.anchor_selector.clone())
    .bind(body.anchor_state.as_deref())
    .bind(body.current_revision_id)
    .bind(body.current_revision_number)
    .bind(thread_id)
    .bind(case_id)
    .bind(&key)
    .execute(state.db.pool())
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound(format!("annotation thread {thread_id}")));
    }
    state.realtime.publish(
        LiveEvent::new("case.annotation.updated", "case_annotation", thread_id)
            .with_company(company_id)
            .with_data(json!({"caseId": case_id, "documentKey": key, "status": body.status})),
    );
    Ok(Json(json!({
        "id": thread_id,
        "caseId": case_id,
        "documentKey": key,
        "updated": true,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddCaseAnnotationCommentBody {
    body: String,
    author_type: Option<String>,
    author_user_id: Option<String>,
    author_agent_id: Option<Uuid>,
}

async fn add_case_annotation_comment(
    State(state): State<AppState>,
    Path((case_id, key, thread_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<AddCaseAnnotationCommentBody>,
) -> ApiResult<impl IntoResponse> {
    if body.body.trim().is_empty() {
        return Err(ApiError::BadRequest("body is required".into()));
    }
    let company_id = ensure_case_exists(&state, case_id).await?;
    // Verify thread exists for case+key
    let thread_exists: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, document_id FROM document_annotation_threads WHERE id = $1 AND case_id = $2 AND document_key = $3",
    )
    .bind(thread_id)
    .bind(case_id)
    .bind(&key)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (_id, document_id) = thread_exists
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;

    let author_type = body.author_type.unwrap_or_else(|| "user".to_owned());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO document_annotation_comments (company_id, case_id, thread_id, document_id, body, author_type, author_user_id, author_agent_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(thread_id)
    .bind(document_id)
    .bind(&body.body)
    .bind(&author_type)
    .bind(body.author_user_id.as_deref())
    .bind(body.author_agent_id)
    .fetch_one(state.db.pool())
    .await?;
    state.realtime.publish(
        LiveEvent::new("case.annotation.comment_added", "case_annotation_comment", id)
            .with_company(company_id)
            .with_data(json!({"threadId": thread_id, "caseId": case_id})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "threadId": thread_id,
            "caseId": case_id,
            "documentKey": key,
            "body": body.body,
            "authorType": author_type,
            "authorUserId": body.author_user_id,
            "authorAgentId": body.author_agent_id,
            "createdAt": chrono::Utc::now(),
        })),
    ))
}

// ── Case document delete + revisions ─────────────────────────

async fn delete_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let company_id = ensure_case_exists(&state, case_id).await?;
    let affected = sqlx::query(
        "DELETE FROM case_documents WHERE case_id = $1 AND key = $2 AND company_id = $3",
    )
    .bind(case_id)
    .bind(&key)
    .bind(company_id)
    .execute(state.db.pool())
    .await?
    .rows_affected();
    if affected == 0 {
        return Err(ApiError::NotFound(format!("case document {case_id}:{key}")));
    }
    // Log event
    let _ = sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload) \
         VALUES ($1, $2, 'document_revised', 'user', jsonb_build_object('key', $3::text, 'deleted', true))",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(&key)
    .execute(state.db.pool())
    .await;
    state.realtime.publish(
        LiveEvent::new("case.document.deleted", "case", case_id)
            .with_company(company_id)
            .with_data(json!({"key": key})),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "key": key,
        "deleted": true,
    })))
}

async fn list_case_document_revisions(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let company_id = ensure_case_exists(&state, case_id).await?;
    let (doc_company_id, document_id) = resolve_case_document_id(&state, case_id, &key).await?;
    if doc_company_id != company_id {
        return Err(ApiError::NotFound(format!("case document {case_id}:{key}")));
    }
    let rows: Vec<(Uuid, i32, Option<String>, Option<String>, Option<String>, Option<Uuid>, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT id, revision_number, title, format, change_summary, created_by_agent_id, created_by_user_id, created_at \
         FROM document_revisions WHERE company_id = $1 AND document_id = $2 \
         ORDER BY revision_number DESC LIMIT 200",
    )
    .bind(company_id)
    .bind(document_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, revision_number, title, format, change_summary, created_by_agent_id, created_by_user_id, created_at)| {
            json!({
                "id": id,
                "revisionNumber": revision_number,
                "title": title,
                "format": format,
                "changeSummary": change_summary,
                "createdByAgentId": created_by_agent_id,
                "createdByUserId": created_by_user_id,
                "createdAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "caseId": case_id,
        "documentKey": key,
        "revisions": items,
        "items": items,
    })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestoreCaseDocumentRevisionBody {
    change_summary: Option<String>,
}

async fn restore_case_document_revision(
    State(state): State<AppState>,
    Path((case_id, key, revision_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<RestoreCaseDocumentRevisionBody>,
) -> ApiResult<Json<Value>> {
    let company_id = ensure_case_exists(&state, case_id).await?;
    let (doc_company_id, document_id) = resolve_case_document_id(&state, case_id, &key).await?;
    if doc_company_id != company_id {
        return Err(ApiError::NotFound(format!("case document {case_id}:{key}")));
    }
    // Fetch source revision body
    let src: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT body, title FROM document_revisions WHERE id = $1 AND document_id = $2 AND company_id = $3",
    )
    .bind(revision_id)
    .bind(document_id)
    .bind(company_id)
    .fetch_optional(state.db.pool())
    .await
    .ok()
    .flatten();
    let (src_body, src_title) = src.ok_or_else(|| ApiError::NotFound(format!("revision {revision_id}")))?;

    let mut tx = state.db.pool().begin().await?;
    // Determine next revision number
    let next_no: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision_number), 0) + 1 FROM document_revisions WHERE document_id = $1",
    )
    .bind(document_id)
    .fetch_one(&mut *tx)
    .await?;
    let new_rev_id: Uuid = sqlx::query_scalar(
        "INSERT INTO document_revisions (company_id, document_id, revision_number, body, change_summary, title) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(company_id)
    .bind(document_id)
    .bind(next_no)
    .bind(&src_body)
    .bind(body.change_summary.clone().unwrap_or_else(|| format!("Restored from revision {revision_id}")))
    .bind(src_title.as_deref())
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE documents SET latest_body = $1, latest_revision_id = $2, latest_revision_number = $3, updated_at = now() WHERE id = $4",
    )
    .bind(&src_body)
    .bind(new_rev_id)
    .bind(next_no)
    .bind(document_id)
    .execute(&mut *tx)
    .await?;
    // Log event
    let _ = sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload) \
         VALUES ($1, $2, 'document_revised', 'user', jsonb_build_object('key', $3::text, 'restoredFromRevisionId', $4::text, 'newRevisionId', $5::text))",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(&key)
    .bind(revision_id)
    .bind(new_rev_id)
    .execute(&mut *tx)
    .await;
    tx.commit().await?;
    state.realtime.publish(
        LiveEvent::new("case.document.revision_restored", "case_document_revision", new_rev_id)
            .with_company(company_id)
            .with_data(json!({"caseId": case_id, "documentKey": key, "fromRevisionId": revision_id, "newRevisionId": new_rev_id})),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "documentKey": key,
        "restoredFromRevisionId": revision_id,
        "revisionId": new_rev_id,
        "revisionNumber": next_no,
    })))
}

// ── Case attachments + issue case-links ─────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCaseAttachmentBody {
    asset_id: Uuid,
}

async fn create_case_attachment(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<CreateCaseAttachmentBody>,
) -> ApiResult<impl IntoResponse> {
    let company_id = ensure_case_exists(&state, case_id).await?;
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO case_attachments (company_id, case_id, asset_id) VALUES ($1, $2, $3) \
         ON CONFLICT (case_id, asset_id) DO UPDATE SET updated_at = now() \
         RETURNING id",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(body.asset_id)
    .fetch_one(state.db.pool())
    .await?;
    let _ = sqlx::query(
        "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload) \
         VALUES ($1, $2, 'attachment_added', 'user', jsonb_build_object('assetId', $3::text))",
    )
    .bind(company_id)
    .bind(case_id)
    .bind(body.asset_id)
    .execute(state.db.pool())
    .await;
    state.realtime.publish(
        LiveEvent::new("case.attachment.added", "case_attachment", row.0)
            .with_company(company_id)
            .with_data(json!({"caseId": case_id, "assetId": body.asset_id})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.0,
            "caseId": case_id,
            "assetId": body.asset_id,
        })),
    ))
}

async fn list_issue_cases(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<(Uuid, Uuid, String, Option<Uuid>, Option<Uuid>, Option<String>, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT cil.id, cil.case_id, cil.role, c.project_id, c.parent_case_id, c.status, cil.created_at \
         FROM case_issue_links cil JOIN cases c ON c.id = cil.case_id \
         WHERE cil.issue_id = $1 ORDER BY cil.created_at DESC LIMIT 200",
    )
    .bind(issue_id)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(link_id, case_id, role, project_id, parent_case_id, status, created_at)| {
            json!({
                "linkId": link_id,
                "caseId": case_id,
                "role": role,
                "projectId": project_id,
                "parentCaseId": parent_case_id,
                "status": status,
                "linkedAt": created_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "issueId": issue_id,
        "cases": items,
        "items": items,
    })))
}
