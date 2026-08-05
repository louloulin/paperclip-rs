//! `cases` 聚合及其链接、事件、文档、标签与附件。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use serde_json::{json, Value};
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

/// Round 113: case_issue_links + issues JOIN 投影。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseIssueLinkWithIssueRow {
    pub id: Uuid,
    pub case_id: Uuid,
    pub issue_id: Uuid,
    pub role: String,
    pub created_by_run_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub issue_title: Option<String>,
    pub issue_status: Option<String>,
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

/// Round 119: case 文档批注列表投影（document_annotations JOIN case_documents）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseDocumentAnnotationRow {
    pub id: Uuid,
    pub kind: String,
    pub thread_id: Option<String>,
    pub payload: serde_json::Value,
}

/// Round 119: issue → cases 反向查询（case_issue_links JOIN cases）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCaseLinkRow {
    pub link_id: Uuid,
    pub case_id: Uuid,
    pub role: String,
    pub project_id: Option<Uuid>,
    pub parent_case_id: Option<Uuid>,
    pub status: Option<String>,
    pub linked_at: Timestamp,
}

/// Round 120: breakdown 子 case 输入参数（用于复合方法 breakdown_case）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewBreakdownChild {
    pub title: String,
    pub case_type: Option<String>,
    pub summary: Option<String>,
    pub fields: Option<serde_json::Value>,
}

/// Round 120: case context_pack 事件投影（最近 50 条 case_events）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseContextEventRow {
    pub kind: String,
    pub actor_type: String,
    pub actor_user_id: Option<String>,
    pub actor_agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub payload: Option<serde_json::Value>,
    pub created_at: Timestamp,
}

/// Round 120: case context_pack 关联 issue 投影（case_issue_links JOIN issues）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseContextIssueRow {
    pub id: Uuid,
    pub title: String,
    pub status: Option<String>,
}

/// Round 120: case outputs 列表（case_issue_links JOIN issues, 含 link role + completed_at）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseOutputRow {
    pub id: Uuid,
    pub title: String,
    pub status: Option<String>,
    pub link_role: String,
    pub completed_at: Option<Timestamp>,
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

// ─────────────────────────────────────────────────────────────────────
// Round 114: case annotation 子模块类型
// ─────────────────────────────────────────────────────────────────────

/// `document_annotation_threads` 当 `case_id` 不为空时（case 文档批注）。
/// 1:1 schema 投影（除了 routine 版多 `original_revision_id` 字段）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseAnnotationThreadRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub document_id: Uuid,
    pub document_key: String,
    pub status: String,
    pub anchor_state: String,
    pub original_revision_id: Option<Uuid>,
    pub original_revision_number: i32,
    pub current_revision_id: Option<Uuid>,
    pub current_revision_number: i32,
    pub selected_text: String,
    pub prefix_text: String,
    pub suffix_text: String,
    pub normalized_start: i32,
    pub normalized_end: i32,
    pub markdown_start: i32,
    pub markdown_end: i32,
    pub anchor_confidence: String,
    pub anchor_selector: Value,
    pub resolved_at: Option<Timestamp>,
    pub resolved_by_user_id: Option<String>,
    pub resolved_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `document_annotation_comments` 1:1 schema 投影（case 路径）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseAnnotationCommentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub thread_id: Uuid,
    pub document_id: Uuid,
    pub body: String,
    pub author_type: String,
    pub author_user_id: Option<String>,
    pub author_agent_id: Option<Uuid>,
    pub created_at: Timestamp,
}

/// Round 114: 创建 case annotation thread 输入。
#[derive(Debug, Clone)]
pub struct NewCaseAnnotationThread {
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub document_id: Uuid,
    pub document_key: String,
    pub status: Option<String>,
    pub original_revision_id: Option<Uuid>,
    pub revision_number: i32,
    pub selected_text: String,
    pub prefix_text: Option<String>,
    pub suffix_text: Option<String>,
    pub normalized_start: i32,
    pub normalized_end: i32,
    pub markdown_start: i32,
    pub markdown_end: i32,
    pub anchor_confidence: Option<String>,
    pub anchor_selector: Option<Value>,
}

