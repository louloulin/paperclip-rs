#![forbid(unsafe_code)]

//! Tool invocation pure helpers -- 1:1 port of small utility helpers in
//! paperclip/server/src/services/tool-access.ts.
//!
//! R741: 零依赖 helpers (key normalization, percent, percentile, actor binding).

/// Normalize key (对齐 Node normalizeKey):
/// - trim + lowercase
/// - 非 [a-z0-9._:-] 替换为 -
/// - 去首尾 -
/// - 截断到 160 字符
/// - 空结果 fallback 为 "tool"
pub fn normalize_key(input: &str) -> String {
    let lower = input.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut prev_dash = false;
    for c in lower.chars() {
        let is_safe = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == ':';
        if is_safe {
            out.push(c);
            prev_dash = false;
        } else {
            if !prev_dash && !out.is_empty() {
                out.push('-');
            }
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        return "tool".to_string();
    }
    if trimmed.chars().count() > 160 {
        trimmed.chars().take(160).collect()
    } else {
        trimmed
    }
}

/// 构造 connection uid (namespace/name-connectionId 前 8 位)。
pub fn connection_uid(namespace: &str, name: &str, connection_id: &str) -> String {
    let prefix_len = 8.min(connection_id.len());
    format!("{}/{}-{}", normalize_key(namespace), normalize_key(name), &connection_id[..prefix_len])
}

/// 安全解析 number value (对齐 Node numberValue)。
pub fn number_value(value: &str) -> Option<f64> {
    let parsed: f64 = value.parse().ok()?;
    if parsed.is_finite() {
        Some(parsed)
    } else {
        None
    }
}

/// 计算百分比（保留 1 位小数）。
///
/// 对齐 Node percent: Math.round((n/d) * 1000) / 10。
pub fn percent(numerator: f64, denominator: f64) -> f64 {
    if denominator <= 0.0 {
        return 0.0;
    }
    let pct = numerator / denominator * 100.0;
    (pct * 10.0).round() / 10.0
}

/// 计算百分位数 (对齐 Node percentile)。
///
/// p: 0..100 的百分位点。
pub fn percentile(values: &[f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = values.iter().copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((((p / 100.0) * sorted.len() as f64).ceil() as i64) - 1)
        .max(0)
        .min(sorted.len() as i64 - 1) as usize;
    Some(sorted[idx])
}

/// OAuth actor 类型白名单。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorType {
    Agent,
    User,
    System,
    Plugin,
}

impl ActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::System => "system",
            Self::Plugin => "plugin",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(Self::Agent),
            "user" => Some(Self::User),
            "system" => Some(Self::System),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }
}

/// 校验 OAuth actor type 字符串。
pub fn oauth_actor_type(value: &str) -> Option<ActorType> {
    ActorType::from_str(value)
}

/// Actor info 绑定 (trim + 校验)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActorBinding {
    pub actor_type: Option<ActorType>,
    pub actor_id: Option<String>,
    pub session_id: Option<String>,
}

