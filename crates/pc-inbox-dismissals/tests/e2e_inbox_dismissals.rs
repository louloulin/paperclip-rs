//! End-to-end tests for `pc-inbox-dismissals`.
//!
//! 覆盖：
//! - Dismiss / Snooze / Restore 语义
//! - Upsert 行为（同 `(company_id, user_id, item_key)` 三元组）
//! - active 列表过滤
//! - Hook 6 个时机的回调
//! - 校验错误（snooze_in_past / dismiss_with_until / empty identifier）
//! - JSON 序列化与 JSON-roundtrip
//! - 跨用户 / 跨公司隔离

use chrono::{Duration, Utc};
use pc_inbox_dismissals::{
    codes, filter_rows, InboxDismissalHook as _, InboxDismissalHookEvent,
    InboxDismissalService, InboxDismissalServiceError, RecordingInboxDismissalHook,
};
use pc_repos::Db;
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

// ============================================================================
// Helpers — DB fixtures
// ============================================================================

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect to db")
}

async fn cleanup(db: &Db, tag: &str) {
    let prefix = format!("IDS-{tag}");
    let _ = sqlx::query(
        "DELETE FROM inbox_dismissals WHERE company_id IN \
         (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query("DELETE FROM companies WHERE issue_prefix = $1")
        .bind(&prefix)
        .execute(db.pool())
        .await;
}

async fn make_company(db: &Db, tag: &str) -> Uuid {
    let name = format!("IDS Co {tag} {}", Uuid::new_v4());
    let row = sqlx::query("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
        .bind(&name)
        .bind(format!("IDS-{tag}"))
        .fetch_one(db.pool())
        .await
        .expect("create company");
    row.try_get::<Uuid, _>("id").expect("id column")
}

// ============================================================================
// Service-level 单元测试 (无 DB)
// ============================================================================

#[test]
fn r679_recording_hook_starts_empty() {
    let h = RecordingInboxDismissalHook::new();
    assert!(h.is_empty());
    assert_eq!(h.len(), 0);
}

#[test]
fn r679_recording_hook_clear_works() {
    use pc_inbox_dismissals::InboxDismissalHook as _;
    let h = Arc::new(RecordingInboxDismissalHook::new());
    let cid = Uuid::new_v4();
    h.before_dismiss(cid, "u", "approval:cm1:ap1");
    h.before_restore(cid, "u", "approval:cm1:ap1");
    assert_eq!(h.len(), 2);
    h.clear();
    assert!(h.is_empty());
}

#[test]
fn r679_codes_constants_match_node() {
    assert_eq!(codes::INBOX_DISMISSAL_SNOOZE_IN_PAST, "inbox_dismissal_snooze_in_past");
    assert_eq!(
        codes::INBOX_DISMISSAL_SNOOZE_REQUIRES_UNTIL,
        "inbox_dismissal_snooze_requires_until"
    );
    assert_eq!(
        codes::INBOX_DISMISSAL_DISMISS_WITH_UNTIL,
        "inbox_dismissal_dismiss_with_until"
    );
    assert_eq!(
        codes::INBOX_DISMISSAL_EMPTY_IDENTIFIER,
        "inbox_dismissal_empty_identifier"
    );
}

#[test]
fn r679_error_infer_code_for_validation_errors() {
    let err = InboxDismissalServiceError::Validation("snooze requires snoozed_until".into());
    assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_SNOOZE_REQUIRES_UNTIL));

    let err =
        InboxDismissalServiceError::Validation("snoozed_until must be in the future".into());
    assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_SNOOZE_IN_PAST));

    let err =
        InboxDismissalServiceError::Validation("dismiss must not carry snoozed_until".into());
    assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_DISMISS_WITH_UNTIL));

    let err =
        InboxDismissalServiceError::Validation("user_id/item_key must not be empty".into());
    assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_EMPTY_IDENTIFIER));
}

#[test]
fn r679_error_infer_code_for_db_error_is_none() {
    let err = InboxDismissalServiceError::Database("oops".into());
    assert_eq!(err.infer_code(), None);
}

// ============================================================================
// E2E — 真实 Postgres
// ============================================================================

#[tokio::test]
async fn r679_e2e_dismiss_creates_row_and_get_returns_it() {
    let db = connect().await;
    cleanup(&db, "dismiss").await;
    let cid = make_company(&db, "dismiss").await;

    let svc = InboxDismissalService::new(db.clone());
    let row = svc
        .dismiss(cid, "alice", "approval:cm1:ap1")
        .await
        .expect("dismiss");
    assert_eq!(row.user_id, "alice");
    assert_eq!(row.item_key, "approval:cm1:ap1");
    assert_eq!(row.kind, "dismiss");
    assert!(row.snoozed_until.is_none());

    let fetched = svc
        .get(cid, "alice", "approval:cm1:ap1")
        .await
        .expect("get")
        .expect("some");
    assert_eq!(fetched.id, row.id);

    cleanup(&db, "dismiss").await;
}

