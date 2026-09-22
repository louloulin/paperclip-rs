//! `PatRepo` — DB-backed Personal Access Token 仓储。
//!
//! 对应 upstream `multica/server/pkg/db/queries/personal_access_token.sql`：
//! - `create`         — 写入 token hash + last4 + 过期时间
//! - `get_by_token`   — 通过 raw token 算 hash 再查（未撤销 + 未过期）
//! - `list_for_user`  — 列用户所有 PAT
//! - `revoke`         — 设置 `revoked_at = now()`
//!
//! 字段差异：上游用 `revoked BOOLEAN` + `token_prefix`，本 schema 用
//! `revoked_at TIMESTAMPTZ`，语义等价；prefix 暂不在 schema 内（last4 已足够
//! 做 UI 展示），后续若需要可以加迁移。
//!
//! 注意：**不要**触碰 `mc_auth::InMemoryPatStore` —— 它是 fallback，本仓库
//! 直接走 DB。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use crate::{RepoError, Result};

/// 新增 PAT 的入参。
#[derive(Debug, Clone)]
pub struct NewPat {
    pub user_id: Id,
    pub name: String,
    /// sha256 hex of the raw token
    pub token_hash: String,
    /// last 4 chars of raw token (for UI display)
    pub token_last4: String,
    pub expires_at: DateTime<Utc>,
    pub scopes: Vec<String>,
}

/// PAT 行视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatRow {
    pub id: Id,
    pub user_id: Id,
    pub name: String,
    pub token_hash: String,
    pub token_last4: String,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub scopes: serde_json::Value,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct PatRepo {
    pool: Arc<PgPool>,
}

