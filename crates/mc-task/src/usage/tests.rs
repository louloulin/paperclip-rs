//! `usage` 的测试。

use super::*;
use crate::retry::FailureReason;

/// 确定性 id（`Id` 的 UUID 是公开字段，方便测试造可比较的值）。
fn id(n: u8) -> Id {
    Id(uuid::Uuid::from_u128(u128::from(n)))
}

fn t(unix: i64) -> Timestamp {
    Timestamp::from_unix(unix)
}

fn report(provider: &str, model: &str, input: i64, cost: i64) -> UsageReport {
    UsageReport {
        provider: provider.to_owned(),
        model: model.to_owned(),
        input_tokens: input,
        output_tokens: input * 2,
        cache_read_tokens: 7,
        cache_write_tokens: 8,
        cost_usd_ticks: cost,
    }
}

fn row(task: u8, provider: &str, input: i64, cost: Option<i64>) -> TaskUsageRow {
    TaskUsageRow {
        task_id: id(task),
        provider: provider.to_owned(),
        model: "gpt-5".to_owned(),
        input_tokens: input,
        output_tokens: input * 2,
        cache_read_tokens: 10,
        cache_write_tokens: 20,
        cost_usd_ticks: cost,
        created_at: t(1_000),
        updated_at: Some(t(1_000)),
    }
}

#[test]
fn provider_is_trimmed_and_lowercased() {
    assert_eq!(normalize_provider("  Grok  "), "grok");
    assert_eq!(normalize_provider("ANTHROPIC"), "anthropic");
    assert_eq!(normalize_provider(""), "");
}

#[test]
fn provider_resolution_falls_back_to_the_runtime() {
    assert_eq!(
        resolve_provider(" OpenAI ", "grok"),
        ("openai".to_owned(), ProviderSource::Reported)
    );
    assert_eq!(
        resolve_provider("", "  GROK "),
        ("grok".to_owned(), ProviderSource::RuntimeFallback)
    );
    assert_eq!(
        resolve_provider("", ""),
        (String::new(), ProviderSource::Missing)
    );
}

#[test]
fn only_a_positive_reported_cost_is_authoritative() {
    assert_eq!(authoritative_cost_ticks(1), Some(1));
    assert_eq!(
        authoritative_cost_ticks(COST_TICKS_PER_USD),
        Some(10_000_000_000)
    );
    // 0 是「daemon 不知道成本」，不是「花了 0 美元」。
    assert_eq!(authoritative_cost_ticks(0), None);
    assert_eq!(authoritative_cost_ticks(-5), None);
}

#[test]
fn tick_formatting_is_fixed_point_and_exact() {
    assert_eq!(cost_ticks_to_usd_string(0), "0.0000000000");
    assert_eq!(cost_ticks_to_usd_string(1), "0.0000000001");
    assert_eq!(cost_ticks_to_usd_string(COST_TICKS_PER_USD), "1.0000000000");
    assert_eq!(cost_ticks_to_usd_string(12_345_678_901), "1.2345678901");
    assert_eq!(
        cost_ticks_to_usd_string(-COST_TICKS_PER_USD),
        "-1.0000000000"
    );
}

#[test]
fn prompt_cache_ratio_uses_the_input_side_total() {
    assert_eq!(prompt_cache_read_ratio(0, 0, 0), None);
    assert_eq!(prompt_cache_read_ratio(0, 0, -3), None);
    assert_eq!(prompt_cache_read_ratio(1, 1, 2), Some(0.25));
    assert_eq!(prompt_cache_read_ratio(100, 0, 0), Some(0.0));
}

