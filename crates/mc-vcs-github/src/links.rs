//! issue ↔ PR 的自动关联与「closing keyword」抽取 —— 上游 `extractIdentifiers` /
//! `extractClosingIdentifiers` / `issueNumberForPrefix`（`github.go` L964–L1997）
//! + 两条正则 `identifierRe` / `closingIdentifierRe`（`github.go:1032-1046`）。
//!
//! M8-4 的 `DoD` 点名了 **6 个边界**（`docs/61` §6.5）：大小写、`#` 前缀、跨行、代码块内、
//! `owner/repo#n` 形态、重复标识去重。全部在本文件的 `tests` 模块里逐条钉住。
//!
//! # 为什么手写扫描器而不用 `regex`
//!
//! `mc-vcs-github` 的依赖边是 anchor **一次接好并冻结**的（`Cargo.toml` 的注释逐字：
//! 「此后 M8-1/4/5 的写者**不得**再新增三方依赖」）——`regex` 不在其中。手写还有一个
//! **语义**上的好处：Go 的 RE2 与 Rust `regex` 在 `\b` 与 `(?i)` 上有两处真实差异
//! （Rust 的 `\b` 默认 Unicode-aware；`(?i)` 的大小折叠集合也与 Go 不同），逐字复刻
//! Go 的语义比「找一个近似正则」更容易验收。差异逐条写在下面对应的函数上。
//!
//! # 上游的两条正则（逐字）
//!
//! ```text
//! identifierRe        = (?i)\b([a-z][a-z0-9]{0,9})-(\d+)\b
//! closingIdentifierRe = (?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)[:\s]+([a-z][a-z0-9]{0,9})-(\d+)\b
//! ```
//!
//! 三条必须逐字复刻的细节：
//!
//! 1. **`\b` 是 ASCII 边界**：Go 的 `\b` 只看 `[0-9A-Za-z_]`，与 `(?i)` 的 Unicode 折叠
//!    无关。Rust 的 `regex::Regex` 默认给 `\b` 加 Unicode 语义 ⇒ 手写时显式只认
//!    `[0-9A-Za-z_]`。
//! 2. **前缀最多 10 个字符**（首字符必须是字母，后 9 个字母或数字）。11 个连续字母开头的
//!    `abcdefghijk-1` 在任何位置都**不**匹配（左边界不成立或前缀超长），这条单独有用例。
//! 3. **`[:\s]+` 里的 `\s` 是 Go 的 ASCII 集合** `[\t\n\f\r ]`（**不含**垂直制表 `\x0b`，
//!    也不含任何 Unicode 空白）—— 这是「跨行」边界能成立的原因（`\s` 含 `\n`）。
//!
//! # 关闭关键词的严格相邻性
//!
//! 上游注释逐字：「`Fix MUL-1` closes MUL-1, but `Fix login MUL-1` does not」——
//! 关键词与标识符之间**只**允许一个 `[:\s]+` 段，中间夹任何其它词都不成立。
//! 分支名刻意**不**参与关闭判定（调用方只传 title / body）。

/// 一个已识别的 `PREFIX-NUMBER` 标识符（前缀已**大写**，数字段**原样保留**）。
///
/// 上游返回的是 `"MUL-1801"` 这样的字符串（`strings.ToUpper(m[1]) + "-" + m[2]`）。
/// 保留数字段的原样是有意的：`MUL-007` 与 `MUL-7` 在字符串层是**两个**不同的标识符
/// （去重按字符串做），但 `issue_number_for_prefix` 会把它们解析成同一个号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identifier {
    /// 大写后的前缀（不含 `-`）。
    pub prefix: String,
    /// 数字段原样（可能带前导零）。
    pub number: String,
}

impl Identifier {
    /// 上游拼出来的那个字符串（`MUL-1801`）。
    pub fn as_identifier(&self) -> String {
        format!("{}-{}", self.prefix, self.number)
    }
}

/// ASCII word 字符（Go `\b` 认的集合：`[0-9A-Za-z_]`）。
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Go 的 `\s`：`[\t\n\f\r ]`（**不含** `\x0b`）。
fn is_go_space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

