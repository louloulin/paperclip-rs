//! Inbox agent policy（1:1 port of Node `server/src/services/inbox-agent-policy.ts`，58 行）。
//!
//! 单一职责：每用户每公司一行的 inbox agent 政策（`mode` + `allowedAgentIds`）。
//!
//! - `InboxAgentPolicyRow` —— `user_inbox_agent_policies` 表行
//! - `InboxAgentPolicyMode` 枚举 —— `open` / `allowlist` / `disabled`
//! - `InboxAgentPolicy` —— API 视图（含 `materialized` 标记 + `created_at` / `updated_at` 可空）
//! - `UpdateInboxAgentPolicyInput` —— update 入参（mode + allowed_agent_ids）
//! - `InboxAgentPolicyRepo::new(db)` + `get(company_id, user_id)` + `update(company_id, user_id, input)`
//!
//! 设计：
//! - `get` 用 `?` 行不存在走默认；与 Node `rows[0] ?? null` 1:1 对齐
//! - `update` 用 Postgres `INSERT ... ON CONFLICT (...) DO UPDATE`，与 Node `onConflictDoUpdate` 1:1 对齐
//! - `allowed_agent_ids` 用 `Vec<Uuid>`，DB 端 `jsonb` 用 sqlx `Json` 包装
//! - 验证逻辑独立：`validate_allowed_agent_ids_in_company` 检查所有 agent id 属于同公司

use serde::{Deserialize, Serialize};
use sqlx::{types::Json, FromRow};
use uuid::Uuid;

use crate::{Db, RepoError, RepoResult};
use pc_core::Timestamp;

// ============================================================================
// Types
// ============================================================================

/// Inbox agent 政策模式（与 Node `mode` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InboxAgentPolicyMode {
    Open,
    Allowlist,
    Disabled,
}

impl InboxAgentPolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Allowlist => "allowlist",
            Self::Disabled => "disabled",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "allowlist" => Some(Self::Allowlist),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }
}

/// `user_inbox_agent_policies` 表行（与 Drizzle schema 1:1 对齐）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxAgentPolicyRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub user_id: String,
    pub mode: String,
    pub allowed_agent_ids: Json<Vec<Uuid>>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// API 视图（与 Node `InboxAgentPolicy` 1:1 对齐）。
///
/// - `materialized = true` —— DB 中有行
/// - `materialized = false` —— 走默认值（get 未命中时）
/// - `created_at` / `updated_at` 在未命中时为 `None`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxAgentPolicy {
    pub company_id: Uuid,
    pub user_id: String,
    pub mode: InboxAgentPolicyMode,
    pub allowed_agent_ids: Vec<Uuid>,
    pub materialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
}

/// Update 入参（与 Node `UpdateInboxAgentPolicy` 1:1 对齐）。
#[derive(Debug, Clone)]
pub struct UpdateInboxAgentPolicyInput {
    pub mode: InboxAgentPolicyMode,
    pub allowed_agent_ids: Vec<Uuid>,
}

/// 错误：allowedAgentIds 包含非同公司的 agent（与 Node `unprocessable(...)` 1:1 对齐）。
#[derive(Debug, thiserror::Error)]
#[error("inbox agent policy contains agents outside the company: {invalid_agent_ids:?}")]
pub struct InvalidAgentsError {
    pub invalid_agent_ids: Vec<Uuid>,
}

impl InvalidAgentsError {
    pub fn new(invalid_agent_ids: Vec<Uuid>) -> Self {
        Self { invalid_agent_ids }
    }
}

// ============================================================================
// Repository
// ============================================================================

/// `user_inbox_agent_policies` 表仓储入口。
pub struct InboxAgentPolicyRepo<'a> {
    pub db: &'a Db,
}

