//! VCS 连接管理面：**4 条**路由（`router.go:1676/1761/1763/1762`）—— 写者 **M8-2**（`LUM-1799`）。
//!
//! | 注册键 | 方法 | 授权层 | 未配置语义 |
//! | --- | :-: | --- | --- |
//! | `/api/workspaces/:id/vcs/connections` | GET | workspace **member** | `available:false`（200；不查库） |
//! | `/api/workspaces/:id/vcs/connections` | POST | workspace **admin** | 边界关 ⇒ **404**；缺密钥 ⇒ **403 `vcs_not_configured`** |
//! | `/api/workspaces/:id/vcs/connections/:connectionId/rotate-webhook` | POST | workspace **admin** | 同上 |
//! | `/api/workspaces/:id/vcs/connections/:connectionId` | DELETE | workspace **admin** | 无（只删本仓的行） |
//!
//! 授权层来自上游 `router.go`（member 组 1676、admin 组 1761–1763）。本仓没有 per-route 的
//! workspace 角色 middleware，所以**在 handler 起点**用 [`workspace_role`] 解析 —— 与
//! M8-1 的 `routes/github/install.rs` 同款。顺序也照上游「middleware 先于 handler」：
//! workspace（400/404）→ 角色（403）→ 产品边界（404）→ 密钥（403）。
//!
//! # ⚠️ 计划文档写 503，上游写 403 —— 本片照**上游**（登记在 `docs/32` §9.12）
//!
//! `docs/61` §2.5 的 VCS 行写「`isVCSConfigured()==false` ⇒ 503」。上游的实际代码是
//! `writeFeatureDisabled(...)`，其实现逐字是 `writeErrorCode(w, http.StatusForbidden, …)`
//! —— 注释也写明了理由：**被关掉的能力不是瞬时故障，回 503 会招来重试与告警噪音**。
//! 本片按上游 403 落地（M8-1 的 `repositories` 端点遇上同一个分歧，做了同一选择）。
//!
//! # 凭据纪律（`docs/61` §2.4 / §6.5 的 M8-2 行）
//!
//! PAT 与 webhook secret **只以 `secretbox` 密文的 base64 入库**（[`seal_secret`]）——
//! 明文**永不**进 SQL 参数以外的地方，响应里只有 connect/rotate 那一次性的
//! `webhook_secret`。本文件**没有任何** `tracing` 调用插值 token / secret。
//!
//! # provider registry
//!
//! [`provider_registry`] 是本片唯一的 registry 构造点（3 个 kind：`forgejo` / `gitea` /
//! `gitlab`）；`webhook.rs` 复用同一个函数，避免两处各注一份而漂移。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::agent::role_is_admin;
use mc_repos::vcs::connection::{NewVcsConnection, VcsConnectionRepo};
use mc_vcs::{Registry, VcsError};
use rand::RngCore;
use serde::Deserialize;

use crate::error::ApiResult;
use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, workspace_role};
use crate::routes::auth_user::AuthUser;
use crate::routes::vcs::dto::{
    error_with_code, VcsConnectResponse, VcsConnectionResponse, VcsConnectionsResponse,
    CODE_VCS_NOT_CONFIGURED,
};
use crate::state::AppState;

/// 本文件的路由切片。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/api/workspaces/:id/vcs/connections",
            get(list_connections).post(connect),
        )
        .route(
            "/api/workspaces/:id/vcs/connections/:connectionId/rotate-webhook",
            post(rotate_webhook),
        )
        .route(
            "/api/workspaces/:id/vcs/connections/:connectionId",
            delete(delete_connection),
        )
}

/// 本片的内建 provider registry（`forgejo` / `gitea` / `gitlab` 三个 kind）。
///
/// 两个 adapter 的 `register` 都跑 ⇒ `Registry::kinds()` 是**确定序**的三元组。
/// `webhook.rs` 用同一个函数（单一构造点）。
pub(crate) fn provider_registry() -> Registry {
    let mut registry = Registry::new();
    mc_vcs::forgejo::register(&mut registry);
    mc_vcs::gitlab::register(&mut registry);
    registry
}

