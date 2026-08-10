//! End-to-end tests for `pc-inbox-agent-policy`.
//!
//! 覆盖：
//! - 默认 policy（materialized=false）
//! - UPSERT 行为（首次 / 二次 update）
//! - allowlist 校验（同公司/跨公司 agent id）
//! - dedup 行为
//! - mode != "allowlist" 时清空 allowedAgentIds
//! - Hook BeforeUpdate / AfterUpdate / AfterGet 触发
//! - JSON 序列化
//! - 跨用户/跨公司隔离
//! - update_unchecked 跳过 allowlist 校验

use pc_inbox_agent_policy::{
    codes, dedup_agent_ids, find_invalid_agent_ids, InboxAgentPolicy,
    InboxAgentPolicyHook as _, InboxAgentPolicyMode, InboxAgentPolicyService,
    NoopInboxAgentPolicyHook, RecordingInboxAgentPolicyHook, UpdateInboxAgentPolicy,
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
    let prefix = format!("IAP-{tag}");
    let _ = sqlx::query(
        "DELETE FROM user_inbox_agent_policies WHERE company_id IN \
         (SELECT id FROM companies WHERE issue_prefix = $1)",
    )
    .bind(&prefix)
    .execute(db.pool())
    .await;
    let _ = sqlx::query(
        "DELETE FROM agents WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
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
    let name = format!("IAP Co {tag} {}", Uuid::new_v4());
    let row = sqlx::query("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
        .bind(&name)
        .bind(format!("IAP-{tag}"))
        .fetch_one(db.pool())
        .await
        .expect("create company");
    row.try_get::<Uuid, _>("id").expect("id column")
}

async fn make_agent(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let row = sqlx::query(
        "INSERT INTO agents (company_id, name, role, status, adapter_type, adapter_config, \
         budget_monthly_cents, spent_monthly_cents) \
         VALUES ($1, $2, 'general', 'idle', 'process', '{}', 0, 0) RETURNING id",
    )
    .bind(company_id)
    .bind(name)
    .fetch_one(db.pool())
    .await
    .expect("create agent");
    row.try_get::<Uuid, _>("id").expect("agent id column")
}

// ============================================================================
// Service-level DTO + Hook 单元测试 (无 DB)
// ============================================================================

#[test]
fn r678_default_policy_structure() {
    let cid = Uuid::new_v4();
    let policy = InboxAgentPolicy {
        company_id: cid,
        user_id: "user-1".into(),
        mode: InboxAgentPolicyMode::Open,
        allowed_agent_ids: Vec::new(),
        materialized: false,
        created_at: None,
        updated_at: None,
    };
    assert_eq!(policy.mode.as_str(), "open");
    assert!(policy.allowed_agent_ids.is_empty());
    assert!(!policy.materialized);
    assert!(policy.created_at.is_none());
    assert!(policy.updated_at.is_none());

    // JSON 序列化
    let v = serde_json::to_value(&policy).unwrap();
    assert_eq!(v["companyId"], serde_json::json!(cid.to_string()));
    assert_eq!(v["userId"], serde_json::json!("user-1"));
    assert_eq!(v["mode"], serde_json::json!("open"));
    assert_eq!(v["allowedAgentIds"], serde_json::json!([]));
    assert_eq!(v["materialized"], serde_json::json!(false));
    assert!(v.get("createdAt").is_none());
    assert!(v.get("updatedAt").is_none());
}

#[test]
fn r678_noop_hook_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<NoopInboxAgentPolicyHook>();
    assert_send_sync::<RecordingInboxAgentPolicyHook>();
}

#[test]
fn r678_recording_hook_records_no_events_by_default() {
    let h = RecordingInboxAgentPolicyHook::new();
    assert!(h.is_empty());
    assert_eq!(h.len(), 0);
    assert_eq!(h.before_update_count(), 0);
    assert_eq!(h.after_update_count(), 0);
    assert_eq!(h.after_get_count(), 0);
}

#[test]
fn r678_recording_hook_clear_works() {
    let h = Arc::new(RecordingInboxAgentPolicyHook::new());
    h.before_update(Uuid::new_v4(), "u1", InboxAgentPolicyMode::Open, &[]);
    assert_eq!(h.len(), 1);
    h.clear();
    assert!(h.is_empty());
}

#[test]
fn r678_recording_hook_after_get_records_payload() {
    let h = Arc::new(RecordingInboxAgentPolicyHook::new());
    let cid = Uuid::new_v4();
    let policy = InboxAgentPolicy {
        company_id: cid,
        user_id: "user-x".into(),
        mode: InboxAgentPolicyMode::Allowlist,
        allowed_agent_ids: vec![Uuid::new_v4()],
        materialized: true,
        created_at: None,
        updated_at: None,
    };
    h.after_get(&policy);
    let events = h.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].variant_name(), "AfterGet");
}

