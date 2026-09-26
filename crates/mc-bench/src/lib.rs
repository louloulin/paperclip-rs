//! `mc-bench` —— W10 的性能 bench（`docs/64-M10-PLAN.md` §2.5 / §9.6；issue `LUM-2108`）。把
//! `plan1.md` §3.6 点名的**三条上游热点**变成可判定的 bench：`benches/issue_list.rs`（issue 列表：
//! `limit=50` + 排序 + 过滤）、`benches/facets.rs`（facets 聚合：disjunctive + total）、
//! `benches/inbox_cursor.rs`（inbox 游标：keyset 翻页）；被测入口分别是 `mc_repos` 的 `IssueRepo` /
//! `IssueTableRepo` / `InboxRepo`。
//!
//! ## 口径（判据只有一处，禁用「同量级」这类措辞）
//!
//! * **同一 workdir、同一 PG（`:5432`）、同一固定数据集**：10000 issue / 5000 comment / 5000 归档 inbox
//!   item。**播种是 bench 的一部分**（确定性 UUID + 时间戳 ⇒ 换台机器也是同一份数据），全部落在一个固定
//!   id 的 workspace 下，跑完即删。criterion 档位见 [`WARM_UP_SECS`] / [`MEASUREMENT_SECS`] /
//!   [`SAMPLE_SIZE`]。
//! * 读数 `p50/p95/p99` 取自 criterion 写的 `target/criterion/<bench>/<case>/new/sample.json`（逐样本
//!   `times/iters` = 单次迭代纳秒），用**线性插值**百分位；criterion 的 `estimates.json::median` 一并落进
//!   报告做**交叉校验**。
//! * **相对阈值**：`p95(本次) ≤ RELATIVE_P95_SLACK × p95(基线)`，由 `MC_BENCH_BASELINE` 指向基线文件时逐条
//!   判定（不设则跳过）。基线 = **本波起手**在**未改任何生产代码**的 `base` 上跑一次的冻结值。
//! * **绝对上界**：见 [`ABSOLUTE_P95_BUDGETS`]。起手实测若远超预算，必须**带理由**改那份表（不许默默放宽）。
//!
//! ## 三条纪律
//!
//! 1. `[[bench]]` **必须** `harness = false` **且** `test = false`（见 `crates/mc-bench/Cargo.toml`）：前者只
//!    解决「criterion 自带 main 不被 libtest 吞掉」，后者才解决 cargo 的 target 选择 —— 否则门 ⑤
//!    `cargo test --workspace` 会把整个 benchmark 当测试跑。
//! 2. **无库即红**：缺 [`DATABASE_URL_ENV`] 时 [`database_url`] 直接 panic，**不许静默跳过**（本仓两次
//!    「绿是空跑」的教训，见 `docs/24`）。
//! 3. `criterion` **必须** `0.7`：`0.8.2` 的 `rust_version = 1.86` > 本仓 `1.80`，`0.7.0` 的 MSRV 恰好 `1.80`。
//!
//! ## R10：prepared statement cache 必须**显式**
//!
//! 上游是 Go（`database/sql` 自带 prepared statement cache）；`sqlx` 的缓存挂在连接上、缺省容量 100。本 crate
//! 的每一个池都由 [`connect_options`] 建：显式 `.statement_cache_capacity(…)`（`> 0`）。
//! [`assert_statement_cache_reuse`] 是那条**可判用例**，**每次 `cargo bench -p mc-bench` 都会跑**：同一连接上
//! 把同一条语句执行两次，`pg_prepared_statements` 里该语句文本的行数**必须是 1**（第二次没有 `Parse`）；**反例
//! 对照**是同一段代码配 `capacity = 0` ⇒ 必须是 **2**（反例必须为真，否则这个判据根本没在观察 `Parse`）。
//!
//! ## 复算命令（`MC_BENCH_REPORT_OUT` 写基线；`MC_BENCH_BASELINE` 判相对阈值）
//!
//! ```text
//! MC_BENCH_REPORT_OUT=$PWD/docs/fixtures/bench-baseline.json MULTICA_TEST_DATABASE_URL=… cargo bench -p mc-bench
//! MC_BENCH_BASELINE=$PWD/docs/fixtures/bench-baseline.json  MULTICA_TEST_DATABASE_URL=… cargo bench -p mc-bench
//! ```
//!
//! ## 偏离登记（D1…D4；本片不新增 `docs/32` 号段，理由见交付说明）
//!
//! * **D1（构建图）**：`crates/mc-bench/Cargo.toml` 给 `mc-db` 打开 `test-util` ⇒ **全 workspace 构建**里
//!   `mc-db` 都带上它。该 feature 只**新增两个 pub 构造函数**（`Db::from_pool` / `Db::placeholder`），不改
//!   任何既有语义（理由见该 manifest 的注释：把裸池交给 `IssueRepo` / `IssueTableRepo` 的唯一公开入口）。
//! * **D2（口径替换）**：`plan1.md` §5 的 W10 门禁「与 Go 版并行双跑」在本仓**不可执行**（无 Go 工具链、无前端
//!   资产）⇒ 由本 crate 的相对/绝对阈值 + M10-5 的冻结 golden 取代（`docs/64` §9.6 已登记该口径修订）。
//! * **D3（测库层，不测 HTTP 层）**：三条热点都**直接调仓储层**（不经 `axum`）—— `plan1.md` §3.6 把热点定义在
//!   数据访问层，HTTP 层开销不在本片预算内。
//! * **D4（相对阈值在本片的取值）**：本片 **0 路由、0 生产代码改动** ⇒ 基线 run 与「本片结果」测的是**逐字节
//!   相同**的生产代码，相对阈值是**恒等断言**：它的价值是给**后续切片**一份可机器比对的冻结参照。

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;
use mc_repos::inbox::{ArchivedCursor, ArchivedInboxFilter, ArchivedInboxPage, InboxRepo};
use mc_repos::issue::{IssueFilter, IssueOrderBy, IssueRepo};
use mc_repos::issue_table::{
    IssueTableRepo, TableFacetKind, TableFacetsPage, TableFacetsQuery, TableFilter,
};

