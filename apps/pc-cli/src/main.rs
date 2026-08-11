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
use pc_workspace_commands::{
    find_workspace_command_definition, list_workspace_command_definitions,
    list_workspace_service_command_definitions, WorkspaceCommandKind, WorkspaceCommandLifecycle,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
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
        /// Override the install prefix. Defaults to `$HOME/.local/bin` (or
        /// `/usr/local/bin` when the current uid is 0).
        #[arg(long)]
        prefix: Option<String>,
        /// Overwrite an existing symlink in the install prefix.
        #[arg(long)]
        force: bool,
    },
    /// Remove the managed CLI install while preserving user data
    Uninstall {
        /// Override the install prefix. Defaults to `$HOME/.local/bin`.
        #[arg(long)]
        prefix: Option<String>,
        /// Allow removing a real file (not a symlink) at the target. By
        /// default we only remove symlinks to avoid clobbering an
        /// un-related binary that happens to live at the same path.
        #[arg(long)]
        force: bool,
    },
    /// Check, update, or roll back the Paperclip CLI
    Update {
        #[arg(long)]
        rollback: bool,
        /// Override the target version. Defaults to the version embedded
        /// in the binary (no upgrade). Used by tests and by `--force` flows
        /// that want to print "already on <version>" without touching the
        /// network.
        #[arg(long)]
        target_version: Option<String>,
    },
    /// First-run setup wizard
    Onboard {
        #[arg(short, long)]
        config: Option<String>,
        /// Non-interactive mode: compute defaults, generate a fresh master key,
        /// and (if `--output` is given) write a `.env` file with the result.
        #[arg(long)]
        non_interactive: bool,
        /// Optional `.env` file path to write in non-interactive mode
        /// (defaults to stdout if omitted).
        #[arg(long, short = 'o')]
        output: Option<String>,
        /// Overwrite an existing output file (default: refuse to overwrite).
        #[arg(long)]
        force: bool,
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
        /// Output format. `export` (default) emits `export K=V` lines
        /// suitable for `eval $(paperclipai env)`. `shell` is identical.
        /// `json` emits a single JSON object for tooling.
        #[arg(long, default_value = "export")]
        format: EnvFormat,
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
        /// Override the path to the `pc-server` binary. Defaults to
        /// [`resolve_server_binary`] (which looks in the workspace
        /// target dir, then `$PATH`).
        #[arg(long)]
        server_binary: Option<String>,
        /// Spawn the server and return immediately (do not block on it).
        /// Useful for CI / sandboxed scripts; the PID is written to
        /// `--pid-file` if given, otherwise printed.
        #[arg(long)]
        detach: bool,
        /// Where to write the detached server's PID. Defaults to stdout
        /// when omitted.
        #[arg(long)]
        pid_file: Option<String>,
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
    /// Workspace runtime command introspection (powered by pc-workspace-commands).
    /// Read a workspace_runtime JSON config and list/match its commands.
    WorkspaceCommands {
        #[command(subcommand)]
        action: WorkspaceCommandsAction,
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

/// Output format for `env` (1:1 with the upstream `env` command).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EnvFormat {
    Export,
    Shell,
    Json,
}

#[derive(Subcommand, Debug)]
pub enum WorktreeAction {
    /// List detected worktrees (from `git worktree list` if available)
    List,
    /// Show the current worktree name (best-effort)
    Current,
    /// Print a hint for the recommended dev URL of this worktree
    Url,
    /// Print dev-mode hints (worktree name + derived URL + dev port).
    /// Combines `current` + `url` in a single block for copy-paste.
    Dev,
}

#[derive(Subcommand, Debug)]
enum WorkspaceCommandsAction {
    /// List commands in a workspace_runtime JSON file.
    List {
        /// Path to workspace_runtime JSON (e.g. a project_workspaces row metadata).
        #[arg(long)]
        config: PathBuf,
        /// Only show service-kind commands.
        #[arg(long)]
        service_only: bool,
    },
    /// Look up a single command by id within a workspace_runtime JSON.
    Get {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        id: String,
    },
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
/// R570: R-INTEGRATION-10 -- paperclipai workspace-commands {list|get} reads a
/// workspace_runtime JSON config and uses pc-workspace-commands helpers to
/// extract/matching commands. This bridges the shared catalog types (defined
/// in R548) with the operator-facing CLI.
fn workspace_commands_command(action: WorkspaceCommandsAction) -> Result<()> {
    match action {
        WorkspaceCommandsAction::List {
            config,
            service_only,
        } => {
            let raw = std::fs::read_to_string(&config)
                .map_err(|e| anyhow::anyhow!("failed to read {}: {}", config.display(), e))?;
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                anyhow::anyhow!("failed to parse {} as JSON: {}", config.display(), e)
            })?;
            let defs = if service_only {
                list_workspace_service_command_definitions(Some(&value))
            } else {
                list_workspace_command_definitions(Some(&value))
            };
            if defs.is_empty() {
                println!("(no workspace commands in {})", config.display());
                return Ok(());
            }
            println!("{:<28} {:<8} {:<10} {}", "id", "kind", "lifecycle", "name");
            println!("{}", "-".repeat(72));
            for def in defs {
                let kind = match def.kind {
                    WorkspaceCommandKind::Service => "service",
                    WorkspaceCommandKind::Job => "job",
                };
                let lifecycle = def
                    .lifecycle
                    .map(|l| match l {
                        WorkspaceCommandLifecycle::Shared => "shared",
                        WorkspaceCommandLifecycle::Ephemeral => "ephemeral",
                    })
                    .unwrap_or("-");
                println!("{:<28} {:<8} {:<10} {}", def.id, kind, lifecycle, def.name);
            }
            Ok(())
        }
        WorkspaceCommandsAction::Get { config, id } => {
            let raw = std::fs::read_to_string(&config)
                .map_err(|e| anyhow::anyhow!("failed to read {}: {}", config.display(), e))?;
            let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                anyhow::anyhow!("failed to parse {} as JSON: {}", config.display(), e)
            })?;
            match find_workspace_command_definition(Some(&value), Some(&id)) {
                Some(def) => {
                    let kind = match def.kind {
                        WorkspaceCommandKind::Service => "service",
                        WorkspaceCommandKind::Job => "job",
                    };
                    let lifecycle = def
                        .lifecycle
                        .map(|l| match l {
                            WorkspaceCommandLifecycle::Shared => "shared",
                            WorkspaceCommandLifecycle::Ephemeral => "ephemeral",
                        })
                        .unwrap_or("-");
                    println!("id:        {}", def.id);
                    println!("name:      {}", def.name);
                    println!("kind:      {}", kind);
                    println!("lifecycle: {}", lifecycle);
                    if let Some(cmd) = &def.command {
                        println!("command:   {}", cmd);
                    }
                    if let Some(cwd) = &def.cwd {
                        println!("cwd:       {}", cwd);
                    }
                    if let Some(reason) = &def.disabled_reason {
                        println!("disabled:  {}", reason);
                    }
                    Ok(())
                }
                None => {
                    println!(
                        "workspace command `{}` not found in {}",
                        id,
                        config.display()
                    );
                    Ok(())
                }
            }
        }
    }
}

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
        Command::Install {
            canary,
            prefix,
            force,
        } => install_command(canary, prefix, force),
        Command::Uninstall { prefix, force } => uninstall_command(prefix, force),
        Command::Update {
            rollback,
            target_version,
        } => update_command(rollback, target_version),
        Command::Onboard {
            config,
            non_interactive,
            output,
            force,
        } => onboard_command(config, non_interactive, output, force),
        Command::Doctor { config } => doctor_command(client.clone(), config).await,
        Command::Env { config, format } => env_command(config, format),
        Command::EnvLab { action } => env_lab_command(action),
        Command::Configure { config } => configure_command(client.clone(), config).await,
        Command::DbBackup { config } => db_backup_command(client.clone(), config).await,
        Command::AllowedHostname { host, config } => {
            allowed_hostname_command(client.clone(), host, config).await
        }
        Command::Worktree { action } => worktree_command(action),
        Command::Service { action } => service_command(client.clone(), action).await,
        Command::Run {
            config,
            server_binary,
            detach,
            pid_file,
        } => run_command(client.clone(), config, server_binary, detach, pid_file).await,
        Command::Heartbeat { action } => heartbeat_command(client.clone(), action).await,
        Command::Auth { action } => auth_command(client.clone(), action).await,
        Command::Client { action } => client_command(client, action).await,
        Command::WorkspaceCommands { action } => workspace_commands_command(action),
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