#[test]
fn r678_update_input_open_helper() {
    let input = UpdateInboxAgentPolicy::open();
    assert_eq!(input.mode, InboxAgentPolicyMode::Open);
    assert!(input.allowed_agent_ids.is_empty());
}

#[test]
fn r678_update_input_disabled_helper() {
    let input = UpdateInboxAgentPolicy::disabled();
    assert_eq!(input.mode, InboxAgentPolicyMode::Disabled);
    assert!(input.allowed_agent_ids.is_empty());
}

#[test]
fn r678_update_input_allowlist_helper() {
    let ids = vec![Uuid::new_v4(), Uuid::new_v4()];
    let input = UpdateInboxAgentPolicy::allowlist(ids.clone());
    assert_eq!(input.mode, InboxAgentPolicyMode::Allowlist);
    assert_eq!(input.allowed_agent_ids, ids);
}

#[test]
fn r678_codes_are_stable() {
    assert_eq!(codes::INBOX_AGENT_POLICY_INVALID_AGENTS, "inbox_agent_policy_invalid_agents");
    assert_eq!(codes::INBOX_AGENT_POLICY_INVALID_MODE, "inbox_agent_policy_invalid_mode");
}

#[test]
fn r678_dedup_preserves_order_with_repeats() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let c = Uuid::new_v4();
    let d = Uuid::new_v4();
    let result = dedup_agent_ids(&[a, b, a, c, b, d]);
    assert_eq!(result, vec![a, b, c, d]);
}

#[test]
fn r678_find_invalid_agent_ids_returns_only_unknown() {
    let known_a = Uuid::new_v4();
    let known_b = Uuid::new_v4();
    let unknown = Uuid::new_v4();
    let result =
        find_invalid_agent_ids(&[known_a, unknown, known_b], &[known_a, known_b]);
    assert_eq!(result, vec![unknown]);
}

#[test]
fn r678_find_invalid_returns_empty_when_all_known() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let result = find_invalid_agent_ids(&[a, b], &[a, b]);
    assert!(result.is_empty());
}

#[test]
fn r678_find_invalid_returns_all_when_empty_company() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    let result = find_invalid_agent_ids(&[a, b], &[]);
    assert_eq!(result.len(), 2);
}

#[test]
fn r678_infer_error_code_matches_node() {
    use pc_inbox_agent_policy::InboxAgentPolicyServiceError;
    use pc_repos::RepoError;

    let err: InboxAgentPolicyServiceError =
        RepoError::Invalid("agents outside the company".into()).into();
    assert_eq!(
        InboxAgentPolicyService::infer_error_code(&err),
        Some(codes::INBOX_AGENT_POLICY_INVALID_AGENTS)
    );

    let err: InboxAgentPolicyServiceError =
        RepoError::Invalid("anything else".into()).into();
    assert_eq!(
        InboxAgentPolicyService::infer_error_code(&err),
        Some(codes::INBOX_AGENT_POLICY_INVALID_MODE)
    );

    let err = InboxAgentPolicyServiceError::InvalidMode("oops".into());
    assert_eq!(
        InboxAgentPolicyService::infer_error_code(&err),
        Some(codes::INBOX_AGENT_POLICY_INVALID_MODE)
    );

    let err = InboxAgentPolicyServiceError::Database("x".into());
    assert_eq!(InboxAgentPolicyService::infer_error_code(&err), None);
}

