//! `/api/issues*` 的列表查询参数 → `mc_repos::issue::IssueFilter`（从 `issues.rs` 拆出，
//! R7 单文件 800 行上限）。

use mc_core::status::{IssueStatus, CANONICAL_KEYS};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue::{
    parse_assignee_type, split_comma_param, IssueFilter, IssueOrderBy, LIST_MAX_LIMIT,
};
use mc_repos::issue_status::{category_str, parse_category};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use super::context::{parse_target_id, StatusCatalog, WorkspaceQuery};
use super::helpers::{normalize_assignee_type, validation};

// ---------------------------------------------------------------------------
// 列表查询参数
// ---------------------------------------------------------------------------

/// `/api/issues` 系列共用的过滤参数（`query` 接口用同一套 key，值是字符串）。
#[derive(Debug, Default, Deserialize)]
pub struct ListIssuesQuery {
    pub workspace_id: Option<String>,
    pub workspace_slug: Option<String>,
    /// 全文（title / description / identifier）
    pub q: Option<String>,
    /// 单个 status key
    pub status: Option<String>,
    /// status key CSV
    pub statuses: Option<String>,
    /// `open` / `closed`
    pub status_category: Option<String>,
    /// 分类 CSV
    pub status_categories: Option<String>,
    pub priority: Option<String>,
    pub priorities: Option<String>,
    pub assignee_id: Option<String>,
    pub assignee_ids: Option<String>,
    pub assignee_type: Option<String>,
    pub assignee_types: Option<String>,
    pub creator_id: Option<String>,
    pub parent_issue_id: Option<String>,
    pub project_id: Option<String>,
    pub stage: Option<i32>,
    /// `true` = 不过滤终态
    pub include_closed: Option<bool>,
    /// 只看未关闭（上游 `open_only`）
    pub open_only: Option<bool>,
    /// 只看顶层 issue
    pub only_parentless: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// `updated_at`（默认）/ `created_at` / `position` / `number`
    pub sort: Option<String>,
    /// 接受但忽略（白名单排序已固定方向，见 docs/11 §5）
    pub direction: Option<String>,
    /// `/api/issues/grouped` 专用
    pub group_by: Option<String>,
}

impl ListIssuesQuery {
    /// `POST /api/issues/query`：body 是「与 query string 同 key 的扁平对象」，
    /// 值接受字符串（上游约定）也接受 number / bool。
    pub(crate) fn from_pairs(pairs: &HashMap<String, JsonValue>) -> Result<Self, Error> {
        let mut query = Self::default();
        for (key, value) in pairs {
            let Some(text) = scalar_to_string(value) else {
                return Err(validation(format!("query param {key} must be a scalar")));
            };
            match key.as_str() {
                "q" => query.q = Some(text),
                "status" => query.status = Some(text),
                "statuses" => query.statuses = Some(text),
                "status_category" => query.status_category = Some(text),
                "status_categories" => query.status_categories = Some(text),
                "priority" => query.priority = Some(text),
                "priorities" => query.priorities = Some(text),
                "assignee_id" => query.assignee_id = Some(text),
                "assignee_ids" => query.assignee_ids = Some(text),
                "assignee_type" => query.assignee_type = Some(text),
                "assignee_types" => query.assignee_types = Some(text),
                "creator_id" => query.creator_id = Some(text),
                "parent_issue_id" => query.parent_issue_id = Some(text),
                "project_id" => query.project_id = Some(text),
                "workspace_id" => query.workspace_id = Some(text),
                "workspace_slug" => query.workspace_slug = Some(text),
                "stage" => query.stage = Some(parse_number("stage", &text)?),
                "limit" => query.limit = Some(parse_number("limit", &text)?),
                "offset" => query.offset = Some(parse_number("offset", &text)?),
                "sort" => query.sort = Some(text),
                "direction" => query.direction = Some(text),
                "group_by" => query.group_by = Some(text),
                "include_closed" => {
                    query.include_closed = Some(parse_bool("include_closed", &text)?);
                }
                "open_only" => query.open_only = Some(parse_bool("open_only", &text)?),
                "only_parentless" => {
                    query.only_parentless = Some(parse_bool("only_parentless", &text)?);
                }
                // 上游 `QueryIssues` 直接透传给 `ListIssues`，未知 key 被忽略
                _ => {}
            }
        }
        Ok(query)
    }

    pub(crate) fn workspace_selector(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            workspace_id: self.workspace_id.clone(),
            workspace_slug: self.workspace_slug.clone(),
        }
    }
}

pub(crate) fn scalar_to_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(s) => Some(s.clone()),
        JsonValue::Number(n) => Some(n.to_string()),
        JsonValue::Bool(b) => Some(b.to_string()),
        JsonValue::Null | JsonValue::Array(_) | JsonValue::Object(_) => None,
    }
}

pub(crate) fn parse_number<T: std::str::FromStr>(field: &str, raw: &str) -> Result<T, Error> {
    raw.trim()
        .parse()
        .map_err(|_| validation(format!("{field} has an invalid value: {raw}")))
}

pub(crate) fn parse_bool(field: &str, raw: &str) -> Result<bool, Error> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(validation(format!("{field} must be true or false"))),
    }
}

