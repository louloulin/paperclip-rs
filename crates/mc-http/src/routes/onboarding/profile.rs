//! 档案/问卷面（`PATCH /api/me/onboarding`）—— **写者 M9-3** / `LUM-1818`。
//!
//! 上游：`internal/handler/onboarding.go` 的 `PatchOnboarding`（372 行文件里的 1 条）。
//! 形状（`QuestionnaireAnswers` 的 `stringOrSlice` 宽容、`in_flow_resolved`、v2 版本判定）
//! 全在 [`mc_core::onboarding`] —— 本文件**不复制**一份校验，只做「体 → 仓储 → 响应」。
//!
//! # 授权链（`docs/62` §1.5：**A 行** = user-scoped，零 workspace 上下文）
//!
//! 上游 `router.go:1617` 把 5 条挂在同一个 user-scoped 组里（`middleware.Auth`，
//! 与 `GET/PATCH /api/me` 同一组）⇒ 本片**一条** user 闸。
//! ⚠️ 形态差异（登记 `docs/32` §48 / §9.18）：`/api/me/onboarding/complete` 挂在
//! `workspaces.rs` 里，那条**带** `route_layer(require_user)`（`from_fn_with_state`）；
//! 而本目录三个 `router()` **拿不到** `Arc<AppState>`（anchor 冻结的签名 `router()`），
//! `axum::middleware::from_fn` 在 `Router<Arc<AppState>>` 上推不出 `S` ⇒ 按本仓既有
//! 做法（`routes/agents.rs` 同一个形状）改用 [`AuthUser`] 提取器判 401。
//! 无会话 ⇒ **401**。
//!
//! # 体上限（上游 `patchOnboardingBodyLimit = 16 KiB`）
//!
//! 上游逐字注释：「Bound the body so the JSONB column can't be weaponized as bulk storage
//! —— otherwise every subsequent `/api/me` read would have to return the bloat.」
//! ⚠️ 上游用 `http.MaxBytesReader` + `json.Decode`，**超限表现为解码错误 ⇒ 400**
//! （不是 413）⇒ 本片照抄那一档：超限与非法体**同一条 400** `invalid request body`。
//!
//! # 问卷状态机（`DoD` 第 1 条）
//!
//! - **`complete` 的幂等**不在这条路由上，在 `POST /api/me/onboarding/complete`
//!   （`routes/workspaces.rs`，`COALESCE(onboarded_at, now())`，见
//!   [`mc_repos::onboarding::OnboardingRepo::mark_onboarded`]）；
//! - **「缺 `role` / `use_case`」的处置**：上游 `PatchOnboarding` 对**任何**问卷都 **200**
//!   （它不校验，缺项只影响漏斗计数），`DoD` 第 1 条写的「⇒ 400」与上游冲突 ⇒
//!   **按上游落地**（偏离与一行翻转点登记在 `docs/32` §48 / §9.18）。本文件给出的
//!   可测等价物是：缺 `role` / `use_case` 的问卷**逐字落库**，而
//!   [`QuestionnaireAnswers::in_flow_resolved`] 为 `false` ⇒ **不算问卷已答完**
//!   （`version != 2` 同理不算，见 `docs/62` §9.7 的 `complete()` 判据）。
//!
//! # 形态（`docs/62` §1.4 实测 `dual-form required: 3`，本簇不在其中）
//!
//! 只按上游字面量注册 `PATCH /api/me/onboarding` **那一形态**，不补尾斜杠别名。
//!
//! `#[cfg(test)] mod tests` 的路径锚在本文件（`onboarding/mod.rs` 是 anchor 冻结文件，
//! **不得**编辑）⇒ 用 `#[path]` 挂到同目录的 `tests.rs`，用例的子模块再落 `tests/`。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::patch;
use axum::{Json, Router};
use mc_core::onboarding::CompleteOnboardingRequest;
use mc_repos::onboarding::OnboardingRepo;

use crate::error::{ApiError, ApiResult};
// ⚠️ 本仓有**两个** `AuthUser`：`middleware::authn::AuthUser` 是中间件写进
// `Extensions` 的那一个（**不是**提取器），`routes::auth_user::AuthUser` 才是提取器。
use crate::routes::auth_user::AuthUser;
use crate::routes::workspaces::{me_response, MeResponse};
use crate::state::AppState;
use mc_errors::Error;

/// 上游 `patchOnboardingBodyLimit = 16 * 1024`（逐字）。
pub const PATCH_ONBOARDING_BODY_LIMIT: usize = 16 * 1024;

/// 上游 `writeError(w, 400, "invalid request body")` 逐字。
const MSG_BODY_INVALID: &str = "invalid request body";

/// 档案/问卷切片：`PATCH /api/me/onboarding`（1 条）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/me/onboarding", patch(patch_onboarding))
}

