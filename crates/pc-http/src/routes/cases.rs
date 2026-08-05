//! `/api/cases*` 路由：CRUD。

#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use pc_core::Timestamp;
use pc_realtime::LiveEvent;
use pc_repos::case::{CaseAnnotationCommentRow, CaseAnnotationPatch, CaseAnnotationThreadRow, CaseLinkRole, CaseRepo, CaseRow, DocumentRevisionRow, NewCaseAnnotationComment, NewCaseAnnotationThread};

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
        // ── Round 43: node-compatible alias for /annotations/:thread_id ──
        .route(
            "/api/cases/:case_id/documents/:key/annotations/:thread_id",
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
        // ---- Round 36: case sub-resources (children / tree / issue-links / rollup / review) ----
        .route("/api/cases/:case_id/children", get(list_case_children))
        .route("/api/cases/:case_id/children/tree", get(list_case_children_tree))
        .route("/api/cases/:case_id/issue-links", get(list_case_issue_links_route))
        .route(
            "/api/cases/:case_id/issue-links/:link_id",
            delete(delete_case_issue_link),
        )
        .route("/api/cases/:case_id/rollup", get(get_case_rollup))
        .route("/api/cases/:case_id/review", post(review_case_route))
        // ---- Round 40: case automation lifecycle (breakdown / suggest-transition / resolve-suggestion / acknowledge-drift / blockers / open-conversation / context-pack / outputs) ----
        .route("/api/cases/:case_id/breakdown", post(breakdown_case_route))
        .route("/api/cases/:case_id/suggest-transition", post(suggest_transition_route))
        .route("/api/cases/:case_id/resolve-suggestion", post(resolve_suggestion_route))
        .route("/api/cases/:case_id/acknowledge-drift", post(acknowledge_drift_route))
        .route("/api/cases/:case_id/blockers", put(replace_case_blockers_route))
        .route("/api/cases/:case_id/open-conversation", post(open_conversation_route))
        .route("/api/cases/:case_id/context-pack", get(get_case_context_pack))
        .route("/api/cases/:case_id/outputs", get(get_case_outputs))
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

// Round 106: 仓储化。直接走 CaseRepo::list_events_by_case_id。
async fn list_case_events(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> ApiResult<Json<Value>> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500) as i64;
    let rows = CaseRepo::new(&state.db)
        .list_events_by_case_id(case_id, limit)
        .await?;

    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "kind": r.kind,
                "actorType": r.actor_type,
                "actorUserId": r.actor_user_id,
                "actorAgentId": r.actor_agent_id,
                "runId": r.run_id,
                "payload": r.payload,
                "createdAt": r.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

// Round 113: 仓储化。CaseRepo::link_issue + record_issue_linked_event。
async fn create_case_link(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<CreateCaseLinkBody>,
) -> ApiResult<Json<Value>> {
    let case_row = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let role_str = body.role.clone().unwrap_or_else(|| "reference".to_string());
    let role: CaseLinkRole = role_str.parse().unwrap_or(CaseLinkRole::Reference);
    let link = CaseRepo::new(&state.db)
        .link_issue(case_row.company_id, case_id, body.issue_id, role, None)
        .await?;
    CaseRepo::new(&state.db)
        .record_issue_linked_event(case_row.company_id, case_id, body.issue_id, &role_str)
        .await?;
    state.realtime.publish(
        LiveEvent::new("case.issue_linked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"issueId": body.issue_id, "role": role_str})),
    );
    Ok(Json(json!({
        "id": link.id,
        "caseId": case_id,
        "issueId": body.issue_id,
        "role": role_str,
    })))
}

// Round 109: 仓储化。CaseRepo::list_documents 需要 company_id + case_id。
// 先 SELECT company_id FROM cases 反查一次，再调 Repo。
async fn list_case_documents(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?
        .company_id;
    let rows = repo.list_documents(company_id, case_id).await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|d| {
            json!({
                "id": d.id,
                "companyId": d.company_id,
                "caseId": d.case_id,
                "documentId": d.document_id,
                "key": d.key,
                "createdAt": d.created_at,
                "updatedAt": d.updated_at,
            })
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
    let row = CaseRepo::new(&state.db)
        .link_document(case_row.company_id, case_id, body.document_id, &body.key)
        .await?;
    state.realtime.publish(
        LiveEvent::new("case.document.upserted", "case", case_id)
            .with_company(case_row.company_id),
    );
    Ok(Json(json!({"id": row.id, "caseId": case_id, "key": body.key, "documentId": body.document_id})))
}

// Round 109: 仓储化。CaseRepo::get_document(company_id, case_id, key) 返回 CaseDocumentRow。
async fn get_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?
        .company_id;
    let row = repo
        .get_document(company_id, case_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case document {key}")))?;
    Ok(Json(json!({
        "id": row.id,
        "caseId": row.case_id,
        "key": row.key,
        "documentId": row.document_id,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
    })))
}

// Round 109: 仓储化。CaseRepo::lock_document 单事务内完成 UPDATE + event INSERT。
async fn lock_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let case_row = repo.get(case_id).await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let n = repo
        .lock_document(case_row.company_id, case_id, &key)
        .await?;
    if !n {
        return Err(ApiError::NotFound(format!("case document {key}")));
    }
    state.realtime.publish(
        LiveEvent::new("case.document.locked", "case", case_id)
            .with_company(case_row.company_id)
            .with_data(json!({"key": key})),
    );
    Ok(Json(json!({"locked": true, "caseId": case_id, "key": key})))
}

// Round 109: 仓储化。
async fn unlock_case_document(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let case_row = repo.get(case_id).await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let n = repo
        .unlock_document(case_row.company_id, case_id, &key)
        .await?;
    if !n {
        return Err(ApiError::NotFound(format!("case document {key}")));
    }
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
    let rows = CaseRepo::new(&state.db)
        .list_case_document_annotations(case_id, &key)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "kind": row.kind,
                "threadId": row.thread_id,
                "payload": row.payload,
            })
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

// ── Case annotation threads ──────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListAnnotationThreadsQuery {
    status: Option<String>,
    include_comments: Option<bool>,
}

