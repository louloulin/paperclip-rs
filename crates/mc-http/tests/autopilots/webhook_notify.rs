//! 投递 worker 的**入站侧唤醒口** e2e（M5-D8 / `LUM-1745`，登记 `docs/32` §27）。
//!
//! # 这里测什么
//!
//! 上游把「叫醒投递 worker」放在 4 个调用点（`h.WebhookDeliveryWorker.Notify()`）：
//!
//! | # | 上游 | 本地 | 本文件 |
//! | --- | --- | --- | --- |
//! | 1 | `webhook_delivery.go:344` replay 新建了一行 | `routes/autopilots/delivery.rs` | 第三步 |
//! | 2 | `autopilot_webhook.go:499` 去重命中且行仍是 `queued` | `mc_autopilot::webhook::admission` | 第二步 |
//! | 3 | `autopilot_webhook.go:592` 同步准入失败（行留在队列） | 同上 | 需要人为造库错，本文件不覆盖 |
//! | 4 | `autopilot_webhook.go:627` 已接受 / 已跳过 | 同上 | 第一步 |
//!
//! 判据是**端口被叫了几次**（替身 = [`RecordingNotify`]，注入 `mc-http` 的进程级槽），断言一律
//! 用「**前后差值 > 0**」，绝不用绝对值 —— 槽是进程级的，同 binary 的用例并发跑。
//!
//! # 为什么三步合在一个用例里
//!
//! 三步都要往**同一个**进程级槽里放自己的替身。拆成三个用例并发跑，后一个的
//! `set_webhook_notify_port` 会把前一个的替身顶掉（前一个的「差值 > 0」会记到后一个的计数上，
//! 后一个自己反而看不到自己那一次）⇒ 一个用例按顺序走完三步，是这个共享点唯一的干净解法。
//!
//! 唤醒口与 worker 池之间那一段（提示 ⇒ 真的被消费）在 `apps/mc-server` 的真库用例里
//! （`webhook_worker/tests.rs` 的 `notify_drives_a_queued_delivery_long_before_the_next_tick`）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

use mc_autopilot::webhook::WebhookNotify;
use mc_http::routes::webhooks::autopilots::{reset_webhook_notify_port, set_webhook_notify_port};

use super::deliveries::{seed_delivery, DeliverySpec};
use super::support::{app_with_db, call, cleanup, connect, seed_workspace};
use super::webhook_support::{post, seed_agent, seed_trigger, unique_ip, unique_token};

/// 只数被叫了几次的替身端口。
#[derive(Default)]
struct RecordingNotify {
    calls: AtomicUsize,
}

impl RecordingNotify {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl WebhookNotify for RecordingNotify {
    fn notify(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

/// 三步：① 入站已接受（上游 `:627`）→ ② 入站去重命中（上游 `:499`）→ ③ replay 新行（上游 `:344`）。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn the_inbound_and_replay_faces_tell_the_worker_to_wake_up() {
    let Some((pool, db)) = connect().await else {
        println!("skip the_inbound_and_replay_faces_tell_the_worker_to_wake_up: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    // `active` + `run_only`：入站走同步准入那一段（⑫），无论准入结果是被接受还是被跳过，都要经
    // `acknowledge` ⇒ 上游 `:627` 那一处必然被叫到。
    // ⚠️ 用 `webhook_support` 的那一份（多两个参数：`execution_mode` / 真实 `assignee_id`）；
    // `support.rs` 里的同名函数是 M5-1 写集的五参形态（签名不同，不通用）。
    let autopilot =
        super::webhook_support::seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger = seed_trigger(&pool, autopilot, &token, "generic", true, None, None).await;
    let ip = unique_ip();
    let body = br#"{"action":"opened","payload":{}}"#;
    let headers = [("idempotency-key", "notify-wake-1")];

    let recording = Arc::new(RecordingNotify::default());
    set_webhook_notify_port(recording.clone());

    // ① 已接受 / 已跳过：`AcknowledgeWebhookDelivery` 只写响应字段、**不动** `status`
    //    ⇒ 这一条仍是 `queued`，正等着 worker 认领 ⇒ 必须叫一声。
    let before = recording.calls();
    let first = post(&app, &token, Some(&ip), &headers, body).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.raw);
    assert!(
        recording.calls() > before,
        "已接受/已跳过的入站没有叫投递 worker（{}）",
        first.raw
    );

    // ② 同一次投递再来一遍 = 去重命中（`attempt_count` 自增、行仍是 `queued`）⇒ 上游 `:499` 同款。
    let before = recording.calls();
    let second = post(&app, &token, Some(&ip), &headers, body).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.raw);
    assert_eq!(second.status_field(), "duplicate", "{}", second.raw);
    assert!(
        recording.calls() > before,
        "去重命中（行仍是 queued）没有叫投递 worker（{}）",
        second.raw
    );

    // 去重命中**不新建行**：这个 trigger 上只有那一条投递。
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webhook_delivery WHERE trigger_id = $1")
            .bind(trigger)
            .fetch_one(&pool)
            .await
            .expect("count deliveries");
    assert_eq!(rows, 1, "去重命中不该新建第二行");

    // ③ replay 新建一行（上游 `webhook_delivery.go:344`）：上游在 `Notify()` 之前的两条出口
    //    （幂等命中 / 读回已存在的行）都提前 `return` 了 ⇒ 这里走「真的插进去了一行」那条。
    let original = seed_delivery(
        &pool,
        ws,
        autopilot,
        trigger,
        &DeliverySpec {
            status: "failed",
            signature_status: "valid",
            raw_body: Some(br#"{"action":"opened","payload":{"n":1}}"#),
            selected_headers: json!({"content-type": "application/json"}),
            ..DeliverySpec::new()
        },
        "1 hour",
    )
    .await;
    let uri = format!("/api/autopilots/{autopilot}/deliveries/{original}/replay");
    let before = recording.calls();
    let (status, body) = call(&app, "POST", &uri, ws, owner, None).await;
    assert_eq!(status, 202, "{body}");
    assert_eq!(body["status"], "queued");
    assert!(
        recording.calls() > before,
        "replay 新建了 queued 行却没有叫投递 worker（{body}）"
    );

    reset_webhook_notify_port();
    cleanup(&pool, ws, &[owner]).await;
}
