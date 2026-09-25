//! M5-D8（`LUM-1745`）的用例：提示口的容量/非阻塞（**不需要库**，门 ⑤ 就跑）+ 轮询循环的四条
//! 行为（**真库**，全部 `#[ignore]`，门 ⑥ 会带上 `-p mc-server` 跑）。
//!
//! 跑法（真库部分；先 `MULTICA_DATABASE_URL=… cargo run -p mc-migrate -- run --dir migrations`）：
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://… \
//!   cargo test -p mc-server -- --ignored --nocapture webhook_worker
//! ```
//!
//! # 与 `scheduler/tests.rs` 同款的两条约束
//!
//! * `apps/mc-server` 是**纯二进制**（没有 lib target、没有 `[dev-dependencies]`）⇒
//!   `mc-http/tests/**/support.rs` 里那些 `pub(crate)` 夹具导不进来，本文件自带最小夹具
//!   （7 条裸 SQL INSERT，字段与 `mc-http/tests/autopilots/webhook_support.rs` 逐字同源）。
//! * 每个用例一个**新 workspace**，结束时删 workspace（级联清 `agent_runtime` / `agent` /
//!   `autopilot` / `autopilot_trigger` / `webhook_delivery`）再删 user。
//!
//! # 为什么本文件必须串行（[`db_lock`]）
//!
//! worker 池的认领是**整库**的（上游 `ClaimQueuedWebhookDelivery` 没有 workspace 过滤，worker
//! 是单例）—— 同一 binary 里若两个用例并发跑，A 的池会把 B 刚落的 `queued` 行抢走，于是 B 的
//! **负对照**（「不提示 ⇒ 就该一直在 `queued`」）会假红。`DB_LOCK` 把本文件的用例串起来；
//! `scheduler/tests.rs` 的用例不写 `webhook_delivery`，所以两条线可以并行。
//!
//! # 为什么终态断言选 `ignored`
//!
//! 夹具造一个**暂停**的 autopilot：[`WebhookIngress::process_next_delivery`] 的 ⑧ 段在「查不到
//! 该投递的 run」且 `autopilot.status != 'active'` 时收口 `ignored`（上游逐字 `autopilot_paused`），
//! 不需要 agent runtime 在线、不需要建 issue/task。`DoD` 1 明确接受 `ignored`。派发面（`dispatched`）
//! 的语义已有 `mc-http/tests/autopilots/webhook_worker.rs` 的 8 条用例覆盖，那**不是**本片的缺口。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::postgres::PgPool;
use uuid::Uuid;

use mc_autopilot::webhook::WebhookNotify;
use mc_db::Db;
use mc_http::routes::webhooks::autopilots::{
    reset_webhook_notify_port, set_webhook_notify_port, webhook_notify_port,
};
use mc_realtime::RealtimeHandle;

use super::{notify_slots, start, start_with, Options, NOTIFY_SLOT_CAPACITY, SHUTDOWN_TIMEOUT};

// ---------------------------------------------------------------------------
// 不需要库：提示口的容量与非阻塞（`DoD` 5）
// ---------------------------------------------------------------------------

/// 记录型端口（用例替身）：只数被提示了几次。
#[derive(Default)]
struct RecordingNotify {
    calls: AtomicUsize,
}

