//! 热点 ①：issue 列表（`limit=50` + 排序 + 过滤）与列表 + 总数。
//!
//! 三个 case 都在 `M10-6` 的报告里登记（见 `mc_bench::ISSUE_LIST_CASES` 与
//! `mc_bench::ABSOLUTE_P95_BUDGETS`）—— 缺任何一个 `report::finalize` 都会 panic。

mod common;

use std::hint::black_box;

use criterion::Criterion;
use mc_bench::{
    issue_list_filter, run_issue_list, run_issue_list_with_total, ISSUE_LIST_BENCH,
    ISSUE_LIST_CASES,
};
use mc_repos::issue::IssueOrderBy;

fn bench(c: &mut Criterion, ctx: &common::BenchContext) {
    let mut group = c.benchmark_group(ISSUE_LIST_BENCH);

    // case 1：默认排序（`updated_at DESC, number DESC`）的 `limit=50`，无过滤。
    let plain = issue_list_filter(&ctx.dataset, IssueOrderBy::UpdatedDesc, false);
    group.bench_function(ISSUE_LIST_CASES[0], |b| {
        b.to_async(&ctx.runtime)
            .iter(|| async { black_box(run_issue_list(&ctx.issues, &plain).await) });
    });

    // case 2：同一排序 + status/priority 两个过滤（`statuses` + `priorities`）。
    let filtered = issue_list_filter(&ctx.dataset, IssueOrderBy::UpdatedDesc, true);
    group.bench_function(ISSUE_LIST_CASES[1], |b| {
        b.to_async(&ctx.runtime)
            .iter(|| async { black_box(run_issue_list(&ctx.issues, &filtered).await) });
    });

    // case 3：列表 + `COUNT(*)`（同一个 WHERE 两条查询 —— 上游 `include_total`）。
    let with_total = issue_list_filter(&ctx.dataset, IssueOrderBy::CreatedDesc, false);
    group.bench_function(ISSUE_LIST_CASES[2], |b| {
        b.to_async(&ctx.runtime).iter(|| async {
            black_box(run_issue_list_with_total(&ctx.issues, &with_total).await)
        });
    });

    group.finish();
}

fn main() {
    let ctx = common::setup();
    let mut criterion = common::criterion();
    bench(&mut criterion, &ctx);
    criterion.final_summary();
    ctx.finish(ISSUE_LIST_BENCH, &ISSUE_LIST_CASES);
}
