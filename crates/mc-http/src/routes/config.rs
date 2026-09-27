//! `GET /api/config` —— 浏览器前端**登录前**就要读的公开启动配置（**M10-4 原地填充**，
//! `docs/64` §2.2 / §2.4 / §4.1 第 5 行）。
//!
//! 上游：`server/internal/handler/config.go`（223 行）的 `AppConfig` **17 个字段** +
//! `EvaluateFrontendPublicFlags` 的 **6 个公开 feature flag**。
//!
//! ## 🔴 契约的权威 pin 是 `90e0bdf830436b3981b32a7017e1c18d41c7cdea`，不是 `f41fae6b08fb`
//!
//! 本仓有**两条**不同的上游 pin（`docs/64` §1.6，差 12 提交）：⑦ 路由表钉 `f41fae6b08fb`、
//! ⑨ 契约 fixture（`contracts/golden/PIN`）钉 `90e0bdf`。两者的实际差异只在
//! `config.go` **+7 行** —— 就是下面**第 17 个字段** `issue_create_properties_supported`。
//! ⇒ **字段集读 `90e0bdf`，路由行号（`router.go:1478`）读 `f41fae6b08fb`**。
//!
//! ## ⚠️ ⑨ 的 17 条 `config/*` fixture 的 `json_subset` **全是空对象**
//!
//! 它们只证明"挂上了、200、body 是 JSON 对象"，**不**证明字段集（`docs/64` §2.2 实测）。
//! 字段级判据只有两处：上游 `config_test.go`（`90e0bdf`）的 19 个断言 + **M10-5** 的
//! `contracts/golden-local/**`。**不许**把 ⑨ 变绿当成 config 做完。
//!
//! ## M10-4 落地（`LUM-2106`）后
//!
//! `router()` 真实注册**一个键** `/api/config` ⇒ ⑨ 里那 17 条 `config/*` fixture 从
//! `unmounted` 变 **`pass`**。🔴 但它们的 `json_subset` 全是空对象 ⇒ ⑨ **只**证明
//! "挂上了、200、body 是 JSON 对象"；字段级判据是本文件的用例 + 上游 `config_test.go`
//! 的 19 个断言（`docs/64` §2.2）。**不许**把 ⑨ 变绿当成 config 做完。
//!
//! 形态：上游 plain `r.Get("/api/config", h.GetConfig)`（`router.go:1478`）⇒ 只注册
//! **无尾斜杠**那一形态（`docs/64` §1.4 实测 `dual-form required: 0`）。
//!
//! ## 边界纪律（`docs/64` §2.4，两条硬纪律）
//!
//! ① **不得** `serde` 序列化 `mc_config::Config`（进程级配置可以含 DB URL 与密钥）；
//! ② 字段值**只**来自 ① env ② crate 常量 ③ `mc-storage` / `mc-feature-flags` 的只读查询 ——
//!    上游注释逐字：*never user- or tenant-scoped data* ⇒ 匿名可读 + **不触库**。

//! ## 实现（M10-4，`LUM-2106`）
//!
//! 三个 helper（`daemonSetupURLsFromEnv` / `normalizePublicURL` / `isOfficialCloudDaemonConfig`）
//! 与 host 归一化（`urlHostEquals` / `canonicalURLHost`）都是**逐字移植**，不含本仓发明的语义。
//! 字段装配集中在 [`AppConfig::from_env_with`]：它收一个「名字 → 值」的查询函数（生产 =
//! 进程 env，单测 = 注入闭包），**因此本文件既不碰用户/租户数据也不触库**（`docs/64` §2.2
//! 的硬要求：字段值只来自 ① env ② crate 常量 ③ `mc-feature-flags` 的只读求值）。
//!
//! ### 新增 env 清单（`docs/64` §2.4 第 ② 条要求逐条登记；**均只由本路由读**）
//!
//! | env | 字段 | 缺省 |
//! | --- | --- | --- |
//! | `MULTICA_CDN_DOMAIN` | `cdn_domain` | `""`（键**总出现**，`cdn_domain` 无 `omitempty`） |
//! | `ALLOW_SIGNUP` | `allow_signup` | `""` ⇒ `true`（上游口径：只有 `"false"` 才关） |
//! | `GOOGLE_CLIENT_ID` | `google_client_id` | `""`（键不出现） |
//! | `DISABLE_WORKSPACE_CREATION` | `workspace_creation_disabled` | `""` ⇒ `false`（**逐字** `"true"` 比较） |
//! | `MULTICA_DAEMON_SERVER_URL` / `MULTICA_PUBLIC_URL` | `daemon_server_url` | 回落链第三级 = `app_url` |
//! | `MULTICA_APP_URL` / `FRONTEND_ORIGIN` | `daemon_app_url` | `""` ⇒ 两个 URL **都**不出现 |
//! | `POSTHOG_API_KEY` / `POSTHOG_HOST` | `posthog_key` / `posthog_host` | `""` |
//! | `ANALYTICS_DISABLED` | 三个 analytics 字段的**短路闸** | `""` ⇒ 不短路 |
//! | `ANALYTICS_ENVIRONMENT` / `APP_ENV` | `analytics_environment` | `"dev"` |
//!
//! `MULTICA_VCS_INTEGRATION_ENABLED` **不是**新增 env：只读复用 `state.integrations::VcsKeys`
//! （M8-2 落的读取口，`AppState::vcs_keys.is_enabled()`）——`docs/64` §2.2 第 8 行的要求。
//! `MULTICA_FEATURE_FLAGS_FILE` / `FF_*` 由 `mc-feature-flags/src/frontend.rs` 读（同一份
//! `/api/config` 面，不在此处解析）。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::Json;
use axum::Router;
use serde::Serialize;

