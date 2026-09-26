//! `GET /healthz` + `GET /readyz` —— readiness 探针（**M10-2 原地填充**，`docs/64` §2.1 / §4.1 第 3 行）。
//!
//! 上游：`server/cmd/server/health.go::readyHandler` + `readiness()`（`f41fae6b08fb` L93-182）——
//! `db.Ping` + **所有** up 版本都已记入 `schema_migrations` ⇒ 200 或 **503**；
//! body `{status, checks:{db, migrations}}`，`migrations ∈ {ok, error, out_of_date, unknown}`。
//! **同一个 handler 挂两个路径**（不是两份实现），并复刻上游的 **3 秒缓存 + 单飞**。
//!
//! ```go
//! // health.go L21 —— 判据不是"有最新一行"，而是"每个必需版本都有记账"：
//! // readinessQuery counts how many of the binary's required migration versions
//! // are recorded as applied. We compare the count to the number of required
//! // versions rather than checking a single "latest" row, so a missing
//! // out-of-order migration (numbered below an already-applied later one) is
//! // detected instead of being masked by the later version's presence.
//! const readinessQuery = `SELECT COUNT(*) FROM schema_migrations WHERE version = ANY($1)`
//!
//! const readinessCacheTTL = 3 * time.Second
//! ```
//!
//! （上面这段 Go 在文档注释里用**空格**缩进而不是制表符：`clippy::tabs_in_doc_comments`
//! 是 pedantic 档、门 ③ 把它当错误 —— 与代码语义无关，只是文档排版；先例 `probes/live.rs`。）
//!
//! ## 语义：`/health` ≠ `/healthz` = `/readyz`（`docs/64` §2.1 的三分法）
//!
//! `/health`（liveness，`probes/live.rs`，M10-1）**不触库**；本文件回答 readiness：
//! 库能不能 `Ping` 通 + 这个二进制需要的迁移版本是不是**全部**都记了账。
//! `/healthz` 与 `/readyz` 是**同一个 handler**（上游 `router.go:1400/1401` 两行注册的是
//! 逐字同一个 `health.readyHandler`）⇒ 本文件只用**一份**闭包、**一份**缓存挂两个路径。
//! 这不是"为了少写几行"：两份缓存会让两条路径各自过期，运维交替探它们时 `Ping` 频率翻倍。
//!
//! ## 三态 + 200（上游 `computeReadiness` 逐分支）
//!
//! | # | 上游分支 | 状态码 | `status` | `checks.db` | `checks.migrations` |
//! | :-: | --- | :-: | --- | --- | --- |
//! | 1 | `h.db == nil` | 503 | `not_ready` | `error` | `unknown` |
//! | 2 | `Ping` 失败 | 503 | `not_ready` | `error` | `unknown` |
//! | 3 | `initErr != nil\|\| len(required)==0` | 503 | `not_ready` | `ok` | `error` |
//! | 4 | 记账查询失败 | 503 | `not_ready` | `ok` | `error` |
//! | 5 | 记账数 < 必需数 | 503 | `not_ready` | `ok` | `out_of_date` |
//! | 6 | 其余 | **200** | `ok` | `ok` | `ok` |
//!
//! ⚠️ 分支 2 的 `migrations` 是 **`unknown`**（不是 `error`）：`Ping` 都没过，本片**根本没问**
//! 记账表 ⇒ 报 `error` 就是撒谎（上游逐字如此）。分支 3/4/5 的 `db` 保持 **`ok`** ——
//! 库是通的，问题在迁移面。这三条在 `ready/tests.rs` 里逐条钉住。
//!
//! ## 复用 `mc_migrate::verify()`（**禁止**在这里另写一份 `SELECT COUNT(*)`）
//!
//! `mc_migrate::verify(db, dirs)`（`crates/mc-migrate/src/lib.rs:127`）返回
//! `Readiness{loaded, applied, pending, missing_tables}`，其中 `pending` 是
//! `Migrator::pending()` 的**逐版本**差集 —— 正是上游 `readinessQuery` 要防的那个病
//! （"编号低于已应用版本的乱序补丁漏记账"，`docs/64` §6.5 的 M10-2 专属验收点）。
//! 本文件**不**新增任何 SQL：`Ping` 走既有的 `mc_db::health::check`，迁移面走 `verify()`。
//!
//! ### 与上游的两条**已知差异**（登记在 `docs/32-M3-DAEMON-FACE.md` §44）
//!
//! | # | 上游 | 本地 | 影响 |
//! | :-: | --- | --- | --- |
//! | 1 | `h.db == nil` ⇒ 503 `db=error/migrations=unknown`（分支 1） | **无对应分支**：`AppState.db` 不是 `Option`，装配期就没有"没有库"这个状态 | 不可达的库走分支 2（同状态码、同 body）⇒ 可判定性质不变 |
//! | 2 | `requiredMigrations` 由 `migrations.AllVersions()` 在 `newServerHealth()` **编译期嵌入**一次 | `MULTICA_MIGRATIONS_DIR`（缺省 `migrations`，与 `apps/mc-server/src/main.rs:99-102` **同一个** env/缺省）在 [`router`] 装配时解析一次 | 上游是编译期面、本地是运行时面；判据（"清单读不到 ⇒ `error`"）两侧都有，只是发现的时刻不同 |
//! | 3 | `missing_tables` 不参与判定 | **参与**：`Readiness::is_ready()` 的两项（`pending` 与 `missing_tables`）都算不齐 ⇒ `out_of_date` | 本地比上游**严**：记账齐但核心表缺失也判 503。`verify()` 本来就同时回答这两个问题，只取一项会把这个信息丢掉（登记见 §44） |
//! | 4 | `writeJSON` 在 body 末尾补 `'\n'` 并显式写 `Content-Length` | axum `Json`（无尾换行，length 自动） | `Content-Type: application/json` 一致；⑨ 判决是 `json_subset`，本路径**无 fixture** ⇒ 不受影响 |
//!
//! ## 形态：只注册**无尾斜杠**那一形态
//!
//! 上游 `router.go:1400/1401` 是 plain `r.Get("/healthz", …)` / `r.Get("/readyz", …)`
//! （`docs/64` §1.4 实测 `dual-form required: 0`）⇒ chi 只服务一种形态。多注册 `/healthz/`
//! 就是 `EXTRA_ALIAS` 硬失败，本波 `docs/fixtures/slash-alias-allowlist.tsv` 是 **0 数据行**、
//! 没有豁免退路。
//!
//! ## 线程/进程模型：缓存放在 `router()` 里，不进 `AppState`
//!
//! 上游把 `cache`/`refreshMu` 挂在 `serverHealth` 实例上（`main` 里建一次 ⇒ **进程级**）。
//! 本仓的对应物就是 [`router`] 被调用的次数 —— 生产只有 `apps/mc-server/src/main.rs:175`
//! 那一次装机 ⇒ 闭包里的 `Arc<ReadinessCache<_>>` 等价于上游实例字段。
//! **不得**为此改 `crate::state::AppState`（M10-0 anchor 冻结的共享文件，不在本片写集）。

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use mc_migrate::Readiness;
use serde::Serialize;
use tokio::sync::Mutex as AsyncMutex;

