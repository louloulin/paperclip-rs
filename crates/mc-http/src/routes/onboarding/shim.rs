//! 两条 **DEPRECATED** shim（`runtime-bootstrap` / `no-runtime-bootstrap`）—— **写者 M9-3**。
//!
//! 上游：`internal/handler/onboarding_shim.go`（623 行）+ `bootstrapOnboarding{Runtime,NoRuntime}
//! Request/Response`。
//! ⚠️ 这两条**是活路由**（不是反向验收）：`docs/62` §1.8 实测本波有 2 条 DEPRECATED 活路由。
//!
//! # 上游模块头逐字（决定本文件的三条纪律）
//!
//! > 「These handlers are intentionally minimal copies of the pre-v3 implementation,
//! > **condensed to inline DB calls (no OnboardingService / WorkspaceContentService layer**).
//! > … **DO NOT add features here. DO NOT change behavior. The contract is "what pre-v3
//! > desktop expects".」
//!
//! ⇒ ① **inline DB calls** 是上游自己的形态（本文件照落：`sqlx` 直接在**一个事务**里跑
//! provision 链，不经任何 service 层）；② **禁止加功能**；③ 行为**逐字**照抄。
//!
//! # provision 链（两条的**逐行**判据，`DoD` 第 3 条）
//!
//! | 步骤 | `runtime-bootstrap` | `no-runtime-bootstrap` |
//! | --- | --- | --- |
//! | 成员校验 | 403 `not a member of this workspace` | 同 |
//! | runtime 校验 | 400 `invalid runtime_id` / 403 `this runtime is private; only its owner can create agents on it` | **不查**（上游逐字：用户显式跳过 runtime 步 ⇒ 无条件 seed） |
//! | Helper agent | 找或建（`name = 'Multica Helper'` ∧ `visibility = 'workspace'`） | **不建** |
//! | starter issue | 找或建（标题 = [`ONBOARDING_ISSUE_TITLE`]，`starter_prompt` 非空时**整体替换**正文） | 找或建（标题 = [`NO_RUNTIME_ISSUE_TITLE`]，EN/ZH 正文按 `user.language` 选） |
//! | 标记完成 | `COALESCE(onboarded_at, now())` | 同 |
//! | starter content | `starter_content_state` `NULL → 'imported'` | 同 |
//! | 事务 | **一个** | **一个** |
//!
//! 文案常量在 [`shim_content`]（`DoD` 第 5 条的预判拆法）。
//!
//! # 授权链（A 行 = user-scoped；`router.go:1626-1627` 与 `GET/PATCH /api/me` 同一组）
//!
//! 无会话 ⇒ **401**（[`AuthUser`] 提取器，形态差异见 `profile.rs` 模块头的登记）；
//! 成员/角色判定在 handler 体里
//! （上游的 `GetMemberByUserAndWorkspace` 失败 ⇒ **403**，不是全仓 member 口径的 404 ——
//! 上游逐字就是 403，本片照抄）。
//!
//! # 与上游的两处**有意**偏离（登记 `docs/32` §48 / §9.18）
//!
//! 1. **`LockAndFindActiveDuplicate` 的本地等价**：上游用 `SELECT … FOR UPDATE` +
//!    `pg_advisory` 风格的锁去重；本片照落「`SELECT … FOR UPDATE` 命中活跃行则复用、
//!    否则新建」，条件与上游逐字同（`workspace_id` + `title` + 无 parent + 无 project +
//!    `category <> 'closed'`）。差异：并发下两次调用可能各建一条（上游的锁更严）。
//! 2. **分析事件（`analytics.OnboardingCompleted` / `AgentCreated` / `IssueCreated`）不发**：
//!    本仓的 funnel 面是 `mc-conformance` 的 ⑨ 门与 autopilot 评估，不在 HTTP 切片里。
//!    ⇒ **可观察的状态**（`onboarded_at` / `starter_content_state` / agent / issue）**逐条**照落。

use std::sync::Arc;

// `onboarding/mod.rs` 是 anchor 冻结文件（**不得**编辑）⇒ 子模块用 `#[path]` 挂。
#[path = "shim_content.rs"]
pub(crate) mod shim_content;

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use mc_core::Id;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use shim_content::{
    no_runtime_issue_description, NO_RUNTIME_ISSUE_TITLE, ONBOARDING_ASSISTANT_AVATAR_URL,
    ONBOARDING_ASSISTANT_DESCRIPTION, ONBOARDING_ASSISTANT_INSTRUCTIONS, ONBOARDING_ASSISTANT_NAME,
    ONBOARDING_ISSUE_DESCRIPTION, ONBOARDING_ISSUE_TITLE,
};

