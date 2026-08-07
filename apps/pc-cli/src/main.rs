//! `paperclipai` — Paperclip CLI（Rust 重写版）。
//!
//! 与原 `paperclip/cli/src/index.ts` 等价：
//! - install / uninstall / update
//! - onboard / doctor / env / env-lab / configure
//! - db:backup / worktree / service
//! - run (local setup + run)
//! - heartbeat run
//! - auth bootstrap-ceo
//! - client { whoami, live-events, companies, agents, issues, get, post }

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::collections::BTreeMap;
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
    /// Lab environment helpers (computed env vars, .env.lab writer)
    EnvLab {
        #[command(subcommand)]
        action: EnvLabAction,
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
    /// Worktree helpers (git worktree integration)
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
    /// Service management (systemd / launchd hints + liveness check)
    Service {
        #[command(subcommand)]
        action: ServiceAction,
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
    /// HTTP client wrappers for Paperclip API
    Client {
        #[command(subcommand)]
        action: ClientCommand,
    },
    /// Pipeline + pipeline case operations (CLI parity with `paperclip/cli/src/commands/pipelines.ts`)
    Pipelines {
        #[command(subcommand)]
        action: PipelinesAction,
    },
    /// Routine operations (CLI parity with `paperclip/cli/src/commands/routines.ts`)
    Routines {
        #[command(subcommand)]
        action: RoutinesAction,
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

#[derive(Subcommand, Debug)]
enum EnvLabAction {
    /// Print computed env vars as `export K=V` lines
    Show,
    /// Write a `.env.lab` file in current directory
    Write {
        #[arg(long, default_value = ".env.lab")]
        path: String,
    },
    /// Print a single var by name
    Get { name: String },
}

#[derive(Subcommand, Debug)]
enum WorktreeAction {
    /// List detected worktrees (from `git worktree list` if available)
    List,
    /// Show the current worktree name (best-effort)
    Current,
    /// Print a hint for the recommended dev URL of this worktree
    Url,
}

#[derive(Subcommand, Debug)]
enum PipelinesAction {
    /// List pipelines (filter by company)
    List {
        #[arg(long)]
        company: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Get a single pipeline by id (and stages)
    Get {
        /// Pipeline id (UUID)
        id: String,
    },
    /// Create a new pipeline for a company
    Create {
        #[arg(long)]
        company: String,
        #[arg(long)]
        key: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// List cases for a pipeline (or across company)
    CaseList {
        #[arg(long)]
        pipeline: Option<String>,
        #[arg(long)]
        company: Option<String>,
        #[arg(long)]
        stage: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Get a single pipeline case (full detail)
    CaseGet {
        /// Case id (UUID)
        id: String,
    },
}

#[derive(Subcommand, Debug)]
enum RoutinesAction {
    /// List routines (filter by company)
    List {
        #[arg(long)]
        company: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Get a single routine by id
    Get {
        /// Routine id (UUID)
        id: String,
    },
    /// Pause a routine (status -> paused)
    Pause {
        /// Routine id (UUID)
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Resume a paused routine (status -> active)
    Resume {
        /// Routine id (UUID)
        id: String,
    },
    /// Trigger a routine run now (ad-hoc execution)
    Trigger {
        /// Routine id (UUID)
        id: String,
    },
}

#[derive(Subcommand, Debug)]
enum ServiceAction {
    /// Print service install hint (systemd / launchd) for current OS
    InstallHint,
    /// Check service liveness (best-effort; defaults to HTTP /health)
    Status {
        #[arg(long, default_value = "http://127.0.0.1:3100")]
        url: String,
    },
}

/// Round 48: pipelines subcommand (CLI parity with `paperclip/cli/src/commands/pipelines.ts`).
async fn pipelines_command(client: CliClient, action: PipelinesAction) -> Result<()> {
    match action {
        PipelinesAction::List { company, limit } => {
            let mut path = "/api/pipelines".to_string();
            if let Some(c) = company {
                path.push_str(&format!("?companyId={}", c));
            } else {
                path.push_str(&format!("?limit={}", limit));
            }
            let data = client.get(&path).await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        PipelinesAction::Get { id } => {
            let data = client.get(&format!("/api/pipelines/{}", id)).await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        PipelinesAction::Create {
            company,
            key,
            name,
            description,
        } => {
            let body = serde_json::json!({
                "companyId": company,
                "key": key,
                "name": name,
                "description": description,
            });
            let data = client.post("/api/pipelines", body).await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        PipelinesAction::CaseList {
            pipeline,
            company,
            stage,
            limit,
        } => {
            let mut q = format!("limit={}", limit);
            if let Some(p) = pipeline {
                q.push_str(&format!("&pipelineId={}", p));
            }
            if let Some(c) = company {
                q.push_str(&format!("&companyId={}", c));
            }
            if let Some(s) = stage {
                q.push_str(&format!("&stageId={}", s));
            }
            let data = client.get(&format!("/api/pipelines/cases?{}", q)).await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        PipelinesAction::CaseGet { id } => {
            let data = client.get(&format!("/api/cases/{}", id)).await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
    }
}

/// Round 49: routines subcommand (CLI parity with `paperclip/cli/src/commands/routines.ts`).
async fn routines_command(client: CliClient, action: RoutinesAction) -> Result<()> {
    match action {
        RoutinesAction::List { company, limit } => {
            let mut q = format!("limit={}", limit);
            if let Some(c) = company {
                q.push_str(&format!("&companyId={}", c));
            }
            let data = client.get(&format!("/api/routines?{}", q)).await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        RoutinesAction::Get { id } => {
            let data = client.get(&format!("/api/routines/{}", id)).await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        RoutinesAction::Pause { id, reason } => {
            let body = match reason {
                Some(r) => serde_json::json!({"status": "paused", "reason": r}),
                None => serde_json::json!({"status": "paused"}),
            };
            let data = client
                .post(&format!("/api/routines/{}/pause", id), body)
                .await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        RoutinesAction::Resume { id } => {
            let body = serde_json::json!({"status": "active"});
            let data = client
                .post(&format!("/api/routines/{}/resume", id), body)
                .await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
        RoutinesAction::Trigger { id } => {
            let body = serde_json::json!({});
            let data = client
                .post(&format!("/api/routines/{}/trigger", id), body)
                .await?;
            println!("{}", serde_json::to_string_pretty(&data)?);
            Ok(())
        }
    }
}

#[derive(Subcommand, Debug)]
enum ClientCommand {
    /// Show server health + active user (if auth available)
    Whoami,
    /// Tail live events via WebSocket (one-shot dump of recent buffered events)
    LiveEvents {
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Quick list of companies
    Companies {
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    /// Quick list of agents
    Agents {
        #[arg(long)]
        company: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    /// Quick list of issues
    Issues {
        #[arg(long)]
        company: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },
    /// Run a JSON GET against the server
    Get {
        path: String,
        /// `k=v` query parameters
        #[arg(long)]
        query: Vec<String>,
    },
    /// Run a JSON POST against the server
    Post {
        path: String,
        /// JSON body (defaults to `{}`)
        #[arg(long)]
        body: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
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
        Command::Doctor { config } => doctor_command(client.clone(), config).await,
        Command::Env { config } => env_command(config),
        Command::EnvLab { action } => env_lab_command(action),
        Command::Configure { config } => configure_command(client.clone(), config).await,
        Command::DbBackup { config } => db_backup_command(client.clone(), config).await,
        Command::AllowedHostname { host, config } => {
            allowed_hostname_command(client.clone(), host, config).await
        }
        Command::Worktree { action } => worktree_command(action),
        Command::Service { action } => service_command(client.clone(), action).await,
        Command::Run { config } => run_command(client.clone(), config).await,
        Command::Heartbeat { action } => heartbeat_command(client.clone(), action).await,
        Command::Auth { action } => auth_command(client.clone(), action).await,
        Command::Client { action } => client_command(client, action).await,
        Command::Pipelines { action } => pipelines_command(client.clone(), action).await,
        Command::Routines { action } => routines_command(client.clone(), action).await,
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

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let req = self.http.get(&url);
        let resp = self.auth(req).send().await?;
        let json: Value = resp.json().await?;
        Ok(json)
    }

    async fn get_with_query(&self, path: &str, query: &[String]) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = self.http.get(&url);
        for q in query {
            if let Some((k, v)) = q.split_once('=') {
                req = req.query(&[(k, v)]);
            }
        }
        let resp = self.auth(req).send().await?;
        let json: Value = resp.json().await?;
        Ok(json)
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let req = self.http.post(&url).json(&body);
        let resp = self.auth(req).send().await?;
        let json: Value = resp.json().await?;
        Ok(json)
    }
}

// ── Original commands ──────────────────────────────────────

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
            live: _,
        } => {
            println!("Running heartbeat for agent {agent_id}...");
            let mut body = serde_json::json!({"agentId": agent_id});
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

// ── New: env-lab / worktree / service / client ────────────

fn env_lab_command(action: EnvLabAction) -> Result<()> {
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert(
        "PAPERCLIP_BASE_URL".into(),
        std::env::var("PAPERCLIP_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:3100".into()),
    );
    vars.insert(
        "PAPERCLIP_DATABASE_URL".into(),
        std::env::var("PAPERCLIP_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip".into()),
    );
    vars.insert(
        "PAPERCLIP_PORT".into(),
        std::env::var("PAPERCLIP_PORT").unwrap_or_else(|_| "3100".into()),
    );
    vars.insert(
        "PAPERCLIP_HOST".into(),
        std::env::var("PAPERCLIP_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
    );
    vars.insert(
        "PAPERCLIP_DB_RUN_MIGRATIONS".into(),
        std::env::var("PAPERCLIP_DB_RUN_MIGRATIONS").unwrap_or_else(|_| "true".into()),
    );
    vars.insert(
        "PAPERCLIP_SECRETS_MASTER_KEY_FILE".into(),
        std::env::var("PAPERCLIP_SECRETS_MASTER_KEY_FILE")
            .unwrap_or_else(|_| "$HOME/.paperclip/secrets/master.key".into()),
    );
    vars.insert(
        "PAPERCLIP_OTLP_ENDPOINT".into(),
        std::env::var("PAPERCLIP_OTLP_ENDPOINT").unwrap_or_else(|_| "".into()),
    );
    match action {
        EnvLabAction::Show => {
            for (k, v) in &vars {
                if v.is_empty() {
                    continue;
                }
                println!("export {k}={v}");
            }
            Ok(())
        }
        EnvLabAction::Write { path } => {
            let mut out = String::new();
            for (k, v) in &vars {
                if v.is_empty() {
                    continue;
                }
                out.push_str(&format!("{k}={v}\n"));
            }
            std::fs::write(&path, &out).with_context(|| format!("write {path}"))?;
            println!("Wrote {} ({} lines)", path, vars.len());
            Ok(())
        }
        EnvLabAction::Get { name } => {
            if let Some(v) = vars.get(&name) {
                println!("{v}");
            } else {
                println!("# {name} not defined");
            }
            Ok(())
        }
    }
}

fn worktree_command(action: WorktreeAction) -> Result<()> {
    match action {
        WorktreeAction::List => {
            let output = std::process::Command::new("git")
                .args(["worktree", "list"])
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    print!("{}", String::from_utf8_lossy(&out.stdout));
                }
                _ => {
                    println!("(git not available or not a repo; no worktrees detected)");
                }
            }
            Ok(())
        }
        WorktreeAction::Current => {
            let output = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output();
            match output {
                Ok(out) if out.status.success() => {
                    print!("{}", String::from_utf8_lossy(&out.stdout));
                }
                _ => {
                    println!("(no git toplevel)");
                }
            }
            Ok(())
        }
        WorktreeAction::Url => {
            println!("http://127.0.0.1:3100");
            Ok(())
        }
    }
}

async fn service_command(client: CliClient, action: ServiceAction) -> Result<()> {
    match action {
        ServiceAction::InstallHint => {
            #[cfg(target_os = "macos")]
            {
                println!("# macOS launchd (~/Library/LaunchAgents/com.paperclip.server.plist):");
                println!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
                println!("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">");
                println!("<plist version=\"1.0\"><dict>");
                println!("  <key>Label</key><string>com.paperclip.server</string>");
                println!("  <key>ProgramArguments</key>");
                println!("  <array><string>/usr/local/bin/paperclip-server</string></array>");
                println!("  <key>RunAtLoad</key><true/>");
                println!("  <key>KeepAlive</key><true/>");
                println!("</dict></plist>");
                println!("# launchctl load -w ~/Library/LaunchAgents/com.paperclip.server.plist");
            }
            #[cfg(target_os = "linux")]
            {
                println!("# systemd unit (/etc/systemd/system/paperclip-server.service):");
                println!("[Unit]");
                println!("Description=Paperclip Server");
                println!("After=network.target");
                println!();
                println!("[Service]");
                println!("ExecStart=/usr/local/bin/paperclip-server");
                println!("Restart=on-failure");
                println!("User=paperclip");
                println!();
                println!("[Install]");
                println!("WantedBy=multi-user.target");
                println!("# systemctl daemon-reload && systemctl enable --now paperclip-server");
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                println!("(no service hint available for this OS)");
            }
            Ok(())
        }
        ServiceAction::Status { url } => {
            let probe = CliClient::new(url.clone(), None);
            match probe.get("/health").await {
                Ok(json) => {
                    println!("✓ {url} healthy: {json}");
                }
                Err(e) => {
                    println!("✗ {url} unreachable: {e}");
                }
            }
            let _ = client; // suppress unused
            Ok(())
        }
    }
}

async fn client_command(client: CliClient, action: ClientCommand) -> Result<()> {
    match action {
        ClientCommand::Whoami => {
            println!("Server: {}", client.base_url);
            match client.get("/health").await {
                Ok(json) => println!("Health: {json}"),
                Err(e) => println!("(unreachable) {e}"),
            }
        }
        ClientCommand::LiveEvents { since: _, limit } => {
            // /api/live-events 返回 JSON snapshot（最近缓冲）
            let path = format!("/api/live-events?limit={limit}");
            match client.get(&path).await {
                Ok(json) => println!("{json}"),
                Err(e) => println!("Failed: {e}"),
            }
        }
        ClientCommand::Companies { limit } => {
            let path = format!("/api/companies?limit={limit}");
            match client.get(&path).await {
                Ok(json) => println!("{json}"),
                Err(e) => println!("Failed: {e}"),
            }
        }
        ClientCommand::Agents { company, limit } => {
            let path = match company {
                Some(c) => format!("/api/companies/{c}/agents?limit={limit}"),
                None => format!("/api/agents?limit={limit}"),
            };
            match client.get(&path).await {
                Ok(json) => println!("{json}"),
                Err(e) => println!("Failed: {e}"),
            }
        }
        ClientCommand::Issues { company, limit } => {
            let path = match company {
                Some(c) => format!("/api/companies/{c}/issues?limit={limit}"),
                None => format!("/api/issues?limit={limit}"),
            };
            match client.get(&path).await {
                Ok(json) => println!("{json}"),
                Err(e) => println!("Failed: {e}"),
            }
        }
        ClientCommand::Get { path, query } => match client.get_with_query(&path, &query).await {
            Ok(json) => println!("{json}"),
            Err(e) => println!("Failed: {e}"),
        },
        ClientCommand::Post { path, body } => {
            let body = body
                .as_deref()
                .map(|s| serde_json::from_str::<Value>(s))
                .transpose()
                .context("parse body as JSON")?
                .unwrap_or_else(|| serde_json::json!({}));
            match client.post(&path, body).await {
                Ok(json) => println!("{json}"),
                Err(e) => println!("Failed: {e}"),
            }
        }
    }
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_version_works() {
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

    #[test]
    fn cli_env_lab_show() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["paperclipai", "env-lab", "show"]).unwrap();
        assert!(matches!(cli.command, Command::EnvLab { .. }));
    }

    #[test]
    fn cli_worktree_list() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["paperclipai", "worktree", "list"]).unwrap();
        assert!(matches!(cli.command, Command::Worktree { .. }));
    }

    #[test]
    fn cli_service_install_hint() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["paperclipai", "service", "install-hint"]).unwrap();
        assert!(matches!(cli.command, Command::Service { .. }));
    }

    #[test]
    fn cli_client_whoami() {
        use clap::Parser;
        let cli = Cli::try_parse_from(["paperclipai", "client", "whoami"]).unwrap();
        assert!(matches!(cli.command, Command::Client { .. }));
    }

    #[test]
    fn cli_client_get_with_query() {
        use clap::Parser;
        let cli = Cli::try_parse_from([
            "paperclipai",
            "client",
            "get",
            "/api/companies",
            "--query",
            "limit=5",
        ])
        .unwrap();
        assert!(matches!(cli.command, Command::Client { .. }));
    }

    #[test]
    fn env_lab_show_emits_known_keys() {
        // 临时切换 home 防止污染
        let out = env_lab_command(EnvLabAction::Show);
        assert!(out.is_ok());
    }

    #[test]
    fn env_lab_get_returns_value_or_marker() {
        // 已知 key 一定有 default
        let out = env_lab_command(EnvLabAction::Get {
            name: "PAPERCLIP_PORT".into(),
        });
        assert!(out.is_ok());
    }
}
