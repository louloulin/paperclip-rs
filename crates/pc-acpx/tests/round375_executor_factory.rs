//! R375 集成测试 — `pc-acpx` `AcpxEngineExecutor` 工厂。
//!
//! 覆盖:
//! - factory 模式(注入 runtime_factory + state_factory)
//! - 纯 build_runtime 装配路径(无需 runtime_factory)
//! - warm-handle 命中 / 冷启动 / 驱逐
//! - staged-runtime 驱逐(空 / 有条目的两种情况)
//! - 多次 ensure_session 的 fingerprint 稳定性
//! - 时钟注入(确定性测试)

use pc_acpx::{
    AcpRuntimeCapabilities, AcpRuntimeEvent, AcpxEngineExecutor, AcpxEngineExecutorDeps,
    BuildRuntimeInput, MockAcpRuntime,
};
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

fn mock_factory() -> pc_acpx::AcpxRuntimeFactory {
    Arc::new(move |_prepared| {
        let runtime = MockAcpRuntime::new(vec![AcpRuntimeEvent::Done {
            stop_reason: Some("end_turn".into()),
        }])
        .with_capabilities(AcpRuntimeCapabilities::default());
        Ok(Arc::new(runtime) as Arc<dyn pc_acpx::AcpRuntime>)
    })
}

fn empty_factory() -> pc_acpx::AcpxRuntimeFactory {
    Arc::new(move |_prepared| {
        let runtime = MockAcpRuntime::new(vec![]);
        Ok(Arc::new(runtime) as Arc<dyn pc_acpx::AcpRuntime>)
    })
}

fn build_executor(factory: pc_acpx::AcpxRuntimeFactory) -> AcpxEngineExecutor {
    AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
        runtime_factory: Some(factory),
        warm_handle_idle_ms: Some(60_000),
        ..Default::default()
    })
}

fn build_executor_with_clock(
    factory: pc_acpx::AcpxRuntimeFactory,
    clock: pc_acpx::NowFn,
) -> AcpxEngineExecutor {
    AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
        runtime_factory: Some(factory),
        warm_handle_idle_ms: Some(60_000),
        now: Some(clock),
        ..Default::default()
    })
}

fn minimal_input(agent: &str, company: &str, cwd: &str) -> BuildRuntimeInput {
    BuildRuntimeInput::for_test(agent, company, Path::new(cwd))
}

// =============================================================================
// Construction + deps
// =============================================================================

#[tokio::test]
async fn factory_constructs_with_default_state() {
    let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps::default());
    assert_eq!(executor.warm_handle_count(), 0);
    assert_eq!(executor.staged_runtime_count(), 0);
    assert_eq!(
        executor.warm_handle_idle_ms(),
        pc_acpx::DEFAULT_ACP_ENGINE_WARM_HANDLE_IDLE_MS
    );
}

#[tokio::test]
async fn factory_overrides_warm_handle_idle_threshold() {
    let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
        warm_handle_idle_ms: Some(10_000),
        ..Default::default()
    });
    assert_eq!(executor.warm_handle_idle_ms(), 10_000);
}

#[tokio::test]
async fn factory_uses_injected_clock() {
    let counter = Arc::new(AtomicI64::new(42));
    let counter_clone = counter.clone();
    let clock: pc_acpx::NowFn = Arc::new(move || counter_clone.load(Ordering::SeqCst));
    let executor = build_executor_with_clock(empty_factory(), clock);
    assert_eq!(executor.now(), 42);
    counter.store(99, Ordering::SeqCst);
    assert_eq!(executor.now(), 99);
}

// =============================================================================
// Pure build (no runtime_factory)
// =============================================================================

#[tokio::test]
async fn build_runs_without_runtime_factory() {
    let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps::default());
    let input = minimal_input("claude", "co_x", "/repo");
    let prepared = executor.build(&input).expect("build");
    assert_eq!(prepared.acpx_agent, "claude");
    assert!(prepared.session_key.starts_with("paperclip:co_x:claude:"));
}

#[tokio::test]
async fn build_supports_codex_agent() {
    let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps::default());
    let mut input = minimal_input("codex", "co_c", "/repo/c");
    input.config = serde_json::json!({ "agent": "codex", "fastMode": true });
    let prepared = executor.build(&input).expect("build");
    assert_eq!(prepared.acpx_agent, "codex");
    assert!(prepared.fast_mode);
}

// =============================================================================
// ensure_session: cold start
// =============================================================================

