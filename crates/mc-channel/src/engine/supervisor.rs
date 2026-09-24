//! 长连接监管：**端口** + 退避重连（上游 `channel/engine/` 的 supervisor 面）。
//!
//! **状态：M7-0 anchor 只落端口**（`LUM-1765`）—— [`InstallationStore`] / [`LeaseStore`] 是
//! trait（**先定 port，实现归 M7-1 / M7-2**）；[`Supervisor`] 的行为是 `todo!()`。
//!
//! # 三个端口各自解决什么（`docs/60` §2.5 / §2.4）
//!
//! 1. [`InstallationStore`]：宿主起连接前要知道"这个 kind 现在有哪些活跃安装"。
//! 2. [`LeaseStore`]：**无 Redis 的进程内替身**（R-M7-1）。上游 `redis_lease_store.go`
//!    （167 行）用 Redis 做 WS 租约 CAS，多副本下只有一个副本持有某 installation 的长连接。
//!    本仓没有 Redis 且本波不引入 ⇒ 进程内实现 + **单副本部署契约**：多副本部署下同一个
//!    installation 可能被两个副本同时连接。**上游四处"无 Redis"路径都有等价降级语义**
//!    （`router.go:742` 有明文 warn）⇒ 这是换部署形态，不是伪造行为。
//! 3. [`Supervisor`]：对每个已装配的 kind × 活跃 installation 做
//!    `build → connect` → 链路断开后**指数退避**重连；`connect` 返回非 `Err` 就是"这次
//!    尝试失败"。退避时间序列必须**可注入 `Now`** 才能测（`docs/60` §6.5 的 M7-2 `DoD`）。
//!
//! # 宿主（谁调 `spawn`）
//!
//! `apps/mc-server/src/channels.rs`（`docs/60` §2.4）。停机链固定：
//! **先停渠道连接 → 再停调度器 → 最后停 actor**；本模块的 handle 就是那条链的第一步。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::installation::Installation;
use mc_core::channel::ChannelKind;
use mc_core::id::Id;

use crate::channel::ChannelResult;

/// 安装行读取端口（`channel_installation`）。
///
/// 实现落在 M7-1（仓储侧走 `mc_repos::channel`）。
#[async_trait]
pub trait InstallationStore: Send + Sync {
    /// 该 kind 下所有**可连接**的安装（`status = 'active'`）。
    async fn list_active(&self, kind: ChannelKind) -> ChannelResult<Vec<Installation>>;

    /// 单条安装；不存在返回 `None`（**不**报错：撤销与不存在在读取侧无差别）。
    async fn get(&self, installation_id: Id) -> ChannelResult<Option<Installation>>;
}

/// 一条长连接租约的凭据（上游 `engine.Lease`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseGrant {
    pub kind: ChannelKind,
    pub installation_id: Id,
    /// 持有者标识（上游用副本 id / 进程 id 做审计与接管判据）。
    pub owner: String,
    /// 租约令牌（对应 `channel_installation.ws_lease_token`）。
    pub token: String,
    /// 到期时间（对应 `ws_lease_expires_at`）。
    pub expires_at: mc_core::timestamp::Timestamp,
}

/// 长连接租约端口（上游 `redis_lease_store.go` 的进程内替身，见模块文档第 2 条）。
///
/// 实现归 **M7-2**（`docs/60` §3.3 的 `engine/lease.rs`）。
#[async_trait]
pub trait LeaseStore: Send + Sync {
    /// 尝试取得某 installation 的租约：`Ok(None)` = 被别的持有者占着（调用方**不要**连），
    /// `Ok(Some(grant))` = 拿到（含续租成功的情形）。
    async fn acquire(
        &self,
        kind: ChannelKind,
        installation_id: Id,
        owner: &str,
        ttl_secs: u64,
    ) -> ChannelResult<Option<LeaseGrant>>;

    /// 主动释放（停机路径；幂等 —— 已过期/已被接管的释放是 no-op）。
    async fn release(&self, grant: &LeaseGrant) -> ChannelResult<()>;
}

/// 监管句柄：宿主用它停掉整条渠道连接面。
///
/// ⚠️ anchor 期**不可构造**（[`Supervisor::spawn`] 是 `todo!()`），但类型先定死，
/// 因为停机顺序（渠道 → 调度器 → actor）是 `main.rs` 的**一行**、不能等到 M7-1 才改。
pub struct SupervisorHandle {
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for SupervisorHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SupervisorHandle")
            .field("finished", &self.task.is_finished())
            .finish()
    }
}

impl SupervisorHandle {
    /// 由 [`Supervisor::spawn`] 构造（M7-1）。
    pub fn new(task: tokio::task::JoinHandle<()>) -> Self {
        Self { task }
    }

    /// 是否已经结束（诊断用）。
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// 优雅停机：先请求取消、再等任务收尾。
    ///
    /// 上游语义是"连接挂着不退"必须被消灭（`docs/60` §8 的 R-M7-7），所以这里是
    /// `abort` + `await`：`abort` 保证**不**无限等一条卡死的平台连接，
    /// `await` 保证收尾（关 socket / 释放租约）已经在返回前跑完或已被取消。
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

/// 长连接监管器（上游 `engine.Supervisor`）。
///
/// anchor 期只持有依赖；行为（backoff 循环 / 每个 installation 一个任务 / 租约续期）
/// 归 M7-1，租约实现归 M7-2。
pub struct Supervisor {
    deps: Arc<crate::engine::ChannelDeps>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Supervisor")
            .field("deps", &self.deps)
            .finish()
    }
}

impl Supervisor {
    /// 装配（不启动）。
    pub fn new(deps: Arc<crate::engine::ChannelDeps>) -> Self {
        Self { deps }
    }

    /// 依赖袋（诊断用）。
    pub fn deps(&self) -> &Arc<crate::engine::ChannelDeps> {
        &self.deps
    }

    /// 起监管任务并返回停机句柄（M7-1）。
    ///
    /// # Panics
    ///
    /// anchor 期未实现：**本 anchor 的 `channels.rs` 不调用它**（无密钥 ⇒ 不装配 ⇒
    /// 返回空 handle），所以启动路径上不会 panic。
    ///
    /// # Errors
    ///
    /// 装配失败（未知 kind / 配置非法）时返回 [`crate::channel::ChannelError`]。
    pub fn spawn(self: Arc<Self>) -> ChannelResult<SupervisorHandle> {
        todo!("M7-1：退避重连 + 每 installation 一条连接 + 租约续期")
    }
}
