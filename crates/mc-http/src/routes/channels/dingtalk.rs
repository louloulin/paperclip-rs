//! `DingTalk` 安装 / 群身份面：**7 条**路由（写者 **M7-9**；`docs/60-M7-PLAN.md` §1.1 / §3.3）。
//!
//! | 注册键 | `router.go` | 授权层 | 未配置语义 |
//! | --- | ---: | --- | --- |
//! | `GET /api/workspaces/:id/dingtalk/installations` | 1814 | workspace **member** | **200** + `installations: []` / `configured:false` / `install_supported:false` |
//! | `GET /api/workspaces/:id/dingtalk/groups` | 1815 | workspace **member** | **200** + 空清单（`group_discovery_supported:true`） |
//! | `DELETE /api/workspaces/:id/dingtalk/installations/:installationId/groups/:conversationId` | 1816 | workspace **owner/admin** | **403** `dingtalk_not_configured` |
//! | `DELETE /api/workspaces/:id/dingtalk/installations/:installationId` | 1817 | agent owner 或 workspace **owner/admin** | **403** `dingtalk_not_configured` |
//! | `POST /api/workspaces/:id/dingtalk/install/byo` | 1818 | workspace member + agent owner/admin | **403** `dingtalk_not_configured` |
//! | `POST /api/dingtalk/binding/redeem` | 1850 | 登录用户（**无** workspace 前缀） | **403** `dingtalk_not_configured` |
//! | `GET /api/agents/:id/dingtalk/groups` | 2192 | agent 详情门的 `canAccessPrivateAgent` | **200** + 空清单（**但 agent 门先跑**，见下） |
//!
//! - **业务逻辑在 adapter**：安装 / 绑定 / 群清单的判决在
//!   `mc_channel::dingtalk::{install,binding,group_identity}`，本文件只做四件事 ——
//!   **鉴权层**、**wire 形状**、**未配置语义**、以及把「channel 层不碰 SQL」这条边界铁律落成
//!   端口实现（[`store`]）。
//! - **形态纪律**：只按上游字面量注册**那一形态**（M7 的 `dual-form required: 0` ⇒ 补尾斜杠是
//!   `EXTRA_ALIAS`、漏字面量是 `MISSING_EXACT`，两类都是硬失败）。路径参数必须写 `:name`
//!   （matchit 0.7 把 `{name}` 当字面量 ⇒ 编译通过且恒 404）。
//! - **第 7 条不经过 `routes/agents.rs`**：`channels/mod.rs` 的「结构决策第 1 条：全路径注册，
//!   不 `nest`」—— 上游把 agent 级那条 nest 进 `/api/agents/{id}` 子路由，本仓照抄会与该文件
//!   既有挂载点抢同一前缀（axum 0.7 嵌套重叠会 panic）。同形先例：`routes/mcp/agent.rs`
//!   的 4 条 `/api/agents/:id/mcp-servers*`（M8-3）就注册在自己的切片文件里，
//!   `agents.rs` **一行没动**。⚠️ 本文件原头注表格末尾写「（挂在既有 `/api/agents/{id}`
//!   子路由内部）」是**上游 Go 的注册位置**，不是本仓的落法（`docs/32` §23 的 D3）。
//!
//! # 反向验收：`GET …/dingtalk/group-routes` **必须保持 404**（`docs/60` §1.6）
//!
//! 上游已退役该路由并在 `integration_test.go:786` **主动断言 404**，且它**不在** 456 条里
//! ⇒ 本文件**不注册**它，也**不得**为它建读面（`mc-repos` 里的 `dingtalk_group_route` 只有
//! **行级**读写，不服务 HTTP）。用例钉住它（`tests.rs::the_retired_group_routes_route_stays_a_404`）。
//!
//! # 未配置语义**逐端点不同**（`docs/60` §2.4 / R-M7-3，不许"统一 503"）
//!
//! 上游的判据是 `h.DingTalkInstall == nil`（= 落库加密密钥缺失）。本仓的对应物是
//! [`AppState::channel_keys`] 里 `DingTalk` 那一格（`MULTICA_DINGTALK_SECRET_KEY`）：
//!
//! 1. **列表 / 群清单**回 200 + 空（**不**查库、**不**读身份 —— 上游这两个 handler 的第一句
//!    就是 nil 判断）。这条形状是 ⑨ 的 `TestListTelegramInstallationsNotConfiguredReturnsEmpty`
//!    那一族判据（M7-5 的 D1），M7-9 沿用**同一个**顺序（见 `docs/32` §23 的 D7）；
//! 2. **撤销 / 摘群 / BYO / 兑换**回 **403 `dingtalk_not_configured`**（上游 `writeFeatureDisabled`；
//!    被关掉的能力不是瞬时故障，回 503 会招来重试与告警噪音）；
//! 3. **agent 级群清单是唯一的例外**：上游那条把 `loadAgentForUser` + `canAccessPrivateAgent`
//!    放在 nil 判断**之前** ⇒ 认不出的 agent 是 **404**、看得见但未配置才是 200 空。
//!    顺序逐字照抄（用例 `the_agent_level_gate_runs_before_the_unconfigured_branch`）。
//!
//! # 授权矩阵（本片的专属验收：workspace 成员 × 私有 agent 的 owner）
//!
//! | 调用者 | `GET …/dingtalk/groups` | `GET /api/agents/:id/dingtalk/groups` |
//! | --- | --- | --- |
//! | workspace **owner/admin** | 全量清单 | `can_manage` ⇒ 放行 |
//! | workspace **member**（非 owner） | 只看**自己能打开的** agent 的群 | 私有 agent **403**；`public_to` 命中白名单 ⇒ 放行 |
//! | agent 的 **owner** | 该 agent 的群（`member_allowed_to_view` 通过） | 放行 |
//! | **非成员 / 外部人** | **404**（`workspace_role` 的 `workspace` 不是我的） | **404 `agent`** |
//!
//! 判据复用 `routes/agents.rs` 的 [`AgentScope`]（`can_manage` / `member_allowed_to_view` /
//! `can_access_private`），**不新增**第二份可见性真值（`routes/tasks.rs` 的先例逐字）。
//!
//! # 凭据纪律（`docs/60` §2.3 / §6.5 的 M7-9 行）
//!
//! - BYO 贴进来的 `AppSecret` **只经 `secretbox` 密文入库**（`mc_channel::dingtalk::install`
//!   的职责），本文件的 SQL 只搬运**已经封好的** `config`；
//! - 响应 DTO（[`DingTalkInstallationResponse`]）**不含** config（它是密文，且是服务端内部的事）；
//! - 本文件**没有任何** `tracing::*` 插值 `AppSecret` / 密文；错误文案只带 `DingTalk` 自己的
//!   错误码（平台错误体可能回声请求体 —— 它一律被丢掉，`docs/32` §22 差异 3）。
//!
//! # 与上游的落点差异（逐条登记 `docs/32` §23，**不是**静默略过）
//!
//! 1. **上游的六条 SQL 没有泛化仓储**（`ListChannelInstallationsByWorkspace` /
//!    `UpsertChannelInstallation` + 死主回收 + 唯一冲突分类 / `ConsumeChannelBindingToken` /
//!    `ListDingTalkGroupPresencesByWorkspace` 一族），而本片写集**不含**
//!    `crates/mc-repos/src/channel/**` ⇒ 它们以**端口实现**的形态落在 [`store`]
//!    （与 M7-4 / M7-5 同手法：channel 层只拿到 trait）；
//! 2. **`dingtalk_*` 三张表的写入**（群存在性 / bot 身份）在 M7-7 是**诚实默认值**
//!    （`NoGroupPresence`）⇒ 本片给真实现 [`store::PgGroupPresenceStore`] +
//!    [`mc_channel::dingtalk::group_identity::PresenceObserver`]，并把装配收进
//!    [`resolver_set`]（宿主在 `apps/mc-server/src/channels.rs` 调它，那一步是 anchor 写集）；
//! 3. **`AppKey` 不是秘密**：`config` 里的 `app_id` / `robot_code` 明文出现在列表响应里
//!    （上游逐字：`The AppKey itself is not a secret`）。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use mc_channel::dingtalk::binding::{BindingStore, BindingTokenService, RedeemOutcome};
use mc_channel::dingtalk::group_identity::{
    GroupInventory, GroupInventoryStore, GroupQuery, PresenceObserver,
};
use mc_channel::dingtalk::install::{
    InstallError, InstallService, InstallStore, RegisterByoParams,
};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_errors::Error;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, not_found, parse_uuid, AgentScope};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;
use dto::{configured, feature_disabled, unauthorized};
use scope::{agent_visibility, collect_groups, role_in_workspace, DingTalkScope};
use store::{PgBindingStore, PgGroupInventoryStore, PgInstallStore};

