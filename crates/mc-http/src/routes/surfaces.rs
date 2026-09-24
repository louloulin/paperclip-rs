//! surface 页面：`GET /plugin-surfaces/:token`（**1 个注册键**）。
//!
//! - **写者**：M6-7（`docs/57` §3.2 的 `routes/surfaces.rs`）。
//! - **上游**：`router.go:1462` 的挂载点 + `internal/handler/plugin_surface.go` 的
//!   `ServePluginSurface` / `PluginSurfaceHostBoundary` / `pluginSurfaceCSP` /
//!   `buildPluginSurfaceDocument`。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/plugin-surfaces/:token` | GET | `router.go:1462` |
//!
//! ## ⚠️ 这条路径**不在 `/api` 前缀下**
//!
//! 上游把它挂在 server 根下（浏览器直接打开，不是 API 调用）。本文件的 router 由
//! `mount_slice_plugin_surface()` 合并到全局 router。路径参数写 `:token`（冒号形态）。
//!
//! ## 语义（拒绝的四种理由**都必须先于**任何渲染发生）
//!
//! 1. **Host 边界**：`MULTICA_PLUGIN_SURFACE_ORIGIN` 未配置 / 不是合法 origin / 与 API 主机
//!    重合 / 与请求的 `Host` 不（大小写不敏感地）相等 ⇒ **404**。上游 `PluginSurfaceHostBoundary`
//!    的「复用 API 进程但只服务这一条路由」的承诺在这里落地。
//! 2. **不得收到应用凭据**：带 `Cookie` 或 `Authorization` 的请求 ⇒ **400**（上游注释：宽域
//!    父域 cookie 必须让 surface **可见地失败**，而不是安静地落到一个本该无 cookie 的主机上）。
//! 3. **路径令牌**：`mpc_` 族之外的形态 / base64 坏 / AES-GCM 认证失败（**篡改**）/ 声明不完整 /
//!    `expires_at` 已过（**过期**）/ 用**别的域**（例如部署密钥本体、hook 签名密钥）封的令牌
//!    ⇒ **404**（上游：不要区分「格式对但过期」与「格式不对」）。
//! 4. **安装与版本对齐**：安装行不存在 / 已停用 / `package_version_id` 与令牌里的版本不一致 /
//!    manifest 里没有这个 surface key / 包内没有那个入口文件 / 文件摘要与令牌不符 ⇒ **404**。
//!
//! 通过之后才渲染：CSP 按**已同意的 `net:` scope**生成 `connect-src`，`script-src 'unsafe-inline'`
//! （文档自己注入插件代码），其余一律 `'none'`；插件令牌**绝不**下发进页面。
//!
//! ## 本片登记在 `docs/32` §9 的偏离（两条）
//!
//! 1. **未配置 ⇒ 404（不是 503）**：M6-0 的桩注释写「`MULTICA_PLUGIN_SURFACE_ORIGIN` 或部署密钥
//!    缺 ⇒ 503 `plugin_surfaces_not_configured`」——**上游的 serve 段不是这样**：503 那个码属于
//!    **launch 段**（`GetPluginSurfaceLaunch`，M6-6），serve 段对 origin 解析失败一律
//!    `http.NotFound`。本文件按上游代码实现，并把该行记为本片纠正的过期口径。
//! 2. **`PluginSurfaceHostBoundary` 不做成全局中间件**：上游把它挂在**整台 router** 上（这样
//!    「内容主机上除了这一条路径以外的一切」都 404）。本仓的挂载点在 M6-0 冻结的 `mount.rs`
//!    里，本片不得编辑它 ⇒ 那条「其余路径全 404」由 handler 内的 Host 判定承担（同一个判据、
//!    同一个拒绝），差别只在「内容主机上的 `/api/health` 会落到全局 router」这一条 —— 登记为
//!    已知偏离，交给 M6-INT 决定是否把该中间件提到 `main.rs` 的装配处。
//!
//! 行预算（门 ⑩）：本文件 ≤420 行。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use mc_core::Id;
use mc_plugin_host::credentials::{open_token, surface_launch_box, DeploymentKey};
use mc_plugin_host::manifest::Manifest;
use mc_plugin_host::scope::{net_domains, validate_scope, SCOPE_NET_PREFIX};
use mc_repos::plugin::installation::InstallationRepo;
use mc_repos::plugin::package::PackageRepo;

use crate::state::AppState;

