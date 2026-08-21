//! Company artifacts service — port of `paperclip/server/src/services/company-artifacts.ts`
//! (773 lines, R770).
//!
//! 起源：paperclip/server/src/services/company-artifacts.ts（R770，773 行）。
//!
//! This module ports the pure helper layer of the Node `companyArtifactsService`:
//! - Cursor encode / decode (base64url-encoded JSON)
//! - LIKE-pattern escaping
//! - Markdown → plain-text preview normalization
//! - Content-Type → media-kind classification
//! - URL builders (issue href, artifacts group href, attachment content path)
//! - Sort / page / group helpers for artifact lists
//! - Issue parent-walk resolution
//!
//! The DB-touching `list` method is exposed as a thin async shell that accepts a
//! `CompanyArtifactsQuery` and returns a `CompanyArtifactsResponse`, delegating
//! actual fetching to a `CompanyArtifactsLister` trait so concrete impls (or
//! mocks) plug in without re-implementing the pure helpers.

#![forbid(unsafe_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;
use uuid::Uuid;

// ---- 公共常量（与 Node 1:1 对齐，COMPANY_ARTIFACTS_MAX_LIMIT 来自 @paperclipai/shared） ----

/// Node: `TEXT_PREVIEW_BYTES = 4096`
pub const TEXT_PREVIEW_BYTES: usize = 4096;
/// Node: `PREVIEW_TEXT_MAX_LENGTH = 280`
pub const PREVIEW_TEXT_MAX_LENGTH: usize = 280;
/// Node: `GROUP_PREVIEW_ARTIFACT_LIMIT = 3`
pub const GROUP_PREVIEW_ARTIFACT_LIMIT: usize = 3;
/// Node: `COMPANY_ARTIFACTS_MAX_LIMIT` from `@paperclipai/shared`.
/// 上游默认 200；这里保留常量以便下游对齐。
pub const COMPANY_ARTIFACTS_MAX_LIMIT: usize = 200;
/// Node: `GROUPED_ARTIFACT_FETCH_LIMIT = COMPANY_ARTIFACTS_MAX_LIMIT * 10`
pub const GROUPED_ARTIFACT_FETCH_LIMIT: usize = COMPANY_ARTIFACTS_MAX_LIMIT * 10;

// ---- Error ----

/// 业务错误。
#[derive(Debug, Error)]
pub enum CompanyArtifactsError {
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("repository error: {0}")]
    Repo(String),
}

pub type CompanyArtifactsResult<T> = Result<T, CompanyArtifactsError>;

// ---- 公共类型（与 Node schema 1:1） ----

/// Artifact media kind（与 Node `CompanyArtifactMediaKind` 1:1）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CompanyArtifactMediaKind {
    Image,
    Video,
    Text,
    File,
    Document,
    Empty,
}

/// Group-by 轴（与 Node `CompanyArtifactGroupBy` 1:1）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CompanyArtifactGroupBy {
    #[default]
    None,
    Issue,
    Task,
}

/// Artifact kind 过滤（与 Node `kind` query field 1:1）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CompanyArtifactKind {
    All,
    Document,
    Image,
    Video,
    Text,
    File,
}

impl Default for CompanyArtifactKind {
    fn default() -> Self {
        Self::All
    }
}

/// Issue 摘要（在 artifact 中嵌入）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactIssueRef {
    pub id: Uuid,
    pub identifier: String,
    pub title: String,
}

/// Project 摘要（在 artifact 中嵌入，可选）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactProjectRef {
    pub id: Uuid,
    pub name: String,
}

/// Agent 摘要（在 artifact 中嵌入，可选）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactAgentRef {
    pub id: Uuid,
    pub name: String,
}

/// 单个 artifact（与 Node `CompanyArtifact` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyArtifact {
    pub id: String,
    /// 来源：`"document" | "work_product" | "attachment"`。
    pub source: String,
    pub media_kind: CompanyArtifactMediaKind,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_path: Option<String>,
    pub issue: ArtifactIssueRef,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ArtifactProjectRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by_agent: Option<ArtifactAgentRef>,
    pub updated_at: DateTime<Utc>,
    pub href: String,
}

/// 单个 artifact group（与 Node `CompanyArtifactGroup` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyArtifactGroup {
    pub id: String,
    pub group_by: CompanyArtifactGroupBy,
    pub issue: ArtifactIssueRef,
    pub title: String,
    pub count: usize,
    pub media_kinds: Vec<CompanyArtifactMediaKind>,
    pub preview_artifacts: Vec<CompanyArtifact>,
    pub updated_at: DateTime<Utc>,
    pub href: String,
}

/// Issue 分组行（与 Node `IssueGroupingRow` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueGroupingRow {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub identifier: Option<String>,
    pub title: String,
    pub updated_at: DateTime<Utc>,
}

/// 查询参数（与 Node `CompanyArtifactsQuery` 1:1）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyArtifactsQuery {
    #[serde(default)]
    pub kind: CompanyArtifactKind,
    #[serde(default)]
    pub group_by: CompanyArtifactGroupBy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_issue_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(default)]
    pub starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// 默认 50，上限为 `COMPANY_ARTIFACTS_MAX_LIMIT`。
    #[serde(default = "default_artifact_limit")]
    pub limit: usize,
}

fn default_artifact_limit() -> usize {
    50
}

/// 列表响应（与 Node `CompanyArtifactsResponse` 1:1）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanyArtifactsResponse {
    #[serde(default)]
    pub artifacts: Vec<CompanyArtifact>,
    #[serde(default)]
    pub groups: Vec<CompanyArtifactGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_group: Option<CompanyArtifactGroup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

