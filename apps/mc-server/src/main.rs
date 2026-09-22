//! multica-server：Multica 后端服务器二进制入口。
//!
//! 启动序列：
//! 1. 加载配置（mc-config）
//! 2. 初始化遥测（mc-telemetry）
//! 3. 连接数据库（mc-db）
//! 4. 运行迁移（可选；`MULTICA_DB_RUN_MIGRATIONS` 控制）
//! 5. 装配 axum 路由（mc-http）
//! 6. 监听 / graceful shutdown

use std::sync::Arc;

use anyhow::Context;
use axum::Router;

use mc_config::Config;
use mc_core::actor::{spawn_system_actor, ActorKey, ActorRegistry};
use mc_db::{Db, Migrator};
use mc_http::apply_default_middleware;
use mc_http::state::{AdapterRegistryStub, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use mc_telemetry::{log_banner, StartupBanner, TelemetryOptions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let startup_start = std::time::Instant::now();

    // 1. 加载配置
    let config = Config::from_env().context("load config")?;
    let cfg = Arc::new(config.clone());

    // 2. 初始化遥测
    let telemetry_opts = TelemetryOptions {
        service_name: "multica-server".into(),
        json_output: cfg.server.mode != mc_config::RunMode::Development,
        default_level: tracing::Level::INFO,
    };
    mc_telemetry::init(&telemetry_opts)?;

    #[cfg(feature = "otlp")]
    {
        if let Err(error) = mc_telemetry::install_global(&mc_telemetry::OtlpConfig {
            service_name: telemetry_opts.service_name.clone(),
            ..Default::default()
        }) {
            tracing::info!(error = %error, "otlp not installed");
        }
    }

    // 3. 启动横幅
    let banner = StartupBanner {
        service: "multica-server".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        build_time: std::env::var("MULTICA_BUILD_TIME").unwrap_or_else(|_| "dev".into()),
        commit: std::env::var("MULTICA_COMMIT").unwrap_or_else(|_| "dev".into()),
        mode: match cfg.server.mode {
            mc_config::RunMode::Development => "development",
            mc_config::RunMode::Production => "production",
            mc_config::RunMode::Test => "test",
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
        // 默认从 ./migrations 目录加载
        let dir = std::env::var("MULTICA_MIGRATIONS_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("migrations"));
        match Migrator::load(dir) {
            Ok(steps) if !steps.is_empty() => {
                tracing::info!(count = steps.len(), "applying migrations");
                Migrator::run(&db, steps).await.context("run migrations")?;
            }
            Ok(_) => {
                tracing::info!("no migration files found; skipping");
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not load migrations directory; skipping");
            }
        }
    } else {
        tracing::info!("migrations skipped (MULTICA_DB_RUN_MIGRATIONS=false)");
    }

    // 6. 装配 axum 路由
    let actors = ActorRegistry::new();
    actors
        .register(
            ActorKey::new("system", "root"),
            spawn_system_actor("multica-root"),
        )
        .context("register root actor")?;

    let adapters = Arc::new(AdapterRegistryStub::default());
    // M0/M1 stub：注册占位 adapter 名（真实 adapter 注册在 M3）。
    adapters.register("stub-adapter");

    let realtime = RealtimeHandle::start(1024);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs"));

    let state = Arc::new(AppState::new(
        db.clone(),
        RuntimeHandles {
            actors: actors.clone(),
            adapters,
        },
        ConfigSnapshot {
            host: cfg.server.host.clone(),
            port: cfg.server.port,
            session_cookie: cfg.auth.session_cookie_name.clone(),
            api_key_header: cfg.auth.api_key_header.clone(),
            csrf_header: cfg.auth.csrf_header.clone(),
            dev_mode: cfg.server.mode == mc_config::RunMode::Development
                || cfg.server.mode == mc_config::RunMode::Test,
            session_ttl_secs: cfg.auth.session_ttl_secs,
            verification_code_ttl_secs: cfg.auth.verification_code_ttl_secs,
            send_code_per_email_per_min: cfg.auth.send_code_per_email_per_min,
        },
        realtime,
        ws,
    ));

    // routes::router 以 A 的签名为基（接受 Arc<AppState>）；这里再 with_state 注入，
    // 使 Router<Arc<AppState>> → Router<()> 后套默认 middleware 链。
    let api_router = mc_http::routes::router(state.clone());
    let app: Router = apply_default_middleware(api_router).with_state(state);

    let addr = std::net::SocketAddr::from((
        cfg.server.host.parse::<std::net::IpAddr>()?,
        cfg.server.port,
    ));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        host = %cfg.server.host,
        port = cfg.server.port,
        total_startup_ms = startup_start.elapsed().as_millis() as u64,
        "multica http listening"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("axum serve")?;

    actors.shutdown().await.context("shutdown actors")?;
    tracing::info!("shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("ctrl-c received, shutting down"),
        () = terminate => tracing::info!("SIGTERM received, shutting down"),
    }
}
