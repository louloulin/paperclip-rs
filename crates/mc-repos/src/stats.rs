//! `StatsRepo` —— `GET /api/assignee-frequency` 的聚合读（M2-A 尾 / LUM-1691）。
//!
//! 对应上游 `server/internal/handler/activity.go` 的 `GetAssigneeFrequency` +
//! `server/pkg/db/queries/{activity,issue}.sql` 的两条计数查询。**没有新表**：只读
//! `activity_log` 与 `issue`，所以本片 0 迁移。
//!
//! 上游口径逐字（`grep` 自 `server/pkg/db/generated/{activity,issue}.sql.go`）：
//!
//! ```sql
//! -- 源 1：本人在本 workspace 改派过谁（CountAssigneeChangesByActor）
//! SELECT details->>'to_type' AS assignee_type,
//!        details->>'to_id'   AS assignee_id,
//!        COUNT(*)::bigint    AS frequency
//!   FROM activity_log
//!  WHERE workspace_id = $1 AND actor_id = $2
//!    AND actor_type = 'member' AND action = 'assignee_changed'
//!    AND details->>'to_type' IS NOT NULL AND details->>'to_id' IS NOT NULL
//!  GROUP BY details->>'to_type', details->>'to_id'
//!
//! -- 源 2：本人建过、且建的时候就带指派人（CountCreatedIssueAssignees）
//! SELECT assignee_type, assignee_id, COUNT(*)::bigint AS frequency
//!   FROM issue
//!  WHERE workspace_id = $1 AND creator_id = $2
//!    AND creator_type = 'member'
//!    AND assignee_type IS NOT NULL AND assignee_id IS NOT NULL
//!  GROUP BY assignee_type, assignee_id
//! ```
//!
//! 两处细节决定实现形态，别「顺手统一」：
//!
//! 1. **源 1 的 id 是文本、源 2 的 id 是 uuid**。`details->>'to_id'` 是 JSON 取值的结果，
//!    PostgreSQL 给它 `text` 类型 —— 上游扫进 `interface{}` 再断言 `string`，用的是**不透明
//!    字符串**。所以合并键是 `"type:id"` 字符串而不是 `(type, Uuid)` 二元组，本模块也照此
//!    建模（源 2 的 uuid 先 `to_string()`），否则源 1 里任何非 UUID 的历史 `to_id` 会被静默丢。
//! 2. **排序只有「频次降序」一条**。上游 `sort.Slice` 非稳定 + 合并源是 Go map ⇒ 同频次的
//!    相对顺序上游自己也不确定。本模块在频次相同时按 `(assignee_type, assignee_id)` 升序
//!    补一个确定性 tiebreak —— 这不是行为收紧，而是把一个上游随机值固定下来
//!    （见 `docs/63-M2A-TAIL-ISSUE-VIEW-PIN.md` 的偏差表）。

use mc_core::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 源 1（改派活动）的原始计数行：id 是 `details->>'to_id'` 的**文本**。
#[derive(Debug, Clone, FromRow)]
pub struct AssigneeChangeCountRow {
    pub assignee_type: String,
    pub assignee_id: String,
    pub frequency: i64,
}

/// 源 2（建单时的指派人）的原始计数行：id 是 `issue.assignee_id` 的 **uuid**。
#[derive(Debug, Clone, FromRow)]
pub struct CreatedIssueAssigneeCountRow {
    pub assignee_type: String,
    pub assignee_id: Uuid,
    pub frequency: i64,
}

/// 合并后的响应条目（上游 `AssigneeFrequencyEntry`）。
///
/// 字段顺序即 JSON 顺序：`assignee_type` / `assignee_id` / `frequency`。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AssigneeFrequencyEntry {
    /// `member` / `agent` / `squad` / `autopilot` …（不枚举化：值来自历史数据）
    pub assignee_type: String,
    /// **不透明** id 字符串（见模块文档第 1 条）。
    pub assignee_id: String,
    pub frequency: i64,
}

/// 把两路计数合并成 `"type:id" -> 频次` 的累加表（上游 `GetAssigneeFrequency` 的第一段）。
///
/// 空 `type` 或空 `id` 的源 1 行被跳过（上游 `if aType != "" && aID != ""`）。
pub fn merge_frequencies(
    activity_counts: &[AssigneeChangeCountRow],
    issue_counts: &[CreatedIssueAssigneeCountRow],
) -> Vec<AssigneeFrequencyEntry> {
    use std::collections::HashMap;

    let mut freq: HashMap<(String, String), i64> = HashMap::new();
    for row in activity_counts {
        if row.assignee_type.is_empty() || row.assignee_id.is_empty() {
            continue;
        }
        *freq
            .entry((row.assignee_type.clone(), row.assignee_id.clone()))
            .or_insert(0) += row.frequency;
    }
    for row in issue_counts {
        // 源 2 的 SQL 已经过滤 NULL，这里再判一次「空的 type 才算无效」，与上游的
        // `!row.AssigneeType.Valid || !row.AssigneeID.Valid` 等价而不依赖 pgtype。
        if row.assignee_type.is_empty() {
            continue;
        }
        *freq
            .entry((row.assignee_type.clone(), row.assignee_id.to_string()))
            .or_insert(0) += row.frequency;
    }

    let mut result: Vec<AssigneeFrequencyEntry> = freq
        .into_iter()
        .map(
            |((assignee_type, assignee_id), frequency)| AssigneeFrequencyEntry {
                assignee_type,
                assignee_id,
                frequency,
            },
        )
        .collect();
    // 频次降序；同频次按 (type, id) 升序 —— 上游同频次顺序不确定，这里固定下来。
    result.sort_by(|a, b| {
        b.frequency
            .cmp(&a.frequency)
            .then_with(|| a.assignee_type.cmp(&b.assignee_type))
            .then_with(|| a.assignee_id.cmp(&b.assignee_id))
    });
    result
}

