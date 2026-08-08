#![forbid(unsafe_code)]

//! Claude prompt 段落拼接 + 指标（对齐 Node `execute.ts` L789-828）。
//!
//! Node 端把多个 markdown 段拼接成最终 prompt：
//!   `[bootstrap, wake, handoff, taskContext, rendered]`
//! 每段都是可选的（可能为空字符串），最终用 `\n\n` 连接并去尾部空白。
//!
//! 同时输出 `promptMetrics`，记录每段字符数（用于 telemetry/debug）。
//!
//! 本模块只处理**字符串拼接 + 度量**，不做模板渲染（模板由
//! `pc-acpx::prompt_compose` 或 `pc-acpx::render_paperclip_wake_prompt` 提供）。
//!
//! 对齐 Node L810-826。

/// 各个段落的字符数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptMetrics {
    pub prompt_chars: usize,
    pub bootstrap_prompt_chars: usize,
    pub wake_prompt_chars: usize,
    pub session_handoff_chars: usize,
    pub task_context_chars: usize,
    pub heartbeat_prompt_chars: usize,
}

/// 所有 prompt 段落（顺序与 Node `joinPromptSections` 一致）。
#[derive(Debug, Clone, Default)]
pub struct PromptSections {
    /// 启动提示（首次 fresh session 才注入）
    pub bootstrap_prompt: String,
    /// wake delta 提示
    pub wake_prompt: String,
    /// session handoff 备注
    pub session_handoff_note: String,
    /// task context markdown
    pub task_context_note: String,
    /// 渲染后的任务心跳 prompt
    pub heartbeat_prompt: String,
}

impl PromptSections {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bootstrap_prompt: String::new(),
            wake_prompt: String::new(),
            session_handoff_note: String::new(),
            task_context_note: String::new(),
            heartbeat_prompt: String::new(),
        }
    }

    /// 把所有段落按 Node `joinPromptSections` 的顺序拼接。
    /// 空段被跳过，段间用 `\n\n` 分隔，最后 trim 尾部空白。
    #[must_use]
    pub fn join(&self) -> String {
        join_prompt_sections(&[
            self.bootstrap_prompt.as_str(),
            self.wake_prompt.as_str(),
            self.session_handoff_note.as_str(),
            self.task_context_note.as_str(),
            self.heartbeat_prompt.as_str(),
        ])
    }

    /// 计算所有指标。
    #[must_use]
    pub fn metrics(&self) -> PromptMetrics {
        build_prompt_metrics(self)
    }
}

/// 把 5 个段落拼接为最终 prompt，对齐 Node `joinPromptSections`。
///
/// 规则：
/// 1. 跳过空字符串段（trim 后为空视为空）
/// 2. 非空段间用 `\n\n` 连接
/// 3. 最终结果 trim 尾部空白
#[must_use]
pub fn join_prompt_sections(sections: &[&str]) -> String {
    let mut buf = String::new();
    let mut first = true;
    for section in sections {
        let trimmed = section.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !first {
            buf.push_str("\n\n");
        }
        buf.push_str(trimmed);
        first = false;
    }
    buf
}

