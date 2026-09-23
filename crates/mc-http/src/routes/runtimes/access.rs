//! runtime 切片的请求上下文：成员/角色、runtime 行载入、错误映射、时间格式化
//! （从 `runtimes.rs` 拆出，R7 单文件 800 行上限）。
//!
//! workspace 解析复用 `crate::routes::issues::resolve_workspace`（`x-workspace-id`
//! header → `x-workspace-slug` → `?workspace_id=` → `?workspace_slug=`，四个来源全缺
//! → 400）。`issues` 通过 `pub(crate) use` 导出它，同 `issue_table` 的既有用法。
//!
//! 成员/角色判定刻意**不用** `super::super::invitations::{require_workspace_member,
//! require_workspace_admin}`：那两个函数只回 `Result<(), Error>`，而 `canEditRuntime`
//! 与 `GET /api/runtimes/` 的三口径都需要**角色本身**。这里自建一个回
//! [`RuntimeMember`] 的版本，语义与它们一致（非成员 → 404，角色不足 → 403）。

use axum::body::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use mc_core::Id;
use mc_errors::Error;
use mc_repos::runtime::{AgentRuntimeRepo, AgentRuntimeRow};
use mc_repos::RepoError;
use serde::de::DeserializeOwned;

use crate::state::AppState;

pub(crate) use crate::routes::issues::{resolve_workspace, WorkspaceQuery};

/// 角色不足的提示语。上游是 `"insufficient permissions"`；本地沿用 M1/M2 既有措辞
/// （状态码一致，文案偏离见 `docs/39-M3-4-RUNTIME-PROFILES.md` §4）。
pub(crate) const ADMIN_REQUIRED: &str = "workspace admin role required";

// ---------------------------------------------------------------------------
// 错误构造
// ---------------------------------------------------------------------------

pub(crate) fn validation(message: impl Into<String>) -> Error {
    Error::Validation {
        message: message.into(),
        details: Vec::new(),
    }
}

pub(crate) fn not_found(resource: &str) -> Error {
    Error::NotFound {
        resource: resource.into(),
    }
}

pub(crate) fn forbidden(message: impl Into<String>) -> Error {
    Error::Forbidden {
        message: message.into(),
    }
}

pub(crate) fn db_err(e: &sqlx::Error) -> Error {
    Error::Database(e.to_string())
}

/// 仓储错误 → HTTP 错误。`resource` 决定 404 / 409 的文案主语。
pub(crate) fn repo_err(e: RepoError, resource: &str) -> Error {
    match e {
        RepoError::NotFound => not_found(resource),
        RepoError::Conflict => Error::Conflict {
            message: format!("{resource} state conflict"),
        },
        RepoError::Db(message) => Error::Database(message),
    }
}

/// `:<field>` 路径参数解析：非法 → 400（上游 `parseUUIDOrBadRequest` → `invalid <field>`）。
pub(crate) fn parse_path_id(field: &str, raw: &str) -> Result<Id, Error> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("{field} must be a uuid")))
}

/// 请求体解码：形状不符 / 空 body → 400 `invalid request body`。
///
/// 刻意不用 `Json<T>` 提取器 —— 它对类型不符回 **422**，而本仓（与上游
/// `json.NewDecoder().Decode`）都是 400。`null` body 等价于 Go 的「所有字段缺失」。
pub(crate) fn decode_body<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T, Error> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|_| validation("invalid request body"))?;
    if value.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(value).map_err(|_| validation("invalid request body"))
}

// ---------------------------------------------------------------------------
// 时间戳
// ---------------------------------------------------------------------------

/// 上游 `timestampToString`：RFC3339，秒精度、UTC `Z`。
pub(crate) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 上游 `timestampToPtr`：无效时间戳回 `null`（本地 `Option` 就是那个 `null`）。
pub(crate) fn timestamp_opt(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(timestamp)
}

// ---------------------------------------------------------------------------
// 成员 / 角色
// ---------------------------------------------------------------------------

/// 成员身份 + 角色（upstream `db.Member` 的最小投影）。
#[derive(Debug, Clone)]
pub(crate) struct RuntimeMember {
    pub(crate) user_id: Id,
    pub(crate) role: String,
}

impl RuntimeMember {
    /// `owner` / `admin` —— 治理权限（能看见并改名/删除别人的 runtime，但**不能**用）。
    pub(crate) fn is_admin(&self) -> bool {
        matches!(self.role.as_str(), "owner" | "admin")
    }

