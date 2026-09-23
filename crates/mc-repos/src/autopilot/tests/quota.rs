//! 配额仓储的真库语义（M5-1 读面 + 预留生命周期）。
//!
//! 覆盖的是「读面依赖的那一半」：
//! - `get_period` 的「没有行 = 0」（`usage` 路由的正常路径，不是错误）；
//! - `ensure_period` 的空更新（`ON CONFLICT ... SET updated_at = updated_at`）不会把计数清零；
//! - 幂等键唯一索引 `WHERE state <> 'released'`：`released` **会**释放幂等键；
//! - `consume` 的 `reserved → used` 单调性与终态重放（`Ok(None)`）；
//! - 已消费的预留**不能**再 `release`（额度已计入周期用量）；
//! - `increment_blocked` 写进 `blocked_counts` 的 key 就是 `usage` 响应里客户端看到的 key。
//!
//! 写面编排（`admit` 的事务、`autopilot_run` 回填）属 M5-4，这里不铺 run 表。

use chrono::{Duration, Utc};
use serde_json::json;

use super::{setup, teardown};
use crate::autopilot::quota::{
    consume, ensure_period, get_period, get_reservation_by_key, increment_blocked,
    increment_reserved, list_recoverable, release, reserve, STATE_CONSUMED, STATE_RELEASED,
    STATE_RESERVED,
};
use crate::RepoError;