/// 本文件的路由切片（**逐字**上游路径；见模块头的形态纪律）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/dingtalk/installations",
            get(list_installations),
        )
        .route("/api/workspaces/:id/dingtalk/groups", get(list_groups))
        .route(
            "/api/workspaces/:id/dingtalk/installations/:installationId",
            delete(revoke_installation),
        )
        .route(
            "/api/workspaces/:id/dingtalk/installations/:installationId/groups/:conversationId",
            delete(forget_group),
        )
        .route(
            "/api/workspaces/:id/dingtalk/install/byo",
            post(register_byo),
        )
        .route("/api/dingtalk/binding/redeem", post(redeem_binding))
        .route(
            "/api/agents/:id/dingtalk/groups",
            get(list_groups_for_agent),
        )
}

/// 把 adapter 的安装错误映射到 HTTP（逐条对齐上游 `handler/dingtalk.go` 的 switch）。
fn install_error(error: &InstallError) -> Response {
    match error {
        InstallError::NotFound => {
            crate::error::ApiError::from(not_found("dingtalk installation")).into_response()
        }
        // 空凭据与"平台拒了"都是 **400**（上游逐字：a user error）。
        InstallError::InvalidAppKey | InstallError::InvalidAppSecret => {
            error_with_code(StatusCode::BAD_REQUEST, error.code(), &error.to_string())
        }
        InstallError::CredentialValidation => error_with_code(
            StatusCode::BAD_REQUEST,
            error.code(),
            "could not verify the DingTalk credentials — check the AppKey (client id) and \
             AppSecret (client secret), and that the robot is a Stream-mode robot in your \
             organization",
        ),
        InstallError::OwnedBySameWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this DingTalk robot is already connected to another agent in this workspace — \
             disconnect it there first, then connect it here",
        ),
        InstallError::OwnedByArchivedAgent => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this DingTalk robot is connected to an archived agent in this workspace — restore \
             that agent, or disconnect its robot, before connecting it here",
        ),
        InstallError::OwnedByAnotherWorkspace => error_with_code(
            StatusCode::CONFLICT,
            error.code(),
            "this DingTalk robot is already connected to a different Multica workspace — \
             disconnect it there before connecting it here",
        ),
        // 兜底 500：上游逐字 —— 加密 / 落库 / 意外都是**服务端**问题，不该被说成"你的凭据不对"。
        other => error_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            other.code(),
            "could not connect the DingTalk robot",
        ),
    }
}

