//! R639.2: pc-pipelines::aggregation_db DB glue 集成测试（真实 PG）。
//!
//! 验证 \`list_pipeline_attention\`：
//! - suggestions：投影 pending_suggestion IS NOT NULL cases
//! - reviews：投影 stage.kind='review' + caller filter
//! - bounded_limit 边界
//! - reviewer_kind fallback

use pc_pipelines::aggregation::{
    bounded_limit, AttentionCaller, PIPELINE_ATTENTION_DEFAULT_LIMIT,
};
use pc_pipelines::aggregation_db::{
    build_heads_up_items, list_drift_events, list_pipeline_attention, list_reviews,
    list_suggestions, load_active_work_for_cases, load_open_work_issues_for_cases,
    load_upstream_cases,
};
use pc_repos::Db;
use uuid::Uuid;

const TEST_DATABASE_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(TEST_DATABASE_URL, 4, 0).await.expect("connect")
}

async fn cleanup(db: &Db) {
    let _ = sqlx::query(
        "DELETE FROM pipeline_case_events WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r6392pa-%')",
    )
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM pipeline_case_issue_links WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r6392pa-%')",
    )
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM pipeline_cases WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r6392pa-%')",
    )
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM pipeline_stages WHERE pipeline_id IN (SELECT id FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r6392pa-%'))",
    )
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM pipelines WHERE company_id IN (SELECT id FROM companies WHERE name LIKE 'r6392pa-%')",
    )
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM companies WHERE name LIKE 'r6392pa-%'",
    )
    .execute(db.pool())
    .await;
}

async fn fixture(db: &Db, label: &str) -> (Uuid, Uuid) {
    let company_id = Uuid::new_v4();
    let pipeline_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO companies (id, name, status, issue_prefix, created_at, updated_at) \
         VALUES ($1, $2, 'active', $3, now(), now())",
    )
    .bind(company_id)
    .bind(format!("r6392pa-{label}-{company_id}"))
    .bind(format!("R{}", Uuid::new_v4().simple().to_string().chars().take(5).collect::<String>()))
    .execute(db.pool())
    .await
    .unwrap();

    sqlx::query(
        "INSERT INTO pipelines (id, company_id, key, name, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, now(), now())",
    )
    .bind(pipeline_id)
    .bind(company_id)
    .bind(format!("p-{label}"))
    .bind(format!("Pipeline {label}"))
    .execute(db.pool())
    .await
    .unwrap();

    (company_id, pipeline_id)
}

async fn insert_stage(db: &Db, pipeline_id: Uuid, key: &str, kind: &str, config: serde_json::Value) -> Uuid {
    let stage_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_stages (id, pipeline_id, key, name, kind, position, config, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, 0, $6, now(), now())",
    )
    .bind(stage_id)
    .bind(pipeline_id)
    .bind(key)
    .bind(format!("Stage {key}"))
    .bind(kind)
    .bind(config)
    .execute(db.pool())
    .await
    .unwrap();
    stage_id
}

async fn insert_case_with_suggestion(
    db: &Db,
    company_id: Uuid,
    pipeline_id: Uuid,
    stage_id: Uuid,
    suggestion_id: &str,
    to_stage_key: &str,
    suggested_by: Option<&str>,
) -> Uuid {
    let case_id = Uuid::new_v4();
    let suggestion = serde_json::json!({
        "id": suggestion_id,
        "toStageKey": to_stage_key,
        "rationale": "test rationale",
        "confidence": 0.85,
        "createdAt": "2026-08-12T00:00:00Z",
        "suggestedByAgentId": suggested_by.unwrap_or(""),
    });
    sqlx::query(
        "INSERT INTO pipeline_cases \
         (id, company_id, pipeline_id, stage_id, case_key, title, fields, child_count, terminal_child_count, version, pending_suggestion, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, 0, 0, 1, $7, now(), now())",
    )
    .bind(case_id)
    .bind(company_id)
    .bind(pipeline_id)
    .bind(stage_id)
    .bind(format!("CASE-{case_id}"))
    .bind(format!("Case {case_id}"))
    .bind(suggestion)
    .execute(db.pool())
    .await
    .unwrap();
    case_id
}

