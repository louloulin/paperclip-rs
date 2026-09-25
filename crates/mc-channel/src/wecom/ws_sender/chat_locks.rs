//! 每个目标聊一把锁（上游 `chatLocks` / `chatLock`）。
//!
//! 本文件是 `ws_sender.rs` 的子模块：拆分的理由是 `docs/60-M7-PLAN.md` §6.3 要求把
//! 1,187 行的上游 `ws_frame.go` 按「帧编解码 / 帧路由」拆开，加上门 ⑩ 的 800 行硬限。
//! 逐条清单见 `docs/32` §33 的 D10。

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout_at;

use super::{deadline_or_far, Deadline, SenderError};

// =====================================================================
// 每个聊一把锁
// =====================================================================

/// 每个**目标聊**一把锁，按需建、最后一个持有者离开时丢掉（上游 `chatLocks`）——
/// 所以一个跟很多聊说过话的进程不会为每个聊永远留一个条目。
///
/// **按聊而不是按连接**是故意的：给另一个房间的第二条回答没有理由排在这条后面；而 ping
/// 循环走 `request`/`write`、**从不**取聊锁，所以它不会被一次发送拖住。
#[derive(Debug, Default)]
pub struct ChatLocks {
    inner: Mutex<HashMap<String, Arc<ChatLock>>>,
}

#[derive(Debug)]
struct ChatLock {
    sem: Arc<Semaphore>,
    refs: AtomicUsize,
}

/// 持有一把聊锁（上游 `acquire` 返回的那个 `release` 函数）。
#[derive(Debug)]
pub struct ChatGuard {
    locks: Arc<ChatLocks>,
    chat_id: String,
    lock: Arc<ChatLock>,
    permit: Option<OwnedSemaphorePermit>,
}

impl Drop for ChatGuard {
    fn drop(&mut self) {
        // 先放掉 permit（字段默认的析构顺序做不到这一点，所以是 `Option` + `take`），
        // 再减引用、必要时把条目从表里删掉（上游 `release` 的顺序）。
        drop(self.permit.take());
        if self.lock.refs.fetch_sub(1, Ordering::AcqRel) == 1 {
            let mut table = match self.locks.inner.lock() {
                Ok(table) => table,
                Err(poisoned) => poisoned.into_inner(),
            };
            table.remove(&self.chat_id);
        }
    }
}

impl ChatLocks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 这一把锁现在被几个持有者引着（诊断与用例用：条目应当在最后一个持有者离开时消失）。
    #[must_use]
    pub fn tracked_chats(&self) -> usize {
        match self.inner.lock() {
            Ok(table) => table.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// 等到这个聊空出来，或 `deadline` 到点。失败时返回的错误**可证明没发出去**
    /// （上游 `errChatBusy` 包着 `errNotAttempted`）。
    ///
    /// # Errors
    ///
    /// [`SenderError::ChatBusy`]。
    pub async fn acquire(
        self: &Arc<Self>,
        chat_id: &str,
        deadline: Deadline,
    ) -> Result<ChatGuard, SenderError> {
        let lock = {
            let mut table = match self.inner.lock() {
                Ok(table) => table,
                Err(poisoned) => poisoned.into_inner(),
            };
            let entry = table
                .entry(chat_id.to_owned())
                .or_insert_with(|| {
                    Arc::new(ChatLock {
                        sem: Arc::new(Semaphore::new(1)),
                        refs: AtomicUsize::new(0),
                    })
                })
                .clone();
            entry.refs.fetch_add(1, Ordering::AcqRel);
            entry
        };

        let abandon = |lock: &Arc<ChatLock>| {
            if lock.refs.fetch_sub(1, Ordering::AcqRel) == 1 {
                let mut table = match self.inner.lock() {
                    Ok(table) => table,
                    Err(poisoned) => poisoned.into_inner(),
                };
                table.remove(chat_id);
            }
        };

        // **空着的聊不查截止时刻**（上游逐字）：Go 的 `select` 在就绪的分支里**随机**取，
        // 所以一个预算已经用完的调用方走到一个没人在用的聊前面，会被"半个回合"拒掉 ——
        // 而那个聊根本没人在用。这里同样先做一次无等待的尝试。
        let semaphore = Arc::clone(&lock.sem);
        if let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() {
            return Ok(ChatGuard {
                locks: Arc::clone(self),
                chat_id: chat_id.to_owned(),
                lock,
                permit: Some(permit),
            });
        }
        if let Ok(Ok(permit)) =
            timeout_at(deadline_or_far(deadline), semaphore.acquire_owned()).await
        {
            return Ok(ChatGuard {
                locks: Arc::clone(self),
                chat_id: chat_id.to_owned(),
                lock,
                permit: Some(permit),
            });
        }
        abandon(&lock);
        Err(SenderError::ChatBusy)
    }
}
