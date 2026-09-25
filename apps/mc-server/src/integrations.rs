//! M8 anchor（`LUM-1797`）建桩、**M8-5（`LUM-1802`）原地填充**：GitHub PR 快照刷新的
//! 后台宿主 —— worker 池 + TTL sweeper + 停机句柄 + **存储端口的实现**。
//!
//! # 这一片解决什么问题
//!
//! `mc_vcs_github::ghsnapshot::Manager` 是**长期后台 worker**（worker 池 + TTL sweeper +
//! 限流暂停 + 单地址串行），上游 `handler.go:513-519` 在 `NewHandler` 里造它、由
//! `cmd/server/main.go` 调 `h.PRRefresh.Start(ctx)`（`docs/61` §2.6）⇒ 它的宿主必须是
//! `apps/mc-server`，而不是 `mc-http` 的一层中间件（worker 的重试/退避/限流不属于请求面）。
//!
//! 本文件做四件事：
//!
//! 1. 用部署密钥造 `ghsnapshot::Client`（缺 App id / 私钥 ⇒ **整体不装配**，正常路径）；
//! 2. 实现 `SnapshotStore`（**直连 SQL**，四段查询逐字对照上游
//!    `pkg/db/queries/github_snapshot.sql`）；
//! 3. `Manager::start()` + 把句柄注册进 `mc-http` 的端口注入槽
//!    （`set_pr_refresh_port`）⇒ webhook 与页面访问两条入队路径都活了；
//! 4. 进**同一条停机链**（`main.rs` 固定：「先停渠道连接 → 再停 PR 刷新 → 再停调度器 →
//!    最后停 actor」），停机时收 worker + 关掉本宿主自己的连接池。
//!
//! # 为什么存储实现在这里（`docs/61` §3.3 的落点裁定 + 偏离 D5）
//!
//! `mc-vcs-github` 的依赖边被 anchor 冻结（`Cargo.toml` 逐字「此后 M8-1/4/5 的写者不得再
//! 新增三方依赖」）⇒ 它**没有** `sqlx` / `mc-db`，`SnapshotStore` 只能由宿主实现。这
//! **正是 M5-9 的既有手法**：`apps/mc-server/src/scheduler/{schedule,wakeup,hook}_port.rs`
//! 三个生产端口同样是「在本 crate 里直打 SQL」，理由逐字相同（端口 trait 的签名里有
//! `Uuid` / `DateTime<Utc>`，而本 crate 只有纯 HTTP 依赖 ⇒ 补 `sqlx`/`uuid`/`chrono` 三条
//! workspace 边）。
//!
//! # 数据库句柄：本宿主**自建一个小池**（偏离 D5，登记 `docs/32` §21.2）
//!
//! anchor 冻结的 `main.rs` 用 `integrations::start(&github_keys)` 调用本模块（注释逐字
//! 「接线后这里换成真调用，**签名不变**」）⇒ 宿主**拿不到** `main.rs` 的那个 `Db`。三条出路
//! 里选了第二条：
//!
//! | 出路 | 为什么不选 |
//! | --- | --- |
//! | 改 `main.rs` 传 `&db` | `main.rs` 是本片写集的**只读**文件（anchor 冻结，§11.1） |
//! | **自建小池（本片取）** | 用 `Config::from_env()` 取**同一个 URL**（生产路径上 `main` 已经解析成功过 ⇒ 这里必然成功），`Db::connect_lazy` 懒拨号，池上限 8、`min_connections = 0`，停机时 `close()` |
//! | 不装配存储（登记成缺口） | 那会让「配了密钥但快照永远不落库」变成静默失效 —— 本仓明确拒绝这类处理（`docs/37` 的 R7 类） |
//!
//! **代价**：多一个（懒拨号的）连接池。**建议 M8-7**：在 `main.rs` 里把 `&db` 传进来
//! （一行改动 + 一处签名），本文件的池随之删掉。
//!
//! # 广播：**本片不接线**（登记缺口）
//!
//! 上游 `Manager.onApplied` 让「快照真的写进去了」广播一条 `pull_request:updated`；同一个
//! 理由（宿主拿不到 `realtime` 句柄）⇒ `Manager::with_options` 的 `on_applied` 留空。
//! 后果：页面访问触发的刷新写进库了，但客户端不会收到推送（下次打开卡片才看到新值）。
//!
//! # 装配判据 = **App 凭据存在**（`docs/61` §2.4 / §2.5）
//!
//! 缺 `GITHUB_APP_ID` / `GITHUB_APP_PRIVATE_KEY` ⇒ `ghsnapshot` 整体不装配
//! （`Client::disabled()`），页面访问与 webhook 都不会触发刷新 —— 这是**正常**路径
//! （「能连接」与「能浏览仓库」是两个独立判据）。**绝不**假装刷新已接上。

