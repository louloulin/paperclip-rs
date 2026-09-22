# M3-3 任务队列领域层 —— 状态机 / 租约 / 重试 / 取消 / 用量结算

> **切片**：M3-3（W3a），`docs/15-M3-PLAN.md` §4。
> **crate**：`crates/mc-task`（纯 Rust，**0 路由、0 SQL、0 落库**）。
> **分支**：`feat/multica-rs-m3a-task-domain`。
> **上游对照**：`louloulin/multica` @ `90e0bdf`（Go 单仓）。
>
> 本文件是 M3-6（`TaskRepo` 的 Pg 实现 + 15 条路由）与 M3-7（daemon 面 / ws）
> 的**领域契约**：状态机真值、失败分类判定、租约清扫阈值、取消与取消确认语义、
> 用量结算公式。文中每张映射表都注明上游文件与行号，可逐行核对。

---

## 1. 这一片做了什么，没做什么

| 做 | 不做 |
| --- | --- |
| 状态机 + 迁移表 + 写集（`ColumnWrite`） | SQL / `sqlx` 查询（M3-6） |
| `prepare` 租约视图 + 僵尸清扫**纯判定** | 定时器 / 后台 sweeper 进程（M3-7） |
| 失败分类（5 粗类）+ 细化原因 + 重试判定 | 真正创建重试子任务（M3-6 落库） |
| 取消三形态 + `cancel-ack` 写集计划 | 路由 / 200 响应码（M3-6） |
| 用量结算纯计算（`task_usage` 行形状、小时桶折叠） | 定价（rate table 在客户端）/ 小时桶落库 |
| `TaskStore` trait（port，8 方法） | port 的生产实现（M3-6；本仓只允许 `#[cfg(test)]` 内存实现） |

**为什么 `TaskStore` 只有 trait 没有内存实现**：上游只有 PostgreSQL 一种队列。
给一个「重启即丢」的内存队列会让 M3-6 误以为可以拿它跑通路由面，而真正的契约
（部分唯一索引、claim 串行化栅栏、`priority` 排序、wakeup 门）**只在 SQL 里**。
本片的 `store/tests.rs` 里的内存实现是 `#[cfg(test)]` 的**可满足性证明**：
用来证明这 8 个方法足以驱动完整生命周期。

---

## 2. 状态机

### 2.1 八个状态

`crates/mc-task/src/status.rs`。真值 = 上游最终 CHECK
（`contracts/upstream-schema.sql:1056`）：

```
queued, dispatched, running, completed, failed, cancelled,
waiting_local_directory, deferred
```

- 终态（`TERMINAL`）：`completed` / `failed` / `cancelled`。
- 在飞（`IN_FLIGHT`）：`dispatched` / `running` / `waiting_local_directory`。
- 未决（`PENDING`）：`queued` / `deferred`。

> **坑（`docs/15` §2.3 同款）**：本仓 `migrations/0001_init.up.sql:230` 的
> `agent_task_queue.status` CHECK 是
> `(queued, running, terminal_completed, terminal_cancelled, terminal_failed, delegated_failure)`：
> **缺 `dispatched` 与 `waiting_local_directory`**，并把 `delegated_failure` 当成状态。
> 它**不是**契约（W0-B2 会把 apply-set 切到 `migrations/upstream/`）。本 crate
> 以事件表 + `022`/`055`/`109` 为真值，**不读本仓 CHECK**。

`delegated_failure` **不是状态**：上游它是 `status='failed'` 的一个子类（委派子任务
失败）。本 crate 建模为 `TaskState::is_delegated_failure()` 谓词 + `failed` 事件上的
`delegated_from_task_id` 载荷，见 `state.rs`。

### 2.2 事件表（9 条 wire 事件，逐行对 `events.go` L34–L42）

上游 `server/pkg/protocol/events.go:32-42` 有 **9 个** `task:` 常量。`docs/15` §2.3
把 `task:progress` / `task:message` 并成一行写成「8 条」；本 crate 逐条建 9 个 wire 事件，
测试 `wire_event_names_are_the_nine_upstream_constants` 钉住这个计数。

