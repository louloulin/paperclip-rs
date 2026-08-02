//! paperclip-server：Paperclip 后端服务器二进制入口。
//!
//! Phase A 启动序列：
//! 1. 加载配置（pc-config）
//! 2. 初始化遥测（pc-telemetry）
//! 3. 启动横幅
//! 4. 连接数据库（pc-db）
//! 5. 执行迁移（`pc-db::Migrator`）
//! 6. 装配 axum 路由 + 监听
//! 7. 等待信号；graceful shutdown

use std::sync::Arc;

use anyhow::Context;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json, Router};
use serde::Serialize;
use tokio::signal;
use tracing::info;

use pc_config::Config;
use pc_db::{Db, HealthCheck, Migrator};
use pc_telemetry::{log_banner, StartupBanner, TelemetryOptions};

#[derive(Clone)]
#[allow(dead_code)]
struct AppState {
    config: Arc<Config>,
    db: Db,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    db: pc_db::health::DbHealth,
}

async fn health_handler(State(state): State<AppState>) -> impl IntoResponse {
    let db = HealthCheck::check(&state.db).await;
    let overall_ok = db.ok;
    let body = HealthResponse {
        status: if overall_ok { "ok" } else { "degraded" },
        version: env!("CARGO_PKG_VERSION"),
        db,
    };
    let status = if overall_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(body))
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", axum::routing::get(health_handler))
        .with_state(state)
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

    // 6. 启动 HTTP
    let state = AppState {
        config: cfg.clone(),
        db,
    };
    let app = build_router(state);

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
