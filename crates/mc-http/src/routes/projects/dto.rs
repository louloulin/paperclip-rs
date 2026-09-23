//! `/api/projects*` 的线上形状（wire shape）与请求体。
//!
//! 上游对照：`ProjectResponse` / `CreateProjectRequest` / `CreateProjectResourceRequestPayload` /
//! `UpdateProjectRequest`（`handler/project.go`）、`ProjectResourceResponse` /
//! `CreateProjectResourceRequest` / `UpdateProjectResourceRequest` /
//! `githubRepoRef` / `localDirectoryRef`（`handler/project_resource.go`）、
//! `SearchProjectResponse`（`handler/project.go`）。
//!
//! 字段名、`omitempty` 语义逐字抄写。三处**有意不同**：
//! - 时间串用本仓既有约定 `to_rfc3339()`（Go 侧 `util.TimestampToString` = `time.RFC3339`）。
//! - `SearchProjectResponse` 用 `#[serde(flatten)]` 复刻 Go 的匿名字段内联
//!   （Go 的 `ProjectResponse` 匿名嵌入没有 json tag ⇒ 字段是**平铺**的，不是嵌套对象）。
//! - `resource_ref` 用 `serde_json::Value`（Go 是 `json.RawMessage`）：`null` 与
//!   「缺失」的区别由 [`UpdateProjectResourceRequest`] 的三态字段表达。

use mc_repos::project::ProjectRow;
use mc_repos::project_resource::ProjectResourceRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use super::helpers::double_option;

fn uuid_str(id: uuid::Uuid) -> String {
    id.to_string()
}

fn date_str(date: chrono::NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

// ---------------------------------------------------------------------------
// 响应
// ---------------------------------------------------------------------------

/// 上游 `ProjectResponse`（`handler/project.go:24`）。
///
/// `description` / `icon` / `lead_type` / `lead_id` / `start_date` / `due_date` 没有
/// `omitempty` ⇒ `null` 要照发（`Option<String>` 的默认序列化行为）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectResponse {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub status: String,
    pub priority: String,
    pub lead_type: Option<String>,
    pub lead_id: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub issue_count: i64,
    pub done_count: i64,
    pub resource_count: i64,
}

impl ProjectResponse {
    /// `projectToResponse`：计数默认 0，由调用方按需填充。
    pub(crate) fn from_row(row: &ProjectRow) -> Self {
        Self {
            id: uuid_str(row.id),
            workspace_id: uuid_str(row.workspace_id),
            title: row.title.clone(),
            description: row.description.clone(),
            icon: row.icon.clone(),
            status: row.status.clone(),
            priority: row.priority.clone(),
            lead_type: row.lead_type.clone(),
            lead_id: row.lead_id.map(uuid_str),
            start_date: row.start_date.map(date_str),
            due_date: row.due_date.map(date_str),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            issue_count: 0,
            done_count: 0,
            resource_count: 0,
        }
    }
}

/// `GET /api/projects` / `GET /api/projects/search` 之外的集合响应体
/// （上游 `map[string]any{"projects": …, "total": n}`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectListResponse {
    pub projects: Vec<ProjectResponse>,
    pub total: usize,
}

/// 上游 `SearchProjectResponse`：`ProjectResponse` + `match_source`（+ 可选
/// `matched_snippet`）。Go 侧匿名字段内联 ⇒ `#[serde(flatten)]`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SearchProjectResponse {
    #[serde(flatten)]
    pub project: ProjectResponse,
    pub match_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_snippet: Option<String>,
}

/// `POST /api/projects` 带 `resources[]` 时的一次性回显（上游匿名 struct）：
/// 父项目字段平铺 + `resources`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CreateProjectEcho {
    #[serde(flatten)]
    pub project: ProjectResponse,
    pub resources: Vec<ProjectResourceResponse>,
}

/// 上游 `ProjectResourceResponse`（`handler/project_resource.go:23`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectResourceResponse {
    pub id: String,
    pub project_id: String,
    pub workspace_id: String,
    pub resource_type: String,
    pub resource_ref: JsonValue,
    pub label: Option<String>,
    pub position: i32,
    pub created_at: String,
    pub created_by: Option<String>,
}