| 上游常量（`events.go`） | 本 crate 事件 | 上游注释的边 | SQL 实际接受（更宽） |
| --- | --- | --- | --- |
| `task:queued` | `TaskEventKind::Queued` | `∅ → queued` | 仅 `deferred → queued`（提升；入队走构造器） |
| `task:dispatch` | `TaskEventKind::Dispatch` | `queued → dispatched` | 同 |
| `task:running` | `TaskEventKind::Running` | `dispatched → running` | `dispatched` / `waiting_local_directory → running` |
| `task:waiting_local_directory` | `TaskEventKind::WaitingLocalDirectory` | `dispatched → waiting_local_directory` | 同 |
| `task:progress` | `TaskEventKind::Progress` | 运行期附加信息 | 任意状态，**不改状态** |
| `task:message` | `TaskEventKind::Message` | 运行期附加信息 | 任意状态，**不改状态** |
| `task:completed` | `TaskEventKind::Completed` | `running → completed` | 同（`CompleteAgentTask` 的 `WHERE status='running'`） |
| `task:failed` | `TaskEventKind::Failed` | `running → failed` | 全部非终态 → `failed`（`FailStaleTasks` 等并发语句的 WHERE 并集） |
| `task:cancelled` | `TaskEventKind::Cancelled` | `* → cancelled` | 全部非终态 → `cancelled` |

「SQL 实际接受更宽」不是猜测：`state.rs` 的模块文档逐条列了每条 `UPDATE` 的
WHERE 子句与出处（`agent.sql` / `runtime.sql` 的行号）。事件表是**文档**，
SQL 是**行为**；两者冲突时本 crate 依 SQL 放宽，并在迁移表里标出来源。

### 2.3 三条服务端内部边（没有 `task:` 常量）

上游有三个只由服务端执行的迁移，daemon 观察不到、`events.go` 里也没有常量。
它们同样要能持久化，所以本 crate 给它们事件 —— 但**不**给 `task:` 名字：

| 内部事件 | 边 | 上游出处 |
| --- | --- | --- |
| `TaskEventKind::Deferred` | `∅ → deferred`（仅创建路径） | 迁移 `109`；`fire_at` 未到期 / `runtime_offline` 重试子任务 |
| `TaskEventKind::Reclaim` | `dispatched → dispatched`（重投递并刷新租约） | `ReclaimStaleDispatchedTaskForRuntime`（`agent.sql:865`/`:910`） |
| `TaskEventKind::RequeueAfterClaimFailure` | `dispatched → queued` | `RequeueAgentTaskAfterClaimFailure`（`agent.sql:854`） |

**序列化前缀**：wire 事件序列化成 `task:<name>`（与上游常量逐字相同），内部事件
序列化成 `internal.<name>`（`INTERNAL_EVENT_PREFIX`，见 `state/wire.rs`）。
这不是洁癖 —— M3-7 的 ws 发布路径会按前缀过滤，内部事件永远不会被当成一个
「看起来合法的 wire 事件」发出去。`internal_ops_never_claim_a_task_prefix` 用例钉住它。

> 手写 `Serialize`/`Deserialize` 而不是 `#[derive(Serialize, rename_all="snake_case")]`：
> 派生会产出 `"queued"` 而不是 `"task:queued"`。**任何**枚举的 serde 线格式都必须与
> `as_str()` / `parse()` 逐字一致，否则事件在 DB 里读回来就变了个东西。

### 2.4 完整迁移表（12 事件 × 8 状态 = 96 组合）

`state/tests.rs::transition_matrix_is_exhaustive_and_exact` 穷举 96 个组合：
**34 条合法、62 条非法**。逐条列出（`→` 表示状态改变，`≡` 表示状态不变）：

