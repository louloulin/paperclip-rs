//! `/v1/issues*`（**4 个注册键**）+ handler 共享实现（bridge 面复用）。
//!
//! - **写者**：M6-7（`docs/57` §3.2）。
//! - **上游**：`router.go:104-107` 的挂载点 + `internal/handler/plugin_action.go` 的
//!   `GetPluginIssue` / `PatchPluginIssue` / `ListPluginComments` / `CreatePluginComment`。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/v1/issues/:issue_ref` | GET, PATCH | `router.go:104-105` |
//! | `/v1/issues/:issue_ref/comments` | GET, POST | `router.go:106-107` |
//!
//! - **`:issue_ref` 是不透明引用**（可能是 uuid，也可能是 `PLUG-12` 形态）⇒ 按 `String` 收再解析：
//!   先按 identifier 查、再按 uuid 查。**不要**用 `Uuid` 提取器（合法引用会被判成 400）。
//! - **共享实现**：本文件的 handler 被 `routes/plugin_bridge/issues.rs` 直接注册到另一个前缀上
//!   ⇒ 投影/授权口径只有一份（`DoD` 的「两侧字节相同」靠这个成立）。
//! - **权限**：插件永远不能让某人做到他本来做不到的事 —— 第三步「调用者能否碰这个资源」由
//!   **普通资源读取器**保证（workspace 来自安装行、成员身份在 `policy::resolve_caller` 已验）。
//! - **不做什么**：
//!   1. 不在这里做 issue 的写入审计（本仓没有这张表）；
//!   2. **不跑 @mention 派发**（上游注释逐字）：能发 mention 的插件就等价于能从一次「点按钮」
//!      里启动 agent 运行、消耗工作区预算。上游只拒绝这一件事，其余（WS 事件、回复未解决线程
//!      自动重新打开）照做 —— 本地这两件的落点见下面的登记。
//!
//! ## 本片登记在 `docs/32` §9 的偏离（四条，都在本文件）
//!
//! 1. **`via_plugin_id` 由一次后置 UPDATE 补写**：本仓 `comment` 的仓储
//!    （`mc-repos/src/comment.rs`，M2 的写集）的 `NewComment` 没有这个字段 ⇒ 复用
//!    `CommentRepo::create`（不新写评论服务，`DoD` 第 4 条）之后补一条**单列** UPDATE，
//!    把「这次写入由哪个插件产生」记下来（上游 `CreateCommentParams.ViaPluginID`）。
//!    跨片缺口：M2 给 `NewComment` 补字段后应删掉这段。
//! 2. **列表里的 `comment.type` 由一次后置查询补齐**：`CommentRow` 的列投影不含 `type`
//!    （同上，M2 的文件），而公开 DTO 有 `type` 字段 ⇒ 这里对返回的行做一次
//!    `SELECT id, type` 补齐，而不是把 `type` 恒写成 `"comment"`（那会把线程里的
//!    `status_change` / `system` 评论说成普通评论）。
//! 3. **不发 WS 事件、不自动重新打开已解决的线程**：本仓的评论面在 M2 落地时没有事件总线
//!    （`docs/32` §9 的 M2-D15 / M3-D12 同源缺口），`CommentRepo::create` 已经 bump 了 issue 的
//!    `revision` / `last_activity_at`（上游那两条 follow-up 之外的部分）。登记为**跨片缺口**：
//!    插件发的评论在下一次拉取前不会实时出现。
//! 5. **`author_type` 的拼写归一**：M2 的评论仓储对「人」写 `'user'`（迁移 `538` 的 CHECK 同时
//!    放行 `'user'`/`'member'`），而上游写 `'member'`。公开契约（本片）折成上游的 `'member'`，
//!    否则同一个「人发的评论」在这份契约里会有两种取值。跨片缺口：M2 若把写侧改成 `member`，
//!    这段归一就可以删掉。
//! 4. **`status_category` 用行内推导**（`issue_category`：内置 status ⇒ `open`/`closed`），
//!    与 App 面 `IssueDto` 的「目录优先、行内兜底」在本仓同源（`mc-repos` 的
//!    `IssueRow::status_category`）。上游公开面写的是 status **key**（`issuestatus.IsBuiltIn`
//!    为真时 `statusCategory = i.Status`）—— 字段形状相同、取值域不同，已在偏离表登记。
//!
//! 行预算（门 ⑩）：本文件 ≤620 行。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use mc_core::comment::CommentAuthorType;
use mc_core::Id;
use mc_openapi::v1::{
    Comment, CommentListResponse, CreateCommentRequest, Issue, PatchIssueRequest,
};
use mc_plugin_host::scope::{
    SCOPE_COMMENTS_READ, SCOPE_COMMENTS_WRITE, SCOPE_ISSUES_READ, SCOPE_ISSUES_WRITE,
};
use mc_repos::comment::{CommentFilter, CommentRepo, CommentRow, NewComment};
use mc_repos::issue::{IssueRepo, IssueRow, IssueUpdate};
use mc_repos::issue_status::category_str;
use mc_repos::RepoError;