// ============================================================================
// E2E — 真实 Postgres
// ============================================================================

#[tokio::test]
async fn r678_e2e_get_default_when_no_row() {
    let db = connect().await;
    cleanup(&db, "default").await;
    let company_id = make_company(&db, "default").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    let policy = svc.get(company_id, "nobody").await.expect("get default");
    assert_eq!(policy.mode, InboxAgentPolicyMode::Open);
    assert!(policy.allowed_agent_ids.is_empty());
    assert!(!policy.materialized);
    assert!(policy.created_at.is_none());
    assert!(policy.updated_at.is_none());

    cleanup(&db, "default").await;
}

#[tokio::test]
async fn r678_e2e_update_creates_materialized_row() {
    let db = connect().await;
    cleanup(&db, "create").await;
    let company_id = make_company(&db, "create").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    let policy = svc
        .update(company_id, "alice", UpdateInboxAgentPolicy::disabled())
        .await
        .expect("update");
    assert_eq!(policy.mode, InboxAgentPolicyMode::Disabled);
    assert!(policy.materialized);
    assert!(policy.created_at.is_some());
    assert!(policy.updated_at.is_some());

    let re = svc.get(company_id, "alice").await.expect("get re");
    assert!(re.materialized);
    assert_eq!(re.mode, InboxAgentPolicyMode::Disabled);

    cleanup(&db, "create").await;
}

#[tokio::test]
async fn r678_e2e_update_upsert_overwrites_existing() {
    let db = connect().await;
    cleanup(&db, "upsert").await;
    let company_id = make_company(&db, "upsert").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    let p1 = svc
        .update(company_id, "bob", UpdateInboxAgentPolicy::open())
        .await
        .expect("first update");
    let created_at_1 = p1.created_at.unwrap();

    let p2 = svc
        .update(
            company_id,
            "bob",
            UpdateInboxAgentPolicy::disabled(),
        )
        .await
        .expect("second update");
    // UPSERT 不重置 created_at（Postgres EXCLUDED 不含 created_at 列）
    assert_eq!(p2.created_at, Some(created_at_1));
    assert_eq!(p2.mode, InboxAgentPolicyMode::Disabled);
    assert!(p2.updated_at.unwrap().as_datetime() >= created_at_1.as_datetime());

    cleanup(&db, "upsert").await;
}

#[tokio::test]
async fn r678_e2e_update_allowlist_resets_when_mode_changes() {
    let db = connect().await;
    cleanup(&db, "reset").await;
    let company_id = make_company(&db, "reset").await;
    let a1 = make_agent(&db, company_id, "a1").await;
    let a2 = make_agent(&db, company_id, "a2").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    let p1 = svc
        .update(
            company_id,
            "carol",
            UpdateInboxAgentPolicy::allowlist(vec![a1, a2]),
        )
        .await
        .expect("allowlist");
    assert_eq!(p1.allowed_agent_ids.len(), 2);

    let p2 = svc
        .update(company_id, "carol", UpdateInboxAgentPolicy::open())
        .await
        .expect("open");
    assert_eq!(p2.mode, InboxAgentPolicyMode::Open);
    assert!(p2.allowed_agent_ids.is_empty());

    let p3 = svc
        .update(company_id, "carol", UpdateInboxAgentPolicy::disabled())
        .await
        .expect("disabled");
    assert!(p3.allowed_agent_ids.is_empty());

    cleanup(&db, "reset").await;
}

#[tokio::test]
async fn r678_e2e_allowlist_dedupes_repeated_ids() {
    let db = connect().await;
    cleanup(&db, "dedup").await;
    let company_id = make_company(&db, "dedup").await;
    let a1 = make_agent(&db, company_id, "a1").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    let p = svc
        .update(
            company_id,
            "dan",
            UpdateInboxAgentPolicy::allowlist(vec![a1, a1, a1]),
        )
        .await
        .expect("dedup update");
    assert_eq!(p.allowed_agent_ids.len(), 1);
    assert_eq!(p.allowed_agent_ids[0], a1);

    cleanup(&db, "dedup").await;
}