| 事件 | 合法源状态 | 目标 | 写集（`ColumnWrite`） |
| --- | --- | --- | --- |
| `Queued` | `deferred` | `queued` | `FireAt(None)`, `PrepareLeaseExpiresAt(None)` |
| `Dispatch` | `queued` | `dispatched` | `DispatchedAt(now)`（+认领方补 `PrepareLeaseExpiresAt`） |
| `Running` | `dispatched`, `waiting_local_directory` | `running` | `StartedAt(now)`, `WaitReason(None)`, `PrepareLeaseExpiresAt(None)` |
| `WaitingLocalDirectory` | `dispatched` | `waiting_local_directory` | `WaitReason(Some(reason))` |
| `Progress` | 任意 8 个 | ≡ | 空 |
| `Message` | 任意 8 个 | ≡ | 空 |
| `Completed` | `running` | `completed` | `CompletedAt(now)` |
| `Failed` | 全部 5 个非终态 | `failed` | `CompletedAt(now)`, `FailureReason`, `ErrorMessage`, `WaitReason(None)`, `PrepareLeaseExpiresAt(None)` |
| `Cancelled` | 全部 5 个非终态 | `cancelled` | `CompletedAt(now)`, `CancelledBy(..)`, `PrepareLeaseExpiresAt(None)` |
| `Deferred` | ∅（仅创建） | `deferred` | — |
| `Reclaim` | `dispatched` | ≡ `dispatched` | `DispatchedAt(now)` |
| `RequeueAfterClaimFailure` | `dispatched` | `queued` | `DispatchedAtCleared`, `PrepareLeaseExpiresAt(None)` |

不变式（`state/tests.rs` + `tests/state_properties.rs` 属性测试）：

1. **终态吸收**：终态上任何*改变状态*的事件 ⇒ `TaskError::TerminalState`；
   `Progress`/`Message` 在终态仍是 `Ok` 空操作（上游会晚到通知）。
2. **失败不留半截写入**：`apply` 先 `validate` 后写；`Err` ⇒ 行逐字节未变
   （属性测试逐事件断言）。M3-6 的 CAS 依赖这一点：坏状态会被当成源状态写下去。
3. `apply` 到不了 `deferred`：`Deferred` 是仅创建事件，在既有行上 ⇒ `IllegalTransition`。
4. `waiting_local_directory` 必有非空 `wait_reason`（空字符串 ⇒ `MalformedEvent`）。
5. `Failed { reason: Manual }` ⇒ `MalformedEvent`（见 §3.3）。

### 2.5 创建路径

| 构造器 | 结果 |
| --- | --- |
| `TaskState::enqueue(budget)` | `∅ → queued`，`attempt=1`，`max_attempts=budget.ceiling()` |
| `TaskState::defer(budget, fire_at)` | `∅ → deferred`，写 `fire_at`（`None` = 等外部信号） |
| `TaskState::from_retry_child(child, parent_task_id, fire_at)` | 重试子任务：`child` 携带 `RunKind`/`TaskLink`/原因，落 `delegated_from_task_id` / `escalation_for_task_id` |

构造器返回 `(TaskState, TaskTransition)`，`transition.from == None`。这类迁移
**不能**走 `TaskStore::commit`（那里要有源状态才能写 `WHERE status = <from>`）。

---

## 3. 失败分类与重试

### 3.1 两级分类（`retry.rs`）

- **粗分类 `FailureClass`（5 个）**：逐字来自迁移 `055`
  (`055_task_lease_and_retry.up.sql`)：`agent_error` / `timeout` /
  `runtime_offline` / `runtime_recovery` / `manual`。
- **细化原因 `FailureReason`（31 个）**：上游 `server/pkg/taskfailure/failure.go`
  的 `allReasons` 恰好 **27** 个（顺序逐字相同），加上平台侧的 4 个值
  （`codex_semantic_inactivity` / `agent_fallback_message` /
  `codex_resume_oversized` / `manual`）—— 后 4 个不在上游 `allReasons` 里，
  但确实会出现在同一列。`FailureReason::ALL` 是「27 + 前两个平台值」= 29，
  用于逐字比对上游顺序；另外 2 个由 `parse`/`as_str` 的用例覆盖。