use super::policy::{self as policy, ActionCaller, ActionError, ActionResult};
use crate::state::AppState;

/// 上游 `maxPluginCommentsPerRead`：一次读的上限（查询取**最新** N 条，按时间升序返回）。
const MAX_COMMENTS_PER_READ: u32 = 200;

/// 上游 `maxPluginCommentBytes`：评论正文的上限（64 KiB），免得 surface 把评论当批量存储。
const MAX_COMMENT_BYTES: usize = 64 * 1024;

/// `/v1/issues*`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/issues/:issue_ref", get(get_issue).patch(patch_issue))
        .route(
            "/v1/issues/:issue_ref/comments",
            get(list_comments).post(create_comment),
        )
}

// ---------------------------------------------------------------------------
// handler（两侧挂载点共用）
// ---------------------------------------------------------------------------

/// `GET /v1/issues/:issue_ref`。
pub(crate) async fn get_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(issue_ref): Path<String>,
) -> Response {
    let request_id = policy::request_id(&headers);
    match get_issue_inner(&state, &headers, &issue_ref).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn get_issue_inner(
    state: &AppState,
    headers: &HeaderMap,
    issue_ref: &str,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, SCOPE_ISSUES_READ).await?;
    let issue = plugin_issue_for_caller(state, &caller, issue_ref).await?;
    Ok(issue_response(&issue))
}

/// `PATCH /v1/issues/:issue_ref`：**只允许 title / description**。
///
/// status / priority / assignee / parent / project / stage 各自带派发、目录或层级语义
/// （改 status 可能启动一次 agent 运行，自定义 status 要过每 workspace 的目录），在这里复制
/// 那些规则就是给插件第二份会漂移的副本。以后放宽是增量；现在把副作用做错不是。
pub(crate) async fn patch_issue(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(issue_ref): Path<String>,
    body: Bytes,
) -> Response {
    let request_id = policy::request_id(&headers);
    match patch_issue_inner(&state, &headers, &issue_ref, &body).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn patch_issue_inner(
    state: &AppState,
    headers: &HeaderMap,
    issue_ref: &str,
    body: &Bytes,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, SCOPE_ISSUES_WRITE).await?;
    let issue = plugin_issue_for_caller(state, &caller, issue_ref).await?;

    let request: PatchIssueRequest = decode(body)?;
    if request.title.is_none() && request.description.is_none() {
        return Err(ActionError::invalid("title or description is required"));
    }
    let expected = expected_revision(headers, request.expected_revision)?;
    if let Some(expected) = expected {
        if issue.revision != expected {
            return Err(revision_conflict());
        }
    }

    let title = request
        .title
        .as_deref()
        .map(|raw| {
            let value = sanitize_null_bytes(raw);
            if value.is_empty() {
                return Err(ActionError::invalid("title must not be empty"));
            }
            Ok(value)
        })
        .transpose()?;
    // 三态（`Option<Option<String>>`）：没给 ⇒ 不动；给了 ⇒ 写值。写成 `Some(map(...))` 会把
    // 「没给」变成「置 NULL」—— 那是一次静默的数据丢失。
    let description = request
        .description
        .map(|value| Some(sanitize_null_bytes(&value)));

    let updated = issue_repo(state)
        .update(
            caller.workspace_id,
            issue.id(),
            &IssueUpdate {
                expected_revision: expected,
                title,
                description,
                ..IssueUpdate::default()
            },
        )
        .await
        .map_err(|error| match error {
            RepoError::Conflict => revision_conflict(),
            RepoError::NotFound => ActionError::not_found("issue not found"),
            RepoError::Db(message) => {
                ActionError::unavailable(format!("update the issue: {message}"))
            }
        })?;
    Ok(issue_response(&updated))
}

