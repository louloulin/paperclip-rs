//! Auth 服务层：把 R565 / R568 的 typed 抽象编排成 sign-up / sign-in
//! / verify-email / refresh / sign-out 等高阶 API。
//!
//! 与 Node `auth/better-auth.ts` 中 `auth.api` 等价：上层路由只需
//! 调 `AuthService::sign_up_email`，不需要写流程胶水。
//!
//! 设计原则：
//! - Store 抽象（trait）：生产用 DB-backed；测试用 in-memory。
//! - 输入校验集中在 [`validate_sign_up_input`] / [`validate_sign_in_input`]。
//! - 错误统一为 [`AuthServiceError`]，分类清晰、可重试易判。
//! - email 发送走 [`EmailSender`] trait（R568）；service 不耦合具体 provider。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::email_sender::{EmailAddress, EmailMessage, EmailSender, EmailSenderError};
use crate::email_verification::{
    consume_email_verification, issue_email_verification, verify_email_token,
    EmailVerificationOutcome, EmailVerificationRecord,
};
use crate::session_refresh::{
    check_session, detect_reuse, is_revoked, mark_revoked, new_session_record, rotate_session,
    ReuseOutcome, SessionCheckOutcome, SessionPolicy,
};

// ============================================================================
// 输入 / 输出
// ============================================================================

/// sign-up 输入（已 trim、已 lowercase email）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignUpInput {
    pub email: String,
    pub password: String,
    pub name: Option<String>,
}

/// sign-in 输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInInput {
    pub email: String,
    pub password: String,
}

/// sign-up 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignUpResult {
    pub user_id: String,
    pub session_token: String,
    pub session_expires_at: DateTime<Utc>,
    /// 仅在启用 email 验证时返回；调用方应通过邮件发给用户。
    pub verification_token: Option<String>,
}

/// sign-in 结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInResult {
    pub user_id: String,
    pub session_token: String,
    pub session_expires_at: DateTime<Utc>,
}

/// 用户记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRecord {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub password_hash: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// session 记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub token_hash: String,
    pub user_id: String,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
    pub last_rotated_at: DateTime<Utc>,
    /// R512: family 链标识；同一 sign-in 产生的所有轮换 token 共享一个 family。
    /// 若 family 内检测到 token 重用，整个 family 全部作废。
    #[serde(default = "Uuid::new_v4")]
    pub family_id: Uuid,
    /// R512: 作废时间；`Some` 表示该 token 已被轮换 / 显式登出 / 重用作废。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

// ============================================================================
// 错误
// ============================================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthServiceError {
    /// 邮件地址无效。
    InvalidEmail(String),
    /// 密码不符合强度要求。
    WeakPassword(String),
    /// 用户已存在。
    UserExists,
    /// 用户不存在。
    UserNotFound,
    /// 密码错误。
    InvalidCredentials,
    /// 邮箱尚未验证。
    EmailNotVerified,
    /// session 不存在 / 已过期。
    SessionNotFound,
    /// R512: 检测到 token 重用 —— 攻击信号，整个 family 已作废。
    SessionReuseDetected,
    /// 内部存储错误。
    Storage(String),
    /// 邮件发送失败。
    EmailSend(EmailSenderError),
    /// 其它。
    Other(String),
}

impl std::fmt::Display for AuthServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEmail(s) => write!(f, "invalid email: {s}"),
            Self::WeakPassword(s) => write!(f, "weak password: {s}"),
            Self::UserExists => write!(f, "user already exists"),
            Self::UserNotFound => write!(f, "user not found"),
            Self::InvalidCredentials => write!(f, "invalid credentials"),
            Self::EmailNotVerified => write!(f, "email not verified"),
            Self::SessionNotFound => write!(f, "session not found"),
            Self::SessionReuseDetected => write!(f, "session reuse detected"),
            Self::Storage(s) => write!(f, "storage: {s}"),
            Self::EmailSend(e) => write!(f, "email send: {e}"),
            Self::Other(s) => write!(f, "other: {s}"),
        }
    }
}

impl std::error::Error for AuthServiceError {}

impl AuthServiceError {
    /// 是否可短暂重试（仅 Storage 与 EmailSend::Upstream/RateLimited 视为可重试）。
    #[must_use]
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Storage(_) => true,
            Self::EmailSend(e) => e.is_transient(),
            _ => false,
        }
    }
}

impl From<EmailSenderError> for AuthServiceError {
    fn from(err: EmailSenderError) -> Self {
        Self::EmailSend(err)
    }
}

// ============================================================================
// Store traits
// ============================================================================

