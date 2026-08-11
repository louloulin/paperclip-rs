//! R558 — pc-responsible-user-denial-copy 综合测试。

#![allow(clippy::doc_markdown)]

use pc_responsible_user_denial_copy::{
    describe_responsible_user_denial, is_responsible_user_denial_code, responsible_user_label,
    ResponsibleUserDenialCode, ResponsibleUserDenialOptions, ResponsibleUserDenialTone,
    RESPONSIBLE_USER_DENIAL_CODES,
};

#[test]
fn r558_constants_match_node() {
    assert_eq!(
        RESPONSIBLE_USER_DENIAL_CODES,
        [
            "RESPONSIBLE_USER_UNAUTHORIZED",
            "RESPONSIBLE_USER_UNAVAILABLE"
        ]
    );
}

#[test]
fn r558_code_round_trip() {
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
fn r558_is_type_guard() {
    assert!(is_responsible_user_denial_code(
        "RESPONSIBLE_USER_UNAUTHORIZED"
    ));
    assert!(is_responsible_user_denial_code(
        "RESPONSIBLE_USER_UNAVAILABLE"
    ));
    assert!(!is_responsible_user_denial_code("OTHER"));
    assert!(!is_responsible_user_denial_code(""));
}

#[test]
fn r558_label_fallbacks() {
    assert_eq!(responsible_user_label(None), "the responsible user");
    assert_eq!(responsible_user_label(Some("")), "the responsible user");
    assert_eq!(responsible_user_label(Some("   ")), "the responsible user");
}

#[test]
fn r558_label_uses_known_name() {
    assert_eq!(responsible_user_label(Some("Alice")), "Alice");
    assert_eq!(responsible_user_label(Some("  Bob  ")), "Bob");
}

#[test]
fn r558_describe_unauthorized_with_name() {
    let copy = describe_responsible_user_denial(
        ResponsibleUserDenialCode::Unauthorized,
        Some(ResponsibleUserDenialOptions {
            user_name: Some("Alice"),
        }),
    );
    assert_eq!(copy.code, ResponsibleUserDenialCode::Unauthorized);
    assert_eq!(copy.tone, ResponsibleUserDenialTone::Unauthorized);
    assert_eq!(copy.title, "Responsible user not authorized");
    assert!(copy.description.contains("Alice"));
    assert!(copy.description.contains("does not have permission"));
    assert!(copy.recommended_action.contains("Alice"));
    assert!(copy.recommended_action.contains("Grant"));
}

#[test]
fn r558_describe_unauthorized_without_name() {
    let copy = describe_responsible_user_denial(ResponsibleUserDenialCode::Unauthorized, None);
    assert!(copy.description.contains("the responsible user"));
    assert!(!copy.description.contains("Alice"));
    assert!(copy.recommended_action.contains("the responsible user"));
}

#[test]
fn r558_describe_unauthorized_with_blank_name() {
    let copy = describe_responsible_user_denial(
        ResponsibleUserDenialCode::Unauthorized,
        Some(ResponsibleUserDenialOptions {
            user_name: Some("   "),
        }),
    );
    assert!(copy.description.contains("the responsible user"));
}

#[test]
fn r558_describe_unavailable_with_name() {
    let copy = describe_responsible_user_denial(
        ResponsibleUserDenialCode::Unavailable,
        Some(ResponsibleUserDenialOptions {
            user_name: Some("Bob"),
        }),
    );
    assert_eq!(copy.code, ResponsibleUserDenialCode::Unavailable);
    assert_eq!(copy.tone, ResponsibleUserDenialTone::Unavailable);
    assert_eq!(copy.title, "Responsible user unavailable");
    assert!(copy.description.contains("Bob"));
    assert!(copy.description.contains("removed or deactivated"));
    assert!(copy
        .recommended_action
        .contains("reassign a responsible user"));
    assert!(copy.recommended_action.contains("reactivate"));
}

#[test]
fn r558_describe_unavailable_without_name() {
    let copy = describe_responsible_user_denial(ResponsibleUserDenialCode::Unavailable, None);
    assert!(copy.description.contains("the responsible user"));
    assert!(copy.recommended_action.contains("Mark the work blocked"));
}

#[test]
fn r558_tone_matches_code() {
    let copy1 = describe_responsible_user_denial(ResponsibleUserDenialCode::Unauthorized, None);
    assert_eq!(copy1.tone, ResponsibleUserDenialTone::Unauthorized);
    let copy2 = describe_responsible_user_denial(ResponsibleUserDenialCode::Unavailable, None);
    assert_eq!(copy2.tone, ResponsibleUserDenialTone::Unavailable);
}

#[test]
fn r558_default_options() {
    let opts = ResponsibleUserDenialOptions::default();
    assert!(opts.user_name.is_none());
}

#[test]
fn r558_codes_are_distinct_from_other_denial_crate() {
    // These codes are intentionally different from `pc-responsible-user-denial`'s
    // server-side codes (rate_limited, not_entitled, etc).
    for c in RESPONSIBLE_USER_DENIAL_CODES {
        assert!(
            !c.contains("rate_limited")
                && !c.contains("not_entitled")
                && !c.contains("unsupported_channel")
                && !c.contains("quota_exceeded")
        );
    }
}
