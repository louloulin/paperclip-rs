//! E2E tests for `pc-status-card-finalization`.
//!
//! 与 Node `server/src/__tests__/status-cards.test.ts` 中 finalization 部分 1:1 对齐。

use pc_repos::Db;
use pc_status_card_finalization::{
    finalize_status_cards_for_stalled_generation, StatusCardFinalizationService,
    StalledGenerationIssue, STALLED_GENERATION_STATUSES,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect to db")
}

async fn cleanup(db: &Db, prefix: &str) {
    let _ = sqlx::query(
        "DELETE FROM status_card_updates WHERE card_id IN (SELECT id FROM status_cards WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1))",
    )
    .bind(prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM status_cards WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query("DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)")
        .bind(prefix)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)")
        .bind(prefix)
        .execute(db.pool())
        .await;
    let _ = sqlx::query("DELETE FROM companies WHERE issue_prefix = $1")
        .bind(prefix)
        .execute(db.pool())
        .await;
}

async fn make_company(db: &Db, prefix: &str) -> Uuid {
    let name = format!("SCF Co {} {}", prefix, Uuid::new_v4());
    let row = sqlx::query("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
        .bind(&name)
        .bind(prefix)
        .fetch_one(db.pool())
        .await
        .expect("create company");
    row.try_get::<Uuid, _>("id").expect("id column")
}

async fn make_issue(db: &Db, company_id: Uuid, prefix: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO issues (id, company_id, title, status, priority) VALUES ($1, $2, $3, 'in_progress', 'medium')",
    )
    .bind(id)
    .bind(company_id)
    .bind(format!("SCF issue {prefix}"))
    .execute(db.pool())
    .await
    .expect("create issue");
    id
}

async fn seed_status_card(
    db: &Db,
    company_id: Uuid,
    generating_issue_id: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    let query = if let Some(gid) = generating_issue_id {
        format!(
            "INSERT INTO status_cards (id, company_id, title, interest_prompt, state, generating_issue_id, queries, refresh_policy) \
             VALUES ('{id}', '{company_id}', 'Test card', 'prompt', 'compiling', '{gid}', '{{}}'::jsonb, '{{}}'::jsonb)"
        )
    } else {
        format!(
            "INSERT INTO status_cards (id, company_id, title, interest_prompt, state, queries, refresh_policy) \
             VALUES ('{id}', '{company_id}', 'Test card', 'prompt', 'ready', '{{}}'::jsonb, '{{}}'::jsonb)"
        )
    };
    sqlx::query(&query)
        .execute(db.pool())
        .await
        .expect("insert status card");
    id
}

async fn seed_status_card_update(
    db: &Db,
    card_id: Uuid,
    generation_issue_id: Option<Uuid>,
    finished: bool,
) -> Uuid {
    let id = Uuid::new_v4();
    let gid = generation_issue_id.expect("must provide generation_issue_id");
    if finished {
        sqlx::query(
            "INSERT INTO status_card_updates (id, card_id, kind, trigger, status, started_at, finished_at, changes, generation_issue_id)              VALUES ($1, $2, 'recompile', 'manual', 'running', now(), now(), '{}'::jsonb, $3)",
        )
        .bind(id)
        .bind(card_id)
        .bind(gid)
        .execute(db.pool())
        .await
        .expect("insert finished update");
    } else {
        sqlx::query(
            "INSERT INTO status_card_updates (id, card_id, kind, trigger, status, started_at, changes, generation_issue_id)              VALUES ($1, $2, 'recompile', 'manual', 'running', now(), '{}'::jsonb, $3)",
        )
        .bind(id)
        .bind(card_id)
        .bind(gid)
        .execute(db.pool())
        .await
        .expect("insert unfinished update");
    }
    id
}

// ============================================================================
// Pure helper tests (no DB)
// ============================================================================

#[test]
fn r675_constants_match_node() {
    assert_eq!(STALLED_GENERATION_STATUSES.len(), 3);
    assert!(STALLED_GENERATION_STATUSES.contains(&"done"));
    assert!(STALLED_GENERATION_STATUSES.contains(&"cancelled"));
    assert!(STALLED_GENERATION_STATUSES.contains(&"blocked"));
}

