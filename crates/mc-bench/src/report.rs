//! 报告面 —— `sample.json` → `p50/p95/p99` → 阈值判定 → JSON。
//!
//! 读数取自 criterion 写的 `target/criterion/<bench>/<case>/new/sample.json`（逐样本 `times/iters`
//! = 单次迭代纳秒），用**线性插值**百分位；criterion 的 `estimates.json::median` 一并落进报告做
//! **交叉校验**。任一条判据不成立 ⇒ `panic`（`cargo bench -p mc-bench` 非 0 退出）⇒「跑通」与
//! 「在预算内」是同一件事。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{
    p95_budget, BASE_SHA_ENV, MEASUREMENT_SECS, RELATIVE_P95_SLACK, SAMPLE_SIZE, WARM_UP_SECS,
};
use crate::conn::StatementCacheEvidence;
use crate::dataset::Dataset;

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
#[allow(clippy::cast_precision_loss)]
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
    let estimates: EstimatesJson = read_json(&dir.join("estimates.json")).unwrap_or_else(|| {
        panic!(
            "missing {} — criterion did not write the estimates for {bench}/{case}",
            dir.join("estimates.json").display()
        )
    });
    // 逐样本 `times/iters` = 单次迭代纳秒（criterion 的 times 是**整样本**的总耗时）。
    let mut per_iter: Vec<f64> = sample
        .iters
        .iter()
        .zip(sample.times.iter())
        .map(|(iters, total)| total / iters)
        .collect();
    per_iter.sort_by(|a, b| a.partial_cmp(b).expect("NaN in criterion samples"));
    CaseReport {
        case: case.to_string(),
        p50_ms: percentile(&per_iter, 0.50) / 1_000_000.0,
        p95_ms: percentile(&per_iter, 0.95) / 1_000_000.0,
        p99_ms: percentile(&per_iter, 0.99) / 1_000_000.0,
        criterion_median_ms: estimates.median.point_estimate / 1_000_000.0,
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

/// 从基线文件里取某个 case 的 p95（毫秒）。
///
/// * 文件**不存在** ⇒ `None`（没给基线 ⇒ 不判相对阈值，但绝对预算照判）；
/// * 文件存在但读不动 / 缺这个 bench / 缺这个 case ⇒ **panic**（不许静默跳过：基线给歪了会让
///   相对阈值整条失效，而报告仍是「绿」—— 正是本仓两次「绿是空跑」的形态）。
fn baseline_p95(path: &str, bench: &str, case: &str) -> Option<f64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let report: BenchReport = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("MC_BENCH_BASELINE {path} is not a valid mc-bench report: {e}"));
    let baseline_cases = report
        .benches
        .get(bench)
        .and_then(|entry| entry.get("cases"))
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("MC_BENCH_BASELINE {path} has no bench '{bench}'"));
    for entry in baseline_cases {
        if entry.get("case").and_then(serde_json::Value::as_str) == Some(case) {
            return Some(
                entry
                    .get("p95_ms")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or_else(|| {
                        panic!(
                            "MC_BENCH_BASELINE {path} has a non-numeric p95_ms for {bench}/{case}"
                        )
                    }),
            );
        }
    }
    panic!("MC_BENCH_BASELINE {path} has no case '{bench}/{case}'")
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
/// 「在预算内」是同一件事。`r10` 由 harness 从 [`crate::assert_statement_cache_reuse`] 传入并落盘。
pub fn finalize(bench: &str, cases: &[&str], r10: &StatementCacheEvidence) {
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
        println!(
            "{:<32}{:>10.3}{:>10.3}{:>10.3}{:>10.3}{:>11.3}{:>10}",
            r.case, r.p50_ms, r.p95_ms, r.p99_ms, r.budget_p95_ms, r.criterion_median_ms, ratio
        );
    }

    let own = serde_json::json!({ "cases": reports });
    write_atomic(
        &report_dir().join(format!("{bench}.json")),
        &serde_json::to_string_pretty(&own).expect("cannot serialize the bench report"),
    );

    if let Ok(path) = std::env::var("MC_BENCH_REPORT_OUT") {
        let merged = merge_report(&path, bench, &own, r10);
        write_atomic(Path::new(&path), &merged);
        println!("merged into {path}");
    }

    assert!(
        failures.is_empty(),
        "mc-bench threshold failures:\n  {}",
        failures.join("\n  ")
    );
}

/// 把本 bench 的一段合并进 `path`（其余 bench 的段、以及 `r10` / `base_sha` 原样保留）。
fn merge_report(
    path: &str,
    bench: &str,
    own: &serde_json::Value,
    r10: &StatementCacheEvidence,
) -> String {
    let mut root: serde_json::Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(default_report_skeleton);
    if let Some(benches) = root
        .get_mut("benches")
        .and_then(serde_json::Value::as_object_mut)
    {
        benches.insert(bench.to_string(), own.clone());
    }
    if let Some(obj) = root.as_object_mut() {
        obj.insert("r10".to_string(), serde_json::json!(r10));
        obj.insert(
            "base_sha".to_string(),
            std::env::var(BASE_SHA_ENV).map_or(serde_json::Value::Null, serde_json::Value::String),
        );
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
        "statement_cache_capacity": crate::config::STATEMENT_CACHE_CAPACITY,
        "relative_p95_slack": RELATIVE_P95_SLACK,
        "absolute_p95_budget_ms": crate::config::ABSOLUTE_P95_BUDGETS,
        "benches": {},
    })
}