/// `GET /v1/issues/:issue_ref/comments`。
pub(crate) async fn list_comments(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(issue_ref): Path<String>,
) -> Response {
    let request_id = policy::request_id(&headers);
    match list_comments_inner(&state, &headers, &issue_ref).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn list_comments_inner(
    state: &AppState,
    headers: &HeaderMap,
    issue_ref: &str,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, SCOPE_COMMENTS_READ).await?;
    let issue = plugin_issue_for_caller(state, &caller, issue_ref).await?;

    let mut filter = CommentFilter::for_issue(issue.id());
    filter.limit = MAX_COMMENTS_PER_READ;
    let listed = comment_repo(state)
        .list_for_issue(filter)
        .await
        .map_err(|error| ActionError::unavailable(format!("list comments: {error}")))?;

    let types = comment_types(state, &listed.comments).await?;
    let comments = listed
        .comments
        .iter()
        .map(|row| public_comment(row, types.get(&row.id())))
        .collect::<Vec<_>>();
    Ok(Json(CommentListResponse { comments }).into_response())
}

/// `POST /v1/issues/:issue_ref/comments`。
///
/// 评论的作者是**那个人**，并标出是哪个插件产生的（时间线可以渲染「某人（via 某面板）」）。
pub(crate) async fn create_comment(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(issue_ref): Path<String>,
    body: Bytes,
) -> Response {
    let request_id = policy::request_id(&headers);
    match create_comment_inner(&state, &headers, &issue_ref, &body).await {
        Ok(response) => response,
        Err(error) => error.into_response_for(&request_id),
    }
}

