//! surface 启动路由：签发一次性 surface 访问（**1 个注册键**）。
//!
//! - **写者**：M6-6（`docs/57` §3.2）。真正的 `/plugin-surfaces/:token` 页面在 M6-7 的
//!   `routes/surfaces.rs` —— 本文件只负责「从管理面拿到启动凭据」这一步。
//! - **上游**：`internal/handler/plugin_surface.go` 的 launch 段（+ `surfaceToken` 的签发/校验）。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/workspaces/:id/plugins/:installationId/surfaces/:surfaceKey/launch` | GET | `router.go:1694` |
//!
//! - **三条硬纪律**（`docs/57` §4.2 M6-6/M6-7 的安全约束）：
//!   1. **未配置即禁用**：`MULTICA_PLUGIN_SURFACE_ORIGIN` 与部署密钥缺任何一个 ⇒
//!      503 `plugin_surfaces_not_configured`（上游 `writeFeatureDisabled` 的逐字口径）。
//!      本仓读这两个值：origin 从配置、密钥从 `state.plugin_key()`；**未配置 ⇒ 不签发、不 panic**。
//!   2. 令牌**绝不进 iframe**：surface 页面只拿短期 token，回调进插件的请求由宿主代发。
//!   3. `surfaceKey` 必须在已安装 manifest 的 `contributes.surfaces` 里（判定读
//!      `mc_plugin_host::manifest`），未知 key ⇒ 404。
//! - **不做什么**：不落库（surface token 是进程内的，不持久化）。
//!
//! # M6-6 落地说明（LUM-1671）
//!
//! 1. **门是「成员可见」**：上游把这条放在 member 组里（`router.go:1694` 的注释写得很清楚：
//!    「开一个 issue 就是向它要一次」，而 install / configure / remove 仍是管理员专属）。加上
//!    `requirePluginsV1`（上游在 handler 开头显式调）⇒ `开关门 → 成员门 → 配置门 → 安装行`。
//!    安装行**必须是路径里那个 workspace 的**，且 `enabled` 才签发；否则 403 `this Plugin is
//!    disabled`（上游同一句）。
//! 2. **503 是本地统一口径，上游实为 403**：上游 `writeFeatureDisabled` 落到
//!    `writeErrorCode(w, http.StatusForbidden, …)`（`handler.go:578`）。本仓的 M6-0 anchor
//!    （`state.rs` 的两处文档）、`routes/surfaces.rs` 的桩、以及本 issue 的 `DoD` 都写死 **503**，
//!    且与 `plugin_disabled`（M6-8 的 hook 面）同一口径 ⇒ 本片按 503 落地并登记
//!    `docs/32` §9.8（码与文案逐字不变，只有状态码这一位不同）。
//! 3. **`origin` 合法性 + 「不得与 app/API origin 同机」是本片的判定**（anchor 只落读取口）：
//!    非法 ⇒ 500 `plugin_surfaces_misconfigured`；同机 ⇒ 同一码、另一种文案。**专用候选**退化为
//!    「本进程 host（配了端口就连端口）」+「本次请求的 `Host` 头」—— 本仓没有 `PublicURL` /
//!    `AppURL` / `AttachmentFrameAncestors` 三个配置面，登记 `docs/32` §9.8。
//! 4. **claims 与「打开」放在本片**：上游 `pluginSurfaceLaunchClaims` 的类型与校验在
//!    `plugin_surface.go` 里，签发与承载共用；本仓由本片出 [`SurfaceLaunchClaims`] /
//!    [`mint_surface_launch`] / [`open_surface_launch_claims`]，**M6-7 直接复用**（它那侧不另
//!    定义一份 claims 形状，否则「签发」与「校验」两份契约会漂移）。承载（Host 边界、CSP、
//!    文档渲染）仍然完全归 M6-7。
//!
//! **状态：M6-6 已落地（LUM-1671）**。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine as _;
use chrono::Utc;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use mc_plugin_host::credentials::{
    open_token, seal_to_token, surface_launch_box, CredentialError, DeploymentKey,
};
use mc_plugin_host::manifest::Surface;
use mc_repos::plugin::mcp_approval::{PluginApprovalRepo, PluginInstallationRow};

