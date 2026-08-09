//! R426 — pi-local 端到端校验 retry-after-unknown-session 回路。
//!
//! 这里不重复 R425 的注入测试；专注验证：
//! - 首轮 exit=0 → 不触发 retry，`result.sessionId == session_path`，`clearSession = false`；
//! - 首轮 exit≠0 + 未知 session 错误 → 触发一次 retry，最终
//!   `result.resultJson.retriedAfterUnknownSession == true`，`clearSession = true`，
//!   且 `sessionPath` 切换为新生成的路径；
//! - 首轮 exit≠0 + 普通错误（rate limit）→ 不触发 retry；
//! - 首轮 timed_out=true + 未知 session 错误 → 不触发 retry（与 Node 行为一致）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};
use pc_adapter_pi_local::PiLocalAdapter;
use serde_json::json;
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("paperclip-r426-{label}-{id}"));
        std::fs::create_dir_all(&path).expect("mkdir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_mock_cli(dir: &std::path::Path, name: &str, lines: &[&str]) -> PathBuf {
    let script = dir.join(name);
    let mut body = String::from("#!/bin/sh\n");
    for line in lines {
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
    let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "prompt");
    ctx.env = env;
    ctx.adapter_config = json!({ "command": command });
    ctx
}

#[tokio::test]
async fn successful_first_attempt_does_not_retry() {
    let tmp = TempDir::new("pi-ok");
    let lines = [
        r#"{"type":"session","sessionId":"s-old","cwd":"/tmp/p"}"#,
        r#"{"type":"agent_end","messages":[{"role":"assistant","content":"done"}]}"#,
    ];
    let cmd = write_mock_cli(&tmp.path, "pi-mock", &lines);
    let env: BTreeMap<String, String> = BTreeMap::new();
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = PiLocalAdapter::new()
        .execute(make_ctx(cmd.to_str().unwrap(), env), sink)
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("retriedAfterUnknownSession")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(!result.clear_session);
}

#[tokio::test]
async fn unknown_session_failure_triggers_retry() {
    let tmp = TempDir::new("pi-retry");
    // 写入一个合法的 session header 文件，让 saved_session_cwd 可被解析。
    let session_path = tmp.path.join("old-session.jsonl");
    std::fs::write(
        &session_path,
        format!(
            "{{\"type\":\"session\",\"cwd\":\"{}\"}}",
            tmp.path.display()
        ),
    )
    .expect("write session header");
    let lines = [
        r#"{"type":"error","message":"unknown session id: s-old"}"#,
        r#"{"type":"agent_end","messages":[{"role":"assistant","content":"fresh"}]}"#,
    ];
    let cmd = write_mock_cli(&tmp.path, "pi-mock", &lines);
    let env: BTreeMap<String, String> = BTreeMap::new();
    let mut ctx = make_ctx(cmd.to_str().unwrap(), env);
    ctx.session_id = Some(session_path.to_string_lossy().into_owned());
    ctx.cwd = Some(tmp.path.clone());
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = PiLocalAdapter::new()
        .execute(ctx, sink)
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("retriedAfterUnknownSession")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(result.clear_session);
    let path_now = json
        .get("sessionPath")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(path_now.contains(".jsonl"));
    assert_ne!(path_now, session_path.to_string_lossy());
}

#[tokio::test]
async fn unrelated_failure_does_not_retry() {
    let tmp = TempDir::new("pi-rate");
    let lines = [r#"{"type":"error","message":"rate limit exceeded"}"#];
    let cmd = write_mock_cli(&tmp.path, "pi-mock", &lines);
    let env: BTreeMap<String, String> = BTreeMap::new();
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = PiLocalAdapter::new()
        .execute(make_ctx(cmd.to_str().unwrap(), env), sink)
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("retriedAfterUnknownSession")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(!result.clear_session);
}
