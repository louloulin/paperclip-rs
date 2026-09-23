//! `GET /api/autopilots/usage`（M5-1）。
//!
//! 这条路由的形状有一半取决于 entitlement 平面是否给出策略，而本仓**默认没有** entitlement
//! 平面（R7）⇒ 写死 `off` 就等于让另一半形状没有测试。这里用 `mc-autopilot` 的进程内安装缝
//! （`install_policy_provider`，一次即终态）把 `observe` / `enforce` / 「有策略但还没有周期行」
//! 三条分支都跑一遍真库。
//!
//! 平面是**按工作区**答的，stub 只认本用例自己铺的两个工作区 ⇒ 同进程其它用例仍然拿 `off`，
//! 不会互相污染。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, Utc};
use mc_autopilot::quota::{install_policy_provider, QuotaAction, QuotaPolicy, QuotaPolicyProvider};
use mc_core::Id;
use serde_json::json;
use uuid::Uuid;

use super::support::{call, cleanup, seed_quota_period, seed_workspace};

/// 固定策略的平面（按 workspace id 查表）。
struct StaticPlane {
    policies: HashMap<Uuid, QuotaPolicy>,
}

impl QuotaPolicyProvider for StaticPlane {
    fn policy(&self, workspace_id: Id) -> Option<QuotaPolicy> {
        self.policies.get(&workspace_id.0).cloned()
    }
}

/// `off`（未装平面）→ 装平面后的 `observe` / `enforce` / 无周期行三种形态。
#[tokio::test]
#[ignore = "requires PostgreSQL (MULTICA_TEST_DATABASE_URL)"]
// 四种策略形态（off / observe / enforce / 无周期行）刻意放在**同一个**用例里：`install_policy_provider`
// 是进程级首次生效的 `OnceLock`，拆成多个用例就会互相污染（第二个用例只能拿到第一个装的平面）。
// 代价是函数长 —— 允许，但每段都用注释标出它在钉哪条语义。
#[allow(clippy::too_many_lines)]
async fn usage_off_by_default_and_policy_shapes_from_real_db() {
    let Some((pool, db)) = super::support::connect().await else {
        println!("skip usage_off_by_default_and_policy_shapes_from_real_db: no env");
        return;
    };
    let app = super::support::app_with_db(db);
    let (off_ws, off_owner) = seed_workspace(&pool, "owner").await;
    let (observe_ws, observe_owner) = seed_workspace(&pool, "owner").await;
    let (enforce_ws, enforce_owner) = seed_workspace(&pool, "owner").await;
    let (fresh_ws, fresh_owner) = seed_workspace(&pool, "owner").await;

    // ① 未装平面 ⇒ `off` + 全 null。`blocked_counts` 也是 `null`（不是 `{}`）。
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots/usage",
        off_ws,
        off_owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["action"], "off");
    for key in [
        "used",
        "reserved",
        "total",
        "limit",
        "reached",
        "period_start",
        "period_end",
        "reset_at",
        "blocked_counts",
    ] {
        assert!(body[key].is_null(), "`{key}` 应是显式 null: {body}");
    }

    // ② 装平面：observe（limit 5）/ enforce（limit 5）/ enforce 但还没有周期行。
    let start = Utc::now() - Duration::hours(2);
    let end = start + Duration::days(30);
    let mut policies = HashMap::new();
    for ws in [observe_ws, enforce_ws, fresh_ws] {
        policies.insert(
            ws,
            QuotaPolicy::new(QuotaAction::Enforce, 5, start.into(), end.into()),
        );
    }
    policies.insert(
        observe_ws,
        QuotaPolicy::new(QuotaAction::Observe, 5, start.into(), end.into()),
    );
    assert!(
        install_policy_provider(Arc::new(StaticPlane { policies })),
        "平面在本进程只装一次；装不上说明已有实现占位（用例顺序被打破）"
    );

    // observe：used/reserved/total 来自真库读数，`limit` 照发，`reached` 恒 null。
    seed_quota_period(&pool, observe_ws, start, end, 2, 1, json!({"limit": 3})).await;
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots/usage",
        observe_ws,
        observe_owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["action"], "observe");
    assert_eq!(body["used"], 2);
    assert_eq!(body["reserved"], 1);
    assert_eq!(body["total"], 3);
    assert_eq!(body["limit"], 5);
    assert!(
        body["reached"].is_null(),
        "observe 下 reached 恒 null: {body}"
    );
    assert_eq!(body["blocked_counts"], json!({"limit": 3}));
    let period_start =
        chrono::DateTime::parse_from_rfc3339(body["period_start"].as_str().unwrap()).unwrap();
    assert_eq!(period_start.with_timezone(&Utc), start);

    // enforce：`total >= limit` ⇒ `reached=true`；`blocked_counts={}` 是 `{}` 而不是 null。
    seed_quota_period(&pool, enforce_ws, start, end, 4, 1, json!({})).await;
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots/usage",
        enforce_ws,
        enforce_owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["action"], "enforce");
    assert_eq!(body["total"], 5);
    assert_eq!(body["reached"], json!(true));
    assert_eq!(body["blocked_counts"], json!({}));

    // enforce 且**没有周期行**：`pgx.ErrNoRows` 是正常路径 ⇒ 0/0，不是错误。
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots/usage",
        fresh_ws,
        fresh_owner,
        None,
    )
    .await;
    assert_eq!(status, 200, "body={body}");
    assert_eq!(body["action"], "enforce");
    assert_eq!(body["used"], 0);
    assert_eq!(body["reserved"], 0);
    assert_eq!(body["total"], 0);
    assert_eq!(body["reached"], json!(false));
    assert_eq!(body["blocked_counts"], json!({}));
    // 未装策略的工作区仍然 off（平面是按工作区答的）。
    let (status, body) = call(
        &app,
        "GET",
        "/api/autopilots/usage",
        off_ws,
        off_owner,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["action"], "off");

    cleanup(&pool, off_ws, &[off_owner]).await;
    cleanup(&pool, observe_ws, &[observe_owner]).await;
    cleanup(&pool, enforce_ws, &[enforce_owner]).await;
    cleanup(&pool, fresh_ws, &[fresh_owner]).await;
}