use super::install::{
    deployment_key, parse_installation_manifest, require_plugins_v1, workspace_member, PluginError,
    PluginResult,
};
use crate::routes::auth_user::AuthUser;
use crate::state::{AppState, ConfigSnapshot};

/// 上游 `pluginSurfaceLaunchTTL`：**2 分钟**。
pub const SURFACE_LAUNCH_TTL_SECS: u64 = 120;

/// 承载路径前缀（上游 `mintPluginSurfaceToken` 拼的那一段）。
const SURFACE_PATH_PREFIX: &str = "/plugin-surfaces/";

/// `/api/workspaces/:id/plugins/:installationId/surfaces/:surfaceKey/launch`（M6-6 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/workspaces/:id/plugins/:installationId/surfaces/:surfaceKey/launch",
        get(get_surface_launch),
    )
}

// ---------------------------------------------------------------------------
// origin（上游 `parsePluginSurfaceOrigin` / `pluginSurfaceOriginIsDedicated`）
// ---------------------------------------------------------------------------

/// 一个合法的 surface 内容 origin。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceOrigin {
    /// `scheme://host[:port]`（小写；尾斜杠已由 `state.rs` 剥掉）—— 启动 URL 的前缀。
    prefix: String,
    /// 主机名（小写，不含端口；IPv6 带方括号）。
    host: String,
    /// **有效**端口：明写的值，或该 scheme 的默认端口（`http` 80 / `https` 443）。
    ///
    /// 折成有效端口是因为「同机」判据比的是 `host:port`：`https://a.example.com` 与
    /// `https://a.example.com:443` 是同一台。Go 那边比的是 `url.URL.Host` 字符串，两种写法
    /// 不相等 ⇒ 本仓这一处比上游**更严**（登记 `docs/32` §9.8）。
    port: u16,
}

impl SurfaceOrigin {
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `origin` + `/plugin-surfaces/` + token（上游 `origin.String() + "/plugin-surfaces/" + token`）。
    #[must_use]
    pub fn launch_url(&self, token: &str) -> String {
        format!("{}{SURFACE_PATH_PREFIX}{token}", self.prefix)
    }
}

/// 主机名允许的字符（IPv6 字面量里的 `:` 也算）。
fn is_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':')
}

/// 上游 `parsePluginSurfaceOrigin`：必须是**绝对** http(s) origin、无 path（`/` 除外）、
/// 无 query / fragment / userinfo。
///
/// 手写而不用 `url::Url`：`url` **不是** `mc-http` 的依赖，而 M6-0 anchor 把依赖边冻结在
/// 那一片（「此后 M6 各切片不再改本 manifest、`Cargo.lock` 只在 anchor 重生成」）
/// ⇒ 本片不为了解析一个 origin 引包。
///
/// 与 Go `url.Parse` 的两处细微差异（都更严，登记 `docs/32` §9.8）：空端口（`host:`）被拒；
/// 主机名按字符表校验（Go 允许一些本仓不接受的形态）。
#[must_use]
pub fn parse_plugin_surface_origin(raw: &str) -> Option<SurfaceOrigin> {
    let trimmed = raw.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if rest.is_empty() || rest.contains('?') || rest.contains('#') {
        return None;
    }
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    // 上游：`Path != "" && Path != "/"` ⇒ 非法。`split_once('/')` 之后「剩余非空」就等于有 path。
    if !path.is_empty() || authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(raw_port) => Some(raw_port.parse::<u16>().ok()?),
            None if tail.is_empty() => None,
            None => return None,
        };
        (format!("[{host}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, raw_port)) => (host.to_string(), Some(raw_port.parse::<u16>().ok()?)),
            None => (authority.to_string(), None),
        }
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty() || !host.chars().all(is_host_char) {
        return None;
    }
    // 未写端口 ⇒ 该 scheme 的默认端口（「同机」判据比的是 `host:port`）。
    let port = port.unwrap_or(if scheme == "https" { 443 } else { 80 });
    let prefix = format!("{scheme}://{}", authority.to_ascii_lowercase());
    Some(SurfaceOrigin { prefix, host, port })
}