// --- 常量：口径一旦写在这里，就是「判据只有一处」 ---------------------------

/// bench 需要的库 URL 环境变量名（与仓内其它真库用例同款）。
pub const DATABASE_URL_ENV: &str = "MULTICA_TEST_DATABASE_URL";

/// 🔴 R10：连接池的语句缓存容量，**显式**写入每一个连接（`sqlx` 缺省也是 100，但不许靠缺省）。
pub const STATEMENT_CACHE_CAPACITY: usize = 128;

/// 池大小（无并发 bench，够用即可）。
pub const MAX_CONNECTIONS: u32 = 8;

/// criterion 档位：warm-up 3s / measurement 10s / 100 样本（`docs/64` §2.5）。
pub const WARM_UP_SECS: u64 = 3;
pub const MEASUREMENT_SECS: u64 = 10;
pub const SAMPLE_SIZE: usize = 100;

/// 固定数据集规模：10000 issue / 5000 comment / 5000 归档 inbox item。
pub const ISSUES: usize = 10_000;
pub const COMMENTS: usize = 5_000;
/// 见 [`ISSUES`]（每个 issue 组一行 ⇒ 5000 个组）。
pub const INBOX_ITEMS: usize = 5_000;

/// 列表 / 翻页的页大小（`docs/64` §2.5 的 `limit=50`）。
pub const PAGE_LIMIT: i64 = 50;

/// 数据集：workspace / user（收件人与 comment 作者）/ 唯一的 project。
///
/// 全是**确定性** UUID：换台机器播种出同一份数据。
pub const WORKSPACE_ID: Uuid = Uuid::from_u128(0xb0e0_c4a0_0000_4000_8000_0000_0000_a001);
pub const USER_ID: Uuid = Uuid::from_u128(0xb0e0_c4a0_0000_4000_8000_0000_0000_a002);
pub const PROJECT_ID: Uuid = Uuid::from_u128(0xb0e0_c4a0_0000_4000_8000_0000_0000_a003);

/// 三个 bench target 名（= `benches/<name>.rs`，也是 criterion 的 group 名）。
pub const ISSUE_LIST_BENCH: &str = "issue_list";
pub const FACETS_BENCH: &str = "facets";
pub const INBOX_CURSOR_BENCH: &str = "inbox_cursor";