/// 用户存储抽象。
#[async_trait]
pub trait UserStore: Send + Sync {
    async fn create_user(&self, user: &UserRecord) -> Result<(), AuthServiceError>;
    async fn find_by_email(&self, email: &str) -> Result<Option<UserRecord>, AuthServiceError>;
    async fn find_by_id(&self, id: &str) -> Result<Option<UserRecord>, AuthServiceError>;
    async fn mark_email_verified(
        &self,
        user_id: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AuthServiceError>;
    async fn update_password(
        &self,
        user_id: &str,
        password_hash: &str,
    ) -> Result<(), AuthServiceError>;
}

/// session 存储抽象。
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(&self, session: &SessionRecord) -> Result<(), AuthServiceError>;
    async fn find_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<SessionRecord>, AuthServiceError>;
    async fn delete_by_token_hash(&self, token_hash: &str) -> Result<(), AuthServiceError>;
    async fn rotate(
        &self,
        old_token_hash: &str,
        new_session: &SessionRecord,
    ) -> Result<(), AuthServiceError>;
    /// R512: 列出 family 内所有 session（按 token_hash 索引扫描）。
    async fn find_family(&self, family_id: Uuid) -> Result<Vec<SessionRecord>, AuthServiceError>;
    /// R512: 标记某个 token 已作废（设置 `revoked_at`）。不动其他字段。
    async fn mark_revoked(
        &self,
        token_hash: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AuthServiceError>;
    /// R512: 作废整个 family（重用检测触发）。返回受影响 session 数。
    async fn invalidate_family(
        &self,
        family_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<usize, AuthServiceError>;
}

/// email 验证 token 存储抽象。
#[async_trait]
pub trait VerificationStore: Send + Sync {
    async fn put(&self, record: &EmailVerificationRecord) -> Result<(), AuthServiceError>;
    async fn get_by_user(
        &self,
        user_id: &str,
    ) -> Result<Option<EmailVerificationRecord>, AuthServiceError>;
    async fn delete(&self, user_id: &str) -> Result<(), AuthServiceError>;
    async fn find_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<EmailVerificationRecord>, AuthServiceError>;
}

// ============================================================================
// In-memory 测试实现
// ============================================================================

/// in-memory UserStore（用于单元测试）。
#[derive(Default)]
pub struct InMemoryUserStore {
    inner: Mutex<HashMap<String, UserRecord>>, // id -> user
}

impl InMemoryUserStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl UserStore for InMemoryUserStore {
    async fn create_user(&self, user: &UserRecord) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("user store mutex");
        if g.values().any(|u| u.email == user.email) {
            return Err(AuthServiceError::UserExists);
        }
        g.insert(user.id.clone(), user.clone());
        Ok(())
    }
    async fn find_by_email(&self, email: &str) -> Result<Option<UserRecord>, AuthServiceError> {
        let g = self.inner.lock().expect("user store mutex");
        Ok(g.values().find(|u| u.email == email).cloned())
    }
    async fn find_by_id(&self, id: &str) -> Result<Option<UserRecord>, AuthServiceError> {
        let g = self.inner.lock().expect("user store mutex");
        Ok(g.get(id).cloned())
    }
    async fn mark_email_verified(
        &self,
        user_id: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("user store mutex");
        let user = g.get_mut(user_id).ok_or(AuthServiceError::UserNotFound)?;
        user.email_verified_at = Some(at);
        Ok(())
    }
    async fn update_password(
        &self,
        user_id: &str,
        password_hash: &str,
    ) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("user store mutex");
        let user = g.get_mut(user_id).ok_or(AuthServiceError::UserNotFound)?;
        user.password_hash = password_hash.to_string();
        Ok(())
    }
}

/// in-memory SessionStore。
#[derive(Default)]
pub struct InMemorySessionStore {
    inner: Mutex<HashMap<String, SessionRecord>>, // token_hash -> session
}