async fn create_comment_inner(
    state: &AppState,
    headers: &HeaderMap,
    issue_ref: &str,
    body: &Bytes,
) -> ActionResult<Response> {
    let caller = policy::resolve_caller(state, headers, SCOPE_COMMENTS_WRITE).await?;
    let issue = plugin_issue_for_caller(state, &caller, issue_ref).await?;

    let request: CreateCommentRequest = decode(body)?;
    let content = sanitize_null_bytes(&request.content);
    if content.is_empty() {
        return Err(ActionError::invalid("content is required"));
    }
    if content.len() > MAX_COMMENT_BYTES {
        return Err(ActionError::invalid("content is too long"));
    }

    let parent_id = match request.parent_id.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => {
            let parent = Id::parse(raw)
                .map_err(|_| ActionError::invalid("parent_id must be a valid UUID"))?;
            Some(parent)
        }
        _ => None,
    };

    // 写归属由**怎么认证**决定，不由请求体里的任何字段决定：真人保有自己的署名（另外记下
    // 是哪个插件产生的），而 event hook 以安装身份写入 —— 因为没有人可归属。
    let (author_type, author_id) = match caller.actor {
        policy::ActionActor::Member(user_id) => (CommentAuthorType::User, user_id.to_string()),
        policy::ActionActor::Plugin => (
            CommentAuthorType::Plugin,
            caller.installation.id().to_string(),
        ),
    };

    let created = comment_repo(state)
        .create(NewComment {
            workspace_id: caller.workspace_id,
            issue_id: issue.id(),
            parent_id,
            author_type,
            author_id,
            body: content,
            source_task_id: None,
        })
        .await
        .map_err(|error| match error {
            // 父评论不存在 / 不属于同一 issue / 已软删 ⇒ 上游 400 `invalid_parent_comment`。
            RepoError::NotFound => ActionError::invalid("invalid parent comment"),
            other => ActionError::unavailable(format!("create the comment: {other}")),
        })?;

    // 偏离 1：`via_plugin_id` 只能后置补写（M2 的 `NewComment` 没有这个字段）。
    mark_via_plugin(state, created.id(), caller.installation.id()).await?;

    let types = comment_types(state, std::slice::from_ref(&created)).await?;
    Ok((
        StatusCode::CREATED,
        Json(public_comment(&created, types.get(&created.id()))),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// 第三步授权：issue 的解析与收窄
// ---------------------------------------------------------------------------

/// 上游 `pluginIssueForUser`：把「插件能不能碰这个 issue」判到底。
///
/// workspace 来自**安装行**（不是客户端头），而 `policy::resolve_caller` 已经验过
/// 「调用者是那个 workspace 的成员」⇒「issue 在这个 workspace 里」+「调用者是其成员」
/// 恰好等于这个人本来就有的触达范围。范围外的 issue 是 **404**，理由与普通端点相同：
/// 插件不能借这个端点确认一个它读不到的 id 存在。
///
/// # Errors
///
/// [`ActionError`]（404 `not_found`）。
pub(crate) async fn plugin_issue_for_caller(
    state: &AppState,
    caller: &ActionCaller,
    issue_ref: &str,
) -> ActionResult<IssueRow> {
    let issue_ref = issue_ref.trim();
    if issue_ref.is_empty() {
        return Err(ActionError::not_found("issue not found"));
    }
    let Some(issue) = resolve_issue(state, caller, issue_ref).await? else {
        return Err(ActionError::not_found("issue not found"));
    };
    // 回调令牌是针对**某一个** issue 签发的，就只够到那一个。没有这一步，这枚令牌在它活着的
    // 五分钟里等价于「该 workspace 里 actor 能看到的每一个 issue」。
    //
    // 用 404 而不是 403：调用者很可能能用别的办法看到这个 issue，而「你被限定在别处」
    // 会确认这个 id 存在。
    if let Some(scope) = caller.issue_scope {
        if issue.id() != scope {
            return Err(ActionError::not_found("issue not found"));
        }
    }
    Ok(issue)
}

/// 上游 `resolvePluginIssue`：先按 identifier、再按 uuid —— 比较**解析后的 id**，而不是原始
/// 字符串（否则同一个 issue 换一种写法就会一会儿通过、一会儿不通过）。
async fn resolve_issue(
    state: &AppState,
    caller: &ActionCaller,
    issue_ref: &str,
) -> ActionResult<Option<IssueRow>> {
    let repo = issue_repo(state);
    if let Ok(issue) = repo.get_by_identifier(caller.workspace_id, issue_ref).await {
        return Ok(Some(issue));
    }
    let Ok(id) = Id::parse(issue_ref) else {
        return Ok(None);
    };
    match repo.get(caller.workspace_id, id).await {
        Ok(issue) => Ok(Some(issue)),
        Err(RepoError::NotFound) => Ok(None),
        Err(error) => Err(ActionError::unavailable(format!("load the issue: {error}"))),
    }
}

// ---------------------------------------------------------------------------
// DTO 投影
// ---------------------------------------------------------------------------

/// 上游 `pluginIssuePayload` / `setPublicIssueETag`：显式映射进稳定的公开 DTO，新加 App 面字段
/// 不会顺带漏进公开契约。
fn issue_response(issue: &IssueRow) -> Response {
    let payload = public_issue(issue);
    let mut response = Json(payload).into_response();
    if let Ok(value) = HeaderValue::from_str(&format!("W/\"{}\"", issue.revision)) {
        response.headers_mut().insert(header::ETAG, value);
    }
    response
}

/// `issue` 行 → 公开 DTO。
pub(crate) fn public_issue(issue: &IssueRow) -> Issue {
    Issue {
        id: issue.id.to_string(),
        workspace_id: issue.workspace_id.to_string(),
        number: issue.number,
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        description: issue.description.clone(),
        status: issue.status.clone(),
        status_category: issue
            .status_category()
            .map_or(String::new(), |category| category_str(category).to_string()),
        priority: issue.priority.clone(),
        assignee_type: issue.assignee_type.clone(),
        assignee_id: issue.assignee_id.clone(),
        creator_type: issue.creator_type.clone(),
        creator_id: issue.creator_id.clone(),
        parent_issue_id: issue.parent_issue_id.map(|id| id.to_string()),
        project_id: issue.project_id.map(|id| id.to_string()),
        position: issue.position,
        stage: issue.stage,
        start_date: issue.start_date.map(|date| date.to_string()),
        due_date: issue.due_date.map(|date| date.to_string()),
        created_at: timestamp(issue.created_at),
        updated_at: timestamp(issue.updated_at),
        revision: issue.revision,
        last_activity_at: issue.last_activity_at.map(timestamp_nano),
        metadata: json_object(&issue.metadata),
        properties: json_object(&issue.properties),
    }
}

/// `comment` 行 → 公开 DTO。`comment_type` 来自后置补齐（偏离 2）。
pub(crate) fn public_comment(comment: &CommentRow, comment_type: Option<&String>) -> Comment {
    Comment {
        id: comment.id.to_string(),
        author_type: public_author_type(&comment.author_type),
        author_id: comment.author_id.clone(),
        content: comment.body.clone(),
        comment_type: comment_type
            .cloned()
            .unwrap_or_else(|| "comment".to_string()),
        parent_id: comment
            .parent_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        created_at: timestamp(comment.created_at),
        deleted_at: comment.deleted_at.map(timestamp).unwrap_or_default(),
    }
}

/// 公开契约的 `author_type`：本地对「人」有两种拼写（M2 的 `CommentAuthorType::User` 写 `user`，
/// 上游的 `comment.author_type` 写 `member`，迁移 `538` 的 CHECK 两个都放行）⇒ 公开契约只有
/// 一份拼写，折成上游的 `member`（见文件头偏离 5）。
fn public_author_type(raw: &str) -> String {
    match raw {
        "user" | "member" => "member".to_string(),
        other => other.to_string(),
    }
}

/// 上游 `timestampToString`：RFC3339、秒精度、UTC `Z`。
pub(crate) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 上游 `timestampToNanoPtr`：`RFC3339Nano`（`last_activity_at` 用）。
fn timestamp_nano(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

/// JSONB → 公开 DTO 的 `BTreeMap`（非对象值按空对象处理，与 `skip_serializing_if` 的读法一致）。
fn json_object(value: &serde_json::Value) -> BTreeMap<String, serde_json::Value> {
    value
        .as_object()
        .map(|object| object.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn issue_repo(state: &AppState) -> IssueRepo {
    IssueRepo::new(state.db.clone())
}

fn comment_repo(state: &AppState) -> CommentRepo {
    CommentRepo::new(&state.db)
}

/// 请求体解码：形状不符 / 空 body → 400 `invalid request body`（上游 `json.Decoder` 同判）。
fn decode<T: serde::de::DeserializeOwned>(body: &Bytes) -> ActionResult<T> {
    if body.is_empty() {
        return Err(ActionError::invalid("invalid request body"));
    }
    serde_json::from_slice(body).map_err(|_| ActionError::invalid("invalid request body"))
}

/// 上游 `publicIssueExpectedRevision`：`If-Match` 与 `expected_revision` 必须指向同一个版本。
fn expected_revision(headers: &HeaderMap, body_revision: Option<i64>) -> ActionResult<Option<i64>> {
    let header = headers
        .get(mc_openapi::v1::HEADER_IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let Some(header) = header else {
        if body_revision.is_some_and(|revision| revision < 1) {
            return Err(ActionError::invalid(
                "expected_revision must be a positive integer",
            ));
        }
        return Ok(body_revision);
    };

    let header = header.strip_prefix("W/").unwrap_or(header).trim();
    let header = header.trim_matches('"');
    let parsed = header.parse::<i64>().ok().filter(|value| *value >= 1);
    let Some(parsed) = parsed else {
        return Err(ActionError::new(
            StatusCode::BAD_REQUEST,
            "invalid_if_match",
            "If-Match must contain a positive issue revision",
        ));
    };
    if body_revision.is_some_and(|revision| revision != parsed) {
        return Err(ActionError::new(
            StatusCode::BAD_REQUEST,
            "revision_mismatch",
            "If-Match and expected_revision must identify the same revision",
        ));
    }
    Ok(Some(parsed))
}

/// 上游 `writePublicIssueRevisionConflict`。
fn revision_conflict() -> ActionError {
    ActionError::conflict("resource changed since it was loaded").with_code("revision_conflict")
}

impl ActionError {
    /// 换掉稳定码（保留状态码与文案）。
    fn with_code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }
}

/// 上游 `sanitizeNullBytes`：去掉 NUL —— PG 的 `TEXT` 不接受 `\0`，带上会让整条语句失败。
pub(crate) fn sanitize_null_bytes(raw: &str) -> String {
    raw.replace('\0', "")
}

/// 偏离 2：补齐 `comment.type`（`CommentRow` 的列投影不含它，见文件头）。
async fn comment_types(
    state: &AppState,
    comments: &[CommentRow],
) -> ActionResult<std::collections::HashMap<Id, String>> {
    if comments.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let ids = comments.iter().map(CommentRow::id).collect::<Vec<_>>();
    let raw = ids.iter().map(|id| id.0).collect::<Vec<_>>();
    let rows = sqlx::query_as::<_, (uuid::Uuid, String)>(
        "SELECT id, type FROM comment WHERE id = ANY($1)",
    )
    .bind(&raw)
    .fetch_all(state.db.pool())
    .await
    .map_err(|error| ActionError::unavailable(format!("load comment types: {error}")))?;
    Ok(rows
        .into_iter()
        .map(|(id, comment_type)| (Id(id), comment_type))
        .collect())
}

/// 偏离 1：把「这次写入由哪个插件产生」补记到 `comment.via_plugin_id`。
async fn mark_via_plugin(
    state: &AppState,
    comment_id: Id,
    installation_id: Id,
) -> ActionResult<()> {
    sqlx::query("UPDATE comment SET via_plugin_id = $2 WHERE id = $1")
        .bind(comment_id.0)
        .bind(installation_id.0)
        .execute(state.db.pool())
        .await
        .map_err(|error| ActionError::unavailable(format!("attribute the comment: {error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn issue_row() -> IssueRow {
        IssueRow {
            id: uuid::Uuid::nil(),
            workspace_id: uuid::Uuid::nil(),
            number: 12,
            identifier: "PLUG-12".into(),
            title: "t".into(),
            description: Some("d".into()),
            status: "todo".into(),
            status_name: None,
            priority: "none".into(),
            assignee_type: None,
            assignee_id: None,
            creator_type: "user".into(),
            creator_id: uuid::Uuid::nil().to_string(),
            parent_issue_id: None,
            project_id: None,
            position: 1.0,
            stage: None,
            start_date: None,
            due_date: None,
            last_activity_at: None,
            revision: 7,
            metadata: serde_json::json!({"a": 1}),
            properties: serde_json::json!({"p": null}),
            triage_state: None,
            origin: None,
            origin_task_id: None,
            source_context_id: None,
            created_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap(),
        }
    }

    #[test]
    fn public_issue_projects_the_stable_contract() {
        let payload = public_issue(&issue_row());
        assert_eq!(payload.identifier, "PLUG-12");
        // 上游 `timestampToString`：秒精度 + UTC `Z`（不是 `+00:00`）。
        assert_eq!(payload.created_at, "2026-01-02T03:04:05Z");
        assert_eq!(payload.revision, 7);
        assert_eq!(payload.metadata.get("a"), Some(&serde_json::json!(1)));
        assert_eq!(payload.properties.get("p"), Some(&serde_json::json!(null)));
        // status_category 是行内推导（偏离 4）：todo ⇒ open。
        assert_eq!(payload.status_category, "open");
    }

    #[test]
    fn expected_revision_accepts_body_and_if_match() {
        let mut headers = HeaderMap::new();
        // 空头 + 无 body 版本 ⇒ None（不设乐观锁）。
        assert_eq!(expected_revision(&HeaderMap::new(), None).unwrap(), None);
        // body 版本 < 1 ⇒ 400。
        assert!(expected_revision(&HeaderMap::new(), Some(0)).is_err());
        assert_eq!(
            expected_revision(&HeaderMap::new(), Some(3)).unwrap(),
            Some(3)
        );

        headers.insert(
            mc_openapi::v1::HEADER_IF_MATCH,
            HeaderValue::from_static("W/\"9\""),
        );
        assert_eq!(expected_revision(&headers, None).unwrap(), Some(9));
        assert_eq!(expected_revision(&headers, Some(9)).unwrap(), Some(9));
        // 两个来源打架 ⇒ 400 `revision_mismatch`。
        let error = expected_revision(&headers, Some(8)).unwrap_err();
        assert_eq!(error.code, "revision_mismatch");
        // 非法 `If-Match` ⇒ 400 `invalid_if_match`。
        let mut bad = HeaderMap::new();
        bad.insert(
            mc_openapi::v1::HEADER_IF_MATCH,
            HeaderValue::from_static("\"abc\""),
        );
        assert_eq!(
            expected_revision(&bad, None).unwrap_err().code,
            "invalid_if_match"
        );
    }

    #[test]
    fn revision_conflict_is_409_with_its_own_code() {
        let error = revision_conflict();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.code, "revision_conflict");
    }

    #[test]
    fn sanitize_drops_nul_bytes() {
        assert_eq!(sanitize_null_bytes("a\0b"), "ab");
        assert_eq!(sanitize_null_bytes("abc"), "abc");
    }
}
