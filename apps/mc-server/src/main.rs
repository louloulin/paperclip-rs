//! multica-server：Multica 后端服务器二进制入口。
//!
//! 启动序列（编号与 `main` 里的步骤注释一一对应）：
//! 1. 加载配置（mc-config）
//! 2. 初始化遥测（mc-telemetry）
//! 3. 启动横幅
//! 4. 连接数据库（mc-db）
//! 5. 运行迁移（可选；`MULTICA_DB_RUN_MIGRATIONS` 控制）
//! 6. 装配 axum 路由（mc-http）
//! 7. 启动渠道长连接宿主（mc-channel：五个 IM 平台的出站长连接，缺部署密钥则不装配）
//! 8. 启动代码与制品面后台宿主（mc-vcs-github：GitHub PR 快照刷新；缺 App 凭据则不装配）
//!
//! 8.5 装配 entitlement 平面（M9 anchor：配了云基址也**不装**平面 —— 客户端归 M9-9）
//! 9. 启动调度循环（mc-scheduler：autopilot 计划派发 + issue wakeup 派发）
//! 10. 启动 webhook 投递 worker 池（mc-autopilot：`1s` ticker + `Notify` + 4 并发）
//! 11. 监听 / graceful shutdown（先停渠道连接，再停 PR 刷新，再停投递 worker，再停调度器，
//!     最后停 actor）

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