/// 上游 `pluginSurfaceOriginIsDedicated`：surface origin 不得与 app / API origin 同机。
///
/// 上游候选 = `cfg.PublicURL` / `cfg.AppURL` / `cfg.AttachmentFrameAncestors`；本仓没有这三个
/// 配置面（`ConfigSnapshot` 只有 `host` / `port`，`mc_config::ServerConfig::external_url` 没有被
/// anchor 接进 `AppState`，而 `state.rs` 是冻结文件）⇒ 退化为「本进程 host（配了端口就连端口）」
/// +「本次请求的 `Host` 头」。比上游**更严**的一面：同名的任意端口都算非专用；**更宽**的一面：
/// 经别名（CNAME / 另一个 host）访问的 app origin 认不出来。登记 `docs/32` §9.8。
fn surface_origin_is_dedicated(
    origin: &SurfaceOrigin,
    config: &ConfigSnapshot,
    host_header: Option<&str>,
) -> bool {
    let mut candidates: Vec<(String, Option<u16>)> = Vec::new();
    if !config.host.is_empty() {
        candidates.push((
            config.host.to_ascii_lowercase(),
            (config.port != 0).then_some(config.port),
        ));
    }
    if let Some(raw) = host_header {
        let raw = raw.trim();
        if let Some(candidate) = parse_plugin_surface_origin(&format!("http://{raw}")) {
            // `Host` 头不带 scheme：写了端口就按那个端口比，没写就**只**比主机名 ——
            // 不能拿 `http` 的默认 80 去比一个 `https` origin。
            let explicit_port = match raw.rsplit_once(']') {
                Some((_, tail)) => tail.starts_with(':'),
                None => raw.contains(':'),
            };
            candidates.push((candidate.host, explicit_port.then_some(candidate.port)));
        }
    }
    !candidates
        .iter()
        .any(|(host, port)| host == &origin.host && (port.is_none() || *port == Some(origin.port)))
}

/// 上游 `GetPluginSurfaceLaunch` 的前半段：配置门（未配置 503 / 非法与非专用 500）。
fn configured_origin(state: &AppState, headers: &HeaderMap) -> PluginResult<SurfaceOrigin> {
    let Some(raw) = state.plugin_surface_origin.as_deref() else {
        return Err(surfaces_not_configured());
    };
    if deployment_key(state).is_none() {
        return Err(surfaces_not_configured());
    }
    let Some(origin) = parse_plugin_surface_origin(raw) else {
        return Err(misconfigured(
            "Plugin surfaces require a valid MULTICA_PLUGIN_SURFACE_ORIGIN",
        ));
    };
    let host_header = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());
    if !surface_origin_is_dedicated(&origin, &state.config, host_header) {
        return Err(misconfigured(
            "Plugin surfaces require a dedicated content origin separate from the app and API origins",
        ));
    }
    Ok(origin)
}

/// 上游 `writeFeatureDisabled(w, "plugin_surfaces_not_configured", …)`（状态码见文件头说明 2）。
fn surfaces_not_configured() -> PluginError {
    PluginError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "plugin_surfaces_not_configured",
        "Plugin surfaces are unavailable: MULTICA_PLUGIN_SURFACE_ORIGIN and \
         MULTICA_PLUGIN_SECRET_KEY must be configured",
    )
}

/// 上游 `writeErrorCode(w, 500, "plugin_surfaces_misconfigured", …)`。
fn misconfigured(message: &str) -> PluginError {
    PluginError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "plugin_surfaces_misconfigured",
        message,
    )
}

/// 上游 `writeError(w, http.StatusInternalServerError, "failed to create the Plugin surface launch")`。
fn launch_failed() -> PluginError {
    PluginError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
        "failed to create the Plugin surface launch",
    )
}

// ---------------------------------------------------------------------------
// 令牌（M6-7 复用这一段）
// ---------------------------------------------------------------------------

