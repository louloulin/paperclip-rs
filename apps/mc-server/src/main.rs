//! multica-server：Multica 后端服务器二进制入口。
//!
//! 启动序列：
//! 1. 加载配置（mc-config）
//! 2. 初始化遥测（mc-telemetry）
//! 3. 连接数据库（mc-db）
//! 4. 运行迁移（可选；`MULTICA_DB_RUN_MIGRATIONS` 控制）
//! 5. 装配 axum 路由（mc-http）
//! 6. 启动调度循环（mc-scheduler：autopilot 计划派发 + issue wakeup 派发）
//! 7. 监听 / graceful shutdown（先停调度器，再停 actor）

use std::sync::Arc;

use anyhow::Context;
use axum::Router;

use mc_config::Config;
use mc_core::actor::{spawn_system_actor, ActorKey, ActorRegistry};
use mc_db::{Db, Migrator};
use mc_http::apply_default_middleware;
use mc_http::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};
use mc_realtime::{RealtimeHandle, WsState};
use mc_telemetry::{log_banner, StartupBanner, TelemetryOptions};

mod scheduler;

#[tokio::main]
#[allow(clippy::too_many_lines)] // 启动流程按 1..N 步骤线性展开（配置→迁移→路由→监听）。
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
        let dir = std::env::var("MULTICA_MIGRATIONS_DIR").map_or_else(
            |_| std::path::PathBuf::from("migrations"),
            std::path::PathBuf::from,
        );
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

    // M3-2：生产装配注册内置 adapter —— `pi-local`（其余 24 个走 M3-8，白名单见
    // `mc_runtime::AgentType`）。**只注册、不探测**：`launch` 时才 `exec`，
    // 所以机器上没装 `pi` 不影响起服务。
    let adapters = Arc::new(AdapterRegistry::with_builtin_adapters());

    let realtime = RealtimeHandle::start(1024);
    let ws = Arc::new(WsState::new(realtime.clone(), "multica-rs"));

    // M3 anchor scaffold（LUM-1406 / docs/15 §7.2 第 6 项）：此处生产装配**显式给出全部**
    // 字段，末尾的 `..ConfigSnapshot::default()` 因此是「空更新」，clippy 的
    // `needless_update` 会响 —— 这是刻意的：M3 切片给 `ConfigSnapshot` 追加字段时，
    // 本处（以及 mc-conformance / tests/*.rs 的 10 处）不再需要改动，
    // 只需在 `state.rs` 决定新字段的默认值（`runtime` 是 M3-2 按 §7.6 接线的那个
    // 例外：它显式取 `cfg.runtime`，因为「接上真实配置」才是这个字段存在的意义）。
    #[allow(clippy::needless_update)]
    let config = ConfigSnapshot {
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
        invitation_per_workspace_per_hour: Some(cfg.auth.invitation_per_workspace_per_hour),
        runtime: cfg.runtime.clone(),
        ..ConfigSnapshot::default()
    };

    let state = Arc::new(AppState::new(
        db.clone(),
        RuntimeHandles {
            actors: actors.clone(),
            adapters,
        },
        config,
        realtime,
        ws,
    ));

    // routes::router 以 A 的签名为基（接受 Arc<AppState>）；这里再 with_state 注入，
    // 使 Router<Arc<AppState>> → Router<()> 后套默认 middleware 链。
    let daemon_hub = state.daemon_hub.clone();
    let api_router = mc_http::routes::router(state.clone());
    let app: Router = apply_default_middleware(api_router).with_state(state);

    // 7. 启动调度循环（M5-9 / `docs/48` §7.1）：两个 job 在 `spawn` 前注册，第一轮 tick 立刻跑，
    //    所以「进程起来了但调度器没接」这种静默失效在这里被消灭。失败即启动失败（`register_all`
    //    的错误只可能是规格错误，属于开发者错误，不该带病起服务）。
    //    wakeup 派发的第 7 步要广播 `task.queued` / `task.available`，所以取 `AppState` 里那一个
    //    hub —— 和 HTTP 面 / daemon 面共用同一条广播总线，不能另建。
    let scheduler_handle = scheduler::start(&db, daemon_hub).context("start scheduler")?;

    let addr = std::net::SocketAddr::from((
        cfg.server.host.parse::<std::net::IpAddr>()?,
        cfg.server.port,
    ));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        host = %cfg.server.host,
        port = cfg.server.port,
        total_startup_ms = u64::try_from(startup_start.elapsed().as_millis()).unwrap_or(u64::MAX),
        "multica http listening"
    );

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("axum serve")?;

    // 先停调度器：让在跑的 handler 收尾（handle 内部会等当前 tick 结束），再停 actor 池。
    scheduler_handle.shutdown().await;
    actors.shutdown().context("shutdown actors")?;
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
