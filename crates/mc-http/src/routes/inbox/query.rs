//! inbox 的解析 / 编码 helper：纯函数，不碰 DB。
//!
//! 从 `routes/inbox.rs` 拆出（门 ⑩ 第 8 批）。**0 路由、0 行为变更**：
//! 父模块用 `pub(crate) use` / `pub use` 把符号原样重导出，外部路径逐字不变。

use std::collections::HashMap;

use chrono::{DateTime, SecondsFormat, Utc};
use sha2::{Digest, Sha256};

use mc_core::Id;
use mc_errors::Error;
use mc_repos::inbox::{ArchivedCursor, ArchivedInboxFilter, InboxItemRow};

// ---------------------------------------------------------------------------
// 解析 / 编码 helper
// ---------------------------------------------------------------------------

use crate::routes::inbox::dto::ArchivedCursorWire;
use crate::routes::inbox::{
    ARCHIVED_PAGE_DEFAULT_LIMIT, ARCHIVED_PAGE_MAX_LIMIT, CURSOR_MAX_LEN, FILTER_MAX_RAW_LEN,
    FILTER_MAX_VALUES, LIST_BODY_PREVIEW_LIMIT, LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT,
};
pub(super) fn bad_request(message: &str) -> Error {
    Error::Validation {
        message: message.to_string(),
        details: vec![],
    }
}

/// 上游 `q.Get(name) != ""` 的等价语义：空值等于没传。
pub(super) fn query_value<'a>(query: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    query
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

pub(super) fn parse_list_window(query: &HashMap<String, String>) -> Result<(i64, i64), Error> {
    let limit = match query_value(query, "limit") {
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|v| (1..=LIST_MAX_LIMIT).contains(v))
            .ok_or_else(|| bad_request("limit must be between 1 and 500"))?,
        None => LIST_DEFAULT_LIMIT,
    };
    let offset = match query_value(query, "offset") {
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|v| *v >= 0)
            .ok_or_else(|| bad_request("offset must be >= 0"))?,
        None => 0,
    };
    Ok((limit, offset))
}

pub(super) fn parse_archived_limit(query: &HashMap<String, String>) -> Result<i64, Error> {
    match query_value(query, "limit") {
        Some(raw) => raw
            .parse::<i64>()
            .ok()
            .filter(|v| (1..=ARCHIVED_PAGE_MAX_LIMIT).contains(v))
            .ok_or_else(|| bad_request("limit must be between 1 and 100")),
        None => Ok(ARCHIVED_PAGE_DEFAULT_LIMIT),
    }
}

/// 解析一个逗号分隔的过滤器（排序 + 去重，与上游 `slices.Sort`+`Compact` 一致）。
pub(super) fn parse_filter_values(
    query: &HashMap<String, String>,
    name: &str,
) -> Result<Vec<String>, Error> {
    let Some(raw) = query_value(query, name) else {
        return Ok(Vec::new());
    };
    if raw.len() > FILTER_MAX_RAW_LEN {
        return Err(bad_request("filter is too long"));
    }
    let mut values: Vec<String> = raw.split(',').map(str::to_string).collect();
    if values.len() > FILTER_MAX_VALUES {
        return Err(bad_request("too many filter values"));
    }
    if values.iter().any(String::is_empty) {
        return Err(bad_request("empty filter value"));
    }
    values.sort();
    values.dedup();
    Ok(values)
}

/// 解析 `statuses` / `priorities` / `actors` / `unread_only`（不含 `group_id`）。
pub(super) fn parse_filter(query: &HashMap<String, String>) -> Result<ArchivedInboxFilter, Error> {
    let unread_only = match query_value(query, "unread_only") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(bad_request("invalid unread_only")),
    };
    Ok(ArchivedInboxFilter {
        statuses: parse_filter_values(query, "statuses")?,
        priorities: parse_filter_values(query, "priorities")?,
        actors: parse_filter_values(query, "actors")?,
        unread_only,
        group_id: None,
    })
}

