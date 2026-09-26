//! `GET /healthz` + `GET /readyz`（M10-2）的可判定用例。
//!
//! 上游判据来源：`server/cmd/server/health.go::readyHandler` + `readiness()`（`f41fae6b08fb`
//! L93-182）。⚠️ 这两条路径在 ⑨ 门里**没有 fixture**（`docs/64` §6.2：M10 的 18 条 fixture 只覆盖
//! `/api/config`(17) + `/health`(1)）⇒ 本文件 + `docs/64` §2.1 的逐字契约是唯一的字段级判据。
//!
//! 两层，按"要不要真库"分：
//!
//! | 层 | 驱动 | 跑在哪道门 | 覆盖面 |
//! | --- | --- | --- | --- |
//! | **状态机层**（1–11） | `FakeProbe`（替身） | ⑤ `cargo test --workspace`（**零库**） | 三态 + 200、键集、缓存、单飞、帧形态 |
//! | **生产接线层**（12–15） | `PgReadinessProbe`（真实现） | ⑤（不可达库）/ ⑥ `--with-db`（`#[ignore]`） | `Ping` 路径、`mc_migrate::verify()` 接线、乱序漏记账 |
//!
//! 为什么状态机可以用替身驱动：那一层是**纯逻辑**（六个分支 + TTL + 单飞），与上游在同一个
//! 文件里定义 `readinessDB` 接口的动机逐字相同（见 `ready.rs` 的 `ReadinessProbe`）。生产实现
//! 只有一处（`PgReadinessProbe`，`mc_migrate::verify()` 的唯一调用点），替身**不构成第二份实现**。
//!
//! `AppState` 用真 URL、**不可达**地址的 `connect_lazy` 装（与 `mc-conformance/src/harness.rs`
//! 的 `STATELESS_URL`、`probes/live/tests.rs` 逐字同款）：本片 handler 不读它，一旦有人真让它
//! 查库就会拿到连接错误，而不是静默通过。

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::{
    router_with_probe, PgReadinessProbe, Readiness, ReadinessCache, ReadinessChecks,
    ReadinessProbe, ReadinessResponse, READINESS_CACHE_TTL,
};
use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};

/// 真 URL、**不可达**地址 —— `mc-conformance/src/harness.rs::STATELESS_URL` 的同款形状。
const UNREACHABLE_DB: &str = "postgres://ready-probe:ready-probe@127.0.0.1:1/ready_probe";

// --------------------------------------------------------------------------- #
// 替身与夹具
// --------------------------------------------------------------------------- #

/// 可编排的替身：上游 `readinessDB` 的测试侧对应物。
///
/// `delay` 是**单飞用例的关键**：没有它 `ping()` 会瞬间返回、并发请求之间不重叠，
/// 于是"只 `Ping` 一次"既可能来自缓存、也可能来自时序巧合 —— 两条机制就分不出来了。
struct FakeProbe {
    ping_calls: AtomicUsize,
    verify_calls: AtomicUsize,
    ping_fails: bool,
    readiness: Result<Readiness, String>,
    delay: Duration,
}

impl FakeProbe {
    /// 任意编排；`delay` 让单飞用例能造出重叠的并发窗口。
    fn new(ping_fails: bool, readiness: Result<Readiness, String>, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            ping_calls: AtomicUsize::new(0),
            verify_calls: AtomicUsize::new(0),
            ping_fails,
            readiness,
            delay,
        })
    }

    /// `Ping` 通 + 迁移齐（分支 6）。
    fn ready() -> Arc<Self> {
        Self::new(false, Ok(snapshot(&[], &[])), Duration::ZERO)
    }

    fn pings(&self) -> usize {
        self.ping_calls.load(Ordering::SeqCst)
    }

    fn verifies(&self) -> usize {
        self.verify_calls.load(Ordering::SeqCst)
    }
}

