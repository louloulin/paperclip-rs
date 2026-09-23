//! M5-0 anchor：autopilot **写面**（create / update / delete）—— M5-2 实现。
//!
//! - **写者**：M5-2（`docs/44` §3.2）。切片只实现本文件的 `router()`。
//! - **路由**（`router.go` L2105–L2107）：
//!
//! | # | 方法 | 路径 | 上游 handler | span |
//! | ---: | --- | --- | --- | ---: |
//! | 5 | POST | `/api/autopilots/`（+ 无斜杠别名） | `CreateAutopilot` | 159 |
//! | 6 | PATCH | `/api/autopilots/:id/`（+ 无斜杠别名） | `UpdateAutopilot` | 260 |
//! | 7 | DELETE | `/api/autopilots/:id/`（+ 无斜杠别名） | `DeleteAutopilot` | 64 |
//!
//! - **三态补丁**：#6 的 `UpdateAutopilot`260 是「缺失 / `null` / 有值」三态大户。
//! - **字段口径**：`autopilot` 表**没有** `priority`（`058` DROP）与 `concurrency_policy`
//!   （`043` DROP）——旧桩与计划里列过它们，是错的（见 `mc_core::autopilot` 的「旧 stub 错在哪」表）。
//! - **规则版本**：只有 `autopilotRuleSubstantiveChange`13 判为实质变更才 append
//!   `autopilot_rule_version`（`186`，append-only）；不是每次 PATCH 都写。
//! - **assignee 校验**在 `../assignee.rs`（M5-2），**权限**在 `../access.rs`（M5-1）。
//! - **门 ⑩ 预判**（§6.3）：本文件 + 拆出的 `subscribers.rs`(183) / `assignee.rs`(135)
//!   三者合计约 650 行 ⇒ **anchor 已拆好**，切片不要再往本文件堆协作者/订阅者逻辑。
//!
//! # M5-2 落地了什么
//!
//! 三条路由 6 个注册键，逐字对照上游 `CreateAutopilot` / `UpdateAutopilot` / `DeleteAutopilot`。
//! 几个**不能抄漏的怪癖**（都在代码里带注释）：
//!
//! | 怪癖 | 为什么 |
//! | --- | --- |
//! | 三态补丁用 **`rawFields` + `Option<T>`**，不是 `Option<Option<T>>` | 上游用的就是 `map[string]json.RawMessage` 判「键在不在」，指针判「是不是 null」；两份信息合起来正好是 `Option<Option<T>>`，但**分开更贴上游**（`assignee_type: null` 与 `assignee_type: ""` 在上游是两条不同分支） |
//! | `description` 的 `rawFields` 分支**不实现** | 该列是 `COALESCE($3, description)`：显式 `null` 与「不传」在 SQL 里都保持原值 ⇒ 那条分支**不可观测**，写出来只是死代码 |
//! | `issue_title_template` / `project_id` 是**直赋值**（`$8` / `$9`，不 COALESCE） | 不传时 handler 必须**回填 `prev`**，否则一次 PATCH 就把它们清空 |
//! | Update **不校验** `title` / `status` / `execution_mode` 的取值 | 上游只在 Create 校验 `execution_mode` 白名单（Update 传 `"whatever"` 会撞 DB CHECK → 500），`status` 同理，`title` 允许空串 |
//! | `subscribers: []` 是**断言**（整表替换），`null` / 缺失 = 保持 | 与读面的 fail-closed 500 同源（MUL-6680） |
//! | 403 / 409 是**扁平**体 | 上游用 `writeErrorCode`（稳定码），见 `../assignee.rs` 的判定规则 |
//!
//! ## 顺序（两次「先校验还是先落库」的对照，别顺手统一）
//!
//! ```text
//! Create: body → 字段校验 → 成员门槛 → assignee/project/subscribers 解析 → 事务
//! Update: 成员门槛 → 加载 → **写权** → body → patch 组装 → 事务
//! Delete: 成员门槛 → 加载 → 写权 → 事务（归档 + 规则版本）
//! ```
//!
//! Create 先解析 body 是因为「畸形 payload 不该开事务」（上游注释原文），而 Update 的写权检查
//! 排在 body **之前** ⇒ 对无权者，畸形 body 也回 403（不是 400）。这是上游行为，不是疏漏。
//!
//! ## 事务的四段锁序（与 `mc_repos::autopilot::write` 的锁序表同源）
//!
//! ① `(workspace,user)` advisory 锁（每个订阅者，**全程按 UUID 规范串升序**）→
//! ② `FOR SHARE` 重申成员 → ③ assignee 的 agent/squad 行锁 → ④ autopilot 行 `FOR UPDATE`
//! → ⑤ 各写。①→② 的顺序是全仓统一（成员撤销走同一把 advisory 锁），③ 早于 ④ 是为了不与
//! runtime teardown / squad 改队长（同样是先 Agent/Squad 后 Autopilot）互相死锁。
//!
//! **乐观并发**（§4.2）：事务外加载的 `prev.updated_at` 与 `FOR UPDATE` 后读到的 `updated_at`
//! 不一致 ⇒ 409 `autopilot_update_conflict`（扁平体）。它必须排在**写之前**、锁之后。
//!
//! ## 无实时事件（跨片缺口）
//!
//! 上游在这三条路径上都会 `h.publish(...)`（`EventAutopilotCreated/Updated/Deleted`）。
//! 本仓 M4 的 projects 写面同样没有接 realtime 平面（`projects/crud.rs`），M5-2 沿用
//! 「先不 publish」的口径，缺口记入 `docs/47-M5-2-WRITE-FACE.md` §5；等 realtime 平面统一接。

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use mc_autopilot::collaborator::{
    ordered_for_locking, parse_subscribers, SubscriberInput, ASSIGNEE_TYPE_AGENT,
};
use mc_autopilot::write::{
    is_valid_execution_mode, rule_config_summary, substantive_change, validate_issue_title_template,
};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::agent::AgentRepo;
use mc_repos::autopilot::write::{
    add_subscriber, archive, delete_subscribers_for_autopilot, get_project_in_workspace,
    insert_rule_version, lock_active_member, lock_autopilot_for_update, lock_subscriber_writes,
    set_trigger_publishers_by_autopilot, AutopilotWriteRepo, NewAutopilot, UpdateAutopilot,
    PUBLISHED_BY_MEMBER, STATUS_ACTIVE, STATUS_ARCHIVED,
};
use mc_repos::autopilot::AutopilotRepo;
use mc_repos::RepoError;