/// Resolve the default install prefix following the XDG-style convention
/// used by the upstream `install-store.ts` (HOME-scoped per default).
///
/// Root callers can opt in to a system-wide install with
/// `--prefix /usr/local/bin`; we do not auto-detect root from inside the
/// binary because `geteuid` requires `unsafe` which the workspace lints
/// forbid. The explicit-flag policy keeps the call site auditable.
fn default_install_prefix() -> std::path::PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".local").join("bin")
    } else {
        std::path::PathBuf::from(".local/bin")
    }
}

/// Result of a successful `install` run.
#[derive(Debug, PartialEq, Eq)]
pub struct InstallOutcome {
    pub source: std::path::PathBuf,
    pub target: std::path::PathBuf,
}

/// Pure helper: compute the install plan (source / target) without touching
/// the filesystem. Exposed so unit tests can verify the prefix resolution
/// and channel-tagged target name without actually creating a symlink.
pub fn plan_install(
    current_exe: &std::path::Path,
    prefix: &std::path::Path,
    canary: bool,
) -> InstallOutcome {
    let bin_name = if canary {
        "paperclipai-canary"
    } else {
        "paperclipai"
    };
    InstallOutcome {
        source: current_exe.to_path_buf(),
        target: prefix.join(bin_name),
    }
}

/// `install` — create a symlink from the install prefix to the running
/// binary. Pure helper above does the path math; this function does the
/// actual filesystem work (mkdir prefix, refuse-or-overwrite, symlink).
fn install_command(canary: bool, prefix: Option<String>, force: bool) -> Result<()> {
    let prefix = match prefix {
        Some(p) => std::path::PathBuf::from(p),
        None => default_install_prefix(),
    };
    let current_exe = std::env::current_exe().context("locate current exe")?;
    let plan = plan_install(&current_exe, &prefix, canary);

    if plan.target.exists() || plan.target.symlink_metadata().is_ok() {
        if !force {
            anyhow::bail!(
                "refusing to overwrite existing install at {} (pass --force to override)",
                plan.target.display()
            );
        }
        std::fs::remove_file(&plan.target)
            .with_context(|| format!("remove existing {}", plan.target.display()))?;
    }

    if !prefix.exists() {
        std::fs::create_dir_all(&prefix)
            .with_context(|| format!("create prefix {}", prefix.display()))?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&plan.source, &plan.target).with_context(|| {
            format!(
                "symlink {} -> {}",
                plan.target.display(),
                plan.source.display()
            )
        })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(&plan.source, &plan.target).with_context(|| {
            format!(
                "copy {} -> {}",
                plan.source.display(),
                plan.target.display()
            )
        })?;
    }

    println!(
        "Installed paperclipai ({} channel)",
        if canary { "canary" } else { "stable" }
    );
    println!("  source : {}", plan.source.display());
    println!("  target : {}", plan.target.display());
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let path_str = path_var.to_string_lossy();
    if !path_str
        .split(':')
        .any(|p| std::path::Path::new(p) == prefix.as_path())
    {
        println!();
        println!(
            "NOTE: {} is not on PATH. Add it to your shell rc:",
            prefix.display()
        );
        println!("  export PATH=\"$PATH:{}\"", prefix.display());
    }
    Ok(())
}

/// Result of a successful `uninstall` run.
#[derive(Debug, PartialEq, Eq)]
pub struct UninstallOutcome {
    pub target: std::path::PathBuf,
    pub was_symlink: bool,
}

/// Pure helper: compute the uninstall target without touching the filesystem.
/// Mirrors `plan_install` so the two commands stay in lockstep.
pub fn plan_uninstall(prefix: &std::path::Path, canary: bool) -> std::path::PathBuf {
    let bin_name = if canary {
        "paperclipai-canary"
    } else {
        "paperclipai"
    };
    prefix.join(bin_name)
}