/// `mc_migrate::verify()` 的返回物（只填与本片判据相关的两个清单）。
fn snapshot(pending: &[&str], missing_tables: &[&str]) -> Readiness {
    Readiness {
        loaded: 3,
        applied: 3,
        pending: pending.iter().map(|s| (*s).to_owned()).collect(),
        missing_tables: missing_tables.iter().map(|s| (*s).to_owned()).collect(),
    }
}

impl ReadinessProbe for Arc<FakeProbe> {
    fn ping(&self) -> impl Future<Output = Result<(), String>> + Send {
        let this = Arc::clone(self);
        async move {
            this.ping_calls.fetch_add(1, Ordering::SeqCst);
            if !this.delay.is_zero() {
                tokio::time::sleep(this.delay).await;
            }
            if this.ping_fails {
                Err("connection refused".into())
            } else {
                Ok(())
            }
        }
    }

    fn verify(&self) -> impl Future<Output = Result<Readiness, String>> + Send {
        let this = Arc::clone(self);
        async move {
            this.verify_calls.fetch_add(1, Ordering::SeqCst);
            this.readiness.clone()
        }
    }
}

/// 装一个 `AppState`：库**不可达**，其余按 `mc-conformance` 的 stateless 层同款。
fn state() -> Arc<AppState> {
    let db = mc_db::Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy pool");
    let realtime = mc_realtime::RealtimeHandle::start(8);
    let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-2104"));
    Arc::new(AppState::new(
        db,
        RuntimeHandles {
            actors: mc_core::actor::ActorRegistry::new(),
            adapters: Arc::new(AdapterRegistry::default()),
        },
        ConfigSnapshot {
            host: "127.0.0.1".into(),
            port: 0,
            session_cookie: "multica_session".into(),
            api_key_header: "X-Multica-Api-Key".into(),
            csrf_header: "X-Multica-Csrf".into(),
            ..Default::default()
        },
        realtime,
        ws,
    ))
}

/// 替身驱动的探针面 router（与生产同一份 `router_with_probe`）。
fn probe_router(probe: Arc<FakeProbe>, ttl: Duration) -> Router {
    let state = state();
    router_with_probe(probe, ttl).with_state(state)
}

/// 生产装配的探针面 router（`probes/mod.rs` 的 `ready::router()`，库不可达）。
fn production_probe_router() -> Router {
    let state = state();
    crate::routes::probes::router(state.clone()).with_state(state)
}

/// 全量 router（与 `apps/mc-server/src/main.rs:175` 同款装配）—— 判据是"挂到了根路径"。
fn full_router() -> Router {
    let state = state();
    crate::routes::router(state.clone()).with_state(state)
}

