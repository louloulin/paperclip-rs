//! `/api/issues/:id/wakeups*` 的 6 条路由（7 个注册键）—— M5-6 把 M5-0 的 501 占位换成真实现。
//!
//! - **上游**：`handler/issue_wakeup.go` 320（`ListIssueWakeups`31 / `CreateIssueWakeup`47 /
//!   `DisableIssueWakeup`25 / `EnableIssueWakeup`31 / `EditIssueWakeupInstruction`30），
//!   子路由注册在 `router.go:1986-1991`（挂在 `/api/issues/{id}` 的 chi `Mount` 下）。
//! - **形态**：上游这 6 条是 plain 子路由（**不是** `Route(...) + Get("/")`）⇒ 只有**一个**形态，
//!   不加尾斜杠别名；路径参数沿用 `:wakeupId`（改名等于换 ⑦ 注册键）。注册表**逐字不变**：
//!   7 个键（`GET`/`POST` 同 path + 4 条子路由 + …）仍在，只是 handler 名从 `not_implemented`
//!   换成本文件的真实函数名。
//!
//! # 与门 ⑦ 的关系（R3，**汇报时必须说清**）
//!
//! `route_parity.py` 的占位正则只认 `\bplaceholder\b`，而这 7 条的 handler 名是
//! `not_implemented` ⇒ 它们**一直**被计在 `implemented_real` 里（不是本片造成的）。
//! 本片把行为从 501 换成真实现后：**键数不变**（仍 7 条）、`implemented_real` **不变**、
//! 唯一增量是新增注册的 `GET /api/issue-wakeup-summaries`（`local` **+1**）。
//! 「全仓假实现 13 → 6」的**人工口径**变化才是本片的真实进度信号（`docs/44` §6.1）。
//!
//! # 请求 / 响应契约（逐条对齐上游）
//!
//! | 路由 | 体上限 | 成功 |
//! | --- | ---: | --- |
//! | `GET /api/issues/:id/wakeups` | — | 200 `[...]`（空是 `[]`，不是 `null`） |
//! | `POST /api/issues/:id/wakeups` | 32768 | **201** + 行 |
//! | `PUT /api/issues/:id/wakeups/:wakeupId` | 32768 | **200** + 行（upsert 复用 `POST` 的校验/服务路径） |
//! | `POST .../:wakeupId/disable` | — | 200 + 行 |
//! | `POST .../:wakeupId/enable` | 1024 | 200 + 行 |
//! | `PATCH .../:wakeupId/instruction` | 160000 | **204**（无 body） |
//!
//! 坏 body（超限 / 非法 JSON / 未知字段，等价上游 `DisallowUnknownFields`）⇒ 400
//! `invalid wakeup body` / `invalid enable body` / `invalid instruction body`；
//! `:wakeupId` 非 UUID ⇒ 400 `invalid wakeup id`。鉴权见
//! [`super::super::issue_wakeups`] 文件头（非成员 403、跨 workspace 404）。

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_errors::Error;
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

use super::context::{issue_repo, load_issue, resolve_workspace, WorkspaceQuery};
use crate::routes::auth_user::AuthUser;
use crate::routes::issue_wakeups::{
    accessible_agent_ids, repo_error, require_member_role, wakeup_error, WakeupResult,
};
use crate::state::AppState;
use mc_autopilot::wakeup::service::{self, WakeupEnableInput, WakeupInput, WakeupInstructionInput};
use mc_repos::issue::IssueRow;
use mc_repos::wakeup::{issue as wi, IssueWakeupView, WakeupRow};

/// 体上限（上游 `http.MaxBytesReader` 的字节数）。
const CREATE_BODY_MAX: usize = 32_768;
/// `enable` 的体上限（只回传 revision / at / rearm）。
const ENABLE_BODY_MAX: usize = 1024;
/// `instruction` 的体上限（含 12000 字节指令 + 旧指令回显）。
const INSTRUCTION_BODY_MAX: usize = 160_000;

// ---------------------------------------------------------------------------
// 请求体 DTO（上游 `service.WakeupInput` 的 JSON 形态 + 未知字段拒绝）
// ---------------------------------------------------------------------------

/// `POST/PUT /api/issues/:id/wakeups` 的请求体（上游 `service.WakeupInput`）。
///
/// 字段与 `service::WakeupInput` 一一对应，这里**再定义一次**的唯一目的是
/// `deny_unknown_fields`（上游 `dec.DisallowUnknownFields()`）；服务层 DTO 不承担 HTTP 严格性。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct WakeupBody {
    /// 被唤醒的 agent。
    agent_id: String,
    /// 注入 prompt 的指令。
    instruction: String,
    /// `event | at | every | cron`。
    kind: String,
    /// `once | continuous`。
    mode: String,
    /// 订阅的事件名集合。
    event_types: Vec<String>,
    /// 「只有这个 agent 的动作算数」。
    filter_agent_id: String,
    /// `member | agent`。
    filter_actor_type: String,
    /// 主体过滤 id。
    filter_actor_id: String,
    /// 「只有这个 run 的动作算数」。
    filter_task_id: String,
    /// 注册它的评论。
    parent_comment_id: String,
    /// `kind=at`：多少秒后。
    after_seconds: i64,
    /// `kind=at`：绝对时间（RFC3339）。
    at: Option<DateTime<Utc>>,
    /// `kind=every`：间隔秒。
    interval_seconds: i64,
    /// `kind=cron`：5 字段表达式。
    cron_expression: String,
    /// 调度时区（IANA，空 ⇒ UTC）。
    timezone: String,
}

