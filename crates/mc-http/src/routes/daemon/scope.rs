//! daemon 面的请求上下文：认证、三类门禁、错误与 DTO 投影（R7 拆分自 `daemon.rs`）。
//!
//! ## 认证（upstream `middleware/daemon_auth.go` 的本地等价物）
//!
//! `Authorization: Bearer <token>` 按前缀分流：
//!
//! | 前缀 | 含义 | 本地行为 |
//! |------|------|----------|
//! | `mdt_` | daemon token | `daemon_token.token_hash = hex(sha256(token))` 查表；命中且未过期 → `Daemon { workspace_id, daemon_id }` |
//! | `mul_` | 用户 PAT | `pat.token_hash = hex(sha256(token))` 查表；未撤销未过期 → `User { user_id }` |
//! | `mcn_` | 云端 PAT | 本地无云端签发面 ⇒ 一律 401（fail-closed，与上游 `cloud_pat` 未配置时一致） |
//! | 其它 / 无 `Bearer` | — | 401 `invalid token` |
//!
//! **无 `Authorization` 头**时走本仓 M1/M2 的 dev-mode 约定（`X-Multica-User-Id`，可选
//! `X-Daemon-Id`），否则本仓既有的 daemon e2e 测试无法构造身份。这是**偏离 D-1**，
//! 记在 `docs/32-M3-DAEMON-FACE.md` 的偏离表里。
//!
//! JWT 面（上游最后那条 `jwt` 分支）本地未实现：M1 的 session 中间件用的是
//! `X-Multica-Session` cookie，语义不同；走 dev-mode 兜底即可，登记为 D-7。
//!
//! ## 门禁三级（upstream `requireDaemonWorkspaceAccess` / `requireDaemonRuntimeAccess` /
//! `requireDaemonTaskAccess`）
//!
//! 语义要点，逐条对齐上游：
//!
//! 1. **daemon token 只能看自己 workspace**：不匹配直接 404（不是 403）——
//!    404 才不泄漏"这个 workspace 存在"。
//! 2. **用户身份走 member 表**：不是成员 → 404。上游有 `MembershipCache`，本地每次查库
//!    （正确性优先，偏离 D-4：无缓存）。
//! 3. **`requireDaemonRuntimeAccess` 只把"行不存在"翻成 404**，其它 DB 故障必须落 500
//!    （上游 MUL-7259 的教训：把基础设施抖动伪装成"runtime 没了"会让 daemon 重启）。
//! 4. **`not_found_msg` 由调用方给定**：daemon 面的 runtime/task 都用 `"runtime not found"` /
//!    `"task not found"`，workspace 面用 `"workspace not found"`。

use async_trait::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::daemon::{DaemonRepo, DaemonTokenRow};
use mc_repos::pat::PatRepo;
use mc_repos::runtime::{AgentRuntimeRepo, AgentRuntimeRow};
use mc_repos::task::TaskRow;
use mc_repos::RepoError;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub(crate) const USER_ID_HEADER: &str = "x-multica-user-id";
pub(crate) const DAEMON_ID_HEADER: &str = "x-daemon-id";

/// daemon 面的调用者身份。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonActor {
    /// `mdt_` daemon token：只剩这一台机器 + 这一个 workspace。
    Daemon {
        /// token 绑定的 workspace。
        workspace_id: Id,
        /// token 绑定的 daemon id。
        daemon_id: String,
    },
    /// `mul_` PAT 或 dev-mode 头：workspace 要现查 membership。
    User {
        /// 当前用户。
        user_id: Id,
        /// dev-mode 下由 `X-Daemon-Id` 带上的机器标识（生产 PAT 路径恒 `None`）。
        daemon_id: Option<String>,
    },
}

impl DaemonActor {
    /// 当前机器标识（daemon token 路径恒有；用户路径只有 dev-mode 头能给）。
    #[must_use]
    pub(crate) fn daemon_id(&self) -> Option<&str> {
        match self {
            Self::Daemon { daemon_id, .. } => Some(daemon_id.as_str()),
            Self::User { daemon_id, .. } => daemon_id.as_deref(),
        }
    }
}

/// 已认证的 daemon 面调用者。
#[derive(Debug, Clone)]
pub(crate) struct DaemonAuth {
    /// 身份本体。
    pub(crate) actor: DaemonActor,
}