use crate::error::{ApiError, ApiResult};
// ⚠️ 本仓有**两个** `AuthUser`：`middleware::authn::AuthUser` 是中间件写进
// `Extensions` 的那一个（**不是**提取器），`routes::auth_user::AuthUser` 才是提取器。
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;
use mc_errors::Error;

/// 上游 `runtimeBootstrapBodyLimit = 8 * 1024`（逐字：别让这条端点被当批量存储）。
const RUNTIME_BOOTSTRAP_BODY_LIMIT: usize = 8 * 1024;

/// 上游 `maxStarterPromptLen = 2 * 1024`（**按 rune 数**判，上游用 `utf8.RuneCountInString`）。
const MAX_STARTER_PROMPT_LEN: usize = 2 * 1024;

/// 上游建 Helper agent 时的 `MaxConcurrentTasks`。
const HELPER_MAX_CONCURRENT_TASKS: i32 = 6;

/// 上游 `writeError` 的文本（逐字）。
const MSG_BODY_INVALID: &str = "invalid request body";
const MSG_WORKSPACE_REQUIRED: &str = "workspace_id is required";
const MSG_RUNTIME_REQUIRED: &str = "runtime_id is required";
const MSG_STARTER_PROMPT_TOO_LONG: &str = "starter_prompt exceeds 2048 characters";
const MSG_NOT_MEMBER: &str = "not a member of this workspace";
const MSG_RUNTIME_INVALID: &str = "invalid runtime_id";
const MSG_RUNTIME_PRIVATE: &str = "this runtime is private; only its owner can create agents on it";

/// `POST /api/me/onboarding/runtime-bootstrap` 的请求体（上游 `bootstrapOnboardingRuntimeRequest`）。
#[derive(Debug, Default, Deserialize)]
pub struct BootstrapOnboardingRuntimeRequest {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub runtime_id: String,
    #[serde(default)]
    pub starter_prompt: String,
}

/// `POST /api/me/onboarding/runtime-bootstrap` 的响应（上游 `bootstrapOnboardingRuntimeResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapOnboardingRuntimeResponse {
    pub workspace_id: String,
    pub agent_id: String,
    pub issue_id: String,
}

/// `POST /api/me/onboarding/no-runtime-bootstrap` 的请求体。
#[derive(Debug, Default, Deserialize)]
pub struct BootstrapOnboardingNoRuntimeRequest {
    #[serde(default)]
    pub workspace_id: String,
}

/// `POST /api/me/onboarding/no-runtime-bootstrap` 的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapOnboardingNoRuntimeResponse {
    pub workspace_id: String,
    pub issue_id: String,
}

/// `agent_runtime` 的绑定列（上游 `GetAgentRuntimeForWorkspace` 的返回值子集）。
#[derive(Debug, sqlx::FromRow)]
struct RuntimeRow {
    id: Uuid,
    runtime_mode: String,
    owner_id: Option<Uuid>,
    visibility: String,
}

/// `"user"` 上 shim 需要的两个读面（`before` 那一步）。
///
/// ⚠️ 上游同一个 `GetUser` 读的是**三**列（`onboarded_at` / `language` /
/// `starter_content_state`）；`onboarded_at` 在上游只喂 `firstCompletion` ⇒ 驱动
/// `OnboardingCompleted` **分析事件**，本仓不发该事件（见模块头的偏离登记 2）
/// ⇒ 这里不读它（读了就是 `dead_code`）。
#[derive(Debug, sqlx::FromRow)]
struct UserBeforeRow {
    language: Option<String>,
    starter_content_state: Option<String>,
}

/// DEPRECATED shim 切片：两条 provision 链（helper agent + starter issue）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/me/onboarding/runtime-bootstrap",
            post(runtime_bootstrap),
        )
        .route(
            "/api/me/onboarding/no-runtime-bootstrap",
            post(bootstrap_no_runtime),
        )
}

