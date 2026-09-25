//! `Content-Disposition` 的解析与清洗（上游靠 `mime.ParseMediaType` + 三个自写函数的那一半）。
//!
//! - **写者**：M7-18。门 ⑩ 的切分边界取上游那个"面"的边界（`media_download.go` 的
//!   `mediaFilenameFromDisposition` / `hasExtendedFilename` / `decodeFormEncodedFilename` /
//!   `sameFormEncoding` / `cleanMediaFilename` / `stripControlRunes`）。
//! - 🔴 **`mime.ParseMediaType` 是本仓手写的**（登记 `docs/32` §35 的 D8）：`mime` crate 不在
//!   `mc-channel` 的依赖边里（M7-0 冻结），而这条路线上**没有任何**别的办法拿到"响应头里那个
//!   名字"。手写的范围与**收缩**逐条写在 [`parse_disposition`] 上。
//!
//! # 顺序即安全（上游注释的业务核心）
//!
//! 1. **`filename*`（RFC 5987 扩展形态）优先于它旁边那个朴素 `filename=`**：两个都发的服务器
//!    把真实（非 ASCII）名字放在扩展形态里、把一个被压扁的 ASCII 近似放在朴素形态里 ⇒ 取朴素
//!    那个会把每个中文附件都改名成下划线。上游依赖 `mime.ParseMediaType` 无条件做这个偏好，
//!    **与两者出现的先后无关**；本仓的解析器同样如此，并有用例钉住（这是一个我们**依赖**的
//!    性质，不是我们实现的性质）。
//! 2. **表单解码在取基名之前**（[`decode_form_encoded_filename`] 在
//!    [`clean_media_filename`] 之前）：一个被转义的分隔符（`..%2F..%2Fetc%2Fpasswd`）**解码之后
//!    才**是一条路径，而在它之前跑的清洗器会把一次穿越直接放过去。
//! 3. **控制字符在最后剥掉**：头是**远程输入**，而上面那条解码把 `%00` 还原成真正的 NUL、
//!    把 `%0D%0A` 还原成真正的 CRLF。

use std::fmt::Write as _;

/// 上游 `mediaFilenameFromDisposition`：从 `Content-Disposition` 里读出展示名。
///
/// 空头或解析不了的头 ⇒ 空串（上游完全相同：`mime.ParseMediaType` 出错就返回空）。
#[must_use]
pub fn media_filename_from_disposition(raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }
    let Some(parsed) = parse_disposition(raw) else {
        return String::new();
    };
    let mut name = parsed.filename;
    if !parsed.extended {
        name = decode_form_encoded_filename(&name);
    }
    clean_media_filename(&name)
}

/// 解析出来的那一半。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedDisposition {
    filename: String,
    /// 这个名字是从 `filename*`（RFC 5987）来的吗 —— 决定要不要做表单解码。
    extended: bool,
}

/// 头解析的**收缩范围**（登记 `docs/32` §35 的 D8）。
///
/// 支持：`type; name=token; name="quoted"; name*=charset'lang'<percent-encoded>`、引号内的
/// `\X` 转义、参数名大小写不敏感、扩展形态**无条件优先**（不论出现顺序）。
///
/// **不支持**（`mime.ParseMediaType` 支持，而这里没有实现 ⇒ 结果是回退到朴素 `filename=`）：
///
/// - **RFC 2231 的参数续行**（`filename*0*=…; filename*1*=…`）：COS 不发这种形态，而实现它要
///   在参数收集之后再拼一遍；一条续行的名字因此**只**走朴素形态（可能带百分号转义）；
/// - **`charset` 不是 `utf-8` / `us-ascii`**：Go 会报错（于是整个头不解析），本仓**跳过那一个
///   参数**并继续用同一条头里的其它参数 —— 比 Go 宽松一格。宽松的方向是安全的：名字随后要过
///   [`clean_media_filename`]，而**没有**任何一个分支会把远端字节变成一条路径。
fn parse_disposition(raw: &str) -> Option<ParsedDisposition> {
    let (_, parameters) = split_parameters(raw)?;
    let mut plain: Option<String> = None;
    let mut extended: Option<String> = None;
    for (name, value) in parameters {
        let lowered = name.to_ascii_lowercase();
        if lowered == "filename*" {
            if let Some(decoded) = decode_rfc5987(&value) {
                extended = Some(decoded);
            }
        } else if lowered == "filename" && plain.is_none() {
            plain = Some(unquote(&value));
        }
    }
    // 扩展形态**无条件**优先（上游依赖 `ParseMediaType` 的同一性质）。
    match (extended, plain) {
        (Some(extended), _) => Some(ParsedDisposition {
            filename: extended,
            extended: true,
        }),
        (None, Some(plain)) => Some(ParsedDisposition {
            filename: plain,
            extended: false,
        }),
        (None, None) => Some(ParsedDisposition {
            filename: String::new(),
            extended: false,
        }),
    }
}