// ---------------------------------------------------------------------------
// 调用上下文
// ---------------------------------------------------------------------------

/// 一次请求的 `(workspace, 调用者, 角色)`。
struct VcsScope {
    workspace_id: Id,
    user_id: Id,
    role: String,
}

impl VcsScope {
    /// workspace id（400）→ 成员身份（非成员 404 `workspace`）。
    async fn resolve(
        state: &AppState,
        user: AuthUser,
        raw_workspace_id: &str,
    ) -> Result<Self, Error> {
        let workspace_id = Id(parse_uuid(raw_workspace_id, "workspace id")?);
        let user_id = user.id();
        let role = workspace_role(state, workspace_id, user_id).await?;
        Ok(Self {
            workspace_id,
            user_id,
            role,
        })
    }

    /// 上游 `roleAllowed(member.Role, "owner", "admin")`。失败 ⇒ 403。
    fn require_admin(&self) -> Result<(), Error> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(forbidden("insufficient permissions"))
        }
    }

    fn is_admin(&self) -> bool {
        role_is_admin(&self.role)
    }
}

// ---------------------------------------------------------------------------
// GET /api/workspaces/{id}/vcs/connections
// ---------------------------------------------------------------------------

/// 上游 `ListVCSConnections`（`vcs.go:107`）：**member 可见**。
///
/// 产品边界关闭的部署（云端）回 `available:false` 且**不查库**（上游注释：那种部署上
/// 连接本来就不可能存在，connect 被拒）—— 其余三格恒为 `false`。
async fn list_connections(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(raw_workspace_id): Path<String>,
) -> ApiResult<Json<VcsConnectionsResponse>> {
    let scope = VcsScope::resolve(&state, user, &raw_workspace_id).await?;
    let available = state.vcs_keys.is_enabled();

    if !available {
        return Ok(Json(VcsConnectionsResponse {
            connections: Vec::new(),
            available: false,
            configured: false,
            can_manage: false,
        }));
    }

    let rows = VcsConnectionRepo::new(state.db.clone())
        .list_by_workspace(scope.workspace_id)
        .await
        .map_err(|_| Error::Database("failed to list connections".into()))?;

    Ok(Json(VcsConnectionsResponse {
        connections: rows.iter().map(VcsConnectionResponse::from_row).collect(),
        available,
        configured: state.vcs_keys.is_configured(),
        can_manage: scope.is_admin(),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/workspaces/{id}/vcs/connections
// ---------------------------------------------------------------------------

/// 上游 `connectVCSRequest`。
///
/// 全部字段 `#[serde(default)]`：Go 的 `json.Decoder.Decode(&struct)` 对**缺字段**留零值，
/// serde 默认却要求字段存在 ⇒ 不加 default 会把上游能处理的请求判成 400。调用侧用
/// `Option<Self>` 再把 `null` body 折成零值（Go 对 `null` 也是 no-op）。
#[derive(Debug, Default, Deserialize)]
struct ConnectVcsRequest {
    #[serde(default)]
    provider: String,
    #[serde(default)]
    instance_url: String,
    #[serde(default)]
    access_token: String,
}

/// 上游 `ConnectVCS`（`vcs.go:157`）。
///
/// 顺序逐条对齐：workspace → 角色（admin）→ 产品边界（404）→ 密钥（403）→ body（400）→
/// provider（400）→ `instance_url/token` 非空（400）→ URL 形态（400）→ `validate_token`
/// （401/402 之外 ⇒ 400 / 502）→ 铸新 secret → **两个 secret 都封** → upsert → 广播 →
/// 200 + 一次性明文。
async fn connect(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path(raw_workspace_id): Path<String>,
    body: Bytes,
) -> ApiResult<Response> {
    let scope = VcsScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_admin()?;

    if !state.vcs_keys.is_enabled() {
        return Ok(error_with_code(
            StatusCode::NOT_FOUND,
            "not_found",
            "vcs integration is not available on this deployment",
        ));
    }
    let Some(secret_box) = state.vcs_keys.secret_box() else {
        // 缺 `MULTICA_VCS_SECRET_KEY` ⇒ **绝不**落明文，回可机器识别的 403。
        return Ok(error_with_code(
            StatusCode::FORBIDDEN,
            CODE_VCS_NOT_CONFIGURED,
            "vcs integration not configured (MULTICA_VCS_SECRET_KEY unset)",
        ));
    };

    let request: ConnectVcsRequest = serde_json::from_slice::<Option<ConnectVcsRequest>>(&body)
        .map_err(|_| bad_request("invalid request body"))?
        .unwrap_or_default();

    let registry = provider_registry();
    let provider = match mc_core::vcs::VcsProviderKind::from_str(&request.provider) {
        Some(kind) => registry
            .get(kind)
            .map_err(|_| bad_request("unsupported provider"))?,
        // 未注册 / 不认识的 provider 都是 400 `unsupported provider`（上游 `vcs.For` 的
        // `ok=false` 一支在网页面上就是这个文案）。
        None => return Err(bad_request("unsupported provider").into()),
    };

    let instance_url = mc_vcs::forgejo::normalize_instance_url(&request.instance_url);
    let token = request.access_token.trim();
    if instance_url.is_empty() || token.is_empty() {
        return Err(bad_request("instance_url and access_token are required").into());
    }
    if !is_absolute_http_url(&instance_url) {
        return Err(bad_request("instance_url must be an absolute http(s) URL").into());
    }

    let account = match provider.validate_token(&instance_url, token).await {
        Ok(account) => account,
        Err(VcsError::Unauthorized) => {
            return Err(bad_request("the provider rejected the access token").into())
        }
        Err(_) => {
            // 传输/实例错误（`VcsError::Instance` / `Malformed`）⇒ 502。错误值本身**不含**
            // 凭据（`VcsError` 的每个变体只带原因），所以可以安全插值。
            return Ok(error_with_code(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "could not reach the provider instance",
            ));
        }
    };

    let webhook_secret = mint_webhook_secret();
    let token_encrypted = seal_secret(secret_box, token);
    let secret_encrypted = seal_secret(secret_box, &webhook_secret);
    let (Ok(token_encrypted), Ok(secret_encrypted)) = (token_encrypted, secret_encrypted) else {
        return Ok(error_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "failed to encrypt connection secrets",
        ));
    };

    let row = VcsConnectionRepo::new(state.db.clone())
        .upsert(NewVcsConnection {
            workspace_id: scope.workspace_id,
            provider: provider.kind().as_str().to_string(),
            instance_url,
            account_login: account.login,
            access_token_encrypted: token_encrypted,
            webhook_secret_encrypted: secret_encrypted,
            connected_by_id: Some(scope.user_id),
        })
        .await
        .map_err(|_| Error::Database("failed to save connection".into()))?;

    publish_connection_event(
        &state,
        raw_workspace_id.trim(),
        &row.id.to_string(),
        "created",
    );

    Ok(Json(VcsConnectResponse {
        connection: VcsConnectionResponse::from_row(&row),
        webhook_secret,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// POST /api/workspaces/{id}/vcs/connections/{connectionId}/rotate-webhook
// ---------------------------------------------------------------------------

/// 上游 `RotateVCSConnectionWebhook`（`vcs.go:274`）。
///
/// 旧 secret **立刻失效**（同一行同一列的 UPDATE）、新 secret **立刻生效**、明文**只此一次**
/// （本响应之后任何读面都拿不到它）。
async fn rotate_webhook(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((raw_workspace_id, raw_connection_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let scope = VcsScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_admin()?;
    let connection_id = Id(parse_uuid(&raw_connection_id, "connection id")?);

    if !state.vcs_keys.is_enabled() {
        return Ok(error_with_code(
            StatusCode::NOT_FOUND,
            "not_found",
            "vcs integration is not available on this deployment",
        ));
    }
    let Some(secret_box) = state.vcs_keys.secret_box() else {
        return Ok(error_with_code(
            StatusCode::FORBIDDEN,
            CODE_VCS_NOT_CONFIGURED,
            "vcs integration not configured (MULTICA_VCS_SECRET_KEY unset)",
        ));
    };

    // 上游「取行 → 比 workspace → 404」：跨 workspace 的行与不存在同判。
    let repo = VcsConnectionRepo::new(state.db.clone());
    let existing = repo
        .find_by_id(connection_id)
        .await
        .map_err(|_| Error::Database("failed to load vcs connection".into()))?
        .filter(|row| row.workspace_id() == scope.workspace_id);
    if existing.is_none() {
        return Err(not_found("vcs connection").into());
    }

    let webhook_secret = mint_webhook_secret();
    let Ok(secret_encrypted) = seal_secret(secret_box, &webhook_secret) else {
        return Ok(error_with_code(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "failed to encrypt webhook secret",
        ));
    };

    let rotated = repo
        .rotate_webhook_secret(connection_id, scope.workspace_id, &secret_encrypted)
        .await
        .map_err(|_| Error::Database("failed to rotate webhook secret".into()))?;

    publish_connection_event(
        &state,
        raw_workspace_id.trim(),
        &rotated.id.to_string(),
        "created",
    );

    Ok(Json(VcsConnectResponse {
        connection: VcsConnectionResponse::from_row(&rotated),
        webhook_secret,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// DELETE /api/workspaces/{id}/vcs/connections/{connectionId}
// ---------------------------------------------------------------------------

/// 上游 `DeleteVCSConnection`（`vcs.go:248`）：admin 门内，**不**做出站调用、**不**查
/// 产品边界（删本地行永远允许）。级联清理在同一条语句里（`vcs_connection.rs` 的 CTE）。
///
/// 成功回 **204**（删 0 行也回 204，与上游 `:exec` 不看 `rows_affected` 同判）。
async fn delete_connection(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    Path((raw_workspace_id, raw_connection_id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let scope = VcsScope::resolve(&state, user, &raw_workspace_id).await?;
    scope.require_admin()?;
    let connection_id = Id(parse_uuid(&raw_connection_id, "connection id")?);

    let deleted = VcsConnectionRepo::new(state.db.clone())
        .delete(connection_id, scope.workspace_id)
        .await
        .map_err(|_| Error::Database("failed to remove connection".into()))?;
    if !deleted {
        tracing::debug!(
            connection_id = %raw_connection_id,
            "vcs: delete matched no row (already gone / other workspace)"
        );
    }

    publish_connection_event(
        &state,
        raw_workspace_id.trim(),
        raw_connection_id.trim(),
        "deleted",
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// 纯函数（可单测）
// ---------------------------------------------------------------------------

/// 上游 `newVCSWebhookSecret`：**32 随机字节**的十六进制（64 字符）。
///
/// 与 `mc-vcs` 里三条签名方案都兼容：Forgejo 拿它做 HMAC 密钥，GitLab 拿它做
/// `X-Gitlab-Token` 的明文 token。
fn mint_webhook_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// 上游 `sealVCSSecret`：`secretbox` 封装 + **base64（`StdEncoding`）**（列是 TEXT）。
///
/// 返回 `Err` 时只丢错误类型（`SecretBoxError` 只带长度，不带载荷）。
fn seal_secret(secret_box: &mc_secrets::SecretBox, plaintext: &str) -> Result<String, ()> {
    let sealed = secret_box.seal(plaintext.as_bytes()).map_err(|_| ())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(sealed))
}

/// 上游 `openVCSSecret`（`webhook.rs` 是唯一调用方，实现在那边）：
/// base64 解 → `secretbox` 解封。空密文 ⇒ `None`（上游 `if enc == "" { return "", nil }`）。
///
/// 放在本文件是为了让「封装 / 解封」两个方向挨着可对照；`webhook.rs` 直接调用
/// [`open_secret`]。
pub(crate) fn open_secret(
    secret_box: &mc_secrets::SecretBox,
    encrypted_base64: &str,
) -> Result<Option<String>, ()> {
    if encrypted_base64.is_empty() {
        return Ok(None);
    }
    let ciphertext = base64::engine::general_purpose::STANDARD
        .decode(encrypted_base64)
        .map_err(|_| ())?;
    let plaintext = secret_box.open(&ciphertext).map_err(|_| ())?;
    String::from_utf8(plaintext).map(Some).map_err(|_| ())
}

/// 上游 `url.Parse` 的两条判据（scheme ∈ {http, https}、host 非空）。
///
/// 本仓没有 `url` 依赖（`mc-http` 的 manifest 不含它），所以手写这两条。相对 Go 的差异：
/// 大小写不敏感的 scheme（Go 也把小写化）、authority 里不允许空白（Go 的 `url.Parse` 对
/// 空白直接报错）⇒ 本实现**不比上游宽**。
fn is_absolute_http_url(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty() && !authority.starts_with(':') && !authority.contains(char::is_whitespace)
}

/// 上游 `h.publish(protocol.EventVCSConnection{Created,Deleted}, …)`。
///
/// 本仓的广播面是 `mc_realtime::EventEnvelope`（与 M8-1 的
/// `github_installation:{created,deleted}` 同款）；事件类型名逐字取上游常量
/// （`pkg/protocol/events.go:177-178`）。
fn publish_connection_event(
    state: &AppState,
    workspace_id: &str,
    connection_id: &str,
    action: &str,
) {
    let envelope = mc_realtime::EventEnvelope::new(
        "vcs_connection",
        workspace_id,
        None,
        serde_json::json!({ "id": connection_id }),
    )
    .with_type(format!("vcs_connection:{action}"));
    state.realtime.publish(envelope);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 铸出来的 secret 是 64 位小写十六进制，且两次不同（随机）。
    #[test]
    fn minted_webhook_secret_is_32_random_bytes_hex() {
        let first = mint_webhook_secret();
        let second = mint_webhook_secret();
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(first.chars().all(|c| !c.is_ascii_uppercase()));
        assert_ne!(first, second, "nonce 必须随机");
    }

    /// 封装/解封往返：库里存的是密文（≠ 明文），解回来才是明文。
    #[test]
    fn secret_round_trip_stores_only_ciphertext() {
        let secret_box = mc_secrets::SecretBox::new(&[7u8; 32]).expect("box");
        let plaintext = "glpat-DO-NOT-LOG";
        let sealed = seal_secret(&secret_box, plaintext).expect("seal");
        assert_ne!(sealed, plaintext);
        assert!(!sealed.contains(plaintext));
        assert_eq!(
            open_secret(&secret_box, &sealed).expect("open"),
            Some(plaintext.to_string())
        );
        // 空密文 ⇒ None（上游 `openVCSSecret` 的第一段）。
        assert_eq!(open_secret(&secret_box, "").expect("empty"), None);
        // 换一把密钥 ⇒ 解不开（错误值不带载荷）。
        let other = mc_secrets::SecretBox::new(&[8u8; 32]).expect("box");
        assert!(open_secret(&other, &sealed).is_err());
        // 非法 base64 / 非 UTF-8 明文都只是 Err（**不** panic）。
        assert!(open_secret(&secret_box, "not base64 !!").is_err());
    }

    /// URL 判据：两条都满足才算绝对 http(s) URL。
    #[test]
    fn absolute_http_url_check_matches_upstream_criteria() {
        for good in [
            "http://git.test",
            "https://git.test",
            "https://git.test:8443/sub/path",
            "HTTPS://Git.Test/x",
            "http://127.0.0.1:9",
        ] {
            assert!(is_absolute_http_url(good), "{good} 应当通过");
        }
        for bad in [
            "",
            "git.test",
            "ftp://git.test",
            "http://",
            "http:///path",
            "https://:8443",
            "http://a b.test",
        ] {
            assert!(!is_absolute_http_url(bad), "{bad} 应当被拒");
        }
    }

    /// registry 的三个 kind（`forgejo` / `gitea` / `gitlab`）都在，且顺序确定。
    #[test]
    fn provider_registry_has_the_three_builtin_kinds() {
        let registry = provider_registry();
        assert_eq!(registry.len(), 3);
        assert_eq!(
            registry.kinds(),
            vec![
                mc_core::vcs::VcsProviderKind::Forgejo,
                mc_core::vcs::VcsProviderKind::Gitea,
                mc_core::vcs::VcsProviderKind::GitLab,
            ]
        );
    }
}