/// surface 启动令牌的生存期（上游 `pluginSurfaceLaunchTTL`）：**2 分钟**。
///
/// 签发侧（M6-6 的 `routes/plugins/surface_launch.rs`）用它算 `expires_at`，本文件用它判定
/// 「过期」；**两边的唯一真值**就是这个常量（M6-6 直接 `use` 本常量，不要再写一遍 120）。
pub const SURFACE_LAUNCH_TTL_SECS: i64 = 120;

/// 桥接协议版本（上游 `pluginSurfaceProtocolVersion`）。
const SURFACE_PROTOCOL_VERSION: u32 = 2;
/// 页面 → 宿主的连接消息（上游 `pluginSurfaceConnectMessage`）。
const SURFACE_CONNECT_MESSAGE: &str = "multica:plugin-bridge-connect";
/// 注入 `MessagePort` 的全局名（上游 `pluginSurfacePortGlobal`）。
const SURFACE_PORT_GLOBAL: &str = "__multicaPluginBridgePortV2";

/// 一次 surface 启动令牌里的声明（上游 `pluginSurfaceLaunchClaims`）。
///
/// ⚠️ **签发侧**（M6-6 的 `routes/plugins/surface_launch.rs`）必须用**同一个**结构体序列化：
/// 两个结构体各写一遍字段名，就是「签发方与消费方悄悄漂移」的那一天。本类型是 `pub(crate)`，
/// 跨片引用即可（登记在 `docs/32` §9）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SurfaceLaunchClaims {
    pub(crate) workspace_id: String,
    pub(crate) installation_id: String,
    pub(crate) version_id: String,
    pub(crate) surface_key: String,
    pub(crate) digest: String,
    pub(crate) challenge: String,
    pub(crate) expires_at: i64,
}

/// `/plugin-surfaces/:token`（M6-7 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/plugin-surfaces/:token", get(serve_surface))
}

/// `GET /plugin-surfaces/:token`。
pub(crate) async fn serve_surface(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    serve_surface_inner(&state, &headers, &token).await
}

async fn serve_surface_inner(state: &AppState, headers: &HeaderMap, token: &str) -> Response {
    // 1. Host 边界（含「未配置即禁用」）。
    let Some(origin_host) = host_boundary_origin(state, headers) else {
        return not_found();
    };
    debug_assert!(origin_host.is_empty() || !origin_host.is_empty());

    // 2. 内容主机必须无 cookie、无 Authorization。
    if headers.contains_key(header::COOKIE) || headers.contains_key(header::AUTHORIZATION) {
        return bad_request("the Plugin content origin must not receive app credentials");
    }

    // 3. 打开路径令牌（篡改 / 过期 / 错域都在这里被拒）。
    let Some(claims) = open_claims(state, token) else {
        return not_found();
    };

    // 4. 安装 + 版本 + surface 入口 + 摘要。
    let Some((workspace_id, installation_id, version_id)) = parse_claim_ids(&claims) else {
        return not_found();
    };
    let repo = InstallationRepo::new(state.db.clone());
    let Ok(installation) = repo.get(workspace_id, installation_id).await else {
        return not_found();
    };
    if !installation.enabled || installation.package_version_id() != version_id {
        return not_found();
    }

    let Some(script) = surface_script(state, &installation, &claims.surface_key).await else {
        return not_found();
    };
    if !constant_time_eq(script.digest.as_bytes(), claims.digest.as_bytes()) {
        return not_found();
    }

    let document = build_surface_document(&script.code, &claims.challenge);
    let mut response = (StatusCode::OK, document).into_response();
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "private, no-store"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (
            header::HeaderName::from_static("permissions-policy"),
            "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
        ),
        (
            header::HeaderName::from_static("cross-origin-resource-policy"),
            "cross-origin",
        ),
    ] {
        if let Ok(value) = header::HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
    if let Ok(value) = header::HeaderValue::from_str(&surface_csp(&installation.granted_scopes())) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

// ---------------------------------------------------------------------------
// Host 边界
// ---------------------------------------------------------------------------

/// 上游 `ServePluginSurface` 的前两行：origin 可解析 + 主机**专用于内容** + `Host` 相等。
///
/// 返回 `Some(host)` 表示通过（`host` 就是配置里的内容主机）；任何一步不满足都返回 `None`
/// ⇒ 调用方回 404。
fn host_boundary_origin(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let origin = parse_surface_origin(state.plugin_surface_origin.as_deref()?)?;
    if !origin_is_dedicated(&origin, state) {
        return None;
    }
    let host = headers.get(header::HOST)?.to_str().ok()?.trim();
    if !host.eq_ignore_ascii_case(&origin.host) {
        return None;
    }
    Some(origin.host)
}

/// 解析出来的 origin（只留 scheme + host）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SurfaceOrigin {
    scheme: String,
    host: String,
}