use crate::state::AppState;

/// 上游 `readinessCacheTTL = 3 * time.Second`（`health.go` L23 逐字）。
pub(crate) const READINESS_CACHE_TTL: Duration = Duration::from_secs(3);

/// 上游 `context.WithTimeout(parent, 2*time.Second)`（`health.go` L139 逐字）。
///
/// 本仓的池 `acquire_timeout` 是 5s（`crates/mc-db/src/pool.rs:40`）⇒ 没有这一层超时，
/// "库挂了"要 5s 才回答，而上游是 2s。运维探针的等待预算由它决定，故逐字复刻。
pub(crate) const READINESS_PING_TIMEOUT: Duration = Duration::from_secs(2);

/// `status` 的取值（上游 `readinessResponse.Status`）。
const STATUS_OK: &str = "ok";
const STATUS_NOT_READY: &str = "not_ready";

/// `checks.db` 的取值。
const DB_OK: &str = "ok";
const DB_ERROR: &str = "error";

/// `checks.migrations` 的四个取值（上游 `readinessChecks.Migrations` 的全部取值域）。
const MIGRATIONS_OK: &str = "ok";
const MIGRATIONS_ERROR: &str = "error";
const MIGRATIONS_UNKNOWN: &str = "unknown";
const MIGRATIONS_OUT_OF_DATE: &str = "out_of_date";

