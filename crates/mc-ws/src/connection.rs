//! 连接与注册表：**一条连接长什么样、hub 用哪些索引找到它**。
//!
//! 上游对应 `server/internal/daemonws/hub.go` 的 `client` 结构（L150–L320）与
//! `Hub` 里那几张受 `Hub.mu` 保护的 map（L88–L110）。
//!
//! | 本模块 | 上游 |
//! |--------|------|
//! | [`Connection`] | `client` |
//! | [`Connection::try_send`] | `client.trySend`（`hub.go:280`） |
//! | [`Connection::mark_seen`] | `client.markSeen`（`hub.go:213`），窗口 [`EVENT_DEDUP_CAPACITY`] |
//! | [`Connection::allows_runtime`] / [`Connection::remove_runtime`] | `client.allowsRuntime` / `removeRuntime` |
//! | [`Connection::in_flight`] | `client.rpcSem`（容量 [`mc_daemon_proto::rpc::MAX_IN_FLIGHT_RPC_PER_CLIENT`]） |
//! | [`Registry`] | `Hub.clients` / `byRuntime` / `byWorkspace` / `byUser` |
//! | [`DedupCache`] | `client.seenIDs` + `hub.runtimeGoneSeen` |
//! | [`lock_read`] 等 | Go 的 mutex 不需要处理中毒；Rust 侧统一「忽略中毒继续用」 |
//!
//! 本模块**不做 I/O**：不碰 socket、不起 task。读写泵在 [`crate::pump`]，公开 API 在
//! [`crate::hub`]。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use tokio::sync::{mpsc, watch};
use uuid::Uuid;

use crate::frames::{InFlightLimiter, RpcCancel};
use crate::identity::ClientIdentity;

/// 单连接发送队列深度（上游 `hub.go:174` `make(chan []byte, 16)`）。
///
/// 16 帧之后仍写不出去就是慢客户端 —— 上游选择驱逐而不是继续缓冲。
pub const SEND_BUFFER: usize = 16;

/// 单连接事件去重窗口（上游 `hub.go:206` `eventDedupCapacity`）。
pub const EVENT_DEDUP_CAPACITY: usize = 128;

/// hub 级 runtime-gone 去重窗口（上游 `hub.go:209` `runtimeGoneDedupCapacity`）：
/// 按「一批 runtime GC 最多 500 条」定尺寸，要能完整覆盖本地投递 + Redis 回环。
pub const RUNTIME_GONE_DEDUP_CAPACITY: usize = 512;

/// 连接引用（上游 `*client`）。
pub(crate) type ConnectionRef = Arc<Connection>;

/// 一条已建立的 daemon WS 连接。
#[derive(Debug)]
pub(crate) struct Connection {
    id: Uuid,
    identity: ClientIdentity,
    runtimes: RwLock<HashSet<String>>,
    sender: mpsc::Sender<String>,
    close_tx: watch::Sender<bool>,
    closed: AtomicBool,
    dedup: Mutex<DedupCache>,
    in_flight: InFlightLimiter,
}

impl Connection {
    /// 建连接 + 取出写泵要消费的接收端。
    ///
    /// `send_buffer` 为 0 会 panic（`mpsc::channel` 的要求）；
    /// [`crate::hub::TransportConfig`] 的默认值是 16。
    pub(crate) fn new(
        identity: ClientIdentity,
        send_buffer: usize,
        event_dedup_capacity: usize,
    ) -> (ConnectionRef, mpsc::Receiver<String>) {
        let (sender, receiver) = mpsc::channel(send_buffer);
        let (close_tx, _) = watch::channel(false);
        let runtimes = identity.runtime_set();
        let conn = Arc::new(Self {
            id: Uuid::new_v4(),
            identity,
            runtimes: RwLock::new(runtimes),
            sender,
            close_tx,
            closed: AtomicBool::new(false),
            dedup: Mutex::new(DedupCache::new(event_dedup_capacity)),
            in_flight: InFlightLimiter::default(),
        });
        (conn, receiver)
    }

    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn identity(&self) -> &ClientIdentity {
        &self.identity
    }

    /// 上游 `client.allowsRuntime`：心跳只接受连接已授权的 runtime。
    pub(crate) fn allows_runtime(&self, runtime_id: &str) -> bool {
        lock_read(&self.runtimes).contains(runtime_id)
    }

    /// 上游 `client.removeRuntime`。
    pub(crate) fn remove_runtime(&self, runtime_id: &str) {
        lock_write(&self.runtimes).remove(runtime_id);
    }

    /// runtime 集合快照（排序，便于日志与确定性）。
    pub(crate) fn runtime_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = lock_read(&self.runtimes).iter().cloned().collect();
        ids.sort();
        ids
    }

    pub(crate) fn runtime_count(&self) -> usize {
        lock_read(&self.runtimes).len()
    }

    /// 上游 `client.markSeen`。
    pub(crate) fn mark_seen(&self, event_id: &str) -> bool {
        lock_mutex(&self.dedup).mark_seen(event_id)
    }

    /// 上游 `client.trySend`：非阻塞，队列满或连接已关闭都返回 `false`。
    ///
    /// 先看 `closed` 再看 channel —— 与上游 `sendClosed` 在 `sendMu` 下的判定顺序一致，
    /// 保证拆线之后不会再有帧进入队列。
    pub(crate) fn try_send(&self, frame: &str) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        self.sender.try_send(frame.to_owned()).is_ok()
    }

    /// 拆线：触发取消信号（在飞 RPC handler 据此停止）并让后续 `try_send` 失败。
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        // 没有接收端时 send 会返回 Err；值本身仍是权威状态，晚订阅者读到 true。
        let _ = self.close_tx.send(true);
    }

    pub(crate) fn close_receiver(&self) -> watch::Receiver<bool> {
        self.close_tx.subscribe()
    }

    /// 给 RPC handler 用的取消句柄（上游传给 handler 的 `c.ctx`）。
    pub(crate) fn cancel(&self) -> RpcCancel {
        RpcCancel::from_closed(self.close_tx.subscribe())
    }

    pub(crate) fn in_flight(&self) -> &InFlightLimiter {
        &self.in_flight
    }
}