use crate::error::{ApiError, ApiResult};
use crate::routes::agents::{bad_request, not_found, parse_uuid, repo_err, AgentScope};
use crate::routes::auth_user::AuthUser;
use crate::routes::autopilots::access::{
    acting_user_id, load_in_workspace, member_can_write, require_member,
};
use crate::routes::autopilots::assignee::{
    forbidden_write, is_valid_assignee_type, update_conflict, validate_assignee_for_save,
};
use crate::routes::autopilots::dto::{
    autopilot_to_response, subscriber_entry, AutopilotSubscriberEntry,
};
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

/// 写面 router（3 条路由 / 6 个注册键）。
///
/// #5/#6/#7 都是**双形态**（上游 `Route("/api/autopilots") + Post("/")` 与
/// `r.Patch("/{id}/")`）：axum 不做归一化，少注册一个形态就是 404（不是 307），
/// 且 `slash_alias_audit.py` 会判 `MISSING_ALIAS`。方法集合必须逐字相同。
///
/// 与 `list.rs` 的 4 条读路由**共用同一批 path**（`/api/autopilots[/]`、`/api/autopilots/:id[/]`）：
/// `Router::merge` 允许「同 path 不同 method」合并，但方法**重叠**会 panic ⇒ 这里只能注册
/// `post` / `patch` / `delete`，绝不能顺手补一个 `get`。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/autopilots", post(create_autopilot))
        .route("/api/autopilots/", post(create_autopilot))
        .route(
            "/api/autopilots/:id",
            patch(update_autopilot).delete(delete_autopilot),
        )
        .route(
            "/api/autopilots/:id/",
            patch(update_autopilot).delete(delete_autopilot),
        )
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// 上游 `CreateAutopilotRequest`（`handler/autopilot.go` @359）。
///
/// `title` / `assignee_id` / `execution_mode` 在上游是**非指针** `string` ⇒ 缺失与 `null` 都折成
/// `""`，落到「xxx is required」。本地用 `String` + `#[serde(default)]` 复刻这一点。
#[derive(Debug, Clone, Default, Deserialize)]
struct CreateAutopilotRequest {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    assignee_type: Option<String>,
    #[serde(default)]
    assignee_id: String,
    #[serde(default)]
    execution_mode: String,
    #[serde(default)]
    issue_title_template: Option<String>,
    #[serde(default)]
    subscribers: Option<Vec<SubscriberInput>>,
}

