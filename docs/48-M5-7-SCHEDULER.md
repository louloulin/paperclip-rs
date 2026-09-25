# M5-7：调度租约内核（LUM-1566）

承接 `docs/44-M5-PLAN.md` §3.2 的 `mc-scheduler/**` + `mc-repos/src/scheduler.rs`：把上游
`server/internal/scheduler/{spec,manager,db_ops}.go`（1,153 行）搬进本仓，让「哪个 `(job, scope, plan_time)`
该跑一次」这件事在**多实例并发**下仍然恰好发生一次。本片是 B 波唯一的 **0 路由**切片（`docs/44` §1
「可独立验收」），所以验收面全部在**内核 + 真库**上，⑦ 路由读数**零变化**（§6）。

- **base**：`feat/multica-rs-initial` @ `e75aca5`
- **记录号**：按 `docs/37` §36.5 的表分配（`46 = M5-1`、`47 = M5-6`、**`48 = M5-7`**）⇒ 本文件是 `docs/48`，
  代码里 4 处引用已同步（`mc-scheduler/src/lib.rs:3,57`、`tests/lease_db.rs:19`、`mc-repos/src/scheduler.rs:239`）。
  若 cycle 在合并时改按「先合者占 46」执行，改动只是本文件的一次 `git mv` + 这 4 处引用。
- **分支**：`agent/devbox5/e6b28b1a26b1`
- **提交**：`b4367e3`（仓储）→ `33f6a0e`（内核）→ `27db0e4`（测试 + `connect`）→ `f61a364`（fmt 收口）
- **写集**：10 文件 / +3,243 −8；**未动**任何 `Cargo.toml` / `Cargo.lock` / `scripts/**` /
  `docs/fixtures/**`（§7.1 说明为什么 `apps/mc-server/src/main.rs` 也不在写集里）

## 1. 交付面

| # | 文件 | 行数 | 内容 | 门禁归属 |
| - | --- | ---: | --- | --- |
| ① | `crates/mc-scheduler/src/spec.rs` | 451 | `JobSpec` / `HandlerInput` / `HandlerResult` / `Scope` / `CatchUpMode`；`validate`、`retry_delay`、`stale_secs`、`floor_plan` | ③⑤ |
| ② | `crates/mc-scheduler/src/manager.rs` | 646 | `Manager` / `Options` / `SchedulerHandle`；tick 循环、认领、handler 隔离（`spawn` + `timeout` + `AbortOnDrop`）、心跳任务、终态写回 | ③⑤ |
| ③ | `crates/mc-scheduler/src/db_ops.rs` | 257 | `Claim{kind,lease}` / `ClaimKind` + 仓储封装（0 行 ⇒ `LeaseLost`）+ `Heartbeat` 句柄；**无 SQL** | ③⑤ |
| ④ | `crates/mc-scheduler/src/error.rs` | 124 | `SchedulerError`（thiserror）+ `ErrorClass{Retryable,Permanent,LeaseLost}` 三分类 | ③⑤ |
| ⑤ | `crates/mc-scheduler/src/lib.rs` | 70 | 对外重导出（含 `SchedulerRepo`，故 `main.rs` 只需一条依赖边，见 §7.1） | ③⑤ |
| ⑥ | `crates/mc-scheduler/src/jobs/mod.rs` | 27 | `register_all` **空注册表**（M5-8 加 2 行） | ③⑤ |
| ⑦ | `crates/mc-repos/src/scheduler.rs` | 621 | `sys_cron_executions` 的**全部手写 SQL**（claim/heartbeat/finish/stale/latest_plan）+ `SchedulerRepo` | ③⑤ |
| ⑧ | `crates/mc-scheduler/tests/lease.rs` | 398 | 14 条纯逻辑用例（无 DB） | ⑤ |
| ⑨ | `crates/mc-scheduler/tests/lease_db.rs` | 491 | 7 条真库用例（`#[ignore]`，本片手工跑，§5 / §7.2） | 手工 |
| ⑩ | `crates/mc-repos/tests/scheduler_lease_db.rs` | 250 | 4 条真库用例（SQL 层） | ⑥ |