use crate::error::ApiResult;
use crate::state::AppState;

/// `GET /api/config` 的响应形状（上游 `AppConfig`，`90e0bdf` 逐字：**字段声明顺序与 JSON 键
/// 顺序与上游一致**，`omitempty` 栏决定"缺省时键是否出现"）。
///
/// ⚠️ 本结构是**白名单**：只放匿名安全字段。新增字段前先读上游 `GetConfig` 的注释
/// （*Only add fields here that are safe to expose to anonymous callers*）。
///
/// `#[allow(clippy::struct_excessive_bools)]` 是**有意**的：这 11 个 bool 就是上游 `AppConfig`
/// 的逐字形状（`90e0bdf`），其中 4 个是**能力声明**（客户端 fail-closed 的判据）⇒ 拆成枚举
/// 或状态机会让 JSON 键与上游不再逐字对应。门 ③ 要求零警告，所以在这里显式豁免并登记。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, Serialize)]
pub struct AppConfig {
    /// 1. CDN 域名；本地新 env `MULTICA_CDN_DOMAIN`（缺省 `""`）。**无 omitempty ⇒ 总出现**。
    pub cdn_domain: String,
    /// 2. CDN 是否只服务**签名**内容（CloudFront）。本仓无 `CloudFront` ⇒ **恒 `false`**、
    ///    键**不出现**（登记为已知差异：签名下载走 `mc-storage` 自己的 HMAC，M10-B1）。
    #[serde(skip_serializing_if = "is_false")]
    pub cdn_signed: bool,
    /// 3. `ALLOW_SIGNUP != "false"`。**无 omitempty**。
    pub allow_signup: bool,
    /// 4. `GOOGLE_CLIENT_ID`（omitempty）。
    #[serde(skip_serializing_if = "is_blank")]
    pub google_client_id: String,
    /// 5. `DISABLE_WORKSPACE_CREATION == "true"`（**不是**宽松解析，逐字；omitempty）。
    #[serde(skip_serializing_if = "is_false")]
    pub workspace_creation_disabled: bool,
    /// 6. `MULTICA_DAEMON_SERVER_URL` → `MULTICA_PUBLIC_URL` → `app_url`，经
    ///    `normalizePublicURL` 去尾斜杠（omitempty；**只在 `app_url` 非空时才可能非空**）。
    #[serde(skip_serializing_if = "is_blank")]
    pub daemon_server_url: String,
    /// 7. `MULTICA_APP_URL` → `FRONTEND_ORIGIN`（omitempty）；`multica.ai` 主机 ⇒ 与上面
    ///    这一条**都置空**（`isOfficialCloudDaemonConfig`，逐字移植）。
    #[serde(skip_serializing_if = "is_blank")]
    pub daemon_app_url: String,
    /// 8. **只读复用**已交付的接缝：`crates/mc-http/src/state/integrations.rs` 的
    ///    `MULTICA_VCS_INTEGRATION_ENABLED`（M8-2 落的）—— **不新造开关**（omitempty）。
    #[serde(skip_serializing_if = "is_false")]
    pub vcs_integration_available: bool,
    /// 9. `POSTHOG_API_KEY`；`ANALYTICS_DISABLED ∈ {true,1}` ⇒ 空。**无 omitempty**。
    pub posthog_key: String,
    /// 10. `POSTHOG_HOST`；空且 key 非空 ⇒ 回填 `https://us.i.posthog.com`。**无 omitempty**。
    pub posthog_host: String,
    /// 11. `ANALYTICS_ENVIRONMENT` → `APP_ENV` → `"dev"`（归一化 `production/staging/dev`）。
    ///     **无 omitempty**（`dev` 是缺省值，不是空串）。
    pub analytics_environment: String,
    /// 12. 6 个公开 flag（`EvaluateFrontendPublicFlags`，落 `mc-feature-flags/src/frontend.rs`）。
    ///     `BTreeMap` 的键序 = 上游 Go `map[string]bool` 的**排序**键序，逐字对齐（omitempty）。
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub feature_flags: BTreeMap<String, bool>,
    /// 13. 本 build 的属性（**无 omitempty**）—— ✅ 本地实测成立（`projects/resource_ref.rs`
    ///     的 `execution_mode ∈ {in_place, worktree}` + 能力门 422 `daemon_version_unsupported`）。
    pub local_worktree_supported: bool,
    /// 14. `agent` create/update 是否持久化 `conversation_starters`（**无 omitempty**）——
    ///     ✅ 本地实测成立（`mc-repos/src/agent/`）。
    pub agent_conversation_starters_supported: bool,
    /// 15. `POST /api/issues` 是否校验并持久化 `properties` bag（**无 omitempty**）。
    ///     ❌ **本地取 `false`**：`CreateIssueRequest`（`routes/issues/dto.rs:242-262`）没有
    ///     `properties` 字段 ⇒ serde 静默忽略该 bag（正是上游注释警告的失败模式）——
    ///     登记为已知差异，补实现属 M2-A 面、本波不做（客户端 fail-closed ⇒ `false` 是安全一侧）。
    pub issue_create_properties_supported: bool,
    /// 16. `DELETE /api/comments/{id}` 是否只删该评论、保留回复（**无 omitempty**）——
    ///     ✅ 本地实测成立（`/keep-replies` 路由**已注册** + `CommentRepo::soft_delete(id, true)`）。
    pub comment_delete_keep_replies_supported: bool,
    /// 17. 运行中的 API 版本（`env!("CARGO_PKG_VERSION")`），**仅自建版**：`multica.ai` 抑制分支
    ///     与第 6/7 条同一处（omitempty）。
    #[serde(skip_serializing_if = "is_blank")]
    pub server_version: String,
}