/// `POST /api/me/onboarding/runtime-bootstrap`（上游 `BootstrapOnboardingRuntime`）。
///
/// **单事务**：agent / issue / `onboarded_at` / `starter_content_state` 四步要么全成、要么全不成
/// （上游逐字「Single transaction.」）。中途任何 `?` 提前返回都会 drop 事务 ⇒ 回滚。
pub async fn runtime_bootstrap(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<BootstrapOnboardingRuntimeResponse>> {
    let request: BootstrapOnboardingRuntimeRequest = decode_bootstrap_body(&body)?;
    if request.workspace_id.is_empty() {
        return Err(bad_request(MSG_WORKSPACE_REQUIRED));
    }
    if request.runtime_id.is_empty() {
        return Err(bad_request(MSG_RUNTIME_REQUIRED));
    }
    let starter_prompt = request.starter_prompt.trim();
    if starter_prompt.chars().count() > MAX_STARTER_PROMPT_LEN {
        return Err(bad_request(MSG_STARTER_PROMPT_TOO_LONG));
    }
    let workspace_id = parse_id(&request.workspace_id, MSG_WORKSPACE_REQUIRED)?;
    let runtime_id = parse_id(&request.runtime_id, MSG_RUNTIME_INVALID)?;

    let mut tx = state.db.pool().begin().await.map_err(|e| db_err(&e))?;
    require_member(&mut tx, workspace_id, user.id()).await?;

    // runtime 必须属于本 workspace，且调用者能用它（上游 `canUseRuntimeForAgent`：
    // `public` 所有人可用，`private` 只有其 owner 可用；owner 为空的 runtime 谁都用不了）。
    let runtime: RuntimeRow = sqlx::query_as(
        "SELECT id, runtime_mode, owner_id, visibility FROM agent_runtime \
         WHERE id = $1 AND workspace_id = $2",
    )
    .bind(runtime_id.as_uuid())
    .bind(workspace_id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| db_err(&e))?
    .ok_or_else(|| bad_request(MSG_RUNTIME_INVALID))?;
    let usable = runtime
        .owner_id
        .is_some_and(|owner| owner == user.id().as_uuid() || runtime.visibility == "public");
    if !usable {
        return Err(forbidden(MSG_RUNTIME_PRIVATE));
    }

    let agent_id = find_or_create_helper(&mut tx, workspace_id, user.id(), &runtime).await?;
    let issue_id = find_or_create_issue(
        &mut tx,
        workspace_id,
        user.id(),
        ONBOARDING_ISSUE_TITLE,
        issue_body(starter_prompt),
        "agent",
        agent_id,
    )
    .await?;

    claim_and_complete(&mut tx, user.id()).await?;
    tx.commit().await.map_err(|e| db_err(&e))?;

    Ok(Json(BootstrapOnboardingRuntimeResponse {
        workspace_id: workspace_id.as_string(),
        agent_id: agent_id.as_string(),
        issue_id: issue_id.as_string(),
    }))
}

/// `POST /api/me/onboarding/no-runtime-bootstrap`（上游 `BootstrapOnboardingNoRuntime`）。
///
/// 上游逐字：「The user explicitly skipped the runtime step, so we unconditionally seed
/// regardless of any pre-existing runtime on the workspace.」⇒ 这里**没有** runtime 校验格，
/// 也**不**建 Helper agent。
pub async fn bootstrap_no_runtime(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<BootstrapOnboardingNoRuntimeResponse>> {
    let request: BootstrapOnboardingNoRuntimeRequest = decode_bootstrap_body(&body)?;
    if request.workspace_id.is_empty() {
        return Err(bad_request(MSG_WORKSPACE_REQUIRED));
    }
    let workspace_id = parse_id(&request.workspace_id, MSG_WORKSPACE_REQUIRED)?;

    let mut tx = state.db.pool().begin().await.map_err(|e| db_err(&e))?;
    require_member(&mut tx, workspace_id, user.id()).await?;

    // `userBefore` 既是「首次完成?」的判据，也是 EN/ZH 正文选择的判据（上游同一个 `GetUser`）。
    let before: UserBeforeRow =
        sqlx::query_as("SELECT language, starter_content_state FROM \"user\" WHERE id = $1")
            .bind(user.id().as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_err(&e))?
            .ok_or_else(|| ApiError(Error::Internal("failed to load user".into())))?;

    let issue_id = find_or_create_issue(
        &mut tx,
        workspace_id,
        user.id(),
        NO_RUNTIME_ISSUE_TITLE,
        no_runtime_issue_description(before.language.as_deref()),
        "member",
        user.id(),
    )
    .await?;

    mark_onboarded(&mut tx, user.id()).await?;
    claim_starter_content_state(&mut tx, user.id(), before.starter_content_state.as_deref())
        .await?;
    tx.commit().await.map_err(|e| db_err(&e))?;

    Ok(Json(BootstrapOnboardingNoRuntimeResponse {
        workspace_id: workspace_id.as_string(),
        issue_id: issue_id.as_string(),
    }))
}

// ---------------------------------------------------------------------------
// provision 链的四个零件（两条 shim 共用）
// ---------------------------------------------------------------------------

/// 上游 `GetMemberByUserAndWorkspace` 失败 ⇒ **403**（不是 404）。
async fn require_member(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Id,
    user_id: Id,
) -> ApiResult<()> {
    let found: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| db_err(&e))?;
    found.map(|_| ()).ok_or_else(|| forbidden(MSG_NOT_MEMBER))
}

/// 找或建「Multica Helper」agent（`name` ∧ `visibility = 'workspace'`，上游 `ListAgents` 后
/// 线性找第一个匹配项）。返回 `agent_id`，**不**关心它是不是这次新建的
/// （「建没建」只驱动分析事件，事件在本仓不发 —— 见模块头的偏离登记）。
async fn find_or_create_helper(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Id,
    user_id: Id,
    runtime: &RuntimeRow,
) -> ApiResult<Id> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM agent WHERE workspace_id = $1 AND name = $2 AND visibility = 'workspace' \
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(workspace_id.as_uuid())
    .bind(ONBOARDING_ASSISTANT_NAME)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| db_err(&e))?;
    if let Some((id,)) = existing {
        return Ok(Id::from(id));
    }
    let created: (Uuid,) = sqlx::query_as(
        "INSERT INTO agent (workspace_id, name, description, avatar_url, runtime_mode, \
             runtime_config, runtime_id, visibility, max_concurrent_tasks, owner_id, \
             instructions, custom_env, custom_args, mcp_config) \
         VALUES ($1, $2, $3, $4, $5, '{}'::jsonb, $6, 'workspace', $7, $8, $9, '{}'::jsonb, \
             '[]'::jsonb, NULL) RETURNING id",
    )
    .bind(workspace_id.as_uuid())
    .bind(ONBOARDING_ASSISTANT_NAME)
    .bind(ONBOARDING_ASSISTANT_DESCRIPTION)
    .bind(ONBOARDING_ASSISTANT_AVATAR_URL)
    .bind(&runtime.runtime_mode)
    .bind(runtime.id)
    .bind(HELPER_MAX_CONCURRENT_TASKS)
    .bind(user_id.as_uuid())
    .bind(ONBOARDING_ASSISTANT_INSTRUCTIONS)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| db_err(&e))?;
    Ok(Id::from(created.0))
}

