//! R377 集成测试 — `pc-acpx` session config options + build_session_params +
//! apply_session_config_options 集成。
//!
//! 覆盖:
//! - build_session_params 投影 prepared + handle 到 AcpxSessionParams
//! - session_config_options 跨 agent 行为(claude/codex/gemini/custom)
//! - apply_session_config_options 在 execute() 中自动调用
//! - session_params 在结果中填充

use pc_acpx::session_compat::AcpxPreparedRuntimeLite;
use pc_acpx::{
    build_session_params, session_config_options, AcpRuntimeCapabilities, AcpRuntimeEvent,
    AcpxEngineExecutor, AcpxEngineExecutorDeps, AcpxSessionParams, AdapterExecutionContext,
};
use std::path::Path;
use std::sync::Arc;

fn mock_factory_with_done() -> pc_acpx::AcpxRuntimeFactory {
    Arc::new(move |_prepared| {
        let runtime = pc_acpx::MockAcpRuntime::new(vec![AcpRuntimeEvent::Done {
            stop_reason: Some("end_turn".into()),
        }])
        .with_capabilities(AcpRuntimeCapabilities::default());
        Ok(Arc::new(runtime) as Arc<dyn pc_acpx::AcpRuntime>)
    })
}

fn build_executor() -> AcpxEngineExecutor {
    AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
        runtime_factory: Some(mock_factory_with_done()),
        ..Default::default()
    })
}

fn ctx() -> AdapterExecutionContext {
    AdapterExecutionContext {
        run_id: "run_test".into(),
        agent: pc_acpx::AgentIdentity::new("claude", "co_x"),
        config: serde_json::json!({ "agent": "claude" }),
        context: serde_json::json!({}),
        auth_token: Some("token".into()),
        run_prompt: "test prompt".into(),
        cwd: Path::new("/repo").to_path_buf(),
        state_dir: None,
        workspace_id: "ws_42".into(),
        workspace_repo_url: "git@github.com:foo/bar.git".into(),
        workspace_repo_ref: "main".into(),
        workspace_branch: "main".into(),
        workspace_source: "realized".into(),
        workspace_strategy: "worktree".into(),
        workspace_worktree_path: "/repo".into(),
        agent_home: "/home/agent".into(),
        adapter_type: "claude_local".into(),
        module_dir: Path::new("/module").to_path_buf(),
        package_root_dir: Path::new("/pkg").to_path_buf(),
        execution_target_is_remote: false,
        mcp_servers: Vec::new(),
        ignore_mcp_in_fingerprint: false,
        previous_session_params: None,
        sink: Arc::new(pc_acpx::NoopSink),
    }
}

fn lite(
    agent: &str,
    model: Option<&str>,
    effort: Option<&str>,
    fast: bool,
) -> AcpxPreparedRuntimeLite {
    AcpxPreparedRuntimeLite {
        fingerprint: "abc".into(),
        session_key: "sk".into(),
        acpx_agent: agent.into(),
        mode: "persistent".into(),
        cwd: "/repo".into(),
        remote_execution_identity: None,
        requested_model: model.map(|s| s.to_string()),
        requested_thinking_effort: effort.map(|s| s.to_string()),
        fast_mode: fast,
    }
}

// =============================================================================
// build_session_params
// =============================================================================

#[test]
fn build_session_params_projects_all_workspace_fields() {
    use pc_acpx::AcpRuntimeHandle;
    let prepared = pc_acpx::PreparedRuntime::builder("claude")
        .mode(pc_acpx::PreparedRuntimeMode::Persistent)
        .cwd("/repo")
        .workspace_id("ws_42")
        .workspace_repo_url("git@github.com:foo/bar.git")
        .workspace_repo_ref("main")
        .permission_mode(pc_acpx::PreparedRuntimePermissionMode::ApproveAll)
        .non_interactive_permissions(pc_acpx::PreparedRuntimeNonInteractivePermissions::Deny)
        .state_dir("/state")
        .session_key("paperclip:co:claude:ws_42:abc")
        .fingerprint("abc")
        .env(std::collections::BTreeMap::new())
        .build();
    let handle = AcpRuntimeHandle {
        session_key: "sk".into(),
        backend: "claude".into(),
        runtime_session_name: Some("rsn-1".into()),
        cwd: Some("/repo".into()),
        acpx_record_id: Some("rec-1".into()),
        backend_session_id: Some("bsid-1".into()),
        agent_session_id: Some("asid-1".into()),
    };
    let params = build_session_params(&prepared, &handle);
    assert_eq!(params.runtime_session_name.as_deref(), Some("rsn-1"));
    assert_eq!(
        params.session_key.as_deref(),
        Some("paperclip:co:claude:ws_42:abc")
    );
    assert_eq!(params.acp_session_id.as_deref(), Some("bsid-1"));
    assert_eq!(params.agent_session_id.as_deref(), Some("asid-1"));
    assert_eq!(params.agent.as_deref(), Some("claude"));
    assert_eq!(params.cwd.as_deref(), Some("/repo"));
    assert_eq!(params.mode.as_deref(), Some("persistent"));
    assert_eq!(params.config_fingerprint.as_deref(), Some("abc"));
    assert_eq!(params.workspace_id.as_deref(), Some("ws_42"));
    assert_eq!(
        params.repo_url.as_deref(),
        Some("git@github.com:foo/bar.git")
    );
    assert_eq!(params.repo_ref.as_deref(), Some("main"));
    // Round-trip via serialize.
    let serialized = pc_acpx::session_codec_serialize(Some(&params)).expect("ok");
    assert!(serialized.get("sessionKey").is_some());
}

