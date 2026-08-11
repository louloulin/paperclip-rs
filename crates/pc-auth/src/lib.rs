//! pc-auth：API key / session / actor 解析。
//!
//! 复用原 paperclip 的 `user` + `session` + `board_api_keys` 表。

use axum::http::header;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use pc_db::Db;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

pub mod auth_service;
pub mod email_sender;
pub mod email_verification;
pub mod error;
pub mod oauth_state;
pub mod session_refresh;

pub use email_sender::{
    build_email_sender, render_template, EmailAddress, EmailMessage, EmailSender, EmailSenderError,
    LogEmailSender, NoopEmailSender,
};
pub use email_verification::{
    consume_email_verification, issue_email_verification, verify_email_token, EmailVerificationOutcome,
    EmailVerificationRecord,
};
pub use error::{classify as classify_auth_error, classify_str as classify_auth_str, AuthErrorCategory};
pub use oauth_state::{
    code_challenge_s256, new_oauth_state, verify_oauth_state, OAuthStateOutcome, OAuthStateRecord,
};
pub use auth_service::{
    validate_sign_in_input, validate_sign_up_input, AuthService, AuthServiceConfig,
    AuthServiceError, InMemorySessionStore, InMemoryUserStore, InMemoryVerificationStore,
    NormalizedSignUpInput, SessionRecord, SessionStore, SignInInput, SignInResult, SignUpInput,
    SignUpResult, UserRecord, UserStore, VerificationStore,
};
pub use session_refresh::{
    check_session, new_session_record, rotate_session, should_rotate, touch_session,
    SessionCheckOutcome, SessionPolicy, SessionRecord as SessionRefreshRecord,
};

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("missing credentials")]
    MissingCredentials,
    #[error("invalid token")]
    InvalidToken,
    #[error("session expired")]
    Expired,
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("hash: {0}")]
    Hash(String),
}

/// Actor 来源（与原 `paperclip/server/src/middleware/auth.ts` 中 actor.source 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorSource {
    None,
    Anonymous,
    LocalImplicit,
    Session,
    ApiKey,
    SessionCookie,
    AgentKey,
    AgentHeader,
    AgentJwt,
    CloudTenant,
    System,
}

impl ActorSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ActorSource::None => "none",
            ActorSource::Anonymous => "anonymous",
            ActorSource::LocalImplicit => "local_implicit",
            ActorSource::Session => "session",
            ActorSource::ApiKey => "api_key",
            ActorSource::SessionCookie => "session_cookie",
            ActorSource::AgentKey => "agent_key",
            ActorSource::AgentHeader => "agent_header",
            ActorSource::AgentJwt => "agent_jwt",
            ActorSource::CloudTenant => "cloud_tenant",
            ActorSource::System => "system",
        }
    }
}

/// API key / agent key 作用域
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KeyScope {
    #[serde(default)]
    pub can_manage_company: bool,
    #[serde(default)]
    pub can_manage_policies: bool,
    #[serde(default)]
    pub can_run_agents: bool,
    #[serde(default)]
    pub can_create_issues: bool,
    #[serde(default)]
    pub can_test_skills: bool,
    #[serde(default)]
    pub can_edit_skills: bool,
    #[serde(default)]
    pub raw: serde_json::Value,
}

/// Actor 抽象（与原 paperclip `Actor` 对齐）。
///
/// - User         — 登录用户 / board actor（含 companyIds / isInstanceAdmin）
/// - Agent        — Agent 调用方（含 agentId / companyId / keyId / runId）
/// - System       — 系统内部调用（heartbeat / scheduler）
/// - Anonymous    — 未认证（fallback）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Actor {
    User {
        id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        email: Option<String>,
        #[serde(default)]
        is_instance_admin: bool,
        #[serde(default)]
        company_ids: Vec<Uuid>,
        #[serde(default)]
        memberships: Vec<CompanyMembership>,
        #[serde(default)]
        run_id: Option<Uuid>,
    },
    Agent {
        id: Uuid,
        company_id: Uuid,
        #[serde(default)]
        key_id: Option<Uuid>,
        #[serde(default)]
        key_scope: KeyScope,
        #[serde(default)]
        run_id: Option<Uuid>,
        #[serde(default)]
        on_behalf_of_user_id: Option<String>,
        #[serde(default)]
        on_behalf_of_memberships: Vec<CompanyMembership>,
    },
    System,
    Anonymous,
}

