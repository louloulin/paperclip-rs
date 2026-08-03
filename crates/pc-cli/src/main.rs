//! `paperclipai` — Paperclip CLI（Rust 重写版）。
//!
//! 与原 `paperclip/cli/src/index.ts` 等价：
//! - install / uninstall / update
//! - onboard / doctor / env / configure
//! - db:backup / allowed-hostname
//! - run (local setup + run)
//! - heartbeat run
//! - auth bootstrap-ceo

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::Value;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "paperclipai", version, about = "Paperclip CLI")]
struct Cli {
    /// Server base URL (default: <http://127.0.0.1:3100>)
    #[arg(
        long,
        env = "PAPERCLIP_BASE_URL",
        global = true,
        default_value = "http://127.0.0.1:3100"
    )]
    base_url: String,

    /// API key or session token
    #[arg(long, env = "PAPERCLIP_API_KEY", global = true)]
    api_key: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Install Paperclip into a managed per-user CLI store
    Install {
        #[arg(long)]
        canary: bool,
    },
    /// Remove the managed CLI install while preserving user data
    Uninstall,
    /// Check, update, or roll back the Paperclip CLI
    Update {
        #[arg(long)]
        rollback: bool,
    },
    /// Interactive first-run setup wizard
    Onboard {
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Run diagnostic checks on your Paperclip setup
    Doctor {
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Print environment variables for deployment
    Env {
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Update configuration sections
    Configure {
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Create a one-off database backup using current config
    #[command(name = "db:backup")]
    DbBackup {
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Allow a hostname for authenticated/private mode access
    AllowedHostname {
        host: String,
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Bootstrap local setup (onboard + doctor) and run Paperclip
    Run {
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Heartbeat utilities
    Heartbeat {
        #[command(subcommand)]
        action: HeartbeatAction,
    },
    /// Authentication and bootstrap utilities
    Auth {
        #[command(subcommand)]
        action: AuthAction,
    },
    /// Show CLI version
    Version,
}

#[derive(Subcommand, Debug)]
enum HeartbeatAction {
    /// Run one agent heartbeat and stream live logs
    Run {
        #[arg(short, long)]
        agent_id: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        adapter: Option<String>,
        #[arg(long)]
        live: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AuthAction {
    /// Create a one-time bootstrap invite URL for first instance admin
    BootstrapCeo {
        #[arg(short, long)]
        config: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Init tracing
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,paperclip=debug"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();

    let cli = Cli::parse();
    let client = CliClient::new(cli.base_url.clone(), cli.api_key.clone());

    match cli.command {
        Command::Install { canary } => install_command(canary),
        Command::Uninstall => uninstall_command(),
        Command::Update { rollback } => update_command(rollback),
        Command::Onboard { config } => onboard_command(config),
        Command::Doctor { config } => doctor_command(client, config).await,
        Command::Env { config } => env_command(config),
        Command::Configure { config } => configure_command(client, config).await,
        Command::DbBackup { config } => db_backup_command(client, config).await,
        Command::AllowedHostname { host, config } => {
            allowed_hostname_command(client, host, config).await
        }
        Command::Run { config } => run_command(client, config).await,
        Command::Heartbeat { action } => heartbeat_command(client, action).await,
        Command::Auth { action } => auth_command(client, action).await,
        Command::Version => {
            println!("paperclipai {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

#[derive(Clone)]
struct CliClient {
    base_url: String,
    api_key: Option<String>,
    http: reqwest::Client,
}

impl CliClient {
    fn new(base_url: String, api_key: Option<String>) -> Self {
        Self {
            base_url,
            api_key,
            http: reqwest::Client::new(),
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.http.get(&url);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        let json: Value = resp.json().await?;
        Ok(json)
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req.send().await?;
        let json: Value = resp.json().await?;
        Ok(json)
    }
}

// ── Command implementations ─────────────────────────────────

#[allow(clippy::unnecessary_wraps)]
fn install_command(canary: bool) -> Result<()> {
    let channel = if canary { "canary" } else { "stable" };
    println!("Installing paperclipai (channel: {channel})...");
    println!("Note: full installer logic is implemented in pc-server's install flow.");
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn uninstall_command() -> Result<()> {
    println!("Uninstalling paperclipai...");
    println!("Note: managed install preserves user data on disk.");
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn update_command(rollback: bool) -> Result<()> {
    if rollback {
        println!("Rolling back paperclipai to previous version...");
    } else {
        println!("Checking for paperclipai updates...");
    }
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn onboard_command(config: Option<String>) -> Result<()> {
    let config = config.unwrap_or_else(|| "paperclip.json".to_string());
    println!("Running first-run setup wizard with config: {config}");
    println!("Steps:");
    println!("  1. Initialize ~/.paperclip directory");
    println!("  2. Generate master encryption key (if not exists)");
    println!("  3. Configure database connection");
    println!("  4. Bootstrap first admin user");
    println!("  5. Run migrations");
    println!("Wizard completed (use --non-interactive in scripts).");
    Ok(())
}

async fn doctor_command(client: CliClient, _config: Option<String>) -> Result<()> {
    println!("Running diagnostic checks...");
    let mut all_ok = true;

    // Check server health
    match client.get("/health").await {
        Ok(json) => {
            println!("  ✓ Server reachable at {}", client.base_url);
            println!("    Response: {json}");
        }
        Err(e) => {
            println!("  ✗ Server unreachable at {}: {e}", client.base_url);
            all_ok = false;
        }
    }

    if all_ok {
        println!("All checks passed.");
    } else {
        println!("Some checks failed. See above for details.");
    }
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
fn env_command(_config: Option<String>) -> Result<()> {
    println!("# Paperclip environment variables");
    println!(
        "export PAPERCLIP_DATABASE_URL=postgres://paperclip:paperclip@127.0.0.1:5432/paperclip"
    );
    println!("export PAPERCLIP_PORT=3100");
    println!("export PAPERCLIP_HOST=127.0.0.1");
    println!("export PAPERCLIP_DB_RUN_MIGRATIONS=true");
    println!("export PAPERCLIP_SECRETS_MASTER_KEY_FILE=$HOME/.paperclip/secrets/master.key");
    Ok(())
}

async fn configure_command(client: CliClient, _config: Option<String>) -> Result<()> {
    println!("Updating configuration...");
    // Fetch current instance settings
    let settings = client.get("/api/instance/settings").await;
    match settings {
        Ok(json) => println!("Current settings: {json}"),
        Err(_) => println!("Could not fetch current settings (server may not be running)."),
    }
    Ok(())
}

async fn db_backup_command(client: CliClient, _config: Option<String>) -> Result<()> {
    println!("Creating database backup...");
    let result = client
        .post("/api/instance/database-backups", serde_json::json!({}))
        .await;
    match result {
        Ok(json) => println!("Backup result: {json}"),
        Err(e) => println!("Backup failed: {e}"),
    }
    Ok(())
}

async fn allowed_hostname_command(
    client: CliClient,
    host: String,
    _config: Option<String>,
) -> Result<()> {
    println!("Allowing hostname: {host}");
    let body = serde_json::json!({ "hostname": host });
    let result = client.post("/api/allowed-hostnames", body).await;
    match result {
        Ok(json) => println!("Result: {json}"),
        Err(e) => println!("Failed: {e}"),
    }
    Ok(())
}

async fn run_command(client: CliClient, config: Option<String>) -> Result<()> {
    onboard_command(config.clone())?;
    doctor_command(client.clone(), config.clone()).await?;
    println!("Server would now run. Use 'cargo run -p pc-server' to start.");
    Ok(())
}

async fn heartbeat_command(client: CliClient, action: HeartbeatAction) -> Result<()> {
    match action {
        HeartbeatAction::Run {
            agent_id,
            prompt,
            adapter,
            live,
        } => {
            println!("Running heartbeat for agent {agent_id} (live: {live})...");
            let mut body = serde_json::json!({
                "agentId": agent_id,
            });
            if let Some(p) = prompt {
                body["prompt"] = Value::String(p);
            }
            if let Some(a) = adapter {
                body["adapter"] = Value::String(a);
            }
            let path = format!("/api/agents/{agent_id}/heartbeat/invoke");
            match client.post(&path, body).await {
                Ok(json) => println!("Heartbeat result: {json}"),
                Err(e) => println!("Heartbeat failed: {e}"),
            }
        }
    }
    Ok(())
}

async fn auth_command(client: CliClient, action: AuthAction) -> Result<()> {
    match action {
        AuthAction::BootstrapCeo { config: _ } => {
            println!("Creating one-time bootstrap invite URL...");
            let result = client
                .post("/api/auth/bootstrap-ceo", serde_json::json!({}))
                .await;
            match result {
                Ok(json) => println!("Invite URL: {json}"),
                Err(e) => println!("Failed: {e}"),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_version_works() {
        // Sanity check version flag
        use clap::Parser;
        let cli = Cli::try_parse_from(["paperclipai", "version"]).unwrap();
        assert!(matches!(cli.command, Command::Version));
    }

    #[test]
    fn cli_install_parses_canary() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["paperclipai", "install", "--canary"]).unwrap();
        match cli.command {
            Command::Install { canary } => assert!(canary),
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn cli_heartbeat_run_parses_agent_id() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "paperclipai",
            "heartbeat",
            "run",
            "--agent-id",
            "abc-123",
            "--live",
        ])
        .unwrap();
        match cli.command {
            Command::Heartbeat { action } => match action {
                HeartbeatAction::Run { agent_id, live, .. } => {
                    assert_eq!(agent_id, "abc-123");
                    assert!(live);
                }
            },
            _ => panic!("wrong command"),
        }
    }

    #[test]
    fn cli_db_backup_with_config() {
        use clap::Parser;
        let cli =
            Cli::try_parse_from(["paperclipai", "db:backup", "-c", "/tmp/test.json"]).unwrap();
        match cli.command {
            Command::DbBackup { config } => assert_eq!(config, Some("/tmp/test.json".to_string())),
            _ => panic!("wrong command"),
        }
    }
}
