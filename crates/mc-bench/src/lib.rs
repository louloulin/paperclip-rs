//! `mc-bench` —— W10 的性能 bench（`docs/64-M10-PLAN.md` §2.5 / §9.6；issue `LUM-2108`）。
//!
//! 把 `plan1.md` §3.6 点名的**三条上游热点**变成可判定的 bench：`benches/issue_list.rs`（issue
//! 列表：`limit=50` + 排序 + 过滤）、`benches/facets.rs`（facets 聚合：disjunctive + total）、
//! `benches/inbox_cursor.rs`（inbox 游标：keyset 翻页）；被测入口分别是 `mc_repos` 的 `IssueRepo` /
//! `IssueTableRepo` / `InboxRepo`。
//!
//! 模块分工（**判据只有一处**：口径常量在 [`config`]，调用在 [`hotspots`]）：
//!
//! | 模块 | 内容 |
//! | --- | --- |
//! | [`config`] | 口径常量 + 三条 hot spot 的 case 清单 + 相对/绝对阈值表 |
//! | [`conn`] | 库 URL / 显式 `statement_cache_capacity` 的池 / R10 的可判用例 |
//! | [`dataset`] | 确定性播种的固定数据集（10000 issue / 5000 comment / 5000 归档 inbox item），每次测量前重建 |
//! | [`hotspots`] | 三条热点的入参构造与执行（bench 与证据共用） |
//! | [`report`] | `sample.json` → `p50/p95/p99` → 阈值判定 → JSON |
//!
//! ## 口径（禁用「同量级」这类措辞）
//!
//! * **同一 workdir、同一 PG（`:5432`）、同一固定数据集**：播种**是 bench 的一部分**（确定性 UUID
//!   ⇒ 换台机器也是同一份数据），全部落在一个固定 id 的 workspace 下，跑完即删。criterion 档位见
//!   [`config::WARM_UP_SECS`] / [`config::MEASUREMENT_SECS`] / [`config::SAMPLE_SIZE`]。
//! * 读数 `p50/p95/p99` 取自 criterion 写的 `sample.json`（逐样本 `times/iters` = 单次迭代纳秒），
//!   用**线性插值**百分位；criterion 的 `estimates.json::median` 一并落进报告做**交叉校验**。
//! * **相对阈值**：`p95(本次) ≤ RELATIVE_P95_SLACK × p95(基线)`，由 `MC_BENCH_BASELINE` 指向基线文件
//!   时逐条判定（不设则跳过）。基线 = 本波在**未改任何生产代码**的 `base` 上跑一次的冻结值。
//! * **绝对上界**：见 [`config::ABSOLUTE_P95_BUDGETS`]。实测若远超预算，必须**带理由**改那份表
//!   （不许默默放宽）。
//!
//! ## 三条纪律
//!
//! 1. `[[bench]]` **必须** `harness = false` **且** `test = false`（见 `crates/mc-bench/Cargo.toml`）：
//!    前者只解决「criterion 自带 main 不被 libtest 吞掉」，后者才解决 cargo 的 target 选择 —— 否则
//!    门 ⑤ `cargo test --workspace` 会把整个 benchmark 当测试跑。
//! 2. **无库即红**：[`conn::database_url`] 缺变量时直接 panic，**不许静默跳过**（本仓两次「绿是
//!    空跑」的教训，见 `docs/24-W0-CI.md`）。
//! 3. `criterion` **必须** `0.7`：`0.8.2` 的 `rust_version = 1.86` > 本仓 `1.80`，`0.7.0` 的 MSRV
//!    恰好 `1.80`。
//!
//! ## R10：prepared statement cache 必须**显式**
//!
//! 上游是 Go（`database/sql` 自带 prepared statement cache）；`sqlx` 的缓存挂在连接上、缺省容量 100。
//! 本 crate 的每一个池都由 [`conn::connect_options`] 建：显式 `.statement_cache_capacity(…)`（`> 0`）。
//! [`conn::assert_statement_cache_reuse`] 是那条**可判用例**，**每次 `cargo bench -p mc-bench` 都会跑**：
//! 同一连接上把同一条语句执行两次，`pg_prepared_statements` 里该语句文本的行数**必须是 1**（第二次
//! 没有 `Parse`）；**反例对照**是同一段代码配 `capacity = 0` ⇒ 必须是 **2**（反例必须为真，否则这个
//! 判据根本没在观察 `Parse`）。
//!
//! ## 复算命令（`MC_BENCH_REPORT_OUT` 写基线；`MC_BENCH_BASELINE` 判相对阈值）
//!
//! ```text
//! MC_BENCH_BASE_SHA=$(git rev-parse HEAD) \
//! MC_BENCH_REPORT_OUT=$PWD/docs/fixtures/bench-baseline.json \
//! MULTICA_TEST_DATABASE_URL=… cargo bench -p mc-bench
//!
//! MC_BENCH_BASELINE=$PWD/docs/fixtures/bench-baseline.json \
//! MULTICA_TEST_DATABASE_URL=… cargo bench -p mc-bench
//! ```
//!
//! ⚠️ 反查 case 读数时**不要**给 criterion 传名字过滤器（`cargo bench -p mc-bench list`）：三个 target
//! 的 case 清单是写全的，[`report::finalize`] 缺任何一个 case 都会 panic（这是故意的 —— 缺读数就是
//! 证据不全）。
//!
//! ## 偏离登记（D1…D5；`docs/32-M3-DAEMON-FACE.md` §40 是权威版本）
//!
//! * **D1（构建图）**：`crates/mc-bench/Cargo.toml` 给 `mc-db` 打开 `test-util` ⇒ **全 workspace 构建**
//!   里 `mc-db` 都带上它。该 feature 只**新增两个 pub 构造函数**（`Db::from_pool` / `Db::placeholder`），
//!   不改任何既有语义（理由见该 manifest 的注释：把裸池交给 `IssueRepo` / `IssueTableRepo` 的唯一公开
//!   入口）。
//! * **D2（口径替换）**：`plan1.md` §5 的 W10 门禁「与 Go 版并行双跑」在本仓**不可执行**（无 Go 工具
//!   链、无前端资产）⇒ 由本 crate 的相对/绝对阈值 + M10-5 的冻结 golden 取代（`docs/64` §9.6 已登记
//!   该口径修订）。
//! * **D3（测库层，不测 HTTP 层）**：三条热点都**直接调仓储层**（不经 `axum`）—— `plan1.md` §3.6 把
//!   热点定义在数据访问层，HTTP 层开销不在本片预算内。
//! * **D4（相对阈值在本片的取值）**：本片 **0 路由、0 生产代码改动** ⇒ 基线 run 与「本片结果」测的是
//!   **逐字节相同**的生产代码，相对阈值是**恒等断言**：它的价值是给**后续切片**一份可机器比对的冻结
//!   参照。
//! * **D5（criterion 的 `async_tokio` feature）**：`docs/64` 只写「`criterion = \"0.7\"`」一行，本片
//!   在根 manifest 上加了 `features = ["async_tokio"]`。理由：三条热点的被测入口是 `async fn`，用
//!   `b.to_async(&runtime)` 才测的是「future 完成」而不是「`block_on` 套壳 + future 完成」；该 feature
//!   只引入 `tokio`（**已在** workspace 依赖里），不改任何既有产物。