/// 读面：没有周期行 → `None`；`ensure_period` 之后可读，且重复 `ensure` 不清零计数。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn period_is_absent_until_ensured_and_ensure_keeps_counts() {
    let Some(fixture) = setup().await else {
        println!("skip period_is_absent_until_ensured_and_ensure_keeps_counts: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let start = Utc::now() - Duration::hours(1);
    let end = start + Duration::days(30);

    assert!(
        get_period(pool, ws, start, end).await.unwrap().is_none(),
        "本周期还没用过额度 ⇒ None（`usage` 把它折成 0/0，不是 500）"
    );

    let period = ensure_period(pool, ws, start, end).await.unwrap();
    assert_eq!(period.used_count, 0);
    assert_eq!(period.reserved_count, 0);
    assert_eq!(period.total(), 0);
    assert!(period.blocked_counts.is_null() || period.blocked_counts == json!({}));

    // 记账：`reserved_count + 1` 与 `blocked_counts` 的 key。
    increment_reserved(pool, ws, start, end).await.unwrap();
    let blocked = increment_blocked(pool, ws, start, end, "limit_exceeded")
        .await
        .unwrap();
    assert_eq!(blocked.reserved_count, 1);
    assert_eq!(blocked.used_count, 0);
    assert_eq!(blocked.blocked_counts["limit_exceeded"], json!(1));
    let blocked_again = increment_blocked(pool, ws, start, end, "limit_exceeded")
        .await
        .unwrap();
    assert_eq!(blocked_again.blocked_counts["limit_exceeded"], json!(2));

    // `ensure_period` 的空更新只是拿行锁，**不能**把计数抹掉。
    let re_ensured = ensure_period(pool, ws, start, end).await.unwrap();
    assert_eq!(re_ensured.reserved_count, 1);
    assert_eq!(re_ensured.blocked_counts["limit_exceeded"], json!(2));

    // 周期边界是主键的一部分：换个周期就是另一行。
    let other = ensure_period(
        pool,
        ws,
        start + Duration::days(30),
        end + Duration::days(30),
    )
    .await
    .unwrap();
    assert_eq!(other.used_count, 0);

    teardown(&fixture).await;
}

/// 预留生命周期：幂等键冲突 → `Conflict`；`consume` 把 `reserved` 转成 `used` 且**只生效一次**；
/// 已消费的行不能再 `release`。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn reservation_consume_is_monotonic_and_not_releasable() {
    let Some(fixture) = setup().await else {
        println!("skip reservation_consume_is_monotonic_and_not_releasable: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let start = Utc::now() - Duration::hours(1);
    let end = start + Duration::days(30);
    ensure_period(pool, ws, start, end).await.unwrap();

    // 陈旧扫掠是**全局**的（上游 `ListRecoverableAutopilotQuotaReservations` 不带 workspace 参数，
    // 由后台 sweeper 跑）⇒ 断言必须按工作区收窄，不能断言「全局为空」。
    let sweep = list_recoverable(pool, Utc::now(), Utc::now(), 10)
        .await
        .unwrap();
    assert!(sweep.iter().all(|row| row.workspace_id != ws));

    let key = "idem-lum1564-1";
    let reservation = reserve(pool, ws, start, end, 7, 11, "manual", key)
        .await
        .unwrap();
    assert_eq!(reservation.state, STATE_RESERVED);
    assert!(reservation.is_reserved());
    assert!(reservation.finalized_at.is_none());
    assert_eq!(reservation.policy_revision, 7);
    assert_eq!(reservation.subscription_version, 11);

    // 幂等键唯一（部分索引 `WHERE state <> 'released'`）⇒ 第二次插入是 23505 → Conflict。
    let err = reserve(pool, ws, start, end, 7, 11, "manual", key)
        .await
        .expect_err("同一幂等键不能建两条预留");
    assert!(matches!(err, RepoError::Conflict), "got {err:?}");
    // 幂等命中查询拿得到同一条。
    let found = get_reservation_by_key(pool, ws, start, end, key)
        .await
        .unwrap()
        .expect("命中幂等键");
    assert_eq!(found.id, reservation.id);

    increment_reserved(pool, ws, start, end).await.unwrap();

    // 陈旧扫掠：没有 run 行 → 第一条界就把「已预留但没落 run」的捞出来（`ar.id IS NULL`）。
    let recoverable = list_recoverable(pool, Utc::now() + Duration::seconds(1), Utc::now(), 10)
        .await
        .unwrap();
    assert!(
        recoverable.iter().any(|row| row.id == reservation.id),
        "没有 run 行的预留要被第一条界捞出来"
    );

    // 消费：`reserved - 1`、`used + 1`。
    let period = consume(pool, reservation.id)
        .await
        .unwrap()
        .expect("首次消费");
    assert_eq!(period.reserved_count, 0);
    assert_eq!(period.used_count, 1);
    // 终态重放是正常路径（重试的终态回调会命中）⇒ `Ok(None)`，不是错误。
    assert!(consume(pool, reservation.id).await.unwrap().is_none());
    assert!(release(pool, reservation.id).await.unwrap().is_none());
    let after = get_period(pool, ws, start, end).await.unwrap().unwrap();
    assert_eq!(after.used_count, 1, "`used` 在周期内单调");

    // 消费过的预留不再是「幂等命中」（`state <> 'released'` 仍成立 ⇒ 仍然命中）。
    let found = get_reservation_by_key(pool, ws, start, end, key)
        .await
        .unwrap()
        .expect("consumed 仍在幂等窗口内");
    assert_eq!(found.state, STATE_CONSUMED);

    teardown(&fixture).await;
}

/// `release` 归还额度并**释放幂等键**（同一 key 可以再建预留）——这是重试语义，不是漏洞。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
async fn release_returns_quota_and_frees_idempotency_key() {
    let Some(fixture) = setup().await else {
        println!("skip release_returns_quota_and_frees_idempotency_key: no env");
        return;
    };
    let pool = fixture.db.pool();
    let ws = fixture.workspace_id;
    let start = Utc::now() - Duration::hours(1);
    let end = start + Duration::days(30);
    ensure_period(pool, ws, start, end).await.unwrap();

    let key = "idem-lum1564-release";
    let first = reserve(pool, ws, start, end, 1, 1, "api", key)
        .await
        .unwrap();
    increment_reserved(pool, ws, start, end).await.unwrap();

    let period = release(pool, first.id).await.unwrap().expect("首次释放");
    assert_eq!(period.reserved_count, 0);
    assert_eq!(period.used_count, 0, "释放不产生用量");
    assert!(release(pool, first.id).await.unwrap().is_none(), "终态重放");
    assert!(
        get_reservation_by_key(pool, ws, start, end, key)
            .await
            .unwrap()
            .is_none(),
        "`released` 行不在幂等窗口内"
    );

    // 同一 key 再建：库里两条预留，旧的是 released。
    let second = reserve(pool, ws, start, end, 1, 1, "api", key)
        .await
        .unwrap();
    assert_ne!(second.id, first.id);
    assert_eq!(second.state, STATE_RESERVED);
    let old = sqlx::query_scalar::<_, String>(
        "SELECT state FROM autopilot_quota_reservation WHERE id = $1",
    )
    .bind(first.id)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(old, STATE_RELEASED);
    // 没有 run 行的 released 行不会被陈旧扫掠捞走（只扫 `state = 'reserved'`）。
    assert!(
        list_recoverable(pool, Utc::now() + Duration::seconds(1), Utc::now(), 10)
            .await
            .unwrap()
            .iter()
            .all(|row| row.id != first.id)
    );

    teardown(&fixture).await;
}