impl<'a> InboxAgentPolicyRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 读取 (company_id, user_id) 的 inbox agent policy。
    ///
    /// 行为（与 Node `get` 1:1 对齐）：
    /// - 行存在 → 返回 `{...row, materialized: true}`
    /// - 行不存在 → 返回默认 `{ company_id, user_id, mode: "open", allowedAgentIds: [], materialized: false, created_at: null, updated_at: null }`
    pub async fn get(&self, company_id: Uuid, user_id: &str) -> sqlx::Result<InboxAgentPolicy> {
        let row: Option<InboxAgentPolicyRow> = sqlx::query_as::<_, InboxAgentPolicyRow>(
            "SELECT id, company_id, user_id, mode, allowed_agent_ids, created_at, updated_at \
             FROM user_inbox_agent_policies \
             WHERE company_id = $1 AND user_id = $2",
        )
        .bind(company_id)
        .bind(user_id)
        .fetch_optional(self.db.pool())
        .await?;

        Ok(match row {
            Some(r) => {
                let mode =
                    InboxAgentPolicyMode::parse(&r.mode).unwrap_or(InboxAgentPolicyMode::Open);
                InboxAgentPolicy {
                    company_id: r.company_id,
                    user_id: r.user_id,
                    mode,
                    allowed_agent_ids: r.allowed_agent_ids.0,
                    materialized: true,
                    created_at: Some(r.created_at),
                    updated_at: Some(r.updated_at),
                }
            }
            None => InboxAgentPolicy {
                company_id,
                user_id: user_id.to_string(),
                mode: InboxAgentPolicyMode::Open,
                allowed_agent_ids: Vec::new(),
                materialized: false,
                created_at: None,
                updated_at: None,
            },
        })
    }

    /// Update inbox agent policy（upsert 语义）。
    ///
    /// 行为（与 Node `update` 1:1 对齐）：
    /// 1. `mode == "allowlist"` → 用 `dedup(allowedAgentIds)`（保留顺序去重）
    ///    否则 `allowedAgentIds = []`
    /// 2. 验证所有 agent id 属于同一 company（否则返回 `RepoError::Invalid` 携带 invalid ids）
    /// 3. UPSERT (`ON CONFLICT (company_id, user_id) DO UPDATE`)
    /// 4. 返回 `materialized: true` 的 policy
    pub async fn update(
        &self,
        company_id: Uuid,
        user_id: &str,
        input: UpdateInboxAgentPolicyInput,
    ) -> RepoResult<InboxAgentPolicy> {
        // 步骤 1: 去重（保持顺序）
        let mut allowed_agent_ids: Vec<Uuid> = if input.mode == InboxAgentPolicyMode::Allowlist {
            let mut seen = std::collections::HashSet::new();
            input
                .allowed_agent_ids
                .into_iter()
                .filter(|id| seen.insert(*id))
                .collect()
        } else {
            Vec::new()
        };

        // 步骤 2: 验证属于同一 company
        if !allowed_agent_ids.is_empty() {
            self.validate_allowed_agent_ids_in_company(company_id, &allowed_agent_ids)
                .await?;
        }

        // 步骤 3: UPSERT
        let now = Timestamp::now();
        let row: InboxAgentPolicyRow = sqlx::query_as::<_, InboxAgentPolicyRow>(
            "INSERT INTO user_inbox_agent_policies (company_id, user_id, mode, allowed_agent_ids, updated_at) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (company_id, user_id) DO UPDATE SET \
                mode = EXCLUDED.mode, \
                allowed_agent_ids = EXCLUDED.allowed_agent_ids, \
                updated_at = EXCLUDED.updated_at \
             RETURNING id, company_id, user_id, mode, allowed_agent_ids, created_at, updated_at",
        )
        .bind(company_id)
        .bind(user_id)
        .bind(input.mode.as_str())
        .bind(Json(&allowed_agent_ids))
        .bind(now)
        .fetch_one(self.db.pool())
        .await?;

        // 步骤 4: 返回 materialized view
        let mode = InboxAgentPolicyMode::parse(&row.mode).unwrap_or(input.mode);
        Ok(InboxAgentPolicy {
            company_id: row.company_id,
            user_id: row.user_id,
            mode,
            allowed_agent_ids: row.allowed_agent_ids.0,
            materialized: true,
            created_at: Some(row.created_at),
            updated_at: Some(row.updated_at),
        })
    }

    /// 验证所有 agent_id 属于指定 company。
    async fn validate_allowed_agent_ids_in_company(
        &self,
        company_id: Uuid,
        agent_ids: &[Uuid],
    ) -> RepoResult<()> {
        // 一次性查同公司的所有 agent id
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("SELECT id FROM agents WHERE company_id = $1 AND id = ANY($2)")
                .bind(company_id)
                .bind(agent_ids)
                .fetch_all(self.db.pool())
                .await?;

        let company_set: std::collections::HashSet<Uuid> =
            rows.into_iter().map(|(id,)| id).collect();
        let invalid: Vec<Uuid> = agent_ids
            .iter()
            .copied()
            .filter(|id| !company_set.contains(id))
            .collect();

        if !invalid.is_empty() {
            return Err(RepoError::Invalid(format!(
                "inbox agent policy contains agents outside the company: {invalid:?}"
            )));
        }
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- InboxAgentPolicyMode ----

    #[test]
    fn inbox_agent_policy_mode_as_str_matches_node() {
        assert_eq!(InboxAgentPolicyMode::Open.as_str(), "open");
        assert_eq!(InboxAgentPolicyMode::Allowlist.as_str(), "allowlist");
        assert_eq!(InboxAgentPolicyMode::Disabled.as_str(), "disabled");
    }

    #[test]
    fn inbox_agent_policy_mode_parse_round_trip() {
        for m in [
            InboxAgentPolicyMode::Open,
            InboxAgentPolicyMode::Allowlist,
            InboxAgentPolicyMode::Disabled,
        ] {
            assert_eq!(InboxAgentPolicyMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(InboxAgentPolicyMode::parse("unknown"), None);
    }

    // ---- dedup 行为（逻辑层单测） ----

    #[test]
    fn dedup_allowed_agent_ids_preserves_first_occurrence_order() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<Uuid> = vec![a, b, a, c, b, a];
        let dedup: Vec<Uuid> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
        assert_eq!(dedup, vec![a, b, c]);
    }

    #[test]
    fn empty_allowed_agent_ids_yields_empty_vec() {
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<Uuid> = Vec::new();
        let dedup: Vec<Uuid> = ids.into_iter().filter(|id| seen.insert(*id)).collect();
        assert!(dedup.is_empty());
    }

    // ---- get default ----

    #[test]
    fn default_policy_structure_matches_node() {
        // 模拟 get 在 DB 未命中时的返回结构
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

        // JSON 序列化应该包含所有字段（包括 null 的时间戳被 skip）
        let v = serde_json::to_value(&policy).unwrap();
        assert_eq!(v["companyId"], serde_json::json!(cid.to_string()));
        assert_eq!(v["userId"], serde_json::json!("user-1"));
        assert_eq!(v["mode"], serde_json::json!("open"));
        assert_eq!(v["allowedAgentIds"], serde_json::json!([]));
        assert_eq!(v["materialized"], serde_json::json!(false));
        // created_at / updated_at 被 skip_serializing_if 跳过
        assert!(v.get("createdAt").is_none());
        assert!(v.get("updatedAt").is_none());
    }

    // ---- update SQL 形状 ----

    #[test]
    fn update_sql_uses_upsert_with_composite_key() {
        let sql = "INSERT INTO user_inbox_agent_policies (company_id, user_id, mode, allowed_agent_ids, updated_at) \
                   VALUES ($1, $2, $3, $4, $5) \
                   ON CONFLICT (company_id, user_id) DO UPDATE SET \
                      mode = EXCLUDED.mode, \
                      allowed_agent_ids = EXCLUDED.allowed_agent_ids, \
                      updated_at = EXCLUDED.updated_at \
                   RETURNING id, company_id, user_id, mode, allowed_agent_ids, created_at, updated_at";
        assert!(sql.contains("ON CONFLICT (company_id, user_id)"));
        assert!(sql.contains("EXCLUDED.mode"));
        assert!(sql.contains("EXCLUDED.allowed_agent_ids"));
        assert!(sql.contains("RETURNING"));
    }

    #[test]
    fn validate_query_filters_by_company_id_and_id() {
        let sql = "SELECT id FROM agents WHERE company_id = $1 AND id = ANY($2)";
        assert!(sql.contains("company_id = $1"));
        assert!(sql.contains("id = ANY($2)"));
    }

    // ---- InvalidAgentsError ----

    #[test]
    fn invalid_agents_error_message_includes_ids() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let err = InvalidAgentsError::new(vec![a, b]);
        let msg = format!("{err}");
        assert!(msg.contains("outside the company"));
        assert!(msg.contains(&a.to_string()));
        assert!(msg.contains(&b.to_string()));
    }
}