impl InMemorySessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 测试用：通过 token 找到对应 session 的 family_id，再查该 family 全部成员。
    /// 返回 `None` 表示 token 不在 store 中。
    pub async fn find_family_for_token(&self, token: &str) -> Option<Vec<SessionRecord>> {
        let hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(token.as_bytes());
            format!("{:x}", h.finalize())
        };
        let g = self.inner.lock().expect("session store mutex");
        let family_id = g.get(&hash).map(|r| r.family_id)?;
        let v: Vec<SessionRecord> = g
            .values()
            .filter(|r| r.family_id == family_id)
            .cloned()
            .collect();
        Some(v)
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create_session(&self, session: &SessionRecord) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("session store mutex");
        g.insert(session.token_hash.clone(), session.clone());
        Ok(())
    }
    async fn find_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<SessionRecord>, AuthServiceError> {
        let g = self.inner.lock().expect("session store mutex");
        Ok(g.get(token_hash).cloned())
    }
    async fn delete_by_token_hash(&self, token_hash: &str) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("session store mutex");
        g.remove(token_hash);
        Ok(())
    }
    async fn rotate(
        &self,
        old_token_hash: &str,
        new_session: &SessionRecord,
    ) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("session store mutex");
        // R512: 保留旧 token 记录（标记 revoked），用于 reuse detection。
        // 这里不再 delete；reuse detection 在 refresh_session 入口处先做。
        g.insert(new_session.token_hash.clone(), new_session.clone());
        if let Some(r) = g.get_mut(old_token_hash) {
            r.revoked_at.get_or_insert_with(Utc::now);
        }
        Ok(())
    }

    async fn find_family(&self, family_id: Uuid) -> Result<Vec<SessionRecord>, AuthServiceError> {
        let g = self.inner.lock().expect("session store mutex");
        Ok(g.values()
            .filter(|r| r.family_id == family_id)
            .cloned()
            .collect())
    }

    async fn mark_revoked(
        &self,
        token_hash: &str,
        at: DateTime<Utc>,
    ) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("session store mutex");
        if let Some(r) = g.get_mut(token_hash) {
            r.revoked_at = Some(at);
        }
        Ok(())
    }

    async fn invalidate_family(
        &self,
        family_id: Uuid,
        at: DateTime<Utc>,
    ) -> Result<usize, AuthServiceError> {
        let mut g = self.inner.lock().expect("session store mutex");
        let mut count = 0usize;
        for r in g.values_mut() {
            if r.family_id == family_id && r.revoked_at.is_none() {
                r.revoked_at = Some(at);
                count += 1;
            }
        }
        Ok(count)
    }
}

/// in-memory VerificationStore。
#[derive(Default)]
pub struct InMemoryVerificationStore {
    inner: Mutex<HashMap<String, EmailVerificationRecord>>, // user_id -> record
}

impl InMemoryVerificationStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl VerificationStore for InMemoryVerificationStore {
    async fn put(&self, record: &EmailVerificationRecord) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("verification store mutex");
        g.insert(record.user_id.clone(), record.clone());
        Ok(())
    }
    async fn get_by_user(
        &self,
        user_id: &str,
    ) -> Result<Option<EmailVerificationRecord>, AuthServiceError> {
        let g = self.inner.lock().expect("verification store mutex");
        Ok(g.get(user_id).cloned())
    }
    async fn delete(&self, user_id: &str) -> Result<(), AuthServiceError> {
        let mut g = self.inner.lock().expect("verification store mutex");
        g.remove(user_id);
        Ok(())
    }
    async fn find_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<EmailVerificationRecord>, AuthServiceError> {
        let g = self.inner.lock().expect("verification store mutex");
        Ok(g.values().find(|r| r.token_hash == token_hash).cloned())
    }
}

// ============================================================================
// 输入校验
// ============================================================================

/// 校验 sign-up 输入：返回 `LowercasedInput`（email 已 lowercase、name 已 trim）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedSignUpInput {
    pub email: String,
    pub password: String,
    pub name: Option<String>,
}

pub fn validate_sign_up_input(
    input: &SignUpInput,
) -> Result<NormalizedSignUpInput, AuthServiceError> {
    let email = input.email.trim().to_lowercase();
    if email.is_empty() {
        return Err(AuthServiceError::InvalidEmail("empty".into()));
    }
    // 复用 R568 的 EmailAddress 校验
    let _ = EmailAddress::new(&email).map_err(|e| AuthServiceError::InvalidEmail(e.to_string()))?;
    validate_password_strength(&input.password)?;
    let name = input
        .name
        .as_ref()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    Ok(NormalizedSignUpInput {
        email,
        password: input.password.clone(),
        name,
    })
}

pub fn validate_sign_in_input(input: &SignInInput) -> Result<(), AuthServiceError> {
    let email = input.email.trim().to_lowercase();
    if email.is_empty() {
        return Err(AuthServiceError::InvalidEmail("empty".into()));
    }
    let _ = EmailAddress::new(&email).map_err(|e| AuthServiceError::InvalidEmail(e.to_string()))?;
    if input.password.is_empty() {
        return Err(AuthServiceError::InvalidCredentials);
    }
    Ok(())
}

