//! 口径常量与阈值表 —— **判据只有一处**。
//!
//! 这里定义的每一条都是 `docs/64-M10-PLAN.md` §2.5 / §9.6 要求「可判定、禁『同量级』这类
//! 措辞」的落点：criterion 档位、固定数据集规模、三条热点各自的 case 名、相对/绝对阈值。
//! bench 主体、`benches/common` 的 harness、报告判定都从本模块取值，**不允许**就地另写一份。

use serde::Serialize;
use uuid::Uuid;

/// bench 需要的库 URL 环境变量名（与仓内其它真库用例同款）。
pub const DATABASE_URL_ENV: &str = "MULTICA_TEST_DATABASE_URL";

/// 🔴 R10：连接池的语句缓存容量，**显式**写入每一个连接（`sqlx` 缺省也是 100，但不许靠缺省）。
pub const STATEMENT_CACHE_CAPACITY: usize = 128;

/// 池大小（无并发 bench，够用即可）。
pub const MAX_CONNECTIONS: u32 = 8;

/// criterion 档位：warm-up 3s / measurement 10s / 100 样本（`docs/64` §2.5）。
pub const WARM_UP_SECS: u64 = 3;
/// 见 [`WARM_UP_SECS`]。
pub const MEASUREMENT_SECS: u64 = 10;
/// 见 [`WARM_UP_SECS`]。
pub const SAMPLE_SIZE: usize = 100;

/// 固定数据集规模：10000 issue / 5000 comment / 5000 归档 inbox item。
pub const ISSUES: usize = 10_000;
/// 见 [`ISSUES`]。
pub const COMMENTS: usize = 5_000;
/// 见 [`ISSUES`]（每个 issue 组一行 ⇒ 5000 个组）。
pub const INBOX_ITEMS: usize = 5_000;

/// 列表 / 翻页的页大小（`docs/64` §2.5 的 `limit=50`）。
pub const PAGE_LIMIT: i64 = 50;

/// 数据集：workspace / user（收件人与 comment 作者）/ 唯一的 project。
///
/// 全是**确定性** UUID：换台机器播种出同一份数据。
pub const WORKSPACE_ID: Uuid = Uuid::from_u128(0xb0e0_c4a0_0000_4000_8000_0000_0000_a001);
/// 见 [`WORKSPACE_ID`]。
pub const USER_ID: Uuid = Uuid::from_u128(0xb0e0_c4a0_0000_4000_8000_0000_0000_a002);
/// 见 [`WORKSPACE_ID`]。
pub const PROJECT_ID: Uuid = Uuid::from_u128(0xb0e0_c4a0_0000_4000_8000_0000_0000_a003);

/// 三个 bench target 名（= `benches/<name>.rs`，也是 criterion 的 group 名）。
pub const ISSUE_LIST_BENCH: &str = "issue_list";
/// 见 [`ISSUE_LIST_BENCH`]。
pub const FACETS_BENCH: &str = "facets";
/// 见 [`ISSUE_LIST_BENCH`]。
pub const INBOX_CURSOR_BENCH: &str = "inbox_cursor";

/// 各 bench 的 case 清单（[`crate::finalize`] 用它反查预算 —— **没登记的 case 直接判红**）。
pub const ISSUE_LIST_CASES: [&str; 3] = [
    "list_limit50_updated_desc",
    "list_limit50_filtered",
    "list_with_total_limit50",
];
/// 见 [`ISSUE_LIST_CASES`]。
pub const FACETS_CASES: [&str; 2] = [
    "table_facets_all5_with_total",
    "table_facets_status_priority",
];
/// 见 [`ISSUE_LIST_CASES`]。
pub const INBOX_CURSOR_CASES: [&str; 2] = ["archived_page_first", "archived_page_cursor"];

/// 相对阈值：`p95(本次) ≤ RELATIVE_P95_SLACK × p95(基线)`。
pub const RELATIVE_P95_SLACK: f64 = 1.25;

/// 报告里记录「这份读数是在哪个 base sha 上跑的」的环境变量（可选；不设则写 `null`）。
pub const BASE_SHA_ENV: &str = "MC_BENCH_BASE_SHA";

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

/// 🔴 绝对上界（**M10-6 起手实测后修订**，`docs/32` §41.3 是权威登记处）。
///
/// `docs/64` §2.5 写的第一版估值（issue 5 / facets 20 / inbox 2 ms）**在本仓不成立**：它没有
/// 从「10000 issue / 5000 comment / 5000 归档 inbox item + 本仓的 schema」推出来。起手实测（base
/// 的生产代码，同一 workdir/同一 PG）p95：`issue_list` 最大 **11.018** / `facets` 最大 **17.417** /
/// `inbox_cursor` 最大 **13.568** ms。
///
/// 修订规则（**判据化，不许手调**）：`预算 = ceil(1.5 × 该 bench 实测 p95 的最大值)` ⇒
/// `issue_list 11.018×1.5=16.53→17.0`、`facets 17.417×1.5=26.13→27.0`、
/// `inbox_cursor 13.568×1.5=20.35→21.0`。**1.5 倍**对应「给 p95 自身的跑动噪声留余量」；
/// 真正的**回归**闸门是相对阈值（[`RELATIVE_P95_SLACK`] = 1.25× 冻结基线），绝对上界只档
/// 「没人更新基线却把某一维做坏了 ≥1.4 倍」这种量级的塌方。
pub const ABSOLUTE_P95_BUDGETS: [P95Budget; 7] = [
    P95Budget {
        bench: ISSUE_LIST_BENCH,
        case: "list_limit50_updated_desc",
        p95_ms: 17.0,
    },
    P95Budget {
        bench: ISSUE_LIST_BENCH,
        case: "list_limit50_filtered",
        p95_ms: 17.0,
    },
    P95Budget {
        bench: ISSUE_LIST_BENCH,
        case: "list_with_total_limit50",
        p95_ms: 17.0,
    },
    P95Budget {
        bench: FACETS_BENCH,
        case: "table_facets_all5_with_total",
        p95_ms: 27.0,
    },
    P95Budget {
        bench: FACETS_BENCH,
        case: "table_facets_status_priority",
        p95_ms: 27.0,
    },
    P95Budget {
        bench: INBOX_CURSOR_BENCH,
        case: "archived_page_first",
        p95_ms: 21.0,
    },
    P95Budget {
        bench: INBOX_CURSOR_BENCH,
        case: "archived_page_cursor",
        p95_ms: 21.0,
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
