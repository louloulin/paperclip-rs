//! 请求 DTO（字段名与上游一致；未知字段 → 400，镜像上游 `DisallowUnknownFields`）与
//! 请求体解码。模块文档见父模块（`mod.rs`）。

use axum::body::Bytes;
use serde::Deserialize;
use serde_json::Value as JsonValue;

use mc_errors::Error;

use crate::routes::issues::validation;

use super::super::TableError;

/// 422：分组 / facet 维度不支持的返回（保留上游字段名，见 `super` 的模块注释 2）。
pub(crate) fn unsupported_group_err(code: &str, message: &str) -> TableError {
    TableError::Unsupported {
        code: code.to_string(),
        message: message.to_string(),
    }
}

pub(crate) fn unsupported_filter_err(code: &str, message: &str) -> TableError {
    TableError::UnsupportedFilter {
        code: code.to_string(),
        message: message.to_string(),
    }
}

/// 请求体上限（上游 `http.MaxBytesReader(w, r.Body, 1<<20)`）。
pub(crate) const MAX_BODY_BYTES: usize = 1 << 20;

/// `filters.statuses` 单个 key 的长度上限（上游 `len(status) > 64`）。
pub(crate) const STATUS_KEY_MAX_LEN: usize = 64;

// ---------------------------------------------------------------------------
// 请求 DTO（字段名与上游一致；未知字段 → 400，镜像上游 DisallowUnknownFields）
// ---------------------------------------------------------------------------

/// `{"type": "...", "id": "..."}`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActorDto {
    #[serde(rename = "type", default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) id: String,
}

/// `query.scope`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScopeDto {
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) assignee_types: Vec<String>,
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[serde(default)]
    pub(crate) actor: Option<ActorDto>,
    #[serde(default)]
    pub(crate) relation: String,
}

/// `query.filters.date`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DateFilterDto {
    #[serde(default)]
    pub(crate) field: String,
    #[serde(default)]
    pub(crate) start: String,
    #[serde(default)]
    pub(crate) end: String,
}

/// `query.filters`。上面是已实现字段，下面是**已声明但本仓不支持**的字段：
/// 空值等价于“未提供”，非空一律 422（见模块注释 3）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FiltersDto {
    #[serde(default)]
    pub(crate) statuses: Vec<String>,
    #[serde(default)]
    pub(crate) priorities: Vec<String>,
    #[serde(default)]
    pub(crate) assignees: Option<Vec<ActorDto>>,
    #[serde(default)]
    pub(crate) include_no_assignee: bool,
    #[serde(default)]
    pub(crate) creators: Vec<ActorDto>,
    #[serde(default)]
    pub(crate) project_ids: Vec<String>,
    #[serde(default)]
    pub(crate) include_no_project: bool,
    #[serde(default)]
    pub(crate) date: Option<DateFilterDto>,
    #[serde(default)]
    pub(crate) include_sub_issues: Option<bool>,
    // ---- 已声明但不支持（422）----
    #[serde(default)]
    pub(crate) project_statuses: Vec<String>,
    #[serde(default)]
    pub(crate) label_ids: Vec<String>,
    #[serde(default)]
    pub(crate) properties: Option<JsonValue>,
    #[serde(default)]
    pub(crate) working_only: bool,
    #[serde(default)]
    pub(crate) working_issue_ids: Option<Vec<String>>,
}

/// `query.sort`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SortDto {
    #[serde(default)]
    pub(crate) field: String,
    #[serde(default)]
    pub(crate) direction: String,
}

/// `query`（`scope` / `filters` / `sort` 缺失时按空结构处理，与上游零值一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TableQueryDto {
    #[serde(default)]
    pub(crate) scope: ScopeDto,
    #[serde(default)]
    pub(crate) filters: FiltersDto,
    #[serde(default)]
    pub(crate) search: Option<String>,
    #[serde(default)]
    pub(crate) sort: SortDto,
}

/// `group`（`primary` / `secondary` 是**字符串**，与上游一致）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupDto {
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) property_id: Option<String>,
    /// 上游字段：只在 `status_category` 分组下消费，本仓该维度不支持（接受但忽略）。
    #[serde(default)]
    #[allow(dead_code)] // 上游已安装客户端会带上它，拒收会误伤整个请求
    pub(crate) category_format: Option<String>,
    #[serde(default)]
    pub(crate) include_empty: bool,
    #[serde(default)]
    pub(crate) primary: Option<String>,
    #[serde(default)]
    pub(crate) secondary: Option<String>,
    #[serde(default)]
    pub(crate) secondary_values: Option<Vec<String>>,
}

/// `page`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageDto {
    #[serde(default)]
    pub(crate) limit: i64,
    #[serde(default)]
    pub(crate) cursor: Option<String>,
}

/// `hierarchy`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HierarchyDto {
    #[serde(default)]
    pub(crate) enabled: bool,
}

/// `POST /api/issues/table/groups` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupsRequest {
    #[serde(default)]
    pub(crate) query: TableQueryDto,
    #[serde(default)]
    pub(crate) group: GroupDto,
    #[serde(default)]
    pub(crate) page: PageDto,
}

/// `POST /api/issues/table/rows` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RowsRequest {
    #[serde(default)]
    pub(crate) query: TableQueryDto,
    #[serde(default)]
    pub(crate) group: GroupDto,
    #[serde(default)]
    pub(crate) group_key: Option<String>,
    #[serde(default)]
    pub(crate) hierarchy: HierarchyDto,
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
    #[serde(default)]
    pub(crate) page: PageDto,
}

/// `facets[]`。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FacetSpecDto {
    #[serde(default)]
    pub(crate) kind: String,
    #[serde(default)]
    pub(crate) property_id: Option<String>,
}

/// `POST /api/issues/table/facets` 请求体。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FacetsRequest {
    #[serde(default)]
    pub(crate) query: TableQueryDto,
    #[serde(default)]
    pub(crate) facets: Vec<FacetSpecDto>,
    #[serde(default)]
    pub(crate) include_total: Option<bool>,
}

/// 解码请求体：上限 1 MiB、未知字段 400（上游 `decodeIssueTableJSON`）。
pub(crate) fn decode_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    if body.len() > MAX_BODY_BYTES {
        return Err(validation(
            "invalid issue table query: request body too large",
        ));
    }
    serde_json::from_slice(body).map_err(|e| validation(format!("invalid issue table query: {e}")))
}
