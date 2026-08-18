#![forbid(unsafe_code)]
//! Pure (no sqlx/async) helpers for issue domain.
//! R820: 拆分自 issue.rs, 实现高内聚低耦合.
//! 所有函数纯函数, 不依赖 DB, 可独立单测.

/// Issue 全部可能状态. R820: 从 issue.rs 抽出.
pub const ISSUE_STATUSES: [&str; 7] = [
    "backlog",
    "todo",
    "in_progress",
    "in_review",
    "done",
    "blocked",
    "cancelled",
];

/// 验证 issue 状态字符串是否在合法集合内.
pub fn valid_issue_status(status: &str) -> bool {
    ISSUE_STATUSES.contains(&status)
}

/// 判断一个字节是否是 URL 字符 (alphanumeric + 常见 URL 符号).
pub fn is_url_char(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'.' | b'/' | b'-' | b'_' | b':' | b'%' | b'?' | b'#' | b'=' | b'&' | b'~'
                | b'@' | b'+' | b'!' | b'$' | b'*'
        )
}

/// 生成围绕 `contains` 的 excerpt, 最多 80 chars 前缀 + match + 80 chars 后缀.
pub fn make_excerpt(text: &str, contains: &str, kind: &str) -> Option<String> {
    if contains.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let lower_contains = contains.to_lowercase();
    let bytes = text.as_bytes();
    let lc_bytes = lower.as_bytes();
    let needle = lower_contains.as_bytes();
    if kind == "url" {
        let mut i = 0;
        while i + needle.len() <= lc_bytes.len() {
            if &lc_bytes[i..i + needle.len()] == needle {
                let mut start = i;
                while start > 0 && is_url_char(bytes[start - 1]) {
                    start -= 1;
                }
                let mut end = i + needle.len();
                while end < bytes.len() && is_url_char(bytes[end]) {
                    end += 1;
                }
                let lo = start.saturating_sub(80);
                let hi = (end + 80).min(text.len());
                let mut snippet = text[lo..hi].to_string();
                if lo > 0 {
                    snippet = format!("\u{2026}{snippet}");
                }
                if hi < text.len() {
                    snippet.push('\u{2026}');
                }
                return Some(snippet);
            }
            i += 1;
        }
        None
    } else {
        match lower.find(&lower_contains) {
            Some(idx) => {
                let lo = idx.saturating_sub(80);
                let hi = (idx + contains.len() + 80).min(text.len());
                let mut snippet = text[lo..hi].to_string();
                if lo > 0 {
                    snippet = format!("\u{2026}{snippet}");
                }
                if hi < text.len() {
                    snippet.push('\u{2026}');
                }
                Some(snippet)
            }
            None => None,
        }
    }
}