/// 公司成员资格（与 Node 版 `CompanyMembership` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanyMembership {
    pub company_id: Uuid,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

impl Actor {
    pub fn is_authenticated(&self) -> bool {
        !matches!(self, Actor::Anonymous)
    }
    pub fn system() -> Self {
        Actor::System
    }
    pub fn anonymous() -> Self {
        Actor::Anonymous
    }
    pub fn user_id(&self) -> Option<&str> {
        if let Actor::User { id, .. } = self {
            Some(id)
        } else {
            None
        }
    }
    pub fn agent_id(&self) -> Option<Uuid> {
        if let Actor::Agent { id, .. } = self {
            Some(*id)
        } else {
            None
        }
    }
    pub fn company_id(&self) -> Option<Uuid> {
        match self {
            Actor::Agent { company_id, .. } => Some(*company_id),
            Actor::User { company_ids, .. } => company_ids.first().copied(),
            _ => None,
        }
    }
    pub fn company_ids(&self) -> Vec<Uuid> {
        match self {
            Actor::User { company_ids, .. } => company_ids.clone(),
            Actor::Agent { company_id, .. } => vec![*company_id],
            _ => Vec::new(),
        }
    }
    pub fn has_company_access(&self, company_id: Uuid) -> bool {
        match self {
            Actor::User {
                company_ids,
                is_instance_admin,
                ..
            } => *is_instance_admin || company_ids.contains(&company_id),
            Actor::Agent {
                company_id: cid, ..
            } => *cid == company_id,
            Actor::System => true,
            Actor::Anonymous => false,
        }
    }
    pub fn run_id(&self) -> Option<Uuid> {
        match self {
            Actor::User { run_id, .. } | Actor::Agent { run_id, .. } => *run_id,
            _ => None,
        }
    }
    pub fn is_instance_admin(&self) -> bool {
        matches!(
            self,
            Actor::User {
                is_instance_admin: true,
                ..
            }
        )
    }
    pub fn key_id(&self) -> Option<Uuid> {
        if let Actor::Agent { key_id, .. } = self {
            *key_id
        } else {
            None
        }
    }
    pub fn key_scope(&self) -> Option<&KeyScope> {
        if let Actor::Agent { key_scope, .. } = self {
            Some(key_scope)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    pub actor: Actor,
    pub source: ActorSource,
    pub method: &'static str,
    pub api_key_id: Option<Uuid>,
}

impl AuthContext {
    pub fn anonymous() -> Self {
        Self {
            actor: Actor::Anonymous,
            source: ActorSource::Anonymous,
            method: "anonymous",
            api_key_id: None,
        }
    }
    pub fn system() -> Self {
        Self {
            actor: Actor::System,
            source: ActorSource::System,
            method: "system",
            api_key_id: None,
        }
    }
    pub fn for_actor(actor: Actor, source: ActorSource, method: &'static str) -> Self {
        let api_key_id = actor.key_id();
        Self {
            actor,
            source,
            method,
            api_key_id,
        }
    }
    pub fn require_user(&self) -> Result<&str, AuthError> {
        self.actor.user_id().ok_or(AuthError::InvalidToken)
    }
    pub fn require_authenticated(&self) -> Result<(), AuthError> {
        if self.actor.is_authenticated() {
            Ok(())
        } else {
            Err(AuthError::MissingCredentials)
        }
    }
    pub fn require_company_access(&self, company_id: Uuid) -> Result<(), AuthError> {
        if self.actor.has_company_access(company_id) {
            Ok(())
        } else {
            Err(AuthError::InvalidToken)
        }
    }
}

/// Hash a plaintext password using argon2id with a random salt. The
/// returned string is the standard PHC-formatted hash including parameters
/// and salt, suitable for storage in the `account.password` column.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    use argon2::password_hash::{rand_core::OsRng, PasswordHash, SaltString};
    use argon2::{Algorithm, Argon2, Params, PasswordHasher, Version};
    let salt = SaltString::generate(&mut OsRng);
    let params = Params::new(19_456, 2, 1, None)
        .map_err(|err| AuthError::Hash(format!("argon2 params invalid: {err}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let hash = argon
        .hash_password(password.as_bytes(), &salt)
        .map_err(|err| AuthError::Hash(format!("argon2 hash failed: {err}")))?;
    Ok(hash.to_string())
}

/// Verify a plaintext password against a stored argon2 PHC-formatted hash.
/// Returns `true` when the password matches, `false` otherwise.
pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    use argon2::password_hash::PasswordHash;
    use argon2::{Argon2, PasswordVerifier};
    let Ok(parsed) = PasswordHash::new(stored_hash) else {
        return false;
    };
    let argon = Argon2::default();
    argon.verify_password(password.as_bytes(), &parsed).is_ok()
}

/// Generate a new opaque session token suitable for storing in the
/// `session.token` column. Mirrors Node-side `generateSessionToken`.
pub fn generate_session_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// R514: API key token prefix kinds.
///
/// | Prefix | Purpose | Wire format |
/// |---|---|---|
/// | `pk_`  | machine-to-machine API key (Board user) | `pk_<32 url-safe chars>` |
/// | `sess_` | reserved for future session tokens (currently unused) | `sess_<...>` |
///
/// The prefix is part of the token itself, so even if the hash leaks, the
/// prefix reveals the token kind and lets middleware short-circuit
/// (e.g. a session token can't accidentally be accepted as an API key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPrefix {
    /// `pk_` — Board user API key, machine-to-machine.
    Pk,
    /// `sess_` — reserved (placeholder for future per-session tokens).
    Sess,
}

impl KeyPrefix {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pk => "pk_",
            Self::Sess => "sess_",
        }
    }

