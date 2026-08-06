//! User-scoped inbox dismissal and snooze state.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use pc_core::Timestamp;
use pc_repos::inbox::{InboxDismissalRow, InboxRepo};
use serde::Deserialize;
use uuid::Uuid;

use crate::{state::require_user_id, ApiError, ApiResult, AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/companies/:company_id/inbox-dismissals",
            get(list).post(upsert),
        )
        .route(
            "/api/companies/:company_id/inbox-dismissals/:item_key",
            delete(restore),
        )
        // ── Round 207: explicit dismiss / snooze / count ──
        .route(
            "/api/companies/:company_id/inbox-dismissals/dismiss",
            post(explicit_dismiss),
        )
        .route(
            "/api/companies/:company_id/inbox-dismissals/snooze",
            post(explicit_snooze),
        )
        .route(
            "/api/companies/:company_id/inbox-dismissals/count",
            get(active_count),
        )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DismissalBody {
    item_key: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default)]
    snoozed_until: Option<chrono::DateTime<chrono::Utc>>,
}
fn default_kind() -> String {
    "dismiss".into()
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<Vec<InboxDismissalRow>>> {
    let user_id = require_user_id(&state, &headers).await?;
    Ok(Json(
        InboxRepo::new(&state.db)
            .list_for_user(company_id, &user_id)
            .await?,
    ))
}

async fn upsert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(body): Json<DismissalBody>,
) -> ApiResult<(StatusCode, Json<InboxDismissalRow>)> {
    let user_id = require_user_id(&state, &headers).await?;
    if !matches!(body.kind.as_str(), "dismiss" | "snooze") || body.item_key.trim().is_empty() {
        return Err(ApiError::BadRequest("invalid inbox dismissal".into()));
    }
    if body.kind == "snooze" && body.snoozed_until.is_none() {
        return Err(ApiError::BadRequest(
            "snoozedUntil is required for snooze".into(),
        ));
    }
    let row = InboxRepo::new(&state.db)
        .upsert_simple(
            company_id,
            &user_id,
            body.item_key.trim(),
            &body.kind,
            body.snoozed_until.map(Timestamp::from_dt),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((company_id, item_key)): Path<(Uuid, String)>,
) -> ApiResult<StatusCode> {
    let user_id = require_user_id(&state, &headers).await?;
    InboxRepo::new(&state.db)
        .restore(company_id, &user_id, &item_key)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ============================================================================
// Round 207: explicit dismiss / snooze / count
// ============================================================================

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct ExplicitDismissBody {
    item_key: String,
    /// 业务原因（"manual" / "rule:..." / "bulk-import" 等）
    #[serde(default = "default_dismiss_reason")]
    reason: String,
    /// 可选 TTL（秒），到期后自动恢复
    #[serde(default)]
    expires_in_seconds: Option<i64>,
}

fn default_dismiss_reason() -> String {
    "manual".into()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct SnoozeBody {
    item_key: String,
    /// snooze 持续时间（小时），默认 24
    #[serde(default = "default_snooze_hours")]
    hours: i64,
    /// 可选覆盖时间戳（与 hours 二选一，优先）
    #[serde(default)]
    snoozed_until: Option<chrono::DateTime<Utc>>,
}

fn default_snooze_hours() -> i64 {
    24
}

fn compute_snooze_until(body: &SnoozeBody) -> Option<Timestamp> {
    if let Some(until) = body.snoozed_until {
        return Some(Timestamp::from_dt(until));
    }
    if body.hours > 0 {
        return Some(Timestamp::from_dt(
            Utc::now() + chrono::Duration::hours(body.hours),
        ));
    }
    None
}

async fn explicit_dismiss(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(body): Json<ExplicitDismissBody>,
) -> ApiResult<Json<InboxDismissalRow>> {
    let user_id = require_user_id(&state, &headers).await?;
    if body.item_key.trim().is_empty() {
        return Err(ApiError::BadRequest("itemKey is required".into()));
    }
    let expires_at = body
        .expires_in_seconds
        .filter(|s| *s > 0)
        .map(|s| Timestamp::from_dt(Utc::now() + chrono::Duration::seconds(s)));
    let row = InboxRepo::new(&state.db)
        .upsert_simple(
            company_id,
            &user_id,
            body.item_key.trim(),
            "dismiss",
            expires_at,
        )
        .await?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new("inbox.item.dismissed", "inbox_dismissal", row.id)
            .with_company(company_id)
            .with_data(serde_json::json!({
                "itemKey": row.item_key,
                "reason": body.reason,
                "expiresAt": row.snoozed_until,
            })),
    );
    Ok(Json(row))
}

async fn explicit_snooze(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(body): Json<SnoozeBody>,
) -> ApiResult<Json<InboxDismissalRow>> {
    let user_id = require_user_id(&state, &headers).await?;
    if body.item_key.trim().is_empty() {
        return Err(ApiError::BadRequest("itemKey is required".into()));
    }
    let until = compute_snooze_until(&body)
        .ok_or_else(|| ApiError::BadRequest("must provide hours>0 or snoozedUntil".into()))?;
    let row = InboxRepo::new(&state.db)
        .upsert_simple(
            company_id,
            &user_id,
            body.item_key.trim(),
            "snooze",
            Some(until),
        )
        .await?;
    state.realtime.publish(
        pc_realtime::LiveEvent::new("inbox.item.snoozed", "inbox_dismissal", row.id)
            .with_company(company_id)
            .with_data(serde_json::json!({
                "itemKey": row.item_key,
                "snoozedUntil": row.snoozed_until,
            })),
    );
    Ok(Json(row))
}

async fn active_count(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let user_id = require_user_id(&state, &headers).await?;
    let total = InboxRepo::new(&state.db)
        .count_active(company_id, Timestamp::from_dt(Utc::now()))
        .await?;
    Ok(Json(serde_json::json!({
        "companyId": company_id,
        "userId": user_id,
        "activeCount": total,
    })))
}

#[cfg(test)]
mod round207_tests {
    use super::*;

    #[test]
    fn snooze_uses_hours_when_no_explicit_until() {
        let body = SnoozeBody {
            item_key: "issue/123".to_owned(),
            hours: 6,
            snoozed_until: None,
        };
        let until = compute_snooze_until(&body).expect("until");
        let now = Utc::now();
        // 应当在 now + 6h 附近（容差 5s）
        let dt = until.as_datetime();
        let delta = (dt - now - chrono::Duration::hours(6)).num_seconds().abs();
        assert!(delta < 5, "expected ~6h, got delta={}s", delta);
    }

    #[test]
    fn snooze_prefers_explicit_until() {
        let explicit = Utc::now() + chrono::Duration::days(7);
        let body = SnoozeBody {
            item_key: "issue/456".to_owned(),
            hours: 1, // 此值应被忽略
            snoozed_until: Some(explicit),
        };
        let until = compute_snooze_until(&body).expect("until");
        let dt = until.as_datetime();
        let delta = (dt - explicit).num_seconds().abs();
        assert!(delta < 2, "expected ~7d, got delta={}s", delta);
    }

    #[test]
    fn snooze_rejects_zero_hours_and_no_until() {
        let body = SnoozeBody {
            item_key: "x".to_owned(),
            hours: 0,
            snoozed_until: None,
        };
        assert!(compute_snooze_until(&body).is_none());
    }
}
