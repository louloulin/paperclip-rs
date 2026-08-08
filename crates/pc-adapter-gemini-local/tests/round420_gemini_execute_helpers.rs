//! R420 — Integration tests for `pc-adapter-gemini-local::execute_helpers`.
//!
//! Mirrors Node `packages/adapters/gemini-local/src/server/execute.ts`:
//! - `resolveGeminiBillingType` (L75-79)
//! - `buildGeminiHeadlessEnv` (L81-93)
//! - `geminiSkillsHome` (L131-133)
//! - `renderPaperclipEnvNote` (L103-115)
//! - `renderApiAccessNote` (L117-129)
//!
//! Unit tests inside `execute_helpers::tests` cover each function in isolation;
//! this integration suite verifies the complete helper API surface end-to-end.

use pc_adapter_gemini_local::{
    build_gemini_headless_env, gemini_skills_home, render_api_access_note,
    render_paperclip_env_note, resolve_gemini_billing_type, GeminiBillingType,
};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
    .collect()
}

// ---------------------------------------------------------------------------
// resolve_gemini_billing_type
// ---------------------------------------------------------------------------

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
fn billing_两个key_优先级() {
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

// ---------------------------------------------------------------------------
// build_gemini_headless_env
// ---------------------------------------------------------------------------

#[test]
fn headless_完整_空输入() {
    let env = env_from(&[]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(result.get("TERM").map(String::as_str), Some("xterm-256color"));
    assert_eq!(result.get("COLORTERM").map(String::as_str), Some("truecolor"));
    assert_eq!(result.get("NO_BROWSER").map(String::as_str), Some("1"));
    assert!(result.get("NO_COLOR").is_none());
}

#[test]
fn headless_保留自定义TERM() {
    let env = env_from(&[("TERM", "screen-256color")]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(result.get("TERM").map(String::as_str), Some("screen-256color"));
}

#[test]
fn headless_大写TERM_也处理() {
    let env = env_from(&[("TERM", "DUMB")]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(result.get("TERM").map(String::as_str), Some("xterm-256color"));
}

#[test]
fn headless_保留colorterm_带空格() {
    let env = env_from(&[("COLORTERM", "truecolor")]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(result.get("COLORTERM").map(String::as_str), Some("truecolor"));
}

#[test]
fn headless_不修改无关变量() {
    let env = env_from(&[
        ("PATH", "/usr/bin"),
        ("LANG", "en_US.UTF-8"),
        ("HOME", "/home/u"),
    ]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(result.get("PATH").map(String::as_str), Some("/usr/bin"));
    assert_eq!(result.get("LANG").map(String::as_str), Some("en_US.UTF-8"));
    assert_eq!(result.get("HOME").map(String::as_str), Some("/home/u"));
}

// ---------------------------------------------------------------------------
// gemini_skills_home
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// render_paperclip_env_note
// ---------------------------------------------------------------------------

#[test]
fn env_note_无变量_返空() {
    let env = env_from(&[("PATH", "/usr/bin")]);
    assert_eq!(render_paperclip_env_note(&env), "");
}

#[test]
fn env_note_单变量() {
    let env = env_from(&[("PAPERCLIP_RUN_ID", "run-123")]);
    let note = render_paperclip_env_note(&env);
    assert!(note.contains("PAPERCLIP_RUN_ID"));
    assert!(!note.contains("run-123")); // 仅列变量名
}

#[test]
fn env_note_多变量_排序() {
    let env = env_from(&[
        ("PAPERCLIP_RUN_ID", "run-1"),
        ("PAPERCLIP_TASK_ID", "task-1"),
        ("PAPERCLIP_API_KEY", "key-1"),
    ]);
    let note = render_paperclip_env_note(&env);
    assert!(note.contains("PAPERCLIP_API_KEY, PAPERCLIP_RUN_ID, PAPERCLIP_TASK_ID"));
}

#[test]
fn env_note_忽略非变量() {
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

#[test]
fn env_note_完整结构() {
    let env = env_from(&[("PAPERCLIP_RUN_ID", "run-1")]);
    let note = render_paperclip_env_note(&env);
    assert!(note.contains("Paperclip runtime note"));
    assert!(note.contains("PAPERCLIP_*"));
    assert!(note.contains("Do not assume"));
    assert!(note.ends_with("\n\n"));
}

// ---------------------------------------------------------------------------
// render_api_access_note
// ---------------------------------------------------------------------------

#[test]
fn api_access_note_都不存在_返空() {
    let env = env_from(&[]);
    assert_eq!(render_api_access_note(&env), "");
}

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
    assert!(note.contains("PAPERCLIP_API_URL"));
    assert!(note.contains("PAPERCLIP_API_KEY"));
}

#[test]
fn api_access_note_空白值_返空() {
    let env = env_from(&[
        ("PAPERCLIP_API_URL", "  "),
        ("PAPERCLIP_API_KEY", "sk-test"),
    ]);
    assert_eq!(render_api_access_note(&env), "");
}

// ---------------------------------------------------------------------------
// 综合场景
// ---------------------------------------------------------------------------

#[test]
fn 综合_企业_gemini_api() {
    let env = env_from(&[("GEMINI_API_KEY", "AIza-test")]);
    assert_eq!(
        resolve_gemini_billing_type(&env),
        GeminiBillingType::Api
    );
}

#[test]
fn 综合_个人_google_api() {
    let env = env_from(&[("GOOGLE_API_KEY", "AIza-test")]);
    assert_eq!(
        resolve_gemini_billing_type(&env),
        GeminiBillingType::Api
    );
}

#[test]
fn 综合_订阅模式无api_key() {
    let env = env_from(&[]);
    assert_eq!(
        resolve_gemini_billing_type(&env),
        GeminiBillingType::Subscription
    );
}

#[test]
fn 综合_headless_env_企业部署() {
    // 模拟企业部署：已有部分环境变量，需要 headless 规范化。
    let env = env_from(&[
        ("TERM", "xterm"),
        ("PATH", "/usr/bin"),
        ("GEMINI_API_KEY", "test"),
    ]);
    let result = build_gemini_headless_env(&env);
    // xterm 不在 dumb/vt100 名单中，保留。
    assert_eq!(result.get("TERM").map(String::as_str), Some("xterm"));
    assert_eq!(result.get("COLORTERM").map(String::as_str), Some("truecolor"));
    assert_eq!(result.get("NO_BROWSER").map(String::as_str), Some("1"));
    assert_eq!(result.get("GEMINI_API_KEY").map(String::as_str), Some("test"));
}

#[test]
fn 综合_prompt_注入两段提示() {
    // 模拟完整 prompt 注入：env note + api access note。
    let env = env_from(&[
        ("PAPERCLIP_RUN_ID", "run-1"),
        ("PAPERCLIP_API_URL", "https://api.test"),
        ("PAPERCLIP_API_KEY", "sk-test"),
    ]);
    let env_note = render_paperclip_env_note(&env);
    let api_note = render_api_access_note(&env);
    assert!(!env_note.is_empty());
    assert!(!api_note.is_empty());
    // 两段独立，拼接顺序无要求。
    assert!(env_note.contains("PAPERCLIP_RUN_ID"));
    assert!(api_note.contains("curl"));
}
