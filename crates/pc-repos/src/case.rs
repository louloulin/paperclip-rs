//! `cases` 聚合及其链接、事件、文档、标签与附件。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Draft,
    InProgress,
    InReview,
    Approved,
    Done,
    Cancelled,
}

impl CaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Approved => "approved",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

impl std::str::FromStr for CaseStatus {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "draft" => Ok(Self::Draft),
            "in_progress" => Ok(Self::InProgress),
            "in_review" => Ok(Self::InReview),
            "approved" => Ok(Self::Approved),
            "done" => Ok(Self::Done),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err("invalid case status"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseLinkRole {
    Origin,
    Work,
    Reference,
}

impl CaseLinkRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Origin => "origin",
            Self::Work => "work",
            Self::Reference => "reference",
        }
    }
}

impl std::str::FromStr for CaseLinkRole {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "origin" => Ok(Self::Origin),
            "work" => Ok(Self::Work),
            "reference" => Ok(Self::Reference),
            _ => Err("invalid case link role"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseEventKind {
    Created,
    Updated,
    FieldsChanged,
    StatusChanged,
    IssueLinked,
    IssueUnlinked,
    DocumentRevised,
    ChildLinked,
    AttachmentAdded,
    LabelAdded,
    LabelRemoved,
}

impl CaseEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::FieldsChanged => "fields_changed",
            Self::StatusChanged => "status_changed",
            Self::IssueLinked => "issue_linked",
            Self::IssueUnlinked => "issue_unlinked",
            Self::DocumentRevised => "document_revised",
            Self::ChildLinked => "child_linked",
            Self::AttachmentAdded => "attachment_added",
            Self::LabelAdded => "label_added",
            Self::LabelRemoved => "label_removed",
        }
    }
}

