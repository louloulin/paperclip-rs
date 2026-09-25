//! 入站媒体面的两个本地结构：**按会话保序的脱离式队列** + 一条媒体任务的入参。
//!
//! - **写者**：M7-1（`docs/60` §3.3）。上游 `engine/router.go` 的 `mediaQueueEntry` /
//!   `enqueueMediaJob` / `resolveAndBindMedia` 一批。
//! - 拆出本文件是门 ⑩ 的要求（`router.rs` 超 800 行 ⇒ 拆）。
//! - **纪律**：媒体**脱离** connector 的 ACK 路径（`tokio::spawn`）、**按会话保序**
//!   （每会话一把异步锁）、受**全局并发**（信号量）与**自己的预算**（deadline）约束；
//!   预算耗尽只做**空收尾**（清 pending 标记），远端解析跳过。
//!   `resolve_media` 的失败**绝不**在行内删东西：上传对象都有先于 PUT 写的意图账本行。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use mc_core::id::Id;
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};

use mc_core::channel::message::InboundMessage;

use crate::engine::resolvers::{ChannelIssueCommand, MediaResolver, ResolverSet};

/// 每条会话一条的媒体串行化锁。
pub(super) struct MediaQueue {
    pub(super) sem: Arc<Semaphore>,
    pub(super) sessions: StdMutex<HashMap<Id, Arc<AsyncMutex<()>>>>,
    pub(super) inflight: AtomicUsize,
    pub(super) idle: Notify,
    pub(super) stopping: AtomicBool,
}

impl MediaQueue {
    pub(super) fn new(concurrency: usize) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(concurrency.max(1))),
            sessions: StdMutex::new(HashMap::new()),
            inflight: AtomicUsize::new(0),
            idle: Notify::new(),
            stopping: AtomicBool::new(false),
        }
    }

    /// 会话锁（同一会话的媒体严格保序；不同会话互不阻塞）。
    pub(super) fn session_lock(&self, session_id: Id) -> Arc<AsyncMutex<()>> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            sessions
                .entry(session_id)
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    }

    pub(super) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// 停机：置停止位并**等**在飞任务排空（调用方负责加时限）。
    pub(super) async fn drain(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        while self.inflight.load(Ordering::SeqCst) > 0 {
            self.idle.notified().await;
        }
    }

    pub(super) fn finish_one(&self) {
        if self.inflight.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }
}

/// 一条脱离式媒体任务的入参（把十个形参收进一个结构：见 `enqueue_media` 的调用点）。
pub(super) struct MediaJob {
    pub(super) set: Arc<ResolverSet>,
    pub(super) media: Arc<dyn MediaResolver>,
    pub(super) installation: crate::engine::resolvers::ResolvedInstallation,
    pub(super) identity: crate::engine::resolvers::ResolvedIdentity,
    pub(super) message: InboundMessage,
    pub(super) session_id: Id,
    pub(super) chat_message_id: Option<Id>,
    pub(super) issue_id: Option<Id>,
    pub(super) issue_description_base: Option<String>,
    pub(super) issue_command_text: String,
    pub(super) deadline: tokio::time::Instant,
    pub(super) resolve_remote: bool,
}

impl MediaJob {
    /// `None` = 该平台没有媒体面（`ResolverSet::media` 是 `None`）⇒ 不排任务。
    pub(super) fn new(
        set: &Arc<ResolverSet>,
        installation: &crate::engine::resolvers::ResolvedInstallation,
        identity: &crate::engine::resolvers::ResolvedIdentity,
        message: &InboundMessage,
        session_id: Id,
        chat_message_id: Option<Id>,
        deadline: tokio::time::Instant,
    ) -> Option<Self> {
        Some(Self {
            set: Arc::clone(set),
            media: set.media.clone()?,
            installation: installation.clone(),
            identity: identity.clone(),
            message: message.clone(),
            session_id,
            chat_message_id,
            issue_id: None,
            issue_description_base: None,
            issue_command_text: message.command_text.clone(),
            deadline,
            resolve_remote: false,
        })
    }

    /// 媒体归这次 `/issue` 建出来的 issue（而不是 `chat_message`）。
    #[must_use]
    pub(super) fn with_issue(mut self, issue_id: Id, command: &ChannelIssueCommand) -> Self {
        self.issue_id = Some(issue_id);
        self.issue_description_base = Some(command.description.clone());
        self
    }

    /// 是否跑远端解析（下载 + 上传）；`false` 只做空收尾（清 pending 标记）。
    #[must_use]
    pub(super) fn remote(mut self, resolve: bool) -> Self {
        self.resolve_remote = resolve;
        self
    }
}