/// 各 bench 的 case 清单（[`finalize`] 用它反查预算 —— **没登记的 case 直接判红**）。
pub const ISSUE_LIST_CASES: [&str; 3] = [
    "list_limit50_updated_desc",
    "list_limit50_filtered",
    "list_with_total_limit50",
];
pub const FACETS_CASES: [&str; 2] = [
    "table_facets_all5_with_total",
    "table_facets_status_priority",
];
pub const INBOX_CURSOR_CASES: [&str; 2] = ["archived_page_first", "archived_page_cursor"];

/// 相对阈值：`p95(本次) ≤ RELATIVE_P95_SLACK × p95(基线)`。
pub const RELATIVE_P95_SLACK: f64 = 1.25;

/// 一个 case 的**绝对** p95 上界（毫秒）。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct P95Budget {
    /// bench target 名。
    pub bench: &'static str,
    /// criterion 的 `bench_function` 名。
    pub case: &'static str,
    /// p95 上界（毫秒）。
    pub p95_ms: f64,
}

/// 🔴 绝对上界（`docs/64` §2.5 的第一版预算；`32 vCPU` / 本地 PG / 无并发）。起手实测若远超预算，
/// **必须**改这份表并在 `docs/32` 的登记段写理由 —— 不许默默放宽。
pub const ABSOLUTE_P95_BUDGETS: [P95Budget; 7] = [
    P95Budget {
        bench: ISSUE_LIST_BENCH,
        case: "list_limit50_updated_desc",
        p95_ms: 5.0,
    },
    P95Budget {
        bench: ISSUE_LIST_BENCH,
        case: "list_limit50_filtered",
        p95_ms: 5.0,
    },
    P95Budget {
        bench: ISSUE_LIST_BENCH,
        case: "list_with_total_limit50",
        p95_ms: 5.0,
    },
    P95Budget {
        bench: FACETS_BENCH,
        case: "table_facets_all5_with_total",
        p95_ms: 20.0,
    },
    P95Budget {
        bench: FACETS_BENCH,
        case: "table_facets_status_priority",
        p95_ms: 20.0,
    },
    P95Budget {
        bench: INBOX_CURSOR_BENCH,
        case: "archived_page_first",
        p95_ms: 2.0,
    },
    P95Budget {
        bench: INBOX_CURSOR_BENCH,
        case: "archived_page_cursor",
        p95_ms: 2.0,
    },
];

/// 按 `(bench, case)` 查预算；**没登记 = panic**（新增 case 必须同时登记预算）。
pub fn p95_budget(bench: &str, case: &str) -> f64 {
    ABSOLUTE_P95_BUDGETS
        .iter()
        .find(|b| b.bench == bench && b.case == case)
        .unwrap_or_else(|| panic!("no absolute p95 budget registered for {bench}/{case}"))
        .p95_ms
}

// --- 连接：显式 `statement_cache_capacity`（R10） ---------------------------

/// 读库 URL；**缺变量即 panic**（无库即红，不静默跳过）。
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

// --- 固定数据集：确定性播种（bench 的一部分） ------------------------------

/// 数据集描述（落进报告的 `dataset` 段）。
#[derive(Debug, Clone, Serialize)]
pub struct Dataset {
    /// 数据集的 workspace / user（见 [`WORKSPACE_ID`] / [`USER_ID`]）。
    pub workspace_id: Uuid,
    /// 见 `workspace_id`。
    pub user_id: Uuid,
    /// issue / comment / 归档 inbox item 行数（见 [`ISSUES`]）。
    pub issues: usize,
    /// 见 `issues`。
    pub comments: usize,
    /// 见 `issues`。
    pub inbox_items: usize,
}

impl Dataset {
    /// 默认规模的描述（不触库）。
    pub fn spec() -> Self {
        Self {
            workspace_id: WORKSPACE_ID,
            user_id: USER_ID,
            issues: ISSUES,
            comments: COMMENTS,
            inbox_items: INBOX_ITEMS,
        }
    }
}