/// 找或建那条 starter issue（上游 `issueguard.LockAndFindActiveDuplicate` 的本地等价：
/// `workspace_id` + `title` + 无 parent + 无 project + 非终态，`FOR UPDATE` 命中即复用）。
async fn find_or_create_issue(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Id,
    user_id: Id,
    title: &str,
    description: &str,
    assignee_type: &str,
    assignee_id: Id,
) -> ApiResult<Id> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM issue WHERE workspace_id = $1 AND title = $2 AND parent_issue_id IS NULL \
         AND project_id IS NULL \
         AND status NOT IN (SELECT key FROM issue_status \
                            WHERE workspace_id = $1 AND category = 'closed') \
         ORDER BY number, id LIMIT 1 FOR UPDATE",
    )
    .bind(workspace_id.as_uuid())
    .bind(title)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| db_err(&e))?;
    if let Some((id,)) = existing {
        return Ok(Id::from(id));
    }
    let next_number: (i32,) =
        sqlx::query_as("SELECT COALESCE(MAX(number), 0) + 1 FROM issue WHERE workspace_id = $1")
            .bind(workspace_id.as_uuid())
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| db_err(&e))?;
    let created: (Uuid,) = sqlx::query_as(
        "INSERT INTO issue (workspace_id, title, description, status, priority, assignee_type, \
             assignee_id, creator_type, creator_id, \"position\", number) \
         VALUES ($1, $2, $3, 'todo', 'high', $4, $5, 'member', $6, 0, $7) RETURNING id",
    )
    .bind(workspace_id.as_uuid())
    .bind(title)
    .bind(description)
    .bind(assignee_type)
    .bind(assignee_id.as_uuid())
    // 🔴 `creator_id` **恒**是调用者，`assignee_id` 可以是别的（Helper agent）——
    // 上游两条 shim 都是 `CreatorID: parseUUID(userID)`。
    .bind(user_id.as_uuid())
    .bind(next_number.0)
    .fetch_one(&mut **tx)
    .await
    .map_err(|e| db_err(&e))?;
    Ok(Id::from(created.0))
}