/// 大小写不敏感地比较 `bytes[start..]` 与 `literal`（两者都必须是 ASCII）。
///
/// ⚠️ 与 Go `(?i)` 的差异登记（`docs/32` §9.12）：Go 的 `(?i)` 对 `[a-z]` 会加上
/// **Unicode 简单折叠**的等价类（例如 `ſ` U+017F 折成 `s`、`K` U+212A 折成 `k`），
/// 本实现只做 ASCII 折叠。触发条件是一个 RFC-1034 意义上的 hostname 里出现 U+017F /
/// U+212A 这类字符 —— 实际不可达；**宁可少匹配**（不会误关 issue）。
fn matches_ascii_ignore_case(bytes: &[u8], start: usize, literal: &[u8]) -> bool {
    if bytes.len() < start + literal.len() {
        return false;
    }
    bytes[start..start + literal.len()]
        .iter()
        .zip(literal)
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// `\b` 的 ASCII 语义：位置 `at` 处的**左侧**是「非 word 字符或串首」。
///
/// `\b` 还要求右侧是 word 字符，但两条正则里 `\b` 之后**紧跟**一个 `[a-z]`（必定是
/// word 字符），所以右侧条件恒成立 ⇒ 只需判左侧。
fn left_word_boundary(bytes: &[u8], at: usize) -> bool {
    at == 0 || !is_word_byte(bytes[at - 1])
}

/// 在 `start` 处匹配 `([a-z][a-z0-9]{0,9})-(\d+)\b`，返回 `(前缀字节区间, 数字字节区间, 结束下标)`。
///
/// 贪婪的前缀长度**不需要回溯**：前缀后面必须紧跟 `-`，而更短的前缀的下一个字节仍落在
/// `[a-z0-9]` 里（不是 `-`）⇒ 贪心失败即整体失败（Go 的回溯也只会得出同一结论）。
fn match_identifier_at(bytes: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    if !left_word_boundary(bytes, start) {
        return None;
    }
    // 首字符必须是字母。
    match bytes.get(start) {
        Some(byte) if byte.is_ascii_alphabetic() => {}
        _ => return None,
    }
    let mut cursor = start + 1;
    // 贪婪吃最多 9 个字母/数字。
    while cursor < bytes.len() && cursor - start < 10 && bytes[cursor].is_ascii_alphanumeric() {
        cursor += 1;
    }
    // 必须紧跟 `-`。
    if bytes.get(cursor) != Some(&b'-') {
        return None;
    }
    let dash = cursor;
    cursor += 1;
    let digits_start = cursor;
    while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
        cursor += 1;
    }
    if cursor == digits_start {
        return None;
    }
    // 右侧 `\b`：必须「到串尾」或「下一个字节非 word」。
    if cursor < bytes.len() && is_word_byte(bytes[cursor]) {
        return None;
    }
    Some((start, dash, digits_start))
}

/// 扫描 `text`，按**出现顺序**产出全部标识符（**不去重**）。
///
/// ⚠️ 左边界不成立时**不**推进「已消费到哪」——上游的 `FindAllStringSubmatch` 是纯扫描，
/// 前一个匹配的结束位置与下一个匹配的起点无关。
fn scan_identifiers(text: &str) -> Vec<Identifier> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if let Some((prefix_start, dash, digits_start)) = match_identifier_at(bytes, cursor) {
            let end = {
                let mut end = digits_start;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
                end
            };
            out.push(Identifier {
                prefix: text[prefix_start..dash].to_ascii_uppercase(),
                number: text[digits_start..end].to_string(),
            });
            cursor = end;
        } else {
            cursor += 1;
        }
    }
    out
}

/// 抽取文中提到的全部 issue identifier（**去重，保持出现顺序**，前缀大写）。
///
/// # 6 个边界（`DoD`）
///
/// | 边界 | 行为 |
/// | --- | --- |
/// | 大小写 | `mul-1801` / `MuL-1801` 都命中，输出一律大写 |
/// | `#` 前缀 | `#MUL-1801` 命中（`#` 是非 word 字符，左侧边界成立） |
/// | 跨行 | `\b` 只认单字节邻接，跨行与跨段无关 ⇒ 命中 |
/// | 代码块内 | **命中**（上游不做 Markdown 解析；这是刻意的，逐字对齐） |
/// | `owner/repo#n` | `acme/api#MUL-1801` 命中（`#` 左侧边界成立） |
/// | 去重 | 同标识符多次出现只留第一次，且不同来源的 part 也共用同一张 seen 表 |
pub fn extract_identifiers(parts: &[&str]) -> Vec<String> {
    let mut seen = Vec::new();
    for part in parts {
        for identifier in scan_identifiers(part) {
            let rendered = identifier.as_identifier();
            if !seen.contains(&rendered) {
                seen.push(rendered);
            }
        }
    }
    seen
}