/// `GET <uri>` ⇒ `(status, body-as-json)`。
async fn get_json(router: &Router, uri: &str) -> (StatusCode, Value) {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("dispatch");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

// --------------------------------------------------------------------------- #
// 1. 三态 + 200 的逐字契约
// --------------------------------------------------------------------------- #

/// 分支 6：`200` + `{"status":"ok","checks":{"db":"ok","migrations":"ok"}}`。
///
/// 整体 `Value` 相等（不是逐字段取值）⇒ 多一个键、少一个键都会红。
#[tokio::test]
async fn ready_paths_answer_200_with_the_three_ok_checks() {
    let router = probe_router(FakeProbe::ready(), READINESS_CACHE_TTL);
    for uri in ["/healthz", "/readyz"] {
        let (status, body) = get_json(&router, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(
            body,
            json!({"status": "ok", "checks": {"db": "ok", "migrations": "ok"}}),
            "{uri}"
        );
    }
}

/// 分支 2：`Ping` 失败 ⇒ **503** 且 `migrations` 是 **`unknown`**（不是 `error`）。
///
/// 上游逐字：`Ping` 都没过 ⇒ 根本没问记账表，报 `error` 就是撒谎。
#[tokio::test]
async fn not_ready_when_the_database_ping_fails() {
    let probe = FakeProbe::new(true, Ok(snapshot(&[], &[])), Duration::ZERO);
    let router = probe_router(probe, READINESS_CACHE_TTL);
    let (status, body) = get_json(&router, "/healthz").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({"status": "not_ready", "checks": {"db": "error", "migrations": "unknown"}})
    );
}

/// 🔴 分支 5 的**乱序补丁漏记账**（`docs/64` §6.5 的 M10-2 专属验收点，上游
/// `readinessQuery` 的逐字注释专防此病）。
///
/// 场景：清单里有 `200_beta`，记账表里**没有它** —— 但表里有版本号**更高**的 `300_gamma`。
/// 于是"已应用条数"与"清单条数"可以相等（替身里 `loaded == applied == 3`），
/// "看有没有最新一行"的实现会判 200 ⇒ 只有逐版本差集（`Migrator::pending`）才逮得到。
///
/// 本用例验的是**契约**（`pending` 非空 ⇒ `out_of_date` + 503）；`pending` 是否真能从真库
/// 算出来，由 `out_of_order_ledger_hole_is_detected_on_a_real_database`（⑥ 门）在真库上验。
#[tokio::test]
async fn not_ready_out_of_date_when_a_lower_numbered_patch_is_unrecorded() {
    let probe = FakeProbe::new(false, Ok(snapshot(&["200_beta"], &[])), Duration::ZERO);
    let router = probe_router(Arc::clone(&probe), READINESS_CACHE_TTL);
    let (status, body) = get_json(&router, "/readyz").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({"status": "not_ready", "checks": {"db": "ok", "migrations": "out_of_date"}}),
        "记账表里有更高的 300_gamma、缺 200_beta ⇒ out_of_date"
    );
    assert_eq!(probe.verifies(), 1, "迁移判定必须真的问过一次");
}

/// 分支 3/4：清单读不出（目录不存在、加载报错、记账查询报错）⇒ `migrations="error"`，
/// 而 `db` 保持 **`ok`**（库是通的，问题在迁移面）。
#[tokio::test]
async fn not_ready_error_when_the_migration_manifest_is_unreadable() {
    let probe = FakeProbe::new(
        false,
        Err("migrations directory not found: /nope".into()),
        Duration::ZERO,
    );
    let router = probe_router(probe, READINESS_CACHE_TTL);
    let (status, body) = get_json(&router, "/readyz").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({"status": "not_ready", "checks": {"db": "ok", "migrations": "error"}})
    );
}

/// 本地比上游**严**的那一项（`docs/32` §44 差异表第 3 条）：记账齐但关键表缺失 ⇒ 也判不齐。
///
/// `mc_migrate::verify()` 同时回答 `pending` 与 `missing_tables`，只取前者会把这个信息丢掉
/// —— 于是"迁移都记了账、但核心表一个都不在"这种库会被答成 200。
#[tokio::test]
async fn not_ready_out_of_date_when_required_tables_are_missing() {
    let probe = FakeProbe::new(false, Ok(snapshot(&[], &["issue"])), Duration::ZERO);
    let router = probe_router(probe, READINESS_CACHE_TTL);
    let (status, body) = get_json(&router, "/healthz").await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body,
        json!({"status": "not_ready", "checks": {"db": "ok", "migrations": "out_of_date"}})
    );
}