impl PatRepo {
    pub fn new(db: Db) -> Self {
        Self { pool: Arc::new(db.pool().clone()) }
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool: Arc::new(pool) }
    }

    fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 把 raw token 换算成 hex(sha256(token))。
    pub fn hash_token(raw: &str) -> String {
        let mut h = Sha256::new();
        h.update(raw.as_bytes());
        hex::encode(h.finalize())
    }

    /// 计算 last4。
    pub fn last4(raw: &str) -> String {
        let n = raw.chars().count();
        if n < 4 {
            raw.to_string()
        } else {
            raw.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect()
        }
    }

    /// 插入一条新 PAT；`revoked_at = NULL`。
    pub async fn create(&self, input: NewPat) -> Result<PatRow> {
        let id = Uuid::new_v4();
        let scopes_json = serde_json::to_value(&input.scopes)
            .unwrap_or_else(|_| serde_json::json!([]));
        let row = sqlx::query_as::<_, PatRow>(
            r#"
            INSERT INTO personal_access_token
                (id, user_id, name, token_hash, token_last4,
                 expires_at, scopes, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, now())
            RETURNING id, user_id, name, token_hash, token_last4,
                      expires_at, last_used_at, scopes,
                      revoked_at, created_at
            "#,
        )
        .bind(id)
        .bind(input.user_id.as_uuid())
        .bind(&input.name)
        .bind(&input.token_hash)
        .bind(&input.token_last4)
        .bind(input.expires_at)
        .bind(scopes_json)
        .fetch_one(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(row)
    }

    /// 通过 raw token 查询：先 hash，再查未撤销、未过期。
    pub async fn get_by_token(&self, raw: &str) -> Result<Option<PatRow>> {
        let hash = Self::hash_token(raw);
        let row = sqlx::query_as::<_, PatRow>(
            r#"
            SELECT id, user_id, name, token_hash, token_last4,
                   expires_at, last_used_at, scopes,
                   revoked_at, created_at
            FROM personal_access_token
            WHERE token_hash = $1
              AND revoked_at IS NULL
              AND expires_at > now()
            "#,
        )
        .bind(&hash)
        .fetch_optional(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(row)
    }

    /// 列某用户所有未撤销的 PAT（按创建时间倒序）。
    pub async fn list_for_user(&self, user_id: Id) -> Result<Vec<PatRow>> {
        let rows = sqlx::query_as::<_, PatRow>(
            r#"
            SELECT id, user_id, name, token_hash, token_last4,
                   expires_at, last_used_at, scopes,
                   revoked_at, created_at
            FROM personal_access_token
            WHERE user_id = $1
              AND revoked_at IS NULL
            ORDER BY created_at DESC
            "#,
        )
        .bind(user_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(map_sqlx)?;

        Ok(rows)
    }

    /// 撤销 PAT（设置 `revoked_at = now()`）。
    pub async fn revoke(&self, id: Id) -> Result<()> {
        let res = sqlx::query(
            r#"
            UPDATE personal_access_token
            SET revoked_at = now()
            WHERE id = $1 AND revoked_at IS NULL
            "#,
        )
        .bind(id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx)?;

        if res.rows_affected() == 0 {
            // 已撤销或不存在 —— 都视为 "not found"
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    /// 更新 `last_used_at`（每次命中 token 时调用）。
    #[allow(dead_code)]
    pub async fn touch(&self, id: Id) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE personal_access_token
            SET last_used_at = now()
            WHERE id = $1
            "#,
        )
        .bind(id.as_uuid())
        .execute(self.pool())
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }
}

fn map_sqlx(e: sqlx::Error) -> RepoError {
    RepoError::Db(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn try_db() -> Option<Db> {
        let url = std::env::var("DATABASE_URL").ok()?;
        Db::connect(&url, 4, 1).await.ok()
    }

    /// 找一个或创建用户（需要 `user` 表中有匹配行才能跑 FK 测试）。
    /// 测试中我们直接插入临时 user 以避免依赖外部 fixture。
    async fn ensure_user(db: &Db) -> Id {
        let row: (Uuid,) = sqlx::query_as(
            "INSERT INTO \"user\" (name, email) VALUES ($1, $2) RETURNING id",
        )
        .bind(format!("pat-test-{}", Uuid::new_v4()))
        .bind(format!("pat-{}-{}@example.test", Uuid::new_v4(), Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("insert user");
        Id::from(row.0)
    }

    #[tokio::test]
    async fn create_and_get_by_token() {
        let Some(db) = try_db().await else {
            eprintln!("DATABASE_URL not set; skipping");
            return;
        };
        let user = ensure_user(&db).await;
        let repo = PatRepo::new(db);
        let raw = format!("mk_{}", Uuid::new_v4().simple());
        let row = repo
            .create(NewPat {
                user_id: user,
                name: "ci".into(),
                token_hash: PatRepo::hash_token(&raw),
                token_last4: PatRepo::last4(&raw),
                expires_at: Utc::now() + chrono::Duration::days(30),
                scopes: vec!["read".into()],
            })
            .await
            .expect("create");
        assert_eq!(row.user_id, user);
        assert!(row.revoked_at.is_none());

        let fetched = repo.get_by_token(&raw).await.expect("get").expect("exists");
        assert_eq!(fetched.id, row.id);
        assert_eq!(fetched.token_last4, PatRepo::last4(&raw));
    }

    #[tokio::test]
    async fn revoke_blocks_get_by_token() {
        let Some(db) = try_db().await else {
            return;
        };
        let user = ensure_user(&db).await;
        let repo = PatRepo::new(db);
        let raw = format!("mk_{}", Uuid::new_v4().simple());
        let row = repo
            .create(NewPat {
                user_id: user,
                name: "ci-2".into(),
                token_hash: PatRepo::hash_token(&raw),
                token_last4: PatRepo::last4(&raw),
                expires_at: Utc::now() + chrono::Duration::days(30),
                scopes: vec![],
            })
            .await
            .expect("create");
        repo.revoke(row.id).await.expect("revoke");
        let fetched = repo.get_by_token(&raw).await.expect("get after revoke");
        assert!(fetched.is_none(), "revoked PAT must not be returned");
    }

    #[tokio::test]
    async fn list_for_user_returns_active_only() {
        let Some(db) = try_db().await else {
            return;
        };
        let user = ensure_user(&db).await;
        let repo = PatRepo::new(db);
        let raw_a = format!("mk_{}", Uuid::new_v4().simple());
        let raw_b = format!("mk_{}", Uuid::new_v4().simple());
        let a = repo
            .create(NewPat {
                user_id: user,
                name: "a".into(),
                token_hash: PatRepo::hash_token(&raw_a),
                token_last4: PatRepo::last4(&raw_a),
                expires_at: Utc::now() + chrono::Duration::days(30),
                scopes: vec![],
            })
            .await
            .expect("create a");
        let _ = repo
            .create(NewPat {
                user_id: user,
                name: "b".into(),
                token_hash: PatRepo::hash_token(&raw_b),
                token_last4: PatRepo::last4(&raw_b),
                expires_at: Utc::now() + chrono::Duration::days(30),
                scopes: vec![],
            })
            .await
            .expect("create b");
        repo.revoke(a.id).await.expect("revoke a");

        let list = repo.list_for_user(user).await.expect("list");
        // 用户至少有 b，外加可能之前测试遗留的（未撤销）—— 不能直接 ==
        assert!(list.iter().all(|p| p.revoked_at.is_none()));
        assert!(list.iter().any(|p| p.name == "b"));
    }
}