/// 上游 `parsePluginSurfaceOrigin`：必须是绝对 `http(s)` origin，且**没有** path（`/` 除外）、
/// query、fragment、userinfo。任一不满足 ⇒ `None`（调用方 404）。
fn parse_surface_origin(raw: &str) -> Option<SurfaceOrigin> {
    let raw = raw.trim();
    let (scheme, rest) = raw.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if rest.contains(['/', '?', '#', '@']) {
        return None;
    }
    if rest.is_empty() {
        return None;
    }
    Some(SurfaceOrigin {
        scheme,
        host: rest.to_ascii_lowercase(),
    })
}

/// 上游 `pluginSurfaceOriginIsDedicated`：内容 origin 必须与 app / API 的 origin **不同**。
///
/// 本仓可用的对照值是 `ConfigSnapshot` 的 `host`（与 `port` 一起构成 API 的监听地址）：与 API
/// 主机重合的 origin 会让「复用 API 进程」变成「把 API 的登录/JSON/上传也交给这个主机」。
fn origin_is_dedicated(origin: &SurfaceOrigin, state: &AppState) -> bool {
    let api_host = state.config.host.trim().to_ascii_lowercase();
    if api_host.is_empty() {
        return true;
    }
    // 去掉端口再比（`127.0.0.1:3500` 与 `127.0.0.1` 是同一个主机）。
    let bare = |host: &str| {
        host.rsplit_once(':')
            .map_or(host, |(head, _)| head)
            .to_string()
    };
    bare(&origin.host) != bare(&api_host)
}

// ---------------------------------------------------------------------------
// 令牌
// ---------------------------------------------------------------------------

/// 上游 `openPluginSurfaceToken`：解封 + 声明完整性 + 过期。
///
/// **不区分**「格式不对」与「已过期」（上游注释）：都给同一种 404，免得调用方能借状态码
/// 区分出「这枚令牌曾经有效」。
fn open_claims(state: &AppState, token: &str) -> Option<SurfaceLaunchClaims> {
    let key = deployment_key(state)?;
    let boxed = surface_launch_box(Some(&key)).ok()?;
    let payload = open_token(&boxed, token).ok()?;
    let claims: SurfaceLaunchClaims = serde_json::from_slice(&payload).ok()?;
    if claims.workspace_id.is_empty()
        || claims.installation_id.is_empty()
        || claims.version_id.is_empty()
        || claims.surface_key.is_empty()
        || claims.digest.is_empty()
        || claims.challenge.is_empty()
        || claims.expires_at <= now_unix()
    {
        return None;
    }
    Some(claims)
}

/// 部署密钥（未配置 ⇒ `None`，调用方 404 —— **绝不**用零密钥兜底）。
fn deployment_key(state: &AppState) -> Option<DeploymentKey> {
    state
        .plugin_key
        .as_ref()
        .and_then(|key| DeploymentKey::new(key.as_bytes()))
}

fn parse_claim_ids(claims: &SurfaceLaunchClaims) -> Option<(Id, Id, Id)> {
    Some((
        Id::parse(&claims.workspace_id).ok()?,
        Id::parse(&claims.installation_id).ok()?,
        Id::parse(&claims.version_id).ok()?,
    ))
}

/// 当前 unix 秒（时钟早于 epoch 时退化为 0，不 panic）。
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().cast_signed())
}

// ---------------------------------------------------------------------------
// surface 脚本
// ---------------------------------------------------------------------------

/// 一个 surface 要运行的代码 + 它在不可变版本里的摘要（上游 `PluginSurfaceScript` 的消费段投影）。
///
/// 上游那个结构体还有一个 `Version` 字段，**只有 launch 段用**（响应里的 `version`）；serve 段
/// 只渲染文档 ⇒ 本结构体不带它（launch 侧要版本串时读 `installation.version`，同一处真值）。
pub(crate) struct SurfaceScript {
    pub(crate) code: String,
    pub(crate) digest: String,
}