    /// Parse a token's prefix; returns `None` for unknown / missing prefixes.
    ///
    /// Recognized prefixes for [`Self::Pk`] (board API key):
    /// - `pk_` (R514 current convention)
    /// - `pcak_` (legacy ApiKeyIssuer)
    /// - `pcp_board_` (legacy access.rs bootstrap)
    ///
    /// Legacy aliases keep existing hashed rows resolvable while new keys are
    /// minted with `pk_` going forward.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        if let Some(rest) = token.strip_prefix("pk_") {
            if !rest.is_empty() {
                return Some(Self::Pk);
            }
        }
        if let Some(rest) = token.strip_prefix("pcak_") {
            if !rest.is_empty() {
                return Some(Self::Pk);
            }
        }
        if let Some(rest) = token.strip_prefix("pcp_board_") {
            if !rest.is_empty() {
                return Some(Self::Pk);
            }
        }
        if let Some(rest) = token.strip_prefix("sess_") {
            if !rest.is_empty() {
                return Some(Self::Sess);
            }
        }
        None
    }
}

/// R514: Generate a new API key token with the given prefix.
/// Returns `{prefix}{32 url-safe chars}` — total length 35 (3 prefix + 32 body).
#[must_use]
pub fn generate_api_key(prefix: KeyPrefix) -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 24]; // 24 bytes -> 32 url-safe base64 chars (no padding)
    rand::thread_rng().fill_bytes(&mut bytes);
    use base64::Engine;
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    format!("{}{}", prefix.as_str(), body)
}

/// R514: Validate that `token` has the expected prefix.
///
/// Returns `true` iff `KeyPrefix::parse(token) == Some(expected)`.
///
/// Used by [`resolve_api_key`] as an early reject: a session token (`sess_`)
/// must never be accepted as an API key, and vice-versa, even if both
/// happen to hash to the same value (collision).
#[must_use]
pub fn has_key_prefix(token: &str, expected: KeyPrefix) -> bool {
    KeyPrefix::parse(token) == Some(expected)
}

