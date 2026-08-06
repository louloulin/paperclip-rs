//! `agent_action_audit` 域 — 代理（agent）触发的活动审计查询。
//!
//! 对齐 `paperclip/server/src/services/agent-action-audit.ts`：
//! - 公司维度，按 `agent_id IS NOT NULL` 过滤
//! - cursor 分页（`(created_at, id)` 复合 key，base64url 编码 JSON）
//! - `responsible_user_id` 来自 `activity_log.responsible_user_id`，
//!   缺失时回退到 `heartbeat_runs.responsible_user_id`（coalesce）
//! - 富化：把 `entity_type = issue / issue_comment / issue_document`
//!   的行关联到可见 issue（`hidden_at IS NULL`）上，输出 issue snippet
//!   + comment excerpt + document key
//! - 详情 redact：调用 `crate::redact::sanitize_record` 遮罩 secrets

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::redact::sanitize_record;
use crate::Db;

pub const DEFAULT_LIMIT: i64 = 50;
pub const MAX_LIMIT: i64 = 200;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActionAuditFilters {
    pub company_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
    pub run_id: Option<Uuid>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub action: Option<String>,
    pub actor_type: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActionAuditItem {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_type: String,
    pub actor_id: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub details: Option<Value>,
    pub entity: AgentActionAuditEntity,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentActionAuditEntity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<AuditIssueSnippet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<AuditCommentSnippet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<AuditDocumentSnippet>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditIssueSnippet {
    pub id: Uuid,
    pub identifier: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditCommentSnippet {
    pub id: Uuid,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditDocumentSnippet {
    pub id: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActionAuditPage {
    pub items: Vec<AgentActionAuditItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CursorValue {
    created_at: DateTime<Utc>,
    id: Uuid,
}

pub fn encode_cursor(created_at: DateTime<Utc>, id: Uuid) -> String {
    let value = CursorValue { created_at, id };
    let json = serde_json::to_vec(&value).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
}

pub fn decode_cursor(cursor: &str) -> Result<CursorValue, CursorError> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor.as_bytes())
        .map_err(|_| CursorError::Invalid)?;
    let value: CursorValue = serde_json::from_slice(&bytes).map_err(|_| CursorError::Invalid)?;
    Ok(value)
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CursorError {
    #[error("invalid audit cursor")]
    Invalid,
}

pub fn normalize_limit(value: Option<i64>) -> i64 {
    value.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

pub fn excerpt(value: &str, max_length: usize) -> String {
    let normalized: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_length {
        return normalized;
    }
    let mut out: String = normalized
        .chars()
        .take(max_length.saturating_sub(1))
        .collect();
    out.push('…');
    out
}

#[derive(Clone)]
pub struct AgentActionAuditRepo {
    db: Db,
}

impl AgentActionAuditRepo {
    pub fn new(db: &Db) -> Self {
        Self { db: db.clone() }
    }

    /// 列出公司在指定过滤条件下的 agent 活动审计。
    ///
    /// 算法（对齐 `server/src/services/agent-action-audit.ts`）：
    /// 1. 解码 cursor；缺失 / 错误 → 400
    /// 2. 主查询：`activity_log` LEFT JOIN `heartbeat_runs ON run_id`，
    ///    取 `coalesce(responsible_user_id)`，按 `created_at DESC, id DESC` 排序
    /// 3. 富化：`issue_comment` / `issue` / `issue_document` 三类行再做关联
    /// 4. 详情 redact：调用 `crate::redact::sanitize_record`
    /// 5. 编码 next_cursor：基于本页最后一行 `(created_at, id)`
    pub async fn list(
        &self,
        filters: AgentActionAuditFilters,
    ) -> Result<AgentActionAuditPage, RepoErr> {
        let decoded_cursor = match filters.cursor.as_deref() {
            None => None,
            Some(cursor) => Some(decode_cursor(cursor).map_err(|_| RepoErr::BadCursor)?),
        };
        let limit = normalize_limit(filters.limit);

        // Main query
        let rows = sqlx::query(
            r#"
            SELECT
              a.id,
              a.company_id,
              a.actor_type,
              a.actor_id,
              a.action,
              a.entity_type,
              a.entity_id,
              a.agent_id,
              a.run_id,
              a.details,
              a.created_at,
              to_char(a.created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')
                AS cursor_created_at,
              coalesce(a.responsible_user_id, h.responsible_user_id) AS effective_responsible_user_id
            FROM activity_log a
            LEFT JOIN heartbeat_runs h
              ON h.id = a.run_id AND h.company_id = a.company_id
            WHERE a.company_id = $1
              AND a.agent_id IS NOT NULL
              AND ($2::uuid IS NULL OR a.agent_id = $2)
              AND ($3::text IS NULL OR coalesce(a.responsible_user_id, h.responsible_user_id) = $3)
              AND ($4::uuid IS NULL OR a.run_id = $4)
              AND ($5::text IS NULL OR a.entity_type = $5)
              AND ($6::text IS NULL OR a.entity_id = $6)
              AND ($7::text IS NULL OR starts_with(a.action, $7))
              AND ($8::text IS NULL OR a.actor_type = $8)
              AND ($9::timestamptz IS NULL OR a.created_at >= $9)
              AND ($10::timestamptz IS NULL OR a.created_at <= $10)
              AND (
                $11::timestamptz IS NULL
                OR a.created_at < $11
                OR (a.created_at = $11 AND a.id < $12)
              )
            ORDER BY a.created_at DESC, a.id DESC
            LIMIT $13
            "#,
        )
        .bind(filters.company_id)
        .bind(filters.agent_id)
        .bind(filters.responsible_user_id.as_deref())
        .bind(filters.run_id)
        .bind(filters.entity_type.as_deref())
        .bind(filters.entity_id.as_deref())
        .bind(filters.action.as_deref())
        .bind(filters.actor_type.as_deref())
        .bind(filters.from)
        .bind(filters.to)
        .bind(decoded_cursor.as_ref().map(|c| c.created_at))
        .bind(decoded_cursor.as_ref().map(|c| c.id))
        .bind(limit + 1)
        .fetch_all(self.db.pool())
        .await?;

        let has_more = rows.len() as i64 > limit;
        let page_rows = if has_more {
            &rows[..limit as usize]
        } else {
            &rows[..]
        };

        // Hydrate comments
        let comment_ids: Vec<String> = page_rows
            .iter()
            .filter(|r| {
                r.try_get::<String, _>("entity_type").ok().as_deref() == Some("issue_comment")
            })
            .filter_map(|r| r.try_get::<String, _>("entity_id").ok())
            .collect();
        let comment_ids_unique: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            comment_ids
                .into_iter()
                .filter(|id| seen.insert(id.clone()))
                .collect()
        };
        let comment_rows: Vec<(Uuid, String, Uuid, Option<String>, Option<String>)> =
            if comment_ids_unique.is_empty() {
                Vec::new()
            } else {
                sqlx::query_as(
                    r#"
                    SELECT c.id, c.body, i.id, i.identifier, i.title
                    FROM issue_comments c
                    INNER JOIN issues i
                      ON i.id = c.issue_id
                     AND i.company_id = c.company_id
                    WHERE c.company_id = $1
                      AND i.hidden_at IS NULL
                      AND c.id = ANY($2::uuid[])
                    "#,
                )
                .bind(filters.company_id)
                .bind(&comment_ids_unique)
                .fetch_all(self.db.pool())
                .await?
            };
        let comment_map: std::collections::HashMap<
            Uuid,
            (String, Uuid, Option<String>, Option<String>),
        > = comment_rows
            .into_iter()
            .map(|(id, body, issue_id, identifier, title)| {
                (id, (body, issue_id, identifier, title))
            })
            .collect();

        // Hydrate issues
        let issue_ids: Vec<String> = page_rows
            .iter()
            .filter(|r| r.try_get::<String, _>("entity_type").ok().as_deref() == Some("issue"))
            .filter_map(|r| r.try_get::<String, _>("entity_id").ok())
            .collect();
        let issue_ids_unique: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            issue_ids
                .into_iter()
                .filter(|id| seen.insert(id.clone()))
                .collect()
        };
        let issue_rows: Vec<(Uuid, Option<String>, Option<String>)> = if issue_ids_unique.is_empty()
        {
            Vec::new()
        } else {
            sqlx::query_as(
                r#"
                SELECT id, identifier, title
                FROM issues
                WHERE company_id = $1
                  AND hidden_at IS NULL
                  AND id = ANY($2::uuid[])
                "#,
            )
            .bind(filters.company_id)
            .bind(&issue_ids_unique)
            .fetch_all(self.db.pool())
            .await?
        };
        let issue_map: std::collections::HashMap<Uuid, (Option<String>, Option<String>)> =
            issue_rows
                .into_iter()
                .map(|(id, i, t)| (id, (i, t)))
                .collect();

        // Hydrate documents
        let document_ids: Vec<String> = page_rows
            .iter()
            .filter(|r| {
                r.try_get::<String, _>("entity_type").ok().as_deref() == Some("issue_document")
            })
            .filter_map(|r| r.try_get::<String, _>("entity_id").ok())
            .collect();
        let document_ids_unique: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            document_ids
                .into_iter()
                .filter(|id| seen.insert(id.clone()))
                .collect()
        };
        let document_rows: Vec<(Uuid, Uuid, String, Uuid, Option<String>, Option<String>)> =
            if document_ids_unique.is_empty() {
                Vec::new()
            } else {
                sqlx::query_as(
                    r#"
                    SELECT d.id, d.document_id, d.key, i.id, i.identifier, i.title
                    FROM issue_documents d
                    INNER JOIN issues i
                      ON i.id = d.issue_id
                     AND i.company_id = d.company_id
                    WHERE d.company_id = $1
                      AND i.hidden_at IS NULL
                      AND (d.id = ANY($2::uuid[]) OR d.document_id = ANY($2::text[]))
                    "#,
                )
                .bind(filters.company_id)
                .bind(&document_ids_unique)
                .fetch_all(self.db.pool())
                .await?
            };
        let mut document_map: std::collections::HashMap<
            String,
            (String, Uuid, Option<String>, Option<String>),
        > = std::collections::HashMap::new();
        for (id, document_id, key, issue_id, identifier, title) in document_rows {
            let snippet = (key, issue_id, identifier, title);
            document_map.insert(id.to_string(), snippet.clone());
            document_map.insert(document_id.to_string(), snippet);
        }

        // Build items
        let mut items = Vec::with_capacity(page_rows.len());
        for row in page_rows {
            let id: Uuid = row.try_get("id")?;
            let company_id: Uuid = row.try_get("company_id")?;
            let actor_type: String = row.try_get("actor_type")?;
            let actor_id: String = row.try_get("actor_id")?;
            let action: String = row.try_get("action")?;
            let entity_type: String = row.try_get("entity_type")?;
            let entity_id: String = row.try_get("entity_id")?;
            let agent_id: Option<Uuid> = row.try_get("agent_id")?;
            let run_id: Option<Uuid> = row.try_get("run_id")?;
            let details: Option<Value> = row.try_get("details")?;
            let created_at: DateTime<Utc> = row.try_get("created_at")?;
            let responsible_user_id: Option<String> =
                row.try_get("effective_responsible_user_id")?;

            // Hydrate entity
            let mut entity = AgentActionAuditEntity::default();
            let mut issue_snippet: Option<AuditIssueSnippet> = None;

            if entity_type == "issue_comment" {
                if let Ok(uuid) = Uuid::parse_str(&entity_id) {
                    if let Some((body, issue_id, identifier, title)) = comment_map.get(&uuid) {
                        entity.comment = Some(AuditCommentSnippet {
                            id: uuid,
                            excerpt: excerpt(body, 280),
                        });
                        issue_snippet = Some(AuditIssueSnippet {
                            id: *issue_id,
                            identifier: identifier.clone(),
                            title: title.clone(),
                        });
                    }
                }
            } else if entity_type == "issue_document" {
                if let Some((key, issue_id, identifier, title)) = document_map.get(&entity_id) {
                    entity.document = Some(AuditDocumentSnippet {
                        id: entity_id.clone(),
                        key: key.clone(),
                    });
                    issue_snippet = Some(AuditIssueSnippet {
                        id: *issue_id,
                        identifier: identifier.clone(),
                        title: title.clone(),
                    });
                }
            } else if entity_type == "issue" {
                if let Ok(uuid) = Uuid::parse_str(&entity_id) {
                    if let Some((identifier, title)) = issue_map.get(&uuid) {
                        issue_snippet = Some(AuditIssueSnippet {
                            id: uuid,
                            identifier: identifier.clone(),
                            title: title.clone(),
                        });
                    }
                }
            }

            let is_issue_derived = entity_type == "issue"
                || entity_type == "issue_comment"
                || entity_type == "issue_document";
            entity.issue = issue_snippet;

            // Redact details
            let redacted_details = match details {
                Some(value) if is_issue_derived && entity.issue.is_none() => None,
                Some(value) => Some(sanitize_record(&value)),
                None => None,
            };

            items.push(AgentActionAuditItem {
                id,
                company_id,
                actor_type,
                actor_id,
                action,
                entity_type,
                entity_id,
                agent_id,
                run_id,
                responsible_user_id,
                created_at,
                details: redacted_details,
                entity,
            });
        }

        let next_cursor = if has_more {
            items
                .last()
                .map(|item| encode_cursor(item.created_at, item.id))
        } else {
            None
        };

        Ok(AgentActionAuditPage { items, next_cursor })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepoErr {
    #[error("invalid audit cursor")]
    BadCursor,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cursor_round_trip_preserves_precision() {
        let ts = Utc.with_ymd_and_hms(2026, 7, 17, 12, 34, 56).unwrap()
            + chrono::Duration::microseconds(789_012);
        let id = Uuid::new_v4();
        let cursor = encode_cursor(ts, id);
        let parsed = decode_cursor(&cursor).expect("cursor parses");
        assert_eq!(parsed.id, id);
        assert_eq!(parsed.created_at, ts);
    }

    #[test]
    fn cursor_rejects_garbage() {
        assert_eq!(
            decode_cursor("not-base64").unwrap_err(),
            CursorError::Invalid
        );
        let wrong_json =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"{\"id\":\"not-a-uuid\"}");
        assert_eq!(
            decode_cursor(&wrong_json).unwrap_err(),
            CursorError::Invalid
        );
    }

    #[test]
    fn limit_clamps_within_bounds() {
        assert_eq!(normalize_limit(None), DEFAULT_LIMIT);
        assert_eq!(normalize_limit(Some(0)), 1);
        assert_eq!(normalize_limit(Some(99_999)), MAX_LIMIT);
        assert_eq!(normalize_limit(Some(50)), 50);
    }

    #[test]
    fn excerpt_truncates_with_ellipsis() {
        assert_eq!(excerpt("hello world", 32), "hello world");
        let long = "a".repeat(300);
        let out = excerpt(&long, 280);
        assert_eq!(out.chars().count(), 280);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn excerpt_collapses_whitespace() {
        assert_eq!(excerpt("  hello\n   world  \t!", 32), "hello world !");
    }
}