use std::sync::Arc;

use mc_core::id::Id;
use mc_db::Db;
use mc_http::state::integrations::GithubKeys;
use mc_vcs_github::ghsnapshot::refresh::{
    async_trait, Address, PrRowRef, PrSnapshot, ResolvedTarget, SnapshotStore, StoreError,
};
use mc_vcs_github::ghsnapshot::{Client, Manager};
use sqlx::Row as _;

/// 本宿主自建池的上限（偏离 D5）：worker 的四个存储操作都是**短查询**，8 条连接足够；
/// `min_connections = 0` ⇒ 空闲时一条 TCP 都不占。
const HOST_POOL_MAX_CONNECTIONS: u32 = 8;

/// 代码与制品面的装配结果：PR 刷新宿主 + 已配置判据。
pub struct IntegrationHandles {
    pr_refresh: Option<Arc<Manager>>,
    /// 本宿主自建的池（偏离 D5）；`None` = 没装配。
    db: Option<Db>,
    wired: bool,
    app_configured: bool,
}

impl std::fmt::Debug for IntegrationHandles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IntegrationHandles")
            .field("pr_refresh", &self.pr_refresh.is_some())
            // 池只暴露「有没有」（`Db` 的 Debug 已经是 size/idle，不回显 URL）。
            .field("store_pool", &self.db.is_some())
            .field("wired", &self.wired)
            .field("app_configured", &self.app_configured)
            .finish()
    }
}

impl IntegrationHandles {
    /// PR 刷新宿主（`None` = 未装配）。
    pub fn pr_refresh(&self) -> Option<&Arc<Manager>> {
        self.pr_refresh.as_ref()
    }

    /// 宿主端口是否已接线。
    pub fn is_wired(&self) -> bool {
        self.wired
    }

    /// App 凭据是否配置（诊断用）。
    pub fn is_app_configured(&self) -> bool {
        self.app_configured
    }

    /// 停机：收掉 PR 刷新 worker（worker 在 `Tuning::shutdown_grace` 内退出），
    /// 再关掉本宿主的池。顺序由 `main.rs` 固定为「先停渠道连接 → 再停 PR 刷新 →
    /// 再停调度器 → 最后停 actor」。
    pub async fn shutdown(self) {
        if let Some(manager) = self.pr_refresh {
            manager.shutdown().await;
        }
        if let Some(db) = self.db {
            db.close().await;
        }
    }
}

/// 装配并启动代码与制品面的后台宿主（`main.rs` 在**渠道宿主之后、调度器之前**调用）。
///
/// `keys` = GitHub App 的部署密钥读取口（`mc_http::state::integrations::GithubKeys`）。
///
/// **不**返回错误：装配期的失败（私钥 PEM 非法、库 URL 缺失等）打一条**可操作的**日志并
/// 整体退化 —— 上游语义是「键在但非法 ⇒ 打一条 error、整体退化」，不是进程起不来。
pub fn start(keys: &GithubKeys) -> IntegrationHandles {
    // 生产路径上 `main` 已经成功解析过一次配置（`Config::from_env()`），所以这里**必然**成功；
    // 失败只可能是「本函数被单独调用」的场景（例如单测），那时诚实地不装配。
    let database_url = mc_config::Config::from_env()
        .ok()
        .map(|config| config.database.url);
    start_with(keys, database_url)
}

