//! `multica-migrate` 库入口 —— CLI 子命令 + 共享工具。

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use mc_db::{Db, Migrator};
use serde_json::json;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// 初始化 tracing(幂等)。
pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

/// 解析数据库 URL(优先级: --database-url > MULTICA_DATABASE_URL > DATABASE_URL)。
pub fn resolve_url(cli_db: Option<&str>) -> Result<String> {
    if let Some(url) = cli_db {
        if !url.is_empty() {
            return Ok(url.to_owned());
        }
    }
    if let Ok(url) = std::env::var("MULTICA_DATABASE_URL") {
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
        "database url not set: pass --database-url or MULTICA_DATABASE_URL/DATABASE_URL"
    )
}

/// 从 URL 中脱敏 userinfo。
pub fn redact_url(url: &str) -> String {
    if let Some((scheme, rest)) = url.split_once("://") {
        if let Some((_userinfo, host)) = rest.split_once('@') {
            return format!("{scheme}://***@{host}");
        }
    }
    url.to_string()
}

/// 默认需要 verify 的关键表集合（multica schema_migrations 初始化后可见）。
pub const DEFAULT_REQUIRED_TABLES: &[&str] = &[
    "user",
    "workspace",
    "member",
    "agent",
    "agent_runtime",
    "issue",
    "comment",
    "project",
    "autopilot",
    "squad",
    "skill",
    "chat_session",
    "wakeup",
];

/// 连接到 DB。
pub async fn connect_db(url: &str) -> Result<Db> {
    Db::connect(url, 5, 1).await.context("connect db")
}

/// 应用 migrations 目录里的所有 .up.sql 文件。
pub async fn run_migrations(db: &Db, dir: PathBuf) -> Result<usize> {
    let start = Instant::now();
    let steps = Migrator::load(dir)?;
    info!(count = steps.len(), "loaded migration files");
    Migrator::run(db, steps).await?;
    let applied = Migrator::list_applied(db).await?.len();
    info!(applied, elapsed_ms = start.elapsed().as_millis() as u64, "migrations done");
    Ok(applied)
}

/// 输出 JSON 报告（用于 CI）。
pub fn report_status(applied: usize, db_redacted: &str, elapsed_ms: u64) -> String {
    serde_json::to_string_pretty(&json!({
        "applied": applied,
        "database": db_redacted,
        "elapsed_ms": elapsed_ms,
        "schema_version": env!("CARGO_PKG_VERSION"),
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_hides_password() {
        assert_eq!(
            redact_url("postgres://alice:hunter2@host:5432/db"),
            "postgres://***@host:5432/db"
        );
        assert_eq!(redact_url("postgres://host/db"), "postgres://host/db");
    }

    #[test]
    fn resolve_url_requires_env() {
        // Clean env
        let _saved_db = std::env::var("DATABASE_URL").ok();
        let _saved_mc = std::env::var("MULTICA_DATABASE_URL").ok();
        std::env::remove_var("DATABASE_URL");
        std::env::remove_var("MULTICA_DATABASE_URL");
        assert!(resolve_url(None).is_err());
    }

    #[test]
    fn resolve_url_prefers_cli() {
        assert_eq!(
            resolve_url(Some("postgres://cli/db")).unwrap(),
            "postgres://cli/db"
        );
    }
}