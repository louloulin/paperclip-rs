//! `SKILL.md` 的 YAML frontmatter 解析。
//!
//! - **写者**：M6-2（`docs/57` §3.2：`mc-skill/src/{frontmatter,binary,reserved}.rs` | M6-2 写）。
//! - **上游**：`internal/skill/frontmatter.go` 的 `ParseSkillFrontmatter` —— 结构化 skill 的
//!   `name` / `description` 允许从 `SKILL.md` 顶部的 `---` 围栏里取，正文才是 `skill.content`。
//! - **本仓约定（与 stub 的差异，已登记 `docs/32` §9.6）**：
//!   - **不返回 `Result`**。上游该函数**永不出错**：围栏缺失 → `("", "")`；围栏在但 YAML 非法
//!     → 也 `("", "")`（`yaml.Unmarshal` 的错被丢弃）。stub 原写的 `Result<_, SkillParseError>`
//!     与「非法 YAML = Err」在上游没有对应物，照抄会造成「用户文件被 400 挡下」而 Go 侧静默接受。
//!   - 因此调用方（`routes/skills/*`）**不校验** frontmatter，也不因它返回 400。
//! - **不做什么**：不做 `name` 的**唯一性**判定（那是 `skill` 表的 `UNIQUE(workspace_id,name)`
//!   约束 + M6-2 的 409 映射）；不解析正文里的 `{{ }}` 模板语法（上游没有这一步）。
//!
//! 行预算（门 ⑩）：实现 ~110 行 + 上游 19 例对照表。
//!
//! ## 逐字对齐的三处细节
//!
//! 1. 围栏识别等价于 `(?s)\A---\r?\n(.*?\r?\n)---`：**必须**以 `---` 开头，随后一个换行，
//!    然后找**第一个**「换行 + `---`」；组 1 **含**结尾那个换行（上游注释里专门提过
//!    `|` 的 chomping，别把它吃掉）。
//! 2. 逐键取值后做 Go 的 `any → string` 强转（`coerceFrontmatterValue`），再 `strings.TrimSpace`。
//! 3. 键序只影响 JSON 形态的 `to_string`：Go 的 `json.Marshal(map)` 按键排序，Rust 侧
//!    **显式排序**（不依赖 `serde_json` 的 `preserve_order` feature）。
//!
//! ## 已知偏差（登记 `docs/32` §9.6）
//!
//! - 嵌套 JSON 里的**非整数浮点**指数写法：Go `encoding/json` 对 `abs < 1e-6 || abs >= 1e21`
//!   走 `'e'` 形（`1e+100`），`serde_json` 写 `1e100`。只有当 frontmatter 的
//!   `description` 是「嵌套容器 + 极端浮点」时才有差别，本波不追。

use std::collections::HashMap;

/// `SKILL.md` frontmatter 的取值（上游只取两个键）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub name: String,
    pub description: String,
}

/// 上游 `ParseSkillFrontmatter` 的逐行移植：永不失败。
pub fn parse_skill_frontmatter(content: &str) -> Frontmatter {
    let Some(block) = frontmatter_block(content) else {
        return Frontmatter::default();
    };
    // 上游把 `map[string]any` 解错就整体放弃（`fm` 保持 nil ⇒ 两个字段都是空串）。
    let Ok(serde_yaml::Value::Mapping(entries)) = serde_yaml::from_str::<serde_yaml::Value>(block)
    else {
        return Frontmatter::default();
    };
    let mut map: HashMap<String, serde_yaml::Value> = HashMap::with_capacity(entries.len());
    for (key, value) in entries {
        // Go 的 `map[string]any` 要求键能解成字符串标量：`null` 键与复合键是 Unmarshal 失败
        // ⇒ 整张表放弃（包括本来合法的 `name`）。
        let Some(key) = key_as_string(&key) else {
            return Frontmatter::default();
        };
        map.insert(key, value);
    }
    Frontmatter {
        name: coerce(map.get("name")).trim().to_string(),
        description: coerce(map.get("description")).trim().to_string(),
    }
}

