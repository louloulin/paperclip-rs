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
use pc_config::Config;
use pc_db::{Db, Migrator};
use pc_http::AppState;
use pc_realtime::{RealtimeHandle, WsState};

use pc_telemetry::{log_banner, StartupBanner, TelemetryOptions};
use tokio::signal;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    // 6. 装配 axum 路由（pc-http 56 路由）
    let realtime = RealtimeHandle::start(1024);
    let ws = std::sync::Arc::new(WsState {
        realtime: realtime.clone(),
        server_name: "paperclip-rs".into(),
    });
    let state = AppState::new(
        db,
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
    let app: Router = pc_http::routes::router().with_state(state);

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

    info!("shutdown complete");
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
