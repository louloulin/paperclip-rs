//! paperclip-server：Paperclip 后端服务器二进制入口。
//!
//! 启动序列：
//! 1. 加载配置（pc-config）
//! 2. 初始化遥测（pc-telemetry）
//! 3. 启动横幅
//! 4. 连接数据库（pc-db）
//! 5. 执行迁移（`pc-db::Migrator`）
//! 6. 装配 axum 路由（pc-http 56 路由） + 监听
//! 7. 等待信号；graceful shutdown

use std::sync::Arc;

use anyhow::Context;
use axum::Router;
use pc_adapter_api::AdapterRegistry;
use pc_adapter_claude_local::ClaudeLocalAdapter;
use pc_adapter_codex_local::CodexLocalAdapter;
use pc_adapter_cursor_cloud::CursorCloudAdapter;
use pc_adapter_cursor_local::CursorLocalAdapter;
use pc_adapter_gemini_local::GeminiLocalAdapter;
use pc_adapter_grok_local::GrokLocalAdapter;
use pc_adapter_hermes::HermesAdapter;
use pc_adapter_hermes_gateway::HermesGatewayAdapter;
use pc_adapter_openclaw_gateway::OpenclawGatewayAdapter;
use pc_adapter_opencode_local::OpencodeLocalAdapter;
use pc_adapter_pi_local::PiLocalAdapter;
use pc_config::Config;
use pc_core::{spawn_system_actor, ActorKey, ActorRegistry};
use pc_db::{Db, Migrator};
use pc_heartbeat::spawn_heartbeat_supervisor;
use pc_heartbeat::{StartHeartbeat, StartHeartbeatResult};
use pc_http::AppState;
use pc_realtime::{RealtimeHandle, WsState};
use pc_repos::agent::AgentRepo;
use pc_repos::heartbeat::HeartbeatRepo;
use pc_repos::settings::SettingsRepo;

use pc_telemetry::{log_banner, StartupBanner, TelemetryOptions};
use tokio::signal;
use tracing::info;

