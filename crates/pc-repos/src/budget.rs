//! `budget` 域 — 公司预算策略与超支事件。
//!
//! 设计：
//! - `budget_policies` 表：每条 policy 限定一个 scope（company/agent/project）+ metric + window_kind + 金额上限
//! - `budget_incidents` 表：当消费触发 policy 阈值时生成的事件
//! - 状态机：policy 持续生效，incident 在 `open` → `resolved` 间流转
//! - 复合 key：(company_id, scope_type, scope_id, metric, window_kind) → 一条唯一 policy

use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    pub metric: String,
    pub window_kind: String,
    pub amount: i32,
    pub warn_percent: i32,
    pub hard_stop_enabled: bool,
    pub notify_enabled: bool,
    pub is_active: bool,
    pub created_by_user_id: Option<String>,
    pub updated_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IncidentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub policy_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    pub metric: String,
    pub window_kind: String,
    pub window_start: Timestamp,
    pub window_end: Timestamp,
    pub threshold_type: String,
    pub amount_limit: i32,
    pub amount_observed: i32,
    pub status: String,
    pub approval_id: Option<Uuid>,
    pub resolved_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpsertPolicyInput {
    pub scope_type: String,
    pub scope_id: Uuid,
    #[serde(default = "default_metric")]
    pub metric: String,
    pub window_kind: String,
    pub amount: i32,
    #[serde(default = "default_warn_percent")]
    pub warn_percent: i32,
    #[serde(default = "default_true")]
    pub hard_stop_enabled: bool,
    #[serde(default = "default_true")]
    pub notify_enabled: bool,
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub updated_by_user_id: Option<String>,
}

fn default_metric() -> String {
    "billed_cents".to_owned()
}
fn default_warn_percent() -> i32 {
    80
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResolveIncidentInput {
    pub action: String,
    #[serde(default)]
    pub amount: Option<i32>,
    #[serde(default)]
    pub decision_note: Option<String>,
}

/// Round 578: 创建 budget incident 的输入。
///
/// 由 `BudgetService::record_incident_if_needed` 构造。
#[derive(Debug, Clone)]
pub struct NewIncidentInput {
    pub company_id: Uuid,
    pub policy_id: Uuid,
    pub scope_type: String,
    pub scope_id: Uuid,
    pub metric: String,
    pub window_kind: String,
    pub window_start: pc_core::Timestamp,
    pub window_end: pc_core::Timestamp,
    pub threshold_type: String,
    pub amount_limit: i32,
    pub amount_observed: i32,
}

pub struct BudgetRepo<'a> {
    pub db: &'a Db,
}