#[tokio::test]
async fn r679_e2e_snooze_with_future_until() {
    let db = connect().await;
    cleanup(&db, "snooze").await;
    let cid = make_company(&db, "snooze").await;
    let svc = InboxDismissalService::new(db.clone());

    let future = Utc::now() + Duration::hours(1);
    let row = svc
        .snooze(cid, "bob", "run:cm1:hb1", future)
        .await
        .expect("snooze");
    assert_eq!(row.kind, "snooze");
    let snoozed_until_ts = row.snoozed_until.expect("snoozed_until");
    let delta = (snoozed_until_ts.as_datetime() - future).num_seconds();
    assert!(delta.abs() < 5, "snoozed_until should round-trip within 5s, got delta={delta}");

    cleanup(&db, "snooze").await;
}

#[tokio::test]
async fn r679_e2e_snooze_rejects_past_until() {
    let db = connect().await;
    cleanup(&db, "past").await;
    let cid = make_company(&db, "past").await;
    let svc = InboxDismissalService::new(db.clone());

    let past = Utc::now() - Duration::hours(1);
    let err = svc
        .snooze(cid, "carol", "approval:cm1:ap1", past)
        .await
        .expect_err("should reject past");
    assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_SNOOZE_IN_PAST));

    cleanup(&db, "past").await;
}

#[tokio::test]
async fn r679_e2e_restore_removes_row() {
    let db = connect().await;
    cleanup(&db, "restore").await;
    let cid = make_company(&db, "restore").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.dismiss(cid, "dave", "approval:cm1:ap1")
        .await
        .expect("dismiss");
    let removed = svc
        .restore(cid, "dave", "approval:cm1:ap1")
        .await
        .expect("restore");
    assert!(removed);

    let again = svc
        .restore(cid, "dave", "approval:cm1:ap1")
        .await
        .expect("restore again");
    assert!(!again);

    cleanup(&db, "restore").await;
}

#[tokio::test]
async fn r679_e2e_upsert_changes_kind() {
    let db = connect().await;
    cleanup(&db, "upsert").await;
    let cid = make_company(&db, "upsert").await;
    let svc = InboxDismissalService::new(db.clone());

    // 1) dismiss
    svc.dismiss(cid, "erin", "approval:cm1:ap1")
        .await
        .expect("dismiss");
    // 2) snooze（应 upsert，覆盖 kind）
    let row2 = svc
        .snooze(cid, "erin", "approval:cm1:ap1", Utc::now() + Duration::hours(1))
        .await
        .expect("snooze");
    assert_eq!(row2.kind, "snooze");

    // 3) restore
    // 验证 upsert 后 row.kind 变成了 snooze
    let fetched_mid = svc
        .get(cid, "erin", "approval:cm1:ap1")
        .await
        .expect("get mid")
        .expect("some");
    assert_eq!(fetched_mid.kind, "snooze");

    // restore 删除行
    svc.restore(cid, "erin", "approval:cm1:ap1")
        .await
        .expect("restore");
    let fetched_end = svc
        .get(cid, "erin", "approval:cm1:ap1")
        .await
        .expect("get end");
    assert!(fetched_end.is_none(), "after restore, row should be deleted");

    cleanup(&db, "upsert").await;
}

#[tokio::test]
async fn r679_e2e_list_returns_user_rows() {
    let db = connect().await;
    cleanup(&db, "list").await;
    let cid = make_company(&db, "list").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.dismiss(cid, "frank", "approval:cm1:ap1")
        .await
        .expect("dismiss");
    svc.snooze(cid, "frank", "run:cm1:hb1", Utc::now() + Duration::hours(2))
        .await
        .expect("snooze");

    let list = svc.list(cid, "frank").await.expect("list");
    assert_eq!(list.len(), 2);

    cleanup(&db, "list").await;
}