`FailureClass` **不决定是否重试**。上游自动重试判定读的是细化原因集合
（`retryableReasons`，`service/task.go`），不是粗类。因此：

```
is_retryable() = { runtime_offline, runtime_recovery, timeout,
                   codex_semantic_inactivity, agent_error.provider_network,
                   skill_bundle_unavailable }
```

`retryable_set_equals_upstream_retryable_reasons` 逐字比对（排序后比较 + 断言
上游集合大小 = 6）。

### 3.2 粗类回退规则是**本仓的规则**（不是上游的）

`055` 早于平台上后来新增的细化原因，所以「新原因属于哪个粗类」上游没有明文。
本 crate 在 `FailureReason::class()` 里给出映射，并在模块文档里标注
**「回退规则属本仓约定」**：`agent_error.*` → `agent_error`；
`timeout` / `queued_expired` / `iteration_limit` / `runtime_cli_timeout` → `timeout`；
`runtime_offline` / `runtime_reconnect_timeout` → `runtime_offline`；
其余运行时/环境类 → `runtime_recovery`。M3-6 若需要写粗类，写的是这个映射。

### 3.3 `manual` 永远不进 `failure_reason`

上游人工取消写的是 `status='cancelled' AND cancelled_by_type='user'`，
**不写** `failure_reason`。所以 `Manual` 在本 crate 里是一个分类标签，
`is_persisted_in_failure_reason_column()` 对它返回 `false`；把
`Manual` 当 `failed` 的原因提交 ⇒ `MalformedEvent`。

### 3.4 重试决策

| 输入 | 出处 |
| --- | --- |
| `attempt` / `max_attempts` | `055`（`DEFAULT_ATTEMPT=1`, `DEFAULT_MAX_ATTEMPTS=2`） |
| `max_attempts <= 1` ⇒ 关闭重试 | `055` 注释「1 disables retry」 |
| `provider_network` 上限 = 3 | 上游 `providerNetworkMaxAttempts`（`task.go:5192+`） |
| `provider_network` 末次冷却 = 5s | 上游 `providerNetworkFinalRetryWait` |
| `runtime_offline` 延后 = 1s | 上游 `runtimeOfflineRetryDeferral`（走健康门控提升） |

`decide_retry(reason, budget, gate) -> RetryDecision`：
`Retry`（带 `delay_secs` + 子任务载荷，`retry_delay_secs` 只在
`provider_network` 末次重试与 `runtime_offline` 上非零）/ `Skip(RetrySkip)`。
`RetrySkip` 区分 6 种跳过：`ReasonNotRetryable` / `BudgetExhausted` /
`AutopilotRun` / `TriageRun`（这两类跑有自己的恢复通道）/ `NoRunnableLink` /
`PendingSuccessor`（唯一槽位已被未开工的后继占住，建子任务也会被
`ON CONFLICT DO NOTHING` 吃掉）。
`RetryGate` 用两个枚举承载「是否允许 resume / 是否允许新建会话」，
避免全 `bool` 结构体；`is_resume_unsafe()` 的 6 个原因不允许 resume 原会话
（续一个已被污染的会话会放大故障）。

---

## 4. 租约与僵尸清扫

真实列只有三个：`dispatched_at`、`prepare_lease_expires_at`、`started_at`。
**`last_heartbeat_at` 已被迁移 `069` 删除** —— 存活证据是 daemon 级心跳
`agent_runtime.last_seen_at`（读侧 `COALESCE(last_seen_at, updated_at)`），
由 `RuntimeLiveness { Unbound, Dangling, Known { online, heartbeat_at } }` 承载。