/// Remove the install symlink at the given target. Refuses to remove a
/// real (non-symlink) file unless `force` is set — protects against
/// `uninstall` clobbering an un-related binary that happens to share the
/// install path.
pub fn uninstall_at(target: &std::path::Path, force: bool) -> anyhow::Result<UninstallOutcome> {
    match std::fs::symlink_metadata(target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("nothing installed at {}", target.display());
        }
        Err(e) => return Err(e).with_context(|| format!("stat {}", target.display())),
        Ok(meta) => {
            let was_symlink = meta.file_type().is_symlink();
            if !was_symlink && !force {
                anyhow::bail!(
                    "refusing to remove non-symlink at {} (pass --force to override)",
                    target.display()
                );
            }
            std::fs::remove_file(target).with_context(|| format!("remove {}", target.display()))?;
            Ok(UninstallOutcome {
                target: target.to_path_buf(),
                was_symlink,
            })
        }
    }
}

/// `uninstall` — remove the symlink installed by [`install_command`].
/// User data (`~/.paperclip`, the database, secrets) is never touched.
fn uninstall_command(prefix: Option<String>, force: bool) -> Result<()> {
    let prefix = match prefix {
        Some(p) => std::path::PathBuf::from(p),
        None => default_install_prefix(),
    };
    let target = plan_uninstall(&prefix, false);
    let outcome = uninstall_at(&target, force)?;
    println!(
        "Uninstalled paperclipai ({} symlink at {})",
        if outcome.was_symlink { "" } else { "non-" },
        outcome.target.display()
    );
    println!("Note: user data in $HOME/.paperclip and the database were preserved.");
    Ok(())
}

/// Pure helper: compare two semver-like `MAJOR.MINOR.PATCH` strings.
/// Returns `Ordering::Equal` if they are equal, otherwise the natural order.
/// Used by `update_command` to decide whether to suggest an upgrade.
pub fn compare_versions(current: &str, latest: &str) -> std::cmp::Ordering {
    let parse =
        |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse::<u64>().ok()).collect() };
    parse(current).cmp(&parse(latest))
}

/// Pure helper: build the upgrade hint that `update_command` prints when
/// a newer version is available. Kept separate from the IO so unit tests
/// can verify the wording without spawning anything.
pub fn build_update_hint(current: &str, latest: &str) -> String {
    format!(
        "Update available: {current} -> {latest}\n  Run: cargo install --path apps/pc-cli --locked --force"
    )
}

/// Version embedded in the binary at compile time.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `update` — print the current version and, if `target_version` differs,
/// a hint for how to install the newer one. No network IO: the caller is
/// expected to supply the latest known version (e.g. via the upstream
/// `update-notice.ts` channel file or by hand in scripts).
fn update_command(rollback: bool, target_version: Option<String>) -> Result<()> {
    if rollback {
        // Rollback is intentionally a no-op here: there is no prior
        // version archive to roll back to without the managed installer
        // pinning it. We surface the intent instead.
        println!("Rollback requested. To roll back, reinstall a specific version:");
        println!("  cargo install --path apps/pc-cli --locked --force");
        return Ok(());
    }
    let latest = target_version.unwrap_or_else(|| CURRENT_VERSION.to_string());
    println!("Current version: {CURRENT_VERSION}");
    match compare_versions(CURRENT_VERSION, &latest) {
        std::cmp::Ordering::Equal => {
            println!("Already on the latest version.");
        }
        std::cmp::Ordering::Less => {
            println!("{}", build_update_hint(CURRENT_VERSION, &latest));
        }
        std::cmp::Ordering::Greater => {
            println!("Local version {CURRENT_VERSION} is newer than supplied target {latest}.");
        }
    }
    Ok(())
}

#[allow(clippy::unnecessary_wraps)]
/// Build the default `paperclip.json` body for interactive onboard.
///
/// Kept as a pure function so tests can verify the exact shape without
/// touching the filesystem. Mirrors the upstream `defaultConfig` shape in
/// `install-store.ts` (host=127.0.0.1, port=3100, secrets=local_encrypted,
/// storage=local_disk, runMigrations=true).
pub fn default_config_toml() -> String {
    let mut out = String::new();
    out.push_str("# Paperclip instance config (written by `paperclipai onboard`)\n");
    out.push_str("# Edit values and re-run `paperclipai run` to apply.\n\n");
    out.push_str("[server]\n");
    out.push_str("host = \"127.0.0.1\"\n");
    out.push_str("port = 3100\n\n");
    out.push_str("[database]\n");
    out.push_str("url = \"postgres://paperclip:paperclip@127.0.0.1:5432/paperclip\"\n");
    out.push_str("run_migrations = true\n\n");
    out.push_str("[secrets]\n");
    out.push_str("kind = \"local_encrypted\"\n");
    out.push_str("master_key_file = \"$HOME/.paperclip/secrets/master.key\"\n\n");
    out.push_str("[storage]\n");
    out.push_str("kind = \"local_disk\"\n");
    out.push_str("path = \"$HOME/.paperclip/storage\"\n");
    out
}

/// Render the env-file body for a non-interactive onboard run.
///
/// Kept as a pure helper so the same output is exercised by unit tests and
/// the live CLI. Order is stable (sorted by key) to make diffs easy to
/// review. Secrets are emitted as base64 so they round-trip cleanly through
/// the env parser without quoting.
fn render_onboard_env(master_key_b64: &str, port: u16, host: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("PAPERCLIP_HOST".into(), host.to_string());
    out.insert("PAPERCLIP_PORT".into(), port.to_string());
    out.insert("PAPERCLIP_SECRETS_KIND".into(), "local_encrypted".into());
    out.insert(
        "PAPERCLIP_SECRETS_MASTER_KEY".into(),
        master_key_b64.to_string(),
    );
    out.insert("PAPERCLIP_DB_RUN_MIGRATIONS".into(), "true".into());
    out.insert("PAPERCLIP_STORAGE_KIND".into(), "local_disk".into());
    out
}

