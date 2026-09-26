//! 连接面 —— R10（prepared statement cache **显式**）的**唯一**入口 + 可判用例。
//!
//! 上游是 Go（`database/sql` 自带 prepared statement cache）；`sqlx` 的缓存挂在连接上、缺省容量
//! 100。本 crate 的每一个池都由 [`connect_options`] 建：显式 `.statement_cache_capacity(…)`（`> 0`）。
//! [`assert_statement_cache_reuse`] 是那条**可判用例**，**每次 `cargo bench -p mc-bench` 都会跑**。

use std::str::FromStr;
use std::time::Duration;

use serde::Serialize;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use uuid::Uuid;

use mc_db::Db;

use crate::config::{DATABASE_URL_ENV, MAX_CONNECTIONS, STATEMENT_CACHE_CAPACITY};

/// 读库 URL；**缺变量即 panic**（无库即红，不静默跳过）。
///
/// 本仓两次「绿是空跑」的教训（见 `docs/24-W0-CI.md`）：DB 集成测试不设 `MULTICA_TEST_DATABASE_URL`
/// 时会静默跳过 ⇒ 报告里那个「绿」什么都没测。本 crate 的判据是**跑不出读数就红**。
pub fn database_url() -> String {
    match std::env::var(DATABASE_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "{DATABASE_URL_ENV} is not set: mc-bench 必须在真库上跑，缺库即红（本仓两次「绿是空跑」的教训）。\
             例：MULTICA_TEST_DATABASE_URL=postgres://mc_lum2108:pw@127.0.0.1:5432/multica_lum2108"
        ),
    }
}

/// 🔴 R10 的**唯一**连接参数入口：显式 `.statement_cache_capacity(capacity)`。
pub fn connect_options(url: &str, capacity: usize) -> PgConnectOptions {
    PgConnectOptions::from_str(url)
        .expect("MULTICA_TEST_DATABASE_URL is not a valid PostgreSQL connection URL")
        .statement_cache_capacity(capacity)
}

/// 用**显式**语句缓存容量建一个池（bench 全程只用这个入口 + [`pool`]）。
pub async fn pool_with_capacity(capacity: usize) -> PgPool {
    PgPoolOptions::new()
        .max_connections(MAX_CONNECTIONS)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(10))
        .connect_with(connect_options(&database_url(), capacity))
        .await
        .expect("cannot connect to the bench database")
}

/// [`STATEMENT_CACHE_CAPACITY`] 的池（bench 的默认池）。每次调用新建一个池，调用方负责用完即弃。
pub async fn pool() -> PgPool {
    pool_with_capacity(STATEMENT_CACHE_CAPACITY).await
}

/// 裸池 → `Db`（`mc_repos` 各仓储的入口）。
pub fn db(pool: PgPool) -> Db {
    Db::from_pool(pool)
}

/// bench 用的 tokio runtime（criterion 是同步 harness，仓储调用是 async）。
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("mc-bench")
        .build()
        .expect("cannot build the tokio runtime")
}

// --- R10：语句缓存的可判用例 -----------------------------------------------

/// R10 证据（落进 stdout 与报告）。
#[derive(Debug, Clone, Serialize)]
pub struct StatementCacheEvidence {
    /// 池的显式容量。
    pub capacity: usize,
    /// 正例：同一语句两次执行后 `pg_prepared_statements` 里该语句的行数（期望 `1`）。
    pub cached_rows: i64,
    /// 反例（`capacity = 0`）的同款计数（期望 `2`）。
    pub uncached_rows: i64,
}

/// 同一条语句文本重复执行两次，用来观察第二次有没有再发一次 `Parse`。
///
/// 用**文本过滤**而不是总数，是因为这条计数查询自己也会进 `pg_prepared_statements`。
const R10_PROBE_SQL: &str = "SELECT COUNT(*)::bigint FROM issue WHERE workspace_id = $1";

/// 同一语句在同一连接上执行两次后，`pg_prepared_statements` 里**该语句文本**的行数。
///
/// 判据是「不再 `Parse`」：第二次执行若又发了一次 `Parse`，Postgres 会为它再建一个（自动命名的）
/// prepared statement ⇒ 行数变 2。
async fn prepared_rows(conn: &mut sqlx::PgConnection, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM pg_prepared_statements WHERE statement = $1",
    )
    .bind(sql)
    .fetch_one(&mut *conn)
    .await
    .expect("cannot read pg_prepared_statements")
}

/// 在给定的池上：取**一条**连接，把探针语句跑两次，再数它的 prepared statement 行数。
async fn probe_once(pool: &PgPool, workspace_id: Uuid) -> i64 {
    let mut conn = pool
        .acquire()
        .await
        .expect("cannot acquire a probe connection");
    for _ in 0..2 {
        let _: i64 = sqlx::query_scalar(R10_PROBE_SQL)
            .bind(workspace_id)
            .fetch_one(&mut *conn)
            .await
            .expect("R10 probe query");
    }
    prepared_rows(&mut conn, R10_PROBE_SQL).await
}

/// 🔴 R10 的可判用例（**每次 `cargo bench` 都跑**；任一断言不成立即 panic）。
///
/// * 正例：容量 [`STATEMENT_CACHE_CAPACITY`]（`> 0`）⇒ 两次执行后 `cached_rows == 1`（第二次不 `Parse`）。
/// * 反例对照：容量 `0`（缓存关闭）⇒ `uncached_rows == 2`（**必须**为 2，否则说明这个判据没在观察
///   `Parse`，正例的「1」也就没有意义）。
pub async fn assert_statement_cache_reuse(workspace_id: Uuid) -> StatementCacheEvidence {
    let cached = probe_once(&pool().await, workspace_id).await;
    let uncached = probe_once(&pool_with_capacity(0).await, workspace_id).await;
    let evidence = StatementCacheEvidence {
        capacity: STATEMENT_CACHE_CAPACITY,
        cached_rows: cached,
        uncached_rows: uncached,
    };
    println!(
        "R10 statement_cache_capacity={} reuse: cached_rows={} (expect 1) · control capacity=0 uncached_rows={} (expect 2)",
        evidence.capacity, evidence.cached_rows, evidence.uncached_rows
    );
    assert_eq!(
        cached, 1,
        "R10 FAILED: the same statement executed twice on one connection produced {cached} prepared \
         statements (expected 1) — the statement cache is not reusing the parsed statement"
    );
    assert_eq!(
        uncached, 2,
        "R10 control FAILED: with statement_cache_capacity=0 the second execution did not re-Parse \
         ({uncached} prepared statements, expected 2) — the probe does not observe Parse at all, so \
         the positive case above proves nothing"
    );
    evidence
}
