//! `engine`：把 `Channel` 接到 Multica 的 DB / chat / issue / task 面上的**核心**。
//!
//! **状态：M7-0 anchor 只落文件与边界**（`LUM-1765` / `docs/60-M7-PLAN.md` §5）——
//! 本文件只有模块声明与装配类型；`router.rs` / `resolvers.rs` 是**签名 + `todo!()` 位**，
//! `supervisor.rs` 定的是**端口**（trait），实现归 M7-1 / M7-2。
//!
//! # 为什么 engine 独立成模块而不是塞进 adapter
//!
//! 上游 `channel/engine/` 有 4,582 行（`channel/` 总共 5,315 行），且是**跨渠道共享**的：
//! 路由、会话、去重、命令、媒体解析、租约、退避重连。五个 adapter 各 1.8k–3.4k 行，
//! 但它们**都不重实现**这些语义 —— 边界就是本模块：engine 只认
//! [`crate::channel::Channel`] 与 `mc_core::channel::message::*`。
//!
//! # 两个 stage-2 写者（交接纪律，见 `docs/32` §10）
//!
//! | 文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `mod.rs` | **M7-1**（anchor 只落本文件的骨架） | `Engine` / [`ChannelDeps`] / 模块声明 |
//! | `router.rs` | M7-1 | 入站分类：dedup → 身份 → 命令 → 建 issue/task → 触发 run |
//! | `supervisor.rs` | M7-1 | 退避重连 + 租约 + `InstallationStore` / `LeaseStore` **端口** |
//! | `resolvers.rs` | M7-1 | 把平台路由键解析成 installation / `chat_session`（唯一实现点） |
//! | `session.rs` / `batcher.rs` / `lease.rs` / `commands.rs` | **M7-2** | 会话状态机 / 批量 / 租约实现 / 命令表 |
//!
//! ⚠️ M7-2 落自己的四个文件时，**由它在本文件追加四行 `pub mod …;`**（anchor 的写集不含那四个
//! 文件，所以这里**不**预声明它们；M7-1 与 M7-2 同 stage ⇒ 先起跑的那片追加、另一片 rebase）。
//!
//! # 长连接宿主**不在这里**
//!
//! `Supervisor::spawn` 的调用方是 `apps/mc-server/src/channels.rs`（`docs/60` §2.4）：
//! 停机顺序固定为「先停渠道连接 → 再停调度器 → 最后停 actor」，所以宿主必须是那个进程的
//! 一步，而不是 `mc-http` 里的一层中间件。

pub mod resolvers;
pub mod router;
pub mod supervisor;

use std::sync::Arc;

use crate::message::SharedInboundHandler;
use crate::registry::Registry;

pub use resolvers::ResolverSet;
pub use router::Router;
pub use supervisor::{InstallationStore, LeaseStore, Supervisor, SupervisorHandle};

/// engine 装配时需要的**全部**外部依赖（端口 + 共享 handler）。
///
/// 只放 `Arc<dyn …>`：adapter 的 `register(registry, deps)` 拿到的就是它，
/// 于是"adapter 不得直接写 DB"成为**类型层面**的事实（它手上只有 trait 对象）。
pub struct ChannelDeps {
    /// engine 的共享入站入口（adapter 在接收循环里调它）。
    pub handler: SharedInboundHandler,
    /// 安装行读取（`channel_installation`）。
    pub installations: Arc<dyn InstallationStore>,
    /// 长连接租约（无 Redis ⇒ 进程内实现，见 `docs/60` §2.5）。
    pub leases: Arc<dyn LeaseStore>,
}

impl std::fmt::Debug for ChannelDeps {
    /// trait 对象不可打印 ⇒ 只列出三个端口的**存在性**。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelDeps")
            .field("handler", &"<dyn InboundHandler>")
            .field("installations", &"<dyn InstallationStore>")
            .field("leases", &"<dyn LeaseStore>")
            .finish()
    }
}

/// 渠道引擎：路由 + 监管 + 解析的**装配体**（上游 `engine.Engine`）。
///
/// ⚠️ anchor 期只构造到这一步：字段是"装配好的依赖 + 注册表"。M7-1 会把
/// `router` / `supervisor` 的真实内部状态（会话、批量、媒体账本）挂到这个结构上；
/// M7-2 再加会话与租约的实现。
pub struct Engine {
    registry: Arc<Registry>,
    deps: Arc<ChannelDeps>,
    router: Router,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Engine")
            .field("registry", &self.registry)
            .field("deps", &self.deps)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// 装配（不启动任何连接）。
    ///
    /// # Panics
    ///
    /// anchor 期本函数**未实现**（`todo!()`）：路由与会话的装配归 M7-1
    /// （`docs/60` §4.1 的 M7-1 行）。**调用它一定 panic** —— 这是刻意的：
    /// 让它静默返回一个空壳会让"engine 还没接上"变成运行期才发现的事。
    pub fn new(_registry: Arc<Registry>, _deps: Arc<ChannelDeps>) -> Self {
        todo!("M7-1：装配 router / 会话 / 批量（docs/60 §4.1 的 M7-1 行）")
    }

    /// 注册表（宿主在装配后仍要读它决定起哪些连接）。
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// 依赖袋。
    pub fn deps(&self) -> &Arc<ChannelDeps> {
        &self.deps
    }

    /// 入站路由（M7-1 的 `Router::route` 薄封装）。
    pub fn router(&self) -> &Router {
        &self.router
    }
}
