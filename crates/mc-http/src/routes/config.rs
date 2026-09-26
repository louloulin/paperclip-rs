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
//! ## anchor 期（本文件由 M10-0 `LUM-2102` 建桩：形状 + 签名，实现归 M10-4）
//!
//! `router()` 是**空** `Router::new()` ⇒ **零注册键**、`/api/config` 在 ⑨ 里**停在
//! `unmounted`**（本片 ⑨ 的 `pass/mismatch/unmounted` 必须逐字不变 = `14 / 23 / 22`）。
//! 🔴 **不得**注册 501 占位：那会让 ⑨ 从 `unmounted` 变 **`mismatch`**（`23 → 41`）。
//!
//! 形态：上游 plain `r.Get("/api/config", h.GetConfig)`（`router.go:1478`）⇒ 只注册
//! **无尾斜杠**那一形态（`docs/64` §1.4 实测 `dual-form required: 0`）。
//!
//! ## 边界纪律（`docs/64` §2.4，两条硬纪律）
//!
//! ① **不得** `serde` 序列化 `mc_config::Config`（进程级配置可以含 DB URL 与密钥）；
//! ② 字段值**只**来自 ① env ② crate 常量 ③ `mc-storage` / `mc-feature-flags` 的只读查询 ——
//!    上游注释逐字：*never user- or tenant-scoped data* ⇒ 匿名可读 + **不触库**。

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
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

/// `GET /api/config` 的 handler 签名（**实现归 M10-4**：17 字段装配 + 三个 helper
/// `daemonSetupURLsFromEnv` / `normalizePublicURL` / `isOfficialCloudDaemonConfig`）。
///
/// ⚠️ 签名是 anchor 钉死的形状：返回 `ApiResult<Json<AppConfig>>`（上游 `GetConfig` 永不失败，
/// 但本仓的错误映射走 `ApiError` ⇒ 保持与其它 handler 同形，便于 M10-4 之后加 `?`）。
pub async fn get_config(_state: State<Arc<AppState>>) -> ApiResult<Json<AppConfig>> {
    unimplemented!("M10-4（LUM-2106）落地 GET /api/config 的 17 字段装配（docs/64 §2.2）")
}

/// `/api/config` 切片（M10-4 在这里填 `.route("/api/config", get(get_config))`）。
///
/// anchor 期为空 ⇒ `mount_slice_probes()` 合并它之后**零注册键**。
pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
}
