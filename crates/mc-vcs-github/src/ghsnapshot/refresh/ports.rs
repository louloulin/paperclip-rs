//! `ghsnapshot::refresh` 的**契约面**：端口（trait）、形状类型、调参与注入缝。
//!
//! 从 `refresh.rs` 拆出（`docs/32` §21.1 的落点说明）：`refresh.rs` 一度涨到 **1040 行**，
//! 越过门 ⑩ 的 800 行硬上限 ⇒ 按「**契约面 vs 实现面**」切：本文件只管「谁来实现什么、
//! 形状是什么、哪些缝可注入」，`refresh.rs` 管 worker 池 / 限流 / 退避 / 停机。
//!
//! 对外路径**逐字不变**（`refresh.rs` 里 `pub use ports::{…}` 把全部名字重导一遍）。

use std::sync::Arc;
use std::time::Duration;

use mc_core::id::Id;

use crate::ghsnapshot::refresh::{default_jitter, system_now_unix};
use crate::ghsnapshot::snapshot::fetch_pr_snapshot;
pub use crate::ghsnapshot::snapshot::PrSnapshot;
use crate::ghsnapshot::Client;
use crate::rest::GithubError;

/// **给端口实现者的宏再导出**：`SnapshotStore` 是 async trait，实现它需要 `#[async_trait]`。
/// 本 crate 的依赖边里有 `async-trait`，而宿主 `apps/mc-server` 的 manifest 是 anchor 冻结
/// （不能为它加一条 dev/依赖边）⇒ port 的**定义方**把宏带出去，实现方
/// `use mc_vcs_github::ghsnapshot::refresh::async_trait;` 即可。
pub use async_trait::async_trait;

/// 刷新的**定位键**：一个 `(installation, owner, repo, number)` 元组 —— 上游 `address`
/// （`refresh.go:23`）。
///
/// 一个地址可以**扇出**到多个 workspace 的 `github_pull_request` 行（同一个 installation 被
/// 多个 workspace 绑定时各镜像一行）；**一次 API 抓取服务全部这些行**，每行各自被 head-SHA
/// 守卫保护。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Address {
    pub installation_id: i64,
    pub owner: String,
    pub repo: String,
    pub number: i32,
}

impl Address {
    #[must_use]
    pub fn new(installation_id: i64, owner: &str, repo: &str, number: i32) -> Self {
        Self {
            installation_id,
            owner: owner.to_string(),
            repo: repo.to_string(),
            number,
        }
    }
}

/// 存储层交回的一行 PR —— 上游 `ListGitHubPRRowsByAddress` 的**本仓投影**。
///
/// 两列够用：`head_sha` 守卫在 `apply_snapshot` 的 WHERE 里（上游同款），`workspace_id`
/// 在循环体里不出现。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRowRef {
    pub id: Id,
    /// `open` / `draft` / `merged` / `closed`（迁移 `079` 的字面量）。
    pub state: String,
}

impl PrRowRef {
    /// 上游 `row.State == "open" || row.State == "draft"` 的**唯一**实现点
    /// （chase 窗口与 TTL sweep 都按它判「还开着」）。
    #[must_use]
    pub fn is_open_or_draft(&self) -> bool {
        self.state == "open" || self.state == "draft"
    }
}

/// 一次「请求 → 地址」解析的结果 —— 上游 handler 直接拿在手里的那份信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// GitHub 的 installation id（`github_pull_request.installation_id`）。
    pub installation_id: i64,
    /// 该 PR 行**当前**快照的抓取时刻（unix 秒；`None` = 从没抓过）。
    /// 页面访问的 view TTL 判定读它（偏离 D3）。
    pub snapshot_fetched_at: Option<i64>,
}

/// 存储层错误（**不得**含凭据；这一层只有行 id 与枚举文本）。
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("snapshot store: {0}")]
    Db(String),
}

