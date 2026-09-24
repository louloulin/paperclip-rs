//! 插件安装生命周期路由：列表 / 安装 / 预览 / 卸载 / 配置 / 启停 / 令牌（**9 个注册键**）。
//!
//! - **写者**：M6-5（`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin.go`（+ `internal/service/plugin.go` 的 `DeploymentKey` 派生）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins` | GET, POST | `router.go:1690` / `1736` |
//! | `/api/workspaces/:id/plugins/preview` | POST | `router.go:1735` |
//! | `/api/workspaces/:id/plugins/:installationId` | DELETE | `router.go:1747` |
//! | `/api/workspaces/:id/plugins/:installationId/config` | PUT | `router.go:1744` |
//! | `/api/workspaces/:id/plugins/:installationId/enable` | POST | `router.go:1745` |
//! | `/api/workspaces/:id/plugins/:installationId/disable` | POST | `router.go:1746` |
//! | `/api/workspaces/:id/plugins/:installationId/token` | POST, DELETE | `router.go:1738-1739` |
//!
//! - **部署密钥的唯一 egress**：hook 签名/加密要用部署密钥。本仓的读入口是
//!   `state.plugin_key()`（`Option<&PluginSecretKey>`，读 `MULTICA_PLUGIN_SECRET_KEY`）。
//!   未配置时**不得 panic / 不得用零密钥**：按上游口径降级成明确错误码
//!   （`plugin_disabled`；surface 面是 `plugin_surfaces_not_configured`）。
//!   ⚠️ 本文件**不要**自己读环境变量。
//! - **令牌**：明文只在签发响应里出现一次；库里只有 `token_hash`（`mpc_` 回调令牌根本不落库）。
//!   轮换要同时更新 `token_hash` + `token_rotated_at`。
//! - **不做什么**：不做包管理（`packages.rs`）、不做运行时面（`mcp.rs` / `surface_launch.rs`）。
//!
//! ## 本片登记在 `docs/32` §9 的偏离（三条）
//!
//! 1. **`plugins_v1` 特征开关的口径反转**：上游 `requirePluginsV1` 默认 `false`（⇒ 管理面恒 403
//!    `plugin_api_disabled`）。本仓 `FeatureFlagCatalog` 没有持久化后端（`state.rs` 建的是空目录，
//!    全仓无任何 `register` 调用点），若照抄默认值，M6 整个管理面将**永远**返回 403 —— 而它正是
//!    本波的交付物。故本地口径为「**未登记 = 开启**，显式登记为 `false` 才 403」，同码同体
//!    （`plugin_api_disabled` / `Plugin management is not enabled`）。`/v1` 面的「关闭 ⇒ 403」契约
//!    归 M6-7，它自己登记那条。
//! 2. **`hooks[].schedule` 恒缺省**：上游 `pluginInstallationPayload` 从 `plugin_hook_schedule` 读日程，
//!    并在安装/升级/启停时 `reconcilePluginHookSchedules`。该表的读写**全部**归 M6-8（`mc-repos` 的
//!    `plugin/hook.rs` 在本波是 doc-only 桩，无读口），而 M6-5 的写集只有
//!    `installation/package/skill` 三个文件 ⇒ 本片既不写日程也不读日程，DTO 里该字段始终不出现。
//!    这是**跨片缺口**（M6-8 落地后需回填 payload + 安装/启停三处 reconcile），不是静默省略。
//! 3. **错误信封的码集**：本仓是 `{"error":{"code","message"}}`（上游是 `{"error":"..."}`）。插件面新增一个
//!    本地码 `plugin_unavailable`（映射上游 `PluginErrorUnavailable` ⇒ 502），其余码复用
//!    `mc_errors::Error` 的既有取值（`validation_error` / `not_found` / `conflict` / `forbidden` /
//!    `unprocessable`）。
//!
//! ## 文件布局（门 ⑩：单文件 800 行硬上限）
//!
//! 本文件只留**共享管路**（错误信封 / 门 / 读取助手 / DTO 投影 / 配置校验 / 事务薄封装）
//! 与 `router()`；handler 按「装什么」分在两个子模块里：
//!
//! - `install/lifecycle.rs`：列表 / 预览 / 安装（含原地升级）/ 卸载（5 条路由）；
//! - `install/settings.rs`：配置 / 启停 / 令牌轮换与吊销（5 条路由）。
//!
//! ⚠️ `routes/plugins/mod.rs`（M6-0 anchor，**冻结**）把 M6-5 的子路由固定在 `install` / `packages`
//! 两个文件上，所以这里**不能**新增顶层 `mod`；子模块目录是唯一不碰冻结面的拆分方式。
//!
//! **状态：M6-5 已落地**。
//!
//! 行预算（门 ⑩）：本文件 ≤620 行；`install/lifecycle.rs` ≤400；`install/settings.rs` ≤260。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use mc_core::Id;
use mc_feature_flags::FeatureKey;
use mc_plugin_host::capabilities::{
    check_capabilities, CapabilityUnavailable, ResourceType, HOST_CAPABILITIES,
};
use mc_plugin_host::credentials::{
    hook_signing_secret, secret_box, CredentialError, DeploymentKey,
};
use mc_plugin_host::manifest::{ConfigField, Hook, Manifest};
use mc_plugin_host::token::{hash_token, issue_install_token};
use mc_repos::plugin::installation::{
    self as installations, InstallationRepo, InstallationRow, NewInstallation, UpgradeInstallation,
};
use mc_repos::plugin::package::{self as packages, PackageRepo, PackageVersionRow};
use mc_repos::plugin::skill::{sync_tx as sync_plugin_skills, PluginSkillInput};
use mc_repos::RepoError;
use mc_skill::frontmatter::parse_skill_frontmatter;