/// 上游 `UpdateAutopilotRequest`（@371）：**全部可选**，缺省 = 不改。
///
/// 「键在不在」另由 [`sent`] 从原始 JSON 判断（上游 `rawFields`）——`Option` 分不清
/// 「缺失」与「显式 null」，而这在 `assignee_id` 上是两条不同分支（后者 400）。
#[derive(Debug, Clone, Default, Deserialize)]
struct UpdateAutopilotRequest {
    #[serde(default)]
    title: Option<String>,
    /// 解析出来但**故意不消费**（见模块文档：该列是 `COALESCE`，显式 `null` 与不传等价，
    /// 上游那条 `rawFields["description"]` 分支不可观测）。留着是为了让结构体与上游请求形状一致。
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    assignee_type: Option<String>,
    #[serde(default)]
    assignee_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    execution_mode: Option<String>,
    #[serde(default)]
    issue_title_template: Option<String>,
    #[serde(default)]
    subscribers: Option<Vec<SubscriberInput>>,
}

/// 解析请求体：非法 JSON ⇒ 400 `invalid request body`（上游 `json.Decode` 的文案）。
///
/// `pub(crate)`：`subscribers.rs`（同片写面）复用同一份「Go 风格 body 解析」，
/// 避免同一个语义在两个文件里各写一遍。
pub(crate) fn parse_json_body(body: &Bytes) -> Result<Value, Error> {
    serde_json::from_slice::<Value>(body).map_err(|_| bad_request("invalid request body"))
}

/// `Value` → 请求结构体。
///
/// `null` 折成 `Default`：Go 的 `json.Unmarshal([]byte("null"), &req)` **不报错**，留下零值请求体
/// （Create 随后回 `title is required`，Update 变成一次「什么都不改」的 200 空补丁）。
/// `serde_json` 对 `null` 解结构体会直接报错，所以这里显式补上这一格。
/// `pub(crate)`：同片写面（`subscribers.rs`）复用。
pub(crate) fn decode_body<T: Default + serde::de::DeserializeOwned>(
    raw: &Value,
) -> Result<T, Error> {
    if raw.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value::<T>(raw.clone()).map_err(|_| bad_request("invalid request body"))
}

/// 上游 `rawFields`：这个键**在请求体里出现过**吗（哪怕是 `null`）。
fn sent(raw: &Value, field: &str) -> bool {
    raw.get(field).is_some()
}

// ---------------------------------------------------------------------------
// 共享小工具
// ---------------------------------------------------------------------------

