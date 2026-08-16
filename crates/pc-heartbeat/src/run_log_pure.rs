#![forbid(unsafe_code)]

//! Run log compaction pure helpers — 1:1 port of
//! paperclip/server/src/services/heartbeat.ts#compactRunLogChunk.
//!
//! R730: 压缩持久化的 run log chunk，避免超长日志撑爆 DB row。

use once_cell::sync::Lazy;
use pc_secret_redaction::redact_sensitive_text;
use regex::Regex;

/// 默认最大 chunk 字符数（对齐 Node MAX_PERSISTED_LOG_CHUNK_CHARS=20000）。
pub const DEFAULT_MAX_PERSISTED_LOG_CHUNK_CHARS: usize = 20_000;

/// 默认 head 字符占比（0.6 = 60%）。
pub const DEFAULT_HEAD_FRACTION: f64 = 0.6;

/// 默认 tail 字符占比（0.25 = 25%）。
pub const DEFAULT_TAIL_FRACTION: f64 = 0.25;

/// inline base64 图片数据 regex（对齐 Node INLINE_BASE64_IMAGE_DATA_RE）。
///
/// 匹配形如：
///   "type":"image","source":{"type":"base64","data":"<very long b64>"}
/// 的图片数据字符串，将其替换为「[omitted base64 image data: N chars]」。
static INLINE_BASE64_IMAGE_DATA_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"("type":"image","source":\{"type":"base64","data":")([A-Za-z0-9+/=]{1024,})(")"#,
    )
    .expect("INLINE_BASE64_IMAGE_DATA_RE")
});

/// 把 inline base64 图片数据 redact 成省略标记。
pub fn redact_inline_base64_image_data(chunk: &str) -> String {
    INLINE_BASE64_IMAGE_DATA_RE
        .replace_all(chunk, |caps: &regex::Captures<'_>| {
            let prefix = &caps[1];
            let data = &caps[2];
            let suffix = &caps[3];
            format!("{prefix}[omitted base64 image data: {} chars]{suffix}", data.len())
        })
        .into_owned()
}

/// 计算 head/tail/omitted 三段 split 索引。
///
/// 返回 (head_chars, tail_chars, marker)。
pub fn plan_compaction(
    text_len: usize,
    max_chars: usize,
    head_fraction: f64,
    tail_fraction: f64,
) -> CompactionPlan {
    let head_chars = (max_chars as f64 * head_fraction).floor() as usize;
    let tail_chars = (max_chars as f64 * tail_fraction).floor() as usize;
    let omitted_chars = text_len.saturating_sub(head_chars + tail_chars);
    let marker = format!(
        "
[paperclip truncated run log chunk: omitted {omitted_chars} chars]
"
    );
    CompactionPlan {
        head_chars,
        tail_chars,
        marker,
    }
}

/// 压缩 run log chunk。
///
/// Pipeline（对齐 Node）：
///   1. redact_inline_base64_image_data
///   2. redact_sensitive_text
///   3. 若超过 maxChars → head(60%) + marker + tail(25%)
///   4. 否则原样返回
pub fn compact_run_log_chunk(
    chunk: &str,
    max_chars: usize,
    head_fraction: f64,
    tail_fraction: f64,
) -> String {
    let normalized = redact_sensitive_text(&redact_inline_base64_image_data(chunk));
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let plan = plan_compaction(normalized.chars().count(), max_chars, head_fraction, tail_fraction);
    let head: String = normalized.chars().take(plan.head_chars).collect();
    let tail: String = normalized
        .chars()
        .rev()
        .take(plan.tail_chars)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}{}{tail}", plan.marker)
}

/// 简化签名：使用 DEFAULT_* 常量。
pub fn compact_run_log_chunk_default(chunk: &str) -> String {
    compact_run_log_chunk(
        chunk,
        DEFAULT_MAX_PERSISTED_LOG_CHUNK_CHARS,
        DEFAULT_HEAD_FRACTION,
        DEFAULT_TAIL_FRACTION,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPlan {
    pub head_chars: usize,
    pub tail_chars: usize,
    pub marker: String,
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn compact_passthrough_when_short() {
        let s = "hello world";
        assert_eq!(compact_run_log_chunk_default(s), "hello world");
    }

    #[test]
    fn compact_truncates_long_input() {
        let long = "a".repeat(40_000);
        let out = compact_run_log_chunk_default(&long);
        assert!(out.len() < long.len());
        assert!(out.contains("paperclip truncated run log chunk"));
    }

    #[test]
    fn compact_preserves_head_and_tail() {
        let mut long = String::new();
        for _ in 0..30_000 {
            long.push('H');
        }
        for _ in 0..5_000 {
            long.push('T');
        }
        let out = compact_run_log_chunk_default(&long);
        assert!(out.starts_with('H'));
        assert!(out.ends_with('T'));
    }

    #[test]
    fn redact_base64_replaces_long_data() {
        let b64 = "A".repeat(2000);
        let chunk = format!(
            r#""type":"image","source":{{"type":"base64","data":"{b64}"}}"#
        );
        let out = redact_inline_base64_image_data(&chunk);
        assert!(out.contains("[omitted base64 image data: 2000 chars]"));
    }

    #[test]
    fn redact_base64_keeps_short_data() {
        // < 1024 chars 不匹配
        let b64 = "A".repeat(100);
        let chunk = format!(
            r#""type":"image","source":{{"type":"base64","data":"{b64}"}}"#
        );
        let out = redact_inline_base64_image_data(&chunk);
        assert_eq!(out, chunk);
    }

    #[test]
    fn plan_compaction_marker_includes_omitted() {
        let plan = plan_compaction(40_000, 10_000, 0.6, 0.25);
        assert_eq!(plan.head_chars, 6_000);
        assert_eq!(plan.tail_chars, 2_500);
        assert!(plan.marker.contains("omitted 31500 chars"));
    }

    #[test]
    fn compact_handles_unicode_codepoints() {
        // 每个 emoji 算 1 codepoint
        let s: String = "🚀".repeat(25_000);
        let out = compact_run_log_chunk_default(&s);
        assert!(out.len() < s.len());
    }
}