async fn insert_review_case(
    db: &Db,
    company_id: Uuid,
    pipeline_id: Uuid,
    stage_id: Uuid,
    _config: serde_json::Value,
) -> Uuid {
    let case_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_cases \
         (id, company_id, pipeline_id, stage_id, case_key, title, fields, child_count, terminal_child_count, version, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, '{}'::jsonb, 0, 0, 1, now(), now())",
    )
    .bind(case_id)
    .bind(company_id)
    .bind(pipeline_id)
    .bind(stage_id)
    .bind(format!("REV-{case_id}"))
    .bind(format!("Review Case {case_id}"))
    .execute(db.pool())
    .await
    .unwrap();
    case_id
}

#[tokio::test]
async fn r6392_list_suggestions_returns_pending_only() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "sug").await;
    let stage_id = insert_stage(&db, pipeline_id, "working", "working", serde_json::json!({})).await;
    insert_case_with_suggestion(&db, company_id, pipeline_id, stage_id, "sug-1", "review", Some("agent-1")).await;
    insert_case_with_suggestion(&db, company_id, pipeline_id, stage_id, "sug-2", "done", None).await;

    let rows = list_suggestions(db.pool(), company_id, PIPELINE_ATTENTION_DEFAULT_LIMIT)
        .await
        .expect("list_suggestions");
    assert_eq!(rows.len(), 2, "two pending suggestions");
    let mut ids: Vec<String> = rows
        .iter()
        .map(|r| r.case_pending_suggestion.as_ref().unwrap()["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["sug-1".to_string(), "sug-2".to_string()]);

    cleanup(&db).await;
}

#[tokio::test]
async fn r6392_list_reviews_filters_by_caller() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "rev").await;
    let cfg_any = serde_json::json!({"reviewerKind": "any"});
    let cfg_human = serde_json::json!({"reviewerKind": "human"});
    let cfg_agent_specific = serde_json::json!({"reviewerKind": "agent", "approver": {"kind": "agent", "id": "agent-99"}, "requireApproval": true});

    let stage_any = insert_stage(&db, pipeline_id, "review-any", "review", cfg_any).await;
    let stage_human = insert_stage(&db, pipeline_id, "review-human", "review", cfg_human).await;
    let stage_specific = insert_stage(&db, pipeline_id, "review-specific", "review", cfg_agent_specific).await;

    insert_review_case(&db, company_id, pipeline_id, stage_any, serde_json::json!({})).await;
    insert_review_case(&db, company_id, pipeline_id, stage_human, serde_json::json!({})).await;
    insert_review_case(&db, company_id, pipeline_id, stage_specific, serde_json::json!({})).await;

    let user_caller = AttentionCaller::User { user_id: "u-1".into() };
    let rows = list_reviews(db.pool(), company_id, &user_caller, PIPELINE_ATTENTION_DEFAULT_LIMIT)
        .await
        .expect("list_reviews");
    assert_eq!(rows.len(), 3, "user sees all reviews");

    // agent 99 should see 'any' + the specific stage
    let agent_caller = AttentionCaller::Agent { agent_id: "agent-99".into() };
    let rows = list_reviews(db.pool(), company_id, &agent_caller, PIPELINE_ATTENTION_DEFAULT_LIMIT)
        .await
        .expect("list_reviews");
    assert_eq!(rows.len(), 2, "agent 99 sees 'any' + specific config");

    // agent 1 only sees 'any'
    let other_agent = AttentionCaller::Agent { agent_id: "agent-1".into() };
    let rows = list_reviews(db.pool(), company_id, &other_agent, PIPELINE_ATTENTION_DEFAULT_LIMIT)
        .await
        .expect("list_reviews");
    assert_eq!(rows.len(), 1, "agent 1 only sees 'any'");

    cleanup(&db).await;
}