/// 播种（幂等）：数据集**不在**（或规模不对）时才删+建，已经在就原样复用。
///
/// 行数用三个 `COUNT(*)` 校验 ⇒ 半途崩掉的库会被自动重置。
pub async fn ensure_dataset(pool: &PgPool) -> Dataset {
    let ds = Dataset::spec();
    let expected = (
        i64::try_from(ISSUES).expect("ISSUES fits in i64"),
        i64::try_from(COMMENTS).expect("COMMENTS fits in i64"),
        i64::try_from(INBOX_ITEMS).expect("INBOX_ITEMS fits in i64"),
    );
    if dataset_counts(pool).await == expected {
        return ds;
    }
    reset_dataset(pool).await;
    seed_dataset(pool).await;
    ds
}

async fn dataset_counts(pool: &PgPool) -> (i64, i64, i64) {
    let sql = "SELECT (SELECT COUNT(*) FROM issue WHERE workspace_id = $1)::bigint, \
                      (SELECT COUNT(*) FROM comment WHERE workspace_id = $1)::bigint, \
                      (SELECT COUNT(*) FROM inbox_item WHERE workspace_id = $1)::bigint";
    sqlx::query_as::<_, (i64, i64, i64)>(sql)
        .bind(WORKSPACE_ID)
        .fetch_one(pool)
        .await
        .expect("cannot count the bench dataset")
}

/// 删掉数据集（`workspace` 级联到 issue / comment / `inbox_item` / project）。
pub async fn reset_dataset(pool: &PgPool) {
    for sql in [
        "DELETE FROM workspace WHERE id = $1",
        "DELETE FROM \"user\" WHERE id = $1",
    ] {
        sqlx::query(sql)
            .bind(WORKSPACE_ID)
            .execute(pool)
            .await
            .expect("cannot reset the bench dataset");
    }
}

async fn seed_dataset(pool: &PgPool) {
    let (issues, comments, inbox) = (
        i32::try_from(ISSUES).expect("ISSUES fits in i32"),
        i32::try_from(COMMENTS).expect("COMMENTS fits in i32"),
        i32::try_from(INBOX_ITEMS).expect("INBOX_ITEMS fits in i32"),
    );
    // 每条 `SEED_*` 的绑定顺序在语句里逐条写死；绑定个数由各自的 `$n` 决定。
    macro_rules! seed {
        ($sql:expr $(, $bind:expr)* $(,)?) => {
            sqlx::query($sql) $(.bind($bind))* .execute(pool).await.expect("seed statement failed")
        };
    }
    seed!(SEED_USER, USER_ID);
    seed!(SEED_WORKSPACE, WORKSPACE_ID, issues);
    seed!(SEED_PROJECT, PROJECT_ID, WORKSPACE_ID);
    seed!(SEED_ISSUES, WORKSPACE_ID, PROJECT_ID, issues);
    seed!(SEED_COMMENTS, WORKSPACE_ID, USER_ID, comments);
    seed!(SEED_INBOX, WORKSPACE_ID, USER_ID, inbox);
}

/// bench 结束后清库（**必须**做：同一个库还要跑门 ⑥ 的真库用例）。
pub async fn cleanup_dataset(pool: &PgPool) {
    reset_dataset(pool).await;
}

const SEED_USER: &str =
    "INSERT INTO \"user\"(id, name, email) VALUES ($1, 'mc-bench', 'mc-bench@multica.local')";

const SEED_WORKSPACE: &str = "INSERT INTO workspace(id, name, slug, issue_prefix, issue_counter) \
     VALUES ($1, 'mc-bench', 'mc-bench', 'MCB', $2)";

const SEED_PROJECT: &str =
    "INSERT INTO project(id, workspace_id, title) VALUES ($1, $2, 'mc-bench project')";