/// 在 `start` 处匹配关闭关键词，返回**关键词之后的第一个下标**（即 `[:\s]+` 的起点）。
///
/// 上游的 alternation + 贪婪长度在「某个起点」上**最多只有一个**长度能成功：更短的关键词
/// 后面紧跟的字节仍是字母（`s`/`d`/`e`），落不进 `[:\s]`。所以这里实现成
/// 「贪婪吃掉可选后缀，然后要求 `[:\s]`」——与 RE2 的回溯结果等价。
fn match_closing_keyword_at(bytes: &[u8], start: usize) -> Option<usize> {
    if !left_word_boundary(bytes, start) {
        return None;
    }
    let mut end = if matches_ascii_ignore_case(bytes, start, b"close") {
        start + b"close".len()
    } else if matches_ascii_ignore_case(bytes, start, b"resolve") {
        start + b"resolve".len()
    } else if matches_ascii_ignore_case(bytes, start, b"fix") {
        // `fix(?:e[sd])?`：先试带 `e[sd]`，不行再退回裸 `fix`。
        if matches_ascii_ignore_case(bytes, start + 3, b"es")
            || matches_ascii_ignore_case(bytes, start + 3, b"ed")
        {
            start + 5
        } else {
            start + 3
        }
    } else {
        return None;
    };
    // `close[sd]?` / `resolve[sd]?`：可选后缀。
    if (matches_ascii_ignore_case(bytes, start, b"close")
        || matches_ascii_ignore_case(bytes, start, b"resolve"))
        && (matches_ascii_ignore_case(bytes, end, b"s")
            || matches_ascii_ignore_case(bytes, end, b"d"))
    {
        end += 1;
    }
    Some(end)
}

/// 只抽取**关闭语义**的 identifier（`fixes` / `closes` / `resolves` + 变体）。
///
/// 关键词与标识符之间必须是 `[:\s]+`（至少一个冒号或空白，且**跨行也算**）；分支名刻意
/// 不参与 ⇒ 调用方**只**传 title 与 body（上游注释逐字）。
pub fn extract_closing_identifiers(parts: &[&str]) -> Vec<String> {
    let mut seen = Vec::new();
    for part in parts {
        let bytes = part.as_bytes();
        let mut cursor = 0;
        while cursor < bytes.len() {
            let Some(keyword_end) = match_closing_keyword_at(bytes, cursor) else {
                cursor += 1;
                continue;
            };
            // `[:\s]+`
            let mut at = keyword_end;
            while at < bytes.len() && (bytes[at] == b':' || is_go_space(bytes[at])) {
                at += 1;
            }
            if at == keyword_end {
                cursor += 1;
                continue;
            }
            match match_identifier_at(bytes, at) {
                Some((prefix_start, dash, digits_start)) => {
                    let mut end = digits_start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    let rendered = format!(
                        "{}-{}",
                        part[prefix_start..dash].to_ascii_uppercase(),
                        &part[digits_start..end]
                    );
                    if !seen.contains(&rendered) {
                        seen.push(rendered);
                    }
                    // 与上游一致：从**标识符之后**继续扫（同一个关键词不重复消费）。
                    cursor = end;
                }
                None => cursor += 1,
            }
        }
    }
    seen
}