pub(super) fn parse_group_id(query: &HashMap<String, String>) -> Result<Option<Id>, Error> {
    match query_value(query, "group_id") {
        Some(raw) => Id::parse(raw)
            .map(Some)
            .map_err(|_| bad_request("invalid group_id")),
        None => Ok(None),
    }
}

/// 游标作用域：绑定「用户 + workspace + 过滤条件 + 分组」，防止跨查询续页。
pub(super) fn archive_scope_tag(
    workspace_id: Id,
    user_id: Id,
    filter: &ArchivedInboxFilter,
) -> String {
    let canonical = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        workspace_id.as_string(),
        user_id.as_string(),
        filter.statuses.join(","),
        filter.priorities.join(","),
        filter.actors.join(","),
        filter.unread_only,
        filter.group_id.map_or_else(String::new, Id::as_string),
    );
    hex::encode(Sha256::digest(canonical.as_bytes()))
}

pub(super) fn parse_cursor(
    query: &HashMap<String, String>,
    scope_tag: &str,
) -> Result<Option<ArchivedCursor>, Error> {
    let Some(raw) = query_value(query, "cursor") else {
        return Ok(None);
    };
    if raw.len() > CURSOR_MAX_LEN {
        return Err(bad_request("invalid archive cursor"));
    }
    let bytes = hex::decode(raw).map_err(|_| bad_request("invalid archive cursor"))?;
    let wire: ArchivedCursorWire =
        serde_json::from_slice(&bytes).map_err(|_| bad_request("invalid archive cursor"))?;
    if wire.scope != scope_tag {
        return Err(bad_request("invalid archive cursor"));
    }
    let created_at: DateTime<Utc> = DateTime::parse_from_rfc3339(&wire.time)
        .map_err(|_| bad_request("invalid archive cursor time"))?
        .with_timezone(&Utc);
    let id = Id::parse(&wire.id).map_err(|_| bad_request("invalid cursor id"))?;
    Ok(Some(ArchivedCursor { created_at, id }))
}

pub(super) fn encode_cursor(scope_tag: &str, row: &InboxItemRow) -> Result<String, Error> {
    let wire = ArchivedCursorWire {
        time: row.created_at.to_rfc3339_opts(SecondsFormat::Nanos, true),
        id: row.id().as_string(),
        scope: scope_tag.to_string(),
    };
    let bytes = serde_json::to_vec(&wire).map_err(|e| Error::Internal(e.to_string()))?;
    Ok(hex::encode(bytes))
}

/// 列表响应里的 body 预览：只有「有 issue 的 `new_comment`」才截断，
/// 且截断按**字符**（不是字节）计数，避免切坏多字节字符。
///
/// 上限**含**省略号：超过 200 字符时保留前 199 个字符 + `…`（与上游
/// `inboxListBody` 逐字对应）。
pub(super) fn list_body_preview(
    category: &str,
    has_issue: bool,
    body: Option<&str>,
) -> Option<String> {
    let full = body?;
    if category != "new_comment" || !has_issue {
        return Some(full.to_string());
    }
    let mut cut = 0usize;
    let mut seen = 0usize;
    for (offset, _) in full.char_indices() {
        if seen == LIST_BODY_PREVIEW_LIMIT - 1 {
            cut = offset;
        }
        seen += 1;
        if seen > LIST_BODY_PREVIEW_LIMIT {
            return Some(format!("{}…", &full[..cut]));
        }
    }
    Some(full.to_string())
}

pub(super) fn count_as_i64(count: u64) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

pub(super) fn repo_err(e: mc_repos::RepoError, resource: &str) -> Error {
    match e {
        mc_repos::RepoError::NotFound => Error::NotFound {
            resource: resource.to_string(),
        },
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: format!("{resource} state conflict"),
        },
        mc_repos::RepoError::Db(msg) => Error::Database(msg),
    }
}