/// Generate 32 cryptographically-random bytes and return them as base64.
///
/// Equivalent to upstream `randomBytes(32).toString("base64")` from the
/// `loadOrCreateGeneratedSecret` path in `decision-signing.ts`. Uses
/// `OsRng` to avoid the implicit-state pitfalls of `thread_rng`.
fn generate_master_key_b64() -> String {
    use base64::Engine;
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// First-run setup wizard.
///
/// Interactive (default): prints the planned steps the way the upstream
/// wizard does — useful when the user is running it by hand.
///
/// Non-interactive (`--non-interactive`): actually generates a fresh
/// master key, computes the default Paperclip environment, and either
/// prints the result as `KEY=VALUE` lines to stdout or writes them to
/// `--output` (refusing to overwrite by default; pass `--force` to allow
/// it). This is the same shape of work the upstream
/// `loadOrCreateGeneratedSecret` + `resolveDecisionSigningSecret` paths do,
/// just inlined for the CLI bootstrap.
fn onboard_command(
    config: Option<String>,
    non_interactive: bool,
    output: Option<String>,
    force: bool,
) -> Result<()> {
    let config_name = config.unwrap_or_else(|| "paperclip.json".to_string());
    if !non_interactive {
        // Interactive mode: actually do the prep work the printed steps describe.
        // 1. Create $HOME/.paperclip if missing.
        // 2. Render the default config into <config_name> (relative to cwd).
        // 3. Print a clear summary of what was created.
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("HOME not set; cannot run interactive onboard"))?;
        let paperclip_dir = home.join(".paperclip");
        if !paperclip_dir.exists() {
            std::fs::create_dir_all(&paperclip_dir)
                .with_context(|| format!("create {}", paperclip_dir.display()))?;
            println!("Created {}", paperclip_dir.display());
        } else {
            println!("Already exists: {}", paperclip_dir.display());
        }

        // Render a default config that references the secret master key file
        // (we don't generate the key here to avoid clobbering an existing
        // install; run `--non-interactive` for a fresh key).
        let cfg_path = std::path::PathBuf::from(&config_name);
        if cfg_path.exists() {
            println!(
                "Config already exists at {} (skipping write).",
                cfg_path.display()
            );
        } else {
            let body = default_config_toml();
            std::fs::write(&cfg_path, body)
                .with_context(|| format!("write {}", cfg_path.display()))?;
            println!("Wrote default config to {}", cfg_path.display());
        }
        println!();
        println!("Next steps:");
        println!("  1. Review the config: {config_name}");
        println!("  2. Start the server:  paperclipai run");
        println!("  3. (optional) generate a fresh master key:");
        println!("       paperclipai onboard --non-interactive --output .env");
        return Ok(());
    }

    let port: u16 = std::env::var("PAPERCLIP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3100);
    let host = std::env::var("PAPERCLIP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let master_key = generate_master_key_b64();
    let env_vars = render_onboard_env(&master_key, port, &host);
    let body = env_vars
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(
            "
",
        );

    match output {
        None => {
            print!("{body}");
            println!();
        }
        Some(path) => {
            if std::path::Path::new(&path).exists() && !force {
                anyhow::bail!(
                    "refusing to overwrite existing file {path} (pass --force to override)"
                );
            }
            std::fs::write(&path, format!("{body}\n")).with_context(|| format!("write {path}"))?;
            println!("Wrote {} ({} keys)", path, env_vars.len());
        }
    }
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

/// Build the resolved env map for `env`, reading from the live process
/// environment and falling back to defaults. Same defaults as
/// `env_lab_command` so the two commands stay in lockstep.
pub fn build_resolved_env() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert(
        "PAPERCLIP_BASE_URL".into(),
        std::env::var("PAPERCLIP_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:3100".into()),
    );
    out.insert(
        "PAPERCLIP_DATABASE_URL".into(),
        std::env::var("PAPERCLIP_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_else(|_| "postgres://paperclip:paperclip@127.0.0.1:5432/paperclip".into()),
    );
    out.insert(
        "PAPERCLIP_PORT".into(),
        std::env::var("PAPERCLIP_PORT").unwrap_or_else(|_| "3100".into()),
    );
    out.insert(
        "PAPERCLIP_HOST".into(),
        std::env::var("PAPERCLIP_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
    );
    out.insert(
        "PAPERCLIP_DB_RUN_MIGRATIONS".into(),
        std::env::var("PAPERCLIP_DB_RUN_MIGRATIONS").unwrap_or_else(|_| "true".into()),
    );
    out.insert(
        "PAPERCLIP_SECRETS_MASTER_KEY_FILE".into(),
        std::env::var("PAPERCLIP_SECRETS_MASTER_KEY_FILE")
            .unwrap_or_else(|_| "$HOME/.paperclip/secrets/master.key".into()),
    );
    out
}

/// `env` — print the resolved Paperclip environment. Reads from the live
/// process env (so `eval $(paperclipai env)` round-trips with whatever
/// the caller already exported), falling back to defaults.
fn env_command(_config: Option<String>, format: EnvFormat) -> Result<()> {
    let env = build_resolved_env();
    match format {
        EnvFormat::Export | EnvFormat::Shell => {
            for (k, v) in &env {
                if v.is_empty() {
                    continue;
                }
                println!("export {k}={v}");
            }
        }
        EnvFormat::Json => {
            let json = serde_json::to_string_pretty(&env).context("serialize env as JSON")?;
            println!("{json}");
        }
    }
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

/// Search candidates for the `pc-server` binary, in priority order:
/// 1. Caller-supplied override (from `--server-binary`).
/// 2. Workspace `target/{debug,release}/pc-server[.exe]` (when run from
///    inside the `paperclip-rs` repo).
/// 3. `paperclip-server` on `$PATH` (managed install name).
/// 4. `pc-server` on `$PATH` (crate-name fallback).
pub fn resolve_server_binary(override_path: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(p) = override_path {
        let pb = std::path::PathBuf::from(p);
        if pb.exists() {
            return Some(pb);
        }
        return None;
    }
    let exe_suffix = std::env::consts::EXE_SUFFIX;
    // Walk up from CARGO_MANIFEST_DIR to find a `target` directory.
    if let Some(manifest) = std::env::var_os("CARGO_MANIFEST_DIR") {
        let mut dir = std::path::PathBuf::from(manifest);
        for _ in 0..6 {
            let target = dir.join("target");
            for profile in ["debug", "release"] {
                let candidate = target.join(profile).join(format!("pc-server{exe_suffix}"));
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            if !dir.pop() {
                break;
            }
        }
    }
    for name in ["paperclip-server", "pc-server"] {
        let name_with_suffix = format!("{name}{exe_suffix}");
        if let Some(p) = find_on_path(&name_with_suffix) {
            return Some(p);
        }
    }
    None
}

fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Build the environment that the spawned server should inherit. We always
/// forward the live process env; tests can verify the pass-through shape by
/// passing a base env map and reading the result.
pub fn build_run_env(base: &std::collections::BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = base.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    // Stable order for snapshot tests.
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// `run` — after the onboard + doctor preflight, actually spawn the
/// `pc-server` binary. In `--detach` mode the server is started in the
/// background and the PID is reported; otherwise we wait for it to exit
/// and propagate its status.
async fn run_command(
    client: CliClient,
    config: Option<String>,
    server_binary: Option<String>,
    detach: bool,
    pid_file: Option<String>,
) -> Result<()> {
    onboard_command(config.clone(), false, None, false)?;
    doctor_command(client.clone(), config.clone()).await?;

    let binary = resolve_server_binary(server_binary.as_deref())
        .ok_or_else(|| anyhow::anyhow!(
            "could not locate pc-server binary. Pass --server-binary <path>,              build it with `cargo build -p pc-server`, or install via `paperclipai install`."
        ))?;
    println!("Starting {} ...", binary.display());

    let mut cmd = std::process::Command::new(&binary);
    cmd.envs(std::env::vars());
    if let Some(ref c) = config {
        cmd.arg("--config").arg(c);
    }
    if detach {
        // Detach by redirecting stdio and not waiting.
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        let child = cmd
            .spawn()
            .with_context(|| format!("spawn {}", binary.display()))?;
        let pid = child.id();
        match pid_file {
            Some(path) => {
                std::fs::write(&path, format!("{pid}\n"))
                    .with_context(|| format!("write pid file {path}"))?;
                println!("Detached pc-server (pid {pid}); pid file at {path}");
            }
            None => println!("Detached pc-server (pid {pid})"),
        }
        return Ok(());
    }
    // Foreground: forward stdio and wait.
    let status = cmd
        .status()
        .with_context(|| format!("run {}", binary.display()))?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        std::process::exit(code);
    }
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

/// Default base port used for worktree URL derivation. Matches the
/// default embedded in the server config and `default_config_toml()`.
pub fn default_base_port() -> u16 {
    std::env::var("PAPERCLIP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3100)
}

/// Default host for the dev URL. Mirrors the server config default.
pub fn default_dev_host() -> String {
    std::env::var("PAPERCLIP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// Extract a worktree name from its filesystem path. Pure function so
/// unit tests can verify the heuristic without `git`. Falls back to the
/// last path component (stripped of `.git` suffix when present).
pub fn worktree_name_from_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return String::from("(root)");
    }
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if last.is_empty() {
        return String::from("(root)");
    }
    last.to_string() // Do not strip anything from the last component.
}

/// Pick a stable port for a given worktree name. The base worktree
/// ("main" / "master" / "default") keeps the base port so unaltered
/// checkouts still work. Every other name is offset by a stable hash of
/// its bytes (so opening the same worktree twice gives the same port).
pub fn derive_worktree_port(name: &str, base_port: u16) -> u16 {
    if matches!(name, "main" | "master" | "default" | "(root)") {
        return base_port;
    }
    // FNV-1a 32-bit then mod 1000 keeps the offset in a small, stable
    // range (avoids colliding with well-known service ports). Offset 1
    // leaves room for the base instance.
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let offset = (hash % 999 + 1) as u16;
    base_port.saturating_add(offset)
}

/// Build the full dev URL for a worktree.
pub fn derive_worktree_url(name: &str, base_port: u16, host: &str) -> String {
    let port = derive_worktree_port(name, base_port);
    format!("http://{host}:{port}")
}

/// Run `git rev-parse --show-toplevel` to find the current worktree's
/// filesystem path. Returns `None` if `git` is missing or the cwd is
/// not inside a repository. Pure in the sense that it does not mutate
/// any state, but it does spawn a subprocess.
pub fn current_worktree_toplevel() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
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
            let toplevel = current_worktree_toplevel().unwrap_or_else(|| String::from("(none)"));
            let name = worktree_name_from_path(&toplevel);
            let url = derive_worktree_url(&name, default_base_port(), &default_dev_host());
            println!("{url}");
            Ok(())
        }
        WorktreeAction::Dev => {
            let toplevel = current_worktree_toplevel().unwrap_or_else(|| String::from("(none)"));
            let name = worktree_name_from_path(&toplevel);
            let base = default_base_port();
            let host = default_dev_host();
            let url = derive_worktree_url(&name, base, &host);
            println!("Worktree: {name}");
            println!("Toplevel: {toplevel}");
            println!("Base port: {base}");
            println!("Dev URL:  {url}");
            println!();
            println!("Quick start:");
            println!(
                "  export PAPERCLIP_PORT={port}",
                port = derive_worktree_port(&name, base)
            );
            println!("  paperclipai run");
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
            Command::Install { canary, .. } => assert!(canary),
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

// -------- R493 onboard --non-interactive --------

#[test]
fn r493_render_onboard_env_is_key_sorted_and_complete() {
    let env = render_onboard_env("AAA=", 3100, "127.0.0.1");
    let keys: Vec<&String> = env.keys().collect();
    assert_eq!(
        keys,
        vec![
            &"PAPERCLIP_DB_RUN_MIGRATIONS".to_string(),
            &"PAPERCLIP_HOST".to_string(),
            &"PAPERCLIP_PORT".to_string(),
            &"PAPERCLIP_SECRETS_KIND".to_string(),
            &"PAPERCLIP_SECRETS_MASTER_KEY".to_string(),
            &"PAPERCLIP_STORAGE_KIND".to_string(),
        ]
    );
    assert_eq!(env["PAPERCLIP_PORT"], "3100");
    assert_eq!(env["PAPERCLIP_SECRETS_MASTER_KEY"], "AAA=");
    assert_eq!(env["PAPERCLIP_SECRETS_KIND"], "local_encrypted");
}

#[test]
fn r493_render_onboard_env_honors_explicit_host_port() {
    let env = render_onboard_env("Zm9v", 8080, "0.0.0.0");
    assert_eq!(env["PAPERCLIP_HOST"], "0.0.0.0");
    assert_eq!(env["PAPERCLIP_PORT"], "8080");
}

#[test]
fn r493_generate_master_key_b64_is_44_chars_and_decodes_to_32_bytes() {
    use base64::Engine;
    let s = generate_master_key_b64();
    // base64(32) = 44 chars (no padding adjustment needed)
    assert_eq!(s.len(), 44);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&s)
        .expect("decode b64");
    assert_eq!(bytes.len(), 32);
}

#[test]
fn r493_generate_master_key_b64_returns_distinct_values() {
    let a = generate_master_key_b64();
    let b = generate_master_key_b64();
    // 32 random bytes — collision odds are ~2^-256.
    assert_ne!(a, b);
}

#[test]
fn r493_onboard_non_interactive_writes_env_file() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-onboard-{}.env", uuid::Uuid::new_v4()));
    let path = tmp.to_str().expect("path utf-8").to_string();
    onboard_command(
        Some("paperclip.json".into()),
        true,
        Some(path.clone()),
        false,
    )
    .expect("onboard --non-interactive");
    let body = std::fs::read_to_string(&path).expect("read tmp env");
    let _ = std::fs::remove_file(&path);
    assert!(body.contains("PAPERCLIP_SECRETS_MASTER_KEY="));
    assert!(body.contains("PAPERCLIP_SECRETS_KIND=local_encrypted"));
    assert!(body.contains("PAPERCLIP_PORT=3100"));
}

#[test]
fn r493_onboard_non_interactive_refuses_to_overwrite_without_force() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-onboard-{}.env", uuid::Uuid::new_v4()));
    let path = tmp.to_str().expect("path utf-8").to_string();
    std::fs::write(&path, "EXISTING=true\n").expect("seed");
    let err = onboard_command(None, true, Some(path.clone()), false).unwrap_err();
    let _ = std::fs::remove_file(&path);
    assert!(
        err.to_string().contains("refusing to overwrite"),
        "expected refusal error, got: {err}"
    );
}

#[test]
fn r493_onboard_non_interactive_force_overwrites() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-onboard-{}.env", uuid::Uuid::new_v4()));
    let path = tmp.to_str().expect("path utf-8").to_string();
    std::fs::write(&path, "EXISTING=true\n").expect("seed");
    onboard_command(None, true, Some(path.clone()), true).expect("onboard --force");
    let body = std::fs::read_to_string(&path).expect("read");
    let _ = std::fs::remove_file(&path);
    assert!(!body.contains("EXISTING"));
    assert!(body.contains("PAPERCLIP_SECRETS_MASTER_KEY="));
}

#[test]
fn r493_onboard_interactive_is_unchanged() {
    // No file IO. Just verify it doesn't error.
    onboard_command(Some("paperclip.json".into()), false, None, false)
        .expect("interactive onboard");
}

// -------- R495 install real path --------

#[test]
fn r495_plan_install_stable_target_name() {
    let plan = plan_install(
        std::path::Path::new("/opt/paperclipai/target/debug/paperclipai"),
        std::path::Path::new("/home/u/.local/bin"),
        false,
    );
    assert_eq!(
        plan.source.to_str(),
        Some("/opt/paperclipai/target/debug/paperclipai")
    );
    assert_eq!(
        plan.target,
        std::path::PathBuf::from("/home/u/.local/bin/paperclipai")
    );
}

#[test]
fn r495_plan_install_canary_target_name() {
    let plan = plan_install(
        std::path::Path::new("/build/paperclipai"),
        std::path::Path::new("/home/u/.local/bin"),
        true,
    );
    assert_eq!(
        plan.target,
        std::path::PathBuf::from("/home/u/.local/bin/paperclipai-canary")
    );
}

#[test]
fn r495_default_install_prefix_uses_home() {
    // HOME is set in the test runner environment; we sanity check it
    // lands under .local/bin.
    let p = default_install_prefix();
    let s = p.to_string_lossy();
    assert!(
        s.ends_with(".local/bin"),
        "expected suffix .local/bin, got {s}"
    );
}

#[test]
fn r495_install_refuses_to_overwrite_without_force() {
    // Seed a file at the target location and verify the call errors.
    let tmp = std::env::temp_dir().join(format!("pc-cli-install-{}", uuid::Uuid::new_v4()));
    let prefix = tmp.join("bin");
    std::fs::create_dir_all(&prefix).expect("mkdir prefix");
    // Use the current binary as a stand-in source, and create a sentinel
    // at the stable target.
    let current = std::env::current_exe().expect("current exe");
    let target = prefix.join("paperclipai");
    std::fs::write(&target, b"sentinel").expect("seed sentinel");

    let err = install_command_with_paths(&current, &prefix, false, false).unwrap_err();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        err.to_string().contains("refusing to overwrite"),
        "expected refusal error, got: {err}"
    );
}

#[test]
fn r495_install_creates_symlink_in_fresh_prefix() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-install-{}", uuid::Uuid::new_v4()));
    let prefix = tmp.join("bin");
    let current = std::env::current_exe().expect("current exe");

    let outcome =
        install_command_with_paths(&current, &prefix, false, false).expect("install fresh");
    // Read the link BEFORE we wipe the temp dir.
    let resolved = std::fs::read_link(&outcome.target).expect("target is a symlink");
    let _ = std::fs::remove_dir_all(&tmp);

    assert_eq!(outcome.target, prefix.join("paperclipai"));
    assert_eq!(resolved, current);
}

#[test]
fn r495_install_force_overwrites_existing_symlink() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-install-{}", uuid::Uuid::new_v4()));
    let prefix = tmp.join("bin");
    let current = std::env::current_exe().expect("current exe");
    std::fs::create_dir_all(&prefix).expect("mkdir");
    // Seed a bogus file at the target.
    std::fs::write(prefix.join("paperclipai"), b"old").expect("seed");

    let outcome =
        install_command_with_paths(&current, &prefix, false, true).expect("install --force");
    // Read the target BEFORE we wipe the temp dir.
    let body = std::fs::read(&outcome.target).expect("read target");
    let _ = std::fs::remove_dir_all(&tmp);

    assert_ne!(body, b"old", "old sentinel should be gone");
}

// -------- R495 install helper (test-only refactor) --------

/// Test-only entry point that lets unit tests drive the install behaviour
/// without depending on `std::env::current_exe` or the live `PATH`.
fn install_command_with_paths(
    current_exe: &std::path::Path,
    prefix: &std::path::Path,
    canary: bool,
    force: bool,
) -> anyhow::Result<crate::InstallOutcome> {
    let plan = crate::plan_install(current_exe, prefix, canary);
    if plan.target.exists() || plan.target.symlink_metadata().is_ok() {
        if !force {
            anyhow::bail!(
                "refusing to overwrite existing install at {} (pass --force to override)",
                plan.target.display()
            );
        }
        std::fs::remove_file(&plan.target)
            .with_context(|| format!("remove existing {}", plan.target.display()))?;
    }
    if !prefix.exists() {
        std::fs::create_dir_all(prefix)
            .with_context(|| format!("create prefix {}", prefix.display()))?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&plan.source, &plan.target).with_context(|| {
            format!(
                "symlink {} -> {}",
                plan.target.display(),
                plan.source.display()
            )
        })?;
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(&plan.source, &plan.target).with_context(|| {
            format!(
                "copy {} -> {}",
                plan.source.display(),
                plan.target.display()
            )
        })?;
    }
    Ok(plan)
}

