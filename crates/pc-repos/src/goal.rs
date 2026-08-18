//! `goals` 域 — 公司目标树（支持父子层级）。
//!
//! 设计：
//! - `level` 表示粒度：mission / company / team / project / task
//! - `status` 状态机：planned → active → completed | cancelled | blocked
//! - 父子递归：parent_id 自引用，存 project ref / agent owner
//! - 提供 ancestors() 帮助上溯；descendants() 做子树范围（用 CTE）

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalLevel {
    Mission,
    Company,
    Team,
    Project,
    Task,
}
impl GoalLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mission => "mission",
            Self::Company => "company",
            Self::Team => "team",
            Self::Project => "project",
            Self::Task => "task",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "mission" => Some(Self::Mission),
            "company" => Some(Self::Company),
            "team" => Some(Self::Team),
            "project" => Some(Self::Project),
            "task" => Some(Self::Task),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Planned,
    Active,
    Completed,
    Cancelled,
    Blocked,
}
impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "planned" => Some(Self::Planned),
            "active" => Some(Self::Active),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }
}

const COLS: &str = "id, company_id, title, description, level, status, parent_id,      owner_agent_id, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub level: String,
    pub status: String,
    pub parent_id: Option<Uuid>,
    pub owner_agent_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewGoal {
    pub company_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub level: GoalLevel,
    pub status: GoalStatus,
    pub parent_id: Option<Uuid>,
    pub owner_agent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct GoalPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub level: Option<GoalLevel>,
    pub status: Option<GoalStatus>,
    pub parent_id: Option<Option<Uuid>>,
    pub owner_agent_id: Option<Option<Uuid>>,
}

pub struct GoalRepo<'a> {
    pub db: &'a Db,
}

