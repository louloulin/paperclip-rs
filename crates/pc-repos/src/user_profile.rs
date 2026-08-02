//! 用户资料及工作投入统计仓储。

use chrono::{DateTime, Duration, Utc};
use pc_core::Timestamp;
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

#[derive(Debug, Clone, FromRow)]
pub struct CompanyUserRow {
    pub principal_id: String,
    pub status: String,
    pub membership_role: Option<String>,
    pub membership_created_at: Timestamp,
    pub user_id: Option<String>,
    pub name: Option<String>,
    pub email: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileIdentity {
    pub id: String,
    pub slug: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub image: Option<String>,
    pub membership_role: Option<String>,
    pub membership_status: String,
    pub joined_at: Timestamp,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileWindowStats {
    pub key: String,
    pub label: String,
    pub touched_issues: i32,
    pub created_issues: i32,
    pub completed_issues: i32,
    pub assigned_open_issues: i32,
    pub comment_count: i32,
    pub activity_count: i32,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub cost_event_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileDailyPoint {
    pub date: String,
    pub activity_count: i32,
    pub completed_issues: i32,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileIssueSummary {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub assignee_agent_id: Option<Uuid>,
    pub assignee_user_id: Option<String>,
    pub updated_at: Timestamp,
    pub completed_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileActivitySummary {
    pub id: Uuid,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub details: Option<serde_json::Value>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileAgentUsage {
    pub agent_id: Uuid,
    pub agent_name: Option<String>,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileProviderUsage {
    pub provider: String,
    pub biller: String,
    pub model: String,
    pub cost_cents: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfileResponse {
    pub user: UserProfileIdentity,
    pub stats: Vec<UserProfileWindowStats>,
    pub daily: Vec<UserProfileDailyPoint>,
    pub recent_issues: Vec<UserProfileIssueSummary>,
    pub recent_activity: Vec<UserProfileActivitySummary>,
    pub top_agents: Vec<UserProfileAgentUsage>,
    pub top_providers: Vec<UserProfileProviderUsage>,
}

#[derive(Debug, Clone, FromRow)]
struct WindowIssueStats {
    touched: i32,
    created: i32,
    completed: i32,
    assigned_open: i32,
}

#[derive(Debug, Clone, FromRow)]
struct CountRow {
    count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct CostStats {
    cost_cents: i64,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    cost_event_count: i64,
}

#[derive(Debug, Clone, FromRow)]
struct DailyCountRow {
    date: String,
    count: i32,
}

#[derive(Debug, Clone, FromRow)]
struct DailyCostRow {
    date: String,
    cost_cents: i64,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
}

const ISSUE_INVOLVEMENT: &str = "(i.created_by_user_id = $2 OR i.assignee_user_id = $2 OR EXISTS (SELECT 1 FROM issue_comments ic WHERE ic.company_id = $1 AND ic.issue_id = i.id AND ic.author_user_id = $2))";
const VISIBLE_ISSUE: &str = "i.hidden_at IS NULL AND i.harness_kind IS NULL";

fn slugify_user_part(value: Option<&str>) -> Option<String> {
    let value = value?.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }
    let mut result = String::new();
    let mut pending_separator = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !result.is_empty() {
                result.push('-');
            }
            pending_separator = false;
            result.push(character);
        } else {
            pending_separator = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    (!result.is_empty()).then_some(result)
}

fn user_slug_candidates(row: &CompanyUserRow) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut add = |value: Option<&str>| {
        if let Some(slug) = slugify_user_part(value) {
            if !candidates.contains(&slug) {
                candidates.push(slug);
            }
        }
    };
    add(row.name.as_deref());
    add(row
        .email
        .as_deref()
        .and_then(|email| email.split('@').next()));
    add(row.email.as_deref());
    add(Some(&row.principal_id));
    candidates
}

fn timestamp_or_none(date: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    date
}

fn day_key(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%d").to_string()
}

pub struct UserProfileRepo<'a> {
    pub db: &'a Db,
}

impl<'a> UserProfileRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn resolve_company_user(
        &self,
        company_id: Uuid,
        raw_slug: &str,
    ) -> sqlx::Result<Option<(CompanyUserRow, String)>> {
        let slug = slugify_user_part(Some(raw_slug));
        let Some(slug) = slug else {
            return Ok(None);
        };
        let rows = sqlx::query_as::<_, CompanyUserRow>(
            "SELECT cm.principal_id, cm.status, cm.membership_role, \
                    cm.created_at AS membership_created_at, u.id AS user_id, \
                    u.name, u.email, u.image \
             FROM company_memberships cm \
             LEFT JOIN \"user\" u ON u.id = cm.principal_id \
             WHERE cm.company_id = $1 AND cm.principal_type = 'user' \
             ORDER BY cm.updated_at DESC LIMIT 200",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows.into_iter().find_map(|row| {
            let candidates = user_slug_candidates(&row);
            candidates
                .iter()
                .any(|candidate| candidate == &slug)
                .then(|| (row, candidates.into_iter().next().unwrap_or(slug.clone())))
        }))
    }

    async fn window_stats(
        &self,
        company_id: Uuid,
        user_id: &str,
        key: &str,
        label: &str,
        from: Option<DateTime<Utc>>,
    ) -> sqlx::Result<UserProfileWindowStats> {
        let issue_stats = sqlx::query_as::<_, WindowIssueStats>(&format!(
            "SELECT \
                COUNT(DISTINCT i.id) FILTER (WHERE {ISSUE_INVOLVEMENT} AND ($3::timestamptz IS NULL OR i.updated_at >= $3))::int AS touched, \
                COUNT(DISTINCT i.id) FILTER (WHERE i.created_by_user_id = $2 AND ($3::timestamptz IS NULL OR i.created_at >= $3))::int AS created, \
                COUNT(DISTINCT i.id) FILTER (WHERE {ISSUE_INVOLVEMENT} AND i.status = 'done' AND ($3::timestamptz IS NULL OR i.completed_at >= $3))::int AS completed, \
                COUNT(DISTINCT i.id) FILTER (WHERE i.assignee_user_id = $2 AND i.status IN ('backlog','todo','in_progress','in_review','blocked'))::int AS assigned_open \
             FROM issues i WHERE i.company_id = $1 AND {VISIBLE_ISSUE}",
        ))
        .bind(company_id)
        .bind(user_id)
        .bind(timestamp_or_none(from))
        .fetch_one(self.db.pool())
        .await?;

        let comment_stats = sqlx::query_as::<_, CountRow>(
            "SELECT COUNT(*)::int AS count FROM issue_comments \
             WHERE company_id = $1 AND author_user_id = $2 \
               AND ($3::timestamptz IS NULL OR created_at >= $3)",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(timestamp_or_none(from))
        .fetch_one(self.db.pool())
        .await?;

        let activity_stats = sqlx::query_as::<_, CountRow>(
            "SELECT COUNT(*)::int AS count FROM activity_log \
             WHERE company_id = $1 AND actor_type = 'user' AND actor_id = $2 \
               AND ($3::timestamptz IS NULL OR created_at >= $3)",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(timestamp_or_none(from))
        .fetch_one(self.db.pool())
        .await?;

        let cost_stats = sqlx::query_as::<_, CostStats>(&format!(
            "SELECT COALESCE(SUM(ce.cost_cents), 0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens), 0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens), 0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens), 0)::bigint AS output_tokens, \
                    COUNT(ce.id)::bigint AS cost_event_count \
             FROM cost_events ce \
             INNER JOIN issues i ON i.id = ce.issue_id AND i.company_id = ce.company_id \
             WHERE ce.company_id = $1 AND {ISSUE_INVOLVEMENT} \
               AND ($3::timestamptz IS NULL OR ce.occurred_at >= $3)",
        ))
        .bind(company_id)
        .bind(user_id)
        .bind(timestamp_or_none(from))
        .fetch_one(self.db.pool())
        .await?;

        Ok(UserProfileWindowStats {
            key: key.to_owned(),
            label: label.to_owned(),
            touched_issues: issue_stats.touched,
            created_issues: issue_stats.created,
            completed_issues: issue_stats.completed,
            assigned_open_issues: issue_stats.assigned_open,
            comment_count: comment_stats.count,
            activity_count: activity_stats.count,
            cost_cents: cost_stats.cost_cents,
            input_tokens: cost_stats.input_tokens,
            cached_input_tokens: cost_stats.cached_input_tokens,
            output_tokens: cost_stats.output_tokens,
            cost_event_count: cost_stats.cost_event_count,
        })
    }

    async fn daily_stats(
        &self,
        company_id: Uuid,
        user_id: &str,
    ) -> sqlx::Result<Vec<UserProfileDailyPoint>> {
        let now = Utc::now();
        let first_day = (now - Duration::days(13))
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid")
            .and_utc();
        let mut points: std::collections::BTreeMap<String, UserProfileDailyPoint> = (0..14)
            .map(|index| {
                let date = first_day + Duration::days(index);
                let key = day_key(date);
                (
                    key.clone(),
                    UserProfileDailyPoint {
                        date: key,
                        activity_count: 0,
                        completed_issues: 0,
                        cost_cents: 0,
                        input_tokens: 0,
                        cached_input_tokens: 0,
                        output_tokens: 0,
                    },
                )
            })
            .collect();

        let activity_rows = sqlx::query_as::<_, DailyCountRow>(
            "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS date, \
                    COUNT(*)::int AS count \
             FROM activity_log WHERE company_id = $1 AND actor_type = 'user' \
               AND actor_id = $2 AND created_at >= $3 \
             GROUP BY date",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(first_day)
        .fetch_all(self.db.pool())
        .await?;
        for row in activity_rows {
            if let Some(point) = points.get_mut(&row.date) {
                point.activity_count = row.count;
            }
        }

        let completed_rows = sqlx::query_as::<_, DailyCountRow>(&format!(
            "SELECT to_char(i.completed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS date, \
                    COUNT(DISTINCT i.id)::int AS count FROM issues i \
             WHERE i.company_id = $1 AND {VISIBLE_ISSUE} AND i.status = 'done' \
               AND i.completed_at >= $3 AND {ISSUE_INVOLVEMENT} GROUP BY date",
        ))
        .bind(company_id)
        .bind(user_id)
        .bind(first_day)
        .fetch_all(self.db.pool())
        .await?;
        for row in completed_rows {
            if let Some(point) = points.get_mut(&row.date) {
                point.completed_issues = row.count;
            }
        }

        let cost_rows = sqlx::query_as::<_, DailyCostRow>(&format!(
            "SELECT to_char(ce.occurred_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') AS date, \
                    COALESCE(SUM(ce.cost_cents), 0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens), 0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens), 0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens), 0)::bigint AS output_tokens \
             FROM cost_events ce INNER JOIN issues i \
               ON i.id = ce.issue_id AND i.company_id = ce.company_id \
             WHERE ce.company_id = $1 AND ce.occurred_at >= $3 AND {ISSUE_INVOLVEMENT} \
             GROUP BY date",
        ))
        .bind(company_id)
        .bind(user_id)
        .bind(first_day)
        .fetch_all(self.db.pool())
        .await?;
        for row in cost_rows {
            if let Some(point) = points.get_mut(&row.date) {
                point.cost_cents = row.cost_cents;
                point.input_tokens = row.input_tokens;
                point.cached_input_tokens = row.cached_input_tokens;
                point.output_tokens = row.output_tokens;
            }
        }
        Ok(points.into_values().collect())
    }

    pub async fn load(
        &self,
        company_id: Uuid,
        raw_slug: &str,
    ) -> sqlx::Result<Option<UserProfileResponse>> {
        let Some((row, canonical_slug)) = self.resolve_company_user(company_id, raw_slug).await?
        else {
            return Ok(None);
        };
        let user_id = row
            .user_id
            .clone()
            .unwrap_or_else(|| row.principal_id.clone());
        let windows = [
            ("last7", "Last 7 days", Some(Duration::days(7))),
            ("last30", "Last 30 days", Some(Duration::days(30))),
            ("all", "All time", None),
        ];
        let mut stats = Vec::with_capacity(windows.len());
        for (key, label, duration) in windows {
            let from = duration.map(|value| Utc::now() - value);
            stats.push(
                self.window_stats(company_id, &user_id, key, label, from)
                    .await?,
            );
        }
        let daily = self.daily_stats(company_id, &user_id).await?;
        let recent_issues = sqlx::query_as::<_, UserProfileIssueSummary>(&format!(
            "SELECT i.id, i.identifier, i.title, i.status, i.priority, \
                    i.assignee_agent_id, i.assignee_user_id, i.updated_at, i.completed_at \
             FROM issues i WHERE i.company_id = $1 AND {VISIBLE_ISSUE} AND {ISSUE_INVOLVEMENT} \
             ORDER BY i.updated_at DESC LIMIT 8",
        ))
        .bind(company_id)
        .bind(&user_id)
        .fetch_all(self.db.pool())
        .await?;
        let recent_activity = sqlx::query_as::<_, UserProfileActivitySummary>(
            "SELECT id, action, entity_type, entity_id, details, created_at \
             FROM activity_log WHERE company_id = $1 AND actor_type = 'user' AND actor_id = $2 \
             ORDER BY created_at DESC LIMIT 12",
        )
        .bind(company_id)
        .bind(&user_id)
        .fetch_all(self.db.pool())
        .await?;
        let top_agents = sqlx::query_as::<_, UserProfileAgentUsage>(&format!(
            "SELECT ce.agent_id, a.name AS agent_name, \
                    COALESCE(SUM(ce.cost_cents), 0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens), 0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens), 0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens), 0)::bigint AS output_tokens \
             FROM cost_events ce INNER JOIN issues i \
               ON i.id = ce.issue_id AND i.company_id = ce.company_id \
             LEFT JOIN agents a ON a.id = ce.agent_id \
             WHERE ce.company_id = $1 AND {ISSUE_INVOLVEMENT} \
             GROUP BY ce.agent_id, a.name ORDER BY cost_cents DESC LIMIT 5",
        ))
        .bind(company_id)
        .bind(&user_id)
        .fetch_all(self.db.pool())
        .await?;
        let top_providers = sqlx::query_as::<_, UserProfileProviderUsage>(&format!(
            "SELECT ce.provider, ce.biller, ce.model, \
                    COALESCE(SUM(ce.cost_cents), 0)::bigint AS cost_cents, \
                    COALESCE(SUM(ce.input_tokens), 0)::bigint AS input_tokens, \
                    COALESCE(SUM(ce.cached_input_tokens), 0)::bigint AS cached_input_tokens, \
                    COALESCE(SUM(ce.output_tokens), 0)::bigint AS output_tokens \
             FROM cost_events ce INNER JOIN issues i \
               ON i.id = ce.issue_id AND i.company_id = ce.company_id \
             WHERE ce.company_id = $1 AND {ISSUE_INVOLVEMENT} \
             GROUP BY ce.provider, ce.biller, ce.model ORDER BY cost_cents DESC LIMIT 5",
        ))
        .bind(company_id)
        .bind(&user_id)
        .fetch_all(self.db.pool())
        .await?;

        Ok(Some(UserProfileResponse {
            user: UserProfileIdentity {
                id: user_id,
                slug: canonical_slug,
                name: row.name,
                email: row.email,
                image: row.image,
                membership_role: row.membership_role,
                membership_status: row.status,
                joined_at: row.membership_created_at,
            },
            stats,
            daily,
            recent_issues,
            recent_activity,
            top_agents,
            top_providers,
        }))
    }
}