/// 装好的 BYO 服务（端口实现 + 真 `OpenAPI` 客户端）。
fn install_service(state: &AppState) -> Option<InstallService> {
    let boxed = state.channel_keys.get(ChannelKind::DingTalk)?.clone();
    Some(InstallService::new(
        Arc::new(PgInstallStore::new(state.db.clone())) as Arc<dyn InstallStore>,
        Arc::new(mc_channel::dingtalk::client::Client::new())
            as Arc<dyn mc_channel::dingtalk::client::CredentialProbe>,
        boxed,
    ))
}

/// 群清单的读面（带落库密钥：`AppKey → 明文凭据` 那一步要解 `config` 的密文列）。
fn group_inventory_store(state: &AppState) -> Option<Arc<dyn GroupInventoryStore>> {
    let boxed = state.channel_keys.get(ChannelKind::DingTalk)?.clone();
    Some(Arc::new(PgGroupInventoryStore::new(
        state.db.clone(),
        boxed,
    )))
}

/// 解析器集合（M7-7 的两个诚实默认值在这里换成真实现）。
///
/// **宿主装配点**：`apps/mc-server/src/channels.rs`（anchor 写集）调
/// `mc_channel::dingtalk::register_resolvers(&router, resolver_set(&state)?)`。
/// 返回 `None` = 未配置（该平台整体不装配，`docs/60` §2.6 第 3 条）。
#[must_use]
pub fn resolver_set(
    state: &AppState,
) -> Option<mc_channel::dingtalk::resolvers::DingTalkResolverSet> {
    let boxed = state.channel_keys.get(ChannelKind::DingTalk)?.clone();
    let inventory = Arc::new(PgGroupInventoryStore::new(state.db.clone(), boxed));
    let names = Arc::new(mc_channel::dingtalk::group_identity::BotNameResolver::new(
        Arc::new(mc_channel::dingtalk::client::Client::new()),
        Arc::clone(&inventory) as Arc<dyn GroupInventoryStore>,
    ));
    let presence = Arc::new(PresenceObserver::new(
        Arc::new(store::PgGroupPresenceStore::new(state.db.clone())),
        Some(Arc::clone(&names)),
    ));
    Some(
        mc_channel::dingtalk::resolvers::DingTalkResolverSet::from_repos_with_presence(
            mc_repos::channel::installation::ChannelInstallationRepo::new(state.db.clone()),
            mc_repos::channel::binding::ChannelBindingRepo::new(state.db.clone()),
            mc_repos::member::MemberRepo::new(state.db.clone()),
            mc_repos::channel::dedup::ChannelInboundDedupRepo::new(state.db.clone()),
            Arc::new(mc_repos::channel::session::ChannelChatSessionRepo::new(
                state.db.clone(),
            )),
            mc_repos::channel::inbound_audit::ChannelInboundAuditRepo::new(state.db.clone()),
            presence,
        ),
    )
}

