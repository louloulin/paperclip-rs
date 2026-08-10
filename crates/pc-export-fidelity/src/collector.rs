//! Collector —— Export fidelity counts 聚合查询。
//!
//! 与 Node `collectExportFidelityCounts(db, companyId)` 1:1 对齐。

use std::collections::BTreeMap;

use sqlx::Row;
use uuid::Uuid;

use pc_repos::Db;

use crate::types::{ExportFidelityCounts, EXPORT_FIDELITY_COUNT_KEYS};

/// 收集 company 维度的各表行数。
///
/// 与 Node `collectExportFidelityCounts(db, companyId)` 1:1 对齐：
/// - 10 个 COUNT(*) 并发执行（Node `Promise.all`）
/// - `issueBlockerRelations` 限定 `type = 'blocks'`
/// - `issueMonitors` 限定 `monitor_next_check_at IS NOT NULL OR monitor_scheduled_by IS NOT NULL`
pub async fn collect_export_fidelity_counts(
    db: &Db,
    company_id: Uuid,
) -> sqlx::Result<ExportFidelityCounts> {
    let pool = db.pool();

    // 简单 sub-helper
    async fn count(pool: &sqlx::PgPool, sql: &str, cid: Uuid) -> sqlx::Result<i64> {
        let row = sqlx::query(sql).bind(cid).fetch_one(pool).await?;
        row.try_get::<i64, _>("c")
    }

    // 并发执行 10 个 COUNT
    let f1 = count(
        pool,
        "SELECT COUNT(*) AS c FROM labels WHERE company_id = $1",
        company_id,
    );
    let f2 = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_labels WHERE company_id = $1",
        company_id,
    );
    let f3 = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_relations WHERE company_id = $1 AND type = 'blocks'",
        company_id,
    );
    let f4 = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_documents WHERE company_id = $1",
        company_id,
    );
    let f5 = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_work_products WHERE company_id = $1",
        company_id,
    );
    let f6 = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_attachments WHERE company_id = $1",
        company_id,
    );
    let f7 = count(
        pool,
        "SELECT COUNT(*) AS c FROM approvals WHERE company_id = $1",
        company_id,
    );
    let f8 = count(
        pool,
        "SELECT COUNT(*) AS c FROM cost_events WHERE company_id = $1",
        company_id,
    );
    let f9 = count(
        pool,
        "SELECT COUNT(*) AS c FROM activity_log WHERE company_id = $1",
        company_id,
    );
    let f10 = count(
        pool,
        "SELECT COUNT(*) AS c FROM issues WHERE company_id = $1 \
         AND (monitor_next_check_at IS NOT NULL OR monitor_scheduled_by IS NOT NULL)",
        company_id,
    );

    let (
        label_definitions,
        issue_label_references,
        issue_blocker_relations,
        issue_documents,
        issue_work_products,
        issue_attachments,
        approvals,
        cost_events,
        activity_log_entries,
        issue_monitors,
    ) = tokio::try_join!(f1, f2, f3, f4, f5, f6, f7, f8, f9, f10)?;

    let mut counts: ExportFidelityCounts = BTreeMap::new();
    let values: [(&str, i64); 10] = [
        (EXPORT_FIDELITY_COUNT_KEYS[0], label_definitions),
        (EXPORT_FIDELITY_COUNT_KEYS[1], issue_label_references),
        (EXPORT_FIDELITY_COUNT_KEYS[2], issue_blocker_relations),
        (EXPORT_FIDELITY_COUNT_KEYS[3], issue_documents),
        (EXPORT_FIDELITY_COUNT_KEYS[4], issue_work_products),
        (EXPORT_FIDELITY_COUNT_KEYS[5], issue_attachments),
        (EXPORT_FIDELITY_COUNT_KEYS[6], approvals),
        (EXPORT_FIDELITY_COUNT_KEYS[7], cost_events),
        (EXPORT_FIDELITY_COUNT_KEYS[8], activity_log_entries),
        (EXPORT_FIDELITY_COUNT_KEYS[9], issue_monitors),
    ];
    for (k, v) in values {
        counts.insert(k.to_string(), v);
    }
    Ok(counts)
}