    /// upstream `canEditRuntime`：owner/admin 可改任何 runtime；普通成员只能改自己的。
    pub(crate) fn can_edit_runtime(&self, rt: &AgentRuntimeRow) -> bool {
        self.is_admin() || rt.owner_id == Some(self.user_id)
    }

    /// upstream `canSetRuntimeVisibility`：**故意比 `can_edit_runtime` 更窄**，只有 owner 本人。
    ///
    /// 可见性是「把我的机器借给 workspace」的同意，admin 若能翻它，就等于把
    /// `canUseRuntimeForAgent` 里取消掉的那个豁免又拿回来（跑在别人机器上花的是他的凭据）。
    pub(crate) fn can_set_visibility(&self, rt: &AgentRuntimeRow) -> bool {
        rt.owner_id == Some(self.user_id)
    }
}

/// 读成员行；不存在 → 404 `workspace`（隐藏 workspace 是否存在）。
pub(crate) async fn load_member(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<RuntimeMember, Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_optional(state.db.pool())
            .await
            .map_err(|e| db_err(&e))?;
    row.map(|(role,)| RuntimeMember { user_id, role })
        .ok_or_else(|| not_found("workspace"))
}

/// 成员 + owner/admin 门（上游路由组 `RequireWorkspaceRoleFromURL(..., "owner", "admin")`）。
pub(crate) async fn load_admin(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<RuntimeMember, Error> {
    let member = load_member(state, workspace_id, user_id).await?;
    if !member.is_admin() {
        return Err(forbidden(ADMIN_REQUIRED));
    }
    Ok(member)
}

// ---------------------------------------------------------------------------
// runtime 行载入
// ---------------------------------------------------------------------------

/// 按 `:runtimeId` 取行：非法 id → 400，不存在 → 404 `runtime`。
pub(crate) async fn load_runtime(state: &AppState, raw: &str) -> Result<AgentRuntimeRow, Error> {
    let id = parse_path_id("runtime_id", raw)?;
    AgentRuntimeRepo::new(state.db.clone())
        .get(id)
        .await
        .map_err(|e| repo_err(e, "runtime"))?
        .ok_or_else(|| not_found("runtime"))
}

/// runtime 作用域下的成员校验：**非成员也回 404 `runtime`**。
///
/// 上游所有 runtime 路由都把 `notFoundMsg` 传成 `"runtime not found"`：一个已知但
/// 在别人 workspace 里的 runtime id 不能变成存在性预言机。
pub(crate) async fn load_member_for_runtime(
    state: &AppState,
    rt: &AgentRuntimeRow,
    user_id: Id,
) -> Result<RuntimeMember, Error> {
    load_member(state, rt.workspace_id, user_id)
        .await
        .map_err(|e| match e {
            Error::NotFound { .. } => not_found("runtime"),
            other => other,
        })
}

/// 用量/活动四个读端点的门（upstream `requireRuntimeReadAccess`）：成员 + 可用性。
///
/// 「可用」比「可见」更窄：`private` 机器只有 owner 能读，**owner/admin 也没有豁免**
/// （upstream MUL-6126）。失败一律 404，理由同上。
pub(crate) async fn load_readable_runtime(
    state: &AppState,
    raw: &str,
    user_id: Id,
) -> Result<AgentRuntimeRow, Error> {
    Ok(load_readable_runtime_member(state, raw, user_id).await?.0)
}

/// 同上，但把成员行一起交回 —— 上游 `requireRuntimeReadAccess` 的签名就是
/// `(rt, member, ok)`，而 `canEditRuntime` / `canSetRuntimeVisibility` 这类判定
/// 需要**角色**：`InitiateUpdate` / `GetUpdate` 与本地 skill 的 owner-only 门都用这个。
///
/// 成员只载入一次：先调用本函数再调 `load_readable_runtime` 会多一次 `member` 查询。
pub(crate) async fn load_readable_runtime_member(
    state: &AppState,
    raw: &str,
    user_id: Id,
) -> Result<(AgentRuntimeRow, RuntimeMember), Error> {
    let rt = load_runtime(state, raw).await?;
    let member = load_member_for_runtime(state, &rt, user_id).await?;
    if !rt.usable_by(member.user_id) {
        return Err(not_found("runtime"));
    }
    Ok((rt, member))
}