// Round 114: 仓储化。CaseRepo::get_case_company_id + list_case_annotation_threads +
//             list_case_thread_comments_bulk。
async fn list_case_annotation_threads(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
    axum::extract::Query(q): axum::extract::Query<ListAnnotationThreadsQuery>,
) -> ApiResult<Json<Value>> {
    let _company_id = CaseRepo::new(&state.db)
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let status_filter = q
        .status
        .as_deref()
        .and_then(|s| if s == "open" || s == "resolved" { Some(s) } else { None });
    let include_comments = q.include_comments.unwrap_or(false);
    let rows = CaseRepo::new(&state.db)
        .list_case_annotation_threads(case_id, &key, status_filter, 200)
        .await?;
    let mut items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "documentKey": r.document_key,
                "status": r.status,
                "anchorState": r.anchor_state,
                "originalRevisionId": r.original_revision_id,
                "originalRevisionNumber": r.original_revision_number,
                "currentRevisionId": r.current_revision_id,
                "currentRevisionNumber": r.current_revision_number,
                "selectedText": r.selected_text,
                "prefixText": r.prefix_text,
                "suffixText": r.suffix_text,
                "normalizedStart": r.normalized_start,
                "normalizedEnd": r.normalized_end,
                "markdownStart": r.markdown_start,
                "markdownEnd": r.markdown_end,
                "anchorConfidence": r.anchor_confidence,
                "anchorSelector": r.anchor_selector,
                "resolvedAt": r.resolved_at,
                "resolvedByUserId": r.resolved_by_user_id,
                "resolvedByAgentId": r.resolved_by_agent_id,
                "createdByUserId": r.created_by_user_id,
                "createdByAgentId": r.created_by_agent_id,
                "createdAt": r.created_at,
                "updatedAt": r.updated_at,
            })
        })
        .collect();
    if include_comments {
        let thread_ids: Vec<Uuid> = items
            .iter()
            .filter_map(|v| v.get("id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok()))
            .collect();
        if !thread_ids.is_empty() {
            let comments = CaseRepo::new(&state.db)
                .list_case_thread_comments_bulk(&thread_ids)
                .await?;
            for t in items.iter_mut() {
                let tid = t
                    .get("id")
                    .and_then(Value::as_str)
                    .and_then(|s| Uuid::parse_str(s).ok());
                let cs: Vec<Value> = comments
                    .iter()
                    .filter(|c| Some(c.thread_id) == tid)
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "body": c.body,
                            "authorType": c.author_type,
                            "authorAgentId": c.author_agent_id,
                            "authorUserId": c.author_user_id,
                            "createdAt": c.created_at,
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

// Round 114: 仓储化。CaseRepo::get_case_company_id + resolve_case_document_id +
//             create_case_annotation_thread + create_case_thread_comment。
async fn create_case_annotation_thread(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
    Json(body): Json<CreateCaseAnnotationThreadBody>,
) -> ApiResult<impl IntoResponse> {
    if body.selected_text.is_empty() {
        return Err(ApiError::BadRequest("selectedText is required".into()));
    }
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let (doc_company_id, document_id) = repo
        .resolve_case_document_id(case_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case document {case_id}:{key}")))?;
    if doc_company_id != company_id {
        return Err(ApiError::BadRequest("case/document company mismatch".into()));
    }
    let norm_start = body.normalized_start.unwrap_or(0);
    let norm_end = body.normalized_end.unwrap_or(body.selected_text.len() as i32);
    let md_start = body.markdown_start.unwrap_or(0);
    let md_end = body.markdown_end.unwrap_or(body.selected_text.len() as i32);
    let confidence = body.anchor_confidence.clone().unwrap_or_else(|| "exact".to_owned());
    let selector = body.anchor_selector.clone().unwrap_or_else(|| json!({}));
    let revision_number = body.revision_number.unwrap_or(1);
    let input = NewCaseAnnotationThread {
        company_id,
        case_id,
        document_id,
        document_key: key.clone(),
        status: body.status.clone(),
        original_revision_id: None,
        revision_number,
        selected_text: body.selected_text.clone(),
        prefix_text: body.prefix_text.clone(),
        suffix_text: body.suffix_text.clone(),
        normalized_start: norm_start,
        normalized_end: norm_end,
        markdown_start: md_start,
        markdown_end: md_end,
        anchor_confidence: Some(confidence.clone()),
        anchor_selector: Some(selector.clone()),
    };
    let thread_id = repo.create_case_annotation_thread(&input).await?;
    if let Some(initial_body) = body.body.as_deref() {
        if !initial_body.is_empty() {
            let comment = NewCaseAnnotationComment {
                company_id,
                case_id,
                thread_id,
                document_id,
                body: initial_body.to_owned(),
                author_type: "user".to_owned(),
                author_user_id: None,
                author_agent_id: None,
            };
            repo.create_case_thread_comment(&comment).await?;
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

// Round 114: 仓储化。CaseRepo::get_case_company_id + get_case_annotation_thread +
//             list_case_thread_comments。
async fn get_case_annotation_thread(
    State(state): State<AppState>,
    Path((case_id, key, thread_id)): Path<(Uuid, String, Uuid)>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let _company_id = repo
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let row = repo
        .get_case_annotation_thread(case_id, thread_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    let comments = repo.list_case_thread_comments(thread_id).await?;
    let comment_items: Vec<Value> = comments
        .into_iter()
        .map(|c| {
            json!({
                "id": c.id,
                "body": c.body,
                "authorType": c.author_type,
                "authorAgentId": c.author_agent_id,
                "authorUserId": c.author_user_id,
                "createdAt": c.created_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "id": row.id,
        "caseId": case_id,
        "documentId": row.document_id,
        "documentKey": row.document_key,
        "status": row.status,
        "anchorConfidence": row.anchor_confidence,
        "normalizedStart": row.normalized_start,
        "normalizedEnd": row.normalized_end,
        "selectedText": row.selected_text,
        "anchorSelector": row.anchor_selector,
        "resolvedAt": row.resolved_at,
        "resolvedByAgentId": row.resolved_by_agent_id,
        "resolvedByUserId": row.resolved_by_user_id,
        "createdAt": row.created_at,
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

// Round 114: 仓储化。CaseRepo::get_case_company_id + update_case_annotation_thread。
async fn patch_case_annotation_thread(
    State(state): State<AppState>,
    Path((case_id, key, thread_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<PatchCaseAnnotationThreadBody>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    if let Some(s) = body.status.as_deref() {
        if !matches!(s, "open" | "resolved" | "outdated") {
            return Err(ApiError::BadRequest(format!("invalid status '{s}'")));
        }
    }
    let patch = CaseAnnotationPatch {
        status: body.status.clone(),
        anchor_selector: body.anchor_selector.clone(),
        anchor_state: body.anchor_state.clone(),
        current_revision_id: body.current_revision_id,
        current_revision_number: body.current_revision_number,
    };
    let affected = repo
        .update_case_annotation_thread(case_id, thread_id, &key, &patch)
        .await?;
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

// Round 114: 仓储化。CaseRepo::get_case_company_id + get_case_thread_document_id +
//             create_case_thread_comment。
async fn add_case_annotation_comment(
    State(state): State<AppState>,
    Path((case_id, key, thread_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<AddCaseAnnotationCommentBody>,
) -> ApiResult<impl IntoResponse> {
    if body.body.trim().is_empty() {
        return Err(ApiError::BadRequest("body is required".into()));
    }
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let document_id = repo
        .get_case_thread_document_id(case_id, thread_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("annotation thread {thread_id}")))?;
    let author_type = body.author_type.clone().unwrap_or_else(|| "user".to_owned());
    let input = NewCaseAnnotationComment {
        company_id,
        case_id,
        thread_id,
        document_id,
        body: body.body.clone(),
        author_type: author_type.clone(),
        author_user_id: body.author_user_id.clone(),
        author_agent_id: body.author_agent_id,
    };
    let id = repo.create_case_thread_comment(&input).await?;
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
    let company_id = CaseRepo::new(&state.db)
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let deleted = CaseRepo::new(&state.db)
        .unlink_document(company_id, case_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case document {case_id}:{key}")))?;
    // Log event
    let _ = CaseRepo::new(&state.db)
        .record_case_event(
            company_id,
            case_id,
            "document_revised",
            "user",
            json!({ "key": key, "deleted": true }),
        )
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

// Round 116: 仓储化。CaseRepo::get_case_company_id + resolve_case_document_id +
//             list_document_revisions。
async fn list_case_document_revisions(
    State(state): State<AppState>,
    Path((case_id, key)): Path<(Uuid, String)>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let (doc_company_id, document_id) = repo
        .resolve_case_document_id(case_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case document {case_id}:{key}")))?;
    if doc_company_id != company_id {
        return Err(ApiError::NotFound(format!("case document {case_id}:{key}")));
    }
    let rows = repo
        .list_document_revisions(company_id, document_id, 200)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "revisionNumber": r.revision_number,
                "title": r.title,
                "format": r.format,
                "changeSummary": r.change_summary,
                "createdByAgentId": r.created_by_agent_id,
                "createdByUserId": r.created_by_user_id,
                "createdAt": r.created_at,
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

// Round 116: 仓储化。CaseRepo::get_case_company_id + resolve_case_document_id +
//             get_document_revision_body + restore_document_revision (复合 tx)。
async fn restore_case_document_revision(
    State(state): State<AppState>,
    Path((case_id, key, revision_id)): Path<(Uuid, String, Uuid)>,
    Json(body): Json<RestoreCaseDocumentRevisionBody>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let (doc_company_id, document_id) = repo
        .resolve_case_document_id(case_id, &key)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case document {case_id}:{key}")))?;
    if doc_company_id != company_id {
        return Err(ApiError::NotFound(format!("case document {case_id}:{key}")));
    }
    let (src_body, src_title) = repo
        .get_document_revision_body(company_id, document_id, revision_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("revision {revision_id}")))?;
    let change_summary = body
        .change_summary
        .clone()
        .unwrap_or_else(|| format!("Restored from revision {revision_id}"));
    let (new_rev_id, next_no) = repo
        .restore_document_revision(
            company_id,
            case_id,
            &key,
            document_id,
            &src_body,
            src_title.as_deref(),
            &change_summary,
            revision_id,
        )
        .await?;
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
        "changeSummary": change_summary,
    })))
}

// ── Case attachments + issue case-links ─────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateCaseAttachmentBody {
    asset_id: Uuid,
}

// Round 115: 仓储化。CaseRepo::get_case_company_id + upsert_case_attachment +
//             record_attachment_added_event。
async fn create_case_attachment(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<CreateCaseAttachmentBody>,
) -> ApiResult<impl IntoResponse> {
    let repo = CaseRepo::new(&state.db);
    let company_id = repo
        .get_case_company_id(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let id = repo
        .upsert_case_attachment(company_id, case_id, body.asset_id)
        .await?;
    let _ = repo
        .record_attachment_added_event(company_id, case_id, body.asset_id)
        .await;
    state.realtime.publish(
        LiveEvent::new("case.attachment.added", "case_attachment", id)
            .with_company(company_id)
            .with_data(json!({"caseId": case_id, "assetId": body.asset_id})),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "caseId": case_id,
            "assetId": body.asset_id,
        })),
    ))
}

async fn list_issue_cases(
    State(state): State<AppState>,
    Path(issue_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = CaseRepo::new(&state.db)
        .list_issue_cases(issue_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "linkId": row.link_id,
                "caseId": row.case_id,
                "role": row.role,
                "projectId": row.project_id,
                "parentCaseId": row.parent_case_id,
                "status": row.status,
                "linkedAt": row.linked_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "issueId": issue_id,
        "cases": items,
        "items": items,
    })))
}


// ============================================================================
// Round 36: case children / tree / issue-links list+delete / rollup / review
// ============================================================================

/// Direct children cases (one-level deep).  Mirrors Node
/// `/cases/:caseId/children` — returns `parentCaseId = :case_id` rows.
async fn list_case_children(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let rows = CaseRepo::new(&state.db)
        .list_children(case.company_id, case_id)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.id,
                "caseId": row.id,
                "companyId": row.company_id,
                "parentCaseId": row.parent_case_id,
                "caseNumber": row.case_number,
                "identifier": row.identifier,
                "caseType": row.case_type,
                "title": row.title,
                "summary": row.summary,
                "status": row.status,
                "createdAt": row.created_at,
                "updatedAt": row.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "caseId": case_id,
        "items": items,
        "count": items.len(),
    })))
}

/// Recursive children tree.  Materializes full subtree by walking
/// `parent_case_id`; safe up to ~5k cases per company in practice.
async fn list_case_children_tree(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let root = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let all: Vec<CaseRow> = CaseRepo::new(&state.db)
        .list_all_for_tree(root.company_id)
        .await?;

    use std::collections::HashMap;
    let mut children_by_parent: HashMap<Option<Uuid>, Vec<CaseRow>> = HashMap::new();
    for row in all {
        children_by_parent
            .entry(row.parent_case_id)
            .or_default()
            .push(row);
    }

    fn build_tree(
        node: &CaseRow,
        children_by_parent: &HashMap<Option<Uuid>, Vec<CaseRow>>,
    ) -> Value {
        let kids = children_by_parent
            .get(&Some(node.id))
            .map(|rows| {
                rows.iter()
                    .map(|kid| build_tree(kid, children_by_parent))
                    .collect::<Vec<Value>>()
            })
            .unwrap_or_default();
        json!({
            "id": node.id,
            "caseNumber": node.case_number,
            "identifier": node.identifier,
            "title": node.title,
            "status": node.status,
            "caseType": node.case_type,
            "children": kids,
            "childCount": kids.len(),
        })
    }

    let tree = build_tree(&root, &children_by_parent);
    Ok(Json(json!({
        "caseId": case_id,
        "tree": tree,
    })))
}

// Round 113: 仓储化。CaseRepo::list_issue_links_with_issue。
async fn list_case_issue_links_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let rows = CaseRepo::new(&state.db)
        .list_issue_links_with_issue(case.company_id, case_id)
        .await?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "caseId": r.case_id,
                "issueId": r.issue_id,
                "role": r.role,
                "createdByRunId": r.created_by_run_id,
                "createdAt": r.created_at,
                "issueTitle": r.issue_title,
                "issueStatus": r.issue_status,
            })
        })
        .collect();
    Ok(Json(json!({
        "caseId": case_id,
        "items": items,
        "count": items.len(),
    })))
}

// Round 113: 仓储化。CaseRepo::delete_issue_link_by_id + record_issue_unlinked_event。
async fn delete_case_issue_link(
    State(state): State<AppState>,
    Path((case_id, link_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let issue_id = CaseRepo::new(&state.db)
        .delete_issue_link_by_id(case.company_id, link_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("issue link {link_id}")))?;
    let _ = CaseRepo::new(&state.db)
        .record_issue_unlinked_event(case.company_id, case_id, issue_id)
        .await;
    state.realtime.publish(
        LiveEvent::new("case.issue_unlinked", "case_issue_link", link_id)
            .with_company(case.company_id)
            .with_data(json!({"caseId": case_id, "issueId": issue_id})),
    );
    Ok(StatusCode::NO_CONTENT)
}

// Round 117: 仓储化。CaseRepo::get_case_company_id + get_case_rollup (复合聚合)。
async fn get_case_rollup(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let case = repo
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let rollup = repo.get_case_rollup(case.company_id, case_id).await?;
    let by_status: serde_json::Map<String, serde_json::Value> = rollup
        .status_breakdown
        .into_iter()
        .map(|(k, v)| (k, serde_json::json!(v)))
        .collect();
    Ok(Json(json!({
        "caseId": case_id,
        "companyId": case.company_id,
        "childCount": rollup.child_count,
        "descendantCount": rollup.descendant_count,
        "issueLinkCount": rollup.issue_link_count,
        "openIssueCount": rollup.open_issue_count,
        "statusBreakdown": by_status,
        "status": case.status,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewCaseBody {
    /// Verdict: "approved" | "rejected" | "request_changes"
    decision: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    expected_version: Option<i32>,
}

/// Case review action — transitions case status and records an event.  Mirrors
/// Node `/cases/:caseId/review`.
async fn review_case_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<ReviewCaseBody>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let new_status = match body.decision.as_str() {
        "approved" => "approved",
        "rejected" | "request_changes" => "in_progress",
        "in_review" => "in_review",
        other => {
            return Err(ApiError::BadRequest(format!(
                "unsupported review decision: {other}"
            )));
        }
    };
    let updated = CaseRepo::new(&state.db)
        .update(case_id, None, None, Some(new_status))
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let payload = json!({
        "decision": body.decision,
        "note": body.note,
        "expectedVersion": body.expected_version,
    });
    let _ = CaseRepo::new(&state.db)
        .record_case_event(
            case.company_id,
            case_id,
            "status_changed",
            "user",
            payload,
        )
        .await;
    state.realtime.publish(
        LiveEvent::new("case.reviewed", "case", case_id)
            .with_company(case.company_id)
            .with_data(json!({
                "decision": body.decision,
                "newStatus": new_status,
                "previousStatus": case.status,
            })),
    );
    Ok(Json(serde_json::to_value(updated).unwrap_or_default()))
}


// ============================================================================
// Round 40: case automation lifecycle
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BreakdownCaseBody {
    /// Subcase specs to create as children of this case.
    children: Vec<BreakdownChild>,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BreakdownChild {
    title: String,
    #[serde(default)]
    case_type: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    fields: Option<serde_json::Value>,
}

/// `POST /api/cases/:case_id/breakdown` — create child cases from a breakdown.
/// Mirrors Node `/cases/:caseId/breakdown`.  For each child we INSERT a new
/// case with `parent_case_id = :case_id`, generate a sequential `case_number`
/// and `identifier` (CASE-<n>), and emit a `case.created` event.
async fn breakdown_case_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<BreakdownCaseBody>,
) -> ApiResult<Json<Value>> {
    let parent = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    if body.children.is_empty() {
        return Err(ApiError::BadRequest("children must not be empty".into()));
    }
    let children: Vec<pc_repos::case::NewBreakdownChild> = body
        .children
        .into_iter()
        .map(|c| pc_repos::case::NewBreakdownChild {
            title: c.title,
            case_type: c.case_type,
            summary: c.summary,
            fields: c.fields,
        })
        .collect();
    let created_ids = CaseRepo::new(&state.db)
        .breakdown_case(
            parent.company_id,
            case_id,
            parent.project_id,
            &parent.case_type,
            children,
            body.note.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("case.broken_down", "case", case_id)
            .with_company(parent.company_id)
            .with_data(json!({"childCaseIds": created_ids, "count": created_ids.len()})),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "childCaseIds": created_ids,
        "count": created_ids.len(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuggestTransitionBody {
    to_stage_key: String,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
}

/// `POST /api/cases/:case_id/suggest-transition` — record a transition
/// suggestion.  Mirrors Node `/cases/:caseId/suggest-transition`.  We don't
/// actually transition the case; we record an event + suggestion payload
/// that the UI/agent can later accept or reject.
async fn suggest_transition_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<SuggestTransitionBody>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    if body.to_stage_key.trim().is_empty() {
        return Err(ApiError::BadRequest("toStageKey must not be empty".into()));
    }
    let payload = json!({
        "toStageKey": body.to_stage_key,
        "reason": body.reason,
        "confidence": body.confidence,
    });
    let _ = CaseRepo::new(&state.db)
        .record_case_event(
            case.company_id,
            case_id,
            "fields_changed",
            "system",
            payload,
        )
        .await;
    let suggestion_id = Uuid::new_v4();
    state.realtime.publish(
        LiveEvent::new("case.transition_suggested", "case", case_id)
            .with_company(case.company_id)
            .with_data(json!({
                "suggestionId": suggestion_id,
                "toStageKey": body.to_stage_key,
                "confidence": body.confidence,
            })),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "suggestionId": suggestion_id,
        "toStageKey": body.to_stage_key,
        "reason": body.reason,
        "confidence": body.confidence,
        "createdAt": chrono::Utc::now(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResolveSuggestionBody {
    suggestion_id: Uuid,
    decision: String,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /api/cases/:case_id/resolve-suggestion` — accept or reject a
/// previously recorded suggestion.  Mirrors Node
/// `/cases/:caseId/resolve-suggestion`.
async fn resolve_suggestion_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<ResolveSuggestionBody>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let decision = body.decision.to_lowercase();
    if !matches!(decision.as_str(), "accepted" | "rejected") {
        return Err(ApiError::BadRequest("decision must be 'accepted' or 'rejected'".into()));
    }
    let payload = json!({
        "suggestionId": body.suggestion_id,
        "decision": decision,
        "reason": body.reason,
    });
    let _ = CaseRepo::new(&state.db)
        .record_case_event(
            case.company_id,
            case_id,
            "fields_changed",
            "user",
            payload,
        )
        .await;
    state.realtime.publish(
        LiveEvent::new("case.suggestion_resolved", "case", case_id)
            .with_company(case.company_id)
            .with_data(json!({
                "suggestionId": body.suggestion_id,
                "decision": decision,
            })),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "suggestionId": body.suggestion_id,
        "decision": decision,
        "resolvedAt": chrono::Utc::now(),
    })))
}

/// `POST /api/cases/:case_id/acknowledge-drift` — record drift acknowledgment.
/// Mirrors Node `/cases/:caseId/acknowledge-drift`.
async fn acknowledge_drift_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let _ = CaseRepo::new(&state.db)
        .record_case_event(
            case.company_id,
            case_id,
            "fields_changed",
            "user",
            json!({ "event": "drift_acknowledged" }),
        )
        .await;
    state.realtime.publish(
        LiveEvent::new("case.drift_acknowledged", "case", case_id)
            .with_company(case.company_id),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "acknowledged": true,
        "acknowledgedAt": chrono::Utc::now(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReplaceBlockersBody {
    /// Case IDs that block this case.
    #[serde(default)]
    blocked_by_case_ids: Vec<Uuid>,
}

/// `PUT /api/cases/:case_id/blockers` — replace the full blocker set for a
/// case (idempotent replace).  Mirrors Node `/cases/:caseId/blockers`.  Uses
/// the `pipeline_case_blockers` table.
async fn replace_case_blockers_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<ReplaceBlockersBody>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let payload = json!({
        "blockedByCaseIds": body.blocked_by_case_ids,
        "count": body.blocked_by_case_ids.len(),
    });
    CaseRepo::new(&state.db)
        .replace_blockers(
            case.company_id,
            case_id,
            body.blocked_by_case_ids.clone(),
            payload.clone(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("case.blockers_set", "case", case_id)
            .with_company(case.company_id)
            .with_data(payload.clone()),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "blockedByCaseIds": body.blocked_by_case_ids,
        "count": body.blocked_by_case_ids.len(),
    })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenConversationBody {
    #[serde(default)]
    issue_id: Option<Uuid>,
    #[serde(default)]
    initial_message: Option<String>,
}

/// `POST /api/cases/:case_id/open-conversation` — open a conversation thread
/// linked to this case.  Mirrors Node `/cases/:caseId/open-conversation`.
/// Since the dedicated conversation table is missing, we synthesize by
/// creating an `issue` with `origin_kind='case_conversation'` and linking it
/// back to the case via `case_issue_links`.
async fn open_conversation_route(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
    Json(body): Json<OpenConversationBody>,
) -> ApiResult<Json<Value>> {
    let case = CaseRepo::new(&state.db)
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let issue_id = CaseRepo::new(&state.db)
        .open_conversation(
            case.company_id,
            case_id,
            &case.title,
            body.issue_id,
            body.initial_message.as_deref(),
        )
        .await?;
    state.realtime.publish(
        LiveEvent::new("case.conversation_opened", "case", case_id)
            .with_company(case.company_id)
            .with_data(json!({"issueId": issue_id})),
    );
    Ok(Json(json!({
        "caseId": case_id,
        "issueId": issue_id,
        "openedAt": chrono::Utc::now(),
    })))
}

/// `GET /api/cases/:case_id/context-pack` — bundle of case context for AI.
/// Mirrors Node `/cases/:caseId/context-pack`.  We synthesize the response
/// from `cases` + `case_events` + `case_issue_links` + `issues`.
async fn get_case_context_pack(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let case = repo
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let events = repo
        .list_context_events(case.company_id, case_id)
        .await
        .unwrap_or_default();
    let linked_issues = repo
        .list_context_issues(case.company_id, case_id)
        .await
        .unwrap_or_default();
    let children_count = repo
        .count_children(case.company_id, case_id)
        .await?;
    let event_items: Vec<Value> = events
        .into_iter()
        .map(|e| {
            json!({
                "kind": e.kind,
                "actorType": e.actor_type,
                "actorUserId": e.actor_user_id,
                "actorAgentId": e.actor_agent_id,
                "runId": e.run_id,
                "payload": e.payload,
                "createdAt": e.created_at,
            })
        })
        .collect();
    let issue_items: Vec<Value> = linked_issues
        .into_iter()
        .map(|i| {
            json!({
                "id": i.id,
                "title": i.title,
                "status": i.status,
            })
        })
        .collect();
    Ok(Json(json!({
        "case": {
            "id": case.id,
            "caseNumber": case.case_number,
            "identifier": case.identifier,
            "title": case.title,
            "summary": case.summary,
            "status": case.status,
            "caseType": case.case_type,
            "fields": case.fields,
        },
        "linkedIssues": issue_items,
        "childCount": children_count,
        "events": event_items,
        "recentEventCount": event_items.len(),
    })))
}

/// `GET /api/cases/:case_id/outputs` — aggregate case outputs (linked issue
/// summaries).  Mirrors Node `/cases/:caseId/outputs`.
async fn get_case_outputs(
    State(state): State<AppState>,
    Path(case_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let repo = CaseRepo::new(&state.db);
    let case = repo
        .get(case_id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))?;
    let rows = repo
        .list_outputs(case.company_id, case_id)
        .await
        .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "id": r.id,
                "title": r.title,
                "status": r.status,
                "linkRole": r.link_role,
                "completedAt": r.completed_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "caseId": case_id,
        "items": items,
        "count": items.len(),
    })))
}