/// 响应体 —— 上游 `readinessResponse`（`health.go` L64-70）的逐字段对应物。
///
/// 四个键**都没有** `omitempty`（上游逐字）⇒ 无论哪个分支，`status` / `checks.db` /
/// `checks.migrations` 三个键**必现**。`ready/tests.rs` 有一条不经过 HTTP 的纯序列化用例
/// 把"键集逐字"钉住（谁加了字段、谁删了键都会红）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReadinessResponse {
    pub(crate) status: &'static str,
    pub(crate) checks: ReadinessChecks,
}

/// 上游 `readinessChecks`（`health.go` L72-75）的逐字段对应物。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReadinessChecks {
    pub(crate) db: &'static str,
    pub(crate) migrations: &'static str,
}

impl ReadinessResponse {
    /// 分支 6 的初始值（上游 `computeReadiness` 先把两个 check 都置 `ok` 再逐项降级）。
    fn ready() -> Self {
        Self {
            status: STATUS_OK,
            checks: ReadinessChecks {
                db: DB_OK,
                migrations: MIGRATIONS_OK,
            },
        }
    }
}

/// 就绪判定的两个原语 —— 上游 `readinessDB` 接口（`health.go` L25-28 的
/// `Ping` + `QueryRow`）的等价物。
///
/// 为什么要有这个缝：就绪**状态机**（三态 + 缓存 + 单飞）是可判定逻辑，不该靠真库才能验；
/// 上游为此在同文件里定义了 `readinessDB` 接口（它的测试替身同理）。生产实现
/// [`PgReadinessProbe`] 是 `mc_migrate::verify()` 的**唯一**调用点 —— 替身只出现在
/// `ready/tests.rs` 里，不构成第二份实现。
pub(crate) trait ReadinessProbe: Send + Sync + 'static {
    /// 上游 `readinessDB.Ping(ctx)`：库能不能应答。
    fn ping(&self) -> impl Future<Output = Result<(), String>> + Send;

    /// 上游 `readinessQuery`：这个二进制需要的迁移版本是否**全部**已记账。
    fn verify(&self) -> impl Future<Output = Result<Readiness, String>> + Send;
}

/// 生产实现：`Ping` 走既有的 `mc_db::health`，迁移面走 `mc_migrate::verify()`。
pub(crate) struct PgReadinessProbe {
    db: mc_db::Db,
    /// 迁移目录（[`migration_dirs`] 解析一次；上游是编译期嵌入的版本清单）。
    dirs: Vec<PathBuf>,
    ping_timeout: Duration,
}

impl PgReadinessProbe {
    pub(crate) fn new(db: mc_db::Db, dirs: Vec<PathBuf>) -> Self {
        Self {
            db,
            dirs,
            ping_timeout: READINESS_PING_TIMEOUT,
        }
    }
}

impl ReadinessProbe for PgReadinessProbe {
    fn ping(&self) -> impl Future<Output = Result<(), String>> + Send {
        let db = self.db.clone();
        let budget = self.ping_timeout;
        async move {
            // 复用 `mc_db::health::check`（既有实现，`SELECT 1`）⇒ 本文件不新增 SQL。
            match tokio::time::timeout(budget, mc_db::health::check(&db)).await {
                Ok(check) if check.is_healthy() => Ok(()),
                Ok(check) => Err(check
                    .message
                    .unwrap_or_else(|| format!("database not healthy: {:?}", check.status))),
                Err(_) => Err(format!("database ping timed out after {budget:?}")),
            }
        }
    }