pub fn actor_binding(actor_type: Option<&str>, actor_id: Option<&str>, session_id: Option<&str>) -> ActorBinding {
    ActorBinding {
        actor_type: actor_type.and_then(oauth_actor_type),
        actor_id: actor_id.filter(|s| !s.is_empty()).map(|s| s.to_string()),
        session_id: session_id
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string()),
    }
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn normalize_key_basic() {
        assert_eq!(normalize_key("Hello World"), "hello-world");
    }

    #[test]
    fn normalize_key_special_chars() {
        assert_eq!(normalize_key("foo bar!@#baz"), "foo-bar-baz");
    }

    #[test]
    fn normalize_key_keeps_safe() {
        assert_eq!(normalize_key("foo.bar_baz:qux"), "foo.bar_baz:qux");
    }

    #[test]
    fn normalize_key_trims_dashes() {
        assert_eq!(normalize_key("---foo---"), "foo");
    }

    #[test]
    fn normalize_key_empty_fallback() {
        assert_eq!(normalize_key(""), "tool");
        assert_eq!(normalize_key("!@#$%"), "tool");
    }

    #[test]
    fn normalize_key_truncates() {
        let s = "a".repeat(200);
        let out = normalize_key(&s);
        assert_eq!(out.chars().count(), 160);
    }

    #[test]
    fn connection_uid_format() {
        let uid = connection_uid("ns", "name", "1234567890abcdef");
        assert!(uid.starts_with("ns/name-"));
        assert!(uid.contains("12345678"));
    }

    #[test]
    fn connection_uid_short_id() {
        let uid = connection_uid("NS", "Name", "abc");
        assert_eq!(uid, "ns/name-abc");
    }

    #[test]
    fn number_value_valid() {
        assert_eq!(number_value("3.14"), Some(3.14));
        assert_eq!(number_value("42"), Some(42.0));
        assert_eq!(number_value("-1"), Some(-1.0));
    }

    #[test]
    fn number_value_invalid() {
        assert_eq!(number_value("not a number"), None);
        assert_eq!(number_value(""), None);
    }

    #[test]
    fn percent_basic() {
        assert_eq!(percent(50.0, 100.0), 50.0);
        assert_eq!(percent(1.0, 3.0), 33.3);
        assert_eq!(percent(2.0, 3.0), 66.7);
    }

    #[test]
    fn percent_zero_denominator() {
        assert_eq!(percent(5.0, 0.0), 0.0);
        assert_eq!(percent(5.0, -1.0), 0.0);
    }

    #[test]
    fn percentile_empty() {
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn percentile_50() {
        let v = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&v, 50.0), Some(3.0));
    }

    #[test]
    fn percentile_0_and_100() {
        let v = vec![1.0, 2.0, 3.0];
        assert_eq!(percentile(&v, 0.0), Some(1.0));
        assert_eq!(percentile(&v, 100.0), Some(3.0));
    }

    #[test]
    fn actor_type_round_trip() {
        for t in [ActorType::Agent, ActorType::User, ActorType::System, ActorType::Plugin] {
            assert_eq!(ActorType::from_str(t.as_str()), Some(t));
        }
    }

    #[test]
    fn actor_type_unknown() {
        assert_eq!(ActorType::from_str("unknown"), None);
    }

    #[test]
    fn actor_binding_basic() {
        let b = actor_binding(Some("agent"), Some("a-1"), Some("session-1"));
        assert_eq!(b.actor_type, Some(ActorType::Agent));
        assert_eq!(b.actor_id, Some("a-1".to_string()));
        assert_eq!(b.session_id, Some("session-1".to_string()));
    }

    #[test]
    fn actor_binding_invalid_type() {
        let b = actor_binding(Some("alien"), Some("a"), Some("s"));
        assert_eq!(b.actor_type, None);
        assert_eq!(b.actor_id, Some("a".to_string()));
    }

    #[test]
    fn actor_binding_empty_session() {
        let b = actor_binding(Some("user"), Some("a"), Some("  "));
        assert_eq!(b.session_id, None);
    }

    #[test]
    fn actor_binding_empty_actor_id() {
        let b = actor_binding(Some("user"), Some(""), Some("s"));
        assert_eq!(b.actor_id, None);
    }


    // ---- Round 767: pc-tool::tool_invocation_pure 集成测试 ----

    /// number_value: 解析失败、NaN、Infinity、负数、零、空字符串。
    #[test]
    fn r767_number_value_edges() {
        use super::number_value;
        assert_eq!(number_value("0"), Some(0.0));
        assert_eq!(number_value("-3.14"), Some(-3.14));
        assert_eq!(number_value("  42  "), None, "leading/trailing space not allowed");
        assert_eq!(number_value(""), None);
        assert_eq!(number_value("abc"), None);
        assert_eq!(number_value("nan").map(|f| f.is_nan()), None, "NaN must be filtered out");
        assert_eq!(number_value("inf").map(|f| f.is_finite()), None, "Infinity must be filtered out");
    }

    /// percent: 零分母、负分母、刚好 100、超过 100、精度 1 位小数。
    #[test]
    fn r767_percent_edges() {
        use super::percent;
        assert_eq!(percent(0.0, 100.0), 0.0);
        assert_eq!(percent(100.0, 100.0), 100.0);
        assert_eq!(percent(150.0, 100.0), 150.0);
        assert_eq!(percent(1.0, 3.0), 33.3, "1/3 rounded to 1 decimal");
        assert_eq!(percent(2.0, 3.0), 66.7);
        assert_eq!(percent(5.0, 0.0), 0.0, "zero denominator → 0.0");
        assert_eq!(percent(5.0, -1.0), 0.0, "negative denominator → 0.0");
    }

    /// percentile: 单元素、重复值、p 取极值。
    #[test]
    fn r767_percentile_edges() {
        use super::percentile;
        assert_eq!(percentile(&[42.0], 50.0), Some(42.0));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 0.0), Some(1.0));
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0, 5.0], 100.0), Some(5.0));
        assert_eq!(percentile(&[7.0, 7.0, 7.0], 50.0), Some(7.0));
    }

    /// normalize_key: 全部字符都需要替换 → fallback "_"。
    #[test]
    fn r767_normalize_key_all_invalid() {
        use super::normalize_key;
        assert_eq!(normalize_key("---"), "tool", "empty fallback → tool");
        assert_eq!(normalize_key("!!!@@@"), "tool", "all-invalid fallback → tool");
        assert_eq!(normalize_key("MyTool 1"), "mytool-1");
    }

    /// connection_uid: connection_id 短 / 长，截断到前 8 位。
    #[test]
    fn r767_connection_uid_edges() {
        use super::connection_uid;
        let long = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let r = connection_uid("ns", "name", long);
        assert!(r.starts_with("ns/name-"));
        assert!(r.ends_with("aaaaaaaa"), "truncate to first 8 chars");
        let short = "abc";
        let r2 = connection_uid("ns", "name", short);
        assert!(r2.ends_with("-abc"));
    }

    /// ActorType::as_str: 全部 4 个变体的稳定字符串。
    #[test]
    fn r767_actor_type_all_variants() {
        use super::ActorType;
        assert_eq!(ActorType::User.as_str(), "user");
        assert_eq!(ActorType::Agent.as_str(), "agent");
        assert_eq!(ActorType::Plugin.as_str(), "plugin");
        assert_eq!(ActorType::System.as_str(), "system");
    }
}
