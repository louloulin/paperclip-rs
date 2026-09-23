//! B 段（worker 认领→派发→收口）e2e（M5-5 / LUM-1570）。
//!
//! # 上游映射
//!
//! `handler/webhook_delivery_worker.go` 的 `ProcessNext` + `handleWebhookLeaseMutation`，
//! 与 `service/autopilot.go` 的 `DispatchAutopilotForWebhookDelivery`。本片**只实现**
//! 「认领一条 + 推到终态」这一步（[`mc_autopilot::webhook::WebhookIngress::process_next_delivery_in_workspace`]），
//! **不含轮询循环**（1s ticker / `Notify` / 4 并发属 M5-8 ⇒ `docs/54` D8）。
//!
//! 认领为什么按 workspace 收窄：认领是**整库**的（上游 worker 是单例），而同一 binary 里的用例
//! 并发跑 —— 全局认领会把邻例刚落的 `queued` 行抢走（`deliveries_replay.rs` 正断言那条 `queued`）。
//! 每个用例有自己的 workspace ⇒ 收窄后互不打扰（偏差 `docs/54` D9）。
//!
//! # 为什么在这里驱动
//!
//! 同 `dispatch.rs`：`mc-autopilot` 没有 `tokio` 依赖，本波禁止新增第三方依赖 ⇒
//! `#[tokio::test]` 只能写在 `mc-http` 的测试 target 里。真库 + 真 SQL，不 mock。
//!
//! # 收口判据
//!
//! | 形态 | delivery 终态 | run |
//! | --- | --- | --- |
//! | 派发成功 | `dispatched` | `running`（`run_only`：任务挂 `autopilot_run_id`） |
//! | 准入闸跳过 | `dispatched` | 入站已建的 `skipped`（**复用**，不建第二条） |
//! | 暂停 / 归档 / 停用 | `ignored` | 无 |
//! | 归属交叉校验失败 | `failed` | 无（行本身脏了，重试没意义） |
//! | 归一化失败 | `failed` | 无 |
//! | 已有 run 是 `failed` | `failed` | 沿用既有 run |
//! | 每 trigger 预算用尽 | `queued`（推迟） | 无 |

use mc_autopilot::webhook::provider::WebhookHeaders;
use mc_autopilot::webhook::ratelimit;
use mc_autopilot::webhook::{InboundRequest, WebhookIngress};
use mc_repos::autopilot::ingress as ingress_sql;
use sqlx::PgPool;
use uuid::Uuid;

use super::support::{cleanup, connect, seed_workspace};
use super::webhook_support::{cleanup_all, seed_agent, seed_autopilot, seed_trigger, unique_token};

/// 读一条投递的「收口面」：状态 / 错误 / 原因码 / 派发计数 / run / 租约 / `available_at`。
async fn settled(
    pool: &PgPool,
    id: Uuid,
) -> (
    String,
    Option<String>,
    Option<String>,
    i32,
    Option<Uuid>,
    Option<Uuid>,
    bool,
) {
    sqlx::query_as(
        "SELECT status, error, reason_code, dispatch_attempts, autopilot_run_id, lease_token, \
             available_at > now() \
         FROM webhook_delivery WHERE id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("load delivery")
}

/// 直接插一条 `queued` 投递（绕过入站）：用来造「崩溃窗口」——投递落库了但还没准入 run。
async fn insert_queued_delivery(
    pool: &PgPool,
    workspace_id: Uuid,
    autopilot_id: Uuid,
    trigger_id: Uuid,
    raw_body: &str,
    dedupe_key: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO webhook_delivery \
            (workspace_id, autopilot_id, trigger_id, provider, event, dedupe_key, dedupe_source, \
             signature_status, status, selected_headers, content_type, raw_body) \
         VALUES ($1, $2, $3, 'github', 'github.push', $4, 'x-github-delivery', 'not_required', \
             'queued', '{}'::jsonb, 'application/json', $5::bytea) RETURNING id",
    )
    .bind(workspace_id)
    .bind(autopilot_id)
    .bind(trigger_id)
    .bind(dedupe_key)
    .bind(raw_body.as_bytes())
    .fetch_one(pool)
    .await
    .expect("insert queued delivery")
}

