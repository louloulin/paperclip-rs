//! 凭据解析（上游 `internal/handler/plugin_action.go` 的 `pluginCaller` 一族）。
//!
//! 这一半从 `policy.rs` 拆出来，理由有二：① 门 ⑩ 的单文件 800 行硬上限；② 它是**纯解析**
//! （凭据 → 谁在说话），与 `policy.rs` 的**传输层**（问题体 / 限流 / 凭据前缀门）是两件事。
//! 两者同属 `routes::v1::policy` 模块树 —— 调用方仍写 `policy::resolve_caller(...)`。
//!
//! ⚠️ 本文件**不**做任何写操作、**不**碰限流：改这里是改「谁被允许说话」，改 `policy.rs` 是
//! 改「怎么说话」。

use mc_core::plugin::PluginTokenKind;
use mc_core::Id;
use mc_feature_flags::FeatureKey;
use mc_plugin_host::token::{hash_token, ActorKind, TokenError};
use mc_repos::plugin::installation::{self as installations, InstallationRow};

use super::{
    bearer_token, callback_tokens, is_plugin_bearer_token, ActionError, ActionResult, PLUGINS_V1,
};
use crate::state::AppState;

/// 一次 Action 调用**是谁在说话**（上游 `pluginActor`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionActor {
    /// 有真人：会话调用，或 `ui`/`manual` hook 的回调令牌。写归属到那个人。
    Member(Id),
    /// 没有真人：安装令牌，或 `event` hook 的回调令牌。写归属到安装本身。
    Plugin,
}

/// 一次已授权调用（上游 `PluginActionCaller`）：哪个安装、在哪个 workspace、替谁。
#[derive(Debug, Clone)]
pub(crate) struct ActionCaller {
    pub(crate) installation: InstallationRow,
    pub(crate) workspace_id: Id,
    /// 已同意的 scope 集合（读**安装行**，不是今天源地址吐出来的 manifest）。
    pub(crate) scopes: Vec<String>,
    /// 回调令牌把调用**收窄**到的那个 issue（`None` = 不限）。
    pub(crate) issue_scope: Option<Id>,
    pub(crate) actor: ActionActor,
}

impl ActionCaller {
    /// 上游 `pluginActor.isMember`。
    pub(crate) fn is_member(&self) -> bool {
        matches!(self.actor, ActionActor::Member(_))
    }

    /// 上游 `pluginActor.requireMember`：没有真人的端点（`storage:user`）显式拒绝。
    pub(crate) fn require_member(&self) -> ActionResult<Id> {
        match self.actor {
            ActionActor::Member(user_id) => Ok(user_id),
            ActionActor::Plugin => Err(ActionError::new(
                axum::http::StatusCode::FORBIDDEN,
                "member_required",
                "this endpoint requires a user; the presented token acts as the Plugin itself",
            )),
        }
    }

    /// 已授予的某个 scope（上游 `hasGrantedScope`）。
    pub(crate) fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|granted| granted == scope)
    }
}

/// 上游 `pluginsV1Enabled`：显式登记为 `false` 才关（见 `policy.rs` 文件头偏离 1）。
pub(crate) fn plugins_v1_enabled(state: &AppState) -> bool {
    state
        .feature_flags
        .get(&FeatureKey::new(PLUGINS_V1))
        .is_none_or(|flag| flag.enabled)
}

/// 上游 `requirePluginActionV1`：开关关闭 ⇒ 403 `plugin_api_disabled`。
///
/// # Errors
///
/// [`ActionError`]（403 `plugin_api_disabled`）。
pub(crate) fn require_plugins_v1(state: &AppState) -> ActionResult<()> {
    if plugins_v1_enabled(state) {
        Ok(())
    } else {
        Err(ActionError::new(
            axum::http::StatusCode::FORBIDDEN,
            "plugin_api_disabled",
            "Plugin management is not enabled",
        ))
    }
}

/// 凭据 + 安装 + scope 的三步授权（上游 `pluginCaller` → `pluginTokenCaller` /
/// `pluginSessionCaller` → `AuthorizePluginAction`）。
///
/// 它**故意不**判断「调用者能否碰这个资源」—— 那是第三步，留在普通资源读取器里
/// （插件因此恰好继承其身后那个人的权限，不多一份会漂移的权限规则副本）。
///
/// # Errors
///
/// 见 [`ActionError`] 的各构造点；文案与上游逐字对齐。
pub(crate) async fn resolve_caller(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    scope: &str,
) -> ActionResult<ActionCaller> {
    require_plugins_v1(state)?;
    let token = bearer_token(headers);
    if is_plugin_bearer_token(&token) {
        token_caller(state, &token, scope).await
    } else {
        session_caller(state, headers, scope).await
    }
}

/// 令牌路径（上游 `pluginTokenCaller`）。
async fn token_caller(state: &AppState, token: &str, scope: &str) -> ActionResult<ActionCaller> {
    let mut member_user_id: Option<Id> = None;
    let mut issue_scope: Option<Id> = None;

    let installation = if token
        .trim_start()
        .starts_with(PluginTokenKind::Callback.prefix())
    {
        let grant = callback_tokens()
            .resolve(token)
            .map_err(|error| callback_token_error(&error))?;
        issue_scope = grant.issue_id;
        if grant.actor.kind == ActorKind::Member {
            member_user_id = Some(grant.actor.id);
        }
        installation_by_id(state, grant.installation_id).await?
    } else {
        authenticate_install_token(state, token).await?
    };

    let mut caller = authorize(installation, member_user_id, scope)?;
    // 回调令牌说它针对哪个 issue；带上是「把注释变成检查」的那一步（见 `plugin_issue_for_caller`）。
    caller.issue_scope = issue_scope;

    // 代表某个人的回调令牌，只和那个人**今天**的成员身份一样有效。这里再查一次，
    // 意味着撤销某人的权限对已发出的令牌立刻生效。
    if let Some(user_id) = member_user_id {
        if member_role(state, caller.workspace_id, user_id)
            .await?
            .is_none()
        {
            return Err(ActionError::new(
                axum::http::StatusCode::FORBIDDEN,
                "actor_membership_revoked",
                "the user this callback acts for is no longer a member",
            ));
        }
    }
    Ok(caller)
}