#[test]
fn usage_report_serde_defaults_match_go_zero_values() {
    let parsed: UsageReport = serde_json::from_str(r#"{"model":"auto"}"#).expect("解析");
    assert_eq!(parsed.model, "auto");
    assert_eq!(parsed.provider, "");
    assert_eq!(parsed.input_tokens, 0);
    assert_eq!(parsed.cost_usd_ticks, 0);
    // 老 daemon 的载荷：一个字段都没有。
    let empty: UsageReport = serde_json::from_str("{}").expect("解析");
    assert_eq!(empty, UsageReport::default());
}

#[test]
fn upsert_overwrites_instead_of_accumulating() {
    let first = report("OpenAI", "gpt-5", 100, 500);
    let mut stored = TaskUsageRow::from_report(id(1), &first, "openai", t(1_000));
    assert_eq!(stored.input_tokens, 100);
    assert_eq!(stored.cost_usd_ticks, Some(500));
    assert_eq!(stored.updated_at, Some(t(1_000)));

    let corrected = report("openai", "gpt-5", 30, 0);
    stored.overwrite_with(&corrected, "openai", t(2_000));
    assert_eq!(stored.input_tokens, 30, "覆盖而不是 100+30");
    assert_eq!(stored.output_tokens, 60);
    assert_eq!(stored.cost_usd_ticks, None, "0 覆盖成 NULL");
    assert_eq!(stored.created_at, t(1_000), "created_at 不被 DO UPDATE 碰");
    assert_eq!(stored.updated_at, Some(t(2_000)), "updated_at 刷新是脏标记");
}

#[test]
fn the_natural_key_is_task_provider_model() {
    let row = TaskUsageRow::from_report(id(7), &report("GROK", "grok-4", 1, 1), "grok", t(1_000));
    assert_eq!(
        row.key(),
        UsageKey::new(id(7), "grok", "grok-4"),
        "provider 规范化后与显式构造一致"
    );
    assert_ne!(row.key(), UsageKey::new(id(7), "grok", "grok-4-mini"));
    assert_ne!(row.key(), UsageKey::new(id(8), "grok", "grok-4"));
}

#[test]
fn issue_summary_splits_costed_and_uncosted_tokens() {
    let rows = vec![row(1, "grok", 100, Some(1_000)), row(2, "grok", 50, None)];
    let runs = vec![
        RunCoverage {
            task_id: id(1),
            status: TaskStatus::Completed,
            started_at: Some(t(10)),
            completed_at: Some(t(20)),
        },
        RunCoverage {
            task_id: id(2),
            status: TaskStatus::Failed,
            started_at: Some(t(10)),
            completed_at: Some(t(40)),
        },
    ];
    let summary = summarize_issue_usage(&rows, &runs);
    assert_eq!(summary.totals.input_tokens, 150);
    assert_eq!(summary.totals.output_tokens, 300);
    assert_eq!(summary.totals.cache_read_tokens, 20);
    assert_eq!(
        summary.totals.cost_usd_ticks, 1_000,
        "只算 provider 定价过的行"
    );
    assert_eq!(summary.totals.uncosted_input_tokens, 50);
    assert_eq!(summary.totals.uncosted_output_tokens, 100);
    assert_eq!(summary.totals.uncosted_cache_read_tokens, 10);
    assert_eq!(summary.totals.uncosted_cache_write_tokens, 20);
    assert_eq!(summary.task_count, 2);
    assert_eq!(summary.terminal_task_count, 2);
    assert_eq!(summary.metered_task_count, 2);
    assert_eq!(summary.unreported_task_count, 0);
}

#[test]
fn issue_summary_counts_distinct_tasks_and_unreported_runs() {
    // 同一任务两个 model ⇒ task_count 仍是 1。
    let rows = vec![row(1, "grok", 10, None), row(1, "grok", 20, None)];
    let runs = vec![
        RunCoverage {
            task_id: id(1),
            status: TaskStatus::Completed,
            started_at: Some(t(10)),
            completed_at: Some(t(20)),
        },
        // 跑过但一条 usage 都没报。
        RunCoverage {
            task_id: id(9),
            status: TaskStatus::Completed,
            started_at: Some(t(10)),
            completed_at: Some(t(20)),
        },
        // 从未开跑：不计入终态运行。
        RunCoverage {
            task_id: id(8),
            status: TaskStatus::Cancelled,
            started_at: None,
            completed_at: Some(t(20)),
        },
    ];
    let summary = summarize_issue_usage(&rows, &runs);
    assert_eq!(summary.task_count, 1);
    assert_eq!(summary.terminal_task_count, 2);
    assert_eq!(summary.metered_task_count, 1);
    assert_eq!(summary.unreported_task_count, 1);
}

#[test]
fn run_coverage_requires_a_terminal_status_and_both_timestamps() {
    let base = RunCoverage {
        task_id: id(1),
        status: TaskStatus::Completed,
        started_at: Some(t(10)),
        completed_at: Some(t(20)),
    };
    assert!(base.counts_as_run());
    assert_eq!(base.duration_secs(), Some(10));

    let no_start = RunCoverage {
        started_at: None,
        ..base.clone()
    };
    assert!(!no_start.counts_as_run());
    assert_eq!(no_start.duration_secs(), None);

    let no_end = RunCoverage {
        completed_at: None,
        ..base.clone()
    };
    assert!(!no_end.counts_as_run());

    for status in [
        TaskStatus::Queued,
        TaskStatus::Dispatched,
        TaskStatus::Running,
    ] {
        let in_flight = RunCoverage {
            status,
            ..base.clone()
        };
        assert!(!in_flight.counts_as_run(), "{status} 未终结");
    }
}

#[test]
fn run_time_summary_includes_cancelled_and_failed() {
    let rows = vec![row(1, "grok", 10, None)];
    let runs = vec![
        RunCoverage {
            task_id: id(1),
            status: TaskStatus::Completed,
            started_at: Some(t(100)),
            completed_at: Some(t(160)),
        },
        RunCoverage {
            task_id: id(2),
            status: TaskStatus::Failed,
            started_at: Some(t(100)),
            completed_at: Some(t(140)),
        },
        RunCoverage {
            task_id: id(3),
            status: TaskStatus::Cancelled,
            started_at: Some(t(100)),
            completed_at: Some(t(190)),
        },
    ];
    let summary = summarize_run_time(&runs, &rows);
    assert_eq!(summary.total_seconds, 60 + 40 + 90);
    assert_eq!(summary.task_count, 3);
    assert_eq!(summary.metered_task_count, 1);
    assert_eq!(summary.failed_count, 1);
    assert_eq!(summary.cancelled_count, 1);
}

#[test]
fn failure_buckets_mirror_the_dashboard_case() {
    assert_eq!(
        classify_failure_bucket(TaskStatus::Completed, Some("agent_error")),
        FailureBucket::NonFailure,
        "非 failed 一律空桶"
    );
    assert_eq!(
        classify_failure_bucket(TaskStatus::Cancelled, Some("timeout")),
        FailureBucket::NonFailure,
        "该查询的 WHERE 本来也不含 cancelled"
    );
    assert_eq!(
        classify_failure_bucket(TaskStatus::Failed, Some("timeout")),
        FailureBucket::Reason("timeout".to_owned())
    );
    assert_eq!(
        classify_failure_bucket(TaskStatus::Failed, Some("  ")),
        FailureBucket::Unclassified
    );
    assert_eq!(
        classify_failure_bucket(TaskStatus::Failed, None),
        FailureBucket::Unclassified
    );
    // 认不出的文本照样自成一桶（上游不校验），但能反解出强类型时给出强类型。
    let custom = classify_failure_bucket(TaskStatus::Failed, Some("daemon_自造原因"));
    assert_eq!(custom.as_str(), "daemon_自造原因");
    assert_eq!(custom.canonical(), None);
    assert_eq!(
        classify_failure_bucket(TaskStatus::Failed, Some("timeout")).canonical(),
        Some(FailureReason::Timeout)
    );
    assert_eq!(FailureBucket::NonFailure.as_str(), "");
    assert_eq!(FailureBucket::Unclassified.as_str(), "unclassified");
}

#[test]
fn hour_bucket_truncates_in_utc() {
    assert_eq!(hour_bucket(t(0)), t(0));
    assert_eq!(hour_bucket(t(1)), t(0));
    assert_eq!(hour_bucket(t(SECS_PER_HOUR - 1)), t(0));
    assert_eq!(hour_bucket(t(SECS_PER_HOUR)), t(SECS_PER_HOUR));
    // 1970 之前也要向下取整（rem_euclid 不是截断除）。
    assert_eq!(hour_bucket(t(-1)), t(-SECS_PER_HOUR));
    assert_eq!(hour_bucket(t(-SECS_PER_HOUR)), t(-SECS_PER_HOUR));
}

fn bucket(project: Option<u8>, provider: &str) -> HourlyBucketKey {
    HourlyBucketKey {
        bucket_hour: hour_bucket(t(7_200)),
        workspace_id: id(1),
        runtime_id: id(2),
        agent_id: id(3),
        project_id: project.map(id),
        provider: provider.to_owned(),
        model: "gpt-5".to_owned(),
    }
}

fn source(key: HourlyBucketKey, task: u8, input: i64, cost: Option<i64>) -> HourlySourceRow {
    HourlySourceRow {
        key,
        task_id: id(task),
        input_tokens: input,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd_ticks: cost,
    }
}

#[test]
fn hourly_fold_splits_costed_and_uncosted() {
    let key = bucket(Some(9), "grok");
    let rows = vec![
        source(key.clone(), 1, 100, Some(1_000)),
        source(key.clone(), 2, 50, None),
    ];
    let folded = fold_hourly(&rows);
    assert_eq!(folded.len(), 1);
    let agg = &folded[0];
    assert_eq!(agg.input_tokens, 150);
    assert_eq!(agg.cost_usd_ticks, 1_000, "SUM 跳过 NULL");
    assert_eq!(agg.uncosted_input_tokens, 50);
    assert_eq!(agg.task_count, 2, "两个不同 task");
    assert_eq!(agg.event_count, 2, "两条明细");
}

#[test]
fn hourly_fold_groups_by_the_full_key_including_null_project() {
    let rows = vec![
        source(bucket(Some(9), "grok"), 1, 10, None),
        source(bucket(None, "grok"), 2, 20, None),
        source(bucket(Some(9), "anthropic"), 3, 30, None),
        source(bucket(Some(9), "grok"), 1, 40, None),
    ];
    let folded = fold_hourly(&rows);
    assert_eq!(folded.len(), 3, "project/provider 不同的桶不能合并");
    let first = &folded[0];
    assert_eq!(first.key.project_id, Some(id(9)));
    assert_eq!(first.key.provider, "grok");
    assert_eq!(first.event_count, 2);
    assert_eq!(first.task_count, 1, "同一 task 的两条明细算一次");
    assert_eq!(first.input_tokens, 50);
    assert_eq!(folded[1].key.project_id, None, "NULL 自成一桶");
    assert_eq!(folded[2].key.provider, "anthropic");
    assert!(fold_hourly(&[]).is_empty(), "没有明细 ⇒ 没有桶（脏键要删）");
}
