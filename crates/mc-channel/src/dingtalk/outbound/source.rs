//! 引用源的**进程内**缓存（上游 `reply_source.go` 127 行）。
//!
//! - **写者**：M7-8（`docs/32` §22 的 D1；门 ⑩ 的切分）。
//! - **它是什么**：把"被受理的输入"与"它的提供方坐标（会话 id + 消息 id）"关联起来，
//!   **只**用于可选的完成表情（Done）。它从不修复共享路由、从不持久化回调数据、重启后也不恢复。
//! - **接线点仍是交接项**：上游在 resolver 的 append 成功之后调 `rememberReplySource`
//!   （`resolvers.go:391/486`）；本仓的写入点在宿主 / M7-9 的 binder 路径，见 `mod.rs`
//!   的交接表第 3 条。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mc_core::channel::message::{InboundMessage, Source};
use mc_core::id::Id;

use crate::dingtalk::outbound::SendTarget;

// =====================================================================
// 引用源缓存（上游 `reply_source.go`）
// =====================================================================

/// 一条被受理输入的**进程内**提供方锚点（上游 `replySource`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplySource {
    /// 被受理输入的主键（`chat_message.id`；缓存就按它索引）。
    pub input_id: Id,
    pub installation_id: Id,
    pub session_id: Id,
    /// 源消息 id（只有它 + 路由身份会被用来回贴表情）。
    pub message_id: String,
    /// 源消息的路由身份。
    pub source: Source,
}

impl ReplySource {
    /// 就位构造（上游 `rememberReplySource` 的形参顺序）。
    #[must_use]
    pub fn new(
        installation_id: Id,
        input_id: Id,
        session_id: Id,
        message: &InboundMessage,
    ) -> Self {
        Self {
            input_id,
            installation_id,
            session_id,
            message_id: message.message_id.clone(),
            source: message.source.clone(),
        }
    }

    /// 回贴表情的目标（上游把 `source.message` 直接交给 `react`）。
    #[must_use]
    pub fn reaction_target(&self) -> SendTarget {
        SendTarget::reaction(&self.source, &self.message_id)
    }
}

/// 有界的引用源缓存（上游 `replySourceCache`）。
///
/// 它把"这条输入"与"它的提供方坐标"关联起来，**只**用于可选的完成表情：它从不修复共享
/// 路由、从不持久化回调数据、重启后也不恢复。丢掉一条 ⇒ 那条输入的完成表情被抑制
/// （不是错误）。缓存上限 [`MAX_REPLY_SOURCES`]，按 FIFO 环形淘汰。
#[derive(Debug, Clone)]
pub struct ReplySourceCache {
    inner: Arc<Mutex<ReplySourceState>>,
}

#[derive(Debug, Default)]
struct ReplySourceState {
    entries: HashMap<Id, ReplySource>,
    order: Vec<Id>,
    next: usize,
    sessions: HashMap<Id, usize>,
}

/// 缓存上限（上游 `maxReplySources = 1024`）。
pub const MAX_REPLY_SOURCES: usize = 1024;

impl Default for ReplySourceCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplySourceCache {
    /// 空缓存。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(ReplySourceState::default())),
        }
    }

    /// 记住一次受理（上游 `rememberReplySource`）。
    ///
    /// 两个前置条件必须有值 —— 缺一个就不记（上游逐字）。
    pub fn remember(&self, source: ReplySource) {
        if source.message_id.is_empty() || source.source.chat_id.is_empty() {
            return;
        }
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        let input_id = source.input_id;
        if state.entries.contains_key(&input_id) {
            return;
        }
        if state.order.len() < MAX_REPLY_SOURCES {
            state.order.push(input_id);
        } else {
            let next = state.next;
            let victim = state.order[next];
            if let Some(entry) = state.entries.remove(&victim) {
                release_session(&mut state, entry.session_id);
            }
            state.order[next] = input_id;
            state.next = (next + 1) % MAX_REPLY_SOURCES;
        }
        let session_id = source.session_id;
        state.entries.insert(input_id, source);
        *state.sessions.entry(session_id).or_insert(0) += 1;
    }

    /// 取一条（安装必须**逐字**相符：上游 `source.installationID == installationID`）。
    #[must_use]
    pub fn source_for(&self, installation_id: Id, input_id: Id) -> Option<ReplySource> {
        let state = self.inner.lock().ok()?;
        let source = state.entries.get(&input_id)?;
        if source.installation_id == installation_id {
            Some(source.clone())
        } else {
            None
        }
    }

    /// 按 **`(安装, 消息 id, 路由身份)`** 反查输入 id（上游 `replyInputFor`）。
    #[must_use]
    pub fn input_for(&self, installation_id: Id, message: &InboundMessage) -> Option<Id> {
        let state = self.inner.lock().ok()?;
        state
            .entries
            .iter()
            .find(|(_, source)| {
                source.installation_id == installation_id
                    && source.message_id == message.message_id
                    && source.source == message.source
            })
            .map(|(id, _)| *id)
    }

    /// 提交前登记兴趣，返回"在飞引用"的释放句柄（上游 `beginReplyInput`）。
    ///
    /// ⚠️ 调用方**必须**在提交 / 取到源之后、或失败时调用返回的闭包。保留下来的引用归有界
    /// 缓存所有；并发调用各持各的引用，因此一次失败的追加不会掩盖另一条被受理的输入。
    #[must_use]
    pub fn begin_input(&self, session_id: Id) -> Box<dyn FnOnce() + Send> {
        let inner = Arc::clone(&self.inner);
        if let Ok(mut state) = inner.lock() {
            *state.sessions.entry(session_id).or_insert(0) += 1;
        }
        Box::new(move || {
            if let Ok(mut state) = inner.lock() {
                release_session(&mut state, session_id);
            }
        })
    }

    /// 本会话还有在飞 / 已保留的引用吗（上游 `hasReplySession`）。
    #[must_use]
    pub fn has_session(&self, session_id: Id) -> bool {
        self.inner
            .lock()
            .is_ok_and(|state| state.sessions.get(&session_id).copied().unwrap_or(0) > 0)
    }

    /// 当前缓存的条数（诊断 / 用例）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |state| state.entries.len())
    }

    /// 缓存是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 释放一次会话引用（调用方持有锁）。
fn release_session(state: &mut ReplySourceState, session_id: Id) {
    match state.sessions.get(&session_id).copied() {
        Some(count) if count > 1 => {
            state.sessions.insert(session_id, count - 1);
        }
        _ => {
            state.sessions.remove(&session_id);
        }
    }
}
