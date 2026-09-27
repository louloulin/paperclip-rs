//! `/api/quick-actions*`（目录 4 条）+ `/api/issues/:id/quick-actions/:qaId/{render,run}`（2 条）。
//!
//! M10-B3 / LUM-2114。上游 = `server/internal/handler/quick_action.go`（948 行），
//! 路由注册在 `server/cmd/server/router.go:1995-2027`。
//!
//! ## 形态（`docs/64` §4.2 的 B 面纪律，**本片最容易错的一条**）
//!
//! `m10-declared-routes.tsv` 只声明 M10 的 **A 面 5 条**（`dual-form required: 0`），
//! 本片 6 条里的 4 条属于 `M3+`、**不在**那张表里 ⇒ 形态必须自己从
//! `docs/fixtures/upstream-routes.tsv` 的 `M3+` 行复核：
//!
//! | 上游行 | 上游注册（`router.go:2021-2027`） | 本地必须注册 |
//! |---|---|---|
//! | `GET /api/quick-actions/` | `r.Route("/api/quick-actions")` + `r.Get("/")` | `/api/quick-actions` **和** `/api/quick-actions/` |
//! | `POST /api/quick-actions/` | 同上 `r.Post("/")` | 同上 |
//! | `PATCH /api/quick-actions/{id}/` | `r.Route("/{id}")` + `r.Patch("/")` | `/api/quick-actions/:id` **和** `/api/quick-actions/:id/` |
//! | `DELETE /api/quick-actions/{id}/` | 同上 `r.Delete("/")` | 同上 |
//! | `POST /api/issues/{id}/quick-actions/{quickActionId}/render` | plain `r.Post` | **只**无尾斜杠 |
//! | `POST /api/issues/{id}/quick-actions/{quickActionId}/run` | plain `r.Post` | **只**无尾斜杠 |
//!
//! 判据是 `scripts/slash_alias_audit.py`：fixture 路径**带**尾斜杠 ⇒ 两种形态**都要**
//! （少一个 = `MISSING_ALIAS`）；fixture 路径**不带** ⇒ 只能一种（多补 = `EXTRA_ALIAS`）。
//! 先例 = `routes/properties.rs:151-171`（同一批 `router.go` 邻近的 catalog 面）。
//!
//! ## 中间件归属（逐字复核，**不是**照抄 M9-1/M9-2）
//!
//! 上游 6 条**全部**挂在 `router.go:1948` 的 `r.Group` 里，那一组的 `r.Use` 只有
//! `middleware.RequireWorkspaceMember(queries)`（`router.go:1949`）⇒ **本片不挂**
//! `RequireHumanActor`。反向用例见 `tests.rs::machine_credentials_are_not_gated`：
//! 「不挂」要用**反向**判据钉住，只写注释不算。
//!
//! ## 权限模型（照上游，四条规则）
//!
//! 1. **目录读不做权限工作**：`private` 行按 `created_by_id = viewer` 在 SQL 里过滤；
//!    能不能**跑**由 [`QuickActionRepo::can_invoke`] 回答（上游文件头的
//!    "PERMISSION IS CHECKED IN EXACTLY ONE PLACE"）。
//! 2. **管理面**（建 / 改 / 删）：任何 workspace 成员都能建**私有**动作；
//!    **`public`** 动作要 owner/admin（`require_workspace_admin`）——
//!    改一个**已经**是 public 的动作同样要那个角色（否则成员能改掉一个全 workspace
//!    按钮背后的 prompt）。
//! 3. **可达性**（[`load_reachable`]）：`private` 且非创建者 ⇒ **404 而不是 403**
//!    （「这个 id 存不存在」本身就不是调用者该知道的）。
//! 4. **`visibility` 是作者的心意，不是授权判定**：run 时那道闸有最终发言权。
//!
//! ## 已知偏差（完整清单见 `docs/32` §9.22）
//!
//! - 上游对 `actor == "agent"` 的管理请求 403 `agents cannot manage quick actions`：
//!   本仓 mc-http 的请求上下文只有 `X-Multica-User-Id`（无 agent 身份）⇒ 不可实现；
//! - `run` **不**触发任务：本仓尚无 comment → mention → task 的触发链
//!   （`routes/comments/mod.rs` 模块头把「@agent 触发」列为别的切片），故本片
//!   只落评论 + 计数，不伪造 `trigger_outcomes`；
//! - `run` **不**做 realtime 广播与 issue revision 回写（同样属于那条触发链）。

use std::sync::Arc;

use axum::body::Bytes;
use axum::routing::{get, patch, post};
use axum::Router;
use mc_core::Id;
use mc_errors::Error;
pub use mc_repos::quick_action::{
    QuickActionRepo, QuickActionRow, QuickActionTarget, MAX_ACTIVE_PER_WORKSPACE,
    MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_PROMPT_LEN,
};
use serde::{Deserialize, Serialize};