| 阈值 | 值（秒） | 上游出处 |
| --- | --- | --- |
| `PREPARE_LEASE_SECS` | 45 | `service/task.go:195` `prepareLeaseDuration` |
| `PREPARE_LEASE_EXTEND_INTERVAL_SECS` | 15 | `startTaskPrepareLeaseExtender` |
| `CLAIM_RECOVERY_SECS` | 90 | `service/task.go:194` `claimResponseRecoveryWindow` |
| `RUNTIME_STALE_SECS` | 150 | `service/task.go:189` `RuntimeClaimFreshnessSeconds` |
| `DISPATCH_TIMEOUT_SECS` | 300 | `cmd/server/runtime_sweeper.go:74` |
| `RUNNING_TIMEOUT_SECS` | 9000 | `cmd/server/runtime_sweeper.go:89` |
| `RUNTIME_RECONNECT_GRACE_SECS` | 10800 | `runtime_sweeper.go:43` `defaultRuntimeReconnectGrace`（3h） |
| `QUEUED_GRACE_SECS` | 10800 | `ExpireStaleQueuedTasks` 复用 `@reconnect_grace_secs` |

全部经 `StalePolicy`（7 个 `u64`）**由调用方传入**：`mc-task` 不依赖 `mc-config`，
策略值读配置是 M3-6/M3-7 的事。

| 判定函数 | 输入 | 输出 |
| --- | --- | --- |
| `stale_task_verdict` | 行 + runtime 存活 + policy + now | `StaleVerdict::{Keep, Fail{reason,message}, Reclaim}` |
| `queued_expiry_verdict` | `queued` 行年龄 | `Keep` / `Fail(QueuedExpired)` |
| `reconnect_retry_verdict` | `deferred` 重试行 + runtime | `Keep` / `Fail(RuntimeReconnectTimeout)` |
| `offline_runtime_verdict` | runtime 离线 + 宽限 | `Keep` / `Fail(RuntimeOffline)` |
| `reclaim_stale_dispatch` | `dispatched` 行 | 重投递（`TaskEvent::Reclaim`）或按序报错 |

`reclaim_stale_dispatch` 的错误优先级**是有序的**，因为它决定 M3-6 记录哪个
失败原因：`IllegalTransition` → `AlreadyStarted` → `ReclaimWindowOpen` →
`LeaseStillActive` → `RuntimeNotEligible`。

两个容易搞反的点，用例钉住了：

- **`prepare_lease_expires_at IS NULL` 不是「永久守护」**：NULL 只表示没有活租约，
  清扫照样按 `dispatched_at` 的挂钟上限判定。
- **`within_reconnect_grace` 故意不看 `online`**：宽限窗口是「哪怕掉线也再等一等」，
  与「现在是否在线」正交。

四条清扫语句的 `SET` 列并不一致（`FailStaleTasks` 清 `prepare_lease_expires_at`，
`FailTasksForOfflineRuntimes` 只清 `wait_reason`）。本 crate 给出的写集是它们的
**超集**，M3-6 按语句收窄 —— 列级差异是 SQL 适配层的事，不是状态机的事。

---

## 5. 取消与取消确认

### 5.1 三种取消不是一回事

| 上游函数 | `cancelled_by_type` | `error` / `failure_reason` | 本 crate 表达 |
| --- | --- | --- | --- |
| `CancelAgentTask`（`agent.sql:1564`） | `system` | **NULL（不动）** | `Cancellation::by_system()` |
| `CancelAgentTaskWithReason`（`:1676`） | `system` | 写 `error` + `failure_reason` | `Cancellation::by_system_with_reason(msg, reason)` |
| `CancelAgentTaskByUser`（`:1573`） | `user` | **NULL（不动）** | `Cancellation::by_user(id, name)` |

三者都写 `cancelled_by_id` / `cancelled_by_name`（迁移 `458`），并把
`prepare_lease_expires_at` 置 NULL。`WHERE status IN (5 个非终态)` ⇒
终态行取消是 **CAS miss**：

```
plan_cancellation(..) -> CancelOutcome::AlreadyTerminal { status }   // 不是错误
```

`is_idempotent_replay()` 只对 `cancelled` 为真；对 `completed`/`failed` 是
「冲突」，M3-6 要区别对待（上游路由对已完成任务取消返回 200 + 现状，不是 500）。

