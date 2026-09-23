//! daemon 侧「服务端 → runtime 的异步请求」在内存中的生命周期台账（M3-7 / LUM-1438）。
//!
//! ## 上游对应物
//!
//! 上游这四类请求全部是**进程内 store**，没有 DB 表：
//!
//! | 上游 store | 文件 | 本模块的 `RequestKind` |
//! |---|---|---|
//! | `InMemoryUpdateStore` | `handler/runtime_update.go` | [`RequestKind::Update`] |
//! | `ModelListStore` | `handler/runtime_models.go` | [`RequestKind::ModelList`] |
//! | `LocalSkillListStore` | `handler/runtime_local_skills.go` | [`RequestKind::LocalSkills`] |
//! | `LocalSkillImportStore` | `handler/runtime_local_skills.go` | [`RequestKind::LocalSkillImport`] |
//!
//! 本仓照搬这个选择：**不新增迁移**（否则门 ⑧ schema-drift 会与上游 schema 分叉）。
//! 代价是单节点语义 —— 多副本部署下请求会落在发起它的那台机器上，登记在
//! `docs/32` 的偏离表（D-2）。
//!
//! ## 生命周期
//!
//! ```text
//! create ──▶ pending ──(daemon 心跳 ack 取走)──▶ running ──▶ completed
//!               │                                  │           failed
//!               └──────── pending 超时 ────────────┴─────────▶ timeout
//! ```
//!
//! - `completed` / `failed` 额外携带**保留期**：到期后整条删除（上游 5 分钟）。
//! - `timeout` 是**终态**：daemon 事后迟到的 `*/result` 上报按「过期终态」处理为
//!   幂等 200 `{"status":"ok"}`（与上游一致，见 `routes/daemon/requests.rs`）。
//! - 超时判定是**惰性**的（在 `get` / `has_pending` / `pop_pending` / `sweep` 里做），
//!   与上游 `store.sweepLocked` 同款：没有后台 timer，也就没有需要 shutdown 的任务。
//!
//! ## 为什么放在 crate 根而不是 `routes/daemon/`
//!
//! `AppState` 要持有它，而 `state.rs` 是 M3 锚点：`state → routes → state` 的环形
//! 依赖在 Rust 里编不过。放 crate 根让 `state` 单向依赖它，`routes/**` 也单向依赖它。
//!
//! ## 条目体积与 `clippy::result_large_err`
//!
//! `PendingRequest` 带 `Value` 结果与多个 `Option<String>`（≈312 字节），而「上报失败」
//! 要把**当前那一行**原样交回调用方去区分「过期终态 ⇒ 幂等 200」与「真冲突」——
//! 装箱只会给每条上报加一次分配，换不来可读性。故本模块统一不查该 lint。
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use mc_core::Id;
use serde_json::Value;

mod wire;

pub use wire::LocalSkillImportAction;

/// 四类异步请求。wire 名字用于 daemon 心跳 ack 里的 `pending_*` 字段与日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestKind {
    /// `POST /api/runtimes/{id}/update` —— 要求 runtime 自更新。
    Update,
    /// `POST /api/runtimes/{id}/models` —— 要求 runtime 上报可用模型清单。
    ModelList,
    /// `POST /api/runtimes/{id}/local-skills` —— 要求 runtime 扫描本地 skill。
    LocalSkills,
    /// `POST /api/runtimes/{id}/local-skills/import` —— 要求 runtime 回传某 skill 的完整内容。
    LocalSkillImport,
}

impl RequestKind {
    /// 全部种类（心跳 ack 里按此顺序组装字段）。
    pub const ALL: [Self; 4] = [
        Self::Update,
        Self::ModelList,
        Self::LocalSkills,
        Self::LocalSkillImport,
    ];