/// 映射键的字符串化：Go 的 `d.scalar` 在**字符串**目标上取节点的**字面文本**，
/// 所以 `1:` / `true:` 都是合法键（不会让整张表失败）；只有 `null` 与复合键才是错误。
fn key_as_string(key: &serde_yaml::Value) -> Option<String> {
    use serde_yaml::Value as Yaml;
    match key {
        Yaml::String(text) => Some(text.clone()),
        Yaml::Bool(flag) => Some(flag.to_string()),
        Yaml::Number(number) => Some(coerce_number(number)),
        // `!Tag` 键：只看内层（与 `coerce` 同口径）。
        Yaml::Tagged(tagged) => key_as_string(&tagged.value),
        Yaml::Null | Yaml::Sequence(_) | Yaml::Mapping(_) => None,
    }
}

/// 返回围栏内的原文（**含**结尾换行），与上游正则的组 1 等长。
fn frontmatter_block(content: &str) -> Option<&str> {
    let rest = content.strip_prefix("---")?;
    let body = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    body.char_indices()
        .filter(|(_, c)| *c == '\n')
        .map(|(i, _)| i)
        .find(|i| body[i + 1..].starts_with("---"))
        .map(|i| &body[..=i])
}

/// 单键强转（上游 `coerceFrontmatterValue`）。
fn coerce(value: Option<&serde_yaml::Value>) -> String {
    match value {
        None | Some(serde_yaml::Value::Null) => String::new(),
        Some(serde_yaml::Value::Bool(flag)) => flag.to_string(),
        Some(serde_yaml::Value::Number(number)) => coerce_number(number),
        Some(serde_yaml::Value::String(text)) => text.clone(),
        // `!Tag`：Go 的 yaml.v3 解到 `any` 时丢标签、只留值。
        Some(serde_yaml::Value::Tagged(tagged)) => coerce(Some(&tagged.value)),
        // 序列 / 映射：Go 走 `json.Marshal`，键排序、`nil` 打 `null`。
        Some(other) => coerce_json(other),
    }
}

fn coerce_number(number: &serde_yaml::Number) -> String {
    if let Some(int) = number.as_i64() {
        return int.to_string();
    }
    if let Some(uint) = number.as_u64() {
        return uint.to_string();
    }
    match number.as_f64() {
        // Go 是 `strconv.FormatFloat(f, 'g', -1, 64)`：短表示 + 指数切换（含 2 位指数）。
        Some(float) => format_float(float),
        None => String::new(),
    }
}

fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-Inf"
        } else {
            "+Inf"
        }
        .to_string();
    }
    let abs = value.abs();
    if abs != 0.0 && !(1e-4..1e21).contains(&abs) {
        let raw = format!("{value:e}");
        let (mantissa, exponent) = raw.split_once('e').unwrap_or((raw.as_str(), "0"));
        let exponent: i32 = exponent.parse().unwrap_or(0);
        return format!("{mantissa}e{exponent:+03}");
    }
    format!("{value}")
}

/// `json.Marshal` 的等价物：`nil` → `null`、映射键排序。
///
/// 含非字符串键的映射（任意层级）在 Go 侧是 `json.Marshal` **报错** ⇒ `""`
/// （yaml.v3 把这类映射解成 `map[any]any`，`encoding/json` 不支持）；这里同样退化成空串。
fn coerce_json(value: &serde_yaml::Value) -> String {
    if has_non_string_key(value) {
        return String::new();
    }
    serde_json::to_string(&yaml_to_json(value)).unwrap_or_default()
}

fn has_non_string_key(value: &serde_yaml::Value) -> bool {
    use serde_yaml::Value as Yaml;
    match value {
        Yaml::Mapping(entries) => entries
            .iter()
            .any(|(key, val)| !matches!(key, Yaml::String(_)) || has_non_string_key(val)),
        Yaml::Sequence(items) => items.iter().any(has_non_string_key),
        Yaml::Tagged(tagged) => has_non_string_key(&tagged.value),
        _ => false,
    }
}