/// 计算各段落字符数 + 最终 prompt 字符数。
#[must_use]
pub fn build_prompt_metrics(sections: &PromptSections) -> PromptMetrics {
    let prompt = sections.join();
    PromptMetrics {
        prompt_chars: prompt.chars().count(),
        bootstrap_prompt_chars: sections.bootstrap_prompt.chars().count(),
        wake_prompt_chars: sections.wake_prompt.chars().count(),
        session_handoff_chars: sections.session_handoff_note.chars().count(),
        task_context_chars: sections.task_context_note.chars().count(),
        heartbeat_prompt_chars: sections.heartbeat_prompt.chars().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_empty_sections_returns_empty() {
        assert_eq!(join_prompt_sections(&[]), "");
    }

    #[test]
    fn join_single_section() {
        assert_eq!(join_prompt_sections(&["hello"]), "hello");
    }

    #[test]
    fn join_two_sections_with_double_newline() {
        assert_eq!(join_prompt_sections(&["hello", "world"]), "hello\n\nworld");
    }

    #[test]
    fn join_skips_empty_sections() {
        assert_eq!(join_prompt_sections(&["hello", "", "world"]), "hello\n\nworld");
    }

    #[test]
    fn join_skips_whitespace_only_sections() {
        assert_eq!(join_prompt_sections(&["hello", "   ", "world"]), "hello\n\nworld");
    }

    #[test]
    fn join_preserves_inner_whitespace() {
        assert_eq!(
            join_prompt_sections(&["line1\nline2", "line3"]),
            "line1\nline2\n\nline3"
        );
    }

    #[test]
    fn join_trims_trailing_whitespace() {
        assert_eq!(join_prompt_sections(&["hello   ", "world  "]), "hello\n\nworld");
    }

    #[test]
    fn join_does_not_trim_section_content() {
        // 内层段落不会 trim 内容；只 trim 段首段尾的空白
        assert_eq!(
            join_prompt_sections(&["  hello  ", "  world  "]),
            "hello\n\nworld"
        );
    }

    #[test]
    fn join_all_empty_returns_empty() {
        assert_eq!(join_prompt_sections(&["", "   ", ""]), "");
    }

    #[test]
    fn join_leading_and_trailing_section_whitespace_trimmed() {
        // 第一个非空段也会被 trim，所以首尾空白消失
        assert_eq!(join_prompt_sections(&["\n\nhello\n\n"]), "hello");
    }

    #[test]
    fn build_metrics_counts_chars_per_section() {
        let mut s = PromptSections::new();
        s.bootstrap_prompt = "abc".to_owned();
        s.wake_prompt = "defgh".to_owned();
        s.session_handoff_note = "i".to_owned();
        s.task_context_note = "".to_owned();
        s.heartbeat_prompt = "jklmnop".to_owned();
        let m = build_prompt_metrics(&s);
        assert_eq!(m.bootstrap_prompt_chars, 3);
        assert_eq!(m.wake_prompt_chars, 5);
        assert_eq!(m.session_handoff_chars, 1);
        assert_eq!(m.task_context_chars, 0);
        assert_eq!(m.heartbeat_prompt_chars, 7);
        // 拼接后应该是 3+5+1+0+7 = 16 + 3 个 \n\n 分隔符 = 19
        // 但心跳段是空，按 join 的规则会跳过
        // 实际是 "abc\n\ndefgh\n\ni\n\njklmnop" = 3+2+5+2+1+2+7 = 22
        assert_eq!(m.prompt_chars, 22);
    }

    #[test]
    fn build_metrics_excludes_empty_sections() {
        let s = PromptSections::new();
        let m = build_prompt_metrics(&s);
        assert_eq!(m.prompt_chars, 0);
        assert_eq!(m.bootstrap_prompt_chars, 0);
    }

    #[test]
    fn sections_helper_join_and_metrics() {
        let mut s = PromptSections::new();
        s.bootstrap_prompt = "## Bootstrap".to_owned();
        s.heartbeat_prompt = "## Heartbeat".to_owned();
        assert_eq!(s.join(), "## Bootstrap

## Heartbeat");
        let m = s.metrics();
        assert_eq!(m.prompt_chars, 12 + 2 + 12);
    }

    #[test]
    fn sections_helper_skips_blank_sections_in_join() {
        let mut s = PromptSections::new();
        s.bootstrap_prompt = "boot".to_owned();
        s.wake_prompt = "wake".to_owned();
        s.session_handoff_note = "".to_owned();
        s.task_context_note = "task".to_owned();
        s.heartbeat_prompt = "".to_owned();
        assert_eq!(s.join(), "boot\n\nwake\n\ntask");
    }
}