/// `PATCH /api/me/onboarding`（上游 `PatchOnboarding`）。
///
/// 判定顺序逐字：体上限（⇒ 400）→ 反序列化（⇒ 400）→ 仓储的 `COALESCE($2, col)` 写 → 响应
/// `userToResponse`。**不**落任何「当前第几步」——上游逐字注释：每条 onboarding 入口都从
/// Welcome 开始，那一步**故意不持久化**。
pub async fn patch_onboarding(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<MeResponse>> {
    // 🔴 取**原始** JSON（上游 `json.RawMessage`）⇒ 落库逐字，**不**经本仓的
    // `QuestionnaireAnswers` 重序列化（那会把客户端没写的字段补成默认值 = 改写用户数据）。
    // 上游那一层是 `*json.RawMessage` ⇒ **任何**合法 JSON 都接受（含非对象），本片照抄；
    // 形状语义由 `mc_core::onboarding` 的纯函数在**读**侧负责。
    let raw: serde_json::Value = decode_json(&body).map_err(ApiError)?;
    let questionnaire = raw.get("questionnaire").cloned();
    let repo = OnboardingRepo::new(state.db.clone());
    let user = repo
        .patch_questionnaire(user.id(), questionnaire.as_ref())
        .await
        .map_err(|e| repo_err(e, "user"))?;
    me_response(&state, user).await
}

/// `POST /api/me/onboarding/complete`（上游 `CompleteOnboarding`，`router.go:1618`）。
///
/// **本 handler 的路由注册在 [`crate::routes::workspaces`]**（上游那条也是挂在 `/api/me` 的
/// user-scoped 组里；anchor 的 `onboarding/mod.rs` 模块头逐字点名了这个「不在本目录的
/// 第五条」）；实现放在本文件是因为它与 [`patch_onboarding`] 共用 `me_response` 与错误口径。
///
/// # 幂等（`DoD` 第 1 条的判据就在这一格）
///
/// 上游逐字：「Idempotent: the underlying query uses `COALESCE` so the original timestamp is
/// preserved if called more than once.」⇒ **重复调用不改状态**：第二次返回的
/// `onboarded_at` 与第一次**逐字相同**（判据在 `tests/db.rs`，直读那一列比对）。
///
/// # 体是**可选**的
///
/// 上游逐字：「Body is optional — an empty body is a legal legacy call.」⇒ 空体 **不**是 400
/// （与 [`decode_body`] 的 `PATCH` 语义**相反**，这是两条路由最容易搞混的一格）。
///
/// # `completion_path` 只是分析维度
///
/// 上游：合法值只有 5 个（[`mc_core::onboarding::CompletionPath::VALID`]），**非法/缺省一律
/// 折成 `unknown`**，**不**是 400（它不写任何状态）；`workspace_id` 只做 UUID 形状校验
/// （畸形值 fail fast），同样**不**写。⇒ 本 handler **只**做「校验 + 翻 `onboarded_at`」。
pub async fn complete_onboarding(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<MeResponse>> {
    // 空体 = 合法 legacy 调用（上游只在 `ContentLength > 0` 时才解码）。
    let request: CompleteOnboardingRequest = if body.is_empty() {
        CompleteOnboardingRequest::default()
    } else {
        serde_json::from_slice(&body).map_err(|_| ApiError(invalid_body()))?
    };
    // 畸形 `workspace_id` 要 fail fast（上游 `parseUUIDOrBadRequest`），但**不**用它写任何东西。
    if let Some(raw) = request.workspace_id.as_deref().filter(|v| !v.is_empty()) {
        mc_core::Id::parse(raw.trim()).map_err(|_| ApiError(invalid_body()))?;
    }

    let repo = OnboardingRepo::new(state.db.clone());
    let user = repo
        .mark_onboarded(user.id())
        .await
        .map_err(|e| repo_err(e, "user"))?;
    me_response(&state, user).await
}

/// 16 KiB 上限 + 反序列化（上游 `MaxBytesReader` + `json.Decode` 的本地等价）。
///
/// ⚠️ 上游**不区分**「超限」与「非法 JSON」（两者都落到 `writeError(400, "invalid
/// request body")`）⇒ 本片也**不**引入 413。
///
/// 返回 [`Error`] 而不是 [`ApiError`]：这样**不碰库**的那一半用例能直接 `.expect()`
/// （`ApiError` 按本仓约定**不**实现 `Debug`）。
/// 16 KiB 上限 + 任意合法 JSON（上游 `json.NewDecoder(r.Body).Decode` 的本地等价）。
///
/// ⚠️ 收 `serde_json::Value` 而不是 `PatchOnboardingRequest`：handler 要的是**原始**那一段
/// （上游 `json.RawMessage`），不是本仓的类型化 DTO —— 否则落库就会把客户端没写的字段
/// 补成默认值（那是在改写用户数据）。
///
/// 返回 [`Error`] 而不是 [`ApiError`]：这样**不碰库**的那一半用例能直接 `.expect()`
/// （`ApiError` 按本仓约定**不**实现 `Debug`）。
pub fn decode_json(body: &Bytes) -> Result<serde_json::Value, Error> {
    if body.len() > PATCH_ONBOARDING_BODY_LIMIT {
        return Err(invalid_body());
    }
    // 空体是**非法**的（上游 `json.NewDecoder(r.Body).Decode` 对空体返回 `EOF` ⇒ 400）。
    serde_json::from_slice(body).map_err(|_| invalid_body())
}

fn invalid_body() -> Error {
    Error::Validation {
        message: MSG_BODY_INVALID.to_string(),
        details: Vec::new(),
    }
}

fn repo_err(e: mc_repos::RepoError, resource: &str) -> ApiError {
    ApiError(match e {
        mc_repos::RepoError::NotFound => Error::NotFound {
            resource: resource.into(),
        },
        mc_repos::RepoError::Conflict => Error::Conflict {
            message: format!("{resource} already exists"),
        },
        mc_repos::RepoError::Db(msg) => Error::Internal(msg),
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