/// 上游 `pluginSurfaceLaunchClaims` —— **启动令牌的全部载荷**。
///
/// 无 DB 状态、无会话：token 即凭据。字段名与上游逐字相同（它们进了加密载荷，改名字等于换协议）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceLaunchClaims {
    pub workspace_id: String,
    pub installation_id: String,
    pub version_id: String,
    pub surface_key: String,
    pub digest: String,
    pub challenge: String,
    pub expires_at: i64,
}

/// 打开令牌的四类拒绝（上游 `openPluginSurfaceToken` 的四个分支 + 承载段的过期判定）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SurfaceLaunchError {
    /// 不是合法 base64url。
    #[error("invalid plugin surface token encoding")]
    Encoding,
    /// 解密 / 认证失败 —— 被改过、或换过密钥（**错域**就落在这里：域分离标签不同 ⇒ 派生密钥不同）。
    #[error("invalid plugin surface token")]
    Authentication,
    /// 解出来不是 claims（形状不对）。
    #[error("invalid plugin surface claims")]
    Claims,
    /// 字段缺失或 `expires_at` 已过。
    #[error("expired or incomplete plugin surface claims")]
    Expired,
    /// **签发**侧失败（盒构造 / 序列化 / AES-GCM）—— 上游
    /// `writeError(w, 500, "failed to create the Plugin surface launch")`。
    ///
    /// 与上面四类拒绝同处一个枚举，是为了让 [`mint_surface_launch`] 不必把 `PluginError`
    /// （`routes::plugins` 的私有类型）放进公开签名里 —— M6-7 在同一个 crate 里，两边都不吃亏。
    #[error("failed to create the Plugin surface launch")]
    Mint,
}

/// `base64url_nopad(seal(claims))`，密钥由 `multica/plugin-surface-launch/v1` 域分离标签从部署
/// 密钥派生（M6-1 的 `surface_launch_box`）—— surface URL 永远解不开存储的 config secret。
///
/// ⚠️ `ThreadRng` **不是** `Send`：它一旦活过任何 `.await`，整个 handler 的 future 就不再是
/// `Send`，axum 会在 `.route(...)` 处报「`Handler` 未实现」而不是在这里报错。所以本函数是
/// **同步**函数、作用域内无 `.await`（与 `install.rs` 的 `seal_secret` 同款约束）。
///
/// # Errors
///
/// [`SurfaceLaunchError::Mint`]（盒构造 / 序列化 / 封装失败）。
pub fn mint_surface_launch(
    key: &DeploymentKey,
    claims: &SurfaceLaunchClaims,
) -> Result<String, SurfaceLaunchError> {
    let boxed = surface_launch_box(Some(key)).map_err(|_| SurfaceLaunchError::Mint)?;
    let payload = serde_json::to_vec(claims).map_err(|_| SurfaceLaunchError::Mint)?;
    let mut rng = rand::thread_rng();
    seal_to_token(&boxed, &payload, &mut rng).map_err(|_| SurfaceLaunchError::Mint)
}

/// 打开一枚令牌（当前时钟）。
///
/// # Errors
///
/// [`SurfaceLaunchError`]。
pub fn open_surface_launch_claims(
    key: &DeploymentKey,
    token: &str,
) -> Result<SurfaceLaunchClaims, SurfaceLaunchError> {
    open_surface_launch_claims_at(key, token, Utc::now().timestamp())
}

/// [`open_surface_launch_claims`] 的显式时钟版本（过期判定要能被断言，不能只靠「等两分钟」）。
///
/// # Errors
///
/// [`SurfaceLaunchError`]。
pub fn open_surface_launch_claims_at(
    key: &DeploymentKey,
    token: &str,
    now_unix: i64,
) -> Result<SurfaceLaunchClaims, SurfaceLaunchError> {
    let boxed = surface_launch_box(Some(key)).map_err(|_| SurfaceLaunchError::Authentication)?;
    let payload = open_token(&boxed, token).map_err(|err| match err {
        CredentialError::TokenEncoding => SurfaceLaunchError::Encoding,
        _ => SurfaceLaunchError::Authentication,
    })?;
    let claims: SurfaceLaunchClaims =
        serde_json::from_slice(&payload).map_err(|_| SurfaceLaunchError::Claims)?;
    if claims.workspace_id.is_empty()
        || claims.installation_id.is_empty()
        || claims.version_id.is_empty()
        || claims.surface_key.is_empty()
        || claims.digest.is_empty()
        || claims.challenge.is_empty()
        || claims.expires_at <= now_unix
    {
        return Err(SurfaceLaunchError::Expired);
    }
    Ok(claims)
}