上游 1,153 行 → 本仓 src 2,196 行（含仓储层 621）= **1.90×**，与 `docs/44` §5.3 给本仓其它波次的
折算率（1.7–2.0×）一致；单文件最大 646 行（硬门 800，⑩ 绿）。

## 2. 上游映射（逐条）

| 上游符号（`server/internal/scheduler/`） | Rust 落点 | 备注 |
| --- | --- | --- |
| `spec.go:24` `CatchUpMode.String()`、`:132` `Scope.String()` | `spec.rs` `CatchUpMode` / `Scope` 的 `Display` | 逐字保留字面量；`CatchUpMode` 另加 `FromStr`（本地要解析配置） |
| `spec.go:96` `ScopesProvider`、`StaticScopes` | `spec.rs` `ScopeProvider`（type alias）+ `static_scopes` / `global_scopes` | 作用域集合在构造时**冻结**一份 |
| `spec.go:118` `Handler`、`HandlerResult` | `spec.rs` `Handler`（`Arc<dyn Fn(HandlerInput) -> BoxFuture>`）、`HandlerResult` | 返回值喂审计行 |
| `spec.go:143` `PlansForScope` | `spec.rs` `JobSpec::plans_for_scope` | 一旦设置即**替代** cadence 网格（autopilot 用） |
| `spec.go:174` `validate` | `spec.rs` `JobSpec::validate` | 错误文案带字段名，逐条对应 |
| `spec.go:246` `retryDelay` | `spec.rs` `retry_delay` | 索引夹取 + 空表按 0 |
| `spec.go:253` `floorPlan` | `spec.rs` `floor_plan` | **从 Go 零值 0001-01-01 起算**（§3.4） |
| `db_ops.go` 的 `int64(StaleTimeout / time.Second)` | `spec.rs` `stale_secs_f64` | 整秒截断、下限 1 |
| `db_ops.go:66` `tryClaim`（三个 `bool`） | `db_ops.rs` `Claim{kind,lease}` + 仓储 `claim_fresh` / `claim_steal_or_retry` | 三归宿收敛成枚举（`Won`/`Stole`/`Conflicted`） |
| `db_ops.go` `heartbeat` | `db_ops.rs` `Heartbeat::new(...).beat()` + 仓储 `heartbeat` | 三条件守卫 |
| `db_ops.go` `finishSuccess` / `finishFailure` | `db_ops.rs` `finish_success` / `finish_failure` + 仓储同名方法 | `default_error_code` / `truncate_error_msg`（4000 字）逐字 |
| `db_ops.go:26` `markStaleAsFailed` | `db_ops.rs` `mark_stale_as_failed` | 每个 job 每 tick 都跑 |
| `db_ops.go:333` `LatestPlan` + `:351` `RetryEligible` | `mc-repos/src/scheduler.rs` `LatestPlanInfo` + `retry_eligible` | 判据顺序逐条对应 |
| `manager.go:23` `classifyError` | `error.rs` `SchedulerError::code()` / `class()` | 5 个码逐字，另加 `Permanent`（§3.6） |
| `manager.go` `Manager` / `Options` / `Register` / `Run` | `manager.rs` `Manager` / `Options` / `register` / `run_once`（+ `spawn` 挂后台） | `Options` 零值在 `new` 里归一 |
| `manager.go` `RunJob` / `runClaimed` / `runHeartbeats` | `manager.rs` `run_job` / `run_claimed` / `run_heartbeats` | 心跳是**分离的取消源**（超时不停续期，上游同） |
| `main.go` 的调度主循环 spawn | **未接线**（§7.1） | `Manager::spawn()` 已就绪，差一条 manifest 边 |

## 3. 契约：exactly-once 的完整口径

### 3.1 认领的三归宿（唯一同步点 = 唯一键）

`try_claim` 只发**两条**语句，没有进程内闸门，所有同步都交给
`uq_sys_cron_execution (job_name, scope_kind, scope_id, plan_time)`：

