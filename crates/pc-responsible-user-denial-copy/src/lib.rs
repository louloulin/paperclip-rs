#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Responsible-user ("on behalf of") authorization denial copy contract.
//!
//! R558: Direct port of `paperclip/packages/shared/src/responsible-user-denial.ts`
//! (76 LOC).
//!
//! When an agent run acts on behalf of a human user, authorization is the
//! intersection of the agent's permissions and that user's permissions.
//! When the intersection denies, the authz layer emits one of these two codes.
//! This crate is the single source of truth for how those codes are explained
//! to humans, so every surface renders consistent, actionable language.
//!
//! Distinct from `pc-responsible-user-denial` (which holds the server-side
//! run-outcome code normalization).

/// All denial codes this module knows how to describe.
pub const RESPONSIBLE_USER_DENIAL_CODES: [&str; 2] = [
    "RESPONSIBLE_USER_UNAUTHORIZED",
    "RESPONSIBLE_USER_UNAVAILABLE",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResponsibleUserDenialCode {
    Unauthorized,
    Unavailable,
}

impl ResponsibleUserDenialCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "RESPONSIBLE_USER_UNAUTHORIZED",
            Self::Unavailable => "RESPONSIBLE_USER_UNAVAILABLE",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "RESPONSIBLE_USER_UNAUTHORIZED" => Some(Self::Unauthorized),
            "RESPONSIBLE_USER_UNAVAILABLE" => Some(Self::Unavailable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResponsibleUserDenialTone {
    Unauthorized,
    Unavailable,
}

impl ResponsibleUserDenialTone {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Type guard for an arbitrary `&str` — returns true iff `code` is one of
/// the two known denial codes.
pub fn is_responsible_user_denial_code(code: &str) -> bool {
    ResponsibleUserDenialCode::parse(code).is_some()
}

/// Render a stable label for the responsible user. Falls back to a generic
/// noun when the display name is unknown, so copy never shows a raw id.
pub fn responsible_user_label(user_name: Option<&str>) -> String {
    let trimmed = user_name.map_or("", str::trim);
    if trimmed.is_empty() {
        "the responsible user".to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponsibleUserDenialCopy {
    pub code: ResponsibleUserDenialCode,
    pub tone: ResponsibleUserDenialTone,
    pub title: String,
    pub description: String,
    pub recommended_action: String,
}

/// Describe a responsible-user denial for display.
///
/// `user_name` is the responsible user's display name when known; when omitted,
/// generic phrasing is used. These two codes are distinct from a plain
/// agent-lacks-permission denial: here the *agent* is allowed but the *human
/// this run acts for* is not (or is no longer available).
pub fn describe_responsible_user_denial(
    code: ResponsibleUserDenialCode,
    options: Option<ResponsibleUserDenialOptions<'_>>,
) -> ResponsibleUserDenialCopy {
    let who = responsible_user_label(options.and_then(|o| o.user_name));

    match code {
        ResponsibleUserDenialCode::Unavailable => ResponsibleUserDenialCopy {
            code,
            tone: ResponsibleUserDenialTone::Unavailable,
            title: "Responsible user unavailable".into(),
            description: format!(
                "This run acts on behalf of {who}, but that account was removed or \
                 deactivated, so its permissions can no longer be evaluated. The \
                 agent's own permissions are not enough on their own — every \
                 action still requires an active responsible user."
            ),
            recommended_action: "Mark the work blocked and reassign a responsible user (or \
                 reactivate the account) before the agent continues."
                .into(),
        },
        ResponsibleUserDenialCode::Unauthorized => ResponsibleUserDenialCopy {
            code,
            tone: ResponsibleUserDenialTone::Unauthorized,
            title: "Responsible user not authorized".into(),
            description: format!(
                "This action was denied because {who} — the user this run acts on \
                 behalf of — does not have permission to perform it. The agent \
                 may be allowed, but a run can never exceed the permissions of \
                 the user it acts for, so the action is blocked."
            ),
            recommended_action: format!(
                "Grant {who} the required permission, or have someone who is \
                 authorized take this action instead."
            ),
        },
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ResponsibleUserDenialOptions<'a> {
    pub user_name: Option<&'a str>,
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn code_constants_match_node() {
        assert_eq!(
            RESPONSIBLE_USER_DENIAL_CODES,
            [
                "RESPONSIBLE_USER_UNAUTHORIZED",
                "RESPONSIBLE_USER_UNAVAILABLE"
            ]
        );
    }

    #[test]
    fn code_round_trip() {
        for c in [
            ResponsibleUserDenialCode::Unauthorized,
            ResponsibleUserDenialCode::Unavailable,
        ] {
            let s = c.as_str();
            assert_eq!(ResponsibleUserDenialCode::parse(s), Some(c));
        }
        assert!(ResponsibleUserDenialCode::parse("nope").is_none());
    }

    #[test]
    fn is_type_guard() {
        assert!(is_responsible_user_denial_code(
            "RESPONSIBLE_USER_UNAUTHORIZED"
        ));
        assert!(is_responsible_user_denial_code(
            "RESPONSIBLE_USER_UNAVAILABLE"
        ));
        assert!(!is_responsible_user_denial_code("OTHER_CODE"));
        assert!(!is_responsible_user_denial_code(""));
    }

    #[test]
    fn label_falls_back_when_unknown() {
        assert_eq!(responsible_user_label(None), "the responsible user");
        assert_eq!(responsible_user_label(Some("")), "the responsible user");
        assert_eq!(responsible_user_label(Some("   ")), "the responsible user");
    }

    #[test]
    fn label_uses_name_when_known() {
        assert_eq!(responsible_user_label(Some("Alice")), "Alice");
        assert_eq!(responsible_user_label(Some("  Bob  ")), "Bob");
    }
}
