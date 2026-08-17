#![forbid(unsafe_code)]

//! Realtime event payload pure helpers -- 校验 event name / payload size / channel.
//!
//! R735: 零依赖校验 realtime event payload（避免向 WS 客户端发送过大事件）。

use serde_json::Value;

/// 单个 realtime event payload 最大字节数（4 KB）。
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 4096;

/// 合法 event name 最小长度（避免空字符串）。
pub const MIN_EVENT_NAME_LENGTH: usize = 3;

/// 合法 event name 最大长度。
pub const MAX_EVENT_NAME_LENGTH: usize = 128;

/// Channel 名称最大长度。
pub const MAX_CHANNEL_NAME_LENGTH: usize = 256;

/// 校验 event name：trim + 长度边界。
pub fn validate_event_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("event name must not be empty".into());
    }
    let len = trimmed.chars().count();
    if len < MIN_EVENT_NAME_LENGTH {
        return Err(format!("event name must be at least {MIN_EVENT_NAME_LENGTH} characters"));
    }
    if len > MAX_EVENT_NAME_LENGTH {
        return Err(format!("event name must be at most {MAX_EVENT_NAME_LENGTH} characters"));
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("event name must not contain control / whitespace characters".into());
    }
    Ok(())
}

/// 校验 channel name（trim + 长度 + 不含控制字符）。
pub fn validate_channel_name(channel: &str) -> Result<(), String> {
    let trimmed = channel.trim();
    if trimmed.is_empty() {
        return Err("channel name must not be empty".into());
    }
    if trimmed.chars().count() > MAX_CHANNEL_NAME_LENGTH {
        return Err(format!("channel name must be at most {MAX_CHANNEL_NAME_LENGTH} characters"));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("channel name must not contain control characters".into());
    }
    Ok(())
}

/// 校验 payload 字节大小（用 serde_json 序列化后估算）。
pub fn validate_payload_size(payload: &Value) -> Result<usize, String> {
    let bytes = serde_json::to_vec(payload)
        .map_err(|e| format!("payload not serializable: {e}"))?;
    let size = bytes.len();
    if size > MAX_EVENT_PAYLOAD_BYTES {
        return Err(format!("payload size {size} exceeds max {MAX_EVENT_PAYLOAD_BYTES} bytes"));
    }
    Ok(size)
}

/// 判断 channel name 是否为全局 channel ("*")。
pub fn is_global_channel(channel: &str) -> bool {
    channel.trim() == "*"
}

/// 合并 channel filter 列表去重。
pub fn dedup_channels(channels: &[String]) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<String> = BTreeSet::new();
    for c in channels {
        set.insert(c.trim().to_string());
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn validate_event_name_accepts() {
        assert!(validate_event_name("issue.created").is_ok());
        assert!(validate_event_name("agent.run_started").is_ok());
    }

    #[test]
    fn validate_event_name_rejects_empty() {
        assert!(validate_event_name("").is_err());
        assert!(validate_event_name("   ").is_err());
    }

    #[test]
    fn validate_event_name_rejects_too_short() {
        assert!(validate_event_name("ab").is_err());
    }

    #[test]
    fn validate_event_name_rejects_too_long() {
        let s = "a".repeat(MAX_EVENT_NAME_LENGTH + 1);
        assert!(validate_event_name(&s).is_err());
    }

    #[test]
    fn validate_event_name_rejects_whitespace() {
        assert!(validate_event_name("foo bar").is_err());
    }

    #[test]
    fn validate_channel_name_accepts() {
        assert!(validate_channel_name("company:abc").is_ok());
        assert!(validate_channel_name("*").is_ok());
    }

    #[test]
    fn validate_channel_name_rejects_empty() {
        assert!(validate_channel_name("").is_err());
    }

    #[test]
    fn validate_channel_name_rejects_control_char() {
        let bad = format!("foo{}bar", '\u{0}');
        assert!(validate_channel_name(&bad).is_err());
    }

    #[test]
    fn validate_payload_size_small_ok() {
        let p = serde_json::json!({"k": "v"});
        let size = validate_payload_size(&p).unwrap();
        assert!(size > 0);
        assert!(size < MAX_EVENT_PAYLOAD_BYTES);
    }

    #[test]
    fn validate_payload_size_too_big() {
        let big = "x".repeat(MAX_EVENT_PAYLOAD_BYTES + 100);
        let p = serde_json::json!({"data": big});
        assert!(validate_payload_size(&p).is_err());
    }

    #[test]
    fn is_global_channel_star() {
        assert!(is_global_channel("*"));
        assert!(is_global_channel("  *  "));
    }

    #[test]
    fn is_global_channel_false() {
        assert!(!is_global_channel("company:abc"));
        assert!(!is_global_channel(""));
    }

    #[test]
    fn dedup_channels_preserves_order() {
        let chans = vec!["a".into(), "b".into(), "a".into(), "  c  ".into()];
        let d = dedup_channels(&chans);
        assert_eq!(d, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn dedup_channels_trims_whitespace() {
        let chans = vec!["  a  ".into(), "a".into()];
        let d = dedup_channels(&chans);
        assert_eq!(d, vec!["a".to_string()]);
    }
}
