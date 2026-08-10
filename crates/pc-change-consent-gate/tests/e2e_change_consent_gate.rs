//! E2E tests for `pc-change-consent-gate`.
//!
//! 与 Node `server/src/__tests__/change-consent-gate.test.ts` 1:1 对齐。

use pc_change_consent_gate::{
    codes, payload_has_displayed_diff, request_confirmation_result_consumed,
    skill_change_target_key, AssertConsentedInput, ChangeConsentError,
    ChangeConsentGateService,
};
use pc_repos::Db;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect to db")
}

async fn cleanup(db: &Db, tag: &str) {
    let prefix = format!("CCG-{tag}");
    let _ = sqlx::query("DELETE FROM issue_thread_interactions WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)")
        .bind(&prefix)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)")
        .bind(&prefix)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM heartbeat_runs WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)")
        .bind(&prefix)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)")
        .bind(&prefix)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE issue_prefix = $1")
        .bind(&prefix)
        .execute(db.pool())
        .await;
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let name = format!("CCG Co {tag} {}", Uuid::new_v4());
    let row = sqlx::query("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
        .bind(&name)
        .bind(&format!("CCG-{tag}"))
        .fetch_one(db.pool())
        .await
        .expect("create company");
    row.try_get::<Uuid, _>("id").expect("id column")
}

// ============================================================================
// Pure-function e2e tests (no DB)
// ============================================================================

#[test]
fn r674_payload_has_displayed_diff_matches_node() {
    assert!(payload_has_displayed_diff(&json!({
        "detailsMarkdown": "```diff\n-old\n+new\n```",
    })));
    assert!(payload_has_displayed_diff(&json!({
        "detailsMarkdown": "Summary\n+ added\n- removed",
    })));
    assert!(!payload_has_displayed_diff(&json!({
        "detailsMarkdown": "--- a/file\n\n+++ b/file",
    })));
    assert!(!payload_has_displayed_diff(&json!({
        "detailsMarkdown": "No diff here",
    })));
}

#[test]
fn r674_result_consumed_matches_node() {
    assert!(!request_confirmation_result_consumed(None));
    assert!(!request_confirmation_result_consumed(Some(&json!({"outcome": "accepted"}))));
    assert!(request_confirmation_result_consumed(Some(&json!({
        "outcome": "accepted",
        "consumedByRunId": "run-1",
    }))));
    assert!(request_confirmation_result_consumed(Some(&json!({
        "outcome": "accepted",
        "consumedAt": "2026-01-01T00:00:00Z",
    }))));
}

#[test]
fn r674_target_key_helpers_match_node() {
    assert_eq!(skill_change_target_key("skill-123"), "skill:skill-123");
}

// ============================================================================
// DB e2e tests (require running Postgres)
// ============================================================================

async fn seed_accepted_request_confirmation(
    db: &Db,
    company_id: Uuid,
    actor_agent_id: Uuid,
    source_run_id: Uuid,
    issue_id: Uuid,
    target_key: &str,
    with_diff: bool,
) -> Uuid {
    let details = if with_diff {
        Some("Here is the diff:\n```diff\n-old\n+new\n```")
    } else {
        Some("No diff here.")
    };

    let payload = json!({
        "version": 1,
        "prompt": "Apply changes?",
        "detailsMarkdown": details,
        "target": {
            "type": "custom",
            "key": target_key,
        },
    });

    let result = json!({
        "version": 1,
        "outcome": "accepted",
    });

    let row = sqlx::query(
        r#"
        INSERT INTO issue_thread_interactions (
            company_id, issue_id, kind, status, continuation_policy,
            source_run_id, title, summary,
            created_by_agent_id, payload, result, resolved_at, created_at, updated_at
        ) VALUES (
            $1, $2, 'request_confirmation', 'accepted', 'wake_assignee',
            $3, 'Test confirmation', 'Test summary',
            $4, $5, $6, now(), now(), now()
        )
        RETURNING id
        "#,
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(source_run_id)
    .bind(actor_agent_id)
    .bind(&payload)
    .bind(&result)
    .fetch_one(db.pool())
    .await
    .expect("insert interaction");
    row.try_get::<Uuid, _>("id").expect("id column")
}

async fn seed_agent_and_issue(db: &Db, company_id: Uuid, tag: &str) -> (Uuid, Uuid) {
    let agent_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO agents (id, company_id, name, role, adapter_type, adapter_config, runtime_config, permissions)
        VALUES ($1, $2, $3, 'general', 'codex_local', '{}'::jsonb, '{}'::jsonb, '{}'::jsonb)
        "#,
    )
    .bind(agent_id)
    .bind(company_id)
    .bind(format!("CCG agent {tag}"))
    .execute(db.pool())
    .await
    .expect("insert agent");

    let issue_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO issues (id, company_id, title, status, priority)
        VALUES ($1, $2, 'CCG test issue', 'in_review', 'medium')
        "#,
    )
    .bind(issue_id)
    .bind(company_id)
    .execute(db.pool())
    .await
    .expect("insert issue");

    (agent_id, issue_id)
}