/// 一次入站（走真 HTTP 面的那部分在 `webhook.rs`；这里只要「投递 + 准入 run」这个前置态）。
async fn accept_inbound(pool: &PgPool, token: &str, body: &[u8], dedupe_key: &str) -> Uuid {
    let ingress = WebhookIngress::new(pool.clone());
    let mut headers = WebhookHeaders::new();
    headers.set("x-github-event", "push");
    headers.set("x-github-delivery", dedupe_key);
    headers.set("content-type", "application/json");
    let outcome = ingress
        .handle_inbound(&InboundRequest {
            token,
            peer_ip: None,
            headers,
            body,
        })
        .await
        .expect("inbound");
    outcome_delivery_id(&outcome)
}

fn outcome_delivery_id(outcome: &mc_autopilot::webhook::InboundOutcome) -> Uuid {
    use mc_autopilot::webhook::InboundOutcome as O;
    match outcome {
        O::Accepted { delivery_id, .. }
        | O::Skipped { delivery_id, .. }
        | O::EventFiltered { delivery_id, .. }
        | O::Ignored { delivery_id, .. }
        | O::QuotaExceeded { delivery_id }
        | O::Duplicate { delivery_id, .. }
        | O::Rejected { delivery_id, .. } => *delivery_id,
    }
}

async fn count_runs(pool: &PgPool, autopilot_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM autopilot_run WHERE autopilot_id = $1")
        .bind(autopilot_id)
        .fetch_one(pool)
        .await
        .expect("count runs")
}

/// 入站同步建出的那条 run（按投递 id 找）。
async fn run_for_delivery(pool: &PgPool, delivery_id: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT id FROM autopilot_run WHERE webhook_delivery_id = $1")
        .bind(delivery_id)
        .fetch_one(pool)
        .await
        .expect("admitted run")
}

// ---------------------------------------------------------------------------
// ① 成功路径
// ---------------------------------------------------------------------------