/// 有界事件去重窗口（上游 `client.markSeen` + `hub.markRuntimeGoneSeen`）。
#[derive(Debug)]
pub(crate) struct DedupCache {
    capacity: usize,
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl DedupCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            seen: HashSet::new(),
            order: VecDeque::new(),
        }
    }

    /// 记下 `event_id`；**空 id 禁用去重**（永远返回 `true`，上游行为）。
    pub(crate) fn mark_seen(&mut self, event_id: &str) -> bool {
        if event_id.is_empty() {
            return true;
        }
        if !self.seen.insert(event_id.to_owned()) {
            return false;
        }
        self.order.push_back(event_id.to_owned());
        if self.order.len() > self.capacity {
            if let Some(drop) = self.order.pop_front() {
                self.seen.remove(&drop);
            }
        }
        true
    }

    /// 撤销一次 `mark_seen`（上游 `forgetRuntimeGoneSeen`：事件可能抢在新连接注册之前
    /// 到达，不能消费它，否则新连接收不到失效通知）。
    pub(crate) fn forget(&mut self, event_id: &str) {
        if event_id.is_empty() {
            return;
        }
        if self.seen.remove(event_id) {
            if let Some(index) = self.order.iter().position(|id| id == event_id) {
                self.order.remove(index);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.seen.len()
    }
}

/// 广播索引的三个维度（上游 `byRuntime` / `byWorkspace` / `byUser`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Index {
    Runtime,
    Workspace,
    User,
}

/// 三个广播索引 + 连接表（上游 `Hub.mu` 保护的那几张 map）。
///
/// 索引用 `BTreeSet<Uuid>` 而不是 `HashSet`：上游是 Go map（遍历顺序随机），这里选择
/// **按连接 id 稳定排序**，让「一次扇出到多连接的顺序」可复现。语义无差别（每个连接
/// 都各自收到一帧、各自去重）。
#[derive(Debug, Default)]
pub(crate) struct Registry {
    pub(crate) clients: HashMap<Uuid, ConnectionRef>,
    by_runtime: HashMap<String, BTreeSet<Uuid>>,
    by_workspace: HashMap<String, BTreeSet<Uuid>>,
    by_user: HashMap<String, BTreeSet<Uuid>>,
}

impl Registry {
    fn map(&self, index: Index) -> &HashMap<String, BTreeSet<Uuid>> {
        match index {
            Index::Runtime => &self.by_runtime,
            Index::Workspace => &self.by_workspace,
            Index::User => &self.by_user,
        }
    }

    fn map_mut(&mut self, index: Index) -> &mut HashMap<String, BTreeSet<Uuid>> {
        match index {
            Index::Runtime => &mut self.by_runtime,
            Index::Workspace => &mut self.by_workspace,
            Index::User => &mut self.by_user,
        }
    }

    pub(crate) fn index(&self, index: Index, key: &str) -> Option<&BTreeSet<Uuid>> {
        self.map(index).get(key)
    }

    pub(crate) fn insert(&mut self, index: Index, key: &str, id: Uuid) {
        if key.is_empty() {
            return;
        }
        self.map_mut(index)
            .entry(key.to_owned())
            .or_default()
            .insert(id);
    }

    pub(crate) fn remove(&mut self, index: Index, key: &str, id: &Uuid) {
        let map = self.map_mut(index);
        let Some(ids) = map.get_mut(key) else {
            return;
        };
        ids.remove(id);
        let empty = ids.is_empty();
        if empty {
            map.remove(key);
        }
    }

    /// 摘掉某个 runtime 索引下的全部连接 id（`invalidateRuntime` 的第一步）。
    pub(crate) fn take_runtime(&mut self, runtime_id: &str) -> BTreeSet<Uuid> {
        self.by_runtime.remove(runtime_id).unwrap_or_default()
    }
}

/// 读锁；中毒（某线程持锁 panic）时继续用内层数据而不是级联 panic。
pub(crate) fn lock_read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(PoisonError::into_inner)
}

/// 写锁，中毒处理同 [`lock_read`]。
pub(crate) fn lock_write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(PoisonError::into_inner)
}

/// mutex，中毒处理同 [`lock_read`]。
pub(crate) fn lock_mutex<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_cache_dedups_and_evicts_in_order() {
        let mut cache = DedupCache::new(2);
        assert!(cache.mark_seen("a"));
        assert!(!cache.mark_seen("a"));
        assert!(cache.mark_seen("b"));
        // "a" 被挤出窗口后可再次投递。
        assert!(cache.mark_seen("c"));
        assert_eq!(cache.len(), 2);
        assert!(cache.mark_seen("a"));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn empty_event_id_disables_dedup() {
        let mut cache = DedupCache::new(4);
        assert!(cache.mark_seen(""));
        assert!(cache.mark_seen(""));
        assert_eq!(cache.len(), 0);
        cache.forget("");
    }

    #[test]
    fn dedup_cache_forget_undoes_mark_seen() {
        let mut cache = DedupCache::new(4);
        assert!(cache.mark_seen("e1"));
        cache.forget("e1");
        assert!(cache.mark_seen("e1"));
        // 撤销不存在的 id 是 no-op。
        cache.forget("nope");
    }
}