async fn seed_heartbeat_run(db: &Db, company_id: Uuid, agent_id: Uuid) -> Uuid {
    let run_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO heartbeat_runs (id, company_id, agent_id, status)
        VALUES ($1, $2, $3, 'succeeded')
        "#,
    )
    .bind(run_id)
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert heartbeat run");
    run_id
}

fn code(err: &ChangeConsentError) -> Option<&'static str> {
    match err {
        ChangeConsentError::Forbidden { code, .. } => Some(*code),
        _ => None,
    }
}

#[tokio::test]
async fn r674_assert_consented_succeeds_with_accepted_confirmation_with_diff() {
    let db = connect().await;
    cleanup(&db, "ok").await;
    let company_id = make_company(&db, &format!("ok-{}", Uuid::new_v4())).await;
    let (agent_id, issue_id) = seed_agent_and_issue(&db, company_id, "ok").await;
    let source_run_id = seed_heartbeat_run(&db, company_id, agent_id).await;
    let actor_run_id = seed_heartbeat_run(&db, company_id, agent_id).await;
    let skill_id = Uuid::new_v4();
    let target_key = skill_change_target_key(&skill_id.to_string());

    let interaction_id = seed_accepted_request_confirmation(
        &db,
        company_id,
        agent_id,
        source_run_id,
        issue_id,
        &target_key,
        true,
    )
    .await;

    let svc = ChangeConsentGateService::new(db.clone());
    let input = AssertConsentedInput::new(
        company_id,
        Some(agent_id.to_string()),
        Some(actor_run_id.to_string()),
        vec![target_key.clone()],
    );
    let result = svc.assert_consented(&input).await;
    assert!(result.is_ok(), "expected Ok, got {result:?}");

    // Verify the row is now marked consumed.
    let row: (Value,) = sqlx::query_as("SELECT result FROM issue_thread_interactions WHERE id = $1")
        .bind(interaction_id)
        .fetch_one(db.pool())
        .await
        .expect("fetch");
    let result_json = &row.0;
    assert_eq!(
        result_json["consumedByRunId"],
        Value::String(actor_run_id.to_string())
    );
    assert!(result_json["consumedAt"].is_string());

    cleanup(&db, "ok").await;
}

#[tokio::test]
async fn r674_assert_consented_rejects_when_already_consumed() {
    let db = connect().await;
    cleanup(&db, "consumed").await;
    let company_id = make_company(&db, &format!("consumed-{}", Uuid::new_v4())).await;
    let (agent_id, issue_id) = seed_agent_and_issue(&db, company_id, "consumed").await;
    let source_run_id = seed_heartbeat_run(&db, company_id, agent_id).await;
    let actor_run_id = seed_heartbeat_run(&db, company_id, agent_id).await;
    let skill_id = Uuid::new_v4();
    let target_key = skill_change_target_key(&skill_id.to_string());

    seed_accepted_request_confirmation(
        &db,
        company_id,
        agent_id,
        source_run_id,
        issue_id,
        &target_key,
        true,
    )
    .await;

    let svc = ChangeConsentGateService::new(db.clone());
    let input = AssertConsentedInput::new(
        company_id,
        Some(agent_id.to_string()),
        Some(actor_run_id.to_string()),
        vec![target_key.clone()],
    );

    // First call succeeds
    svc.assert_consented(&input).await.expect("first call ok");

    // Second call should fail (consumed)
    let result = svc.assert_consented(&input).await;
    assert!(result.is_err(), "second call should fail");
    assert_eq!(
        code(&result.unwrap_err()),
        Some(codes::REFLECTION_COACH_MUTATION_GATE_REQUIRED)
    );

    cleanup(&db, "consumed").await;
}