/// `omitempty` 的逐字段替身：Go 的 `omitempty` 对 `false` 判"零值"。
///
/// `#[allow(clippy::trivially_copy_pass_by_ref)]`：签名由 `serde` 的
/// `skip_serializing_if` 定死（它以 `&T` 调用谓词），不是随手传引用。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(v: &bool) -> bool {
    !*v
}

/// `omitempty` 的逐字段替身：Go 的 `omitempty` 对 `""` 判"零值"。
fn is_blank(v: &str) -> bool {
    v.is_empty()
}

/// `GET /api/config` 的 handler。
///
/// 签名是 anchor 钉死的形状（返回 `ApiResult<Json<AppConfig>>`）：上游 `GetConfig` 永不失败
/// —— 它是**离线**装配（读 env + 进程常量），没有任何可失败的来源，所以这里也不会失败；
/// 保持 `ApiResult` 只是为了与其它 handler 同形。
pub async fn get_config(state: State<Arc<AppState>>) -> ApiResult<Json<AppConfig>> {
    // 只读复用 M8 的 VCS 开关读取口（**不**在这里重新解析 env，见模块头）。
    let cfg =
        AppConfig::from_env_with(|name| std::env::var(name).ok(), state.vcs_keys.is_enabled());
    Ok(Json(cfg))
}

/// 上游 `daemonSetupURLsFromEnv`（`config.go`）的逐字移植。
///
/// 判据链：`MULTICA_DAEMON_SERVER_URL` → `MULTICA_PUBLIC_URL` → `app_url`；
/// **`app_url` 为空 ⇒ 两个都空**；`multica.ai` 主机 ⇒ 两个都置空。
fn daemon_setup_urls_from_env<F>(get: &F) -> (String, String)
where
    F: Fn(&str) -> Option<String>,
{
    let mut server_url = normalize_public_url(&env_or_empty(get, "MULTICA_DAEMON_SERVER_URL"));
    if server_url.is_empty() {
        server_url = normalize_public_url(&env_or_empty(get, "MULTICA_PUBLIC_URL"));
    }
    let app_url = resolve_frontend_app_url(get);
    if app_url.is_empty() {
        return (String::new(), String::new());
    }
    if server_url.is_empty() {
        server_url.clone_from(&app_url);
    }
    if is_official_cloud_daemon_config(&app_url) {
        return (String::new(), String::new());
    }
    (server_url, app_url)
}