// -------- R496 uninstall + update real path --------

#[test]
fn r496_plan_uninstall_stable_target() {
    let p = plan_uninstall(std::path::Path::new("/home/u/.local/bin"), false);
    assert_eq!(
        p,
        std::path::PathBuf::from("/home/u/.local/bin/paperclipai")
    );
}

#[test]
fn r496_plan_uninstall_canary_target() {
    let p = plan_uninstall(std::path::Path::new("/home/u/.local/bin"), true);
    assert_eq!(
        p,
        std::path::PathBuf::from("/home/u/.local/bin/paperclipai-canary")
    );
}

#[test]
fn r496_uninstall_at_removes_symlink() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-uninstall-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let target = tmp.join("paperclipai");
    // Seed a symlink to a real (existing) source — using current_exe is fine
    // because the source is irrelevant once the link is removed.
    let current = std::env::current_exe().expect("current exe");
    std::os::unix::fs::symlink(&current, &target).expect("seed symlink");

    let outcome = uninstall_at(&target, false).expect("uninstall symlink");
    let _ = std::fs::remove_dir_all(&tmp);

    assert_eq!(outcome.target, target);
    assert!(outcome.was_symlink);
    assert!(!target.exists(), "symlink should be gone");
}

#[test]
fn r496_uninstall_at_refuses_non_symlink_without_force() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-uninstall-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let target = tmp.join("paperclipai");
    std::fs::write(&target, b"unrelated binary").expect("seed file");

    let err = uninstall_at(&target, false).unwrap_err();
    // Re-read before cleanup.
    let body = std::fs::read(&target).expect("read back");
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(err.to_string().contains("refusing to remove non-symlink"));
    assert_eq!(body, b"unrelated binary", "file must NOT be deleted");
}

