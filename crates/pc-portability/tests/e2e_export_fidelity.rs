//! E2E test: export fidelity counts collector
//! 1:1 port of Node paperclip/server/src/services/export-fidelity.ts e2e suite
//!
//! 运行: cargo test -p pc-portability --test e2e_export_fidelity -- --include-ignored

use pc_core::portability_fidelity::{
    build_export_fidelity_report, build_export_fidelity_warnings, ExportFidelityCounts,
    PortabilityFidelitySeverity, EXPORT_FIDELITY_REPORT_SCHEMA,
};
use pc_portability::fidelity_collector::collect_export_fidelity_counts;
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const DB_URL: &str = "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos";

async fn connect() -> Db {
    Db::connect(DB_URL, 2, 1).await.expect("connect db")
}

async fn cleanup(db: &Db, suffix: &str) {
    let name_pattern = format!("Export Fidelity Test {}%", suffix);
    let _ = sqlx::query(
        "DELETE FROM labels WHERE company_id IN (SELECT id FROM companies WHERE name LIKE $1)",
    )
    .bind(name_pattern.clone())
    .execute(db.pool())
    .await;
    let _ = sqlx::query("DELETE FROM companies WHERE name LIKE $1")
        .bind(name_pattern)
        .execute(db.pool())
        .await;
}

async fn make_company(db: &Db, suffix: &str) -> Uuid {
    let id = Uuid::new_v4();
    // issue_prefix is unique-indexed; derive per-test prefix from suffix
    // and a uuid tail to avoid collisions across runs sharing the DB.
    let prefix = format!(
        "E{}{}",
        suffix,
        Uuid::new_v4().simple().to_string().chars().take(4).collect::<String>()
    );
    sqlx::query("INSERT INTO companies (id, name, issue_prefix) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(format!("Export Fidelity Test {suffix}"))
        .bind(prefix)
        .execute(db.pool())
        .await
        .expect("insert company");
    id
}

async fn make_label(db: &Db, cid: Uuid, name: &str) {
    sqlx::query("INSERT INTO labels (company_id, name, color) VALUES ($1, $2, $3)")
        .bind(cid)
        .bind(name)
        .bind("#fff")
        .execute(db.pool())
        .await
        .expect("insert label");
}

async fn make_agent(db: &Db, cid: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO agents (id, company_id, name, role, adapter_type, status, adapter_config, permissions, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), now())",
    )
    .bind(id)
    .bind(cid)
    .bind(format!("Agent-{id}"))
    .bind("worker")
    .bind("test")
    .bind("idle")
    .bind(serde_json::json!({}))
    .bind(serde_json::json!({}))
    .execute(db.pool())
    .await
    .expect("insert agent");
    id
}

#[tokio::test]
async fn e2e_collect_counts_returns_zero_for_empty_company() {
    let db = connect().await;
    cleanup(&db, "empty").await;
    let cid = make_company(&db, "empty").await;
    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    assert_eq!(counts, ExportFidelityCounts::ZERO);
    cleanup(&db, "empty").await;
}

#[tokio::test]
async fn e2e_collect_counts_for_company_with_labels() {
    let db = connect().await;
    cleanup(&db, "lbl").await;
    let cid = make_company(&db, "lbl").await;
    make_label(&db, cid, "l1").await;
    make_label(&db, cid, "l2").await;
    make_label(&db, cid, "l3").await;
    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    assert_eq!(counts.label_definitions, 3);
    cleanup(&db, "lbl").await;
}

#[tokio::test]
async fn e2e_warnings_triggered_for_unsupported_data() {
    let db = connect().await;
    cleanup(&db, "warn").await;
    let cid = make_company(&db, "warn").await;
    let aid = make_agent(&db, cid).await;
    sqlx::query("INSERT INTO approvals (company_id, type, status, payload) VALUES ($1, $2, 'pending', '{}'::jsonb)")
        .bind(cid)
        .bind(Uuid::new_v4().to_string())
        .execute(db.pool())
        .await
        .expect("insert approval");
    for _ in 0..3 {
        sqlx::query(
            "INSERT INTO cost_events (company_id, agent_id, provider, model, cost_cents, occurred_at) VALUES ($1, $2, 'test', 'test', 100, now())",
        )
        .bind(cid)
        .bind(aid)
        .execute(db.pool())
        .await
        .expect("insert cost");
    }
    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    let warnings = build_export_fidelity_warnings(&counts);
    let codes: Vec<_> = warnings.iter().map(|w| w.code.as_str()).collect();
    assert!(codes.contains(&"approvals_not_exported"));
    assert!(codes.contains(&"cost_history_not_exported"));
    assert!(!codes.contains(&"activity_history_not_exported"));
    let a = warnings
        .iter()
        .find(|w| w.code == "approvals_not_exported")
        .unwrap();
    assert!(a.message.contains("1 approval is"));
    assert_eq!(a.severity, PortabilityFidelitySeverity::Warning);
    let c = warnings
        .iter()
        .find(|w| w.code == "cost_history_not_exported")
        .unwrap();
    assert!(c.message.contains("3 cost events are"));
    cleanup(&db, "warn").await;
}

#[tokio::test]
async fn e2e_full_report_has_schema_and_counts() {
    let db = connect().await;
    cleanup(&db, "full").await;
    let cid = make_company(&db, "full").await;
    make_label(&db, cid, "l1").await;
    let counts = collect_export_fidelity_counts(&db, cid)
        .await
        .expect("collect");
    let report = build_export_fidelity_report(&cid.to_string(), counts, None);
    assert_eq!(report.schema, EXPORT_FIDELITY_REPORT_SCHEMA);
    assert_eq!(report.company_id, cid.to_string());
    assert_eq!(report.counts.label_definitions, 1);
    assert!(report.warnings.is_empty());
    cleanup(&db, "full").await;
}

#[tokio::test]
async fn e2e_distinct_companies_isolated() {
    let db = connect().await;
    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
    let cid_a = make_company(&db, "iso-a").await;
    let cid_b = make_company(&db, "iso-b").await;
    make_label(&db, cid_a, "l1").await;
    make_label(&db, cid_a, "l2").await;
    make_label(&db, cid_b, "l3").await;
    let a = collect_export_fidelity_counts(&db, cid_a).await.expect("a");
    let b = collect_export_fidelity_counts(&db, cid_b).await.expect("b");
    assert_eq!(a.label_definitions, 2);
    assert_eq!(b.label_definitions, 1);
    cleanup(&db, "iso-a").await;
    cleanup(&db, "iso-b").await;
}

#[tokio::test]
async fn e2e_report_with_empty_counts_no_warnings() {
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
async fn e2e_counts_json_roundtrip_with_camel_case_keys() {
    let counts = ExportFidelityCounts {
        label_definitions: 1,
        issue_attachments: 5,
        ..ExportFidelityCounts::ZERO
    };
    let report = build_export_fidelity_report("c-1", counts, None);
    let s = serde_json::to_string(&report).expect("serialize");
    let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
    assert_eq!(v["counts"]["labelDefinitions"], json!(1));
    assert_eq!(v["counts"]["issueAttachments"], json!(5));
    assert_eq!(v["schema"], json!(EXPORT_FIDELITY_REPORT_SCHEMA));
}
