//! `Content-Disposition` 解析的用例：**扩展形态无条件优先**、表单解码的往返判据、
//! 以及"先解码、再取基名、最后剥控制字符"这个**顺序**本身。

use pretty_assertions::assert_eq;

use super::*;

/// 扩展形态（RFC 5987）**优先于**朴素 `filename=`，**与先后无关**。
#[test]
fn the_extended_filename_wins_regardless_of_order() {
    let extended_first =
        r#"attachment; filename*=UTF-8''%E5%AD%A3%E6%8A%A5.docx; filename="___ .docx""#;
    let plain_first =
        r#"attachment; filename="___ .docx"; filename*=UTF-8''%E5%AD%A3%E6%8A%A5.docx"#;
    assert_eq!(media_filename_from_disposition(extended_first), "季报.docx");
    assert_eq!(media_filename_from_disposition(plain_first), "季报.docx");
    // 大写的 CHARSET 与小写都收。
    assert_eq!(
        media_filename_from_disposition(r"attachment; filename*=utf-8''%E5%AD%A3%E6%8A%A5.docx"),
        "季报.docx"
    );
    assert!(has_extended_filename(extended_first));
    assert!(!has_extended_filename(
        r#"attachment; filename="plain.docx""#
    ));
}

/// 只给朴素形态时它才生效，并**照旧**走表单解码。
#[test]
fn a_plain_filename_is_used_and_form_decoded() {
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="PC+D%26T+Strategy+2026.docx""#),
        "PC D&T Strategy 2026.docx"
    );
    // 不带引号的 token 形态同样认。
    assert_eq!(
        media_filename_from_disposition("attachment; filename=report.pdf"),
        "report.pdf"
    );
    // 小写 hex 的中文名（服务器写小写十六进制是常态）。
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="%e5%ad%a3%e6%8a%a5.png""#),
        "季报.png"
    );
}

/// 往返判据保护住"有 `+` 但没有空格"的名字；两边都有的那种按上游**已知的**代价取舍。
#[test]
fn the_form_decode_round_trip_protects_names_that_only_have_pluses() {
    // 名字里只有 `+` 碰了编码器 ⇒ 重新编码对不上 ⇒ 原样保留。
    assert_eq!(
        decode_form_encoded_filename("C++ notes.docx"),
        "C++ notes.docx"
    );
    assert_eq!(
        "C++ notes.docx",
        media_filename_from_disposition(r#"attachment; filename="C++ notes.docx""#)
    );
    // 上游点名的那种"不可消解的歧义"：这里**刻意**选了带空格的那一边。
    assert_eq!(decode_form_encoded_filename("C++.docx"), "C  .docx");
    // RFC 3986 风格（空格是 `%20`）⇒ 对不上 ⇒ 保留转义。
    assert_eq!(
        decode_form_encoded_filename("PC%20D&T.docx"),
        "PC%20D&T.docx"
    );
    // 孤立的 `%` 不是一次编码。
    assert_eq!(decode_form_encoded_filename("100%.docx"), "100%.docx");
    // 解不成 UTF-8 的转义 ⇒ 不是一次可用的编码。
    assert_eq!(decode_form_encoded_filename("%FF%FE.docx"), "%FF%FE.docx");
    // 没有任何编码器碰过的名字走原路（快路径）。
    assert_eq!(decode_form_encoded_filename("report.pdf"), "report.pdf");
}

/// 🔴 **顺序**：一次被转义的分隔符**先解码、再取基名**才拦得住穿越 —— 顺序反过来就放行。
#[test]
fn escapes_are_decoded_before_the_basename_reduction() {
    // 朴素形态里的 %2F：解码之后才是路径，而取基名把它压成一个段。
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="..%2F..%2Fetc%2Fpasswd""#),
        "passwd"
    );
    // 反斜杠同样先归一成正斜杠（**不带引号**的形态：引号里的 `\` 是 RFC 的转义字符，
    // 会被 unquote 吃掉 —— 那是 `ParseMediaType` 的行为，也对）。
    assert_eq!(
        media_filename_from_disposition(r"attachment; filename=..\..\windows\x.dll"),
        "x.dll"
    );
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="..\..\windows\x.dll""#),
        "....windowsx.dll"
    );
    // 扩展形态里的转义（这里没有表单解码那一步，但取基名一样要跑）。
    assert_eq!(
        media_filename_from_disposition(r"attachment; filename*=UTF-8''%2Fetc%2Fshadow"),
        "shadow"
    );
    // 只剩路径本身的名字 ⇒ 没有可用的基名。
    for raw in [
        r#"attachment; filename="/""#,
        r#"attachment; filename="..""#,
        r#"attachment; filename=".""#,
        r#"attachment; filename="/etc/""#,
    ] {
        assert_eq!(media_filename_from_disposition(raw), "", "{raw}");
    }
}