pub(crate) fn parse_order(sort: Option<&str>) -> Result<IssueOrderBy, Error> {
    let raw = sort.unwrap_or("").trim();
    match raw {
        "" | "updated_at" | "last_activity" | "last_activity_at" => Ok(IssueOrderBy::UpdatedDesc),
        "position" => Ok(IssueOrderBy::PositionAsc),
        "created_at" => Ok(IssueOrderBy::CreatedDesc),
        "number" => Ok(IssueOrderBy::NumberAsc),
        _ => Err(validation(format!("unsupported sort: {raw}"))),
    }
}

/// 过滤参数 → `IssueFilter`（`status_categories` 在这里展开成具体 key）。
pub(crate) fn build_filter(
    workspace_id: Id,
    query: &ListIssuesQuery,
    terminal_statuses: &[String],
) -> Result<IssueFilter, Error> {
    let mut filter = IssueFilter::new(workspace_id);

    let mut statuses = split_comma_param(query.statuses.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.status.as_deref().unwrap_or("")));

    // 分类过滤：展开成具体 key 后与显式 status 取交集（上游是 AND）
    let categories = split_comma_param(query.status_categories.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.status_category.as_deref().unwrap_or("")));
    if let Some(categories) = categories {
        // 需要内置 + 自定义目录；这里只展开内置（自定义 key 由 handler 追加）
        let mut expanded: Vec<String> = Vec::new();
        for raw in categories {
            let category = parse_category(&raw)
                .ok_or_else(|| validation(format!("unsupported status_category: {raw}")))?;
            for key in CANONICAL_KEYS {
                if IssueStatus::from_key(key).map(IssueStatus::category) == Some(category) {
                    expanded.push((*key).to_string());
                }
            }
        }
        statuses = Some(match statuses {
            Some(explicit) => explicit
                .into_iter()
                .filter(|s| expanded.contains(s))
                .collect(),
            None => expanded,
        });
    }
    filter.statuses = statuses;

    filter.priorities = split_comma_param(query.priorities.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.priority.as_deref().unwrap_or("")));

    let assignee_types = split_comma_param(query.assignee_types.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.assignee_type.as_deref().unwrap_or("")));
    if let Some(types) = assignee_types {
        if types.len() > 1 {
            return Err(validation("assignee_types accepts a single value"));
        }
        if let Some(raw) = types.first() {
            let normalized = normalize_assignee_type(raw);
            if parse_assignee_type(normalized).is_none() {
                return Err(validation(format!("invalid assignee_type: {raw}")));
            }
            filter.assignee_type = Some(normalized.to_string());
        }
    }

    filter.assignee_ids = split_comma_param(query.assignee_ids.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.assignee_id.as_deref().unwrap_or("")));
    filter.creator_id = split_comma_param(query.creator_id.as_deref().unwrap_or(""))
        .and_then(|v| v.into_iter().next());

    if let Some(raw) = query.parent_issue_id.as_deref() {
        filter.parent_issue_id = Some(parse_target_id("parent_issue_id", raw)?);
    }
    if let Some(raw) = query.project_id.as_deref() {
        filter.project_id = Some(parse_target_id("project_id", raw)?);
    }
    filter.stage = query.stage;
    filter.q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_string);

    // 终态过滤：`open_only` / `include_closed=false` 都排除终态 key
    let include_closed = match (query.include_closed, query.open_only) {
        (_, Some(true)) => false,
        (Some(value), _) => value,
        (None, _) => true,
    };
    filter.include_closed = include_closed;
    filter.terminal_statuses = terminal_statuses.to_vec();
    filter.only_parentless = query.only_parentless.unwrap_or(false);
    filter.limit = query.limit.map(|l| l.clamp(1, LIST_MAX_LIMIT));
    filter.offset = query.offset.map(|o| o.max(0));
    filter.order = parse_order(query.sort.as_deref())?;
    Ok(filter)
}

/// 把目录里的自定义 closed status 也塞进 `status_categories` 展开结果。
///
/// `IssueRepo::terminal_status_keys` 已覆盖「内置终态 + 自定义 closed」，这里复用同一
/// 语义去补全分类过滤（内置部分由 `build_filter` 负责）。
pub(crate) fn expand_custom_categories(
    filter: &mut IssueFilter,
    query: &ListIssuesQuery,
    catalog: &StatusCatalog,
) -> Result<(), Error> {
    let categories = split_comma_param(query.status_categories.as_deref().unwrap_or(""))
        .or_else(|| split_comma_param(query.status_category.as_deref().unwrap_or("")));
    let Some(categories) = categories else {
        return Ok(());
    };
    let mut custom: Vec<String> = Vec::new();
    for raw in categories {
        let category = parse_category(&raw)
            .ok_or_else(|| validation(format!("unsupported status_category: {raw}")))?;
        custom.extend(
            catalog
                .keys_in_category(category)
                .into_iter()
                .filter(|key| !CANONICAL_KEYS.contains(&key.as_str())),
        );
    }
    if custom.is_empty() {
        return Ok(());
    }
    let mut keys = filter.statuses.clone().unwrap_or_default();
    keys.extend(custom);
    keys.sort();
    keys.dedup();
    filter.statuses = Some(keys);
    let _ = category_str;
    Ok(())
}
