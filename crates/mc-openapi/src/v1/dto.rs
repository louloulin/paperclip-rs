//! `/v1` 公开契约的**线格式 DTO**（上游 `pkg/publicapi/v1/types.go` 逐文件对应）。
//!
//! 这些类型是 `/v1` 与 `/api/plugin-bridge/v1` **两侧共用**的（同一批 handler 两个挂载点）：
//! M6-7 **不要**在 `routes/v1/` 里另抄一份 —— 抄一份就是两侧响应字节开始漂移的那天。
//!
//! 字段的「可选」严格按照上游的指针 / `omitempty`：
//!
//! | Go | 本仓 |
//! |---|---|
//! | `*T`（无 omitempty） | `Option<T>`（序列化成 `null`） |
//! | `*T` + `omitempty` | `Option<T>` + `skip_serializing_if = "Option::is_none"` |
//! | `string` + `omitempty` | `String` + `skip_serializing_if = "String::is_empty"` |
//! | `map[string]any` / `[]T` | `BTreeMap<String, Value>` / `Vec<T>` |
//!
//! 最后一行的**零值形态**与 Go 有差别（Go 的 nil map / nil slice 序列化成 `null`，本仓是
//! `{}` / `[]`）：两边客户端拿到的都是空容器，差别只在字节层面（见 `v1.rs` 模块头偏差 2）。
//! 反序列化侧全部 `#[serde(default)]`，因为 Go 的缺失 `map`/`slice` 就是 nil。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// 插件专用的启动上下文（上游 `Context`）。
///
/// 放在**版本化契约包**里，好让 service 层的字段不会意外漏进线格式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub workspace: ContextWorkspace,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<ContextUser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<ContextIssue>,
    #[serde(default)]
    pub config: BTreeMap<String, Value>,
    #[serde(default)]
    pub granted_net_domains: Vec<String>,
    pub actor: String,
}

/// 上下文里的工作区摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWorkspace {
    pub id: String,
    pub name: String,
    pub slug: String,
}

/// 上下文里的用户摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextUser {
    pub id: String,
    pub name: String,
}

/// 上下文里的 issue 摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextIssue {
    pub id: String,
    pub identifier: String,
    pub title: String,
}

/// issue 的公开形状（上游 `Issue`）。
///
/// 它**故意**独立于既有 `handler.IssueResponse`（上游注释）：给 App API 加一个字段，
/// 因此不会再顺带拓宽公开契约。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub workspace_id: String,
    pub number: i32,
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_category: String,
    pub priority: String,
    pub assignee_type: Option<String>,
    pub assignee_id: Option<String>,
    pub creator_type: String,
    pub creator_id: String,
    pub parent_issue_id: Option<String>,
    pub project_id: Option<String>,
    pub position: f64,
    pub stage: Option<i32>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: i64,
    pub last_activity_at: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, Value>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
}

/// `PATCH /issues/{issue_ref}` 的请求体（上游 `PatchIssueRequest`）。
///
/// `expected_revision` 是**可选**的乐观并发：上游把「别人先改了」表达成 `409 conflict`
/// （客户端应当重读再重试，而不是覆盖）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchIssueRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 一条评论（上游 `Comment`）。
///
/// `deleted_at` **只在墓碑**上出现：删除时仍有回复的评论会被保留（内容清空），好让那些
/// 回复保住自己的父节点（上游注释逐字）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author_type: String,
    pub author_id: String,
    pub content: String,
    #[serde(rename = "type")]
    pub comment_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_id: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub deleted_at: String,
}

/// `GET /issues/{issue_ref}/comments` 的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentListResponse {
    pub comments: Vec<Comment>,
}

/// `POST /issues/{issue_ref}/comments` 的请求体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCommentRequest {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

/// 插件存储的一个键（上游 `StorageKey`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageKey {
    pub key: String,
    pub size_bytes: i64,
    pub updated_at: String,
}

/// `GET /storage/{scope}` 的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageKeyListResponse {
    pub keys: Vec<StorageKey>,
}

/// `GET /storage/{scope}/{key}` 的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageValueResponse {
    pub value: String,
}

/// `PUT /storage/{scope}/{key}` 的请求体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutStorageValueRequest {
    pub value: String,
}
