//! `paperclip-migrate` —— Paperclip 数据库迁移独立 CLI。
//!
//! 业务逻辑全部在 `pc_migrate::` lib 中;本文件只负责 clap 解析 + 入口路由。

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use pc_db::Db;
use pc_migrate::{
    cmd_baseline, cmd_create, cmd_down, cmd_seed, cmd_status, cmd_up, cmd_verify, init_tracing,
    resolve_url,
};

#[derive(Parser, Debug)]
#[command(
    name = "paperclip-migrate",
    version,
    about = "Paperclip database migration tool"
)]
struct Cli {
    /// PostgreSQL connection string.
    /// 默认从 `PAPERCLIP_DATABASE_URL` 或 `DATABASE_URL` 读取。
    #[arg(long, env = "PAPERCLIP_DATABASE_URL", global = true)]
    database_url: Option<String>,

    /// 最大连接池大小。
    #[arg(long, default_value_t = 4, global = true)]
    max_connections: u32,

    /// 输出 JSON 格式(用于 CI / 自动化)。
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// 应用所有 pending 迁移。
    Up {
        /// 仅打印计划,不真正执行。
        #[arg(long)]
        dry_run: bool,
    },
    /// 回滚最近一次迁移(需对应 down.sql)。
    Down {
        /// 回滚步数(默认 1)。
        #[arg(long, default_value_t = 1)]
        steps: u32,
    },
    /// 显示迁移状态。
    Status,
    /// 校验 schema:列出关键表是否存在。
    Verify {
        /// 期望存在的关键表名(逗号分隔)。
        #[arg(long, value_delimiter = ',', default_value = "companies,agents,issues,projects,heartbeat_runs,plugin_jobs,tool_invocations,tool_connections")]
        required_tables: Vec<String>,
    },
    /// 把当前 schema 标记为基线(仅插入历史记录,不跑迁移)。
    Baseline {
        /// 基线标签名(写入 __drizzle_migrations 表)。
        #[arg(long, default_value = "external_baseline")]
        label: String,
    },
    /// 创建新迁移文件骨架(命名: YYYYMMDDHHMMSS_<name>.sql)。
    Create {
        /// 迁移名(snake_case)。
        name: String,
        /// 输出目录(默认 ./migrations)。
        #[arg(long, default_value = "./migrations")]
        dir: PathBuf,
    },
    /// 跑 seed SQL(若存在 ./migrations/seed.sql)。
    Seed {
        /// seed 文件路径(默认 ./migrations/seed.sql)。
        #[arg(long, default_value = "./migrations/seed.sql")]
        file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    // `create` doesn't need a DB connection.
    if let Command::Create { name, dir } = &cli.command {
        cmd_create(name, dir, cli.json)?;
        return Ok(());
    }

    let url = resolve_url(cli.database_url.as_deref())?;
    let db = Db::connect(&url, cli.max_connections, 1).await?;

    match cli.command {
        Command::Up { dry_run } => cmd_up(&db, dry_run, cli.json).await,
        Command::Down { steps } => cmd_down(&db, steps, cli.json).await,
        Command::Status => cmd_status(&db, cli.json).await,
        Command::Verify { required_tables } => cmd_verify(&db, &required_tables, cli.json).await,
        Command::Baseline { label } => cmd_baseline(&db, &label, cli.json).await,
        Command::Seed { file } => { cmd_seed(&db, &file, cli.json).await?; Ok(()) }
        Command::Create { .. } => unreachable!("handled above"),
    }
}
