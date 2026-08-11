//! `pc-responsible-user-denial` → `pc-responsible-user-denial-copy` bridge.
//!
//! Server-side error handlers and middleware need both halves of the
//! responsible-user-denial contract:
//!
//! 1. **Copy contract** (`pc-responsible-user-denial-copy`) — the user-facing
//!    tone/title/description strings for the two authz-layer denial codes
//!    (`RESPONSIBLE_USER_UNAUTHORIZED`, `RESPONSIBLE_USER_UNAVAILABLE`).
//!    These are the canonical identifiers emitted by Node's
//!    `server/src/middleware/auth.ts:364`.
//!
//! 2. **Run-outcome normalization** (this crate's [`codes`] module) — the
//!    snake_case classifier used by [`crate::run_outcomes::record_*`] when a
//!    run hits a denial mid-flight (`rate_limited`, `unsupported_channel`,
//!    `quota_exceeded`, `not_entitled`, `other`).
//!
//! These two sets serve different domains and must remain disjoint, but the
//! server needs to surface both through one entry point. This module re-exports
//! the copy contract under the unified `pc_responsible_user_denial::copy`
//! namespace and adds a single helper that maps a copy-side code string to a
//! rendered `ResponsibleUserDenialCopy` (with optional responsible-user name).
//!
//! ## Design
//! - **High cohesion**: all denial code logic + copy lives under one crate,
//!   one re-export surface for server-side consumers.
//! - **Low coupling**: this module does not call into `codes` or
//!   `run_outcomes`; it is a pure re-export + delegation over the copy crate.
//! - **Single source of truth**: `pc-responsible-user-denial-copy` owns the
//!   copy strings; we never duplicate them here.

#![forbid(unsafe_code)]

pub use pc_responsible_user_denial_copy::{
    describe_responsible_user_denial, is_responsible_user_denial_code, responsible_user_label,
    ResponsibleUserDenialCode, ResponsibleUserDenialCopy, ResponsibleUserDenialOptions,
    ResponsibleUserDenialTone, RESPONSIBLE_USER_DENIAL_CODES,
};

/// Render user-facing copy for a responsible-user-denial code string.
///
/// This is the convenience entry point that middleware / error handlers use
/// when they have already classified an error code (typically by reading
/// `ApiError.code`) and want to produce a stable, user-facing message.
///
/// Returns `None` when `code` is not a known responsible-user-denial code
/// (e.g. it is a run-outcome classifier like `rate_limited`), so callers can
/// fall back to a generic denial message without an extra `match`.
///
/// # Arguments
///
/// * `code` — one of [`RESPONSIBLE_USER_DENIAL_CODES`].
/// * `user_name` — optional display name of the responsible user. When `None`
///   or blank, copy uses generic phrasing.
pub fn render_responsible_user_denial_copy(
    code: &str,
    user_name: Option<&str>,
) -> Option<ResponsibleUserDenialCopy> {
    let parsed = ResponsibleUserDenialCode::parse(code)?;
    Some(describe_responsible_user_denial(
        parsed,
        Some(ResponsibleUserDenialOptions { user_name }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_re_exported_match_canonical() {
        assert_eq!(
            RESPONSIBLE_USER_DENIAL_CODES,
            [
                "RESPONSIBLE_USER_UNAUTHORIZED",
                "RESPONSIBLE_USER_UNAVAILABLE"
            ]
        );
    }

    #[test]
    fn is_responsible_user_denial_code_gates_copy_codes() {
        assert!(is_responsible_user_denial_code(
            "RESPONSIBLE_USER_UNAUTHORIZED"
        ));
        assert!(is_responsible_user_denial_code(
            "RESPONSIBLE_USER_UNAVAILABLE"
        ));
        // Run-outcome codes must NOT match (they are a separate domain).
        assert!(!is_responsible_user_denial_code("rate_limited"));
        assert!(!is_responsible_user_denial_code("not_entitled"));
        assert!(!is_responsible_user_denial_code(""));
    }

    #[test]
    fn render_unauthorized_with_name_produces_copy() {
        let copy =
            render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAUTHORIZED", Some("Alice"))
                .expect("should resolve to a copy");
        assert_eq!(copy.code, ResponsibleUserDenialCode::Unauthorized);
        assert_eq!(copy.tone, ResponsibleUserDenialTone::Unauthorized);
        assert!(!copy.title.is_empty());
        assert!(!copy.description.is_empty());
        assert!(!copy.recommended_action.is_empty());
        assert!(copy.description.contains("Alice"));
    }

    #[test]
    fn render_unavailable_without_name_falls_back() {
        let copy = render_responsible_user_denial_copy("RESPONSIBLE_USER_UNAVAILABLE", None)
            .expect("should resolve");
        assert_eq!(copy.code, ResponsibleUserDenialCode::Unavailable);
        assert_eq!(copy.tone, ResponsibleUserDenialTone::Unavailable);
        assert!(copy.description.contains("the responsible user"));
    }

    #[test]
    fn render_returns_none_for_run_outcome_codes() {
        assert!(render_responsible_user_denial_copy("rate_limited", None).is_none());
        assert!(render_responsible_user_denial_copy("not_entitled", Some("Bob")).is_none());
        assert!(render_responsible_user_denial_copy("", None).is_none());
        assert!(render_responsible_user_denial_copy("nope", None).is_none());
    }
}