/// 控制字符在最后被剥掉（解码把它们从"看上去可打印"变成了真正的字节）。
#[test]
fn control_characters_are_stripped_after_decoding() {
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="a%00b.docx""#),
        "ab.docx"
    );
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="a%0D%0Ab.docx""#),
        "ab.docx"
    );
    // 头里直接放一个 TAB 或 NEL 时也剥。
    assert_eq!(
        media_filename_from_disposition("attachment; filename=\"a\tb\u{85}c.docx\""),
        "abc.docx"
    );
    // 全是控制字符 ⇒ 没有可用名字。
    assert_eq!(
        media_filename_from_disposition("attachment; filename=\"\u{1}\""),
        ""
    );
}

/// 解析不了 / 没有名字 / 空的头：一律空串（上游靠 `ParseMediaType` 的报错路径）。
#[test]
fn unparseable_or_nameless_headers_yield_no_name() {
    assert_eq!(media_filename_from_disposition(""), "");
    assert_eq!(media_filename_from_disposition("   "), "");
    assert_eq!(media_filename_from_disposition("attachment"), "");
    assert_eq!(media_filename_from_disposition("attachment; filename"), "");
    assert_eq!(media_filename_from_disposition("attachment; =value"), "");
    assert_eq!(media_filename_from_disposition("attachment; broken"), "");
    // 非 UTF-8 / 非 us-ascii 的 charset ⇒ 跳过扩展形态；这里没有朴素形态可退 ⇒ 空。
    assert_eq!(
        media_filename_from_disposition(r"attachment; filename*=ISO-8859-1''a%20b.docx"),
        ""
    );
    // 有朴素形态可退时退回去。
    assert_eq!(
        media_filename_from_disposition(
            r#"attachment; filename*=ISO-8859-1''ignored; filename="fallback.docx""#
        ),
        "fallback.docx"
    );
}

/// 引号内的 `\X` 转义与引号内的分号（上游 `ParseMediaType` 的同一套）。
#[test]
fn quoted_values_are_unquoted_and_unescaped() {
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="a\"b.docx""#),
        "a\"b.docx"
    );
    assert_eq!(
        media_filename_from_disposition(r#"attachment; filename="a;b.docx""#),
        "a;b.docx"
    );
    // 参数名大小写不敏感。
    assert_eq!(
        media_filename_from_disposition(r#"attachment; FILENAME="upper.docx""#),
        "upper.docx"
    );
}

/// 两个纯函数各自的边界。
#[test]
fn the_two_helpers_are_pinned_on_their_own() {
    assert!(has_extended_filename("FILENAME*=UTF-8''x"));
    // 值里含 "filename*" 字样的朴素名是**误报**，代价只是"原样保留"（上游逐字）。
    assert!(has_extended_filename(
        r#"attachment; filename="weird filename* name.docx""#
    ));

    assert!(same_form_encoding("a%2Fb", "a%2fb"));
    assert!(same_form_encoding("abc", "abc"));
    assert!(!same_form_encoding("a%2Fb", "a%2Bb"));
    assert!(!same_form_encoding("abc", "abd"));
    assert!(!same_form_encoding("abc", "abcd"));
    assert!(!same_form_encoding("%2F", "x%2F"));

    assert_eq!(clean_media_filename("  report.pdf  "), "report.pdf");
    assert_eq!(clean_media_filename("dir/report.pdf"), "report.pdf");
    assert_eq!(clean_media_filename("dir\\report.pdf"), "report.pdf");
    assert_eq!(clean_media_filename(".."), "");
    assert_eq!(clean_media_filename(""), "");
    assert_eq!(strip_control_runes("a\u{0}b"), "ab");
    assert_eq!(strip_control_runes("普通名字"), "普通名字");
}
