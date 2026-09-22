//! `multica-migrate` 库入口 —— CLI 子命令 + 共享工具。

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use mc_db::{Db, Migrator};
use serde::Serialize;
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

/// 解析数据库 URL(优先级: `--database-url` > `MULTICA_DATABASE_URL` > `DATABASE_URL`)。
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
    anyhow::bail!("database url not set: pass --database-url or MULTICA_DATABASE_URL/DATABASE_URL")
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

/// 默认需要 verify 的关键表集合（multica `schema_migrations` 初始化后可见）。
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
    // 上游没有本地自造的 `wakeup` / `plugin` 表（`344_plugin_v2_reset` 之后
    // 插件状态落在 `plugin_installation`）：唤醒用 `issue_wakeup` 系列。
    "issue_wakeup",
    "issue_wakeup_receipt",
];

/// 连接到 DB。
pub async fn connect_db(url: &str) -> Result<Db> {
    Db::connect(url, 5, 1).await.context("connect db")
}

/// 应用 migrations 目录里的所有 .up.sql 文件（可传多个目录，按词干合并）。
pub async fn run_migrations(db: &Db, dirs: Vec<PathBuf>) -> Result<usize> {
    let start = Instant::now();
    let steps = Migrator::load_dirs(&dirs).context("load migration files")?;
    info!(count = steps.len(), dirs = ?dirs, "loaded migration files");
    let applied = Migrator::run(db, steps).await?;
    info!(
        applied,
        elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
        "migrations done"
    );
    Ok(applied)
}

/// 就绪性报告：上游 readiness = **所有** up 版本都已记账 + 关键表存在。
///
/// 只比条数会漏掉「编号低于已应用版本的乱序补丁漏记录」（上游 `AllVersions()` 专防此病），
/// 所以这里列出**缺记账的版本清单**而不是只给布尔值。
#[derive(Debug, Clone, Serialize)]
pub struct Readiness {
    pub loaded: usize,
    pub applied: usize,
    pub pending: Vec<String>,
    pub missing_tables: Vec<String>,
}

impl Readiness {
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.pending.is_empty() && self.missing_tables.is_empty()
    }
}

/// 关键表是否存在（逐表 `to_regclass`）。
pub async fn missing_tables(db: &Db) -> Result<Vec<String>> {
    let mut missing = Vec::new();
    for table in DEFAULT_REQUIRED_TABLES {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(db.pool())
            .await
            .with_context(|| format!("probe table {table}"))?;
        if !exists {
            missing.push((*table).to_owned());
        }
    }
    Ok(missing)
}

/// 校验就绪性（不写库）。
pub async fn verify(db: &Db, dirs: Vec<PathBuf>) -> Result<Readiness> {
    let steps = Migrator::load_dirs(&dirs).context("load migration files")?;
    Ok(Readiness {
        loaded: steps.len(),
        applied: Migrator::list_applied(db).await?.len(),
        pending: Migrator::pending(db, &steps).await?,
        missing_tables: missing_tables(db).await?,
    })
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