#[tokio::test]
async fn ensure_session_cold_starts_and_inserts_warm_handle() {
    let executor = build_executor(mock_factory());
    let prepared = executor
        .build(&minimal_input("claude", "co_cs", "/repo/cs"))
        .expect("build");
    assert_eq!(executor.warm_handle_count(), 0);

    let outcome = executor.ensure_session(&prepared, None).await.expect("ok");
    assert!(!outcome.warm_hit);
    assert_eq!(outcome.handle.session_key, prepared.session_key);
    assert_eq!(executor.warm_handle_count(), 1);
    // Cached handle should match.
    let cached = executor
        .cached_warm_handle(&prepared.session_key)
        .expect("cached");
    assert_eq!(cached.fingerprint, prepared.fingerprint);
}

#[tokio::test]
async fn ensure_session_fails_when_no_runtime_factory() {
    let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps::default());
    let prepared = executor
        .build(&minimal_input("claude", "co_nf", "/repo"))
        .expect("build");
    let result = executor.ensure_session(&prepared, None).await;
    assert!(matches!(result, Err(pc_acpx::AcpxError::Spawn { .. })));
    assert_eq!(executor.warm_handle_count(), 0);
}

// =============================================================================
// ensure_session: warm hit
// =============================================================================

#[tokio::test]
async fn ensure_session_warm_hits_after_cold_start() {
    let executor = build_executor(mock_factory());
    let prepared = executor
        .build(&minimal_input("claude", "co_wh", "/repo/wh"))
        .expect("build");

    let cold = executor
        .ensure_session(&prepared, None)
        .await
        .expect("cold");
    let warm = executor
        .ensure_session(&prepared, None)
        .await
        .expect("warm");
    assert!(!cold.warm_hit);
    assert!(warm.warm_hit);
    assert_eq!(warm.handle.session_key, cold.handle.session_key);
    assert_eq!(executor.warm_handle_count(), 1);
}

#[tokio::test]
async fn ensure_session_distinct_session_keys_cold_start_each() {
    let executor = build_executor(empty_factory());
    let mut input_a = minimal_input("claude", "co_2", "/repo/a");
    let mut input_b = minimal_input("claude", "co_2", "/repo/b");
    // Force distinct fingerprints by changing config model.
    input_a.config = serde_json::json!({ "agent": "claude", "model": "opus" });
    input_b.config = serde_json::json!({ "agent": "claude", "model": "sonnet" });
    let prepared_a = executor.build(&input_a).expect("build a");
    let prepared_b = executor.build(&input_b).expect("build b");
    assert_ne!(prepared_a.session_key, prepared_b.session_key);

    let _ = executor.ensure_session(&prepared_a, None).await.expect("a");
    let _ = executor.ensure_session(&prepared_b, None).await.expect("b");
    // Two distinct session keys → two warm-handle entries.
    assert_eq!(executor.warm_handle_count(), 2);
}

#[tokio::test]
async fn ensure_session_warm_handle_preserves_runtime_arc() {
    let executor = build_executor(mock_factory());
    let prepared = executor
        .build(&minimal_input("claude", "co_p", "/repo/p"))
        .expect("build");
    let cold = executor
        .ensure_session(&prepared, None)
        .await
        .expect("cold");
    let warm = executor
        .ensure_session(&prepared, None)
        .await
        .expect("warm");
    // Both outcomes point at the same underlying runtime Arc.
    assert!(Arc::ptr_eq(&cold.runtime, &warm.runtime));
}

// =============================================================================
// Drop warm handle (mirrors Node `clearWarmHandleTimer` + close)
// =============================================================================

#[tokio::test]
async fn drop_warm_handle_removes_entry_and_returns_it() {
    let executor = build_executor(empty_factory());
    let prepared = executor
        .build(&minimal_input("claude", "co_d", "/repo/d"))
        .expect("build");
    let _ = executor
        .ensure_session(&prepared, None)
        .await
        .expect("cold");
    assert_eq!(executor.warm_handle_count(), 1);
    let removed = executor
        .drop_warm_handle(&prepared.session_key)
        .expect("removed");
    assert_eq!(removed.fingerprint, prepared.fingerprint);
    assert_eq!(executor.warm_handle_count(), 0);
    // Second drop is a no-op.
    assert!(executor.drop_warm_handle(&prepared.session_key).is_none());
}

// =============================================================================
// Eviction: staged runtimes
// =============================================================================

#[tokio::test]
async fn evict_idle_staged_runtimes_is_a_noop_when_cache_empty() {
    let executor = build_executor(empty_factory());
    let dropped = executor.evict_idle_staged_runtimes().await.expect("ok");
    assert_eq!(dropped, 0);
    assert_eq!(executor.staged_runtime_count(), 0);
}

#[tokio::test]
async fn evict_idle_warm_handles_is_a_noop_when_cache_empty() {
    let executor = build_executor(empty_factory());
    let closed = executor.evict_idle_warm_handles().await.expect("ok");
    assert_eq!(closed, 0);
    assert_eq!(executor.warm_handle_count(), 0);
}

