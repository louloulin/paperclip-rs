//! Collector — Export fidelity counts DB 聚合查询。
//!
//! 与 Node `collectExportFidelityCounts(db, companyId)` 1:1 对齐。
//! 复用 [`pc_core::portability_fidelity`] 的 typed [`ExportFidelityCounts`]
//! 与警告构造函数；DB IO 仅在本文件内。

use pc_core::portability_fidelity::ExportFidelityCounts;
use pc_repos::Db;
use sqlx::Row;
use uuid::Uuid;

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

    async fn count(pool: &sqlx::PgPool, sql: &str, cid: Uuid) -> sqlx::Result<i64> {
        let row = sqlx::query(sql).bind(cid).fetch_one(pool).await?;
        row.try_get::<i64, _>("c")
    }

    // 并发执行 10 个 COUNT
    let label_definitions = count(
        pool,
        "SELECT COUNT(*) AS c FROM labels WHERE company_id = $1",
        company_id,
    );
    let issue_label_references = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_labels WHERE company_id = $1",
        company_id,
    );
    let issue_blocker_relations = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_relations WHERE company_id = $1 AND type = 'blocks'",
        company_id,
    );
    let issue_documents = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_documents WHERE company_id = $1",
        company_id,
    );
    let issue_work_products = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_work_products WHERE company_id = $1",
        company_id,
    );
    let issue_attachments = count(
        pool,
        "SELECT COUNT(*) AS c FROM issue_attachments WHERE company_id = $1",
        company_id,
    );
    let approvals = count(
        pool,
        "SELECT COUNT(*) AS c FROM approvals WHERE company_id = $1",
        company_id,
    );
    let cost_events = count(
        pool,
        "SELECT COUNT(*) AS c FROM cost_events WHERE company_id = $1",
        company_id,
    );
    let activity_log_entries = count(
        pool,
        "SELECT COUNT(*) AS c FROM activity_log WHERE company_id = $1",
        company_id,
    );
    let issue_monitors = count(
        pool,
        "SELECT COUNT(*) AS c FROM issues WHERE company_id = $1          AND (monitor_next_check_at IS NOT NULL OR monitor_scheduled_by IS NOT NULL)",
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
    ) = tokio::try_join!(
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
    )?;

    Ok(ExportFidelityCounts {
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
    })
}
