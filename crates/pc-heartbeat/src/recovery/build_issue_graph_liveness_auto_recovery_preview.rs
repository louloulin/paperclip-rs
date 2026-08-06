//! `buildIssueGraphLivenessAutoRecoveryPreview` 预览构建器。
//!
//! 对齐 Node `services/recovery/service.ts` 的
//! `buildIssueGraphLivenessAutoRecoveryPreview`：基于当前 findings +
//! dependency updated_at 渲染一个只读预览（不创建 escalation），用于
//! `GET /instance/settings/experimental/preview-issue-graph-liveness-auto-recovery`。
//!
//! 数据流：
//! 1. `collect_issue_graph_liveness_findings` → 当前所有 findings
//! 2. `load_liveness_dependency_updated_at_by_issue` → 关联 updated_at map
//! 3. 对每个 finding 计算 `latest_dependency_updated_at`
//! 4. < cutoff → skippedOutsideLookback
//! 5. ≥ cutoff → 进 items（同时加载 recoveryIssue 元信息）
//!
//! 设计：
//! - 纯编排器：所有 DB 操作复用既有 helpers
//! - DB 副作用只 2 处：collect + load_updated_at + recovery 元信息加载
//! - 时间窗逻辑纯函数化：cutoff = now - lookbackHours * 3600s
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;
use uuid::Uuid;

use pc_repos::Db;

use super::collect_issue_graph_liveness_findings::{
    collect_issue_graph_liveness_findings, CollectFindingsOptions,
};
use super::issue_graph_liveness::IssueLivenessFinding;
use super::liveness_dependency_cleanup::{
    is_finding_inside_auto_recovery_lookback, latest_dependency_updated_at_for_finding,
    load_liveness_dependency_updated_at_by_issue, normalize_lookback_hours,
};

// ============================================================================
// Public types
// ============================================================================

/// 单条 preview item（与 Node `IssueGraphLivenessAutoRecoveryPreviewItem` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueGraphLivenessAutoRecoveryPreviewItem {
    pub issue_id: Uuid,
    pub identifier: Option<String>,
    pub title: String,
    pub state: String,
    pub severity: String,
    pub reason: String,
    pub recovery_issue_id: Uuid,
    pub recovery_identifier: Option<String>,
    pub recovery_title: Option<String>,
    pub recommended_owner_agent_id: Option<Uuid>,
    pub incident_key: String,
    pub latest_dependency_updated_at: DateTime<Utc>,
    pub dependency_path: Vec<super::issue_graph_liveness::IssueLivenessDependencyPathEntry>,
}

/// 整个 preview 输出（与 Node `IssueGraphLivenessAutoRecoveryPreview` 对齐）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueGraphLivenessAutoRecoveryPreview {
    pub lookback_hours: i64,
    pub cutoff: DateTime<Utc>,
    pub generated_at: DateTime<Utc>,
    pub findings: i64,
    pub recoverable_findings: i64,
    pub skipped_outside_lookback: i64,
    pub items: Vec<IssueGraphLivenessAutoRecoveryPreviewItem>,
}

/// `build_issue_graph_liveness_auto_recovery_preview` 选项。
#[derive(Debug, Clone, Default)]
pub struct AutoRecoveryPreviewOptions {
    /// Lookback 窗口（小时）。None = 默认 24h。超出 [1, 720] 会被钳制。
    pub lookback_hours: Option<i64>,
    /// 注入的 "now"（便于测试）。None = `Utc::now()`。
    pub now: Option<DateTime<Utc>>,
    /// 限定单个公司（None = 全公司）。
    pub company_id: Option<Uuid>,
}

// ============================================================================
// Main entry point
// ============================================================================

