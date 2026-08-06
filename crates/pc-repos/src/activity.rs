//! `activity_log` 域 — 公司活动审计日志。
//!
//! 设计：
//! - 不可变 append-only 流水
//! - 通过 `actor_type` / `actor_id` 标识执行主体（user/agent/system/board/api_key）
//! - `entity_type` + `entity_id` 标识被作用对象（强类型业务对象）
//! - 提供按时间窗 / actor / entity / action 多维查询
//! - `details` 是自由 JSON（具体负载由 actor 上层决定）

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

const COLS: &str = "id, company_id, actor_type, actor_id, action, entity_type, entity_id,      agent_id, run_id, responsible_user_id, details, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub actor_type: String,
    pub actor_id: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
    pub details: Option<Value>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorType {
    User,
    Agent,
    System,
    Board,
    ApiKey,
    Plugin,
}
impl ActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
            Self::Board => "board",
            Self::ApiKey => "api_key",
            Self::Plugin => "plugin",
        }
    }

    /// Round 249: 从 Node 风格的小写字符串解析 actor 类型。
    /// 未识别值退化为 `System` —— 保持原有 `unwrap_or_else(|| "system".into())` 的语义。
    pub fn from_node_str(s: &str) -> Self {
        match s {
            "user" => Self::User,
            "agent" => Self::Agent,
            "board" => Self::Board,
            "api_key" => Self::ApiKey,
            "plugin" => Self::Plugin,
            _ => Self::System,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActivityFilter {
    pub actor_type: Option<ActorType>,
    pub actor_id: Option<String>,
    pub action: Option<String>,
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
    pub since: Option<Timestamp>,
    pub until: Option<Timestamp>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewActivity {
    pub company_id: Uuid,
    pub actor_type: ActorType,
    pub actor_id: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub agent_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub responsible_user_id: Option<String>,
    pub details: Option<Value>,
}

pub struct ActivityRepo<'a> {
    pub db: &'a Db,
}

impl<'a> ActivityRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 写入一条活动。不允许 actor_id/action/entity_id 为空。
    pub async fn record(&self, e: &NewActivity) -> RepoResult<ActivityRow> {
        if e.actor_id.trim().is_empty()
            || e.action.trim().is_empty()
            || e.entity_type.trim().is_empty()
            || e.entity_id.trim().is_empty()
        {
            return Err(RepoError::Invalid(
                "activity actor_id/action/entity_type/entity_id must not be empty".into(),
            ));
        }
        let sql = format!(
            "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type,                 entity_id, agent_id, run_id, responsible_user_id, details)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, ActivityRow>(&sql)
            .bind(e.company_id)
            .bind(e.actor_type.as_str())
            .bind(&e.actor_id)
            .bind(&e.action)
            .bind(&e.entity_type)
            .bind(&e.entity_id)
            .bind(e.agent_id)
            .bind(e.run_id)
            .bind(e.responsible_user_id.as_deref())
            .bind(e.details.clone())
            .fetch_one(self.db.pool())
            .await?)
    }

    /// 批量写入（一次性 insert with unnest），减少 round-trip。
    pub async fn record_batch(&self, items: &[NewActivity]) -> RepoResult<u64> {
        if items.is_empty() {
            return Ok(0);
        }
        let mut company_ids = Vec::with_capacity(items.len());
        let mut actor_types = Vec::with_capacity(items.len());
        let mut actor_ids = Vec::with_capacity(items.len());
        let mut actions = Vec::with_capacity(items.len());
        let mut entity_types = Vec::with_capacity(items.len());
        let mut entity_ids = Vec::with_capacity(items.len());
        let mut agent_ids = Vec::with_capacity(items.len());
        let mut run_ids = Vec::with_capacity(items.len());
        let mut responsible_user_ids = Vec::with_capacity(items.len());
        let mut details = Vec::with_capacity(items.len());
        for e in items {
            company_ids.push(e.company_id);
            actor_types.push(e.actor_type.as_str().to_owned());
            actor_ids.push(e.actor_id.clone());
            actions.push(e.action.clone());
            entity_types.push(e.entity_type.clone());
            entity_ids.push(e.entity_id.clone());
            agent_ids.push(e.agent_id);
            run_ids.push(e.run_id);
            responsible_user_ids.push(e.responsible_user_id.clone());
            details.push(e.details.clone());
        }
        let n = sqlx::query(
            "INSERT INTO activity_log (company_id, actor_type, actor_id, action, entity_type,                 entity_id, agent_id, run_id, responsible_user_id, details)              SELECT * FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[],                 $6::text[], $7::uuid[], $8::uuid[], $9::text[], $10::jsonb[])",
        )
        .bind(&company_ids)
        .bind(&actor_types)
        .bind(&actor_ids)
        .bind(&actions)
        .bind(&entity_types)
        .bind(&entity_ids)
        .bind(&agent_ids)
        .bind(&run_ids)
        .bind(&responsible_user_ids)
        .bind(&details)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    pub async fn list_for_company(
        &self,
        company_id: Uuid,
        filter: &ActivityFilter,
    ) -> RepoResult<Vec<ActivityRow>> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1000);
        let mut qb: sqlx::QueryBuilder<sqlx::Postgres> = sqlx::QueryBuilder::new(
            "SELECT id, company_id, actor_type, actor_id, action, entity_type, entity_id,              agent_id, run_id, responsible_user_id, details, created_at              FROM activity_log WHERE company_id = ",
        );
        qb.push_bind(company_id);
        if let Some(t) = filter.actor_type {
            qb.push(" AND actor_type = ").push_bind(t.as_str());
        }
        if let Some(id) = &filter.actor_id {
            qb.push(" AND actor_id = ").push_bind(id);
        }
        if let Some(action) = &filter.action {
            qb.push(" AND action = ").push_bind(action);
        }
        if let Some(et) = &filter.entity_type {
            qb.push(" AND entity_type = ").push_bind(et);
        }
        if let Some(eid) = &filter.entity_id {
            qb.push(" AND entity_id = ").push_bind(eid);
        }
        if let Some(aid) = filter.agent_id {
            qb.push(" AND agent_id = ").push_bind(aid);
        }
        if let Some(rid) = filter.run_id {
            qb.push(" AND run_id = ").push_bind(rid);
        }
        if let Some(uid) = &filter.responsible_user_id {
            qb.push(" AND responsible_user_id = ").push_bind(uid);
        }
        if let Some(s) = filter.since {
            qb.push(" AND created_at >= ").push_bind(s);
        }
        if let Some(u) = filter.until {
            qb.push(" AND created_at <= ").push_bind(u);
        }
        qb.push(" ORDER BY created_at DESC LIMIT ").push_bind(limit);
        let rows = qb
            .build_query_as::<ActivityRow>()
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    pub async fn list_for_entity(
        &self,
        company_id: Uuid,
        entity_type: &str,
        entity_id: &str,
        limit: i64,
    ) -> RepoResult<Vec<ActivityRow>> {
        let limit = limit.clamp(1, 1000);
        let sql = format!(
            "SELECT {COLS} FROM activity_log              WHERE company_id=$1 AND entity_type=$2 AND entity_id=$3              ORDER BY created_at DESC LIMIT $4"
        );
        Ok(sqlx::query_as::<_, ActivityRow>(&sql)
            .bind(company_id)
            .bind(entity_type)
            .bind(entity_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_for_run(
        &self,
        run_id: Uuid,
    ) -> RepoResult<Vec<ActivityRow>> {
        let sql = format!(
            "SELECT {COLS} FROM activity_log WHERE run_id=$1 ORDER BY created_at ASC"
        );
        Ok(sqlx::query_as::<_, ActivityRow>(&sql)
            .bind(run_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// 统计某种 action 出现的次数（用于 dashboard）
    pub async fn count_action(
        &self,
        company_id: Uuid,
        action: &str,
        since: Timestamp,
    ) -> RepoResult<i64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM activity_log              WHERE company_id=$1 AND action=$2 AND created_at >= $3",
        )
        .bind(company_id)
        .bind(action)
        .bind(since)
        .fetch_one(self.db.pool())
        .await?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_type_strings() {
        assert_eq!(ActorType::User.as_str(), "user");
        assert_eq!(ActorType::Agent.as_str(), "agent");
        assert_eq!(ActorType::System.as_str(), "system");
        assert_eq!(ActorType::Board.as_str(), "board");
        assert_eq!(ActorType::ApiKey.as_str(), "api_key");
        assert_eq!(ActorType::Plugin.as_str(), "plugin");
    }

    #[test]
    fn new_activity_validation() {
        let bad = NewActivity {
            company_id: Uuid::new_v4(),
            actor_type: ActorType::User,
            actor_id: "".into(),
            action: "x".into(),
            entity_type: "company".into(),
            entity_id: "c1".into(),
            agent_id: None,
            run_id: None,
            responsible_user_id: None,
            details: None,
        };
        assert!(bad.actor_id.trim().is_empty());
    }
}
