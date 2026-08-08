//! R417 — Integration tests for `pc-adapter-pi-local::execute_helpers`.
//!
//! Mirrors Node `packages/adapters/pi-local/src/server/execute.ts`:
//! - `parseModelProvider` (L69-74)
//! - `parseModelId` (L76-81)
//! - `resolvePiBiller` (L135-137)
//! - `readSessionHeaderCwd` (L162-176)
//! - `normalizeExecutionCwd` (L154-156)
//! - `executionCwdsMatch` (L158-160)
//!
//! Unit tests inside `execute_helpers::tests` cover each function in isolation;
//! this integration suite verifies the complete helper API surface end-to-end.

use pc_adapter_pi_local::{
    cwds_match, model_id, model_provider, normalize_cwd, parse_session_header_cwd,
    resolve_pi_biller, should_clear_session, should_resume,
};
use std::collections::BTreeMap;

fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

// ---------------------------------------------------------------------------
// model_provider / model_id
// ---------------------------------------------------------------------------

#[test]
fn model_provider_id_标准拆分() {
    assert_eq!(
        model_provider(Some("anthropic/claude-sonnet-4")),
        Some("anthropic".to_owned())
    );
    assert_eq!(
        model_id(Some("anthropic/claude-sonnet-4")),
        Some("claude-sonnet-4".to_owned())
    );
}

#[test]
fn model_provider_id_带空格() {
    // Node 行为：trim 各部分。
    assert_eq!(
        model_provider(Some("  openai / gpt-5  ")),
        Some("openai".to_owned())
    );
    assert_eq!(
        model_id(Some("  openai / gpt-5  ")),
        Some("gpt-5".to_owned())
    );
}

#[test]
fn model_provider_id_无斜杠() {
    // 无 "/" 时 provider=None，id=整串。
    assert_eq!(model_provider(Some("claude-sonnet-4")), None);
    assert_eq!(
        model_id(Some("claude-sonnet-4")),
        Some("claude-sonnet-4".to_owned())
    );
}

#[test]
fn model_provider_空前缀() {
    // "/foo" → provider="" → None。
    assert_eq!(model_provider(Some("/foo")), None);
}

#[test]
fn model_id_空后缀() {
    // "anthropic/" → id="" → None。
    assert_eq!(model_id(Some("anthropic/")), None);
}

#[test]
fn model_provider_id_空输入() {
    assert_eq!(model_provider(None), None);
    assert_eq!(model_provider(Some("")), None);
    assert_eq!(model_provider(Some("   ")), None);
    assert_eq!(model_id(None), None);
    assert_eq!(model_id(Some("")), None);
    assert_eq!(model_id(Some("   ")), None);
}

#[test]
fn model_provider_id_多斜杠() {
    // 只取第一个 "/" 切分。
    assert_eq!(
        model_provider(Some("a/b/c")),
        Some("a".to_owned())
    );
    assert_eq!(
        model_id(Some("a/b/c")),
        Some("b/c".to_owned())
    );
}

// ---------------------------------------------------------------------------
// resolve_pi_biller
// ---------------------------------------------------------------------------

#[test]
fn biller_默认unknown() {
    assert_eq!(resolve_pi_biller(&env_from(&[]), None), "unknown");
}

#[test]
fn biller_provider_fallback() {
    assert_eq!(
        resolve_pi_biller(&env_from(&[]), Some("anthropic")),
        "anthropic"
    );
}

#[test]
fn biller_openrouter_env优先() {
    let env = env_from(&[("OPENROUTER_API_KEY", "sk-or-test")]);
    assert_eq!(
        resolve_pi_biller(&env, Some("anthropic")),
        "openrouter"
    );
}

#[test]
fn biller_openrouter_env空值不触发() {
    let env = env_from(&[("OPENROUTER_API_KEY", "")]);
    assert_eq!(
        resolve_pi_biller(&env, Some("anthropic")),
        "anthropic"
    );
}

// ---------------------------------------------------------------------------
// parse_session_header_cwd
// ---------------------------------------------------------------------------

#[test]
fn session_header_合法() {
    let raw = r#"{"type":"session","cwd":"/home/u/proj","timestamp":"2026-08-08T00:00:00Z"}
{"type":"message","role":"assistant","content":"hi"}"#;
    assert_eq!(
        parse_session_header_cwd(raw).as_deref(),
        Some("/home/u/proj")
    );
}

#[test]
fn session_header_type不匹配() {
    let raw = r#"{"type":"message","role":"assistant","cwd":"/x"}"#;
    assert_eq!(parse_session_header_cwd(raw), None);
}

#[test]
fn session_header_损坏JSON() {
    assert_eq!(parse_session_header_cwd("not-json"), None);
    assert_eq!(parse_session_header_cwd("{broken"), None);
}

#[test]
fn session_header_空cwd() {
    let raw = r#"{"type":"session","cwd":""}"#;
    assert_eq!(parse_session_header_cwd(raw), None);
}

#[test]
fn session_header_无cwd字段() {
    let raw = r#"{"type":"session","timestamp":"2026-08-08"}"#;
    assert_eq!(parse_session_header_cwd(raw), None);
}

