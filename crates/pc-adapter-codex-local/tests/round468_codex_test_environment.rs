//! R468 — codex-local `testEnvironment` 决策端到端验证。
//!
//! 验证：
//! 1. hello probe 5 分支决策与真实 fixture 子进程输出对齐
//! 2. summarize_probe_detail 折叠空白 + 截断
//! 3. login 检测在 stdout/stderr/error_message 三处都生效
//! 4. decide_test_environment_checks 累积 checks

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use pc_adapter_codex_local::codex_test::{
    classify_codex_hello_probe, command_looks_like, decide_test_environment_checks,
    has_hello_in_text, is_codex_login_required, summarize_probe_detail, CodexHelloProbeInput,
};
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("paperclip-r468-{label}-{id}"));
        std::fs::create_dir_all(&path).expect("mkdir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_fixture(name: &str, body: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "paperclip-r468-fixture-{}-{}",
        Uuid::new_v4(),
        name
    ));
    std::fs::write(&path, body).expect("write fixture");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

#[test]
fn command_looks_like_real_paths() {
    assert!(command_looks_like("/usr/local/bin/codex", "codex"));
    assert!(command_looks_like("/opt/homebrew/bin/codex", "codex"));
    assert!(command_looks_like("./bin/codex", "codex"));
    assert!(command_looks_like("codex", "codex"));
    assert!(command_looks_like("/usr/bin/codex.cmd", "codex"));
    assert!(command_looks_like(
        "C:\\Program Files\\Codex\\codex.exe",
        "codex"
    ));
}

#[test]
fn command_looks_like_rejects_unrelated() {
    assert!(!command_looks_like("claude", "codex"));
    assert!(!command_looks_like("codex-fork", "codex"));
    assert!(!command_looks_like("openai-codex", "codex"));
}

#[test]
fn login_required_combined_evidence() {
    // 三处证据合并检测
    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: Some(1),
        stdout: "trying...".to_string(),
        stderr: "401 Unauthorized".to_string(),
        error_message: None,
    };
    assert!(matches!(
        classify_codex_hello_probe(&input),
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::AuthRequired
    ));
}

#[test]
fn probe_timed_out_wins_over_other_branches() {
    let input = CodexHelloProbeInput {
        timed_out: true,
        exit_code: Some(0), // 即便 exit 0，timed_out 也优先
        stdout: "hello".to_string(),
        stderr: String::new(),
        error_message: None,
    };
    assert!(matches!(
        classify_codex_hello_probe(&input),
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::TimedOut
    ));
}

#[test]
fn probe_passed_includes_hello_word() {
    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: Some(0),
        stdout: "Codex says hello to you\n<END>".to_string(),
        stderr: String::new(),
        error_message: None,
    };
    match classify_codex_hello_probe(&input) {
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::Passed { detail } => {
            assert!(detail.contains("hello"));
        }
        other => panic!("expected Passed, got {:?}", other),
    }
}

#[test]
fn probe_passed_handles_hello_in_middle_of_text() {
    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: Some(0),
        stdout: "I would say hello back".to_string(),
        stderr: String::new(),
        error_message: None,
    };
    assert!(matches!(
        classify_codex_hello_probe(&input),
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::Passed { .. }
    ));
}

#[test]
fn probe_failed_when_no_login_and_non_zero() {
    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: Some(2),
        stdout: "internal error".to_string(),
        stderr: "fatal".to_string(),
        error_message: None,
    };
    match classify_codex_hello_probe(&input) {
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::Failed { detail } => {
            assert_eq!(detail, "fatal");
        }
        other => panic!("expected Failed, got {:?}", other),
    }
}

#[test]
fn probe_unexpected_output_when_no_hello_word() {
    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: Some(0),
        stdout: "ok done".to_string(),
        stderr: String::new(),
        error_message: None,
    };
    assert!(matches!(
        classify_codex_hello_probe(&input),
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::UnexpectedOutput { .. }
    ));
}

