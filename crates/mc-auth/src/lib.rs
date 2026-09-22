//! Multica auth: session / cookie / API key / PAT / verification code。
//!
//! 与 paperclip-rs `pc-auth` 风格一致，但 API key / cookie 命名走 multica 前缀。

pub mod api_key;
pub mod cookie;
pub mod password;
pub mod pat;
pub mod session;
pub mod verification;

pub use session::{InMemorySessionStore, Session, SessionStore};

pub use api_key::ApiKey;
pub use container::{
    DefaultSecretsBackend, PatStoreContainer, SessionStoreContainer, VerificationStoreContainer,
};
pub use cookie::{CookieOptions, SameSite};
pub use password::{hash_password, verify_password};
pub use pat::{InMemoryPatStore, Pat, PatStore};
pub use verification::{InMemoryVerificationStore, VerificationCode, VerificationCodeStore};

pub mod container;

/// Multica auth 命名空间默认配置常量。
pub const DEFAULT_SESSION_COOKIE: &str = "multica_session";
pub const DEFAULT_API_KEY_HEADER: &str = "X-Multica-Api-Key";
pub const DEFAULT_CSRF_HEADER: &str = "X-Multica-Csrf";
pub const DEFAULT_API_KEY_PREFIX: &str = "mk_"; // multica key

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_stable() {
        assert_eq!(DEFAULT_SESSION_COOKIE, "multica_session");
        assert_eq!(DEFAULT_API_KEY_HEADER, "X-Multica-Api-Key");
        assert!(DEFAULT_API_KEY_PREFIX.starts_with("mk_"));
    }
}