/// 边界：**空清单 + 空记账** ⇒ 200（有意与上游不同，登记在 §44 差异表第 2 条）。
///
/// 本地 `pending` 是"清单减记账"的差集，空清单没有可缺的版本 ⇒ 齐；而上游对
/// `len(required) == 0` 是**显式**报错。本地 `load_dirs` 对空目录返回 `Ok(0 个文件)` ——
/// 那不是"读不出"，把它当 `error` 会把一台只读部署误报成不健康。
#[tokio::test]
async fn an_empty_manifest_with_an_empty_ledger_is_ready() {
    let router = probe_router(FakeProbe::ready(), READINESS_CACHE_TTL);
    let (status, _) = get_json(&router, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

// --------------------------------------------------------------------------- #
// 2. 3 秒缓存 + 单飞（上游 `readinessCacheTTL`）
// --------------------------------------------------------------------------- #

/// 缓存命中：第二次请求**不再**碰库；且两条路径**共享**同一份缓存 —— 这就是
/// "同一个 handler 挂两个路径"的可观测证据（两份缓存会让交替探针把 `Ping` 频率翻倍）。
#[tokio::test]
async fn the_two_paths_share_one_cache() {
    let probe = FakeProbe::ready();
    let router = probe_router(Arc::clone(&probe), READINESS_CACHE_TTL);

    let (first, first_body) = get_json(&router, "/healthz").await;
    let (second, second_body) = get_json(&router, "/readyz").await;

    assert_eq!(first, StatusCode::OK);
    assert_eq!(second, StatusCode::OK);
    assert_eq!(first_body, second_body);
    assert_eq!(probe.pings(), 1, "3s TTL 内两条路径只 Ping 一次");
    assert_eq!(probe.verifies(), 1, "迁移判定同样只算一次");
}

/// **单飞**：8 个并发请求只 `Ping` 一次。
///
/// 替身让 `ping()` 睡 150ms ⇒ 8 个任务在 runtime 上重叠；没有 `refreshMu`、或少了持锁后
/// 那道 `load_cached` 检查的实现都会各自看到空缓存 ⇒ 8 次 `Ping`。默认 `#[tokio::test]` 是
/// **单线程** runtime ⇒ 任务只在 `await` 点交错，本用例因此是确定性的。
#[tokio::test]
async fn concurrent_requests_collapse_into_a_single_refresh() {
    let probe = FakeProbe::new(false, Ok(snapshot(&[], &[])), Duration::from_millis(150));
    let router = probe_router(Arc::clone(&probe), READINESS_CACHE_TTL);

    let mut handles = Vec::new();
    for _ in 0..8 {
        let router = router.clone();
        handles.push(tokio::spawn(
            async move { get_json(&router, "/readyz").await.0 },
        ));
    }
    for handle in handles {
        assert_eq!(handle.await.expect("task"), StatusCode::OK);
    }

    assert_eq!(probe.pings(), 1, "8 个并发请求必须只 Ping 一次（单飞）");
    assert_eq!(probe.verifies(), 1);
}

/// TTL 过期后**重新**计算（上游 `now.Before(cached.expiresAt)` 的时间边界）。
#[tokio::test]
async fn an_expired_entry_is_recomputed() {
    let probe = FakeProbe::ready();
    let router = probe_router(Arc::clone(&probe), Duration::from_millis(50));

    let (first, _) = get_json(&router, "/healthz").await;
    assert_eq!(first, StatusCode::OK);
    assert_eq!(probe.pings(), 1);

    tokio::time::sleep(Duration::from_millis(120)).await;

    let (second, _) = get_json(&router, "/healthz").await;
    assert_eq!(second, StatusCode::OK);
    assert_eq!(probe.pings(), 2, "TTL 过期后必须重算");
}

/// `TTL <= 0` ⇒ 直算不缓存（上游 `if h.cacheTTL <= 0 { return h.computeReadiness() }`）。
///
/// 生产用不到这一档（TTL 是常量 3s），但它是上游**显式**写出来的：少了它，"把 TTL 配成 0"
/// 会退化成"永远返回第一次的陈旧结论"。
#[tokio::test]
async fn a_zero_ttl_never_caches() {
    let probe = FakeProbe::ready();
    let router = probe_router(Arc::clone(&probe), Duration::ZERO);

    let (first, _) = get_json(&router, "/healthz").await;
    let (second, _) = get_json(&router, "/readyz").await;

    assert_eq!(first, StatusCode::OK);
    assert_eq!(second, StatusCode::OK);
    assert_eq!(probe.pings(), 2, "TTL=0 ⇒ 每次请求都真算");
}

/// 不健康结论**也会**被缓存（上游 `cachedReadiness` 同时存 `response` 与 `statusCode`）。
///
/// 这是防"库挂了 ⇒ 每个探针请求都去撞 5s acquire 超时"的那一条：503 也要 TTL。
#[tokio::test]
async fn the_not_ready_verdict_is_cached_too() {
    let probe = FakeProbe::new(true, Ok(snapshot(&[], &[])), Duration::ZERO);
    let router = probe_router(Arc::clone(&probe), READINESS_CACHE_TTL);

    let (first, body) = get_json(&router, "/healthz").await;
    assert_eq!(first, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["checks"]["db"], json!("error"));

    let (second, _) = get_json(&router, "/readyz").await;
    assert_eq!(second, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(probe.pings(), 1, "503 同样进缓存（否则故障期会把库探爆）");
}

// --------------------------------------------------------------------------- #
// 3. 键集 / 序列化契约（不经过 HTTP）
// --------------------------------------------------------------------------- #

/// 上游 `readinessResponse` 只有两个键、`readinessChecks` 只有两个键，且四个键**都没有**
/// `omitempty` ⇒ 无论哪个分支，这三个值键**必现**。
///
/// 不依赖进程状态、也不依赖 HTTP：谁加了字段、谁把 `&'static str` 换成 `Option<String>`
/// （想让某个键消失），这里立刻红。
#[test]
fn readiness_response_serializes_to_exactly_the_upstream_keys() {
    let cases = [
        (
            response("ok", "ok", "ok"),
            json!({"status": "ok", "checks": {"db": "ok", "migrations": "ok"}}),
        ),
        (
            response("not_ready", "error", "unknown"),
            json!({"status": "not_ready", "checks": {"db": "error", "migrations": "unknown"}}),
        ),
        (
            response("not_ready", "ok", "error"),
            json!({"status": "not_ready", "checks": {"db": "ok", "migrations": "error"}}),
        ),
        (
            response("not_ready", "ok", "out_of_date"),
            json!({"status": "not_ready", "checks": {"db": "ok", "migrations": "out_of_date"}}),
        ),
    ];
    for (response, expected) in cases {
        assert_eq!(
            serde_json::to_value(&response).expect("serialize"),
            expected
        );
    }
}

/// `ready.rs` 模块头那张表的四个取值组合。
fn response(status: &'static str, db: &'static str, migrations: &'static str) -> ReadinessResponse {
    ReadinessResponse {
        status,
        checks: ReadinessChecks { db, migrations },
    }
}

// --------------------------------------------------------------------------- #
// 4. 形态门：只注册**无尾斜杠**那一形态（`docs/64` §1.4）
// --------------------------------------------------------------------------- #

/// 上游 `router.go:1400/1401` 是 plain `r.Get("/healthz", …)` / `r.Get("/readyz", …)`
/// ⇒ chi 只服务一种形态。多注册尾斜杠形态就是 `EXTRA_ALIAS` 硬失败
/// （本波 `slash-alias-allowlist.tsv` 是 0 数据行、没有豁免退路）。
/// 这条是静态判据（`scripts/slash_alias_audit.py`）的**运行时对照**。
#[tokio::test]
async fn only_the_plain_forms_are_served() {
    let router = probe_router(FakeProbe::ready(), READINESS_CACHE_TTL);
    for plain in ["/healthz", "/readyz"] {
        let (status, _) = get_json(&router, plain).await;
        assert_eq!(status, StatusCode::OK, "{plain} 必须服务");
    }
    for slashed in ["/healthz/", "/readyz/"] {
        let (status, _) = get_json(&router, slashed).await;
        assert_ne!(status, StatusCode::OK, "{slashed} 不得被本切片服务");
    }
}

// --------------------------------------------------------------------------- #
// 5. 生产接线层：真 `PgReadinessProbe` + 真 `probes::router`
// --------------------------------------------------------------------------- #

/// 生产装配下这两条路径**真的挂在根路径上**（不是 404），且在库不可达时报 503 ——
/// 单条判据同时钉住"`probes/mod.rs` 的接线"与"生产探针走 `Ping` 路径"。
#[tokio::test]
async fn the_production_router_serves_both_paths_and_reports_db_error() {
    for router in [production_probe_router(), full_router()] {
        for uri in ["/healthz", "/readyz"] {
            let (status, body) = get_json(&router, uri).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            assert_eq!(
                body,
                json!({"status": "not_ready", "checks": {"db": "error", "migrations": "unknown"}}),
                "{uri}"
            );
        }
    }
}

/// 真 `PgReadinessProbe` 的 `Ping` 路径：不可达库 ⇒ 503 `db=error`，**且不依赖池的
/// 5s `acquire_timeout`**（本片的 2s 上限，`health.go` L139 逐字）。
///
/// 断言用 `< 5s`：`127.0.0.1:1` 通常立刻拒连（2s 是**上限**不是耗时），要防的是
/// "没有上限、于是要等池的 5s"这一形态。
#[tokio::test]
async fn the_production_probe_reports_db_error_without_waiting_for_the_pool_timeout() {
    assert!(
        std::env::var("MULTICA_TEST_DATABASE_URL").map_or(true, |v| v.trim().is_empty()),
        "本用例的前提是**没有**可用测试库；请在不带 MULTICA_TEST_DATABASE_URL 的环境里跑（门 ⑤ 正是如此）"
    );

    let db = mc_db::Db::connect_lazy(UNREACHABLE_DB, 1, 0).expect("lazy pool");
    let probe = PgReadinessProbe::new(db, vec![std::path::PathBuf::from("migrations")]);
    assert!(probe.ping().await.is_err(), "不可达库的 Ping 必须失败");

    let started = std::time::Instant::now();
    let (response, status) = ReadinessCache::new(probe, READINESS_CACHE_TTL)
        .readiness()
        .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.status, "not_ready");
    assert_eq!(response.checks.db, "error");
    assert_eq!(response.checks.migrations, "unknown");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "不得落在池的 5s acquire 超时上，实测 {:?}",
        started.elapsed()
    );
}

// --------------------------------------------------------------------------- #
// 6. 真库层（⑥ 门 `--ignored`；`MULTICA_TEST_DATABASE_URL`）
// --------------------------------------------------------------------------- #

/// gate ⑥（`--ignored`）用 `MULTICA_TEST_DATABASE_URL`；本地手跑可用 `DATABASE_URL`。
fn require_test_db() -> Option<String> {
    let url = std::env::var("MULTICA_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|u| !u.trim().is_empty());
    if url.is_none() {
        eprintln!("MULTICA_TEST_DATABASE_URL/DATABASE_URL not set; skipping ready e2e tests");
    }
    url
}

/// 本地仓库的迁移根目录（与 `apps/mc-server/src/main.rs:99-102` 的缺省一致）。
fn repo_migrations_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("migrations")
}

/// 真库 + **真的**迁移树 ⇒ 200（`mc_migrate::verify()` 的接线在真库上成立）。
/// 前提：gate ⑥ 先跑过 `mc-migrate run --dir migrations`（`scripts/gates.sh` 的 ⑥ 正是如此）。
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn a_real_database_with_the_real_migration_tree_is_ready() {
    let Some(url) = require_test_db() else {
        return;
    };
    let db = mc_db::Db::connect(&url, 2, 0).await.expect("db");
    let probe = PgReadinessProbe::new(db, vec![repo_migrations_dir()]);
    let (response, status) = ReadinessCache::new(probe, READINESS_CACHE_TTL)
        .readiness()
        .await;

    assert_eq!(
        status,
        StatusCode::OK,
        "真库 + 真迁移树必须就绪；实际 {response:?}"
    );
    assert_eq!(response.status, "ok");
    assert_eq!(response.checks.db, "ok");
    assert_eq!(response.checks.migrations, "ok");
}

/// 库是通的、**清单读不出** ⇒ 503 `db=ok` / `migrations=error`（上游 `initErr` 分支的本地
/// 对应物）。这一条**必须**有真库才验得了：没有真库时 `Ping` 先失败，落进的是 `db=error` 那条分支。
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn a_reachable_database_with_an_unreadable_manifest_reports_migrations_error() {
    let Some(url) = require_test_db() else {
        return;
    };
    let db = mc_db::Db::connect(&url, 2, 0).await.expect("db");
    let probe = PgReadinessProbe::new(
        db,
        vec![std::path::PathBuf::from("/nonexistent-migrations-lum2104")],
    );
    let (response, status) = ReadinessCache::new(probe, READINESS_CACHE_TTL)
        .readiness()
        .await;

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.status, "not_ready");
    assert_eq!(response.checks.db, "ok", "库是通的 ⇒ db 保持 ok");
    assert_eq!(response.checks.migrations, "error");
}