// ============================================================================
// DB e2e tests
// ============================================================================

#[tokio::test]
async fn r675_releases_cards_for_done_status() {
    let db = connect().await;
    let prefix = format!("SCF-done-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_id = make_issue(&db, company_id, &prefix).await;
    let card_id = seed_status_card(&db, company_id, Some(issue_id)).await;

    let svc = StatusCardFinalizationService::new();
    let issue = StalledGenerationIssue {
        id: issue_id,
        company_id,
        identifier: Some("SCF-1"),
        title: "Generate status card",
        status: "done",
    };
    let outcome = svc
        .finalize_for_stalled_generation(&db, &issue)
        .await
        .expect("finalize");
    assert_eq!(outcome.released_cards.len(), 1);
    assert_eq!(outcome.released_cards[0].id, card_id);
    assert!(outcome.failure_reason.is_some());
    assert!(outcome.failure_reason.as_ref().unwrap().contains("finished without writing a summary"));

    // Verify card is now in error state with generating_issue_id = NULL
    let row: (String, Option<Uuid>, Option<String>) = sqlx::query_as(
        "SELECT state, generating_issue_id, failure_reason FROM status_cards WHERE id = $1",
    )
    .bind(card_id)
    .fetch_one(db.pool())
    .await
    .expect("fetch card");
    assert_eq!(row.0, "error");
    assert!(row.1.is_none(), "generating_issue_id should be cleared");
    assert!(row.2.is_some());

    cleanup(&db, &prefix).await;
}

#[tokio::test]
async fn r675_releases_cards_for_cancelled_status() {
    let db = connect().await;
    let prefix = format!("SCF-cancelled-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_id = make_issue(&db, company_id, &prefix).await;
    let card_id = seed_status_card(&db, company_id, Some(issue_id)).await;

    let issue = StalledGenerationIssue {
        id: issue_id,
        company_id,
        identifier: None,
        title: "Some title",
        status: "cancelled",
    };
    let outcome = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("finalize");
    assert_eq!(outcome.released_cards.len(), 1);
    assert_eq!(outcome.released_cards[0].id, card_id);
    assert!(outcome.failure_reason.as_ref().unwrap().contains("cancelled"));

    cleanup(&db, &prefix).await;
}

#[tokio::test]
async fn r675_releases_cards_for_blocked_status() {
    let db = connect().await;
    let prefix = format!("SCF-blocked-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_id = make_issue(&db, company_id, &prefix).await;
    let card_id = seed_status_card(&db, company_id, Some(issue_id)).await;

    let issue = StalledGenerationIssue {
        id: issue_id,
        company_id,
        identifier: None,
        title: "Some title",
        status: "blocked",
    };
    let outcome = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("finalize");
    assert_eq!(outcome.released_cards.len(), 1);
    assert!(outcome.failure_reason.as_ref().unwrap().contains("blocked"));
    assert!(outcome.failure_reason.as_ref().unwrap().contains("re-run"));

    cleanup(&db, &prefix).await;
}

#[tokio::test]
async fn r675_does_nothing_for_non_stalled_status() {
    let db = connect().await;
    let prefix = format!("SCF-progress-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_id = make_issue(&db, company_id, &prefix).await;
    let _card_id = seed_status_card(&db, company_id, Some(issue_id)).await;

    let issue = StalledGenerationIssue {
        id: issue_id,
        company_id,
        identifier: None,
        title: "Some title",
        status: "in_progress",
    };
    let outcome = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("finalize");
    assert_eq!(outcome.released_cards.len(), 0);
    assert!(outcome.failure_reason.is_none());

    cleanup(&db, &prefix).await;
}

#[tokio::test]
async fn r675_only_releases_cards_owned_by_issue() {
    // 同一 company 下有两个 issue 各自持有不同 card，只释放传入 issue 的。
    let db = connect().await;
    let prefix = format!("SCF-scope-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_a = make_issue(&db, company_id, &prefix).await;
    let issue_b = make_issue(&db, company_id, &prefix).await;
    let card_a = seed_status_card(&db, company_id, Some(issue_a)).await;
    let card_b = seed_status_card(&db, company_id, Some(issue_b)).await;

    let issue = StalledGenerationIssue {
        id: issue_a,
        company_id,
        identifier: None,
        title: "Title A",
        status: "done",
    };
    let outcome = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("finalize");
    assert_eq!(outcome.released_cards.len(), 1);
    assert_eq!(outcome.released_cards[0].id, card_a);

    // card_b 应保持 compiling 状态
    let row: (String, Option<Uuid>) = sqlx::query_as(
        "SELECT state, generating_issue_id FROM status_cards WHERE id = $1",
    )
    .bind(card_b)
    .fetch_one(db.pool())
    .await
    .expect("fetch card_b");
    assert_eq!(row.0, "compiling");
    assert_eq!(row.1, Some(issue_b));

    cleanup(&db, &prefix).await;
}

#[tokio::test]
async fn r675_marks_unfinished_updates_as_failed() {
    let db = connect().await;
    let prefix = format!("SCF-updates-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_id = make_issue(&db, company_id, &prefix).await;
    let card_id = seed_status_card(&db, company_id, Some(issue_id)).await;

    // Seed 2 unfinished updates + 1 finished update
    let _u1 = seed_status_card_update(&db, card_id, Some(issue_id), false).await;
    let _u2 = seed_status_card_update(&db, card_id, Some(issue_id), false).await;
    let _u3 = seed_status_card_update(&db, card_id, Some(issue_id), true).await;

    let issue = StalledGenerationIssue {
        id: issue_id,
        company_id,
        identifier: None,
        title: "Title",
        status: "done",
    };
    let outcome = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("finalize");
    assert!(outcome.updates_failed_count >= 1);

    // Verify no unfinished updates remain for this generation_issue_id
    let unfinished_count: (i64,) = sqlx::query_as(
        "SELECT count(*)::bigint FROM status_card_updates WHERE generation_issue_id = $1 AND finished_at IS NULL",
    )
    .bind(issue_id)
    .fetch_one(db.pool())
    .await
    .expect("count");
    assert_eq!(unfinished_count.0, 0, "all unfinished updates should be marked failed");

    cleanup(&db, &prefix).await;
}

#[tokio::test]
async fn r675_idempotent_when_called_twice() {
    let db = connect().await;
    let prefix = format!("SCF-idem-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_id = make_issue(&db, company_id, &prefix).await;
    let _card_id = seed_status_card(&db, company_id, Some(issue_id)).await;

    let issue = StalledGenerationIssue {
        id: issue_id,
        company_id,
        identifier: None,
        title: "Title",
        status: "cancelled",
    };
    let outcome1 = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("first call");
    assert_eq!(outcome1.released_cards.len(), 1);

    // 第二次调用：card 已被释放，generating_issue_id 已为 NULL，所以应无 card 被释放
    let outcome2 = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("second call");
    assert_eq!(outcome2.released_cards.len(), 0);

    cleanup(&db, &prefix).await;
}

#[tokio::test]
async fn r675_failure_reason_includes_identifier_and_title() {
    let db = connect().await;
    let prefix = format!("SCF-reason-{}", Uuid::new_v4());
    cleanup(&db, &prefix).await;
    let company_id = make_company(&db, &prefix).await;
    let issue_id = make_issue(&db, company_id, &prefix).await;
    let _card_id = seed_status_card(&db, company_id, Some(issue_id)).await;

    let issue = StalledGenerationIssue {
        id: issue_id,
        company_id,
        identifier: Some("PAP-42"),
        title: "Recompile board summary",
        status: "blocked",
    };
    let outcome = finalize_status_cards_for_stalled_generation(&db, &issue)
        .await
        .expect("finalize");
    let reason = outcome.failure_reason.expect("reason");
    assert!(reason.contains("PAP-42"));
    assert!(reason.contains("Recompile board summary"));
    assert!(reason.contains("blocked"));

    cleanup(&db, &prefix).await;
}