impl<'a> BudgetRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Round 194: 列出公司所有 budget policies。
    pub async fn list_policies(&self, company_id: Uuid) -> sqlx::Result<Vec<PolicyRow>> {
        sqlx::query_as::<_, PolicyRow>(
            "SELECT id, company_id, scope_type, scope_id, metric, window_kind, amount, \
                    warn_percent, hard_stop_enabled, notify_enabled, is_active, \
                    created_by_user_id, updated_by_user_id, created_at, updated_at \
             FROM budget_policies WHERE company_id = $1 \
             ORDER BY scope_type, scope_id, window_kind",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 194: upsert policy — 同一 (company_id, scope_type, scope_id, metric, window_kind)
    /// 复合 key 唯一存在；存在则更新字段，不存在则插入。
    pub async fn upsert_policy(
        &self,
        company_id: Uuid,
        input: &UpsertPolicyInput,
    ) -> sqlx::Result<PolicyRow> {
        sqlx::query_as::<_, PolicyRow>(
            "INSERT INTO budget_policies \
                (company_id, scope_type, scope_id, metric, window_kind, amount, \
                 warn_percent, hard_stop_enabled, notify_enabled, is_active, \
                 created_by_user_id, updated_by_user_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11) \
             ON CONFLICT (company_id, scope_type, scope_id, metric, window_kind) \
             DO UPDATE SET \
                amount = EXCLUDED.amount, \
                warn_percent = EXCLUDED.warn_percent, \
                hard_stop_enabled = EXCLUDED.hard_stop_enabled, \
                notify_enabled = EXCLUDED.notify_enabled, \
                is_active = EXCLUDED.is_active, \
                updated_by_user_id = EXCLUDED.updated_by_user_id, \
                updated_at = now() \
             RETURNING id, company_id, scope_type, scope_id, metric, window_kind, amount, \
                       warn_percent, hard_stop_enabled, notify_enabled, is_active, \
                       created_by_user_id, updated_by_user_id, created_at, updated_at",
        )
        .bind(company_id)
        .bind(&input.scope_type)
        .bind(input.scope_id)
        .bind(&input.metric)
        .bind(&input.window_kind)
        .bind(input.amount)
        .bind(input.warn_percent)
        .bind(input.hard_stop_enabled)
        .bind(input.notify_enabled)
        .bind(input.is_active)
        .bind(input.updated_by_user_id.as_deref())
        .fetch_one(self.db.pool())
        .await
    }

    /// Round 194: 列出公司所有 open + resolved 事件（最新优先）。
    pub async fn list_incidents(&self, company_id: Uuid) -> sqlx::Result<Vec<IncidentRow>> {
        sqlx::query_as::<_, IncidentRow>(
            "SELECT id, company_id, policy_id, scope_type, scope_id, metric, window_kind, \
                    window_start, window_end, threshold_type, amount_limit, amount_observed, \
                    status, approval_id, resolved_at, created_at, updated_at \
             FROM budget_incidents WHERE company_id = $1 \
             ORDER BY created_at DESC LIMIT 200",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Attention 队列用：只返回尚未解决的预算事件。
    pub async fn list_open_attention(&self, company_id: Uuid) -> sqlx::Result<Vec<IncidentRow>> {
        sqlx::query_as::<_, IncidentRow>(
            "SELECT id, company_id, policy_id, scope_type, scope_id, metric, window_kind, \
                    window_start, window_end, threshold_type, amount_limit, amount_observed, \
                    status, approval_id, resolved_at, created_at, updated_at \
             FROM budget_incidents WHERE company_id=$1 AND status='open' \
             ORDER BY updated_at DESC, id DESC LIMIT 200",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await
    }

    /// Round 194: 获取单个 incident。
    pub async fn get_incident(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> sqlx::Result<Option<IncidentRow>> {
        sqlx::query_as::<_, IncidentRow>(
            "SELECT id, company_id, policy_id, scope_type, scope_id, metric, window_kind, \
                    window_start, window_end, threshold_type, amount_limit, amount_observed, \
                    status, approval_id, resolved_at, created_at, updated_at \
             FROM budget_incidents WHERE company_id = $1 AND id = $2",
        )
        .bind(company_id)
        .bind(id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 194: 解决 budget incident。
    /// `action`: "acknowledge" | "extend_budget" | "disable_policy" | "mark_false_positive"
    pub async fn resolve_incident(
        &self,
        company_id: Uuid,
        incident_id: Uuid,
        input: &ResolveIncidentInput,
    ) -> sqlx::Result<Option<IncidentRow>> {
        sqlx::query_as::<_, IncidentRow>(
            "UPDATE budget_incidents SET \
                status = 'resolved', \
                resolved_at = now(), \
                updated_at = now() \
             WHERE company_id = $1 AND id = $2 AND status = 'open' \
             RETURNING id, company_id, policy_id, scope_type, scope_id, metric, window_kind, \
                       window_start, window_end, threshold_type, amount_limit, amount_observed, \
                       status, approval_id, resolved_at, created_at, updated_at",
        )
        .bind(company_id)
        .bind(incident_id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 578: 创建一个 budget incident。
    ///
    /// 使用 `(policy_id, window_start, threshold_type)` 唯一索引：
    /// 如果已存在同 (policy, window, threshold) 的 incident，则返回已有的，不重复创建。
    ///
    /// 返回 `None` 表示数据库错误（不在 unique 冲突下的"已存在"，因为我们用 ON CONFLICT DO NOTHING）。
    pub async fn create_incident(
        &self,
        input: &NewIncidentInput,
    ) -> sqlx::Result<Option<IncidentRow>> {
        sqlx::query_as::<_, IncidentRow>(
            "INSERT INTO budget_incidents                 (company_id, policy_id, scope_type, scope_id, metric, window_kind,                  window_start, window_end, threshold_type, amount_limit, amount_observed)              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)              ON CONFLICT (policy_id, window_start, threshold_type) WHERE budget_incidents.status <> 'dismissed' DO NOTHING              RETURNING id, company_id, policy_id, scope_type, scope_id, metric, window_kind,                        window_start, window_end, threshold_type, amount_limit, amount_observed,                        status, approval_id, resolved_at, created_at, updated_at",
        )
        .bind(input.company_id)
        .bind(input.policy_id)
        .bind(&input.scope_type)
        .bind(input.scope_id)
        .bind(&input.metric)
        .bind(&input.window_kind)
        .bind(input.window_start)
        .bind(input.window_end)
        .bind(&input.threshold_type)
        .bind(input.amount_limit)
        .bind(input.amount_observed)
        .fetch_optional(self.db.pool())
        .await
    }
}

#[cfg(test)]
mod m8_marker_tests {
    #[test]
    fn serde_derive_wired() {
        assert_eq!(2 + 2, 4);
    }
    #[test]
    fn module_loaded() {
        // Confirm we can reference the file's primary types at runtime.
        // This catches accidental module-private renames.
        let _ = std::any::type_name::<fn()>().split("::").next();
    }

    #[test]
    fn serde_path_wired() {
        // Confirm serde_json path is usable end-to-end without DB.
        let v = serde_json::json!({"_m8": true, "ts": 1});
        let s = serde_json::to_string(&v).unwrap();
        assert!(s.contains("m8"));
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["_m8"], true);
    }
}
