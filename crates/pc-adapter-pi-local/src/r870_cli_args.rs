//! R870 — pi_local adapter CLI args builder tests.
//!
//! Verifies that `build_pi_exec_args` emits the expected flags for the
//! full R870 CLI surface (temperature, max-tokens, sandbox, system-
//! prompt, append-system-prompt-file, cwd, model).
//!
//! These tests use serde_json::json! to construct realistic adapter
//! configs; the helper avoids any process spawning.

use crate::build_pi_exec_args;
use serde_json::json;

#[test]
fn r870_minimal_config_only_output_format() {
    let cfg = json!({});
    let args = build_pi_exec_args(&cfg);
    assert_eq!(args, vec!["--output-format", "stream-json"]);
}

#[test]
fn r870_model_arg() {
    let cfg = json!({"model": "claude-sonnet-4"});
    let args = build_pi_exec_args(&cfg);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"claude-sonnet-4".to_string()));
}

#[test]
fn r870_temperature_arg() {
    let cfg = json!({"temperature": 0.7});
    let args = build_pi_exec_args(&cfg);
    let pos = args.iter().position(|a| a == "--temperature").unwrap();
    assert_eq!(args[pos + 1], "0.7");
}

#[test]
fn r870_max_tokens_arg() {
    let cfg = json!({"maxTokens": 4096u64});
    let args = build_pi_exec_args(&cfg);
    let pos = args.iter().position(|a| a == "--max-tokens").unwrap();
    assert_eq!(args[pos + 1], "4096");
}

#[test]
fn r870_sandbox_true_emits_flag() {
    let cfg = json!({"sandbox": true});
    let args = build_pi_exec_args(&cfg);
    assert!(args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_sandbox_false_omits_flag() {
    let cfg = json!({"sandbox": false});
    let args = build_pi_exec_args(&cfg);
    assert!(!args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_system_prompt_arg() {
    let cfg = json!({"systemPrompt": "Be terse."});
    let args = build_pi_exec_args(&cfg);
    let pos = args.iter().position(|a| a == "--system-prompt").unwrap();
    assert_eq!(args[pos + 1], "Be terse.");
}

#[test]
fn r870_system_prompt_empty_omits_flag() {
    let cfg = json!({"systemPrompt": ""});
    let args = build_pi_exec_args(&cfg);
    assert!(!args.contains(&"--system-prompt".to_string()));
}

#[test]
fn r870_append_system_prompt_file_arg() {
    let cfg = json!({"appendSystemPromptFile": "/etc/pi/system.md"});
    let args = build_pi_exec_args(&cfg);
    let pos = args
        .iter()
        .position(|a| a == "--append-system-prompt-file")
        .unwrap();
    assert_eq!(args[pos + 1], "/etc/pi/system.md");
}

#[test]
fn r870_cwd_arg() {
    let cfg = json!({"cwd": "/tmp/work"});
    let args = build_pi_exec_args(&cfg);
    let pos = args.iter().position(|a| a == "--cwd").unwrap();
    assert_eq!(args[pos + 1], "/tmp/work");
}

#[test]
fn r870_extra_args_appended_at_end() {
    let cfg = json!({"extraArgs": ["--verbose", "--debug"]});
    let args = build_pi_exec_args(&cfg);
    let pos_verbose = args.iter().position(|a| a == "--verbose").unwrap();
    let pos_debug = args.iter().position(|a| a == "--debug").unwrap();
    assert_eq!(pos_debug, pos_verbose + 1);
    // Both come AFTER --output-format.
    let pos_output = args.iter().position(|a| a == "--output-format").unwrap();
    assert!(pos_verbose > pos_output);
}

#[test]
fn r870_full_config_ordering_and_presence() {
    let cfg = json!({
        "model": "claude-sonnet-4",
        "temperature": 0.5,
        "maxTokens": 8192u64,
        "sandbox": true,
        "systemPrompt": "Be helpful.",
        "appendSystemPromptFile": "/etc/p.md",
        "cwd": "/work"
    });
    let args = build_pi_exec_args(&cfg);

    for flag in [
        "--output-format",
        "stream-json",
        "--model",
        "claude-sonnet-4",
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
    // Output format first; model right after.
    let pos_output = args.iter().position(|a| a == "--output-format").unwrap();
    let pos_model = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(pos_output, 0);
    assert!(pos_model < pos_output + 3);
}