/// 校验密码强度。要求：长度 >= 8，包含字母 + 数字。
pub fn validate_password_strength(password: &str) -> Result<(), AuthServiceError> {
    if password.len() < 8 {
        return Err(AuthServiceError::WeakPassword(
            "must be at least 8 characters".into(),
        ));
    }
    if password.len() > 256 {
        return Err(AuthServiceError::WeakPassword(
            "must be at most 256 characters".into(),
        ));
    }
    let has_letter = password.chars().any(|c| c.is_ascii_alphabetic());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    if !has_letter || !has_digit {
        return Err(AuthServiceError::WeakPassword(
            "must contain both letters and digits".into(),
        ));
    }
    Ok(())
}

// ============================================================================
// AuthService
// ============================================================================

/// Auth 服务配置。
#[derive(Debug, Clone)]
pub struct AuthServiceConfig {
    pub from_email: String,
    pub from_name: Option<String>,
    pub session_policy: SessionPolicy,
    pub require_email_verification: bool,
    pub verification_ttl_hours: i64,
}

impl Default for AuthServiceConfig {
    fn default() -> Self {
        Self {
            from_email: "noreply@paperclip.local".into(),
            from_name: Some("Paperclip".into()),
            session_policy: SessionPolicy::default(),
            require_email_verification: true,
            verification_ttl_hours: 24,
        }
    }
}

pub struct AuthService {
    config: AuthServiceConfig,
    users: Arc<dyn UserStore>,
    sessions: Arc<dyn SessionStore>,
    verifications: Arc<dyn VerificationStore>,
    email: Arc<dyn EmailSender>,
}

impl AuthService {
    pub fn new(
        config: AuthServiceConfig,
        users: Arc<dyn UserStore>,
        sessions: Arc<dyn SessionStore>,
        verifications: Arc<dyn VerificationStore>,
        email: Arc<dyn EmailSender>,
    ) -> Self {
        Self {
            config,
            users,
            sessions,
            verifications,
            email,
        }
    }

    /// 便捷构造：全部用 in-memory store（仅用于测试）。
    pub fn in_memory(config: AuthServiceConfig, email: Arc<dyn EmailSender>) -> Self {
        Self::new(
            config,
            Arc::new(InMemoryUserStore::new()),
            Arc::new(InMemorySessionStore::new()),
            Arc::new(InMemoryVerificationStore::new()),
            email,
        )
    }

    fn from_address(&self) -> Result<EmailAddress, AuthServiceError> {
        match &self.config.from_name {
            Some(n) => EmailAddress::with_name(&self.config.from_email, n)
                .map_err(|e| AuthServiceError::Other(e.to_string())),
            None => EmailAddress::new(&self.config.from_email)
                .map_err(|e| AuthServiceError::Other(e.to_string())),
        }
    }

    fn hash_token(token: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(token.as_bytes());
        hex::encode(digest)
    }

    /// 注册新用户。如果 require_email_verification，会发验证邮件并返回
    /// `verification_token`（仅用于测试 / 调试；生产不应在响应里返回）。
    pub async fn sign_up_email(
        &self,
        input: &SignUpInput,
    ) -> Result<SignUpResult, AuthServiceError> {
        let normalized = validate_sign_up_input(input)?;
        // 1. 查重
        if self.users.find_by_email(&normalized.email).await?.is_some() {
            return Err(AuthServiceError::UserExists);
        }
        // 2. hash 密码
        let password_hash = crate::hash_password(&normalized.password)
            .map_err(|e| AuthServiceError::Other(format!("hash: {e}")))?;
        // 3. 创建用户
        let now = Utc::now();
        let user = UserRecord {
            id: Uuid::new_v4().to_string(),
            email: normalized.email.clone(),
            name: normalized.name.clone(),
            password_hash,
            email_verified_at: if self.config.require_email_verification {
                None
            } else {
                Some(now)
            },
            created_at: now,
        };
        self.users.create_user(&user).await?;
        // 4. 发验证邮件（如启用）
        let mut verification_token = None;
        if self.config.require_email_verification {
            let (raw, record) = issue_email_verification(
                &user.id,
                &user.email,
                Duration::hours(self.config.verification_ttl_hours),
            );
            self.verifications.put(&record).await?;
            self.send_verification_email(&user, &raw).await?;
            verification_token = Some(raw);
        }
        // 5. 创建 session
        let session_token = crate::generate_session_token();
        let session = self.new_session_record(&user.id, &session_token);
        self.sessions.create_session(&session).await?;
        Ok(SignUpResult {
            user_id: user.id,
            session_token,
            session_expires_at: session.expires_at,
            verification_token,
        })
    }