mod channels;
// M8 anchor（LUM-1797 / docs/61-M8-PLAN.md §2.6）：代码与制品面的后台宿主位
// （ghsnapshot 的 PR 刷新 worker）。与 `channels` / `scheduler` 同造型。
mod integrations;
// M9 anchor（LUM-1815 / docs/62-M9-PLAN.md §2.1 / §9.8）：entitlement 平面的**组合根适配器**
// 宿主位。它必须在 `apps/mc-server`（唯一同时看得见 `mc-entitlement` 与本仓
// `mc_autopilot::quota` 接缝的地方）。与 `channels` / `integrations` / `scheduler` 同造型。
mod entitlement;
mod scheduler;
// M5-D8（LUM-1745 / docs/54-M5-5-WEBHOOK-INGRESS.md 的 D8 行）：webhook 投递 worker 的
// 轮询循环宿主。此前 `process_next_delivery*` 在**生产路径上零调用点** ⇒ 入站落下的 `queued`
// 行除测试外永远不被消费。与 `channels` / `integrations` / `scheduler` 同造型。
mod webhook_worker;

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
    // 渠道密钥在这里 clone 一份：下面 `with_state(state)` 会把 `Arc<AppState>` **移进** router，
    // 而第 7 步的渠道宿主仍要读它（`ChannelKeys` 是 `Clone`，内部就是五个 `SecretBox`）。
    let channel_keys = state.channel_keys.clone();
    // M8 anchor（`LUM-1797`）：GitHub App 的部署密钥也 clone 一份（同一理由）。
    let github_keys = state.github_keys.clone();
    // M9 anchor（`LUM-1815`）：entitlement 面的部署事实也 clone 一份（同一理由）——
    // 第 8.5 步的适配器要在 `with_state(state)` **之后**才跑，而那一步已经把
    // `Arc<AppState>` 移进 router 了。
    let entitlement_config = state.entitlement.clone();
    // M5-D8（`LUM-1745`）：第 9 步的投递 worker 池要 `realtime` 出口（与入站面共用同一条
    // 事件总线 —— B 段派发 `dispatch_run` 会广播 `task.queued` / `task.available`，不能另建）。
    let realtime = state.realtime.clone();
    let api_router = mc_http::routes::router(state.clone());
    let app: Router = apply_default_middleware(api_router).with_state(state);

    // 7. 启动渠道长连接宿主（M7 anchor / `LUM-1765`）：五个 IM 平台全部是**出站长连接**
    //    （无 webhook 路由，`docs/60` §1.5），所以它们与 HTTP 面**共享同一个进程**但走
    //    自己的装配点。判据 = **部署密钥存在**（缺则整体不装配），装配与停机都在
    //    `channels.rs`。
    //    ⚠️ 第二个实参现在是 `None`（端口实现归 M7-1/M7-2）⇒ 即使配了密钥也**只 warn 不起连接**，
    //    绝不假装连上了；接线后这里换成 `Some(deps)`，签名不变。
    let channel_handles = channels::start(&channel_keys, None);
    tracing::info!(
        configured = ?channel_handles.configured(),
        factories = ?channel_handles.registry().kinds(),
        wired = channel_handles.is_wired(),
        connections = channel_handles.has_connections(),
        "channel host started"
    );

    // 8. 启动代码与制品面的后台宿主（M8 anchor / `LUM-1797`）：GitHub PR 快照刷新的
    //    worker 池 + TTL sweeper（`mc_vcs_github::ghsnapshot::Manager`）。判据 = **App 凭据
    //    存在**（缺则整体不装配），装配与停机都在 `integrations.rs`。
    //    ⚠️ 锚点期 `Manager::start` 仍是 `todo!()`（实现归 M8-5）⇒ 即使配了凭据也**只 warn
    //    不起 worker**，绝不假装刷新已接上；接线后这里换成真调用，签名不变。
    let integration_handles = integrations::start(&github_keys);
    tracing::info!(
        app_configured = integration_handles.is_app_configured(),
        wired = integration_handles.is_wired(),
        pr_refresh = integration_handles.pr_refresh().is_some(),
        "integration host started"
    );

    // 8.5 装配 entitlement 平面（M9 anchor / `LUM-1815`）：云基址的**部署事实**经
    //     `AppState::entitlement` 进来（生产只有一处 env 解析）。
    //     🔴 anchor 期**配了也不装平面**（策略客户端归 M9-9）—— 装一个替身平面会让
    //     quota 从 `off` 变成有策略并**真的拦住** autopilot，那是生产事故而不是接线完成。
    //     装机点在**调度器之前**：调度器与 HTTP 面读的是同一个进程级平面，
    //     第一轮 tick 之前就必须定下来（`install_policy_provider` 是**一次性**的）。
    let entitlement_handles = entitlement::start(&entitlement_config);
    tracing::info!(
        configured = entitlement_handles.is_configured(),
        wired = entitlement_handles.is_wired(),
        "entitlement host started"
    );

    // 9. 启动调度循环（M5-9 / `docs/48` §7.1）：两个 job 在 `spawn` 前注册，第一轮 tick 立刻跑，
    //    所以「进程起来了但调度器没接」这种静默失效在这里被消灭。失败即启动失败（`register_all`
    //    的错误只可能是规格错误，属于开发者错误，不该带病起服务）。
    //    wakeup 派发的第 7 步要广播 `task.queued` / `task.available`，所以取 `AppState` 里那一个
    //    hub —— 和 HTTP 面 / daemon 面共用同一条广播总线，不能另建。
    let scheduler_handle = scheduler::start(&db, daemon_hub).context("start scheduler")?;

    // 10. 启动 webhook 投递 worker 池（M5-D8 / `LUM-1745`）：`/api/webhooks/**` 的入站把投递落成
    //     `queued`，本池是**唯一**把它推到终态的消费者（`process_next_delivery`；上游
    //     `cmd/server/main.go:744` 的 `go h.WebhookDeliveryWorker.Run(sweepCtx)`）。无装配
    //     判据（队列/租约都在 Postgres，worker 无条件起）。池同时把自己的提示口注进 `mc-http`
    //     的进程级槽（`routes/webhooks/autopilots.rs::set_webhook_notify_port`）⇒ 入站那一头
    //     才能「多快被消费」而不只是「一拍 ticker 内被消费」。
    let webhook_handles = webhook_worker::start(&db, realtime);

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

    // 停机顺序固定（`docs/60` §2.4 / R-M7-7 + `docs/61` §2.6 + `docs/32` §27）：**先停渠道连接**
    // （挂着不退的长连接会让 graceful shutdown 永远等在那里），**再停 PR 刷新**（在飞的
    // GraphQL 请求先收尾），**再停投递 worker**（它自己等 5s，让正在收口的那条投递跑完 ——
    // 停机面的**最后一段消费者**必须先于调度器停，否则调度器收尾时新冒出来的投递没人接），
    // **再停调度器**（让在跑的 handler 收尾 —— handle 内部会等当前 tick 结束），
    // **最后停 actor 池**。
    channel_handles.shutdown().await;
    integration_handles.shutdown().await;
    // entitlement 面没有后台生命周期（上游逐字「no goroutines or background
    // lifecycle」）⇒ 这一步现在只关掉自己的记账；顺序放在**调度器之前**，
    // 与「消费者先于生产者停」的既有纪律一致。
    entitlement_handles.shutdown().await;
    webhook_handles.shutdown().await;
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
