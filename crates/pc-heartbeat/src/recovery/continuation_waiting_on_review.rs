//! `resolveContinuationWaitingOnReview` —— Node `services/recovery/service.ts:3229`。
//!
//! 业务语义：
//! - issue 在 `in_review` 状态下，latest run 报告 "continuation waiting on review" 错误码
//! - 收集 unresolved blocker issues + open children issues 作为新的 blocker set
//! - 若非空 → status='blocked' + 替换 issue_relations blocks + 加 system comment + 写 activity log
//! - 若空 → 返回 None，不做改动
//!
//! 设计原则：
//! - 顶层函数 `resolve_continuation_waiting_on_review` 是 DB I/O 入口（事务 + 行更新）
//! - 内部 helper（`list_open_children_ids` / `set_blocked_by_issue_ids` /
//!   `set_issue_status_blocked` / `add_recovery_system_comment` /
//!   `log_recovery_issue_activity`）拆分为最小职责
//! - 文本构造器 `build_waiting_on_review_comment_body` 是 pure 函数，可单测
//! - 全部与 Node 业务语义 1:1 对齐
use crate::recovery::issue_graph_liveness_db::existing_unresolved_blocker_issue_ids;
use pc_repos::issue::{IssueRepo, IssueRow};
use pc_repos::Db;
use serde_json::json;
use uuid::Uuid;

const WAITING_ON_REVIEW_SOURCE: &str = "recovery.reconcile_continuation_waiting_on_review";