impl std::str::FromStr for CaseEventKind {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "created" => Ok(Self::Created),
            "updated" => Ok(Self::Updated),
            "fields_changed" => Ok(Self::FieldsChanged),
            "status_changed" => Ok(Self::StatusChanged),
            "issue_linked" => Ok(Self::IssueLinked),
            "issue_unlinked" => Ok(Self::IssueUnlinked),
            "document_revised" => Ok(Self::DocumentRevised),
            "child_linked" => Ok(Self::ChildLinked),
            "attachment_added" => Ok(Self::AttachmentAdded),
            "label_added" => Ok(Self::LabelAdded),
            "label_removed" => Ok(Self::LabelRemoved),
            _ => Err("invalid case event kind"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseActorType {
    User,
    Agent,
    System,
}

impl CaseActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct CaseRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub case_number: i32,
    pub identifier: String,
    pub case_type: String,
    pub key: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub status: String,
    pub fields: serde_json::Value,
    pub parent_case_id: Option<Uuid>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub completed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl CaseRow {
    pub fn case_status(&self) -> Option<CaseStatus> {
        self.status.parse().ok()
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseIssueLinkRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub issue_id: Uuid,
    pub role: String,
    pub created_by_run_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseEventRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub kind: String,
    pub actor_type: String,
    pub actor_user_id: Option<String>,
    pub actor_agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseDocumentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub document_id: Uuid,
    pub key: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseLabelRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub label_id: Uuid,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseAttachmentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub asset_id: Uuid,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone)]
pub struct CaseActor {
    pub actor_type: CaseActorType,
    pub actor_user_id: Option<String>,
    pub actor_agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
}

impl CaseActor {
    pub fn system() -> Self {
        Self {
            actor_type: CaseActorType::System,
            actor_user_id: None,
            actor_agent_id: None,
            run_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewCaseRecord {
    pub company_id: Uuid,
    pub project_id: Option<Uuid>,
    pub case_type: String,
    pub key: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub status: CaseStatus,
    pub fields: serde_json::Value,
    pub parent_case_id: Option<Uuid>,
    pub actor: CaseActor,
}

#[derive(Debug, Clone)]
pub struct CaseUpsertResult {
    pub row: CaseRow,
    pub created: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CaseFilter {
    pub case_types: Vec<String>,
    pub statuses: Vec<CaseStatus>,
    pub project_ids: Vec<Uuid>,
    pub include_no_project: bool,
    pub label_id: Option<Uuid>,
    pub parent_case_id: Option<Uuid>,
    pub search: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct CasePatch {
    pub project_id: Option<Option<Uuid>>,
    pub title: Option<String>,
    pub summary: Option<Option<String>>,
    pub status: Option<CaseStatus>,
    pub fields: Option<serde_json::Value>,
    pub parent_case_id: Option<Option<Uuid>>,
}

const CASE_COLS: &str = "id, company_id, project_id, case_number, identifier, case_type, key, \
                         title, summary, status, fields, parent_case_id, created_by_agent_id, \
                         created_by_user_id, completed_at, created_at, updated_at";
const ISSUE_LINK_COLS: &str = "id, company_id, case_id, issue_id, role, created_by_run_id, \
                              created_at, updated_at";
const EVENT_COLS: &str = "id, company_id, case_id, kind, actor_type, actor_user_id, actor_agent_id, \
                          run_id, payload, created_at, updated_at";
const DOCUMENT_COLS: &str =
    "id, company_id, case_id, document_id, key, created_at, updated_at";
const LABEL_COLS: &str = "id, company_id, case_id, label_id, created_at, updated_at";
const ATTACHMENT_COLS: &str = "id, company_id, case_id, asset_id, created_at, updated_at";

pub struct CaseRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CaseRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> sqlx::Result<Vec<CaseRow>> {
        self.list_by_company_filtered(company_id, &CaseFilter::default())
            .await
    }

    pub async fn list_by_company_filtered(
        &self,
        company_id: Uuid,
        filter: &CaseFilter,
    ) -> sqlx::Result<Vec<CaseRow>> {
        let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(format!(
            "SELECT {CASE_COLS} FROM cases c WHERE c.company_id="
        ));
        query.push_bind(company_id);
        if !filter.case_types.is_empty() {
            query
                .push(" AND c.case_type=ANY(")
                .push_bind(filter.case_types.clone())
                .push("::text[])");
        }
        if !filter.statuses.is_empty() {
            let statuses: Vec<String> = filter
                .statuses
                .iter()
                .map(|status| status.as_str().to_owned())
                .collect();
            query
                .push(" AND c.status=ANY(")
                .push_bind(statuses)
                .push("::text[])");
        }
        if !filter.project_ids.is_empty() {
            query.push(" AND (");
            if filter.include_no_project {
                query.push("c.project_id IS NULL OR ");
            }
            query
                .push("c.project_id=ANY(")
                .push_bind(filter.project_ids.clone())
                .push("::uuid[]))");
        } else if filter.include_no_project {
            query.push(" AND c.project_id IS NULL");
        }
        if let Some(label_id) = filter.label_id {
            query
                .push(" AND EXISTS (SELECT 1 FROM case_labels cl WHERE cl.company_id=c.company_id ")
                .push("AND cl.case_id=c.id AND cl.label_id=")
                .push_bind(label_id)
                .push(")");
        }
        if let Some(parent_case_id) = filter.parent_case_id {
            query
                .push(" AND c.parent_case_id=")
                .push_bind(parent_case_id);
        }
        if let Some(search) = filter.search.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            let pattern = format!("%{search}%");
            query
                .push(" AND (c.title ILIKE ")
                .push_bind(pattern.clone())
                .push(" OR c.identifier ILIKE ")
                .push_bind(pattern.clone())
                .push(" OR c.summary ILIKE ")
                .push_bind(pattern)
                .push(")");
        }
        query
            .push(" ORDER BY c.created_at DESC, c.id DESC LIMIT ")
            .push_bind(filter.limit.unwrap_or(200).clamp(1, 200));
        query
            .build_query_as::<CaseRow>()
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_all(&self, limit: i64) -> sqlx::Result<Vec<CaseRow>> {
        let sql = format!("SELECT {CASE_COLS} FROM cases ORDER BY created_at DESC LIMIT $1");
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(limit.clamp(1, 1_000))
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get(&self, id: Uuid) -> sqlx::Result<Option<CaseRow>> {
        let sql = format!("SELECT {CASE_COLS} FROM cases WHERE id=$1");
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn get_for_company(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> sqlx::Result<Option<CaseRow>> {
        let sql = format!("SELECT {CASE_COLS} FROM cases WHERE company_id=$1 AND id=$2");
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn get_by_id_or_identifier(
        &self,
        company_id: Uuid,
        identity: &str,
    ) -> sqlx::Result<Option<CaseRow>> {
        let id = Uuid::parse_str(identity).ok();
        let sql = format!(
            "SELECT {CASE_COLS} FROM cases WHERE company_id=$1 \
             AND (identifier=upper($2) OR ($3::uuid IS NOT NULL AND id=$3)) LIMIT 1"
        );
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(company_id)
            .bind(identity.trim())
            .bind(id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn create(
        &self,
        company_id: Uuid,
        case_type: &str,
        title: &str,
        project_id: Option<Uuid>,
        summary: Option<&str>,
    ) -> sqlx::Result<CaseRow> {
        let mut transaction = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("paperclip:cases:{company_id}"))
            .execute(&mut *transaction)
            .await?;
        let next_number: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(case_number), 0) + 1 FROM cases WHERE company_id=$1",
        )
        .bind(company_id)
        .fetch_one(&mut *transaction)
        .await?;
        let identifier = format!("CASE-{}", Uuid::new_v4().simple());
        let sql = format!(
            "INSERT INTO cases \
                (company_id, case_type, title, project_id, summary, case_number, identifier) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING {CASE_COLS}"
        );
        let row = sqlx::query_as::<_, CaseRow>(&sql)
            .bind(company_id)
            .bind(case_type)
            .bind(title)
            .bind(project_id)
            .bind(summary)
            .bind(next_number)
            .bind(identifier)
            .fetch_one(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(row)
    }

    pub async fn create_or_update(
        &self,
        input: NewCaseRecord,
    ) -> sqlx::Result<CaseUpsertResult> {
        let mut transaction = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!(
                "paperclip:case-upsert:{}:{}:{}",
                input.company_id,
                input.case_type,
                input.key.as_deref().unwrap_or("<null>")
            ))
            .execute(&mut *transaction)
            .await?;
        let existing_sql = format!(
            "SELECT {CASE_COLS} FROM cases WHERE company_id=$1 AND case_type=$2 \
             AND key IS NOT DISTINCT FROM $3 FOR UPDATE"
        );
        if let Some(existing) = sqlx::query_as::<_, CaseRow>(&existing_sql)
            .bind(input.company_id)
            .bind(&input.case_type)
            .bind(&input.key)
            .fetch_optional(&mut *transaction)
            .await?
        {
            let sql = format!(
                "UPDATE cases SET project_id=COALESCE($2,project_id), title=$3, \
                    summary=COALESCE($4,summary), status=$5, fields=$6, \
                    parent_case_id=COALESCE($7,parent_case_id), \
                    completed_at=CASE WHEN $5 IN ('done','cancelled') \
                        THEN COALESCE(completed_at,now()) ELSE NULL END, updated_at=now() \
                 WHERE company_id=$1 AND id=$8 RETURNING {CASE_COLS}"
            );
            let row = sqlx::query_as::<_, CaseRow>(&sql)
                .bind(input.company_id)
                .bind(input.project_id)
                .bind(&input.title)
                .bind(&input.summary)
                .bind(input.status.as_str())
                .bind(&input.fields)
                .bind(input.parent_case_id)
                .bind(existing.id)
                .fetch_one(&mut *transaction)
                .await?;
            Self::insert_event_with_executor(
                &mut transaction,
                input.company_id,
                row.id,
                CaseEventKind::Updated,
                &input.actor,
                serde_json::json!({"upsert": true}),
            )
            .await?;
            transaction.commit().await?;
            return Ok(CaseUpsertResult {
                row,
                created: false,
            });
        }

        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("paperclip:cases:{}", input.company_id))
            .execute(&mut *transaction)
            .await?;
        let issue_prefix: String =
            sqlx::query_scalar("SELECT issue_prefix FROM companies WHERE id=$1")
                .bind(input.company_id)
                .fetch_one(&mut *transaction)
                .await?;
        let case_number: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(case_number),0)+1 FROM cases WHERE company_id=$1",
        )
        .bind(input.company_id)
        .fetch_one(&mut *transaction)
        .await?;
        let identifier = format!("{}-C{case_number}", issue_prefix.to_uppercase());
        let sql = format!(
            "INSERT INTO cases \
                (company_id, project_id, case_number, identifier, case_type, key, title, summary, \
                 status, fields, parent_case_id, created_by_agent_id, created_by_user_id, completed_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, \
                 CASE WHEN $9 IN ('done','cancelled') THEN now() ELSE NULL END) \
             RETURNING {CASE_COLS}"
        );
        let row = sqlx::query_as::<_, CaseRow>(&sql)
            .bind(input.company_id)
            .bind(input.project_id)
            .bind(case_number)
            .bind(identifier)
            .bind(&input.case_type)
            .bind(&input.key)
            .bind(&input.title)
            .bind(&input.summary)
            .bind(input.status.as_str())
            .bind(&input.fields)
            .bind(input.parent_case_id)
            .bind(input.actor.actor_agent_id)
            .bind(&input.actor.actor_user_id)
            .fetch_one(&mut *transaction)
            .await?;
        Self::insert_event_with_executor(
            &mut transaction,
            input.company_id,
            row.id,
            CaseEventKind::Created,
            &input.actor,
            serde_json::json!({"identifier": row.identifier}),
        )
        .await?;
        transaction.commit().await?;
        Ok(CaseUpsertResult { row, created: true })
    }

    pub async fn update(
        &self,
        id: Uuid,
        title: Option<&str>,
        summary: Option<&str>,
        status: Option<&str>,
    ) -> sqlx::Result<Option<CaseRow>> {
        let sql = format!(
            "UPDATE cases SET title=COALESCE($2,title), summary=COALESCE($3,summary), \
                status=COALESCE($4,status), \
                completed_at=CASE WHEN $4 IS NULL THEN completed_at \
                    WHEN $4 IN ('done','cancelled') THEN COALESCE(completed_at,now()) ELSE NULL END, \
                updated_at=now() WHERE id=$1 RETURNING {CASE_COLS}"
        );
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(id)
            .bind(title)
            .bind(summary)
            .bind(status)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn update_full(
        &self,
        company_id: Uuid,
        id: Uuid,
        patch: CasePatch,
    ) -> sqlx::Result<Option<CaseRow>> {
        let project_specified = patch.project_id.is_some();
        let project_id = patch.project_id.flatten();
        let summary_specified = patch.summary.is_some();
        let summary = patch.summary.flatten();
        let parent_specified = patch.parent_case_id.is_some();
        let parent_case_id = patch.parent_case_id.flatten();
        let sql = format!(
            "UPDATE cases SET \
                project_id=CASE WHEN $3 THEN $4 ELSE project_id END, \
                title=COALESCE($5,title), \
                summary=CASE WHEN $6 THEN $7 ELSE summary END, \
                status=COALESCE($8,status), \
                fields=COALESCE($9,fields), \
                parent_case_id=CASE WHEN $10 THEN $11 ELSE parent_case_id END, \
                completed_at=CASE WHEN $8 IS NULL THEN completed_at \
                    WHEN $8 IN ('done','cancelled') THEN COALESCE(completed_at,now()) ELSE NULL END, \
                updated_at=now() WHERE company_id=$1 AND id=$2 RETURNING {CASE_COLS}"
        );
        sqlx::query_as::<_, CaseRow>(&sql)
            .bind(company_id)
            .bind(id)
            .bind(project_specified)
            .bind(project_id)
            .bind(patch.title)
            .bind(summary_specified)
            .bind(summary)
            .bind(patch.status.map(CaseStatus::as_str))
            .bind(patch.fields)
            .bind(parent_specified)
            .bind(parent_case_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn delete(&self, id: Uuid) -> sqlx::Result<bool> {
        let result = sqlx::query("DELETE FROM cases WHERE id=$1")
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn delete_for_company(&self, company_id: Uuid, id: Uuid) -> sqlx::Result<bool> {
        let result = sqlx::query("DELETE FROM cases WHERE company_id=$1 AND id=$2")
            .bind(company_id)
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn list_issue_links(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseIssueLinkRow>> {
        let sql = format!(
            "SELECT {ISSUE_LINK_COLS} FROM case_issue_links \
             WHERE company_id=$1 AND case_id=$2 ORDER BY created_at ASC"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn list_case_links_for_issue(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<CaseIssueLinkRow>> {
        let sql = format!(
            "SELECT {ISSUE_LINK_COLS} FROM case_issue_links \
             WHERE company_id=$1 AND issue_id=$2 ORDER BY created_at ASC"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(issue_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn link_issue(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        issue_id: Uuid,
        role: CaseLinkRole,
        created_by_run_id: Option<Uuid>,
    ) -> sqlx::Result<CaseIssueLinkRow> {
        let sql = format!(
            "INSERT INTO case_issue_links \
                (company_id, case_id, issue_id, role, created_by_run_id) \
             VALUES ($1,$2,$3,$4,$5) ON CONFLICT (case_id,issue_id) DO UPDATE SET \
                role=EXCLUDED.role, created_by_run_id=COALESCE(EXCLUDED.created_by_run_id, \
                case_issue_links.created_by_run_id), updated_at=now() \
             RETURNING {ISSUE_LINK_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(issue_id)
            .bind(role.as_str())
            .bind(created_by_run_id)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn unlink_issue(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<Option<CaseIssueLinkRow>> {
        let sql = format!(
            "DELETE FROM case_issue_links WHERE company_id=$1 AND case_id=$2 AND issue_id=$3 \
             RETURNING {ISSUE_LINK_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(issue_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn list_events(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<CaseEventRow>> {
        let sql = format!(
            "SELECT {EVENT_COLS} FROM case_events WHERE company_id=$1 AND case_id=$2 \
             ORDER BY created_at DESC, id DESC LIMIT $3"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(limit.clamp(1, 500))
            .fetch_all(self.db.pool())
            .await
    }

/// Round 106: 按 case_id 单查（不需 company_id），用于 `GET /api/cases/:id/events`
    /// 这种纯 id-based 端点。
    pub async fn list_events_by_case_id(
        &self,
        case_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<CaseEventRow>> {
        let limit = limit.clamp(1, 500);
        let sql = format!(
            "SELECT {EVENT_COLS} FROM case_events WHERE case_id=$1              ORDER BY created_at DESC, id DESC LIMIT $2"
        );
        sqlx::query_as::<_, CaseEventRow>(&sql)
            .bind(case_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

        /// 跨 case 列出公司在指定 kind 下的事件（company-level feed）。
    /// `kind_filter` 为 `None` 时返回所有 kind；为 `Some("")` 时同样返回所有（与原路由兼容）。
    pub async fn list_events_by_company(
        &self,
        company_id: Uuid,
        kind_filter: Option<&str>,
        limit: i64,
    ) -> sqlx::Result<Vec<CaseEventRow>> {
        let limit = limit.clamp(1, 500);
        let sql = if kind_filter.map(str::trim).filter(|s| !s.is_empty()).is_some() {
            format!(
                "SELECT {EVENT_COLS} FROM case_events \
                 WHERE company_id=$1 AND kind=$2 \
                 ORDER BY created_at DESC, id DESC LIMIT $3"
            )
        } else {
            format!(
                "SELECT {EVENT_COLS} FROM case_events \
                 WHERE company_id=$1 \
                 ORDER BY created_at DESC, id DESC LIMIT $2"
            )
        };
        let q = sqlx::query_as::<_, CaseEventRow>(&sql).bind(company_id);
        let q = if let Some(kind) = kind_filter.filter(|s| !s.trim().is_empty()) {
            q.bind(kind).bind(limit)
        } else {
            q.bind(limit)
        };
        q.fetch_all(self.db.pool()).await
    }

    pub async fn create_event(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        kind: CaseEventKind,
        actor: &CaseActor,
        payload: serde_json::Value,
    ) -> sqlx::Result<CaseEventRow> {
        let mut transaction = self.db.pool().begin().await?;
        let row = Self::insert_event_with_executor(
            &mut transaction,
            company_id,
            case_id,
            kind,
            actor,
            payload,
        )
        .await?;
        transaction.commit().await?;
        Ok(row)
    }

    async fn insert_event_with_executor(
        executor: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        company_id: Uuid,
        case_id: Uuid,
        kind: CaseEventKind,
        actor: &CaseActor,
        payload: serde_json::Value,
    ) -> sqlx::Result<CaseEventRow> {
        let sql = format!(
            "INSERT INTO case_events \
                (company_id,case_id,kind,actor_type,actor_user_id,actor_agent_id,run_id,payload) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {EVENT_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(kind.as_str())
            .bind(actor.actor_type.as_str())
            .bind(&actor.actor_user_id)
            .bind(actor.actor_agent_id)
            .bind(actor.run_id)
            .bind(payload)
            .fetch_one(&mut **executor)
            .await
    }

    pub async fn list_documents(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseDocumentRow>> {
        let sql = format!(
            "SELECT {DOCUMENT_COLS} FROM case_documents WHERE company_id=$1 AND case_id=$2 \
             ORDER BY key ASC"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn get_document(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        key: &str,
    ) -> sqlx::Result<Option<CaseDocumentRow>> {
        let sql = format!(
            "SELECT {DOCUMENT_COLS} FROM case_documents \
             WHERE company_id=$1 AND case_id=$2 AND key=$3"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(key)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn link_document(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        document_id: Uuid,
        key: &str,
    ) -> sqlx::Result<CaseDocumentRow> {
        let sql = format!(
            "INSERT INTO case_documents (company_id,case_id,document_id,key) VALUES ($1,$2,$3,$4) \
             ON CONFLICT (company_id,case_id,key) DO UPDATE SET \
                document_id=EXCLUDED.document_id, updated_at=now() RETURNING {DOCUMENT_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(document_id)
            .bind(key)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn unlink_document(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        key: &str,
    ) -> sqlx::Result<Option<CaseDocumentRow>> {
        let sql = format!(
            "DELETE FROM case_documents WHERE company_id=$1 AND case_id=$2 AND key=$3 \
             RETURNING {DOCUMENT_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(key)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn list_labels(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseLabelRow>> {
        let sql = format!(
            "SELECT {LABEL_COLS} FROM case_labels WHERE company_id=$1 AND case_id=$2 \
             ORDER BY created_at ASC"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn add_label(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        label_id: Uuid,
    ) -> sqlx::Result<CaseLabelRow> {
        let sql = format!(
            "INSERT INTO case_labels (company_id,case_id,label_id) VALUES ($1,$2,$3) \
             ON CONFLICT (case_id,label_id) DO UPDATE SET updated_at=now() RETURNING {LABEL_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(label_id)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn remove_label(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        label_id: Uuid,
    ) -> sqlx::Result<Option<CaseLabelRow>> {
        let sql = format!(
            "DELETE FROM case_labels WHERE company_id=$1 AND case_id=$2 AND label_id=$3 \
             RETURNING {LABEL_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(label_id)
            .fetch_optional(self.db.pool())
            .await
    }

    pub async fn replace_labels(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        label_ids: &[Uuid],
    ) -> sqlx::Result<Vec<CaseLabelRow>> {
        let unique: std::collections::BTreeSet<Uuid> = label_ids.iter().copied().collect();
        let mut transaction = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("paperclip:case-labels:{company_id}:{case_id}"))
            .execute(&mut *transaction)
            .await?;
        if !unique.is_empty() {
            let values: Vec<Uuid> = unique.iter().copied().collect();
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM labels WHERE company_id=$1 AND id=ANY($2::uuid[])",
            )
            .bind(company_id)
            .bind(&values)
            .fetch_one(&mut *transaction)
            .await?;
            if count != values.len() as i64 {
                return Err(sqlx::Error::RowNotFound);
            }
        }
        sqlx::query("DELETE FROM case_labels WHERE company_id=$1 AND case_id=$2")
            .bind(company_id)
            .bind(case_id)
            .execute(&mut *transaction)
            .await?;
        let sql = format!(
            "INSERT INTO case_labels (company_id,case_id,label_id) VALUES ($1,$2,$3) \
             RETURNING {LABEL_COLS}"
        );
        let mut rows = Vec::with_capacity(unique.len());
        for label_id in unique {
            rows.push(
                sqlx::query_as(&sql)
                    .bind(company_id)
                    .bind(case_id)
                    .bind(label_id)
                    .fetch_one(&mut *transaction)
                    .await?,
            );
        }
        transaction.commit().await?;
        Ok(rows)
    }

    pub async fn list_attachments(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseAttachmentRow>> {
        let sql = format!(
            "SELECT {ATTACHMENT_COLS} FROM case_attachments \
             WHERE company_id=$1 AND case_id=$2 ORDER BY created_at ASC"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .fetch_all(self.db.pool())
            .await
    }

    pub async fn add_attachment(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        asset_id: Uuid,
    ) -> sqlx::Result<CaseAttachmentRow> {
        let sql = format!(
            "INSERT INTO case_attachments (company_id,case_id,asset_id) VALUES ($1,$2,$3) \
             ON CONFLICT (asset_id) DO UPDATE SET updated_at=now() RETURNING {ATTACHMENT_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(asset_id)
            .fetch_one(self.db.pool())
            .await
    }

    pub async fn remove_attachment(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        attachment_id: Uuid,
    ) -> sqlx::Result<Option<CaseAttachmentRow>> {
        let sql = format!(
            "DELETE FROM case_attachments WHERE company_id=$1 AND case_id=$2 AND id=$3 \
             RETURNING {ATTACHMENT_COLS}"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .bind(attachment_id)
            .fetch_optional(self.db.pool())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::{CaseEventKind, CaseLinkRole, CaseStatus};

    #[test]
    fn case_schema_enum_values_round_trip() {
        for status in [
            CaseStatus::Draft,
            CaseStatus::InProgress,
            CaseStatus::InReview,
            CaseStatus::Approved,
            CaseStatus::Done,
            CaseStatus::Cancelled,
        ] {
            assert_eq!(status.as_str().parse(), Ok(status));
        }
        for role in [
            CaseLinkRole::Origin,
            CaseLinkRole::Work,
            CaseLinkRole::Reference,
        ] {
            assert_eq!(role.as_str().parse(), Ok(role));
        }
        for kind in [
            CaseEventKind::Created,
            CaseEventKind::Updated,
            CaseEventKind::FieldsChanged,
            CaseEventKind::StatusChanged,
            CaseEventKind::IssueLinked,
            CaseEventKind::IssueUnlinked,
            CaseEventKind::DocumentRevised,
            CaseEventKind::ChildLinked,
            CaseEventKind::AttachmentAdded,
            CaseEventKind::LabelAdded,
            CaseEventKind::LabelRemoved,
        ] {
            assert_eq!(kind.as_str().parse(), Ok(kind));
        }
    }

    #[test]
    fn only_done_and_cancelled_are_terminal() {
        assert!(CaseStatus::Done.is_terminal());
        assert!(CaseStatus::Cancelled.is_terminal());
        assert!(!CaseStatus::Approved.is_terminal());
        assert!(!CaseStatus::InReview.is_terminal());
    }
}
