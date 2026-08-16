//! R660 — real heartbeat tick E2E invocation
//!
//! Runs run_heartbeat_tick for one company against the existing test PG.
//!
//! Integration test gated by real PG availability; runs full sweep.

#[tokio::test]
async fn r660_real_pg_heartbeat_tick_runs() {
    let db = pc_repos::Db::connect(
        "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos",
        4, 0,
    ).await.expect("connect");
    let companies = pc_heartbeat::recovery::list_active_companies(&db)
        .await.expect("list active companies");
    eprintln!("R660: {} active companies", companies.len());
    let wake = pc_repos::agent::NewAgentWakeupRequest {
        company_id: uuid::Uuid::nil(),
        agent_id: uuid::Uuid::nil(),
        source: pc_repos::agent::HeartbeatInvocationSource::Timer,
        trigger_detail: None,
        reason: Some("r660-test".into()),
        payload: None,
        status: pc_repos::agent::WakeupRequestStatus::Queued,
        coalesced_count: 1,
        requested_by_actor_type: None,
        requested_by_actor_id: None,
        idempotency_key: None,
        run_id: None,
        error: None,
    };
    let config = pc_heartbeat::recovery::HeartbeatTickerConfig {
        max_candidates: 5,
        ..Default::default()
    };
    let target = companies.first().copied().into_iter().collect::<Vec<_>>();
    let result = pc_heartbeat::recovery::run_heartbeat_tick(
        &db, &config, &wake, &target,
    ).await.expect("tick");
    eprintln!(
        "R660 tick: stale_lock_cleared={}, elapsed_ms={}, stranded={:?}",
        result.stale_lock_cleared, result.elapsed_ms, result.stranded,
    );
    assert!(result.elapsed_ms < 30_000, "tick must complete in <30s");
}
