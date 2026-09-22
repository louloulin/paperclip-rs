//! `/api/issues*` / `/api/issue-statuses*` 的响应 DTO 与请求体（从 `issues.rs` 拆出，
//! R7 单文件 800 行上限）。
#![allow(clippy::option_option)]

use mc_repos::issue::{IssueRow, IssueStatusRow};
use mc_repos::issue_status::category_str;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::context::StatusCatalog;
use super::helpers::double_option;
use super::DATE_FORMAT;

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

/// issue 响应（对齐上游 `IssueResponse` 字段名）。
#[derive(Debug, Clone, Serialize)]
pub struct IssueDto {
    pub id: String,
    pub workspace_id: String,
    pub number: i32,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub status_category: String,
    pub status_name: String,
    pub priority: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    pub creator_type: String,
    pub creator_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_issue_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub position: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    pub revision: i64,
    pub metadata: JsonValue,
    pub properties: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
}

impl IssueDto {
    pub(crate) fn from_row(row: &IssueRow, catalog: &StatusCatalog) -> Self {
        let category = catalog
            .category_of(&row.status)
            .or_else(|| row.status_category());
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            number: row.number,
            identifier: row.identifier.clone(),
            title: row.title.clone(),
            description: row.description.clone(),
            status: row.status.clone(),
            status_category: category.map_or(String::new(), |c| category_str(c).to_string()),
            status_name: catalog.display_name(row),
            priority: row.priority.clone(),
            assignee_type: row.assignee_type.clone(),
            assignee_id: row.assignee_id.clone(),
            creator_type: row.creator_type.clone(),
            creator_id: row.creator_id.clone(),
            parent_issue_id: row.parent_issue_id.map(|id| id.to_string()),
            project_id: row.project_id.map(|id| id.to_string()),
            position: row.position,
            stage: row.stage,
            start_date: row.start_date.map(|d| d.format(DATE_FORMAT).to_string()),
            due_date: row.due_date.map(|d| d.format(DATE_FORMAT).to_string()),
            revision: row.revision,
            metadata: row.metadata.clone(),
            properties: row.properties.clone(),
            origin: row.origin.clone(),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            last_activity_at: row.last_activity_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct IssueListResponse {
    pub(crate) issues: Vec<IssueDto>,
    pub(crate) total: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct IssueChildrenResponse {
    pub(crate) issues: Vec<IssueDto>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchHitDto {
    #[serde(flatten)]
    pub(crate) issue: IssueDto,
    pub(crate) match_source: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct SearchResponse {
    pub(crate) issues: Vec<SearchHitDto>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GroupedGroupDto {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) assignee_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) assignee_id: Option<String>,
    pub(crate) total: i64,
    pub(crate) done: i64,
    pub(crate) issues: Vec<IssueDto>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GroupedResponse {
    pub(crate) group_by: String,
    pub(crate) groups: Vec<GroupedGroupDto>,
}

/// 子 issue 进度。
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct ChildProgressDto {
    pub parent_issue_id: String,
    pub total: i64,
    pub done: i64,
    pub percent: f64,
}

#[derive(Debug, Serialize)]
pub(crate) struct ChildProgressResponse {
    pub(crate) progress: Vec<ChildProgressDto>,
}

#[derive(Debug, Serialize)]
pub(crate) struct MetadataResponse {
    pub(crate) metadata: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) issue_revision: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PropertiesResponse {
    pub(crate) properties: JsonValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) issue_revision: Option<i64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct UpdatedResponse {
    pub(crate) updated: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct DeletedResponse {
    pub(crate) deleted: u64,
}

/// `issue_reaction` 响应。
#[derive(Debug, Clone, Serialize)]
pub struct IssueReactionDto {
    pub id: String,
    pub issue_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub emoji: String,
    pub created_at: String,
}

/// `issue_status` 响应（对齐上游 `IssueStatusResponse`；本仓没有的列用固定值补齐）。
#[derive(Debug, Clone, Serialize)]
pub struct IssueStatusDto {
    pub id: String,
    pub workspace_id: String,
    pub key: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub is_system: bool,
    pub position: f64,
    pub archived_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl IssueStatusDto {
    pub(crate) fn from_row(row: &IssueStatusRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            key: row.key.clone(),
            name: row.name.clone(),
            // 本仓 `issue_status` 没有 description / color / archived_at 列（无新迁移可用）
            description: String::new(),
            color: String::new(),
            category: row.category.clone(),
            icon: row.icon.clone(),
            is_system: row.is_builtin(),
            position: row.position,
            archived_at: None,
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct StatusListResponse {
    pub(crate) statuses: Vec<IssueStatusDto>,
    pub(crate) categories: Vec<&'static str>,
    pub(crate) total: usize,
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CreateIssueRequest {
    pub(crate) title: String,
    pub(crate) description: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) priority: Option<String>,
    pub(crate) assignee_type: Option<String>,
    pub(crate) assignee_id: Option<String>,
    pub(crate) parent_issue_id: Option<String>,
    pub(crate) project_id: Option<String>,
    pub(crate) stage: Option<i32>,
    pub(crate) start_date: Option<String>,
    pub(crate) due_date: Option<String>,
    pub(crate) metadata: Option<JsonValue>,
    /// 上游字段：附件绑定（本仓只有校验面，无 `attachment` 表，见 `parse_attachment_ids`）
    pub(crate) attachment_ids: Vec<String>,
    /// `quick_create`（目前只接受这一个来源）
    pub(crate) origin_type: Option<String>,
    pub(crate) origin_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct UpdateIssueRequest {
    pub(crate) expected_revision: Option<i64>,
    pub(crate) title: Option<String>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) description: Option<Option<String>>,
    pub(crate) status: Option<String>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) status_name: Option<Option<String>>,
    pub(crate) priority: Option<String>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) assignee_type: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) assignee_id: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) parent_issue_id: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) project_id: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) stage: Option<Option<i32>>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) start_date: Option<Option<String>>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) due_date: Option<Option<String>>,
    /// 上游字段，本仓忽略（见 docs/11 §5）
    #[serde(deserialize_with = "double_option")]
    pub(crate) triage_state: Option<Option<String>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct BatchUpdateRequest {
    pub(crate) issue_ids: Vec<String>,
    pub(crate) updates: UpdateIssueRequest,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct BatchDeleteRequest {
    pub(crate) issue_ids: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ChildrenQuery {
    pub(crate) workspace_id: Option<String>,
    pub(crate) workspace_slug: Option<String>,
    /// 父 issue id CSV
    pub(crate) parent_ids: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct ReactionRequest {
    #[serde(default)]
    pub(crate) emoji: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ValueRequest {
    pub(crate) value: JsonValue,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct CreateIssueStatusRequest {
    pub(crate) name: String,
    pub(crate) key: Option<String>,
    pub(crate) category: String,
    pub(crate) icon: Option<String>,
    pub(crate) position: Option<f64>,
    /// 上游字段，本仓无对应列 → 接受但忽略
    pub(crate) description: Option<String>,
    pub(crate) color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct UpdateIssueStatusRequest {
    pub(crate) name: Option<String>,
    pub(crate) category: Option<String>,
    #[serde(deserialize_with = "double_option")]
    pub(crate) icon: Option<Option<String>>,
    pub(crate) position: Option<f64>,
    /// 上游字段，本仓无对应列 → 接受但忽略
    pub(crate) description: Option<String>,
    pub(crate) color: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct ReorderStatusesRequest {
    pub(crate) category: Option<String>,
    pub(crate) ids: Vec<String>,
    pub(crate) include_system: Option<bool>,
}
