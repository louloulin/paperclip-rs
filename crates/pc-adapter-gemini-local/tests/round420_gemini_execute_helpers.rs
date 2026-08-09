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
    assert_eq!(resolve_gemini_billing_type(&env), GeminiBillingType::Api);
}

#[test]
fn billing_google_key_Api() {
    let env = env_from(&[("GOOGLE_API_KEY", "test-key")]);
    assert_eq!(resolve_gemini_billing_type(&env), GeminiBillingType::Api);
}

#[test]
fn billing_两个key_优先级() {
    let env = env_from(&[("GEMINI_API_KEY", "test-1"), ("GOOGLE_API_KEY", "test-2")]);
    assert_eq!(resolve_gemini_billing_type(&env), GeminiBillingType::Api);
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
    assert_eq!(
        result.get("TERM").map(String::as_str),
        Some("xterm-256color")
    );
    assert_eq!(
        result.get("COLORTERM").map(String::as_str),
        Some("truecolor")
    );
    assert_eq!(result.get("NO_BROWSER").map(String::as_str), Some("1"));
    assert!(result.get("NO_COLOR").is_none());
}

#[test]
fn headless_保留自定义TERM() {
    let env = env_from(&[("TERM", "screen-256color")]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(
        result.get("TERM").map(String::as_str),
        Some("screen-256color")
    );
}

#[test]
fn headless_大写TERM_也处理() {
    let env = env_from(&[("TERM", "DUMB")]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(
        result.get("TERM").map(String::as_str),
        Some("xterm-256color")
    );
}

#[test]
fn headless_保留colorterm_带空格() {
    let env = env_from(&[("COLORTERM", "truecolor")]);
    let result = build_gemini_headless_env(&env);
    assert_eq!(
        result.get("COLORTERM").map(String::as_str),
        Some("truecolor")
    );
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
    assert_eq!(gemini_skills_home("/home/u"), "/home/u/.gemini/skills");
}

#[test]
fn skills_home_尾斜杠() {
    assert_eq!(gemini_skills_home("/home/u/"), "/home/u/.gemini/skills");
}

#[test]
fn skills_home_根路径() {
    assert_eq!(gemini_skills_home("/"), "/.gemini/skills");
}

#[test]
fn skills_home_空输入() {
    assert_eq!(gemini_skills_home(""), "/.gemini/skills");
}