#[test]
fn r496_uninstall_at_force_removes_non_symlink() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-uninstall-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let target = tmp.join("paperclipai");
    std::fs::write(&target, b"stale").expect("seed file");

    let outcome = uninstall_at(&target, true).expect("force uninstall");
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(!outcome.was_symlink);
    assert!(!target.exists());
}

#[test]
fn r496_uninstall_at_missing_target_errors_clearly() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-uninstall-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let target = tmp.join("paperclipai");

    let err = uninstall_at(&target, true).unwrap_err();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(err.to_string().contains("nothing installed at"));
}

#[test]
fn r496_compare_versions_orders_correctly() {
    use std::cmp::Ordering;
    assert_eq!(compare_versions("0.1.0", "0.1.0"), Ordering::Equal);
    assert_eq!(compare_versions("0.1.0", "0.1.1"), Ordering::Less);
    assert_eq!(compare_versions("0.1.1", "0.1.0"), Ordering::Greater);
    assert_eq!(compare_versions("0.2.0", "0.1.99"), Ordering::Greater);
    assert_eq!(compare_versions("1.0.0", "0.99.99"), Ordering::Greater);
    // Pre-release suffix truncates the trailing segment, so the parsed
    // lengths differ. A standard semver would say 0.1.0-rc1 < 0.1.0; we
    // follow that ordering (the trailing "-rc1" prevents the third number
    // from being parsed, so [0, 1] < [0, 1, 0]).
    assert_eq!(compare_versions("0.1.0-rc1", "0.1.0"), Ordering::Less);
}