impl From<WakeupBody> for WakeupInput {
    fn from(body: WakeupBody) -> Self {
        Self {
            agent_id: body.agent_id,
            instruction: body.instruction,
            kind: body.kind,
            mode: body.mode,
            event_types: body.event_types,
            filter_agent_id: body.filter_agent_id,
            filter_actor_type: body.filter_actor_type,
            filter_actor_id: body.filter_actor_id,
            filter_task_id: body.filter_task_id,
            parent_comment_id: body.parent_comment_id,
            after_seconds: body.after_seconds,
            at: body.at,
            interval_seconds: body.interval_seconds,
            cron_expression: body.cron_expression,
            timezone: body.timezone,
        }
    }
}

/// `POST /api/issues/:id/wakeups/:wakeupId/enable` 的请求体（上游 `service.WakeupEnableInput`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct EnableBody {
    /// 乐观并发版本号。
    revision: i64,
    /// `kind=at` 顺手改一次时间。
    at: Option<DateTime<Utc>>,
    /// 已消费的 `once` 必须显式 rearm。
    rearm: bool,
}

/// `PATCH .../instruction` 的请求体（上游 `service.WakeupInstructionInput`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct InstructionBody {
    /// 新指令。
    instruction: String,
    /// 客户端读到的旧指令。
    expected_instruction: String,
    /// 客户端读到的旧版本号。
    revision: i64,
}

// ---------------------------------------------------------------------------
// 上下文 / 解析小工具
// ---------------------------------------------------------------------------

/// issue 面的请求上下文：workspace + issue 行 + 调用者角色。
struct IssueWakeupCtx {
    /// 目标 issue（已确认在本 workspace）。
    issue: IssueRow,
    /// 调用者的成员角色（`owner` / `admin` / `member` …）。
    role: String,
}

/// workspace 解析（400）→ 成员身份（**非成员 403**）→ issue 定位（404）。
///
/// 上游顺序是 issue → 成员；这里反过来（成员先），为的是让「非成员」在任何 id 上都得到
/// 一致的 403（DoD ⑦），而不是先泄漏/否认 issue 的存在性。见
/// [`super::super::issue_wakeups`] 文件头。
async fn load_ctx(
    state: &AppState,
    headers: &HeaderMap,
    query: &WorkspaceQuery,
    raw_id: &str,
    user: AuthUser,
) -> WakeupResult<IssueWakeupCtx> {
    let workspace_id = resolve_workspace(state, headers, query).await?;
    let role = require_member_role(state, workspace_id, user.id()).await?;
    // `load_issue` 把 NotFound 映射成 `issue`（`issues/helpers::repo_err`），与上游
    // `404 issue not found` 同口径。
    let issue = load_issue(&issue_repo(state), workspace_id, raw_id).await?;
    Ok(IssueWakeupCtx { issue, role })
}

/// `:wakeupId` 解析（上游 `parseUUIDOrBadRequest(w, raw, "wakeup id")`）。
fn parse_wakeup_id(raw: &str) -> WakeupResult<Uuid> {
    Id::parse(raw.trim())
        .map(|id| id.0)
        .map_err(|_| {
            Error::Validation {
                message: "invalid wakeup id".into(),
                details: Vec::new(),
            }
            .into()
        })
}

/// 体上限 + 反序列化（等价上游 `MaxBytesReader` + `DisallowUnknownFields`）。
fn parse_body<T: for<'de> Deserialize<'de>>(bytes: &Bytes, limit: usize, label: &str) -> WakeupResult<T> {
    if bytes.len() > limit {
        return Err(body_error(label));
    }
    serde_json::from_slice(bytes).map_err(|_| body_error(label))
}

