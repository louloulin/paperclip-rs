//! 无认证入站面 e2e（M5-5 / LUM-1570）：`POST /api/webhooks/autopilots/:token`。
//!
//! # 这里测什么
//!
//! 上游 `HandleAutopilotWebhook` 的 12 步里**入站那一段**（A 段）：限流两闸、token→trigger、
//! body 上限、归一化、签名、去重、trigger/autopilot 状态、事件作用域、同步准入。B 段
//! （worker 认领→派发→收口）在 `webhook_worker.rs`。
//!
//! **本波唯一的无认证入口** ⇒ 断言里有一类别处没有的判据：**响应形态不许泄漏存在性**
//! （未知 token / 轮换掉的旧 token / 空 token 都是同一句 `{"error":"webhook not found"}`，
//! 见 [`unknown_token_leaks_nothing_and_rotated_tokens_die_immediately`]），以及**凭据不回显**
//! （签名密钥与 token 既不进响应也不进投递行，见 [`secrets_and_tokens_are_never_echoed_back`]）。
//!
//! # 为什么全在这个 target 里
//!
//! 与 `dispatch.rs` 同因：`AppState` 要 `mc_db::Db`（真库），所以本文件的用例一律
//! `#[ignore]` + `MULTICA_TEST_DATABASE_URL`。**真正「无 DB」的那几层**在 crate 自己的单测里
//! （`mc-autopilot/src/webhook/{signature,ratelimit,provider}.rs` 的 `#[cfg(test)]`：7 + 16 例，
//! 覆盖纯函数与限流窗口）。
//!
//! # 夹具
//!
//! `support.rs` 是 M5-1 的写集（不改）⇒ 本片自带种子；观测面与夹具在
//! `webhook_support.rs`（R7 800 行上限，门 ⑩），本文件只放用例。

use axum::http::StatusCode;
use mc_core::hash::hmac_sha256;
use serde_json::json;
use uuid::Uuid;

use super::support::{app_with_db, connect, seed_workspace};
use super::webhook_support::*;

// ---------------------------------------------------------------------------
// ① 无效 token：同形响应 + 不泄漏存在性
// ---------------------------------------------------------------------------