use crate::routes::invitations::not_found;
pub(crate) use crate::routes::invitations::{require_workspace_admin, require_workspace_member};
use crate::routes::issues::{resolve_workspace, WorkspaceQuery};
use crate::state::AppState;

pub mod invoke;
pub mod lifecycle;
pub mod list;

/// 目录面 + issue 侧 6 条（两形态合计 **8 个注册点**，6 条上游键）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        // 目录读 / 建：上游 `r.Route("/api/quick-actions")` + `r.Get("/")` / `r.Post("/")`
        // ⇒ chi 的 Mount 服务**两种**写法（`slash_alias_audit.py`: fixture 带尾斜杠 ⇒
        // 两形态都得注册）。两个 `.route()` 各写各的 handler 调用 —— 共用一个
        // `MethodRouter` 变量会让门 ⑦ 的抽取器**看不见**这个键（`docs/37` §15.6 实测）。
        .route(
            "/api/quick-actions",
            get(list::list_actions).post(list::create_action),
        )
        .route(
            "/api/quick-actions/",
            get(list::list_actions).post(list::create_action),
        )
        // PATCH / DELETE：上游 `r.Route("/{id}")` + `r.Patch("/")` / `r.Delete("/")`，同款两形态。
        .route(
            "/api/quick-actions/:id",
            patch(lifecycle::update_action).delete(lifecycle::delete_action),
        )
        .route(
            "/api/quick-actions/:id/",
            patch(lifecycle::update_action).delete(lifecycle::delete_action),
        )
        // issue 侧两条是 **plain** 注册（`router.go:1995-1996`）⇒ 只注册无尾斜杠。
        .route(
            "/api/issues/:id/quick-actions/:quickActionId/render",
            post(invoke::render_action),
        )
        .route(
            "/api/issues/:id/quick-actions/:quickActionId/run",
            post(invoke::run_action),
        )
}

// ---------------------------------------------------------------------------
// 请求体 / 响应
// ---------------------------------------------------------------------------

/// `GET /api/quick-actions` 的查询（workspace 选择器 + `?include_archived`）。
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub include_archived: Option<String>,
    #[serde(flatten)]
    pub workspace: WorkspaceQuery,
}

/// `POST /api/quick-actions`（上游 `CreateQuickActionRequest`）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CreateRequest {
    pub name: String,
    pub description: String,
    pub assignee_type: String,
    pub assignee_id: String,
    pub prompt: String,
    pub visibility: String,
}

/// `PATCH /api/quick-actions/:id`（上游 `UpdateQuickActionRequest`；`*T` 三态 ⇒ 缺失 = 不动）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PatchRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub assignee_type: Option<String>,
    pub assignee_id: Option<String>,
    pub prompt: Option<String>,
    pub visibility: Option<String>,
    pub status: Option<String>,
}

/// quick action 响应（上游 `QuickActionResponse`）。
///
/// ⚠️ `target_name` 带 `omitempty`（上游逐字如此）：目标解析不出来时它整个消失，
/// 而 `target_public` / `target_missing` **恒**出现（后两者没有 `omitempty`）。
#[derive(Debug, Clone, Serialize)]
pub struct ActionResponse {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub assignee_type: String,
    pub assignee_id: String,
    pub prompt: String,
    pub visibility: String,
    pub status: String,
    pub last_used_at: Option<String>,
    pub use_count: i64,
    pub created_by_id: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    pub target_public: bool,
    pub target_missing: bool,
}

impl ActionResponse {
    /// 行 + 已解析（或未解析）的目标 ⇒ 响应（上游 `quickActionToResponse*` 两支的合一）。
    pub fn new(row: &QuickActionRow, target: Option<&QuickActionTarget>) -> Self {
        let (target_name, target_public) = match target {
            Some(t) => (Some(t.name.clone()), t.invocable_by_everyone),
            None => (None, false),
        };
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            name: row.name.clone(),
            description: row.description.clone(),
            assignee_type: row.assignee_type.clone(),
            assignee_id: row.assignee_id.to_string(),
            prompt: row.prompt.clone(),
            visibility: row.visibility.clone(),
            status: row.status.clone(),
            last_used_at: row.last_used_at.map(|at| at.to_rfc3339()),
            use_count: row.use_count,
            created_by_id: row.created_by_id.to_string(),
            created_at: row.created_at.to_rfc3339(),
            updated_at: row.updated_at.to_rfc3339(),
            target_name,
            target_public,
            target_missing: target.is_none(),
        }
    }
}

/// `GET /api/quick-actions` 的响应（上游 `ListQuickActionsResponse`）。
#[derive(Debug, Serialize)]
pub struct ListResponse {
    pub quick_actions: Vec<ActionResponse>,
}