// =====================================================================
// GET /api/workspaces/:id/dingtalk/installations
// =====================================================================

/// 上游 `ListDingTalkInstallations`：**member 可见**（非管理员也能渲染 Integrations 页）。
///
/// 未配置 ⇒ **不**查库、**不**读身份，回 200 空 + 两个 `false`（`docs/60` R-M7-3）。
async fn list_installations(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
) -> ApiResult<Json<DingTalkInstallationsResponse>> {
    if !configured(&state) {
        return Ok(Json(DingTalkInstallationsResponse::not_configured()));
    }
    let scope = DingTalkScope::resolve(
        &state,
        user.ok_or_else(unauthorized)?,
        &raw_workspace_id,
        "workspace",
    )
    .await?;
    let (available, visible) = agent_visibility(&scope).await?;

    let store = PgInstallStore::new(state.db.clone());
    let records = InstallStore::list_by_workspace(&store, scope.workspace_id)
        .await
        .map_err(Error::Database)?;

    // **只看自己**的绑定（上游 `ListDingTalkUserBindingsForMember`）：返回每个人的 staff id
    // 会把身份暴露得比必要更宽 ⇒ 只给 owner/admin 这一列。
    let bindings = if scope.is_admin() {
        store::member_bindings(&state.db, scope.workspace_id, scope.user_id)
            .await
            .map_err(|error| Error::Database(error.to_string()))?
    } else {
        HashMap::new()
    };

    let mut out = Vec::with_capacity(records.len());
    for record in &records {
        if !scope.is_admin() && !visible.contains(&record.agent_id) {
            continue;
        }
        let mut response = DingTalkInstallationResponse::from_record(record);
        response.agent_available = available.contains(&record.agent_id);
        if scope.is_admin() {
            response.bound_dingtalk_user_ids =
                Some(bindings.get(&record.id.0).cloned().unwrap_or_default());
        }
        out.push(response);
    }
    Ok(Json(DingTalkInstallationsResponse::configured_with(out)))
}