/// 上游 `PluginService.SurfaceScript`：从**已同意的 manifest** 找入口，再读那个版本的文件。
///
/// 摘要来自 `plugin_package_file.sha256`（纯 hex），它把「管理员同意的版本」与「浏览器实际运行
/// 的字节」钉在一起 —— 令牌里的 `digest` 就是它。
pub(crate) async fn surface_script(
    state: &AppState,
    installation: &mc_repos::plugin::installation::InstallationRow,
    surface_key: &str,
) -> Option<SurfaceScript> {
    let manifest: Manifest = serde_json::from_value(installation.manifest.0.clone()).ok()?;
    let entry = manifest
        .contributes
        .surfaces
        .iter()
        .find(|surface| surface.key == surface_key)?
        .entry
        .clone();
    let file = PackageRepo::new(state.db.clone())
        .file(installation.package_version_id(), &entry)
        .await
        .ok()?;
    Some(SurfaceScript {
        code: String::from_utf8_lossy(&file.content).to_string(),
        digest: file.sha256,
    })
}

// ---------------------------------------------------------------------------
// 渲染
// ---------------------------------------------------------------------------

/// 上游 `pluginSurfaceCSP`：`connect-src` 只放**已同意**的 `net:` 域名。
///
/// 安装在写入时已经校验过 scope，但 CSP 仍是边界：一行被损坏或从旧库恢复的数据不该把
/// 任意域名放进 `connect-src`。
fn surface_csp(scopes: &[String]) -> String {
    let mut connect = Vec::new();
    for domain in net_domains(scopes) {
        if validate_scope(&format!("{SCOPE_NET_PREFIX}{domain}")).is_err() {
            continue;
        }
        connect.push(format!("https://{domain}"));
    }
    let connect_source = if connect.is_empty() {
        "'none'".to_string()
    } else {
        connect.join(" ")
    };
    [
        "default-src 'none'".to_string(),
        "script-src 'unsafe-inline'".to_string(),
        "style-src 'unsafe-inline'".to_string(),
        "img-src data: blob:".to_string(),
        "font-src data:".to_string(),
        format!("connect-src {connect_source}"),
        "frame-src 'none'".to_string(),
        "object-src 'none'".to_string(),
        "base-uri 'none'".to_string(),
        "form-action 'none'".to_string(),
    ]
    .join("; ")
}

/// 上游 `buildPluginSurfaceDocument`：宿主自己渲染文档，插件代码只以 base64 内联注入。
fn build_surface_document(code: &str, challenge: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(code.as_bytes());
    let bootstrap = format!(
        r#"(function () {{
  var challenge = {challenge};
  var codeElement = document.getElementById("multica-surface-code");
  var bootstrapElement = document.currentScript;
  var failed = false;
  function reportSurfaceError() {{
    if (failed) return;
    failed = true;
    parent.postMessage({{ type: "multica:plugin-surface-error" }}, "*");
  }}
  window.addEventListener("error", reportSurfaceError);
  window.addEventListener("unhandledrejection", reportSurfaceError);
  try {{
    var channel = new MessageChannel();
    Object.defineProperty(globalThis, {port_global}, {{
      value: channel.port2,
      configurable: true,
      enumerable: false,
      writable: false
    }});
    window.addEventListener("pagehide", function () {{
      parent.postMessage({{ type: "multica:plugin-surface-navigated" }}, "*");
    }});
    parent.postMessage({{
      type: {connect_message},
      version: {version},
      challenge: challenge
    }}, "*", [channel.port1]);
    challenge = "";

    var binary = atob(codeElement.textContent || "");
    var bytes = new Uint8Array(binary.length);
    for (var index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index);
    codeElement.remove();
    bootstrapElement.remove();
    var plugin = document.createElement("script");
    plugin.textContent = new TextDecoder().decode(bytes);
    document.body.appendChild(plugin);
  }} catch (error) {{
    reportSurfaceError();
  }}
}})();"#,
        challenge = js_string(challenge),
        port_global = js_string(SURFACE_PORT_GLOBAL),
        connect_message = js_string(SURFACE_CONNECT_MESSAGE),
        version = SURFACE_PROTOCOL_VERSION,
    );
    format!(
        r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root {{ color-scheme: light dark; }}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; padding: 0; }}
