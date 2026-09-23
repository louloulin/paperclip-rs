//! `mc-repos::wakeup` 的纯 Rust 单测（不连库）。
//!
//! 真库 e2e 在 `crates/mc-http/tests/issues/wakeups.rs`（`--ignored` + `MULTICA_TEST_DATABASE_URL`）。
//!
//! 这里守的两件事都**不需要**数据库，但一旦漂移就会静默破坏契约：
//!
//! 1. **响应键名 = 列名**：上游 sqlc 结构直接序列化成 JSON，键名就是列名（`snake_case`）。
//!    本仓手写 `FromRow` + `Serialize` ⇒ 用「键集合逐字相等」断言钉住，防止有人顺手改字段名。
//! 2. **容量错误按约束名识别**：`530` 的触发器抛 `23514` +
//!    `issue_wakeup_active_limit`；只要有人把判断改成匹配错误文案，这里会红。

use std::borrow::Cow;

use serde_json::json;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// 假 `DatabaseError`：让「按约束名 / SQLSTATE 识别」的判定可以在不连库的情况下测
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FakeDbError {
    code: Option<&'static str>,
    constraint: Option<&'static str>,
}

impl std::fmt::Display for FakeDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fake db error")
    }
}

impl std::error::Error for FakeDbError {}

impl sqlx::error::DatabaseError for FakeDbError {
    fn kind(&self) -> sqlx::error::ErrorKind {
        sqlx::error::ErrorKind::Other
    }

    fn code(&self) -> Option<Cow<'_, str>> {
        self.code.map(Cow::Borrowed)
    }

    fn constraint(&self) -> Option<&str> {
        self.constraint
    }

    fn message(&self) -> &str {
        "fake db error"
    }

    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        self
    }

    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self
    }
}

fn db_error(code: &'static str, constraint: Option<&'static str>) -> sqlx::Error {
    sqlx::Error::Database(Box::new(FakeDbError { code: Some(code), constraint }))
}

// ---------------------------------------------------------------------------
// 错误识别
// ---------------------------------------------------------------------------

#[test]
fn capacity_violation_is_identified_by_constraint_name() {
    let err = db_error("23514", Some("issue_wakeup_active_limit"));
    assert!(is_active_limit_violation(&err));
    // 同类 SQLSTATE 但换个约束名（例如同一张表上的其它 CHECK）不算容量超限。
    assert!(!is_active_limit_violation(&db_error("23514", Some("issue_wakeup_kind_check"))));
    // 约束名对但 SQLSTATE 不是 CHECK 违规 —— 仍按约束名认（上游同口径：只看 ConstraintName）。
    assert!(is_active_limit_violation(&db_error("23503", Some("issue_wakeup_active_limit"))));
    assert!(!is_active_limit_violation(&db_error("23514", None)));
    assert!(!is_active_limit_violation(&sqlx::Error::RowNotFound));
}

#[test]
fn source_busy_is_55p03_from_nowait_locks() {
    assert!(is_source_busy(&db_error("55P03", None)));
    assert!(!is_source_busy(&db_error("55P02", None)));
    assert!(!is_source_busy(&sqlx::Error::RowNotFound));
}

#[test]
fn unique_violation_reports_the_index_name() {
    let err = db_error("23505", Some("issue_wakeup_receipt_key_idx"));
    assert!(is_unique_violation(&err));
    assert_eq!(
        unique_violation_constraint(&err),
        Some("issue_wakeup_receipt_key_idx")
    );
    // 其它唯一冲突必须让 `capture_issue_wakeup` 重新抛（不能被当成重复事件吞掉）。
    let other = db_error("23505", Some("issue_wakeup_pkey"));
    assert!(is_unique_violation(&other));
    assert_eq!(unique_violation_constraint(&other), Some("issue_wakeup_pkey"));
    // 非唯一冲突时不给约束名（调用方据此区分「重复事件」与「真冲突」）。
    assert_eq!(unique_violation_constraint(&db_error("23514", None)), None);
}

// ---------------------------------------------------------------------------
// 响应键集合 = 列名集合
// ---------------------------------------------------------------------------

fn keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn sorted(mut v: Vec<&str>) -> Vec<String> {
    v.sort_unstable();
    v.into_iter().map(str::to_string).collect()
}