// =====================================================================
// GET /api/workspaces/:id/dingtalk/groups
// =====================================================================

/// 上游 `ListDingTalkGroups`：**member 可见**；owner/admin 看全量，普通成员只看自己能打开的
/// agent 的群（与 `ListAgents` / Agent 详情同一套规则）。
///
/// 未配置 ⇒ 200 + 空清单（`GroupInventory::empty()` 的 `group_discovery_supported` 是 **true**，
/// 与 lark 的 `install_supported:false` **不是**同一个意思）。
async fn list_groups(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
    Query(query): Query<GroupQuery>,
) -> ApiResult<Json<GroupInventory>> {
    if !configured(&state) {
        return Ok(Json(GroupInventory::empty()));
    }
    let scope = DingTalkScope::resolve(
        &state,
        user.ok_or_else(unauthorized)?,
        &raw_workspace_id,
        "workspace",
    )
    .await?;
    let visible: Option<HashSet<Id>> = if scope.is_admin() {
        None
    } else {
        let (_, visible) = agent_visibility(&scope).await?;
        Some(visible)
    };
    let Some(store) = group_inventory_store(&state) else {
        return Ok(Json(GroupInventory::empty()));
    };
    let inventory = collect_groups(
        &state,
        &store,
        scope.workspace_id,
        None,
        visible.as_ref(),
        &query,
    )
    .await?;
    Ok(Json(inventory))
}

// =====================================================================
// GET /api/agents/:id/dingtalk/groups
// =====================================================================

/// 上游 `ListDingTalkGroupsForAgent`：只暴露**这一个** agent 的 1:1 机器人与它观察到的群。
///
/// 故意复用 Agent 详情那道门（上游逐字：*if the caller cannot open this Agent, they cannot
/// infer its `DingTalk` group activity either*）⇒ 顺序是
/// `resolve workspace`（400）→ `load agent`（404）→ `canAccessPrivateAgent`（403）→ nil 判断。
async fn list_groups_for_agent(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    headers: HeaderMap,
    Path(raw_agent_id): Path<String>,
    Query(raw_query): Query<HashMap<String, String>>,
    Query(query): Query<GroupQuery>,
) -> ApiResult<Json<GroupInventory>> {
    let _ = query.as_map();
    let scope = AgentScope::resolve(&state, user, &headers, &raw_query).await?;
    let agent = scope.load_agent(&raw_agent_id).await?;
    let targets = scope
        .repo
        .list_invocation_targets(agent.id())
        .await
        .map_err(|error| Error::Database(error.to_string()))?;
    scope.require_can_access_private(&agent, &targets)?;

    if !configured(&state) {
        return Ok(Json(GroupInventory::empty()));
    }
    let Some(store) = group_inventory_store(&state) else {
        return Ok(Json(GroupInventory::empty()));
    };
    let inventory = collect_groups(
        &state,
        &store,
        scope.workspace_id,
        Some(agent.id()),
        None,
        &query,
    )
    .await?;
    Ok(Json(inventory))
}

// =====================================================================
// DELETE /api/workspaces/:id/dingtalk/installations/:installationId/groups/:conversationId
// =====================================================================

