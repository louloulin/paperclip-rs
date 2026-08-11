//! Cursor Cloud prompt rendering — 对齐 Node `buildPrompt` / `renderTemplate`
//! (`packages/adapter-utils/server-utils.ts`)。
//!
//! 拼接顺序（按 Node）：
//! 1. instructions file contents（若提供）
//! 2. bootstrapPromptTemplate 渲染结果（仅在 **没有** resuming session 时）
//! 3. wakePrompt（若 `context.paperclipWake` 存在 / recovery wake）
//! 4. paperclipEnvNote
//! 5. promptTemplate 渲染结果（若可复用 session + wake 存在时为空）
//! 6. sessionHandoffMarkdown（追加）

#![allow(dead_code)]

use serde_json::{json, Map, Value};

use crate::wake_env::{paperclip_keys, WakeEnvOutput};

/// 模板里可替换的变量集合。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TemplateContext {
    pub agent_id: String,
    pub company_id: String,
    pub run_id: String,
    pub agent: Value,
}

impl TemplateContext {
    pub fn from_agent_run(agent: &Value, run_id: &str) -> Self {
        Self {
            agent_id: agent
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            company_id: agent
                .get("companyId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned(),
            run_id: run_id.to_owned(),
            agent: agent.clone(),
        }
    }

    pub fn to_template_data(&self) -> Value {
        json!({
            "agentId": self.agent_id,
            "companyId": self.company_id,
            "runId": self.run_id,
            "company": {"id": self.company_id},
            "agent": self.agent,
            "run": {"id": self.run_id, "source": "on_demand"},
        })
    }
}

/// `{{ var }}` / `{{ a.b }}` 风格的简单模板渲染。
pub fn render_template(template: &str, ctx: &Value) -> String {
    let mut out = template.to_owned();
    // 简单两层 var：`{{ a.b }}` 用 `a.get("b")`；否则 `{{ var }}` 用 `var`.
    while let Some(start) = out.find("{{") {
        let Some(end_rel) = out[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + end_rel;
        let path = out[start + 2..end].trim().to_owned();
        let replacement = resolve_path(&path, ctx);
        out.replace_range(start..end + 2, &replacement);
    }
    out
}

fn resolve_path(path: &str, ctx: &Value) -> String {
    let mut current = ctx.clone();
    for segment in path.split('.') {
        match current.get(segment) {
            Some(next) => current = next.clone(),
            None => return String::new(),
        }
    }
    match current {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

/// 拼接多个字符串段，跳过空白段（对齐 `joinPromptSections`）。
pub fn join_prompt_sections(parts: &[&str]) -> String {
    let mut buf = String::new();
    for (i, part) in parts.iter().enumerate() {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !buf.is_empty() {
            buf.push('\n');
        }
        buf.push_str(trimmed);
        let _ = i;
    }
    buf
}

/// 决策：是否复用 session（基于 Node `sessionMatches` + 上下文字段）。
pub fn should_resume_session(can_reuse_session: bool, recovery_wake: bool) -> bool {
    can_reuse_session && !recovery_wake
}

/// 构造 prompt 拼接所需的所有字段（从 Runtime 函数直接调用）。
#[derive(Debug, Clone)]
pub struct PromptParts<'a> {
    pub instructions_prefix: &'a str,
    pub bootstrap_prompt: &'a str,
    pub wake_prompt: &'a str,
    pub env_note: &'a str,
    pub rendered_prompt: &'a str,
    pub session_handoff: &'a str,
}

/// 拼接最终 prompt（对齐 Node `joinPromptSections` 顺序）。
pub fn assemble(parts: &PromptParts<'_>) -> String {
    join_prompt_sections(&[
        parts.instructions_prefix,
        parts.bootstrap_prompt,
        parts.wake_prompt,
        parts.env_note,
        parts.rendered_prompt,
    ])
}

/// 拼接最终 prompt + handoff append。
pub fn assemble_with_handoff(parts: &PromptParts<'_>) -> String {
    let base = assemble(parts);
    join_prompt_sections(&[&base, parts.session_handoff])
}

/// 渲染 PAPERCLIP_WORKSPACE env note 子句。
pub fn env_note_from_wake_env(out: &WakeEnvOutput) -> String {
    let keys = paperclip_keys(&out.env);
    if keys.is_empty() {
        return String::new();
    }
    format!(
        "Paperclip runtime note:\nThe following PAPERCLIP_* environment variables are available in the cloud agent shell: {}\nUse them directly instead of assuming they are absent.",
        keys.join(", ")
    )
}

/// Helper — 从 config 解析 bool 默认（trim/lower-aware）。
pub fn read_bool(v: Option<&Value>, default: bool) -> bool {
    let Some(v) = v else {
        return default;
    };
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => {
            let t = s.trim().to_lowercase();
            matches!(t.as_str(), "1" | "true" | "yes" | "on" | "enabled")
        }
        _ => default,
    }
}

/// Helper — 从 config 解析 trim 字符串。
pub fn read_trimmed_string(v: Option<&Value>) -> Option<String> {
    v.and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Helper — 把 model 字符串换成 SDK ModelSelection 形状。
pub fn to_model_selection(raw: &str) -> Option<Map<String, Value>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        let mut map = Map::new();
        map.insert("id".into(), json!(trimmed));
        Some(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_template_substitutes_simple_var() {
        let ctx = json!({"agentId": "a-1", "companyId": "c-1"});
        let s = render_template("hello {{ agentId }} from {{ companyId }}", &ctx);
        assert_eq!(s, "hello a-1 from c-1");
    }

    #[test]
    fn render_template_resolves_nested_path() {
        let ctx = json!({"company": {"id": "c-9"}});
        let s = render_template("company={{ company.id }}", &ctx);
        assert_eq!(s, "company=c-9");
    }

    #[test]
    fn render_template_missing_var_yields_empty() {
        let ctx = json!({});
        let s = render_template("a={{ x }} b={{ y }}", &ctx);
        assert_eq!(s, "a= b=");
    }

    #[test]
    fn join_sections_skips_empty_segments() {
        let s = join_prompt_sections(&["", "  ", "first", "", "second"]);
        assert_eq!(s, "first\nsecond");
    }

    #[test]
    fn join_sections_single_segment() {
        let s = join_prompt_sections(&["only"]);
        assert_eq!(s, "only");
    }

    #[test]
    fn should_resume_session_when_reuse_true_and_no_recovery() {
        assert!(should_resume_session(true, false));
        assert!(!should_resume_session(true, true));
        assert!(!should_resume_session(false, false));
    }

    #[test]
    fn assemble_orders_segments_with_skip() {
        let parts = PromptParts {
            instructions_prefix: "INST",
            bootstrap_prompt: "",
            wake_prompt: "WAKE",
            env_note: "",
            rendered_prompt: "PROMPT",
            session_handoff: "HANDOFF",
        };
        let s = assemble(&parts);
        assert_eq!(s, "INST\nWAKE\nPROMPT");
        let full = assemble_with_handoff(&parts);
        assert_eq!(full, "INST\nWAKE\nPROMPT\nHANDOFF");
    }

    #[test]
    fn read_bool_handles_aliases_and_defaults() {
        assert!(read_bool(Some(&json!(true)), false));
        assert!(!read_bool(Some(&json!(false)), true));
        assert!(read_bool(Some(&json!("yes")), false));
        assert!(read_bool(Some(&json!("on")), false));
        assert!(read_bool(Some(&json!("1")), false));
        assert!(!read_bool(Some(&json!("nope")), true));
        assert!(read_bool(None, true));
    }

    #[test]
    fn read_trimmed_string_skips_empty() {
        assert!(read_trimmed_string(None).is_none());
        assert!(read_trimmed_string(Some(&json!(""))).is_none());
        assert_eq!(
            read_trimmed_string(Some(&json!("  hello  "))).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn to_model_selection_returns_id_map_when_present() {
        assert!(to_model_selection("").is_none());
        assert!(to_model_selection("   ").is_none());
        let m = to_model_selection("gpt-4").unwrap();
        assert_eq!(m.get("id").unwrap(), "gpt-4");
    }

    #[test]
    fn env_note_from_wake_env_handles_empty() {
        let mut env = Map::new();
        env.insert("DEBUG".into(), json!("true"));
        let s = env_note_from_wake_env(&WakeEnvOutput {
            env,
            dropped_keys: vec![],
        });
        assert!(s.is_empty());
    }

    #[test]
    fn env_note_from_wake_env_includes_keys() {
        let mut env = Map::new();
        env.insert("PAPERCLIP_AGENT_ID".into(), json!("a-1"));
        env.insert("PAPERCLIP_TASK_ID".into(), json!("t-1"));
        env.insert("OTHER".into(), json!("o"));
        let s = env_note_from_wake_env(&WakeEnvOutput {
            env,
            dropped_keys: vec![],
        });
        assert!(s.contains("PAPERCLIP_AGENT_ID, PAPERCLIP_TASK_ID"));
    }

    #[test]
    fn template_context_from_agent_run_sets_fields() {
        let agent = json!({"id": "a-1", "companyId": "c-9", "name": "Foo"});
        let ctx = TemplateContext::from_agent_run(&agent, "r-1");
        assert_eq!(ctx.agent_id, "a-1");
        assert_eq!(ctx.company_id, "c-9");
        assert_eq!(ctx.run_id, "r-1");
        let data = ctx.to_template_data();
        assert_eq!(data["agentId"], "a-1");
        assert_eq!(data["run"]["id"], "r-1");
    }
}