/// 上游 `resolveFrontendAppURL`：`MULTICA_APP_URL` → `FRONTEND_ORIGIN`，逐字归一化。
fn resolve_frontend_app_url<F>(get: &F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let app_url = normalize_public_url(&env_or_empty(get, "MULTICA_APP_URL"));
    if app_url.is_empty() {
        return normalize_public_url(&env_or_empty(get, "FRONTEND_ORIGIN"));
    }
    app_url
}

/// 上游 `normalizePublicURL`：`TrimRight(TrimSpace(raw), "/")`。
///
/// ⚠️ **只**去**尾**斜杠（上游逐字 `TrimRight`），不去首斜杠也不折叠中间的 `//`。
fn normalize_public_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_owned()
}

/// 上游 `isOfficialCloudDaemonConfig`：只按**前端主机**（`multica.ai`）判官方云。
fn is_official_cloud_daemon_config(app_url: &str) -> bool {
    url_host_equals(app_url, "multica.ai")
}

/// 上游 `urlHostEquals`（`config_test.go::TestURLHostEqualsCanonicalizesCommonHostForms` 逐字）。
fn url_host_equals(raw: &str, want: &str) -> bool {
    let host = canonical_url_host(raw);
    if host.is_empty() {
        return false;
    }
    let want = want.trim().to_lowercase();
    host == want.strip_suffix('.').unwrap_or(&want)
}

/// 上游 `canonicalURLHost` 的等价物：`u.Hostname()`，**只**去**一个**尾点（上游 `TrimSuffix`）。
///
/// 上游用 `net/url`；本仓 `mc-http` 的依赖图里**没有** `url` crate（`docs/64` §2.4 的纪律
/// 不许为一个 host 比较新拉一条边）⇒ 这里按 `net/url` 的口径手拆：
///
/// 1. 先按 `scheme://authority[/path…]` 取 authority；`u.Hostname()` 只认 authority；
/// 2. authority 去掉 `userinfo@`（上游 `Hostname()` 逐字不含 userinfo）；
/// 3. 去掉 `:port`（IPv6 字面量 `[::1]:80` 走方括号分支）；
/// 4. 解析不出主机且原文**不含 `://`** ⇒ 补 `https://` 重试（上游同款：裸主机 / 带端口）。
fn canonical_url_host(raw: &str) -> String {
    let raw = raw.trim();
    let mut host = authority_host(raw);
    if host.is_empty() && !raw.contains("://") {
        host = authority_host(&format!("https://{raw}"));
    }
    host.strip_suffix('.').unwrap_or(&host).to_lowercase()
}

/// 取 `scheme://authority` 里的主机部分（不含 userinfo / port）。
fn authority_host(raw: &str) -> String {
    let Some(rest) = raw.split_once("://").map(|(_, rest)| rest) else {
        // 没有 scheme ⇒ `url.Parse` 的 `Host` 为空（裸主机由调用方补 `https://` 重试）。
        return String::new();
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_and_port = match authority.rsplit_once('@') {
        Some((_, after_at)) => after_at,
        None => authority,
    };
    if let Some(after_bracket) = host_and_port.strip_prefix('[') {
        // IPv6 字面量：主机到 `]` 为止，其后是 `:port`。
        return after_bracket
            .split(']')
            .next()
            .unwrap_or_default()
            .to_owned();
    }
    match host_and_port.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_owned(),
        _ => host_and_port.to_owned(),
    }
}

/// 上游 `analytics.normalizeEnvironment`：只认 `production/prod`、`staging/stage`、
/// `development/dev/test/local`，其余 ⇒ `""`（触发回落）。
fn normalize_environment(v: &str) -> &'static str {
    match v.trim().to_lowercase().as_str() {
        "production" | "prod" => "production",
        "staging" | "stage" => "staging",
        "development" | "dev" | "test" | "local" => "dev",
        _ => "",
    }
}

/// 上游 `analytics.EnvironmentFromEnv`：`ANALYTICS_ENVIRONMENT` → `APP_ENV` → `"dev"`。
fn analytics_environment_from_env<F>(get: &F) -> String
where
    F: Fn(&str) -> Option<String>,
{
    let v = normalize_environment(&env_or_empty(get, "ANALYTICS_ENVIRONMENT"));
    if !v.is_empty() {
        return v.to_owned();
    }
    let v = normalize_environment(&env_or_empty(get, "APP_ENV"));
    if !v.is_empty() {
        return v.to_owned();
    }
    "dev".to_owned()
}

