//! pi 文本增量的消毒：控制 token、结构化工具标记、以及"半个 token 不能提前吐"。
//!
//! 逐函数对齐上游 `server/pkg/agent/pi.go`（L42-320）：
//!
//! | 本文件 | 上游 |
//! |---|---|
//! | [`strip_control_tokens`] | `piControlTokenRE.ReplaceAllString` |
//! | [`strip_structured_tool_markup`] | `stripPiStructuredToolMarkup` |
//! | [`drain_sanitized`] | `drainPiSanitizedText` |
//! | [`safe_emit_len`] | `safePiTextEmitLen` |
//! | [`TextDrain::push`] / [`TextDrain::flush`] | `drainPiTextBuffer` / `flushPiTextBuffer` |
//!
//! 为什么不用 `regex`：正则只有两种形态（控制 token），且都要和"字节级回退扫描"
//! 配合；手写扫描少一个依赖，行为可以逐字节对照上游。**注意**这里刻意复刻了上游
//! RE 的贪婪语义 —— 控制 token 后紧跟的词字符会被一起吞掉（见测试注释）。
//!
//! 「半个 token 不能提前吐」是这个模块存在的唯一理由：pi 的 `text_delta` 会从任意
//! 位置切开，`<|end_of_t` 这种半截字符串一旦进了用户可见文本就再也收不回来了。

/// 控制 token 的两条形态（上游 `piControlTokenRE`）：
/// `<\|[A-Za-z0-9_-]+>[A-Za-z0-9_-]*` 与 `<[A-Za-z0-9_-]+\|>`。
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_tool_name_byte(b: u8) -> bool {
    is_name_byte(b)
}

/// `bytes[at..]` 处若是一个完整控制 token，返回其字节长度（否则 0）。
fn control_token_len(bytes: &[u8], at: usize) -> usize {
    let rest = &bytes[at..];
    // 形态 A：<| NAME+ > NAME*
    if rest.starts_with(b"<|") {
        let mut i = 2;
        let name_start = i;
        while i < rest.len() && is_name_byte(rest[i]) {
            i += 1;
        }
        if i > name_start && rest.get(i) == Some(&b'>') {
            let mut j = i + 1;
            while j < rest.len() && is_name_byte(rest[j]) {
                j += 1;
            }
            return j;
        }
    }
    // 形态 B：< NAME+ |>
    if rest.first() == Some(&b'<') {
        let mut i = 1;
        let name_start = i;
        while i < rest.len() && is_name_byte(rest[i]) {
            i += 1;
        }
        if i > name_start && rest.get(i) == Some(&b'|') && rest.get(i + 1) == Some(&b'>') {
            return i + 2;
        }
    }
    0
}

