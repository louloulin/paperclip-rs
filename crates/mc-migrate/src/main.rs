//! `multica-migrate` CLI

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use mc_migrate::{
    connect_db, init_tracing, redact_url, report_status, resolve_url, run_migrations, verify,
};

#[derive(Parser)]
#[command(name = "multica-migrate", version, about = "Multica migration CLI")]
struct Cli {
    /// Database URL override (default: `MULTICA_DATABASE_URL`)
    #[arg(long)]
    database_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run pending migrations
    Run {
        /// Migrations directory (repeatable; defaults to `migrations`)
        #[arg(long, default_value = "migrations")]
        dir: Vec<PathBuf>,
        /// Output JSON report
        #[arg(long)]
        json: bool,
    },
    /// Check readiness: every loaded version recorded + required tables present
    Verify {
        /// Migrations directory (repeatable; defaults to `migrations`)
        #[arg(long, default_value = "migrations")]
        dir: Vec<PathBuf>,
        /// Output JSON report
        #[arg(long)]
        json: bool,
    },
    /// Print applied migration versions
    Status,
    /// Print the resolved (redacted) DB URL
    Doctor,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.command {
        Command::Run { dir, json } => {
            let url = resolve_url(cli.database_url.as_deref())?;
            let db = connect_db(&url).await?;
            let start = std::time::Instant::now();
            let applied = run_migrations(&db, dir).await?;
            if json {
                println!(
                    "{}",
                    report_status(
                        applied,
                        &redact_url(&url),
                        u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
                    )
                );
            } else {
                println!("applied {applied} migration(s)");
            }
        }
        Command::Verify { dir, json } => {
            let url = resolve_url(cli.database_url.as_deref())?;
            let db = connect_db(&url).await?;
            let r = verify(&db, dir).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                println!(
                    "loaded={} applied={} pending={} missing_tables={:?}",
                    r.loaded,
                    r.applied,
                    r.pending.len(),
                    r.missing_tables
                );
            }
            anyhow::ensure!(
                r.is_ready(),
                "schema is not ready: {} pending version(s), {} missing table(s)",
                r.pending.len(),
                r.missing_tables.len()
            );
        }
        Command::Status => {
            let url = resolve_url(cli.database_url.as_deref())?;
            let db = connect_db(&url).await?;
            let applied = mc_db::Migrator::list_applied(&db).await?;
            println!("applied migrations: {applied:?}");
        }
        Command::Doctor => {
            let url = resolve_url(cli.database_url.as_deref())?;
            println!("database: {}", redact_url(&url));
        }
    }
    Ok(())
}
