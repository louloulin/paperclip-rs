//! R425 — claude-local 端到端校验 `paperclipEnvNote` 与 `apiAccessNote` 注入。

use std::collections::BTreeMap;
use std::path::PathBuf;

use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};
use pc_adapter_claude_local::ClaudeLocalAdapter;
use serde_json::json;
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("paperclip-r425-{label}-{}", id));
        std::fs::create_dir_all(&path).expect("mkdir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_mock_cli(dir: &std::path::Path, name: &str, stdout_lines: &[&str]) -> PathBuf {
    let script = dir.join(name);
    let mut body = String::from("#!/bin/sh\n");
    for line in stdout_lines {
        body.push_str(&format!("printf '%s\\n' '{line}'\n"));
    }
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
    let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "test prompt");
    ctx.env = env;
    ctx.adapter_config = json!({ "command": command });
    ctx
}

#[tokio::test]
async fn result_json_carries_prompt_notes() {
    let tmp = TempDir::new("claude");
    let stdout_lines = [
        r#"{"type":"thread.started","thread_id":"th-1"}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#,
    ];
    let cmd = write_mock_cli(&tmp.path, "claude-mock", &stdout_lines);
    let env: BTreeMap<String, String> = [
        ("PAPERCLIP_RUN_ID".to_owned(), "run-1".to_owned()),
        (
            "PAPERCLIP_API_URL".to_owned(),
            "https://api.test".to_owned(),
        ),
        ("PAPERCLIP_API_KEY".to_owned(), "sk-test".to_owned()),
    ]
    .into_iter()
    .collect();
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = ClaudeLocalAdapter::new()
        .execute(make_ctx(cmd.to_str().unwrap(), env), sink)
        .await
        .expect("execute ok");
    let value = result.result_json.expect("result_json present");
    let note = value
        .get("paperclipEnvNote")
        .and_then(|v| v.as_str())
        .unwrap();
    let api = value.get("apiAccessNote").and_then(|v| v.as_str()).unwrap();
    assert!(note.contains("PAPERCLIP_RUN_ID"));
    assert!(api.contains("curl"));
}
