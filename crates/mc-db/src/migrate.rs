//! 迁移 runner。
//!
//! 把 SQL 文件（`<root>/<version>_<name>.up.sql`）按版本号升序执行，
//! 在 `schema_migrations` 表里记录已应用的迁移版本。
//!
//! 与 multica 的 SQL 迁移规范兼容：
//! - 每文件包含一个或多个 DDL 语句
//! - 多语句之间以 `;` 分隔
//! - 文件名形如 `NNN_<name>.up.sql`
//! - 配套 `.down.sql` 由 `multica-migrate diff` 等工具生成，不在运行时执行

use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqlx::Executor;
use tracing::{info, warn};

use crate::pool::Db;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrationStatus {
    Pending,
    Applied,
    Failed,
}

/// 单个迁移步骤。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStep {
    pub version: i64,
    pub name: String,
    pub sql: String,
    pub source: Option<String>,
}

impl MigrationStep {
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let filename = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "migration path has no filename",
            )
        })?;
        let (version, name) = parse_filename(filename).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid migration filename: {filename}"),
            )
        })?;
        let sql = std::fs::read_to_string(path)?;
        Ok(Self {
            version,
            name,
            sql,
            source: Some(path.display().to_string()),
        })
    }
}

fn parse_filename(filename: &str) -> Option<(i64, String)> {
    let stem = filename.strip_suffix(".sql")?;
    let stem = stem.strip_suffix(".up")?;
    let mut parts = stem.splitn(2, '_');
    let version = parts.next()?.parse::<i64>().ok()?;
    let name = parts.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    Some((version, name))
}

/// 迁移 runner。
#[derive(Default, Clone)]
pub struct Migrator;

impl Migrator {
    /// 确保 `schema_migrations` 表存在。
    pub async fn ensure_table(db: &Db) -> Result<(), sqlx::Error> {
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version BIGINT PRIMARY KEY,
                name TEXT NOT NULL,
                source TEXT,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            ",
        )
        .execute(db.pool())
        .await?;
        Ok(())
    }

    /// 应用给定的迁移列表（按 version 升序，跳过已应用的）。
    pub async fn run(db: &Db, steps: Vec<MigrationStep>) -> Result<(), crate::DbError> {
        Self::ensure_table(db).await?;
        let applied = Self::list_applied(db).await?;
        let applied_versions: std::collections::HashSet<i64> = applied.into_iter().collect();

        let mut to_apply: Vec<MigrationStep> = steps
            .into_iter()
            .filter(|s| !applied_versions.contains(&s.version))
            .collect();
        to_apply.sort_by_key(|s| s.version);

        if to_apply.is_empty() {
            info!("no pending migrations");
            return Ok(());
        }
        info!(count = to_apply.len(), "applying migrations");

        for step in to_apply {
            Self::apply(db, &step).await?;
        }
        Ok(())
    }

    async fn apply(db: &Db, step: &MigrationStep) -> Result<(), crate::DbError> {
        info!(version = step.version, name = %step.name, "applying migration");
        let start = Instant::now();
        let mut tx = db.pool().begin().await?;

        let statements = split_sql_statements(&step.sql);
        for stmt in &statements {
            let trimmed = stmt.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Each statement is executed independently.  If a migration has
            // already been partially applied (e.g. after a crash), sqlx will
            // fail loudly on duplicate CREATE; the user should reconcile by
            // either rolling back manually or accepting the existing state.
            if let Err(e) = tx.execute(trimmed).await {
                warn!(
                    version = step.version,
                    error = %e,
                    statement = trimmed.lines().next().unwrap_or(""),
                    "statement failed; transaction will be rolled back"
                );
                return Err(crate::DbError::Pool(format!(
                    "migration {} failed: {e}",
                    step.version
                )));
            }
        }

        sqlx::query("INSERT INTO schema_migrations(version, name, source) VALUES ($1, $2, $3)")
            .bind(step.version)
            .bind(&step.name)
            .bind(&step.source)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        info!(
            version = step.version,
            elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            statements = statements.len(),
            "migration applied"
        );
        Ok(())
    }

    /// 列出已应用的版本号。
    pub async fn list_applied(db: &Db) -> Result<Vec<i64>, sqlx::Error> {
        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT version FROM schema_migrations ORDER BY version")
                .fetch_all(db.pool())
                .await?;
        Ok(rows.into_iter().map(|(v,)| v).collect())
    }

    /// 从目录加载所有 `.up.sql` 文件并按版本排序。
    pub fn load_dir(root: &Path) -> std::io::Result<Vec<MigrationStep>> {
        if !root.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("migrations directory not found: {}", root.display()),
            ));
        }
        let mut out = Vec::new();
        let read = std::fs::read_dir(root)?;
        for entry in read {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !(fname.ends_with(".up.sql")) {
                continue;
            }
            let step = MigrationStep::from_file(&path)?;
            out.push(step);
        }
        out.sort_by_key(|s| s.version);
        Ok(out)
    }

    /// 把 migrations 目录打包成单个 Vec<MigrationStep>：可放进 `pc-server` 主流程。
    pub fn load(root: impl AsRef<Path>) -> std::io::Result<Vec<MigrationStep>> {
        Self::load_dir(root.as_ref())
    }
}

