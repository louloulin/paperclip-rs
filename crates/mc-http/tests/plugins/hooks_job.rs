//! `POST /api/plugin-bridge/v1/hooks/:key` 的 **job 数据面**测试（M6-8 / `LUM-1673`）。
//!
//! 这里只放 `hooks_job::dispatch_scheduled_hook` 的三个真库用例（上游
//! `scheduler/jobs_plugin_hook.go` 的 handler 实体）：一格投递落 `trigger='schedule'` 行
//! （带 `delivery_id` / `planned_at`）并推进展示列、换代判无效、manifest 与投影不一致判跳过。
//!
//! 路由与凭据面在 `hooks.rs`（门 ⑩ 的 800 行硬上限逼出来的拆分）；
//! 夹具（`hook_req` / `installed` / `invocations` 等）从 `hooks.rs` 借。
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`），由门 ⑥ 用 `-- --ignored` 拉起。

use super::hooks::*;
use super::runtime_support::*;
use super::support::*;
use serde_json::json;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ⑤ job 的数据面（`dispatch_scheduled_hook`）
// ---------------------------------------------------------------------------

/// 装一个**带日程**的钩子，并把 `plugin_hook_schedule` 行按 `reconcile_tx` 的语义造出来。
async fn scheduled_installation(
    app: &axum::Router,
    pool: &sqlx::PgPool,
    workspace_id: Uuid,
    owner: Uuid,
) -> (Uuid, Uuid, Uuid) {
    let contributes = json!({ "hooks": [ { "key": "sync", "name": "Sync",
        "description": "scheduled delivery",
        "triggers": ["schedule"],
        "schedule": { "cron": "*/5 * * * *", "timezone": "UTC" },
        "transport": { "type": "http", "url": "https://mcp.example.com/hook" } } ] });
    let (installation_id, version_id) = installed(
        app,
        pool,
        workspace_id,
        owner,
        "com.example.sched",
        &contributes,
    )
    .await;
    let installation_uuid = installation_id;
    // `ON CONFLICT`：上一轮失败留下的孤儿行（表没有外键）不该让夹具红 ——
    // 本用例要验的是投递语义，不是唯一索引。
    let generation: Uuid = sqlx::query_scalar(
        "INSERT INTO plugin_hook_schedule \
           (installation_id, workspace_id, hook_key, cron_expression, timezone, enabled) \
         VALUES ($1, $2, 'sync', '*/5 * * * *', 'UTC', TRUE) \
         ON CONFLICT (installation_id, hook_key) DO UPDATE \
           SET cron_expression = EXCLUDED.cron_expression, \
               timezone = EXCLUDED.timezone, enabled = TRUE, updated_at = now() \
         RETURNING generation",
    )
    .bind(installation_uuid)
    .bind(workspace_id)
    .fetch_one(pool)
    .await
    .expect("seed schedule");
    let schedule_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM plugin_hook_schedule WHERE installation_id = $1 AND hook_key = 'sync'",
    )
    .bind(installation_uuid)
    .fetch_one(pool)
    .await
    .expect("schedule id");
    (schedule_id, generation, version_id)
}

/// `DoD`：一格投递落 `trigger='schedule'` 行（带 `delivery_id` / `planned_at`）并推进展示列；
/// 换代之后同一个 scope 判无效。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_scheduled_occurrence_is_delivered_once_and_advances_the_display_column() {
    use mc_core::Id;
    use mc_http::routes::plugins::hooks_job::{
        self, HookRuntime, ScheduledHookOutcome, ScheduledHookRequest,
    };

    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let state = state_for(&pool, db);
    let app = app_from(state.clone());
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    let (schedule_id, generation, version_id) =
        scheduled_installation(&app, &pool, workspace_id, owner).await;
    let installation_id = installation_uuid_of(&pool, schedule_id).await;

    let runtime = HookRuntime::from_state(&state);
    let plan_time = chrono::Utc::now();
    let request = ScheduledHookRequest {
        schedule_id: Id::from(schedule_id),
        generation: Id::from(generation),
        plan_time,
        attempt: 1,
        last_attempt: true,
    };

    // 端点解析不到 ⇒ 目的地判据拒了（403，与路由面同一套判据）；但 `attempt == max`
    // ⇒ 展示列**必须**被推进（否则时间线卡死）。
    let error = hooks_job::dispatch_scheduled_hook(&runtime, request)
        .await
        .expect_err("端点不可达 ⇒ 这一格失败");
    assert_eq!(error.status(), 403, "{error}");
    assert_eq!(error.invocation_status(), "refused", "{error}");

    let rows = invocations(&pool, installation_id).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let (trigger, status, attempt, _, delivery_id) = &rows[0];
    assert_eq!(trigger, "schedule");
    assert_eq!(status, "refused");
    assert_eq!(*attempt, 1);
    let delivery_id = delivery_id.clone().expect("计划投递必须带 delivery_id");
    assert!(delivery_id.starts_with("psd_"), "{delivery_id}");
    assert_eq!(
        delivery_id,
        hooks_job::schedule_delivery_id(
            Id::from(installation_id),
            "sync",
            Id::from(generation),
            plan_time
        ),
        "投递 id 必须与 job 侧算出来的一致（同一次计划投递跨重试稳定）"
    );
    let planned_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT planned_at FROM plugin_invocation WHERE installation_id = $1")
            .bind(installation_id)
            .fetch_one(&pool)
            .await
            .expect("planned_at");
    assert_eq!(
        planned_at.map(|value| value.timestamp()),
        Some(plan_time.timestamp()),
        "planned_at 是 cron 的计划发生时刻，不是真实尝试时刻"
    );

    let next_run_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT next_run_at FROM plugin_hook_schedule WHERE id = $1")
            .bind(schedule_id)
            .fetch_one(&pool)
            .await
            .expect("next_run_at");
    let next_run_at = next_run_at.expect("last_attempt ⇒ 展示列必须被推进");
    assert!(
        next_run_at > plan_time,
        "展示列必须指向下一格：{next_run_at} vs {plan_time}"
    );

    // 换代 ⇒ 同一个 scope 判无效（老一代的投递不能打到新配置上）。
    let outcome = hooks_job::dispatch_scheduled_hook(
        &runtime,
        ScheduledHookRequest {
            generation: Id::from(Uuid::new_v4()),
            ..request
        },
    )
    .await
    .expect("换代是「跳过」而不是错误");
    assert_eq!(
        outcome,
        ScheduledHookOutcome::Skipped("schedule_generation_changed")
    );
    assert_eq!(invocations(&pool, installation_id).await.len(), 1);

    // 日程行不在 ⇒ 也是「跳过」。
    let outcome = hooks_job::dispatch_scheduled_hook(
        &runtime,
        ScheduledHookRequest {
            schedule_id: Id::from(Uuid::new_v4()),
            ..request
        },
    )
    .await
    .expect("未知日程是「跳过」");
    assert_eq!(outcome, ScheduledHookOutcome::Skipped("schedule_not_found"));

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_schedules(&pool, installation_id).await;
    cleanup_runtime(&pool, installation_id, version_id).await;
}

/// 日程行所属的安装 id（job 数据面用例要按它读 `plugin_invocation`）。
pub(crate) async fn installation_uuid_of(pool: &sqlx::PgPool, schedule_id: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT installation_id FROM plugin_hook_schedule WHERE id = $1")
        .bind(schedule_id)
        .fetch_one(pool)
        .await
        .expect("installation id")
}

/// 让「安装行的 manifest 与日程一致」这条判据也可测：manifest 里没有 schedule 段 ⇒ 跳过。
#[tokio::test]
#[ignore = "requires MULTICA_TEST_DATABASE_URL"]
async fn a_schedule_without_a_matching_manifest_is_skipped() {
    use mc_core::Id;
    use mc_http::routes::plugins::hooks_job::{
        self, HookRuntime, ScheduledHookOutcome, ScheduledHookRequest,
    };

    let Some((pool, db)) = connect().await else {
        println!("skipping: MULTICA_TEST_DATABASE_URL not set");
        return;
    };
    let state = state_for(&pool, db);
    let app = app_from(state.clone());
    let (workspace_id, owner) = seed_workspace(&pool, "owner").await;
    // manifest 的 hook 只有 `manual` 触发、**没有** schedule 段。
    let (installation_id, version_id) = installed(
        &app,
        &pool,
        workspace_id,
        owner,
        "com.example.nosched",
        &http_hook_manifest("sync"),
    )
    .await;
    let installation_uuid = installation_id;
    let (schedule_id, generation): (Uuid, Uuid) = sqlx::query_as(
        "INSERT INTO plugin_hook_schedule \
           (installation_id, workspace_id, hook_key, cron_expression, timezone, enabled) \
         VALUES ($1, $2, 'sync', '0 9 * * *', 'UTC', TRUE) \
         ON CONFLICT (installation_id, hook_key) DO UPDATE \
           SET cron_expression = EXCLUDED.cron_expression, \
               timezone = EXCLUDED.timezone, enabled = TRUE, updated_at = now() \
         RETURNING id, generation",
    )
    .bind(installation_uuid)
    .bind(workspace_id)
    .fetch_one(&pool)
    .await
    .expect("seed schedule");

    let outcome = hooks_job::dispatch_scheduled_hook(
        &HookRuntime::from_state(&state),
        ScheduledHookRequest {
            schedule_id: Id::from(schedule_id),
            generation: Id::from(generation),
            plan_time: chrono::Utc::now(),
            attempt: 1,
            last_attempt: true,
        },
    )
    .await
    .expect("manifest 不符是「跳过」");
    assert_eq!(
        outcome,
        ScheduledHookOutcome::Skipped("manifest_changed"),
        "投影与已同意的 manifest 不一致时不能照旧调用"
    );
    assert!(invocations(&pool, installation_uuid).await.is_empty());

    cleanup(&pool, workspace_id, &[owner]).await;
    cleanup_runtime(&pool, installation_uuid, version_id).await;
}