### 5.2 人工取消的旁路：`delivered_comment_ids` 重算

`CancelAgentTaskByUser` 里有一个 CASE：人工取消委派子任务时，若该任务的
`trigger_comment_id` / `coalesced_comment_ids` 指向的评论里存在 system 作者的
「恢复信号」，就要**重算** `delivered_comment_ids`（否则那条恢复信号永远不会被
终态确认）。本 crate 把它拆成纯判定：

| 输入 | 输出 |
| --- | --- |
| 非人工取消 | `DeliveredCommentsPlan::Untouched` |
| 人工 + 无 `trigger_comment_id` + `coalesced_comment_ids` 为空 | `KeepUnchanged`（廉价路径） |
| 人工 + 有评论键 + 探针未发现恢复信号 | `KeepUnchanged` |
| 人工 + 有评论键 + 发现恢复信号 | `RecomputeRecoverySignalReceipts` |

`recovery_signal_present` 是**探针的结果**（「有没有可能」），真正的 join 在
M3-6；判定函数只决定走哪条分支。

`plan_cancellation(&TaskState, ...)` **不改调用方的行**（内部 clone）：
返回值里的 `transition` 才是写单，调用方拿它去 CAS。

### 5.3 取消确认（`cancel-ack`）

daemon 收到取消后回一包（`daemon.go:5187` `TaskCancelAckRequest`）：
`branch_name` / `durable_work_dir` / `error_message` / `failure_reason`。
本 crate 输出 `CancelAckPlan { writes, rebroadcast }`，规则 5 条：

| # | 规则 | 对应上游语句 |
| --- | --- | --- |
| 1 | 只在 `status='cancelled'` 上写 | 三条语句共用的 WHERE |
| 2 | 值先 trim，空白视为未提供（跳过，不覆盖） | 路由侧 sanitize |
| 3 | `branch_name` / `durable_work_dir` 用 COALESCE 语义：**已有值不覆盖** | `SetAgentTaskBranchName:1640` / `SetAgentTaskDurableWorkDir:1650` |
| 4 | `error` 是**全有全无**：仅当 `error IS NULL OR error=''` 才写（同时 COALESCE `failure_reason`） | `SetAgentTaskErrorIfEmpty:1662` |
| 5 | `rebroadcast = !writes.is_empty()`（真的写了列才重播） | `RebroadcastCancelledTask` |

端点**永远返回 200**；`is_noop()` 不是错误 —— 重放的 ack 是正常的
（daemon 重连后会重发）。写失败才是 500（上游也是 loud fail）。

> **不在本片范围**：`FinalizeDeferredCancelledChat`（`service/task.go:3305`）属于
> chat 域（M5）。本 crate 只覆盖任务行的取消列。

---

## 6. 用量结算（纯计算）

### 6.1 `task_usage` 行形状（`usage.rs`）

上游 `contracts/upstream-schema.sql` 的 `task_usage`：11 列 + 代理主键 `id`
（`gen_random_uuid()`）+ `UNIQUE (task_id, provider, model)`。
本 crate 的 `TaskUsageRow` **不含** `id`：这一行的身份就是自然键
`UsageKey { task_id, provider, model }`，`UpsertTaskUsage` 从不需要 `id`。

两条语义要点：

- **覆盖，不是累加**。同一自然键再报一次 ⇒ `DO UPDATE SET … = EXCLUDED.…`
  （`overwrite_with`），`updated_at` 刷新（它是小时汇总的脏标记），
  `created_at` 不动。
- **`cost_usd_ticks = 0` 不等于「花了 0 元」**。单位是 1e-10 USD，而**所有**
  不知道成本的 daemon 都发 0；存 0 会让读取侧以为真有 $0，从而不再按 rate table
  估算。所以 `authoritative_cost_ticks(ticks)`：只有 `> 0` 才是权威值，否则
  **NULL**（= 请按价目表估算）。上游原文见 `daemon.go:4804`。

其余归一化：

