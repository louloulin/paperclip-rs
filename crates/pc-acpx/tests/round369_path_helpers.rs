//! R369 path + claude settings + session config helpers tests.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use pc_acpx::{
    build_codex_startup_config, default_state_dir, is_compatible_session,
    normalize_gemini_acp_command_shell_with_env, paperclip_claude_settings_write_with,
    referenced_source_content_signature, render_api_access_note, render_paperclip_env_note,
    resolve_managed_codex_home_dir, resolve_paperclip_instance_root, result_error_message,
    session_config_options, unique_sorted, usage_breakdowns_equal, AcpxPreparedRuntimeLite,
    ClaudeSettingsWriteInput, CodexStartupConfigInput, PaperclipClaudeSettingsResult,
    SessionConfigOption,
};

fn unique_tempdir(label: &str) -> PathBuf {
    let pid = std::process::id();
    let uuid = uuid::Uuid::new_v4();
    std::env::temp_dir().join(format!("pc-acpx-{label}-{pid}-{uuid}"))
}

fn cleanup(dir: &PathBuf) {
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unique_sorted_dedupes_and_sorts_strings() {
    let values: Vec<Option<String>> = vec![Some("b".into()), Some("a".into()), Some("b".into())];
    assert_eq!(
        unique_sorted(values),
        vec!["a".to_string(), "b".to_string()]
    );
}

#[test]
fn unique_sorted_drops_nulls_and_empties() {
    let values: Vec<Option<String>> = vec![
        Some(String::new()),
        None,
        Some("c".into()),
        Some("a".into()),
    ];
    assert_eq!(
        unique_sorted(values),
        vec!["a".to_string(), "c".to_string()]
    );
}

#[test]
fn resolve_paperclip_instance_root_prefers_inputs() {
    let root =
        resolve_paperclip_instance_root(Some("/var/lib/paperclip"), Some("prod"), &HashMap::new())
            .expect("valid");
    assert_eq!(root, PathBuf::from("/var/lib/paperclip/instances/prod"));
}

#[test]
fn resolve_paperclip_instance_root_falls_back_to_env() {
    let mut env = HashMap::new();
    env.insert("PAPERCLIP_HOME".to_string(), "/srv/paperclip".to_string());
    env.insert("PAPERCLIP_INSTANCE_ID".to_string(), "staging".to_string());
    let root = resolve_paperclip_instance_root(None, None, &env).expect("valid");
    assert_eq!(root, PathBuf::from("/srv/paperclip/instances/staging"));
}

#[test]
fn resolve_paperclip_instance_root_uses_default_when_unset() {
    let env = HashMap::new();
    let root = resolve_paperclip_instance_root(None, None, &env).expect("valid");
    assert!(root.ends_with("instances/default"));
}

#[test]
fn resolve_paperclip_instance_root_rejects_invalid_instance_id() {
    let err = resolve_paperclip_instance_root(None, Some("../bad"), &HashMap::new())
        .expect_err("must reject path traversal");
    assert!(err.to_string().contains("PAPERCLIP_INSTANCE_ID"));
}

#[test]
fn default_state_dir_uses_company_and_agent() {
    let dir = default_state_dir("acme", "agent-007");
    assert!(dir.ends_with("companies/acme/acp-engine/agents/agent-007"));
}

#[test]
fn resolve_managed_codex_home_dir_uses_company() {
    let dir = resolve_managed_codex_home_dir("acme");
    assert!(dir.ends_with("companies/acme/codex-home"));
}

#[test]
fn normalize_gemini_acp_command_shell_passthrough_when_unparseable() {
    let normalized = normalize_gemini_acp_command_shell_with_env("echo hello", &HashMap::new());
    assert_eq!(normalized, "echo hello");
}

#[test]
fn normalize_gemini_acp_command_shell_rewrites_old_flag() {
    let mut env = HashMap::new();
    env.insert(
        "PAPERCLIP_GEMINI_VERSION_OVERRIDE".to_string(),
        "0.30.0".to_string(),
    );
    let normalized = normalize_gemini_acp_command_shell_with_env("gemini --acp", &env);
    assert_eq!(normalized, "gemini --experimental-acp");
}

#[test]
fn normalize_gemini_acp_command_shell_keeps_new_flag() {
    let mut env = HashMap::new();
    env.insert(
        "PAPERCLIP_GEMINI_VERSION_OVERRIDE".to_string(),
        "1.0.0".to_string(),
    );
    let normalized = normalize_gemini_acp_command_shell_with_env("gemini --acp", &env);
    assert_eq!(normalized, "gemini --acp");
}

#[test]
fn build_codex_startup_config_returns_null_when_no_runtime_config() {
    let out = build_codex_startup_config(CodexStartupConfigInput {
        existing_config: None,
        requested_model: "".to_string(),
        requested_thinking_effort: "".to_string(),
        fast_mode: false,
    });
    assert!(out.value.is_none());
    assert!(!out.invalid_existing_config);
}

#[test]
fn build_codex_startup_config_merges_model_into_existing() {
    let out = build_codex_startup_config(CodexStartupConfigInput {
        existing_config: Some(r#"{"sandbox":"worktree"}"#.to_string()),
        requested_model: "gpt-5".to_string(),
        requested_thinking_effort: "".to_string(),
        fast_mode: false,
    });
    assert!(!out.invalid_existing_config);
    let parsed: serde_json::Value = serde_json::from_str(&out.value.expect("value")).expect("json");
    assert_eq!(parsed["model"], "gpt-5");
    assert_eq!(parsed["sandbox"], "worktree");
}

#[test]
fn build_codex_startup_config_flags_invalid_existing() {
    let out = build_codex_startup_config(CodexStartupConfigInput {
        existing_config: Some("not-json".to_string()),
        requested_model: "gpt-5".to_string(),
        requested_thinking_effort: "".to_string(),
        fast_mode: false,
    });
    assert!(out.invalid_existing_config);
    let parsed: serde_json::Value = serde_json::from_str(&out.value.expect("value")).expect("json");
    assert_eq!(parsed["model"], "gpt-5");
}

#[test]
fn build_codex_startup_config_appends_fast_mode() {
    let out = build_codex_startup_config(CodexStartupConfigInput {
        existing_config: Some(r#"{"features":{"chat_history":true}}"#.to_string()),
        requested_model: "".to_string(),
        requested_thinking_effort: "".to_string(),
        fast_mode: true,
    });
    let parsed: serde_json::Value = serde_json::from_str(&out.value.expect("value")).expect("json");
    assert_eq!(parsed["service_tier"], "fast");
    assert_eq!(parsed["features"]["chat_history"], true);
    assert_eq!(parsed["features"]["fast_mode"], true);
}

#[test]
fn is_compatible_session_rejects_fingerprint_mismatch() {
    let mut params: HashMap<String, String> = HashMap::new();
    params.insert("configFingerprint".to_string(), "fp1".into());
    params.insert("sessionKey".to_string(), "s1".into());
    params.insert("agent".to_string(), "claude".into());
    params.insert("mode".to_string(), "persistent".into());
    params.insert("cwd".to_string(), "/work".into());
    let mut runtime =
        AcpxPreparedRuntimeLite::new("fp2", "s1", "claude", "persistent", "/work", None);
    assert!(!is_compatible_session(&params, &runtime));
    runtime.fingerprint = "fp1".into();
    assert!(is_compatible_session(&params, &runtime));
}

#[test]
fn session_config_options_skips_claude_and_codex_model() {
    let prep = AcpxPreparedRuntimeLite::new("f", "s", "claude", "persistent", "/w", None)
        .with_overrides(Some("sonnet".into()), Some("high".into()), true);
    let opts = session_config_options(&prep);
    // claude skips model only; effort + fast_mode are pushed.
    assert!(!opts.iter().any(|opt| opt.key == "model"));
    assert!(opts.iter().any(|opt| opt.key == "effort"));
    assert!(opts.iter().any(|opt| opt.key == "service_tier"));
    let gemini = AcpxPreparedRuntimeLite::new("f", "s", "gemini", "persistent", "/w", None)
        .with_overrides(Some("sonnet".into()), Some("high".into()), true);
    let opts = session_config_options(&gemini);
    assert_eq!(
        opts,
        vec![
            SessionConfigOption {
                key: "model".into(),
                value: "sonnet".into(),
            },
            SessionConfigOption {
                key: "effort".into(),
                value: "high".into(),
            },
            SessionConfigOption {
                key: "service_tier".into(),
                value: "fast".into(),
            },
            SessionConfigOption {
                key: "features.fast_mode".into(),
                value: "true".into(),
            },
        ]
    );
}

#[test]
fn result_error_message_returns_none_for_successful_turn() {
    let msg = result_error_message(&None);
    assert!(msg.is_none());
}

#[test]
fn result_error_message_returns_message_for_failed_turn() {
    let msg = result_error_message(&Some("oops".into()));
    assert_eq!(msg.as_deref(), Some("oops"));
}

#[test]
fn usage_breakdowns_equal_handles_empty_and_unequal() {
    assert!(usage_breakdowns_equal(&[][..], &[]));
    assert!(!usage_breakdowns_equal(
        &[("a".to_string(), 1.0_f64)][..],
        &[("a".to_string(), 1.0_f64), ("b".to_string(), 2.0_f64)]
    ));
}

#[test]
fn render_paperclip_env_note_lists_keys_or_returns_empty() {
    let mut env = BTreeMap::new();
    env.insert("PAPERCLIP_API_URL".into(), "x".into());
    env.insert("PAPERCLIP_RUN_ID".into(), "1".into());
    env.insert("OTHER".into(), "y".into());
    let note = render_paperclip_env_note(&env);
    assert!(note.contains("PAPERCLIP_API_URL"));
    assert!(note.contains("PAPERCLIP_RUN_ID"));
    assert!(!note.contains("OTHER"));
    let empty = render_paperclip_env_note(&BTreeMap::new());
    assert!(empty.is_empty());
}

#[test]
fn render_api_access_note_requires_both_keys() {
    let env: BTreeMap<String, String> = BTreeMap::new();
    assert!(render_api_access_note(&env).is_empty());
    let mut env = BTreeMap::new();
    env.insert("PAPERCLIP_API_URL".into(), "https://api.example/".into());
    env.insert("PAPERCLIP_API_KEY".into(), "secret".into());
    let note = render_api_access_note(&env);
    assert!(note.contains("curl"));
    assert!(note.contains("agents/me"));
    let mut with_task = env.clone();
    with_task.insert("PAPERCLIP_TASK_ID".into(), "issue-1".into());
    let note = render_api_access_note(&with_task);
    assert!(note.contains("issue-1"));
}

#[tokio::test]
async fn write_paperclip_claude_settings_creates_new_file() {
    let dir = unique_tempdir("claude-new");
    let cwd = dir.join("repo");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let input = ClaudeSettingsWriteInput {
        instance_root: "/var/lib/paperclip/instances/default".to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        state_dir: dir.join("state").to_string_lossy().to_string(),
        agent_home: dir.join("home").to_string_lossy().to_string(),
        company_id: "acme".to_string(),
    };
    let result: PaperclipClaudeSettingsResult = paperclip_claude_settings_write_with(input)
        .await
        .expect("write");
    let expected = cwd.join(".claude/settings.local.json");
    assert_eq!(result.file_path, expected.to_string_lossy().to_string());
    let on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&expected).expect("read")).expect("json");
    assert_eq!(on_disk["permissions"]["defaultMode"], "default");
    assert!(on_disk["permissions"]["allow"]
        .as_array()
        .expect("allow array")
        .iter()
        .any(|entry| entry.as_str().expect("str") == "Bash(curl:*)"));
    cleanup(&dir);
}

#[tokio::test]
async fn write_paperclip_claude_settings_preserves_existing_allow() {
    let dir = unique_tempdir("claude-merge");
    let cwd = dir.join("repo");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let settings_dir = cwd.join(".claude");
    std::fs::create_dir_all(&settings_dir).expect("dir");
    std::fs::write(
        settings_dir.join("settings.local.json"),
        r#"{"permissions":{"allow":["Bash(custom:*)"],"defaultMode":"acceptEdits"}}"#,
    )
    .expect("write");
    let input = ClaudeSettingsWriteInput {
        instance_root: "/var/lib/paperclip/instances/default".to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        state_dir: dir.join("state").to_string_lossy().to_string(),
        agent_home: dir.join("home").to_string_lossy().to_string(),
        company_id: "acme".to_string(),
    };
    let result = paperclip_claude_settings_write_with(input)
        .await
        .expect("write");
    assert!(result.allow.contains(&"Bash(custom:*)".to_string()));
    assert!(result.allow.contains(&"Bash(curl:*)".to_string()));
    assert!(!result.overrode_dont_ask);
    assert_eq!(result.default_mode, "acceptEdits");
    cleanup(&dir);
}

#[tokio::test]
async fn write_paperclip_claude_settings_overrides_dont_ask() {
    let dir = unique_tempdir("claude-dont-ask");
    let cwd = dir.join("repo");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let settings_dir = cwd.join(".claude");
    std::fs::create_dir_all(&settings_dir).expect("dir");
    std::fs::write(
        settings_dir.join("settings.local.json"),
        r#"{"permissions":{"defaultMode":"dontAsk"}}"#,
    )
    .expect("write");
    let input = ClaudeSettingsWriteInput {
        instance_root: "/var/lib/paperclip/instances/default".to_string(),
        cwd: cwd.to_string_lossy().into_owned(),
        state_dir: dir.join("state").to_string_lossy().to_string(),
        agent_home: dir.join("home").to_string_lossy().to_string(),
        company_id: "acme".to_string(),
    };
    let result = paperclip_claude_settings_write_with(input)
        .await
        .expect("write");
    assert!(result.overrode_dont_ask);
    assert_eq!(result.default_mode, "default");
    cleanup(&dir);
}

#[tokio::test]
async fn referenced_source_content_signature_changes_with_content() {
    let dir = unique_tempdir("sig");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("a.txt"), "alpha").expect("write");
    let sig_a = referenced_source_content_signature(&dir).expect("sig");
    std::fs::write(dir.join("a.txt"), "beta").expect("rewrite");
    let sig_b = referenced_source_content_signature(&dir).expect("sig");
    assert_ne!(sig_a, sig_b);
    cleanup(&dir);
}