/// 会话路径（上游 `pluginSessionCaller`）：真正的登录用户，其自身权限就是插件能触达的上限。
async fn session_caller(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    scope: &str,
) -> ActionResult<ActionCaller> {
    let user = headers
        .get(crate::routes::auth_user::USER_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ActionError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing authenticated user",
            )
        })?;
    let user_id =
        Id::parse(user).map_err(|_| ActionError::invalid("user_id must be a valid UUID"))?;

    let installation_header = headers
        .get("x-multica-plugin-installation")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();

    let caller = authorize(
        installation_by_id_or_header(state, &installation_header).await?,
        Some(user_id),
        scope,
    )?;

    // workspace 来自安装行（绝不是客户端头）：否则调用方能把一个安装指向它从未被安装过的
    // workspace。成员身份随后按**那个** workspace 判定。
    if member_role(state, caller.workspace_id, user_id)
        .await?
        .is_none()
    {
        return Err(ActionError::not_found("workspace not found"));
    }
    Ok(caller)
}

/// 上游 `AuthorizePluginAction`：安装真实且启用，且持有本次调用需要的 scope。
fn authorize(
    installation: InstallationRow,
    user_id: Option<Id>,
    scope: &str,
) -> ActionResult<ActionCaller> {
    // 关掉的插件就是关掉：一个留在旧标签页里的 iframe 不能在管理员停用它之后继续工作。
    if !installation.enabled {
        return Err(ActionError::forbidden("this Plugin is disabled"));
    }
    let scopes = installation.granted_scopes();
    if !scope.is_empty() && !scopes.iter().any(|granted| granted == scope) {
        return Err(ActionError::forbidden(format!(
            "this Plugin was not granted the {scope} scope"
        )));
    }
    Ok(ActionCaller {
        workspace_id: installation.workspace_id(),
        installation,
        scopes,
        issue_scope: None,
        // 身份由**怎么认证**决定，不由任何请求字段决定：会话 / 代表某人的回调令牌 ⇒ 成员；
        // 安装令牌 / 无人的回调令牌 ⇒ 安装本身。
        actor: user_id.map_or(ActionActor::Plugin, ActionActor::Member),
    })
}

/// 安装头/路径段解析成安装行（上游 `parseInstallationID` + `GetPluginInstallation`）。
///
/// 空串 ⇒ 400（`plugin installation is required`）；非 uuid ⇒ **404**（上游把「不存在的安装」
/// 与「格式不对的安装」合成同一句，免得调用方能确认一个它读不到的 id 存在）。
async fn installation_by_id_or_header(
    state: &AppState,
    raw: &str,
) -> ActionResult<InstallationRow> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ActionError::invalid("plugin installation is required"));
    }
    let Ok(id) = Id::parse(raw) else {
        return Err(ActionError::not_found("plugin installation not found"));
    };
    installation_by_id(state, id).await
}

async fn installation_by_id(state: &AppState, id: Id) -> ActionResult<InstallationRow> {
    sqlx::query_as::<_, InstallationRow>(&format!(
        "SELECT {} FROM plugin_installation WHERE id = $1",
        installations::COLUMNS
    ))
    .bind(id.0)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|error| ActionError::unavailable(format!("load plugin installation: {error}")))?
    .ok_or_else(|| ActionError::not_found("plugin installation not found"))
}

/// 上游 `AuthenticateInstallToken`：只认 `mpi_`，按 `sha256` 哈希查安装行。
async fn authenticate_install_token(
    state: &AppState,
    token: &str,
) -> ActionResult<InstallationRow> {
    let token = token.trim();
    if !token.starts_with(PluginTokenKind::Install.prefix()) {
        return Err(ActionError::forbidden("invalid plugin token"));
    }
    let hash = hash_token(token);
    // 见 `policy.rs` 文件头偏离 3：`mc-repos` 的 installation 仓储（M6-5 的写集）没有 token_hash
    // 读口，这里复用那个文件公开的**列投影常量**直查一次，列清单仍只有一份。
    let row = sqlx::query_as::<_, InstallationRow>(&format!(
        "SELECT {} FROM plugin_installation WHERE token_hash = $1",
        installations::COLUMNS
    ))
    .bind(&hash)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|error| ActionError::unavailable(format!("load plugin installation: {error}")))?
    .ok_or_else(|| ActionError::forbidden("invalid plugin token"))?;
    if !row.enabled {
        return Err(ActionError::forbidden("this Plugin is disabled"));
    }
    Ok(row)
}

/// 上游 `Callbacks.Resolve` 的错误映射（`ErrPluginTokenInvalid` ⇒ 403）。
fn callback_token_error(error: &TokenError) -> ActionError {
    match error {
        TokenError::InvalidCallbackToken | TokenError::CallbackTokenUnavailable => {
            ActionError::forbidden(error.to_string())
        }
        _ => ActionError::unavailable(error.to_string()),
    }
}

/// 成员角色（`member` 表的 `role`）；非成员 ⇒ `None`。
pub(crate) async fn member_role(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> ActionResult<Option<String>> {
    mc_repos::wakeup::lookup::member_role(state.db.pool(), workspace_id.0, user_id.0)
        .await
        .map_err(|error| ActionError::from(mc_errors::Error::Database(error.to_string())))
}