1. `INSERT … ON CONFLICT DO NOTHING RETURNING` —— 赢家一次插入拿到租约（`lease_token` 由 DB 的
   `gen_random_uuid()` 生成；`id` 用 `Uuid::now_v7()` 求主键局部性）。输家 0 行，**碰都不碰**已存在的行
   —— 绝不轮换别人的 token。
2. 冲突（0 行）才走 `UPDATE … WHERE (status='FAILED' AND COALESCE(next_retry_at, now) <= now)
   OR (status='RUNNING' AND stale_after < now AND $allow_stale_reentry) AND attempt < max_attempts`
   —— 能不能夺由**单条语句**在 DB 里判。

三个归宿：`Won`（插入成功）/ `Stole`（夺下别人的陈旧租约，`attempt+1`、token 轮换）/ `Conflicted`
（0 行，**调用方一律当 no-op**，上游同）。「`RowsAffected == 1` 才算赢」是这里唯一的判据。

### 3.2 两条不变量

* **陈旧是算出来的，从不物化**：`status='RUNNING' AND stale_after < now()`。没有 `stale` 状态列，
  所以偷租约和 `mark_stale_as_failed` 看的是同一个判据，不可能漂移。
* **三条守卫**：心跳与两个终态写的 `WHERE` 都是 `id = $1 AND lease_token = $2 AND status = 'RUNNING'`。
  租约被偷后旧持有者手里的 token 已轮换 ⇒ 它的终态写影响 **0 行** ⇒ `Err(LeaseLost)`。
  这是 R2 里「租约被偷」那条的唯一证据来源（§5 用例 5）。
* **时钟只有 DB 的 `now()`**：`db_now()` 每 tick 读一次、所有 scope 共用；`stale_after` 由 SQL 侧的
  `$2 + make_interval(secs => $3)` 推进。两实例进程时钟漂移不会改变 `plan_time` 分桶（`spec.rs` 的
  `stale_secs_f64` 与仓储的 `make_interval` 都吃同一份 `db_time`）。

### 3.3 失败与重试（逐字照 `retryDelay` + `RetryEligible`）

| 处置 | `next_retry_at` | `attempt` 覆盖 | 下一 tick 能被重认领？ |
| --- | --- | --- | --- |
| `ErrorClass::Retryable` 且 `attempt < max_attempts` | `db_time + retry_delay(attempt)` | 不动 | 退避到期后 ✓ |
| `ErrorClass::Retryable` 且 `attempt >= max_attempts` | `NULL` | 不动 | ✗（预算烧完） |
| `ErrorClass::Permanent`（本地新增，§3.6） | `NULL` | `Some(max_attempts)` | ✗ |
| `ErrorClass::LeaseLost` | 写不进去（0 行） | 不动 | 由新持有者决定 |

`LatestPlanInfo::retry_eligible` 把同一件事重述一遍（`manager` 用它做追赶游标）：
`found && status == FAILED && attempt < max_attempts && (next_retry_at IS NULL || next_retry_at <= now)`。
**`RUNNING` 行永远不可重试**（在飞的不算欠账）—— 这一条在 §5 的用例里被显式断言过。

### 3.4 `plan_time` 取整：从 **Go 零值**起算，不是 Unix 纪元

上游 `time.Time.Truncate` 把时间截到「距 **0001-01-01T00:00:00Z** 的整数倍」，而本地容易误写成
「距 1970-01-01 的整数倍」——两者对 7 小时（25200s）网格**不等价**：
`62_135_596_800 mod 25_200 = 7_200` ⇒ 合法桶是 `u ≡ 18_000 (mod 25_200)`。
`floor_plan` 因此用 `i128` 纳秒 + `rem_euclid`（负时间也安全）：
`floor_plan(epoch, 7h) == -7_200`（= 05:00/12:00/19:00 UTC），而 `-7_199 → -7_200`、`-7_201 → -32_400`。
`cadence <= 0` ⇒ 原样返回 `eligible`（上游 `c <= 0` 直接返回）。`tests/lease.rs` 第一条用例
就是为这个差异写的（它同时排除了「按 Unix 纪元截断」的错实现）。

