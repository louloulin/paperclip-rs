//! Round 346：Node `redactCurrentUserText` + 关键脱敏规则的 Rust 端口。

use pc_heartbeat::recovery::redact_watchdog_evidence_text::{
    redact_watchdog_evidence_text, CurrentUserRedactionOptions,
};

#[test]
fn masks_user_name_with_first_letter_and_stars() {
    let redacted = redact_watchdog_evidence_text(
        "tail by alice",
        CurrentUserRedactionOptions {
            enabled: true,
            user_names: vec!["alice".to_owned()],
            home_dirs: vec!["/Users/alice".to_owned()],
            replacement: None,
        },
    );
    assert_eq!(redacted, "tail by a*****");
}

#[test]
fn replaces_home_directory_segments_with_masked_user() {
    let redacted = redact_watchdog_evidence_text(
        "log: /Users/alice/projects/secret.txt",
        CurrentUserRedactionOptions {
            enabled: true,
            user_names: vec!["alice".to_owned()],
            home_dirs: vec!["/Users/alice".to_owned()],
            replacement: None,
        },
    );
    assert_eq!(redacted, "log: /Users/a*****/projects/secret.txt");
}

#[test]
fn respects_word_boundary_for_user_names() {
    let redacted = redact_watchdog_evidence_text(
        "bob=alice&bobby",
        CurrentUserRedactionOptions {
            enabled: true,
            user_names: vec!["bob".to_owned()],
            home_dirs: vec![],
            replacement: None,
        },
    );
    assert_eq!(redacted, "b***=alice&bobby");
}

#[test]
fn disabled_option_returns_input_unchanged() {
    let raw = "/Users/alice ran the build";
    let redacted = redact_watchdog_evidence_text(
        raw,
        CurrentUserRedactionOptions {
            enabled: false,
            user_names: vec!["alice".to_owned()],
            home_dirs: vec!["/Users/alice".to_owned()],
            replacement: None,
        },
    );
    assert_eq!(redacted, raw);
}
