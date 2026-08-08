//! R428 — grok-local 端到端验证「resume 失败 → 真实重跑」主回路。
//!
//! 复刻 Node `packages/adapters/grok-local/src/server/execute.ts` 中
//! `initial = runAttempt(sessionId); isGrokUnknownSessionError → runAttempt(null)` 回路。

use std::collections::BTreeMap;
use std::path::PathBuf;

use pc_adapter_api::{Adapter, AdapterExecutionContext, AdapterEventSink};
use pc_adapter_grok_local::GrokLocalAdapter;
use serde_json::json;
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("paperclip-r428-{label}-{id}"));
        std::fs::create_dir_all(&path).expect("mkdir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn write_mock(dir: &std::path::Path, body: &str) -> PathBuf {
    let script = dir.join("grok-mock");
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

fn make_ctx(command: &str, session_id: Option<&str>, env: BTreeMap<String, String>) -> AdapterExecutionContext {
    let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "prompt");
    ctx.env = env;
    ctx.adapter_config = json!({ "command": command });
    ctx.session_id = session_id.map(str::to_owned);
    ctx
}

fn call_log(dir: &std::path::Path) -> Vec<String> {
    let path = dir.join("calls.log");
    if !path.exists() {
        return Vec::new();
    }
    let raw = std::fs::read_to_string(&path).expect("read log");
    raw.lines().map(str::to_owned).collect()
}

#[tokio::test]
async fn unknown_session_triggers_real_retry_without_resume() {
    let tmp = TempDir::new("grok-retry");
    let log = tmp.path.join("calls.log");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\nN=$(wc -l < '{log}' | tr -d ' ')\nif [ \"$N\" = \"1\" ]; then\n  echo 'unknown session id: sess-a' >&2\n  exit 1\nelse\n  printf '%s\\n' '{{\"type\":\"text\",\"data\":\"Recovered\"}}'\n  printf '%s\\n' '{{\"type\":\"end\",\"stopReason\":\"EndTurn\",\"sessionId\":\"sess-fresh\",\"requestId\":\"req-1\"}}'\n  exit 0\nfi\n",
        log = log.display()
    );
    let script = write_mock(&tmp.path, &body);

    let env: BTreeMap<String, String> = BTreeMap::new();
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = GrokLocalAdapter::new()
        .execute(
            make_ctx(script.to_str().unwrap(), Some("sess-a"), env),
            sink,
        )
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("retriedAfterUnknownSession").and_then(|v| v.as_bool()),
        Some(true)
    );
    let calls = call_log(&tmp.path);
    assert_eq!(calls.len(), 2, "应被调用两次；calls={calls:?}");
    assert!(calls[0].contains("--resume"), "首次应带 --resume: {}", calls[0]);
    assert!(calls[0].contains("sess-a"), "首次应带 session id: {}", calls[0]);
    assert!(!calls[1].contains("--resume"), "重试必须去掉 --resume: {}", calls[1]);
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.session_id.as_deref(), Some("sess-fresh"));
    assert_eq!(result.summary.as_deref(), Some("Recovered"));
}

#[tokio::test]
async fn unrelated_failure_does_not_retry() {
    let tmp = TempDir::new("grok-rate");
    let log = tmp.path.join("calls.log");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\necho 'rate limit exceeded' >&2\nexit 1\n",
        log = log.display()
    );
    let script = write_mock(&tmp.path, &body);
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = GrokLocalAdapter::new()
        .execute(
            make_ctx(script.to_str().unwrap(), Some("sess-a"), BTreeMap::new()),
            sink,
        )
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("retriedAfterUnknownSession").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(call_log(&tmp.path).len(), 1);
}