/// 列出 issue 的 open children（parent_id = issue_id AND status NOT IN done/cancelled
/// AND hidden_at IS NULL）。
///
/// 与 Node `existingUnresolvedBlockerIssues` 第二段（open children 查询）对齐。
pub async fn list_open_children_ids(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Vec<Uuid>> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM issues \
         WHERE company_id = $1 AND parent_id = $2 \
           AND status NOT IN ('done','cancelled') \
           AND hidden_at IS NULL \
         ORDER BY id",
    )
    .bind(company_id)
    .bind(issue_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// 把 issue 的 blockers 集合替换为指定 set（idempotent）。
///
/// 内部逻辑：
/// 1. DELETE 所有 type='blocks' AND related_issue_id = issue_id 的旧边
/// 2. INSERT 新边（ON CONFLICT DO NOTHING 保持幂等）
///
/// 注意：禁止把 issue 加为自身的 blocker（Node 与 Rust 一致）。
pub async fn set_blocked_by_issue_ids(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    blocker_ids: &[Uuid],
) -> sqlx::Result<()> {
    let mut tx = db.pool().begin().await?;
    // 删除旧边
    sqlx::query(
        "DELETE FROM issue_relations \
         WHERE company_id = $1 AND related_issue_id = $2 AND type = 'blocks'",
    )
    .bind(company_id)
    .bind(issue_id)
    .execute(&mut *tx)
    .await?;
    // 插入新边（去重 + 排除 self）
    let unique: std::collections::BTreeSet<Uuid> = blocker_ids
        .iter()
        .copied()
        .filter(|id| *id != issue_id)
        .collect();
    for blocker in unique {
        sqlx::query(
            "INSERT INTO issue_relations (company_id, issue_id, related_issue_id, type) \
             VALUES ($1, $2, $3, 'blocks') ON CONFLICT DO NOTHING",
        )
        .bind(company_id)
        .bind(blocker)
        .bind(issue_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// 把 issue status 设为 'blocked'（仅当当前 status != 'blocked'）。
pub async fn set_issue_status_blocked(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE issues SET status = 'blocked', updated_at = now() \
         WHERE id = $1 AND company_id = $2 AND status <> 'blocked'",
    )
    .bind(issue_id)
    .bind(company_id)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 加 system comment。author_user_id = "system"（与 escalate_db 对齐）。
pub async fn add_recovery_system_comment(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    body: &str,
) -> sqlx::Result<Uuid> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO issue_comments (company_id, issue_id, author_user_id, body) \
         VALUES ($1, $2, 'system', $3) RETURNING id",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(body)
    .fetch_one(db.pool())
    .await?;
    Ok(row.0)
}

/// 写 `issue.updated` activity log。
pub async fn log_recovery_issue_activity(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
    identifier: Option<&str>,
    previous_status: &str,
    blocked_by_issue_ids: &[Uuid],
) -> sqlx::Result<()> {
    let blocked_strs: Vec<String> = blocked_by_issue_ids.iter().map(|u| u.to_string()).collect();
    let details = json!({
        "identifier": identifier,
        "status": "blocked",
        "previousStatus": previous_status,
        "source": WAITING_ON_REVIEW_SOURCE,
        "blockedByIssueIds": blocked_strs,
    });
    sqlx::query(
        "INSERT INTO activity_log \
         (company_id, actor_type, actor_id, action, entity_type, entity_id, details) \
         VALUES ($1, 'system', 'system', 'issue.updated', 'issue', $2, $3)",
    )
    .bind(company_id)
    .bind(issue_id)
    .bind(&details)
    .execute(db.pool())
    .await?;
    Ok(())
}

/// 构造 issue comment body（pure 函数）。
///
/// 与 Node 第 3308 行 `addComment` 第三个字符串参数对齐：
///   "This task is waiting on ${waitingOn} to finish. ..."
///
/// Node 用 `${formatIssueLinksForComment([...openChildren, ...existingBlockers])}`，
/// 本实现采用 Node 端展示的最简形式（仅 issue identifiers 列表），
/// 真实链接渲染由 UI 端负责。
pub fn build_waiting_on_review_comment_body(blocker_issues: &[(Uuid, Option<String>)]) -> String {
    if blocker_issues.is_empty() {
        return String::new();
    }
    let waiting_on = blocker_issues
        .iter()
        .filter_map(|(_, ident)| ident.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let waiting_on = if waiting_on.is_empty() {
        "the listed dependency".to_string()
    } else {
        waiting_on
    };
    format!(
        "This task is waiting on {waiting_on} to finish. \
         It will continue automatically when that work is done — there's nothing you need to do. \
         (It was paused because the latest run reported it was waiting for review/approval; \
         Paperclip turned that into a normal dependency wait instead of flagging it as stuck.)"
    )
}

/// 主入口：in_review issue 报告 "waiting on review" 错误码时，转换为 blocked。
///
/// 步骤：
/// 1. 读取 unresolved blockers + open children → 唯一集合
/// 2. 若空 → return None
/// 3. set_blocked_by_issue_ids 替换 issue_relations blocks
/// 4. set_issue_status_blocked
/// 5. add_recovery_system_comment
/// 6. log_recovery_issue_activity
/// 7. 返回最新 IssueRow
///
/// 返回 None 表示没有依赖可等待；返回 Some 表示已切换为 blocked。
pub async fn resolve_continuation_waiting_on_review(
    db: &Db,
    company_id: Uuid,
    issue_id: Uuid,
) -> sqlx::Result<Option<IssueRow>> {
    // 1. 收集 blockers + children
    let mut blocker_ids: Vec<Uuid> =
        existing_unresolved_blocker_issue_ids(db, company_id, issue_id).await?;
    let open_children = list_open_children_ids(db, company_id, issue_id).await?;
    for child_id in open_children {
        if !blocker_ids.contains(&child_id) {
            blocker_ids.push(child_id);
        }
    }
    if blocker_ids.is_empty() {
        return Ok(None);
    }

    // 2. 读出 issue 当前状态与 identifier（供 activity log）
    let existing = IssueRepo::new(db).get(issue_id).await?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    let previous_status = existing.status.clone();
    let identifier = existing.identifier.clone();

    // 3. 取 blockers 的 (id, identifier) 给 comment 用
    let blocker_pairs: Vec<(Uuid, Option<String>)> = if blocker_ids.is_empty() {
        Vec::new()
    } else {
        let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
            "SELECT id, identifier FROM issues WHERE id = ANY($1) ORDER BY identifier NULLS LAST, id",
        )
        .bind(&blocker_ids)
        .fetch_all(db.pool())
        .await?;
        rows
    };

    // 4. 替换 blocks
    set_blocked_by_issue_ids(db, company_id, issue_id, &blocker_ids).await?;
    // 5. status -> blocked
    set_issue_status_blocked(db, company_id, issue_id).await?;
    // 6. comment
    let body = build_waiting_on_review_comment_body(&blocker_pairs);
    if !body.is_empty() {
        add_recovery_system_comment(db, company_id, issue_id, &body).await?;
    }
    // 7. activity log
    log_recovery_issue_activity(
        db,
        company_id,
        issue_id,
        identifier.as_deref(),
        &previous_status,
        &blocker_ids,
    )
    .await?;

    // 8. 重新读取（status 已被更新）
    let updated = IssueRepo::new(db).get(issue_id).await?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_body_with_identifiers() {
        let body = build_waiting_on_review_comment_body(&[
            (Uuid::nil(), Some("PAP-1".into())),
            (Uuid::nil(), Some("PAP-2".into())),
        ]);
        assert!(body.contains("PAP-1, PAP-2"));
        assert!(body.contains("This task is waiting on"));
        assert!(body.contains("automatically"));
    }

    #[test]
    fn build_body_empty_when_no_blockers() {
        let body = build_waiting_on_review_comment_body(&[]);
        assert!(body.is_empty());
    }

    #[test]
    fn build_body_with_missing_identifiers() {
        let body = build_waiting_on_review_comment_body(&[(Uuid::nil(), None)]);
        assert!(body.contains("the listed dependency"));
        assert!(body.contains("This task is waiting on"));
    }
}
