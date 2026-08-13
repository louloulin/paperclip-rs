//! Effect execution for decisions.
//!
//! 与上游 `decisionService.executeEffect` 等价：
//! 1. advisory_xact_lock 序列化同一 effect 的并发
//! 2. INSERT INTO decision_effect_executions (claim)，或读已有行
//! 3. 若 status != "claimed" → 直接返回（已经被处理过）
//! 4. 根据 effect.type 分发到对应的 issue 服务
//! 5. 写回 executed / failed / skipped 终态

use serde_json::{json, Value};
use uuid::Uuid;

use pc_repos::decision::{DecisionEffectExecutionRow, DecisionRepo};

/// Effect 执行结果（用于 service 层聚合）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectExecutionOutcome {
    pub effect_index: i32,
    pub status: String,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub execution_id: Uuid,
}

/// Effect 执行器。
///
/// 包装 DecisionRepo。**不直接调用 IssueService** —— 业务路由层将 effect
/// 类型对应的 issue 变更交给 issue 服务，然后这里只做 effect tracking + 状态机。
///
/// 设计要点：
/// - 状态机：`claimed → executed | failed | skipped`。
/// - 幂等：同一 (decision_id, effect_index) 多次调用，结果一致。
/// - 解耦：调用方负责 issue 层 side-effect（本模块只做记账）。
pub struct EffectExecutor<'a> {
    repo: &'a DecisionRepo<'a>,
}

impl<'a> EffectExecutor<'a> {
    pub fn new(repo: &'a DecisionRepo<'a>) -> Self {
        Self { repo }
    }

    /// 原子声明一个 effect execution 行。
    /// 返回 (execution_row, was_claimed_now)。
    pub async fn claim(
        &self,
        decision_id: Uuid,
        effect_index: i32,
        effect_type: &str,
        target_issue_id: Uuid,
    ) -> sqlx::Result<(DecisionEffectExecutionRow, bool)> {
        let row = self
            .repo
            .claim_effect_execution(decision_id, effect_index, effect_type, target_issue_id)
            .await?;
        let Some(row) = row else {
            return Err(sqlx::Error::RowNotFound);
        };
        let was_claimed_now = row.status == "claimed";
        Ok((row, was_claimed_now))
    }

    /// 标记执行成功。返回更新后的行。
    pub async fn mark_executed(
        &self,
        execution_id: Uuid,
        result: &Value,
    ) -> sqlx::Result<()> {
        self.repo
            .finish_effect_execution(execution_id, "executed", None, Some(result))
            .await
    }

    /// 标记执行失败。
    pub async fn mark_failed(
        &self,
        execution_id: Uuid,
        error: &str,
        result: Option<&Value>,
    ) -> sqlx::Result<()> {
        self.repo
            .fail_effect_execution(execution_id, error, result)
            .await
    }

    /// 标记跳过（effect 已变更、被 staleness 检查拒绝等）。
    pub async fn mark_skipped(
        &self,
        execution_id: Uuid,
        reason: &str,
        result: Option<&Value>,
    ) -> sqlx::Result<()> {
        self.repo
            .finish_effect_execution(execution_id, "skipped", Some(reason), result)
            .await
    }

    /// 写入 outcome 包装（用于 service 层返回）。
    pub fn outcome_from(
        row: &DecisionEffectExecutionRow,
        effect_index: i32,
    ) -> EffectExecutionOutcome {
        EffectExecutionOutcome {
            effect_index,
            status: row.status.clone(),
            result: row.result.clone(),
            error: row.error.clone(),
            execution_id: row.id,
        }
    }
}

/// 给定一组 executions，按 effect_index 升序聚合 `(successful, total)`。
pub fn aggregate_execution_outcomes(rows: &[DecisionEffectExecutionRow]) -> (usize, usize, String) {
    let total = rows.len();
    let successful = rows.iter().filter(|r| r.status == "executed").count();
    let status = if total == 0 {
        "succeeded"
    } else if successful == total {
        "succeeded"
    } else if successful == 0 {
        "failed"
    } else {
        "partial"
    };
    (successful, total, status.to_string())
}

/// 把 effect type 字符串分类成 Action（与上游 `classifyEffectType` 等价）。
/// 已在 pure.rs 中实现，这里保留 facade 用于 effect_executor 内部调用一致性。
pub use crate::pure::classify_effect_type;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_execution_outcomes_empty_returns_succeeded() {
        let (succ, total, status) = aggregate_execution_outcomes(&[]);
        assert_eq!(succ, 0);
        assert_eq!(total, 0);
        assert_eq!(status, "succeeded");
    }

    #[test]
    fn classify_effect_type_dispatch() {
        assert_eq!(
            classify_effect_type("comment_on_issue").as_str(),
            "issue:comment"
        );
        assert_eq!(
            classify_effect_type("update_issue_status").as_str(),
            "issue:mutate"
        );
        assert_eq!(
            classify_effect_type("cancel_issue_tree").as_str(),
            "issue:mutate"
        );
        // Unknown / future → issue:mutate (safe default)
        assert_eq!(
            classify_effect_type("some_future_type").as_str(),
            "issue:mutate"
        );
    }

    #[test]
    fn aggregate_partial_vs_full_failure() {
        let rows = vec![
            DecisionEffectExecutionRow {
                id: Uuid::new_v4(),
                decision_id: Uuid::new_v4(),
                effect_index: 0,
                effect_type: "comment_on_issue".into(),
                target_issue_id: Uuid::new_v4(),
                status: "executed".into(),
                result: Some(json!({"commentId": "c-1"})),
                error: None,
                activity_log_id: None,
                executed_at: None,
            },
            DecisionEffectExecutionRow {
                id: Uuid::new_v4(),
                decision_id: Uuid::new_v4(),
                effect_index: 1,
                effect_type: "update_issue_status".into(),
                target_issue_id: Uuid::new_v4(),
                status: "failed".into(),
                result: None,
                error: Some("forbidden".into()),
                activity_log_id: None,
                executed_at: None,
            },
        ];
        let (succ, total, status) = aggregate_execution_outcomes(&rows);
        assert_eq!(succ, 1);
        assert_eq!(total, 2);
        assert_eq!(status, "partial");
    }
}
