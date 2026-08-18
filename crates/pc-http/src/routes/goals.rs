//! `/api/goals*` 路由：
//!
//! | Method | Path | Node 等价 | 备注 |
//! |---|---|---|---|
//! | GET    | `/api/goals` | ✅ | list (支持 ?company_id=) |
//! | POST   | `/api/goals` | ✅ | create |
//! | GET    | `/api/companies/:company_id/goals` | ✅ | list by company |
//! | POST   | `/api/companies/:company_id/goals` | ✅ | create for company |
//! | GET    | `/api/goals/:id` | ✅ | get |
//! | PATCH  | `/api/goals/:id` | ✅ | update |
//! | DELETE | `/api/goals/:id` | ✅ | delete |

use axum::Extension as AxumExtension;
#[allow(unused_imports)]
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use pc_auth::AuthContext;
use pc_authz::{enforce_permission, PermissionKey};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx;
use uuid::Uuid;

use pc_realtime::LiveEvent;
use pc_repos::goal::GoalRepo;

use crate::{ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/goals", get(list).post(create))
        .route(
            "/api/companies/:company_id/goals",
            get(list_company_goals).post(create_company_goal),
        )
        .route("/api/goals/:id", get(get_one).patch(update).delete(remove))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ListQuery {
    #[serde(default)]
    company_id: Option<Uuid>,
}
async fn list(
    State(s): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    Ok(Json(match q.company_id {
        Some(c) => {
            serde_json::to_value(GoalRepo::new(&s.db).list_by_company(c).await?).unwrap_or_default()
        }
        None => serde_json::to_value(GoalRepo::new(&s.db).list_all(200).await?).unwrap_or_default(),
    }))
}
async fn get_one(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let r = GoalRepo::new(&s.db)
        .get_id(id)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("goal {id}")))?;
    Ok(Json(serde_json::to_value(r).unwrap_or_default()))
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CreateBody {
    company_id: Uuid,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    owner_agent_id: Option<Uuid>,
}
async fn create(
    State(s): State<AppState>,
    Json(b): Json<CreateBody>,
) -> ApiResult<impl IntoResponse> {
    if b.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title required".into()));
    }
    let r = GoalRepo::new(&s.db)
        .create_simple(
            b.company_id,
            &b.title,
            b.description.as_deref(),
            b.owner_agent_id,
        )
        .await?;
    s.realtime
        .publish(LiveEvent::new("goal.created", "goal", r.id).with_company(r.company_id));
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":r.id,"title":r.title,"status":r.status})),
    ))
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct UpdateBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: Option<String>,
}
async fn list_company_goals(
    State(s): State<AppState>,
    axum::extract::Path(company_id): axum::extract::Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let rows = GoalRepo::new(&s.db).list_by_company(company_id).await?;
    Ok(Json(serde_json::to_value(rows).unwrap_or_default()))
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CompanyGoalCreateBody {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    owner_agent_id: Option<Uuid>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    parent_id: Option<Uuid>,
}

async fn create_company_goal(
    State(s): State<AppState>,
    axum::extract::Path(company_id): axum::extract::Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(b): Json<CompanyGoalCreateBody>,
) -> ApiResult<impl IntoResponse> {
    // pc-authz：创建 company goal 需要 UsersInvite 权限
    if let Err(err) =
        enforce_permission(&s.db, &actor, company_id, PermissionKey::UsersInvite).await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    if b.title.trim().is_empty() {
        return Err(ApiError::BadRequest("title required".into()));
    }
    let level = b
        .level
        .as_deref()
        .and_then(pc_repos::goal::GoalLevel::parse)
        .unwrap_or(pc_repos::goal::GoalLevel::Company);
    let new_goal = pc_repos::goal::NewGoal {
        company_id,
        title: b.title.clone(),
        description: b.description.clone(),
        level,
        status: pc_repos::goal::GoalStatus::Planned,
        parent_id: b.parent_id,
        owner_agent_id: b.owner_agent_id,
    };
    let r = GoalRepo::new(&s.db).create(&new_goal).await?;
    s.realtime
        .publish(LiveEvent::new("goal.created", "goal", r.id).with_company(r.company_id));
    Ok((
        StatusCode::CREATED,
        Json(serde_json::to_value(r).unwrap_or_default()),
    ))
}

async fn update(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    // pc-authz：先查 company_id
    let preview: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM goals WHERE id = $1")
        .bind(id)
        .fetch_optional(s.db.pool())
        .await?;
    let preview_company_id = preview
        .ok_or_else(|| ApiError::NotFound(format!("goal {id}")))?
        .0;
    if let Err(err) = enforce_permission(
        &s.db,
        &actor,
        preview_company_id,
        PermissionKey::UsersInvite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    let r = GoalRepo::new(&s.db)
        .update(
            id,
            b.title.as_deref(),
            b.description.as_deref(),
            b.status.as_deref(),
            None,
            None,
        )
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("goal {id}")))?;
    s.realtime
        .publish(LiveEvent::new("goal.updated", "goal", r.id).with_company(r.company_id));
    Ok(Json(serde_json::to_value(r).unwrap_or_default()))
}
async fn remove(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    AxumExtension(actor): AxumExtension<AuthContext>,
) -> ApiResult<StatusCode> {
    let preview: Option<(Uuid,)> = sqlx::query_as("SELECT company_id FROM goals WHERE id = $1")
        .bind(id)
        .fetch_optional(s.db.pool())
        .await?;
    let preview_company_id = preview
        .ok_or_else(|| ApiError::NotFound(format!("goal {id}")))?
        .0;
    if let Err(err) = enforce_permission(
        &s.db,
        &actor,
        preview_company_id,
        PermissionKey::UsersInvite,
    )
    .await
    {
        return Err(ApiError::Forbidden(err.to_string()));
    }
    // R799: delete_one returns GoalRow directly; RepoError::NotFound -> 404
    let row = GoalRepo::new(&s.db).delete_one(id).await.map_err(|err| match err {
        pc_repos::RepoError::NotFound { .. } => ApiError::NotFound(format!("goal {id}")),
        other => ApiError::from(other),
    })?;
    s.realtime.publish(
        LiveEvent::new("goal.removed", "goal", row.id)
            .with_company(row.company_id),
    );
    Ok(StatusCode::NO_CONTENT)
}
