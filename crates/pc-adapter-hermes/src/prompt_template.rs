//! Hermes prompt 模板渲染（对齐 Node `renderTemplate` +
//! `renderConditionalSections` + `joinPromptSections`，来自
//! `packages/adapter-utils/src/server-utils.ts`）。
//!
//! 三个纯函数：
//! - [`render_template`] — `{{path.to.var}}` 占位符替换
//! - [`render_conditional_sections`] — `{{#key}}...{{/key}}` 条件块
//! - [`join_prompt_sections`] — 过滤空白 + 用分隔符拼接
//!
//! 所有函数纯函数 + 不依赖外部 IO，便于测试与复用。

use serde_json::Value;

/// `{{path.to.var}}` 替换：按 `.` 分段走 `data` JSON 树。
///
/// 不存在的路径替换为空串（与 Node 行为一致：`resolvePathValue` 返回
/// `undefined` 时 `String.replace` 会保留 `undefined`，Node 中是 `"undefined"`，
/// Rust 中替换为空字符串以避免把字面量 `"undefined"` 注入 prompt）。
pub fn render_template(template: &str, data: &Value) -> String {
    let mut result = String::with_capacity(template.len());
    let mut cursor = 0usize;
    let bytes = template.as_bytes();
    while cursor < bytes.len() {
        if cursor + 1 < bytes.len() && bytes[cursor] == b'{' && bytes[cursor + 1] == b'{' {
            // 寻找 `}}` 闭合
            if let Some(close_offset) = find_double_close(template, cursor + 2) {
                let inside = &template[cursor + 2..close_offset];
                let var_path = inside.trim();
                result.push_str(&resolve_path(data, var_path));
                cursor = close_offset + 2;
                continue;
            }
        }
        // 普通字符
        let ch_end = template[cursor..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| cursor + i)
            .unwrap_or(template.len());
        result.push_str(&template[cursor..ch_end]);
        cursor = ch_end;
    }
    result
}

/// 寻找 `}}` 闭合（不跨越中间的 `{`）。返回相对 `template` 的 byte offset。
fn find_double_close(template: &str, from: usize) -> Option<usize> {
    let bytes = template.as_bytes();
    let mut cursor = from;
    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'}' && bytes[cursor + 1] == b'}' {
            return Some(cursor);
        }
        if bytes[cursor] == b'{' {
            return None; // 嵌套未支持
        }
        cursor += 1;
    }
    None
}

