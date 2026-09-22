//! multica CLI: 客户端工具。

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "multica", version, about = "Multica CLI")]
struct Cli {
    /// Base URL of multica-server (default <http://127.0.0.1:3500>)
    #[arg(
        long,
        env = "MULTICA_BASE_URL",
        default_value = "http://127.0.0.1:3500"
    )]
    base_url: String,

    /// API key for authentication (env `MULTICA_API_KEY`)
    #[arg(long, env = "MULTICA_API_KEY")]
    api_key: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show current user
    Whoami,
    /// Open live-events websocket stream
    LiveEvents,
    /// Print server version
    Version,
    /// Health check
    Health,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::builder().build()?;

    let url = format!("{}/api/health", cli.base_url);
    match cli.command {
        Command::Health => {
            let r = client.get(&url).send().await?;
            println!("HTTP {}", r.status());
            let body = r.text().await?;
            println!("{body}");
        }
        Command::Version => {
            println!("multica CLI {}", env!("CARGO_PKG_VERSION"));
            println!("target: {}", cli.base_url);
        }
        Command::Whoami => {
            println!("whoami: not implemented in M0; lands in M1");
        }
        Command::LiveEvents => {
            println!("live-events: not implemented in M0; lands in M1");
        }
    }
    Ok(())
}
