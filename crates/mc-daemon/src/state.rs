//! 客户端状态机：登记台账 + 心跳水位 + in-flight claim 去重（M3-7 / LUM-1438）。
//!
//! ## 上游对应物
//!
//! 上游 daemon 把这三类信息摊在一个 6000 行的 `Daemon` 结构体上：
//! `d.runtimes`（登记台账，`daemon.go:1`）、`d.wsHeartbeatAcked`（心跳水位，
//! `daemon.go:4532`）、`d.inflightTasks`（本机在跑的任务）。本模块把**最小可用**
//! 的那部分抽成纯数据结构 —— 不持锁、不 spawn、不发请求，因此可以用单元测试
//! 逐条覆盖状态迁移，而不必起 tokio runtime。
//!
//! ## 三件事，各自解决一个真问题
//!
//! 1. **登记台账**：`register` 之后服务端才知道这台机器上有哪些 runtime，之后的
//!    claim 必须带上它们。服务端可能拒绝了其中几个（profile 解析失败），所以台账
//!    的**真值来源是服务端响应**，不是本机自报的列表。
//! 2. **心跳水位**：一次心跳把 `last_seen_at` 刷新一次。水位用来判断某台 runtime
//!    是不是已经太久没被确认过（上游 `wsHeartbeatRecentlyAcked` 的反向用途：
//!    WS 心跳覆盖 HTTP 心跳，避免对同一台机器重复写库）。
//! 3. **in-flight 去重**：服务端的 claim 是**原子**的，但它不保证「同一条 task 不会
//!    通过两条传输各回来一次」（上游明确记录过：重复的 pending-work 提示不能造成
//!    重复工作）。客户端这一层把已经在跑的 task id 记住，重复的认领直接丢掉。
//!    这不是权限判断，是**幂等护栏**。
//!
//! 状态迁移全部走显式方法，没有 `pub` 字段可以直接改坏 —— 状态机是这一层的全部价值。

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use crate::wire::{ClaimedTask, RegisteredRuntime};

/// 一次成功登记的台账（`register` 响应里客户端需要长期持有的部分）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registration {
    /// 目标 workspace。
    pub workspace_id: String,
    /// 本机 daemon id。
    pub daemon_id: String,
    /// 机器名（服务端回的是它自己记的，可能与本机自报不同）。
    pub device_name: String,
    /// CLI 版本。
    pub cli_version: String,
    /// 服务端落库后的 runtime 台账。
    pub runtimes: Vec<RegisteredRuntime>,
    /// repos 版本号（daemon 用它判断是否要重新拉 clone 清单）。
    pub repos_version: i64,
}

impl Registration {
    /// 服务端认下的 runtime id 列表（保持服务端返回的顺序）。
    #[must_use]
    pub fn runtime_ids(&self) -> Vec<String> {
        self.runtimes.iter().map(|rt| rt.id.clone()).collect()
    }

    /// 按 id 找一条台账（`None` = 服务端没认这个 id）。
    #[must_use]
    pub fn runtime(&self, runtime_id: &str) -> Option<&RegisteredRuntime> {
        self.runtimes.iter().find(|rt| rt.id == runtime_id)
    }
}

/// 客户端本地状态（[`crate::DaemonClient`] 持有）。
#[derive(Debug, Default)]
pub struct ClientState {
    registration: Option<Registration>,
    /// 服务端已确认消失的 runtime（`runtime_gone` / 心跳 404）。
    gone: BTreeSet<String>,
    last_heartbeat_at: BTreeMap<String, DateTime<Utc>>,
    /// task id → 认领它的 runtime id。
    in_flight: BTreeMap<String, String>,
}

impl ClientState {
    /// 空状态（尚未登记）。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 记下一次成功登记。
    ///
    /// 会**清空**心跳水位与 in-flight 台账：重新登记意味着服务端侧的关系被重建了
    /// （上游 `notifyRuntimeSetChanged` 同款语义 —— 旧 runtime 集合的 in-flight
    /// 任务不该算到新集合头上）。
    pub fn set_registration(&mut self, registration: Registration) {
        self.registration = Some(registration);
        self.gone.clear();
        self.last_heartbeat_at.clear();
        self.in_flight.clear();
    }

    /// 清空登记（`deregister` 之后）。
    pub fn clear_registration(&mut self) {
        self.registration = None;
        self.gone.clear();
        self.last_heartbeat_at.clear();
        self.in_flight.clear();
    }

