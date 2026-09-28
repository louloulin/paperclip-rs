//! `GET /api/issues/:id/timeline` —— **写者 M9-8**（`LUM-1823`，`docs/62` §4.1 第 9 行）。
//!
//! | method | 路径 | 上游 handler | 上游行 |
//! |---|---|---|---|
//! | GET | `/api/issues/:id/timeline` | `ListTimeline`（`activity.go:149`） | `L63–L393` |
//!
//! # 这条路由的历史（anchor 的「原地搬运」，`docs/62` §3.1 / §5）
//!
//! 本路由原先注册在 `crate::routes::issues::router()`，handler 是 `not_implemented`（501）。
//! M9-0 把它**搬到这里**（注册键逐字不变），**本片把那个 501 占位换成真实现** ——
//! 也就是门 ⑦ 记的**占位升级**：`local` / `baseline` / `known_gap` / `local_only`
//! **一个数都不动**，只有 `implemented_placeholder 2 → 1` / `implemented_real +1`。
//!
//! # 两种响应形态**共存**（上游 `activity.go:127-147`，`#1929` 的边界兼容）
//!
//! - **四参全缺** ⇒ 裸 JSON 数组，**ASC**（最早的在前）。新契约。
//! - **`limit` / `before` / `after` / `around` 任一非空** ⇒ wrapped 对象，
//!   **DESC** + `next_cursor` / `prev_cursor` **恒 null** + `has_more_after` **恒 false**。
//!
//! 两种形态装的是**同一批** entry，只差排序与外壳。老客户端（Desktop ≤ v0.2.25 与
//! `#2128`…`#1929` 之间的 Web bundle）发四参、按 `TimelinePageSchema` 解析；游标行走
//! 现在是**空操作**，客户端只看到一整页。
//!
//! 🔴 判定是「任一非空」（上游逐字 `q.Get(x) != ""`）⇒ `?limit=`（给了键、值是空串）
//! **不**触发 wrapped。`around` 另有唯一作用：在 DESC 切片里定位锚点并回 `target_index`。
//!
//! 🔴 **四参的「边界」另一半**：上游**已经删掉**时间游标分页（`#2128` → `#1929`）——
//! 它把回复线程**切在了页边界上**，而在实测规模（每 issue p99 ≈ 30 条评论）下游标机制
//! 纯属开销 ⇒ `limit` / `before` / `after` **除了选形态之外没有任何别的效果**（不裁窗口、
//! 不报 400、非法值也不回落）。
//!
//! # 🔴 两侧独立截断、**不** clamp 到同一个 floor（上游注释逐字，本片最承重的一段）
//!
//! 两半各自按 [`mc_repos::timeline::TIMELINE_HARD_CAP`] 砍到 newest-N，**各自**报自己被砍了，
//! 响应头 [`HEADER_TIMELINE_TRUNCATED`] 的取值由 [`truncated_kinds`] 渲染。
//!
//! 为什么**不能**共享一个 floor：评论是**人类节奏**的（p99 ≈ 30、生产上见过最多 ≈ 1.1k）
//! 所以几乎永远顶不到上限，而活动是**机器节奏**的（描述自动保存、每次 agent run、
//! 状态 / assignee 变更）所以**常规**顶得到 ⇒ 共享 floor **几乎总是活动面的 floor 在砍
//! 已经取回、且本来能正常渲染的评论**。它为了一个纯装饰性的属性（「返回的窗口是一段
//! 正确交错、没有哪一段只含一种」的连续切片）去删真实内容，在一条三十条评论的繁忙 issue
//! 上是**纯亏损**。不 clamp 的代价只是**较老一段**里活动密度变高 —— 那是元数据、不是
//! 内容 —— 而且它被**报告**而不是被藏起来。
//!
//! # 为什么截断信号走**响应头**而不是体字段
//!
//! 未分页的响应是一个**裸 JSON 数组**（`z.array(TimelineEntrySchema)`），**没有地方**
//! 放标志位。头是**加性**的：老客户端继续按原样校验。🔴 刻意**没有**配一个「窗口起点」
//! 头：`timestampToString` 是**秒级** RFC3339、而真正的排序键是全精度的
//! `(created_at, id)` ⇒ 它没法在不跳过、不重复同一秒内若干行的前提下用来续读。
//!
//! # 授权链（`docs/62` §3 表的 J 行 / §6.5 的 M9-8 行）
//!
//! - **无会话 ⇒ 401**：[`AuthUser`] 提取器（缺 `X-Multica-User-Id`）；
//! - **workspace 解析 ⇒ 400**：复用 `/api/issues*` 的 [`resolve_workspace`]（header →
//!   `?workspace_id` → `?workspace_slug`），与同前缀的其余键逐字一致；
//! - **非 member ⇒ 403**：[`require_member`] 查 `member` 表、没有行即 403；
//! - 🔴 **非本 workspace 的 issue ⇒ 404**（**不是** 403、也不是空列表）—— [`load_issue`]
//!   的 `workspace_id` 谓词是唯一来源。
//!
//! # 两条**不做**
//!
//! - **不碰** `GetAssigneeFrequency`（`activity.go:394+`）—— **M2-A** 的账；
//! - **不补** `activity_log` 的写入面（覆盖率事实 R-M9-4，由 M9-10 登记）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::attachment::AttachmentRow;
use mc_repos::comment::CommentReactionRow;
use mc_repos::timeline::{
    ActivityLogRow, MemberIdentityRow, TimelineCommentRow, TimelineLimits, TimelineRepo,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::ApiResult;
use crate::routes::attachments::read::AttachmentResponse;
use crate::routes::auth_user::AuthUser;
use crate::routes::comments::ReactionDto;
use crate::routes::issues::{issue_repo, load_issue, resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

/// 截断信号头（上游 `activity.go:104` 的 `HeaderTimelineTruncated`）。
///
/// 上游导出它是为了让 CORS 层能引用**同一个标识符**（`corsExposedHeaders`）：
/// 自定义响应头对浏览器 JS 是**不可见**的，除非被显式 expose ⇒ 一次「改了名却没改到
/// CORS 列表」的重命名会**静默**把这个信号关掉。
pub const HEADER_TIMELINE_TRUNCATED: &str = "x-timeline-truncated";

/// 本片的 1 条注册键（**单形态** —— 上游是 `r.Get("/timeline")` 这样的 plain 子路由，
/// 见 `router.go:1981`；补尾斜杠会触发 `EXTRA_ALIAS`，`docs/62` §1.4 / §6.5 第 3 条）。
pub fn router() -> Router<Arc<AppState>> {
    router_with_limits(TimelineLimits::DEFAULT)
}

/// 同一条键，但截断上限可注入（真库用例用它把 2000 的上限缩到个位数）。
///
/// 生产路径**只**走 [`router`] ⇒ 上限恒为 [`mc_repos::timeline::TIMELINE_HARD_CAP`]。
pub fn router_with_limits(limits: TimelineLimits) -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/issues/:id/timeline", get(list_timeline))
        .layer(Extension(limits))
}

// ---------------------------------------------------------------------------
// 响应形状（上游 `activity.go:19-58` 的 `TimelineEntry`）
// ---------------------------------------------------------------------------

/// 时间线上的一条：要么是 `activity_log` 行，要么是 `comment` 行。
///
/// 上游的 `omitempty` 语义在本仓落成 `skip_serializing_if`（可空列**不写键**）——
/// 与全仓惯例一致（见 `routes/attachments/read.rs` 的同款登记）。
#[derive(Debug, Clone, Serialize)]
pub struct TimelineEntry {
    /// `"activity"` 或 `"comment"`。
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    pub actor_type: String,
    pub actor_id: String,
    pub created_at: String,
    /// 展示用身份，由 member actor 水合得来；**成员离开本 workspace 后仍然可读**，
    /// 而 `actor_type + actor_id` 才是耐久的归属键。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub actor_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub actor_avatar_url: String,
    // ---- 仅 activity ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    // ---- 仅 comment ----
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "is_zero_i64")]
    pub revision: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_type: Option<String>,
    /// 只在 quick action 跑出来的评论上出现。**不可伪造**：通用评论端点上没有
    /// 任何请求字段能设置它。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quick_action_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<ReactionDto>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_by_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    /// 只在 tombstone（还有回复而被删掉的评论）上出现。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde 的 skip_serializing_if 只接受 `&T`
fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// wrapped 形态的外壳（上游 `activity.go:127-135` 的 `timelinePaginatedResponse`）。
///
/// **游标恒 null、`has_more_after` 恒 false**（新服务端一次返回整条时间线）；
/// `has_more_before` 现在是**诚实的** —— 它报告硬上限的 clamp，而不是硬编码 false。
#[derive(Debug, Clone, Serialize)]
pub struct TimelinePageResponse {
    pub entries: Vec<TimelineEntry>,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
    pub has_more_before: bool,
    pub has_more_after: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_index: Option<usize>,
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/issues/{id}/timeline`（上游 `ListTimeline`）。
#[allow(clippy::too_many_arguments)]
async fn list_timeline(
    State(state): State<Arc<AppState>>,
    Extension(limits): Extension<TimelineLimits>,
    headers: HeaderMap,
    Path(raw_issue_id): Path<String>,
    Query(raw): Query<HashMap<String, String>>,
    user: AuthUser,
) -> ApiResult<Response> {
    let raw_params = TimelineQuery::from(&raw);
    // `/api/issues*` 的 workspace 解析逐字复用（header → `?workspace_id` → `?workspace_slug`），
    // 免得同一个前缀下出现**两套**选 workspace 的口径。
    let workspace_query = WorkspaceQuery {
        workspace_id: raw.get("workspace_id").cloned(),
        workspace_slug: raw.get("workspace_slug").cloned(),
    };
    let workspace_id = resolve_workspace(&state, &headers, &workspace_query).await?;
    require_member(&state, workspace_id, user.id()).await?;
    // 🔴 跨 workspace 的 issue ⇒ 404（`load_issue` 的 `workspace_id` 谓词）。
    let issue = load_issue(&issue_repo(&state), workspace_id, &raw_issue_id).await?;

    let repo = TimelineRepo::new(state.db.clone());
    // 两侧**各自**按 newest-N 读（多一行探针），**不**共享 floor。
    let comments = repo
        .list_comments(
            issue.id().as_uuid(),
            workspace_id.as_uuid(),
            limits.probe_limit(),
        )
        .await
        .map_err(db_err)?;
    let activities = repo
        .list_activities(issue.id().as_uuid(), limits.probe_limit())
        .await
        .map_err(db_err)?;

    let (comments, comments_truncated) = take_newest(comments, limits.hard_cap);
    let (activities, activities_truncated) = take_newest(activities, limits.hard_cap);

    // 🔴 截断头的**唯一**写点是 `finish`（两种形态共用）—— 上游逐字也只有一处。
    let kinds = truncated_kinds(comments_truncated, activities_truncated);
    let truncated = comments_truncated || activities_truncated;

    // 上游 `commentsToEntries`：reactions + attachments 各**一次**批量查询。
    let ids: Vec<Uuid> = comments.iter().map(|c| c.id).collect();
    let reactions = group_reactions(&state, &ids).await;
    let attachments = group_attachments(
        repo.list_attachments_for_comments(workspace_id.as_uuid(), &ids)
            .await
            .map_err(db_err)?,
    );

    let mut entries = merge_entries(
        comments_to_entries(&comments, &reactions, &attachments),
        activities.iter().map(activity_to_entry).collect(),
        !raw_params.want_wrapped,
    );
    // 🔴 404 那道闸**已经**过了 ⇒ 这里水合的 member id 全都来自一条已授权的 issue，
    // 不构成任意的用户查询面。查不到**只是展示层**的问题，绝不让整条时间线不可用。
    hydrate_member_actors(&state, &mut entries).await;

    if raw_params.want_wrapped {
        let page = TimelinePageResponse {
            target_index: raw_params
                .around
                .as_deref()
                .and_then(|anchor| entries.iter().position(|e| e.id == anchor)),
            entries,
            next_cursor: None,
            prev_cursor: None,
            has_more_before: truncated,
            has_more_after: false,
        };
        return Ok(finish(Json(page), kinds));
    }
    Ok(finish(Json(entries), kinds))
}

/// 两条半开路径共用的一步：把截断信号**写进响应头**（值是 [`truncated_kinds`] 的渲染）。
fn finish<T: Serialize>(body: Json<T>, kinds: &str) -> Response {
    let mut response = body.into_response();
    if !kinds.is_empty() {
        if let Ok(value) = HeaderValue::from_str(kinds) {
            response
                .headers_mut()
                .insert(HEADER_TIMELINE_TRUNCATED, value);
        }
    }
    response
}

fn db_err(err: mc_repos::RepoError) -> Error {
    match err {
        mc_repos::RepoError::NotFound => Error::NotFound {
            resource: "timeline".into(),
        },
        other => Error::Database(other.to_string()),
    }
}

/// 非 member ⇒ 403（`docs/62` §6.5 的 M9-8 行逐字）。
///
/// 顺序逐字：提取器先跑（401）⇒ 再解 workspace（400）⇒ 最后查成员（403）。
/// 成员闸**先于**任何表访问（不存在的 workspace 与非成员都落 403，不泄露 workspace 是否存在）。
async fn require_member(state: &AppState, workspace_id: Id, user_id: Id) -> Result<(), Error> {
    let role: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
    role.map(|(role,)| role)
        .map(|_| ())
        .ok_or_else(|| Error::Forbidden {
            message: "issue timeline requires workspace membership".into(),
        })
}

// ---------------------------------------------------------------------------
// keyset 四参（`docs/62` §6.5 的 M9-8 行第 2 条）
// ---------------------------------------------------------------------------

/// 四参的原始取值。**只看「有没有非空值」**，不看它合不合法。
struct TimelineQuery {
    want_wrapped: bool,
    around: Option<String>,
}

/// 四参里被认得的四个键（`?workspace_id` / `?workspace_slug` 由 `resolve_workspace` 管）。
const KEYSET_PARAMS: [&str; 4] = ["limit", "before", "after", "around"];

impl TimelineQuery {
    /// 只看「有没有**非空**值」，不看它合不合法（上游逐字 `q.Get(x) != ""`）。
    fn from(raw: &HashMap<String, String>) -> Self {
        let value = |name: &str| raw.get(name).filter(|v| !v.is_empty());
        Self {
            want_wrapped: KEYSET_PARAMS.iter().any(|name| value(name).is_some()),
            around: value("around").cloned(),
        }
    }
}

// ---------------------------------------------------------------------------
// 截断（上游 `activity.go:68-116`）
// ---------------------------------------------------------------------------

/// `truncatedKinds(comments, activities)`：两个上限**互相独立** ⇒ 取值点名**哪几类**
/// 被砍了，什么都没砍时是**空串**（⇒ 不发那个头）。
fn truncated_kinds(comments: bool, activities: bool) -> &'static str {
    match (comments, activities) {
        (true, true) => "activity,comment",
        (true, false) => "comment",
        (false, true) => "activity",
        (false, false) => "",
    }
}
/// `takeNewest`：把一次 `probe_limit` 读裁到 `cap`，并用**探针行**证明还有更老的行。
///
/// `rows` 必须**升序**（两条 SQL 的外层已排好）⇒ 最新的那 `cap` 条是**尾部**。
fn take_newest<T: Clone>(rows: Vec<T>, cap: usize) -> (Vec<T>, bool) {
    if rows.len() <= cap {
        return (rows, false);
    }
    (rows[rows.len() - cap..].to_vec(), true)
}

// ---------------------------------------------------------------------------
// 映射与合并（上游 `activity.go:236-296`）
// ---------------------------------------------------------------------------

/// 上游 `commentsToEntries`：按**给定顺序**逐行映射，并挂上批量取回的
/// reactions / attachments。
fn comments_to_entries(
    comments: &[TimelineCommentRow],
    reactions: &HashMap<Uuid, Vec<CommentReactionRow>>,
    attachments: &HashMap<Uuid, Vec<AttachmentRow>>,
) -> Vec<TimelineEntry> {
    comments
        .iter()
        .map(|c| TimelineEntry {
            entry_type: "comment".into(),
            id: c.id.to_string(),
            actor_type: c.author_type.clone(),
            actor_id: c.author_id.to_string(),
            created_at: ts(c.created_at),
            actor_name: String::new(),
            actor_avatar_url: String::new(),
            action: None,
            details: None,
            content: Some(c.content.clone()),
            parent_id: c.parent_id.map(|v| v.to_string()),
            updated_at: Some(ts(c.updated_at)),
            revision: c.revision,
            comment_type: Some(c.comment_type.clone()),
            quick_action_id: c.quick_action_id.map(|v| v.to_string()),
            reactions: reactions
                .get(&c.id)
                .map(|rows| rows.iter().map(ReactionDto::from).collect())
                .unwrap_or_default(),
            attachments: attachments
                .get(&c.id)
                .map(|rows| rows.iter().map(AttachmentResponse::stable).collect())
                .unwrap_or_default(),
            resolved_at: c.resolved_at.map(ts),
            resolved_by_type: c.resolved_by_type.clone(),
            resolved_by_id: c.resolved_by_id.map(|v| v.to_string()),
            source_task_id: c.source_task_id.map(|v| v.to_string()),
            deleted_at: c.deleted_at.map(ts),
        })
        .collect()
}

/// 上游 `activityToEntry`。
///
/// 🔴 `actor_type` 可空 ⇒ 上游把它渲染成**空串**（不是 `"system"`、也不是省略）：
/// 归属键的「取不到」必须与「取到了一个叫空串的 actor」区分得开。
fn activity_to_entry(a: &ActivityLogRow) -> TimelineEntry {
    TimelineEntry {
        entry_type: "activity".into(),
        id: a.id.to_string(),
        actor_type: a.actor_type.clone().unwrap_or_default(),
        actor_id: a.actor_id.map(|v| v.to_string()).unwrap_or_default(),
        created_at: ts(a.created_at),
        actor_name: String::new(),
        actor_avatar_url: String::new(),
        action: Some(a.action.clone()),
        details: Some(a.details.clone()),
        content: None,
        parent_id: None,
        updated_at: None,
        revision: 0,
        comment_type: None,
        quick_action_id: None,
        reactions: Vec::new(),
        attachments: Vec::new(),
        resolved_at: None,
        resolved_by_type: None,
        resolved_by_id: None,
        source_task_id: None,
        deleted_at: None,
    }
}

/// 上游 `mergeTimeline`：合并后按 `(created_at, id)` 排序。
///
/// 🔴 排序键是 `TimelineEntry` 上那两个**字符串**（`CreatedAt string` + `ID string`），
/// 上游 `sort.Slice` 逐字比的就是它们 ⇒ 秒级 RFC3339 定宽 ⇒ 字典序 == 时间序。
/// 这也正是**不能**发「窗口起点」头的原因（见文件头）。
///
/// `ascending = true` ⇒ 最老在前（新契约的裸数组）；`false` ⇒ 最新在前
/// （wrapped 老契约）。
fn merge_entries(
    mut comments: Vec<TimelineEntry>,
    mut activities: Vec<TimelineEntry>,
    ascending: bool,
) -> Vec<TimelineEntry> {
    let mut out = Vec::with_capacity(comments.len() + activities.len());
    out.append(&mut comments);
    out.append(&mut activities);
    // 排序键 `(created_at, id)` 是**全序**（id 是主键）⇒ 结果里不可能有两条同键行，
    // 这就是「去重」的可观测形态；`sort_by` 的稳定性在这里只影响**不可能发生**的平局。
    out.sort_by(|a, b| {
        let ord = a
            .created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id));
        if ascending {
            ord
        } else {
            ord.reverse()
        }
    });
    out
}

/// 上游 `groupReactions`：**一次**批量查询 + 按 `comment_id` 分组；查不到就是空
/// （展示层的问题绝不让整条时间线不可用）。
async fn group_reactions(
    state: &AppState,
    comment_ids: &[Uuid],
) -> HashMap<Uuid, Vec<CommentReactionRow>> {
    let ids: Vec<Id> = comment_ids.iter().copied().map(Id).collect();
    let Ok(rows) = mc_repos::comment::CommentRepo::new(&state.db)
        .list_reactions(&ids)
        .await
    else {
        return HashMap::new();
    };
    let mut grouped: HashMap<Uuid, Vec<CommentReactionRow>> = HashMap::new();
    for row in rows {
        grouped.entry(row.comment_id).or_default().push(row);
    }
    grouped
}

/// 上游 `groupAttachments` 的分组半边（取回的半边在 `TimelineRepo` 上）。
fn group_attachments(rows: Vec<AttachmentRow>) -> HashMap<Uuid, Vec<AttachmentRow>> {
    let mut grouped: HashMap<Uuid, Vec<AttachmentRow>> = HashMap::new();
    for row in rows {
        if let Some(comment_id) = row.comment_id {
            grouped.entry(comment_id).or_default().push(row);
        }
    }
    grouped
}

/// 上游 `hydrateTimelineMemberActors`：**只**给 `member` actor 补水合身份，
/// 整条响应**跑一次**（绝不逐行跑）。
///
/// 🔴 本仓的「人」写作 `user` 而不是 `member`（`migrations/compat/538` 与
/// `routes/issues` 的 `normalize_assignee_type`：`member` → `user`，单向）。
/// 只认 `member` 会让**本仓自己写的**评论**永远**没有 `actor_name` ⇒ 两种词表都认。
async fn hydrate_member_actors(state: &AppState, entries: &mut [TimelineEntry]) {
    let mut seen = HashSet::new();
    let ids: Vec<Uuid> = entries
        .iter()
        .filter(|e| is_human_actor(&e.actor_type))
        .filter_map(|e| Uuid::parse_str(&e.actor_id).ok())
        .filter(|id| seen.insert(*id))
        .collect();
    if ids.is_empty() {
        return;
    }
    let Ok(rows) = TimelineRepo::new(state.db.clone())
        .member_identities(&ids)
        .await
    else {
        return;
    };
    let by_id: HashMap<Uuid, MemberIdentityRow> = rows.into_iter().map(|r| (r.id, r)).collect();
    for entry in entries.iter_mut() {
        if !is_human_actor(&entry.actor_type) {
            continue;
        }
        let Ok(id) = Uuid::parse_str(&entry.actor_id) else {
            continue;
        };
        let Some(user) = by_id.get(&id) else {
            continue;
        };
        entry.actor_name.clone_from(&user.name);
        // 上游 `resolveAvatarURL` 在 `Storage == nil`（本仓没有 CDN 概念）时逐字
        // **原样返回**存储值；全仓惯例也是原样透传（`routes/workspaces.rs`）。
        entry.actor_avatar_url = user.avatar_url.clone().unwrap_or_default();
    }
}

fn is_human_actor(actor_type: &str) -> bool {
    matches!(actor_type, "member" | "user")
}

/// 秒级 RFC3339（本仓全仓约定；上游 `timestampToString` 逐字同精度）。
fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 证据面（门 ⑤ 不碰库 / 门 ⑥ 真库）—— 因门 ⑩ 的 800 行硬上限拆成子模块，
/// 先例 = `docs/32` §30 的 **D10**（`routes/cloud/subscriptions/tests/`）与
/// `routes/onboarding/tests/`、`routes/uploads/tests/`。拆分理由与登记见 `docs/32` §9.13。
#[cfg(test)]
mod tests;