/// `$1` = workspace、`$2` = project、`$3` = 行数。
///
/// 分布是**确定性**的：status 7 值轮转、priority 5 值轮转、`assignee` 每 3 行空一次（4 个 id）、
/// `creator` 2 值轮转、`project` 每 5 行空一次；`updated_at` 用 `g + g % 97` ⇒ 排序有并列，
/// 顺带压到 `ORDER BY updated_at DESC, number DESC` 的 tie-break。`number` 必须唯一
/// （`uq_issue_workspace_number`）。
const SEED_ISSUES: &str = "INSERT INTO issue \
     (id, workspace_id, number, identifier, title, description, status, priority, \
      assignee_type, assignee_id, creator_type, creator_id, project_id, position, created_at, updated_at) \
     SELECT ('b0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            $1, g + 1, 'MCB-' || (g + 1), 'mc-bench issue ' || g, 'mc-bench issue body ' || g, \
            CASE g % 7 WHEN 0 THEN 'backlog' WHEN 1 THEN 'todo' WHEN 2 THEN 'in_progress' \
                       WHEN 3 THEN 'in_review' WHEN 4 THEN 'done' WHEN 5 THEN 'blocked' \
                       ELSE 'cancelled' END, \
            CASE g % 5 WHEN 0 THEN 'urgent' WHEN 1 THEN 'high' WHEN 2 THEN 'medium' \
                       WHEN 3 THEN 'low' ELSE 'none' END, \
            CASE WHEN g % 3 = 0 THEN NULL ELSE 'user' END, \
            CASE WHEN g % 3 = 0 THEN NULL \
                 ELSE ('b0e0c4a0-0000-4000-8000-' || lpad(to_hex(100 + g % 4), 12, '0'))::uuid END, \
            'user', \
            ('b0e0c4a0-0000-4000-8000-' || lpad(to_hex(200 + g % 2), 12, '0'))::uuid, \
            CASE WHEN g % 5 = 0 THEN NULL ELSE $2::uuid END, \
            g::float8, \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second'), \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + ((g + g % 97) * INTERVAL '1 second') \
     FROM generate_series(0, $3 - 1) AS g";

/// `$1` = workspace、`$2` = author、`$3` = 行数。
const SEED_COMMENTS: &str = "INSERT INTO comment \
     (id, issue_id, workspace_id, author_type, author_id, content, type, created_at, updated_at) \
     SELECT ('c0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            ('b0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            $1, 'user', $2, 'mc-bench comment ' || g, 'comment', \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second'), \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second') \
     FROM generate_series(0, $3 - 1) AS g";

/// `$1` = workspace、`$2` = recipient / actor、`$3` = 行数。
///
/// 全部 `archived_at IS NOT NULL` 且每个 issue 组只有一行 ⇒ 归档视图的 `newest` CTE 恰好
/// 命中 5000 个组；`read_at` 只写一半（`read` 布尔与之双写，与 `mc_repos::inbox` 的口径一致）。
const SEED_INBOX: &str = "INSERT INTO inbox_item \
     (id, workspace_id, recipient_type, recipient_id, type, severity, issue_id, title, body, \
      read, archived, read_at, archived_at, actor_type, actor_id, created_at) \
     SELECT ('d0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            $1, 'user', $2, 'issue_assigned', 'info', \
            ('b0000000-0000-4000-8000-' || lpad(to_hex(g), 12, '0'))::uuid, \
            'mc-bench inbox ' || g, 'mc-bench inbox body ' || g, \
            (g % 2 = 0), TRUE, \
            CASE WHEN g % 2 = 0 \
                 THEN TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second') \
                 ELSE NULL END, \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second'), \
            'user', $2, \
            TIMESTAMPTZ '2026-09-01 00:00:00+00' + (g * INTERVAL '1 second') \
     FROM generate_series(0, $3 - 1) AS g";

// --- 三条热点：bench 与证据共用同一份调用 ----------------------------------

/// 热点 ①：issue 列表的过滤条件（`limit=50` + 排序 + 可选过滤）。
pub fn issue_list_filter(ds: &Dataset, order: IssueOrderBy, filtered: bool) -> IssueFilter {
    let mut filter = IssueFilter::new(Id::from(ds.workspace_id));
    filter.limit = Some(PAGE_LIMIT);
    filter.order = order;
    filter.include_closed = true;
    if filtered {
        filter.statuses = Some(vec!["todo".to_string(), "in_progress".to_string()]);
        filter.priorities = Some(vec!["high".to_string(), "urgent".to_string()]);
    }
    filter
}

/// 热点 ①：跑一次列表（返回行数，喂 `black_box`）。
pub async fn run_issue_list(repo: &IssueRepo, filter: &IssueFilter) -> usize {
    repo.list(filter).await.expect("issue list").len()
}

/// 热点 ①：跑一次列表 + 总数（同一 WHERE 两条查询）。
pub async fn run_issue_list_with_total(repo: &IssueRepo, filter: &IssueFilter) -> (usize, i64) {
    let (rows, total) = repo
        .list_with_total(filter)
        .await
        .expect("issue list with total");
    (rows.len(), total)
}