/// 把头按 `;` 切开，跳过第一个 `type` 段，再逐段拆 `name=value`（认引号内的分号与转义）。
fn split_parameters(raw: &str) -> Option<(String, Vec<(String, String)>)> {
    let segments = split_top_level(raw);
    let mut iterator = segments.into_iter();
    let kind = iterator.next()?;
    let mut parameters = Vec::new();
    for segment in iterator {
        let Some((name, value)) = split_assignment(&segment) else {
            // 一个不成形的参数（上游的 `ParseMediaType` 在这里报错 ⇒ 整个头不解析）。
            return None;
        };
        parameters.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    Some((kind.trim().to_owned(), parameters))
}

/// 顶层按 `;` 切（引号内的 `;` 不算分隔符，`\X` 也不结束引号）。
fn split_top_level(raw: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in raw.chars() {
        if escaped {
            current.push(character);
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => {
                current.push(character);
                escaped = true;
            }
            '"' => {
                quoted = !quoted;
                current.push(character);
            }
            ';' if !quoted => {
                segments.push(current.clone());
                current.clear();
            }
            other => current.push(other),
        }
    }
    segments.push(current);
    segments
}

/// `name=value` 的第一处 `=`（拆不出来 ⇒ `None`）。
fn split_assignment(segment: &str) -> Option<(String, String)> {
    let (name, value) = segment.split_once('=')?;
    if name.trim().is_empty() {
        return None;
    }
    Some((name.to_owned(), value.to_owned()))
}

/// 剥掉一对可选的引号，并还原引号内的 `\X` 转义（上游 `ParseMediaType` 的引号处理）。
fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let Some(inner) = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return trimmed.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut escaped = false;
    for character in inner.chars() {
        if escaped {
            out.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            out.push(character);
        }
    }
    out
}

/// RFC 5987 的 `charset'lang'value`：只认 `utf-8` / `us-ascii`（其余 ⇒ `None`，见收缩清单）。
fn decode_rfc5987(value: &str) -> Option<String> {
    let unquoted = unquote(value);
    let mut parts = unquoted.splitn(3, '\'');
    let charset = parts.next()?;
    let _language = parts.next()?;
    let encoded = parts.next()?;
    let charset = charset.trim().to_ascii_lowercase();
    if charset != "utf-8" && charset != "us-ascii" {
        return None;
    }
    percent_decode(encoded)
}

/// 百分号解码（扩展形态用；不解 `+` —— 那是**表单**编码的事，见 [`decode_form_encoded_filename`]）。
fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).ok()
}

/// 上游 `hasExtendedFilename`：这条头**到底**有没有给出 RFC 5987 的 `filename*`。
///
/// `mime.ParseMediaType` 把扩展参数折进 `params["filename"]` 而不说它来自哪种形态，所以原头是
/// 唯一还能问的地方。一个**值**里含 "filename*" 字样的朴素文件名会在这里读成误报 —— 那没有
/// 代价：唯一的后果是它被原样保留。
#[must_use]
pub fn has_extended_filename(raw: &str) -> bool {
    raw.to_ascii_lowercase().contains("filename*")
}

/// 上游 `decodeFormEncodedFilename`：对**可证明**带表单编码的文件名做 `application/x-www-form-urlencoded`
/// 的反解，其余名字逐字节保留。
///
/// 为什么需要：一个真实租户发过 `PC D&T Strategy 2026.docx`，它被存成
/// `PC+D%26T+Strategy+2026.docx` —— 空格变 `+`、`&` 变 `%26`，正是原名的 `url.QueryEscape`。
/// COS 对朴素 `filename` 参数做表单编码，因为那个参数按规范只能放 ASCII，而中文名走同一条路。
///
/// 为什么是**有条件**的：`url.QueryUnescape` 单独用会把 `+` 读成空格，于是它会把
/// `C++ notes.docx` 改写成 `C   notes.docx`。一个裸 `+` 的两种读法都合法，而头里没有任何字段
/// 能区分它们。能区分的是**自洽性** —— 一次真的表单编码经得起再过一遍编码器，一次偶然的通常
/// 不能：`C++ notes.docx` 重新编码成 `C+++notes.docx`（那个字面空格本来也会变成 `+`）⇒ 于是
/// 它被原样保留。所以规则是：**只有当把结果再编码一遍能复现头里的值时**才解码。
///
/// # 这里**不**处理的（上游逐条列出，本仓照抄）
///
/// * **唯一被编码器碰过的字符是 `+` 的名字**：它确实是"同名但空格版"的规范编码，所以往返判据
///   排除不掉它 ⇒ 这里会解码并丢掉那些 `+`（`C++.docx` → `C  .docx`）。从这根头本身看这个歧义
///   不可消解，而这里**刻意**选了一边：带空格的名字比"有 `+` 且没空格"的名字常见得多，
///   而两者都有的名字已经被上面那条往返判据保护住了。
/// * **RFC 3986 风格**（空格是 `%20` 而不是 `+`）的名字重新编码后对不上 ⇒ 保留它的转义。
/// * **发送方的非保留字符集与 Go 不一致**的名字：往返用 `url.QueryEscape` 的口径比较，
///   所以一个越出那个字符集的字符会让**整个**名字保留转义（全有或全无，不是部分）。
#[must_use]
pub fn decode_form_encoded_filename(name: &str) -> String {
    // 编码器产出不了这样的东西 ⇒ 没有可解的东西，也没有值得跑的往返。
    if !name.contains(['+', '%']) {
        return name.to_owned();
    }
    let Some(decoded) = form_unescape(name) else {
        // 一个起不了任何有效转义的孤立 `%`（`100%.docx`）说明这根本不是一次编码。
        return name.to_owned();
    };
    if decoded == name || !same_form_encoding(&query_escape(&decoded), name) {
        return name.to_owned();
    }
    decoded
}