### 3.5 运行纪律（handler 的隔离与关闭）

* handler 跑在 `tokio::spawn` 出的任务里，外面套 `timeout(job.run_timeout, …)`；超时 ⇒ `abort_now()`
  + `Err(RunTimeout)`。`timeout` 只是丢掉 `JoinHandle`（= detach），**必须**真的 abort，否则 handler
  会在「已经写了 FAILED」之后继续写业务行。
* `struct AbortOnDrop(AbortHandle)` 把 handler 的寿命绑在本次 `run_claimed` 上：关闭 / 取消导致
  future 被 drop 时，还在跑的 handler 一起被 abort（DoD 的「不泄漏 task」）。
* 心跳是**分离的取消源**（`run_timeout` 到期不该停续期，上游同），`MissedTickBehavior::Delay`、
  跳过立即触发的第一拍、每次续期单独 5s 超时；`LeaseLost` ⇒ warn + 退出心跳任务。
* **终态写入之前先 cancel 并 await 心跳任务**：保证「跑完之后不会再有心跳 UPDATE」与
  「终态行是这一轮的最后一次写」两个性质（§5 用例 5 靠它拿确定性）。
* `SchedulerHandle::shutdown()`：cancel → await 主循环 → `Drop` 兜底。关闭后**不**留 RUNNING 悬挂
  （用例 7 验的是「handler 被 abort、主循环 500ms 内退出、行仍是 RUNNING 而不是被写成 SUCCESS」）。

### 3.6 有意分歧（都已登记，无一是为了省事）

| # | 分歧 | 理由 |
| - | --- | --- |
| 1 | `ErrorClass` **多了 `Permanent`** | 上游只有「按 `attempt` 重试」一条路；本地把「不可重试」（如规格错误、鉴权拒绝）单独成类，落 `attempt=max_attempts + next_retry_at=NULL`，语义是「预算一次烧完」而不是「再也不写行」 |
| 2 | `Claim` 用**枚举**而不是三个 `bool` | 上游三个 `bool` 有 8 种组合、合法只有 3 种；枚举让「`Won`/`Stole` 必带租约」成为类型事实 |
| 3 | `JobSpec` **多 builder、`CatchUpMode` 多 `FromStr`** | 上游裸结构体零值可跑（Go 零值有语义），Rust 侧把「必填」推到类型上并在 `validate` 里给出带字段名的错误 |
| 4 | `ErrorClass::Permanent` 与 `code()` 覆盖「规格 / 重名 / 仓储」三类错误 | 上游这几类错误在 `Register` / `dbNow` 处直接返回，没有 `error_code`；本仓要求审计行总有稳定的 `error_code` |
| 5 | `SchedulerRepo::connect(url,max,min)` 是本仓新增入口 | 上游用包级 `db`；本仓 `mc-scheduler` 不能依赖 `mc-db`（`docs/44` §5.4），为内核真库测试留一个入口 |
| 6 | 注册表在 `spawn` 时**冻结**（`Arc` 切片） | 上游可在运行期 `Register`；本仓按 `docs/44` §2.3「一个 spawn 点 + 两行注册」的顺序边把注册收敛到启动期 |

## 4. 为什么 `mc-scheduler` 里没有 SQL

内核（`manager.rs` / `db_ops.rs` / `spec.rs`）**一行 SQL 都没有**：全部手写 SQL 集中在
`mc-repos/src/scheduler.rs`（⑦）。收益三条：

1. **可测**：14 条纯逻辑用例（⑤）不需要库；7 条内核真库用例（⑨）只依赖 `SchedulerRepo` 一个面。
2. **复用**：M6（`jobs_plugin_hook.go`353）与 M9/M3（`jobs_task_usage.go`120）只加 `jobs/*.rs`，
   不需要再碰 SQL（`docs/44` §2.3 的跨波登记）。