    /// 登录。如果 require_email_verification，邮箱未验证时返回 EmailNotVerified。
    pub async fn sign_in_email(
        &self,
        input: &SignInInput,
    ) -> Result<SignInResult, AuthServiceError> {
        validate_sign_in_input(input)?;
        let email = input.email.trim().to_lowercase();
        let user = self
            .users
            .find_by_email(&email)
            .await?
            .ok_or(AuthServiceError::InvalidCredentials)?;
        if !crate::verify_password(&input.password, &user.password_hash) {
            return Err(AuthServiceError::InvalidCredentials);
        }
        if self.config.require_email_verification && user.email_verified_at.is_none() {
            return Err(AuthServiceError::EmailNotVerified);
        }
        let session_token = crate::generate_session_token();
        let session = self.new_session_record(&user.id, &session_token);
        self.sessions.create_session(&session).await?;
        Ok(SignInResult {
            user_id: user.id,
            session_token,
            session_expires_at: session.expires_at,
        })
    }

    /// 验证邮箱 token。
    pub async fn verify_email(&self, raw_token: &str) -> Result<String, AuthServiceError> {
        // 查找匹配 hash 的 record（线性扫描 in-memory；DB 实现可建索引）
        // 这里简化：调用方传入 user_id 已知，但本接口仅按 token 验证。
        // 通过遍历 in-memory store 找到 hash 匹配的 record。
        // 真实 DB 实现：可建 email_verifications_token_hash_idx。
        let record = self.find_verification_by_token(raw_token).await?;
        match verify_email_token(raw_token, &record) {
            EmailVerificationOutcome::Ok => {
                let now = Utc::now();
                let user_id = record.user_id.clone();
                self.users.mark_email_verified(&record.user_id, now).await?;
                self.verifications.delete(&record.user_id).await?;
                // 标记 record 为已消费（持久化层无影响，但语义清晰）
                let _ = consume_email_verification(record);
                Ok(user_id)
            }
            EmailVerificationOutcome::Expired => {
                Err(AuthServiceError::Other("verification token expired".into()))
            }
            EmailVerificationOutcome::AlreadyConsumed => Err(AuthServiceError::Other(
                "verification token already consumed".into(),
            )),
            EmailVerificationOutcome::NotFound => Err(AuthServiceError::Other(
                "verification token not found".into(),
            )),
        }
    }

    /// 登出：删除当前 session。
    pub async fn sign_out(&self, session_token: &str) -> Result<(), AuthServiceError> {
        let hash = Self::hash_token(session_token);
        self.sessions.delete_by_token_hash(&hash).await?;
        Ok(())
    }

    /// 刷新 session：旋转 token。
    pub async fn refresh_session(&self, old_token: &str) -> Result<SignInResult, AuthServiceError> {
        let old_hash = Self::hash_token(old_token);
        let old = self
            .sessions
            .find_by_token_hash(&old_hash)
            .await?
            .ok_or(AuthServiceError::SessionNotFound)?;
        // 检查 idle / absolute
        let record = crate::session_refresh::SessionRecord {
            issued_at: old.issued_at,
            expires_at: old.expires_at,
            last_used_at: old.last_used_at,
            last_rotated_at: old.last_rotated_at,
            revoked_at: old.revoked_at,
        };
        match check_session(&self.config.session_policy, &record, Utc::now()) {
            SessionCheckOutcome::Revoked => {
                // 旧 token 已作废 —— 当作重用信号，作废整个 family。
                self.sessions
                    .invalidate_family(old.family_id, Utc::now())
                    .await?;
                return Err(AuthServiceError::SessionReuseDetected);
            }
            SessionCheckOutcome::ExpiredAbsolute | SessionCheckOutcome::ExpiredIdle => {
                self.sessions.delete_by_token_hash(&old_hash).await?;
                return Err(AuthServiceError::SessionNotFound);
            }
            SessionCheckOutcome::Ok { .. } => {}
        }
        // R512: 重用检测 —— 拉取 family 内所有 session，扫描是否被轮换后又被使用。
        let family = self.sessions.find_family(old.family_id).await?;
        // 把 storage SessionRecord 投影到纯 session_refresh::SessionRecord 再判断。
        let pure_family: Vec<crate::session_refresh::SessionRecord> = family
            .iter()
            .map(|s| crate::session_refresh::SessionRecord {
                issued_at: s.issued_at,
                expires_at: s.expires_at,
                last_used_at: s.last_used_at,
                last_rotated_at: s.last_rotated_at,
                revoked_at: s.revoked_at,
            })
            .collect();
        if detect_reuse(&record, &pure_family).is_reuse() {
            self.sessions
                .invalidate_family(old.family_id, Utc::now())
                .await?;
            return Err(AuthServiceError::SessionReuseDetected);
        }
        let now = Utc::now();
        let new_record = rotate_session(&self.config.session_policy, &record, now);
        let new_token = crate::generate_session_token();
        let new_hash = Self::hash_token(&new_token);
        let new_session = SessionRecord {
            token_hash: new_hash,
            user_id: old.user_id.clone(),
            issued_at: new_record.issued_at,
            expires_at: new_record.expires_at,
            last_used_at: new_record.last_used_at,
            last_rotated_at: new_record.last_rotated_at,
            family_id: old.family_id,
            revoked_at: None,
        };
        // R512: 轮换时旧 token 自动作废（由 \ 内部设置 revoked_at）。
        self.sessions.rotate(&old_hash, &new_session).await?;
        Ok(SignInResult {
            user_id: old.user_id,
            session_token: new_token,
            session_expires_at: new_session.expires_at,
        })
    }

