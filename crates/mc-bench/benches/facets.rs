//! 热点 ②：issue table 的 facets 聚合（disjunctive 计数 + 可选 total）。
//!
//! 两个 case 都在 `M10-6` 的报告里登记（`mc_bench::FACETS_CASES` /
//! `mc_bench::ABSOLUTE_P95_BUDGETS`）：全 5 维 + total（`table_facets_all5_with_total`）、
//! Status/Priority 两维不带 total（`table_facets_status_priority`）。

mod common;

use std::hint::black_box;

use criterion::Criterion;
use mc_bench::{facets_query, run_facets, FACETS_BENCH, FACETS_CASES};

fn bench(c: &mut Criterion, ctx: &common::BenchContext) {
    let mut group = c.benchmark_group(FACETS_BENCH);

    // case 1：全 5 维（Status/Priority/Assignee/Creator/Project）+ `include_total`。
    let all_five = facets_query(&ctx.dataset, true);
    group.bench_function(FACETS_CASES[0], |b| {
        b.to_async(&ctx.runtime)
            .iter(|| async { black_box(run_facets(&ctx.tables, &all_five).await) });
    });

    // case 2：只算 Status/Priority 两维、不带 total（面板首屏的形态）。
    let two = facets_query(&ctx.dataset, false);
    group.bench_function(FACETS_CASES[1], |b| {
        b.to_async(&ctx.runtime)
            .iter(|| async { black_box(run_facets(&ctx.tables, &two).await) });
    });

    group.finish();
}

fn main() {
    let ctx = common::setup();
    let mut criterion = common::criterion();
    bench(&mut criterion, &ctx);
    criterion.final_summary();
    ctx.finish(FACETS_BENCH, &FACETS_CASES);
}