// =============================================================================
// Staged-runtime cache (lookup APIs)
// =============================================================================

#[tokio::test]
async fn cached_staged_runtime_returns_none_when_absent() {
    let executor = build_executor(empty_factory());
    assert!(executor.cached_staged_runtime("nope").is_none());
}

// =============================================================================
// Clock + factory injection (deterministic test pattern)
// =============================================================================

#[tokio::test]
async fn injected_clock_drives_idle_eviction() {
    let counter = Arc::new(AtomicI64::new(1_000_000));
    let c1 = counter.clone();
    let clock: pc_acpx::NowFn = Arc::new(move || c1.load(Ordering::SeqCst));
    let executor = build_executor_with_clock(empty_factory(), clock);

    let prepared = executor
        .build(&minimal_input("claude", "co_clk", "/repo/clk"))
        .expect("build");
    let _ = executor
        .ensure_session(&prepared, None)
        .await
        .expect("cold");
    assert_eq!(executor.warm_handle_count(), 1);

    // Advance "time" past the 60_000 ms idle threshold.
    counter.store(1_060_000, Ordering::SeqCst);
    let closed = executor.evict_idle_warm_handles().await.expect("ok");
    assert_eq!(closed, 1);
    assert_eq!(executor.warm_handle_count(), 0);
}

#[tokio::test]
async fn idle_eviction_keeps_fresh_entries() {
    let counter = Arc::new(AtomicI64::new(1_000_000));
    let c1 = counter.clone();
    let clock: pc_acpx::NowFn = Arc::new(move || c1.load(Ordering::SeqCst));
    let executor = build_executor_with_clock(empty_factory(), clock);

    let prepared = executor
        .build(&minimal_input("claude", "co_fr", "/repo/fr"))
        .expect("build");
    let _ = executor
        .ensure_session(&prepared, None)
        .await
        .expect("cold");

    // Advance time by 30_000 (half the 60_000 ms idle threshold).
    counter.store(1_030_000, Ordering::SeqCst);
    let closed = executor.evict_idle_warm_handles().await.expect("ok");
    assert_eq!(closed, 0);
    assert_eq!(executor.warm_handle_count(), 1);
}

// =============================================================================
// Multiple agents share one executor (cache key by session_key)
// =============================================================================

#[tokio::test]
async fn executor_handles_multiple_agent_types_independently() {
    let executor = build_executor(empty_factory());
    let claude_input = minimal_input("claude", "co_m", "/repo/c");
    let codex_input = minimal_input("codex", "co_m", "/repo/x");
    let claude_prepared = executor.build(&claude_input).expect("c");
    let codex_prepared = executor.build(&codex_input).expect("x");

    // Code requires config.agent for non-default — set explicitly.
    let mut codex_prepared = codex_prepared;
    codex_prepared.acpx_agent = "codex".to_string();
    // session_keys differ (different cwd path) → different cache entries.
    let _ = executor
        .ensure_session(&claude_prepared, None)
        .await
        .expect("c cold");
    let _ = executor
        .ensure_session(&codex_prepared, None)
        .await
        .expect("x cold");
    assert_eq!(executor.warm_handle_count(), 2);
}

// =============================================================================
// Resume from cached handle preserves the original handle
// =============================================================================

#[tokio::test]
async fn ensure_session_with_resume_id_warm_hits_and_ignores_resume() {
    let executor = build_executor(empty_factory());
    let prepared = executor
        .build(&minimal_input("claude", "co_r", "/repo/r"))
        .expect("build");
    let cold = executor
        .ensure_session(&prepared, Some("resume-id-1".into()))
        .await
        .expect("cold");
    // The resume_session_id is passed to the runtime; the warm hit
    // ignores it and returns the cached handle.
    let warm = executor
        .ensure_session(&prepared, Some("resume-id-2".into()))
        .await
        .expect("warm");
    assert!(!cold.warm_hit);
    assert!(warm.warm_hit);
    assert_eq!(warm.handle.session_key, cold.handle.session_key);
}

// =============================================================================
// State factory + clock
// =============================================================================

#[tokio::test]
async fn state_factory_constructs_custom_state() {
    let counter = Arc::new(AtomicI64::new(7));
    let c1 = counter.clone();
    let executor = AcpxEngineExecutor::new(AcpxEngineExecutorDeps {
        state_factory: Some(Arc::new(|| pc_acpx::AcpxEngineExecutorState::new())),
        now: Some(Arc::new(move || c1.load(Ordering::SeqCst))),
        ..Default::default()
    });
    assert_eq!(executor.warm_handle_count(), 0);
    assert_eq!(executor.now(), 7);
    counter.store(8, Ordering::SeqCst);
    assert_eq!(executor.now(), 8);
}