/// 🔴 **乱序补丁漏记账**的真库版（本片最不可替代的一条）。三相共用一份隔离 schema：
///
/// | 相 | 记账表 | 清单 | 期望 | 打死哪种实现 |
/// | :-: | --- | --- | :-: | --- |
/// | 1 | `100_alpha`, `300_gamma` | `100`,`**200**`,`300` | 503 `out_of_date` | "只看有没有最新一行"（`300` 在、`200` 缺） |
/// | 2 | 同上 | `100`, `300` | **200** | 对照：证明第 1 相的 503 只来自清单差集 |
/// | 3 | `100`, `200`, `999_extra` | `100`,`200`,`300` | 503 `out_of_date` | "无 `WHERE version = ANY($1)` 的 `COUNT(*)`"（行数 3 == 清单 3） |
///
/// 第 3 相正是本片 `DoD` 里"**禁止**在路由里另写一份 `SELECT COUNT(*)`"禁的那个形态。
///
/// 用独立 schema 而不是改共享库：`schema_migrations` 与那 14 张关键表都建在 schema 内
/// （零列表即满足 `to_regclass` ⇒ `missing_tables` 为空，判据只剩 `pending`），
/// 因此这个 schema 的形状与一张刚迁移完的库等价，而**共享库一字节不动**。
/// 连接用 URL 的 `options=-csearch_path=…` 落到该 schema。
#[ignore = "needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL"]
#[tokio::test]
async fn out_of_order_ledger_hole_is_detected_on_a_real_database() {
    let Some(url) = require_test_db() else {
        return;
    };
    let schema = format!("probe_lum2104_{}", std::process::id());
    let (admin, scoped) = scratch_schema(&url, &schema).await;

    // 记账表：有更高的 300_gamma，缺 200_beta（乱序补丁漏记账的形状）。
    seed_ledger(&admin, &schema, "('100_alpha'), ('300_gamma')").await;

    let with_hole = TempMigrations::new(&["100_alpha", "200_beta", "300_gamma"]);
    let without_hole = TempMigrations::new(&["100_alpha", "300_gamma"]);
    let manifest = with_hole.path();

    let (hole, hole_status) = verdict(scoped.clone(), manifest.clone()).await;
    assert_eq!(
        hole_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "清单里有 200_beta、记账表没有 ⇒ 必须 503；实际 {hole:?}"
    );
    assert_eq!(hole.checks.migrations, "out_of_date");
    assert_eq!(hole.checks.db, "ok");

    // 第 2 相（对照）：同一份库、同一份 schema，只把清单里那条漏记账的版本去掉。
    let (closed, closed_status) = verdict(scoped.clone(), without_hole.path()).await;
    assert_eq!(
        closed_status,
        StatusCode::OK,
        "同一库、同一 schema，清单与记账表一致 ⇒ 200（否则第 1 相的 503 不是这条差异造成的）；实际 {closed:?}"
    );

    // 第 3 相：行数相等但**记的不是清单上那条** ⇒ 仍然必须 503。
    seed_ledger(
        &admin,
        &schema,
        "('100_alpha'), ('200_beta'), ('999_extra')",
    )
    .await;
    let (decoy, decoy_status) = verdict(scoped, manifest).await;
    assert_eq!(
        decoy_status,
        StatusCode::SERVICE_UNAVAILABLE,
        "记账表 3 行、清单 3 条，但 300_gamma 没记账 ⇒ 只数行数的实现会错答 200；实际 {decoy:?}"
    );
    assert_eq!(decoy.checks.migrations, "out_of_date");

    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .expect("drop schema");
}

