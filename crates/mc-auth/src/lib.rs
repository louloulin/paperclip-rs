//! Multica auth: session / cookie / API key / PAT / verification code。
//!
//! 与 paperclip-rs `pc-auth` 风格一致，但 API key / cookie 命名走 multica 前缀。

pub mod session;
pub mod cookie;
pub mod api_key;
pub mod pat;
pub mod verification;
pub mod password;

pub use session::{Session, SessionStore, InMemorySessionStore};

pub use container::{SessionStoreContainer, PatStoreContainer, VerificationStoreContainer, DefaultSecretsBackend};
pub use cookie::{CookieOptions, SameSite};
pub use api_key::ApiKey;
pub use pat::{Pat, PatStore, InMemoryPatStore};
pub use verification::{VerificationCode, VerificationCodeStore, InMemoryVerificationStore};
pub use password::{hash_password, verify_password};

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