/// `url.QueryUnescape` 的本地形态：`+` → 空格、`%XX` → 字节（UTF-8 校验）。
fn form_unescape(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
                continue;
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8(out).ok()
}

/// `url.QueryEscape` 的本地形态（非保留字符集 = `A-Za-z0-9-_.~`，空格 → `+`）。
fn query_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let byte = *byte;
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else if byte == b' ' {
            out.push('+');
        } else {
            // `write!` 到一个 `String` 上不会失败。
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// 上游 `sameFormEncoding`：两个表单编码是否相同，**只**允许百分号转义里十六进制的大小写差异。
///
/// 这点容忍不是装饰：`url.QueryEscape` 写大写十六进制，而很多服务器写小写，于是一次字节比较会
/// 拒掉 `%e5%ad%a3%e6%8a%a5.png` —— 一个中文文件名，也就是这次解码最要紧的那种（非 ASCII 的名字
/// 整个就是转义）—— 然后**悄悄地什么都不做**。
///
/// 十六进制大小写是**唯一**的容忍，这也正是这个往返判据暗含"发送方的非保留字符集就是
/// `url.QueryEscape` 的"的原因。
#[must_use]
pub fn same_form_encoding(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let mut index = 0;
    while index < left.len() {
        if left[index] == b'%' && right[index] == b'%' && index + 2 < left.len() {
            if !left[index + 1..index + 3].eq_ignore_ascii_case(&right[index + 1..index + 3]) {
                return false;
            }
            index += 3;
            continue;
        }
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// 上游 `cleanMediaFilename`：把任何一个名字压成**一个路径段**、剥掉控制字符；没有可用内容 ⇒ 空。
///
/// 控制字符这一剥是因为 [`decode_form_encoded_filename`] 加宽了能到达这里的输入（上游逐字，
/// 逐条都有实测）：原始 TAB 会被 `net/http` 原样放进头值，U+0085（NEL）也是（对
/// `unicode.IsControl` 是控制字符、对传输层是两个普通字节），而 NUL、CR、LF、ESC 与 DEL 会让
/// 传输层拒掉整个响应。既然 `ParseMediaType` 从不给朴素 `filename=` 做百分号解码，
/// `filename="a%00b.docx"` 曾经一直是它看上去的那个可打印字符串；把转义还原会把它们变成它们
/// 命名的那些字节：`%00` 变成真正的 NUL、`%0D%0A` 变成真正的 CRLF。
///
/// NUL 是**有代价**的那一个：这个名字会被写进 `attachments.filename`，那是 Postgres 的 `TEXT`，
/// 而 Postgres **根本存不下**一个 text 值里的 NUL —— 插入失败、整个附件丢掉，而那个文件的字节
/// 下载与解密都完全正常。CR 与 LF 是头注入的形态。
#[must_use]
pub fn clean_media_filename(name: &str) -> String {
    let stripped = strip_control_runes(name);
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // 反斜杠先归一成正斜杠，再取基名（上游 `path.Base(strings.ReplaceAll(name, "\\", "/"))`）。
    let normalized = trimmed.replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or_default();
    if base.is_empty() || base == "." || base == ".." {
        return String::new();
    }
    base.to_owned()
}

/// 上游 `stripControlRunes`：丢掉每一个控制字符 —— **丢掉**而不是替换。
///
/// 一个占位符会把发送方没打过的字符放进名字里，而一个真名叫 `ab.docx` 的附件被叫做 `a_b.docx`
/// 是它自己的一个小谎。
///
/// 解成**非法 UTF-8** 的字节是另一码事，而且**不**被丢掉：`strings.Map` 给它们产出的
/// U+FFFD 正是这个函数该给的东西 —— Postgres 的 `TEXT` 同样收不下非法 UTF-8，所以一个保留原始
/// 字节的名字会像 NUL 那样让插入失败，而 U+FFFD 是"这里曾经有一个字符、它没活下来"的标准说法。
#[must_use]
pub fn strip_control_runes(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests;
