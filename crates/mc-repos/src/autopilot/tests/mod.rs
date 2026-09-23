//! `crate::autopilot` 的真库测试脚手架（M5-1 / LUM-1564）。
//!
//! 运行：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1563:…@127.0.0.1:5432/multica_lum1563 \
//!   cargo test -p mc-repos --lib -- --ignored autopilot::tests
//! ```
//!
//! 没有 `MULTICA_TEST_DATABASE_URL` → 静默跳过；**设了却连不上 → panic**（库坏了必须红，
//! 静默跳过会让「空跑」伪装成绿）。
//!
//! 只铺配额两张表需要的东西（`workspace` 是它们唯一的外键目标）：读面 `get_period` 与
//! 预留生命周期（`reserve` → `consume` / `release`）的语义都是「内存版看着也对」的那种，
//! 只有真 PostgreSQL 的部分唯一索引与 `FOR UPDATE` 能给结论。

use std::env;

use mc_db::Db;
use uuid::Uuid;

mod quota;
mod write;

/// 一个工作区 + 一个连接池（配额行只挂在 `workspace` 上）。
pub(super) struct Fixture {
    pub(super) db: Db,
    pub(super) workspace_id: Uuid,
}

pub(super) async fn setup() -> Option<Fixture> {
    let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
    let db = Db::connect(&url, 4, 1)
        .await
        .expect("MULTICA_TEST_DATABASE_URL is set but connect failed");
    let pool = db.pool();
    let workspace_id: Uuid = sqlx::query_scalar(
        "INSERT INTO workspace(name, slug) VALUES ('itest-lum1564-quota', $1) RETURNING id",
    )
    .bind(format!("itest-lum1564-quota-{}", Uuid::new_v4()))
    .fetch_one(pool)
    .await
    .expect("insert workspace");
    let _ = pool;
    Some(Fixture { db, workspace_id })
}

pub(super) async fn teardown(fixture: &Fixture) {
    // 订阅/预留表都没有到 workspace 的外键级联保证（预留的 workspace_id 有 FK，period 也有），
    // 顺序删即可。
    let pool = fixture.db.pool();
    let _ = sqlx::query("DELETE FROM autopilot_quota_reservation WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM autopilot_quota_period WHERE workspace_id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
        .bind(fixture.workspace_id)
        .execute(pool)
        .await;
}
