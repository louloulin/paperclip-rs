//! R870 — grok_local adapter CLI args builder tests.
//!
//! Verifies that `build_grok_exec_args` emits the expected flags for
//! the full R870 CLI surface (temperature, max-tokens, sandbox,
//! system-prompt, append-system-prompt-file, effort, cwd, model, resume).
//!
//! These tests use serde_json::json! to construct realistic adapter
//! configs; the helper avoids any process spawning.

use pc_adapter_grok_local::build_grok_exec_args;
use serde_json::json;

#[test]
fn r870_minimal_config_only_output_format() {
    let cfg = json!({});
    let args = build_grok_exec_args(&cfg, None);
    assert_eq!(args, vec!["--output-format", "stream-json"]);
}

#[test]
fn r870_model_arg() {
    let cfg = json!({"model": "grok-4"});
    let args = build_grok_exec_args(&cfg, None);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"grok-4".to_string()));
}

#[test]
fn r870_temperature_arg() {
    let cfg = json!({"temperature": 0.7});
    let args = build_grok_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--temperature").unwrap();
    assert_eq!(args[pos + 1], "0.7");
}

#[test]
fn r870_max_tokens_arg() {
    let cfg = json!({"maxTokens": 8192u64});
    let args = build_grok_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--max-tokens").unwrap();
    assert_eq!(args[pos + 1], "8192");
}

#[test]
fn r870_sandbox_true_emits_flag() {
    let cfg = json!({"sandbox": true});
    let args = build_grok_exec_args(&cfg, None);
    assert!(args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_sandbox_false_omits_flag() {
    let cfg = json!({"sandbox": false});
    let args = build_grok_exec_args(&cfg, None);
    assert!(!args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_system_prompt_arg() {
    let cfg = json!({"systemPrompt": "You are a helpful assistant."});
    let args = build_grok_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--system-prompt").unwrap();
    assert_eq!(args[pos + 1], "You are a helpful assistant.");
}

#[test]
fn r870_empty_system_prompt_omits_flag() {
    let cfg = json!({"systemPrompt": ""});
    let args = build_grok_exec_args(&cfg, None);
    assert!(!args.contains(&"--system-prompt".to_string()));
}

#[test]
fn r870_append_system_prompt_file_arg() {
    let cfg = json!({"appendSystemPromptFile": "/etc/paperclip/system.md"});
    let args = build_grok_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--append-system-prompt-file").unwrap();
    assert_eq!(args[pos + 1], "/etc/paperclip/system.md");
}

#[test]
fn r870_effort_arg() {
    let cfg = json!({"effort": "high"});
    let args = build_grok_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--effort").unwrap();
    assert_eq!(args[pos + 1], "high");
}

#[test]
fn r870_cwd_arg() {
    let cfg = json!({"cwd": "/tmp/work"});
    let args = build_grok_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--cwd").unwrap();
    assert_eq!(args[pos + 1], "/tmp/work");
}

#[test]
fn r870_resume_session_id_appended() {
    let cfg = json!({});
    let args = build_grok_exec_args(&cfg, Some("session-123"));
    let pos = args.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(args[pos + 1], "session-123");
}

#[test]
fn r870_resume_session_id_trimmed() {
    let cfg = json!({});
    let args = build_grok_exec_args(&cfg, Some("  session-456  "));
    let pos = args.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(args[pos + 1], "session-456");
}

#[test]
fn r870_resume_empty_session_id_omits_flag() {
    let cfg = json!({});
    let args = build_grok_exec_args(&cfg, Some(""));
    assert!(!args.contains(&"--resume".to_string()));
}

#[test]
fn r870_extra_args_appended_at_end() {
    let cfg = json!({"extraArgs": ["--verbose", "--debug"]});
    let args = build_grok_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--verbose").unwrap();
    assert_eq!(args[pos + 1], "--debug");
}

#[test]
fn r870_full_config_produces_ordered_args() {
    let cfg = json!({
        "model": "grok-4",
        "temperature": 0.5,
        "maxTokens": 4096u64,
        "sandbox": true,
        "cwd": "/tmp/work",
        "systemPrompt": "Be brief.",
        "appendSystemPromptFile": "/etc/paperclip/system.md",
        "effort": "medium",
        "extraArgs": ["--no-cache"]
    });
    let args = build_grok_exec_args(&cfg, Some("session-789"));

    // Verify ordering: output-format first, resume last
    assert_eq!(args[0], "--output-format");
    assert_eq!(args[1], "stream-json");

    let resume_pos = args.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(resume_pos, args.len() - 2);
    assert_eq!(args[resume_pos + 1], "session-789");

    // All flags present
    for needle in &[
        "--model", "grok-4",
        "--temperature", "0.5",
        "--max-tokens", "4096",
        "--sandbox",
        "--cwd", "/tmp/work",
        "--system-prompt", "Be brief.",
        "--append-system-prompt-file", "/etc/paperclip/system.md",
        "--effort", "medium",
        "--no-cache",
    ] {
        assert!(
            args.contains(&needle.to_string()),
            "expected {:?} in args {:?}",
            needle,
            args
        );
    }
}