impl WebhookNotify for RecordingNotify {
    fn notify(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// **`DoD` 5**：容量满了**既不 panic 也不阻塞**（对照上游
/// `select { case w.notify <- struct{}{}: default: }` 的 `default:` 分支）。
///
/// 判据分两条：① 十万次提示在秒级以内返回（没有任何一次在等空位）；② 每条槽只留下
/// [`NOTIFY_SLOT_CAPACITY`] 枚 —— 多出来的**被丢掉**，而不是排队等着。
#[test]
fn notify_drops_when_the_slot_is_full_and_never_blocks() {
    let (port, receivers) = notify_slots(4);
    assert_eq!(receivers.len(), 4);

    let started = Instant::now();
    for _ in 0..100_000 {
        port.notify();
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "10 万次提示耗时 {elapsed:?} ⇒ 提示路径上有等待"
    );

    for mut receiver in receivers {
        for slot in 0..NOTIFY_SLOT_CAPACITY {
            assert!(receiver.try_recv().is_ok(), "第 {slot} 枚没被缓存");
        }
        assert!(
            receiver.try_recv().is_err(),
            "槽容量大于 {NOTIFY_SLOT_CAPACITY} ⇒ 提示会积压"
        );
    }
}

/// 一次提示**扇到全部 loop**（每条槽各一枚）—— 上游那条共享 chan 的一次 `Notify()` 只唤醒
/// 其中一个；本仓的形态扇得更开（延迟更短，正确性不变：提示本来就可以丢）。
#[test]
fn one_notify_wakes_every_loop_slot() {
    let (port, mut receivers) = notify_slots(4);
    port.notify();
    for (index, receiver) in receivers.iter_mut().enumerate() {
        assert!(receiver.try_recv().is_ok(), "第 {index} 条槽没收到提示");
    }
}

/// `mc-http` 那个进程级槽的三件：注入（生产装配点）→ 读到的是它 → 复位后回到 no-op 缺省。
///
/// 这是**不需要库**的用例，所以门 ⑤ 就会跑它（真库用例都是 `#[ignore]`）。
#[test]
fn the_notify_slot_round_trips_and_falls_back_to_the_noop_default() {
    reset_webhook_notify_port();
    let recording = Arc::new(RecordingNotify::default());
    set_webhook_notify_port(recording.clone());

    webhook_notify_port().notify();
    assert_eq!(recording.calls.load(Ordering::SeqCst), 1);

    // 复位后是 `DisabledNotify` 缺省：再调既不 panic，也不该记到 recording 上。
    reset_webhook_notify_port();
    webhook_notify_port().notify();
    assert_eq!(recording.calls.load(Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 真库（没有 URL 就显式失败，不静默跳过 —— 仓库统一口径）。
async fn pool() -> PgPool {
    PgPool::connect(&database_url())
        .await
        .expect("connect test db")
}

fn database_url() -> String {
    std::env::var("MULTICA_TEST_DATABASE_URL")
        .expect("set MULTICA_TEST_DATABASE_URL to enable DB tests")
}

/// 本文件各用例的串行锁（见模块头的「为什么本文件必须串行」）。
///
/// 用 `tokio::sync::Mutex` 而不是 `std::sync::Mutex`：守卫要**跨 `await`** 持有整个用例
/// （用例主体全是 await），`std` 的守卫会被 `clippy::await_holding_lock`（pedantic，本仓
/// `-D warnings`）判红。这里没有重入，tokio 的异步互斥量与 `std` 的语义完全一致。
static DB_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn db_lock() -> tokio::sync::MutexGuard<'static, ()> {
    DB_LOCK.lock().await
}

/// 一个用例的 workspace + user（`cleanup` 只需要这两个 id）。
struct World {
    ws: Uuid,
    user: Uuid,
    /// `autopilot.assignee_id` 是 `NOT NULL REFERENCES agent(id)` ⇒ 夹具必须造一条 agent
    /// （本片的收口路径走 `ignored`，用不到 agent 的任何字段，只用到它的**存在**）。
    autopilot: Uuid,
    trigger: Uuid,
}

/// `workspace` + `user` + `agent_runtime` + `agent` + `autopilot`（**暂停**）+ `trigger`（启用）。
///
/// `autopilot.status = 'paused'` 是刻意的：worker 的 ⑧ 段因此收口 `ignored`（`autopilot_paused`），
/// 本用例只验「队列被消费、行到终态」，不碰派发面（那是 M5-5 的既有用例）。
async fn seed_world(pool: &PgPool) -> World {
    let ws: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-m5d8-ws', $1) RETURNING id",
    )
    .bind(format!("itest-m5d8-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");

    let user: Uuid = sqlx::query_scalar(
        r#"INSERT INTO "user"(name, email) VALUES ('itest-m5d8-user', $1) RETURNING id"#,
    )
    .bind(format!("m5d8-{}@example.com", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert user");

    let runtime: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_runtime \
            (workspace_id, daemon_id, name, runtime_mode, provider, status, last_seen_at) \
         VALUES ($1, $2, $3, 'local', 'claude', 'online', now()) RETURNING id",
    )
    .bind(ws)
    .bind(format!("daemon-{}", Uuid::new_v4()))
    .bind(format!("rt-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert agent_runtime");

    let agent: Uuid = sqlx::query_scalar(
        "INSERT INTO agent \
            (workspace_id, name, runtime_mode, status, kind, runtime_id, owner_id, permission_mode) \
         VALUES ($1, $2, 'local', 'idle', 'user', $3, $4, 'private') RETURNING id",
    )
    .bind(ws)
    .bind(format!("itest-m5d8-agent-{}", Uuid::new_v4()))
    .bind(runtime)
    .bind(user)
    .fetch_one(pool)
    .await
    .expect("insert agent");

    let autopilot: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot \
            (workspace_id, title, description, assignee_id, status, execution_mode, \
             created_by_type, created_by_id) \
         VALUES ($1, $2, 'm5-d8 worker loop fixture', $3, 'paused', 'run_only', 'member', $4) \
         RETURNING id",
    )
    .bind(ws)
    .bind(format!("itest-m5d8-ap-{}", Uuid::new_v4()))
    .bind(agent)
    .bind(user)
    .fetch_one(pool)
    .await
    .expect("insert autopilot");

    let trigger: Uuid = sqlx::query_scalar(
        "INSERT INTO autopilot_trigger \
            (autopilot_id, kind, enabled, webhook_token, provider) \
         VALUES ($1, 'webhook', true, $2, 'github') RETURNING id",
    )
    .bind(autopilot)
    .bind(format!("awt_{}", Uuid::new_v4().simple()))
    .fetch_one(pool)
    .await
    .expect("insert autopilot_trigger");

    World {
        ws,
        user,
        autopilot,
        trigger,
    }
}

/// 直接插一条 `queued` 投递（绕过入站）：本片验的是**消费端**，落库那一半是 M5-5 的既有面。
///
/// `dedupe_key` 每行唯一：`idx_webhook_delivery_dedupe` 的谓词是
/// `status NOT IN ('rejected','failed')` ⇒ 本用例收口出来的 `ignored` 行**仍在**索引里，
/// 同键第二行会撞唯一约束。
async fn seed_delivery(pool: &PgPool, world: &World, ordinal: usize) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO webhook_delivery \
            (workspace_id, autopilot_id, trigger_id, provider, event, dedupe_key, dedupe_source, \
             signature_status, status, selected_headers, content_type, raw_body) \
         VALUES ($1, $2, $3, 'github', 'github.push', $4, 'x-github-delivery', 'not_required', \
             'queued', '{}'::jsonb, 'application/json', $5::bytea) RETURNING id",
    )
    .bind(world.ws)
    .bind(world.autopilot)
    .bind(world.trigger)
    .bind(format!("m5d8-dedupe-{ordinal}-{}", Uuid::new_v4()))
    .bind(format!(
        r#"{{"ref":"refs/heads/main","ordinal":{ordinal}}}"#
    ))
    .fetch_one(pool)
    .await
    .expect("insert queued delivery")
}

/// 投递行的收口面（`status` + `error` —— 末者就是收口原因字符串，如 `autopilot_paused`）。
async fn settled(pool: &PgPool, id: Uuid) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, error FROM webhook_delivery WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("load delivery")
}

/// 等到池**静下来**：`empty_polls` 在 `quiet` 时长内不再增长，且每个 loop 至少空转过一次。
///
/// 每起一个池，每个 loop 在进 `select!` 之前会**立刻**先认领一次（上游 `runLoop` 起身就先
/// `ProcessNext`），而 `tokio::time::interval` 的首拍也是立即就绪洤 ⇒ 起步阶段有两次紧挨着的
/// 认领。负对照必须在「这两次认领都过去」之后才落行，否则它会被起步那一次抢走，用例就假红了。
async fn await_pool_idle(
    worker: &super::WebhookWorkerHandles,
    quiet: Duration,
    deadline: Duration,
) {
    let started = Instant::now();
    let mut last = worker.empty_polls();
    loop {
        tokio::time::sleep(quiet).await;
        let now = worker.empty_polls();
        if now == last && now >= worker.concurrency() {
            return;
        }
        assert!(
            started.elapsed() < deadline,
            "池在 {deadline:?} 内没静下来（empty_polls 从 {last} 涨到 {now}）"
        );
        last = now;
    }
}

/// 轮询到「不再是 `queued`」为止，返回（终态, 从调用起的耗时）。
///
/// 超时即失败（带当时的 `status`），绝不静默返回 `queued` —— 那样断言就失去意义了。
async fn await_terminal(pool: &PgPool, id: Uuid, deadline: Duration) -> (String, Duration) {
    let started = Instant::now();
    loop {
        let status: String =
            sqlx::query_scalar("SELECT status FROM webhook_delivery WHERE id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .expect("load delivery status");
        if status != "queued" {
            return (status, started.elapsed());
        }
        assert!(
            started.elapsed() < deadline,
            "投递 {id} 在 {deadline:?} 内仍是 `queued` ⇒ 入站落下的那一行从未被消费"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 删 workspace（级联清 `agent_runtime` / `agent` / `autopilot` / `autopilot_trigger` /
/// `webhook_delivery`）再删 user。
async fn cleanup(pool: &PgPool, world: &World) {
    sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(world.ws)
        .execute(pool)
        .await
        .expect("delete workspace");
    sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
        .bind(world.user)
        .execute(pool)
        .await
        .expect("delete user");
}

// ---------------------------------------------------------------------------
// 真库：轮询循环的四条行为（`DoD` 1–4）
// ---------------------------------------------------------------------------

/// **`DoD` 1 + `DoD` 2**：队列里落一条 `queued` 行 ⇒ 无人干预、**不碰提示口**，`1s` ticker 把它推到终态。
///
/// 判据 = `SELECT status` 前后对比（`queued` → `ignored`）+ 实际耗时。落行**刻意放在池静下来
/// 之后**：池起步时每个 loop 会立刻先认领一次，若在那之前落行，量到的耗时是「起飞」而不是
/// 「ticker」，这条用例就不再是 `DoD` 2 的证据了（实测落在 **~1s** 量级）。这条用例同时是
/// 「内存通知只是延迟提示」那条上游语义的照抄证据：**没有**任何 `Notify` 参与。
#[tokio::test]
#[ignore = "需要真库（MULTICA_TEST_DATABASE_URL）；门禁 ⑥ 会带 -p mc-server 跑"]
async fn ticker_alone_drives_a_queued_delivery_to_terminal() {
    let _guard = db_lock().await;
    let pool = pool().await;
    let world = seed_world(&pool).await;

    let worker = start_with(pool.clone(), RealtimeHandle::start(16), Options::default());
    assert_eq!(worker.concurrency(), 4);
    assert_eq!(worker.poll_interval(), Duration::from_secs(1));
    // 等池静下来：`empty_polls` 在 300ms 内不再增长 ⇒ 4 个 loop 都跑完了起步那两轮认领、正在
    // 等各自的 ticker。落行放在这之后，量到的才是「tick → 被认领」这一段（实测 ~0.4s，
    // 即 1s 一拍 ticker 的相位余量），而不是「池刚起来顺手扫了一把」。
    await_pool_idle(&worker, Duration::from_millis(300), Duration::from_secs(5)).await;

    let delivery = seed_delivery(&pool, &world, 0).await;
    let before = settled(&pool, delivery).await;
    assert_eq!(before.0, "queued", "夹具就该落一条 queued 行");

    let (after, elapsed) = await_terminal(&pool, delivery, Duration::from_secs(2)).await;
    println!(
        "DoD 1/2 ticker-only: {delivery} status {} -> {} in {elapsed:?} \
         (empty_polls={}, processed={})",
        before.0,
        after,
        worker.empty_polls(),
        worker.processed()
    );
    assert_eq!(
        after, "ignored",
        "暂停的 autopilot ⇒ 上游收口文案是 autopilot_paused"
    );
    assert!(
        elapsed <= Duration::from_secs(2),
        "`DoD` 1 要求 ≤2s（1s 一拍 ticker），实测 {elapsed:?}"
    );
    assert_eq!(
        settled(&pool, delivery).await.1.as_deref(),
        Some("autopilot_paused")
    );

    worker.shutdown().await;
    reset_webhook_notify_port();
    cleanup(&pool, &world).await;
}

/// **`Notify` 面**：ticker 间隔拉到 60s ⇒ 「被消费」只可能来自提示。
///
/// 两条对照：
/// ① **负**：落一条、**不**提示 ⇒ 2s 内必须仍是 `queued`（否则说明 ticker 间隔没生效，正对照
///    就什么也证明不了）；
/// ② **正**：再落一条、走**生产装配路径读到的那个端口**（`start_with` 注进 `mc-http` 槽的就是
///    池自己的提示口）提示一声 ⇒ 5s 内到终态。
#[tokio::test]
#[ignore = "需要真库（MULTICA_TEST_DATABASE_URL）；门禁 ⑥ 会带 -p mc-server 跑"]
async fn notify_drives_a_queued_delivery_long_before_the_next_tick() {
    let _guard = db_lock().await;
    let pool = pool().await;
    let world = seed_world(&pool).await;
    let worker = start_with(
        pool.clone(),
        RealtimeHandle::start(16),
        Options {
            poll_interval: Duration::from_secs(60),
            concurrency: 4,
        },
    );

    // ① 负对照：先等池静下来（起步那两次紧挨着的认领跑完），再落一条、**不**提示 ⇒
    //    60s 的 ticker 等不到、也没人提示 ⇒ 必须原地不动。
    await_pool_idle(&worker, Duration::from_millis(300), Duration::from_secs(5)).await;
    let idle = seed_delivery(&pool, &world, 0).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        settled(&pool, idle).await.0,
        "queued",
        "不提示却动了 ⇒ ticker 间隔没生效（正对照就证明不了任何事）"
    );

    // ② 正对照：从 `mc-http` 的进程级槽里取出端口提示一声。
    let armed = seed_delivery(&pool, &world, 1).await;
    webhook_notify_port().notify();
    let (after, elapsed) = await_terminal(&pool, armed, Duration::from_secs(5)).await;
    println!(
        "notify-only (ticker=60s): {armed} status queued -> {after} in {elapsed:?}；\
         负对照 {idle} 2s 后仍是 queued"
    );
    assert_eq!(after, "ignored");
    assert!(
        elapsed < Duration::from_secs(5),
        "提示没把 worker 叫起来（{elapsed:?}）"
    );

    worker.shutdown().await;
    reset_webhook_notify_port();
    cleanup(&pool, &world).await;
}

/// **`DoD` 3**：一次落 8 条 ⇒ 同时在飞**不超过池大小**，且池确实在并行（峰值 ≥2）。
///
/// 观测点 = 池自己的 `in_flight` 峰值（每次认领进出各动一次原子计数）——比 `pg_stat_activity`
/// 采样稳，也不受库里别的用例干扰。
#[tokio::test]
#[ignore = "需要真库（MULTICA_TEST_DATABASE_URL）；门禁 ⑥ 会带 -p mc-server 跑"]
async fn in_flight_never_exceeds_the_pool_size() {
    let _guard = db_lock().await;
    let pool = pool().await;
    let world = seed_world(&pool).await;
    let mut deliveries = Vec::new();
    for ordinal in 0..8 {
        deliveries.push(seed_delivery(&pool, &world, ordinal).await);
    }

    let worker = start_with(pool.clone(), RealtimeHandle::start(16), Options::default());
    for delivery in &deliveries {
        await_terminal(&pool, *delivery, Duration::from_secs(10)).await;
    }
    let peak = worker.peak_in_flight();
    println!(
        "DoD 3: {} 条 queued 全部终态，同时在飞峰值 = {peak}（池 = {}）",
        deliveries.len(),
        worker.concurrency()
    );
    assert!(
        peak <= worker.concurrency(),
        "同时在飞 {peak} > 池大小 {}",
        worker.concurrency()
    );
    assert!(peak >= 2, "峰值 {peak} ⇒ 池没有真的并行");

    worker.shutdown().await;
    reset_webhook_notify_port();
    cleanup(&pool, &world).await;
}

/// **`DoD` 4**：`shutdown()` 在 [`SHUTDOWN_TIMEOUT`] 内返回，且 4 个 loop 真的停了。
///
/// 「真的停了」的判据是**行为**的：停机后再落一条 `queued`，2s（≥1 拍 ticker）后必须仍是
/// `queued` —— 池若还活着，它早就被收走了。
#[tokio::test]
#[ignore = "需要真库（MULTICA_TEST_DATABASE_URL）；门禁 ⑥ 会带 -p mc-server 跑"]
async fn shutdown_stops_the_loops_within_the_timeout() {
    let _guard = db_lock().await;
    let pool = pool().await;
    let world = seed_world(&pool).await;
    let worker = start_with(pool.clone(), RealtimeHandle::start(16), Options::default());

    // 先证明池在工作（否则「停机很快」是空话）。
    let first = seed_delivery(&pool, &world, 0).await;
    let _ = await_terminal(&pool, first, Duration::from_secs(2)).await;

    let started = Instant::now();
    worker.shutdown().await;
    let elapsed = started.elapsed();
    println!("DoD 4: shutdown 在 {elapsed:?} 内返回（上限 {SHUTDOWN_TIMEOUT:?}）");
    assert!(
        elapsed <= SHUTDOWN_TIMEOUT,
        "停机耗时 {elapsed:?} 超过 {SHUTDOWN_TIMEOUT:?}"
    );

    let after = seed_delivery(&pool, &world, 1).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        settled(&pool, after).await.0,
        "queued",
        "停机后池还在认领 ⇒ loop 没退出"
    );

    reset_webhook_notify_port();
    cleanup(&pool, &world).await;
}

/// **生产装配路径**：`start(&db, realtime)`（`main.rs` 调的就是它）自己起池、并把池的提示口写进
/// `mc-http` 的进程级槽 ⇒ 从槽里取出的端口叫得动它。
///
/// 与 `notify_drives_a_queued_delivery_long_before_the_next_tick` 的分工：那条验「提示 → 消费」
/// 的因果（60s ticker 排除 ticker 的功劳），本条验**装配**（`start` 一句话就把宿主、槽、池串起来）。
#[tokio::test]
#[ignore = "需要真库（MULTICA_TEST_DATABASE_URL）；门禁 ⑥ 会带 -p mc-server 跑"]
async fn start_wires_the_worker_and_the_mc_http_notify_slot() {
    let _guard = db_lock().await;
    let pool = pool().await;
    let db = Db::connect_lazy(&database_url(), 4, 0).expect("lazy db");
    reset_webhook_notify_port();

    let world = seed_world(&pool).await;
    let handles = start(&db, RealtimeHandle::start(16));
    assert_eq!(handles.concurrency(), 4);

    let delivery = seed_delivery(&pool, &world, 0).await;
    webhook_notify_port().notify();
    let (after, elapsed) = await_terminal(&pool, delivery, Duration::from_secs(2)).await;
    println!("start(): {delivery} status queued -> {after} in {elapsed:?}");
    assert_eq!(after, "ignored");

    handles.shutdown().await;
    reset_webhook_notify_port();
    db.close().await;
    cleanup(&pool, &world).await;
}
