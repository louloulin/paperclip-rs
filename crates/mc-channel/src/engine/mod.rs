//! `engine`：把 [`crate::channel::Channel`] 接到 Multica 的 DB / chat / issue / task 面上的
//! **核心**（上游 `server/internal/integrations/channel/engine/`，5,315 行里的运行时那一半）。
//!
//! - **写者**：M7-0 anchor 建文件与边界；**M7-1 落 `mod.rs` / `router.rs` / `supervisor.rs` /
//!   `resolvers.rs`**（`docs/60` §3.3 的写集表）。
//! - **M7-2 落**：`session.rs` / `batcher.rs` / `lease.rs` / `commands.rs`（**它自己**在本文件
//!   追加四行 `pub mod …;`，见下面「两个 stage-2 写者」）。
//!
//! # 为什么 engine 独立成模块而不是塞进 adapter
//!
//! 上游 `channel/engine/` 有 4,582 行，且是**跨渠道共享**的：路由、会话、去重、命令、媒体解析、
//! 租约、退避重连。五个 adapter 各 1.8k–3.4k 行，但**都不重实现**这些语义 —— 边界就是本模块：
//! engine 只认 `Channel` 与 `mc_core::channel::message::*`。
//!
//! # 边界契约（`docs/60` §2.6 第 1 条，逐条可测）
//!
//! `src/engine/**` **不得** `use` 任何 `slack` / `lark` / `dingtalk` / `wecom` /
//! `telegram` 具体类型；反向同理，adapter **不得**直接写 DB（手上只有 `Arc<dyn …>` 端口）。
//!
//! # 两个 stage-2 写者（交接纪律，见 `docs/32` §10）
//!
//! | 文件 | 写者 | 内容 |
//! | --- | :-: | --- |
//! | `mod.rs` / `router.rs` / `supervisor.rs` / `resolvers.rs` | **M7-1** | 端口 + 流水线 + 监管 |
//! | `session.rs` / `batcher.rs` / `lease.rs` / `commands.rs` | **M7-2** | 会话状态机 / 去抖 / 租约实现 / 命令表 |
//!
//! ⚠️ M7-2 落自己的四个文件时**由它**在本文件追加四行 `pub mod …;`（anchor 的写集不含那四个
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

// M7-2（`LUM-1767`）落自己的四个文件，并**由它**在这里追加下面四行
// （anchor 的写集不含那四个文件 ⇒ 这里不预声明；M7-1 与 M7-2 同 stage，先起跑的追加、
// 另一片 rebase —— 本片与 M7-1 的约定逐字一致）。
pub mod batcher;
pub mod commands;
pub mod lease;
pub mod session;

use std::sync::Arc;

use crate::message::SharedInboundHandler;
use crate::registry::Registry;

pub use resolvers::OutboundReplier;
pub use resolvers::{
    AppendParams, AppendResult, Auditor, BindMediaParams, BindMediaResult, ChannelIssue,
    ChannelIssueCommand, ChannelIssueOutcome, ChannelIssueParams, ChatRunParams, CommandClassifier,
    CommandIntent, Deduper, DropReason, EngineError, EngineResult, EnsureSessionParams,
    IdentityResolver, InstallationResolver, IssueCreator, MediaIntentLedger, MediaResolver,
    NoCommands, Outcome, PendingContext, PipelineError, RecordPendingMediaObjectParams,
    ResolvedIdentity, ResolvedInstallation, ResolverSet, RouteResult, RunTriggerer, SessionBinder,
    SessionReader, StartSessionParams, StartSessionResult, TypingNotifier, WorkspaceIdentity,
};
pub use router::{Router, RouterConfig, DEFAULT_MEDIA_TIMEOUT, NO_RESOLVER_SET};
pub use supervisor::{
    AcquireLeaseParams, Backoff, Config, Installation, InstallationStore, LeaseStore, NowFn,
    ReleaseLeaseParams, Supervisor, SupervisorHandle, DEFAULT_LEASE_TTL, DEFAULT_POLL_INTERVAL,
    DEFAULT_SHUTDOWN_TIMEOUT,
};

// —— M7-2 的四份交付：去抖触发、命令/标题、进程内租约、会话状态机与三个端口适配 ——
pub use batcher::{
    RunBatcher, TimerHandle, TimerScheduler, TokioTimers, DEFAULT_CHAT_RUN_BATCH_WINDOW,
};
pub use commands::{
    chat_title_source, derive_chat_title, derive_first_message_title, media_type_title,
    parse_control_command, parse_fresh_session_command, parse_issue_command,
    parse_new_chat_command, task_input_is_channel_ingested, ChannelCommandClassifier,
    ControlCommand, ControlCommandKind, DETERMINISTIC_TITLE_LIMIT,
};
pub use lease::{InProcessLeaseStore, LeaseMetrics, LeaseMetricsSnapshot};
pub use session::{
    drop_from_message, plan_pending_contexts, AuditStore, BindingKeyPolicy, ChannelAuditor,
    ChannelDeduper, ChannelSessionBinder, ContextWindow, DedupStore, PendingContextPlan,
    SessionBinderConfig,
};