// ---- Cursor ----

/// 分页 cursor（与 Node `ArtifactCursor` 1:1）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCursor {
    pub updated_at: DateTime<Utc>,
    pub id: String,
}

/// 将 cursor 编码为 base64url(JSON)（Node: `encodeCursor`）。
pub fn encode_cursor(cursor: &ArtifactCursor) -> String {
    let json = serde_json::to_string(cursor).expect("cursor serialization");
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

/// 解码 base64url(JSON) cursor（Node: `decodeCursor`）。
///
/// `None` / 空 → `Ok(None)`；非法 → `Err(InvalidCursor)`。
pub fn decode_cursor(cursor: Option<&str>) -> CompanyArtifactsResult<Option<ArtifactCursor>> {
    let raw = match cursor {
        None => return Ok(None),
        Some(s) if s.is_empty() => return Ok(None),
        Some(s) => s,
    };
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.as_bytes())
        .map_err(|e| CompanyArtifactsError::InvalidCursor(format!("base64: {e}")))?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| CompanyArtifactsError::InvalidCursor(format!("json: {e}")))?;
    let id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CompanyArtifactsError::InvalidCursor("missing id".into()))?
        .to_string();
    let updated_at_str = parsed
        .get("updatedAt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| CompanyArtifactsError::InvalidCursor("missing updatedAt".into()))?;
    let updated_at = DateTime::parse_from_rfc3339(updated_at_str)
        .map_err(|e| CompanyArtifactsError::InvalidCursor(format!("date: {e}")))?
        .with_timezone(&Utc);
    Ok(Some(ArtifactCursor { updated_at, id }))
}

/// 判断 item 是否在 cursor 之后（用于分页过滤）。
///
/// 行为：cursor 为 None → 全部 true；否则按 `(updated_at desc, id desc)` 比较。
pub fn is_after_cursor(item_updated_at: DateTime<Utc>, item_id: &str, cursor: Option<&ArtifactCursor>) -> bool {
    let cursor = match cursor {
        None => return true,
        Some(c) => c,
    };
    let date_diff = item_updated_at.cmp(&cursor.updated_at);
    if date_diff.is_lt() {
        return true;
    }
    if date_diff.is_gt() {
        return false;
    }
    // 同时间按 id 字典序逆序（与 Node `id.localeCompare` 方向一致）
    item_id < cursor.id.as_str()
}

// ---- SQL LIKE pattern ----

/// 转义 LIKE pattern 中的特殊字符 `\` / `%` / `_`（Node: `escapeLikePattern`）。
pub fn escape_like_pattern(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '%' | '_' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
    out
}

// ---- Preview text ----

/// 把 markdown 文本压成纯文本预览（≤ `PREVIEW_TEXT_MAX_LENGTH` 字符）。
///
/// 规则（与 Node `normalizePreviewText` 1:1）：
/// - 移除 fenced code block
/// - 移除 inline code 反引号
/// - 移除图片语法
/// - 把 link 保留文字
/// - 把 markdown 强调符号替换为空格
/// - 合并空白
/// - 超长截断 + `...`
pub fn normalize_preview_text(input: Option<&str>) -> Option<String> {
    let raw = input?;
    // 1. fenced code block
    let re_fence = Regex::new(r"```[\s\S]*?```").expect("valid regex");
    let s = re_fence.replace_all(raw, " ");
    // 2. inline code
    let re_inline = Regex::new(r"`([^`]+)`").expect("valid regex");
    let s = re_inline.replace_all(&s, "$1");
    // 3. image syntax
    let re_image = Regex::new(r"!\[[^\]]*\]\([^)]*\)").expect("valid regex");
    let s = re_image.replace_all(&s, " ");
    // 4. link syntax -> keep text
    let re_link = Regex::new(r"\[([^\]]+)\]\([^)]*\)").expect("valid regex");
    let s = re_link.replace_all(&s, "$1");
    // 5. markdown emphasis/punctuation -> space
    let re_punct = Regex::new(r"[#>*_\-~|]+").expect("valid regex");
    let s = re_punct.replace_all(&s, " ");
    // 6. collapse whitespace
    let re_ws = Regex::new(r"\s+").expect("valid regex");
    let s = re_ws.replace_all(&s, " ");
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    let char_count = trimmed.chars().count();
    if char_count > PREVIEW_TEXT_MAX_LENGTH {
        // 保留前 (MAX-3) 个字符，加上 "..."
        let take = PREVIEW_TEXT_MAX_LENGTH - 3;
        let prefix: String = trimmed.chars().take(take).collect();
        Some(format!("{}...", prefix.trim_end()))
    } else {
        Some(trimmed.to_string())
    }
}

// ---- Media kind classification ----

/// 把 content-type 分类为 `CompanyArtifactMediaKind`（Node: `classifyMediaKind`）。
///
/// 规则：
/// - 空 → 返回 fallback（默认 `"file"`）
/// - `image/*` → `Image`
/// - `video/*` → `Video`
/// - `text/*` / `application/json` / `+json` / `application/xml` / `+xml` / `application/markdown` → `Text`
/// - 其它 → `File`
pub fn classify_media_kind(content_type: Option<&str>, fallback: CompanyArtifactMediaKind) -> CompanyArtifactMediaKind {
    let normalized = content_type.map(|s| s.to_ascii_lowercase()).unwrap_or_default();
    if normalized.is_empty() {
        return fallback;
    }
    if normalized.starts_with("image/") {
        return CompanyArtifactMediaKind::Image;
    }
    if normalized.starts_with("video/") {
        return CompanyArtifactMediaKind::Video;
    }
    if normalized.starts_with("text/")
        || normalized == "application/json"
        || normalized.ends_with("+json")
        || normalized == "application/xml"
        || normalized.ends_with("+xml")
        || normalized == "application/markdown"
    {
        return CompanyArtifactMediaKind::Text;
    }
    CompanyArtifactMediaKind::File
}

