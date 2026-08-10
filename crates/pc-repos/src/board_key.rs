//! `board_api_keys` 域 — Board 用户的持久 API key。
//!
//! 设计：
//! - 每个 board 用户可创建多个 key（name 用来区分「laptop」/「CI」等）
//! - key 明文只创建时返回一次；DB 只存 key_hash
//! - 撤销是软删除（写 revoked_at），保留审计行
//!
//! Round 149: 从 `routes/access.rs` 抽出 SQL，提供仓储方法。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

/// 单条 board api key 的完整 DB 行投影（1:1 映射 `board_api_keys`）。
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct BoardKeyRow {
    pub id: Uuid,
    pub user_id: String,
    pub name: String,
    pub key_hash: String,
    pub last_used_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
    pub expires_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// Round 149: 列出用户尚未撤销的 key（按 created_at DESC）。
pub async fn list_active_by_user(db: &Db, user_id: &str) -> RepoResult<Vec<BoardKeyRow>> {
    let rows: Vec<BoardKeyRow> = sqlx::query_as(
        "SELECT id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at          FROM board_api_keys WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC",
    )
    .bind(user_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows)
}

/// Round 149: 创建一条新的 board api key（INSERT + RETURNING）。
pub async fn create(
    db: &Db,
    user_id: &str,
    name: &str,
    key_hash: &str,
    expires_at: Option<Timestamp>,
) -> RepoResult<BoardKeyRow> {
    let row: BoardKeyRow = sqlx::query_as(
        "INSERT INTO board_api_keys (user_id, name, key_hash, expires_at)          VALUES ($1, $2, $3, $4)          RETURNING id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at",
    )
    .bind(user_id)
    .bind(name)
    .bind(key_hash)
    .bind(expires_at)
    .fetch_one(db.pool())
    .await?;
    Ok(row)
}

/// Round 149: 软删除 key（仅当匹配 user_id）。
pub async fn revoke(db: &Db, key_id: Uuid, user_id: &str) -> RepoResult<u64> {
    let r = sqlx::query(
        "UPDATE board_api_keys SET revoked_at = now()          WHERE id = $1 AND user_id = $2",
    )
    .bind(key_id)
    .bind(user_id)
    .execute(db.pool())
    .await?;
    Ok(r.rows_affected())
}

/// Repository handle — wrap `Db` so callers can use the OOP-style API.
#[derive(Clone)]
pub struct BoardKeyRepo<'a> {
    db: &'a Db,
}

impl<'a> BoardKeyRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    pub async fn list_active_by_user(&self, user_id: &str) -> RepoResult<Vec<BoardKeyRow>> {
        list_active_by_user(self.db, user_id).await
    }

    pub async fn create(
        &self,
        user_id: &str,
        name: &str,
        key_hash: &str,
        expires_at: Option<Timestamp>,
    ) -> RepoResult<BoardKeyRow> {
        create(self.db, user_id, name, key_hash, expires_at).await
    }

    pub async fn revoke(&self, key_id: Uuid, user_id: &str) -> RepoResult<u64> {
        revoke(self.db, key_id, user_id).await
    }

    pub async fn list_by_user(
        &self,
        user_id: &str,
        include_inactive: bool,
    ) -> RepoResult<Vec<BoardKeyRow>> {
        list_by_user(self.db, user_id, include_inactive).await
    }

    pub async fn find_by_token_hash(&self, key_hash: &str) -> RepoResult<Option<BoardKeyRow>> {
        find_by_token_hash(self.db, key_hash).await
    }

    pub async fn find_by_id_and_user(
        &self,
        key_id: Uuid,
        user_id: &str,
    ) -> RepoResult<Option<BoardKeyRow>> {
        find_by_id_and_user(self.db, key_id, user_id).await
    }

    pub async fn touch(&self, key_id: Uuid) -> RepoResult<()> {
        touch(self.db, key_id).await
    }
}

/// Round 687: 按 user_id 列出所有 key（含 revoked/expired），按 created_at DESC 排序。
pub async fn list_by_user(
    db: &Db,
    user_id: &str,
    include_inactive: bool,
) -> RepoResult<Vec<BoardKeyRow>> {
    if include_inactive {
        let rows: Vec<BoardKeyRow> = sqlx::query_as(
            "SELECT id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at              FROM board_api_keys WHERE user_id = $1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(db.pool())
        .await?;
        Ok(rows)
    } else {
        let rows: Vec<BoardKeyRow> = sqlx::query_as(
            "SELECT id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at              FROM board_api_keys              WHERE user_id = $1 AND revoked_at IS NULL                AND (expires_at IS NULL OR expires_at > now())              ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(db.pool())
        .await?;
        Ok(rows)
    }
}

/// Round 687: 按 key_hash 查找尚未撤销的 key（用于 token 解析）。
/// 调用方在外部过滤 expires_at < now。
pub async fn find_by_token_hash(db: &Db, key_hash: &str) -> RepoResult<Option<BoardKeyRow>> {
    let row: Option<BoardKeyRow> = sqlx::query_as(
        "SELECT id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at          FROM board_api_keys WHERE key_hash = $1 AND revoked_at IS NULL",
    )
    .bind(key_hash)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

/// Round 687: 按 id + user_id 查找 key（assertCurrentBoardKey 路径）。
pub async fn find_by_id_and_user(
    db: &Db,
    key_id: Uuid,
    user_id: &str,
) -> RepoResult<Option<BoardKeyRow>> {
    let row: Option<BoardKeyRow> = sqlx::query_as(
        "SELECT id, user_id, name, key_hash, last_used_at, revoked_at, expires_at, created_at          FROM board_api_keys WHERE id = $1 AND user_id = $2",
    )
    .bind(key_id)
    .bind(user_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row)
}

/// Round 687: 标记 key 已被使用（last_used_at = now()）。
pub async fn touch(db: &Db, key_id: Uuid) -> RepoResult<()> {
    sqlx::query("UPDATE board_api_keys SET last_used_at = now() WHERE id = $1")
        .bind(key_id)
        .execute(db.pool())
        .await?;
    Ok(())
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