/// 主入口：构建 issue graph liveness auto recovery 预览。
///
/// 与 Node `buildIssueGraphLivenessAutoRecoveryPreview` 对齐：
/// - lookbackHours 经 `normalize_lookback_hours` 钳制
/// - cutoff = now - lookbackHours * 3600s
/// - 每个 finding：latest updated_at >= cutoff 才进入 items
/// - 否则计入 skippedOutsideLookback
///
/// 返回 `IssueGraphLivenessAutoRecoveryPreview`：
/// - `findings` = collect 出来的 finding 总数
/// - `recoverableFindings` = items.len()
/// - `skippedOutsideLookback` = 因 updated_at < cutoff 而跳过的数量
/// - `items` = 进入 lookback 窗口的具体项（已附带 recovery 元信息）
pub async fn build_issue_graph_liveness_auto_recovery_preview(
    db: &Db,
    opts: AutoRecoveryPreviewOptions,
) -> sqlx::Result<IssueGraphLivenessAutoRecoveryPreview> {
    let now = opts.now.unwrap_or_else(Utc::now);
    let lookback_hours = normalize_lookback_hours(opts.lookback_hours);
    let cutoff = now - Duration::hours(lookback_hours);

    // Step 1: collect findings
    let findings = collect_issue_graph_liveness_findings(
        db,
        CollectFindingsOptions {
            company_id: opts.company_id,
            issue_limit: None,
        },
    )
    .await?;

    // Step 2: load updated_at map
    let updated_at_map = load_liveness_dependency_updated_at_by_issue(db, &findings).await?;

    // Step 3: collect recovery_issue_ids for metadata lookup
    let recovery_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = findings
            .iter()
            .filter_map(|f| f.recovery_issue_id)
            .collect();
        ids.sort();
        ids.dedup();
        ids
    };
    let recovery_meta = load_recovery_issue_metadata(db, &recovery_ids).await?;

    // Step 4: classify findings
    let mut items = Vec::new();
    let mut skipped_outside_lookback: i64 = 0;
    for finding in &findings {
        if !is_finding_inside_auto_recovery_lookback(finding, cutoff, &updated_at_map) {
            skipped_outside_lookback += 1;
            continue;
        }
        let latest_updated_at =
            match latest_dependency_updated_at_for_finding(finding, &updated_at_map) {
                Some(ts) => ts,
                None => {
                    // 没有 updated_at → 不可能进 lookback，但保险起见跳过
                    skipped_outside_lookback += 1;
                    continue;
                }
            };
        let recovery_issue_id = match finding.recovery_issue_id {
            Some(id) => id,
            None => {
                // 没有 recovery_issue_id 不应进入预览
                skipped_outside_lookback += 1;
                continue;
            }
        };
        let (recovery_identifier, recovery_title) = recovery_meta
            .get(&recovery_issue_id)
            .map(|(id, title)| (id.clone(), title.clone()))
            .unwrap_or((None, None));

        items.push(IssueGraphLivenessAutoRecoveryPreviewItem {
            issue_id: finding.source_issue_id,
            identifier: None,
            title: finding
                .dependency_path
                .first()
                .map(|e| e.title.clone())
                .unwrap_or_else(|| finding.source_issue_label.clone()),
            state: finding.state.as_str().to_string(),
            severity: finding.severity.as_str().to_string(),
            reason: finding.reason.clone(),
            recovery_issue_id,
            recovery_identifier,
            recovery_title,
            recommended_owner_agent_id: finding.recommended_owner_agent_id,
            incident_key: finding.incident_key.clone(),
            latest_dependency_updated_at: latest_updated_at,
            dependency_path: finding.dependency_path.clone(),
        });
    }

    Ok(IssueGraphLivenessAutoRecoveryPreview {
        lookback_hours,
        cutoff,
        generated_at: now,
        findings: findings.len() as i64,
        recoverable_findings: items.len() as i64,
        skipped_outside_lookback,
        items,
    })
}

// ============================================================================
// Helpers (private)
// ============================================================================

/// 加载一批 issue 的 (identifier, title) 元信息，返回 `Map<id, (Option<identifier>, Option<title>)>`。
///
/// 仅返回 identifier 和 title 两列（对齐 Node 中的 `recoveryRows` 查询）。
async fn load_recovery_issue_metadata(
    db: &Db,
    ids: &[Uuid],
) -> sqlx::Result<HashMap<Uuid, (Option<String>, Option<String>)>> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let rows = sqlx::query("SELECT id, identifier, title FROM issues WHERE id = ANY($1)")
        .bind(ids)
        .fetch_all(db.pool())
        .await?;
    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let identifier: Option<String> = row.try_get("identifier").ok().flatten();
        let title: Option<String> = row.try_get("title").ok().flatten();
        out.insert(id, (identifier, title));
    }
    Ok(out)
}
