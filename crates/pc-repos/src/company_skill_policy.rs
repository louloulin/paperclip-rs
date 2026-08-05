//! `company_skill_policies` 域。

use pc_core::Timestamp;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct PolicyRow {
    pub company_id: Uuid,
    pub schema_version: i32,
    pub revision: i32,
    pub default_effect: String,
    pub rules: serde_json::Value,
    pub updated_at: Timestamp,
}

pub struct CompanySkillPolicyRepo<'a> {
    pub db: &'a Db,
}

impl<'a> CompanySkillPolicyRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// Round 181: 取公司 skill 策略（不存在返回 None）。
    pub async fn fetch(&self, company_id: Uuid) -> sqlx::Result<Option<PolicyRow>> {
        sqlx::query_as::<_, PolicyRow>(
            "SELECT company_id, schema_version, revision, default_effect, rules, updated_at \\
             FROM company_skill_policies WHERE company_id = $1",
        )
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await
    }

    /// Round 181: upsert 公司 skill 策略。new_revision 由调用方计算（= body.revision + 1）。
    pub async fn upsert(
        &self,
        company_id: Uuid,
        new_revision: i32,
        default_effect: &str,
        rules: &serde_json::Value,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO company_skill_policies \\
                (company_id, schema_version, revision, default_effect, rules, updated_at) \\
             VALUES ($1, 1, $2, $3, $4, now()) \\
             ON CONFLICT (company_id) DO UPDATE SET \\
                revision = company_skill_policies.revision + 1, \\
                default_effect = EXCLUDED.default_effect, \\
                rules = EXCLUDED.rules, \\
                updated_at = now()",
        )
        .bind(company_id)
        .bind(new_revision)
        .bind(default_effect)
        .bind(rules)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// Round 181: 删除公司 skill 策略。
    pub async fn delete(&self, company_id: Uuid) -> sqlx::Result<bool> {
        let n = sqlx::query("DELETE FROM company_skill_policies WHERE company_id = $1")
            .bind(company_id)
            .execute(self.db.pool())
            .await?
            .rows_affected();
        Ok(n > 0)
    }
}