fn yaml_to_json(value: &serde_yaml::Value) -> serde_json::Value {
    use serde_yaml::Value as Yaml;
    match value {
        Yaml::Null => serde_json::Value::Null,
        Yaml::Bool(flag) => serde_json::Value::Bool(*flag),
        Yaml::Number(number) => number_to_json(number),
        Yaml::String(text) => serde_json::Value::String(text.clone()),
        Yaml::Sequence(items) => serde_json::Value::Array(items.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(entries) => {
            // `serde_json::Map` 默认是 BTreeMap（workspace 未开 `preserve_order`），
            // 这里显式排序，两种 feature 组合下都与 Go 一致。
            let mut pairs: Vec<(String, serde_json::Value)> = entries
                .iter()
                .map(|(key, val)| (coerce(Some(key)), yaml_to_json(val)))
                .collect();
            pairs.sort_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(pairs.into_iter().collect())
        }
        // `!Tag`：Go 的 yaml.v3 解到 `any` 时丢掉标签，只留值。
        Yaml::Tagged(tagged) => yaml_to_json(&tagged.value),
    }
}

fn number_to_json(number: &serde_yaml::Number) -> serde_json::Value {
    if let Some(int) = number.as_i64() {
        return serde_json::Value::from(int);
    }
    if let Some(uint) = number.as_u64() {
        return serde_json::Value::from(uint);
    }
    match number.as_f64() {
        Some(float) => serde_json::Number::from_f64(float)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        None => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(content: &str) -> (String, String) {
        let front = parse_skill_frontmatter(content);
        (front.name, front.description)
    }

    fn check(cases: &[(&str, &str, &str)]) {
        for (content, name, description) in cases {
            assert_eq!(
                parse(content),
                (name.to_string(), description.to_string()),
                "content: {content:?}"
            );
        }
    }

    /// 上游 `frontmatter_test.go` 的 19 例（逐条搬运，含测试名注释）。
    #[test]
    fn matches_upstream_table() {
        check(&[
            // single line
            ("---\nname: foo\ndescription: bar\n---\nbody", "foo", "bar"),
            // double quoted
            (
                "---\nname: \"foo\"\ndescription: \"hello world\"\n---\nbody",
                "foo",
                "hello world",
            ),
            // single quoted
            (
                "---\nname: 'foo'\ndescription: 'hello world'\n---\nbody",
                "foo",
                "hello world",
            ),
            // literal block scalar keeps interior newlines
            (
                "---\nname: foo\ndescription: |\n  line1\n  line2\n---\nbody",
                "foo",
                "line1\nline2",
            ),
            // literal strip chomping drops trailing newline
            (
                "---\nname: foo\ndescription: |-\n  line1\n  line2\n---\nbody",
                "foo",
                "line1\nline2",
            ),
            // folded block scalar joins with spaces
            (
                "---\nname: foo\ndescription: >\n  line1\n  line2\n---\nbody",
                "foo",
                "line1 line2",
            ),
            // CRLF line endings
            (
                "---\r\nname: foo\r\ndescription: bar\r\n---\r\nbody",
                "foo",
                "bar",
            ),
            // no frontmatter returns empty
            ("no frontmatter here", "", ""),
            // unterminated frontmatter returns empty
            ("---\nname: foo\ndescription: bar\n", "", ""),
            // invalid YAML falls back to empty
            ("---\n: : : not valid\n---\nbody", "", ""),
            // name only
            ("---\nname: foo\n---\nbody", "foo", ""),
            // description only
            ("---\ndescription: bar\n---\nbody", "", "bar"),
            // leading blank line is not frontmatter
            ("\n---\nname: foo\ndescription: bar\n---\nbody", "", ""),
            // triple dash in body stops at first fence（非贪婪组）
            (
                "---\nname: foo\ndescription: bar\n---\nintro\n---\nmore",
                "foo",
                "bar",
            ),
            // non-string scalars coerce to literal
            ("---\nname: 123\ndescription: 456\n---\nbody", "123", "456"),
            // sequence description keeps name and is JSON-encoded
            (
                "---\nname: my-skill\ndescription:\n  - first feature\n  - second feature\n---\nbody",
                "my-skill",
                r#"["first feature","second feature"]"#,
            ),
            // mapping description keeps name and is JSON-encoded（键排序）
            (
                "---\nname: my-skill\ndescription:\n  a: 1\n  b: 2\n---\nbody",
                "my-skill",
                r#"{"a":1,"b":2}"#,
            ),
            // surrounding whitespace is trimmed off both fields
            (
                "---\nname: \"  foo  \"\ndescription: \"  hello world\\n\"\n---\nbody",
                "foo",
                "hello world",
            ),
            // issue 3495 chinese literal block scalar
            (
                concat!(
                    "---\n",
                    "name: requirements-workshop\n",
                    "description: |\n",
                    "  当用户想要开发新功能、讨论需求、梳理业务逻辑时触发。通过多轮对话将模糊的想法转化为结构化的需求文档，为后续技术方案设计提供输入。\n",
                    "  适用场景：用户说\"我想实现XX功能\"、\"讨论一下这个需求\"、\"帮我分析一下怎么做\"、\"这个需求该怎么设计\"、\"写个需求文档\"等。\n",
                    "  本skill只负责产出需求文档，不进入技术设计或代码实现阶段。\n",
                    "---\nbody",
                ),
                "requirements-workshop",
                concat!(
                    "当用户想要开发新功能、讨论需求、梳理业务逻辑时触发。通过多轮对话将模糊的想法转化为结构化的需求文档，为后续技术方案设计提供输入。\n",
                    "适用场景：用户说\"我想实现XX功能\"、\"讨论一下这个需求\"、\"帮我分析一下怎么做\"、\"这个需求该怎么设计\"、\"写个需求文档\"等。\n",
                    "本skill只负责产出需求文档，不进入技术设计或代码实现阶段。",
                ),
            ),
        ]);
    }

    /// 上游的多余键用例（`license` / `version` 不得让 `name` / `description` 失效）。
    #[test]
    fn ignores_other_keys() {
        assert_eq!(
            parse("---\nname: n\nlicense: MIT\nversion: 2\n---\n"),
            ("n".to_string(), String::new())
        );
    }

    /// 空围栏 / 空白围栏：Go 的 `yaml.Unmarshal` 不出错也不填 map ⇒ 两个字段都空。
    #[test]
    fn empty_fences_are_tolerated() {
        assert_eq!(parse("---\n---\nbody"), (String::new(), String::new()));
        assert_eq!(parse("---\n\n---\nbody"), (String::new(), String::new()));
    }

    /// `1:` / `true:` 在 Go 侧是**合法**键（取字面文本），不得连带丢掉 `name`。
    #[test]
    fn scalar_non_string_keys_are_legal() {
        assert_eq!(
            parse("---\n1: x\nname: test\n---\n"),
            ("test".to_string(), String::new())
        );
        assert_eq!(
            parse("---\ntrue: x\nname: test\n---\n"),
            ("test".to_string(), String::new())
        );
    }

    /// `null` 键（`~:` / 裸 `:`）与复合键在 Go 侧是 Unmarshal 失败 ⇒ 整张表放弃。
    #[test]
    fn null_keys_discard_the_whole_map() {
        for content in [
            "---\n~: x\nname: test\n---\n",
            "---\n: x\nname: test\n---\n",
            "---\n? [a]\n: x\nname: test\n---\n",
        ] {
            assert_eq!(
                parse(content),
                (String::new(), String::new()),
                "content: {content:?}"
            );
        }
    }

    /// 顶层不是映射（`---\nfoo\n---`）⇒ Go 的 `map[string]any` 报错。
    #[test]
    fn non_mapping_document_discards_everything() {
        for content in ["---\nfoo\n---\nbody", "---\n- a\n- b\n---\nbody"] {
            assert_eq!(
                parse(content),
                (String::new(), String::new()),
                "content: {content:?}"
            );
        }
    }

    /// 嵌套映射含非字符串键时 Go 的 `json.Marshal(map[any]any)` 失败 ⇒ `""`。
    #[test]
    fn nested_non_string_keys_coerce_to_empty() {
        assert_eq!(
            parse("---\nname: n\ndescription:\n  1: x\n---\n"),
            ("n".to_string(), String::new())
        );
    }

    /// 带标签的值（`!Tag`）：yaml.v3 解到 `any` 时丢标签只留值。
    #[test]
    fn tagged_scalars_keep_their_value() {
        assert_eq!(
            parse("---\nname: !custom foo\n---\n"),
            ("foo".to_string(), String::new())
        );
    }

    /// Go `strconv.FormatFloat(f, 'g', -1, 64)` 的指数形态（两位以上指数、带符号）。
    #[test]
    fn floats_use_go_shortest_form() {
        assert_eq!(format_float(1.5), "1.5");
        assert_eq!(format_float(-0.25), "-0.25");
        assert_eq!(format_float(1e-5), "1e-05");
        assert_eq!(format_float(1e100), "1e+100");
        assert_eq!(format_float(1e21), "1e+21");
    }
}