/// `issue` / `activity_log` 的聚合仓储。
pub struct StatsRepo {
    db: Db,
}

impl StatsRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 源 1：该用户在本 workspace 通过 `assignee_changed` 改派到各目标上的次数。
    pub async fn count_assignee_changes_by_actor(
        &self,
        workspace_id: Id,
        actor_id: Id,
    ) -> Result<Vec<AssigneeChangeCountRow>> {
        sqlx::query_as::<_, AssigneeChangeCountRow>(
            "SELECT details->>'to_type' AS assignee_type, \
                    details->>'to_id'   AS assignee_id, \
                    COUNT(*)::bigint    AS frequency \
               FROM activity_log \
              WHERE workspace_id = $1 AND actor_id = $2 \
                AND actor_type = 'member' AND action = 'assignee_changed' \
                AND details->>'to_type' IS NOT NULL AND details->>'to_id' IS NOT NULL \
              GROUP BY details->>'to_type', details->>'to_id'",
        )
        .bind(workspace_id.0)
        .bind(actor_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 源 2：该用户在本 workspace 建单时已带上的各指派人次数。
    pub async fn count_created_issue_assignees(
        &self,
        workspace_id: Id,
        creator_id: Id,
    ) -> Result<Vec<CreatedIssueAssigneeCountRow>> {
        sqlx::query_as::<_, CreatedIssueAssigneeCountRow>(
            "SELECT assignee_type, assignee_id, COUNT(*)::bigint AS frequency \
               FROM issue \
              WHERE workspace_id = $1 AND creator_id = $2 \
                AND creator_type = 'member' \
                AND assignee_type IS NOT NULL AND assignee_id IS NOT NULL \
              GROUP BY assignee_type, assignee_id",
        )
        .bind(workspace_id.0)
        .bind(creator_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 两路合并 + 排序后的最终响应（上游 handler 的全部业务逻辑都在这里）。
    pub async fn assignee_frequency(
        &self,
        workspace_id: Id,
        user_id: Id,
    ) -> Result<Vec<AssigneeFrequencyEntry>> {
        let activity = self
            .count_assignee_changes_by_actor(workspace_id, user_id)
            .await?;
        let issues = self
            .count_created_issue_assignees(workspace_id, user_id)
            .await?;
        Ok(merge_frequencies(&activity, &issues))
    }
}

impl RepoWithDb for StatsRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

// ---------------------------------------------------------------------------
// 纯单测（无需 DB）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(assignee_type: &str, assignee_id: &str, frequency: i64) -> AssigneeChangeCountRow {
        AssigneeChangeCountRow {
            assignee_type: assignee_type.into(),
            assignee_id: assignee_id.into(),
            frequency,
        }
    }

    fn created(
        assignee_type: &str,
        assignee_id: Uuid,
        frequency: i64,
    ) -> CreatedIssueAssigneeCountRow {
        CreatedIssueAssigneeCountRow {
            assignee_type: assignee_type.into(),
            assignee_id,
            frequency,
        }
    }

    #[test]
    fn both_sources_accumulate_onto_the_same_key() {
        let member = Uuid::from_u128(7);
        let merged = merge_frequencies(
            &[activity("member", &member.to_string(), 2)],
            &[created("member", member, 3)],
        );
        assert_eq!(
            merged,
            vec![AssigneeFrequencyEntry {
                assignee_type: "member".into(),
                assignee_id: member.to_string(),
                frequency: 5,
            }],
            "同一个 (type,id) 的两路计数必须相加，不是各出一行"
        );
    }

    #[test]
    fn ordering_is_frequency_desc_then_key_asc() {
        let merged = merge_frequencies(
            &[
                activity("agent", "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", 1),
                activity("agent", "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", 1),
                activity("member", "cccccccc-cccc-cccc-cccc-cccccccccccc", 3),
            ],
            &[],
        );
        let keys: Vec<(String, String)> = merged
            .iter()
            .map(|e| (e.assignee_type.clone(), e.assignee_id.clone()))
            .collect();
        assert_eq!(
            keys,
            vec![
                (
                    "member".into(),
                    "cccccccc-cccc-cccc-cccc-cccccccccccc".into()
                ),
                (
                    "agent".into(),
                    "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa".into()
                ),
                (
                    "agent".into(),
                    "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb".into()
                ),
            ]
        );
    }

    #[test]
    fn empty_type_or_id_rows_are_dropped_not_counted() {
        let merged = merge_frequencies(
            &[
                activity("", "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", 9),
                activity("agent", "", 9),
            ],
            &[created("", Uuid::from_u128(1), 4)],
        );
        assert!(merged.is_empty(), "空 type/id 的行不得进入结果：{merged:?}");
    }

    #[test]
    fn non_uuid_to_id_survives_as_an_opaque_string() {
        // 源 1 的 id 是 JSON 文本，历史上不保证是 UUID；上游原样透传 ⇒ 本模块也不得丢。
        let merged = merge_frequencies(&[activity("squad", "legacy-squad-key", 1)], &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].assignee_id, "legacy-squad-key");
    }

    #[test]
    fn empty_inputs_yield_an_empty_list() {
        assert!(merge_frequencies(&[], &[]).is_empty());
    }
}