#[tokio::test]
async fn r678_e2e_allowlist_rejects_invalid_agent_ids() {
    let db = connect().await;
    cleanup(&db, "invalid").await;
    cleanup(&db, "invalid-other").await;
    let cid = make_company(&db, "invalid").await;
    let other_cid = make_company(&db, "invalid-other").await;

    let valid = make_agent(&db, cid, "valid-agt").await;
    let other_company_agent = make_agent(&db, other_cid, "other-agt").await;
    let totally_unknown = Uuid::new_v4();

    let svc = InboxAgentPolicyService::new(db.clone());
    let err = svc
        .update(
            cid,
            "erin",
            UpdateInboxAgentPolicy::allowlist(vec![valid, other_company_agent, totally_unknown]),
        )
        .await
        .expect_err("should reject invalid ids");
    let code = InboxAgentPolicyService::infer_error_code(&err);
    assert_eq!(code, Some(codes::INBOX_AGENT_POLICY_INVALID_AGENTS));

    cleanup(&db, "invalid").await;
    cleanup(&db, "invalid-other").await;
}

#[tokio::test]
async fn r678_e2e_allowlist_accepts_only_valid() {
    let db = connect().await;
    cleanup(&db, "valid").await;
    let cid = make_company(&db, "valid").await;
    let a1 = make_agent(&db, cid, "a1").await;
    let a2 = make_agent(&db, cid, "a2").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    let p = svc
        .update(
            cid,
            "frank",
            UpdateInboxAgentPolicy::allowlist(vec![a1, a2]),
        )
        .await
        .expect("valid update");
    assert_eq!(p.allowed_agent_ids.len(), 2);

    cleanup(&db, "valid").await;
}

#[tokio::test]
async fn r678_e2e_after_update_hook_fires() {
    let db = connect().await;
    cleanup(&db, "hookupd").await;
    let cid = make_company(&db, "hookupd").await;

    let hook = Arc::new(RecordingInboxAgentPolicyHook::new());
    let svc = InboxAgentPolicyService::with_hook(db.clone(), hook.clone());

    svc.update(cid, "grace", UpdateInboxAgentPolicy::open())
        .await
        .expect("update");

    assert_eq!(hook.before_update_count(), 1);
    assert_eq!(hook.after_update_count(), 1);

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].variant_name(), "BeforeUpdate");
    assert_eq!(events[1].variant_name(), "AfterUpdate");

    cleanup(&db, "hookupd").await;
}

#[tokio::test]
async fn r678_e2e_after_get_hook_fires() {
    let db = connect().await;
    cleanup(&db, "hookget").await;
    let cid = make_company(&db, "hookget").await;

    let hook = Arc::new(RecordingInboxAgentPolicyHook::new());
    let svc = InboxAgentPolicyService::with_hook(db.clone(), hook.clone());

    let _ = svc.get(cid, "hank").await.expect("get default");
    let _ = svc.get(cid, "hank").await.expect("get default again");

    assert_eq!(hook.after_get_count(), 2);
    assert_eq!(hook.before_update_count(), 0);
    assert_eq!(hook.after_update_count(), 0);

    cleanup(&db, "hookget").await;
}

#[tokio::test]
async fn r678_e2e_delete_removes_row() {
    let db = connect().await;
    cleanup(&db, "del").await;
    let cid = make_company(&db, "del").await;
    let svc = InboxAgentPolicyService::new(db.clone());

    svc.update(cid, "ivy", UpdateInboxAgentPolicy::disabled())
        .await
        .expect("create");
    let pre = svc.get(cid, "ivy").await.expect("get pre");
    assert!(pre.materialized);

    let removed = svc.delete(cid, "ivy").await.expect("delete");
    assert_eq!(removed, 1);
    let post = svc.get(cid, "ivy").await.expect("get post");
    assert!(!post.materialized);

    let again = svc.delete(cid, "ivy").await.expect("delete again");
    assert_eq!(again, 0);

    cleanup(&db, "del").await;
}