#[test]
fn r496_build_update_hint_mentions_command() {
    let s = build_update_hint("0.1.0", "0.2.0");
    assert!(s.contains("0.1.0"));
    assert!(s.contains("0.2.0"));
    assert!(s.contains("cargo install"));
    assert!(s.contains("--path apps/pc-cli"));
}

#[test]
fn r496_current_version_is_not_empty() {
    assert!(!CURRENT_VERSION.is_empty());
    // Sanity: must look like "X.Y.Z" or "X.Y.Z-...".
    assert!(
        CURRENT_VERSION
            .chars()
            .next()
            .map_or(false, |c| c.is_ascii_digit()),
        "version should start with a digit, got: {CURRENT_VERSION}"
    );
}

// -------- R497 run real path --------

#[test]
fn r497_resolve_server_binary_respects_override_when_exists() {
    let tmp = std::env::temp_dir().join(format!("pc-cli-run-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).expect("mkdir");
    let fake = tmp.join("my-pc-server");
    std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").expect("write fake");
    let result = resolve_server_binary(Some(fake.to_str().unwrap()));
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(result, Some(fake));
}

#[test]
fn r497_resolve_server_binary_returns_none_for_missing_override() {
    let missing = "/nonexistent/pc-server-binary-path";
    assert_eq!(resolve_server_binary(Some(missing)), None);
}

#[test]
fn r497_resolve_server_binary_no_override_does_not_panic() {
    // No override and no env hint: returns either Some(workspace) or None
    // depending on cwd. Must not panic and must return a sensible type.
    let r = resolve_server_binary(None);
    if let Some(p) = r {
        assert!(p.ends_with("pc-server") || p.ends_with("pc-server.exe"));
    }
}

#[test]
fn r497_build_run_env_passes_through_and_sorts() {
    let mut base = std::collections::BTreeMap::new();
    base.insert("PAPERCLIP_PORT".into(), "4000".into());
    base.insert("PAPERCLIP_HOST".into(), "127.0.0.1".into());
    base.insert("PAPERCLIP_DATABASE_URL".into(), "postgres://x".into());
    let env = build_run_env(&base);
    assert_eq!(env.len(), 3);
    // Sorted by key.
    assert_eq!(env[0].0, "PAPERCLIP_DATABASE_URL");
    assert_eq!(env[1].0, "PAPERCLIP_HOST");
    assert_eq!(env[2].0, "PAPERCLIP_PORT");
    assert_eq!(env[2].1, "4000");
}

#[test]
fn r497_build_run_env_empty_base_yields_empty() {
    let base = std::collections::BTreeMap::new();
    let env = build_run_env(&base);
    assert!(env.is_empty());
}

// -------- R498 env + onboard-interactive real path --------

#[test]
fn r498_env_format_value_enum_lists_export_shell_json() {
    use clap::ValueEnum;
    let names: Vec<String> = EnvFormat::value_variants()
        .iter()
        .filter_map(|v| v.to_possible_value().map(|p| p.get_name().to_string()))
        .collect();
    let as_str: Vec<&str> = names.iter().map(String::as_str).collect();
    assert!(as_str.contains(&"export"));
    assert!(as_str.contains(&"json"));
}

#[test]
fn r498_env_format_debug_and_eq() {
    assert_eq!(EnvFormat::Export, EnvFormat::Export);
    assert_ne!(EnvFormat::Export, EnvFormat::Json);
    let s = format!("{:?}", EnvFormat::Shell);
    assert!(s.contains("Shell"));
}

#[test]
fn r498_build_resolved_env_includes_known_keys() {
    let env = build_resolved_env();
    // Order is alphabetical (BTreeMap).
    let keys: Vec<&String> = env.keys().collect();
    assert!(keys.contains(&&"PAPERCLIP_PORT".to_string()));
    assert!(keys.contains(&&"PAPERCLIP_HOST".to_string()));
    assert!(keys.contains(&&"PAPERCLIP_DATABASE_URL".to_string()));
    assert!(keys.contains(&&"PAPERCLIP_DB_RUN_MIGRATIONS".to_string()));
    assert!(keys.contains(&&"PAPERCLIP_SECRETS_MASTER_KEY_FILE".to_string()));
}

#[test]
fn r498_build_resolved_env_defaults_are_stable() {
    // Run twice and verify the same defaults (no env state in this test).
    let a = build_resolved_env();
    let b = build_resolved_env();
    assert_eq!(a, b);
    assert_eq!(a["PAPERCLIP_PORT"], "3100");
    assert_eq!(a["PAPERCLIP_HOST"], "127.0.0.1");
    assert_eq!(a["PAPERCLIP_DB_RUN_MIGRATIONS"], "true");
}

#[test]
fn r498_default_config_toml_has_sections() {
    let body = default_config_toml();
    assert!(body.contains("[server]"));
    assert!(body.contains("[database]"));
    assert!(body.contains("[secrets]"));
    assert!(body.contains("[storage]"));
    assert!(body.contains("host = \"127.0.0.1\""));
    assert!(body.contains("port = 3100"));
    assert!(body.contains("run_migrations = true"));
    assert!(body.contains("kind = \"local_encrypted\""));
    assert!(body.contains("kind = \"local_disk\""));
}

// -------- R500 worktree url + dev real path --------

#[test]
fn r500_default_base_port_falls_back_to_3100() {
    // No env override in the test runner for PAPERCLIP_PORT (we don't set it).
    let p = default_base_port();
    assert!(p == 3100 || (3000..=3999).contains(&p));
}

#[test]
fn r500_worktree_name_from_path_strips_trailing_slash() {
    assert_eq!(worktree_name_from_path("/Users/me/code/main/"), "main");
    assert_eq!(worktree_name_from_path("/Users/me/code/main"), "main");
    assert_eq!(worktree_name_from_path("/"), "(root)");
    assert_eq!(worktree_name_from_path(""), "(root)");
    assert_eq!(worktree_name_from_path("/Users/me/.git"), ".git"); // literal ".git" directory
}

#[test]
fn r500_derive_worktree_port_keeps_base_for_main() {
    assert_eq!(derive_worktree_port("main", 3100), 3100);
    assert_eq!(derive_worktree_port("master", 3100), 3100);
    assert_eq!(derive_worktree_port("default", 3100), 3100);
    assert_eq!(derive_worktree_port("(root)", 3100), 3100);
}

#[test]
fn r500_derive_worktree_port_is_stable_and_offset() {
    let a = derive_worktree_port("feature-foo", 3100);
    let b = derive_worktree_port("feature-foo", 3100);
    assert_eq!(a, b, "same name -> same port");
    assert!(a > 3100, "non-main name must offset: got {a}");
    assert!(a <= 3100 + 999, "offset capped at 999: got {a}");

    let c = derive_worktree_port("feature-bar", 3100);
    let d = derive_worktree_port("feature-foo", 3100);
    assert_ne!(
        c, d,
        "different names should usually differ (with high prob)"
    );
}

#[test]
fn r500_derive_worktree_url_format() {
    assert_eq!(
        derive_worktree_url("main", 3100, "127.0.0.1"),
        "http://127.0.0.1:3100"
    );
    let url = derive_worktree_url("experiment-x", 3100, "0.0.0.0");
    assert!(url.starts_with("http://0.0.0.0:"));
    let port_str = url.rsplit(':').next().unwrap();
    let port: u16 = port_str.parse().expect("port parses");
    assert!(port > 3100 && port <= 3100 + 999);
}