#[tokio::test]
async fn r674_assert_consented_rejects_when_diff_missing() {
    let db = connect().await;
    cleanup(&db, "nodiff").await;
    let company_id = make_company(&db, &format!("nodiff-{}", Uuid::new_v4())).await;
    let (agent_id, issue_id) = seed_agent_and_issue(&db, company_id, "nodiff").await;
    let source_run_id = seed_heartbeat_run(&db, company_id, agent_id).await;
    let actor_run_id = seed_heartbeat_run(&db, company_id, agent_id).await;
    let skill_id = Uuid::new_v4();
    let target_key = skill_change_target_key(&skill_id.to_string());

    seed_accepted_request_confirmation(
        &db,
        company_id,
        agent_id,
        source_run_id,
        issue_id,
        &target_key,
        false, // without diff
    )
    .await;

    let svc = ChangeConsentGateService::new(db.clone());
    let input = AssertConsentedInput::new(
        company_id,
        Some(agent_id.to_string()),
        Some(actor_run_id.to_string()),
        vec![target_key.clone()],
    );
    let result = svc.assert_consented(&input).await;
    assert!(result.is_err());
    assert_eq!(
        code(&result.unwrap_err()),
        Some(codes::REFLECTION_COACH_MUTATION_GATE_REQUIRED)
    );

    cleanup(&db, "nodiff").await;
}

#[tokio::test]
async fn r674_assert_consented_rejects_when_source_run_id_equals_actor_run_id() {
    // The Node logic requires source_run_id !== actor_run_id
    let db = connect().await;
    cleanup(&db, "samerun").await;
    let company_id = make_company(&db, &format!("samerun-{}", Uuid::new_v4())).await;
    let (agent_id, issue_id) = seed_agent_and_issue(&db, company_id, "samerun").await;
    let shared_run_id = seed_heartbeat_run(&db, company_id, agent_id).await;
    let skill_id = Uuid::new_v4();
    let target_key = skill_change_target_key(&skill_id.to_string());

    seed_accepted_request_confirmation(
        &db,
        company_id,
        agent_id,
        shared_run_id, // same as actor
        issue_id,
        &target_key,
        true,
    )
    .await;

    let svc = ChangeConsentGateService::new(db.clone());
    let input = AssertConsentedInput::new(
        company_id,
        Some(agent_id.to_string()),
        Some(shared_run_id.to_string()),
        vec![target_key.clone()],
    );
    let result = svc.assert_consented(&input).await;
    assert!(result.is_err(), "same source/actor run id should reject");
    assert_eq!(
        code(&result.unwrap_err()),
        Some(codes::REFLECTION_COACH_MUTATION_GATE_REQUIRED)
    );

    cleanup(&db, "samerun").await;
}

#[tokio::test]
async fn r674_assert_consented_rejects_when_actor_run_id_missing() {
    let db = connect().await;
    cleanup(&db, "samerun").await;
    let company_id = make_company(&db, &format!("norunid-{}", Uuid::new_v4())).await;

    let svc = ChangeConsentGateService::new(db.clone());
    let input = AssertConsentedInput::new(
        company_id,
        Some(Uuid::new_v4().to_string()),
        None,
        vec!["skill:abc".to_string()],
    );
    let result = svc.assert_consented(&input).await;
    assert!(result.is_err());
    assert_eq!(
        code(&result.unwrap_err()),
        Some(codes::REFLECTION_COACH_MUTATION_RUN_ID_REQUIRED)
    );

    cleanup(&db, "norunid").await;
}

#[tokio::test]
async fn r674_assert_consented_rejects_when_target_keys_empty() {
    let db = connect().await;
    cleanup(&db, "norunid").await;
    let company_id = make_company(&db, &format!("notarget-{}", Uuid::new_v4())).await;

    let svc = ChangeConsentGateService::new(db.clone());
    let input = AssertConsentedInput::new(
        company_id,
        Some(Uuid::new_v4().to_string()),
        Some(Uuid::new_v4().to_string()),
        vec![],
    );
    let result = svc.assert_consented(&input).await;
    assert!(result.is_err());
    assert_eq!(
        code(&result.unwrap_err()),
        Some(codes::REFLECTION_COACH_MUTATION_TARGET_REQUIRED)
    );

    cleanup(&db, "notarget").await;
}

#[tokio::test]
async fn r674_assert_consented_rejects_when_actor_agent_id_missing() {
    let db = connect().await;
    cleanup(&db, "notarget").await;
    let company_id = make_company(&db, &format!("noagent-{}", Uuid::new_v4())).await;

    let svc = ChangeConsentGateService::new(db.clone());
    let input = AssertConsentedInput::new(
        company_id,
        None,
        Some(Uuid::new_v4().to_string()),
        vec!["skill:abc".to_string()],
    );
    let result = svc.assert_consented(&input).await;
    assert!(result.is_err());
    assert_eq!(
        code(&result.unwrap_err()),
        Some(codes::REFLECTION_COACH_MUTATION_RUN_ID_REQUIRED)
    );

    cleanup(&db, "noagent").await;
}
