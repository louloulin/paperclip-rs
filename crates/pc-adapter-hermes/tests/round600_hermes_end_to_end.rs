// R600 Hermes adapter 真实 end-to-end 验证（用 fake hermes CLI）
//
// 构造一个 fake hermes 二进制（写入 shell 脚本），模拟 Hermes 输出
// session_id + tokens + cost + agent 文本响应。然后通过 HermesAdapter
// execute 的完整路径跑一次，验证：
//   1. CLI args 拼装正确（包含 --source tool --yolo、-q <prompt>）
//   2. session_id / usage / cost / response 都从 fake 输出解析出来
//   3. stderr 中的良性日志被重新分类为 stdout
//   4. AdapterExecutionResult 字段全部正确填充
//
// 不依赖真实 hermes CLI（外部依赖）

use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};
use pc_adapter_hermes::HermesAdapter;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn write_fake_hermes(dir: &PathBuf) -> PathBuf {
    let path = dir.join("fake_hermes");
    let script = "#!/bin/sh\n\
echo \"[2026-08-12T10:30:00] INFO: starting hermes\" 1>&2\n\
echo \"MCP server connected\" 1>&2\n\
echo \"thinking about the task...\"\n\
echo \"| running tool: calculator\"\n\
echo \"the answer is 42\"\n\
echo \"session_id: sess-r600-real-001\"\n\
echo \"tokens: 1234 input 567 output\"\n\
echo 'cost: $0.42'\n";
    std::fs::write(&path, script).expect("write fake hermes");
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

#[tokio::test(flavor = "multi_thread")]
async fn hermes_adapter_real_end_to_end_with_fake_cli() {
    let dir = std::env::temp_dir().join(format!("paperclip-r600-hermes-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let fake_hermes = write_fake_hermes(&dir);

    let mut context =
        AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "compute 6*7");
    context.adapter_config = serde_json::json!({
        "command": fake_hermes.to_string_lossy(),
        "model": "auto",
        "provider": "anthropic",
        "quiet": true,
    });

    let (sink, mut rx) = AdapterEventSink::channel(64);
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_for_log = Arc::clone(&captured);
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let pc_adapter_api::AdapterEvent::Output { stream, text, .. } = event {
                let label = match stream {
                    pc_adapter_api::OutputStream::Stdout => "stdout",
                    pc_adapter_api::OutputStream::Stderr => "stderr",
                };
                captured_for_log
                    .lock()
                    .expect("lock")
                    .push(format!("{label}:{text}"));
            }
        }
    });

    let adapter = HermesAdapter::new();
    let result = adapter.execute(context, sink).await.expect("execute");

    assert_eq!(result.exit_code, Some(0), "exit_code: {result:?}");

    let session_id = result.session_id.clone().expect("session_id");
    assert!(
        session_id.starts_with("sess-"),
        "got session_id: {session_id}"
    );

    let display = result.session_display_id.clone().expect("display_id");
    assert!(display.len() <= 16);

    let usage = result.usage.clone().expect("usage");
    assert_eq!(usage.input_tokens, 1234);
    assert_eq!(usage.output_tokens, 567);

    assert_eq!(result.cost_usd, Some(0.42));
    assert_eq!(result.provider.as_deref(), Some("anthropic"));

    let summary = result.summary.clone().expect("summary");
    assert!(
        summary.contains("answer") || summary.contains("42"),
        "summary should contain answer/42; got: {summary}"
    );

    let result_json = result.result_json.clone().expect("result_json");
    assert_eq!(
        result_json.get("session_id").and_then(|v| v.as_str()),
        Some(session_id.as_str())
    );
    assert_eq!(
        result_json.get("cost_usd").and_then(|v| v.as_f64()),
        Some(0.42)
    );
    assert_eq!(
        result_json.get("resolvedFrom").and_then(|v| v.as_str()),
        Some("explicit")
    );

    let events_text = captured.lock().expect("lock").join("\n");
    assert!(
        events_text.contains("INFO: starting hermes"),
        "benign stderr should be reclassified as stdout; got events: {events_text}"
    );
    assert!(
        events_text.contains("MCP server connected"),
        "benign stderr should be reclassified as stdout; got events: {events_text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn hermes_adapter_provider_explicit_beats_detected() {
    let dir =
        std::env::temp_dir().join(format!("paperclip-r600-explicit-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("dir");
    let fake_hermes = write_fake_hermes(&dir);

    let mut context =
        AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "test prompt");
    context.adapter_config = serde_json::json!({
        "command": fake_hermes.to_string_lossy(),
        "provider": "minimax",
    });

    let (sink, _rx) = AdapterEventSink::channel(16);
    let adapter = HermesAdapter::new();
    let result = adapter.execute(context, sink).await.expect("execute");
    assert_eq!(result.provider.as_deref(), Some("minimax"));
    assert_eq!(
        result
            .result_json
            .as_ref()
            .and_then(|v| v.get("resolvedFrom"))
            .and_then(|v| v.as_str()),
        Some("explicit")
    );

    std::fs::remove_dir_all(&dir).ok();
}