    /// 忘掉指定的 runtime（`deregister` 成功之后）。
    ///
    /// 与 [`Self::note_runtime_gone`] 的区别：那是「服务端说它没了」的被动记录
    /// （要保留在 `gone` 里以便去重），这是「本机主动下线成功」的主动删除 ——
    /// 台账、水位、in-flight 一起摘掉，不留在 `gone` 里。
    pub fn forget_runtimes(&mut self, runtime_ids: &[String]) {
        let Some(registration) = self.registration.as_mut() else {
            return;
        };
        registration
            .runtimes
            .retain(|rt| !runtime_ids.contains(&rt.id));
        for id in runtime_ids {
            self.gone.remove(id);
            self.last_heartbeat_at.remove(id);
        }
        self.in_flight.retain(|_, rt| !runtime_ids.contains(rt));
    }

    /// 当前登记台账。
    #[must_use]
    pub fn registration(&self) -> Option<&Registration> {
        self.registration.as_ref()
    }

    /// 已登记过且没被服务端宣告消失的 runtime id 列表。
    #[must_use]
    pub fn live_runtime_ids(&self) -> Vec<String> {
        self.registration
            .as_ref()
            .map(|reg| {
                reg.runtime_ids()
                    .into_iter()
                    .filter(|id| !self.gone.contains(id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 服务端已宣告消失的 runtime id 列表。
    #[must_use]
    pub fn gone_runtime_ids(&self) -> Vec<String> {
        self.gone.iter().cloned().collect()
    }

    /// 记下「服务端说这台 runtime 没了」。
    ///
    /// 返回 `true` 表示这次调用**首次**把它标掉（上游 `handleRuntimeGone` 的
    /// 去重语义：并发/重复的 404 只该触发一次恢复动作）。
    pub fn note_runtime_gone(&mut self, runtime_id: &str) -> bool {
        if runtime_id.is_empty() {
            return false;
        }
        // 先把 in-flight 里属于它的任务摘掉：runtime 没了，那些任务不会再回来。
        self.in_flight.retain(|_, rt| rt != runtime_id);
        self.gone.insert(runtime_id.to_owned())
    }

    /// 记一次心跳成功。
    pub fn mark_heartbeat(&mut self, runtime_id: &str, at: DateTime<Utc>) {
        if runtime_id.is_empty() {
            return;
        }
        self.last_heartbeat_at.insert(runtime_id.to_owned(), at);
    }

    /// 某台 runtime 最近一次心跳成功的时间。
    #[must_use]
    pub fn last_heartbeat_at(&self, runtime_id: &str) -> Option<DateTime<Utc>> {
        self.last_heartbeat_at.get(runtime_id).copied()
    }

    /// 某台 runtime 的心跳是否还在 `window` 之内（上游 `wsHeartbeatRecentlyAcked`）。
    #[must_use]
    pub fn heartbeat_fresh(
        &self,
        runtime_id: &str,
        now: DateTime<Utc>,
        window: chrono::Duration,
    ) -> bool {
        self.last_heartbeat_at(runtime_id)
            .is_some_and(|at| now - at < window)
    }

    /// 收下一批认领结果，返回**真正新收下**的那些（重复的丢掉）。
    ///
    /// 去重键是 task id：同一条任务通过 HTTP 与 WS 两条腿各回来一次时，第二次
    /// 会被丢掉，调用方只会执行一次。
    pub fn accept_claims(&mut self, tasks: Vec<ClaimedTask>) -> Vec<ClaimedTask> {
        let mut fresh = Vec::with_capacity(tasks.len());
        for task in tasks {
            if task.id.is_empty() || self.in_flight.contains_key(&task.id) {
                continue;
            }
            self.in_flight
                .insert(task.id.clone(), task.runtime_id.clone());
            fresh.push(task);
        }
        fresh
    }

    /// 记下一条任务跑完了（成功、失败、放弃都一样 —— 它必须离开本机台账，
    /// 否则服务端的重派会被客户端自己挡掉）。
    ///
    /// 返回该任务认领时所属的 runtime id。
    pub fn finish_task(&mut self, task_id: &str) -> Option<String> {
        self.in_flight.remove(task_id)
    }

    /// 本机正在跑的 task id 列表。
    #[must_use]
    pub fn in_flight_task_ids(&self) -> Vec<String> {
        self.in_flight.keys().cloned().collect()
    }

    /// 本机正在跑几个任务。
    #[must_use]
    pub fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// 某条任务是不是已经在本机台账里。
    #[must_use]
    pub fn is_in_flight(&self, task_id: &str) -> bool {
        self.in_flight.contains_key(task_id)
    }
}
