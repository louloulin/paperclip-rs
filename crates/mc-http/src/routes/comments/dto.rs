//! `/api/comments*` 的响应 / 请求 DTO（LUM-1458 从 `comments/mod.rs` 拆出）。
//!
//! 拆出来的原因不是「组织更美」而是 **gate ⑩**：`comments/mod.rs` 当时正好顶到基线
//! 831 行（`scripts/file_size_baseline.tsv`，800 硬上限 + 31 行宽限），而 ⑩ 的规则是
//! 「基线内的文件只允许变短」⇒ 补 `PUT|DELETE /api/comments/:commentId/` 这两个键之前
//! 必须先拆（`docs/37` §15.6）。这里只搬 DTO 与时间戳序列化，**handler / 路由一行不动**。
//!
//! 注意（**给下一个想省行数的人**）：`docs/37` §15.6 实测过「两个 `.route()` 共用一个
//! `MethodRouter` 变量」这条捷径 —— `cargo fmt` 与 ⑩ 都绿，但 ⑦ 的抽取器要求
//! `.route()` 第二个参数里出现 `get/post/put/...` 调用，共用变量会让这个键从路由清单里
//! **静默消失**。省行数要靠拆文件，不要靠共用 handler 变量。

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use mc_repos::comment::CommentRow;

/// 时间戳序列化：UTC 一律用 `Z` 后缀（上游 `timestampToString` 的 Go `RFC3339Nano`
/// 对 UTC 也输出 `Z`）。
///
/// 这不只是美观：`X-Multica-Next-Before` 里的游标会被客户端直接拼回
/// `?before=` 查询串，而 `+00:00` 的 `+` 在 query 里解码成空格 → 翻页 400。
/// 微秒精度与 PG `TIMESTAMPTZ` 一致，无损。
///
/// `pub(super)`：列表 handler 用它算下一页游标响应头（`mod.rs` 里的 `list_comments`）。
pub(super) fn ts(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

/// 评论 reaction 响应（对齐上游 `ReactionResponse`）。
#[derive(Debug, Clone, Serialize)]
pub struct ReactionDto {
    pub id: String,
    pub comment_id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub emoji: String,
    pub created_at: String,
}

impl From<&mc_repos::comment::CommentReactionRow> for ReactionDto {
    fn from(row: &mc_repos::comment::CommentReactionRow) -> Self {
        Self {
            id: row.id().to_string(),
            comment_id: row.comment_id().to_string(),
            actor_type: row.actor_type.clone(),
            actor_id: row.actor_id.clone(),
            emoji: row.emoji.clone(),
            created_at: ts(&row.created_at),
        }
    }
}

/// 评论响应（对齐上游 `CommentResponse` 的子集）。
///
/// 省略的字段都是本仓库 0001 schema 里**没有列**的上游扩展：
/// `resolved_by_type` / `resolved_by_id` / `quick_action_id` / `source_task_id` 之外的
/// 触发 / fold / summary 投影。`attachments` 恒为空数组（M3 才有附件表），
/// 保留该字段是为了让上游客户端的 `attachments.length` 读取不炸。
#[derive(Debug, Clone, Serialize)]
pub struct CommentDto {
    pub id: String,
    pub issue_id: String,
    pub parent_id: Option<String>,
    pub author_type: String,
    pub author_id: String,
    pub content: String,
    /// 上游 `type`（`comment` / `progress_update`）。本仓库无 `comment.type` 列，
    /// 只可能写出默认值 `comment`（`progress_update` 在写入前就被 400 拒绝）。
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    pub revision: i64,
    pub resolved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub reactions: Vec<ReactionDto>,
    pub attachments: Vec<serde_json::Value>,
}

impl CommentDto {
    /// `pub(super)`：`mod.rs` 的 handler 构造响应（列表带 reactions，单条用 `bare`）。
    pub(super) fn new(row: &CommentRow, reactions: Vec<ReactionDto>) -> Self {
        Self {
            id: row.id().to_string(),
            issue_id: row.issue_id().to_string(),
            parent_id: row.parent_id().map(|p| p.to_string()),
            // 直出 DB 原始字符串（0001 的 CHECK 限定 user/agent/system/plugin/squad/autopilot），
            // 不用 `CommentRow::author_type()` —— 那个会把未知值回落成 user。
            author_type: row.author_type.clone(),
            author_id: row.author_id.clone(),
            content: row.body.clone(),
            kind: "comment".into(),
            source_task_id: row.source_task_id().map(|t| t.to_string()),
            revision: row.revision,
            resolved_at: row.resolved_at.as_ref().map(ts),
            deleted_at: row.deleted_at.as_ref().map(ts),
            created_at: ts(&row.created_at),
            updated_at: ts(&row.updated_at),
            reactions,
            attachments: Vec::new(),
        }
    }

    pub(super) fn bare(row: &CommentRow) -> Self {
        Self::new(row, Vec::new())
    }
}

/// `POST /api/issues/{id}/comments` 请求体（对齐上游 `CreateCommentRequest`）。
#[derive(Debug, Deserialize)]
pub struct CreateCommentRequest {
    #[serde(default)]
    pub content: String,
    /// 上游字段名是 `type`（Rust 关键字，故 rename）。
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

/// `PUT /api/comments/{commentId}` 请求体（对齐上游子集）。
#[derive(Debug, Deserialize)]
pub struct UpdateCommentRequest {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub expected_revision: Option<i64>,
}

/// reaction 请求体（上游 inline struct：`{"emoji": "..."}`）。
#[derive(Debug, Deserialize)]
pub struct ReactionRequest {
    #[serde(default)]
    pub emoji: String,
}

/// `POST /api/comments/{commentId}/sub-issues` 的 501 响应体。
#[derive(Debug, Serialize)]
pub struct NotImplementedBody {
    pub code: &'static str,
    pub message: &'static str,
    pub todo: &'static str,
}