use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

// 子模块：`mod.rs`（M6-0）冻结在 5 个文件上，所以门 ⑩ 的 800 行硬上限只能靠
// **子模块目录**消化 —— `install.rs` 声明的 `mod x;` 解析到 `install/x.rs`。
mod lifecycle;
mod settings;

/// 上游 `featureflags.PluginsV1`。
const PLUGINS_V1: &str = "plugins_v1";

/// 与 `routes/runtimes/access.rs::ADMIN_REQUIRED` 逐字相同（那个模块是 `runtimes` 私有的）。
const ADMIN_REQUIRED: &str = "workspace admin role required";

/// 上游 `service.MaxPluginSecretBytes`。
const MAX_PLUGIN_SECRET_BYTES: usize = 8192;

/// 上游 `plugincontract.ConfigString` 的最大长度。
const MAX_CONFIG_STRING_BYTES: usize = 4096;

/// `/api/workspaces/:id/plugins*` 的安装面（M6-5 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/plugins",
            get(lifecycle::list_plugins).post(lifecycle::install_plugin),
        )
        .route(
            "/api/workspaces/:id/plugins/preview",
            post(lifecycle::preview_plugin),
        )
        .route(
            "/api/workspaces/:id/plugins/:installationId",
            delete(lifecycle::uninstall_plugin),
        )
        .route(
            "/api/workspaces/:id/plugins/:installationId/config",
            put(settings::configure_plugin),
        )
        .route(
            "/api/workspaces/:id/plugins/:installationId/enable",
            post(settings::enable_plugin),
        )
        .route(
            "/api/workspaces/:id/plugins/:installationId/disable",
            post(settings::disable_plugin),
        )
        .route(
            "/api/workspaces/:id/plugins/:installationId/token",
            post(settings::rotate_plugin_token).delete(settings::revoke_plugin_token),
        )
}

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 插件面错误：状态码 + 稳定码 + 上游文案。
///
/// 上游用 `service.PluginError{Kind, Message}` 让 handler 不必按文案猜状态码；本仓保留这层
/// 区分，但把 Kind 直接落成 `(status, code)`，好让 `docs/32` 的码集是**枚举出来的**而不是推导的。
#[derive(Debug)]
pub(super) struct PluginError {
    pub(super) status: StatusCode,
    pub(super) code: &'static str,
    pub(super) message: String,
}

pub(super) type PluginResult<T> = Result<T, PluginError>;