/// 500（上游 `writeError(w, 500, …)`；文案逐字，用于诊断是哪一步炸的）。
///
/// `pub(crate)`：同片写面（`subscribers.rs`）复用，保证 500 的形状与文案风格一致。
/// 返回裸 `Error` 供**内部工具函数**（`Result<_, Error>`）使用；handler 里用 [`failed`]。
pub(crate) fn internal(message: &str) -> Error {
    Error::Internal(message.to_string())
}

/// handler 里的 500：`ApiResult` 的错误类型是 `ApiError`，这里一步到位。
///
/// 为什么不做成 `internal(..).into()`：`.map_err(|_| …into())` 的闭包返回类型**无法推断**
/// （`From<Error>` 有两个候选 `impl`）⇒ 闭包里必须写具名构造。
fn failed(message: &str) -> ApiError {
    ApiError(internal(message))
}

/// 组装 [`AgentScope`]（`../assignee.rs` 的 invoke 门要它在 `targets_of` / `member_hits_targets`）。
///
/// **不要**改调 `AgentScope::resolve`：那会再查一次 `member` 表，产生第二份成员真值 ——
/// 本片已经在 handler 开头用 `require_member`（全仓共用那份）拿过 `role` 了。
fn agent_scope(state: &AppState, workspace_id: Id, user_id: Id, role: &str) -> AgentScope {
    AgentScope {
        workspace_id,
        user_id,
        role: role.to_string(),
        repo: AgentRepo::new(state.db.clone()),
    }
}

/// 上游 `parseAutopilotProjectID`23：`nil` / 空串 ⇒ `NULL`（合法）；非法 UUID 或跨工作区 ⇒ 400。
async fn resolve_project_id(
    state: &AppState,
    raw: Option<&str>,
    workspace_id: Id,
) -> Result<Option<Uuid>, Error> {
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let project_id = parse_uuid(raw, "project_id")?;
    let exists = get_project_in_workspace(state.db.pool(), project_id, workspace_id.0)
        .await
        .map_err(|_| internal("failed to validate project_id"))?;
    if !exists {
        return Err(bad_request(
            "project_id must reference a project in this workspace",
        ));
    }
    Ok(Some(project_id))
}

/// 上游 `lockAndValidateAutopilotSubscribers`39：advisory 锁全家 → `FOR SHARE` 重申成员。
///
/// 两条腿的顺序**不能合并成一个循环**：先把所有身份锁拿满再逐个重申成员，才能保证
/// 「同一批订阅者的不同请求顺序」不会互相死锁（上游注释原文）。
async fn lock_and_validate_subscribers(
    conn: &mut sqlx::PgConnection,
    workspace_id: Uuid,
    candidates: &[mc_autopilot::collaborator::SubscriberCandidate],
) -> Result<(), Error> {
    let ordered = ordered_for_locking(candidates);
    for candidate in &ordered {
        lock_subscriber_writes(conn, workspace_id, candidate.user_id)
            .await
            .map_err(|_| internal("failed to validate autopilot subscribers"))?;
    }
    for candidate in &ordered {
        let is_member = lock_active_member(conn, workspace_id, candidate.user_id)
            .await
            .map_err(|_| internal("failed to validate autopilot subscribers"))?;
        if !is_member {
            return Err(bad_request(format!(
                "subscribers[{}] is not a member of this workspace",
                candidate.input_index
            )));
        }
    }
    Ok(())
}

/// 提交后重读订阅者（上游 `ListAutopilotSubscribers`，失败则 `subs = nil`）。
///
/// 上游那条失败路径会把 `subscribers` 序列化成 `null`（Go 的 nil slice），本地恒回 `[]` ——
/// 「本字段恒为数组」是本仓写面更强的口径（MUL-6680 的教训），且该分支在一次刚提交的
/// 同连接查询里不可达。记入 `docs/47` §5。
async fn reload_subscribers(
    repo: &AutopilotRepo,
    autopilot_id: Uuid,
) -> Vec<AutopilotSubscriberEntry> {
    repo.list_subscribers(autopilot_id)
        .await
        .unwrap_or_default()
        .iter()
        .map(subscriber_entry)
        .collect()
}