| 规则 | 本 crate | 上游 |
| --- | --- | --- |
| provider 归一化 | `normalize_provider` = trim + lowercase | `normalizeProvider`（`daemon.go:294`） |
| 空 provider 回退 | `resolve_provider(reported, runtime_provider)` | 用任务的 runtime provider 兜底（否则 `auto` 这类模型 id 会存成 `''` 并定价 $0）|
| 单位 | `COST_TICKS_PER_USD = 10^10` | `cost_usd_ticks` = 1e-10 USD |
| prompt cache 命中率 | `cache_read / (input + cache_read + cache_write)`，总量 ≤0 ⇒ `None` | `daemon.go:4866` 日志指标 |
| `UsageReport` 缺字段 | `#[serde(default)]` ⇒ `""`/0 | Go 零值语义（旧 daemon 不发新字段） |

### 6.2 汇总（读数，与上游 SQL 对齐）

| 函数 | 语义 | 上游出处 |
| --- | --- | --- |
| `TokenTotals::add` | token 求和；**未定价行只进 `uncosted_*`** | `task_usage.sql` 的 `FILTER (WHERE cost_usd_ticks IS NULL)` |
| `IssueUsageSummary`（`summarize_issue_usage`） | `task_count` = distinct task；`terminal_task_count` = 有起止时间且终态；`metered` = 终态且有 usage 行；`unreported` = 终态但无 usage | `GetIssueUsageSummary:80` |
| `RunTimeSummary`（`summarize_run_time`） | 时长 = `completed_at - started_at`（**不裁剪**负值） | dashboard 时长 rollup |
| `FailureBucket` / `classify_failure_bucket` | 非 `failed` ⇒ 非失败；`failed` 但 `failure_reason` 空 ⇒ `'unclassified'`；否则原样串 | dashboard 的 CASE（`:114/:152/:180`） |

`FailureBucket` **故意不派生 `Serialize`**：它的线格式是 SQL 字符串
（`''` / `'unclassified'` / 原始 reason 文本），派生会产出 `non_failure` 这种
数据库里不存在的值。要拿线格式用 `as_str()`。

### 6.3 小时桶（`task_usage_hourly`）

- `hour_bucket(ts)` = `date_trunc('hour', ts AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'`
  （`migrations/102:31-37`）：**显式 UTC**。实现用 `rem_euclid`，1970 之前的
  负数时间戳也按 Postgres 一样向下取整。
- 桶键 = `(bucket_hour, workspace_id, runtime_id, agent_id, project_id, provider, model)`，
  与 `UNIQUE NULLS NOT DISTINCT` 一一对应 ⇒ `project_id: Option<Id>`，
  两个 NULL 是同一个桶；`runtime_id` 非可选（rollup 只认 `runtime_id IS NOT NULL` 的行）。
- `fold_hourly(rows)` 复刻 `migrations/213_task_usage_authoritative_cost.up.sql:80-235`
  的 `recomputed` CTE：`SUM(tokens)`、`COALESCE(SUM(cost_usd_ticks), 0)`（NULL 行不计）、
  `uncosted_*` = 未定价行的 token 之和、`task_count = COUNT(DISTINCT task_id)`、
  `event_count = COUNT(*)`。
- **重算为空的桶要删掉**：脏键在明细被修正/删除后可能已无对应行，
  「聚合结果里不出现」正是 DELETE 信号（上游会真的 `DELETE`）。

---

## 7. `TaskStore` port（`store.rs`）

8 个方法，SQL 实现（M3-6）必须遵守的三条硬约束：

1. **`commit` 是 CAS**：`WHERE id = $1 AND status = <transition.from>`；
   没命中 ⇒ `CommitOutcome::LostRace`（**正常结果**，不是错误），调用方重读后重试。
2. **claim 的串行化栅栏必须在 SQL 里**：`NOT EXISTS` 的跨行条件
   （同一 agent 在同一串行化键上没有在飞行）没法用「先查后写」表达。
   `TaskState::occupies_serialization_slot` 只是读侧表达式。