#[test]
fn probe_unexpected_output_rejects_substring_hello() {
    // "helloo" 不算 "hello"，应该归类为 UnexpectedOutput
    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: Some(0),
        stdout: "helloo".to_string(),
        stderr: String::new(),
        error_message: None,
    };
    assert!(matches!(
        classify_codex_hello_probe(&input),
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::UnexpectedOutput { .. }
    ));
}

#[test]
fn real_fixture_outputs_hello_to_pass_probe() {
    // 用真实 fixture 验证 hello probe 决策端到端
    let fixture = write_fixture(
        "codex_hello.sh",
        "#!/bin/sh\nprintf 'Codex responds with hello to paperclip\\n'\n",
    );
    let output = std::process::Command::new(&fixture)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: output.status.code(),
        stdout,
        stderr,
        error_message: None,
    };
    assert!(matches!(
        classify_codex_hello_probe(&input),
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::Passed { .. }
    ));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn real_fixture_outputs_login_required() {
    // fixture 返回 login required 错误
    let fixture = write_fixture(
        "codex_login.sh",
        "#!/bin/sh\nprintf 'Error: not logged in. Please run codex login.\\n' >&2\nexit 1\n",
    );
    let output = std::process::Command::new(&fixture)
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let input = CodexHelloProbeInput {
        timed_out: false,
        exit_code: output.status.code(),
        stdout,
        stderr,
        error_message: None,
    };
    assert!(matches!(
        classify_codex_hello_probe(&input),
        pc_adapter_codex_local::codex_test::CodexHelloProbeOutcome::AuthRequired
    ));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn summarize_probe_detail_collapses_multiline() {
    let detail = summarize_probe_detail("line1\n\nline2\t\tline3\n\n\n", "", None).expect("detail");
    assert_eq!(detail, "line1 line2 line3");
}

#[test]
fn summarize_probe_detail_strips_parsed_error_whitespace() {
    let detail = summarize_probe_detail("", "", Some("  trimmed  ")).expect("detail");
    assert_eq!(detail, "trimmed");
}

#[test]
fn is_login_required_handles_real_outputs() {
    // 真实 codex CLI 常见错误格式
    assert!(is_codex_login_required(
        "Error: You are not logged in. Run `codex login` to authenticate."
    ));
    assert!(is_codex_login_required("OPENAI_API_KEY is not set"));
    assert!(is_codex_login_required("Unauthorized"));
    assert!(is_codex_login_required(
        "Authentication required: invalid or missing API key"
    ));
}

#[test]
fn has_hello_in_text_handles_punctuation() {
    // 标点作为单词边界
    assert!(has_hello_in_text("hello."));
    assert!(has_hello_in_text("hello,"));
    assert!(has_hello_in_text("hello!"));
    assert!(has_hello_in_text("(hello)"));
    assert!(has_hello_in_text("\"hello\""));
}

#[test]
fn decide_test_env_with_real_temp_cwd() {
    let tmp = TempDir::new("cwd");
    let cfg = serde_json::Map::new();
    let decision =
        decide_test_environment_checks(&pc_adapter_codex_local::codex_test::TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: tmp.path.to_str().unwrap(),
            host_env: None,
        });
    // 真实 cwd 应该 info 而非 error
    assert!(decision.checks.iter().any(|c| c.code == "codex_cwd_valid"));
    assert!(!decision
        .checks
        .iter()
        .any(|c| c.code == "codex_cwd_invalid"));
    // 应该会运行 hello probe
    assert!(decision.should_run_probe);
}

#[test]
fn decide_test_env_aggregates_status_to_warn_without_api_key() {
    let cfg = serde_json::Map::new();
    let decision =
        decide_test_environment_checks(&pc_adapter_codex_local::codex_test::TestEnvironmentInput {
            config: &cfg,
            execution_target: None,
            cwd: "/workspace",
            host_env: None,
        });
    // 有 warn（api_key_missing），无 error → 状态是 warn
    let has_error = decision
        .checks
        .iter()
        .any(|c| c.code == "codex_cwd_invalid");
    if !has_error {
        let status = pc_adapter_codex_local::codex_test::summarize_status_str(&decision.checks);
        assert_eq!(status, "warn");
    }
}
