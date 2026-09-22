//! Workspace slug：URL 安全、kebab-case。

use serde::{Deserialize, Serialize};
use std::fmt;

const SLUG_REGEX_STR: &str = r"^[a-z0-9]+(?:-[a-z0-9]+)*$";
const MAX_SLUG_LENGTH: usize = 64;
const MIN_SLUG_LENGTH: usize = 2;

/// Multica 保留 slug（与 multica `reserved_slugs.json` 对齐的子集）。
pub const RESERVED_SLUGS: &[&str] = &[
    "api",
    "admin",
    "dashboard",
    "settings",
    "auth",
    "login",
    "logout",
    "signup",
    "register",
    "billing",
    "support",
    "docs",
    "help",
    "issues",
    "agents",
    "runtimes",
    "skills",
    "plugins",
    "channels",
    "autopilots",
    "squads",
    "projects",
    "inbox",
    "inbox-archive",
    "workspaces",
    "members",
    "profile",
    "me",
];

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum SlugError {
    #[error("slug too short (min {MIN_SLUG_LENGTH})")]
    TooShort,
    #[error("slug too long (max {MAX_SLUG_LENGTH})")]
    TooLong,
    #[error("invalid slug format: must match kebab-case ({SLUG_REGEX_STR})")]
    InvalidFormat,
    #[error("slug is reserved: {0}")]
    Reserved(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slug(String);

impl Slug {
    pub fn parse(s: &str) -> Result<Self, SlugError> {
        if s.len() < MIN_SLUG_LENGTH {
            return Err(SlugError::TooShort);
        }
        if s.len() > MAX_SLUG_LENGTH {
            return Err(SlugError::TooLong);
        }
        let regex = regex::Regex::new(SLUG_REGEX_STR).unwrap();
        if !regex.is_match(s) {
            return Err(SlugError::InvalidFormat);
        }
        if RESERVED_SLUGS.contains(&s) {
            return Err(SlugError::Reserved(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }

    pub fn parse_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Slug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_slug_parses() {
        assert!(Slug::parse("my-team").is_ok());
        assert!(Slug::parse("acme").is_ok());
        assert!(Slug::parse("team-123").is_ok());
    }

    #[test]
    fn invalid_slug_rejected() {
        assert!(Slug::parse("My-Team").is_err()); // uppercase
        assert!(Slug::parse("-leading").is_err());
        assert!(Slug::parse("trailing-").is_err());
        assert!(Slug::parse("with--double").is_err());
        assert!(Slug::parse("a").is_err()); // too short
        assert!(Slug::parse(&"x".repeat(MAX_SLUG_LENGTH + 1)).is_err());
    }

    #[test]
    fn reserved_slug_rejected() {
        assert!(matches!(Slug::parse("api"), Err(SlugError::Reserved(_))));
        assert!(matches!(Slug::parse("admin"), Err(SlugError::Reserved(_))));
    }
}