#[test]
fn session_header_空输入与全空行() {
    assert_eq!(parse_session_header_cwd(""), None);
    assert_eq!(parse_session_header_cwd("\n\n\n"), None);
}

#[test]
fn session_header_忽略前导空行() {
    let raw = "\n\n  {\"type\":\"session\",\"cwd\":\"/a\"}  \n";
    assert_eq!(
        parse_session_header_cwd(raw).as_deref(),
        Some("/a")
    );
}

// ---------------------------------------------------------------------------
// normalize_cwd / cwds_match
// ---------------------------------------------------------------------------

#[test]
fn normalize_cwd_绝对路径基本() {
    assert_eq!(normalize_cwd("/a/b/c"), "/a/b/c");
    assert_eq!(normalize_cwd("/a/./b"), "/a/b");
    assert_eq!(normalize_cwd("/a/b/../c"), "/a/c");
}

#[test]
fn normalize_cwd_相对路径基本() {
    assert_eq!(normalize_cwd("a/b"), "a/b");
    assert_eq!(normalize_cwd("./a/b"), "a/b");
    assert_eq!(normalize_cwd("a/./b"), "a/b");
    assert_eq!(normalize_cwd("a/b/../c"), "a/c");
}

#[test]
fn normalize_cwd_根路径() {
    assert_eq!(normalize_cwd("/"), "/");
    assert_eq!(normalize_cwd("/."), "/");
    assert_eq!(normalize_cwd("/.."), "/");
}

#[test]
fn normalize_cwd_空输入() {
    assert_eq!(normalize_cwd(""), ".");
}

#[test]
fn cwds_match_同一路径() {
    assert!(cwds_match("/a/b", "/a/b"));
    assert!(cwds_match("a/b", "a/b"));
}

#[test]
fn cwds_match_规范化后一致() {
    assert!(cwds_match("/a/./b", "/a/b"));
    assert!(cwds_match("/a/b/../c", "/a/c"));
    assert!(cwds_match("a/./b", "a/b"));
}

#[test]
fn cwds_match_不一致() {
    assert!(!cwds_match("/a/b", "/a/c"));
    assert!(!cwds_match("/a/b", "/a/b/c"));
    assert!(!cwds_match("/a", "/b"));
}

#[test]
fn cwds_match_大小写敏感() {
    // POSIX 区分大小写；保持严格区分（与 Node `path.resolve` 一致）。
    assert!(!cwds_match("/a/B", "/a/b"));
}

// ---------------------------------------------------------------------------
// should_clear_session / should_resume
// ---------------------------------------------------------------------------

#[test]
fn should_clear_触发场景() {
    assert!(should_clear_session("", "unknown session id: abc"));
    assert!(should_clear_session("Session not found", ""));
    assert!(should_clear_session("", "no session available"));
    assert!(should_clear_session("session abc not found", ""));
}

#[test]
fn should_clear_不触发场景() {
    assert!(!should_clear_session("ok", ""));
    assert!(!should_clear_session("", "ok"));
    assert!(!should_clear_session("session ready", "ready"));
}

#[test]
fn should_resume_cwd一致() {
    assert!(should_resume(Some("/home/u/proj"), "/home/u/proj"));
    assert!(should_resume(Some("/home/u/proj/."), "/home/u/proj"));
}

#[test]
fn should_resume_cwd不一致() {
    assert!(!should_resume(Some("/home/u/proj"), "/home/u/other"));
    assert!(!should_resume(Some("/home/u/proj"), "/home/u/proj/sub"));
}

#[test]
fn should_resume_saved为None或空() {
    assert!(!should_resume(None, "/any"));
    assert!(!should_resume(Some(""), "/any"));
    assert!(!should_resume(Some("   "), "/any"));
}

// ---------------------------------------------------------------------------
// 综合场景
// ---------------------------------------------------------------------------

#[test]
fn resume_完整决策流程() {
    // 模拟 Node pi execute.ts 中的 resume 决策：
    // 1. 读 session header cwd。
    // 2. 与当前 cwd 比对。
    // 3. 若匹配且无 unknown session 错，则可 resume；否则 clear_session。
    let session_raw = r#"{"type":"session","cwd":"/work/proj"}"#;
    let current_cwd = "/work/proj";

    let saved_cwd = parse_session_header_cwd(session_raw);
    assert_eq!(saved_cwd.as_deref(), Some("/work/proj"));

    let can_resume = should_resume(saved_cwd.as_deref(), current_cwd);
    assert!(can_resume);

    // 假设首次 --resume 成功，无 unknown session error：
    let stdout = r#"{"type":"turn_end","sessionId":"abc","message":{"role":"assistant","content":"ok"}}"#;
    let stderr = "";
    assert!(!should_clear_session(stdout, stderr));
}

#[test]
fn resume_失败_触发_clear_session() {
    // 模拟首次 resume 失败，pi 报 unknown session。
    let stdout = "";
    let stderr = "Error: unknown session id: abc-123";

    // 不再尝试 resume，直接 clear_session=true。
    assert!(should_clear_session(stdout, stderr));
}