/// Round 114: 创建 case annotation comment 输入。
#[derive(Debug, Clone)]
pub struct NewCaseAnnotationComment {
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub thread_id: Uuid,
    pub document_id: Uuid,
    pub body: String,
    pub author_type: String,
    pub author_user_id: Option<String>,
    pub author_agent_id: Option<Uuid>,
}

/// Round 117: case rollup 复合聚合结果。
#[derive(Debug, Clone)]
pub struct CaseRollupRow {
    pub child_count: i64,
    pub descendant_count: i64,
    pub issue_link_count: i64,
    pub open_issue_count: i64,
    pub status_breakdown: Vec<(String, i64)>,
}

/// Round 116: document_revisions 1:1 schema 投影 (without body/format 留给 routes 决定)。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRevisionRow {
    pub id: Uuid,
    pub revision_number: i32,
    pub title: Option<String>,
    pub format: Option<String>,
    pub change_summary: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub created_at: Timestamp,
}

/// Round 114: case annotation thread patch 输入。
#[derive(Debug, Clone, Default)]
pub struct CaseAnnotationPatch {
    pub status: Option<String>,
    pub anchor_selector: Option<Value>,
    pub anchor_state: Option<String>,
    pub current_revision_id: Option<Uuid>,
    pub current_revision_number: Option<i32>,
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