/// engine 装配时需要的**全部**外部依赖（端口 + 共享入站入口）。
///
/// 只放 `Arc<…>`：adapter 的 `register(registry, deps)` 拿到的就是它，于是"adapter **不得**
/// 直接写 DB"成为**类型层面**的事实（它手上只有 trait 对象），且"入站汇进同一个 handler"
/// 是一次 `Arc::clone`（`deps.handler()`）。
pub struct ChannelDeps {
    /// engine 的共享入站入口：**唯一**的 `InboundHandler`（`Router` 实现它）。
    pub router: Arc<Router>,
    /// 安装行读取（`channel_installation`）。
    pub installations: Arc<dyn InstallationStore>,
    /// 长连接租约（无 Redis ⇒ 进程内实现，见 `docs/60` §2.5）。
    pub leases: Arc<dyn LeaseStore>,
}

impl std::fmt::Debug for ChannelDeps {
    /// trait 对象不可打印 ⇒ 只列出端口的**存在性**。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelDeps")
            .field("router", &self.router)
            .field("installations", &"<dyn InstallationStore>")
            .field("leases", &"<dyn LeaseStore>")
            .finish()
    }
}

impl ChannelDeps {
    /// 装配（端口 + 共享入口）。
    pub fn new(
        router: Arc<Router>,
        installations: Arc<dyn InstallationStore>,
        leases: Arc<dyn LeaseStore>,
    ) -> Self {
        Self {
            router,
            installations,
            leases,
        }
    }

    /// 注入给每个 adapter 的共享入站句柄（同一个 handler，5 个 adapter 各一次 `Arc::clone`）。
    pub fn handler(&self) -> SharedInboundHandler {
        self.router.clone()
    }
}

/// 渠道引擎：路由 + 监管 + 解析的**装配体**（上游 `engine.Engine` 的等价物）。
///
/// ⚠️ 装配**不**启动任何连接：[`Supervisor::spawn`] 是宿主
/// （`apps/mc-server/src/channels.rs`）的事。
pub struct Engine {
    registry: Arc<Registry>,
    deps: Arc<ChannelDeps>,
    router: Arc<Router>,
    supervisor: Arc<Supervisor>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Engine")
            .field("registry", &self.registry)
            .field("deps", &self.deps)
            .field("supervisor", &self.supervisor)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// 装配（不启动任何连接；`Supervisor::new` 会校验时间不变式）。
    ///
    /// # Errors
    ///
    /// 配置时间关系非法（`poll <= renew < ttl` 不成立）时返回链路错误。
    pub fn new(
        registry: Arc<Registry>,
        deps: Arc<ChannelDeps>,
    ) -> crate::channel::ChannelResult<Self> {
        let supervisor = Arc::new(
            Supervisor::new(
                Arc::clone(&deps.installations),
                Arc::clone(&deps.leases),
                Arc::clone(&registry),
                deps.handler(),
                Config::default(),
            )
            .map_err(EngineError::into_channel_error)?,
        );
        let router = Arc::clone(&deps.router);
        Ok(Self {
            registry,
            deps,
            router,
            supervisor,
        })
    }

    /// 注册表（宿主在装配后仍要读它决定起哪些连接）。
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// 依赖袋。
    pub fn deps(&self) -> &Arc<ChannelDeps> {
        &self.deps
    }

    /// 入站路由（`Router::route` 的薄封装；adapter 调 `deps.handler()` 也一样）。
    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    /// 长连接监管器（宿主拿它 `spawn` / 停机）。
    pub fn supervisor(&self) -> &Arc<Supervisor> {
        &self.supervisor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **边界契约的源码扫描**（`docs/60` §2.6 第 1 条）：engine 的四个文件**不得**出现任何
    /// `slack` / `lark` / `dingtalk` / `wecom` / `telegram` 具体类型引用。
    ///
    /// 这是唯一能真正钉住"engine 不知道平台"的形态：`use` 一个平台类型会让这条断言红。
    #[test]
    fn engine_never_names_a_platform_module() {
        let engine_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/engine");
        let mut files = 0;
        for entry in std::fs::read_dir(&engine_dir).expect("engine dir") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            files += 1;
            let raw = std::fs::read_to_string(&path).expect("read engine file");
            // 只看非测试代码：本测试自己的 needle 列表就在 `#[cfg(test)]` 模块里。
            let source = raw.split("\n#[cfg(test)]").next().unwrap_or_default();
            for needle in ["slack::", "lark::", "dingtalk::", "wecom::", "telegram::"] {
                assert!(
                    !source.contains(needle),
                    "engine 引用平台类型：{} 里出现 {needle}",
                    path.display()
                );
            }
        }
        assert_eq!(files, 8, "engine 目录 = M7-1 的四个文件 + M7-2 的四个文件");
    }