// ---------------------------------------------------------------------------
// #5 POST /api/autopilots[/]
// ---------------------------------------------------------------------------

/// `POST /api/autopilots[/]`（上游 `CreateAutopilot` 159 行）。
///
/// 校验顺序逐字：body → `title` → `assignee_id` → `execution_mode`（有无 + 白名单）→
/// `issue_title_template` → **成员门槛** → `assignee_id` UUID → `assignee_type`（缺省 `agent` +
/// 白名单）→ `project_id` → `subscribers` → 事务。
#[allow(clippy::too_many_lines)] // 上游 159 行；拆开会让「校验顺序」这条契约更难对照
async fn create_autopilot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Response> {
    let raw = parse_json_body(&body)?;
    let req: CreateAutopilotRequest = decode_body(&raw)?;

    // ①-④ 字段门槛（文案逐字，含 `execution_mode` 的**两种**错误）。
    if req.title.is_empty() {
        return Err(bad_request("title is required").into());
    }
    if req.assignee_id.is_empty() {
        return Err(bad_request("assignee_id is required").into());
    }
    if req.execution_mode.is_empty() {
        return Err(bad_request("execution_mode is required").into());
    }
    if !is_valid_execution_mode(&req.execution_mode) {
        return Err(bad_request("execution_mode must be create_issue or run_only").into());
    }
    if let Some(template) = req.issue_title_template.as_deref() {
        // `{{var}}` 只认 `date`；空模板合法（上游 `ValidateIssueTitleTemplate`）。
        validate_issue_title_template(template).map_err(|err| bad_request(err.to_string()))?;
    }

    // ⑤ 成员门槛（上游是路由组中间件）：非成员 ⇒ 404 workspace。
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    let role = require_member(&state, workspace_id, user_id).await?;

    // ⑥ assignee_id 必须是 UUID；⑦ assignee_type 缺省 `agent` + 二态白名单。
    let assignee_id = parse_uuid(&req.assignee_id, "assignee_id")?;
    let assignee_type = req
        .assignee_type
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(ASSIGNEE_TYPE_AGENT)
        .to_string();
    if !is_valid_assignee_type(&assignee_type) {
        return Err(bad_request("assignee_type must be agent or squad").into());
    }

    // ⑧ project_id（非事务读）→ ⑨ subscribers 解析（都在开事务之前）。
    let project_id = resolve_project_id(&state, req.project_id.as_deref(), workspace_id).await?;
    let candidates = parse_subscribers(&req.subscribers.unwrap_or_default())
        .map_err(|err| bad_request(err.to_string()))?;

    let write_repo = AutopilotWriteRepo::new(state.db.clone());
    let mut tx = write_repo
        .begin()
        .await
        .map_err(|_| failed("failed to create autopilot"))?;
    let conn = &mut *tx;

    // 锁序 ①②③：订阅者身份 → 成员重申 → assignee 行（`requireRuntime = true`：新建即 active）。
    lock_and_validate_subscribers(conn, workspace_id.0, &candidates).await?;
    let scope = agent_scope(&state, workspace_id, user_id, &role);
    validate_assignee_for_save(conn, &scope, &assignee_type, assignee_id, true).await?;

    let new = NewAutopilot {
        workspace_id: workspace_id.0,
        title: req.title.clone(),
        description: req.description.clone(),
        assignee_type: assignee_type.clone(),
        assignee_id,
        status: STATUS_ACTIVE.to_string(),
        execution_mode: req.execution_mode.clone(),
        issue_title_template: req.issue_title_template.clone(),
        project_id,
        created_by_type: PUBLISHED_BY_MEMBER.to_string(),
        created_by_id: user_id.0,
    };
    let row = mc_repos::autopilot::write::create(conn, &new)
        .await
        .map_err(|_| failed("failed to create autopilot"))?;

    // 创建**就是**一次实质发布：写 v1 规则版本，让每个 autopilot 在派单时都有一个可追责的人
    // （MUL-4302 §3.4）。
    insert_rule_version(
        conn,
        row.id,
        workspace_id.0,
        PUBLISHED_BY_MEMBER,
        Some(user_id.0),
        Some(&rule_config_summary(&row)),
    )
    .await
    .map_err(|_| failed("failed to create autopilot"))?;

    for candidate in &candidates {
        add_subscriber(conn, row.id, candidate.user_id)
            .await
            .map_err(|_| failed("failed to add autopilot subscriber"))?;
    }
    tx.commit()
        .await
        .map_err(|_| failed("failed to create autopilot"))?;

    let repo = AutopilotRepo::new(state.db.clone());
    let subscribers = reload_subscribers(&repo, row.id).await;
    let response = autopilot_to_response(&row, subscribers);
    Ok((StatusCode::CREATED, Json(response)).into_response())
}

