use pc_adapter_claude_local::claude_errors::{
    describe_claude_failure, extract_claude_retry_not_before, is_claude_login_required,
    is_claude_max_turns_result, is_claude_model_not_found_error,
    is_claude_poisoned_previous_message_id_error, is_claude_provider_quota_error,
    is_claude_refusal_result, is_claude_transient_upstream_error,
};
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn failure描述包含subtype和错误正文() {
    let value = serde_json::json!({"subtype":"error_api","errors":[{"message":"upstream failed"}]});
    assert_eq!(
        describe_claude_failure(&value).as_deref(),
        Some("Claude run failed: subtype=error_api: upstream failed")
    );
    assert!(describe_claude_failure(&serde_json::json!({})).is_none());
}

#[test]
fn 模型不存在和最大回合分类() {
    assert!(is_claude_model_not_found_error(
        None,
        "",
        "404 model not found",
        None
    ));
    assert!(!is_claude_model_not_found_error(
        None,
        "model ready",
        "",
        None
    ));
    assert!(is_claude_max_turns_result(Some(
        &serde_json::json!({"subtype":"error_max_turns"})
    )));
    assert!(is_claude_max_turns_result(Some(
        &serde_json::json!({"stop_reason":"turn_limit_exhausted"})
    )));
}

#[test]
fn refusal和previous_message污染分类() {
    assert!(is_claude_refusal_result(Some(
        &serde_json::json!({"errorCode":"refusal"})
    )));
    assert!(is_claude_poisoned_previous_message_id_error(
        &serde_json::json!({"result":"diagnostics.previous_message_id starts with `msg_`"})
    ));
}

#[test]
fn transient与provider_quota互斥() {
    assert!(is_claude_transient_upstream_error(
        None,
        "",
        "HTTP 503 overloaded",
        None
    ));
    assert!(!is_claude_transient_upstream_error(
        None,
        "",
        "usage limit reached",
        None
    ));
    assert!(is_claude_provider_quota_error(
        None,
        "",
        "weekly limit reached",
        None
    ));
    assert!(!is_claude_provider_quota_error(
        None,
        "",
        "authentication required",
        None
    ));
}

#[test]
fn 登录错误不是transient() {
    assert!(is_claude_login_required(
        None,
        "",
        "Please run claude login"
    ));
    assert!(!is_claude_transient_upstream_error(
        None,
        "",
        "Please run claude login",
        None
    ));
}

#[test]
fn reset时间在当天已过时滚到次日() {
    let now = UNIX_EPOCH + Duration::from_secs(23 * 3600 + 30 * 60);
    let retry =
        extract_claude_retry_not_before("Usage limit reached; resets 3:15 AM (UTC)", now).unwrap();
    assert_eq!(
        retry.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        24 * 3600 + 3 * 3600 + 15 * 60
    );
}

#[test]
fn reset时间支持下午格式并拒绝无效输入() {
    let now = UNIX_EPOCH + Duration::from_secs(10 * 3600);
    let retry = extract_claude_retry_not_before("out of extra usage · resets 4pm", now).unwrap();
    assert_eq!(
        retry.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        16 * 3600
    );
    assert!(extract_claude_retry_not_before("try again later", now).is_none());
}
