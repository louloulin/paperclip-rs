//! `POST /auth/google`（**没有** `/api` 前缀）—— 上游 `Handler.GoogleLogin`
//! （`handler/auth.go:546-728`）的逐条移植。
//!
//! 11 步流程与状态码 / 错误码的对齐表见 `google_login` 上方的文档注释；
//! 登记偏离的完整清单见 `docs/29-W1-GOOGLE.md`。
//!
//! 拆分自拆分前的单文件 `routes/auth.rs`（LUM-2530）：item 逐字搬移。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use mc_auth::session::Session;
use mc_repos::user::{NewUser, UserRepo};
use mc_repos::Repository;

use super::code::UserView;
use super::common::{email_local_part, session_response};
use crate::state::{AppState, GoogleOAuthConfig};

// ============================================================
// POST /auth/google
// ============================================================

/// 上游 `writeErrorCode`/`writeFeatureDisabled` 的等价物。三点：① 状态码由调用方显式给出
/// （`mc_errors::Error` 的固定映射里 `Upstream` 是 500，而上游 `GoogleLogin` 的 502 必须
/// 逐条对齐）；② 错误体沿用本仓 M1 的嵌套 envelope（`mc_errors::ErrorBody`，与 `ApiError`
/// 逐字段同形 —— 上游是扁平 `{"error","code"}`，登记 `docs/29-W1-GOOGLE.md`）；
/// ③ `code` 用上游的字符串常量（`mc_errors::Error::code()` 只能返回固定映射）。
fn google_error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(mc_errors::ErrorBody {
            error: mc_errors::ErrorResponse::new(code, message),
        }),
    )
        .into_response()
}

/// 上游 502 系列（换 token / 拉 userinfo 的传输层与协议层失败）。
fn google_upstream_error(message: &str) -> Response {
    google_error(StatusCode::BAD_GATEWAY, "upstream_error", message)
}

/// 上游 500 系列（userinfo 请求构造失败、user 落库失败、签发 session 失败）。
fn google_internal_error(code: &str, message: &str) -> Response {
    google_error(StatusCode::INTERNAL_SERVER_ERROR, code, message)
}

// 上游 `auth.go:43-47` 的 code 常量，字符串逐字保留（`pub` 供 M9 接上配置面后复用）。
/// 上游 `googleLoginCodeAccountDisabled`：当前不可达（见上方偏离清单）。
pub const GOOGLE_CODE_ACCOUNT_DISABLED: &str = "account_disabled";
/// 上游 `googleLoginCodeSignupProhibited`：当前不可达（本仓无 `ALLOW_SIGNUP`）。
pub const GOOGLE_CODE_SIGNUP_PROHIBITED: &str = "signup_prohibited";
/// 上游 `googleLoginCodeEmailNotAllowed`：当前不可达（本仓无邮箱白名单）。
pub const GOOGLE_CODE_EMAIL_NOT_ALLOWED: &str = "email_not_allowed";
/// 上游 `googleLoginCodeAccountWithoutEmail`。
pub const GOOGLE_CODE_ACCOUNT_WITHOUT_EMAIL: &str = "google_account_no_email";
/// 上游 `googleLoginCodeInvalidOAuthCode`。
pub const GOOGLE_CODE_INVALID_OAUTH_CODE: &str = "oauth_code_invalid";
/// `writeFeatureDisabled` 的 code。
pub const GOOGLE_CODE_NOT_CONFIGURED: &str = "google_login_not_configured";

#[derive(Debug, Deserialize)]
pub struct GoogleLoginRequest {
    /// 上游是 `Code string` —— 字段缺失等同于空串（→ 400 `code is required`）。
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default, rename = "id_token")]
    #[allow(dead_code)]
    id_token: Option<String>,
    #[serde(default, rename = "token_type")]
    #[allow(dead_code)]
    token_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleTokenError {
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    picture: Option<String>,
}