    fn verify(&self) -> impl Future<Output = Result<Readiness, String>> + Send {
        let db = self.db.clone();
        let dirs = self.dirs.clone();
        async move {
            // 唯一实现：`mc_migrate::verify()`（`pending` 逐版本差集 + 关键表存在性）。
            mc_migrate::verify(&db, dirs)
                .await
                .map_err(|e| format!("{e:#}"))
        }
    }
}

/// 缓存条目 —— 上游 `cachedReadiness`（`health.go` L47-51）。
#[derive(Clone)]
struct CachedReadiness {
    response: ReadinessResponse,
    status: StatusCode,
    expires_at: Instant,
}

/// 3 秒 TTL + 单飞的就绪缓存 —— 上游 `(*serverHealth).readiness` 的逐行等价物。
pub(crate) struct ReadinessCache<P> {
    probe: P,
    ttl: Duration,
    /// 上游 `refreshMu sync.Mutex`：只让**一个**请求真去算，其余等它算完再读缓存（单飞）。
    refresh: AsyncMutex<()>,
    /// 上游 `cache atomic.Pointer[cachedReadiness]`；本地用 `Mutex<Option<_>>`。
    cache: Mutex<Option<CachedReadiness>>,
}

impl<P: ReadinessProbe> ReadinessCache<P> {
    pub(crate) fn new(probe: P, ttl: Duration) -> Self {
        Self {
            probe,
            ttl,
            refresh: AsyncMutex::new(()),
            cache: Mutex::new(None),
        }
    }

    /// 上游 `readiness(ctx)`：`TTL<=0` 直算；命中缓存即返回；未命中则持 `refreshMu` 复算。
    ///
    /// 🔴 第二道 `load_cached` **不是**冗余：等锁期间先到的那个请求已经把缓存写好了
    /// （这正是单飞的实现方式）⇒ 少了它，"并发 N 个请求"会变成 N 次 `Ping`。
    pub(crate) async fn readiness(&self) -> (ReadinessResponse, StatusCode) {
        if self.ttl.is_zero() {
            return self.compute().await;
        }

        let now = Instant::now();
        if let Some(cached) = self.load_cached(now) {
            return (cached.response, cached.status);
        }

        let _single_flight = self.refresh.lock().await;

        let now = Instant::now();
        if let Some(cached) = self.load_cached(now) {
            return (cached.response, cached.status);
        }

        let (response, status) = self.compute().await;
        // 上游把 `expiresAt` 记成**持锁后、计算前**的 `now`（不是算完的时刻）⇒ 一次慢查询
        // 不会把 TTL 顺延。逐字复刻（`health.go` L113 的 `now.Add(h.cacheTTL)`）。
        let entry = CachedReadiness {
            response: response.clone(),
            status,
            expires_at: now + self.ttl,
        };
        *self.cache.lock().expect("readiness cache poisoned") = Some(entry);
        (response, status)
    }

    /// 上游 `loadCachedReadiness`：命中且**未过期**才返回。
    fn load_cached(&self, now: Instant) -> Option<CachedReadiness> {
        let guard = self.cache.lock().expect("readiness cache poisoned");
        match guard.as_ref() {
            Some(cached) if now < cached.expires_at => Some(cached.clone()),
            _ => None,
        }
    }