3. **策略值全部由调用方传入**（`ClaimRequest.policy: StalePolicy`、`RetryBudget`）：
   本 crate 不依赖 `mc-config`。

| 方法 | 对应上游 |
| --- | --- |
| `insert(id, state)` | `CreateAgentTask`（`agent.sql:293`）；`id` 由**应用**铸造（上游 `pkg/dbid` 的 UUIDv7 让连续入队落在相邻主键区间），必须让 `idx_one_pending_task_per_issue` 真的生效 |
| `get(id)` | 读单行 |
| `commit(id, transition)` | `StartAgentTask` / `CompleteAgentTask` / `FailAgentTask` / `CancelAgentTask` 等 |
| `claim_next(request)` | `ClaimAgentTask`（`agent.sql:743`），挑序 `priority DESC, created_at ASC, id ASC` |
| `cancel(id, cancellation, at)` | 三种 Cancel 语句；适配层串 `delivered_comments_plan` → `plan_cancellation` → CAS |
| `apply_cancel_ack(id, ack)` | 三条 ack 语句；返回计划让调用方知道要不要重播 |
| `list_usage(task_id)` | `GetTaskUsage`（`ORDER BY model`） |
| `upsert_usage(row)` | `UpsertTaskUsage`（`ON CONFLICT … DO UPDATE`） |

`claim_next` 的完整谓词（除状态与归属外）还包括 wakeup 门、`agent.runtime_id`
的重新绑定、runtime 可见性、以及上面第 2 条的串行化 `NOT EXISTS` —— 都在
`ClaimAgentTask` 的 SQL 里，端口只负责把结果（`TaskClaim { task_id, state, transition }`）
交回来。**认领要自己补 `PrepareLeaseExpiresAt(prepare_lease_deadline(now, policy))`**：
`TaskEvent::Dispatch` 的写集只有 `DispatchedAt`，上游是在同一条语句里写租约的。

---

## 8. 验收证据

| 项 | 结果 |
| --- | --- |
| 单元 + 集成测试 | `cargo test -p mc-task`：**107 个测试全绿**（105 lib + 2 proptest）+ 1 doc-test |
| 状态迁移矩阵 | 96 组合穷举：34 合法 / 62 非法（`transition_matrix_is_exhaustive_and_exact`） |
| `events.go` 逐行 | 9 个 `task:` 常量的名字与边都有对应用例（§2.2 表） |
| 属性测试 | `proptest`：任意事件序列不产生非法状态 + 终态吸收（`tests/state_properties.rs`） |
| 租约/重试/取消/用量 | 各自模块的判定表逐条用例（§3/§4/§5/§6 的表） |
| 门禁（默认集） | `bash scripts/gates.sh` → **8/8 PASS**（①fmt ②build ③clippy ④clippy-test-util ⑤test ⑦route-parity ⑨conformance ⑩file-size，153s） |
| 门禁（含库） | `bash scripts/gates.sh --with-db` → **10/10 PASS**（另加 ⑥db migrate+e2e、⑧schema-drift，59s） |
| 文件尺寸（门 ⑩） | 全部 ≤ 800 行（最大 `state.rs` 779）；测试拆到 `state/`、`retry/`、`lease/`、`cancel/`、`usage/`、`store/` 子目录 |

**本片对 `docs/15` §4 的三处修正**（已在上文各节标注）：

1. `delegated_failure` **不是状态**，是 `failed` 的子类（§2.1）。
2. `events.go` 是 **9 条** `task:` 事件，不是 8 条（`docs/15` §2.3 把
   `progress`/`message` 并成了一行）（§2.2）。
3. `manual` **不写** `failure_reason`（上游人工取消只写 `cancelled_by_type='user'`）（§3.3）。

**与计划文字不一致的实测值**：`claim_recovery_secs` 是 **90**
（`service/task.go:194` `claimResponseRecoveryWindow = 90 * time.Second`），
早期笔记里的 240 是错的，`lease.rs` 的常量表按上游 90 实现。