/// 与 [`start`] 同，但库 URL 由调用方给（**确定性接缝**：单测不必碰进程 env）。
///
/// `database_url == None` ⇒ 只 warn 不起 worker（诚实退化，不假装）。
pub(crate) fn start_with(keys: &GithubKeys, database_url: Option<String>) -> IntegrationHandles {
    let webhook_configured = keys.is_webhook_configured();
    if !keys.is_app_configured() {
        // 缺 App 凭据 ⇒ 整体不装配（**正常**路径）。
        tracing::info!(
            webhook_configured,
            "github app credentials are not configured; PR snapshot refresh disabled"
        );
        return IntegrationHandles {
            pr_refresh: None,
            db: None,
            wired: false,
            app_configured: false,
        };
    }

    let Some(url) = database_url else {
        tracing::warn!(
            "github app credentials are set but no database URL is configured; \
             no PR refresh worker will be started"
        );
        return IntegrationHandles {
            pr_refresh: None,
            db: None,
            wired: false,
            app_configured: true,
        };
    };
    let db = match Db::connect_lazy(&url, HOST_POOL_MAX_CONNECTIONS, 0) {
        Ok(db) => db,
        Err(error) => {
            // 只带原因，不回显 URL（可能内嵌口令）。
            tracing::error!(
                error = %error,
                "github: could not build the snapshot store pool; \
                 no PR refresh worker will be started"
            );
            return IntegrationHandles {
                pr_refresh: None,
                db: None,
                wired: false,
                app_configured: true,
            };
        }
    };

    let client = Arc::new(Client::new(
        keys.app_id.clone(),
        keys.private_key_pem.clone(),
    ));
    let store = Arc::new(McSnapshotStore::new(&db));
    let manager = Arc::new(Manager::new(client, store));
    if let Err(error) = manager.start() {
        tracing::error!(%error, "github: could not start the PR refresh workers");
        return IntegrationHandles {
            pr_refresh: None,
            db: None,
            wired: false,
            app_configured: true,
        };
    }
    // 端口注入：`mc-http` 的 webhook handler 与页面访问都读这个进程级槽
    // （`routes/github/webhook.rs` 的 `set_pr_refresh_port`；M8-4 的交接第 1 条点名要接）。
    // ⚠️ 槽是**按请求读**的 ⇒ `main.rs` 先建 router、后起本宿主也不影响生效。
    mc_http::routes::github::webhook::set_pr_refresh_port(manager.clone());
    tracing::info!(
        webhook_configured,
        pool_max_connections = HOST_POOL_MAX_CONNECTIONS,
        "github PR snapshot refresh wired (worker pool + TTL sweeper)"
    );
    IntegrationHandles {
        pr_refresh: Some(manager),
        db: Some(db),
        wired: true,
        app_configured: true,
    }
}

// ---------------------------------------------------------------------------
// 存储端口的生产实现：四段查询逐字对照 `pkg/db/queries/github_snapshot.sql`
// ---------------------------------------------------------------------------

/// 按地址列出 PR 行的 SQL。
///
/// ⚠️ 与上游的**一处措辞差异**：上游查询按 `installation_id` 过滤（地址天然带 installation）；
/// 本仓的 `resolve_installation` 也是按行读出来的，两者用的是**同一列**，语义相同。
const LIST_ROWS_BY_ADDRESS: &str = "\
SELECT id, state
FROM github_pull_request
WHERE installation_id = $1 AND repo_owner = $2 AND repo_name = $3 AND pr_number = $4";

/// 解析安装（端口的请求定位键 → 地址）：一行读取，顺带带回快照时刻（view TTL 用）。
const RESOLVE_INSTALLATION: &str = "\
SELECT installation_id,
       (EXTRACT(EPOCH FROM snapshot_fetched_at))::BIGINT AS snapshot_fetched_at_epoch
FROM github_pull_request
WHERE workspace_id = $1 AND repo_owner = $2 AND repo_name = $3 AND pr_number = $4
LIMIT 1";