#[tokio::test]
async fn r6392_list_pipeline_attention_combines_suggestions_and_reviews() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "combined").await;
    let cfg_any = serde_json::json!({"reviewerKind": "any"});
    let stage_working = insert_stage(&db, pipeline_id, "working", "working", serde_json::json!({})).await;
    let stage_review = insert_stage(&db, pipeline_id, "review", "review", cfg_any).await;

    insert_case_with_suggestion(&db, company_id, pipeline_id, stage_working, "sug-a", "review", None).await;
    insert_review_case(&db, company_id, pipeline_id, stage_review, serde_json::json!({})).await;

    let caller = AttentionCaller::User { user_id: "u-x".into() };
    let result = list_pipeline_attention(db.pool(), company_id, &caller, None)
        .await
        .expect("list_pipeline_attention");
    assert_eq!(result.suggestions.len(), 1);
    assert_eq!(result.reviews.len(), 1);
    assert_eq!(result.counts.suggestions, 1);
    assert_eq!(result.counts.reviews, 1);
    assert_eq!(result.suggestions[0].suggestion.id, "sug-a");
    assert_eq!(result.reviews[0].review.reviewer_kind, "any");

    cleanup(&db).await;
}

#[tokio::test]
async fn r6392_bounded_limit_clamps_inputs() {
    assert_eq!(bounded_limit(None, 50, 100), 50);
    assert_eq!(bounded_limit(Some(0), 50, 100), 1);
    assert_eq!(bounded_limit(Some(1000), 50, 100), 100);
}

async fn insert_drift_event(db: &Db, company_id: Uuid, case_id: Uuid, payload: serde_json::Value) -> Uuid {
    let event_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_case_events (id, company_id, case_id, type, actor_type, payload, created_at, updated_at) \
         VALUES ($1, $2, $3, 'upstream_drift', 'system', $4, now(), now())",
    )
    .bind(event_id)
    .bind(company_id)
    .bind(case_id)
    .bind(payload)
    .execute(db.pool())
    .await
    .unwrap();
    event_id
}

async fn insert_drift_ack(db: &Db, company_id: Uuid, case_id: Uuid) -> Uuid {
    let ack_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipeline_case_events (id, company_id, case_id, type, actor_type, payload, created_at, updated_at) \
         VALUES ($1, $2, $3, 'drift_acknowledged', 'system', '{}'::jsonb, now() + interval '1 second', now())",
    )
    .bind(ack_id)
    .bind(company_id)
    .bind(case_id)
    .execute(db.pool())
    .await
    .unwrap();
    ack_id
}

#[tokio::test]
async fn r6392_list_drift_events_filters_acked() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "drift").await;
    let stage_id = insert_stage(&db, pipeline_id, "working", "working", serde_json::json!({})).await;
    let upstream_case_id = insert_case_with_suggestion(
        &db, company_id, pipeline_id, stage_id, "upstream-sug", "review", None,
    ).await;
    let pending_case = insert_review_case(&db, company_id, pipeline_id, stage_id, serde_json::json!({})).await;
    let acked_case = insert_review_case(&db, company_id, pipeline_id, stage_id, serde_json::json!({})).await;

    insert_drift_event(
        &db, company_id, pending_case,
        serde_json::json!({"upstreamCaseId": upstream_case_id.to_string(), "previousVersion": 1, "version": 2}),
    ).await;
    insert_drift_event(
        &db, company_id, acked_case,
        serde_json::json!({"upstreamCaseId": upstream_case_id.to_string(), "previousVersion": 1, "version": 2}),
    ).await;
    insert_drift_ack(&db, company_id, acked_case).await;

    let rows = list_drift_events(db.pool(), company_id, PIPELINE_ATTENTION_DEFAULT_LIMIT)
        .await
        .expect("list_drift_events");
    assert_eq!(rows.len(), 1, "only pending (un-acked) drift event");
    assert_eq!(rows[0].case_id, pending_case);
    let payload = rows[0].event_payload.as_object().unwrap();
    assert_eq!(payload["previousVersion"], serde_json::json!(1));
    assert_eq!(payload["version"], serde_json::json!(2));

    cleanup(&db).await;
}

