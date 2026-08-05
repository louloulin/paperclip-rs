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
    let r = GoalRepo::new(&s.db).get_id(id)
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
    let r = GoalRepo::new(&s.db).create_simple(
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
    let rows = GoalRepo::new(&s.db)
        .list_by_company(company_id)
        .await?;
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
    Json(b): Json<CompanyGoalCreateBody>,
) -> ApiResult<impl IntoResponse> {
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
    let r = GoalRepo::new(&s.db)
        .create(&new_goal)
        .await?;
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
    Json(b): Json<UpdateBody>,
) -> ApiResult<Json<Value>> {
    let r = GoalRepo::new(&s.db)
        .update(id, b.title.as_deref(), b.description.as_deref(), b.status.as_deref(), None, None)
        .await?
        .ok_or_else(|| ApiError::NotFound(format!("goal {id}")))?;
    s.realtime
        .publish(LiveEvent::new("goal.updated", "goal", r.id).with_company(r.company_id));
    Ok(Json(serde_json::to_value(r).unwrap_or_default()))
}
async fn remove(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if GoalRepo::new(&s.db).delete_one(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::NotFound(format!("goal {id}")))
    }
}