/// 热点 ②：facets 请求（`all_five` = 全 5 维 + total；否则 Status/Priority 两维、不带 total）。
pub fn facets_query(ds: &Dataset, all_five: bool) -> TableFacetsQuery {
    TableFacetsQuery {
        workspace_id: Id::from(ds.workspace_id),
        filter: TableFilter::default(),
        facets: if all_five {
            vec![
                TableFacetKind::Status,
                TableFacetKind::Priority,
                TableFacetKind::Assignee,
                TableFacetKind::Creator,
                TableFacetKind::Project,
            ]
        } else {
            vec![TableFacetKind::Status, TableFacetKind::Priority]
        },
        include_total: all_five,
    }
}

/// 热点 ②：跑一次 facets 聚合。
pub async fn run_facets(repo: &IssueTableRepo, query: &TableFacetsQuery) -> TableFacetsPage {
    repo.table_facets(query).await.expect("table facets")
}

/// 热点 ③：inbox 归档游标的一页（`cursor = None` 是第一页）。
pub async fn run_inbox_page(
    repo: &InboxRepo,
    ds: &Dataset,
    cursor: Option<&ArchivedCursor>,
) -> ArchivedInboxPage {
    repo.list_archived_page(
        Id::from(ds.workspace_id),
        Id::from(ds.user_id),
        &ArchivedInboxFilter::default(),
        cursor,
        PAGE_LIMIT,
    )
    .await
    .expect("archived inbox page")
}

/// 翻第二页要用的游标：第一页最后一行的 `(created_at, id)`（行比较 keyset 翻页）。
pub fn cursor_after(page: &ArchivedInboxPage) -> Option<ArchivedCursor> {
    page.items.last().map(|row| ArchivedCursor {
        created_at: row.created_at,
        id: Id::from(row.id),
    })
}

// --- R10：语句缓存的可判用例 -----------------------------------------------

const R10_PROBE_SQL: &str = "SELECT COUNT(*)::bigint FROM issue WHERE workspace_id = $1";

/// R10 证据（落进 stdout 与报告）。
#[derive(Debug, Clone, Serialize)]
pub struct StatementCacheEvidence {
    /// 池的显式容量。
    pub capacity: usize,
    /// 正例：同一语句两次执行后 `pg_prepared_statements` 里该语句的行数（期望 `1`）；反例
    /// （`capacity = 0`）的同款计数（期望 `2`）。
    pub cached_rows: i64,
    /// 见 `cached_rows`。
    pub uncached_rows: i64,
}

/// 同一语句在同一连接上执行两次后，`pg_prepared_statements` 里**该语句文本**的行数。
///
/// 判据是「不再 `Parse`」：第二次执行若又发了一次 `Parse`，Postgres 会为它再建一个（自动命名的）
/// prepared statement ⇒ 行数变 2。用文本过滤而不是总数，是因为这条计数查询自己也会进那张视图。
async fn prepared_rows(conn: &mut sqlx::PgConnection, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::bigint FROM pg_prepared_statements WHERE statement = $1",
    )
    .bind(sql)
    .fetch_one(&mut *conn)
    .await
    .expect("cannot read pg_prepared_statements")
}

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
pub async fn assert_statement_cache_reuse(ds: &Dataset) -> StatementCacheEvidence {
    let cached = probe_once(&pool().await, ds.workspace_id).await;
    let uncached = probe_once(&pool_with_capacity(0).await, ds.workspace_id).await;
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

// --- 报告：sample.json → p50/p95/p99 → 阈值判定 → JSON ---------------------

/// criterion 的 `sample.json`（只取用得上的两个数组）。
#[derive(Debug, Deserialize)]
struct SampleJson {
    iters: Vec<f64>,
    times: Vec<f64>,
}

/// criterion 的 `estimates.json`（只取中位数做交叉校验）。
#[derive(Debug, Deserialize)]
struct EstimatesJson {
    median: PointEstimate,
}

#[derive(Debug, Deserialize)]
struct PointEstimate {
    point_estimate: f64,
}

/// 一个 case 的读数（毫秒）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseReport {
    /// criterion 的 `bench_function` 名。
    pub case: String,
    /// 逐样本单次迭代耗时（毫秒）的 p50。
    pub p50_ms: f64,
    /// p95（**判据用这个**）。
    pub p95_ms: f64,
    /// p99。
    pub p99_ms: f64,
    /// criterion 自己的中位数（交叉校验：应当与 `p50_ms` 接近）。
    pub criterion_median_ms: f64,
    /// 样本数。
    pub samples: usize,
    /// 绝对上界（毫秒）。
    pub budget_p95_ms: f64,
    /// 有基线时：基线 p95（毫秒）。
    #[serde(default)]
    pub baseline_p95_ms: Option<f64>,
    /// 有基线时：`p95_ms / baseline_p95_ms`。
    #[serde(default)]
    pub p95_ratio: Option<f64>,
}

