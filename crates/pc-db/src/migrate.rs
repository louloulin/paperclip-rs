//! 与原 Paperclip Drizzle 数据库迁移兼容的运行器。

use std::collections::HashSet;

use serde::Deserialize;
use sqlx::{Executor, PgPool, Postgres, Row, Transaction};
use tracing::info;

use crate::{Db, DbError};

const STATEMENT_BREAKPOINT: &str = "--> statement-breakpoint";
const JOURNAL_JSON: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/migrations/drizzle/meta/_journal.json"
));

#[derive(Debug, Clone, Copy)]
struct MigrationSource {
    name: &'static str,
    sql: &'static str,
}

static MIGRATIONS: &[MigrationSource] =
    include!(concat!(env!("OUT_DIR"), "/drizzle_migrations.rs"));

#[derive(Debug, Deserialize)]
struct Journal {
    entries: Vec<JournalEntry>,
}

#[derive(Debug, Deserialize)]
struct JournalEntry {
    idx: i64,
    tag: String,
    when: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationStatus {
    pub available: usize,
    pub applied: usize,
    pub pending: Vec<String>,
}

pub struct Migrator;

impl Migrator {
    pub async fn run(db: &Db) -> Result<(), DbError> {
        ensure_history_table(db.pool()).await?;
        let applied = load_applied_names(db.pool()).await?;

        for migration in ordered_migrations()? {
            if applied.contains(migration.name) {
                continue;
            }
            apply_migration(db.pool(), migration).await?;
            info!(migration = migration.name, "database migration applied");
        }
        Ok(())
    }

    pub async fn status(db: &Db) -> Result<MigrationStatus, DbError> {
        ensure_history_table(db.pool()).await?;
        let applied = load_applied_names(db.pool()).await?;
        let ordered = ordered_migrations()?;
        let pending = ordered
            .iter()
            .filter(|migration| !applied.contains(migration.name))
            .map(|migration| migration.name.to_owned())
            .collect();

        Ok(MigrationStatus {
            available: ordered.len(),
            applied: applied.len(),
            pending,
        })
    }
}

fn ordered_migrations() -> Result<Vec<MigrationSource>, DbError> {
    let journal: Journal = serde_json::from_str(JOURNAL_JSON)
        .map_err(|error| DbError::MigrationManifest(error.to_string()))?;
    let by_name = MIGRATIONS
        .iter()
        .map(|migration| (migration.name, *migration))
        .collect::<std::collections::HashMap<_, _>>();

    let mut entries = journal.entries;
    entries.sort_by_key(|entry| entry.idx);
    entries
        .into_iter()
        .map(|entry| {
            let name = format!("{}.sql", entry.tag);
            by_name.get(name.as_str()).copied().ok_or_else(|| {
                DbError::MigrationManifest(format!("journal references missing migration {name}"))
            })
        })
        .collect()
}

async fn ensure_history_table(pool: &PgPool) -> Result<(), DbError> {
    pool.execute("CREATE SCHEMA IF NOT EXISTS drizzle").await?;
    pool.execute(
        "CREATE TABLE IF NOT EXISTS drizzle.__drizzle_migrations (\
         id BIGSERIAL PRIMARY KEY, \
         hash TEXT NOT NULL, \
         created_at BIGINT NOT NULL, \
         name TEXT UNIQUE)",
    )
    .await?;
    pool.execute("ALTER TABLE drizzle.__drizzle_migrations ADD COLUMN IF NOT EXISTS name TEXT")
        .await?;
    Ok(())
}

async fn load_applied_names(pool: &PgPool) -> Result<HashSet<String>, DbError> {
    let rows = sqlx::query("SELECT name FROM drizzle.__drizzle_migrations WHERE name IS NOT NULL")
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| row.try_get::<Option<String>, _>("name").ok().flatten())
        .collect())
}

async fn apply_migration(pool: &PgPool, migration: MigrationSource) -> Result<(), DbError> {
    let journal: Journal = serde_json::from_str(JOURNAL_JSON)
        .map_err(|error| DbError::MigrationManifest(error.to_string()))?;
    let created_at = journal
        .entries
        .iter()
        .find(|entry| format!("{}.sql", entry.tag) == migration.name)
        .map_or(0, |entry| entry.when);
    let hash = sha256_hex(migration.sql);
    let mut transaction = pool.begin().await?;

    for statement in split_statements(migration.sql) {
        transaction.execute(statement).await?;
    }
    record_history(&mut transaction, migration.name, &hash, created_at).await?;
    transaction.commit().await?;
    Ok(())
}

async fn record_history(
    transaction: &mut Transaction<'_, Postgres>,
    name: &str,
    hash: &str,
    created_at: i64,
) -> Result<(), DbError> {
    sqlx::query(
        "INSERT INTO drizzle.__drizzle_migrations (hash, created_at, name) VALUES ($1, $2, $3) \
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(hash)
    .bind(created_at)
    .bind(name)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn split_statements(sql: &str) -> impl Iterator<Item = &str> {
    sql.split(STATEMENT_BREAKPOINT)
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
}

fn sha256_hex(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_manifest_matches_embedded_files() {
        let ordered = ordered_migrations().unwrap();
        assert_eq!(ordered.len(), 205);
        assert_eq!(
            ordered.first().unwrap().name,
            "0000_mature_masked_marvel.sql"
        );
        // 最后一条以序号 + sql 结尾；不需要硬编码名字以避免每次新迁移都要改测试
        let last = ordered.last().unwrap().name.clone();
        assert!(
            last.ends_with(".sql"),
            "last migration must end with .sql: {last}"
        );
    }

    #[test]
    fn split_drizzle_statements() {
        let statements =
            split_statements("SELECT 1;--> statement-breakpoint\nSELECT 2;").collect::<Vec<_>>();
        assert_eq!(statements, ["SELECT 1;", "SELECT 2;"]);
    }

    #[test]
    fn split_drizzle_statements_with_comments() {
        let statements =
            split_statements("-- header comment\nSELECT 1;--> statement-breakpoint\n\nSELECT 2;")
                .collect::<Vec<_>>();
        assert_eq!(statements, ["-- header comment\nSELECT 1;", "SELECT 2;"]);
    }
}