// ---------------------------------------------------------------------------
// 校验（逐字搬上游的四个 validator）
// ---------------------------------------------------------------------------

/// `validateQuickActionName`：trim 后非空 + ≤ 32 个**字符**（上游按 rune 数）。
pub fn validate_name(raw: &str) -> Result<String, Error> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(validation("name is required"));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(validation(format!(
            "name must be at most {MAX_NAME_LEN} characters"
        )));
    }
    Ok(name.to_string())
}

/// `validateQuickActionPrompt`：trim + ≤ 4000 字符 + **拒绝 `{{...}}`** +
/// **拒绝会点到人的 mention**。
///
/// 后两条是**故意的**（上游文件头）：模板变量会让 `{{issue.title}}` 原样落进 agent 的
/// 指令；而 prompt 是被**逐字**追加到一条要走 mention 管线的评论上，所以里面的
/// `mention://agent|squad|member|all/` 会让**每次点击**多叫一个人。
/// `mention://issue/...` 是唯一例外（渲染成链接、谁也碰不到）。
pub fn validate_prompt(raw: &str) -> Result<String, Error> {
    let prompt = raw.trim();
    if prompt.is_empty() {
        return Err(validation("prompt is required"));
    }
    if prompt.chars().count() > MAX_PROMPT_LEN {
        return Err(validation(format!(
            "prompt must be at most {MAX_PROMPT_LEN} characters"
        )));
    }
    if let Some(token) = find_template_token(prompt) {
        return Err(validation(format!(
            "template variables are not supported yet; remove {token} — the agent already reads this issue"
        )));
    }
    if has_side_effect_mention(prompt) {
        return Err(validation(
            "the prompt cannot @mention an agent, squad, or person; a quick action reaches exactly \
             the one target it is bound to (an issue link is fine)",
        ));
    }
    Ok(prompt.to_string())
}

/// 上游 `quickActionTemplateTokenRe` = `\{\{[^}]*\}\}`（手写扫描，不引 regex 依赖：
/// `[^}]*` 不跨 `}`，所以**第一个** `{{` 与它之后**第一对** `}}` 之间就是整个匹配 ——
/// 只写一个 `}` **不算**模板）。
pub fn find_template_token(prompt: &str) -> Option<String> {
    let start = prompt.find("{{")?;
    let end = prompt[start + 2..].find("}}")? + start + 4;
    Some(prompt[start..end].to_string())
}

/// 上游 `quickActionSideEffectMentionRe` = `mention://(agent|squad|member|all)/`。
pub fn has_side_effect_mention(prompt: &str) -> bool {
    for kind in ["agent", "squad", "member", "all"] {
        if prompt.contains(&format!("mention://{kind}/")) {
            return true;
        }
    }
    false
}

/// `validateQuickActionAssignee`：类型只允许 `agent` / `squad`，id 非空。
pub fn validate_assignee(assignee_type: &str, assignee_id: &str) -> Result<(), Error> {
    if assignee_type != "agent" && assignee_type != "squad" {
        return Err(validation("assignee_type must be \"agent\" or \"squad\""));
    }
    if assignee_id.trim().is_empty() {
        return Err(validation("assignee_id is required"));
    }
    Ok(())
}

/// `normalizeQuickActionVisibility`：空 ⇒ `public`；只允许 `public` / `private`。
pub fn normalize_visibility(raw: &str) -> Result<String, Error> {
    let v = raw.trim();
    if v.is_empty() {
        return Ok("public".to_string());
    }
    if v != "public" && v != "private" {
        return Err(validation("visibility must be \"public\" or \"private\""));
    }
    Ok(v.to_string())
}

/// `trimmedWithinLimit`（上游）：trim + rune 上限。
pub fn trimmed_within_limit(raw: &str, limit: usize, field: &str) -> Result<String, Error> {
    let v = raw.trim();
    if v.chars().count() > limit {
        return Err(validation(format!(
            "{field} must be at most {limit} characters"
        )));
    }
    Ok(v.to_string())
}

// ---------------------------------------------------------------------------
// 门 / 装载
// ---------------------------------------------------------------------------

/// 本模块的 repo 构造。
pub fn repo(state: &AppState) -> QuickActionRepo {
    QuickActionRepo::new(state.db.clone())
}

/// 管理面门（上游 `requireQuickActionActor` 去掉 agent 那一支后的等价物）。
///
/// 顺序刻意照上游：**成员门在前**（非成员 404），角色门**不在**这里 —— 它取决于
/// 即将写入的 `visibility`（`public` 要 owner/admin，`private` 谁都行）。
pub async fn require_manage_actor(
    state: &AppState,
    workspace_id: Id,
    user: Id,
) -> Result<(), Error> {
    require_workspace_member(state, workspace_id, user).await
}