/// `LoginResponse`（上游 `handler/auth.go:112`）。
#[derive(Debug, Serialize)]
pub struct GoogleLoginResponse {
    /// 上游是 HS256 JWT。本仓 M1 没有 JWT 层，这里是不透明 session id
    /// （同时写进 `multica_session` cookie）—— 见 `docs/29-W1-GOOGLE.md`。
    pub token: String,
    pub user: UserView,
}

/// 上游 `Handler.GoogleLogin`（`handler/auth.go:546-728`）的逐条移植。
///
/// 请求：`POST /auth/google`（**没有** `/api` 前缀），body
/// `{"code": "<google authorization code>", "redirect_uri": "<可选>"}`。
///
/// 11 步流程与状态码/错误码对齐上游（行号见 `docs/29-W1-GOOGLE.md` 的表）：
/// 1. body 解析失败 → 400 `invalid request body`
/// 2. `code` 为空 → 400 `code is required`
/// 3. 未配置 `GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_SECRET` → 403 `google_login_not_configured`
///    （上游 `writeFeatureDisabled` 故意用 403 而不是 503，避免重试与告警噪音）
/// 4. 换 token `POST {MC_GOOGLE_TOKEN_URL}`（表单）→ 传输失败 502
/// 5. 非 200：`400 + {"error":"invalid_grant"}` → 400 `oauth_code_invalid`；其余 → 502
/// 6. 响应解析失败 / 空 `access_token` → 502
/// 7. `GET {MC_GOOGLE_USERINFO_URL}`（Bearer）→ 请求构造失败 500；传输失败 / 非 200 → 502
/// 8. 空 email → 400 `google_account_no_email`
/// 9. `findOrCreateUser(email)`（按 email 查，缺失则用 `@` 前缀建号；
///    禁用/白名单三条 403 见下）→ 落库失败 500
/// 10. 回填 name（仅当 name == email 前缀）/ avatar（仅当为空）—— 失败只记日志
/// 11. 签发 session + `Set-Cookie` → `{"token", "user"}`；签发失败 500
///
/// 与上游的登记偏离（完整清单见 `docs/29-W1-GOOGLE.md`）：
/// - 错误体是嵌套 envelope（本仓 M1 约定），`code` 字符串一致；
/// - `token` 是 session id 而非 JWT；
/// - `user` 是本仓 `UserView`（字段少于上游 `UserResponse`）；
/// - `account_disabled` / `signup_prohibited` / `email_not_allowed` 三条 403 在本仓不可达
///   （M1 无 signup 白名单与禁用邮箱配置面）；
/// - 不签发上游 `SetAuthCookies` 的 CF region cookie，也无上游的 per-IP 限流
///   （`RATE_LIMIT_AUTH`，默认 5/min）。
// 逐条对齐上游 11 步流程与错误码；拆函数会把「步骤 ↔ 状态码」的对应关系割裂。
#[allow(clippy::too_many_lines)]
pub(super) async fn google_login(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    // ---- 1. 请求体 ----
    // 用 `Bytes` 而不是 `Json<T>`：axum 的 `Json` rejection 对「字段类型错误」是
    // 422，而上游 `json.Decode` 失败一律 400 "invalid request body"。
    let req: GoogleLoginRequest = match serde_json::from_slice(&body) {
        Ok(req) => req,
        Err(err) => {
            tracing::debug!(error = %err, "google login: invalid request body");
            return google_error(
                StatusCode::BAD_REQUEST,
                "validation_error",
                "invalid request body",
            );
        }
    };

    // ---- 2. code 必填（上游不 trim，此处保持一致）----
    if req.code.is_empty() {
        return google_error(
            StatusCode::BAD_REQUEST,
            "validation_error",
            "code is required",
        );
    }

    // ---- 3. 功能开关 ----
    let cfg: GoogleOAuthConfig = state.google_oauth.clone();
    let Some((client_id, client_secret)) = cfg.client_id.clone().zip(cfg.client_secret.clone())
    else {
        return google_error(
            StatusCode::FORBIDDEN,
            GOOGLE_CODE_NOT_CONFIGURED,
            "Google login is not configured",
        );
    };
    let Some(http) = cfg.http.clone() else {
        tracing::error!("google oauth http client unavailable");
        return google_upstream_error("failed to exchange code with Google");
    };

    // ---- 4. 用 authorization code 换 token ----
    let redirect_uri = cfg.redirect_uri_for(req.redirect_uri.as_deref());
    let form = [
        ("code", req.code.as_str()),
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
    ];
    let token_resp = match http.post(&cfg.token_url).form(&form).send().await {
        Ok(resp) => resp,
        Err(err) => {
            tracing::error!(error = %err, url = %cfg.token_url, "google oauth token exchange failed");
            return google_upstream_error("failed to exchange code with Google");
        }
    };
    let token_status = token_resp.status();
    let token_body = match token_resp.text().await {
        Ok(body) => body,
        Err(err) => {
            tracing::error!(error = %err, "google oauth token response could not be read");
            return google_upstream_error("failed to read Google token response");
        }
    };

    // ---- 5. 非 200 ----
    // 只有「400 + error == invalid_grant」说明授权码被拒；配置/上游/畸形响应都是
    // 服务端失败（上游注释原话：Only a valid invalid_grant response identifies a
    // rejected authorization code）。
    if token_status != StatusCode::OK {
        let provider_error = serde_json::from_str::<GoogleTokenError>(&token_body)
            .ok()
            .and_then(|err| err.error)
            .unwrap_or_default();
        tracing::error!(
            status = token_status.as_u16(),
            body = %token_body,
            "google oauth token exchange returned error"
        );
        if token_status == StatusCode::BAD_REQUEST && provider_error == "invalid_grant" {
            return google_error(
                StatusCode::BAD_REQUEST,
                GOOGLE_CODE_INVALID_OAUTH_CODE,
                "failed to exchange code with Google",
            );
        }
        return google_upstream_error("failed to exchange code with Google");
    }

    // ---- 6. token 响应体 ----
    let token: GoogleTokenResponse = match serde_json::from_str(&token_body) {
        Ok(token) => token,
        Err(err) => {
            tracing::error!(error = %err, body = %token_body, "google oauth token response could not be parsed");
            return google_upstream_error("failed to parse Google token response");
        }
    };
    let access_token = token.access_token.unwrap_or_default();
    let access_token = access_token.trim();
    if access_token.is_empty() {
        tracing::error!("google oauth token response has no access token");
        return google_upstream_error("invalid Google token response");
    }

    // ---- 7. 拉 userinfo ----
    // 请求构造失败（URL/header 非法）→ 500，与上游 `http.NewRequestWithContext`
    // 失败一致；`build()` 是唯一能在 `send()` 之前区分它的点。
    let userinfo_request = match http
        .get(&cfg.userinfo_url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .build()
    {
        Ok(request) => request,
        Err(err) => {
            tracing::error!(error = %err, url = %cfg.userinfo_url, "failed to create userinfo request");
            return google_internal_error("internal_error", "internal error");
        }
    };
    let userinfo_resp = match http.execute(userinfo_request).await {
        Ok(resp) => resp,
        Err(err) => {
            tracing::error!(error = %err, "google userinfo fetch failed");
            return google_upstream_error("failed to fetch user info from Google");
        }
    };
    if userinfo_resp.status() != StatusCode::OK {
        let status = userinfo_resp.status();
        let body = userinfo_resp.text().await.unwrap_or_default();
        tracing::error!(status = status.as_u16(), body = %body, "google userinfo returned error");
        return google_upstream_error("failed to fetch user info from Google");
    }
    let userinfo_body = match userinfo_resp.text().await {
        Ok(body) => body,
        Err(err) => {
            tracing::error!(error = %err, "google userinfo body could not be read");
            return google_upstream_error("failed to parse Google user info");
        }
    };
    // 上游把 body 解到 `*googleUserInfo`：字面量 `null` 解出 nil 指针 → 502
    // "invalid Google user info"。`Option<GoogleUserInfo>` 保留了这条区分。
    let userinfo: Option<GoogleUserInfo> = match serde_json::from_str(&userinfo_body) {
        Ok(userinfo) => userinfo,
        Err(err) => {
            tracing::error!(error = %err, body = %userinfo_body, "google userinfo could not be parsed");
            return google_upstream_error("failed to parse Google user info");
        }
    };
    let Some(userinfo) = userinfo else {
        return google_upstream_error("invalid Google user info");
    };

    // ---- 8. email 必填（小写 + trim）----
    let email = userinfo
        .email
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    if email.is_empty() {
        return google_error(
            StatusCode::BAD_REQUEST,
            GOOGLE_CODE_ACCOUNT_WITHOUT_EMAIL,
            "Google did not provide an email address for this sign-in",
        );
    }

    // ---- 9. findOrCreateUser ----
    // 上游在 findOrCreateUser 之前还有一道 `IsTemporarilyDisabledUserEmail`（403
    // `account_disabled`）；本仓 M1 没有这个硬编码列表，故该分支不可达。
    // `account_disabled` / `signup_prohibited` / `email_not_allowed` 三个 code
    // 保留为 `pub const`，M9 接上配置面（`ALLOW_SIGNUP` / `ALLOWED_EMAILS`）后再启用。
    let users = UserRepo::new(state.db.clone());
    let existing = match users.get_by_email(&email).await {
        Ok(user) => user,
        Err(err) => {
            tracing::error!(error = %err, %email, "google login: user lookup failed");
            return google_internal_error("database_error", "failed to create user");
        }
    };
    let mut is_new = false;
    let mut user = if let Some(user) = existing {
        user
    } else {
        is_new = true;
        match users
            .create(NewUser {
                name: email_local_part(&email),
                email: email.clone(),
                avatar_url: None,
            })
            .await
        {
            Ok(user) => user,
            Err(err) => {
                tracing::error!(error = %err, %email, "google login: user create failed");
                return google_internal_error("database_error", "failed to create user");
            }
        }
    };

    // ---- 10. 回填 Google profile ----
    // 上游只在「用户没改过」时回填：name 仍等于 email 本地部分、avatar 还是空。
    // 回填失败只记日志，继续用旧 user（上游 `if err == nil { user = updated }`）。
    let profile_name = userinfo.name.unwrap_or_default();
    let picture = userinfo.picture.unwrap_or_default();
    let needs_name = !profile_name.is_empty() && user.name == email_local_part(&email);
    let has_avatar = user
        .avatar_url
        .as_deref()
        .is_some_and(|url| !url.is_empty());
    let needs_avatar = !picture.is_empty() && !has_avatar;
    if needs_name || needs_avatar {
        // `UserRepo::update` 没有 avatar 字段（M1-A 的签名），改用按 email 的
        // 幂等 upsert：EXCLUDED 就是我们算好的最终值，语义等价且不动 mc-repos。
        let patch = NewUser {
            name: if needs_name {
                profile_name
            } else {
                user.name.clone()
            },
            email: email.clone(),
            avatar_url: if needs_avatar {
                Some(picture)
            } else {
                user.avatar_url.clone()
            },
        };
        match users.upsert_by_email(patch).await {
            Ok(updated) => user = updated,
            Err(err) => tracing::warn!(
                error = %err,
                %email,
                "google login: profile backfill failed, keeping stored user"
            ),
        }
    }

    // ---- 11. 签发 session + cookie ----
    let session = Session::new(user.id, state.config.session_ttl_secs);
    if let Err(err) = state.auth.store().put(session.clone()).await {
        tracing::error!(error = %err, "google login: session put failed");
        return google_internal_error("internal_error", "failed to generate token");
    }

    let body = GoogleLoginResponse {
        token: session.id.clone(),
        user: UserView {
            id: user.id.as_string(),
            name: user.name.clone(),
            email: user.email.clone(),
            created_at: user.created_at.as_iso(),
        },
    };

    tracing::info!(
        user_id = %user.id.as_string(),
        email = %email,
        is_new,
        "user logged in via google"
    );
    session_response(
        &state,
        &session,
        StatusCode::OK,
        serde_json::to_value(body).unwrap(),
    )
}