// ---------------------------------------------------------------------------
// #6 PATCH /api/autopilots/:id[/]
// ---------------------------------------------------------------------------

/// `PATCH /api/autopilots/:id[/]`（上游 `UpdateAutopilot` 260 行）。
///
/// 三态补丁的组装见 [`UpdateAutopilot`]（`mc-repos`）的文档；本函数只负责**把「键在不在」
/// 翻译成上游那两组参数**，并保证 `issue_title_template` / `project_id` 在缺失时回填 `prev`。
#[allow(clippy::too_many_lines)] // 上游 260 行；这里的行数本身就是「三态补丁」的复杂度
async fn update_autopilot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    user: AuthUser,
    Path(id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    // 上游顺序：workspace → 加载（404）→ **写权（403）** → body。⇒ 无权者的畸形 body 也回 403。
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    let role = require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let prev = load_in_workspace(&repo, &id, workspace_id).await?;
    if !member_can_write(&repo, &prev, &role, user_id).await {
        return Ok(forbidden_write());
    }

    let raw = parse_json_body(&body)?;
    let req: UpdateAutopilotRequest = decode_body(&raw)?;

    // 直赋值列（`issue_title_template` / `project_id`）：缺失必须回填 `prev`，否则一次 PATCH 清空。
    // `title` / `status` / `execution_mode` 是 COALESCE 列：`None` 天然 = 保持。
    let mut params = UpdateAutopilot {
        id: prev.id,
        issue_title_template: prev.issue_title_template.clone(),
        project_id: prev.project_id,
        ..UpdateAutopilot::default()
    };
    // `description` **故意不处理**：列是 `COALESCE($3, description)`，显式 `null` 与不传等价
    // （上游那条 `rawFields["description"]` 分支不可观测）。
    if let Some(title) = req.title.clone() {
        params.title = Some(title);
    }
    if let Some(status) = req.status.clone() {
        params.status = Some(status);
    }
    if let Some(mode) = req.execution_mode.clone() {
        params.execution_mode = Some(mode);
    }
    if sent(&raw, "issue_title_template") {
        if let Some(template) = req.issue_title_template.as_deref() {
            validate_issue_title_template(template).map_err(|err| bad_request(err.to_string()))?;
        }
        // `None`（显式 null）= 清空该列，上游如此。
        params.issue_title_template = req.issue_title_template.clone();
    }
    if sent(&raw, "project_id") {
        params.project_id =
            resolve_project_id(&state, req.project_id.as_deref(), workspace_id).await?;
    }

    // assignee 成对校验（上游原文：只改一个字段会让行指向错误的表）。
    let type_sent = sent(&raw, "assignee_type");
    let id_sent = sent(&raw, "assignee_id");
    let mut next_type = prev.assignee_type.clone();
    let mut next_id = prev.assignee_id;
    if type_sent || id_sent {
        if type_sent {
            if let Some(value) = req
                .assignee_type
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                next_type = value.to_string();
            }
        }
        if !is_valid_assignee_type(&next_type) {
            return Err(bad_request("assignee_type must be agent or squad").into());
        }
        if id_sent {
            let Some(raw_id) = req.assignee_id.as_deref() else {
                return Err(bad_request("assignee_id cannot be null").into());
            };
            next_id = parse_uuid(raw_id, "assignee_id")?;
        }
        if type_sent && !id_sent && next_type != prev.assignee_type {
            return Err(bad_request("assignee_id is required when changing assignee_type").into());
        }
        if type_sent {
            params.assignee_type = Some(next_type.clone());
        }
        if id_sent {
            params.assignee_id = Some(next_id);
        }
    }

    // 订阅者：`subscribers` 出现即**整表替换**（空数组 = 清空，断言语义）。
    let replace_subscribers = sent(&raw, "subscribers");
    let candidates = if replace_subscribers {
        parse_subscribers(&req.subscribers.clone().unwrap_or_default())
            .map_err(|err| bad_request(err.to_string()))?
    } else {
        Vec::new()
    };

    // 是否要在事务里重验 assignee：改派 / 改类型 / 或把状态置为 active（唤醒一个暂停的 autopilot）。
    let next_status = req.status.clone().unwrap_or_else(|| prev.status.clone());
    let require_runtime = next_status == STATUS_ACTIVE;
    let validate_assignee = type_sent || id_sent || require_runtime;

    let write_repo = AutopilotWriteRepo::new(state.db.clone());
    let mut tx = write_repo
        .begin()
        .await
        .map_err(|_| failed("failed to update autopilot"))?;
    let conn = &mut *tx;

    lock_and_validate_subscribers(conn, workspace_id.0, &candidates).await?;
    if validate_assignee {
        let scope = agent_scope(&state, workspace_id, user_id, &role);
        validate_assignee_for_save(conn, &scope, &next_type, next_id, require_runtime).await?;
    }

    // 锁序 ④：autopilot 行 `FOR UPDATE`。取不到 = 并发删除（上游同样折 404）。
    let locked = match lock_autopilot_for_update(conn, prev.id, workspace_id.0).await {
        Ok(row) => row,
        Err(RepoError::NotFound) => return Err(not_found("autopilot").into()),
        Err(err) => return Err(repo_err(err, "autopilot").into()),
    };
    // 乐观并发：`updated_at` 变了说明别人先提交 ⇒ 409（**扁平体**，唯一带 `code` 的写面 409）。
    if locked.updated_at != prev.updated_at {
        return Ok(update_conflict());
    }

    let updated = mc_repos::autopilot::write::update(conn, &params)
        .await
        .map_err(|_| failed("failed to update autopilot"))?;

    // 实质变更（who / whether / what 指令变了）才 append 规则版本，并把所有触发器的 config
    // 责任人转给本次编辑者（title / project_id 是装饰与归档位，不算）。
    if substantive_change(&prev, &updated) {
        insert_rule_version(
            conn,
            updated.id,
            workspace_id.0,
            PUBLISHED_BY_MEMBER,
            Some(user_id.0),
            Some(&rule_config_summary(&updated)),
        )
        .await
        .map_err(|_| failed("failed to update autopilot"))?;
        set_trigger_publishers_by_autopilot(conn, updated.id, user_id.0)
            .await
            .map_err(|_| failed("failed to update autopilot"))?;
    }

    if replace_subscribers {
        delete_subscribers_for_autopilot(conn, updated.id)
            .await
            .map_err(|_| failed("failed to update subscribers"))?;
        for candidate in &candidates {
            add_subscriber(conn, updated.id, candidate.user_id)
                .await
                .map_err(|_| failed("failed to add autopilot subscriber"))?;
        }
    }
    tx.commit()
        .await
        .map_err(|_| failed("failed to update autopilot"))?;

    let subscribers = reload_subscribers(&repo, updated.id).await;
    Ok(Json(autopilot_to_response(&updated, subscribers)).into_response())
}