#[tokio::main]
#[allow(clippy::too_many_lines)]
async fn main() -> anyhow::Result<()> {
    pc_secrets::ensure_decision_signing_secret().context("initialize decision signing secret")?;

    // 1. 加载配置
    let config = Config::from_env().context("load config")?;
    let cfg = Arc::new(config.clone());

    // 2. 遥测
    let telemetry_opts = TelemetryOptions {
        service_name: "paperclip-server".into(),
        json_output: cfg.mode != pc_config::RunMode::Development,
        default_level: tracing::Level::INFO,
    };
    pc_telemetry::init(&telemetry_opts)?;

    // 2a. 可选 OTLP exporter
    #[cfg(feature = "otlp")]
    {
        match pc_telemetry::install_global(&pc_telemetry::OtlpConfig {
            service_name: telemetry_opts.service_name.clone(),
            service_version: env!("CARGO_PKG_VERSION").to_string(),
            ..Default::default()
        }) {
            Ok(_provider) => {
                info!("otlp: tracing layer installed");
            }
            Err(error) => {
                info!(error = %error, "otlp: not installed (disabled or invalid config)");
            }
        }
    }

    // 3. 启动横幅
    let banner = StartupBanner {
        service: "paperclip-server".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        build_time: std::env::var("PAPERCLIP_BUILD_TIME").unwrap_or_else(|_| "dev".into()),
        commit: std::env::var("PAPERCLIP_COMMIT").unwrap_or_else(|_| "dev".into()),
        mode: match cfg.mode {
            pc_config::RunMode::Development => "development",
            pc_config::RunMode::Production => "production",
            pc_config::RunMode::Test => "test",
        },
    };
    log_banner(&banner);

    // 4. 连接数据库
    let db = Db::connect(
        &cfg.database.url,
        cfg.database.max_connections,
        cfg.database.min_connections,
    )
    .await
    .context("connect db")?;

    // 5. 迁移
    if cfg.database.run_migrations {
        Migrator::run(&db).await.context("run migrations")?;
    } else {
        info!("migrations skipped (PAPERCLIP_DB_RUN_MIGRATIONS=false)");
    }

    // 6. 启动 Actor 根运行时并装配 axum 路由（pc-http 56 路由）
    let actors = ActorRegistry::new();
    actors
        .register(
            ActorKey::new("system", "root"),
            spawn_system_actor("paperclip-root"),
        )
        .context("register root actor")?;
    let heartbeat = spawn_heartbeat_supervisor(50, actors.clone());
    let agent_supervisor = pc_agent::spawn_agent_supervisor(db.clone());
    let adapters = AdapterRegistry::new();
    {
        adapters
            .register(Arc::new(CodexLocalAdapter::new()))
            .context("register codex local adapter")?;
        adapters
            .register(Arc::new(ClaudeLocalAdapter::new()))
            .context("register claude local adapter")?;
        adapters
            .register(Arc::new(CursorCloudAdapter::new()))
            .context("register cursor cloud adapter")?;
        adapters
            .register(Arc::new(CursorLocalAdapter::new()))
            .context("register cursor local adapter")?;
        adapters
            .register(Arc::new(GeminiLocalAdapter::new()))
            .context("register gemini local adapter")?;
        adapters
            .register(Arc::new(GrokLocalAdapter::new()))
            .context("register grok local adapter")?;
        adapters
            .register(Arc::new(HermesAdapter::new()))
            .context("register hermes adapter")?;
        adapters
            .register(Arc::new(HermesGatewayAdapter::new()))
            .context("register hermes gateway adapter")?;
        adapters
            .register(Arc::new(OpenclawGatewayAdapter::new()))
            .context("register openclaw gateway adapter")?;
        adapters
            .register(Arc::new(OpencodeLocalAdapter::new()))
            .context("register opencode local adapter")?;
        adapters
            .register(Arc::new(PiLocalAdapter::new()))
            .context("register pi local adapter")?;
    }
    actors
        .register(
            ActorKey::new("system", "heartbeat-supervisor"),
            heartbeat.clone(),
        )
        .context("register heartbeat supervisor")?;
    actors
        .register(
            ActorKey::new("system", "agent-supervisor"),
            agent_supervisor.clone(),
        )
        .context("register agent supervisor")?;
    recover_heartbeat_runs(&db, &heartbeat).await?;
    let realtime = RealtimeHandle::start(1024);
    let ws = std::sync::Arc::new(WsState::new(realtime.clone(), "paperclip-rs"));
    let state = AppState::new(
        db,
        pc_http::state::RuntimeHandles {
            actors: actors.clone(),
            heartbeat,
            agents: agent_supervisor,
            adapters,
        },
        pc_http::state::ConfigSnapshot {
            host: cfg.server.host.clone(),
            port: cfg.server.port,
            session_cookie: cfg.auth.session_cookie_name.clone(),
            api_key_header: cfg.auth.api_key_header.clone(),
            csrf_header: cfg.auth.csrf_header.clone(),
        },
        telemetry_opts,
        ws,
        realtime.clone(),
    );
    let scheduler_state = state.clone();
    let heartbeat_scheduler = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if heartbeat_scheduling_suppressed(&scheduler_state.db)
                .await
                .is_some()
            {
                continue;
            }
            let runs = match HeartbeatRepo::new(&scheduler_state.db)
                .list_recoverable(200)
                .await
            {
                Ok(runs) => runs,
                Err(error) => {
                    tracing::warn!(error = %error, "heartbeat scheduler query failed");
                    continue;
                }
            };
            for run in runs {
                let run = if run.status == "scheduled_retry" {
                    match HeartbeatRepo::new(&scheduler_state.db)
                        .promote_due_scheduled_retry(run.id)
                        .await
                    {
                        Ok(Some(promoted)) => promoted,
                        Ok(None) => continue,
                        Err(error) => {
                            tracing::warn!(error = %error, run_id = %run.id, "heartbeat retry promotion failed");
                            continue;
                        }
                    }
                } else if run.status == "queued" {
                    run
                } else {
                    continue;
                };
                match pc_http::routes::agents::dispatch_queued_heartbeat(&scheduler_state, run)
                    .await
                {
                    Ok(Some(run)) => {
                        tracing::debug!(run_id = %run.id, "heartbeat scheduler dispatched run")
                    }
                    Ok(None) => {}
                    Err(error) => {
                        tracing::warn!(error = %error, "heartbeat scheduler dispatch failed")
                    }
                }
            }
            match pc_http::routes::agents::dispatch_due_issue_monitors(&scheduler_state, 50).await {
                Ok(count) if count > 0 => {
                    tracing::debug!(count, "heartbeat scheduler dispatched issue monitors")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(error = %error, "issue monitor scheduler failed"),
            }
            match pc_http::routes::agents::dispatch_due_timer_heartbeats(&scheduler_state, 200)
                .await
            {
                Ok(count) if count > 0 => {
                    tracing::debug!(count, "heartbeat scheduler dispatched timer heartbeats")
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(error = %error, "timer heartbeat scheduler failed"),
            }
            // Recover stale wakeup claims: any wakeup held in `claimed` state
            // for longer than 5 minutes is reset to `requested` so it can be
            // claimed by the next scheduler tick.
            match AgentRepo::new(&scheduler_state.db)
                .recover_stale_wakeup_claims(300)
                .await
            {
                Ok(0) => {}
                Ok(count) => {
                    tracing::debug!(count, "heartbeat scheduler recovered stale wakeup claims")
                }
                Err(error) => tracing::warn!(error = %error, "stale wakeup recovery failed"),
            }
            // Status card tick: claim pending status cards whose next_eval_at
            // has passed and dispatch them to the refresh pipeline.
            match pc_http::routes::status_cards::claim_due_status_card_updates(&scheduler_state, 50)
                .await
            {
                Ok(0) => {}
                Ok(count) => tracing::debug!(count, "status card scheduler claimed updates"),
                Err(error) => tracing::warn!(error = %error, "status card scheduler failed"),
            }
        }
    });
    // ---- Bootstrap runtime services into AppState ----
    {
        use pc_storage::LocalDiskStorage;

        // Register local-disk provider with a default root (under $HOME/.paperclip/storage).
        let storage_root = dirs::home_dir().map_or_else(
            || std::path::PathBuf::from(".paperclip-storage"),
            |h| h.join(".paperclip").join("storage"),
        );
        let local = std::sync::Arc::new(LocalDiskStorage::new(storage_root.clone()));
        if let Err(e) = state.storage.register(local) {
            tracing::warn!(error = %e, "storage.register(local_disk) failed");
        }
        if let Err(e) = state.storage.route_bucket("paperclip-assets", "local_disk") {
            tracing::warn!(error = %e, "storage.route_bucket failed");
        }
        if let Err(e) = state.storage.route_bucket("paperclip-public", "local_disk") {
            tracing::warn!(error = %e, "storage.route_bucket failed");
        }
        tracing::info!(root = %storage_root.display(), "storage: local_disk provider registered");

        // Register a default feature flag so /api/feature-flags has non-empty data on startup.
        state.feature_flags.catalog().register(
            pc_feature_flags::FeatureKey::new("pc.ui.dense-mode"),
            true,
            None,
        );
        state.feature_flags.catalog().register(
            pc_feature_flags::FeatureKey::new("pc.workflows.auto-archive"),
            true,
            Some(pc_feature_flags::rules::RolloutRule {
                strategy: pc_feature_flags::rules::RolloutStrategy::Percentage { pct: 25 },
            }),
        );
        tracing::info!("feature flags: registered 2 default flags");
    }

    // ---- Bootstrap plugins into WorkerPool ----
    {
        use pc_plugin_host::registry::{PluginEntry, PluginStatus};
        use pc_plugin_protocol::manifest::PaperclipPluginManifestV1;
        use pc_repos::plugin::PluginRepo;
        let plugin_repo = PluginRepo::new(&state.db);
        match plugin_repo.list_filtered(Some("ready")).await {
            Ok(rows) => {
                let mut count = 0usize;
                for row in rows {
                    let manifest: PaperclipPluginManifestV1 = match serde_json::from_value(
                        row.manifest_json.clone(),
                    ) {
                        Ok(m) => m,
                        Err(e) => {
                            tracing::warn!(plugin = %row.plugin_key, error = %e, "plugin manifest parse failed");
                            continue;
                        }
                    };
                    let (cmd, args) = if manifest.entry.contains(' ') {
                        let mut parts = manifest.entry.split_whitespace();
                        let c = parts.next().unwrap_or("node").to_string();
                        (c, parts.map(String::from).collect())
                    } else {
                        (manifest.entry.clone(), vec![])
                    };
                    let package_path = row
                        .package_path
                        .clone()
                        .unwrap_or_else(|| format!("./plugins/{}", row.plugin_key));
                    let opts = pc_plugin_host::WorkerOptions {
                        plugin_id: row.id,
                        command: cmd,
                        args,
                        cwd: Some(std::path::PathBuf::from(package_path)),
                        env: vec![],
                        plugin_version: row.version.clone(),
                        manifest_version: manifest.manifest_version.clone(),
                        instance_id: uuid::Uuid::nil(),
                        init_timeout: std::time::Duration::from_secs(15),
                    };
                    let entry = PluginEntry {
                        plugin_id: row.id,
                        plugin_key: row.plugin_key.clone(),
                        manifest,
                        install_order: row.install_order.unwrap_or(0),
                        status: PluginStatus::Ready,
                    };
                    if let Err(e) = state.plugin_registry.register(entry) {
                        tracing::warn!(plugin = %row.plugin_key, error = %e, "plugin registry insert failed");
                        continue;
                    }
                    match state.plugin_workers.spawn(opts).await {
                        Ok(_handle) => {
                            tracing::info!(plugin = %row.plugin_key, "plugin worker spawned");
                            count += 1;
                        }
                        Err(e) => {
                            tracing::warn!(plugin = %row.plugin_key, error = %e, "plugin worker spawn failed");
                        }
                    }
                }
                tracing::info!(count, "plugin workers bootstrapped");
            }
            Err(e) => tracing::warn!(error = %e, "plugin bootstrap query failed"),
        }
    }

    let api_router = pc_http::middleware::apply_default_middleware(pc_http::routes::router())
        // auth_layer: 必须用 route_layer 而不是 layer，因为 router 尚未带 state
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            pc_http::middleware::auth::auth_layer,
        ))
        .with_state(state.clone());
    // Look for a UI dist bundle to serve alongside the API.
    let ui_dist_path: Option<std::path::PathBuf> = std::env::var("UI_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.join("index.html").exists())
        .or_else(|| {
            let candidates = [
                std::path::PathBuf::from("ui/dist"),
                std::path::PathBuf::from("../ui/dist"),
            ];
            candidates
                .into_iter()
                .find(|p| p.join("index.html").exists())
        });

    let app: Router = if let Some(ui_path) = ui_dist_path.clone() {
        tracing::info!(path = %ui_path.display(), "serving UI bundle from dist");
        let index_html = ui_path.join("index.html");
        api_router.fallback_service(
            tower_http::services::ServeDir::new(ui_path.clone())
                .fallback(tower_http::services::ServeFile::new(index_html)),
        )
    } else {
        api_router
    };

    let addr = std::net::SocketAddr::from((
        cfg.server.host.parse::<std::net::IpAddr>()?,
        cfg.server.port,
    ));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    info!(host = %cfg.server.host, port = cfg.server.port, "http listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("axum serve")?;

    heartbeat_scheduler.abort();

    actors.shutdown().await.context("shutdown actors")?;

    info!("shutdown complete");
    Ok(())
}

/// Check whether the heartbeat scheduler should be suppressed. Mirrors
/// Node `resolveHeartbeatSchedulingSuppression` plus the worktree run
/// execution override cache. Returns the suppression reason when the
/// scheduler should skip the current tick.
async fn heartbeat_scheduling_suppressed(db: &pc_db::Db) -> Option<&'static str> {
    fn truthy(name: &str) -> bool {
        matches!(
            std::env::var(name).ok().as_deref(),
            Some("true" | "1" | "yes" | "on")
        )
    }
    if truthy("PAPERCLIP_DATABASE_RESTORE_IN_PROGRESS") || truthy("PAPERCLIP_RESTORE_IN_PROGRESS") {
        return Some("database_restore_in_progress");
    }
    if !truthy("PAPERCLIP_IN_WORKTREE") {
        return None;
    }
    if truthy("PAPERCLIP_ENABLE_WORKTREE_RUN_EXECUTION") {
        return None;
    }
    // Worktree instance: honor the experimental override. A read failure
    // fails closed to the safe suppressed state.
    let instance_id = std::env::var("PAPERCLIP_INSTANCE_ID").ok();
    match SettingsRepo::new(db)
        .resolve_worktree_run_execution_activation(instance_id.as_deref())
        .await
    {
        Ok(activation) if activation.armed => None,
        Ok(_) => Some("worktree_instance"),
        Err(error) => {
            tracing::warn!(
                ?error,
                "worktree run execution activation read failed; defaulting to suppressed"
            );
            Some("worktree_instance")
        }
    }
}

async fn recover_heartbeat_runs(
    db: &Db,
    heartbeat: &pc_core::actor_runtime::kameo_api::ActorRef<pc_heartbeat::HeartbeatSupervisor>,
) -> anyhow::Result<()> {
    let runs = HeartbeatRepo::new(db)
        .list_recoverable(10_000)
        .await
        .context("list recoverable heartbeat runs")?;
    let mut recovered = 0usize;
    let mut deferred = 0usize;
    for run in runs {
        match heartbeat.ask(StartHeartbeat { run_id: run.id }).await {
            Ok(StartHeartbeatResult::Started | StartHeartbeatResult::AlreadyActive) => {
                recovered += 1;
            }
            Err(pc_core::actor_runtime::kameo_api::SendError::HandlerError(
                pc_heartbeat::HeartbeatSupervisorError::CapacityExceeded { .. },
            )) => {
                deferred += 1;
            }
            Err(error) => {
                tracing::warn!(run_id = %run.id, error = %error, "heartbeat run recovery failed");
            }
        }
    }
    info!(recovered, deferred, "heartbeat run recovery complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("ctrl-c received, shutting down"),
        () = terminate => info!("SIGTERM received, shutting down"),
    }
}