/// 造一份隔离 schema（14 张零列关键表 + 记账表），返回 `(admin 连接, 落到该 schema 的连接)`。
async fn scratch_schema(url: &str, schema: &str) -> (mc_db::Db, mc_db::Db) {
    let admin = mc_db::Db::connect(url, 2, 0).await.expect("admin db");
    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
        .execute(admin.pool())
        .await
        .expect("drop leftover schema");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(admin.pool())
        .await
        .expect("create schema");
    sqlx::query(&format!(
        "CREATE TABLE {schema}.schema_migrations ( \
             version TEXT PRIMARY KEY, applied_at TIMESTAMPTZ NOT NULL DEFAULT now())"
    ))
    .execute(admin.pool())
    .await
    .expect("create ledger");
    for table in mc_migrate::DEFAULT_REQUIRED_TABLES {
        sqlx::query(&format!("CREATE TABLE {schema}.\"{table}\" ()"))
            .execute(admin.pool())
            .await
            .unwrap_or_else(|e| panic!("create {table}: {e}"));
    }
    let scoped = mc_db::Db::connect(&with_search_path(url, schema), 2, 0)
        .await
        .expect("scoped db");
    (admin, scoped)
}

/// 重写记账表的全部行（三相之间换形状）。
async fn seed_ledger(admin: &mc_db::Db, schema: &str, values: &str) {
    sqlx::query(&format!("DELETE FROM {schema}.schema_migrations"))
        .execute(admin.pool())
        .await
        .expect("clear ledger");
    sqlx::query(&format!(
        "INSERT INTO {schema}.schema_migrations (version) VALUES {values}"
    ))
    .execute(admin.pool())
    .await
    .expect("seed ledger");
}