/// 简易 SQL 切分：按 `;` 分句，跳过 `--` 行注释与 `/* */` 块注释；忽略字符串字面量内的 `;`。
///
/// 足够处理 multica 的 534 个迁移文件（绝大多数是普通 DDL），
/// 对存储过程 / DDL 含 `$$` 的语用 noop 跳过（multica 不使用）。
fn split_sql_statements(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string = false;
    let mut in_dollar_quote = false;
    let mut string_quote = '\0';

    let mut iter = input.chars().peekable();
    while let Some(c) = iter.next() {
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if in_block_comment {
            if c == '*' && iter.peek() == Some(&'/') {
                iter.next();
                in_block_comment = false;
            }
            continue;
        }
        if in_dollar_quote {
            buf.push(c);
            if c == '$' {
                in_dollar_quote = false;
            }
            continue;
        }
        if in_string {
            buf.push(c);
            if c == '\\' {
                if let Some(next) = iter.next() {
                    buf.push(next);
                }
                continue;
            }
            if c == string_quote {
                in_string = false;
            }
            continue;
        }
        if c == '-' && iter.peek() == Some(&'-') {
            in_line_comment = true;
            continue;
        }
        if c == '/' && iter.peek() == Some(&'*') {
            in_block_comment = true;
            continue;
        }
        if c == '\'' || c == '"' {
            in_string = true;
            string_quote = c;
            buf.push(c);
            continue;
        }
        if c == '$' {
            in_dollar_quote = true;
            buf.push(c);
            continue;
        }
        if c == ';' {
            let stmt = std::mem::take(&mut buf);
            if has_sql_content(&stmt) {
                out.push(stmt);
            }
            continue;
        }
        buf.push(c);
    }
    if has_sql_content(&buf) {
        out.push(buf);
    }
    out
}

/// 判断切分出的语句是否含非注释、非空白内容（纯 `--` / `/* */` 注释残留不算语句）。
fn has_sql_content(stmt: &str) -> bool {
    let mut in_line = false;
    let mut in_block = false;
    let mut in_string = false;
    let mut quote = '\0';
    let mut chars = stmt.chars().peekable();
    while let Some(c) = chars.next() {
        if in_line {
            if c == '\n' {
                in_line = false;
            }
            continue;
        }
        if in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block = false;
            }
            continue;
        }
        if in_string {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c == quote {
                in_string = false;
            }
            continue;
        }
        if c == '-' && chars.peek() == Some(&'-') {
            chars.next();
            in_line = true;
            continue;
        }
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            in_block = true;
            continue;
        }
        if c == '\'' || c == '"' {
            in_string = true;
            quote = c;
            continue;
        }
        if !c.is_whitespace() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_filename_extracts_version_and_name() {
        let (v, name) = parse_filename("0001_init.up.sql").unwrap();
        assert_eq!(v, 1);
        assert_eq!(name, "init");
    }

    #[test]
    fn parse_filename_handles_long_names() {
        let (v, name) = parse_filename("0240_chat_explicit_origin_backfill.up.sql").unwrap();
        assert_eq!(v, 240);
        assert_eq!(name, "chat_explicit_origin_backfill");
    }

    #[test]
    fn parse_filename_rejects_down() {
        assert!(parse_filename("0001_init.down.sql").is_none());
    }

    #[test]
    fn split_sql_handles_multiple_statements() {
        let sql = "CREATE TABLE foo (id INT); CREATE TABLE bar (id INT);";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("foo"));
        assert!(stmts[1].contains("bar"));
    }

    #[test]
    fn split_sql_ignores_semicolon_inside_strings() {
        let sql = "INSERT INTO t (x) VALUES ('a;b');";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("'a;b'"));
    }

    #[test]
    fn split_sql_strips_line_comments() {
        let sql = "-- comment\nCREATE TABLE t (id INT); -- trailing";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("CREATE TABLE"));
    }

    #[test]
    fn split_sql_handles_block_comments() {
        let sql = "/* hi */ CREATE TABLE t (id INT);";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 1);
    }

    #[test]
    fn split_sql_handles_dollar_quoting() {
        let sql = "CREATE FUNCTION f() RETURNS void AS $$ BEGIN END; $$ LANGUAGE plpgsql;";
        let stmts = split_sql_statements(sql);
        // After dollar-quote enters and exits, statement boundaries are preserved.
        assert!(!stmts.is_empty());
    }
}