/// criterion 的默认输出目录（`<target>/criterion`）。
pub fn criterion_dir() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir).join("criterion"),
        None => workspace_root().join("target").join("criterion"),
    }
}

/// 本 run 的逐 bench 报告目录（`<target>/mc-bench`）。
pub fn report_dir() -> PathBuf {
    criterion_dir()
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_path_buf)
        .join("mc-bench")
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| manifest.clone(), Path::to_path_buf)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// 线性插值百分位（`p ∈ [0,1]`；输入是**已排序**的逐样本迭代耗时）。
///
/// 纳秒计时与百分位都走 `f64`：`f64` 的 53 位尾数足以精确表示任何 < 2^53 ns（≈104 天）的计时。
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let last = sorted.len() - 1;
    let pos = p * last as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = pos - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// 读一个 case 的读数；criterion 没跑过 / 文件缺失 ⇒ panic（**不许**静默产出空报告）。
pub fn read_case(bench: &str, case: &str) -> CaseReport {
    let dir = criterion_dir().join(bench).join(case).join("new");
    let sample: SampleJson = read_json(&dir.join("sample.json")).unwrap_or_else(|| {
        panic!(
            "missing {} — run `cargo bench -p mc-bench` first, or check the criterion output dir",
            dir.join("sample.json").display()
        )
    });
    assert_eq!(
        sample.iters.len(),
        sample.times.len(),
        "criterion sample.json is malformed for {bench}/{case}"
    );
    // 逐样本 `times/iters` = 单次迭代纳秒（criterion 的 times 是**整样本**的总耗时）。
    let mut per_iter: Vec<f64> = sample
        .iters
        .iter()
        .zip(sample.times.iter())
        .map(|(iters, total)| total / iters)
        .collect();
    per_iter.sort_by(|a, b| a.partial_cmp(b).expect("NaN in criterion samples"));
    let median =
        read_json::<EstimatesJson>(&dir.join("estimates.json")).map(|e| e.median.point_estimate);
    CaseReport {
        case: case.to_string(),
        p50_ms: percentile(&per_iter, 0.50) / 1_000_000.0,
        p95_ms: percentile(&per_iter, 0.95) / 1_000_000.0,
        p99_ms: percentile(&per_iter, 0.99) / 1_000_000.0,
        criterion_median_ms: median.map_or(f64::NAN, |ns| ns / 1_000_000.0),
        samples: per_iter.len(),
        budget_p95_ms: p95_budget(bench, case),
        baseline_p95_ms: None,
        p95_ratio: None,
    }
}

/// 落进 `MC_BENCH_REPORT_OUT` 的整份报告（三个 bench 的合并结果，`benches.<name>.cases[]`）。
#[derive(Debug, Serialize, Deserialize)]
pub struct BenchReport {
    /// 格式版本。
    pub schema_version: u32,
    /// 工具名（写死，便于 reader 认）。
    pub tool: String,
    /// 数据集描述 / criterion 档位。
    pub dataset: serde_json::Value,
    /// 见 `dataset`。
    pub criterion: serde_json::Value,
    /// `{ "<bench>": { "cases": [CaseReport …] } }`。
    pub benches: serde_json::Value,
}

/// 从基线文件里取某个 case 的 p95（毫秒）；文件/键不存在 ⇒ `None`（不判、不红）。
fn baseline_p95(path: &str, bench: &str, case: &str) -> Option<f64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let report: BenchReport = serde_json::from_str(&raw).ok()?;
    for entry in report.benches.get(bench)?.get("cases")?.as_array()? {
        if entry.get("case")?.as_str()? == case {
            return entry.get("p95_ms")?.as_f64();
        }
    }
    None
}

