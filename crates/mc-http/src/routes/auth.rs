//! `/auth/*` + `/api/auth/*` 切片。
//!
//! M1-B 认证流（对应 upstream `multica/server/internal/handler/auth.go` +
//! `session.go`）：
//!
//! | Method | Path | Handler | 上游对应 |
//! | --- | --- | --- | --- |
//! | POST | `/auth/send-code` | `send_code` | `Handler.SendCode` (auth.go) |
//! | POST | `/auth/verify-code` | `verify_code` | `Handler.VerifyCode` (auth.go) |
//! | POST | `/auth/logout` | `logout` | `Handler.Logout` (auth.go) |
//! | POST | `/api/auth/refresh` | `refresh_session` | `Handler.RefreshSession` (session.go) |
//! | POST | `/auth/google` | `google_login` | `Handler.GoogleLogin` (auth.go:546) |
//!
//! 路径遵循 upstream：浏览器登录页用 `/auth/send-code`、`/auth/verify-code`，
//! 而已经 cookie 化的会话通过 `/api/auth/refresh` 续期。
//!
//! 邮件发送在本 sub-issue 用 `tracing::info!` 占位，不接 SMTP ——
//! M9（mailer）才会接 Resend / SES。`MULTICA_DEV_VERIFICATION_CODE` 在
//! 非 production 模式下作为万能验证码（参考 upstream `isDevVerificationCode`）。

use std::sync::Arc;

use axum::routing::post;
use axum::Router;

use crate::state::AppState;

mod cli_token;
mod code;
mod common;
mod google;
mod session;

use cli_token::cli_token;
use code::{send_code, verify_code};
use google::google_login;
use session::{logout, refresh_session};

pub use cli_token::CliTokenResponse;
pub use code::{
    SendCodeRequest, SendCodeResponse, UserView, VerifyCodeRequest, VerifyCodeResponse,
};
pub use google::{
    GoogleLoginRequest, GoogleLoginResponse, GOOGLE_CODE_ACCOUNT_DISABLED,
    GOOGLE_CODE_ACCOUNT_WITHOUT_EMAIL, GOOGLE_CODE_EMAIL_NOT_ALLOWED,
    GOOGLE_CODE_INVALID_OAUTH_CODE, GOOGLE_CODE_NOT_CONFIGURED, GOOGLE_CODE_SIGNUP_PROHIBITED,
};
pub use session::{LogoutResponse, RefreshRequest, RefreshResponse};

/// Auth 路由切片。
///
/// 返回的 Router 类型是 `Router<Arc<AppState>>`（未注入 state），
/// 在 `mount.rs::router()` 合并后由 `mc-http::router()` 顶层 `.with_state()` 注入。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/auth/send-code", post(send_code))
        .route("/auth/verify-code", post(verify_code))
        .route("/auth/logout", post(logout))
        .route("/auth/google", post(google_login))
        .route("/api/auth/refresh", post(refresh_session))
        // M1-D（LUM-1347）从 LUM-1335（`feat/multica-rs-m1`）cherry-pick 的增量：
        // CLI 登录用的一次性 PAT（浏览器会话 → token）。
        //
        // 路径 M1-E（LUM-1362）修正：上游是 `POST /api/cli-token`
        // （`router.go:1628`，**没有** `/auth` 这一层）；M1-B 曾误注册为
        // `/api/auth/cli-token`。见 docs/17-M1-CONTRACT-GAPS.md 缺口 #7。
        .route("/api/cli-token", post(cli_token))
    // 注：`/api/me` 由 M1-A 的 routes/workspaces.rs 真实实现（仲裁 #4）。
    // 此处不得再注册同 path+method —— axum 0.7 `.merge` 重复注册会 panic。
}

#[cfg(test)]
mod tests;
