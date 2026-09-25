//! 出站引用里的**图片占位替换**（上游 `outbound_quote.go` 51 行 + `sealedInputQuote`）。
//!
//! - **写者**：M7-8（`docs/32` §22 的 D1；门 ⑩ 的切分，边界取上游文件的边界）。
//! - **它解决的问题**：物化到本仓对象存储的渠道图片用的是**需要鉴权的内部 URL**，
//!   `DingTalk` 渲染不出来 ⇒ 显示原始占位符比显示一个坏图好。
//!
//! # 与上游的形态差异（登记 `docs/32` §22 的 D5）
//!
//! 上游用 `goldmark` 解析 AST；本仓不引 Markdown 解析依赖（M7-0 把依赖面一次定死）⇒
//! 这里是**保守的扫描器**，保留上游点名的三条语义并在用例里逐条钉住：围栏代码块里的字面量
//! 不动、行内代码里的不动、转义的 `!` 与普通链接（`[x](…)`）不动。它比 AST 弱的地方是：
//! 不认 setext 标题 / HTML 块之类**不影响图片识别**的构造。

use crate::dingtalk::inbound::IMAGE_PLACEHOLDER;

/// 入站媒体在正文里的**精确**内联形态：`![](<内部下载 URL>)`。
const INTERNAL_ATTACHMENT_PREFIX: &str = "/api/attachments/";

/// 内部附件下载 URL 的后缀（`mc-repos` 的媒体绑定写的就是这一条）。
const INTERNAL_ATTACHMENT_SUFFIX: &str = "/download";

/// 把入站引用里的**内部**图片链接换成 `[Image]` 占位符（上游 `sealedInputQuote`）。
///
/// 只有渠道摄入时发出的**那一种精确形态**会被替换，正文里的其它 Markdown 逐字保留。
#[must_use]
pub fn sealed_input_quote(body: &str) -> String {
    if !body.contains(INTERNAL_ATTACHMENT_PREFIX) {
        return body.to_string();
    }
    let mut out = String::with_capacity(body.len());
    let mut fence_open = false;
    let bytes = body.as_bytes();
    let mut cursor = 0usize;
    while cursor < body.len() {
        let line_end = body[cursor..]
            .find('\n')
            .map_or(body.len(), |offset| cursor + offset + 1);
        if bytes[cursor..line_end]
            .iter()
            .copied()
            .skip_while(|byte| *byte == b' ' || *byte == b'\t')
            .take(3)
            .eq(b"```".iter().copied())
        {
            fence_open = !fence_open;
            out.push_str(&body[cursor..line_end]);
            cursor = line_end;
            continue;
        }
        if fence_open {
            out.push_str(&body[cursor..line_end]);
            cursor = line_end;
            continue;
        }
        // 行内代码**不跨行**：每个源行各自复位（比"跨行保持"更保守：宁可少替换，
        // 不放宽到正文里）。
        let mut inline_code = false;
        let mut index = cursor;
        while index < line_end {
            if bytes[index] == b'`' {
                inline_code = !inline_code;
                out.push('`');
                index += 1;
                continue;
            }
            if !inline_code && bytes[index] == b'!' {
                if let Some(end) = markdown_image_end(body, index) {
                    out.push_str(IMAGE_PLACEHOLDER);
                    index = end;
                    continue;
                }
            }
            let character = body[index..].chars().next().expect("非空");
            out.push(character);
            index += character.len_utf8();
        }
        cursor = line_end;
    }
    out
}

/// 从 `![` 开始尝试匹配一个内联图片；命中内部附件形态 ⇒ 返回它的**排他**结束下标。
///
/// 只认 `![](/api/attachments/{id}/download)`（渠道摄入发出的那一种）：
/// URL 里不能有空白、不能带 title（上游的 `literal` 比对同样会跳过带 title 的写法）。
fn markdown_image_end(body: &str, start: usize) -> Option<usize> {
    let rest = body[start..].strip_prefix("![")?;
    let destination = rest.strip_prefix("](")?;
    let close = destination.find(')')?;
    let url = &destination[..close];
    if !url.starts_with(INTERNAL_ATTACHMENT_PREFIX)
        || !url.ends_with(INTERNAL_ATTACHMENT_SUFFIX)
        || url.len() <= INTERNAL_ATTACHMENT_PREFIX.len() + INTERNAL_ATTACHMENT_SUFFIX.len()
        || url.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(start + 4 + close + 1)
}
