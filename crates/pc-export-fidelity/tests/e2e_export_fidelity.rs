//! End-to-end tests for `pc-export-fidelity` against real Postgres.
//!
//! 覆盖：
//! - `collect_export_fidelity_counts` —— 真实 COUNT 查询
//! - 全 report（counts + warnings + schema）的构造
//! - 跨 company 隔离

use pc_export_fidelity::{
    build_export_fidelity_report, collect_export_fidelity_counts,
    build_export_fidelity_warnings,
};
use pc_repos::Db;
use serde_json::json;
use sqlx::Row;
use std::collections::BTreeMap;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(DB_URL, 5, 1).await.expect("connect")
}

async fn cleanup(db: &Db, tag: &str) {
    let prefix = format!("EF-{tag}");
    // 删除各子表（依赖 companies）
    for table in [
        "labels",
        "issue_labels",
        "issue_relations",
        "issue_documents",
        "issue_work_products",
        "issue_attachments",
        "approvals",
        "cost_events",
        "activity_log",
    ] {
        let sql = format!(
            "DELETE FROM {table} WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)"
        );
        let _ = sqlx::query(&sql)
            .bind(&prefix)
            .execute(db.pool())
            .await;
    }
    let _ = sqlx::query(
        "DELETE FROM issues WHERE company_id IN (SELECT id FROM companies WHERE issue_prefix = $1)",
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
    let name = format!("EF Co {tag} {}", Uuid::new_v4());
    let row = sqlx::query("INSERT INTO companies (name, issue_prefix) VALUES ($1, $2) RETURNING id")
        .bind(&name)
        .bind(format!("EF-{tag}-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("create company");
    row.try_get::<Uuid, _>("id").expect("id")
}

async fn make_issue(db: &Db, company_id: Uuid, title: &str) -> Uuid {
    let row = sqlx::query(
        "INSERT INTO issues (company_id, title, status, created_by_user_id) \
         VALUES ($1, $2, 'todo', $3) RETURNING id",
    )
    .bind(company_id)
    .bind(title)
    .bind(Uuid::new_v4().to_string())
    .fetch_one(db.pool())
    .await
    .expect("create issue");
    row.try_get::<Uuid, _>("id").expect("issue id")
}

async fn make_approval(db: &Db, company_id: Uuid) {
    sqlx::query(
        "INSERT INTO approvals (company_id, type, status, requested_by_user_id, payload) \
         VALUES ($1, 'test', 'pending', $2, '{}')",
    )
    .bind(company_id)
    .bind(Uuid::new_v4().to_string())
    .execute(db.pool())
    .await
    .expect("insert approval");
}

async fn make_cost_event(db: &Db, company_id: Uuid, agent_id: Uuid) {
    sqlx::query(
        "INSERT INTO cost_events (company_id, agent_id, provider, model, input_tokens, output_tokens, cost_cents, occurred_at) \
         VALUES ($1, $2, 'test-provider', 'test-model', 100, 50, 10, now())",
    )
    .bind(company_id)
    .bind(agent_id)
    .execute(db.pool())
    .await
    .expect("insert cost event");
}

async fn make_label(db: &Db, company_id: Uuid, name: &str) -> Uuid {
    let row = sqlx::query(
        "INSERT INTO labels (company_id, name, color) \
         VALUES ($1, $2, '#000') RETURNING id",
    )
    .bind(company_id)
    .bind(name)
    .fetch_one(db.pool())
    .await
    .expect("insert label");
    row.try_get::<Uuid, _>("id").expect("label id")
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
    row.try_get::<Uuid, _>("id").expect("agent id")
}

#[tokio::test]
async fn r682_e2e_collect_counts_on_empty_company() {
    let db = connect().await;
    cleanup(&db, "empty").await;
    let cid = make_company(&db, "empty").await;

    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    // 10 个 key 应都在
    assert_eq!(counts.len(), 10);
    // 全部为 0
    for v in counts.values() {
        assert_eq!(*v, 0);
    }

    cleanup(&db, "empty").await;
}

#[tokio::test]
async fn r682_e2e_collect_counts_with_populated_tables() {
    let db = connect().await;
    cleanup(&db, "pop").await;
    let cid = make_company(&db, "pop").await;

    // 插入 2 个 labels
    let l1 = make_label(&db, cid, "bug").await;
    make_label(&db, cid, "feature").await;
    // 1 个 issue
    let issue_id = make_issue(&db, cid, "Main issue").await;
    // 1 个 issue_label
    sqlx::query(
        "INSERT INTO issue_labels (company_id, issue_id, label_id) \
         VALUES ($1, $2, $3)",
    )
    .bind(cid)
    .bind(issue_id)
    .bind(l1)
    .execute(db.pool())
    .await
    .expect("insert issue_labels");
    // 1 个 approval
    make_approval(&db, cid).await;
    // 1 agent + 1 cost event
    let agent_id = make_agent(&db, cid, "agent-1").await;
    make_cost_event(&db, cid, agent_id).await;

    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    assert_eq!(counts.get("labelDefinitions"), Some(&2));
    assert_eq!(counts.get("issueLabelReferences"), Some(&1));
    assert_eq!(counts.get("approvals"), Some(&1));
    assert_eq!(counts.get("costEvents"), Some(&1));
    // 其余仍为 0
    assert_eq!(counts.get("issueDocuments"), Some(&0));
    assert_eq!(counts.get("issueWorkProducts"), Some(&0));
    assert_eq!(counts.get("issueAttachments"), Some(&0));
    assert_eq!(counts.get("issueBlockerRelations"), Some(&0));
    assert_eq!(counts.get("issueMonitors"), Some(&0));
    assert_eq!(counts.get("activityLogEntries"), Some(&0));

    cleanup(&db, "pop").await;
}

#[tokio::test]
async fn r682_e2e_warnings_appear_only_for_supported_keys() {
    let db = connect().await;
    cleanup(&db, "warn").await;
    let cid = make_company(&db, "warn").await;

    let agent_id = make_agent(&db, cid, "agt").await;
    make_cost_event(&db, cid, agent_id).await;
    make_cost_event(&db, cid, agent_id).await;
    // 1 approval, 2 cost events
    make_approval(&db, cid).await;
    let agent_id2 = make_agent(&db, cid, "agt2").await;
    make_cost_event(&db, cid, agent_id2).await;

    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    let warnings = build_export_fidelity_warnings(&counts);
    // approvals (1), costEvents (3) 触发
    assert_eq!(warnings.len(), 2);
    let codes: Vec<_> = warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(codes.contains(&"approvals_not_exported"));
    assert!(codes.contains(&"cost_history_not_exported"));
    // activityLogEntries = 0 → 不触发
    assert!(!codes.contains(&"activity_history_not_exported"));
    // singular form (count=1)
    let approvals_w = warnings.iter().find(|w| w.code == "approvals_not_exported").unwrap();
    assert!(approvals_w.message.contains("1 approval is"));
    // plural form (count=3)
    let cost_w = warnings.iter().find(|w| w.code == "cost_history_not_exported").unwrap();
    assert!(cost_w.message.contains("3 cost events are"));

    cleanup(&db, "warn").await;
}

#[tokio::test]
async fn r682_e2e_full_report_has_schema_and_counts() {
    let db = connect().await;
    cleanup(&db, "full").await;
    let cid = make_company(&db, "full").await;
    let _l1 = make_label(&db, cid, "l1").await;

    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    let report = build_export_fidelity_report(&cid.to_string(), counts, None);
    assert_eq!(report.schema, "paperclip-export-fidelity-v1");
    assert_eq!(report.company_id, cid.to_string());
    assert_eq!(report.counts.get("labelDefinitions"), Some(&1));
    assert!(report.warnings.is_empty());

    let json = serde_json::to_string(&report).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parse");
    assert_eq!(v["schema"], json!("paperclip-export-fidelity-v1"));
    assert_eq!(v["companyId"], json!(cid.to_string()));
    assert_eq!(v["counts"]["labelDefinitions"], json!(1));

    cleanup(&db, "full").await;
}

#[tokio::test]
async fn r682_e2e_distinct_companies_isolated() {
    let db = connect().await;
    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
    let cid_a = make_company(&db, "iso-a").await;
    let cid_b = make_company(&db, "iso-b").await;

    make_label(&db, cid_a, "l1").await;
    make_label(&db, cid_a, "l2").await;
    make_label(&db, cid_b, "l3").await; // 另一个 company

    let counts_a = collect_export_fidelity_counts(&db, cid_a)
        .await
        .expect("a");
    let counts_b = collect_export_fidelity_counts(&db, cid_b)
        .await
        .expect("b");
    assert_eq!(counts_a.get("labelDefinitions"), Some(&2));
    assert_eq!(counts_b.get("labelDefinitions"), Some(&1));

    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
}

#[tokio::test]
async fn r682_e2e_warnings_emitted_for_activity_log() {
    let db = connect().await;
    cleanup(&db, "act").await;
    let cid = make_company(&db, "act").await;

    // 直接插 activity_log
    sqlx::query(
        "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type, entity_id) \
         VALUES ($1, 'user', $2, 'test', 'company', 'c-1')",
    )
    .bind(cid)
    .bind(Uuid::new_v4().to_string())
    .execute(db.pool())
    .await
    .expect("insert activity");

    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    let warnings = build_export_fidelity_warnings(&counts);
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].code, "activity_history_not_exported");
    assert!(warnings[0].message.contains("1 activity log entry is"));

    cleanup(&db, "act").await;
}

#[tokio::test]
async fn r682_e2e_report_with_empty_counts_no_warnings() {
    let db = connect().await;
    cleanup(&db, "z").await;
    let cid = make_company(&db, "z").await;
    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    let warnings = build_export_fidelity_warnings(&counts);
    assert!(warnings.is_empty());
    let report = build_export_fidelity_report(&cid.to_string(), counts, None);
    assert_eq!(report.warnings.len(), 0);
    cleanup(&db, "z").await;
}

#[tokio::test]
async fn r682_e2e_empty_counts_struct_roundtrip() {
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    counts.insert("labelDefinitions".to_string(), 0);
    counts.insert("issueLabelReferences".to_string(), 0);
    counts.insert("issueBlockerRelations".to_string(), 0);
    counts.insert("issueDocuments".to_string(), 0);
    counts.insert("issueWorkProducts".to_string(), 0);
    counts.insert("issueAttachments".to_string(), 0);
    counts.insert("approvals".to_string(), 0);
    counts.insert("costEvents".to_string(), 0);
    counts.insert("activityLogEntries".to_string(), 0);
    counts.insert("issueMonitors".to_string(), 0);

    let report = build_export_fidelity_report("c-1", counts, None);
    let s = serde_json::to_string(&report).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
    assert!(v["warnings"].as_array().unwrap().is_empty());
    assert_eq!(v["counts"]["labelDefinitions"], json!(0));
}