impl PluginError {
    pub(super) fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// 上游 `PluginErrorInvalid` ⇒ 400。
    pub(super) fn invalid(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "validation_error", message)
    }

    /// 上游 `PluginErrorNotFound` ⇒ 404。
    pub(super) fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    /// 上游 `PluginErrorConflict` ⇒ 409。
    pub(super) fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    /// 上游 `PluginErrorForbidden` ⇒ 403。
    pub(super) fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", message)
    }

    /// 上游 `PluginErrorIncompatible` ⇒ 422。
    pub(super) fn incompatible(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, "unprocessable", message)
    }

    /// 上游 `PluginErrorQuota` ⇒ 507（`docs/32` §9 已登记）。
    ///
    /// 本片（M6-5）**不产生**它：上游只在 `plugin_storage.go`（存储配额）与 `plugin_hook.go`
    /// （hook 每分钟调用上限）里抛出 —— 两个面分别归 M6-6 / M6-8。留着它是为了让那两片**直接用**
    /// 同一张状态码表，而不是各自再补一个 507（本文件是 `PluginError` 的唯一所有者）。
    #[allow(dead_code)]
    pub(super) fn quota(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            message,
        )
    }

    /// 上游 `PluginErrorUnavailable` ⇒ 502（默认分支）。
    pub(super) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, "plugin_unavailable", message)
    }

    /// 上游 `writeError(413, …)`：`mc_errors` 没有 413 变体，码与
    /// `routes/tasks/builder.rs::payload_too_large` 逐字相同。
    pub(super) fn too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", message)
    }

    /// 上游 `requirePluginsV1` 的响应（见文件头偏离 1）。
    pub(super) fn feature_disabled() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "plugin_api_disabled",
            "Plugin management is not enabled",
        )
    }
}

