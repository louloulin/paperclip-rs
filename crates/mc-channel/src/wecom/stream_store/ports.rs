//! `stream_store` 与后续片（M7-17 / M7-20）之间的**端口**，以及诊断读数。
//!
//! 本文件是 `stream_store.rs` 的子模块：拆分依据是 `docs/60-M7-PLAN.md` §6.3 的强制拆分
//! 加上门 ⑩ 的 800 行硬限。逐条清单见 `docs/32` §33 的 D10。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::id::Id;

use super::{RoundKey, RoundTurn, StreamHandle, StreamStore};

/// 一次收尾要的那一半外部世界（上游 `seal` 用的 `sendersRegistry`：`stream` /
/// `streamRewrite` / `recordEnding`）。本地落点是 M7-20 的 `senders.rs`。
///
/// 收窄成三条正是为了让本片**不依赖**注册表：`sendersRegistry` 的其余能力是
/// "按 installation 找活的 socket"（M7-20）与"流句柄的过期检查"（M7-17），
/// 而 `seal` 只问这三件。
#[async_trait]
pub trait StreamSender: Send + Sync {
    /// 写一帧流帧（上游 `sendersRegistry.stream`）。
    ///
    /// # Errors
    ///
    /// 发送侧的任意失败。
    async fn stream(
        &self,
        handle: &StreamHandle,
        text: &str,
        finish: bool,
    ) -> Result<(), crate::wecom::ws_sender::SenderError>;

    /// 把**同一帧**再写一遍（上游 `sendersRegistry.streamRewrite`）。
    ///
    /// # Errors
    ///
    /// 发送侧的任意失败。
    async fn stream_rewrite(
        &self,
        handle: &StreamHandle,
        text: &str,
        finish: bool,
    ) -> Result<(), crate::wecom::ws_sender::SenderError>;

    /// 记一次结束（上游 `sendersRegistry.recordEnding`）：`None` = 气泡正常收尾。
    fn record_ending(&self, error: Option<&crate::wecom::ws_sender::SenderError>);
}

/// 把一次收尾的 task id 解析回它所属那一轮的 root task id（上游 `roundTaker` 的
/// `taskLookup.GetAgentTask` → `ChatInputTaskID`）。
///
/// 上游逐字：这条查询要花**一次读**，而存储只在"在一个还有轮次开着的会话里查不着"时才问它；
/// 没有配置查询时，查不着就是查不着。
#[async_trait]
pub trait RootResolver: Send + Sync {
    /// 一个 task 所属的输入批次 —— 首次尝试是它自己的 id，自动重试的 clone 是父亲的 id。
    ///
    /// # Errors
    ///
    /// 读库失败；调用方按"查不着"处理（上游只记一条 debug 日志）。
    async fn root_task_id(&self, task_id: &str) -> Option<String>;
}

/// 把 task 生命周期事件匹配到它所属的轮次（上游 `roundTaker`）。存储的两种身份都活在它后面：
/// `task:queued` 归档的那个绑定，以及把那一个自动重试 clone 解回来的**那一列**。
pub struct RoundTaker {
    /// 没有存储就内联回复被禁用：什么都找不到（上游 `roundTaker.streams == nil`）。
    streams: Option<Arc<StreamStore>>,
    resolver: Option<Arc<dyn RootResolver>>,
}

impl RoundTaker {
    /// 建一个收尾器。
    #[must_use]
    pub fn new(streams: Arc<StreamStore>, resolver: Option<Arc<dyn RootResolver>>) -> Self {
        Self {
            streams: Some(streams),
            resolver,
        }
    }

    /// 一个**没有存储**的收尾器（标签用：什么都找不到）。
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            streams: None,
            resolver: None,
        }
    }

    /// [`RoundTaker`] 的唯一职责：在存储上 `take`，并带上自动重试的血缘查询。
    ///
    /// 事件上的 id 先试，因为那是 `bind_next` 归档的 id。`retry_unbind` 通常把轮次直接交给
    /// clone 自己的 `task:queued`，所以 clone 的收尾在第一次就匹配上；那一列是这条背带的
    /// **一根腰带**。
    pub async fn take(&self, session: Id, key: &RoundKey) -> (Option<RoundTurn>, bool) {
        let Some(streams) = self.streams.as_ref() else {
            return (None, false);
        };
        streams.take(session, key, self.resolver.as_deref()).await
    }
}

/// 一个"什么都没有"的根解析器（生产里没有配置血缘查询时的取值；也是用例的默认值）。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRootResolver;

#[async_trait]
impl RootResolver for NoRootResolver {
    async fn root_task_id(&self, _task_id: &str) -> Option<String> {
        None
    }
}

/// 一个按表回答的根解析器（上游 `taskLookup` 的**替身**，给用例与 M7-17 的早期接线用）。
pub struct StaticRootResolver {
    roots: HashMap<String, String>,
}

impl StaticRootResolver {
    /// 从 `(task_id, root_task_id)` 对建。
    #[must_use]
    pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            roots: pairs.into_iter().collect(),
        }
    }
}

#[async_trait]
impl RootResolver for StaticRootResolver {
    async fn root_task_id(&self, task_id: &str) -> Option<String> {
        self.roots.get(task_id).cloned()
    }
}

/// 一个会话的轮次里**所有** task id（诊断与用例用）。
#[must_use]
pub fn rounds_task_ids(store: &StreamStore, session: Id) -> Vec<String> {
    let inner = store.lock();
    inner
        .sessions
        .get(&session)
        .map(|rounds| rounds.iter().map(|entry| entry.task_id.clone()).collect())
        .unwrap_or_default()
}

/// 一个会话里"从没绑过 run 的轮次"的个数（诊断与用例用）。
#[must_use]
pub fn collecting_count(store: &StreamStore, session: Id) -> usize {
    let inner = store.lock();
    let Some(rounds) = inner.sessions.get(&session) else {
        return 0;
    };
    rounds
        .iter()
        .filter(|entry| entry.task_id.is_empty() && !entry.ever_bound)
        .count()
}

/// 一个会话的 `pending` 队列（诊断与用例用）。
#[must_use]
pub fn pending_task_ids(store: &StreamStore, session: Id) -> Vec<String> {
    let inner = store.lock();
    inner
        .pending
        .get(&session)
        .map(|queue| {
            queue
                .iter()
                .map(|pending| pending.task_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// 一个会话的 `finished` 环（诊断与用例用）。
#[must_use]
pub fn finished_task_ids(store: &StreamStore, session: Id) -> Vec<String> {
    let inner = store.lock();
    inner
        .finished
        .get(&session)
        .map(|ring| ring.tasks.clone())
        .unwrap_or_default()
}

/// 某个会话里画过的轮次的 stream id（诊断与用例用）。
#[must_use]
pub fn sealed_streams(store: &StreamStore, session: Id) -> HashSet<String> {
    let inner = store.lock();
    let Some(rounds) = inner.sessions.get(&session) else {
        return HashSet::new();
    };
    rounds
        .iter()
        .filter(|entry| entry.painted)
        .map(|entry| entry.handle.stream_id.clone())
        .collect()
}