    fn new_session_record(&self, user_id: &str, token: &str) -> SessionRecord {
        let now = Utc::now();
        let r = new_session_record(now);
        let hash = Self::hash_token(token);
        SessionRecord {
            token_hash: hash,
            user_id: user_id.to_string(),
            issued_at: r.issued_at,
            expires_at: r.expires_at,
            last_used_at: r.last_used_at,
            last_rotated_at: r.last_rotated_at,
            family_id: Uuid::new_v4(),
            revoked_at: None,
        }
    }

    async fn send_verification_email(
        &self,
        user: &UserRecord,
        raw_token: &str,
    ) -> Result<(), AuthServiceError> {
        let from = self.from_address()?;
        let to =
            EmailAddress::new(&user.email).map_err(|e| AuthServiceError::Other(e.to_string()))?;
        let mut vars = std::collections::HashMap::new();
        vars.insert(
            "name".into(),
            user.name.clone().unwrap_or_else(|| user.email.clone()),
        );
        vars.insert("email".into(), user.email.clone());
        vars.insert("token".into(), raw_token.to_string());
        vars.insert(
            "ttl_hours".into(),
            self.config.verification_ttl_hours.to_string(),
        );
        let subject = crate::email_sender::render_template("Verify your Paperclip email", &vars);
        let body = crate::email_sender::render_template(
            "Hi {name},\n\nPlease verify your email ({email}) by using this token:\n\n  {token}\n\nThis token expires in {ttl_hours} hours.\n",
            &vars,
        );
        let msg = EmailMessage::new(from, vec![to], subject, body);
        self.email.send(&msg).await?;
        Ok(())
    }