3. **依赖方向**：`mc-scheduler` 的依赖是 `mc-autopilot` / `mc-core` / `mc-repos` —— 不依赖
   `mc-http`、不依赖 `mc-db`、**没有新增任何第三方依赖**（`docs/15` §8.4 的仲裁）。

`jobs/mod.rs` 的 `pub mod autopilot;` / `pub mod issue_wakeup;` 是 **M5-0 骨架里已有的**（两个 9/8 行的
文档占位），本片只在其下追加 `register_all` 空实现，**没有扩大 M5-8 的写集**。

## 5. 真库验证

一次性测试库（不进仓）：角色 `mc_lum1566`（本片临时，已 `createdb` 以满足 ⑧）、库
`multica_lum1566`；`mc-migrate run --dir migrations` 应用 **566** 个迁移（含
`113_sys_cron_executions`），`\d sys_cron_executions` 的 22 列与
`uq_sys_cron_execution UNIQUE (job_name, scope_kind, scope_id, plan_time)` 均已实测在位。

| 套件 | 命令 | 读数 |
| --- | --- | --- |
| 内核真库（⑨） | `MULTICA_TEST_DATABASE_URL=… cargo test -p mc-scheduler --test lease_db -- --ignored --test-threads=1` | **7 passed / 0 failed**（首轮 4.09s / 复跑 7.76s） |
| 仓储真库（⑥） | `cargo test -p mc-repos --test scheduler_lease_db -- --ignored` | **4 passed / 0 failed**（0.28–0.29s） |

`docs/44` §6.2 给 M5-7 的四条硬要求与用例的对应：

| §6.2 要求 | 用例 |
| --- | --- |
| 并发 claim **只一个**成功 | 内核 ①`manager_runs_a_claimed_job_exactly_once`（两个 `Manager` 抢同一个 `(job,scope,plan_time)`，handler 计数必须 == 1）+ 仓储 `concurrent_fresh_claim_has_single_winner` |
| 租约被偷后写终态**匹配 0 行** | 内核 ⑤`stale_lease_is_stolen_and_the_old_holder_gets_lease_lost` + 仓储 `stolen_lease_makes_old_holder_terminal_write_a_noop` |
| stale 转 `FAILED` | 仓储 `stale_lease_is_closed_as_failed`（`error_code='stale_timeout'`，且只影响本 job 的陈旧行） |
| retry 退避 | 内核 ②`failure_schedules_a_backoff_retry_that_blocks_early_reclaim`、③`permanent_failure_burns_the_retry_budget`、④`every_plan_returns_to_the_failed_bucket_when_its_backoff_has_burned` |
| 一条真库 lease 测试 | 上表 11 条全部真库 |

另两条内核用例：⑥`heartbeat_renewal_pushes_the_stale_window_forward`（§5.1）、
⑦ `shutdown_stops_the_loop_and_aborts_the_running_handler`（`timeout(500ms, handle.shutdown())` 通过、
handler 未写完、行仍是 `RUNNING`/`attempt=1`、`retry_eligible(false)`、窗口未到的 `mark_stale_as_failed` 返回 0）。

### 5.1 一个被规格不变量逼出来的测试重设计（教训）

第一版心跳用例写的是「handler 慢跑 3s，窗口 2s，看另一个实例偷不走」——
**它连 `validate` 都过不了**：`stale_timeout > run_timeout` 是硬不变量，所以「活着的 handler 比窗口
活得久」在合法规格里**造不出来**（那种 handler 会先撞 `run_timeout` 被 abort）。心跳的可观测效果
只能这样验：**直接持租约续期**，看「本应过期」的时点上别人还偷不偷得走（1s 窗口，t=0.5s 与 t=1.0s
各续一次 ⇒ t=1.2s 的抢占得到 `Conflicted`；对照面无续期的同形场景在另一条用例里确实是 `Stole`）。