/// 快照管道的**存储端口** —— 上游 `pkg/db/queries/github_snapshot.sql` 四段查询的本仓形状。
///
/// 实现是宿主的责任（`apps/mc-server/src/integrations.rs`）；本 crate 只定义契约与调用时序。
/// 四个方法全部 `async`（都落真库），且**必须**保持各自的语义：
///
/// | 方法 | 上游对应 | 不可让步的语义 |
/// | --- | --- | --- |
/// | [`SnapshotStore::resolve_installation`] | `github_pull_request` 行读取（端口形状的补偿，D2） | 找不到行 ⇒ `Ok(None)`（**不是**错误） |
/// | [`SnapshotStore::list_rows`] | `ListGitHubPRRowsByAddress` | **跨 workspace** 返回**全部**匹配行 |
/// | [`SnapshotStore::apply_snapshot`] | `UpdateGitHubPRSnapshot` + `Delete`/`InsertGitHubPRCheckRun` | 一条事务；`head_sha` 不匹配 ⇒ `Ok(false)` 且**一行都不写**（含逐 check 行）；`now_unix` 就是上游 `m.now()` 写进 `snapshot_fetched_at` 的那个时刻（**注入**的时钟，不是库时钟） |
/// | [`SnapshotStore::list_stale_undecided`] | `ListStaleUndecidedGitHubPRs` | 只回 open/draft、陈旧**且**未决的地址；按 `after` 游标取一批、游标之后的行排前 |
#[async_trait]
pub trait SnapshotStore: Send + Sync {
    /// 把端口的**请求定位键**解析成**地址**（`installation_id` + 当前快照时刻）。
    ///
    /// # Errors
    ///
    /// 查询失败 ⇒ [`StoreError`]（调用方记一条 warn 并丢弃这一次请求）。
    async fn resolve_installation(
        &self,
        workspace_id: Id,
        owner: &str,
        repo: &str,
        number: i32,
    ) -> Result<Option<ResolvedTarget>, StoreError>;

    /// 地址下的全部 PR 行（扇出）。
    ///
    /// # Errors
    ///
    /// 查询失败 ⇒ [`StoreError`]。
    async fn list_rows(&self, address: &Address) -> Result<Vec<PrRowRef>, StoreError>;

    /// **head-SHA 守卫的原子批次替换**：一条事务里「守更新的 PR 行 + 全删全插逐 check 行」。
    ///
    /// 返回 `Ok(false)` = 行的 head 已经前进（快照作废）⇒ **什么都没写**。
    ///
    /// `now_unix` = 这次抓取的时刻（上游 `tsFromTime(m.now())` 写进 `snapshot_fetched_at`）；
    /// 由调用方注入而不是取库时钟，好让「抓取时刻」与 TTL sweep 的比较用**同一个**时钟。
    ///
    /// # Errors
    ///
    /// 事务失败 ⇒ [`StoreError`]。
    async fn apply_snapshot(
        &self,
        pr_id: Id,
        snapshot: &PrSnapshot,
        now_unix: i64,
    ) -> Result<bool, StoreError>;

    /// TTL sweep 的一批候选地址 —— 上游 `ListStaleUndecidedGitHubPRs`。
    ///
    /// `older_than_unix` = `now - sweep_ttl`；`after` = 上一次批次的**末地址**（首批用零值
    /// [`Address::default`]，与上游的 4 个零值实参同判）；`max_rows` 有界。
    ///
    /// # Errors
    ///
    /// 查询失败 ⇒ [`StoreError`]。
    async fn list_stale_undecided(
        &self,
        older_than_unix: i64,
        after: &Address,
        max_rows: i32,
    ) -> Result<Vec<Address>, StoreError>;
}

/// 延迟回调端口 —— 「按时间重排一次」这件事的唯一出口（chase / 限流暂停 / 重试）。
///
/// 生产实现是 [`TokioTimer`]；测试用记录型替身**捕获**延迟序列并手动触发 ⇒ `DoD` 的
/// 「退避时间序列可测（注入 `Now`，不许 sleep 真实时间）」。
pub trait Timer: Send + Sync {
    /// 在 `delay` 之后调用 `callback`（**至多一次**）。实现**不得**阻塞调用方。
    fn schedule(&self, delay: Duration, callback: Box<dyn FnOnce() + Send + 'static>);
}

/// 默认定时器：`tokio::spawn(sleep + callback)`（回调在独立任务里跑，绝不阻塞 worker）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioTimer;

impl Timer for TokioTimer {
    fn schedule(&self, delay: Duration, callback: Box<dyn FnOnce() + Send + 'static>) {
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            callback();
        });
    }
}