/// 查询函数的"缺省"包装：没配 / 非 UTF-8 都当空串（上游 `os.Getenv` 的零值口径）。
fn env_or_empty<F>(get: &F, name: &str) -> String
where
    F: Fn(&str) -> Option<String>,
{
    get(name).unwrap_or_default()
}

impl AppConfig {
    /// 17 字段装配（上游 `GetConfig` 的函数体逐字）。`get` 是「名字 → 值」的查询函数。
    ///
    /// ⚠️ `vcs_integration_available` **不走** `get`：它只读复用 `AppState::vcs_keys`
    /// （M8-2 的 env 读取口）—— `docs/64` §2.2 第 8 行明令"不新造开关"。
    pub fn from_env_with<F>(get: F, vcs_integration_available: bool) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let (daemon_server_url, daemon_app_url) = daemon_setup_urls_from_env(&get);
        // 上游用 `!isOfficialCloudDeployment()`（= 同一个 `resolveFrontendAppURL()` 信号）。
        let official_cloud = is_official_cloud_daemon_config(&resolve_frontend_app_url(&get));

        // 分析三字段共用一个短路闸：`ANALYTICS_DISABLED ∈ {true,1}` ⇒ 三个字段**全空**
        // （注意 `analytics_environment` 此时是**空串**而不是 `"dev"` —— 上游的
        // if 块整段不执行，Go 零值就是 `""`；该字段无 `omitempty` ⇒ 键出现、值为 `""`）。
        let disabled = {
            let v = env_or_empty(&get, "ANALYTICS_DISABLED");
            v == "true" || v == "1"
        };
        let (posthog_key, posthog_host, analytics_environment) = if disabled {
            (String::new(), String::new(), String::new())
        } else {
            let key = env_or_empty(&get, "POSTHOG_API_KEY");
            let mut host = env_or_empty(&get, "POSTHOG_HOST");
            if host.is_empty() && !key.is_empty() {
                "https://us.i.posthog.com".clone_into(&mut host);
            }
            (key, host, analytics_environment_from_env(&get))
        };

        Self {
            cdn_domain: env_or_empty(&get, "MULTICA_CDN_DOMAIN"),
            // 本仓无 CloudFront signer ⇒ 恒 false、键不出现（已知差异，见 `docs/32` §45）。
            cdn_signed: false,
            allow_signup: env_or_empty(&get, "ALLOW_SIGNUP") != "false",
            google_client_id: env_or_empty(&get, "GOOGLE_CLIENT_ID"),
            // 逐字 `== "true"`，不是宽松布尔解析。
            workspace_creation_disabled: env_or_empty(&get, "DISABLE_WORKSPACE_CREATION") == "true",
            daemon_server_url,
            daemon_app_url,
            vcs_integration_available,
            posthog_key,
            posthog_host,
            analytics_environment,
            feature_flags: mc_feature_flags::frontend::evaluate_frontend_public_flags(get),
            // 13–16 是**本 build 的属性**（上游注释：如果这份代码在跑，能力门就在跑），
            // 不该被部署开关关掉 —— 客户端按"缺键 = 不支持"fail-closed（见 docs/32 §45）。
            local_worktree_supported: true,
            agent_conversation_starters_supported: true,
            // 唯一取 `false` 的能力声明：`CreateIssueRequest` 没有 `properties` 字段
            // ⇒ serde 静默忽略该 bag ⇒ 宣告 true 就是撒谎（客户端会以为存进去了）。
            issue_create_properties_supported: false,
            comment_delete_keep_replies_supported: true,
            server_version: if official_cloud {
                String::new()
            } else {
                env!("CARGO_PKG_VERSION").to_owned()
            },
        }
    }
}

/// `/api/config` 切片：1 个注册键。
///
/// 形态：上游 plain `r.Get("/api/config", h.GetConfig)`（`router.go:1478`）⇒ 只注册**无尾斜杠**
/// 那一形态（`docs/64` §1.4 实测 `dual-form required: 0`；补 `/api/config/` = `EXTRA_ALIAS` 硬失败）。
///
/// ⚠️ 匿名可读：本 router 由 `mount_slice_probes` 直接 `merge` 进全局 router，**不挂**任何
/// `AuthUser` 提取器（`AuthUser` 按路由挂、不全局），与上游"登录前调用"一致（`docs/64` §1.5）。
pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new().route("/api/config", get(get_config))
}

#[cfg(test)]
mod tests;