    /// `ChannelDeps::handler()` 与 `deps.router` 是同一个 handler（一次 `Arc::clone`）。
    #[test]
    fn deps_handler_is_the_router() {
        let router = Arc::new(Router::new(
            Arc::new(NoCommands),
            Arc::new(StubTrigger),
            Arc::new(StubReader),
            Arc::new(StubIssues),
            RouterConfig::default(),
        ));
        let deps = ChannelDeps::new(
            Arc::clone(&router),
            Arc::new(StubInstalls),
            Arc::new(StubLeases),
        );
        let handler = deps.handler();
        assert!(Arc::ptr_eq(
            &handler,
            &(Arc::clone(&router) as SharedInboundHandler)
        ));
        assert!(deps.router.kinds().is_empty());
        assert!(format!("{deps:?}").contains("<dyn InstallationStore>"));
    }

    /// `Engine::new` 能装配（不再有 `todo!()`），且暴露的三件套非空。
    #[test]
    fn engine_assembles_without_starting_connections() {
        let router = Arc::new(Router::new(
            Arc::new(NoCommands),
            Arc::new(StubTrigger),
            Arc::new(StubReader),
            Arc::new(StubIssues),
            RouterConfig::default(),
        ));
        let deps = ChannelDeps::new(
            Arc::clone(&router),
            Arc::new(StubInstalls),
            Arc::new(StubLeases),
        );
        let engine = Engine::new(Arc::new(Registry::new()), Arc::new(deps)).expect("engine");
        assert!(engine.registry().is_empty());
        assert!(engine.supervisor().supervised().is_empty());
        assert!(Arc::ptr_eq(engine.router(), &router));
        assert!(!format!("{engine:?}").is_empty());
    }

    // ---- 端口替身（只为本文件的装配断言服务） ----

    use crate::engine::resolvers::{
        ChannelIssueOutcome, ChatRunParams, EnsureSessionParams, ResolvedIdentity,
        ResolvedInstallation, StartSessionParams,
    };
    use async_trait::async_trait;
    use mc_core::channel::message::InboundMessage;
    use mc_core::id::Id;

    struct StubTrigger;

    #[async_trait]
    impl RunTriggerer for StubTrigger {
        async fn schedule_chat_run(&self, _params: ChatRunParams) -> EngineResult<()> {
            Ok(())
        }
        async fn drain(&self) -> EngineResult<()> {
            Ok(())
        }
    }

    struct StubReader;

    #[async_trait]
    impl SessionReader for StubReader {
        async fn workspace_identity(&self, _workspace_id: Id) -> EngineResult<WorkspaceIdentity> {
            Ok(WorkspaceIdentity::default())
        }
    }

    struct StubIssues;

    #[async_trait]
    impl IssueCreator for StubIssues {
        async fn create_issue(
            &self,
            _params: ChannelIssueParams,
        ) -> EngineResult<ChannelIssueOutcome> {
            Ok(ChannelIssueOutcome {
                issue: ChannelIssue {
                    id: Id::new(),
                    number: 1,
                    title: "t".into(),
                },
                duplicate: false,
                assigned_task_id: None,
            })
        }
    }

    struct StubInstalls;

    #[async_trait]
    impl InstallationStore for StubInstalls {
        async fn list_active(&self) -> EngineResult<Vec<Installation>> {
            Ok(Vec::new())
        }
    }

    struct StubLeases;

    #[async_trait]
    impl LeaseStore for StubLeases {
        async fn list_held(&self, _ids: &[Id]) -> EngineResult<std::collections::HashSet<Id>> {
            Ok(std::collections::HashSet::new())
        }
        async fn try_acquire(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
            Ok(())
        }
        async fn renew(&self, _params: AcquireLeaseParams) -> EngineResult<()> {
            Ok(())
        }
        async fn release(&self, _params: ReleaseLeaseParams) -> EngineResult<()> {
            Ok(())
        }
    }

    /// 端口替身的签名完整性（本文件只编译它们，语义由各片自己的用例钉）。
    #[test]
    fn stub_ports_implement_the_contracts() {
        fn assert_ports<T: Send + Sync>() {}
        assert_ports::<StubTrigger>();
        assert_ports::<StubReader>();
        assert_ports::<StubIssues>();
        assert_ports::<StubInstalls>();
        assert_ports::<StubLeases>();
        let _ = core::mem::size_of::<EnsureSessionParams>();
        let _ = core::mem::size_of::<StartSessionParams>();
        let _ = core::mem::size_of::<ResolvedInstallation>();
        let _ = core::mem::size_of::<ResolvedIdentity>();
        let _ = core::mem::size_of::<InboundMessage>();
        assert_eq!(DropReason::Duplicate.as_str(), "duplicate");
    }
}