/// 400 `invalid <label> body`。
fn body_error(label: &str) -> crate::routes::issue_wakeups::WakeupHttpError {
    Error::Validation {
        message: format!("invalid {label} body"),
        details: Vec::new(),
    }
    .into()
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// `GET /api/issues/:id/wakeups`：issue 级列表（含掩码后的过滤字段与展示名）。
async fn list_issue_wakeups(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> WakeupResult<Json<Vec<IssueWakeupView>>> {
    let ctx = load_ctx(&state, &headers, &query, &raw_id, user).await?;
    let agent_ids = accessible_agent_ids(&state, ctx.issue.workspace_id(), user.id(), &ctx.role).await?;
    let rows = wi::list_issue_wakeups(
        state.db.pool(),
        ctx.issue.workspace_id,
        ctx.issue.id,
        &agent_ids,
    )
    .await
    .map_err(repo_error)?;
    Ok(Json(rows))
}

/// `POST /api/issues/:id/wakeups`：新建订阅（201）。
async fn create_issue_wakeup(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(raw_id): Path<String>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> WakeupResult<(StatusCode, Json<WakeupRow>)> {
    let ctx = load_ctx(&state, &headers, &query, &raw_id, user).await?;
    let input = parse_body::<WakeupBody>(&body, CREATE_BODY_MAX, "wakeup")?;
    let row = service::create(
        state.db.pool(),
        ctx.issue.id,
        user.id().0,
        // 本地无 agent actor ⇒ source_task_id 恒空（见 issue_wakeups.rs 文件头）。
        None,
        input.into(),
    )
    .await
    .map_err(wakeup_error)?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// `PUT /api/issues/:id/wakeups/:wakeupId`：upsert（同一个 `CreateIssueWakeup` handler，200）。
async fn upsert_issue_wakeup(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, raw_wakeup)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> WakeupResult<Json<WakeupRow>> {
    let ctx = load_ctx(&state, &headers, &query, &raw_id, user).await?;
    let wakeup_id = parse_wakeup_id(&raw_wakeup)?;
    let input = parse_body::<WakeupBody>(&body, CREATE_BODY_MAX, "wakeup")?;
    let row = service::save(
        state.db.pool(),
        ctx.issue.id,
        user.id().0,
        None,
        Some(wakeup_id),
        input.into(),
        None,
    )
    .await
    .map_err(wakeup_error)?;
    Ok(Json(row))
}

/// `POST /api/issues/:id/wakeups/:wakeupId/disable`：停用 + 丢弃待处理收据 + 撤销未启动 run。
async fn disable_issue_wakeup(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, raw_wakeup)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
) -> WakeupResult<Json<WakeupRow>> {
    let ctx = load_ctx(&state, &headers, &query, &raw_id, user).await?;
    let wakeup_id = parse_wakeup_id(&raw_wakeup)?;
    let (row, _cancelled) = service::disable(state.db.pool(), ctx.issue.id, wakeup_id, user.id().0)
        .await
        .map_err(wakeup_error)?;
    // `_cancelled`：被撤销的未启动 run（上游在这里广播 `task.cancelled`）。
    // 本地没有 task 事件广播原语 ⇒ 交给 M5-8（`docs/44` §3.2 的派发片），本片只交付数据面。
    Ok(Json(row))
}

/// `POST /api/issues/:id/wakeups/:wakeupId/enable`：重新启用（可顺手改 `at` / 显式 rearm）。
async fn enable_issue_wakeup(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, raw_wakeup)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> WakeupResult<Json<WakeupRow>> {
    let ctx = load_ctx(&state, &headers, &query, &raw_id, user).await?;
    let wakeup_id = parse_wakeup_id(&raw_wakeup)?;
    let body: EnableBody = parse_body(&body, ENABLE_BODY_MAX, "enable")?;
    let enable = WakeupEnableInput {
        revision: body.revision,
        at: body.at,
        rearm: body.rearm,
    };
    let row = service::enable(
        state.db.pool(),
        ctx.issue.id,
        user.id().0,
        None,
        wakeup_id,
        &enable,
    )
    .await
    .map_err(wakeup_error)?;
    Ok(Json(row))
}

/// `PATCH /api/issues/:id/wakeups/:wakeupId/instruction`：只改指令（204）。
async fn edit_issue_wakeup_instruction(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((raw_id, raw_wakeup)): Path<(String, String)>,
    Query(query): Query<WorkspaceQuery>,
    user: AuthUser,
    body: Bytes,
) -> WakeupResult<StatusCode> {
    let ctx = load_ctx(&state, &headers, &query, &raw_id, user).await?;
    let wakeup_id = parse_wakeup_id(&raw_wakeup)?;
    let body: InstructionBody = parse_body(&body, INSTRUCTION_BODY_MAX, "instruction")?;
    let input = WakeupInstructionInput {
        instruction: body.instruction,
        expected_instruction: body.expected_instruction,
        revision: body.revision,
    };
    service::edit_instruction(state.db.pool(), ctx.issue.id, wakeup_id, user.id().0, &input)
        .await
        .map_err(wakeup_error)?;
    Ok(StatusCode::NO_CONTENT)
}

/// wakeup 子切片：6 条路由 / 7 个注册键（见文件头表）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/issues/:id/wakeups",
            get(list_issue_wakeups).post(create_issue_wakeup),
        )
        .route("/api/issues/:id/wakeups/:wakeupId", put(upsert_issue_wakeup))
        .route(
            "/api/issues/:id/wakeups/:wakeupId/disable",
            post(disable_issue_wakeup),
        )
        .route(
            "/api/issues/:id/wakeups/:wakeupId/enable",
            post(enable_issue_wakeup),
        )
        .route(
            "/api/issues/:id/wakeups/:wakeupId/instruction",
            patch(edit_issue_wakeup_instruction),
        )
}
