#![allow(dead_code)] // 三个 bench target 共用本模块，各自只用得到其中一部分。
//! 三个 bench target 共用的 harness：真库 + 固定数据集 + R10 断言 + criterion 档位 + 收尾。
//!
//! `[[bench]]` 都是 `harness = false` ⇒ **main 由本仓自己写**（`benches/<name>.rs` 的 `fn main`）。
//! 三个 main 的形状逐字相同：`setup → criterion() → 各自注册 case → final_summary → finish`。
//! 差别只在中间注册的那几行，所以这里只放**共用**部分（含「无库即红」与 R10 断言 —— 任何一个
//! target 单独跑也都必须过这两关）。

use std::time::Duration;

use criterion::Criterion;
use mc_bench::{
    assert_statement_cache_reuse, cleanup_dataset, db, finalize, pool, prepare_dataset, runtime,
    Dataset, StatementCacheEvidence, MEASUREMENT_SECS, SAMPLE_SIZE, WARM_UP_SECS,
};
use mc_db::Db;
use mc_repos::inbox::InboxRepo;
use mc_repos::issue::IssueRepo;
use mc_repos::issue_table::IssueTableRepo;
use sqlx::postgres::PgPool;

/// 一次 bench run 的上下文：真库连接 + 固定数据集 + 三个被测仓储 + R10 证据。
pub struct BenchContext {
    /// criterion 的 async 执行器（`b.to_async(&runtime)`）。
    pub runtime: tokio::runtime::Runtime,
    /// bench 自己的池（收尾时用它清库）。
    pub pool: PgPool,
    /// 数据集描述。
    pub dataset: Dataset,
    /// R10 的可判证据（收尾时落进报告）。
    pub r10: StatementCacheEvidence,
    /// 热点 ①：issue 列表。
    pub issues: IssueRepo,
    /// 热点 ②：facets 聚合。
    pub tables: IssueTableRepo,
    /// 热点 ③：inbox 归档游标。
    pub inbox: InboxRepo,
}

/// 🔴 建上下文：**缺库即红**（[`mc_bench::pool`] 会 panic），R10 的可判用例在这里就跑一次。
///
/// 数据集每次**重建**（[`mc_bench::prepare_dataset`]）：三个 target 各自独立起跑，不能靠
/// 「上一个 target 留下的库」（那会让读数取决于别人清没清库，实测差 29%，见 `docs/32` §41.4）。
pub fn setup() -> BenchContext {
    let runtime = runtime();
    let pool = runtime.block_on(pool());
    let (dataset, r10) = runtime.block_on(async {
        let dataset = prepare_dataset(&pool).await;
        let r10 = assert_statement_cache_reuse(dataset.workspace_id).await;
        (dataset, r10)
    });
    let database: Db = db(pool.clone());
    BenchContext {
        runtime,
        pool,
        dataset,
        r10,
        issues: IssueRepo::new(database.clone()),
        tables: IssueTableRepo::new(database.clone()),
        inbox: InboxRepo::new(database),
    }
}

impl BenchContext {
    /// 收尾：清库（**必须**做 —— 同一个库还要跑门 ⑥/⑧）→ 判阈值写报告。
    ///
    /// `finish` 要在 `criterion.final_summary()` **之后**调用（那时 `sample.json` 才落盘）。
    pub fn finish(self, bench: &str, cases: &[&str]) {
        self.runtime.block_on(cleanup_dataset(&self.pool));
        finalize(bench, cases, &self.r10);
    }
}

/// criterion 的档位：**先**写本片口径，**再** `configure_from_args()`（显式传的 CLI 参数优先，
/// 没传的保持本片值 —— `docs/64` §2.5 的 `--sample-size 100` 因此可复算也可覆盖）。
pub fn criterion() -> Criterion {
    Criterion::default()
        .warm_up_time(Duration::from_secs(WARM_UP_SECS))
        .measurement_time(Duration::from_secs(MEASUREMENT_SECS))
        .sample_size(SAMPLE_SIZE)
        .configure_from_args()
}
