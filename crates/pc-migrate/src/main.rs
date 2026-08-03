//! `paperclip-migrate` — Paperclip 数据库迁移独立工具。
//!
//! 与 `pc-server` 内置迁移等价，但作为独立二进制可：
//! - 在 CI / 部署脚本中独立调用
//! - 启动时检查迁移状态而不启动 HTTP 服务
//! - 在多副本部署中只让一个 pod 跑迁移（leader-election 由调用方处理）
//!
//! 子命令：
//! - `up`         应用所有 pending 迁移
//! - `status`     列出 available / applied / pending
//! - `verify`     比对 schema 与目标 manifest（轻量校验：表数量 + 关键表存在）
//! - `baseline`   把当前 schema 标记为基线（不执行任何迁移；用于从外部初始化 DB）

use std::time::Instant;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pc_db::{Db, Migrator};
use serde_json::json;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "paperclip-migrate",
    version,
    about = "Paperclip database migration tool"
)]
struct Cli {
    /// PostgreSQL connection string.
    /// 默认从 `PAPERCLIP_DATABASE_URL` 或 `DATABASE_URL` 读取。
    #[arg(
        long,
        env = "PAPERCLIP_DATABASE_URL",
        global = true
    )]
    database_url: Option<String>,

    /// 最大连接池大小。
    #[arg(long, default_value_t = 4, global = true)]
    max_connections: u32,

    /// 输出 JSON 格式（用于 CI / 自动化）。
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 应用所有 pending 迁移。
    Up {
        /// 仅打印计划，不真正执行。
        #[arg(long)]
        dry_run: bool,
    },
    /// 显示迁移状态。
    Status,
    /// 校验 schema：列出关键表是否存在。
    Verify {
        /// 期望存在的关键表名（逗号分隔）。默认与原 server 一致。
        #[arg(long, value_delimiter = ',', default_value = "companies,agents,issues,projects,heartbeat_runs,plugin_jobs,tool_invocations,tool_connections")]
        required_tables: Vec<String>,
    },
    /// 把当前 schema 标记为基线（仅插入历史记录，不跑迁移）。
    Baseline {
        /// 基线标签名（写入 __drizzle_migrations 表）
        #[arg(long, default_value = "external_baseline")]
        label: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    let url = resolve_url(&cli)?;

    let db = Db::connect(&url, cli.max_connections, 1)
        .await
        .with_context(|| format!("connect {}", redact_url(&url)))?;

    match cli.command {
        Command::Up { dry_run } => cmd_up(&db, dry_run, cli.json).await,
        Command::Status => cmd_status(&db, cli.json).await,
        Command::Verify { required_tables } => cmd_verify(&db, &required_tables, cli.json).await,
        Command::Baseline { label } => cmd_baseline(&db, &label, cli.json).await,
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn resolve_url(cli: &Cli) -> Result<String> {
    if let Some(url) = &cli.database_url {
        if !url.is_empty() {
            return Ok(url.clone());
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
    );
}

fn redact_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((_userinfo, host)) = rest.split_once('@') {
            return format!("{scheme}://***@{host}");
        }
    }
    url.to_string()
}

async fn cmd_up(db: &Db, dry_run: bool, json: bool) -> Result<()> {
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

async fn cmd_status(db: &Db, json: bool) -> Result<()> {
    let status = Migrator::status(db).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&json!({
            "available": status.available,
            "applied": status.applied,
            "pending": status.pending,
        }))?);
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

async fn cmd_verify(db: &Db, required: &[String], json: bool) -> Result<()> {
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
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "publicTables": total.0,
                "present": present,
                "missing": missing,
                "ok": missing.is_empty(),
            }))?
        );
    } else {
        println!("public tables: {}", total.0);
        println!("present: {} table(s)", present.len());
        for t in &present {
            println!("  ✓ {t}");
        }
        if !missing.is_empty() {
            println!("missing: {} table(s)", missing.len());
            for t in &missing {
                println!("  ✗ {t}");
            }
        }
    }
    if !missing.is_empty() {
        anyhow::bail!(
            "schema verification failed: {} required table(s) missing",
            missing.len()
        );
    }
    Ok(())
}

async fn cmd_baseline(db: &Db, label: &str, json: bool) -> Result<()> {
    // 仅插入历史记录；不执行 SQL。
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
}