impl<'a> GoalRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_by_company(&self, company_id: Uuid) -> RepoResult<Vec<GoalRow>> {
        let sql =
            format!("SELECT {COLS} FROM goals WHERE company_id=$1 ORDER BY level, created_at DESC");
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_all(&self, limit: i64) -> RepoResult<Vec<GoalRow>> {
        let sql = format!("SELECT {COLS} FROM goals ORDER BY created_at DESC LIMIT $1");
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_roots(&self, company_id: Uuid) -> RepoResult<Vec<GoalRow>> {
        let sql = format!(
            "SELECT {COLS} FROM goals WHERE company_id=$1 AND parent_id IS NULL              ORDER BY created_at DESC"
        );
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn list_children(&self, parent_id: Uuid) -> RepoResult<Vec<GoalRow>> {
        let sql = format!("SELECT {COLS} FROM goals WHERE parent_id=$1 ORDER BY created_at DESC");
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(parent_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<GoalRow>> {
        let sql = format!("SELECT {COLS} FROM goals WHERE company_id=$1 AND id=$2");
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, g: &NewGoal) -> RepoResult<GoalRow> {
        if g.title.trim().is_empty() {
            return Err(RepoError::Invalid("goal title must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO goals (company_id, title, description, level, status, parent_id, owner_agent_id)              VALUES ($1,$2,$3,$4,$5,$6,$7)              RETURNING {COLS}"
        );
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(g.company_id)
            .bind(&g.title)
            .bind(g.description.as_deref())
            .bind(g.level.as_str())
            .bind(g.status.as_str())
            .bind(g.parent_id)
            .bind(g.owner_agent_id)
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn patch(
        &self,
        company_id: Uuid,
        id: Uuid,
        p: &GoalPatch,
    ) -> RepoResult<Option<GoalRow>> {
        if let Some(Some(new_parent)) = p.parent_id {
            if new_parent == id {
                return Err(RepoError::Invalid("goal cannot be its own parent".into()));
            }
            if self.would_create_cycle(id, new_parent).await? {
                return Err(RepoError::Invalid(
                    "moving goal would create a cycle".into(),
                ));
            }
        }
        let sql = format!(
            "UPDATE goals SET                 title = COALESCE($2, title),                 description = COALESCE($3, description),                 level = COALESCE($4, level),                 status = COALESCE($5, status),                 parent_id = CASE WHEN $6::bool THEN $7::uuid ELSE parent_id END,                 owner_agent_id = CASE WHEN $8::bool THEN $9::uuid ELSE owner_agent_id END,                 updated_at = now()              WHERE company_id=$1 AND id=$10              RETURNING {COLS}"
        );
        let has_parent = p.parent_id.is_some();
        let parent_value = p.parent_id.flatten();
        let has_owner = p.owner_agent_id.is_some();
        let owner_value = p.owner_agent_id.flatten();
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(company_id)
            .bind(p.title.as_deref())
            .bind(p.description.as_deref())
            .bind(p.level.map(|l| l.as_str()))
            .bind(p.status.map(|s| s.as_str()))
            .bind(has_parent)
            .bind(parent_value)
            .bind(has_owner)
            .bind(owner_value)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    async fn would_create_cycle(&self, id: Uuid, new_parent: Uuid) -> RepoResult<bool> {
        let mut cur: Option<Uuid> = Some(new_parent);
        for _ in 0..512 {
            match cur {
                None => return Ok(false),
                Some(p) if p == id => return Ok(true),
                Some(p) => {
                    let n: Option<Uuid> =
                        sqlx::query_scalar("SELECT parent_id FROM goals WHERE id=$1")
                            .bind(p)
                            .fetch_optional(self.db.pool())
                            .await?;
                    cur = n;
                }
            }
        }
        Ok(true)
    }

    /// Back-compat shim: simple update.
    #[allow(dead_code)]
    pub async fn update(
        &self,
        id: Uuid,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        parent_id: Option<Uuid>,
        owner_agent_id: Option<Uuid>,
    ) -> RepoResult<Option<GoalRow>> {
        let cid: Option<Uuid> = sqlx::query_scalar("SELECT company_id FROM goals WHERE id=$1")
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?;
        let cid = cid.ok_or_else(|| RepoError::NotFound {
            entity: "goal",
            id: id.to_string(),
        })?;
        let p = GoalPatch {
            title: title.map(String::from),
            description: description.map(String::from),
            status: status.and_then(|s| GoalStatus::parse(s)),
            parent_id: if parent_id.is_some() {
                Some(parent_id)
            } else {
                None
            },
            owner_agent_id: if owner_agent_id.is_some() {
                Some(owner_agent_id)
            } else {
                None
            },
            ..Default::default()
        };
        self.patch(cid, id, &p).await
    }

    pub async fn ancestors(&self, goal_id: Uuid) -> RepoResult<Vec<GoalRow>> {
        // PostgreSQL recursive CTE
        let rows = sqlx::query_as::<_, GoalRow>(
            "WITH RECURSIVE chain AS (                SELECT g.* FROM goals g WHERE g.id = $1                 UNION ALL                 SELECT g.* FROM goals g JOIN chain c ON g.id = c.parent_id             ) SELECT id, company_id, title, description, level, status, parent_id, owner_agent_id,                created_at, updated_at FROM chain WHERE id <> $1 ORDER BY level DESC"
        )
        .bind(goal_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    pub async fn descendants(&self, goal_id: Uuid) -> RepoResult<Vec<GoalRow>> {
        let rows = sqlx::query_as::<_, GoalRow>(
            "WITH RECURSIVE subtree AS (                SELECT g.* FROM goals g WHERE g.id = $1                 UNION ALL                 SELECT g.* FROM goals g JOIN subtree s ON g.parent_id = s.id             ) SELECT id, company_id, title, description, level, status, parent_id, owner_agent_id,                created_at, updated_at FROM subtree WHERE id <> $1 ORDER BY level"
        )
        .bind(goal_id)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    /// R799: returns the deleted row directly (was bool). 0 rows = `RowNotFound`.
    pub async fn delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<GoalRow> {
        // FK 不强制 ON DELETE CASCADE：将子级变孤儿 → 拒绝
        let cnt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM goals WHERE parent_id=$1")
            .bind(id)
            .fetch_one(self.db.pool())
            .await?;
        if cnt > 0 {
            return Err(RepoError::Invalid(
                "goal has children; re-parent or delete children first".into(),
            ));
        }
        sqlx::query_as::<_, GoalRow>(
            "DELETE FROM goals WHERE company_id=$1 AND id=$2 \
             RETURNING id, company_id, title, description, level, status, parent_id, \
                owner_agent_id, created_at, updated_at",
        )
        .bind(company_id)
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or_else(|| RepoError::NotFound { entity: "goal", id: id.to_string() })
    }

    pub async fn count_by_status(&self, company_id: Uuid, status: GoalStatus) -> RepoResult<i64> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM goals WHERE company_id=$1 AND status=$2")
                .bind(company_id)
                .bind(status.as_str())
                .fetch_one(self.db.pool())
                .await?;
        Ok(n)
    }

    /// Back-compat: simple create(company_id, title, description, owner_agent_id).
    #[allow(dead_code)]
    pub async fn create_simple(
        &self,
        company_id: Uuid,
        title: &str,
        description: Option<&str>,
        owner_agent_id: Option<Uuid>,
    ) -> RepoResult<GoalRow> {
        let n = NewGoal {
            company_id,
            title: title.into(),
            description: description.map(String::from),
            level: GoalLevel::Task,
            status: GoalStatus::Planned,
            parent_id: None,
            owner_agent_id,
        };
        self.create(&n).await
    }

    /// Back-compat: get by id only.
    #[allow(dead_code)]
    pub async fn get_id(&self, id: Uuid) -> RepoResult<Option<GoalRow>> {
        let sql = format!("SELECT {COLS} FROM goals WHERE id=$1");
        Ok(sqlx::query_as::<_, GoalRow>(&sql)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// Back-compat: delete by id only. R799: returns GoalRow (was bool).
    #[allow(dead_code)]
    pub async fn delete_one(&self, id: Uuid) -> RepoResult<GoalRow> {
        sqlx::query_as::<_, GoalRow>(
            "DELETE FROM goals WHERE id=$1 \
             RETURNING id, company_id, title, description, level, status, parent_id, \
                owner_agent_id, created_at, updated_at",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or_else(|| RepoError::NotFound { entity: "goal", id: id.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_status_state_machine() {
        assert!(!GoalStatus::Planned.is_terminal());
        assert!(!GoalStatus::Active.is_terminal());
        assert!(!GoalStatus::Blocked.is_terminal());
        assert!(GoalStatus::Completed.is_terminal());
        assert!(GoalStatus::Cancelled.is_terminal());
    }

    #[test]
    fn level_round_trip() {
        for l in [
            GoalLevel::Mission,
            GoalLevel::Company,
            GoalLevel::Team,
            GoalLevel::Project,
            GoalLevel::Task,
        ] {
            assert_eq!(GoalLevel::parse(l.as_str()), Some(l));
        }
        assert_eq!(GoalLevel::parse("nope"), None);
    }

    #[test]
    fn new_goal_requires_title() {
        let g = NewGoal {
            company_id: Uuid::new_v4(),
            title: "".into(),
            description: None,
            level: GoalLevel::Task,
            status: GoalStatus::Planned,
            parent_id: None,
            owner_agent_id: None,
        };
        assert!(g.title.trim().is_empty());
    }
}
