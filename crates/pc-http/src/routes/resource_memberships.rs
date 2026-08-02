//! 资源成员关系路由。

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::{get, put},
    Json, Router,
};
use pc_core::Timestamp;
use pc_repos::membership::{MembershipRepo, ResourceMembershipSnapshot};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/resource-memberships/me",
            get(list),
        )
        .route(
            "/api/companies/:company_id/resource-memberships/me/projects/:project_id",
            put(update_project),
        )
        .route(
            "/api/companies/:company_id/resource-memberships/me/agents/:agent_id",
            put(update_agent),
        )
        .route(
            "/api/companies/:company_id/resource-memberships/me/documents/:document_id",
            put(update_document),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateBody {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    starred: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateResponse {
    resource_type: &'static str,
    resource_id: Uuid,
    state: &'static str,
    starred_at: Option<Timestamp>,
    updated_at: Timestamp,
}

fn validate_body(body: &UpdateBody) -> ApiResult<()> {
    if body.state.is_none() && body.starred.is_none() {
        return Err(ApiError::BadRequest(
            "state or starred is required".to_owned(),
        ));
    }
    if body
        .state
        .as_deref()
        .is_some_and(|state| state != "joined" && state != "left")
    {
        return Err(ApiError::BadRequest(
            "state must be joined or left".to_owned(),
        ));
    }
    if body.state.as_deref() == Some("left") && body.starred == Some(true) {
        return Err(ApiError::BadRequest(
            "starred resources must be joined".to_owned(),
        ));
    }
    Ok(())
}

fn next_membership_values(
    previous_state: &str,
    previous_starred_at: Option<Timestamp>,
    body: &UpdateBody,
) -> (&'static str, Option<Timestamp>) {
    let next_state = if body.starred == Some(true) {
        "joined"
    } else if body.state.as_deref() == Some("left")
        || (body.state.is_none() && previous_state == "left")
    {
        "left"
    } else {
        "joined"
    };
    let next_starred_at = if next_state == "left" {
        None
    } else if body.starred == Some(true) {
        Some(previous_starred_at.unwrap_or_else(MembershipRepo::now_timestamp))
    } else if body.starred == Some(false) {
        None
    } else {
        previous_starred_at
    };
    (next_state, next_starred_at)
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<ResourceMembershipSnapshot>> {
    let user_id = require_user_id(&state, &headers).await?;
    Ok(Json(
        MembershipRepo::new(&state.db)
            .snapshot(company_id, &user_id)
            .await?,
    ))
}

async fn update_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((company_id, project_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<UpdateResponse>> {
    validate_body(&body)?;
    let user_id = require_user_id(&state, &headers).await?;
    let repo = MembershipRepo::new(&state.db);
    if !repo.project_exists(company_id, project_id).await? {
        return Err(ApiError::NotFound(format!("project {project_id}")));
    }
    let existing = repo.get_project(company_id, &user_id, project_id).await?;
    let previous_state = existing.as_ref().map_or("joined", |row| {
        if row.state == "left" {
            "left"
        } else {
            "joined"
        }
    });
    let previous_starred_at = existing.as_ref().and_then(|row| row.starred_at);
    let (next_state, next_starred_at) =
        next_membership_values(previous_state, previous_starred_at, &body);
    let changed = previous_state != next_state
        || previous_starred_at.map(|value| value.as_datetime())
            != next_starred_at.map(|value| value.as_datetime());
    if !changed {
        return Ok(Json(UpdateResponse {
            resource_type: "project",
            resource_id: project_id,
            state: if next_state == "left" {
                "left"
            } else {
                "joined"
            },
            starred_at: previous_starred_at,
            updated_at: existing
                .as_ref()
                .map_or_else(MembershipRepo::now_timestamp, |row| row.updated_at),
        }));
    }
    let row = repo
        .upsert_project(
            company_id,
            project_id,
            &user_id,
            next_state,
            next_starred_at,
        )
        .await?;
    Ok(Json(UpdateResponse {
        resource_type: "project",
        resource_id: project_id,
        state: if row.state == "left" {
            "left"
        } else {
            "joined"
        },
        starred_at: row.starred_at,
        updated_at: row.updated_at,
    }))
}

async fn update_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((company_id, agent_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<UpdateResponse>> {
    validate_body(&body)?;
    let user_id = require_user_id(&state, &headers).await?;
    let repo = MembershipRepo::new(&state.db);
    if !repo.agent_exists(company_id, agent_id).await? {
        return Err(ApiError::NotFound(format!("agent {agent_id}")));
    }
    let existing = repo.get_agent(company_id, &user_id, agent_id).await?;
    let previous_state = existing.as_ref().map_or("joined", |row| {
        if row.state == "left" {
            "left"
        } else {
            "joined"
        }
    });
    let previous_starred_at = existing.as_ref().and_then(|row| row.starred_at);
    let (next_state, next_starred_at) =
        next_membership_values(previous_state, previous_starred_at, &body);
    let changed = previous_state != next_state
        || previous_starred_at.map(|value| value.as_datetime())
            != next_starred_at.map(|value| value.as_datetime());
    if !changed {
        return Ok(Json(UpdateResponse {
            resource_type: "agent",
            resource_id: agent_id,
            state: if next_state == "left" {
                "left"
            } else {
                "joined"
            },
            starred_at: previous_starred_at,
            updated_at: existing
                .as_ref()
                .map_or_else(MembershipRepo::now_timestamp, |row| row.updated_at),
        }));
    }
    let row = repo
        .upsert_agent(company_id, agent_id, &user_id, next_state, next_starred_at)
        .await?;
    Ok(Json(UpdateResponse {
        resource_type: "agent",
        resource_id: agent_id,
        state: if row.state == "left" {
            "left"
        } else {
            "joined"
        },
        starred_at: row.starred_at,
        updated_at: row.updated_at,
    }))
}

async fn update_document(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((company_id, document_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<UpdateResponse>> {
    let Some(starred) = body.starred else {
        return Err(ApiError::BadRequest("starred is required".to_owned()));
    };
    if body.state.is_some() {
        return Err(ApiError::BadRequest(
            "document membership does not accept state".to_owned(),
        ));
    }
    let user_id = require_user_id(&state, &headers).await?;
    let repo = MembershipRepo::new(&state.db);
    if !repo.document_exists(company_id, document_id).await? {
        return Err(ApiError::NotFound(format!("document {document_id}")));
    }
    let existing = repo.get_document(company_id, &user_id, document_id).await?;
    if starred {
        if let Some(row) = existing.as_ref().filter(|row| row.starred_at.is_some()) {
            return Ok(Json(UpdateResponse {
                resource_type: "document",
                resource_id: document_id,
                state: "joined",
                starred_at: row.starred_at,
                updated_at: row.updated_at,
            }));
        }
        let row = repo
            .upsert_document(
                company_id,
                document_id,
                &user_id,
                MembershipRepo::now_timestamp(),
            )
            .await?;
        return Ok(Json(UpdateResponse {
            resource_type: "document",
            resource_id: document_id,
            state: "joined",
            starred_at: row.starred_at,
            updated_at: row.updated_at,
        }));
    }
    if existing.is_none() {
        return Ok(Json(UpdateResponse {
            resource_type: "document",
            resource_id: document_id,
            state: "joined",
            starred_at: None,
            updated_at: MembershipRepo::now_timestamp(),
        }));
    }
    repo.delete_document(company_id, document_id, &user_id)
        .await?;
    Ok(Json(UpdateResponse {
        resource_type: "document",
        resource_id: document_id,
        state: "joined",
        starred_at: None,
        updated_at: MembershipRepo::now_timestamp(),
    }))
}