#[tokio::test]
async fn r6392_build_heads_up_items_resolves_upstream() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "hup").await;
    let stage_id = insert_stage(&db, pipeline_id, "working", "working", serde_json::json!({})).await;
    let upstream_case_id = insert_case_with_suggestion(
        &db, company_id, pipeline_id, stage_id, "upstream-sug", "review", None,
    ).await;
    let drift_case = insert_review_case(&db, company_id, pipeline_id, stage_id, serde_json::json!({})).await;

    insert_drift_event(
        &db, company_id, drift_case,
        serde_json::json!({"upstreamCaseId": upstream_case_id.to_string(), "previousVersion": 1, "version": 2}),
    ).await;

    let drift_rows = list_drift_events(db.pool(), company_id, PIPELINE_ATTENTION_DEFAULT_LIMIT)
        .await
        .expect("list_drift_events");
    let drift_case_ids: Vec<Uuid> = drift_rows.iter().map(|r| r.case_id).collect();
    let active_work_rows = load_active_work_for_cases(db.pool(), company_id, &drift_case_ids)
        .await
        .expect("load_active_work");
    let work_issue_rows = load_open_work_issues_for_cases(db.pool(), company_id, &drift_case_ids)
        .await
        .expect("load_open_work_issues");
    let upstream_case_ids: Vec<Uuid> = drift_rows
        .iter()
        .filter_map(|r| {
            r.event_payload.get("upstreamCaseId")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
        })
        .collect();
    let upstream_rows = load_upstream_cases(db.pool(), company_id, &upstream_case_ids)
        .await
        .expect("load_upstream_cases");

    let items = build_heads_up_items(drift_rows, active_work_rows, work_issue_rows, upstream_rows);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].drift.previous_version, Some(1));
    assert_eq!(items[0].drift.version, Some(2));
    assert_eq!(items[0].drift.upstream.case_id.as_deref(), Some(upstream_case_id.to_string().as_str()));
    assert!(items[0].drift.upstream.title.is_some(), "upstream title resolved from JOIN");
    assert!(items[0].active_work.is_none(), "no active work without issue link");
    assert!(items[0].work_issue.is_none(), "no open work issue without link");

    cleanup(&db).await;
}

#[tokio::test]
async fn r6392_list_pipeline_attention_includes_heads_up() {
    let db = connect().await;
    cleanup(&db).await;
    let (company_id, pipeline_id) = fixture(&db, "hup-combined").await;
    let cfg_any = serde_json::json!({"reviewerKind": "any"});
    let stage_working = insert_stage(&db, pipeline_id, "working", "working", serde_json::json!({})).await;
    let stage_review = insert_stage(&db, pipeline_id, "review", "review", cfg_any).await;
    let upstream_id = insert_case_with_suggestion(
        &db, company_id, pipeline_id, stage_working, "upstream-sug", "review", None,
    ).await;
    let drift_case = insert_review_case(&db, company_id, pipeline_id, stage_review, serde_json::json!({})).await;
    insert_drift_event(
        &db, company_id, drift_case,
        serde_json::json!({"upstreamCaseId": upstream_id.to_string(), "previousVersion": 1, "version": 2}),
    ).await;

    let caller = AttentionCaller::User { user_id: "u-x".into() };
    let result = list_pipeline_attention(db.pool(), company_id, &caller, None)
        .await
        .expect("list_pipeline_attention");
    assert_eq!(result.heads_up.len(), 1);
    assert_eq!(result.counts.heads_up, 1);
    assert_eq!(result.heads_up[0].drift.upstream.case_id.as_deref(), Some(upstream_id.to_string().as_str()));

    cleanup(&db).await;
}