    /// 通过 hash 查找 verification record。
    async fn find_verification_by_token(
        &self,
        raw_token: &str,
    ) -> Result<EmailVerificationRecord, AuthServiceError> {
        let hash = Self::hash_token(raw_token);
        self.verifications
            .find_by_token_hash(&hash)
            .await?
            .ok_or_else(|| AuthServiceError::Other("verification token not found".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::email_sender::LogEmailSender;

    fn test_service(require_email_verification: bool) -> (AuthService, Arc<LogEmailSender>) {
        let email = Arc::new(LogEmailSender::new(
            EmailAddress::new("noreply@paperclip.local").unwrap(),
        ));
        let log_email = email.clone();
        let mut config = AuthServiceConfig::default();
        config.require_email_verification = require_email_verification;
        (AuthService::in_memory(config, email), log_email)
    }

    /// 测试辅助：同时返回 InMemorySessionStore 引用，供 family/revoke 检查。
    fn test_service_with_session_store(
        require_email_verification: bool,
    ) -> (AuthService, Arc<InMemorySessionStore>, Arc<LogEmailSender>) {
        let email = Arc::new(LogEmailSender::new(
            EmailAddress::new("noreply@paperclip.local").unwrap(),
        ));
        let log_email = email.clone();
        let mut config = AuthServiceConfig::default();
        config.require_email_verification = require_email_verification;
        let sessions = Arc::new(InMemorySessionStore::new());
        let users: Arc<dyn UserStore> = Arc::new(InMemoryUserStore::new());
        let verifications: Arc<dyn VerificationStore> = Arc::new(InMemoryVerificationStore::new());
        let svc = AuthService::new(config, users, sessions.clone(), verifications, email);
        (svc, sessions, log_email)
    }

    #[test]
    fn r569_validate_sign_up_input_normalizes_email() {
        let input = SignUpInput {
            email: "  Alice@Example.COM ".into(),
            password: "hunter2pw".into(),
            name: Some("  Alice  ".into()),
        };
        let n = validate_sign_up_input(&input).unwrap();
        assert_eq!(n.email, "alice@example.com");
        assert_eq!(n.name.as_deref(), Some("Alice"));
    }

    #[test]
    fn r569_validate_sign_up_rejects_weak_password() {
        assert!(matches!(
            validate_password_strength("short"),
            Err(AuthServiceError::WeakPassword(_))
        ));
        assert!(matches!(
            validate_password_strength("nodigitshere"),
            Err(AuthServiceError::WeakPassword(_))
        ));
        assert!(matches!(
            validate_password_strength("12345678"),
            Err(AuthServiceError::WeakPassword(_))
        ));
        assert!(validate_password_strength("hunter2pw").is_ok());
    }

    #[test]
    fn r569_validate_sign_up_rejects_invalid_email() {
        let input = SignUpInput {
            email: "not-an-email".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        assert!(matches!(
            validate_sign_up_input(&input),
            Err(AuthServiceError::InvalidEmail(_))
        ));
    }

    #[tokio::test]
    async fn r569_sign_up_creates_user_and_sends_verification() {
        let (svc, log) = test_service(true);
        let input = SignUpInput {
            email: "alice@example.com".into(),
            password: "hunter2pw".into(),
            name: Some("Alice".into()),
        };
        let result = svc.sign_up_email(&input).await.unwrap();
        assert!(!result.user_id.is_empty());
        assert!(
            result.verification_token.is_some(),
            "should issue verification token"
        );
        // 邮件已发送
        let messages = log.messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].to[0].address == "alice@example.com");
        assert!(messages[0].body_text.contains("alice@example.com"));
        // session 已创建
        assert!(!result.session_token.is_empty());
    }

    #[tokio::test]
    async fn r569_sign_up_rejects_duplicate_email() {
        let (svc, _) = test_service(false);
        let input = SignUpInput {
            email: "bob@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        svc.sign_up_email(&input).await.unwrap();
        let err = svc.sign_up_email(&input).await.unwrap_err();
        assert_eq!(err, AuthServiceError::UserExists);
    }

    #[tokio::test]
    async fn r569_sign_in_with_correct_password_succeeds() {
        let (svc, _) = test_service(false);
        let input = SignUpInput {
            email: "bob@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        svc.sign_up_email(&input).await.unwrap();
        let r = svc
            .sign_in_email(&SignInInput {
                email: "BOB@example.com".into(),
                password: "hunter2pw".into(),
            })
            .await
            .unwrap();
        assert!(!r.session_token.is_empty());
    }

    #[tokio::test]
    async fn r569_sign_in_with_wrong_password_fails() {
        let (svc, _) = test_service(false);
        let input = SignUpInput {
            email: "bob@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        svc.sign_up_email(&input).await.unwrap();
        let err = svc
            .sign_in_email(&SignInInput {
                email: "bob@example.com".into(),
                password: "WRONG-PASSWORD".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err, AuthServiceError::InvalidCredentials);
    }

    #[tokio::test]
    async fn r569_sign_in_when_email_not_verified_fails() {
        let (svc, _) = test_service(true);
        let input = SignUpInput {
            email: "carol@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        svc.sign_up_email(&input).await.unwrap();
        // 未验证邮箱前不能 sign in
        let err = svc
            .sign_in_email(&SignInInput {
                email: "carol@example.com".into(),
                password: "hunter2pw".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err, AuthServiceError::EmailNotVerified);
    }

    #[tokio::test]
    async fn r569_verify_email_then_sign_in_succeeds() {
        let (svc, _) = test_service(true);
        let input = SignUpInput {
            email: "dan@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        let r = svc.sign_up_email(&input).await.unwrap();
        let token = r.verification_token.unwrap();
        // 验证邮箱
        let user_id = svc.verify_email(&token).await.unwrap();
        assert_eq!(user_id, r.user_id);
        // 现在可以 sign in
        let r2 = svc
            .sign_in_email(&SignInInput {
                email: "dan@example.com".into(),
                password: "hunter2pw".into(),
            })
            .await
            .unwrap();
        assert_eq!(r2.user_id, r.user_id);
    }

    #[tokio::test]
    async fn r569_verify_email_rejects_expired_token() {
        // 手工创建一个 1 小时前过期的 verification record
        let verifications = Arc::new(InMemoryVerificationStore::new());
        let user_id = "u-1";
        let past = Utc::now() - Duration::hours(2);
        let mut rec = EmailVerificationRecord {
            user_id: user_id.to_string(),
            email: "x@y.co".into(),
            token_hash: "h".into(),
            expires_at: past,
            consumed_at: None,
        };
        // 用一个稳定的 raw token 让 hash 匹配
        let raw = "expired-token-12345";
        rec.token_hash = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(raw.as_bytes()))
        };
        verifications.put(&rec).await.unwrap();
        // 通过自定义 service 调用
        let mut config = AuthServiceConfig::default();
        config.require_email_verification = true;
        let svc = AuthService::new(
            config,
            Arc::new(InMemoryUserStore::new()),
            Arc::new(InMemorySessionStore::new()),
            verifications,
            Arc::new(LogEmailSender::new(
                EmailAddress::new("noreply@paperclip.local").unwrap(),
            )),
        );
        let err = svc.verify_email(raw).await.unwrap_err();
        assert!(matches!(err, AuthServiceError::Other(_)));
    }

    #[tokio::test]
    async fn r569_sign_out_deletes_session() {
        let (svc, _) = test_service(false);
        let input = SignUpInput {
            email: "eve@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        let r = svc.sign_up_email(&input).await.unwrap();
        svc.sign_out(&r.session_token).await.unwrap();
        // refresh 应失败
        let err = svc.refresh_session(&r.session_token).await.unwrap_err();
        assert_eq!(err, AuthServiceError::SessionNotFound);
    }

    #[tokio::test]
    async fn r569_refresh_session_rotates_token() {
        let (svc, _) = test_service(false);
        let input = SignUpInput {
            email: "frank@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        let r = svc.sign_up_email(&input).await.unwrap();
        let refreshed = svc.refresh_session(&r.session_token).await.unwrap();
        assert_ne!(refreshed.session_token, r.session_token, "should rotate");
        assert_eq!(refreshed.user_id, r.user_id);
        // R512: 旧 token 已作废 —— 第二次使用触发 reuse detection。
        let err = svc.refresh_session(&r.session_token).await.unwrap_err();
        assert_eq!(err, AuthServiceError::SessionReuseDetected);
    }

    #[tokio::test]
    async fn r512_refresh_session_keeps_family_id_stable_across_rotations() {
        // 连续 N 次轮换后，family_id 不变（同一 sign-in 产生）。
        let (svc, store, _) = test_service_with_session_store(false);
        let input = SignUpInput {
            email: "fam@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        let r1 = svc.sign_up_email(&input).await.unwrap();
        let family1 = store
            .find_family_for_token(&r1.session_token)
            .await
            .unwrap();
        let family_id_1 = family1.first().map(|r| r.family_id);
        let r2 = svc.refresh_session(&r1.session_token).await.unwrap();
        let family2 = store
            .find_family_for_token(&r2.session_token)
            .await
            .unwrap();
        let family_id_2 = family2.first().map(|r| r.family_id);
        let r3 = svc.refresh_session(&r2.session_token).await.unwrap();
        let family3 = store
            .find_family_for_token(&r3.session_token)
            .await
            .unwrap();
        let family_id_3 = family3.first().map(|r| r.family_id);
        // family_id 保持稳定；成员数随轮换递增。
        assert_eq!(family_id_1, family_id_2);
        assert_eq!(family_id_2, family_id_3);
        // 每次轮换 family 都增长一条 (旧 token 被 revoked, 新 token 入 family)。
        assert_eq!(family1.len(), 1);
        assert_eq!(family2.len(), 2);
        assert_eq!(family3.len(), 3);
    }

    #[tokio::test]
    async fn r512_refresh_session_reuse_triggers_family_invalidation() {
        // 攻击者拿到旧 token 来 refresh：触发 reuse detection，整个 family 作废。
        let (svc, store, _) = test_service_with_session_store(false);
        let input = SignUpInput {
            email: "victim@example.com".into(),
            password: "hunter2pw".into(),
            name: None,
        };
        let legit = svc.sign_up_email(&input).await.unwrap();
        // 合法轮换
        let rotated = svc.refresh_session(&legit.session_token).await.unwrap();
        assert_ne!(rotated.session_token, legit.session_token);
        // 攻击者尝试用旧 token 再次 refresh
        let err = svc.refresh_session(&legit.session_token).await.unwrap_err();
        assert_eq!(err, AuthServiceError::SessionReuseDetected);
        // 此时整个 family 已被作废 —— 即便合法的新 token 也无法再 refresh。
        let err2 = svc
            .refresh_session(&rotated.session_token)
            .await
            .unwrap_err();
        assert_eq!(err2, AuthServiceError::SessionReuseDetected);
        // 验证 store 中 family 全部 revoked
        let family = store
            .find_family_for_token(&rotated.session_token)
            .await
            .unwrap();
        for r in &family {
            assert!(r.revoked_at.is_some(), "all family members must be revoked");
        }
        // 至少包含旧 + 新 两条记录
        assert!(family.len() >= 2);
    }
}