推论（写进 M5-8 / M6 / M9 的运行约束）：**心跳真正的价值在「进程死掉」这条路径上**
—— 进程不再续期 ⇒ `stale_after` 到期 ⇒ 另一实例偷走；而进程内的 lease 之所以要续期，是为了不
让**陈旧扫描**（`mark_stale_as_failed`）误收一条还在跑的行（当 `run_timeout` 接近 `stale_timeout` 时）。

## 6. 门禁读数（当轮 `2026-09-23`，base `e75aca5`，`--with-db`）

```
overall: PASS — 10/10 gate(s) green in 317s      # 冷缓存那一轮
overall: PASS — 10/10 gate(s) green in 155s      # 复跑（②20s ③11s ⑤31s ⑥43s）
```

| 门 | 命令 | 读数 |
| --- | --- | --- |
| ① fmt | `cargo fmt --all --check` | PASS（本片先跑了一次 fmt 收口，见下行） |
| ② build | `cargo build --workspace --all-targets --locked` | PASS（82s） |
| ③ clippy | `cargo clippy --workspace --all-targets -- -D warnings` | PASS（28s，0 warning） |
| ④ clippy-test-util | `cargo clippy -p mc-http --all-targets --features mc-http/test-util -- -D warnings` | PASS（19s） |
| ⑤ test | `cargo test --workspace` | PASS：**1192 passed / 0 failed / 110 ignored**（含 `tests/lease.rs` **14 passed**） |
| ⑥ db | `mc-migrate run --dir migrations` + `cargo test -p mc-repos -p mc-http --features mc-http/test-util -- --ignored` | PASS（migrate 0 / e2e 0）：**221 passed / 0 failed / 0 ignored**（含 `tests/scheduler_lease_db.rs` **4 passed**） |
| ⑦ route-parity | `route_parity.py --quiet` + `slash_alias_audit.py --quiet` | PASS：`local 300 / baseline 290 / implemented 243 (241 real + 2 placeholder) / known_gap 213 / regression 0 / local_only 11` |
| ⑧ schema-drift | `schema_drift.py --quiet` | PASS（26s） |
| ⑨ conformance | `mc-conformance --no-db --check` | PASS：`pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306` |
| ⑩ file-size | `file_size_check.py --quiet` | PASS（单片最大 646 行 < 800） |

**交叉核验**：base `e75aca5` 那一轮 cycle（不含本片）的同两门读数是 ⑤ `1178/0/99`、⑥ `217/0`；
本片分支（代码提交 `f61a364`；其后只有 docs / 注释级改动）跑出来的是 ⑤ `1192/0/110` 与 ⑥ `221/0` —— **passed 差 +14 / +4**（正好等于本片
`tests/lease.rs` 14 例与 `tests/scheduler_lease_db.rs` 4 例），**ignored 差 +11 = 7 + 4**（无 DB URL 时
不跑的 7 条内核真库用例 + 4 条仓储真库用例）。两道差值都逐数对上了 ⇒ 本片的读数不是「抳一下多几个」。
⑦ 读数**零变化**（0 路由片，预期如此）。

一个坑记在这里：**`cargo fmt` 能把 clippy pedantic 跑红**——它把本片一处「单行 match 臂」
（`=> expr,` 是表达式，不受 `semicolon_if_nothing_returned` 管）展开成了块，于是该 lint 立刻触发。
所以**正确顺序是 fmt → clippy**（本片在 `f61a364` 补上了那个 `;`），反过来就会把红门带进提交。

## 7. 请求与交接口

### 7.1 【P0，需 owner】`apps/mc-server/src/main.rs` 的接线缺一条 manifest 边

`docs/44` §3.1 只把「`Cargo.lock` + **两个新 crate 的** `Cargo.toml`」派给了 M5-0，把
`apps/mc-server/src/main.rs` 派给了 M5-7 —— **没有任何一格负责**给 `apps/mc-server` 加
`mc-scheduler` 依赖边。实测：`apps/mc-server/Cargo.toml` 有 13 条 `mc-*` path 依赖但没有
`mc-scheduler`/`mc-repos`，`Cargo.lock` 的 `[[package]] name = "mc-server"` 的 `dependencies` 里同样没有。
而本片的纪律是「**不碰任何 `Cargo.toml` / `Cargo.lock`**」（缺依赖 = 计划有洞，要问不要自己加），
且门 ② 跑 `--locked` —— 所以如果我硬写 spawn 块，整个 workspace 当场编译不过
⇒ 本片把接线以 **ready-to-apply** 形态交在这里，而不是把红树推上去。