impl From<mc_errors::Error> for PluginError {
    /// 让成员/角色门的 `mc_errors::Error` 走同一个渲染器（体与 [`crate::error::ApiError`] 逐字相同）。
    fn from(err: mc_errors::Error) -> Self {
        Self {
            status: StatusCode::from_u16(err.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: err.code(),
            message: err.message(),
        }
    }
}

impl IntoResponse for PluginError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": { "code": self.code, "message": self.message } })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// 门 / 读取助手
// ---------------------------------------------------------------------------

/// 上游 `pluginsV1Enabled`：显式登记为 `false` 才关（见文件头偏离 1）。
fn plugins_v1_enabled(state: &AppState) -> bool {
    state
        .feature_flags
        .get(&FeatureKey::new(PLUGINS_V1))
        .is_none_or(|flag| flag.enabled)
}

/// 成员角色（`member` 表的 `role`）；非成员 ⇒ `None`。
///
/// 复用 `mc-repos` 的既有读口（`GetMemberByUserAndWorkspace`），**不在这里另写一条 SELECT**：
/// 成员语义（例如将来加 `pending` 状态）只应有一个实现。
async fn member_role(
    state: &AppState,
    workspace_id: Id,
    user_id: Id,
) -> PluginResult<Option<String>> {
    mc_repos::wakeup::lookup::member_role(state.db.pool(), workspace_id.0, user_id.0)
        .await
        .map_err(|error| PluginError::from(mc_errors::Error::Database(error.to_string())))
}

/// 上游 `requirePluginsV1`。
pub(super) fn require_plugins_v1(state: &AppState) -> PluginResult<()> {
    if plugins_v1_enabled(state) {
        Ok(())
    } else {
        Err(PluginError::feature_disabled())
    }
}

/// 上游 `parseUUIDOrBadRequest(w, raw, "workspace_id")`；本仓口径见 `AgentScope::resolve`。
pub(super) fn parse_workspace_id(raw: &str) -> PluginResult<Id> {
    Id::parse(raw.trim()).map_err(|_| {
        PluginError::from(mc_errors::Error::Validation {
            message: "workspace_id must be a valid uuid".to_string(),
            details: vec![],
        })
    })
}

/// 非成员（含 workspace 不存在）⇒ 404 `workspace`，形态与其余路由族逐字一致。
fn workspace_not_found() -> PluginError {
    PluginError::from(mc_errors::Error::NotFound {
        resource: "workspace".to_string(),
    })
}

/// 路径段 workspace + 成员门：非法 id ⇒ 400，非成员 ⇒ 404 `workspace`
/// （上游中间件 `RequireWorkspaceMemberFromURL` 在前，语义是「非成员不可见」）。
pub(super) async fn workspace_member(state: &AppState, raw: &str, user_id: Id) -> PluginResult<Id> {
    let workspace_id = parse_workspace_id(raw)?;
    if member_role(state, workspace_id, user_id).await?.is_none() {
        return Err(workspace_not_found());
    }
    Ok(workspace_id)
}

/// 写路由的 workspace 解析：成员 + owner/admin（角色不足 ⇒ 403）。
///
/// 等价于上游路由组的 `RequireWorkspaceRoleFromURL(queries, "id", "owner", "admin")`。
pub(super) async fn workspace_admin(state: &AppState, raw: &str, user_id: Id) -> PluginResult<Id> {
    let workspace_id = parse_workspace_id(raw)?;
    match member_role(state, workspace_id, user_id).await? {
        Some(role) if mc_repos::agent::role_is_admin(&role) => Ok(workspace_id),
        Some(_) => Err(PluginError::forbidden(ADMIN_REQUIRED)),
        None => Err(workspace_not_found()),
    }
}

/// 上游 `pluginInstallationFromURL`：安装行必须属于路径里的 workspace。
async fn installation_for_workspace(
    state: &AppState,
    workspace_id: Id,
    raw: &str,
) -> PluginResult<InstallationRow> {
    let Ok(id) = Id::parse(raw.trim()) else {
        return Err(PluginError::not_found("plugin installation not found"));
    };
    installation_repo(state)
        .get(workspace_id, id)
        .await
        .map_err(|err| match err {
            RepoError::NotFound => PluginError::not_found("plugin installation not found"),
            _ => PluginError::unavailable("load the Plugin"),
        })
}

/// 上游 `VersionForWorkspace`：版本必须属于路径里的 workspace（先无锁拒明显非法入参）。
async fn version_for_workspace(
    state: &AppState,
    workspace_id: Id,
    raw: &str,
) -> PluginResult<PackageVersionRow> {
    let Ok(id) = Id::parse(raw.trim()) else {
        return Err(PluginError::not_found("published plugin version not found"));
    };
    package_repo(state)
        .get_version(workspace_id, id)
        .await
        .map_err(|err| match err {
            RepoError::NotFound => PluginError::not_found("published plugin version not found"),
            _ => PluginError::unavailable("load published plugin version"),
        })
}

fn installation_repo(state: &AppState) -> InstallationRepo {
    InstallationRepo::new(state.db.clone())
}

pub(super) fn package_repo(state: &AppState) -> PackageRepo {
    PackageRepo::new(state.db.clone())
}

/// 部署密钥的**唯一**转写点（未配置 ⇒ `None`，调用方按缺失处理，绝不用零密钥兜底）。
fn deployment_key(state: &AppState) -> Option<DeploymentKey> {
    state
        .plugin_key
        .as_ref()
        .and_then(|key| DeploymentKey::new(key.as_bytes()))
}

/// 请求体解码：形状不符 / 空 body → 400 `invalid request body`（上游 `json.Decoder` 同判）。
pub(super) fn decode<T: DeserializeOwned>(body: &Bytes) -> PluginResult<T> {
    if body.is_empty() {
        return Err(PluginError::invalid("invalid request body"));
    }
    serde_json::from_slice(body).map_err(|_| PluginError::invalid("invalid request body"))
}

/// 凭据派生的失败文案 —— 直通 `CredentialError` 的 `Display`。
///
/// `mc-plugin-host` 的每个变体文案都是照上游逐字抄的（例如 `PluginSecretsDisabled` ⇒
/// `plugin secrets are disabled: MULTICA_PLUGIN_SECRET_KEY is not configured`），
/// 所以这里**不要**再包一层前缀，否则同一件事会有第二种说法。
fn credential_message(err: &CredentialError) -> String {
    err.to_string()
}

/// 上游 `timestampToString`：RFC3339、秒精度、UTC `Z`。
pub(super) fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// 封一条密钥明文（上游 `Secrets.Seal`）。
///
/// ⚠️ `ThreadRng` **不是** `Send`：它一旦活过任何 `.await`，整个 handler 的 future 就不再是
/// `Send`，axum 会在 `.route(...)` 处报「`Handler` 未实现」而不是在 encrypt 处报错。
/// 所以这里必须是独立函数（作用域内无 `.await`），**不要**把 `thread_rng()` 提到调用方的循环外。
fn seal_secret(key: Option<&DeploymentKey>, plaintext: &str) -> PluginResult<Vec<u8>> {
    let mut rng = rand::thread_rng();
    secret_box(key)
        .and_then(|boxed| boxed.seal(plaintext.as_bytes(), &mut rng))
        .map_err(|error| PluginError::unavailable(credential_message(&error)))
}

/// 签发一枚安装令牌（上游 `IssueInstallToken`）。作用域约束同 [`seal_secret`]。
fn new_install_token() -> PluginResult<String> {
    let mut rng = rand::thread_rng();
    issue_install_token(&mut rng).map_err(|_| PluginError::unavailable("generate install token"))
}

// ---------------------------------------------------------------------------
// manifest / DTO 投影
// ---------------------------------------------------------------------------

/// 上游 `ManifestForVersion`：读回发布时冻结的 manifest，**绝不**重新抓取。
fn manifest_of_version(version: &PackageVersionRow) -> PluginResult<Manifest> {
    serde_json::from_value(version.manifest.0.clone())
        .map_err(|_| PluginError::invalid("published plugin manifest is unreadable"))
}

/// 上游 `ParseInstallationManifest`：读回**管理员同意过**的快照。
fn installation_manifest(installation: &InstallationRow) -> PluginResult<Manifest> {
    serde_json::from_value(installation.manifest.0.clone())
        .map_err(|_| PluginError::invalid("stored plugin manifest is unreadable"))
}

/// 上游 `Manifest.CheckCapabilities` 的 422 形态。
pub(super) fn require_supported(manifest: &Manifest) -> PluginResult<()> {
    check_capabilities(manifest, &HOST_CAPABILITIES)
        .map_err(|missing| PluginError::incompatible(capability_message(&missing)))
}

/// 上游 `capabilityMessage`。
pub(super) fn capability_message(missing: &CapabilityUnavailable) -> String {
    format!(
        "This plugin declares capabilities that are not enabled yet: {}",
        missing.missing.join(", ")
    )
}

/// 上游 `requireExactScopes`：部分同意会让插件静默坏掉，多同意是管理员没被展示过的权限。
fn require_exact_scopes(manifest_scopes: &[String], granted: &[String]) -> PluginResult<()> {
    if manifest_scopes.len() != granted.len() {
        return Err(PluginError::conflict(
            "granted_scopes must match the manifest scopes exactly",
        ));
    }
    for scope in granted {
        if !manifest_scopes.contains(scope) {
            return Err(PluginError::conflict(format!(
                "granted_scopes contains {scope:?}, which the manifest does not request"
            )));
        }
    }
    Ok(())
}

/// 上游 `ConfigFieldsForManifest`：按声明顺序摊平（客户端渲染的表单与服务端校验同一份）。
fn config_fields(manifest: &Manifest) -> Vec<Value> {
    manifest
        .config
        .fields
        .iter()
        .map(config_field_payload)
        .collect()
}

fn config_field_payload(field: &ConfigField) -> Value {
    let mut out = Map::new();
    out.insert("key".into(), json!(field.key));
    out.insert("type".into(), json!(field.kind));
    out.insert("label".into(), json!(field.label));
    if !field.description.is_empty() {
        out.insert("description".into(), json!(field.description));
    }
    out.insert("required".into(), json!(field.required));
    if !field.options.is_empty() {
        out.insert("options".into(), json!(field.options));
    }
    if !field.placeholder.is_empty() {
        out.insert("placeholder".into(), json!(field.placeholder));
    }
    if field.multiline {
        out.insert("multiline".into(), json!(true));
    }
    Value::Object(out)
}

/// 上游 `pluginHookResponse`：**不下发** `input_schema`（设置页只需要知道 hook 是什么、谁能调）。
fn hook_payload(hook: &Hook) -> Value {
    let mut out = Map::new();
    out.insert("key".into(), json!(hook.key));
    out.insert("name".into(), json!(hook.name));
    out.insert("description".into(), json!(hook.description));
    out.insert("triggers".into(), json!(hook.triggers));
    if !hook.events.is_empty() {
        out.insert("events".into(), json!(hook.events));
    }
    out.insert("transport".into(), json!(hook.transport.kind));
    // `schedule` 见文件头偏离 2（本波恒缺省）。
    Value::Object(out)
}

/// 上游 `pluginInstallationResponse`：**永不携带 secret 值**（`config` 只有非 secret 字段，
/// secret 只以**键名**出现在 `configured_secrets`）。
async fn installation_payload(state: &AppState, row: &InstallationRow) -> PluginResult<Value> {
    let manifest = installation_manifest(row)?;
    let secrets = installation_repo(state)
        .secret_keys(row.id())
        .await
        .map_err(|_| PluginError::unavailable("list plugin secrets"))?;

    let mut out = Map::new();
    out.insert("id".into(), json!(row.id.to_string()));
    out.insert("plugin_key".into(), json!(row.plugin_key));
    out.insert("name".into(), json!(manifest.name));
    if !manifest.description.is_empty() {
        out.insert("description".into(), json!(manifest.description));
    }
    out.insert("version".into(), json!(row.version));
    out.insert(
        "package_version_id".into(),
        json!(row.package_version_id.to_string()),
    );
    out.insert("enabled".into(), json!(row.enabled));
    out.insert("granted_scopes".into(), json!(row.granted_scopes()));
    out.insert("config_schema".into(), json!(config_fields(&manifest)));
    out.insert("config".into(), Value::Object(row.config_object()));
    out.insert("configured_secrets".into(), json!(secrets));
    out.insert("surfaces".into(), json!(manifest.contributes.surfaces));
    out.insert(
        "hooks".into(),
        Value::Array(
            manifest
                .contributes
                .hooks
                .iter()
                .map(hook_payload)
                .collect(),
        ),
    );
    out.insert("resources".into(), json!(manifest.contributes.resources));
    out.insert("created_at".into(), json!(timestamp(row.created_at)));
    out.insert("updated_at".into(), json!(timestamp(row.updated_at)));
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------------------
// 配置校验（上游 `normalizeConfigValue`）
// ---------------------------------------------------------------------------

fn is_secret_field(manifest: &Manifest, key: &str) -> bool {
    manifest
        .config
        .field(key)
        .is_some_and(|field| field.kind == SECRET_KIND)
}

const SECRET_KIND: &str = "secret";

fn normalize_config_value(field: &ConfigField, value: &Value) -> PluginResult<Value> {
    let label = format!("config field {:?}", field.key);
    match field.kind.as_str() {
        "string" => {
            let Some(text) = value.as_str() else {
                return Err(PluginError::invalid(format!("{label} must be a string")));
            };
            if text.len() > MAX_CONFIG_STRING_BYTES {
                return Err(PluginError::invalid(format!(
                    "{label} exceeds {MAX_CONFIG_STRING_BYTES} bytes"
                )));
            }
            Ok(Value::String(text.to_string()))
        }
        "number" => {
            if value.as_f64().is_none() {
                return Err(PluginError::invalid(format!("{label} must be a number")));
            }
            Ok(value.clone())
        }
        "bool" => {
            if !value.is_boolean() {
                return Err(PluginError::invalid(format!("{label} must be a boolean")));
            }
            Ok(value.clone())
        }
        "enum" => {
            let Some(text) = value.as_str() else {
                return Err(PluginError::invalid(format!("{label} must be a string")));
            };
            if !field.options.iter().any(|option| option == text) {
                return Err(PluginError::invalid(format!(
                    "{label} must be one of the declared options"
                )));
            }
            Ok(Value::String(text.to_string()))
        }
        "secret" => Err(PluginError::invalid(format!(
            "{label} must be submitted through the secret path"
        ))),
        _ => Err(PluginError::invalid(format!(
            "{label} has an unsupported type"
        ))),
    }
}

/// 上游 `pruneConfig`：新 manifest 丢掉过的字段就地剪掉，而不是留成够不着的状态。
fn prune_config(existing: &Map<String, Value>, manifest: &Manifest) -> Map<String, Value> {
    existing
        .iter()
        .filter(|(key, _)| manifest.config.field(key).is_some())
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

// ---------------------------------------------------------------------------
// 事务薄封装（把 sqlx 的错误折成插件面的 502）
// ---------------------------------------------------------------------------

/// 开事务（`sqlx::Pool::begin` 的返回类型是 `Transaction<'static, Postgres>`，与仓储的 `Tx<'a>` 同型）。
///
/// 池耗尽 / 连接断开在这里只表现为「拿不到事务」，所以统一折成插件面的 502。
/// **不要**改回 `unwrap`：panic 会让 axum 丢掉整个连接，客户端连错误码都拿不到。
pub(super) async fn begin(
    state: &AppState,
    fallback: &str,
) -> PluginResult<installations::Tx<'static>> {
    state
        .db
        .pool()
        .begin()
        .await
        .map_err(|_| PluginError::unavailable(fallback))
}

/// 提交事务。
pub(super) async fn commit(tx: installations::Tx<'static>, fallback: &str) -> PluginResult<()> {
    tx.commit()
        .await
        .map_err(|_| PluginError::unavailable(fallback))
}
