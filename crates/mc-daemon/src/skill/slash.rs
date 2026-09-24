//! 正文里的 slash-skill 引用（上游 `internal/daemon/slash_skill.go`，35 行）。
//!
//! 上游用一条正则：
//!
//! ```text
//! \[/((?:[^\]\\]|\\.)+)\]\(slash://skill/([^)]+)\)
//! ```
//!
//! 即 `[/<label>](slash://skill/<id>)`，label 里可以用 `\[` / `\]` 转义方括号。
//! 本仓**没有** `regex` 边（`mc-daemon` 的 `Cargo.toml` 不出这行），所以这里按正则的
//! **语义**手写扫描器：最左起点 + label 的贪婪匹配（能从后往前挑的关门位置就挑最靠后的
//! 那个）+ 非重叠推进。行为差异只有一处，且是收紧：上游 `[^)]+` 允许 id 里含空白与换行，
//! 本实现同样允许（不额外校验），只在「找不到闭合 `)`」时放弃该起点。
//!
//! 去重口径与上游逐字一致：**按 id** 去重、先出现的胜出（label 也取先出现那次的）。

/// 一个 slash-skill 引用。
///
/// `label` 是**已反转义**的展示串（`\[` → `[`、`\]` → `]`；其余反斜杠序列原样保留，
/// 与上游那两行 `strings.ReplaceAll` 一致）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SlashSkillRef {
    /// 展示名。
    pub label: String,
    /// skill id（`slash://skill/<id>` 的 `<id>`，未做任何规范化）。
    pub id: String,
}

const PREFIX: &str = "[/";
const LINK: &str = "](slash://skill/";

/// 提取正文里的全部 slash-skill 引用（上游 `ExtractSlashSkills`）。
///
/// 不复用调用方给的容器：返回顺序 = 正文出现顺序（去重后）。
#[must_use]
pub fn extract_slash_skills(markdown: &str) -> Vec<SlashSkillRef> {
    let bytes = markdown.as_bytes();
    let mut refs: Vec<SlashSkillRef> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        let Some(offset) = markdown[cursor..].find(PREFIX) else {
            break;
        };
        let start = cursor + offset;
        let label_start = start + PREFIX.len();

        match match_at(markdown, label_start) {
            Some((id_end, raw_label, id)) => {
                if !seen.contains(&id) {
                    seen.push(id);
                    refs.push(SlashSkillRef {
                        label: unescape_label(raw_label),
                        id: id.to_string(),
                    });
                }
                // 非重叠：从整条匹配的末尾继续（上游 `FindAllStringSubmatch` 同）。
                cursor = id_end;
            }
            None => cursor = label_start,
        }
    }

    refs
}

/// 在 `label_start` 处试匹配一条引用，返回 `(整条结束偏移, 原始 label, id)`。
///
/// label 的收尾符只能是**第一个未被转义的 `]`**：正则里 label 是 `(?:[^\]\\]|\\.)+`，
/// 而 `[^\]\\]` 匹配不到 `]`、`\\` 又必须成对出现 ⇒ 一个 `]` 一旦出现在 label 内部就已经
/// 违反模式，**不存在**「换一个更靠后的关门符」这种回溯（这与 `x+` 那样可以吃满再退的组不同）。
/// 所以这里不搜集候选、也不用贪婪——扫到第一个未转义的 `]` 就是唯一候选；它后面接不上固定
/// 后缀时就是「这个起点不匹配」，由调用方推进到下一个起点。
fn match_at(markdown: &str, label_start: usize) -> Option<(usize, &str, &str)> {
    let bytes = markdown.as_bytes();
    let mut index = label_start;
    let closer = loop {
        if index >= bytes.len() {
            return None;
        }
        match bytes[index] {
            b'\\' if index + 1 < bytes.len() => index += 2,
            b']' => break index,
            _ => index += 1,
        }
    };

    // label 至少一个字符（正则里是 `+`）——空 label 在 Go 侧就匹配不上。
    if closer == label_start {
        return None;
    }
    if !markdown[closer..].starts_with(LINK) {
        return None;
    }
    let id_start = closer + LINK.len();
    let relative_end = markdown[id_start..].find(')')?;
    let id_end = id_start + relative_end;
    // id 非空（正则 `[^)]+`）。
    if id_end == id_start {
        return None;
    }
    Some((
        id_end + 1,
        &markdown[label_start..closer],
        &markdown[id_start..id_end],
    ))
}

/// label 的转义还原：上游只做 `\[` → `[`、`\]` → `]` 两条替换，**不是**通用反转义。
fn unescape_label(raw: &str) -> String {
    raw.replace("\\[", "[").replace("\\]", "]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(markdown: &str) -> Vec<(String, String)> {
        extract_slash_skills(markdown)
            .into_iter()
            .map(|entry| (entry.label, entry.id))
            .collect()
    }

    #[test]
    fn extracts_a_single_reference() {
        assert_eq!(
            refs("see [/review](slash://skill/abc) now"),
            vec![("review".to_string(), "abc".to_string())]
        );
        // 上游的 label 是「`[/` 之后、收尾 `]` 之前」的全部内容 —— 括号之类都算 label 的一部分。
        assert_eq!(
            refs("[/(review)](slash://skill/abc)"),
            vec![("(review)".to_string(), "abc".to_string())]
        );
    }

    #[test]
    fn unescapes_brackets_in_the_label() {
        assert_eq!(
            refs("[/\\[a\\]b](slash://skill/x)"),
            vec![("[a]b".to_string(), "x".to_string())]
        );
    }

    #[test]
    fn deduplicates_by_id_keeping_the_first_label() {
        assert_eq!(
            refs("[/first](slash://skill/same) then [/second](slash://skill/same)"),
            vec![("first".to_string(), "same".to_string())]
        );
    }

    #[test]
    fn keeps_two_references_with_different_ids() {
        assert_eq!(
            refs("[/a](slash://skill/1)[/b](slash://skill/2)"),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    /// `]` 不能出现在 label 里：一个多出来的 `]` 会让整个起点不匹配（正则里没有可回溯的
    /// 第二个关门候选），而不是「贪婪地吃下它」。
    #[test]
    fn an_unescaped_bracket_inside_the_label_fails_the_match() {
        assert!(refs("[/a]b](slash://skill/x)").is_empty());
        // 两个引用紧挨着时，各自从自己的 `[/` 起算。
        assert_eq!(
            refs("[/a](slash://skill/1)[/b](slash://skill/2)"),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn unfinished_or_malformed_references_are_ignored() {
        for markdown in [
            "[/label](slash://skill/abc",  // 没有收尾 `)`
            "[/label](slash://other/abc)", // 不是 slash://skill
            "[/](slash://skill/abc)",      // 空 label
            "[/label](slash://skill/)",    // 空 id
            "no references here",
            "[label](slash://skill/abc)", // 缺 `[/` 的 `/`
        ] {
            assert!(refs(markdown).is_empty(), "should ignore {markdown:?}");
        }
    }

    /// 非重叠推进：一条匹配吃掉的区间不会再生出第二条。
    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(
            refs("[/a](slash://skill/1)[/b](slash://skill/2)"),
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    /// 转义反斜杠后跟关门符：`\]` 是一个转义对，所以真正关门的是后面那个 `]`。
    #[test]
    fn escaped_closer_is_not_a_closer() {
        assert_eq!(
            refs("[/a\\]b](slash://skill/x)"),
            vec![("a]b".to_string(), "x".to_string())]
        );
        // 只有转义对、没有真关门符 ⇒ 不匹配。
        assert!(refs("[/a\\](slash://skill/x)").is_empty());
    }
}