// ---------------------------------------------------------------------------
// #7 DELETE /api/autopilots/:id[/]
// ---------------------------------------------------------------------------

/// `DELETE /api/autopilots/:id[/]`（上游 `DeleteAutopilot` 64 行）。
///
/// 「删除」= **归档**（`status='archived'` + 清 `pause_reason`）：run / task / webhook delivery /
/// subscriber / collaborator 全部保留为执行历史，列表侧靠 `status <> 'archived'` 隐藏。
/// 归档是实质状态变更 ⇒ 与归档**同事务**再 append 一条规则版本（`status = "archived"`）。
/// 返回 **204 无 body**。
async fn delete_autopilot(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    user: AuthUser,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let workspace_id = resolve_workspace_id(&headers, &query)?;
    let user_id = acting_user_id(user);
    let role = require_member(&state, workspace_id, user_id).await?;
    let repo = AutopilotRepo::new(state.db.clone());
    let row = load_in_workspace(&repo, &id, workspace_id).await?;
    if !member_can_write(&repo, &row, &role, user_id).await {
        return Ok(forbidden_write());
    }

    let write_repo = AutopilotWriteRepo::new(state.db.clone());
    let mut tx = write_repo
        .begin()
        .await
        .map_err(|_| failed("failed to delete autopilot"))?;
    let conn = &mut *tx;

    archive(conn, row.id)
        .await
        .map_err(|_| failed("failed to delete autopilot"))?;
    // 版本快照反映**归档后**的状态（上游 `ap.Status = "archived"` 后才写版本）。
    let mut archived = row.clone();
    archived.status = STATUS_ARCHIVED.to_string();
    insert_rule_version(
        conn,
        row.id,
        workspace_id.0,
        PUBLISHED_BY_MEMBER,
        Some(user_id.0),
        Some(&rule_config_summary(&archived)),
    )
    .await
    .map_err(|_| failed("failed to delete autopilot"))?;
    tx.commit()
        .await
        .map_err(|_| failed("failed to delete autopilot"))?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 聚合路由必须能构造：`autopilots/mod.rs` 把读面（`list.rs`）与写面（本文件）merge 到
    /// 同一批 path 上，方法重叠会在**构造期** panic（运行时才 404 反而看不出来）。
    #[test]
    fn aggregate_router_has_no_conflicting_routes() {
        let _ = super::super::router();
        let _ = router();
    }

    #[test]
    fn sent_distinguishes_missing_from_explicit_null() {
        let raw: Value = serde_json::json!({"assignee_id": null, "title": "t"});
        assert!(sent(&raw, "assignee_id"));
        assert!(sent(&raw, "title"));
        assert!(!sent(&raw, "subscribers"));
    }

    #[test]
    fn decode_body_maps_json_null_to_default() {
        let req: UpdateAutopilotRequest = decode_body(&Value::Null).expect("null is not an error");
        assert!(req.title.is_none() && req.assignee_id.is_none());
        let create: CreateAutopilotRequest =
            decode_body(&Value::Null).expect("null is not an error");
        assert!(create.title.is_empty());
    }

    #[test]
    fn decode_body_rejects_non_object_payloads() {
        assert!(decode_body::<UpdateAutopilotRequest>(&serde_json::json!([1, 2])).is_err());
        assert!(parse_json_body(&Bytes::from_static(b"{")).is_err());
    }

    /// 请求结构体只消费自己认识的键，未知键静默忽略（Go `json.Unmarshal` 同）。
    #[test]
    fn unknown_fields_are_ignored() {
        let raw = serde_json::json!({"title": "t", "priority": "high"});
        let req: CreateAutopilotRequest = decode_body(&raw).expect("decodes");
        assert_eq!(req.title, "t");
    }
}
