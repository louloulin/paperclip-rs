use pc_adapter_codex_local::codex_errors::{
    classify_codex_auth_refresh_failure, extract_codex_retry_not_before, is_codex_harness_crash,
    is_codex_provider_quota_error, is_codex_transient_upstream_error,
    is_codex_unknown_session_error, CodexAuthRefreshFailureClass, CodexProtocolState,
};
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn harness崩溃只在协议启动但没有终态时成立() {
    assert!(is_codex_harness_crash(CodexProtocolState {
        exit_code: Some(1),
        saw_protocol_event: true,
        saw_protocol_terminal_event: false
    }));
    assert!(!is_codex_harness_crash(CodexProtocolState {
        exit_code: Some(1),
        saw_protocol_event: true,
        saw_protocol_terminal_event: true
    }));
    assert!(!is_codex_harness_crash(CodexProtocolState {
        exit_code: Some(0),
        saw_protocol_event: true,
        saw_protocol_terminal_event: false
    }));
}

#[test]
fn stale_session文案覆盖rollout路径错误() {
    assert!(is_codex_unknown_session_error(
        "",
        "no rollout found for thread id abc"
    ));
    assert!(is_codex_unknown_session_error("unknown thread id", ""));
    assert!(is_codex_unknown_session_error(
        "",
        "state db returned stale rollout path for thread abc"
    ));
    assert!(!is_codex_unknown_session_error("", "model overloaded"));
}

#[test]
fn oauth刷新失败分类遵循优先级() {
    assert_eq!(
        classify_codex_auth_refresh_failure(None, Some("refresh token has expired"), None),
        Some(CodexAuthRefreshFailureClass::RefreshTokenExpired)
    );
    assert_eq!(
        classify_codex_auth_refresh_failure(Some("refresh_token_reused"), None, None),
        Some(CodexAuthRefreshFailureClass::RefreshTokenReused)
    );
    assert_eq!(
        classify_codex_auth_refresh_failure(Some("OAuth returned invalid_grant"), None, None),
        Some(CodexAuthRefreshFailureClass::RefreshTokenInvalidated)
    );
    assert_eq!(
        classify_codex_auth_refresh_failure(None, None, Some("bare 401")),
        None
    );
}

#[test]
fn usage_limit是quota而非transient() {
    let now = UNIX_EPOCH + Duration::from_secs(22 * 3600);
    let message = "You've hit your usage limit for GPT-5. Switch to another model now, or try again at 11:31 PM.";
    assert!(is_codex_provider_quota_error(
        None,
        None,
        Some(message),
        now
    ));
    assert!(!is_codex_transient_upstream_error(
        None,
        None,
        Some(message),
        now
    ));
    assert!(extract_codex_retry_not_before(message, now).is_some());
}

#[test]
fn remote_compaction高需求错误是可重试上游错误() {
    assert!(is_codex_transient_upstream_error(None, Some("Error running remote compact task: We're currently experiencing high demand, which may cause temporary errors."), None, UNIX_EPOCH));
    assert!(!is_codex_transient_upstream_error(
        None,
        None,
        Some("Error running remote compact task: unknown parameter"),
        UNIX_EPOCH
    ));
}

#[test]
fn 无重试提示的capacity仍分类quota() {
    let now = UNIX_EPOCH + Duration::from_secs(10 * 3600);
    assert!(is_codex_provider_quota_error(
        None,
        Some("The requested model is at capacity"),
        None,
        now
    ));
    assert!(extract_codex_retry_not_before("The requested model is at capacity", now).is_none());
}

#[test]
fn retry时间已过会滚到次日() {
    let now = UNIX_EPOCH + Duration::from_secs(23 * 3600);
    let retry = extract_codex_retry_not_before("try again at 11:31 PM", now).unwrap();
    assert_eq!(
        retry.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        23 * 3600 + 31 * 60
    );
}

#[test]
fn iana时区提示按当地墙上时间换算() {
    let now = UNIX_EPOCH + Duration::from_secs(1776983342); // 2026-04-24 17:09 UTC
    let retry = extract_codex_retry_not_before(
        "You've hit your usage limit for GPT. Switch to another model now, or try again at 11:31 PM (America/Chicago).",
        now,
    ).unwrap();
    assert_eq!(
        retry.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        1776987060
    ); // 2026-04-24 23:31 UTC
}
