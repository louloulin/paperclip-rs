//! PR 快照刷新的出站队列与后台宿主 —— 上游 `ghsnapshot/refresh.go`（562 行）
//! （M8-0 anchor 建桩，**实现归 M8-5**）。
//!
//! # worker 池 + TTL sweeper + 限流暂停（`docs/61` §2.6 / R-M8-5）
//!
//! 三级退避：`RateLimitError` ⇒ `rateLimitPause` / `deferActive` / `scheduleRetry`。
//! **无 Redis ⇒ 单副本部署契约**（与 M7 的 R-M7-1 同源，登记 `docs/32` §9.12）。
//! 上游语义是「每个 PR 同时只有一单在飞 + 有界并发 + 单地址串行」。
//!
//! # 宿主与停机链
//!
//! `Manager::start` 由 `apps/mc-server/src/integrations.rs` 调用；停机顺序固定为
//! 「先停渠道连接 → **再停 PR 刷新** → 再停调度器 → 最后停 actor」（`docs/61` §2.6）。
//! 停机后 worker 必须在 N 秒内退出（M8-5 的 `DoD`）。

use std::sync::Arc;

use tokio::sync::Notify;

use crate::port::{PrRefreshPort, PrRefreshRequest};
use crate::rest::GithubError;

/// 后台刷新的宿主句柄（`start` 的返回值；`shutdown` 进停机链）。
pub struct Manager {
    /// 客户端（`ghsnapshot::client::Client`；未配置时 `enabled()==false`）。
    client: Arc<crate::ghsnapshot::Client>,
    /// 停机信号（anchor 期先落形状；M8-5 让 worker 监听它）。
    shutdown: Arc<Notify>,
}

impl Manager {
    /// 构造（不启动 worker）。
    pub fn new(client: Arc<crate::ghsnapshot::Client>) -> Self {
        Self {
            client,
            shutdown: Arc::new(Notify::new()),
        }
    }

    /// 客户端（诊断/装配用）。
    pub fn client(&self) -> &Arc<crate::ghsnapshot::Client> {
        &self.client
    }

    /// 启动 worker 池 + TTL sweeper —— **anchor 期是桩**，实现归 M8-5。
    ///
    /// # Errors
    ///
    /// 依赖未配置时按上游语义**不装配**（返回一个空 handle，而不是错误）；真正的
    /// 错误只可能来自内部线程/通道创建失败。
    pub async fn start(&self) -> Result<(), GithubError> {
        todo!("M8-5：worker 池 + TTL sweeper 启动（docs/61 §2.6）")
    }

    /// 停机（`apps/mc-server` 的停机链调用）—— **anchor 期是桩**，实现归 M8-5。
    pub async fn shutdown(&self) {
        self.shutdown.notify_waiters();
        todo!("M8-5：等 worker 在 N 秒内退出（docs/61 §6.5 的 M8-5 行）")
    }
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ghsnapshot::Manager")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

/// `Manager` 实现 [`PrRefreshPort`] 的**位置**是 M8-5 的 `refresh.rs`（anchor 不实现）：
/// 这里先落一个编译期断言用的空实现占位类型，确保 trait 与 `Manager` 在同一 crate 内可接。
///
/// ⚠️ anchor 期 **不** `impl PrRefreshPort for Manager`：`enqueue`/`maybe_enqueue_on_view`
/// 的真实语义（去重 / 在飞去重 / 节流）是 M8-5 的一部分，提前写空体会留下「假装刷新了」
/// 的静默失效。trait 形状用例用 [`crate::port::DisabledPrRefresh`]。
#[allow(dead_code)]
type ManagerWillImplementPort = fn(&Manager) -> &dyn PrRefreshPort;

/// 入队一次刷新的辅助（供 M8-5 的 `impl PrRefreshPort for Manager` 复用它的形状）。
#[allow(dead_code)]
pub(crate) fn noop_enqueue(_request: PrRefreshRequest) {}
