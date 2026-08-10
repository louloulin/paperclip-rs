use chrono::{TimeZone, Timelike, Utc};
use pc_tool_runtime_metrics::{MetricHook, MetricHookEvent, NoopMetricHook, RecordingMetricHook};
use uuid::Uuid;
#[tokio::test]
async fn noop_ok() {
    let e = MetricHookEvent::AuditWriteFailureRecorded { company_id: Uuid::new_v4() };
    assert!(MetricHook::on_metric_event(&NoopMetricHook, e).await.is_ok());
}
#[tokio::test]
async fn recorder_captures_variants() {
    let h = RecordingMetricHook::default();
    let bucket = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let ev1 = MetricHookEvent::Incremented { company_id: Uuid::new_v4(), metric: "m".into(), bucket_start_at: bucket };
    let ev2 = MetricHookEvent::AuditWriteFailureRecorded { company_id: Uuid::new_v4() };
    MetricHook::on_metric_event(&h, ev1).await.unwrap();
    MetricHook::on_metric_event(&h, ev2).await.unwrap();
    assert_eq!(h.len(), 2);
    h.clear(); assert!(h.is_empty());
}
#[test]
fn tag_is_camel_case() {
    let v: serde_json::Value = serde_json::to_value(MetricHookEvent::AuditWriteFailureRecorded { company_id: Uuid::nil() }).unwrap();
    assert_eq!(v["type"], "auditWriteFailureRecorded");
}
#[test]
fn minute_bucket_helper_via_service() {
    use pc_tool_runtime_metrics::ToolRuntimeMetricsService;
    let at = Utc.with_ymd_and_hms(2026, 8, 10, 12, 30, 45).unwrap() + chrono::Duration::milliseconds(700);
    let bucket = ToolRuntimeMetricsService::minute_bucket(at);
    assert_eq!(bucket.second(), 0);
    assert_eq!(bucket.nanosecond(), 0);
}