/// 抓取端口 —— 上游 `Manager.fetch` 字段（`refresh.go:74` 的测试缝）。
///
/// 生产实现只有一个：[`HttpSnapshotFetcher`]（跑真 GraphQL）。测试用它注入「立刻返回预算好的
/// 快照 / `RateLimited` / 错误」，于是队列、退避、停机三件事都能在零网络下逐条钉住。
#[async_trait]
pub trait SnapshotFetcher: Send + Sync {
    /// # Errors
    ///
    /// 传输 / 查询级错误 ⇒ [`GithubError`]（[`GithubError::RateLimited`] 会被上层翻译成
    /// installation 级暂停）。
    async fn fetch(
        &self,
        client: &Client,
        address: &Address,
        now_unix: i64,
    ) -> Result<PrSnapshot, GithubError>;
}

/// 默认抓取器：单查询 + contexts 分页（[`fetch_pr_snapshot`]）。
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpSnapshotFetcher;

#[async_trait]
impl SnapshotFetcher for HttpSnapshotFetcher {
    async fn fetch(
        &self,
        client: &Client,
        address: &Address,
        now_unix: i64,
    ) -> Result<PrSnapshot, GithubError> {
        fetch_pr_snapshot(
            client,
            address.installation_id,
            &address.owner,
            &address.repo,
            address.number,
            now_unix,
        )
        .await
    }
}

/// 管道调参 —— 上游 `refresh.go:39-52` 那些包级 `default*` 变量的本仓形状。
///
/// 默认值与上游**逐字相同**；`Default` 就是生产取值，测试按需覆写（尤其 `chase_backoff`、
/// `sweep_interval` 与 `shutdown_grace`）。
#[derive(Debug, Clone)]
pub struct Tuning {
    /// worker 池大小（上游 `defaultConcurrency = 12`）。
    pub concurrency: usize,
    /// 页面访问的 view TTL：比它新就不抓（上游 `defaultViewTTL = 60s`）。
    pub view_ttl_secs: i64,
    /// TTL sweep 的陈旧阈值（上游 `defaultSweepTTL = 10m`）。
    pub sweep_ttl_secs: i64,
    /// TTL sweep 的间隔（上游 `defaultSweepInterval = 10m`）。
    pub sweep_interval: Duration,
    /// 一轮 sweep 最多取多少行地址（上游 `defaultSweepMaxRows = 200`）。
    pub sweep_max_rows: i32,
    /// chase 窗口的退避序列（上游 `defaultChaseBackoff = 30s/1m/2m/5m`；用尽后**停在末项**）。
    pub chase_backoff: Vec<Duration>,
    /// chase 上限（上游 `maxChaseAttempts = 12`）。
    pub max_chase_attempts: usize,
    /// 出站队列容量（上游 `queueBuffer = 2048`）。
    pub queue_capacity: usize,
    /// 停机时等 worker 退出的上限（`docs/61` §6.5 的 M8-5 `DoD`：「N 秒内退出」）。
    /// 上游没有这个数（靠 ctx 取消 + 进程退出）；本仓把它**显式化**且有用例。
    pub shutdown_grace: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            concurrency: 12,
            view_ttl_secs: 60,
            sweep_ttl_secs: 600,
            sweep_interval: Duration::from_secs(600),
            sweep_max_rows: 200,
            chase_backoff: vec![
                Duration::from_secs(30),
                Duration::from_secs(60),
                Duration::from_secs(120),
                Duration::from_secs(300),
            ],
            max_chase_attempts: 12,
            queue_capacity: 2048,
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

/// 构造 [`Manager`] 的全部注入缝（生产只用 [`ManagerOptions::default`]）。
pub struct ManagerOptions {
    pub tuning: Tuning,
    pub fetcher: Arc<dyn SnapshotFetcher>,
    pub timer: Arc<dyn Timer>,
    pub clock: Box<dyn Fn() -> i64 + Send + Sync>,
    pub jitter: Box<dyn Fn() -> Duration + Send + Sync>,
    /// 「快照真的写进去了」的回调（上游 `onApplied`）；默认 `None`（见模块头的缺口登记）。
    pub on_applied: Option<Arc<dyn Fn(Id) + Send + Sync>>,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            tuning: Tuning::default(),
            fetcher: Arc::new(HttpSnapshotFetcher),
            timer: Arc::new(TokioTimer),
            clock: Box::new(system_now_unix),
            jitter: Box::new(default_jitter),
            on_applied: None,
        }
    }
}

impl std::fmt::Debug for ManagerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagerOptions")
            .field("tuning", &self.tuning)
            .finish_non_exhaustive()
    }
}