/// Hash a session token for storage. Mirrors the existing `hash_token`.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

pub async fn resolve_api_key(db: &Db, token: &str) -> Result<Option<(Uuid, String)>, AuthError> {
    // R514: 防御性 prefix 校验 —— 即使 hash 巧合碰撞，错误前缀也不会被当成 API key。
    if !has_key_prefix(token, KeyPrefix::Pk) {
        return Ok(None);
    }
    let h = hash_token(token);
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, user_id FROM board_api_keys \
         WHERE key_hash = $1 AND revoked_at IS NULL \
         AND (expires_at IS NULL OR expires_at > now())",
    )
    .bind(&h)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

pub async fn touch_api_key(db: &Db, key_id: Uuid) -> Result<(), AuthError> {
    sqlx::query("UPDATE board_api_keys SET last_used_at = now() WHERE id = $1")
        .bind(key_id)
        .execute(db.pool())
        .await?;
    Ok(())
}

pub async fn resolve_session(
    db: &Db,
    token: &str,
) -> Result<Option<(String, DateTime<Utc>)>, AuthError> {
    let row: Option<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT user_id, expires_at FROM session WHERE token = $1 AND expires_at > now()",
    )
    .bind(token)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

/// 解析请求的 auth 上下文（不依赖 axum extractor，方便从任意地方调用）。
/// 从 user_id 加载 actor 详细信息（company_ids / memberships / isInstanceAdmin）
async fn load_user_actor(db: &Db, user_id: &str) -> Actor {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as(r#"SELECT id, name FROM "user" WHERE id = $1"#)
            .bind(user_id)
            .fetch_optional(db.pool())
            .await
            .ok()
            .flatten();
    let (id, name) = row.unwrap_or_else(|| (user_id.to_string(), None));
    let email: Option<String> = sqlx::query_scalar(
        "SELECT email FROM account WHERE user_id = $1 AND provider = 'credential' LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(db.pool())
    .await
    .ok()
    .flatten();
    let is_instance_admin: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM instance_user_roles WHERE user_id = $1 AND role = 'instance_admin')",
    )
    .bind(user_id)
    .fetch_one(db.pool())
    .await
    .unwrap_or(false);
    let memberships: Vec<(Uuid, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT company_id, role::text, status::text FROM company_member \
         WHERE user_id = $1 AND (status IS NULL OR status = 'active')",
    )
    .bind(user_id)
    .fetch_all(db.pool())
    .await
    .unwrap_or_default();
    let company_ids: Vec<Uuid> = memberships.iter().map(|(c, _, _)| *c).collect();
    Actor::User {
        id,
        name,
        email,
        is_instance_admin,
        company_ids,
        memberships: memberships
            .into_iter()
            .map(|(company_id, role, status)| CompanyMembership {
                company_id,
                role,
                status,
            })
            .collect(),
        run_id: None,
    }
}

/// 从 axum `Parts` 解析认证上下文（向后兼容）。
pub async fn resolve_auth(db: &Db, parts: &Parts) -> Result<AuthContext, AuthError> {
    resolve_auth_from_headers(
        db,
        parts.headers.clone(),
        &parts.method.to_string(),
        &parts.uri.to_string(),
    )
    .await
}