/// 上游 `randomPluginSurfaceChallenge`：32 字节 → base64url（无填充）。
///
/// 约束同 [`mint_surface_launch`]（rng 不得活过 `.await`）。
fn random_challenge() -> String {
    let mut value = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

// ---------------------------------------------------------------------------
// handler
// ---------------------------------------------------------------------------

/// 上游 `SurfaceScript` 的清单段：`surfaceKey` 必须在**已安装**的 manifest 快照里。
fn surface_entry<'a>(surfaces: &'a [Surface], surface_key: &str) -> PluginResult<&'a str> {
    surfaces
        .iter()
        .find(|surface| surface.key == surface_key)
        .map(|surface| surface.entry.as_str())
        .ok_or_else(|| {
            PluginError::not_found(format!(
                "this Plugin does not contribute a surface named {surface_key:?}"
            ))
        })
}

/// 上游 `SurfaceScript` 的取文件段：**只要摘要** —— 启动路径不读脚本字节（那是 M6-7 的
/// `ServePluginSurface`），也就不该把包体拉进内存。
async fn surface_digest(
    state: &AppState,
    installation: &PluginInstallationRow,
    entry: &str,
) -> PluginResult<String> {
    PluginApprovalRepo::new(state.db.clone())
        .package_file_sha256(installation.package_version_id(), entry)
        .await
        .map_err(|_| PluginError::unavailable("read the Plugin surface"))?
        .ok_or_else(|| {
            PluginError::not_found(format!("the installed version does not contain {entry:?}"))
        })
}