#[tokio::test]
async fn r679_e2e_list_active_excludes_expired_snooze() {
    let db = connect().await;
    cleanup(&db, "active").await;
    let cid = make_company(&db, "active").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.dismiss(cid, "grace", "approval:cm1:ap1")
        .await
        .expect("dismiss");
    // 插入一条已经过期的 snooze
    let past = Utc::now() - Duration::hours(1);
    let _ = sqlx::query(
        "INSERT INTO inbox_dismissals (company_id, user_id, item_key, kind, dismissed_at, snoozed_until, created_at, updated_at) \
         VALUES ($1, $2, $3, 'snooze', now(), $4, now(), now())",
    )
    .bind(cid)
    .bind("grace")
    .bind("run:cm1:hb1")
    .bind(past)
    .execute(db.pool())
    .await
    .expect("insert past snooze");

    let now = Utc::now();
    let active = svc.list_active(cid, "grace", now).await.expect("list_active");
    assert_eq!(active.len(), 1, "expired snooze should be excluded");
    assert_eq!(active[0].item_key, "approval:cm1:ap1");

    cleanup(&db, "active").await;
}

#[tokio::test]
async fn r679_e2e_count_active_per_company() {
    let db = connect().await;
    cleanup(&db, "count").await;
    let cid = make_company(&db, "count").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.dismiss(cid, "hank", "approval:cm1:ap1")
        .await
        .expect("dismiss");
    svc.snooze(cid, "hank", "run:cm1:hb1", Utc::now() + Duration::hours(2))
        .await
        .expect("snooze");
    svc.dismiss(cid, "ivy", "approval:cm1:ap2")
        .await
        .expect("dismiss ivy");

    let n = svc.count_active(cid, Utc::now()).await.expect("count");
    assert_eq!(n, 3);

    cleanup(&db, "count").await;
}

#[tokio::test]
async fn r679_e2e_dismiss_and_snooze_hooks_fire() {
    let db = connect().await;
    cleanup(&db, "hooks").await;
    let cid = make_company(&db, "hooks").await;

    let hook = Arc::new(RecordingInboxDismissalHook::new());
    let svc = InboxDismissalService::with_hook(db.clone(), hook.clone());

    svc.dismiss(cid, "jack", "approval:cm1:ap1").await.unwrap();
    svc.snooze(cid, "jack", "run:cm1:hb1", Utc::now() + Duration::hours(2))
        .await
        .unwrap();

    let (before_d, after_d) = hook.count_for(pc_repos::inbox::DismissKind::Dismiss);
    let (before_s, after_s) = hook.count_for(pc_repos::inbox::DismissKind::Snooze);

    assert_eq!(before_d, 1);
    assert_eq!(after_d, 1);
    assert_eq!(before_s, 1);
    assert_eq!(after_s, 1);

    cleanup(&db, "hooks").await;
}

#[tokio::test]
async fn r679_e2e_restore_hook_fires() {
    let db = connect().await;
    cleanup(&db, "rhk").await;
    let cid = make_company(&db, "rhk").await;

    let hook = Arc::new(RecordingInboxDismissalHook::new());
    let svc = InboxDismissalService::with_hook(db.clone(), hook.clone());

    svc.dismiss(cid, "kate", "approval:cm1:ap1").await.unwrap();
    let removed = svc.restore(cid, "kate", "approval:cm1:ap1").await.unwrap();
    assert!(removed);

    let (before, after) = hook.restore_count();
    assert_eq!(before, 1);
    assert_eq!(after, 1);

    let events = hook.events();
    let last = events.last().expect("at least one event");
    match last {
        InboxDismissalHookEvent::AfterRestore { removed: r, .. } => assert!(r),
        _ => panic!("expected AfterRestore"),
    }

    cleanup(&db, "rhk").await;
}

#[tokio::test]
async fn r679_e2e_empty_user_item_key_rejected() {
    let db = connect().await;
    cleanup(&db, "empty").await;
    let cid = make_company(&db, "empty").await;
    let svc = InboxDismissalService::new(db.clone());

    let err = svc
        .dismiss(cid, "  ", "approval:cm1:ap1")
        .await
        .expect_err("should reject empty user_id");
    assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_EMPTY_IDENTIFIER));

    let err = svc
        .dismiss(cid, "u", "")
        .await
        .expect_err("should reject empty item_key");
    assert_eq!(err.infer_code(), Some(codes::INBOX_DISMISSAL_EMPTY_IDENTIFIER));

    cleanup(&db, "empty").await;
}

#[tokio::test]
async fn r679_e2e_distinct_users_isolated() {
    let db = connect().await;
    cleanup(&db, "iso-user").await;
    let cid = make_company(&db, "iso-user").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.dismiss(cid, "u1", "approval:cm1:ap1").await.unwrap();
    svc.dismiss(cid, "u2", "approval:cm1:ap1").await.unwrap();

    let r1 = svc.list(cid, "u1").await.unwrap();
    let r2 = svc.list(cid, "u2").await.unwrap();
    assert_eq!(r1.len(), 1);
    assert_eq!(r2.len(), 1);
    assert_ne!(r1[0].id, r2[0].id);

    cleanup(&db, "iso-user").await;
}