pub mod config;
pub mod conn;
pub mod dataset;
pub mod hotspots;
pub mod report;

pub use config::{
    p95_budget, P95Budget, ABSOLUTE_P95_BUDGETS, COMMENTS, DATABASE_URL_ENV, FACETS_BENCH,
    FACETS_CASES, INBOX_CURSOR_BENCH, INBOX_CURSOR_CASES, INBOX_ITEMS, ISSUES, ISSUE_LIST_BENCH,
    ISSUE_LIST_CASES, MAX_CONNECTIONS, MEASUREMENT_SECS, PAGE_LIMIT, PROJECT_ID,
    RELATIVE_P95_SLACK, SAMPLE_SIZE, STATEMENT_CACHE_CAPACITY, USER_ID, WARM_UP_SECS, WORKSPACE_ID,
};
pub use conn::{
    assert_statement_cache_reuse, connect_options, database_url, db, pool, pool_with_capacity,
    runtime, StatementCacheEvidence,
};
pub use dataset::{
    assert_clean_database, cleanup_dataset, prepare_dataset, reset_dataset, Dataset,
};
pub use hotspots::{
    cursor_after, facets_query, issue_list_filter, run_facets, run_inbox_page, run_issue_list,
    run_issue_list_with_total,
};
pub use report::{criterion_dir, finalize, read_case, report_dir, BenchReport, CaseReport};