依赖边（`apps/mc-server/Cargo.toml` 的 `[dependencies]`，与现有 13 条 `mc-*` path 依赖同形、放哪一行都行）：

```toml
mc-scheduler = { path = "../../crates/mc-scheduler" }
```

`mc-scheduler/src/lib.rs` 已重导出 `SchedulerRepo` / `Manager` / `Options` / `SchedulerHandle` /
`jobs`，所以**只需这一条**（不需要再加 `mc-repos`）。`Cargo.lock` 会因此多出 `"mc-scheduler"`
这一项（`mc-server` 的 `dependencies` 数组是显式的）—— 这两个文件必须**在同一个提交**里改，
否则 ② 的 `--locked` 会红。

落地代码（两处，位置见下）：

```rust
    // 紧接现有 「6. 装配 axum 路由」之后、`axum::serve` 之前：
    // 注册必须在 `spawn` 之前（本片把注册表在 spawn 时冻结）。
    let mut scheduler = mc_scheduler::Manager::new(
        mc_scheduler::SchedulerRepo::new(db.clone()),
        mc_scheduler::Options::default()
            .with_runner_id(format!("mc-server-{}", std::process::id())),
    );
    mc_scheduler::jobs::register_all(&mut scheduler).context("register scheduler jobs")?;
    let scheduler_handle = scheduler.spawn();
```

```rust
    // 现有 graceful shutdown 处（`axum::serve(…).with_graceful_shutdown(…).await?` 之后）：
    scheduler_handle.shutdown().await;
    actors.shutdown().context("shutdown actors")?;
```

`register_all` 现在是空实现 ⇒ 上面这段接完只会「空转」：没有 job、每 30s 一次空 tick、
信号到达时干净退出。**这是刻意的**：M5-7 的 DoD 是「0 个 job 下能空转并干净退出」，
两个真 job 由 M5-8 往 `register_all` 里加两行（那时才需要 ⑦ 的 `-p` 收集，见 §7.2）。

### 7.2 【建议，一行改动】门 ⑥ 没收集 `mc-scheduler`

现命令（`scripts/gates.sh` 的 db 门）是 `cargo test -p mc-repos -p mc-http …` ⇒ 它不会跑
`crates/mc-scheduler/tests/lease_db.rs`。 `scripts/gates.sh` **不在本片写集内**，所以这里只提建议：

```diff
- cargo test -p mc-repos -p mc-http --features mc-http/test-util -- --ignored
+ cargo test -p mc-repos -p mc-http -p mc-scheduler --features mc-http/test-util -- --ignored
```

在那之前，内核的 7 条真库用例靠本片记录的命令手工跑（§5）—— **不是静默跳过**：
`lease_db.rs` 的测试体在拿不到 `MULTICA_TEST_DATABASE_URL` 时 `.expect("set MULTICA_TEST_DATABASE_URL …")`
直接 panic，不会当绿（这是本仓真库测试的统一口径）。

### 7.3 对下游片（M5-8 / M6 / M9）的接口承诺

* `Manager::register(&mut self, JobSpec) -> SchedulerResult<()>`：查重 + 校验 + 冻结；重复名报错。
* `HandlerInput { job, scope, plan_time, attempt, runner_id, heartbeat }`：`plan_time` 已取整到 cadence 桶；
  `heartbeat` 是给长跑 handler 的手动续期句柄（自动心跳不受影响）。
* `JobSpec::new(name, cadence, scopes, handler)` —— `scopes` / `handler` 是**必填参数**，
  时间预算故意留零值 ⇒ 漏填会被 `validate` 拒掉。作用域用
  `global_scopes()` / `static_scopes(vec![…])`，也可以自己给 `ScopeProvider`
  （`Fn(now) -> Vec<Scope>`，每 tick 现算）。