// ---- URL builders ----

/// 构造 issue href（Node: `buildIssueHref`）。
pub fn build_issue_href(company_prefix: &str, identifier: &str, anchor: &str) -> String {
    format!(
        "/{}/issues/{}#{}",
        url_encode(company_prefix),
        url_encode(identifier),
        anchor
    )
}

/// 构造 artifacts group href（Node: `buildArtifactsGroupHref`）。
///
/// groupBy & groupIssueId 必填；kind / projectId / q 仅在非空时附加为 query。
pub fn build_artifacts_group_href(
    company_prefix: &str,
    group_by: CompanyArtifactGroupBy,
    group_issue_id: &str,
    kind: CompanyArtifactKind,
    project_id: Option<&str>,
    q: Option<&str>,
) -> String {
    let mut params: Vec<(String, String)> = Vec::new();
    let group_by_str = match group_by {
        CompanyArtifactGroupBy::None => "none",
        CompanyArtifactGroupBy::Issue => "issue",
        CompanyArtifactGroupBy::Task => "task",
    };
    params.push(("groupBy".to_string(), group_by_str.to_string()));
    params.push(("groupIssueId".to_string(), group_issue_id.to_string()));
    if kind != CompanyArtifactKind::All {
        let kind_str = match kind {
            CompanyArtifactKind::Document => "document",
            CompanyArtifactKind::Image => "image",
            CompanyArtifactKind::Video => "video",
            CompanyArtifactKind::Text => "text",
            CompanyArtifactKind::File => "file",
            CompanyArtifactKind::All => "all",
        };
        params.push(("kind".to_string(), kind_str.to_string()));
    }
    if let Some(pid) = project_id {
        params.push(("projectId".to_string(), pid.to_string()));
    }
    if let Some(qs) = q {
        params.push(("q".to_string(), qs.to_string()));
    }
    let qs: String = params
        .iter()
        .map(|(k, v)| format!("{}={}", url_encode(k), url_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("/{}/artifacts?{}", url_encode(company_prefix), qs)
}

/// Attachment content 路径（Node: `attachmentContentPath`）。
pub fn attachment_content_path(attachment_id: &str) -> String {
    format!("/api/attachments/{}/content", attachment_id)
}

/// 最小 URL encoder（处理 `:` / `/` / `?` / `#` / `&` / `=` / 空白 等）。
/// Node 用 `encodeURIComponent`，这里用同样的语义做最小实现。
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '-' | '_' | '.' | '~'
            )
        {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            let encoded = ch.encode_utf8(&mut buf);
            for b in encoded.as_bytes() {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

// ---- Sort / page ----

/// 按 `(updated_at desc, id desc)` 排序（Node: `sortArtifacts`）。
///
/// `sort_dates` 提供 artifact 的 "实际" 排序日期（如 starred 用 `starred_at`）；
/// 缺省时回落到 `updated_at`。
pub fn sort_artifacts(
    mut artifacts: Vec<CompanyArtifact>,
    sort_dates: &HashMap<String, DateTime<Utc>>,
) -> Vec<CompanyArtifact> {
    artifacts.sort_by(|a, b| {
        let da = sort_dates.get(&a.id).copied().unwrap_or(a.updated_at);
        let db = sort_dates.get(&b.id).copied().unwrap_or(b.updated_at);
        let date_cmp = db.cmp(&da); // desc
        if date_cmp.is_ne() {
            return date_cmp;
        }
        b.id.cmp(&a.id) // desc by id
    });
    artifacts
}

/// 通用 cursor 分页（Node: `pageByCursor`）。
///
/// 返回 `(page, next_cursor)`：当过滤后剩余 > limit 时生成 next_cursor。
pub fn page_by_cursor<T>(items: Vec<T>, limit: usize, cursor: Option<&ArtifactCursor>) -> (Vec<T>, Option<String>)
where
    T: HasUpdatedAtAndId + Clone,
{
    let filtered: Vec<T> = items
        .into_iter()
        .filter(|item| is_after_cursor(item.updated_at(), item.id(), cursor))
        .collect();
    let page: Vec<T> = filtered.iter().take(limit).cloned().collect();
    let next_cursor = if filtered.len() > limit {
        page.last().map(|last| {
            encode_cursor(&ArtifactCursor {
                updated_at: last.updated_at(),
                id: last.id().to_string(),
            })
        })
    } else {
        None
    };
    (page, next_cursor)
}

/// 给定类型提供 `(updated_at, id)`，用于 `page_by_cursor`。
pub trait HasUpdatedAtAndId {
    fn updated_at(&self) -> DateTime<Utc>;
    fn id(&self) -> &str;
}

impl HasUpdatedAtAndId for CompanyArtifact {
    fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
    fn id(&self) -> &str {
        &self.id
    }
}

impl HasUpdatedAtAndId for CompanyArtifactGroup {
    fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
    fn id(&self) -> &str {
        &self.id
    }
}

// ---- Issue / group resolution ----

/// 构造 issue 摘要（Node: `getIssueSummary`）。
pub fn get_issue_summary(issue: &IssueGroupingRow) -> ArtifactIssueRef {
    ArtifactIssueRef {
        id: issue.id,
        identifier: issue.identifier.clone().unwrap_or_else(|| issue.id.to_string()),
        title: issue.title.clone(),
    }
}

/// 沿 parent 链向上走到 root（Node: `resolveRootIssueId`）。
///
/// 没有 parent 或已知 row 缺失 → 停在最后已知节点。
pub fn resolve_root_issue_id(issue_id: Uuid, issue_rows: &HashMap<Uuid, IssueGroupingRow>) -> Uuid {
    let mut current = match issue_rows.get(&issue_id) {
        Some(r) => r.clone(),
        None => return issue_id,
    };
    let mut seen: HashSet<Uuid> = HashSet::new();
    while let Some(parent_id) = current.parent_id {
        if !seen.insert(current.id) {
            break;
        }
        match issue_rows.get(&parent_id) {
            Some(p) => current = p.clone(),
            None => break,
        }
    }
    current.id
}

/// Group 解析（Node: `resolveGroupIssueId`）：`task` → 自己；`issue` → root。
pub fn resolve_group_issue_id(
    group_by: CompanyArtifactGroupBy,
    issue_id: Uuid,
    issue_rows: &HashMap<Uuid, IssueGroupingRow>,
) -> Uuid {
    match group_by {
        CompanyArtifactGroupBy::Task => issue_id,
        _ => resolve_root_issue_id(issue_id, issue_rows),
    }
}

/// 构造空的 group（Node: `emptyGroup`）。
pub fn empty_group(
    company_prefix: &str,
    query: &CompanyArtifactsQuery,
    group_by: CompanyArtifactGroupBy,
    issue: &IssueGroupingRow,
) -> CompanyArtifactGroup {
    let summary = get_issue_summary(issue);
    CompanyArtifactGroup {
        id: format!("{:?}:{}", group_by, issue.id),
        group_by,
        issue: summary.clone(),
        title: summary.title,
        count: 0,
        media_kinds: Vec::new(),
        preview_artifacts: Vec::new(),
        updated_at: issue.updated_at,
        href: build_artifacts_group_href(
            company_prefix,
            group_by,
            &issue.id.to_string(),
            query.kind,
            query.project_id.map(|u| u.to_string()).as_deref(),
            query.q.as_deref(),
        ),
    }
}

/// 按 `(group_by, issue_id)` 把 artifacts 聚合成 groups（Node: `buildArtifactGroups`）。
///
/// 返回结果按 `(updated_at desc, id desc)` 排序。
pub fn build_artifact_groups(
    artifacts: &[CompanyArtifact],
    company_prefix: &str,
    query: &CompanyArtifactsQuery,
    group_by: CompanyArtifactGroupBy,
    issue_rows: &HashMap<Uuid, IssueGroupingRow>,
) -> Vec<CompanyArtifactGroup> {
    let mut groups: HashMap<String, CompanyArtifactGroup> = HashMap::new();

    for artifact in artifacts {
        let group_issue_id = resolve_group_issue_id(group_by, artifact.issue.id, issue_rows);
        let group_issue = issue_rows.get(&group_issue_id).cloned().unwrap_or_else(|| IssueGroupingRow {
            id: artifact.issue.id,
            parent_id: None,
            identifier: Some(artifact.issue.identifier.clone()),
            title: artifact.issue.title.clone(),
            updated_at: artifact.updated_at,
        });
        let group_id = format!("{:?}:{}", group_by, group_issue_id);
        let group = groups.entry(group_id.clone()).or_insert_with(|| {
            empty_group(company_prefix, query, group_by, &group_issue)
        });
        group.count += 1;
        if !group.media_kinds.contains(&artifact.media_kind) {
            group.media_kinds.push(artifact.media_kind);
        }
        if group.preview_artifacts.len() < GROUP_PREVIEW_ARTIFACT_LIMIT {
            group.preview_artifacts.push(artifact.clone());
        }
        if artifact.updated_at > group.updated_at {
            group.updated_at = artifact.updated_at;
        }
    }

    let mut out: Vec<CompanyArtifactGroup> = groups.into_values().collect();
    out.sort_by(|a, b| {
        let cmp = b.updated_at.cmp(&a.updated_at);
        if cmp.is_ne() {
            return cmp;
        }
        b.id.cmp(&a.id)
    });
    out
}

// ---- Lister trait + service shell ----

/// DB 数据源抽象（Node: `companyArtifactsService.list` 中的 SQL 部分）。
///
/// 端口层只暴露 trait；具体实现由调用方（HTTP handler / 测试 mock）提供。
#[async_trait::async_trait]
pub trait CompanyArtifactsLister: Send + Sync {
    async fn list_artifacts(
        &self,
        company_id: Uuid,
        query: &CompanyArtifactsQuery,
        options: CompanyArtifactsListOptions,
    ) -> CompanyArtifactsResult<CompanyArtifactsResponse>;
}

/// 透传到 `list` 的可选参数（对应 Node `options` 第二参数）。
#[derive(Debug, Clone, Default)]
pub struct CompanyArtifactsListOptions {
    /// 调用方注入的额外 issue 条件（具体 SQL 由实现决定）。
    pub issue_conditions: Vec<String>,
    /// 用户 id — 当 `query.starred=true` 时必填。
    pub user_id: Option<String>,
}

/// 业务入口（Node: `companyArtifactsService(db, storage)`）。
pub struct CompanyArtifactsService<L: CompanyArtifactsLister + ?Sized> {
    lister: std::sync::Arc<L>,
}

impl<L: CompanyArtifactsLister + ?Sized> CompanyArtifactsService<L> {
    pub fn new(lister: std::sync::Arc<L>) -> Self {
        Self { lister }
    }

    /// 列出 artifacts（Node: `list`）。
    ///
    /// 当前实现：把纯 helper（cursor 解码 / 默认 limit 校验 / cursor 编码）
    /// 走本地，把 DB 查询委托给 `Lister`。
    pub async fn list(
        &self,
        company_id: Uuid,
        raw_query: CompanyArtifactsQuery,
        options: CompanyArtifactsListOptions,
    ) -> CompanyArtifactsResult<CompanyArtifactsResponse> {
        // 1. 校验 cursor
        let _cursor = decode_cursor(raw_query.cursor.as_deref())?;
        // 2. limit clamp
        let query = CompanyArtifactsQuery {
            limit: raw_query
                .limit
                .min(COMPANY_ARTIFACTS_MAX_LIMIT)
                .max(1),
            ..raw_query
        };
        // 3. starred 缺 user_id → 空响应（与 Node 一致）
        if query.starred && options.user_id.is_none() {
            return Ok(CompanyArtifactsResponse {
                artifacts: Vec::new(),
                groups: Vec::new(),
                selected_group: None,
                next_cursor: None,
            });
        }
        // 4. delegate to lister
        self.lister.list_artifacts(company_id, &query, options).await
    }
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn artifact(id: &str, updated_at: &str) -> CompanyArtifact {
        CompanyArtifact {
            id: id.to_string(),
            source: "work_product".to_string(),
            media_kind: CompanyArtifactMediaKind::File,
            title: format!("artifact {id}"),
            preview_text: None,
            content_type: None,
            content_path: None,
            open_path: None,
            download_path: None,
            issue: ArtifactIssueRef {
                id: Uuid::nil(),
                identifier: "X-1".to_string(),
                title: "Issue".to_string(),
            },
            project: None,
            created_by_agent: None,
            updated_at: dt(updated_at),
            href: "/x/issues/X-1#wp".to_string(),
        }
    }

    fn issue_row(id: Uuid, parent: Option<Uuid>, title: &str, updated: &str) -> IssueGroupingRow {
        IssueGroupingRow {
            id,
            parent_id: parent,
            identifier: Some(format!("X-{id}")),
            title: title.to_string(),
            updated_at: dt(updated),
        }
    }

    // ---- encode_cursor / decode_cursor ----

    #[test]
    fn r770_cursor_roundtrip() {
        let c = ArtifactCursor {
            updated_at: dt("2025-01-02T03:04:05Z"),
            id: "abc".to_string(),
        };
        let s = encode_cursor(&c);
        let parsed = decode_cursor(Some(&s)).unwrap().unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn r770_cursor_none_or_empty() {
        assert!(decode_cursor(None).unwrap().is_none());
        assert!(decode_cursor(Some("")).unwrap().is_none());
    }

    #[test]
    fn r770_cursor_invalid_base64() {
        let err = decode_cursor(Some("not base64!!")).unwrap_err();
        assert!(matches!(err, CompanyArtifactsError::InvalidCursor(_)));
    }

    #[test]
    fn r770_cursor_invalid_json() {
        let bad = URL_SAFE_NO_PAD.encode(b"{not json");
        let err = decode_cursor(Some(&bad)).unwrap_err();
        assert!(matches!(err, CompanyArtifactsError::InvalidCursor(_)));
    }

    #[test]
    fn r770_cursor_missing_field() {
        let bad = URL_SAFE_NO_PAD.encode(br#"{"id":"a"}"#);
        let err = decode_cursor(Some(&bad)).unwrap_err();
        assert!(matches!(err, CompanyArtifactsError::InvalidCursor(_)));
    }

    // ---- is_after_cursor ----

    #[test]
    fn r770_is_after_cursor_no_cursor() {
        assert!(is_after_cursor(dt("2025-01-01T00:00:00Z"), "a", None));
    }

    #[test]
    fn r770_is_after_cursor_by_date() {
        let c = ArtifactCursor {
            updated_at: dt("2025-01-02T00:00:00Z"),
            id: "z".to_string(),
        };
        assert!(is_after_cursor(dt("2025-01-01T00:00:00Z"), "z", Some(&c)));
        assert!(!is_after_cursor(dt("2025-01-03T00:00:00Z"), "z", Some(&c)));
    }

    #[test]
    fn r770_is_after_cursor_by_id_same_date() {
        let c = ArtifactCursor {
            updated_at: dt("2025-01-02T00:00:00Z"),
            id: "m".to_string(),
        };
        // id < "m" → after
        assert!(is_after_cursor(dt("2025-01-02T00:00:00Z"), "a", Some(&c)));
        // id > "m" → not after
        assert!(!is_after_cursor(dt("2025-01-02T00:00:00Z"), "z", Some(&c)));
    }

    // ---- escape_like_pattern ----

    #[test]
    fn r770_escape_like_pattern_basic() {
        assert_eq!(escape_like_pattern("plain"), "plain");
        assert_eq!(escape_like_pattern("a%b"), "a\\%b");
        assert_eq!(escape_like_pattern("a_b"), "a\\_b");
        assert_eq!(escape_like_pattern("a\\b"), "a\\\\b");
        assert_eq!(escape_like_pattern("a%_\\b"), "a\\%\\_\\\\b");
    }

    // ---- normalize_preview_text ----

    #[test]
    fn r770_normalize_preview_text_none() {
        assert!(normalize_preview_text(None).is_none());
    }

    #[test]
    fn r770_normalize_preview_text_empty() {
        assert!(normalize_preview_text(Some("")).is_none());
    }

    #[test]
    fn r770_normalize_preview_text_strips_markdown() {
        let md = "# Heading\n\nA `code` and **bold** and *italic* with [link](http://x) and ![img](a.png)";
        let out = normalize_preview_text(Some(md)).unwrap();
        assert!(out.contains("Heading"));
        assert!(out.contains("code"));
        assert!(out.contains("bold"));
        assert!(out.contains("italic"));
        assert!(out.contains("link"));
        assert!(!out.contains("!["));
        assert!(!out.contains("**"));
        assert!(!out.contains("`"));
    }

    #[test]
    fn r770_normalize_preview_text_strips_fenced_code() {
        let md = "before ```code\nblock\n``` after";
        let out = normalize_preview_text(Some(md)).unwrap();
        assert_eq!(out, "before after");
    }

    #[test]
    fn r770_normalize_preview_text_truncates() {
        let long: String = "a".repeat(PREVIEW_TEXT_MAX_LENGTH + 50);
        let out = normalize_preview_text(Some(&long)).unwrap();
        assert!(out.ends_with("..."));
        assert!(out.chars().count() <= PREVIEW_TEXT_MAX_LENGTH);
    }

    // ---- classify_media_kind ----

    #[test]
    fn r770_classify_media_kind_image() {
        assert_eq!(
            classify_media_kind(Some("image/png"), CompanyArtifactMediaKind::File),
            CompanyArtifactMediaKind::Image
        );
        assert_eq!(
            classify_media_kind(Some("IMAGE/JPEG"), CompanyArtifactMediaKind::File),
            CompanyArtifactMediaKind::Image
        );
    }

    #[test]
    fn r770_classify_media_kind_video() {
        assert_eq!(
            classify_media_kind(Some("video/mp4"), CompanyArtifactMediaKind::File),
            CompanyArtifactMediaKind::Video
        );
    }

    #[test]
    fn r770_classify_media_kind_text() {
        for ct in &[
            "text/plain",
            "text/markdown",
            "application/json",
            "application/vnd.api+json",
            "application/xml",
            "application/atom+xml",
            "application/markdown",
        ] {
            assert_eq!(
                classify_media_kind(Some(ct), CompanyArtifactMediaKind::File),
                CompanyArtifactMediaKind::Text,
                "{ct}"
            );
        }
    }

    #[test]
    fn r770_classify_media_kind_file_fallback() {
        assert_eq!(
            classify_media_kind(Some("application/octet-stream"), CompanyArtifactMediaKind::File),
            CompanyArtifactMediaKind::File
        );
        assert_eq!(
            classify_media_kind(None, CompanyArtifactMediaKind::File),
            CompanyArtifactMediaKind::File,
            "empty fallback"
        );
        assert_eq!(
            classify_media_kind(None, CompanyArtifactMediaKind::Empty),
            CompanyArtifactMediaKind::Empty,
            "fallback override"
        );
    }

    // ---- URL builders ----

    #[test]
    fn r770_build_issue_href_encodes() {
        let h = build_issue_href("acme corp", "X 1", "doc-1");
        assert_eq!(h, "/acme%20corp/issues/X%201#doc-1");
    }

    #[test]
    fn r770_attachment_content_path() {
        assert_eq!(attachment_content_path("att-9"), "/api/attachments/att-9/content");
    }

    #[test]
    fn r770_build_artifacts_group_href_all_kind() {
        let h = build_artifacts_group_href(
            "acme",
            CompanyArtifactGroupBy::Task,
            "00000000-0000-0000-0000-000000000001",
            CompanyArtifactKind::All,
            None,
            None,
        );
        assert!(h.contains("groupBy=task"));
        assert!(h.contains("groupIssueId="));
        assert!(!h.contains("kind="));
    }

    #[test]
    fn r770_build_artifacts_group_href_with_filters() {
        let h = build_artifacts_group_href(
            "acme",
            CompanyArtifactGroupBy::Issue,
            "i-1",
            CompanyArtifactKind::Image,
            Some("p-2"),
            Some("hello world"),
        );
        assert!(h.contains("groupBy=issue"));
        assert!(h.contains("kind=image"));
        assert!(h.contains("projectId=p-2"));
        assert!(h.contains("q=hello%20world"));
    }

    // ---- sort_artifacts ----

    #[test]
    fn r770_sort_artifacts_by_date_then_id() {
        let a = artifact("a1", "2025-01-01T00:00:00Z");
        let b = artifact("a2", "2025-01-02T00:00:00Z");
        let c = artifact("a3", "2025-01-01T00:00:00Z");
        let sorted = sort_artifacts(vec![a, b, c], &HashMap::new());
        assert_eq!(sorted[0].id, "a2");
        // a3 与 a1 同日期，按 id desc
        assert_eq!(sorted[1].id, "a3");
        assert_eq!(sorted[2].id, "a1");
    }

    #[test]
    fn r770_sort_artifacts_overrides_date() {
        let a = artifact("a1", "2025-01-01T00:00:00Z");
        let b = artifact("a2", "2025-01-02T00:00:00Z");
        let mut overrides = HashMap::new();
        overrides.insert("a1".to_string(), dt("2025-02-01T00:00:00Z"));
        let sorted = sort_artifacts(vec![a, b], &overrides);
        assert_eq!(sorted[0].id, "a1", "starred date overrides updated_at");
    }

    // ---- page_by_cursor ----

    #[test]
    fn r770_page_by_cursor_no_cursor() {
        let items: Vec<CompanyArtifact> = (0..5)
            .map(|i| artifact(&format!("id{i}"), "2025-01-01T00:00:00Z"))
            .collect();
        let (page, next) = page_by_cursor(items, 3, None);
        assert_eq!(page.len(), 3);
        assert!(next.is_some());
    }

    #[test]
    fn r770_page_by_cursor_with_cursor_filters() {
        let items: Vec<CompanyArtifact> = (0..5)
            .map(|i| {
                let day = 5 - i; // 5..1 — id0 最旧, id4 最新
                artifact(
                    &format!("id{i}"),
                    &format!("2025-01-{day:02}T00:00:00Z"),
                )
            })
            .collect();
        // Cursor 指向 id2 (2025-01-03)。desc 排序下「after cursor」= 更旧。
        // id0/id1 更新 → drop；id2 自身 → drop；id3/id4 更旧 → keep。
        let cur = ArtifactCursor {
            updated_at: dt("2025-01-03T00:00:00Z"),
            id: "id2".to_string(),
        };
        let (page, _next) = page_by_cursor(items, 5, Some(&cur));
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, "id3");
        assert_eq!(page[1].id, "id4");
    }

    #[test]
    fn r770_page_by_cursor_no_next_when_exhausted() {
        let items = vec![artifact("id0", "2025-01-01T00:00:00Z")];
        let (page, next) = page_by_cursor(items, 5, None);
        assert_eq!(page.len(), 1);
        assert!(next.is_none());
    }

    // ---- issue / group resolution ----

    #[test]
    fn r770_resolve_root_issue_id_no_parent() {
        let id = Uuid::new_v4();
        let mut rows = HashMap::new();
        rows.insert(id, issue_row(id, None, "root", "2025-01-01T00:00:00Z"));
        assert_eq!(resolve_root_issue_id(id, &rows), id);
    }

    #[test]
    fn r770_resolve_root_issue_id_chain() {
        let root = Uuid::new_v4();
        let mid = Uuid::new_v4();
        let leaf = Uuid::new_v4();
        let mut rows = HashMap::new();
        rows.insert(root, issue_row(root, None, "root", "2025-01-01T00:00:00Z"));
        rows.insert(mid, issue_row(mid, Some(root), "mid", "2025-01-02T00:00:00Z"));
        rows.insert(leaf, issue_row(leaf, Some(mid), "leaf", "2025-01-03T00:00:00Z"));
        assert_eq!(resolve_root_issue_id(leaf, &rows), root);
    }

    #[test]
    fn r770_resolve_root_issue_id_missing_known_returns_input() {
        let unknown = Uuid::new_v4();
        assert_eq!(resolve_root_issue_id(unknown, &HashMap::new()), unknown);
    }

    #[test]
    fn r770_resolve_root_issue_id_cycle_protection() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut rows = HashMap::new();
        rows.insert(a, issue_row(a, Some(b), "a", "2025-01-01T00:00:00Z"));
        rows.insert(b, issue_row(b, Some(a), "b", "2025-01-01T00:00:00Z"));
        // 不死循环：停在某节点
        assert_ne!(resolve_root_issue_id(a, &rows), Uuid::nil());
    }

    #[test]
    fn r770_resolve_group_issue_id_task_vs_issue() {
        let root = Uuid::new_v4();
        let leaf = Uuid::new_v4();
        let mut rows = HashMap::new();
        rows.insert(root, issue_row(root, None, "root", "2025-01-01T00:00:00Z"));
        rows.insert(leaf, issue_row(leaf, Some(root), "leaf", "2025-01-02T00:00:00Z"));
        assert_eq!(
            resolve_group_issue_id(CompanyArtifactGroupBy::Task, leaf, &rows),
            leaf
        );
        assert_eq!(
            resolve_group_issue_id(CompanyArtifactGroupBy::Issue, leaf, &rows),
            root
        );
    }

    #[test]
    fn r770_build_artifact_groups_aggregates() {
        let root = Uuid::new_v4();
        let leaf = Uuid::new_v4();
        let mut rows = HashMap::new();
        rows.insert(root, issue_row(root, None, "root", "2025-01-01T00:00:00Z"));
        rows.insert(leaf, issue_row(leaf, Some(root), "leaf", "2025-01-03T00:00:00Z"));

        let a1 = CompanyArtifact {
            id: "a1".into(),
            source: "work_product".into(),
            media_kind: CompanyArtifactMediaKind::Image,
            title: "a1".into(),
            preview_text: None,
            content_type: None,
            content_path: None,
            open_path: None,
            download_path: None,
            issue: ArtifactIssueRef { id: leaf, identifier: "X-1".into(), title: "leaf".into() },
            project: None,
            created_by_agent: None,
            updated_at: dt("2025-01-02T00:00:00Z"),
            href: "/x".into(),
        };
        let a2 = CompanyArtifact {
            id: "a2".into(),
            source: "document".into(),
            media_kind: CompanyArtifactMediaKind::Document,
            title: "a2".into(),
            preview_text: None,
            content_type: None,
            content_path: None,
            open_path: None,
            download_path: None,
            issue: ArtifactIssueRef { id: leaf, identifier: "X-1".into(), title: "leaf".into() },
            project: None,
            created_by_agent: None,
            updated_at: dt("2025-01-04T00:00:00Z"),
            href: "/x".into(),
        };

        let query = CompanyArtifactsQuery {
            kind: CompanyArtifactKind::All,
            group_by: CompanyArtifactGroupBy::Issue,
            ..Default::default()
        };
        let groups = build_artifact_groups(
            &[a1, a2],
            "acme",
            &query,
            CompanyArtifactGroupBy::Issue,
            &rows,
        );
        assert_eq!(groups.len(), 1, "两个 artifact 同属一个 root issue → 1 个 group");
        assert_eq!(groups[0].count, 2);
        assert!(groups[0].media_kinds.contains(&CompanyArtifactMediaKind::Image));
        assert!(groups[0].media_kinds.contains(&CompanyArtifactMediaKind::Document));
        assert_eq!(groups[0].preview_artifacts.len(), 2);
        assert_eq!(groups[0].updated_at, dt("2025-01-04T00:00:00Z"));
    }

    #[test]
    fn r770_build_artifact_groups_sorted_desc() {
        let i1 = Uuid::new_v4();
        let i2 = Uuid::new_v4();
        let mut rows = HashMap::new();
        rows.insert(i1, issue_row(i1, None, "older", "2025-01-01T00:00:00Z"));
        rows.insert(i2, issue_row(i2, None, "newer", "2025-01-05T00:00:00Z"));
        let mk = |id: &str, issue: Uuid, t: &str| CompanyArtifact {
            id: id.into(),
            source: "work_product".into(),
            media_kind: CompanyArtifactMediaKind::File,
            title: id.into(),
            preview_text: None,
            content_type: None,
            content_path: None,
            open_path: None,
            download_path: None,
            issue: ArtifactIssueRef { id: issue, identifier: format!("X-{issue}"), title: t.into() },
            project: None,
            created_by_agent: None,
            updated_at: dt("2025-01-01T00:00:00Z"),
            href: "/x".into(),
        };
        let artifacts = vec![mk("a", i1, "older"), mk("b", i2, "newer")];
        let query = CompanyArtifactsQuery::default();
        let groups = build_artifact_groups(
            &artifacts,
            "acme",
            &query,
            CompanyArtifactGroupBy::Task,
            &rows,
        );
        assert_eq!(groups[0].issue.id, i2, "newer group first");
        assert_eq!(groups[1].issue.id, i1);
    }

    // ---- empty_group ----

    #[test]
    fn r770_empty_group_shape() {
        let id = Uuid::new_v4();
        let row = issue_row(id, None, "title", "2025-01-01T00:00:00Z");
        let q = CompanyArtifactsQuery::default();
        let g = empty_group("acme", &q, CompanyArtifactGroupBy::Issue, &row);
        assert_eq!(g.count, 0);
        assert!(g.media_kinds.is_empty());
        assert!(g.preview_artifacts.is_empty());
        assert!(g.href.contains("/artifacts?"));
        assert_eq!(g.title, "title");
    }

    // ---- Service shell ----

    struct MockLister {
        captured_company_id: std::sync::Mutex<Option<Uuid>>,
        captured_query: std::sync::Mutex<Option<CompanyArtifactsQuery>>,
        captured_user_id: std::sync::Mutex<Option<String>>,
    }

    impl MockLister {
        fn new() -> Self {
            Self {
                captured_company_id: std::sync::Mutex::new(None),
                captured_query: std::sync::Mutex::new(None),
                captured_user_id: std::sync::Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl CompanyArtifactsLister for MockLister {
        async fn list_artifacts(
            &self,
            company_id: Uuid,
            query: &CompanyArtifactsQuery,
            options: CompanyArtifactsListOptions,
        ) -> CompanyArtifactsResult<CompanyArtifactsResponse> {
            *self.captured_company_id.lock().unwrap() = Some(company_id);
            *self.captured_query.lock().unwrap() = Some(query.clone());
            *self.captured_user_id.lock().unwrap() = options.user_id;
            Ok(CompanyArtifactsResponse::default())
        }
    }

    #[tokio::test]
    async fn r770_service_starred_without_user_returns_empty() {
        let lister = std::sync::Arc::new(MockLister::new());
        let svc = CompanyArtifactsService::new(lister.clone());
        let q = CompanyArtifactsQuery {
            starred: true,
            ..Default::default()
        };
        let resp = svc
            .list(Uuid::new_v4(), q, CompanyArtifactsListOptions::default())
            .await
            .unwrap();
        assert!(resp.artifacts.is_empty());
        // 不应调用底层
        assert!(lister.captured_company_id.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn r770_service_delegates_with_normalized_query() {
        let lister = std::sync::Arc::new(MockLister::new());
        let svc = CompanyArtifactsService::new(lister.clone());
        let q = CompanyArtifactsQuery {
            limit: COMPANY_ARTIFACTS_MAX_LIMIT + 999,
            ..Default::default()
        };
        let resp = svc
            .list(Uuid::new_v4(), q, CompanyArtifactsListOptions {
                user_id: Some("u-1".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(resp.artifacts.is_empty());
        let captured = lister.captured_query.lock().unwrap().clone().unwrap();
        assert_eq!(captured.limit, COMPANY_ARTIFACTS_MAX_LIMIT);
        let uid = lister.captured_user_id.lock().unwrap().clone();
        assert_eq!(uid, Some("u-1".into()));
    }

    #[tokio::test]
    async fn r770_service_rejects_invalid_cursor() {
        let lister = std::sync::Arc::new(MockLister::new());
        let svc = CompanyArtifactsService::new(lister);
        let q = CompanyArtifactsQuery {
            cursor: Some("garbage!!".into()),
            ..Default::default()
        };
        let err = svc
            .list(Uuid::new_v4(), q, CompanyArtifactsListOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, CompanyArtifactsError::InvalidCursor(_)));
    }
}
