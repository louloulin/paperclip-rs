//! `POST /api/me/onboarding/cloud-waitlist` 1 条 —— **写者 M9-3** / `LUM-1818`。
//!
//! 上游：`internal/handler/onboarding.go` 的 `JoinCloudWaitlist`（+ `joinCloudWaitlistRequest`）。
//!
//! # 授权链（A 行 = user-scoped；`router.go:1619` 与 `GET/PATCH /api/me` 同一组）
//!
//! [`AuthUser`] 提取器 ⇒ 无会话 **401**（形态差异见 `profile.rs` 模块头的登记）。
//! 本条**不**需要 workspace 上下文。
//!
//! # 纯副作用（上游注释逐字）
//!
//! 「Pure side effect — does **NOT** complete onboarding. The user still has to pick a real
//! Step 3 path … or Skip to move on. Repeating the call overwrites email + reason.」
//! ⇒ 仓储的 `join_cloud_waitlist` **不**碰 `onboarded_at`（[`SQL_JOIN_WAITLIST`] 那条 SQL
//! 里没有这一列，是一条可被变异测试打中的判据）。
//!
//! # 判定顺序（逐字照上游 `JoinCloudWaitlist`）
//!
//! 体解码 → **400**；`email` 规范化 `ToLower(TrimSpace(·))` → 空 ⇒ **400** `email is required`；
//! `len > 254` ⇒ **400** `email is too long`；解析失败 ⇒ **400** `email is invalid`；
//! `reason` 规范化 `TrimSpace` → `len > 500` ⇒ **400** `reason is too long`；然后**覆盖**写两列。
//!
//! ⚠️ 邮箱解析：上游用 `net/mail.ParseAddress`；本仓**不**引入 RFC 5322 解析库（依赖边被
//! anchor 冻结）⇒ 用 [`mc_core::onboarding::is_acceptable_waitlist_email`] 的**保守**校验，
//! 它在**更严**的方向上偏离（只会多拒、不会放过非法值）。登记 `docs/32` §48 / §9.18。
//!
//! # 🔴 对齐断言的**唯一**口径（`DoD` 第 2 条 / `docs/62` §9.7）
//!
//! 本条**必须**用 [`OnboardingRepo::read_waitlist_columns`] **直读两列**再与请求体逐字比对，
//! **不经** API 回显 —— 响应体是 handler 自己从返回行拼出来的，走响应比对会有
//! 「handler 把请求体原样回显」的假绿。判据落在 `tests/db.rs`。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use mc_core::onboarding::{
    is_acceptable_waitlist_email, is_acceptable_waitlist_reason, normalize_waitlist_email,
    normalize_waitlist_reason, JoinCloudWaitlistRequest, CLOUD_WAITLIST_EMAIL_MAX_LEN,
};
use mc_repos::onboarding::OnboardingRepo;

use crate::error::{ApiError, ApiResult};
// ⚠️ 本仓有**两个** `AuthUser`：`middleware::authn::AuthUser` 是中间件写进
// `Extensions` 的那一个（**不是**提取器），`routes::auth_user::AuthUser` 才是提取器。
use crate::routes::auth_user::AuthUser;
use crate::routes::workspaces::{me_response, MeResponse};
use crate::state::AppState;
use mc_errors::Error;

/// 上游 `writeError` 的四条文本（逐字）。
const MSG_BODY_INVALID: &str = "invalid request body";
const MSG_EMAIL_REQUIRED: &str = "email is required";
const MSG_EMAIL_TOO_LONG: &str = "email is too long";
const MSG_EMAIL_INVALID: &str = "email is invalid";
const MSG_REASON_TOO_LONG: &str = "reason is too long";

/// cloud waitlist 切片（1 条）：写 `user.cloud_waitlist_{email,reason}` 两列。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route(
        "/api/me/onboarding/cloud-waitlist",
        post(join_cloud_waitlist),
    )
}

/// `POST /api/me/onboarding/cloud-waitlist`（上游 `JoinCloudWaitlist`）。
pub async fn join_cloud_waitlist(
    State(state): State<Arc<AppState>>,
    user: AuthUser,
    body: Bytes,
) -> ApiResult<Json<MeResponse>> {
    let request: JoinCloudWaitlistRequest =
        serde_json::from_slice(&body).map_err(|_| validation(MSG_BODY_INVALID))?;

    let email = normalize_waitlist_email(&request.email);
    if email.is_empty() {
        return Err(validation(MSG_EMAIL_REQUIRED));
    }
    // RFC 5321 上限 254（列宽 `VARCHAR(254)`）：先查长度再查格式，与上游同序。
    if email.len() > CLOUD_WAITLIST_EMAIL_MAX_LEN {
        return Err(validation(MSG_EMAIL_TOO_LONG));
    }
    if !is_acceptable_waitlist_email(&email) {
        return Err(validation(MSG_EMAIL_INVALID));
    }
    let reason = normalize_waitlist_reason(&request.reason);
    if !is_acceptable_waitlist_reason(&reason) {
        return Err(validation(MSG_REASON_TOO_LONG));
    }
    // 上游逐字：空 reason ⇒ `pgtype.Text{}`（写 `NULL`），不是空串。
    let reason_param = (!reason.is_empty()).then_some(reason.as_str());

    let repo = OnboardingRepo::new(state.db.clone());
    let user = repo
        .join_cloud_waitlist(user.id(), &email, reason_param)
        .await
        .map_err(|e| repo_err(e, "user"))?;
    me_response(&state, user).await
}

fn validation(msg: &str) -> ApiError {
    ApiError(Error::Validation {
        message: msg.to_string(),
        details: Vec::new(),
    })
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