#[async_trait]
impl FromRequestParts<Arc<AppState>> for DaemonAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if let Some(raw) = bearer_token(parts) {
            return authenticate(state, &raw).await;
        }
        // dev-mode 兜底（偏离 D-1）。
        let user = header(parts, USER_ID_HEADER)
            .and_then(|v| Id::parse(v.trim()).ok())
            .ok_or_else(|| unauthorized("missing Authorization header"))?;
        let daemon_id = header(parts, DAEMON_ID_HEADER).map(|v| v.trim().to_string());
        Ok(Self {
            actor: DaemonActor::User {
                user_id: user,
                daemon_id: daemon_id.filter(|v| !v.is_empty()),
            },
        })
    }
}

fn header(parts: &Parts, name: &str) -> Option<String> {
    parts
        .headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// `Bearer <token>` 提取；大小写不敏感，空 token 视为无凭据。
fn bearer_token(parts: &Parts) -> Option<String> {
    let raw = parts
        .headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let rest = raw
        .strip_prefix("Bearer ")
        .or_else(|| raw.strip_prefix("bearer "))?;
    let token = rest.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// upstream `auth.HashToken` = `hex(sha256(token))`（与 `mc-repos::PatRepo::hash_token` 同算法）。
fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

async fn authenticate(state: &AppState, raw: &str) -> Result<DaemonAuth, ApiError> {
    if raw.starts_with("mdt_") {
        let repo = DaemonRepo::new(&state.db);
        let row: Option<DaemonTokenRow> = repo
            .lookup_daemon_token(&hash_token(raw))
            .await
            .map_err(db_err)?;
        let Some(row) = row else {
            return Err(unauthorized("invalid daemon token"));
        };
        if row.expires_at <= chrono::Utc::now() {
            return Err(unauthorized("invalid daemon token"));
        }
        return Ok(DaemonAuth {
            actor: DaemonActor::Daemon {
                workspace_id: row.workspace_id(),
                daemon_id: row.daemon_id,
            },
        });
    }
    if raw.starts_with("mul_") {
        let row = PatRepo::new(state.db.clone())
            .get_by_token(raw)
            .await
            .map_err(|e| match e {
                RepoError::Db(message) => db_err(message),
                other => internal(format!("pat lookup failed: {other:?}")),
            })?;
        let Some(row) = row else {
            return Err(unauthorized("invalid token"));
        };
        return Ok(DaemonAuth {
            actor: DaemonActor::User {
                user_id: row.user_id,
                daemon_id: None,
            },
        });
    }
    // `mcn_` 云端 PAT 与未知前缀：本地无签发面 ⇒ fail-closed（与上游未配置云端时一致）。
    Err(unauthorized("invalid token"))
}

// ---------------------------------------------------------------------------
// 错误构造
// ---------------------------------------------------------------------------

// 这一族**直接产出 `ApiError`**（而非 `mc_errors::Error`）：daemon 面的 handler 本体
// 一律返回 `ApiResult<T>`，`Err(validation(..))` 是最常见的形态；若这里返回裸 `Error`，
// 每个调用点都要多一层包装。门禁函数（`require_*`）与解析函数（`parse_path_id`）
// 同样收敛到 `ApiError`，让 `?` 在整条调用链上无需转换。
pub(crate) fn validation(message: impl Into<String>) -> ApiError {
    ApiError(Error::Validation {
        message: message.into(),
        details: Vec::new(),
    })
}

pub(crate) fn not_found(resource: &str) -> ApiError {
    ApiError(Error::NotFound {
        resource: resource.into(),
    })
}

/// 409：状态机 / CAS 拒绝（upstream `writeError(w, http.StatusConflict, msg)`）。
pub(crate) fn conflict(message: impl Into<String>) -> ApiError {
    ApiError(Error::Conflict {
        message: message.into(),
    })
}

pub(crate) fn forbidden(message: impl Into<String>) -> ApiError {
    ApiError(Error::Forbidden {
        message: message.into(),
    })
}

pub(crate) fn unauthorized(message: impl Into<String>) -> ApiError {
    ApiError(Error::Unauthorized {
        message: message.into(),
    })
}

/// 仓储 / 基础设施错误 → 500（`RepoError` 与 `sqlx::Error` 都实现 `Display`）。
pub(crate) fn db_err(e: impl std::fmt::Display) -> ApiError {
    ApiError(Error::Database(e.to_string()))
}

/// 基础设施故障（上游 `writeError(w, 500, msg)`）。`msg` 是**操作**描述，不是错误原文。
pub(crate) fn internal(message: impl Into<String>) -> ApiError {
    ApiError(Error::Internal(message.into()))
}

// ---------------------------------------------------------------------------
// 门禁
// ---------------------------------------------------------------------------

/// workspace 门（upstream `requireDaemonWorkspaceAccess`）。
///
/// daemon token：`token.workspace_id` 必须等于目标，否则 404。
/// 用户身份：`member` 表命中即过，否则 404。
pub(crate) async fn require_workspace_access(
    state: &AppState,
    auth: &DaemonAuth,
    workspace_id: Id,
    not_found_msg: &str,
) -> Result<(), ApiError> {
    if workspace_allowed(state, auth, workspace_id).await? {
        Ok(())
    } else {
        Err(not_found(not_found_msg))
    }
}

/// workspace 门的**布尔版**：批量场景里「这条不属于我」= 跳过（而不是整批 404）。
///
/// 上游同样有两个形态：`requireDaemonWorkspaceAccess`（写 404 并返回 false，调用方
/// `return`）与 `verifyDaemonWorkspaceAccess`（只返回 false，调用方 `continue`）。
/// 读 `member` 失败仍是 500（不静默降级成 404）。
pub(crate) async fn workspace_allowed(
    state: &AppState,
    auth: &DaemonAuth,
    workspace_id: Id,
) -> ApiResult<bool> {
    match &auth.actor {
        DaemonActor::Daemon {
            workspace_id: token_ws,
            ..
        } => Ok(*token_ws == workspace_id),
        DaemonActor::User { user_id, .. } => is_workspace_member(state, workspace_id, *user_id)
            .await
            .map_err(db_err),
    }
}

/// 读 `member` 表（0 行 = 非成员）。
///
/// 上游还有 `MembershipCache`（Redis）；本地每次查库。偏离 D-4：无缓存层。
pub(crate) async fn is_workspace_member(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> Result<bool, sqlx::Error> {
    let found: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.as_uuid())
            .bind(user_id.as_uuid())
            .fetch_optional(state.db.pool())
            .await?;
    Ok(found.is_some())
}

/// runtime 门（upstream `requireDaemonRuntimeAccess`）：载行 → workspace 门。
///
/// **只把"行不存在"翻成 404**；其它 DB 故障原样上抛（500）。
pub(crate) async fn require_runtime_access(
    state: &AppState,
    auth: &DaemonAuth,
    raw_runtime_id: &str,
    not_found_msg: &str,
) -> Result<AgentRuntimeRow, ApiError> {
    let runtime_id = parse_path_id("runtime_id", raw_runtime_id)?;
    let runtime = AgentRuntimeRepo::new(state.db.clone())
        .get(runtime_id)
        .await
        .map_err(|e| match e {
            RepoError::Db(message) => db_err(message),
            _ => internal("failed to load runtime"),
        })?
        .ok_or_else(|| not_found(not_found_msg))?;
    require_workspace_access(state, auth, runtime.workspace_id, not_found_msg).await?;
    Ok(runtime)
}

/// task 门（upstream `requireDaemonTaskAccess`）：载行 → 解析 workspace → workspace 门。
pub(crate) async fn require_task_access(
    state: &AppState,
    auth: &DaemonAuth,
    raw_task_id: &str,
    not_found_msg: &str,
) -> Result<(TaskRow, Id), ApiError> {
    let task_id = parse_path_id("task_id", raw_task_id)?;
    let repo = DaemonRepo::new(&state.db);
    let task = repo
        .task_by_id(task_id)
        .await
        .map_err(|e| match e {
            RepoError::Db(message) => db_err(message),
            _ => internal("failed to load task"),
        })?
        .ok_or_else(|| not_found(not_found_msg))?;
    let workspace_id = repo
        .task_workspace_id(task_id)
        .await
        .map_err(db_err)?
        .ok_or_else(|| not_found(not_found_msg))?;
    require_workspace_access(state, auth, workspace_id, not_found_msg).await?;
    Ok((task, workspace_id))
}

// ---------------------------------------------------------------------------
// 解析 / 格式化
// ---------------------------------------------------------------------------

/// `:<field>` 路径参数：非法 → 400 `<field> must be a uuid`（上游 `parseUUID` panic→500，
/// 本地沿用 M3-4 的 400 约定，登记为偏离 D-5）。
pub(crate) fn parse_path_id(field: &str, raw: &str) -> Result<Id, ApiError> {
    Id::parse(raw.trim()).map_err(|_| validation(format!("{field} must be a uuid")))
}

/// upstream `normalizeProvider`：trim + lowercase；空串返回空串。
#[must_use]
pub(crate) fn normalize_provider(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// upstream `timestampToString`：RFC3339 秒精度 UTC `Z`。
#[must_use]
pub(crate) fn timestamp(value: chrono::DateTime<chrono::Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 可选时间戳 → `Option<String>`（`None` 落 `null`，偏离 D-3 见模块文档）。
#[must_use]
pub(crate) fn timestamp_opt(value: Option<chrono::DateTime<chrono::Utc>>) -> Option<String> {
    value.map(timestamp)
}
