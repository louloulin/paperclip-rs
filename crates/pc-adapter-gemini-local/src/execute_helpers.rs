//! Gemini-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/gemini-local/src/server/execute.ts`
//! 中与 billing 解析、headless env 规范化、skills 路径、提示注记相关的纯函数。

use std::collections::BTreeMap;

use pc_acpx::env_helpers::has_non_empty_env_value;

/// Gemini 的 billing 模式。
///
/// Node 等价：`resolveGeminiBillingType` 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiBillingType {
    /// API key 认证（`GEMINI_API_KEY` 或 `GOOGLE_API_KEY`）。
    Api,
    /// Gemini 订阅（无 API key）。
    Subscription,
}

impl GeminiBillingType {
    pub fn as_str(&self) -> &'static str {
        match self {
            GeminiBillingType::Api => "api",
            GeminiBillingType::Subscription => "subscription",
        }
    }
}

/// 解析 Gemini 的 billing 类型。
///
/// Node 等价：`resolveGeminiBillingType`。`GEMINI_API_KEY` 或 `GOOGLE_API_KEY`
/// 任一非空 → `Api`；否则 `Subscription`。
pub fn resolve_gemini_billing_type(env: &BTreeMap<String, String>) -> GeminiBillingType {
    if has_non_empty_env_value(env, "GEMINI_API_KEY")
        || has_non_empty_env_value(env, "GOOGLE_API_KEY")
    {
        GeminiBillingType::Api
    } else {
        GeminiBillingType::Subscription
    }
}

/// 为 headless 模式规范化环境变量。
///
/// Node 等价：`buildGeminiHeadlessEnv`：
/// - `TERM` 为空 / "dumb" / "vt100" → 设为 `"xterm-256color"`。
/// - `COLORTERM` 为空 → 设为 `"truecolor"`。
/// - 强制 `NO_BROWSER=1`。
/// - 删除 `NO_COLOR`。
/// - `GEMINI_CLI_HOME` 转绝对路径（如已设置）。
pub fn build_gemini_headless_env(
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut next: BTreeMap<String, String> = env.clone();

    let term = env
        .get("TERM")
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if term.is_empty() || term == "dumb" || term == "vt100" {
        next.insert("TERM".to_owned(), "xterm-256color".to_owned());
    }

    let colorterm = env
        .get("COLORTERM")
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if colorterm {
        next.insert("COLORTERM".to_owned(), "truecolor".to_owned());
    }

    next.insert("NO_BROWSER".to_owned(), "1".to_owned());
    next.remove("NO_COLOR");
    next
}

/// 解析 Gemini skills 目录路径。
///
/// Node 等价：`geminiSkillsHome`。返回绝对路径 `<homedir>/.gemini/skills`。
/// 提供 `homedir` 参数以便测试可注入（生产环境传 `std::env::var("HOME")`）。
pub fn gemini_skills_home(homedir: &str) -> String {
    let home_trimmed = homedir.trim_end_matches('/');
    if home_trimmed.is_empty() {
        return "/.gemini/skills".to_owned();
    }
    if home_trimmed == "/" {
        return "/.gemini/skills".to_owned();
    }
    format!("{home_trimmed}/.gemini/skills")
}

/// 渲染 Paperclip env 提示段（注入到 prompt 头部）。
///
/// Node 等价：`renderPaperclipEnvNote`：
/// - 列出所有 `PAPERCLIP_*` 变量（按字母排序）。
/// - 没变量则返空字符串。
pub fn render_paperclip_env_note(env: &BTreeMap<String, String>) -> String {
    let mut keys: Vec<&String> = env.keys().filter(|k| k.starts_with("PAPERCLIP_")).collect();
    keys.sort();
    if keys.is_empty() {
        return String::new();
    }
    let joined = keys
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Paperclip runtime note:\n\
         The following PAPERCLIP_* environment variables are available in this run: {joined}\n\
         Do not assume these variables are missing without checking your shell environment.\n\n\n"
    )
}