/// 入站准入 → worker 认领 → `run_only` 任务入队 → 投递 `dispatched` + trigger `last_fired_at`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn worker_dispatches_a_queued_delivery_and_marks_it_dispatched() {
    let Some((pool, _db)) = connect().await else {
        println!("skip worker_dispatches_a_queued_delivery_and_marks_it_dispatched: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;

    let delivery_id = accept_inbound(
        &pool,
        &token,
        br#"{"ref":"refs/heads/main"}"#,
        &format!("w1-{}", Uuid::new_v4()),
    )
    .await;
    let queued = settled(&pool, delivery_id).await;
    assert_eq!(queued.0, "queued", "入站只准入，不派发");
    // 入站同步建了 run，但**不回填** `webhook_delivery.autopilot_run_id` —— 上游入站只
    // `Acknowledge`（status/response_*），run 链是 worker 收口时写的。
    assert!(queued.4.is_none(), "入站不写 run 链");
    let admitted_run = run_for_delivery(&pool, delivery_id).await;

    let ingress = WebhookIngress::new(pool.clone());
    let processed = ingress
        .process_next_delivery_in_workspace(ws)
        .await
        .expect("process next")
        .expect("一条到期投递");
    assert_eq!(processed.id, delivery_id);

    let row = settled(&pool, delivery_id).await;
    assert_eq!(row.0, "dispatched", "error={:?}", row.1);
    assert_eq!(row.1, None, "成功路径不写 error");
    assert_eq!(row.2, None, "成功路径不写 reason_code");
    assert_eq!(
        row.3, 1,
        "`CompleteClaimedWebhookDelivery` 一律 +1（上游同款）"
    );
    assert_eq!(
        row.4,
        Some(admitted_run),
        "投递回链的 run 必须还是入站那一条"
    );
    assert_eq!(row.5, None, "终态必须释放租约");

    // `run_only`：任务挂 `autopilot_run_id`、不建 issue；run 直接 `running`。
    let run_id = row.4.expect("run id");
    let run: (String, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT status, issue_id, task_id FROM autopilot_run WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("load run");
    assert_eq!(run.0, "running");
    assert!(run.1.is_none(), "run_only 不建 issue");
    let task_id = run.2.expect("run.task_id");
    let task: (Option<Uuid>, Option<Uuid>, String) = sqlx::query_as(
        "SELECT issue_id, autopilot_run_id, status FROM agent_task_queue WHERE id = $1",
    )
    .bind(task_id)
    .fetch_one(&pool)
    .await
    .expect("load task");
    assert!(task.0.is_none());
    assert_eq!(task.1, Some(run_id));
    assert_eq!(task.2, "queued");
    assert_eq!(count_runs(&pool, autopilot_id).await, 1);

    // `TouchAutopilotTriggerFiredAt`：worker 收口的最后一步。
    let fired: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_fired_at FROM autopilot_trigger WHERE id = $1")
            .bind(trigger_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(fired.is_some(), "派发成功要把 last_fired_at 前移");

    // 队列现在空了：第二次调用返回 `None`，且什么都不改（幂等）。
    assert!(
        ingress
            .process_next_delivery_in_workspace(ws)
            .await
            .expect("second")
            .is_none(),
        "已收口的投递不该再被认领"
    );
    assert_eq!(settled(&pool, delivery_id).await.0, "dispatched");
    assert_eq!(count_runs(&pool, autopilot_id).await, 1);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ② 崩溃窗口：投递在库、run 未建
// ---------------------------------------------------------------------------

/// 入站准入闸就判跳过（assignee 已不存在）⇒ 投递 `dispatched`，run **复用**那条 `skipped`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn a_skipped_run_still_settles_the_delivery_as_dispatched() {
    let Some((pool, _db)) = connect().await else {
        println!("skip a_skipped_run_still_settles_the_delivery_as_dispatched: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    // 指一个不存在的 assignee：`resolveLeader` 判 `Missing{squad:false}` ⇒ 准入闸在**入站**就跳过
    // （`assignee agent no longer exists` / `target_unavailable`），run 直接落 `skipped`、不建任务。
    // （「绑了 agent 但 agent 没 runtime」那条跳过不在这里：上游准入闸不管 readiness，那是 worker
    //   建任务前才判的，本地由 `require_bound_runtime` 在建 task 前拦 —— 见 `docs/52`。）
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", Uuid::new_v4(), owner).await;
    let token = unique_token();
    seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;

    // 入站：响应就是 `skipped`，但投递留在 `queued` 等 worker 收口（上游同款）。
    let ingress = WebhookIngress::new(pool.clone());
    let mut headers = WebhookHeaders::new();
    headers.set("x-github-event", "push");
    headers.set("content-type", "application/json");
    let outcome = ingress
        .handle_inbound(&InboundRequest {
            token: &token,
            peer_ip: None,
            headers,
            body: br#"{"ref":"refs/heads/main"}"#,
        })
        .await
        .expect("inbound");
    let delivery_id = outcome_delivery_id(&outcome);
    let skipped_run = match outcome {
        mc_autopilot::webhook::InboundOutcome::Skipped { run_id, .. } => run_id,
        other => panic!("expected skipped, got {other:?}"),
    };

    let processed = ingress
        .process_next_delivery_in_workspace(ws)
        .await
        .expect("process next")
        .expect("一条到期投递");
    assert_eq!(processed.id, delivery_id);

    let row = settled(&pool, delivery_id).await;
    assert_eq!(row.0, "dispatched", "跳过也要对 provider 有个交代");
    assert_eq!(row.4, Some(skipped_run), "必须复用入站那条 skipped run");
    assert_eq!(count_runs(&pool, autopilot_id).await, 1, "不许建第二条 run");

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 崩溃窗口（投递在库、run 未建）+ 暂停 ⇒ worker 重查可变状态，收口 `ignored`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn worker_rechecks_mutable_state_only_when_no_run_exists() {
    let Some((pool, _db)) = connect().await else {
        println!("skip worker_rechecks_mutable_state_only_when_no_run_exists: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "paused", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let delivery_id = insert_queued_delivery(
        &pool,
        ws,
        autopilot_id,
        trigger_id,
        r#"{"ref":"refs/heads/main"}"#,
        &format!("w3-{}", Uuid::new_v4()),
    )
    .await;

    WebhookIngress::new(pool.clone())
        .process_next_delivery_in_workspace(ws)
        .await
        .expect("process next")
        .expect("一条到期投递");

    let row = settled(&pool, delivery_id).await;
    assert_eq!(row.0, "ignored");
    assert_eq!(row.1.as_deref(), Some("autopilot_paused"));
    // 原因写在 `error` 里（上游 worker 的 `complete(...)` 第 5 个参数）；`reason_code` 只有配额那条会写。
    assert_eq!(row.2, None, "reason_code 仅配额路径写");
    assert_eq!(row.4, None);
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ③ 失败面
// ---------------------------------------------------------------------------

/// 归属交叉校验：投递指的 autopilot 与 trigger 的宿主不是同一个 ⇒ `failed`，**不重试**。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn worker_fails_the_delivery_on_an_ownership_mismatch() {
    let Some((pool, _db)) = connect().await else {
        println!("skip worker_fails_the_delivery_on_an_ownership_mismatch: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let owner_autopilot = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let other_autopilot = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, owner_autopilot, &token, "github", true, None, None).await;
    // 脏行：投递挂在别的 autopilot 上。
    let delivery_id = insert_queued_delivery(
        &pool,
        ws,
        other_autopilot,
        trigger_id,
        r#"{"ref":"refs/heads/main"}"#,
        &format!("w4-{}", Uuid::new_v4()),
    )
    .await;

    WebhookIngress::new(pool.clone())
        .process_next_delivery_in_workspace(ws)
        .await
        .expect("process next")
        .expect("一条到期投递");

    let row = settled(&pool, delivery_id).await;
    assert_eq!(row.0, "failed");
    assert_eq!(row.1.as_deref(), Some("delivery ownership mismatch"));
    assert_eq!(
        row.3, 1,
        "complete 也 +1，但**不排下一次**（租约已释放、无 backoff）"
    );
    assert_eq!(row.4, None);
    assert_eq!(count_runs(&pool, other_autopilot).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 落库的原始 body 解不开 ⇒ `failed` + `normalize stored body: …`（上游同款，**不重试**）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn worker_fails_the_delivery_when_the_stored_body_cannot_be_normalized() {
    let Some((pool, _db)) = connect().await else {
        println!(
            "skip worker_fails_the_delivery_when_the_stored_body_cannot_be_normalized: no env"
        );
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let delivery_id = insert_queued_delivery(
        &pool,
        ws,
        autopilot_id,
        trigger_id,
        "not json at all",
        &format!("w5-{}", Uuid::new_v4()),
    )
    .await;

    WebhookIngress::new(pool.clone())
        .process_next_delivery_in_workspace(ws)
        .await
        .expect("process next")
        .expect("一条到期投递");

    let row = settled(&pool, delivery_id).await;
    assert_eq!(row.0, "failed");
    assert!(
        row.1
            .as_deref()
            .is_some_and(|e| e.starts_with("normalize stored body: ")),
        "{:?}",
        row.1
    );
    assert_eq!(row.3, 1, "归一化失败是终态：+1 后不再重排");
    assert_eq!(row.4, None);
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 已有 run 是 `failed` ⇒ 投递跟着 `failed`，错误文案取 run 的 `failure_reason`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn worker_settles_the_delivery_as_failed_when_the_run_is_failed() {
    let Some((pool, _db)) = connect().await else {
        println!("skip worker_settles_the_delivery_as_failed_when_the_run_is_failed: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let delivery_id = insert_queued_delivery(
        &pool,
        ws,
        autopilot_id,
        trigger_id,
        r#"{"ref":"refs/heads/main"}"#,
        &format!("w6-{}", Uuid::new_v4()),
    )
    .await;
    // 崩溃窗口的另一种形态：run 已准入（且已经跑失败了），投递还在 `queued`。
    let run_id: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot_run \
            (autopilot_id, trigger_id, source, status, webhook_delivery_id, failure_reason, \
             reason_code) \
         VALUES ($1, $2, 'webhook', 'failed', $3, 'agent exploded', 'internal_error') RETURNING id",
    )
    .bind(autopilot_id)
    .bind(trigger_id)
    .bind(delivery_id)
    .fetch_one(&pool)
    .await
    .expect("insert failed run");

    WebhookIngress::new(pool.clone())
        .process_next_delivery_in_workspace(ws)
        .await
        .expect("process next")
        .expect("一条到期投递");

    let row = settled(&pool, delivery_id).await;
    assert_eq!(row.0, "failed");
    assert_eq!(row.1.as_deref(), Some("agent exploded"));
    assert_eq!(row.4, Some(run_id), "沿用既有 run，不新建");
    assert_eq!(count_runs(&pool, autopilot_id).await, 1);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ④ 每 trigger 预算（推迟）与退避 SQL
// ---------------------------------------------------------------------------

/// 每 trigger 预算用尽 ⇒ **只推迟**（`queued` + `available_at` 前移），不计派发尝试。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn worker_defers_when_the_per_trigger_budget_is_spent() {
    let Some((pool, _db)) = connect().await else {
        println!("skip worker_defers_when_the_per_trigger_budget_is_spent: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let delivery_id = insert_queued_delivery(
        &pool,
        ws,
        autopilot_id,
        trigger_id,
        r#"{"ref":"refs/heads/main"}"#,
        &format!("w7-{}", Uuid::new_v4()),
    )
    .await;

    // 把这条 trigger 的 60/60s 预算花光（键是 `trigger_id`，本用例独有）。
    for _ in 0..60 {
        assert!(ratelimit::allow_trigger(trigger_id).is_ok());
    }
    assert!(ratelimit::allow_trigger(trigger_id).is_err(), "预算该满了");

    let processed = WebhookIngress::new(pool.clone())
        .process_next_delivery_in_workspace(ws)
        .await
        .expect("process next")
        .expect("一条到期投递");
    assert_eq!(processed.id, delivery_id);

    let row = settled(&pool, delivery_id).await;
    assert_eq!(row.0, "queued", "推迟不是终态");
    assert_eq!(row.3, 0, "推迟**不计**派发尝试（上游同款）");
    assert_eq!(row.4, None);
    assert_eq!(row.5, None, "推迟要释放租约");
    assert!(row.6, "available_at 必须前移");
    assert!(row.1.is_none(), "推迟不是失败：不写 error");
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 退避/重试的 SQL 语义（`RetryClaimedWebhookDelivery`）：计数 +1、租约释放、`available_at` 前移、
/// 返回给 provider 的 `response_*` **不动**。
///
/// 上游这条腿只在「瞬时失败」时走到；本地 e2e 拿不到可靠触发点（要库故障注入），
/// 所以直接对 SQL 钉语义（`mc-repos` 的 `ingress` 自由函数是本片写集，允许在测试里直调）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn retry_sql_bumps_the_attempt_counter_and_keeps_the_response_fields() {
    let Some((pool, _db)) = connect().await else {
        println!("skip retry_sql_bumps_the_attempt_counter_and_keeps_the_response_fields: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let delivery_id = insert_queued_delivery(
        &pool,
        ws,
        autopilot_id,
        trigger_id,
        r#"{"ref":"refs/heads/main"}"#,
        &format!("w8-{}", Uuid::new_v4()),
    )
    .await;
    // 入站那一次的 200 已经发出去了，事后改写只会让运维看到两条矛盾的记录。
    sqlx::query(
        "UPDATE webhook_delivery SET response_status = 200, response_body = '{}' WHERE id = $1",
    )
    .bind(delivery_id)
    .execute(&pool)
    .await
    .unwrap();

    let claimed = ingress_sql::claim_queued_in_workspace(&pool, ws)
        .await
        .expect("claim")
        .expect("一条到期投递");
    assert_eq!(claimed.id, delivery_id);
    let lease = claimed.lease_token.expect("租约");
    let retried = ingress_sql::retry_claimed(
        &pool,
        delivery_id,
        lease,
        chrono::Utc::now() + chrono::Duration::seconds(2),
        "transient failure",
    )
    .await
    .expect("retry")
    .expect("租约仍归本 worker");
    assert_eq!(retried.status, "queued");
    assert_eq!(retried.dispatch_attempts, 1);
    assert_eq!(retried.error.as_deref(), Some("transient failure"));
    assert_eq!(retried.lease_token, None);
    assert_eq!(retried.response_status, Some(200));
    assert_eq!(retried.response_body.as_deref(), Some("{}"));

    // 租约丢了 ⇒ `None`（旧 worker 不报错、也不越权收口）。
    let lost = ingress_sql::retry_claimed(
        &pool,
        delivery_id,
        lease,
        chrono::Utc::now(),
        "stale worker",
    )
    .await
    .expect("retry");
    assert!(lost.is_none(), "租约已被释放，旧 token 不该再改行");

    cleanup(&pool, ws, &[owner]).await;
}

/// `claim_queued` 只认到期行：把 `available_at` 推到未来 ⇒ 队列为空。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn claim_skips_deliveries_that_are_not_due_yet() {
    let Some((pool, _db)) = connect().await else {
        println!("skip claim_skips_deliveries_that_are_not_due_yet: no env");
        return;
    };
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let delivery_id = insert_queued_delivery(
        &pool,
        ws,
        autopilot_id,
        trigger_id,
        r#"{"ref":"refs/heads/main"}"#,
        &format!("w9-{}", Uuid::new_v4()),
    )
    .await;
    sqlx::query(
        "UPDATE webhook_delivery SET available_at = now() + interval '1 hour' WHERE id = $1",
    )
    .bind(delivery_id)
    .execute(&pool)
    .await
    .unwrap();

    assert!(
        WebhookIngress::new(pool.clone())
            .process_next_delivery_in_workspace(ws)
            .await
            .expect("process next")
            .is_none(),
        "未到期的投递不该被认领"
    );

    cleanup(&pool, ws, &[owner]).await;
}
