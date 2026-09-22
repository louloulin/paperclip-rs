//! User 领域类型。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Id,
    pub name: String,
    pub email: String,
    pub avatar_url: Option<String>,
    pub email_verified_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub onboarded_at: Option<Timestamp>,
    pub onboarding_state: Option<serde_json::Value>,
    pub language: Option<String>,
    pub timezone: Option<String>,
    pub profile_description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_constructs() {
        let u = User {
            id: Id::new(),
            name: "alice".into(),
            email: "alice@example.com".into(),
            avatar_url: None,
            email_verified_at: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            onboarded_at: None,
            onboarding_state: None,
            language: None,
            timezone: None,
            profile_description: None,
        };
        assert_eq!(u.email, "alice@example.com");
    }
}