    /// 上游 `computeReadiness`：六个分支逐条对应（见模块头的表）。
    ///
    /// 上游分支 1（`h.db == nil`）在本地**没有对应物**（`AppState.db` 不是 `Option`）——
    /// 已登记为已知差异第 1 条；它的可判定射程由不可达库（分支 2）覆盖，状态码与 body 相同。
    async fn compute(&self) -> (ReadinessResponse, StatusCode) {
        let mut response = ReadinessResponse::ready();

        if self.probe.ping().await.is_err() {
            response.status = STATUS_NOT_READY;
            response.checks.db = DB_ERROR;
            // ⚠️ `unknown` 而不是 `error`：Ping 没过 ⇒ 根本没问记账表（上游逐字）。
            response.checks.migrations = MIGRATIONS_UNKNOWN;
            return (response, StatusCode::SERVICE_UNAVAILABLE);
        }

        match self.probe.verify().await {
            // 上游分支 3（清单读不出/为空）与分支 4（查询失败）在本地合并：`verify()` 的 `Err`
            // 同时覆盖"目录读不到""加载报错""记账查询报错"——三者上游都是 `migrations="error"`。
            Err(_) => {
                response.status = STATUS_NOT_READY;
                response.checks.migrations = MIGRATIONS_ERROR;
                (response, StatusCode::SERVICE_UNAVAILABLE)
            }
            // 上游分支 5：登记项与 `missing_tables` 任一非空 ⇒ 不齐（本地比上游严，见差异表第 3 条）。
            Ok(readiness)
                if !readiness.pending.is_empty() || !readiness.missing_tables.is_empty() =>
            {
                response.status = STATUS_NOT_READY;
                response.checks.migrations = MIGRATIONS_OUT_OF_DATE;
                (response, StatusCode::SERVICE_UNAVAILABLE)
            }
            // 上游分支 6。
            Ok(_) => (response, StatusCode::OK),
        }
    }
}

/// 迁移目录 —— 与 `apps/mc-server/src/main.rs:99-102` **同一个** env 与同一个缺省值。
///
/// 只给**根目录**：`Migrator::load_dirs` 会递归（运行时集合 = `migrations/upstream/`
/// 562 个上游文件 + `migrations/compat/` 本仓补丁，两处都在 `migrations/` 之下）。
fn migration_dirs() -> Vec<PathBuf> {
    let root = std::env::var("MULTICA_MIGRATIONS_DIR")
        .map_or_else(|_| PathBuf::from("migrations"), PathBuf::from);
    vec![root]
}

/// 就绪切片 —— **同一个 handler 挂两个路径**（上游 `router.go:1400/1401`）。
///
/// `probes/mod.rs`（M10-0 冻结）已经把 `pub mod ready;` 与 `ready::router()` 接好 ⇒
/// 本片**不碰**那个文件；这里只把 anchor 期的空 `Router::new()` 换成两条注册。
/// 两条都只注册**无尾斜杠**形态（模块头的「形态」节）。
// `needless_pass_by_value`：签名由 M10-0 anchor（`probes/mod.rs` 的 `ready::router(state.clone())`）
// 冻结 —— 四个子 router 形状一致，同 `routes/mount.rs:24` 的豁免理由。
#[allow(clippy::needless_pass_by_value)]
pub fn router(state: Arc<AppState>) -> Router<Arc<AppState>> {
    router_with_probe(
        PgReadinessProbe::new(state.db.clone(), migration_dirs()),
        READINESS_CACHE_TTL,
    )
}

/// [`router`] 的通用形式：把"用哪个探针、TTL 多长"交出来，好让 `ready/tests.rs` 用替身
/// 驱动**同一份**状态机（含缓存与单飞）。
///
/// 生产调用点只有 [`router`] 一处 ⇒ 两条路径共享同一个 `Arc<ReadinessCache<_>>`：
/// `/healthz` 与 `/readyz` 互相续期，交替探它们不会把 `Ping` 频率翻倍。
fn router_with_probe<P: ReadinessProbe>(probe: P, ttl: Duration) -> Router<Arc<AppState>> {
    let cache = Arc::new(ReadinessCache::new(probe, ttl));
    let handler = move || {
        let cache = Arc::clone(&cache);
        async move {
            let (response, status) = cache.readiness().await;
            (status, Json(response))
        }
    };
    Router::new()
        .route("/healthz", get(handler.clone()))
        .route("/readyz", get(handler))
}

#[cfg(test)]
mod tests;