/// 一次就绪判定（生产探针 + 指定清单）。
async fn verdict(db: mc_db::Db, manifest: std::path::PathBuf) -> (ReadinessResponse, StatusCode) {
    ReadinessCache::new(
        PgReadinessProbe::new(db, vec![manifest]),
        READINESS_CACHE_TTL,
    )
    .readiness()
    .await
}

/// 把 `search_path` 只指到隔离 schema 的连接 URL（sqlx 支持 URL 的 `options` 参数）。
fn with_search_path(url: &str, schema: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}options=-csearch_path%3D{schema}")
}

/// 一组合成迁移文件（`<digits>_<name>.up.sql`），`Drop` 时整目录删除。
struct TempMigrations {
    dir: std::path::PathBuf,
}

impl TempMigrations {
    fn new(versions: &[&str]) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "lum2104-migrations-{}-{}",
            std::process::id(),
            versions.join("-").replace('_', "x")
        ));
        std::fs::create_dir_all(&dir).expect("temp migrations dir");
        for version in versions {
            std::fs::write(dir.join(format!("{version}.up.sql")), "SELECT 1;\n")
                .expect("write synthetic migration");
        }
        Self { dir }
    }

    fn path(&self) -> std::path::PathBuf {
        self.dir.clone()
    }
}

impl Drop for TempMigrations {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
