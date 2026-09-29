//! `POST /auth/send-code` + `POST /auth/verify-code` —— 邮箱验证码的签发与核销。
//!
//! 对应上游 `multica/server/internal/handler/auth.go` 的 `Handler.SendCode` 与
//! `Handler.VerifyCode`（`auth.go:388`）。邮件发送仍是 `tracing::info!` 占位 ——
//! M9（mailer）才接 Resend / SES。
//!
//! 拆分自拆分前的单文件 `routes/auth.rs`（LUM-2530）：item 逐字搬移。

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use mc_auth::session::Session;
use mc_auth::verification::VerificationCodePurpose;
use mc_core::Id;
use mc_errors::Error;
use mc_repos::verification_code::{NewVerificationCode, VerificationCodeRepo};

use super::common::{
    dev_mode, dev_verification_code, email_local_part, hash_code, is_six_digits, session_response,
};
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

// ============================================================
// POST /auth/send-code
// ============================================================

#[derive(Debug, Deserialize)]
pub struct SendCodeRequest {
    pub email: String,
    #[serde(default)]
    pub purpose: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendCodeResponse {
    pub message: &'static str,
    /// 仅 dev 模式返回 —— 便于本地 curl 测试；production 永远 None。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev_code: Option<String>,
}

pub(super) async fn send_code(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SendCodeRequest>,
) -> ApiResult<Response> {
    let email = req.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError(Error::Validation {
            message: "email is required".into(),
            details: vec![],
        }));
    }

    let purpose = match req.purpose.as_deref().unwrap_or("email_verification") {
        "password_reset" => VerificationCodePurpose::PasswordReset,
        "two_factor" => VerificationCodePurpose::TwoFactor,
        "workspace_invite" => VerificationCodePurpose::WorkspaceInvite,
        _ => VerificationCodePurpose::EmailVerification,
    };

    let repo = VerificationCodeRepo::new(state.db.clone());

    // 速率限制 —— 单邮箱每分钟上限（参考 upstream RATE_LIMIT_AUTH_VERIFY）。
    let per_min = i64::from(state.config.send_code_per_email_per_min.max(1));
    let recent = repo
        .recent_for(&email, 60)
        .await
        .map_err(|e| ApiError(Error::Database(e.to_string())))?;
    if recent >= per_min {
        return Err(ApiError(Error::RateLimited {
            retry_after_secs: 60,
        }));
    }

    // 生成 6 位数字 code
    let code = format!("{:06}", rand::random::<u32>() % 1_000_000);
    let ttl_secs = state.config.verification_code_ttl_secs;
    let expires_at: DateTime<Utc> =
        Utc::now() + Duration::seconds(i64::try_from(ttl_secs).unwrap_or(i64::MAX));

    let _ = repo
        .create(NewVerificationCode {
            email: Some(email.clone()),
            user_id: None,
            purpose,
            code_hash: hash_code(&code),
            expires_at,
        })
        .await
        .map_err(|e| ApiError(Error::Database(e.to_string())))?;

    // 占位邮件 —— production 不打印 code，仅 email；dev 模式下把 code 一并
    // 写到日志，便于 `curl /auth/send-code` 后跟 verify。
    if dev_mode(&state) {
        tracing::info!(email = %email, code = %code, purpose = ?purpose, "verification code issued (dev placeholder)");
    } else {
        tracing::info!(email = %email, purpose = ?purpose, "verification code issued (mailer placeholder, M9 will send SMTP)");
    }

    let body = SendCodeResponse {
        message: "Verification code sent",
        dev_code: dev_mode(&state).then_some(code),
    };
    Ok((StatusCode::OK, Json(body)).into_response())
}

// ============================================================
// POST /auth/verify-code
// ============================================================

#[derive(Debug, Deserialize)]
pub struct VerifyCodeRequest {
    pub email: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct UserView {
    pub id: String,
    pub name: String,
    pub email: String,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct VerifyCodeResponse {
    pub user: UserView,
    pub session_id: String,
    pub csrf_token: String,
}

pub(super) async fn verify_code(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VerifyCodeRequest>,
) -> ApiResult<Response> {
    let email = req.email.trim().to_lowercase();
    let code = req.code.trim();

    if email.is_empty() || !email.contains('@') {
        return Err(ApiError(Error::Validation {
            message: "email is required".into(),
            details: vec![],
        }));
    }
    if !is_six_digits(code) {
        return Err(ApiError(Error::Validation {
            message: "code must be 6 digits".into(),
            details: vec![],
        }));
    }

    // 校验代码 —— dev 模式下接受 `MULTICA_DEV_VERIFICATION_CODE` 万能码。
    let mut is_dev_pass = false;
    if dev_mode(&state) {
        if let Some(dev) = dev_verification_code() {
            if dev == code {
                is_dev_pass = true;
            }
        }
    }

    let repo = VerificationCodeRepo::new(state.db.clone());
    let purpose = VerificationCodePurpose::EmailVerification;

    let row = if is_dev_pass {
        // dev 路径：跳过 consume；直接复用 / 新建 user
        None
    } else {
        let hash = hash_code(code);
        repo.consume(&hash, purpose)
            .await
            .map_err(|e| ApiError(Error::Database(e.to_string())))?
    };

    if !is_dev_pass && row.is_none() {
        // 与上游 `Handler.VerifyCode`（auth.go:388，内部 L415 调
        // `IncrementVerificationCodeAttempts`）对齐：命中该邮箱待用验证码时累计
        // attempts（best-effort，不影响 401 响应）。`consume` 的 SQL 只匹配
        // `attempts < 5` 的行，累计到 5 后该行自然失效，起到暴力枚举防护作用。
        if let Ok(Some(latest)) = repo.latest_active_for(&email, purpose).await {
            if let Err(e) = repo.increment_attempts(latest.id).await {
                tracing::warn!(error = %e, "increment verification attempts failed");
            }
        }
        return Err(ApiError(Error::VerificationCodeInvalid(
            "code invalid, expired, or already used".into(),
        )));
    }

    // upsert user —— first 命中自动建 user，name 取 email 本地部分。
    // 这里直接走 SQL 是因为 UserRepo 由 sub-issue A 维护，本 sub-issue
    // 不能侵入其文件以免合并冲突。
    let user_row = sqlx::query_as::<_, (Uuid, String, String, DateTime<Utc>)>(
        r#"
        INSERT INTO "user" (name, email)
        VALUES ($1, $2)
        ON CONFLICT (email) DO UPDATE SET updated_at = now()
        RETURNING id, name, email, created_at
        "#,
    )
    .bind(email_local_part(&email))
    .bind(&email)
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError(Error::Database(e.to_string())))?;

    let user_id = Id::from(user_row.0);
    let user_view = UserView {
        id: user_id.as_string(),
        name: user_row.1,
        email: user_row.2,
        created_at: user_row.3.to_rfc3339(),
    };

    // 颁发 session
    let session = Session::new(user_id, state.config.session_ttl_secs);
    let session_store = state.auth.store();
    session_store
        .put(session.clone())
        .await
        .map_err(|e| ApiError(Error::Internal(format!("session put: {e}"))))?;

    let body = VerifyCodeResponse {
        user: user_view,
        session_id: session.id.clone(),
        csrf_token: session.csrf_token.clone(),
    };
    Ok(session_response(
        &state,
        &session,
        StatusCode::OK,
        serde_json::to_value(body).unwrap(),
    ))
}
