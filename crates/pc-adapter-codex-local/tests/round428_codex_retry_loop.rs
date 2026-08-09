//! R428 — codex-local 端到端验证「resume 失败 → 真实重跑」主回路。
//!
//! 复刻 Node `packages/adapters/codex-local/src/server/execute.ts` 中
//! `initial = runAttempt(sessionId); isCodexUnknownSessionError → runAttempt(null)` 的回路：
//! - 首轮带 `resume <sid>`：失败时返回 unknown-session 错误；
//! - 触发重试，第二轮 **不带** `resume`，必须成功；
//! - `result.clearSession == true`，`result_json.retriedAfterUnknownSession == true`，
//!   且首轮失败被 `errorFamily` 标签（成功重试后 Node 把 errorFamily 重置为 null）。
//!
//! 同时验证：失败为非 unknown-session 时不重跑，`errorFamily` 反映
//! `transient_upstream` 标签（仅打 label，不再跑一次）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};
use pc_adapter_codex_local::CodexLocalAdapter;
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

fn make_ctx(
    command: &str,
    session_id: Option<&str>,
    env: BTreeMap<String, String>,
) -> AdapterExecutionContext {
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

fn write_mock(dir: &std::path::Path, body: &str) -> PathBuf {
    let script = dir.join("codex-mock");
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

#[tokio::test]
async fn unknown_session_triggers_real_retry_without_resume() {
    let tmp = TempDir::new("codex-retry");
    let log = tmp.path.join("calls.log");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\nN=$(wc -l < '{log}' | tr -d ' ')\nif [ \"$N\" = \"1\" ]; then\n  printf '%s\\n' 'unknown session id: th-1'\n  exit 1\nelse\n  printf '%s\\n' '{{\"type\":\"thread.started\",\"thread_id\":\"th-fresh\"}}'\n  printf '%s\\n' '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"Recovered\"}}}}'\n  printf '%s\\n' '{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":3,\"output_tokens\":1}}}}'\n  exit 0\nfi\n",
        log = log.display()
    );
    let script = write_mock(&tmp.path, &body);

    let env: BTreeMap<String, String> = BTreeMap::new();
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = CodexLocalAdapter::new()
        .execute(make_ctx(script.to_str().unwrap(), Some("th-1"), env), sink)
        .await
        .expect("execute ok");
    assert!(
        result.clear_session,
        "unknown-session 重跑后必须 clear_session"
    );
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("retriedAfterUnknownSession")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    // 成功重试后 Node 把 errorFamily 重置为 null；这里映射为 ""。
    assert_eq!(
        json.get("errorFamily").and_then(|v| v.as_str()),
        Some(""),
        "成功重试后 errorFamily 应为空字符串（Node null）"
    );
    let calls = call_log(&tmp.path);
    assert_eq!(calls.len(), 2, "应被调用两次；calls={calls:?}");
    assert!(calls[0].contains("resume"), "首次应带 resume: {}", calls[0]);
    assert!(
        calls[0].contains("th-1"),
        "首次应带 session id: {}",
        calls[0]
    );
    assert!(
        !calls[1].contains("resume"),
        "重试必须去掉 resume: {}",
        calls[1]
    );
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.summary.as_deref(), Some("Recovered"));
    assert_eq!(result.session_id.as_deref(), Some("th-fresh"));
}

#[tokio::test]
async fn unknown_session_retry_still_fails_keeps_family_label() {
    let tmp = TempDir::new("codex-retry-still-fail");
    let log = tmp.path.join("calls.log");
    // 两次都失败，且 stderr 都给出 unknown session。
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\nprintf '%s\\n' 'unknown session id: th-1'\nexit 1\n",
        log = log.display()
    );
    let script = write_mock(&tmp.path, &body);
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = CodexLocalAdapter::new()
        .execute(
            make_ctx(script.to_str().unwrap(), Some("th-1"), BTreeMap::new()),
            sink,
        )
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("errorFamily").and_then(|v| v.as_str()),
        Some("unknown_session")
    );
    assert_eq!(
        json.get("retriedAfterUnknownSession")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(result.clear_session);
    assert_eq!(call_log(&tmp.path).len(), 2);
}

#[tokio::test]
async fn transient_upstream_labels_family_without_retry() {
    let tmp = TempDir::new("codex-transient");
    let log = tmp.path.join("calls.log");
    let body = format!(
        "#!/bin/sh\necho \"$@\" >> '{log}'\nprintf '%s\\n' '{{\"type\":\"thread.started\",\"thread_id\":\"th-x\"}}'\necho 'high demand temporary errors' >&2\nexit 1\n",
        log = log.display()
    );
    let script = write_mock(&tmp.path, &body);

    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = CodexLocalAdapter::new()
        .execute(
            make_ctx(script.to_str().unwrap(), Some("th-x"), BTreeMap::new()),
            sink,
        )
        .await
        .expect("execute ok");
    let json = result.result_json.expect("result_json");
    assert_eq!(
        json.get("errorFamily").and_then(|v| v.as_str()),
        Some("transient_upstream")
    );
    assert_eq!(
        json.get("retriedAfterUnknownSession")
            .and_then(|v| v.as_bool()),
        Some(false)
    );
    assert!(!result.clear_session);
    assert_eq!(call_log(&tmp.path).len(), 1);
}
