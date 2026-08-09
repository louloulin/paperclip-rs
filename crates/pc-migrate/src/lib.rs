//! `paperclip-migrate` 库入口 —— CLI 子命令 + 共享工具，供 binary 与 tests 复用。
//!
//! 与 Node `packages/db/src/migrate.ts` 等价的纯 Rust 实现。

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use pc_db::{Db, Migrator};
use serde_json::json;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// 初始化 tracing(幂等)。在测试里多次调用安全。
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// 解析数据库 URL(优先级: --database-url > PAPERCLIP_DATABASE_URL > DATABASE_URL)。
pub fn resolve_url(cli_db: Option<&str>) -> Result<String> {
    if let Some(url) = cli_db {
        if !url.is_empty() {
            return Ok(url.to_owned());
        }
    }
    if let Ok(url) = std::env::var("PAPERCLIP_DATABASE_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    if let Ok(url) = std::env::var("DATABASE_URL") {
        if !url.is_empty() {
            return Ok(url);
        }
    }
    anyhow::bail!(
        "database url not set: pass --database-url or PAPERCLIP_DATABASE_URL/DATABASE_URL"
    )
}

/// 从 URL 中脱敏 userinfo,保留 scheme+host。
pub fn redact_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((_userinfo, host)) = rest.split_once('@') {
            return format!("{scheme}://***@{host}");
        }
    }
    url.to_string()
}

/// 默认需要 verify 的关键表集合。
pub const DEFAULT_REQUIRED_TABLES: &[&str] = &[
    "companies",
    "agents",
    "issues",
    "projects",
    "heartbeat_runs",
    "plugin_jobs",
    "tool_invocations",
    "tool_connections",
];

/// 应用所有 pending 迁移。
pub async fn cmd_up(db: &Db, dry_run: bool, json: bool) -> Result<()> {
    let start = Instant::now();
    let status_before = Migrator::status(db).await?;
    if dry_run {
        if json {
            println!(
                "{}",
                json!({
                    "dryRun": true,
                    "available": status_before.available,
                    "appliedBefore": status_before.applied,
                    "pending": status_before.pending,
                    "durationMs": start.elapsed().as_millis() as i64,
                })
            );
        } else {
            println!(
                "[dry-run] pending migrations: {} (total available {})",
                status_before.pending.len(),
                status_before.available
            );
            for name in &status_before.pending {
                println!("  - {name}");
            }
        }
        return Ok(());
    }
    Migrator::run(db).await.context("apply migrations")?;
    let status_after = Migrator::status(db).await?;
    if json {
        println!(
            "{}",
            json!({
                "applied": status_after.applied - status_before.applied,
                "available": status_after.available,
                "appliedTotal": status_after.applied,
                "pending": status_after.pending,
                "durationMs": start.elapsed().as_millis() as i64,
            })
        );
    } else {
        info!(
            applied = status_after.applied - status_before.applied,
            available = status_after.available,
            pending = status_after.pending.len(),
            durationMs = start.elapsed().as_millis() as i64,
            "migrations up"
        );
        println!(
            "applied {} migration(s); {} pending ({} total available) in {:?}",
            status_after.applied - status_before.applied,
            status_after.pending.len(),
            status_after.available,
            start.elapsed()
        );
    }
    Ok(())
}

/// 回滚最近 N 步(此 build 无 down.sql,只打印元信息)。
pub async fn cmd_down(db: &Db, steps: u32, json: bool) -> Result<()> {
    let status = Migrator::status(db).await?;
    let _ = steps;
    let _ = status.applied;
    if json {
        println!(
            "{}",
            json!({
                "applied_count": status.applied,
                "note": "down.sql files not present in this build; no schema change applied"
            })
        );
    } else {
        println!(
            "{} migration(s) applied; no down.sql files present in this build, schema unchanged",
            status.applied
        );
    }
    Ok(())
}

/// 打印迁移状态(available / applied / pending)。
pub async fn cmd_status(db: &Db, json: bool) -> Result<()> {
    let status = Migrator::status(db).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "available": status.available,
                "applied": status.applied,
                "pending": status.pending,
            }))?
        );
    } else {
        println!("available: {}", status.available);
        println!("applied:   {}", status.applied);
        println!("pending:   {}", status.pending.len());
        for name in &status.pending {
            println!("  - {name}");
        }
    }
    Ok(())
}

/// verify 结果报告(供 tests 断言)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub public_tables: i32,
    pub present: Vec<String>,
    pub missing: Vec<String>,
}

