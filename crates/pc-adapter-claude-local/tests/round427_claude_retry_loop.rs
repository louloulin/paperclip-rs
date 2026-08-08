//! R427 — claude-local 端到端校验错误族分类驱动的 `clearSession` 决策。

use std::collections::BTreeMap;
use std::path::PathBuf;

use pc_adapter_api::{Adapter, AdapterExecutionContext, AdapterEventSink};
use pc_adapter_claude_local::ClaudeLocalAdapter;
use serde_json::json;
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("paperclip-r427-{label}-{id}"));
        std::fs::create_dir_all(&path).expect("mkdir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_mock_cli(dir: &std::path::Path, name: &str, lines: &[&str], exit_code: i32) -> PathBuf {
    let script = dir.join(name);
    let mut body = String::from("#!/bin/sh\n");
    for line in lines {
        body.push_str(&format!("printf '%s\\n' '{line}'\n"));
    }
    body.push_str(&format!("exit {exit_code}\n"));
    std::fs::write(&script, body).expect("write mock");
    let mut perms = std::fs::metadata(&script).expect("stat").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&script, perms).expect("chmod");
    script
}

fn make_ctx(command: &str, env: BTreeMap<String, String>) -> AdapterExecutionContext {
    let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "prompt");
    ctx.env = env;
    ctx.adapter_config = json!({ "command": command });
    ctx
}

#[tokio::test]
async fn max_turns_result_triggers_clear_session() {
    let tmp = TempDir::new("claude-maxturns");
    let lines = [
        r#"{"type":"system","subtype":"init","session_id":"s1","model":"opus"}"#,
        r#"{"type":"result","subtype":"error_max_turns","session_id":"s1"}"#,
    ];
    let cmd = write_mock_cli(&tmp.path, "claude-mock", &lines, 1);
    let env: BTreeMap<String, String> = BTreeMap::new();
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = ClaudeLocalAdapter::new()
        .execute(make_ctx(cmd.to_str().unwrap(), env), sink)
        .await
        .expect("execute ok");
    assert!(result.clear_session, "max_turns should clear session");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("stopReason").and_then(|v| v.as_str()),
        Some("max_turns_exhausted")
    );
    assert_eq!(
        json.get("errorFamily").and_then(|v| v.as_str()),
        Some("max_turns")
    );
}

#[tokio::test]
async fn provider_quota_sets_error_family_without_clearing_session() {
    let tmp = TempDir::new("claude-quota");
    let lines = [
        r#"{"type":"system","subtype":"init","session_id":"s1","model":"opus"}"#,
        r#"{"type":"result","is_error":true,"result":"Claude usage limit reached","session_id":"s1"}"#,
    ];
    let cmd = write_mock_cli(&tmp.path, "claude-mock", &lines, 1);
    let env: BTreeMap<String, String> = BTreeMap::new();
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = ClaudeLocalAdapter::new()
        .execute(make_ctx(cmd.to_str().unwrap(), env), sink)
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    let family = json.get("errorFamily").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        family == "provider_quota" || family == "transient_upstream",
        "expected provider_quota/transient_upstream, got {family:?}"
    );
    assert!(!result.clear_session, "provider_quota 不应清 session");
}

#[tokio::test]
async fn unknown_session_error_sets_clear_session() {
    let tmp = TempDir::new("claude-unknown");
    let lines = [
        r#"{"type":"system","subtype":"init","session_id":"s1","model":"opus"}"#,
        r#"{"type":"result","is_error":true,"result":"No conversation found with session id abc","session_id":"s1"}"#,
    ];
    let cmd = write_mock_cli(&tmp.path, "claude-mock", &lines, 1);
    let env: BTreeMap<String, String> = BTreeMap::new();
    let mut ctx = make_ctx(cmd.to_str().unwrap(), env);
    ctx.session_id = Some("s1".to_owned());
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = ClaudeLocalAdapter::new().execute(ctx, sink).await.expect("execute ok");
    assert!(result.clear_session, "unknown_session 应清 session");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("errorFamily").and_then(|v| v.as_str()),
        Some("unknown_session")
    );
}
