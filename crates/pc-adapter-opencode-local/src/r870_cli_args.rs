//! R870 — opencode_local adapter CLI args builder tests.
//!
//! Verifies that `build_opencode_exec_args` emits the expected flags
//! for the full R870 CLI surface (temperature, max-tokens, sandbox,
//! system-prompt, append-system-prompt-file, cwd, model, session).
//!
//! These tests use serde_json::json! to construct realistic adapter
//! configs; the helper avoids any process spawning.

use crate::build_opencode_exec_args;
use serde_json::json;

#[test]
fn r870_minimal_config_only_output_format() {
    let cfg = json!({});
    let args = build_opencode_exec_args(&cfg, None);
    assert_eq!(args, vec!["--output-format", "stream-json"]);
}

#[test]
fn r870_model_arg() {
    let cfg = json!({"model": "opencode-big"});
    let args = build_opencode_exec_args(&cfg, None);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"opencode-big".to_string()));
}

#[test]
fn r870_temperature_arg() {
    let cfg = json!({"temperature": 0.3});
    let args = build_opencode_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--temperature").unwrap();
    assert_eq!(args[pos + 1], "0.3");
}

#[test]
fn r870_max_tokens_arg() {
    let cfg = json!({"maxTokens": 4096u64});
    let args = build_opencode_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--max-tokens").unwrap();
    assert_eq!(args[pos + 1], "4096");
}

#[test]
fn r870_sandbox_true_emits_flag() {
    let cfg = json!({"sandbox": true});
    let args = build_opencode_exec_args(&cfg, None);
    assert!(args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_sandbox_false_omits_flag() {
    let cfg = json!({"sandbox": false});
    let args = build_opencode_exec_args(&cfg, None);
    assert!(!args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_system_prompt_arg() {
    let cfg = json!({"systemPrompt": "Stay terse."});
    let args = build_opencode_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--system-prompt").unwrap();
    assert_eq!(args[pos + 1], "Stay terse.");
}

#[test]
fn r870_system_prompt_empty_omits_flag() {
    let cfg = json!({"systemPrompt": ""});
    let args = build_opencode_exec_args(&cfg, None);
    assert!(!args.contains(&"--system-prompt".to_string()));
}

#[test]
fn r870_append_system_prompt_file_arg() {
    let cfg = json!({"appendSystemPromptFile": "/etc/prompt.md"});
    let args = build_opencode_exec_args(&cfg, None);
    let pos = args
        .iter()
        .position(|a| a == "--append-system-prompt-file")
        .unwrap();
    assert_eq!(args[pos + 1], "/etc/prompt.md");
}

#[test]
fn r870_cwd_arg() {
    let cfg = json!({"cwd": "/tmp/work"});
    let args = build_opencode_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--cwd").unwrap();
    assert_eq!(args[pos + 1], "/tmp/work");
}

#[test]
fn r870_session_appended_last() {
    let cfg = json!({"model": "opencode-fast"});
    let args = build_opencode_exec_args(&cfg, Some("sess-xyz"));
    assert_eq!(args.last().unwrap(), "sess-xyz");
    assert!(args[args.len() - 2] == "--session");
}

#[test]
fn r870_session_whitespace_trimmed_or_dropped() {
    let cfg = json!({});
    let args = build_opencode_exec_args(&cfg, Some("   "));
    assert!(!args.contains(&"--session".to_string()));
}

#[test]
fn r870_extra_args_before_session() {
    let cfg = json!({"extraArgs": ["--verbose", "--debug"]});
    let args = build_opencode_exec_args(&cfg, Some("sess-1"));
    let pos_session = args.iter().position(|a| a == "--session").unwrap();
    let pos_verbose = args.iter().position(|a| a == "--verbose").unwrap();
    let pos_debug = args.iter().position(|a| a == "--debug").unwrap();
    assert!(pos_verbose < pos_session);
    assert!(pos_debug < pos_session);
}

#[test]
fn r870_full_config_ordering_and_presence() {
    let cfg = json!({
        "model": "opencode-big",
        "temperature": 0.5,
        "maxTokens": 8192u64,
        "sandbox": true,
        "systemPrompt": "Be helpful.",
        "appendSystemPromptFile": "/etc/p.md",
        "cwd": "/work"
    });
    let args = build_opencode_exec_args(&cfg, None);

    for flag in [
        "--output-format",
        "stream-json",
        "--model",
        "opencode-big",
        "--temperature",
        "0.5",
        "--max-tokens",
        "8192",
        "--sandbox",
        "--system-prompt",
        "Be helpful.",
        "--append-system-prompt-file",
        "/etc/p.md",
        "--cwd",
        "/work",
    ] {
        assert!(
            args.contains(&flag.to_string()),
            "missing {flag} in args: {args:?}"
        );
    }
    // Output format must come first (before --model) so the model can't
    // change output semantics.
    let pos_output = args.iter().position(|a| a == "--output-format").unwrap();
    let pos_model = args.iter().position(|a| a == "--model").unwrap();
    assert!(pos_output < pos_model);
}