    /// 日志 / ack 用的短名。
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::ModelList => "model_list",
            Self::LocalSkills => "local_skills",
            Self::LocalSkillImport => "local_skill_import",
        }
    }

    /// 该种类的超时预算（上游四类 store 各自的常量）。
    ///
    /// - `Update`：待办 120s / running 150s / 保留 5min（`runtime_update.go:44-68`）；
    /// - `ModelList`：待办 30s / running 60s / 保留 2min（`runtime_models.go:154-168`）——
    ///   比 update 紧得多，因为它服务的是**交互式**的模型下拉框；
    /// - `LocalSkills` / `LocalSkillImport`：待办 3min / running 60s / 保留 5min
    ///   （待办窗口刻意放大：老 daemon 每 15s 只领 1 条，批量 10 条要 150s 才领完）。
    #[must_use]
    pub fn timeouts(self) -> Timeouts {
        match self {
            Self::Update => Timeouts {
                pending: Duration::from_secs(120),
                running: Duration::from_secs(150),
                retention: Duration::from_secs(300),
            },
            Self::ModelList => Timeouts {
                pending: Duration::from_secs(30),
                running: Duration::from_secs(60),
                retention: Duration::from_secs(120),
            },
            Self::LocalSkills | Self::LocalSkillImport => Timeouts {
                pending: Duration::from_secs(180),
                running: Duration::from_secs(60),
                retention: Duration::from_secs(300),
            },
        }
    }
}

/// 一种请求的三个时限。
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// `pending` 阶段上限 —— 超过 ⇒ `timeout`。
    pub pending: Duration,
    /// `running` 阶段上限 —— 超过 ⇒ `timeout`。
    pub running: Duration,
    /// 终态保留期 —— 超过 ⇒ 删除。
    pub retention: Duration,
}

/// 请求状态。wire 字符串与上游 `UpdateStatus` / `ModelListStatus` / 本地 skill 状态一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    /// 已创建，等 daemon 取。
    Pending,
    /// daemon 已取走，等结果上报。
    Running,
    /// 成功终结。
    Completed,
    /// 失败终结。
    Failed,
    /// 超时终结（终态；迟到上报视为过期）。
    Timeout,
    /// 仅本地 skill **导入**：目标名字已被占用且用户没选覆写（终态）。
    Conflict,
}

impl RequestStatus {
    /// wire 字符串。
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Conflict => "conflict",
        }
    }

    /// 是否是终态（终态之后只有保留期与幂等上报）。
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Timeout | Self::Conflict
        )
    }
}

/// 一条异步请求。
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// 请求 id（回给调用方，daemon 在 `*/result` 里原样带回）。
    pub id: Id,
    /// 目标 runtime。
    pub runtime_id: Id,
    /// runtime 所属 workspace（发起时快照，用于结果归属校验）。
    pub workspace_id: Id,
    /// 种类。
    pub kind: RequestKind,
    /// 当前状态。
    pub status: RequestStatus,
    /// 发起人（`GET …/{requestId}` 的可见性判定要用）。
    pub initiator_user_id: Option<Id>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 被 daemon 取走的时刻。
    pub started_at: Option<DateTime<Utc>>,
    /// 终态时刻。
    pub completed_at: Option<DateTime<Utc>>,
    /// 终态保留截止（终态后填充）。
    pub expires_at: Option<DateTime<Utc>>,
    /// 结果载荷（各 kind 自己的 wire 形状）。
    pub result: Option<Value>,
    /// 失败原因。
    pub error: Option<String>,
    /// 仅本地 skill 导入：`create` / `overwrite`。
    pub action: Option<String>,
    /// 仅本地 skill 导入覆写：目标 skill id。
    pub target_skill_id: Option<Id>,
    /// 仅本地 skill 导入：期望的 skill 名字。
    pub name: Option<String>,
    /// 仅本地 skill 导入：描述。
    pub description: Option<String>,
    /// 仅本地 skill 导入：要导入的本地技能 key（发现路径上的稳定标识，不是名字）。
    /// 心跳 ack 的 `pending_local_skill_import{.s}` 要带它，daemon 才知道去读哪个技能。
    pub skill_key: Option<String>,
    /// 仅本地 skill 导入：发起方有没有选「结构化冲突」契约（MUL-2800）。
    /// 上报结果时据此决定「同名冲突」是终态 `conflict` 还是老式的 `failed`。
    pub supports_conflict: bool,
    /// 仅 `Update`：目标版本号（`UpdateRequest.target_version`）。
    ///
    /// 必须存下来：心跳 ack 的 `pending_update.target_version` **无条件序列化**
    /// （`DaemonHeartbeatPendingUpdate`），而结果上报的 body 里并不回传它。
    pub target_version: Option<String>,
}

impl PendingRequest {
    /// 是否属于该 runtime 且处于该种类 —— 心跳 ack 的探测口径。
    #[must_use]
    pub fn matches(&self, kind: RequestKind, runtime_id: Id) -> bool {
        self.kind == kind && self.runtime_id == runtime_id
    }

