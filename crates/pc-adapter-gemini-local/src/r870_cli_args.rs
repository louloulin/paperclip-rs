//! R870 — gemini_local adapter CLI args builder tests.
//!
//! Mirrors `crates/pc-adapter-grok-local/src/r870_cli_args.rs` but
//! for Gemini-specific flags. The gemini adapter additionally
//! exposes top-p / top-k sampling and an `allowed-tools` allowlist
//! that the grok adapter does not.

use pc_adapter_gemini_local::build_gemini_exec_args;
use serde_json::json;

#[test]
fn r870_minimal_config_only_output_format() {
    let cfg = json!({});
    let args = build_gemini_exec_args(&cfg, None);
    assert_eq!(args, vec!["--output-format", "stream-json"]);
}

#[test]
fn r870_model_arg() {
    let cfg = json!({"model": "gemini-2.5-pro"});
    let args = build_gemini_exec_args(&cfg, None);
    assert!(args.contains(&"--model".to_string()));
    assert!(args.contains(&"gemini-2.5-pro".to_string()));
}

#[test]
fn r870_temperature_arg() {
    let cfg = json!({"temperature": 0.3});
    let args = build_gemini_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--temperature").unwrap();
    assert_eq!(args[pos + 1], "0.3");
}

#[test]
fn r870_max_output_tokens_arg() {
    let cfg = json!({"maxOutputTokens": 16384u64});
    let args = build_gemini_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--max-output-tokens").unwrap();
    assert_eq!(args[pos + 1], "16384");
}

#[test]
fn r870_top_p_arg() {
    let cfg = json!({"topP": 0.95});
    let args = build_gemini_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--top-p").unwrap();
    assert_eq!(args[pos + 1], "0.95");
}

#[test]
fn r870_top_k_arg() {
    let cfg = json!({"topK": 40u64});
    let args = build_gemini_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--top-k").unwrap();
    assert_eq!(args[pos + 1], "40");
}

#[test]
fn r870_sandbox_true_emits_flag() {
    let cfg = json!({"sandbox": true});
    let args = build_gemini_exec_args(&cfg, None);
    assert!(args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_sandbox_false_omits_flag() {
    let cfg = json!({"sandbox": false});
    let args = build_gemini_exec_args(&cfg, None);
    assert!(!args.contains(&"--sandbox".to_string()));
}

#[test]
fn r870_system_prompt_arg() {
    let cfg = json!({"systemPrompt": "You are a senior reviewer."});
    let args = build_gemini_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--system-prompt").unwrap();
    assert_eq!(args[pos + 1], "You are a senior reviewer.");
}

#[test]
fn r870_allowed_tools_comma_list() {
    let cfg = json!({"allowedTools": "codebase_search,read_file,grep"});
    let args = build_gemini_exec_args(&cfg, None);
    let pos = args.iter().position(|a| a == "--allowed-tools").unwrap();
    assert_eq!(args[pos + 1], "codebase_search,read_file,grep");
}

#[test]
fn r870_empty_allowed_tools_omits_flag() {
    let cfg = json!({"allowedTools": ""});
    let args = build_gemini_exec_args(&cfg, None);
    assert!(!args.contains(&"--allowed-tools".to_string()));
}

#[test]
fn r870_resume_session_id_appended() {
    let cfg = json!({});
    let args = build_gemini_exec_args(&cfg, Some("session-abc"));
    let pos = args.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(args[pos + 1], "session-abc");
    // Resume must be last
    assert_eq!(pos, args.len() - 2);
}

#[test]
fn r870_full_config_produces_ordered_args() {
    let cfg = json!({
        "model": "gemini-2.5-pro",
        "temperature": 0.4,
        "maxOutputTokens": 8192u64,
        "topP": 0.9,
        "topK": 50u64,
        "sandbox": true,
        "cwd": "/tmp/work",
        "systemPrompt": "Be precise.",
        "allowedTools": "codebase_search,read_file",
        "extraArgs": ["--verbose"]
    });
    let args = build_gemini_exec_args(&cfg, Some("session-xyz"));

    assert_eq!(args[0], "--output-format");
    assert_eq!(args[1], "stream-json");

    for needle in &[
        "--model", "gemini-2.5-pro",
        "--temperature", "0.4",
        "--max-output-tokens", "8192",
        "--top-p", "0.9",
        "--top-k", "50",
        "--sandbox",
        "--cwd", "/tmp/work",
        "--system-prompt", "Be precise.",
        "--allowed-tools", "codebase_search,read_file",
        "--verbose",
        "--resume", "session-xyz",
    ] {
        assert!(
            args.contains(&needle.to_string()),
            "expected {:?} in args {:?}",
            needle,
            args
        );
    }
}