/// head-SHA 守卫的**守更新**（上游 `UpdateGitHubPRSnapshot` 的 WHERE 逐字）。
///
/// `$5` = 快照时刻的 unix 秒；`$6` = 行 id；`$4` = `snapshot_head_sha` 与守卫**同一个**值。
const UPDATE_SNAPSHOT: &str = "\
UPDATE github_pull_request
SET api_mergeable = $1,
    api_merge_state_status = $2,
    checks_rollup_state = $3,
    snapshot_head_sha = $4,
    snapshot_fetched_at = to_timestamp($5),
    updated_at = now()
WHERE id = $6 AND head_sha = $4";

/// 批次替换的第一半：全删（上游 `DeleteGitHubPRCheckRuns`）。
const DELETE_CHECK_RUNS: &str = "DELETE FROM github_pull_request_check_run WHERE pr_id = $1";

/// 批次替换的第二半：逐行插入（上游 `InsertGitHubPRCheckRun`）。
const INSERT_CHECK_RUN: &str = "\
INSERT INTO github_pull_request_check_run
    (pr_id, head_sha, ordinal, name, status, conclusion, details_url, is_status_context)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";

/// TTL sweep 的候选地址（上游 `ListStaleUndecidedGitHubPRs` 逐字，含游标排序与回绕）。
const LIST_STALE_UNDECIDED: &str = "\
WITH candidates AS (
    SELECT installation_id, repo_owner, repo_name, pr_number
    FROM github_pull_request AS pr
    WHERE state IN ('open', 'draft')
      AND (snapshot_fetched_at IS NULL OR snapshot_fetched_at < to_timestamp($1))
      AND (
          snapshot_fetched_at IS NULL
          OR api_mergeable IS NULL
          OR api_mergeable = 'UNKNOWN'
          OR checks_rollup_state IN ('PENDING', 'EXPECTED')
          OR EXISTS (
              SELECT 1
              FROM github_pull_request_check_run AS cr
              WHERE cr.pr_id = pr.id AND cr.status <> 'completed'
          )
      )
    GROUP BY installation_id, repo_owner, repo_name, pr_number
)
SELECT installation_id, repo_owner, repo_name, pr_number
FROM candidates
ORDER BY (
    ROW(installation_id, repo_owner, repo_name, pr_number) >
    ROW($2::BIGINT, $3::TEXT, $4::TEXT, $5::INTEGER)
) DESC,
installation_id, repo_owner, repo_name, pr_number
LIMIT $6";

/// 宿主直连 SQL 的存储实现（见模块头「为什么存储实现在这里」）。
///
/// 内部只握一个**池的克隆**（`PgPool` 内部是 `Arc`）：一句话就能既给生产装配用
/// （[`McSnapshotStore::new`] 从 `Db` 取池）又给真库用例用（[`McSnapshotStore::from_pool`]
/// 从裸池构造），而不必碰 `mc-db` 的 `test-util` feature（`Db::from_pool` 在依赖方不可见
/// —— 与 M5-9 的 `scheduler/*_port.rs` 同判）。池的生命周期仍由 [`IntegrationHandles::shutdown`]
/// 里的 `Db::close()` 收口（关闭对每个克隆生效）。
pub struct McSnapshotStore {
    pool: sqlx::postgres::PgPool,
}

impl McSnapshotStore {
    #[must_use]
    pub fn new(db: &Db) -> Self {
        Self {
            pool: db.pool().clone(),
        }
    }