/// `runtime-bootstrap` 的 issue 正文：`starter_prompt`（**先 trim**）非空 ⇒ **整体替换**。
///
/// 上游逐字：`req.StarterPrompt = strings.TrimSpace(req.StarterPrompt)` 之后才判
/// `if req.StarterPrompt != ""` ⇒ 全空白等价于「没给」。
///
/// `pub(crate)` 的一半理由是让**不碰库**的那一半用例（`tests.rs`）能直接断言这一格纯函数。
pub(crate) fn issue_body(starter_prompt: &str) -> &str {
    let trimmed = starter_prompt.trim();
    if trimmed.is_empty() {
        ONBOARDING_ISSUE_DESCRIPTION
    } else {
        trimmed
    }
}

/// 上游 `MarkUserOnboarded`：`COALESCE` ⇒ 重复进入**保留第一次**的时间戳。
async fn mark_onboarded(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Id,
) -> ApiResult<()> {
    sqlx::query(
        "UPDATE \"user\" SET onboarded_at = COALESCE(onboarded_at, now()), \
                 updated_at = now() WHERE id = $1",
    )
    .bind(user_id.as_uuid())
    .execute(&mut **tx)
    .await
    .map_err(|e| db_err(&e))?;
    Ok(())
}

/// 上游 `claimStarterContentStateIfUnset`：`NULL → 'imported'`，已设则**短路不写**。
async fn claim_starter_content_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Id,
    current: Option<&str>,
) -> ApiResult<()> {
    if current.is_some() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE \"user\" SET starter_content_state = 'imported', updated_at = now() \
                 WHERE id = $1",
    )
    .bind(user_id.as_uuid())
    .execute(&mut **tx)
    .await
    .map_err(|e| db_err(&e))?;
    Ok(())
}

/// 读 `before` → 标记完成 → 认领 starter content（`runtime-bootstrap` 的那两个格子）。
async fn claim_and_complete(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Id,
) -> ApiResult<()> {
    let before: UserBeforeRow =
        sqlx::query_as("SELECT language, starter_content_state FROM \"user\" WHERE id = $1")
            .bind(user_id.as_uuid())
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| db_err(&e))?
            .ok_or_else(|| ApiError(Error::Internal("failed to load user".into())))?;
    mark_onboarded(tx, user_id).await?;
    claim_starter_content_state(tx, user_id, before.starter_content_state.as_deref()).await
}

// ---------------------------------------------------------------------------
// 请求体 / 错误小工具
// ---------------------------------------------------------------------------

/// 8 KiB 上限 + 反序列化（上游 `MaxBytesReader` + `json.Decode`；超限与非法体**同一条 400**）。
fn decode_bootstrap_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> ApiResult<T> {
    if body.len() > RUNTIME_BOOTSTRAP_BODY_LIMIT {
        return Err(bad_request(MSG_BODY_INVALID));
    }
    serde_json::from_slice(body).map_err(|_| bad_request(MSG_BODY_INVALID))
}

fn parse_id(raw: &str, message: &str) -> ApiResult<Id> {
    Id::parse(raw.trim()).map_err(|_| bad_request(message))
}

fn bad_request(message: &str) -> ApiError {
    ApiError(Error::Validation {
        message: message.to_string(),
        details: Vec::new(),
    })
}

fn forbidden(message: &str) -> ApiError {
    ApiError(Error::Forbidden {
        message: message.to_string(),
    })
}

fn db_err(err: &sqlx::Error) -> ApiError {
    ApiError(Error::Internal(err.to_string()))
}