#[test]
fn build_session_params_drops_empty_workspace_fields() {
    use pc_acpx::AcpRuntimeHandle;
    let prepared = pc_acpx::PreparedRuntime::builder("claude").build();
    let handle = AcpRuntimeHandle {
        session_key: "sk".into(),
        backend: "claude".into(),
        runtime_session_name: Some("rsn".into()),
        cwd: Some("/repo".into()),
        acpx_record_id: Some("rec".into()),
        backend_session_id: Some("bsid".into()),
        agent_session_id: Some("asid".into()),
    };
    let params = build_session_params(&prepared, &handle);
    assert!(params.workspace_id.is_none());
    assert!(params.repo_url.is_none());
    assert!(params.repo_ref.is_none());
}

// =============================================================================
// session_config_options (existing R369 helper, exercised cross-agent)
// =============================================================================

#[test]
fn session_config_options_for_claude_skips_model_only() {
    // Claude pre-sets model via env (ANTHROPIC_MODEL), so set_config_option
    // skips model — but effort and fast_mode are still valid since claude
    // doesn't set those via env.
    let options = session_config_options(&lite("claude", Some("opus"), Some("high"), true));
    assert!(!options.iter().any(|o| o.key == "model"));
    assert!(options
        .iter()
        .any(|o| o.key == "effort" && o.value == "high"));
    assert!(options
        .iter()
        .any(|o| o.key == "service_tier" && o.value == "fast"));
}

#[test]
fn session_config_options_for_codex_skips_all_overrides() {
    // Codex pre-sets model / effort / fast_mode via CODEX_CONFIG
    // (build_codex_startup_config), so set_config_option skips all of
    // them.
    let options = session_config_options(&lite("codex", Some("gpt-5"), Some("high"), true));
    assert!(options.is_empty());
}

#[test]
fn session_config_options_returns_model_for_custom_agent() {
    let options = session_config_options(&lite("custom-agent", Some("custom-model"), None, false));
    assert!(options
        .iter()
        .any(|o| o.key == "model" && o.value == "custom-model"));
    // No effort / fast when not requested.
    assert!(!options.iter().any(|o| o.key == "effort"));
    assert!(!options.iter().any(|o| o.key == "service_tier"));
}

#[test]
fn session_config_options_returns_effort_for_non_codex() {
    let options = session_config_options(&lite("custom-agent", None, Some("high"), false));
    assert!(options
        .iter()
        .any(|o| o.key == "effort" && o.value == "high"));
}

#[test]
fn session_config_options_returns_fast_mode_for_non_codex() {
    let options = session_config_options(&lite("custom-agent", None, None, true));
    assert!(options
        .iter()
        .any(|o| o.key == "service_tier" && o.value == "fast"));
    assert!(options
        .iter()
        .any(|o| o.key == "features.fast_mode" && o.value == "true"));
}

#[test]
fn session_config_options_skips_empty_model() {
    // requested_model = None → no model option pushed.
    let options = session_config_options(&lite("custom-agent", None, Some("low"), false));
    assert!(!options.iter().any(|o| o.key == "model"));
    assert!(options.iter().any(|o| o.key == "effort"));
}

// =============================================================================
// apply_session_config_options (via executor)
// =============================================================================

#[tokio::test]
async fn execute_attaches_session_params_to_completed_result() {
    let executor = build_executor();
    let result = executor.execute(&ctx()).await.expect("execute");
    assert_eq!(result.status, "completed");
    let params = result.session_params.expect("session_params");
    assert_eq!(params.agent.as_deref(), Some("claude"));
    assert_eq!(params.cwd.as_deref(), Some("/repo"));
    assert_eq!(params.workspace_id.as_deref(), Some("ws_42"));
    assert_eq!(params.config_fingerprint.is_some(), true);
}

#[tokio::test]
async fn execute_propagates_session_params_across_warm_resume() {
    let executor = build_executor();
    let r1 = executor.execute(&ctx()).await.expect("first");
    let r2 = executor.execute(&ctx()).await.expect("second");
    // Same session_key → same fingerprint → same params shape.
    assert_eq!(
        r1.session_params.as_ref().map(|p| p.session_key.clone()),
        r2.session_params.as_ref().map(|p| p.session_key.clone())
    );
}

// =============================================================================
// AdapterExecutionResult session_params integration
// =============================================================================

#[test]
fn adapter_execution_result_with_session_params_sets_field() {
    let params = AcpxSessionParams {
        runtime_session_name: Some("rsn".into()),
        session_key: Some("sk".into()),
        acpx_record_id: Some("rec".into()),
        acp_session_id: Some("bsid".into()),
        agent_session_id: Some("asid".into()),
        agent: Some("claude".into()),
        cwd: Some("/repo".into()),
        mode: Some("persistent".into()),
        state_dir: Some("/state".into()),
        config_fingerprint: Some("abc".into()),
        workspace_id: Some("ws_42".into()),
        repo_url: None,
        repo_ref: None,
        remote_execution: None,
    };
    let result = pc_acpx::AdapterExecutionResult::ok_completed(
        &pc_acpx::AcpRuntimeHandle {
            session_key: "sk".into(),
            backend: "claude".into(),
            runtime_session_name: Some("rsn".into()),
            cwd: Some("/repo".into()),
            acpx_record_id: Some("rec".into()),
            backend_session_id: Some("bsid".into()),
            agent_session_id: Some("asid".into()),
        },
        "summary".into(),
        Some("end_turn".into()),
    )
    .with_session_params(params.clone());
    assert!(result.session_params.is_some());
    assert_eq!(
        result.session_params.unwrap().config_fingerprint.as_deref(),
        Some("abc")
    );
}