#[tokio::test]
async fn r679_e2e_distinct_companies_isolated() {
    let db = connect().await;
    cleanup(&db, "iso-co-a").await;
    cleanup(&db, "iso-co-b").await;
    let cid_a = make_company(&db, "iso-co-a").await;
    let cid_b = make_company(&db, "iso-co-b").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.dismiss(cid_a, "shared", "approval:cm1:ap1")
        .await
        .unwrap();

    let a = svc.list(cid_a, "shared").await.unwrap();
    let b = svc.list(cid_b, "shared").await.unwrap();
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 0);

    cleanup(&db, "iso-co-a").await;
    cleanup(&db, "iso-co-b").await;
}

#[tokio::test]
async fn r679_e2e_snooze_then_dismiss_clears_until() {
    let db = connect().await;
    cleanup(&db, "clear").await;
    let cid = make_company(&db, "clear").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.snooze(cid, "liam", "approval:cm1:ap1", Utc::now() + Duration::hours(2))
        .await
        .unwrap();
    let row = svc
        .dismiss(cid, "liam", "approval:cm1:ap1")
        .await
        .expect("dismiss clears until");
    assert_eq!(row.kind, "dismiss");
    assert!(row.snoozed_until.is_none());

    cleanup(&db, "clear").await;
}

#[tokio::test]
async fn r679_e2e_expire_snoozes_purges_expired() {
    let db = connect().await;
    cleanup(&db, "expire").await;
    let cid = make_company(&db, "expire").await;
    let svc = InboxDismissalService::new(db.clone());

    let past = Utc::now() - Duration::hours(1);
    sqlx::query(
        "INSERT INTO inbox_dismissals (company_id, user_id, item_key, kind, dismissed_at, snoozed_until, created_at, updated_at) \
         VALUES ($1, $2, $3, 'snooze', now(), $4, now(), now())",
    )
    .bind(cid)
    .bind("maya")
    .bind("approval:cm1:ap1")
    .bind(past)
    .execute(db.pool())
    .await
    .unwrap();

    let n = svc.expire_snoozes(Utc::now()).await.expect("expire");
    assert_eq!(n, 1);

    cleanup(&db, "expire").await;
}

#[tokio::test]
async fn r679_e2e_json_serialization_roundtrip() {
    let db = connect().await;
    cleanup(&db, "json").await;
    let cid = make_company(&db, "json").await;
    let svc = InboxDismissalService::new(db.clone());

    let row = svc
        .dismiss(cid, "nina", "approval:cm1:ap1")
        .await
        .expect("dismiss");
    let s = serde_json::to_string(&row).expect("serialize");
    let v: Value = serde_json::from_str(&s).expect("parse");

    assert_eq!(v["companyId"], json!(cid.to_string()));
    assert_eq!(v["userId"], json!("nina"));
    assert_eq!(v["itemKey"], json!("approval:cm1:ap1"));
    assert_eq!(v["kind"], json!("dismiss"));
    assert!(v["dismissedAt"].is_string());
    assert!(v["snoozedUntil"].is_null());

    cleanup(&db, "json").await;
}

#[tokio::test]
async fn r679_e2e_filter_rows_in_memory() {
    let db = connect().await;
    cleanup(&db, "mem").await;
    let cid = make_company(&db, "mem").await;
    let svc = InboxDismissalService::new(db.clone());

    svc.dismiss(cid, "oscar", "approval:cm1:ap1").await.unwrap();
    svc.snooze(cid, "oscar", "run:cm1:hb1", Utc::now() + Duration::hours(2))
        .await
        .unwrap();

    let list = svc.list(cid, "oscar").await.unwrap();
    assert_eq!(list.len(), 2);

    // 仅保留 dismiss
    let only_dismiss = filter_rows(
        list.clone(),
        &pc_inbox_dismissals::InboxDismissalFilter::new()
            .with_kind(pc_repos::inbox::DismissKind::Dismiss),
    );
    assert_eq!(only_dismiss.len(), 1);

    // active 列表 = 全 2 行（snooze 未过期）
    let active = filter_rows(
        list,
        &pc_inbox_dismissals::InboxDismissalFilter::new().with_active_at(Utc::now()),
    );
    assert_eq!(active.len(), 2);

    cleanup(&db, "mem").await;
}