/// 未知 token / 轮换掉的旧 token / 空 token 形态**逐字同形**，且**一行都不落**。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn unknown_token_leaks_nothing_and_rotated_tokens_die_immediately() {
    let Some((pool, db)) = connect().await else {
        println!("skip unknown_token_leaks_nothing_and_rotated_tokens_die_immediately: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let live_token = unique_token();
    let trigger_id =
        seed_trigger(&pool, autopilot_id, &live_token, "github", true, None, None).await;
    let ip = unique_ip();
    let marker = format!("leak-{}", Uuid::new_v4());
    let headers = [
        ("x-github-event", "issues"),
        ("x-github-delivery", marker.as_str()),
    ];

    // (a) 从来不存在过的 token。
    let unknown = post(
        &app,
        &unique_token(),
        Some(&ip),
        &headers,
        br#"{"action":"opened"}"#,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.raw);
    assert_eq!(unknown.raw, "{\"error\":\"webhook not found\"}\n");
    assert_eq!(unknown.content_type(), Some("application/json"));
    assert!(
        !unknown.raw.contains(&live_token),
        "404 体里不该出现任何 token：{}",
        unknown.raw
    );

    // (b) 轮换：旧 token 立即失效（M5-3 的 rotate 行为在这里只消费形态）。
    let rotated_token = unique_token();
    sqlx::query("UPDATE autopilot_trigger SET webhook_token = $2 WHERE id = $1")
        .bind(trigger_id)
        .bind(&rotated_token)
        .execute(&pool)
        .await
        .expect("rotate webhook token");
    let rotated = post(
        &app,
        &live_token,
        Some(&ip),
        &headers,
        br#"{"action":"opened"}"#,
    )
    .await;
    assert_eq!(rotated.status, StatusCode::NOT_FOUND);
    assert_eq!(
        rotated.raw, unknown.raw,
        "轮换后的旧 token 与「未知 token」必须逐字同形"
    );

    // 两次 404 都没落投递行（去重键是同一枚，count 仍是 0）。
    assert_eq!(count_deliveries_by_dedupe(&pool, &marker).await, 0);
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    // 新 token 照常工作：证明上面两条 404 的原因确实是「token 不对」，不是路由坏了。
    let ok = post(
        &app,
        &rotated_token,
        Some(&ip),
        &headers,
        br#"{"action":"opened"}"#,
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.raw);
    assert_eq!(ok.status_field(), "accepted", "{}", ok.raw);
    assert_eq!(count_deliveries_by_dedupe(&pool, &marker).await, 1);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ② body 上限 / 归一化错误
// ---------------------------------------------------------------------------

/// 超上限 = 读流阶段就 413（`DefaultBodyLimit` + `BytesRejection`），**一行都不落**。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn body_over_the_cap_is_rejected_with_413_before_any_persistence() {
    let Some((pool, db)) = connect().await else {
        println!("skip body_over_the_cap_is_rejected_with_413_before_any_persistence: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let marker = format!("cap-{}", Uuid::new_v4());
    let headers = [("x-github-delivery", marker.as_str())];
    let ip = unique_ip();

    // 上限 = 256 KiB（`MAX_WEBHOOK_BODY_BYTES`）；多一个字节就该被拒。
    let over = vec![b'{'; 256 * 1024 + 1];
    let res = post(&app, &token, Some(&ip), &headers, &over).await;
    assert_eq!(res.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", res.raw);
    assert_eq!(res.raw, "{\"error\":\"payload too large\"}\n");
    assert_eq!(res.content_type(), Some("application/json"));
    assert_eq!(count_deliveries_by_dedupe(&pool, &marker).await, 0);
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 归一化失败（空体 / 非 JSON / 标量）→ 400 + 上游文案；**归一化在落库之前** ⇒ 无投递行。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn unnormalizable_bodies_are_rejected_with_400_and_persist_nothing() {
    let Some((pool, db)) = connect().await else {
        println!("skip unnormalizable_bodies_are_rejected_with_400_and_persist_nothing: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let ip = unique_ip();

    let empty = post(&app, &token, Some(&ip), &[], b"").await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert_eq!(empty.raw, "{\"error\":\"empty body\"}\n");

    let broken = post(&app, &token, Some(&ip), &[], b"not json").await;
    assert_eq!(broken.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        broken.error(),
        "invalid json: expected ident at line 1 column 2"
    );
    assert!(broken.raw.ends_with('\n'), "{}", broken.raw);

    let scalar = post(&app, &token, Some(&ip), &[], b"42").await;
    assert_eq!(scalar.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        scalar.raw,
        "{\"error\":\"body must be a JSON object or array\"}\n"
    );

    // 归一化失败发生在 INSERT 之前（上游同序）⇒ 三条都没有投递行。
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webhook_delivery WHERE trigger_id = $1")
            .bind(trigger_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ③ 签名
// ---------------------------------------------------------------------------

/// 配了密钥：缺签名 401 / 错签名 401，两条都在**落库之后**收口成 `rejected`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn missing_and_invalid_signatures_are_rejected_with_401() {
    let Some((pool, db)) = connect().await else {
        println!("skip missing_and_invalid_signatures_are_rejected_with_401: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let secret = format!("secret-{}", Uuid::new_v4());
    seed_trigger(
        &pool,
        autopilot_id,
        &token,
        "github",
        true,
        Some(&secret),
        None,
    )
    .await;
    let ip = unique_ip();
    let body = br#"{"action":"opened"}"#;

    // (a) 没带签名头。
    let missing = post(
        &app,
        &token,
        Some(&ip),
        &[("x-github-event", "issues")],
        body,
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED, "{}", missing.raw);
    assert_eq!(missing.status_field(), "rejected");
    assert_eq!(missing.body["reason"], "missing_signature");
    let row = delivery(&pool, missing.delivery_id()).await;
    assert_eq!(row.0, "rejected"); // status
    assert_eq!(row.1.as_deref(), Some("missing_signature")); // error
    assert_eq!(row.2, None); // reason_code
    assert_eq!(row.3, Some(401)); // response_status
    assert_eq!(row.6, 0); // dispatch_attempts
    assert_eq!(row.7, "missing"); // signature_status
    assert_eq!(row.8, None); // autopilot_run_id：被拒的投递不产生 run
                             // 签名头只记 present 标记，不记值。
    assert_eq!(row.9.get("x-hub-signature-256-present"), None);

    // (b) 带了签名头但 HMAC 不匹配。
    let invalid = post(
        &app,
        &token,
        Some(&ip),
        &[
            ("x-github-event", "issues"),
            ("x-hub-signature-256", "sha256=deadbeef"),
        ],
        body,
    )
    .await;
    assert_eq!(invalid.status, StatusCode::UNAUTHORIZED, "{}", invalid.raw);
    assert_eq!(invalid.body["reason"], "invalid_signature");
    let row = delivery(&pool, invalid.delivery_id()).await;
    assert_eq!(row.0, "rejected");
    assert_eq!(row.7, "invalid");
    assert_eq!(row.9["x-hub-signature-256-present"], json!(true));
    assert_eq!(row.8, None);

    // 两条都只记 present 标记，delivery dump 里**没有任何 HMAC 值**。
    assert!(!row.9.to_string().contains("sha256="), "{}", row.9);
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 正确签名（含**大写十六进制**，上游 `hex.DecodeString` 两种都收）⇒ 200 + `valid`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn a_valid_signature_is_accepted_in_both_hex_cases() {
    let Some((pool, db)) = connect().await else {
        println!("skip a_valid_signature_is_accepted_in_both_hex_cases: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let secret = format!("secret-{}", Uuid::new_v4());
    seed_trigger(
        &pool,
        autopilot_id,
        &token,
        "github",
        true,
        Some(&secret),
        None,
    )
    .await;
    let ip = unique_ip();

    for (index, hex_case) in ["lower", "upper"].into_iter().enumerate() {
        let body = format!(r#"{{"action":"opened","n":{index}}}"#);
        let digest = hmac_sha256(secret.as_bytes(), body.as_bytes());
        let digest = if hex_case == "upper" {
            digest.to_uppercase()
        } else {
            digest
        };
        let signature = format!("sha256={digest}");
        let marker = format!("sig-{}", Uuid::new_v4());
        let res = post(
            &app,
            &token,
            Some(&ip),
            &[
                ("x-github-event", "issues"),
                ("x-github-delivery", marker.as_str()),
                ("x-hub-signature-256", signature.as_str()),
            ],
            body.as_bytes(),
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{hex_case}: {}", res.raw);
        assert_eq!(res.status_field(), "accepted", "{hex_case}: {}", res.raw);
        let row = delivery(&pool, res.delivery_id()).await;
        assert_eq!(row.7, "valid", "{hex_case}");
        assert_eq!(row.0, "queued"); // 入站只准入，终态归 worker
        assert_eq!(row.3, Some(200));
        // 落库的 `response_body` 与响应体同内容，但**不含**结尾 `\n`（上游只在 HTTP 层补）。
        let stored = row.4.expect("response_body");
        assert!(!stored.ends_with('\n'), "{stored}");
        assert!(
            res.raw.starts_with(&stored),
            "raw={} stored={stored}",
            res.raw
        );
        assert!(row.8.is_none(), "入站只 Acknowledge，run 链归 worker 结算");
        // 响应体带回同步建出的 run（上游 v0.4.0 契约），且那一行真的在库里。
        let run_id: Uuid = res.body["run_id"]
            .as_str()
            .expect("accepted 必须带回 run_id")
            .parse()
            .expect("run_id 是 uuid");
        let runs: i64 = sqlx::query_scalar("SELECT count(*) FROM autopilot_run WHERE id = $1")
            .bind(run_id)
            .fetch_one(&pool)
            .await
            .expect("count run by id");
        assert_eq!(runs, 1, "{hex_case}：响应里的 run_id 必须真实存在");
        assert_eq!(row.9["x-hub-signature-256-present"], json!(true));
    }

    // 两条各一个 run（事件不同 ⇒ 不触发重复守卫）。
    assert_eq!(count_runs(&pool, autopilot_id).await, 2);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ④ 幂等（`dedupe_key`）
// ---------------------------------------------------------------------------

/// 同 `dedupe_key` 的第二条请求：同形 200 + **同一枚** delivery + `attempt_count` 自增。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn duplicate_dedupe_key_is_idempotent_and_only_bumps_the_attempt_counter() {
    let Some((pool, db)) = connect().await else {
        println!(
            "skip duplicate_dedupe_key_is_idempotent_and_only_bumps_the_attempt_counter: no env"
        );
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let trigger_id = seed_trigger(&pool, autopilot_id, &token, "github", true, None, None).await;
    let ip = unique_ip();
    let marker = format!("dup-{}", Uuid::new_v4());
    let headers = [
        ("x-github-event", "issues"),
        ("x-github-delivery", marker.as_str()),
    ];
    let body = br#"{"action":"opened"}"#;

    let first = post(&app, &token, Some(&ip), &headers, body).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.raw);
    assert_eq!(first.status_field(), "accepted");
    let second = post(&app, &token, Some(&ip), &headers, body).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.raw);
    assert_eq!(second.status_field(), "duplicate", "{}", second.raw);
    assert_eq!(second.delivery_id(), first.delivery_id());
    assert_eq!(
        second.body["run_id"], first.body["run_id"],
        "重复请求要拿到同一个 run"
    );

    // 一条 delivery、一条 run；`attempt_count`（去重命中计数）自增到 2，
    // `dispatch_attempts`（worker 派发计数）**不动**。
    assert_eq!(count_deliveries_by_dedupe(&pool, &marker).await, 1);
    assert_eq!(count_runs(&pool, autopilot_id).await, 1);
    let row = delivery(&pool, first.delivery_id()).await;
    assert_eq!(row.5, 2, "attempt_count");
    assert_eq!(row.6, 0, "dispatch_attempts");
    let idempotency_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_delivery WHERE trigger_id = $1 AND status = 'queued'",
    )
    .bind(trigger_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(idempotency_rows, 1);

    // generic provider 走 `Idempotency-Key`，同样幂等（去重来源不同的那条腿）。
    let generic_token = unique_token();
    let generic_autopilot = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    seed_trigger(
        &pool,
        generic_autopilot,
        &generic_token,
        "generic",
        true,
        None,
        None,
    )
    .await;
    let key = format!("idem-{}", Uuid::new_v4());
    let generic_headers = [("idempotency-key", key.as_str()), ("x-event-type", "ping")];
    let a = post(
        &app,
        &generic_token,
        Some(&ip),
        &generic_headers,
        b"{\"a\":1}",
    )
    .await;
    let b = post(
        &app,
        &generic_token,
        Some(&ip),
        &generic_headers,
        b"{\"a\":1}",
    )
    .await;
    assert_eq!(a.status_field(), "accepted", "{}", a.raw);
    assert_eq!(b.status_field(), "duplicate", "{}", b.raw);
    assert_eq!(count_deliveries_by_dedupe(&pool, &key).await, 1);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ⑤ 事件作用域 / 状态
// ---------------------------------------------------------------------------

/// `event_filters` 之外的事件 ⇒ 200 `ignored` + `reason=event_filtered`，**不产生 run**。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn event_scope_filter_ignores_the_delivery_without_creating_a_run() {
    let Some((pool, db)) = connect().await else {
        println!("skip event_scope_filter_ignores_the_delivery_without_creating_a_run: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    seed_trigger(
        &pool,
        autopilot_id,
        &token,
        "github",
        true,
        None,
        Some(r#"[{"event":"push"}]"#),
    )
    .await;
    let ip = unique_ip();

    // (a) `issues` 不在作用域内（过滤器只认 `push`）。
    let filtered = post(
        &app,
        &token,
        Some(&ip),
        &[
            ("x-github-event", "issues"),
            ("x-github-delivery", &format!("scope-{}", Uuid::new_v4())),
        ],
        br#"{"action":"opened"}"#,
    )
    .await;
    assert_eq!(filtered.status, StatusCode::OK, "{}", filtered.raw);
    assert_eq!(filtered.status_field(), "ignored");
    assert_eq!(filtered.body["reason"], "event_filtered");
    assert_eq!(filtered.body["event"], "github.issues.opened");
    let row = delivery(&pool, filtered.delivery_id()).await;
    assert_eq!(row.0, "ignored");
    assert_eq!(row.1.as_deref(), Some("event_filtered"));
    // `reason_code` 只有**配额**那条会写（上游 `finaliseDeliveryTerminal` 的 variadic 参数）；
    // 其余 ignored 都把原因写进 `error` —— 所以这里是 `None` 而不是 `event_filtered`。
    assert_eq!(row.2, None, "reason_code 仅配额路径写");
    assert_eq!(row.8, None);
    assert_eq!(count_runs(&pool, autopilot_id).await, 0);

    // (b) 作用域内的事件照常准入。
    let allowed = post(
        &app,
        &token,
        Some(&ip),
        &[
            ("x-github-event", "push"),
            ("x-github-delivery", &format!("scope-{}", Uuid::new_v4())),
        ],
        br#"{"ref":"refs/heads/main"}"#,
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK, "{}", allowed.raw);
    assert_eq!(allowed.status_field(), "accepted", "{}", allowed.raw);
    assert_eq!(count_runs(&pool, autopilot_id).await, 1);

    // (c) 解不开的 `event_filters`（合法 jsonb、但不是数组）⇒ **fail-closed**：一样 `ignored`。
    let broken_autopilot = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let broken_token = unique_token();
    seed_trigger(
        &pool,
        broken_autopilot,
        &broken_token,
        "github",
        true,
        None,
        Some("\"oops\""),
    )
    .await;
    let broken = post(
        &app,
        &broken_token,
        Some(&ip),
        &[("x-github-event", "push")],
        br#"{"ref":"refs/heads/main"}"#,
    )
    .await;
    assert_eq!(broken.status_field(), "ignored", "{}", broken.raw);
    assert_eq!(broken.body["reason"], "event_filtered");
    assert_eq!(count_runs(&pool, broken_autopilot).await, 0);

    cleanup_all(&pool, ws, &[owner]).await;
}

/// 停用 trigger / 暂停 autopilot / 归档 autopilot：三种 200 `ignored`，都不产生 run。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn disabled_trigger_and_inactive_autopilot_are_ignored_with_200() {
    let Some((pool, db)) = connect().await else {
        println!("skip disabled_trigger_and_inactive_autopilot_are_ignored_with_200: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let ip = unique_ip();
    let body = br#"{"action":"opened"}"#;

    for (status, enabled, expected_reason) in [
        ("active", false, "trigger_disabled"),
        ("paused", true, "autopilot_paused"),
        ("archived", true, "autopilot_archived"),
    ] {
        let autopilot_id = seed_autopilot(&pool, ws, status, "run_only", agent, owner).await;
        let token = unique_token();
        seed_trigger(&pool, autopilot_id, &token, "github", enabled, None, None).await;
        let res = post(
            &app,
            &token,
            Some(&ip),
            &[("x-github-event", "issues")],
            body,
        )
        .await;
        assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
        assert_eq!(res.status_field(), "ignored", "{}", res.raw);
        assert_eq!(res.body["reason"], expected_reason, "{}", res.raw);
        let row = delivery(&pool, res.delivery_id()).await;
        assert_eq!(row.0, "ignored", "{expected_reason}");
        assert_eq!(row.1.as_deref(), Some(expected_reason));
        assert_eq!(row.8, None, "{expected_reason}");
        assert_eq!(
            count_runs(&pool, autopilot_id).await,
            0,
            "{expected_reason}"
        );
    }

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ⑥ 限流
// ---------------------------------------------------------------------------

/// `DoD` ②：坏凭据债攒满 30 笔 ⇒ 第 31 次请求 429（`writeWebhookRateLimit` 的形态 + `Retry-After`）。
///
/// 走的是「未知 token」这条腿：每次 404 都 `charge_bad_credential` 一笔（上游同款），
/// 而 `gate_before_lookup` 的 `check` 是**非消费**的 ⇒ 前 30 次放行、第 31 次拦下。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn spending_the_bad_credential_budget_turns_into_a_429_with_retry_after() {
    let Some((pool, db)) = connect().await else {
        println!(
            "skip spending_the_bad_credential_budget_turns_into_a_429_with_retry_after: no env"
        );
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let ip = unique_ip();

    for index in 0..30 {
        let res = post(&app, &unique_token(), Some(&ip), &[], br#"{"event":"x"}"#).await;
        assert_eq!(
            res.status,
            StatusCode::NOT_FOUND,
            "第 {} 次请求应仍是 404：{}",
            index + 1,
            res.raw
        );
    }

    let limited = post(&app, &unique_token(), Some(&ip), &[], br#"{"event":"x"}"#).await;
    assert_eq!(
        limited.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        limited.raw
    );
    assert_eq!(limited.raw, "{\"error\":\"rate limit exceeded\"}\n");
    assert_eq!(limited.content_type(), Some("application/json"));
    assert!(
        limited.retry_after().is_some_and(|secs| secs >= 1),
        "Retry-After 必须存在且 ≥ 1：{:?}",
        limited.headers.get("retry-after")
    );

    // 换一个 IP：闸是 per-IP 的，立刻恢复。
    let other = post(
        &app,
        &unique_token(),
        Some(&unique_ip()),
        &[],
        br#"{"event":"x"}"#,
    )
    .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND, "{}", other.raw);

    cleanup_all(&pool, ws, &[owner]).await;
}

// ---------------------------------------------------------------------------
// ⑦ 凭据面（DoD：日志/响应里不含完整 signing secret 与 token）
// ---------------------------------------------------------------------------

/// 响应与投递行里**都不出现** signing secret；token 也不回显。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn secrets_and_tokens_are_never_echoed_back() {
    let Some((pool, db)) = connect().await else {
        println!("skip secrets_and_tokens_are_never_echoed_back: no env");
        return;
    };
    let app = app_with_db(db);
    let (ws, owner) = seed_workspace(&pool, "owner").await;
    let agent = seed_agent(&pool, ws, owner).await;
    let autopilot_id = seed_autopilot(&pool, ws, "active", "run_only", agent, owner).await;
    let token = unique_token();
    let secret = format!("top-secret-{}", Uuid::new_v4());
    seed_trigger(
        &pool,
        autopilot_id,
        &token,
        "github",
        true,
        Some(&secret),
        None,
    )
    .await;

    let body = br#"{"action":"opened","note":"do-not-log"}"#;
    let signature = format!("sha256={}", hmac_sha256(secret.as_bytes(), body.as_slice()));
    let res = post(
        &app,
        &token,
        Some(&unique_ip()),
        &[
            ("x-github-event", "issues"),
            ("x-hub-signature-256", signature.as_str()),
        ],
        body,
    )
    .await;
    assert_eq!(res.status, StatusCode::OK, "{}", res.raw);
    assert!(
        !res.raw.contains(&secret),
        "响应体泄漏了签名密钥：{}",
        res.raw
    );
    assert!(!res.raw.contains(&token), "响应体回显了 token：{}", res.raw);

    // 投递行的每一个文本列：密钥与 token 都不在里面（签名只留 present 标记）。
    let row = delivery(&pool, res.delivery_id()).await;
    for (label, text) in [
        ("error", row.1.clone()),
        ("reason_code", row.2.clone()),
        ("response_body", row.4.clone()),
        ("selected_headers", Some(row.9.to_string())),
        ("signature_status", Some(row.7.clone())),
    ] {
        let text = text.unwrap_or_default();
        assert!(
            !text.contains(&secret),
            "投递行 {label} 泄漏了签名密钥：{text}"
        );
        assert!(
            !text.contains(&token),
            "投递行 {label} 回显了 token：{text}"
        );
    }
    assert_eq!(row.9["x-hub-signature-256-present"], json!(true));

    cleanup_all(&pool, ws, &[owner]).await;
}