/// 从 HTTP 头解析认证上下文（与 Node 版 `actorMiddleware` 等价）。
/// method / uri 仅用于审计日志；不参与解析逻辑。
pub async fn resolve_auth_from_headers(
    db: &Db,
    headers: axum::http::HeaderMap,
    _method: &str,
    _uri: &str,
) -> Result<AuthContext, AuthError> {
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            if let Some((key_id, user_id)) = resolve_api_key(db, token).await? {
                touch_api_key(db, key_id).await.ok();
                let actor = load_user_actor(db, &user_id).await;
                return Ok(AuthContext {
                    actor,
                    source: ActorSource::ApiKey,
                    method: "api_key",
                    api_key_id: Some(key_id),
                });
            }
            if let Some((user_id, _)) = resolve_session(db, token).await? {
                let actor = load_user_actor(db, &user_id).await;
                return Ok(AuthContext {
                    actor,
                    source: ActorSource::Session,
                    method: "session",
                    api_key_id: None,
                });
            }
            return Err(AuthError::InvalidToken);
        }
    }
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for kv in cookie.split(';') {
            if let Some(v) = kv.trim().strip_prefix("paperclip_session=") {
                if let Some((user_id, _)) = resolve_session(db, v).await? {
                    let actor = load_user_actor(db, &user_id).await;
                    return Ok(AuthContext {
                        actor,
                        source: ActorSource::SessionCookie,
                        method: "session_cookie",
                        api_key_id: None,
                    });
                }
            }
        }
    }
    if let Some(agent_id) = headers
        .get("x-paperclip-agent-id")
        .and_then(|v| v.to_str().ok())
    {
        if let Ok(uuid) = Uuid::parse_str(agent_id) {
            let company_id: Option<Uuid> =
                sqlx::query_scalar("SELECT company_id FROM agents WHERE id = $1")
                    .bind(uuid)
                    .fetch_optional(db.pool())
                    .await
                    .ok()
                    .flatten();
            let actor = match company_id {
                Some(cid) => Actor::Agent {
                    id: uuid,
                    company_id: cid,
                    key_id: None,
                    key_scope: KeyScope::default(),
                    run_id: None,
                    on_behalf_of_user_id: None,
                    on_behalf_of_memberships: Vec::new(),
                },
                None => Actor::Anonymous,
            };
            return Ok(AuthContext {
                actor,
                source: ActorSource::AgentHeader,
                method: "agent_header",
                api_key_id: None,
            });
        }
    }
    Ok(AuthContext::anonymous())
}