/// 上游 `ForgetDingTalkGroup`：摘掉一条观察，**会话与消息历史保留**；同一群之后再来一条被
/// 成功处理的 @ 消息就会重新观察到它。**owner/admin only**。
async fn forget_group(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path((raw_workspace_id, raw_installation_id, raw_conversation_id)): Path<(
        String,
        String,
        String,
    )>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let scope = DingTalkScope::resolve(
        &state,
        user.ok_or_else(unauthorized)?,
        &raw_workspace_id,
        "dingtalk group not found",
    )
    .await?;
    scope.require_admin()?;
    let installation_id = Id(parse_uuid(&raw_installation_id, "installation id")?);
    let conversation_id = raw_conversation_id.trim().to_string();
    if conversation_id.is_empty() {
        return Err(bad_request("conversation id is required").into());
    }
    let Some(store) = group_inventory_store(&state) else {
        return Ok(feature_disabled());
    };
    let forgotten = store
        .forget_presence(scope.workspace_id, installation_id, &conversation_id)
        .await
        .map_err(Error::Database)?;
    if !forgotten {
        return Err(not_found("dingtalk group").into());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

// =====================================================================
// DELETE /api/workspaces/:id/dingtalk/installations/:installationId
// =====================================================================

/// 上游 `RevokeDingTalkInstallation`：状态翻成 `revoked`，**行保留**供审计；重装把状态翻回
/// `active`。授权是"该 agent 的 owner 或 workspace owner/admin"；**孤儿安装**（agent 行已删）
/// 回落到 workspace owner/admin 清理。
async fn revoke_installation(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path((raw_workspace_id, raw_installation_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let user_id = user.ok_or_else(unauthorized)?.id();
    let workspace_id = Id(parse_uuid(&raw_workspace_id, "workspace id")?);
    let installation_id = Id(parse_uuid(&raw_installation_id, "installation id")?);

    // workspace 收窄的读（另一个 workspace 猜 id 也读不到 ⇒ 与不存在同结果）。
    let store = PgInstallStore::new(state.db.clone());
    let record = InstallStore::get_in_workspace(&store, installation_id, workspace_id)
        .await
        .map_err(Error::Database)?
        .ok_or_else(|| not_found("dingtalk installation"))?;

    let role = role_in_workspace(&state, workspace_id, user_id).await?;
    let is_member = role.is_some();
    let scope = DingTalkScope::with_role(&state, workspace_id, user_id, role.unwrap_or_default());
    if let Ok(agent) = scope
        .repo
        .get_in_workspace(workspace_id, record.agent_id)
        .await
    {
        scope.require_agent_manager(&agent)?;
    } else {
        // 孤儿安装：没有 agent owner 可解析 ⇒ 只有 workspace owner/admin 能清理。
        if !is_member {
            return Err(not_found("dingtalk installation").into());
        }
        scope.require_admin()?;
    }
    InstallStore::revoke(&store, workspace_id, installation_id)
        .await
        .map_err(Error::Database)?;
    publish_event(
        &state,
        workspace_id,
        "dingtalk_installation",
        "revoked",
        &installation_id,
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

// =====================================================================
// POST /api/workspaces/:id/dingtalk/install/byo
// =====================================================================

/// 上游 `RegisterDingTalkBYO`（`?agent_id=…`）：workspace 成员 + **目标 agent 的 owner 或
/// workspace owner/admin**；`agent_id` 在**查询串**里，两个凭据在体里。
async fn register_byo(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Path(raw_workspace_id): Path<String>,
    Query(query): Query<ByoQuery>,
    Json(body): Json<RegisterDingTalkByoRequest>,
) -> ApiResult<Response> {
    let Some(service) = install_service(&state) else {
        return Ok(feature_disabled());
    };
    let scope = DingTalkScope::resolve(
        &state,
        user.ok_or_else(unauthorized)?,
        &raw_workspace_id,
        "workspace",
    )
    .await?;

    let raw_agent_id = query.agent_id.trim();
    if raw_agent_id.is_empty() {
        return Err(bad_request("agent_id is required").into());
    }
    let agent_id = Id(parse_uuid(raw_agent_id, "agent_id")?);
    // 边界上的所有权前置校验：错的 agent_id 是明确的 404（而不是落库时才发现）。
    let agent = match scope
        .repo
        .get_in_workspace(scope.workspace_id, agent_id)
        .await
    {
        Ok(agent) => agent,
        Err(mc_repos::RepoError::NotFound) => {
            return Ok(error_with_code(
                StatusCode::NOT_FOUND,
                "agent_not_found",
                "agent not found in this workspace",
            ))
        }
        Err(error) => return Err(Error::Database(error.to_string()).into()),
    };
    scope.require_agent_manager(&agent)?;

    let params = RegisterByoParams::new(
        scope.workspace_id,
        agent_id,
        scope.user_id,
        body.client_id,
        body.client_secret,
    );
    match service.register_byo(&params).await {
        Ok(record) => {
            publish_event(
                &state,
                scope.workspace_id,
                "dingtalk_installation",
                "created",
                &record.id,
            );
            Ok(Json(DingTalkInstallationResponse::from_record(&record)).into_response())
        }
        Err(error) => Ok(install_error(&error)),
    }
}

// =====================================================================
// POST /api/dingtalk/binding/redeem
// =====================================================================

/// 上游 `RedeemDingTalkBindingToken`：**无** workspace 前缀（兑换者在拥有 workspace 上下文
/// **之前**就打它），身份来自**会话**而不是令牌。
///
/// 三种失败各有自己的状态码：`410 Gone`（令牌未知 / 已消费 / 已过期 / 属于别的 adapter）、
/// `409 Conflict`（这个 `DingTalk` id 已属于另一个用户）、`403 Forbidden`（兑换者不是成员）。
async fn redeem_binding(
    State(state): State<Arc<AppState>>,
    user: Option<AuthUser>,
    Json(body): Json<RedeemDingTalkBindingTokenRequest>,
) -> ApiResult<Response> {
    if !configured(&state) {
        return Ok(feature_disabled());
    }
    let user_id = user.ok_or_else(unauthorized)?.id();
    let token = body.token.trim();
    if token.is_empty() {
        return Err(bad_request("token is required").into());
    }
    let service = BindingTokenService::new(
        Arc::new(PgBindingStore::new(state.db.clone())) as Arc<dyn BindingStore>
    );
    match service.redeem(token, user_id).await {
        Ok(RedeemOutcome::Bound(bound)) => {
            publish_event(
                &state,
                bound.workspace_id,
                "dingtalk_installation",
                "binding_updated",
                &bound.installation_id,
            );
            Ok(Json(RedeemDingTalkBindingTokenResponse {
                workspace_id: bound.workspace_id.to_string(),
                installation_id: bound.installation_id.to_string(),
                dingtalk_user_id: bound.channel_user_id,
            })
            .into_response())
        }
        Ok(RedeemOutcome::TokenInvalid) => Ok(error_with_code(
            StatusCode::GONE,
            "dingtalk_binding_token_invalid",
            "binding token invalid or expired",
        )),
        Ok(RedeemOutcome::AlreadyAssigned) => Ok(error_with_code(
            StatusCode::CONFLICT,
            "dingtalk_binding_already_assigned",
            "this DingTalk account is already bound to a different Multica user",
        )),
        Ok(RedeemOutcome::NotMember) => Ok(error_with_code(
            StatusCode::FORBIDDEN,
            "dingtalk_binding_not_member",
            "binding refused (are you a workspace member?)",
        )),
        Err(error) => {
            tracing::warn!(code = error.code(), "dingtalk binding redeem failed");
            Err(Error::Database(error.to_string()).into())
        }
    }
}

// =====================================================================
// 共享小件
// =====================================================================

/// 广播一条安装事件（上游 `publishDingTalkInstallationCreated` /
/// `EventDingTalkInstallationRevoked` / `EventDingTalkAccountBindingUpdated`，
/// 三个 `type` **逐字**：`dingtalk_installation:{created,revoked,binding_updated}`）。
fn publish_event(state: &AppState, workspace_id: Id, kind: &str, action: &str, id: &Id) {
    let envelope = mc_realtime::EventEnvelope::new(
        kind,
        workspace_id.to_string(),
        None,
        serde_json::json!({ "id": id.to_string() }),
    )
    .with_type(format!("{kind}:{action}"));
    state.realtime.publish(envelope);
}

pub mod dto;
pub mod scope;
pub mod store;

pub(crate) use dto::error_with_code;
pub use dto::{
    app_url, binding_url, is_configured, url_encode, ByoQuery, DingTalkInstallationResponse,
    DingTalkInstallationsResponse, RedeemDingTalkBindingTokenRequest,
    RedeemDingTalkBindingTokenResponse, RegisterDingTalkByoRequest, APP_URL_ENV, BINDING_PATH,
    CODE_DINGTALK_NOT_CONFIGURED, FRONTEND_ORIGIN_ENV,
};

#[cfg(test)]
mod tests;
