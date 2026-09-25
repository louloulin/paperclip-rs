//! lark 安装会话的**共享状态表**（上游 `install_session_store.go`，**170 行**）。
//!
//! 写者 **M7-14**。拆出来是**门 ⑩**（单文件 800 行硬限）的要求；切点是「协议客户端 / 会话表 /
//! 状态机与服务」—— 三者的**判据不同**：前者可以对着假传输确定性测，中者可以对着假时钟测
//! 过期与首次写入胜，后者要把两者缝在一起跑后台协程。
//!
//! 全部可见性经由 [`super`] 的再导出；本模块不引入任何新的公开面。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::id::Id;
use tokio::sync::Mutex;

use super::SessionStatus;

/// 安装会话的状态投影（上游 `InstallSessionState`；**唯一**读面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSessionState {
    /// 会话 id（浏览器拿它轮询）。
    pub id: String,
    /// 所属 workspace（读路径靠它收窄）。
    pub workspace_id: Id,
    /// 发起人（**不**序列化给客户端：只用来授权那次读）。
    pub initiator_id: Id,
    /// 当前状态。
    pub status: SessionStatus,
    /// 成功时的安装行 id。
    pub installation_id: Option<Id>,
    /// 失败时的稳定原因码。
    pub error_reason: String,
    /// 失败时的一句人话。
    pub error_message: String,
    /// `device_code` 的截止时刻（过了它、还 `pending` ⇒ 读路径报过期）。
    pub expires_at: DateTime<Utc>,
}

/// 会话不存在（未知 / 过期出队 / **属于别的 workspace**）。
///
/// 三合一**是故意的**：可区分的"存在但不是你的"会让调用方跨 workspace 枚举会话 id。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("lark: install session not found")]
pub struct SessionNotFound;

/// 终态的那一半（`Create` 之后唯一会变的部分）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallSessionOutcome {
    /// 终态状态（`Success` / `Error`）。
    pub status: SessionStatus,
    /// 成功时的安装行 id。
    pub installation_id: Option<Id>,
    /// 失败原因码。
    pub error_reason: String,
    /// 失败文案。
    pub error_message: String,
}

/// 在飞安装会话的共享家（上游 `InstallSessionStore`）。
///
/// [`Self::mark_terminal`] 按契约是**首次写入胜**：过期截止与一次 poll 结果会并发，
/// 用户必须保留他**已经看到过**的结局；输掉的那次是 **no-op 而不是错误**。
#[async_trait]
pub trait InstallSessionStore: Send + Sync {
    /// 登记一个 `pending` 会话；`ttl` 由调用方按"QR 窗口 + 终态读窗口"算好。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn create(&self, state: InstallSessionState, ttl: Duration) -> Result<(), String>;

    /// 按 workspace 收窄读一条。
    ///
    /// # Errors
    ///
    /// 不存在 ⇒ [`SessionNotFound`]。
    async fn get(&self, workspace_id: Id, id: &str)
        -> Result<InstallSessionState, SessionNotFound>;

    /// 记录**第一个**终态并忽略之后的任何一次；同时把保活窗口移到终态窗口上
    /// （自修复：输掉竞态的那次写也必须移动 retention）。
    ///
    /// # Errors
    ///
    /// 会话已不在 ⇒ [`SessionNotFound`]（重试调用方的"没得救了"信号）。
    async fn mark_terminal(
        &self,
        id: &str,
        outcome: InstallSessionOutcome,
        ttl: Duration,
    ) -> Result<(), SessionNotFound>;
}

/// 单进程实现（上游 `MemoryInstallSessionStore`；**D2** 的落地形态）。
pub struct MemoryInstallSessionStore {
    entries: Mutex<Vec<MemoryEntry>>,
    now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

#[derive(Debug, Clone)]
struct MemoryEntry {
    state: InstallSessionState,
    retain_til: DateTime<Utc>,
}

impl MemoryInstallSessionStore {
    /// 用真实时钟装配。
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(Utc::now))
    }

    /// 注入时钟（retention 因此可在**不睡觉**的情况下测）。
    #[must_use]
    pub fn with_clock(now: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            now,
        }
    }

    /// 丢掉落出保活窗口的记录（只在 `create` —— 冷路径 —— 上跑，不在每 ~5s 的 `get` 上跑）。
    async fn prune(&self) {
        let now = (self.now)();
        let mut entries = self.entries.lock().await;
        entries.retain(|entry| entry.retain_til > now);
    }
}

impl Default for MemoryInstallSessionStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl InstallSessionStore for MemoryInstallSessionStore {
    async fn create(&self, state: InstallSessionState, ttl: Duration) -> Result<(), String> {
        self.prune().await;
        let retain_til = (self.now)() + chrono::Duration::from_std(ttl).unwrap_or_default();
        let mut entries = self.entries.lock().await;
        entries.retain(|entry| entry.state.id != state.id);
        entries.push(MemoryEntry { state, retain_til });
        Ok(())
    }

    async fn get(
        &self,
        workspace_id: Id,
        id: &str,
    ) -> Result<InstallSessionState, SessionNotFound> {
        let now = (self.now)();
        let entries = self.entries.lock().await;
        let entry = entries
            .iter()
            .find(|entry| entry.state.id == id)
            // 镜像 Redis 的 TTL：过了保活窗口就是**没了**，不是"陈旧"。
            .filter(|entry| entry.retain_til > now)
            .ok_or(SessionNotFound)?;
        if entry.state.workspace_id != workspace_id {
            return Err(SessionNotFound);
        }
        Ok(entry.state.clone())
    }

    async fn mark_terminal(
        &self,
        id: &str,
        outcome: InstallSessionOutcome,
        ttl: Duration,
    ) -> Result<(), SessionNotFound> {
        let now = (self.now)();
        let mut entries = self.entries.lock().await;
        let entry = entries
            .iter_mut()
            .find(|entry| entry.state.id == id)
            .filter(|entry| entry.retain_til > now)
            .ok_or(SessionNotFound)?;
        // 首次写入胜 —— 第二次终态写**不动**已记录的结局。
        // retention 仍然移动（与 Redis 脚本里那句无条件 EXPIRE 对齐）⇒ 两个实现
        // 对"一次重试做了什么"的答案一致。
        if entry.state.status == SessionStatus::Pending {
            entry.state.status = outcome.status;
            entry.state.installation_id = outcome.installation_id;
            entry.state.error_reason = outcome.error_reason;
            entry.state.error_message = outcome.error_message;
        }
        entry.retain_til = now + chrono::Duration::from_std(ttl).unwrap_or_default();
        Ok(())
    }
}