/// `GET /api/workspaces/:id/plugins/:installationId/surfaces/:surfaceKey/launch` —— **成员可见**。
async fn get_surface_launch(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    Path((workspace, installation, surface_key)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let inner = async {
        require_plugins_v1(&state)?;
        let workspace_id = workspace_member(&state, &workspace, auth.id()).await?;
        let origin = configured_origin(&state, &headers)?;
        let installation = PluginApprovalRepo::new(state.db.clone())
            .installation_for_workspace(workspace_id, &installation)
            .await
            .map_err(|err| match err {
                mc_repos::RepoError::NotFound => {
                    PluginError::not_found("plugin installation not found")
                }
                _ => PluginError::unavailable("load the Plugin"),
            })?;
        if !installation.enabled {
            return Err(PluginError::forbidden("this Plugin is disabled"));
        }
        let manifest = parse_installation_manifest(&installation.manifest)?;
        let entry = surface_entry(&manifest.contributes.surfaces, &surface_key)?;
        let digest = surface_digest(&state, &installation, entry).await?;

        let key = deployment_key(&state).ok_or_else(surfaces_not_configured)?;
        let challenge = random_challenge();
        let claims = SurfaceLaunchClaims {
            workspace_id: workspace_id.to_string(),
            installation_id: installation.id.to_string(),
            version_id: installation.package_version_id.to_string(),
            surface_key,
            digest: digest.clone(),
            challenge: challenge.clone(),
            expires_at: Utc::now().timestamp()
                + i64::try_from(SURFACE_LAUNCH_TTL_SECS).unwrap_or(120),
        };
        let token = mint_surface_launch(&key, &claims).map_err(|_| launch_failed())?;
        Ok::<Value, PluginError>(json!({
            "url": origin.launch_url(&token),
            "bridge_token": challenge,
            "version": installation.version,
            "digest": digest,
        }))
    }
    .await;
    match inner {
        Ok(body) => ([(header::CACHE_CONTROL, "private, no-store")], Json(body)).into_response(),
        Err(err) => err.into_response(),
    }
}

// ---------------------------------------------------------------------------
// 单元测试：origin 解析 / 专用判定 / 令牌的三类拒绝（不接库）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mc_plugin_host::credentials::secret_box;

    /// 32 字节 → base64（`StdEncoding`，带填充）；与 `state.rs` 的 `from_env_with` 同口径。
    const KEY_B64: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

    fn key() -> DeploymentKey {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(KEY_B64)
            .expect("fixture key is base64");
        DeploymentKey::new(raw).expect("fixture key is 32 bytes")
    }

    fn claims(expires_at: i64) -> SurfaceLaunchClaims {
        SurfaceLaunchClaims {
            workspace_id: "11111111-1111-1111-1111-111111111111".into(),
            installation_id: "22222222-2222-2222-2222-222222222222".into(),
            version_id: "33333333-3333-3333-3333-333333333333".into(),
            surface_key: "panel".into(),
            digest: "a".repeat(64),
            challenge: "challenge-value".into(),
            expires_at,
        }
    }

    fn config(host: &str, port: u16) -> ConfigSnapshot {
        ConfigSnapshot {
            host: host.into(),
            port,
            ..Default::default()
        }
    }

    #[test]
    fn origin_parser_accepts_bare_origins_and_rejects_everything_with_a_path() {
        let origin = parse_plugin_surface_origin("https://surfaces.example.com").expect("bare");
        assert_eq!(origin.host(), "surfaces.example.com");
        // 未写明 ⇒ scheme 的默认端口。
        assert_eq!(origin.port(), 443);
        assert_eq!(origin.prefix(), "https://surfaces.example.com");
        assert_eq!(
            origin.launch_url("tok"),
            "https://surfaces.example.com/plugin-surfaces/tok"
        );

        let with_port = parse_plugin_surface_origin("http://127.0.0.1:9000").expect("port");
        assert_eq!(with_port.port(), 9000);

        // 尾斜杠已由 `state.rs` 剥掉，但即使传进来也不该被当成有 path。
        assert!(parse_plugin_surface_origin("https://surfaces.example.com/").is_some());
        // 上游 `parsePluginSurfaceOrigin` 拒绝的每一条，逐条。
        for bad in [
            "surfaces.example.com",
            "ftp://surfaces.example.com",
            "https://surfaces.example.com/panel",
            "https://surfaces.example.com/?a=1",
            "https://surfaces.example.com/#frag",
            "https://user@surfaces.example.com",
            "https://",
            "https://surfaces.example.com:",
            "https://surfaces.example.com:notaport",
        ] {
            assert!(
                parse_plugin_surface_origin(bad).is_none(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn dedicated_origin_check_rejects_same_host_on_any_port() {
        // `config.port == 0` = 「API 进程就在这台主机上，但不钉端口」⇒ 同主的任意端口都算非专用。
        let wildcard_api = config("127.0.0.1", 0);
        // 钉了端口的候选用例（上游比的是 `host:port`）。
        let pinned_api = config("api.example.com", 443);

        let dedicated =
            parse_plugin_surface_origin("https://surfaces.example.com").expect("origin");
        assert!(surface_origin_is_dedicated(&dedicated, &pinned_api, None));

        for same_host in ["http://127.0.0.1:9999", "https://127.0.0.1"] {
            let origin = parse_plugin_surface_origin(same_host).expect("origin");
            assert!(
                !surface_origin_is_dedicated(&origin, &wildcard_api, None),
                "{same_host} is not a dedicated origin"
            );
        }
        // 主机名与 scheme/端口的大小写都不敏感：未写端口的 `https` = 443。
        assert!(!surface_origin_is_dedicated(
            &parse_plugin_surface_origin("https://API.example.com").expect("origin"),
            &pinned_api,
            None
        ));
        // 端口不同 ⇒ 不是同一台（上游比的是 `host:port`）。
        assert!(surface_origin_is_dedicated(
            &parse_plugin_surface_origin("https://api.example.com:8443").expect("origin"),
            &pinned_api,
            None
        ));

        // 请求的 `Host` 头也是候选（本仓没有 PublicURL/AppURL 配置面，见文件头说明 3）。
        assert!(!surface_origin_is_dedicated(
            &dedicated,
            &wildcard_api,
            Some("surfaces.example.com")
        ));
        assert!(surface_origin_is_dedicated(
            &parse_plugin_surface_origin("https://surfaces.example.com:8443").expect("origin"),
            &wildcard_api,
            Some("surfaces.example.com:443")
        ));
    }

    /// `DoD`：**TTL = 2 分钟**（不是「大概两分钟」—— 载荷里的 `expires_at` 必须逐秒可算）。
    #[test]
    fn minted_claims_carry_exactly_a_two_minute_ttl() {
        assert_eq!(SURFACE_LAUNCH_TTL_SECS, 120);
        let now = 1_700_000_000;
        let token = mint_surface_launch(&key(), &claims(now + 120)).expect("mint");
        let opened = open_surface_launch_claims_at(&key(), &token, now).expect("open");
        assert_eq!(opened.expires_at - now, 120);
        assert_eq!(opened.surface_key, "panel");
        assert_eq!(opened.challenge, "challenge-value");
        // 差一秒就过期（边界是 `expires_at <= now`）。
        assert_eq!(
            open_surface_launch_claims_at(&key(), &token, now + 120),
            Err(SurfaceLaunchError::Expired)
        );
        assert!(open_surface_launch_claims_at(&key(), &token, now + 119).is_ok());
    }

    /// `DoD`：**过期**拒绝。
    #[test]
    fn an_expired_token_is_refused() {
        let now = 1_700_000_000;
        let token = mint_surface_launch(&key(), &claims(now - 1)).expect("mint");
        assert_eq!(
            open_surface_launch_claims_at(&key(), &token, now),
            Err(SurfaceLaunchError::Expired)
        );
    }

    /// `DoD`：**篡改**拒绝（改一个字节就过不了 GCM 认证）。
    #[test]
    fn a_tampered_token_is_refused() {
        let now = 1_700_000_000;
        let token = mint_surface_launch(&key(), &claims(now + 120)).expect("mint");
        let mut bytes = token.clone().into_bytes();
        // 只动载荷本体（签名尾部的改动同样会落在 GCM tag 上）。
        bytes[10] = if bytes[10] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).expect("still utf8");
        assert_eq!(
            open_surface_launch_claims_at(&key(), &tampered, now),
            Err(SurfaceLaunchError::Authentication)
        );
        // 连编码都不是的时候是另一类拒绝（上游 `invalid plugin surface token encoding`）。
        assert_eq!(
            open_surface_launch_claims_at(&key(), "not a token!!", now),
            Err(SurfaceLaunchError::Encoding)
        );
    }

    /// `DoD`：**错域**拒绝 —— 域分离标签（`multica/plugin-surface-launch/v1`）与「换了一把部署
    /// 密钥」都会让派生密钥不同，因而这份载荷解不开。surface URL 永远解不开存储的 config secret。
    #[test]
    fn a_token_from_another_domain_is_refused() {
        let now = 1_700_000_000;
        let token = mint_surface_launch(&key(), &claims(now + 120)).expect("mint");

        // ① 另一把部署密钥（= 另一个部署域）。
        let other = DeploymentKey::new(vec![9_u8; 32]).expect("32 bytes");
        assert_eq!(
            open_surface_launch_claims_at(&other, &token, now),
            Err(SurfaceLaunchError::Authentication)
        );

        // ② 同一把密钥、但没有域分离标签的那个盒子（存储 config secret 用的那个）。
        let plain_box = secret_box(Some(&key())).expect("box");
        let payload = serde_json::to_vec(&claims(now + 120)).expect("claims json");
        let mut rng = rand::thread_rng();
        let cross_domain = seal_to_token(&plain_box, &payload, &mut rng).expect("seal");
        assert_eq!(
            open_surface_launch_claims_at(&key(), &cross_domain, now),
            Err(SurfaceLaunchError::Authentication)
        );
    }
}
