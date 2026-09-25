//! `supervisor` 的四个端口（`Installation` / `AcquireLeaseParams` / `ReleaseLeaseParams` +
//! `InstallationStore` / `LeaseStore`）。上游 `engine/supervisor.go` 的同一批定义。
//!
//! - **写者**：M7-1（`docs/60` §3.3）。**实现**归后续片（`LeaseStore` 的进程内实现 =
//!   M7-2 的 `engine/lease.rs`）。
//! - 拆出本文件是门 ⑩ 的要求（`supervisor.rs` 超 800 行 ⇒ 拆，见 `scripts/gates.sh` 的提示）；
//!   路径 `engine/supervisor/` 仍是 M7-1 的写集。

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_core::timestamp::Timestamp;

use crate::engine::resolvers::EngineResult;

// =====================================================================
// 端口
// =====================================================================

/// 监管器需要的**一行**安装（上游 `engine.Installation`）：engine 从**不**读平台凭据。
#[derive(Debug, Clone, PartialEq)]
pub struct Installation {
    /// `channel_installation.id`：租约键 + 监管任务的 map 键（一个安装一个任务）。
    pub id: Id,
    /// 选工厂用的平台判别式。
    pub kind: ChannelKind,
    /// 凭据指纹（不透明；由 store 计算）：sweep 之间变了 ⇒ 拆掉在跑的连接并按新凭据重建，
    /// 免得重装过的渠道拿着过期凭据一直跑。
    pub fingerprint: String,
    /// 平台凭据/配置 blob，原样交给工厂（`ChannelConfig::raw`）。
    pub config: serde_json::Value,
}

/// 取 / 续租约的围栏参数（上游 `AcquireLeaseParams`）。
#[derive(Debug, Clone, PartialEq)]
pub struct AcquireLeaseParams {
    pub installation_id: Id,
    pub kind: ChannelKind,
    pub token: String,
    pub expires_at: Timestamp,
    pub ttl: Duration,
}

/// 释放租约的参数（上游 `ReleaseLeaseParams`）：store **必须**用 token 围栏，否则一次迟到的
/// 释放会清掉后继者的新租约。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseLeaseParams {
    pub installation_id: Id,
    pub token: String,
}

/// 活跃安装枚举端口（上游 `InstallationStore`）：**跨全部渠道类型**，没有 per-platform 过滤
/// —— 上游把这个硬编码的 `feishu` 当成要消灭的限制（MUL-3620）。DB 实现由后续片落地。
#[async_trait]
pub trait InstallationStore: Send + Sync {
    /// 所有**可连接**的活跃安装。
    async fn list_active(&self) -> EngineResult<Vec<Installation>>;
}

/// 长连接租约端口（上游 `LeaseStore`；实现 = M7-2 的 `engine/lease.rs`）。
///
/// 与 `InstallationStore` 分开是**故意的**：生产可以用 PG 存安装元数据、用（将来的）低写入
/// 存储放租约。
#[async_trait]
pub trait LeaseStore: Send + Sync {
    /// 当前**有任意持有者**的安装 id 集合。只是 sweep 的优化；[`LeaseStore::try_acquire`]
    /// 仍是唯一权威。
    async fn list_held(&self, ids: &[Id]) -> EngineResult<HashSet<Id>>;

    /// 无主 / 已过期 / 令牌相同（同一持有的安全重试）⇒ 授予；
    /// 否则 ⇒ `Err(Pipeline(LeaseNotAcquired))`。
    async fn try_acquire(&self, params: AcquireLeaseParams) -> EngineResult<()>;

    /// 仅当当前值等于 `token` 时续期；`LeaseNotAcquired` = 所有权已丢。
    async fn renew(&self, params: AcquireLeaseParams) -> EngineResult<()>;

    /// 仅当当前值等于 `token` 时释放。
    async fn release(&self, params: ReleaseLeaseParams) -> EngineResult<()>;
}