* builder 全集：`with_timing(run, stale, hb)`、`with_retry(max_attempts, backoff)`、
  `with_catch_up(mode, window, max_plans_per_tick)`、`with_schedule_delay`、
  `with_allow_stale_reentry`、`with_plans_for_scope(hook)`（后者一旦设置就**替代** cadence 网格，
  是 autopilot 任意 cron 形态的入口）。
* M5-8 只需在 `jobs/mod.rs::register_all` 里加两行 `manager.register(crate::jobs::…::job()?)?;`。

### 7.4 本片**没有**覆盖的（诚实清单）

1. **没有跑起来的服务**：`main.rs` 未接线（§7.1）⇒ 本片没有任何「进程级」证据，全部证据止于内核 + 真库。
2. **没有跨进程并发**：并发 claim 的两方是两个 `Manager`（同一进程、同一连接池）。跨进程同步靠的是
   DB 唯一键与 `RowsAffected`，理论上与进程数无关，但**本片未在真两进程下重复验过**。
3. **没有时钟回拨 / 长暂停（`SIGSTOP`）用例**：陈旧判据在 DB 侧算，回拨只影响本地 `Utc::now()` 的退路
   （仅在 `db_now` 失败时用到）。
4. **没有指标上报**：`mark_stale_as_failed` / `Conflicted` 的次数只进日志，未进 metrics（`docs/44` §6.2 未要求）。
5. **没有改 ⑦ 基线**：0 路由片，读数应保持 300/290/243/213（§6 已核）。

## 8. 已知语义：`catch_up_window <= 0` 会吞掉「还能重试的 FAILED 桶」（`LUM-1980` 登记）

§3 规则 1 与 `catch_up_window` 的**优先级**：`every_plan_plans`（`crates/mc-scheduler/src/manager.rs`）
先按规则 1 取 `start = info.plan_time`（最新行 `FAILED` 且还能重试 ⇒ **停在同一个 `plan_time`**，
否则那个 FAILED 桶会被**永久跳过**），**随后**被窗口夹一次：

```rust
let oldest_allowed = if job.catch_up_window <= Duration::zero() { latest } else { now - job.catch_up_window };
...
if start < oldest_allowed { start = floor_plan(oldest_allowed, job.cadence); ... }
```

⇒ `catch_up_window <= 0` 时 `oldest_allowed = latest`（**本 tick 的桶**）：一旦两次 tick 之间**跨过桶边界**，
规则 1 的 `plan_time` 被换成**新桶** ⇒ 那个 FAILED 桶**再也不会被重试**。

* **实现选择的是「窗口赢」，不是规则 1** —— 与 §3 规则 1 的自述**冲突**，故登记在本节。
* **生产不可达**：三个 job 全是 `CatchUpMode::LatestOnly`（`jobs/plugin_hook.rs` / `jobs/issue_wakeup.rs` /
  `jobs/autopilot.rs`），其中两个还带 `PlansHook`（`catch_up_mode` 只当审计）
  ⇒ `EveryPlan + window <= 0` 这个组合**只存在于测试里**，不是产品缺陷、不拦合并。
* 该组合下的行为是**时钟依赖的确定红**（窗口 = 桶边界，约 0.1–1%/次），不是「概率抖动」。
* **测试侧已修**：`crates/mc-scheduler/tests/lease_db.rs` 的
  `every_plan_returns_to_the_failed_bucket_when_its_backoff_has_burned` 改用
  `Duration::minutes(5)`（正窗口 ⇒ 夹取分支对该用例**结构性死掉**，规则 1 被单独钉住）。
  确定性 A/B 取证（base 配置 + 强制跨桶边界 ⇒ 红；正窗口 + 同一强制 ⇒ 绿）见 `docs/32` §36.9。
* 若将来要把它变成**产品语义**（规则 1 压过窗口），需 M5 spec 仲裁；`LUM-1980` 不动产品代码。