/// 按 `.` 分段走 JSON 树。任意一段缺失 → 返回 `""`。
fn resolve_path(data: &Value, path: &str) -> String {
    let mut current = data;
    for segment in path.split('.') {
        match current {
            Value::Object(map) => match map.get(segment) {
                Some(value) => current = value,
                None => return String::new(),
            },
            Value::Array(arr) => {
                if let Ok(index) = segment.parse::<usize>() {
                    if let Some(value) = arr.get(index) {
                        current = value;
                        continue;
                    }
                }
                return String::new();
            }
            _ => return String::new(),
        }
    }
    match current {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `{{#key}}...{{/key}}` 条件块替换。
///
/// `is_truthy(key)` 返回 true 时保留块内容，否则整段删除。
/// key 的取值规则：
/// - `"noTask"` → `!vars.taskId`（无论 vars 中是否有 noTask 字段）
/// - 其他 → 数组非空 / 字符串非空 / 数字非 0 / 布尔 true
pub fn render_conditional_sections(template: &str, vars: &Value) -> String {
    let mut result = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if cursor + 2 < bytes.len()
            && bytes[cursor] == b'{'
            && bytes[cursor + 1] == b'{'
            && bytes[cursor + 2] == b'#'
        {
            // 寻找 `}}` 闭合
            if let Some(close_offset) = find_double_close(template, cursor + 3) {
                let key = template[cursor + 3..close_offset].trim().to_string();
                // 寻找 `{{/key}}`
                let end_marker = format!("{{{{/{key}}}}}");
                if let Some(end_offset) = template[close_offset + 2..].find(&end_marker) {
                    let body_start = close_offset + 2;
                    let body_end = body_start + end_offset;
                    if is_truthy(&key, vars) {
                        result.push_str(&template[body_start..body_end]);
                    }
                    cursor = body_end + end_marker.len();
                    continue;
                }
            }
        }
        // 普通字符
        let ch_end = template[cursor..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| cursor + i)
            .unwrap_or(template.len());
        result.push_str(&template[cursor..ch_end]);
        cursor = ch_end;
    }
    result
}

fn is_truthy(key: &str, vars: &Value) -> bool {
    if key == "noTask" {
        return vars
            .get("taskId")
            .and_then(Value::as_str)
            .map(str::is_empty)
            .unwrap_or(true);
    }
    match vars.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// 过滤空白段 + 用分隔符拼接（默认 `"\n\n"`）。
pub fn join_prompt_sections(sections: &[Option<&str>], separator: &str) -> String {
    sections
        .iter()
        .filter_map(|value| value.map(str::trim).filter(|s| !s.is_empty()))
        .collect::<Vec<_>>()
        .join(separator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_replaces_simple_path() {
        let template = "Hello {{name}}!";
        let data = json!({"name": "world"});
        assert_eq!(render_template(template, &data), "Hello world!");
    }

    #[test]
    fn render_replaces_dotted_path() {
        let template = "{{agent.id}} - {{run.id}}";
        let data = json!({"agent": {"id": "a-1"}, "run": {"id": "r-9"}});
        assert_eq!(render_template(template, &data), "a-1 - r-9");
    }

    #[test]
    fn render_missing_path_replaces_with_empty() {
        let template = "{{missing}} end";
        let data = json!({});
        assert_eq!(render_template(template, &data), " end");
    }

    #[test]
    fn render_supports_array_index() {
        let template = "{{list.0}}";
        let data = json!({"list": ["alpha", "beta"]});
        assert_eq!(render_template(template, &data), "alpha");
    }

    #[test]
    fn render_passes_through_literal_braces() {
        let template = "code {single} stays";
        let data = json!({});
        assert_eq!(render_template(template, &data), "code {single} stays");
    }

    #[test]
    fn render_handles_whitespace_inside_braces() {
        let template = "{{  spaced.key  }}";
        let data = json!({"spaced": {"key": "v"}});
        assert_eq!(render_template(template, &data), "v");
    }

    #[test]
    fn render_number_and_bool() {
        let template = "{{n}} {{b}}";
        let data = json!({"n": 42, "b": true});
        assert_eq!(render_template(template, &data), "42 true");
    }

    #[test]
    fn conditional_keeps_truthy_section() {
        let template = "{{#hasTask}}do work{{/hasTask}}";
        let vars = json!({"hasTask": true});
        assert_eq!(render_conditional_sections(template, &vars), "do work");
    }

    #[test]
    fn conditional_removes_falsy_section() {
        let template = "before {{#hasTask}}do work{{/hasTask}} after";
        let vars = json!({"hasTask": false});
        assert_eq!(
            render_conditional_sections(template, &vars),
            "before  after"
        );
    }

    #[test]
    fn conditional_noTask_truthy_when_task_id_absent() {
        let template = "{{#noTask}}no task{{/noTask}}";
        let vars = json!({});
        assert_eq!(render_conditional_sections(template, &vars), "no task");
    }

    #[test]
    fn conditional_noTask_falsy_when_task_id_present() {
        let template = "{{#noTask}}no task{{/noTask}}";
        let vars = json!({"taskId": "T-1"});
        assert_eq!(render_conditional_sections(template, &vars), "");
    }

    #[test]
    fn conditional_non_empty_array_is_truthy() {
        let template = "{{#list}}x{{/list}}";
        let vars = json!({"list": ["a"]});
        assert_eq!(render_conditional_sections(template, &vars), "x");
    }

    #[test]
    fn conditional_empty_array_is_falsy() {
        let template = "{{#list}}x{{/list}}";
        let vars = json!({"list": []});
        assert_eq!(render_conditional_sections(template, &vars), "");
    }

    #[test]
    fn join_filters_and_joins() {
        let a = Some("section A");
        let b = None;
        let c = Some("");
        let d = Some("section D");
        let joined = join_prompt_sections(&[a, b, c, d], "\n---\n");
        assert_eq!(joined, "section A\n---\nsection D");
    }

    #[test]
    fn join_default_separator() {
        let joined = join_prompt_sections(&[Some("a"), Some("b")], "\n\n");
        assert_eq!(joined, "a\n\nb");
    }
}