pub struct ApiKeyIssuer;
impl ApiKeyIssuer {
    pub fn new_token() -> (String, String) {
        use base64::Engine;
        let mut bytes = [0u8; 32];
        let now = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let uuid = Uuid::new_v4();
        #[allow(clippy::cast_sign_loss)]
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (now as u64).to_le_bytes()[i % 8] ^ uuid.as_bytes()[i % 16];
        }
        let raw = format!(
            "pcak_{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        );
        let hash = hash_token(&raw);
        (raw, hash)
    }
    pub async fn create(db: &Db, user_id: &str, name: &str) -> Result<(Uuid, String), AuthError> {
        let (raw, hash) = Self::new_token();
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO board_api_keys (user_id, name, key_hash) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(user_id)
        .bind(name)
        .bind(&hash)
        .fetch_one(db.pool())
        .await?;
        Ok((id, raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_deterministic() {
        let a = hash_token("hello");
        let b = hash_token("hello");
        assert_eq!(a, b);
        let c = hash_token("world");
        assert_ne!(a, c);
    }
    #[test]
    fn hash_password_produces_argon2id_phc_string() {
        let hash = hash_password("hunter2").expect("hash");
        assert!(hash.starts_with("$argon2id$"));
    }

    #[test]
    fn verify_password_round_trips() {
        let hash = hash_password("CorrectHorseBatteryStaple").expect("hash");
        assert!(verify_password("CorrectHorseBatteryStaple", &hash));
        assert!(!verify_password("wrong-password", &hash));
    }

    #[test]
    fn verify_password_rejects_invalid_hash_format() {
        assert!(!verify_password("any", "not-a-valid-phc-string"));
        assert!(!verify_password("any", ""));
    }

    #[test]
    fn generate_session_token_is_unique_and_url_safe() {
        let a = generate_session_token();
        let b = generate_session_token();
        assert_ne!(a, b);
        assert!(a.len() >= 32);
        // URL-safe base64: no `+` or `/` or `=`
        assert!(!a.contains('+'));
        assert!(!a.contains('/'));
        assert!(!a.contains('='));
    }


    // -------- r514: pk_ prefix convention --------

    #[test]
    fn r514_key_prefix_pk_is_pk() {
        assert_eq!(KeyPrefix::Pk.as_str(), "pk_");
    }

    #[test]
    fn r514_key_prefix_sess_is_sess() {
        assert_eq!(KeyPrefix::Sess.as_str(), "sess_");
    }

    #[test]
    fn r514_key_prefix_parse_recognizes_pk() {
        assert_eq!(KeyPrefix::parse("pk_abc123"), Some(KeyPrefix::Pk));
    }

    #[test]
    fn r514_key_prefix_parse_recognizes_legacy_pcak() {
        // R514 legacy alias: pcak_ tokens minted by ApiKeyIssuer.
        assert_eq!(KeyPrefix::parse("pcak_abc123"), Some(KeyPrefix::Pk));
    }

    #[test]
    fn r514_key_prefix_parse_recognizes_legacy_pcp_board() {
        // R514 legacy alias: pcp_board_ tokens minted by access.rs.
        assert_eq!(KeyPrefix::parse("pcp_board_abc123"), Some(KeyPrefix::Pk));
    }

    #[test]
    fn r514_key_prefix_parse_recognizes_sess() {
        assert_eq!(KeyPrefix::parse("sess_xyz"), Some(KeyPrefix::Sess));
    }

    #[test]
    fn r514_key_prefix_parse_rejects_empty_body() {
        assert_eq!(KeyPrefix::parse("pk_"), None);
        assert_eq!(KeyPrefix::parse("pcak_"), None);
        assert_eq!(KeyPrefix::parse("pcp_board_"), None);
        assert_eq!(KeyPrefix::parse("sess_"), None);
    }

    #[test]
    fn r514_key_prefix_parse_rejects_unknown_prefix() {
        assert_eq!(KeyPrefix::parse("totally_random_token"), None);
        assert_eq!(KeyPrefix::parse("sk_abc"), None); // sk_ is reserved for future
        assert_eq!(KeyPrefix::parse(""), None);
    }

    #[test]
    fn r514_generate_api_key_has_pk_prefix() {
        let token = generate_api_key(KeyPrefix::Pk);
        assert!(token.starts_with("pk_"));
        assert_eq!(token.len(), 3 + 32, "3 prefix + 32 url-safe chars");
        assert_eq!(KeyPrefix::parse(&token), Some(KeyPrefix::Pk));
    }

    #[test]
    fn r514_generate_api_key_unique_across_calls() {
        let a = generate_api_key(KeyPrefix::Pk);
        let b = generate_api_key(KeyPrefix::Pk);
        assert_ne!(a, b);
    }

    #[test]
    fn r514_has_key_prefix_accepts_matching() {
        assert!(has_key_prefix("pk_abc", KeyPrefix::Pk));
        assert!(has_key_prefix("pcak_abc", KeyPrefix::Pk));
        assert!(has_key_prefix("sess_xyz", KeyPrefix::Sess));
    }

    #[test]
    fn r514_has_key_prefix_rejects_mismatch() {
        // session token can't be accepted as API key
        assert!(!has_key_prefix("sess_abc", KeyPrefix::Pk));
        // API key can't be accepted as session
        assert!(!has_key_prefix("pk_abc", KeyPrefix::Sess));
        // unknown prefix
        assert!(!has_key_prefix("totally_random", KeyPrefix::Pk));
        assert!(!has_key_prefix("", KeyPrefix::Pk));
    }

    #[test]
    fn r514_pk_token_url_safe() {
        // body part (after pk_) should be URL-safe base64 (no + / =)
        let token = generate_api_key(KeyPrefix::Pk);
        let body = token.strip_prefix("pk_").expect("has pk_ prefix");
        assert!(!body.contains('+'));
        assert!(!body.contains('/'));
        assert!(!body.contains('='));
    }

    #[test]
    fn new_token_format_and_uniqueness() {
        let (raw1, h1) = ApiKeyIssuer::new_token();
        let (raw2, h2) = ApiKeyIssuer::new_token();
        assert!(raw1.starts_with("pcak_"));
        assert!(raw2.starts_with("pcak_"));
        assert_ne!(raw1, raw2);
        assert_eq!(h1, hash_token(&raw1));
        assert_eq!(h2, hash_token(&raw2));
    }
}
