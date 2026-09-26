//! 热点 ③：inbox 归档视图的 keyset 游标翻页（`archived/page`）。
//!
//! 两个 case 都在 `M10-6` 的报告里登记（`mc_bench::INBOX_CURSOR_CASES` /
//! `mc_bench::ABSOLUTE_P95_BUDGETS`）：第一页（`cursor = None`）、第二页（游标 = 第一页最后一行的
//! `(created_at, id)`）。游标在**测量前**从同一份数据上算好，不把第一页的开销算进第二页的读数。

mod common;

use std::hint::black_box;

use criterion::Criterion;
use mc_bench::{cursor_after, run_inbox_page, INBOX_CURSOR_BENCH, INBOX_CURSOR_CASES};

fn bench(c: &mut Criterion, ctx: &common::BenchContext) {
    // 第二页的游标：第一页的最后一行（`(created_at, id)` 行比较 keyset 翻页）。
    let first_page = ctx
        .runtime
        .block_on(run_inbox_page(&ctx.inbox, &ctx.dataset, None));
    assert!(
        !first_page.items.is_empty(),
        "the archived inbox page 1 is empty — the bench dataset is not seeded"
    );
    let cursor = cursor_after(&first_page).expect("page 1 has rows, so it has a last row");

    let mut group = c.benchmark_group(INBOX_CURSOR_BENCH);

    group.bench_function(INBOX_CURSOR_CASES[0], |b| {
        b.to_async(&ctx.runtime)
            .iter(|| async { black_box(run_inbox_page(&ctx.inbox, &ctx.dataset, None).await) });
    });

    group.bench_function(INBOX_CURSOR_CASES[1], |b| {
        b.to_async(&ctx.runtime).iter(|| async {
            black_box(run_inbox_page(&ctx.inbox, &ctx.dataset, Some(&cursor)).await)
        });
    });

    group.finish();
}

fn main() {
    let ctx = common::setup();
    let mut criterion = common::criterion();
    bench(&mut criterion, &ctx);
    criterion.final_summary();
    ctx.finish(INBOX_CURSOR_BENCH, &INBOX_CURSOR_CASES);
}