#[tokio::test]
async fn r678_e2e_json_serialization_roundtrip() {
    let db = connect().await;
    cleanup(&db, "json").await;
    let cid = make_company(&db, "json").await;
    let a1 = make_agent(&db, cid, "a1").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    let p = svc
        .update(
            cid,
            "jane",
            UpdateInboxAgentPolicy::allowlist(vec![a1]),
        )
        .await
        .expect("update");
    let json_str = serde_json::to_string(&p).expect("serialize");
    let v: Value = serde_json::from_str(&json_str).expect("parse");

    assert_eq!(v["mode"], json!("allowlist"));
    assert_eq!(v["materialized"], json!(true));
    assert!(v["allowedAgentIds"].is_array());
    assert_eq!(v["allowedAgentIds"].as_array().unwrap().len(), 1);
    assert!(v["createdAt"].is_string());
    assert!(v["updatedAt"].is_string());

    cleanup(&db, "json").await;
}

#[tokio::test]
async fn r678_e2e_distinct_users_distinct_rows() {
    let db = connect().await;
    cleanup(&db, "distinct").await;
    let cid = make_company(&db, "distinct").await;
    let svc = InboxAgentPolicyService::new(db.clone());

    let pa = svc
        .update(cid, "user_a", UpdateInboxAgentPolicy::open())
        .await
        .expect("a");
    let pb = svc
        .update(cid, "user_b", UpdateInboxAgentPolicy::disabled())
        .await
        .expect("b");

    assert_eq!(pa.user_id, "user_a");
    assert_eq!(pb.user_id, "user_b");
    assert_eq!(pa.mode, InboxAgentPolicyMode::Open);
    assert_eq!(pb.mode, InboxAgentPolicyMode::Disabled);

    svc.update(cid, "user_a", UpdateInboxAgentPolicy::allowlist(vec![]))
        .await
        .expect("a again");
    let pb_after = svc.get(cid, "user_b").await.expect("b re");
    assert_eq!(pb_after.mode, InboxAgentPolicyMode::Disabled);

    cleanup(&db, "distinct").await;
}

#[tokio::test]
async fn r678_e2e_distinct_companies_isolated() {
    let db = connect().await;
    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
    let cid_a = make_company(&db, "iso-a").await;
    let cid_b = make_company(&db, "iso-b").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    svc.update(cid_a, "shared-user", UpdateInboxAgentPolicy::disabled())
        .await
        .expect("a update");

    let pa = svc.get(cid_a, "shared-user").await.expect("a get");
    let pb = svc.get(cid_b, "shared-user").await.expect("b get");

    assert!(pa.materialized);
    assert_eq!(pa.mode, InboxAgentPolicyMode::Disabled);
    assert!(!pb.materialized);
    assert_eq!(pb.mode, InboxAgentPolicyMode::Open);

    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
}

#[tokio::test]
async fn r678_e2e_update_unchecked_skips_allowlist_validation() {
    let db = connect().await;
    cleanup(&db, "unc-a").await;
    cleanup(&db, "unc-b").await;
    let cid = make_company(&db, "unc-a").await;
    let other = make_company(&db, "unc-b").await;
    let a_local = make_agent(&db, cid, "local").await;
    let a_other = make_agent(&db, other, "other-agt").await;

    let svc = InboxAgentPolicyService::new(db.clone());
    // update() 应拒绝
    let err = svc
        .update(
            cid,
            "kate",
            UpdateInboxAgentPolicy::allowlist(vec![a_local, a_other]),
        )
        .await
        .expect_err("should fail");
    assert_eq!(
        InboxAgentPolicyService::infer_error_code(&err),
        Some(codes::INBOX_AGENT_POLICY_INVALID_AGENTS)
    );

    // update_unchecked() 应成功
    let policy = svc
        .update_unchecked(
            cid,
            "kate",
            UpdateInboxAgentPolicy::allowlist(vec![a_local, a_other]),
        )
        .await
        .expect("unchecked should succeed");
    assert_eq!(policy.mode, InboxAgentPolicyMode::Allowlist);
    assert_eq!(policy.allowed_agent_ids.len(), 2);

    cleanup(&db, "unc-a").await;
    cleanup(&db, "unc-b").await;
}