impl ProjectResourceResponse {
    /// `projectResourceToResponse`：空 ref（SQL NULL / 零长）按 `{}` 发。
    pub(crate) fn from_row(row: &ProjectResourceRow) -> Self {
        let mut resource_ref = row.resource_ref.clone();
        if resource_ref.is_null() {
            resource_ref = json!({});
        }
        Self {
            id: uuid_str(row.id),
            project_id: uuid_str(row.project_id),
            workspace_id: uuid_str(row.workspace_id),
            resource_type: row.resource_type.clone(),
            resource_ref,
            label: row.label.clone(),
            position: row.position,
            created_at: row.created_at.to_rfc3339(),
            created_by: row.created_by.map(uuid_str),
        }
    }
}

/// `GET /api/projects/:id/resources`：`{"resources": […], "total": n}`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProjectResourceListResponse {
    pub resources: Vec<ProjectResourceResponse>,
    pub total: usize,
}

/// `GET /api/projects/search`：`{"projects": […]}`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SearchProjectsResponse {
    pub projects: Vec<SearchProjectResponse>,
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// 上游 `CreateProjectRequest`：`title` / `status` / `priority` 是**零值即缺省**的
/// 字符串（不是指针），`resources` 是 `omitempty` 切片。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct CreateProjectRequest {
    #[serde(default)]
    pub title: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub priority: String,
    pub lead_type: Option<String>,
    pub lead_id: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    #[serde(default)]
    pub resources: Vec<CreateProjectResourcePayload>,
}

/// 上游 `CreateProjectResourceRequestPayload`（捆绑创建内嵌形态，故意与单体请求分开）。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CreateProjectResourcePayload {
    #[serde(default)]
    pub resource_type: String,
    pub resource_ref: Option<JsonValue>,
    pub label: Option<String>,
    pub position: Option<i32>,
}

/// 上游 `UpdateProjectRequest`。`title` / `status` / `priority` 是 `*string`：
/// **缺失或 `null` 都算「不动」**（走 SQL 的 `COALESCE`），因此这里保持 `Option<String>`。
/// 其余字段按 `rawFields` 的 key 存在性区分，故用 `Option<Option<T>>`（三态是上游口径，
/// 豁免 `clippy::option_option`；同 `agents/dto/input.rs` 的做法）。
#[allow(clippy::option_option)]
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpdateProjectRequest {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub description: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub icon: Option<Option<String>>,
    pub status: Option<String>,
    pub priority: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    pub lead_type: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub lead_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub start_date: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub due_date: Option<Option<String>>,
}

/// 上游 `CreateProjectResourceRequest`。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CreateProjectResourceRequest {
    #[serde(default)]
    pub resource_type: String,
    pub resource_ref: Option<JsonValue>,
    pub label: Option<String>,
    pub position: Option<i32>,
}

/// 上游 `UpdateProjectResourceRequest`：`resource_type` **不可改**。
///
/// 这里刻意用 `Option<JsonValue>` 而不是 `JsonValue`：Go 侧从
/// `map[string]json.RawMessage` 取 key，`rawRef` 的 `ok` 才是「有没有传」。第三态
/// `Some(JsonValue::Null)` 会走 `validateAndNormalizeResourceRef` 并拿到 url/path
/// 缺失的 400 —— 与上游 `json.RawMessage("null")` 同款。
///
/// 注意：`routes/projects/resources.rs` 的 `update_resource` 按上游原样**先解到裸 map**
/// （要区分「键缺失」与「键为 null」，且逐字段的 400 文案与上游逐字一致），所以本结构
/// 只作线上形状的文档与后续切片复用点，当前不在编译期被构造。
#[allow(dead_code)]
#[allow(clippy::option_option)] // `label` / `position` 的上游三态（缺失 / null / 值）
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpdateProjectResourceRequest {
    pub resource_ref: Option<JsonValue>,
    #[serde(default, deserialize_with = "double_option")]
    pub label: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    pub position: Option<Option<i32>>,
}

/// `local_directory` 的两种执行模式（上游 `localDirectoryModeInPlace` /
/// `localDirectoryModeWorktree`）。
pub(crate) const LOCAL_DIRECTORY_MODE_IN_PLACE: &str = "in_place";
pub(crate) const LOCAL_DIRECTORY_MODE_WORKTREE: &str = "worktree";

/// 上游 `githubRepoRef`（`omitempty` ⇒ 空串键不落 JSONB）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct GithubRepoRef {
    #[serde(default)]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_branch_hint: String,
    #[serde(default, rename = "ref", skip_serializing_if = "String::is_empty")]
    pub ref_: String,
}

/// 上游 `localDirectoryRef`（同款 `omitempty`）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(crate) struct LocalDirectoryRef {
    #[serde(default)]
    pub local_path: String,
    #[serde(default)]
    pub daemon_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub execution_mode: String,
}