/// 上游 `issueNumberForPrefix`：identifier 里的号，且前缀必须**等于**该 workspace 的前缀。
///
/// 大小写不敏感（分支名习惯小写，而 issue 前缀是大写）。取**最后一个** `-` 作为分隔
/// （`strings.LastIndex`）：`acme-api-1801` 的前缀是 `acme-api`。
///
/// # 与上游的两处差异（登记 `docs/32` §9.12）
///
/// - `strings.EqualFold` 是 Unicode 折叠，本实现用 `eq_ignore_ascii_case`（同上，
///   实际不可达）；
/// - 数字段解析是 `strconv.Atoi` 再截成 `int32`（Go 会**回绕**）；本实现溢出即 `None`。
///   输入来自 [`extract_identifiers`]，只可能是一串 ASCII 数字，两种实现在这条输入域上
///   只在「超过 `i32::MAX` 的号」上分叉，而回绕出一个负号毫无意义 ⇒ 取 `None` 更安全。
pub fn issue_number_for_prefix(identifier: &str, prefix: &str) -> Option<i32> {
    let idx = identifier.rfind('-')?;
    let (got_prefix, number) = identifier.split_at(idx);
    let number = &number[1..];
    if !got_prefix.eq_ignore_ascii_case(prefix) {
        return None;
    }
    number.parse::<i32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // extract_identifiers 的 6 个边界（DoD 逐条）
    // -----------------------------------------------------------------------

    #[test]
    fn boundary_case_is_folded_to_uppercase() {
        assert_eq!(extract_identifiers(&["mul-1801"]), vec!["MUL-1801"]);
        assert_eq!(extract_identifiers(&["MuL-1801"]), vec!["MUL-1801"]);
        // 数字段原样（前导零保留：字符串层是另一个标识符）。
        assert_eq!(extract_identifiers(&["MUL-007"]), vec!["MUL-007"]);
    }

    #[test]
    fn boundary_hash_prefix_matches() {
        // `#` 是非 word 字符 ⇒ 左侧边界成立。
        assert_eq!(extract_identifiers(&["#MUL-1801"]), vec!["MUL-1801"]);
        assert_eq!(
            extract_identifiers(&["see #mul-1801 and MUL-1801"]),
            vec!["MUL-1801"],
            "同一个标识符只留一次"
        );
    }

    #[test]
    fn boundary_cross_line_matches() {
        assert_eq!(
            extract_identifiers(&["first line\nMUL-1801 third"]),
            vec!["MUL-1801"]
        );
        // 换行就在标识符前面（`\n` 非 word ⇒ 左边界成立）。
        assert_eq!(extract_identifiers(&["x\n#MUL-1801"]), vec!["MUL-1801"]);
    }

    #[test]
    fn boundary_inside_code_block_still_matches() {
        // 上游**不**做 Markdown 解析：代码块里的标识符照样命中（刻意逐字对齐）。
        let text = "```\nMUL-1801\n```\n";
        assert_eq!(extract_identifiers(&[text]), vec!["MUL-1801"]);
    }

    #[test]
    fn boundary_owner_repo_hash_form_matches() {
        assert_eq!(
            extract_identifiers(&["acme/api#MUL-1801"]),
            vec!["MUL-1801"]
        );
        // `owner/repo#1801`（无前缀）**不**命中：正则要求 `[a-z]` 打头。
        assert!(extract_identifiers(&["acme/api#1801"]).is_empty());
        // GitLab 的 `!1801` 同理不命中。
        assert!(extract_identifiers(&["acme/api!1801"]).is_empty());
    }

    #[test]
    fn boundary_dedup_keeps_first_occurrence_order_across_parts() {
        assert_eq!(
            extract_identifiers(&["MUL-2 MUL-1 MUL-2", "MUL-1 MUL-3"]),
            vec!["MUL-2", "MUL-1", "MUL-3"]
        );
    }

    #[test]
    fn prefix_is_capped_at_ten_characters() {
        // 10 个字符的前缀是上限 ⇒ 命中。
        assert_eq!(extract_identifiers(&["abcdefghij-1"]), vec!["ABCDEFGHIJ-1"]);
        // 11 个字符 ⇒ 任何起点都不成立（更短前缀的下一个字节仍落在 `[a-z0-9]`）。
        assert!(extract_identifiers(&["abcdefghijk-1"]).is_empty());
    }

    #[test]
    fn identifier_needs_a_non_word_left_edge_and_a_digit_right_edge() {
        // 左边界不成立（前面紧贴 word 字符）⇒ 整段被当成一个更长的前缀。
        assert_eq!(
            extract_identifiers(&["xMUL-1"]),
            vec!["XMUL-1"],
            "左边界紧贴 word 字符时，前缀会吞掉它（上游同判）"
        );
        // 右边界不成立（数字之后紧跟 word 字符）⇒ 不命中。
        assert!(extract_identifiers(&["MUL-1x"]).is_empty());
        // `v1.2-3` 这类版本号：`2-3` 的左边界不成立（前面是 `.`，成立！但 `v1` 之后的
        // `-` 后面不是数字）⇒ 逐条对齐上游：只有 `(?i)([a-z][a-z0-9]{0,9})-(\d+)\b`
        // 能命中，而 `2-3` 的首字符不是字母。
        assert!(extract_identifiers(&["v1.2-3"]).is_empty());
    }

    // -----------------------------------------------------------------------
    // extract_closing_identifiers
    // -----------------------------------------------------------------------

    #[test]
    fn closing_keywords_require_strict_adjacency() {
        // 关键词 + 单个空格 ⇒ 命中。
        assert_eq!(extract_closing_identifiers(&["Fix MUL-1"]), vec!["MUL-1"]);
        // 中间夹词 ⇒ **不**命中（上游注释逐字点名这一条）。
        assert!(extract_closing_identifiers(&["Fix login MUL-1"]).is_empty());
        // 冒号分隔 ⇒ 命中。
        assert_eq!(
            extract_closing_identifiers(&["Closes: MUL-2"]),
            vec!["MUL-2"]
        );
        // 裸提及不算关闭。
        assert!(extract_closing_identifiers(&["Follow up in MUL-2"]).is_empty());
        assert!(extract_closing_identifiers(&["MUL-1: do it"]).is_empty());
    }

    #[test]
    fn closing_keyword_variants_cover_all_three_families() {
        for keyword in [
            "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
        ] {
            let text = format!("{keyword} MUL-9");
            assert_eq!(
                extract_closing_identifiers(&[text.as_str()]),
                vec!["MUL-9"],
                "{keyword}"
            );
        }
        // 大小写不敏感。
        assert_eq!(
            extract_closing_identifiers(&["CLOSES mul-9"]),
            vec!["MUL-9"]
        );
        // 近似词不命中。
        for near in ["closing", "clos", "fixe", "resolv", "uncloses", "prefix"] {
            let text = format!("{near} MUL-9");
            assert!(
                extract_closing_identifiers(&[text.as_str()]).is_empty(),
                "{near} 不应命中"
            );
        }
    }

    #[test]
    fn closing_keyword_tolerates_newlines_and_the_go_space_set() {
        // `[:\s]+` 的 `\s` 含 `\n` ⇒ 跨行成立。
        assert_eq!(
            extract_closing_identifiers(&["Closes\nMUL-3"]),
            vec!["MUL-3"]
        );
        assert_eq!(
            extract_closing_identifiers(&["Fixes \t\n: MUL-3"]),
            vec!["MUL-3"]
        );
        // 垂直制表 **不**在 Go 的 `\s` 里（`[\t\n\f\r ]`）⇒ 不成立。
        assert!(extract_closing_identifiers(&["Closes\u{000b}MUL-3"]).is_empty());
    }

    #[test]
    fn closing_identifiers_dedup_and_skip_branch_names_by_contract() {
        assert_eq!(
            extract_closing_identifiers(&["Closes MUL-1", "close Mul-1"]),
            vec!["MUL-1"]
        );
        // 分支名形态：`mul-1/fix-login` 里 `fix-login` 的前缀会被抽成 `FIX`？不会 ——
        // `fix-` 后面必须是数字，`login` 不是 ⇒ 这一条只作反例钉住。
        assert!(extract_closing_identifiers(&["mul-1/fix-login"]).is_empty());
    }

    // -----------------------------------------------------------------------
    // issue_number_for_prefix
    // -----------------------------------------------------------------------

    #[test]
    fn issue_number_for_prefix_boundaries() {
        assert_eq!(issue_number_for_prefix("MUL-1801", "MUL"), Some(1801));
        // 大小写不敏感（分支名习惯小写、前缀大写）。
        assert_eq!(issue_number_for_prefix("mul-1801", "MUL"), Some(1801));
        assert_eq!(issue_number_for_prefix("MUL-1801", "mul"), Some(1801));
        // 前缀不符 ⇒ None。
        assert_eq!(issue_number_for_prefix("OTHER-1801", "MUL"), None);
        // 取**最后一个** `-`（`strings.LastIndex`）。
        assert_eq!(issue_number_for_prefix("acme-api-7", "acme-api"), Some(7));
        assert_eq!(issue_number_for_prefix("acme-api-7", "acme"), None);
        // 无 `-` / 号不是数字 / 号溢出 ⇒ None（上游 `Atoi` 失败；溢出是登记过的差异）。
        assert_eq!(issue_number_for_prefix("MUL1801", "MUL"), None);
        assert_eq!(issue_number_for_prefix("MUL-x", "MUL"), None);
        assert_eq!(issue_number_for_prefix("MUL-", "MUL"), None);
        assert_eq!(issue_number_for_prefix("MUL-99999999999999", "MUL"), None);
        // 前导零解析成同一个号（与标识符字符串层的去重正交）。
        assert_eq!(issue_number_for_prefix("MUL-007", "MUL"), Some(7));
    }
}
