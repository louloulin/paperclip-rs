//! `/api/issues/table/*` 的 page cursor 编解码（从 `issue_table.rs` 拆出，R7 单文件 800 行上限；
//! `scripts/file_size_check.py` + 门 ⑩ 执行）。
//!
//! 上游 `issueTableCursor`：本仓用 **hex 编码的 JSON**（上游 `base64.RawURLEncoding`），沿用 M2-C
//! `routes/inbox.rs` 的先例，避免给 mc-http 增依赖；cursor 对客户端不透明（有意偏离 1，
//! 见 `docs/14-M2-TABLE.md` §4）。

use serde::{Deserialize, Serialize};

use mc_core::Id;
use mc_errors::Error;
use mc_repos::issue_table::{TableCursor, TableGroupCursor};

use crate::routes::issues::validation;

use super::spec::parse_rfc3339;
use super::TableError;

/// cursor 版本号（上游 `issueTableCursor.Version`）。
pub(crate) const CURSOR_VERSION: u8 = 1;

// ---------------------------------------------------------------------------
// cursor（上游 `issueTableCursor`；本仓 hex 编码，见模块注释 1）
// ---------------------------------------------------------------------------

/// cursor 的 JSON 形状，字段名与上游一致（`v` / `query` / `group_key` / …）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CursorWire {
    pub(crate) v: u8,
    #[serde(rename = "query")]
    pub(crate) query_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group_order: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group_sort_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) group_cursor_key: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) branch_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sort_value: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub(crate) sort_is_null: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) row_created_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) row_id: String,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 skip_serializing_if 只接受 `&T`
pub(crate) fn is_false(value: &bool) -> bool {
    !*value
}

impl CursorWire {
    pub(crate) fn new(query_fingerprint: &str) -> Self {
        Self {
            v: CURSOR_VERSION,
            query_fingerprint: query_fingerprint.to_string(),
            group_key: None,
            parent_id: None,
            group_order: None,
            group_sort_key: None,
            group_cursor_key: None,
            branch_identity: String::new(),
            sort_value: None,
            sort_is_null: false,
            row_created_at: String::new(),
            row_id: String::new(),
        }
    }

    pub(crate) fn encode(&self) -> String {
        let json = serde_json::to_vec(self).expect("cursor is always serializable");
        hex::encode(json)
    }

    pub(crate) fn decode(raw: &str) -> Result<Self, Error> {
        if raw.len() > 16 * 1024 {
            return Err(validation("invalid cursor"));
        }
        let bytes = hex::decode(raw.trim()).map_err(|_| validation("invalid cursor"))?;
        let wire: Self =
            serde_json::from_slice(&bytes).map_err(|_| validation("invalid cursor"))?;
        if wire.v != CURSOR_VERSION {
            return Err(validation("invalid cursor"));
        }
        Ok(wire)
    }

    /// 上游 `issueTableCursorMatches`：指纹 / `group_key` / `parent_id` 任一不符 → 409。
    pub(crate) fn matches(
        &self,
        fingerprint: &str,
        group_key: Option<&str>,
        parent_id: Option<&str>,
    ) -> Result<(), TableError> {
        if self.query_fingerprint != fingerprint
            || self.group_key.as_deref() != group_key
            || self.parent_id.as_deref() != parent_id
        {
            return Err(TableError::CursorMismatch);
        }
        Ok(())
    }

    /// 解析 `/rows` 的 keyset cursor（缺字段 → 400，镜像上游）。
    pub(crate) fn into_row_cursor(self) -> Result<TableCursor, Error> {
        if self.row_id.is_empty() || self.row_created_at.is_empty() {
            return Err(validation("invalid cursor"));
        }
        Ok(TableCursor {
            sort_value: self.sort_value,
            sort_is_null: self.sort_is_null,
            row_created_at: parse_rfc3339("cursor.row_created_at", &self.row_created_at)?,
            row_id: Id::parse(&self.row_id).map_err(|_| validation("invalid cursor"))?,
        })
    }

    /// 解析 `/groups` 的 keyset cursor（缺字段 → 400，镜像上游）。
    pub(crate) fn into_group_cursor(self) -> Result<TableGroupCursor, Error> {
        let (Some(order), Some(sort_key), Some(value)) =
            (self.group_order, self.group_sort_key, self.group_cursor_key)
        else {
            return Err(validation("invalid cursor"));
        };
        Ok(TableGroupCursor {
            order,
            sort_key,
            value,
        })
    }
}

/// 页面 cursor 解码：`None` = 首页；非空串解码失败 → 400（上游 `normalizeIssueTablePage`）。
pub(crate) fn decode_cursor(raw: Option<&str>) -> Result<Option<CursorWire>, Error> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => CursorWire::decode(value).map(Some),
    }
}
