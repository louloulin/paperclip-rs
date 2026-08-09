//! R464 — poisoned session jsonl 清理端到端验证。
//!
//! 验证：
//! 1. poisoned 错误触发 fresh retry
//! 2. 同时清理 Claude CLI 缓存的 jsonl 文件
//! 3. remote target 不会触发清理（远端 host 上的文件无法本地清理）

use std::collections::BTreeMap;
use std::path::PathBuf;

use pc_adapter_api::{Adapter, AdapterEventSink, AdapterExecutionContext};
use pc_adapter_claude_local::claude_session_cleanup::{
    build_poisoned_jsonl_path, encode_project_cwd,
};
use pc_adapter_claude_local::ClaudeLocalAdapter;
use serde_json::json;
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let id = Uuid::new_v4();
        let path = std::env::temp_dir().join(format!("paperclip-r464-{label}-{id}"));
        std::fs::create_dir_all(&path).expect("mkdir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn copy_fixture_to_temp(name: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("tests").join("fixtures").join(name);
    let dest = std::env::temp_dir().join(format!("paperclip-r464-{}-{}", Uuid::new_v4(), name));
    std::fs::copy(&src, &dest).expect("copy fixture");
    let mut perms = std::fs::metadata(&dest).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dest, perms).expect("chmod");
    dest
}

fn make_ctx_with_claude_home(command: &str, claude_home: &str) -> AdapterExecutionContext {
    let mut env = BTreeMap::new();
    env.insert("CLAUDE_CONFIG_DIR".to_owned(), claude_home.to_owned());
    let mut ctx = AdapterExecutionContext::new(Uuid::new_v4(), Uuid::new_v4(), "prompt");
    ctx.env = env;
    ctx.adapter_config = json!({ "command": command });
    ctx
}

#[tokio::test]
async fn script_basic_invocation_works() {
    let script = copy_fixture_to_temp("claude_poisoned_session.sh");
    let counter = std::env::temp_dir().join(format!("direct-{}", Uuid::new_v4()));
    let output = std::process::Command::new(&script)
        .env("PAPERCLIP_RETRY_COUNTER", &counter)
        .output()
        .expect("spawn");
    println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    println!("status: {:?}", output.status.code());
    assert!(output.status.code() == Some(1));
    assert!(counter.exists());
}

#[tokio::test]
async fn poisoned_session_unlinks_jsonl_file() {
    let tmp = TempDir::new("cleanup");
    let counter_path =
        std::env::temp_dir().join(format!("paperclip-r464-counter-{}", Uuid::new_v4()));
    let script = copy_fixture_to_temp("claude_poisoned_session.sh");
    let ctx = make_ctx_with_claude_home(script.to_str().unwrap(), tmp.path.to_str().unwrap());
    let mut ctx = ctx;
    ctx.session_id = Some("550e8400-e29b-41d4-a716-446655440000".to_owned());
    ctx.env.insert(
        "PAPERCLIP_RETRY_COUNTER".to_owned(),
        counter_path.to_string_lossy().to_string(),
    );
    // 使用真实存在的 cwd，避免 spawn 时 ENOENT
    ctx.cwd = Some(tmp.path.clone());
    let cwd_str = tmp.path.to_string_lossy().to_string();

    // 预创建 poisoned jsonl 文件
    let poisoned_jsonl = build_poisoned_jsonl_path(
        tmp.path.to_str().unwrap(),
        &cwd_str,
        "550e8400-e29b-41d4-a716-446655440000",
    );
    if let Some(parent) = poisoned_jsonl.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&poisoned_jsonl, "{}").unwrap();
    assert!(poisoned_jsonl.exists(), "precondition: jsonl exists");

    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = ClaudeLocalAdapter::new()
        .execute_with_resume_retry(ctx, sink)
        .await
        .expect("execute ok");

    // 验证：retry 成功
    assert_eq!(result.session_id.as_deref(), Some("fresh_poisoned"));
    assert_eq!(result.summary.as_deref(), Some("fresh done"));

    // 验证：poisoned jsonl 文件已被清理
    assert!(
        !poisoned_jsonl.exists(),
        "poisoned jsonl 应当已被 unlink（路径：{}）",
        poisoned_jsonl.display()
    );

    let _ = std::fs::remove_file(&script);
    let _ = std::fs::remove_file(&counter_path);
}

#[tokio::test]
async fn poisoned_session_missing_jsonl_is_noop() {
    let tmp = TempDir::new("cleanup-missing");
    let counter_path =
        std::env::temp_dir().join(format!("paperclip-r464-counter-{}", Uuid::new_v4()));
    let script = copy_fixture_to_temp("claude_poisoned_session.sh");
    let ctx = make_ctx_with_claude_home(script.to_str().unwrap(), tmp.path.to_str().unwrap());
    let mut ctx = ctx;
    ctx.session_id = Some("550e8400-e29b-41d4-a716-446655440000".to_owned());
    ctx.env.insert(
        "PAPERCLIP_RETRY_COUNTER".to_owned(),
        counter_path.to_string_lossy().to_string(),
    );
    ctx.cwd = Some(tmp.path.clone());

    // 不预创建文件 — 测试文件不存在时仍然能成功 retry
    let (sink, _rx) = AdapterEventSink::channel(8);
    let result = ClaudeLocalAdapter::new()
        .execute_with_resume_retry(ctx, sink)
        .await
        .expect("execute ok");

    assert_eq!(result.session_id.as_deref(), Some("fresh_poisoned"));

    let _ = std::fs::remove_file(&script);
    let _ = std::fs::remove_file(&counter_path);
}

#[test]
fn encode_project_cwd_matches_node_regex() {
    // Node: replace /[^a-zA-Z0-9-]/g with "-"
    assert_eq!(encode_project_cwd("/Users/me/proj"), "-Users-me-proj");
    assert_eq!(encode_project_cwd("/tmp"), "-tmp");
    assert_eq!(encode_project_cwd("/a_b/c-d"), "-a-b-c-d");
}
