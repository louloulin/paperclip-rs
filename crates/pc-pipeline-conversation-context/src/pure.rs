#![forbid(unsafe_code)]
//! Pure helpers for pipeline conversation body document context.
//!
//! R781 extracted from lib.rs to separate pure logic from DB-touching code.
//! No sqlx / no async - safe to unit test in isolation.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncateWithFlag {
    pub value: String,
    pub truncated: bool,
}

pub fn truncate_with_flag(value: &str, max_chars: usize) -> TruncateWithFlag {
    if value.chars().count() <= max_chars {
        TruncateWithFlag {
            value: value.to_string(),
            truncated: false,
        }
    } else {
        TruncateWithFlag {
            value: value.chars().take(max_chars).collect(),
            truncated: true,
        }
    }
}

/// Wrap a value in a markdown fence whose length is greater than any
/// run of backticks in the value (so the fence cannot be closed by content).
pub fn fence_markdown(value: &str, info: &str) -> String {
    let mut longest_backtick_run = 2usize;
    let mut current = 0usize;
    for c in value.chars() {
        if c == '`' {
            current += 1;
            if current > longest_backtick_run {
                longest_backtick_run = current;
            }
        } else {
            current = 0;
        }
    }
    let fence = "`".repeat(longest_backtick_run + 1);
    format!("{fence}{info}\n{value}\n{fence}")
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn r781_truncate_short_text_unchanged() {
        let r = truncate_with_flag("hello", 100);
        assert_eq!(r.value, "hello");
        assert!(!r.truncated);
    }

    #[test]
    fn r781_truncate_long_text_clipped() {
        let r = truncate_with_flag(&"a".repeat(2_000), 50);
        assert_eq!(r.value.chars().count(), 50);
        assert!(r.truncated);
    }

    #[test]
    fn r781_truncate_empty_returns_empty_not_truncated() {
        let r = truncate_with_flag("", 100);
        assert_eq!(r.value, "");
        assert!(!r.truncated);
    }

    #[test]
    fn r781_truncate_at_exact_boundary_not_truncated() {
        let r = truncate_with_flag("hello", 5);
        assert_eq!(r.value, "hello");
        assert!(!r.truncated);
    }

    #[test]
    fn r781_fence_no_backticks_uses_3() {
        let s = fence_markdown("hello", "markdown");
        assert!(s.starts_with("```markdown\n"));
        assert!(s.ends_with("\n```"));
    }

    #[test]
    fn r781_fence_handles_long_backtick_runs() {
        let v = "```python\nprint(1)\n``````";
        let s = fence_markdown(v, "markdown");
        // v contains a run of 6 backticks, fence must be longer (>= 7)
        assert!(s.starts_with("```````markdown\n"));
    }

    #[test]
    fn r781_fence_picks_longest_run() {
        let v = "no ` then ````` 5 end";
        let s = fence_markdown(v, "text");
        // longest run in v is 5, fence = 6 backticks
        assert!(s.starts_with("``````text\n"));
    }
}