/// 去掉输入里所有完整控制 token。
pub fn strip_control_tokens(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let len = control_token_len(bytes, i);
            if len > 0 {
                i += len;
                continue;
            }
        }
        // token 的字符集是纯 ASCII，命中边界必然落在字符边界上；
        // 未命中时按字符推进，避免切碎多字节字符。
        let ch = input[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 最靠前的结构化工具标记前缀（`call:` / `response:`）及其长度。
fn next_markup_prefix(input: &str, from: usize) -> Option<(usize, usize)> {
    ["call:", "response:"]
        .iter()
        .filter_map(|prefix| {
            input[from..]
                .find(prefix)
                .map(|rel| (from + rel, prefix.len()))
        })
        .min_by_key(|(abs, _)| *abs)
}

/// 从 `NAME{...}` 的 `{` 位置起，扫到一个配平且闭合的 JSON 块结尾（可带
/// `<tool_call|>` 尾巴），返回结束位置。
fn scan_markup_end(input: &str, mut i: usize) -> Option<usize> {
    const QUOTE_MARKER: &str = "<|\"|>";
    const END_MARKER: &str = "<tool_call|>";

    let bytes = input.as_bytes();
    let name_start = i;
    while i < bytes.len() && is_tool_name_byte(bytes[i]) {
        i += 1;
    }
    if i == name_start || bytes.get(i) != Some(&b'{') {
        return None;
    }

    let mut depth: i32 = 0;
    let mut in_quote = false;
    while i < bytes.len() {
        if input[i..].starts_with(QUOTE_MARKER) {
            in_quote = !in_quote;
            i += QUOTE_MARKER.len();
            continue;
        }
        if !in_quote {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth <= 0 {
                        i += 1;
                        if input[i..].starts_with(END_MARKER) {
                            i += END_MARKER.len();
                        }
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// 去掉完整的结构化工具标记块（`call:name{...}<tool_call|>` 整段）。
pub fn strip_structured_tool_markup(input: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    loop {
        match next_markup_prefix(input, i) {
            None => {
                out.push_str(&input[i..]);
                return out;
            }
            Some((start, prefix_len)) => {
                out.push_str(&input[i..start]);
                if let Some(end) = scan_markup_end(input, start + prefix_len) {
                    i = end;
                } else {
                    out.push_str(&input[start..]);
                    return out;
                }
            }
        }
    }
}

/// 以 `<` 开头、且只由 token 字符集组成的短串（可能是被切开的控制 token）。
///
/// 注意 `<` 只允许出现在第 0 位（与上游一致）：`<<|` 不算 token 前缀。
fn looks_like_control_token_prefix(input: &str) -> bool {
    if !input.starts_with('<') || input.len() > 64 {
        return false;
    }
    input
        .bytes()
        .skip(1)
        .all(|b| is_name_byte(b) || b == b'|' || b == b'>')
}

/// `input` 里可以安全吐出的字节长度 —— 末尾可能是被切开的 `call:` / `response:` /
/// 控制 token 的部分要留在缓冲里等下一段增量。
pub fn safe_emit_len(input: &str) -> usize {
    let mut hold = 0;
    for prefix in ["call:", "response:"] {
        for n in 1..prefix.len() {
            if n <= input.len() && input.ends_with(&prefix[..n]) && n > hold {
                hold = n;
            }
        }
    }
    if let Some(i) = input.rfind('<') {
        if looks_like_control_token_prefix(&input[i..]) {
            let pending = input.len() - i;
            if pending > hold {
                hold = pending;
            }
        }
    }
    input.len() - hold
}

/// 取一段可吐出的文本 + 需要留在缓冲里的尾巴（上游 `drainPiSanitizedText`）。
fn drain_sanitized(input: &str) -> (String, String) {
    let mut out = String::new();
    let mut i = 0;
    loop {
        let Some((start, prefix_len)) = next_markup_prefix(input, i) else {
            let safe = safe_emit_len(&input[i..]);
            out.push_str(&input[i..i + safe]);
            return (strip_control_tokens(&out), input[i + safe..].to_owned());
        };
        out.push_str(&input[i..start]);
        match scan_markup_end(input, start + prefix_len) {
            Some(end) => i = end,
            None => return (strip_control_tokens(&out), input[start..].to_owned()),
        }
    }
}

/// 跨 `text_delta` 的文本消毒缓冲。
#[derive(Debug, Default)]
pub struct TextDrain {
    buffered: String,
}

impl TextDrain {
    /// 空缓冲。
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加一段增量，返回本次**可以安全吐出**的文本（可能为空）。
    pub fn push(&mut self, delta: &str) -> String {
        self.buffered.push_str(delta);
        let (emit, pending) = drain_sanitized(&self.buffered);
        self.buffered.clear();
        self.buffered.push_str(&pending);
        emit
    }

    /// stdout 关闭后强制吐出剩余（此时不再保留"半个 token"，直接按控制 token 清一遍）。
    pub fn flush(&mut self) -> String {
        let pending = std::mem::take(&mut self.buffered);
        let (mut emit, rest) = drain_sanitized(&pending);
        emit.push_str(&strip_control_tokens(&rest));
        emit
    }

    /// 清空（`turn_start` 时上游会 reset）。
    pub fn reset(&mut self) {
        self.buffered.clear();
    }

    /// 当前还压着多少字节（测试断言用）。
    pub fn buffered_len(&self) -> usize {
        self.buffered.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_tokens_are_stripped_both_shapes() {
        // 形态 A：`<|name>`。尾部名字字符会被贪婪吞掉 —— 上游 RE 就是这样，上游自己的
        // 用例正是 `before <|turn>model after` → `before  after`（pi_test.go）。
        // 真实 pi 的控制 token 后面跟的是空白/标点，所以正文不受影响。
        assert_eq!(
            strip_control_tokens("before <|turn>model after"),
            "before  after"
        );
        assert_eq!(strip_control_tokens("hello <|end_of_turn>!"), "hello !");
        assert_eq!(strip_control_tokens("a<|x>b<|y>c"), "a");
        // 形态 B：`<name|>`
        assert_eq!(strip_control_tokens("tool <tool_call|> done"), "tool  done");
        assert_eq!(strip_control_tokens("<tool_call|>"), "");
        // 不完整 / 不合法：原样保留。
        assert_eq!(strip_control_tokens("a <| b"), "a <| b");
        assert_eq!(strip_control_tokens("a <|> b"), "a <|> b");
        assert_eq!(strip_control_tokens("2 < 3"), "2 < 3");
        // `<|name|>` 两条形态都不匹配 —— 这是上游 RE 的真实边界（不是我们的取舍），
        // 两条形态分别对应 `<|name>` 与 `<name|>`。
        assert_eq!(
            strip_control_tokens("hello <|end_of_turn|>world"),
            "hello <|end_of_turn|>world"
        );
        // 多字节字符不被切碎（token 字符集是纯 ASCII）。
        assert_eq!(strip_control_tokens("中文<|x|>测试"), "中文<|x|>测试");
    }

    #[test]
    fn structured_tool_markup_is_removed() {
        let raw = r#"call:read{<|"|>path<|"|>}<tool_call|>rest"#;
        assert_eq!(strip_structured_tool_markup(raw), "rest");
        let nested = r#"pre response:write{<|"|>a{b}<|"|>}<tool_call|>post"#;
        assert_eq!(strip_structured_tool_markup(nested), "pre post");
        // 未闭合 → 从标记起点整段保留（等待后续增量）。
        let partial = r#"call:read{<|"|>path"#;
        assert_eq!(strip_structured_tool_markup(partial), partial);
        assert_eq!(strip_structured_tool_markup("no markup"), "no markup");
    }

    #[test]
    fn safe_emit_len_holds_back_partial_markers() {
        assert_eq!(safe_emit_len("hello"), 5);
        assert_eq!(safe_emit_len("hello cal"), 6); // 压住 "cal"
        assert_eq!(safe_emit_len("hello response"), 6);
        assert_eq!(safe_emit_len("hi <|end"), 3); // 压住 "<|end"
                                                  // 完整的 `call:` 不归本函数管（上游循环上限是 `len(prefix)-1`）：
                                                  // 它由 `next_markup_prefix` 那条路留下当 pending，见 `drain_*` 用例。
        assert_eq!(safe_emit_len("hello call:"), 11);
        assert_eq!(safe_emit_len("hi <"), 3); // 只压住 "<"（1 字节）
                                              // 超过 64 字节的"像 token 的前缀"不再压（与上游一致）。
        let long = format!("<|{}", "a".repeat(64));
        assert_eq!(safe_emit_len(&long), long.len());
    }

    #[test]
    fn drain_across_delta_boundaries_never_leaks_half_token() {
        // 上游 pi_test.go 的原用例：`before <|tu` + `rn>model after`。
        let mut drain = TextDrain::new();
        assert_eq!(drain.push("before <|tu"), "before ");
        assert_eq!(drain.push("rn>model after"), " afte"); // 结尾的 "r" 被当成 "response:" 的可能前缀压住
        assert_eq!(drain.push("!"), "r!"); // 下一段增量把压住的字符吐出来
        assert_eq!(drain.push("all done"), "all done");
        assert_eq!(drain.buffered_len(), 0);
    }

    #[test]
    fn drain_keeps_partial_markup_until_closed() {
        let mut drain = TextDrain::new();
        assert_eq!(drain.push("pre call:read{<|\"|>"), "pre ");
        assert!(drain.buffered_len() > 0);
        assert_eq!(
            drain.push("path<|\"|>}<tool_call|>post"),
            "post",
            "标记块整段吃掉，正文只在闭合后吐"
        );
        assert_eq!(drain.buffered_len(), 0);
    }

    #[test]
    fn flush_forces_pending_out() {
        let mut drain = TextDrain::new();
        assert_eq!(drain.push("tail <|e"), "tail ");
        assert_eq!(drain.buffered_len(), 3);
        // flush 不再保留"半个 token"：按控制 token 清一遍后原样吐出。
        assert_eq!(drain.flush(), "<|e");
        assert_eq!(drain.buffered_len(), 0);
        // reset 清缓冲。
        assert_eq!(drain.push("x call:"), "x ");
        drain.reset();
        assert_eq!(drain.buffered_len(), 0);
        assert_eq!(drain.flush(), "");
    }
}