    /// Round 174: 实例统计用 —— 统计某公司的 case 数。
    pub async fn count_for_company(&self, company_id: Uuid) -> sqlx::Result<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM cases WHERE company_id=$1",
        )
        .bind(company_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
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

    // ---- Round 113: case_issue_links 路由仓储化 ----

    /// Round 113: 记录 issue_linked 事件到 case_events。
    pub async fn record_issue_linked_event(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        issue_id: Uuid,
        role: &str,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)                 VALUES ($1, $2, 'issue_linked', 'user', jsonb_build_object('issueId',$3::text,'role',$4::text)) RETURNING id",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(issue_id.to_string())
        .bind(role)
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Round 113: 记录 issue_unlinked 事件到 case_events。
    pub async fn record_issue_unlinked_event(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        issue_id: Uuid,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)                 VALUES ($1, $2, 'issue_unlinked', 'user', jsonb_build_object('issueId',$3::text)) RETURNING id",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(issue_id.to_string())
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Round 113: 列出 case 的 issue links (JOIN issues 取 title/status)。
    pub async fn list_issue_links_with_issue(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseIssueLinkWithIssueRow>> {
        sqlx::query_as::<_, CaseIssueLinkWithIssueRow>(
            "SELECT cil.id, cil.case_id, cil.issue_id, cil.role, cil.created_by_run_id,                     cil.created_at, i.title AS issue_title, i.status AS issue_status                 FROM case_issue_links cil                 INNER JOIN issues i ON i.id = cil.issue_id AND i.company_id = cil.company_id                 WHERE cil.company_id = $1 AND cil.case_id = $2                 ORDER BY cil.created_at ASC",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 113: 按 link_id 删除 case_issue_link，返回被删的 issue_id（None = 找不到）。
    /// 一步完成 SELECT + DELETE（用 RETURNING 避免 race）。
    pub async fn delete_issue_link_by_id(
        &self,
        company_id: Uuid,
        link_id: Uuid,
    ) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "DELETE FROM case_issue_links WHERE id = $1 AND company_id = $2 RETURNING issue_id",
        )
        .bind(link_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(i,)| i))
    }

    // ---- Round 114: case annotation 子模块 ----

    /// Round 114: 查 case 的 company_id（auth 辅助）。
    pub async fn get_case_company_id(&self, case_id: Uuid) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> =
            sqlx::query_as("SELECT company_id FROM cases WHERE id = $1")
                .bind(case_id)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row.map(|(c,)| c))
    }

    /// Round 114: 查 (case_id, key) 对应的 (company_id, document_id)。
    /// None = (case, key) 不存在。
    pub async fn resolve_case_document_id(
        &self,
        case_id: Uuid,
        key: &str,
    ) -> sqlx::Result<Option<(Uuid, Uuid)>> {
        let row: Option<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT company_id, document_id FROM case_documents WHERE case_id = $1 AND key = $2",
        )
        .bind(case_id)
        .bind(key)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 114: 列出 case annotation threads (按 case_id + document_key 过滤)。
    pub async fn list_case_annotation_threads(
        &self,
        case_id: Uuid,
        document_key: &str,
        status_filter: Option<&str>,
        limit: i64,
    ) -> sqlx::Result<Vec<CaseAnnotationThreadRow>> {
        let mut sql = String::from(
            "SELECT id, company_id, case_id, document_id, document_key, status,                     anchor_state, original_revision_id, original_revision_number,                     current_revision_id, current_revision_number,                     selected_text, prefix_text, suffix_text, normalized_start,                     normalized_end, markdown_start, markdown_end, anchor_confidence,                     anchor_selector, resolved_at, resolved_by_user_id, resolved_by_agent_id,                     created_by_user_id, created_by_agent_id, created_at, updated_at                 FROM document_annotation_threads                 WHERE case_id = $1 AND document_key = $2",
        );
        if let Some(s) = status_filter {
            sql.push_str(&format!(" AND status = '{}'", s));
        }
        sql.push_str(" ORDER BY created_at DESC LIMIT $3");
        sqlx::query_as::<_, CaseAnnotationThreadRow>(&sql)
            .bind(case_id)
            .bind(document_key)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await
    }

    /// Round 114: 取单个 case annotation thread。
    pub async fn get_case_annotation_thread(
        &self,
        case_id: Uuid,
        thread_id: Uuid,
        document_key: &str,
    ) -> sqlx::Result<Option<CaseAnnotationThreadRow>> {
        sqlx::query_as::<_, CaseAnnotationThreadRow>(
            "SELECT id, company_id, case_id, document_id, document_key, status,                     anchor_state, original_revision_id, original_revision_number,                     current_revision_id, current_revision_number,                     selected_text, prefix_text, suffix_text, normalized_start,                     normalized_end, markdown_start, markdown_end, anchor_confidence,                     anchor_selector, resolved_at, resolved_by_user_id, resolved_by_agent_id,                     created_by_user_id, created_by_agent_id, created_at, updated_at                 FROM document_annotation_threads                 WHERE id = $1 AND case_id = $2 AND document_key = $3",
        )
        .bind(thread_id)
        .bind(case_id)
        .bind(document_key)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 114: 列 thread comments。
    pub async fn list_case_thread_comments(
        &self,
        thread_id: Uuid,
    ) -> sqlx::Result<Vec<CaseAnnotationCommentRow>> {
        sqlx::query_as::<_, CaseAnnotationCommentRow>(
            "SELECT id, company_id, case_id, thread_id, document_id, body, author_type,                     author_user_id, author_agent_id, created_at                 FROM document_annotation_comments                 WHERE thread_id = $1 ORDER BY created_at ASC",
        )
        .bind(thread_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 114: 批量取多个 case thread 的 comments。
    pub async fn list_case_thread_comments_bulk(
        &self,
        thread_ids: &[Uuid],
    ) -> sqlx::Result<Vec<CaseAnnotationCommentRow>> {
        sqlx::query_as::<_, CaseAnnotationCommentRow>(
            "SELECT id, company_id, case_id, thread_id, document_id, body, author_type,                     author_user_id, author_agent_id, created_at                 FROM document_annotation_comments                 WHERE thread_id = ANY($1::uuid[]) ORDER BY created_at ASC",
        )
        .bind(thread_ids)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 114: 创建 case annotation thread。
    pub async fn create_case_annotation_thread(
        &self,
        input: &NewCaseAnnotationThread,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO document_annotation_threads (                company_id, case_id, document_id, document_key, status, anchor_state,                original_revision_id, original_revision_number, current_revision_number,                selected_text, prefix_text, suffix_text, normalized_start,                normalized_end, markdown_start, markdown_end, anchor_confidence, anchor_selector)             VALUES ($1, $2, $3, $4, COALESCE($5, 'open'), 'active', $6, $7, $7, $8,                     COALESCE($9, ''), COALESCE($10, ''), $11, $12, $13, $14, $15, $16)             RETURNING id",
        )
        .bind(input.company_id)
        .bind(input.case_id)
        .bind(input.document_id)
        .bind(&input.document_key)
        .bind(input.status.as_deref())
        .bind(input.original_revision_id)
        .bind(input.revision_number)
        .bind(&input.selected_text)
        .bind(input.prefix_text.as_deref())
        .bind(input.suffix_text.as_deref())
        .bind(input.normalized_start)
        .bind(input.normalized_end)
        .bind(input.markdown_start)
        .bind(input.markdown_end)
        .bind(input.anchor_confidence.as_deref().unwrap_or("exact"))
        .bind(input.anchor_selector.clone().unwrap_or_else(|| Value::Object(Default::default())))
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Round 114: 创建 case annotation comment。
    pub async fn create_case_thread_comment(
        &self,
        input: &NewCaseAnnotationComment,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO document_annotation_comments (                company_id, case_id, thread_id, document_id, body, author_type,                author_user_id, author_agent_id)             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(input.company_id)
        .bind(input.case_id)
        .bind(input.thread_id)
        .bind(input.document_id)
        .bind(&input.body)
        .bind(&input.author_type)
        .bind(input.author_user_id.as_deref())
        .bind(input.author_agent_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Round 114: 更新 case annotation thread（COALESCE + status 触发 resolved_at）。
    pub async fn update_case_annotation_thread(
        &self,
        case_id: Uuid,
        thread_id: Uuid,
        document_key: &str,
        patch: &CaseAnnotationPatch,
    ) -> sqlx::Result<u64> {
        let r = sqlx::query(
            "UPDATE document_annotation_threads SET                status = COALESCE($1, status),                anchor_selector = COALESCE($2, anchor_selector),                anchor_state = COALESCE($3, anchor_state),                current_revision_id = COALESCE($4, current_revision_id),                current_revision_number = COALESCE($5, current_revision_number),                resolved_at = CASE WHEN $1 = 'resolved' THEN now()                                   WHEN $1 IN ('open', 'outdated') THEN NULL                                   ELSE resolved_at END,                updated_at = now()             WHERE id = $6 AND case_id = $7 AND document_key = $8",
        )
        .bind(patch.status.as_deref())
        .bind(patch.anchor_selector.clone())
        .bind(patch.anchor_state.as_deref())
        .bind(patch.current_revision_id)
        .bind(patch.current_revision_number)
        .bind(thread_id)
        .bind(case_id)
        .bind(document_key)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected())
    }

    /// Round 114: 取 thread 的 document_id（comment insert 需要）。
    pub async fn get_case_thread_document_id(
        &self,
        case_id: Uuid,
        thread_id: Uuid,
        document_key: &str,
    ) -> sqlx::Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT document_id FROM document_annotation_threads             WHERE id = $1 AND case_id = $2 AND document_key = $3",
        )
        .bind(thread_id)
        .bind(case_id)
        .bind(document_key)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(d,)| d))
    }

    // ---- Round 115: case_attachments 仓储化 ----

    /// Round 115: upsert case_attachments (case_id + asset_id)。
    /// ON CONFLICT (case_id, asset_id) DO UPDATE 触发 updated_at 刷新。
    pub async fn upsert_case_attachment(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        asset_id: Uuid,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO case_attachments (company_id, case_id, asset_id)             VALUES ($1, $2, $3)             ON CONFLICT (case_id, asset_id) DO UPDATE SET updated_at = now()             RETURNING id",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(asset_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    /// Round 115: 记录 attachment_added 事件到 case_events。
    pub async fn record_attachment_added_event(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        asset_id: Uuid,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)             VALUES ($1, $2, 'attachment_added', 'user', jsonb_build_object('assetId', $3::text)) RETURNING id",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(asset_id.to_string())
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    // ---- Round 118: 通用 case_events 记录（review / suggest / resolve / acknowledge / document_delete） ----

    /// Round 118: 通用 case_events 记录助手。
    ///
    /// 用于无法套用专用 `record_*_event` 形态的端点（review、suggest-transition、
    /// resolve-suggestion、acknowledge-drift、delete-case-document 等）。kind 与
    /// actor_type 由调用方提供字符串字面量；payload 为完整 JSON。
    pub async fn record_case_event(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        kind: &str,
        actor_type: &str,
        payload: Value,
    ) -> sqlx::Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(kind)
        .bind(actor_type)
        .bind(payload)
        .fetch_one(self.db.pool())
        .await?;
        Ok(id)
    }

    // ---- Round 116: case document revisions ----

    /// Round 116: 列出 document_revisions (按 revision_number DESC)。
    pub async fn list_document_revisions(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        limit: i64,
    ) -> sqlx::Result<Vec<DocumentRevisionRow>> {
        sqlx::query_as::<_, DocumentRevisionRow>(
            "SELECT id, revision_number, title, format, change_summary,                 created_by_agent_id, created_by_user_id, created_at                 FROM document_revisions                 WHERE company_id = $1 AND document_id = $2                 ORDER BY revision_number DESC LIMIT $3",
        )
        .bind(company_id)
        .bind(document_id)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 116: 取单个 document_revision 的 body + title。
    /// None = revision 不存在或 company 不匹配。
    pub async fn get_document_revision_body(
        &self,
        company_id: Uuid,
        document_id: Uuid,
        revision_id: Uuid,
    ) -> sqlx::Result<Option<(String, Option<String>)>> {
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT body, title FROM document_revisions             WHERE id = $1 AND document_id = $2 AND company_id = $3",
        )
        .bind(revision_id)
        .bind(document_id)
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 116: 复合事务 — restore 一个 document revision。
    /// 内部完成:
    /// 1. 计算 next revision_number
    /// 2. INSERT 新 document_revision
    /// 3. UPDATE documents latest_body / latest_revision_id / latest_revision_number
    /// 4. INSERT case_events kind='document_revised' 含 restoredFromRevisionId + newRevisionId
    /// 返回 (new_revision_id, new_revision_number)
    pub async fn restore_document_revision(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        key: &str,
        document_id: Uuid,
        source_body: &str,
        source_title: Option<&str>,
        change_summary: &str,
        source_revision_id: Uuid,
    ) -> sqlx::Result<(Uuid, i32)> {
        let mut tx = self.db.pool().begin().await?;
        let next_no: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(revision_number), 0) + 1 FROM document_revisions WHERE document_id = $1",
        )
        .bind(document_id)
        .fetch_one(&mut *tx)
        .await?;
        let new_rev_id: Uuid = sqlx::query_scalar(
            "INSERT INTO document_revisions (company_id, document_id, revision_number, body, change_summary, title)             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(company_id)
        .bind(document_id)
        .bind(next_no)
        .bind(source_body)
        .bind(change_summary)
        .bind(source_title)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE documents SET latest_body = $1, latest_revision_id = $2,                 latest_revision_number = $3, updated_at = now() WHERE id = $4",
        )
        .bind(source_body)
        .bind(new_rev_id)
        .bind(next_no)
        .bind(document_id)
        .execute(&mut *tx)
        .await?;
        let _ = sqlx::query(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)             VALUES ($1, $2, 'document_revised', 'user', jsonb_build_object('key', $3::text, 'restoredFromRevisionId', $4::text, 'newRevisionId', $5::text))",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(key)
        .bind(source_revision_id)
        .bind(new_rev_id)
        .execute(&mut *tx)
        .await;
        tx.commit().await?;
        Ok((new_rev_id, next_no))
    }

    // ---- Round 117: case rollup (composite aggregate) ----

    /// Round 117: 复合聚合 — get_case_rollup。
    /// 一次调用返回 5 个聚合统计:
    /// 1. child_count (直接子 case 数)
    /// 2. descendant_count (递归所有后代 case 数，CTE)
    /// 3. issue_link_count (case 关联的所有 issue 数)
    /// 4. open_issue_count (关联的 status NOT IN done/cancelled/closed 的 issue 数)
    /// 5. status_breakdown (case + 直接子 case 的 status 分组统计)
    pub async fn get_case_rollup(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<CaseRollupRow> {
        let child_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM cases WHERE company_id=$1 AND parent_case_id=$2",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_one(self.db.pool())
        .await?;
        let descendant_count: i64 = sqlx::query_scalar(
            "WITH RECURSIVE descendants AS (                SELECT id, parent_case_id FROM cases WHERE company_id=$1 AND parent_case_id=$2                UNION ALL                SELECT c.id, c.parent_case_id FROM cases c                  INNER JOIN descendants d ON c.parent_case_id = d.id                  WHERE c.company_id=$1              ) SELECT count(*)::bigint FROM descendants",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_one(self.db.pool())
        .await?;
        let issue_link_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM case_issue_links WHERE company_id=$1 AND case_id=$2",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_one(self.db.pool())
        .await?;
        let open_issue_count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM case_issue_links cil              INNER JOIN issues i ON i.id = cil.issue_id AND i.company_id = cil.company_id              WHERE cil.company_id=$1 AND cil.case_id=$2              AND i.status NOT IN ('done','cancelled','closed')",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_one(self.db.pool())
        .await?;
        let status_breakdown: Vec<(String, i64)> = sqlx::query_as(
            "SELECT status, count(*)::bigint FROM cases              WHERE company_id=$1 AND (id=$2 OR parent_case_id=$2)              GROUP BY status",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(CaseRollupRow {
            child_count,
            descendant_count,
            issue_link_count,
            open_issue_count,
            status_breakdown,
        })
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

/// Round 109: 锁定 case_document (touch updated_at) 并发 case_event。
    /// 合并为单事务返回 OK 是否成功。
    pub async fn lock_document(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        key: &str,
    ) -> sqlx::Result<bool> {
        let mut tx = self.db.pool().begin().await?;
        let row: Option<(Uuid,)> = sqlx::query_as(
            "UPDATE case_documents SET updated_at = now()              WHERE company_id=$1 AND case_id=$2 AND key=$3 RETURNING id",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if row.is_none() {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)              VALUES ($1, $2, 'document_locked', 'user', jsonb_build_object('key',$3::text))",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(key)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Round 109: 解锁 case_document (只发 event)。
    pub async fn unlock_document(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        key: &str,
    ) -> sqlx::Result<bool> {
        let mut tx = self.db.pool().begin().await?;
        let exists: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM case_documents WHERE company_id=$1 AND case_id=$2 AND key=$3",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if exists.is_none() {
            tx.rollback().await?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload)              VALUES ($1, $2, 'document_unlocked', 'user', jsonb_build_object('key',$3::text))",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(key)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
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

    // ---- Round 119: case CRUD / list 清扫 ----

    /// Round 119: 列出 case 绑定的文档批注（document_annotations JOIN case_documents）。
    pub async fn list_case_document_annotations(
        &self,
        case_id: Uuid,
        key: &str,
    ) -> sqlx::Result<Vec<CaseDocumentAnnotationRow>> {
        sqlx::query_as::<_, CaseDocumentAnnotationRow>(
            "SELECT id, kind, thread_id, payload FROM document_annotations             WHERE document_id IN (SELECT document_id FROM case_documents WHERE case_id = $1 AND key = $2)             ORDER BY created_at DESC LIMIT 200",
        )
        .bind(case_id)
        .bind(key)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 119: 列出 issue 关联的所有 cases（case_issue_links JOIN cases）。
    pub async fn list_issue_cases(
        &self,
        issue_id: Uuid,
    ) -> sqlx::Result<Vec<IssueCaseLinkRow>> {
        sqlx::query_as::<_, IssueCaseLinkRow>(
            "SELECT cil.id AS link_id, cil.case_id, cil.role, c.project_id, c.parent_case_id, c.status, cil.created_at AS linked_at             FROM case_issue_links cil JOIN cases c ON c.id = cil.case_id             WHERE cil.issue_id = $1 ORDER BY cil.created_at DESC LIMIT 200",
        )
        .bind(issue_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 119: 列出 case 的直接子 case（parent_case_id = $2）。
    pub async fn list_children(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseRow>> {
        let sql = format!(
            "SELECT {CASE_COLS} FROM cases WHERE company_id=$1 AND parent_case_id=$2             ORDER BY created_at ASC LIMIT 200"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .bind(case_id)
            .fetch_all(self.db.pool())
            .await
    }

    /// Round 120: 统计 case 的直接子 case 数（用于 context_pack 的 childCount）。
    pub async fn count_children(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<i64> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)::bigint FROM cases WHERE company_id=$1 AND parent_case_id=$2",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(count)
    }

    /// Round 119: 列出 company 全部 cases（用于构建 children tree，limit 5000）。
    pub async fn list_all_for_tree(
        &self,
        company_id: Uuid,
    ) -> sqlx::Result<Vec<CaseRow>> {
        let sql = format!(
            "SELECT {CASE_COLS} FROM cases WHERE company_id=$1             ORDER BY parent_case_id NULLS FIRST, created_at ASC LIMIT 5000"
        );
        sqlx::query_as(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await
    }

    // ---- Round 120: case 复合事务 + list 系列 ----

    /// Round 120: breakdown 复合事务——一次调用插入 N 个 child case + 各事件，单事务原子。
    pub async fn breakdown_case(
        &self,
        company_id: Uuid,
        parent_case_id: Uuid,
        parent_project_id: Option<Uuid>,
        parent_case_type: &str,
        children: Vec<NewBreakdownChild>,
        note: Option<&str>,
    ) -> sqlx::Result<Vec<Uuid>> {
        if children.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.db.pool().begin().await?;
        let max_number: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(case_number), 0) FROM cases WHERE company_id=$1",
        )
        .bind(company_id)
        .fetch_one(&mut *tx)
        .await?;
        let mut next_number = max_number + 1;
        let mut created_ids: Vec<Uuid> = Vec::with_capacity(children.len());
        for child in &children {
            let case_type = child
                .case_type
                .clone()
                .unwrap_or_else(|| parent_case_type.to_owned());
            let identifier = format!("CASE-{}", next_number);
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO cases (company_id, project_id, case_number, identifier, case_type, key, \
                                    title, summary, status, fields, parent_case_id, created_by_user_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'draft', $9, $10, $11) RETURNING id",
            )
            .bind(company_id)
            .bind(parent_project_id)
            .bind(next_number)
            .bind(&identifier)
            .bind(&case_type)
            .bind::<Option<String>>(None)
            .bind(&child.title)
            .bind(child.summary.as_deref())
            .bind(child.fields.clone().unwrap_or_else(|| serde_json::json!({})))
            .bind(parent_case_id)
            .bind::<Option<String>>(None)
            .fetch_one(&mut *tx)
            .await?;
            let _ = sqlx::query(
                "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload) \
                 VALUES ($1, $2, 'child_linked', 'user', jsonb_build_object('childCaseId',$3::text,'note',$4::text))",
            )
            .bind(company_id)
            .bind(parent_case_id)
            .bind(id.to_string())
            .bind(note.unwrap_or(""))
            .execute(&mut *tx)
            .await;
            created_ids.push(id);
            next_number += 1;
        }
        tx.commit().await?;
        Ok(created_ids)
    }

    /// Round 120: replace blockers 复合事务——清空 + 重插 + 事件记录，单事务原子。
    pub async fn replace_blockers(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        blocked_by_case_ids: Vec<Uuid>,
        event_payload: serde_json::Value,
    ) -> sqlx::Result<()> {
        let mut tx = self.db.pool().begin().await?;
        let _ = sqlx::query("DELETE FROM pipeline_case_blockers WHERE case_id=$1")
            .bind(case_id)
            .execute(&mut *tx)
            .await?;
        for blocker_id in &blocked_by_case_ids {
            if *blocker_id == case_id {
                continue;
            }
            let _ = sqlx::query(
                "INSERT INTO pipeline_case_blockers (company_id, case_id, blocked_by_case_id) \
                 VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind(company_id)
            .bind(case_id)
            .bind(blocker_id)
            .execute(&mut *tx)
            .await;
        }
        tx.commit().await?;
        let _ = sqlx::query(
            "INSERT INTO case_events (company_id, case_id, kind, actor_type, payload) \
             VALUES ($1, $2, 'fields_changed', 'user', $3)",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(event_payload)
        .execute(self.db.pool())
        .await;
        Ok(())
    }

    /// Round 120: open conversation 复合事务——创建 issue + link + event。
    pub async fn open_conversation(
        &self,
        company_id: Uuid,
        case_id: Uuid,
        case_title: &str,
        existing_issue_id: Option<Uuid>,
        initial_message: Option<&str>,
    ) -> sqlx::Result<Uuid> {
        let issue_id = if let Some(id) = existing_issue_id {
            id
        } else {
            let title = format!("Conversation: {}", case_title);
            sqlx::query_scalar(
                "INSERT INTO issues (company_id, title, description, status, priority, origin_kind, origin_fingerprint) \
                 VALUES ($1, $2, $3, 'todo', 'medium', 'case_conversation', $4) RETURNING id",
            )
            .bind(company_id)
            .bind(&title)
            .bind(initial_message.unwrap_or(""))
            .bind(format!("case-conversation:{}", case_id))
            .fetch_one(self.db.pool())
            .await?
        };
        let _ = sqlx::query(
            "INSERT INTO case_issue_links (company_id, case_id, issue_id, role) \
             VALUES ($1, $2, $3, 'origin') ON CONFLICT (case_id, issue_id) DO NOTHING",
        )
        .bind(company_id)
        .bind(case_id)
        .bind(issue_id)
        .execute(self.db.pool())
        .await;
        let _ = self
            .record_case_event(
                company_id,
                case_id,
                "issue_linked",
                "user",
                json!({ "issueId": issue_id.to_string(), "initialMessage": initial_message }),
            )
            .await;
        Ok(issue_id)
    }

    /// Round 120: case context_pack 事件列表（最近 50 条 case_events）。
    pub async fn list_context_events(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseContextEventRow>> {
        sqlx::query_as::<_, CaseContextEventRow>(
            "SELECT kind, actor_type, actor_user_id, actor_agent_id, run_id, payload, created_at \
             FROM case_events WHERE company_id=$1 AND case_id=$2 \
             ORDER BY created_at DESC LIMIT 50",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 120: case context_pack 关联 issue 列表。
    pub async fn list_context_issues(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseContextIssueRow>> {
        sqlx::query_as::<_, CaseContextIssueRow>(
            "SELECT i.id, i.title, i.status \
             FROM case_issue_links cil \
             INNER JOIN issues i ON i.id = cil.issue_id AND i.company_id = cil.company_id \
             WHERE cil.company_id=$1 AND cil.case_id=$2 \
             ORDER BY cil.created_at ASC",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 120: case outputs 列表（关联 issue + link role + completed_at）。
    pub async fn list_outputs(
        &self,
        company_id: Uuid,
        case_id: Uuid,
    ) -> sqlx::Result<Vec<CaseOutputRow>> {
        sqlx::query_as::<_, CaseOutputRow>(
            "SELECT i.id, i.title, i.status, cil.role AS link_role, i.completed_at \
             FROM case_issue_links cil \
             INNER JOIN issues i ON i.id = cil.issue_id AND i.company_id = cil.company_id \
             WHERE cil.company_id=$1 AND cil.case_id=$2 \
             ORDER BY cil.created_at ASC",
        )
        .bind(company_id)
        .bind(case_id)
        .fetch_all(self.db.pool())
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