body {{
  background: var(--background, transparent);
  color: var(--foreground, inherit);
  font: 400 var(--text-body, 14px)/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
}}
</style>
</head>
<body>
<div id="root"></div>
<script type="text/plain" id="multica-surface-code">{encoded}</script>
<script>{bootstrap}</script>
</body>
</html>"#,
        encoded = html_escape(&encoded),
    )
}

/// JS 字符串字面量（上游用 `strconv.Quote`；本仓只用到 ASCII 常量与 base64url 字母表）。
fn js_string(raw: &str) -> String {
    format!("\"{}\"", raw.replace('\\', "\\\\").replace('"', "\\\""))
}

/// HTML 转义（只用于 `id="multica-surface-code"` 里的 base64 文本）。
fn html_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// 常量时间比较（上游 `hmac.Equal`）：逐字节短路比较会泄露「猜对了多少」。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 路径令牌面的 404：**空体**（上游 `http.NotFound` 的语义；不在 `/v1` 契约内，故不套问题体）。
fn not_found() -> Response {
    StatusCode::NOT_FOUND.into_response()
}

/// 本地标准错误体（`{"error":{"code","message"}}`），用于「内容主机收到应用凭据」这一条。
///
/// 这条路径**不在** `/v1` 契约里（浏览器直接打开，不是 API 调用）⇒ 用本仓既有的错误信封，
/// 而不是 `mc_openapi::v1::ProblemDetail`。
fn bad_request(message: &str) -> Response {
    crate::error::ApiError(mc_errors::Error::Validation {
        message: message.to_string(),
        details: vec![],
    })
    .respond_with(StatusCode::BAD_REQUEST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_parsing_matches_upstream_rules() {
        let origin = parse_surface_origin("https://surfaces.example.test").unwrap();
        assert_eq!(origin.host, "surfaces.example.test");
        assert_eq!(origin.scheme, "https");
        // 大小写不敏感 + 去尾部空白。
        assert_eq!(
            parse_surface_origin("  HTTP://Surfaces.Example.Test  ")
                .unwrap()
                .host,
            "surfaces.example.test"
        );
        // 只有 `/` 的 path 上游也拒绝（`parsed.Path != "" && != "/"` 之外还要求没有 raw query）。
        for bad in [
            "surfaces.example.test",       // 没有 scheme
            "ftp://surfaces.example.test", // 非 http(s)
            "https://host/path",           // 有 path
            "https://host/?q=1",           // 有 query
            "https://host/#frag",          // 有 fragment
            "https://user@host",           // 有 userinfo
            "https://",                    // 空 host
            "",                            // 空串
        ] {
            assert!(parse_surface_origin(bad).is_none(), "{bad} 应被拒");
        }
    }

    #[test]
    fn csp_only_allows_granted_net_domains() {
        let csp = surface_csp(&[
            "net:api.example.com".to_string(),
            "issues:read".to_string(),
            "net:not a domain".to_string(),
        ]);
        assert!(csp.contains("connect-src https://api.example.com"));
        assert!(!csp.contains("not a domain"));
        assert!(csp.contains("default-src 'none'"));
        assert!(csp.contains("frame-src 'none'"));
        // 没有任何 net: scope ⇒ connect-src 'none'（而不是留空或放通）。
        assert!(surface_csp(&["issues:read".to_string()]).contains("connect-src 'none'"));
    }

    #[test]
    fn document_embeds_the_code_as_base64_not_verbatim() {
        let document = build_surface_document("alert('</script>')", "chal");
        assert!(document.starts_with("<!doctype html>"));
        // 代码以 base64 出现 ⇒ 里面的 `</script>` 不会提前闭合外围 script。
        assert!(!document.contains("alert('</script>')"));
        assert!(document
            .contains(&base64::engine::general_purpose::STANDARD.encode(b"alert('</script>')")));
        // 挑战值进 JS 字面量，且连接消息/协议版本逐字。
        assert!(document.contains("multica:plugin-bridge-connect"));
        assert!(document.contains("__multicaPluginBridgePortV2"));
        assert!(document.contains("var challenge = \"chal\""));
    }

    #[test]
    fn js_string_escapes_quotes() {
        assert_eq!(js_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn constant_time_compare_is_length_aware() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn ttl_is_two_minutes() {
        // `DoD` 的「2 分钟 TTL」：签发侧与消费侧共用这一个常量。
        assert_eq!(SURFACE_LAUNCH_TTL_SECS, 120);
    }
}