/// 校验 schema(每个 required 表是否存在 + public 总表数)。
pub async fn cmd_verify_report(db: &Db, required: &[String]) -> Result<VerifyReport> {
    let pool = db.pool();
    let mut present = Vec::<String>::new();
    let mut missing = Vec::<String>::new();
    for table in required {
        let row: Option<(i32,)> = sqlx::query_as(
            "SELECT COUNT(*)::int FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = $1",
        )
        .bind(table)
        .fetch_optional(pool)
        .await
        .with_context(|| format!("query information_schema for {table}"))?;
        match row {
            Some((0,)) => missing.push(table.clone()),
            _ => present.push(table.clone()),
        }
    }
    let total: (i32,) = sqlx::query_as(
        "SELECT COUNT(*)::int FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_one(pool)
    .await
    .context("count public tables")?;
    Ok(VerifyReport {
        public_tables: total.0,
        present,
        missing,
    })
}

/// 校验 schema CLI 包装(打印 + 缺失时 bail)。
pub async fn cmd_verify(db: &Db, required: &[String], json: bool) -> Result<()> {
    let report = cmd_verify_report(db, required).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "publicTables": report.public_tables,
                "present": report.present,
                "missing": report.missing,
                "ok": report.missing.is_empty(),
            }))?
        );
    } else {
        println!("public tables: {}", report.public_tables);
        println!("present: {} table(s)", report.present.len());
        for t in &report.present {
            println!("  + {t}");
        }
        if !report.missing.is_empty() {
            println!("missing: {} table(s)", report.missing.len());
            for t in &report.missing {
                println!("  - {t}");
            }
        }
    }
    if !report.missing.is_empty() {
        anyhow::bail!(
            "schema verification failed: {} required table(s) missing",
            report.missing.len()
        );
    }
    Ok(())
}

/// 把当前 schema 标记为基线(只在 __drizzle_migrations 插入一行)。
pub async fn cmd_baseline(db: &Db, label: &str, json: bool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS __drizzle_migrations (\
         id SERIAL PRIMARY KEY, hash TEXT NOT NULL, created_at BIGINT NOT NULL)",
    )
    .execute(db.pool())
    .await
    .ok();
    let hash = format!("baseline-{label}-{}", chrono::Utc::now().timestamp_millis());
    let now_ms = chrono::Utc::now().timestamp_millis();
    sqlx::query("INSERT INTO __drizzle_migrations (hash, created_at) VALUES ($1, $2)")
        .bind(&hash)
        .bind(now_ms)
        .execute(db.pool())
        .await
        .context("insert baseline row")?;
    if json {
        println!("{}", json!({ "label": label, "hash": hash, "at": now_ms }));
    } else {
        info!(label, hash, "baseline recorded");
        println!("baseline recorded: {label} (hash={hash})");
    }
    Ok(())
}

/// 创建迁移文件骨架(无 DB 依赖)。
pub fn cmd_create(name: &str, dir: &PathBuf, json: bool) -> Result<PathBuf> {
    let safe = sanitize_name(name);
    if !is_valid_migration_name(&safe) {
        anyhow::bail!("invalid migration name: {name}");
    }
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let path = dir.join(format!("{stamp}_{safe}.sql"));
    let template = format!(
        "-- Migration {safe}\n-- Created by paperclip-migrate create\n\n-- Write your forward SQL here.\n",
    );
    std::fs::write(&path, template).with_context(|| format!("write {}", path.display()))?;
    if json {
        println!("{}", json!({ "path": path.display().to_string() }));
    } else {
        println!("created migration skeleton: {}", path.display());
    }
    Ok(path)
}

/// 将非法字符替换为 `_`(保留 `[A-Za-z0-9_-]`)。
pub fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 校验清洗后的迁移名是否仍是合法 snake_case:
/// - 至少 1 个字符
/// - 首字符必须是字母或下划线
/// - 全部字符匹配 `[A-Za-z0-9_-]`
pub fn is_valid_migration_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 跑 seed SQL(若文件存在)。返回是否执行。
pub async fn cmd_seed(db: &Db, file: &PathBuf, json: bool) -> Result<bool> {
    if !file.exists() {
        if json {
            println!(
                "{}",
                json!({ "applied": false, "reason": "seed file not found", "path": file.display().to_string() })
            );
        } else {
            println!("seed file not found: {}", file.display());
        }
        return Ok(false);
    }
    let sql = std::fs::read_to_string(file).with_context(|| format!("read {}", file.display()))?;
    sqlx::raw_sql(&sql)
        .execute(db.pool())
        .await
        .with_context(|| format!("execute {}", file.display()))?;
    if json {
        println!(
            "{}",
            json!({ "applied": true, "path": file.display().to_string(), "bytes": sql.len() })
        );
    } else {
        println!("seed applied: {} ({} bytes)", file.display(), sql.len());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_keeps_scheme_and_host() {
        let r = redact_url("postgres://user:pwd@host:5432/db");
        assert_eq!(r, "postgres://***@host:5432/db");
    }

    #[test]
    fn redact_url_no_userinfo_passthrough() {
        let r = redact_url("postgres://localhost/db");
        assert_eq!(r, "postgres://localhost/db");
    }

    #[test]
    fn sanitize_name_keeps_safe_chars() {
        assert_eq!(sanitize_name("add_companies_table"), "add_companies_table");
        assert_eq!(sanitize_name("add indexes"), "add_indexes");
        assert_eq!(sanitize_name(""), "");
        assert!(!is_valid_migration_name(""));
        assert!(is_valid_migration_name("add_table"));
        assert!(!is_valid_migration_name("1leading_digit"));
        assert!(!is_valid_migration_name("has space"));
    }

    #[test]
    fn default_required_tables_is_non_empty() {
        assert!(!DEFAULT_REQUIRED_TABLES.is_empty());
        assert!(DEFAULT_REQUIRED_TABLES.contains(&"companies"));
    }
}
