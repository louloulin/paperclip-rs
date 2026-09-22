//! `AgentRepo` 的单测 + PG 集成测试（`#[ignore]`，靠 `MULTICA_TEST_DATABASE_URL`）。
//!
//! 与 `issue_status.rs` 同手法：`MULTICA_TEST_DATABASE_URL` 未设置时打印跳过并
//! `return`，因此 `cargo test`（无 DB URL）不会红；**设置了但连不上/缺表则直接
//! panic**（不许静默跳过假装绿）。

use super::*;
use crate::agent::tasks::is_visible_task_history;
use pretty_assertions::assert_eq;
use serde_json::json;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

#[test]
fn visibility_and_permission_mode_whitelists_match_upstream_checks() {
    assert!(is_valid_visibility("workspace"));
    assert!(is_valid_visibility("private"));
    assert!(!is_valid_visibility("public"));
    assert!(!is_valid_visibility(""));

    assert!(is_valid_permission_mode("private"));
    assert!(is_valid_permission_mode("public_to"));
    assert!(!is_valid_permission_mode("workspace"));
}

#[test]
fn max_concurrent_tasks_bounds_are_1_to_50() {
    assert!(validate_max_concurrent_tasks(1).is_ok());
    assert!(validate_max_concurrent_tasks(50).is_ok());
    assert_eq!(
        validate_max_concurrent_tasks(0).unwrap_err(),
        "max_concurrent_tasks must be between 1 and 50"
    );
    assert!(validate_max_concurrent_tasks(51).is_err());
    // 上游 agentconfig.DefaultMaxConcurrentTasks
    assert_eq!(DEFAULT_MAX_CONCURRENT_TASKS, 6);
}

#[test]
fn nullable_field_sql_is_static() {
    assert_eq!(
        NullableAgentField::McpConfig.sql_assign(),
        "mcp_config = NULL"
    );
    assert_eq!(
        NullableAgentField::ThinkingLevel.sql_assign(),
        "thinking_level = NULL"
    );
    assert_eq!(
        NullableAgentField::ServiceTier.sql_assign(),
        "service_tier = NULL"
    );
    assert_eq!(
        NullableAgentField::ComposioToolkitAllowlist.sql_assign(),
        "composio_toolkit_allowlist = NULL"
    );
}

#[test]
fn agent_columns_cover_every_row_field() {
    // AGENT_COLUMNS 与 AgentRow 必须一一对应（少一列 FromRow 就会 panic）。
    assert_eq!(AGENT_COLUMNS.split(',').count(), 29);
    for col in [
        "visibility",
        "permission_mode",
        "conversation_starters",
        "kind",
    ] {
        assert!(AGENT_COLUMNS.contains(col), "missing {col}");
    }
}

#[test]
fn agent_row_accessors_expose_env_key_count_without_values() {
    let row = sample_row(json!({"A": "1", "B": "2"}));
    assert_eq!(row.custom_env_key_count(), 2);
    assert_eq!(row.id(), Id(row.id));
    assert_eq!(row.workspace_id(), Id(row.workspace_id));
    assert!(row.owner_id().is_some());
    assert!(!row.is_archived());
    assert!(!row.is_system());
    assert!(!row.has_composio_allowlist());
}

#[test]
fn task_row_status_helpers_split_active_and_outcome() {
    let mut row = sample_task("queued");
    assert!(row.is_active());
    assert!(!row.is_outcome());
    row.status = "running".into();
    assert!(row.is_active());
    row.status = "completed".into();
    assert!(!row.is_active());
    assert!(row.is_outcome());
    // cancelled 是程序信号，不是结果（上游 snapshot 注释）
    row.status = "cancelled".into();
    assert!(!row.is_outcome());
}

#[test]
fn visible_task_history_omits_unused_escalation_fallback() {
    let mut row = sample_task("cancelled");
    row.escalation_for_task_id = Some(Uuid::now_v7());
    assert!(!is_visible_task_history(&row));
    // 已启动的回退保留
    row.started_at = Some(Utc::now());
    assert!(is_visible_task_history(&row));
    // 普通取消保留
    row.started_at = None;
    row.escalation_for_task_id = None;
    assert!(is_visible_task_history(&row));
}

fn sample_row(custom_env: JsonValue) -> AgentRow {
    AgentRow {
        id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        name: "agent".into(),
        avatar_url: None,
        runtime_mode: "local".into(),
        runtime_config: json!({}),
        visibility: "private".into(),
        status: "offline".into(),
        max_concurrent_tasks: 6,
        owner_id: Some(Uuid::now_v7()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        description: String::new(),
        runtime_id: None,
        instructions: String::new(),
        archived_at: None,
        archived_by: None,
        custom_env,
        custom_args: json!([]),
        mcp_config: None,
        model: None,
        thinking_level: None,
        composio_toolkit_allowlist: None,
        permission_mode: "private".into(),
        kind: "user".into(),
        system_key: None,
        disabled_runtime_skills: json!([]),
        service_tier: None,
        conversation_starters: json!([]),
    }
}

fn sample_task(status: &str) -> AgentTaskRow {
    AgentTaskRow {
        id: Uuid::now_v7(),
        agent_id: Uuid::now_v7(),
        runtime_id: None,
        issue_id: None,
        status: status.into(),
        priority: 0,
        dispatched_at: None,
        started_at: None,
        completed_at: None,
        created_at: Utc::now(),
        attempt: 1,
        max_attempts: 2,
        error: None,
        failure_reason: None,
        escalation_for_task_id: None,
    }
}

#[test]
fn invocation_target_type_whitelist() {
    let target = AgentInvocationTargetRow {
        id: Uuid::now_v7(),
        agent_id: Uuid::now_v7(),
        target_type: "workspace".into(),
        target_id: Uuid::now_v7(),
        created_by: None,
        created_at: Utc::now(),
    };
    assert!(target.is_known_type());
}

#[cfg(test)]
mod db_tests;