#[test]
fn wakeup_row_serialises_with_column_names() {
    let row = WakeupRow {
        id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        issue_id: Uuid::nil(),
        agent_id: Uuid::nil(),
        created_by: Uuid::nil(),
        source_task_id: None,
        parent_comment_id: None,
        instruction: "Look at the failing check".into(),
        kind: "event".into(),
        mode: "continuous".into(),
        event_types: vec!["comment.created".into()],
        filter_actor_type: Some("member".into()),
        filter_actor_id: None,
        filter_agent_id: None,
        filter_task_id: None,
        interval_seconds: None,
        cron_expression: None,
        timezone: "UTC".into(),
        next_fire_at: None,
        enabled: true,
        disabled_at: None,
        revision: 1,
        last_task_id: None,
        last_error: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    assert_eq!(
        keys(&serde_json::to_value(&row).expect("serialize")),
        sorted(vec![
            "id",
            "workspace_id",
            "issue_id",
            "agent_id",
            "created_by",
            "source_task_id",
            "parent_comment_id",
            "instruction",
            "kind",
            "mode",
            "event_types",
            "filter_actor_type",
            "filter_actor_id",
            "filter_agent_id",
            "filter_task_id",
            "interval_seconds",
            "cron_expression",
            "timezone",
            "next_fire_at",
            "enabled",
            "disabled_at",
            "revision",
            "last_task_id",
            "last_error",
            "created_at",
            "updated_at",
        ])
    );
    assert!(row.is_event());
    assert!(!row.is_once());
}

#[test]
fn issue_wakeup_view_serialises_with_column_names() {
    let view = IssueWakeupView {
        id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        issue_id: Uuid::nil(),
        agent_id: Uuid::nil(),
        created_by: Uuid::nil(),
        source_task_id: None,
        parent_comment_id: None,
        instruction: "x".into(),
        kind: "every".into(),
        mode: "continuous".into(),
        event_types: vec![],
        filter_actor_type: None,
        filter_actor_id: None,
        filter_actor_name: String::new(),
        filter_agent_id: None,
        filter_task_id: None,
        interval_seconds: Some(3600),
        cron_expression: None,
        timezone: "UTC".into(),
        next_fire_at: None,
        enabled: true,
        disabled_at: None,
        revision: 2,
        last_task_id: None,
        last_error: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        agent_name: "reviewer".into(),
        filter_agent_name: None,
        last_task_status: None,
    };
    let mut expected = sorted(vec![
        "id",
        "workspace_id",
        "issue_id",
        "agent_id",
        "created_by",
        "source_task_id",
        "parent_comment_id",
        "instruction",
        "kind",
        "mode",
        "event_types",
        "filter_actor_type",
        "filter_actor_id",
        "filter_agent_id",
        "filter_task_id",
        "interval_seconds",
        "cron_expression",
        "timezone",
        "next_fire_at",
        "enabled",
        "disabled_at",
        "revision",
        "last_task_id",
        "last_error",
        "created_at",
        "updated_at",
    ]);
    // 4 个上游别名列（`ListIssueWakeups` 的 JOIN 产物）。
    expected.extend(sorted(vec![
        "filter_actor_name",
        "agent_name",
        "filter_agent_name",
        "last_task_status",
    ]));
    expected.sort_unstable();
    assert_eq!(keys(&serde_json::to_value(&view).expect("serialize")), expected);
}

#[test]
fn summary_row_serialises_with_column_names() {
    let row = WakeupSummaryRow {
        issue_id: Uuid::nil(),
        id: Uuid::nil(),
        agent_id: Uuid::nil(),
        agent_name: "reviewer".into(),
        kind: "cron".into(),
        mode: "continuous".into(),
        event_types: vec![],
        filter_actor_type: None,
        filter_actor_id: None,
        filter_actor_name: String::new(),
        filter_task_id: None,
        filter_agent_name: None,
        interval_seconds: None,
        cron_expression: Some("0 9 * * 1-5".into()),
        timezone: "Asia/Shanghai".into(),
        next_fire_at: None,
        active_count: 3,
        event_count: 1,
    };
    assert_eq!(
        keys(&serde_json::to_value(&row).expect("serialize")),
        sorted(vec![
            "issue_id",
            "id",
            "agent_id",
            "agent_name",
            "kind",
            "mode",
            "event_types",
            "filter_actor_type",
            "filter_actor_id",
            "filter_actor_name",
            "filter_task_id",
            "filter_agent_name",
            "interval_seconds",
            "cron_expression",
            "timezone",
            "next_fire_at",
            "active_count",
            "event_count",
        ])
    );
}

#[test]
fn receipt_row_serialises_with_column_names() {
    let row = WakeupReceiptRow {
        id: Uuid::nil(),
        wakeup_id: Uuid::nil(),
        revision: 1,
        event_key: "evt:1".into(),
        event_type: "task.completed".into(),
        payload: json!({"version": 1}),
        coalesce_key: Some("task.completed".into()),
        task_id: None,
        processed_at: None,
        created_at: chrono::Utc::now(),
    };
    assert_eq!(
        keys(&serde_json::to_value(&row).expect("serialize")),
        sorted(vec![
            "id",
            "wakeup_id",
            "revision",
            "event_key",
            "event_type",
            "payload",
            "coalesce_key",
            "task_id",
            "processed_at",
            "created_at",
        ])
    );
}

#[test]
fn new_ids_are_uuid_v7() {
    let id = new_id();
    // v7 的 version nibble 是 7（时间有序，主键落点集中）。
    assert_eq!(id.get_version_num(), 7);
}