    /// 从裸池构造（**真库用例用**）。
    #[must_use]
    #[cfg(test)]
    pub fn from_pool(pool: sqlx::postgres::PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl SnapshotStore for McSnapshotStore {
    async fn resolve_installation(
        &self,
        workspace_id: Id,
        owner: &str,
        repo: &str,
        number: i32,
    ) -> Result<Option<ResolvedTarget>, StoreError> {
        let row = sqlx::query(RESOLVE_INSTALLATION)
            .bind(workspace_id.0)
            .bind(owner)
            .bind(repo)
            .bind(number)
            .fetch_optional(&self.pool)
            .await
            .map_err(db_error)?;
        row.map(|row| {
            Ok(ResolvedTarget {
                installation_id: get(&row, "installation_id")?,
                snapshot_fetched_at: get(&row, "snapshot_fetched_at_epoch")?,
            })
        })
        .transpose()
    }

    async fn list_rows(&self, address: &Address) -> Result<Vec<PrRowRef>, StoreError> {
        let rows = sqlx::query(LIST_ROWS_BY_ADDRESS)
            .bind(address.installation_id)
            .bind(&address.owner)
            .bind(&address.repo)
            .bind(address.number)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(PrRowRef {
                    id: Id(get(&row, "id")?),
                    state: get(&row, "state")?,
                })
            })
            .collect()
    }

    async fn apply_snapshot(
        &self,
        pr_id: Id,
        snapshot: &PrSnapshot,
        now_unix: i64,
    ) -> Result<bool, StoreError> {
        // 上游 `applySnapshot`：一条事务里「守更新 PR 行 + 全删全插逐 check 行」；
        // 守更新回 0 行 ⇒ **整批作废**（逐 check 行一行都不写）。
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let updated = sqlx::query(UPDATE_SNAPSHOT)
            .bind(snapshot.mergeable.as_deref())
            .bind(snapshot.merge_state_status.as_deref())
            .bind(if snapshot.has_checks {
                snapshot.rollup_state.as_deref()
            } else {
                None
            })
            .bind(&snapshot.head_sha)
            .bind(now_unix)
            .bind(pr_id.0)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?
            .rows_affected();
        if updated == 0 {
            return Ok(false);
        }
        sqlx::query(DELETE_CHECK_RUNS)
            .bind(pr_id.0)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        for (ordinal, check) in snapshot.checks.iter().enumerate() {
            let ordinal = i32::try_from(ordinal)
                .map_err(|_| StoreError::Db(format!("check ordinal out of range: {ordinal}")))?;
            sqlx::query(INSERT_CHECK_RUN)
                .bind(pr_id.0)
                .bind(&snapshot.head_sha)
                .bind(ordinal)
                .bind(&check.name)
                .bind(&check.status)
                .bind(check.conclusion.as_deref())
                .bind(check.details_url.as_deref())
                .bind(check.is_status_context)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        tx.commit().await.map_err(db_error)?;
        Ok(true)
    }

    async fn list_stale_undecided(
        &self,
        older_than_unix: i64,
        after: &Address,
        max_rows: i32,
    ) -> Result<Vec<Address>, StoreError> {
        let rows = sqlx::query(LIST_STALE_UNDECIDED)
            .bind(older_than_unix)
            .bind(after.installation_id)
            .bind(&after.owner)
            .bind(&after.repo)
            .bind(after.number)
            .bind(max_rows)
            .fetch_all(&self.pool)
            .await
            .map_err(db_error)?;
        rows.into_iter()
            .map(|row| {
                Ok(Address {
                    installation_id: get(&row, "installation_id")?,
                    owner: get(&row, "repo_owner")?,
                    repo: get(&row, "repo_name")?,
                    number: get(&row, "pr_number")?,
                })
            })
            .collect()
    }
}

/// `sqlx::Error` → 端口错误（**只带原因**，不回显参数；本面没有任何凭据列）。
///
/// 按值收（`map_err(db_error)` 直接可用）：`sqlx::Error` 的所有者就是调用方，
/// 借出去反而每次都要写闭包。
#[allow(clippy::needless_pass_by_value)]
fn db_error(error: sqlx::Error) -> StoreError {
    StoreError::Db(error.to_string())
}

/// 取一列并把解码失败折成 [`StoreError`]（省掉每个 `?` 上的 `map_err`）。
fn get<'r, T>(row: &'r sqlx::postgres::PgRow, column: &str) -> Result<T, StoreError>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(column).map_err(db_error)
}

#[cfg(test)]
mod tests;