/// 渲染 API access 提示段（注入到 prompt 头部）。
///
/// Node 等价：`renderApiAccessNote`：当 `PAPERCLIP_API_URL` 和 `PAPERCLIP_API_KEY`
/// 都非空时，提供 curl 用法示例。
pub fn render_api_access_note(env: &BTreeMap<String, String>) -> String {
    if !has_non_empty_env_value(env, "PAPERCLIP_API_URL")
        || !has_non_empty_env_value(env, "PAPERCLIP_API_KEY")
    {
        return String::new();
    }
    format!(
        "Paperclip API access note:\n\
         Use run_shell_command with curl to make Paperclip API requests.\n\
         GET example:\n  \
           run_shell_command({{ command: \"curl -s -H \\\\\"Authorization: Bearer $PAPERCLIP_API_KEY\\\\\" \\\\\"$PAPERCLIP_API_URL/api/agents/me\\\\\"\" }})\n\
         POST/PATCH example:\n  \
           run_shell_command({{ command: \"curl -s -X POST -H \\\\\"Authorization: Bearer $PAPERCLIP_API_KEY\\\\\" -H 'Content-Type: application/json' -H \\\\\"X-Paperclip-Run-Id: $PAPERCLIP_RUN_ID\\\\\" -d '{{...}}' \\\\\"$PAPERCLIP_API_URL/api/issues/{{id}}/checkout\\\\\"\" }})\n\n\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
    }

    // -----------------------------------------------------------------
    // resolve_gemini_billing_type
    // -----------------------------------------------------------------

    #[test]
    fn billing_默认subscription() {
        assert_eq!(
            resolve_gemini_billing_type(&env_from(&[])),
            GeminiBillingType::Subscription
        );
    }

    #[test]
    fn billing_gemini_key_Api() {
        let env = env_from(&[("GEMINI_API_KEY", "test-key")]);
        assert_eq!(
            resolve_gemini_billing_type(&env),
            GeminiBillingType::Api
        );
    }

    #[test]
    fn billing_google_key_Api() {
        let env = env_from(&[("GOOGLE_API_KEY", "test-key")]);
        assert_eq!(
            resolve_gemini_billing_type(&env),
            GeminiBillingType::Api
        );
    }

    #[test]
    fn billing_两个key_Api() {
        let env = env_from(&[
            ("GEMINI_API_KEY", "test-1"),
            ("GOOGLE_API_KEY", "test-2"),
        ]);
        assert_eq!(
            resolve_gemini_billing_type(&env),
            GeminiBillingType::Api
        );
    }

    #[test]
    fn billing_key空白_Subscription() {
        let env = env_from(&[("GEMINI_API_KEY", "  ")]);
        assert_eq!(
            resolve_gemini_billing_type(&env),
            GeminiBillingType::Subscription
        );
    }

    #[test]
    fn billing_as_str_映射() {
        assert_eq!(GeminiBillingType::Api.as_str(), "api");
        assert_eq!(GeminiBillingType::Subscription.as_str(), "subscription");
    }

    // -----------------------------------------------------------------
    // build_gemini_headless_env
    // -----------------------------------------------------------------

    #[test]
    fn headless_term_空_设为xterm() {
        let env = env_from(&[]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("TERM").map(String::as_str), Some("xterm-256color"));
    }

    #[test]
    fn headless_term_dumb_设为xterm() {
        let env = env_from(&[("TERM", "dumb")]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("TERM").map(String::as_str), Some("xterm-256color"));
    }

    #[test]
    fn headless_term_vt100_设为xterm() {
        let env = env_from(&[("TERM", "vt100")]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("TERM").map(String::as_str), Some("xterm-256color"));
    }

    #[test]
    fn headless_term_xterm_保留() {
        let env = env_from(&[("TERM", "xterm-256color")]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("TERM").map(String::as_str), Some("xterm-256color"));
    }

    #[test]
    fn headless_colorterm_空_设为truecolor() {
        let env = env_from(&[]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("COLORTERM").map(String::as_str), Some("truecolor"));
    }

    #[test]
    fn headless_colorterm_已设置_保留() {
        let env = env_from(&[("COLORTERM", "24bit")]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("COLORTERM").map(String::as_str), Some("24bit"));
    }

    #[test]
    fn headless_no_browser_强制1() {
        let env = env_from(&[]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("NO_BROWSER").map(String::as_str), Some("1"));
    }

    #[test]
    fn headless_no_color_删除() {
        let env = env_from(&[("NO_COLOR", "1")]);
        let result = build_gemini_headless_env(&env);
        assert!(result.get("NO_COLOR").is_none());
    }

    #[test]
    fn headless_不变更其他变量() {
        let env = env_from(&[("PATH", "/usr/bin"), ("FOO", "bar")]);
        let result = build_gemini_headless_env(&env);
        assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(result.get("FOO").map(String::as_str), Some("bar"));
    }

    // -----------------------------------------------------------------
    // gemini_skills_home
    // -----------------------------------------------------------------

    #[test]
    fn skills_home_标准路径() {
        assert_eq!(
            gemini_skills_home("/home/u"),
            "/home/u/.gemini/skills"
        );
    }

    #[test]
    fn skills_home_尾斜杠() {
        assert_eq!(
            gemini_skills_home("/home/u/"),
            "/home/u/.gemini/skills"
        );
    }

    #[test]
    fn skills_home_根路径() {
        assert_eq!(gemini_skills_home("/"), "/.gemini/skills");
    }

    #[test]
    fn skills_home_空输入() {
        assert_eq!(gemini_skills_home(""), "/.gemini/skills");
    }

    // -----------------------------------------------------------------
    // render_paperclip_env_note
    // -----------------------------------------------------------------

    #[test]
    fn env_note_无paperclip变量_返空() {
        let env = env_from(&[("PATH", "/usr/bin")]);
        assert_eq!(render_paperclip_env_note(&env), "");
    }

    #[test]
    fn env_note_单变量() {
        let env = env_from(&[("PAPERCLIP_RUN_ID", "run-123")]);
        let note = render_paperclip_env_note(&env);
        // Node 实现只列变量名，不含值（避免敏感信息泄露到 prompt）。
        assert!(note.contains("PAPERCLIP_RUN_ID"));
        assert!(!note.contains("run-123"));
    }

    #[test]
    fn env_note_多变量_排序() {
        let env = env_from(&[
            ("PAPERCLIP_RUN_ID", "run-1"),
            ("PAPERCLIP_TASK_ID", "task-1"),
            ("PAPERCLIP_API_KEY", "key-1"),
        ]);
        let note = render_paperclip_env_note(&env);
        // 三个变量按字母排序：API_KEY < RUN_ID < TASK_ID
        assert!(note.contains("PAPERCLIP_API_KEY, PAPERCLIP_RUN_ID, PAPERCLIP_TASK_ID"));
    }

    #[test]
    fn env_note_忽略非PAPERCLIP变量() {
        let env = env_from(&[
            ("PAPERCLIP_X", "a"),
            ("NOT_PAPERCLIP", "b"),
            ("PATH", "/usr/bin"),
        ]);
        let note = render_paperclip_env_note(&env);
        assert!(note.contains("PAPERCLIP_X"));
        assert!(!note.contains("NOT_PAPERCLIP"));
        assert!(!note.contains("PATH"));
    }

    // -----------------------------------------------------------------
    // render_api_access_note
    // -----------------------------------------------------------------

    #[test]
    fn api_access_note_仅api_url_返空() {
        let env = env_from(&[("PAPERCLIP_API_URL", "https://api.test")]);
        assert_eq!(render_api_access_note(&env), "");
    }

    #[test]
    fn api_access_note_仅api_key_返空() {
        let env = env_from(&[("PAPERCLIP_API_KEY", "sk-test")]);
        assert_eq!(render_api_access_note(&env), "");
    }

    #[test]
    fn api_access_note_两者都有_返说明() {
        let env = env_from(&[
            ("PAPERCLIP_API_URL", "https://api.test"),
            ("PAPERCLIP_API_KEY", "sk-test"),
        ]);
        let note = render_api_access_note(&env);
        assert!(note.contains("Paperclip API access note"));
        assert!(note.contains("curl"));
        assert!(note.contains("GET example"));
        assert!(note.contains("POST/PATCH example"));
    }

    #[test]
    fn api_access_note_空白值_返空() {
        let env = env_from(&[
            ("PAPERCLIP_API_URL", "  "),
            ("PAPERCLIP_API_KEY", "sk-test"),
        ]);
        assert_eq!(render_api_access_note(&env), "");
    }
}