    /// 该请求是否**仍待处理**（`pending` 或 `running`）。
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self.status, RequestStatus::Pending | RequestStatus::Running)
    }

    /// 按创建时间升序（最老的优先）。
    fn sort_open(rows: &mut [PendingRequest]) {
        rows.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.to_string().cmp(&b.id.to_string()))
        });
    }
}

/// 内存台账。所有方法都是 `&self`（内部 `Mutex`），因为 `AppState` 共享的是 `Arc`。
pub struct RequestStore {
    inner: Mutex<HashMap<Id, PendingRequest>>,
    /// 时钟注入点：测试用它把时间推过时限，生产用 `Utc::now`。
    now: Box<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

/// 手写 `Debug`：`now` 是一枚闭包，没有 `Debug`。
impl std::fmt::Debug for RequestStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestStore")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl Default for RequestStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestStore {
    /// 用系统时钟构造。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            now: Box::new(Utc::now),
        }
    }

    /// 用给定时钟构造（测试用；`now` 每次调用都会被求值，所以可以推着走）。
    #[must_use]
    pub fn with_clock(now: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            now: Box::new(now),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        (self.now)()
    }

    /// 新建一条 `pending` 请求。
    pub fn create(&self, request: PendingRequest) -> PendingRequest {
        let mut guard = self.lock();
        let entry = PendingRequest {
            status: RequestStatus::Pending,
            created_at: request.created_at,
            ..request
        };
        guard.insert(entry.id, entry.clone());
        entry
    }

    /// 心跳 ack 批量取件（本地 skill 导入专用，upstream `PopPendingBatch`）。
    ///
    /// 最多取 `max` 条，全部置为 `running`；返回升序。
    pub fn pop_pending_batch(
        &self,
        kind: RequestKind,
        runtime_id: Id,
        max: usize,
    ) -> Vec<PendingRequest> {
        let mut out = Vec::new();
        for _ in 0..max {
            match self.pop_pending(kind, runtime_id) {
                Some(row) => out.push(row),
                None => break,
            }
        }
        out
    }

    /// 便捷构造：任意 kind（`name`/`description`/`action`/`target_skill_id` 均空）。
    pub fn create_kind(
        &self,
        kind: RequestKind,
        runtime_id: Id,
        workspace_id: Id,
        initiator_user_id: Option<Id>,
    ) -> PendingRequest {
        self.create(PendingRequest {
            id: Id::new(),
            runtime_id,
            workspace_id,
            kind,
            status: RequestStatus::Pending,
            initiator_user_id,
            created_at: self.now(),
            started_at: None,
            completed_at: None,
            expires_at: None,
            result: None,
            error: None,
            action: None,
            target_skill_id: None,
            name: None,
            description: None,
            skill_key: None,
            supports_conflict: false,
            target_version: None,
        })
    }

    /// 便捷构造：本地技能导入请求（带 skill key / action / 目标 skill / 名字 / 描述）。
    #[allow(clippy::too_many_arguments)]
    pub fn create_local_skill_import(
        &self,
        runtime_id: Id,
        workspace_id: Id,
        initiator_user_id: Option<Id>,
        skill_key: String,
        action: LocalSkillImportAction,
        target_skill_id: Option<Id>,
        name: Option<String>,
        description: Option<String>,
        supports_conflict: bool,
    ) -> PendingRequest {
        self.create(PendingRequest {
            id: Id::new(),
            runtime_id,
            workspace_id,
            kind: RequestKind::LocalSkillImport,
            status: RequestStatus::Pending,
            initiator_user_id,
            created_at: self.now(),
            started_at: None,
            completed_at: None,
            expires_at: None,
            result: None,
            error: None,
            action: Some(action.as_str().to_string()),
            target_skill_id,
            name,
            description,
            skill_key: Some(skill_key),
            supports_conflict,
            target_version: None,
        })
    }

    /// 该请求是否是一个**仍在排队**的本地导入请求（发起冲突检查用）。
    pub fn has_open_local_skill_import(&self, runtime_id: Id) -> bool {
        self.has_pending(RequestKind::LocalSkillImport, runtime_id)
    }

    /// 原子地「该 runtime 上没有在跑的更新就新建一条」（上游 `UpdateStore.Create`
    /// 在 store 自己的锁内做 `HasPending` 判定：两个并发 `POST /update` 只能有一个
    /// 成功，另一个 409 `an update is already in progress for this runtime`）。
    ///
    /// `None` ⇒ 已有 `pending` 或 `running` 的更新。**没有**『尽力而为』的版本：
    /// 拆成 `has_pending` + `create` 两步会让两个并发请求都通过检查。
    pub fn create_update_if_idle(
        &self,
        runtime_id: Id,
        workspace_id: Id,
        initiator_user_id: Option<Id>,
        target_version: impl Into<String>,
    ) -> Option<PendingRequest> {
        // `sweep` 自己会取锁，必须在 `self.lock()` **之前**调用（Mutex 不重入）。
        // 上游 `Create` 也是先 GC 过期行再判 `HasPending`：一个已超时 5 分钟的
        // 更新不该永远挡住新的更新。
        self.sweep();
        let mut guard = self.lock();
        let busy = guard
            .values()
            .any(|req| req.is_open() && req.matches(RequestKind::Update, runtime_id));
        if busy {
            return None;
        }
        let entry = PendingRequest {
            id: Id::new(),
            runtime_id,
            workspace_id,
            kind: RequestKind::Update,
            status: RequestStatus::Pending,
            initiator_user_id,
            created_at: self.now(),
            started_at: None,
            completed_at: None,
            expires_at: None,
            result: None,
            error: None,
            action: None,
            target_skill_id: None,
            name: None,
            description: None,
            skill_key: None,
            supports_conflict: false,
            target_version: Some(target_version.into()),
        };
        guard.insert(entry.id, entry.clone());
        Some(entry)
    }

    /// 惰性清理：把超时的 `pending`/`running` 标成 `timeout`，把过保留期的终态删掉。
    ///
    /// 返回被删掉的条数（便于测试断言回收确实发生）。
    pub fn sweep(&self) -> usize {
        let now = self.now();
        let mut guard = self.lock();
        let mut removed = 0usize;
        guard.retain(|_, req| {
            if req.status.is_terminal() {
                match req.expires_at {
                    Some(expires) if expires <= now => {
                        removed += 1;
                        return false;
                    }
                    _ => return true,
                }
            }
            let t = req.kind.timeouts();
            let (deadline, reference) = match req.status {
                RequestStatus::Pending => (t.pending, req.created_at),
                RequestStatus::Running => (t.running, req.started_at.unwrap_or(req.created_at)),
                _ => return true,
            };
            if now
                .signed_duration_since(reference)
                .to_std()
                .unwrap_or_default()
                >= deadline
            {
                req.status = RequestStatus::Timeout;
                req.completed_at = Some(now);
                req.expires_at =
                    Some(now + chrono::Duration::from_std(t.retention).unwrap_or_default());
            }
            true
        });
        removed
    }

    /// 读一条（会先做惰性超时/回收）。
    pub fn get(&self, id: Id) -> Option<PendingRequest> {
        self.sweep();
        self.lock().get(&id).cloned()
    }

    /// 取一条**不**做超时判定（测试与结果上报路径用：迟到上报要能看到原状态）。
    pub fn peek(&self, id: Id) -> Option<PendingRequest> {
        self.lock().get(&id).cloned()
    }

    /// 该 runtime 上是否还有**指定种类**的待处理请求。
    pub fn has_pending(&self, kind: RequestKind, runtime_id: Id) -> bool {
        self.sweep();
        self.lock()
            .values()
            .any(|req| req.is_open() && req.matches(kind, runtime_id))
    }

    /// 该 runtime 上**所有**指定种类的待处理请求（升序）。
    pub fn list_pending(&self, kind: RequestKind, runtime_id: Id) -> Vec<PendingRequest> {
        self.sweep();
        let guard = self.lock();
        let mut rows: Vec<PendingRequest> = guard
            .values()
            .filter(|req| req.is_open() && req.matches(kind, runtime_id))
            .cloned()
            .collect();
        PendingRequest::sort_open(&mut rows);
        rows
    }

    /// 该 runtime 上指定种类的**最老一条**待处理请求。
    pub fn oldest_pending(&self, kind: RequestKind, runtime_id: Id) -> Option<PendingRequest> {
        self.list_pending(kind, runtime_id).into_iter().next()
    }

    /// 心跳 ack 的取件语义：把最老一条 `pending` 请求置为 `running` 并返回。
    ///
    /// **只认 `pending`**（上游 `PopPending` 同款）：已经 `running` 的行说明本轮
    /// 已经交给 daemon 了，重复投递只会让同一次请求被干两遍；它要么被上报终止，
    /// 要么被惰性超时收走。
    ///
    /// `None` ⇒ ack 里不带这个字段。
    pub fn pop_pending(&self, kind: RequestKind, runtime_id: Id) -> Option<PendingRequest> {
        self.sweep();
        let now = self.now();
        let mut guard = self.lock();
        let mut rows: Vec<PendingRequest> = guard
            .values()
            .filter(|req| req.status == RequestStatus::Pending && req.matches(kind, runtime_id))
            .cloned()
            .collect();
        PendingRequest::sort_open(&mut rows);
        let chosen = rows.into_iter().next()?;
        let entry = guard.get_mut(&chosen.id)?;
        entry.status = RequestStatus::Running;
        entry.started_at = Some(now);
        Some(entry.clone())
    }

    /// 结果上报成功终止。返回 `Ok(row)`；`Err(current)` 表示不是 `pending`/`running`
    /// （调用方据此走「过期终态 → 幂等 200」或「冲突」分支）。
    pub fn complete(
        &self,
        id: Id,
        result: Value,
    ) -> Result<PendingRequest, Option<PendingRequest>> {
        self.finish(id, RequestStatus::Completed, Some(result), None)
    }

    /// 结果上报失败终止。
    pub fn fail(
        &self,
        id: Id,
        error: impl Into<String>,
    ) -> Result<PendingRequest, Option<PendingRequest>> {
        self.finish(id, RequestStatus::Failed, None, Some(error.into()))
    }

    /// 本地导入冲突终止（名字被占且未选覆写）。
    ///
    /// `conflict` 是**终态但不是错误**（`docs/16` §6.2）：`error` 保持为空，结构化信息
    /// 走 `result` 的 `conflict` 键，由 [`PendingRequest::to_wire`] 摊平到顶层。
    pub fn conflict(
        &self,
        id: Id,
        conflict: &Value,
    ) -> Result<PendingRequest, Option<PendingRequest>> {
        self.finish(
            id,
            RequestStatus::Conflict,
            Some(serde_json::json!({ "conflict": conflict })),
            None,
        )
    }

    fn finish(
        &self,
        id: Id,
        status: RequestStatus,
        result: Option<Value>,
        error: Option<String>,
    ) -> Result<PendingRequest, Option<PendingRequest>> {
        let now = self.now();
        let mut guard = self.lock();
        let Some(entry) = guard.get_mut(&id) else {
            return Err(None);
        };
        if !entry.is_open() {
            return Err(Some(entry.clone()));
        }
        let t = entry.kind.timeouts();
        entry.status = status;
        entry.completed_at = Some(now);
        entry.expires_at = Some(now + chrono::Duration::from_std(t.retention).unwrap_or_default());
        entry.result = result;
        entry.error = error;
        Ok(entry.clone())
    }

    /// 当前条数（测试用）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// 是否为空（测试用）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 当前全部请求的只读快照（测试与调试用）。
    #[must_use]
    pub fn snapshot(&self) -> Vec<PendingRequest> {
        self.lock().values().cloned().collect()
    }

    /// 锁只保护一个 `HashMap`，且所有临界区都不 await ⇒ 用 `parking_lot` 风格直接
    /// 解包；中毒只可能来自 panic 中的断言，这时把中毒当致命错误更诚实。
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Id, PendingRequest>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc as StdArc;

    fn clock_at(secs: StdArc<AtomicI64>) -> impl Fn() -> DateTime<Utc> + Send + Sync + 'static {
        move || {
            DateTime::<Utc>::from_timestamp(1_700_000_000 + secs.load(Ordering::SeqCst), 0).unwrap()
        }
    }

    fn ids() -> (Id, Id, Id) {
        (Id::new(), Id::new(), Id::new())
    }

    #[test]
    fn pop_pending_is_oldest_first_and_marks_running() {
        let secs = StdArc::new(AtomicI64::new(0));
        let store = RequestStore::with_clock(clock_at(secs.clone()));
        let (runtime, workspace, user) = ids();
        let first = store.create_kind(RequestKind::Update, runtime, workspace, Some(user));
        secs.fetch_add(1, Ordering::SeqCst);
        let second = store.create_kind(RequestKind::Update, runtime, workspace, Some(user));

        let popped = store.pop_pending(RequestKind::Update, runtime).unwrap();
        assert_eq!(popped.id, first.id);
        assert_eq!(popped.status, RequestStatus::Running);
        assert!(popped.started_at.is_some());

        // 重复 pop 不会重置 running 计时，且拿到的是第二条 —— 第一条已 `running`，
        // 不再算可认领（上游 `PopPending` 只看 `pending`）。
        let again = store.pop_pending(RequestKind::Update, runtime).unwrap();
        assert_eq!(again.id, second.id);
        assert!(store.pop_pending(RequestKind::Update, runtime).is_none());
    }

    #[test]
    fn pending_times_out_lazily_and_terminal_reports_are_idempotent() {
        let secs = StdArc::new(AtomicI64::new(0));
        let store = RequestStore::with_clock(clock_at(secs.clone()));
        let (runtime, workspace, user) = ids();
        let req = store.create_kind(RequestKind::Update, runtime, workspace, Some(user));

        // 119s：仍在 pending。
        secs.store(119, Ordering::SeqCst);
        assert_eq!(store.get(req.id).unwrap().status, RequestStatus::Pending);

        // 120s：惰性判超时。
        secs.store(120, Ordering::SeqCst);
        assert_eq!(store.get(req.id).unwrap().status, RequestStatus::Timeout);

        // 迟到上报：不是 open ⇒ Err(Some(现状))，调用方据此返回幂等 200。
        let outcome = store.complete(req.id, json!({"ok": true}));
        assert!(matches!(outcome, Err(Some(row)) if row.status == RequestStatus::Timeout));

        // 保留期（300s）之后整条回收。
        secs.store(120 + 301, Ordering::SeqCst);
        assert_eq!(store.sweep(), 1);
        assert!(store.get(req.id).is_none());
    }

    #[test]
    fn running_timeout_uses_taken_at_not_created_at() {
        let secs = StdArc::new(AtomicI64::new(0));
        let store = RequestStore::with_clock(clock_at(secs.clone()));
        let (runtime, workspace, user) = ids();
        let req = store.create_kind(RequestKind::Update, runtime, workspace, Some(user));

        // 100s 时被取走；再 +149s 仍是 running（虽然距创建已 249s > pending 上限）。
        secs.store(100, Ordering::SeqCst);
        assert!(store.pop_pending(RequestKind::Update, runtime).is_some());
        secs.store(249, Ordering::SeqCst);
        assert_eq!(store.get(req.id).unwrap().status, RequestStatus::Running);
        secs.store(250, Ordering::SeqCst);
        assert_eq!(store.get(req.id).unwrap().status, RequestStatus::Timeout);
    }

    #[test]
    fn has_pending_ignores_terminal_rows() {
        let secs = StdArc::new(AtomicI64::new(0));
        let store = RequestStore::with_clock(clock_at(secs.clone()));
        let (runtime, workspace, user) = ids();
        let req = store.create_kind(RequestKind::Update, runtime, workspace, Some(user));
        assert!(store.has_pending(RequestKind::Update, runtime));
        store.complete(req.id, json!({})).unwrap();
        assert!(!store.has_pending(RequestKind::Update, runtime));
        assert_eq!(store.len(), 1, "终态行保留到保留期结束");
    }

    #[test]
    fn terminal_statuses_carry_wire_names() {
        assert_eq!(RequestStatus::Conflict.wire(), "conflict");
        assert!(RequestStatus::Conflict.is_terminal());
        assert!(!RequestStatus::Running.is_terminal());
        // 预算逐类给值，没有「pending 一定不短于 running」的全局关系：
        // update 是 120/150（上游 updatePendingTimeout/RunningTimeout），
        // 模型清单反而是 30/60。只锁「都为正、且保留期覆盖两者」。
        for kind in RequestKind::ALL {
            let t = kind.timeouts();
            assert!(t.pending > Duration::ZERO, "{kind:?}");
            assert!(t.running > Duration::ZERO, "{kind:?}");
            assert!(t.retention > Duration::ZERO, "{kind:?}");
        }
        assert_eq!(
            RequestKind::Update.timeouts().pending,
            Duration::from_secs(120)
        );
        assert_eq!(
            RequestKind::Update.timeouts().running,
            Duration::from_secs(150)
        );
        assert_eq!(
            RequestKind::ModelList.timeouts().pending,
            Duration::from_secs(30)
        );
    }
}