/// 写一个**仍然是 `public`** 的动作要的角色（上游 `requirePublicQuickActionRole`）。
pub async fn require_public_role(
    state: &AppState,
    workspace_id: Id,
    user: Id,
) -> Result<(), Error> {
    require_workspace_admin(state, workspace_id, user).await
}

/// 装载一条**调用者够得着**的动作（上游 `loadReachableQuickAction`）。
///
/// 规则只有一条，但它是**唯一**的一条：`private` 且不是创建者 ⇒ **404**（不是 403）。
/// 把这条收在一个函数里，是为了让新端点不可能因为「忘了写」而漏掉它。
pub async fn load_reachable(
    state: &AppState,
    workspace_id: Id,
    id: Id,
    user: Id,
) -> Result<QuickActionRow, Error> {
    let row = repo(state).get(workspace_id, id).await.map_err(repo_err)?;
    if row.is_private() && row.created_by() != user {
        return Err(not_found("quick action"));
    }
    Ok(row)
}

/// 活跃数到顶 ⇒ 400（消息逐字对齐上游）。
pub fn active_cap_error() -> Error {
    validation(format!(
        "a workspace can have at most {MAX_ACTIVE_PER_WORKSPACE} active quick actions; \
         archive one first"
    ))
}

/// `RepoError` → HTTP（未命中 404 `quick action`）。
pub fn repo_err(err: mc_repos::RepoError) -> Error {
    match err {
        mc_repos::RepoError::NotFound => not_found("quick action"),
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: "quick action was modified concurrently; refetch and retry".into(),
        },
        mc_repos::RepoError::Db(message) => Error::Database(message),
    }
}

/// 400 的构造（与本仓既有切片同款：`Error::Validation` ⇒ 400）。
pub fn validation(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: Vec::new(),
    }
}

/// body 解码（上游 `json.NewDecoder` 失败 ⇒ 400 `invalid request body`）。
pub fn parse_body<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, Error> {
    serde_json::from_slice::<T>(body).map_err(|_| validation("invalid request body"))
}

/// `:id` / `:quickActionId` 的 uuid 解析（上游 `parseUUIDOrBadRequest`）。
pub fn parse_id(field: &str, raw: &str) -> Result<Id, Error> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("{field} must be a uuid")))
}

/// `?include_archived` 是**字面量**比较：只有 `"true"` 算真（上游逐字）。
pub fn include_archived(raw: Option<&String>) -> bool {
    raw.is_some_and(|v| v == "true")
}

/// `buildQuickActionBody`：把 mention 行**前置**到 prompt 前面。
///
/// 渲染出来的文本要走**和手打 @mention 完全一样**的那条 mention 解析器 ⇒ 下游
/// 分不出这是 quick action。prompt **逐字**透传（没有插值；见模块头）。
pub fn build_body(row: &QuickActionRow, target: &QuickActionTarget) -> String {
    format!(
        "[@{}](mention://{}/{})",
        target.name, target.mention_type, target.mention_id
    ) + "\n\n"
        + &row.prompt
}

/// 目标缺失时 run / render 的 409（上游 `writeDispatchBlocked(ReasonTargetUnavailable)`）。
///
/// 上游的 `dispatch/reason.go` 给的是结构化 dispatch 错误；本仓没有那一层 ⇒ 用
/// `Error::Conflict` 带同款措辞（登记在 `docs/32` §9.22 的偏差清单里）。
pub fn target_unavailable() -> Error {
    Error::Conflict {
        message: "the quick action target is unavailable (archived or deleted)".into(),
    }
}

/// invoke 被拒时的 403（上游 `ReasonInvocationNotAllowed`）。
pub fn invocation_not_allowed() -> Error {
    Error::Forbidden {
        message: "you are not allowed to invoke this target".into(),
    }
}

/// 归档动作被 run ⇒ 400（上游 `quick action is archived`，逐字）。
pub fn archived_error() -> Error {
    validation("quick action is archived")
}

/// triage 中的 issue 被 run ⇒ 403（上游 `ReasonIssueInTriage`）。
///
/// 产品理由而非规则理由：quick action 是「去把这件事做掉」的指令，不是邀请讨论，
/// 而 triage 正是「还没人同意该做」的状态。上游在**写评论之前**拒（否则会留下一条
/// 写给谁的指令）。
pub fn triage_blocked() -> Error {
    Error::Forbidden {
        message: "this issue is in triage; quick actions cannot be run yet".into(),
    }
}

/// 供子模块复用的 workspace 解析（收敛一处，免得三处各写一遍 selector）。
pub async fn workspace_of(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    query: &WorkspaceQuery,
) -> Result<Id, Error> {
    resolve_workspace(state, headers, query).await
}

#[cfg(test)]
mod tests;