fn write_atomic(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("cannot create the report directory");
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, body).expect("cannot write the report");
    std::fs::rename(&tmp, path).expect("cannot move the report into place");
}

/// 🔴 bench 的收尾：读读数 → 判绝对/相对阈值 → 写报告 →（可选）合并进基线文件。
///
/// 任一条判据不成立 ⇒ **panic**（`cargo bench -p mc-bench` 以非 0 退出），这样「跑通」与
/// 「在预算内」是同一件事。
pub fn finalize(bench: &str, cases: &[&str]) {
    let baseline_path = std::env::var("MC_BENCH_BASELINE").ok();
    let mut reports: Vec<CaseReport> = Vec::with_capacity(cases.len());
    let mut failures: Vec<String> = Vec::new();

    for case in cases {
        let mut report = read_case(bench, case);
        if let Some(path) = &baseline_path {
            report.baseline_p95_ms = baseline_p95(path, bench, case);
            if let Some(base) = report.baseline_p95_ms {
                let ratio = report.p95_ms / base;
                report.p95_ratio = Some(ratio);
                if ratio > RELATIVE_P95_SLACK {
                    failures.push(format!(
                        "{bench}/{case}: p95 {:.3}ms > {RELATIVE_P95_SLACK} x baseline {base:.3}ms \
                         (ratio {ratio:.3})",
                        report.p95_ms
                    ));
                }
            }
        }
        if report.p95_ms > report.budget_p95_ms {
            failures.push(format!(
                "{bench}/{case}: p95 {:.3}ms > absolute budget {:.3}ms",
                report.p95_ms, report.budget_p95_ms
            ));
        }
        reports.push(report);
    }

    println!("\n=== mc-bench report: {bench} ===");
    println!(
        "{:<32}{:>10}{:>10}{:>10}{:>10}{:>11}{:>10}",
        "case", "p50(ms)", "p95(ms)", "p99(ms)", "budget", "crit.med", "vs base"
    );
    for r in &reports {
        let ratio = r
            .p95_ratio
            .map_or_else(|| "-".to_string(), |x| format!("{x:.3}x"));
        let CaseReport {
            case,
            p50_ms,
            p95_ms,
            p99_ms,
            budget_p95_ms,
            criterion_median_ms,
            ..
        } = r;
        println!("{case:<32}{p50_ms:>10.3}{p95_ms:>10.3}{p99_ms:>10.3}{budget_p95_ms:>10.3}{criterion_median_ms:>11.3}{ratio:>10}");
    }

    let own = serde_json::json!({ "cases": reports });
    write_atomic(
        &report_dir().join(format!("{bench}.json")),
        &serde_json::to_string_pretty(&own).expect("cannot serialize the bench report"),
    );

    if let Ok(path) = std::env::var("MC_BENCH_REPORT_OUT") {
        let merged = merge_report(&path, bench, own);
        write_atomic(Path::new(&path), &merged);
        println!("merged into {path}");
    }

    assert!(
        failures.is_empty(),
        "mc-bench threshold failures:\n  {}",
        failures.join("\n  ")
    );
}

/// 把本 bench 的一段合并进 `path`（其余 bench 的段原样保留）。
fn merge_report(path: &str, bench: &str, own: serde_json::Value) -> String {
    let mut root: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(default_report_skeleton);
    if let Some(benches) = root
        .get_mut("benches")
        .and_then(serde_json::Value::as_object_mut)
    {
        benches.insert(bench.to_string(), own);
    }
    serde_json::to_string_pretty(&root).expect("cannot serialize the merged report")
}

fn default_report_skeleton() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "tool": "mc-bench",
        "dataset": Dataset::spec(),
        "criterion": {
            "warm_up_secs": WARM_UP_SECS,
            "measurement_secs": MEASUREMENT_SECS,
            "sample_size": SAMPLE_SIZE,
        },
        "statement_cache_capacity": STATEMENT_CACHE_CAPACITY,
        "relative_p95_slack": RELATIVE_P95_SLACK,
        "absolute_p95_budget_ms": ABSOLUTE_P95_BUDGETS,
        "benches": {},
    })
}
