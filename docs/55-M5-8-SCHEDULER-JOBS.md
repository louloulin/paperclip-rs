# 55 · M5-8 调度 jobs（`autopilot_schedule_dispatch` / `issue_wakeup_dispatch`）（LUM-1571）

D 波第二片：把上游 `scheduler/` 的两个 job 搬进 `mc-scheduler`，并把注册表（`register_all`）
填成「恰好两行 `register`」。

- **记录号**：本片用 **`55`**（`docs/37` §44.6 的权威预约：`52`=M5-4 / `53`=M4-4-fu `LUM-1600` /
  `54`=M5-5 / **`55`=M5-8**；下一个空号 `56` 归 M5-INT `LUM-1572`）。
- **上游基准**：`louloulin/multica` @ `f41fae6b08fb`（与 `scripts/route_parity.py` 内嵌同一 commit）。
  `server/internal/scheduler/jobs_autopilot.go`448（449 行）、`jobs_issue_wakeup.go`21（22 行）、
  函数体在 `server/internal/service/issue_wakeup.go` 的 `Tick`（513–546）。
- **基线**：`origin/feat/multica-rs-initial` @ `84ef946`（本分支已把 docs-only 的 `e796c5d` 合进来，§5.2）。
- **写集**：`crates/mc-scheduler/src/jobs/**` + `crates/mc-scheduler/tests/{common/,jobs_autopilot.rs,jobs_issue_wakeup.rs}`。
  与同波 M5-5（`mc-repos/src/autopilot/ingress.rs`）**零交集**。
- **不碰**：`apps/mc-server/**`（P0，§3）、`Cargo.toml` / `Cargo.lock`、`mc-scheduler` 的租约内核
  （`spec.rs` / `manager.rs` / `db_ops.rs` / `error.rs`）、`mc-repos` / `mc-autopilot` 的任何文件、
  任何 `routes/**` / `mount.rs` / `lib.rs`。

## 0. 交付物

| 文件 | 行数（增量） | 内容 |
| --- | --- | --- |
| `crates/mc-scheduler/src/jobs/autopilot.rs` | 630（+629 −19） | autopilot 调度 job：常量 6 / `AutopilotSchedulePort`(5) / `ScheduleDispatch`（+ `AutopilotDispatcher` 真实现）/ `ScheduleRun` / `TriggerConfig` / `ScheduleCache` / `plan_scopes` / `catalog_scopes` / `plans_hook` + `plans_for_scope` / `is_autopilot_schedule_plan_stale` / `advanced_next_run` / `handle_scope` + `schedule_handler` / `job` |
| `crates/mc-scheduler/src/jobs/issue_wakeup.rs` | 285（+293 −8） | issue wakeup 派发 job：`WakeupDispatchPort`(4) / `WakeupOutcome`(4) / `WakeupTickSummary` + `to_json` / `run_tick(_with_budgets)` / `wakeup_handler` / `job` |
| `crates/mc-scheduler/src/jobs/mod.rs` | 204（+193 −1） | 模块契约；`PortFuture`；`JobPorts`；**`register_all`（两行）**；`JsonObject` / `push_json_escaped` / `json_object` / `one_line` |
| `crates/mc-scheduler/tests/common/mod.rs` | 448（+448） | 共享夹具与桩端口（`trigger_row` / `autopilot_row` / `wakeup_row` / `snapshot` / `plans_now` / `StubCatalog` / `StubDispatch` / `StubWakeup` / `OutcomeWakeup` / `repo`…）。**不是** test target（只是 `mod common;`），头上有 `#![allow(dead_code, unused_imports)]` |
| `crates/mc-scheduler/tests/jobs_autopilot.rs` | 674（+674） | 17 条纯逻辑用例 + 2 条真库用例（`#[ignore]`） |
| `crates/mc-scheduler/tests/jobs_issue_wakeup.rs` | 352（+352） | 7 条纯逻辑用例 + 1 条真库用例（`#[ignore]`，合并了「两行注册 + 起停 + tick」） |

**`apps/mc-server/src/main.rs` 一行未动**：`docs/48` §7.1 的 P0（缺依赖边）本轮复核仍开（§3）。

## 1. job 一：`autopilot_schedule_dispatch`

### 1.1 三个作用面（与上游一一对应）

| 上游 | 本地 | 语义 |
| --- | --- | --- |
| `autopilotScopes`66 | `catalog_scopes` | 每 tick **重列**「可调度的 schedule trigger」，**一 trigger 一 scope**（`scope_kind="autopilot_trigger"`、`scope_id=trigger.id`），并整体替换本 tick 的配置快照 |
| `autopilotPlansForScope`84 | `plans_hook` + `plans_for_scope`（纯函数） | 用 trigger 自己的 cron + 时区算 `plan_time`，只留最近一格 |
| `autopilotHandler` | `schedule_handler` / `handle_scope` | tick 中**重读** trigger + autopilot，停用/暂停立刻生效 |

计划钩子的五步（顺序即语义）：

1. 快照里没有这条 trigger ⇒ 空（在「列 scope」与「算计划」之间被删 ⇒ 静默 no-op）；
2. **重试面优先**：最新审计行是 `FAILED` 且 `retry_eligible(now)` ⇒ **原样回吐那个 `plan_time`**
   （否则半开区间 `(latest.plan_time, now]` 会跳过失败桶，那次触发永久丢失 —— 上游 `#4444` 的教训）；
3. **锚点三选一**：有历史 ⇒ 最新 `plan_time`；否则 `last_fired_at`（防迁移后重放）；否则 `created_at`
   （别补到 trigger 出生之前）；
4. 锚点被 `REPLAY_WINDOW_HOURS=24h` 夹住；
5. `next_occurrences_between_utc(cron, tz, after, now)`（半开 `(after, now]`，升序）取**最后一格**，
   再过迟到闸 `is_autopilot_schedule_plan_stale`（`now - plan_time > MAX_LATENESS_MINUTES=5`）。

handler 的五个分支（前三条写 **SUCCESS + `skipped_reason`** 审计行，**不是**失败）：

| 分支 | 审计 `result_json` | 说明 |
| --- | --- | --- |
| trigger 查不到 | `{"skipped_reason":"trigger_not_found"}` | 为一条已消失的租约写 SUCCESS 是对的 |
| `enabled=false` 或 `kind≠"schedule"` | `{"skipped_reason":"trigger_disabled"}` | 停用立刻生效 |
| autopilot 查不到 | `{"skipped_reason":"autopilot_not_found"}` | |
| `autopilot.status≠"active"` | `{"skipped_reason":"autopilot_inactive","status":…}` | 暂停立刻生效 |
| 派发 | `{"run_id":…,"run_status":…}`，`rows_affected=1` | `skipped` 的 run 也算已派发（上游同） |

派发后推进**展示列**：`advanced_next_run` ⇒ `advance_next_run(trigger, next_run_at)`；
cron/时区解析失败 ⇒ 退化成 `touch_fired_at`。两条写失败都只 `warn!`，**不让 handler 失败**
（权威记录是 `autopilot_run.created_at`，下次派发会重刷）。

`advanced_next_run` 的锚点是 `max(plan_time, now)`：本实例时钟慢于 DB（`plan_time` 由 DB 判）时，
不这样做会把刚跑完的那格又算一次，UI 的「下次触发」永远停在过去（上游 `MUL-3749`）。

### 1.2 job 规格（时间预算逐字对齐上游）

| 项 | 值 | 理由 |
| --- | --- | --- |
| `cadence` | **0** | 钩子驱动（`plans_for_scope` 一设就替代 cadence 网格），cron 表达式是任意的 |
| `catch_up_mode` / `catch_up_window` | `LatestOnly` / 24h | 有钩子时只当审计口径；写 `LatestOnly` 与钩子的折叠一致 |
| `max_plans_per_tick` | 5 | `LatestOnly` 用不到；只在有人换成 `every_plan` 时才起作用 |
| `run` / `stale` / `hb` | 120s / 300s / 30s | 上游 `RunTimeout/StaleTimeout/HeartbeatInterval` |
| `max_attempts` + 退避 | 3 + `1m/5m/15m` | 上游 `RetryBackoff` |
| `allow_stale_reentry` | `true` | 上游同 |

### 1.3 双层幂等（本片不发明新机制）

* **层一（进程级）**：`(job_name, scope)` 的 `sys_cron_executions` 唯一键 —— 「同一 trigger 同一
  `plan_time` 两实例不双跑」由 `db_ops::try_claim` 的 `Conflicted` 保证；
* **层二（业务级）**：M5-4 `dispatch_for_plan` 的幂等键（`schedule:{trigger}:{plannedAt}` +
  `uq_autopilot_run_trigger_planned`）—— 「同一 `(trigger, planned_at)` 不产生第二条 run」。

两层叠起来：陈旧租约被偷 + 重入也只会复用同一条 run。真库用例 `real_db_*` 逐条验了这两层。

## 2. job 二：`issue_wakeup_dispatch`

### 2.1 规格（上游 `jobs_issue_wakeup.go` 的逐字段翻译）

| 项 | 值 |
| --- | --- |
| `name` | `issue_wakeup_dispatch` |
| `cadence` | 30s |
| `catch_up` | `LatestOnly` + 1h 窗口 + `max_plans_per_tick=1` |
| `run` / `stale` / `hb` | 45s / 60s / 10s |
| `max_attempts` | **1**（不重试：一条坏规则不该被反复撞） |
| `allow_stale_reentry` | `true` |
| `scopes` | `StaticScopes(ScopeGlobal)`（本地 `global_scopes()`） |
| 钩子 | **无** —— 走内核的 `floor_plan(now - schedule_delay, cadence)` 网格（上游同，30s 网格） |

### 2.2 一轮 tick 与上游 `Tick` 的逐行对应

| 上游 `Tick` | 本地 |
| --- | --- |
| `DeleteExpiredWakeupReceipts`（2s 预算，错误只收不抛） | `WakeupDispatchPort::tick_candidates`（M5-6 在同一函数里先清 7 天前的收据再列 `ready_wakeups`） |
| `ListReadyWakeups` 失败 ⇒ **整 tick 立刻返回** | 同一个调用 `?` 直接冒泡（`SchedulerError`） |
| 逐行 `ctx.Err()` 检查 | 外层 `run_timeout=45s` 由内核 timeout 掉整个 handler（偏差 D5） |
| `s.dispatch(ctx, w)`（**含** 2s 单规则预算） | `dispatch_wakeup` + `PER_RULE_BUDGET=2s` |
| 失败 ⇒ `NoteWakeupFailure`（100ms 预算，**错误忽略**） | `note_dispatch_failure` + `OUTCOME_BUDGET=100ms`，错误**收进**聚合而不是丢掉 |
| 无论成败 ⇒ `TouchWakeupDispatch`（100ms 预算，错误收集） | `touch_dispatch` + `OUTCOME_BUDGET` |
| `errors.Join(errs...)` | `Err(SchedulerError::Handler(join))` ⇒ 写一行 `FAILED` 审计（`max_attempts=1` ⇒ 不重试，同上游） |

一条规则失败**不**阻断后续行：一批里一条坏规则不拖住其它规则，而每行的权威状态
（收据 / `last_error` / 调度推进）都落在 wakeup 侧的表里。计数汇总
（`candidates/dispatched/waiting/settled/removed/errors`）写进 `result_json`（**加法**，偏差 D2）。

## 3. 端口与接线（P0 仍开 ⇒ 本片只交付「可接线的 body」）

### 3.1 为什么 `job()` 必须带参数（这是**硬约束**，不是设计偏好）

上游两个 handler 直接拿 `*db.Queries` 打 SQL（列 trigger / 重读行 / 推展示列 / 取 ready wakeup /
建队列行）。本地三条实测把 SQL 挡在 `jobs/**` 外：

1. `mc-scheduler` 的依赖表**没有 `sqlx`**（M5-0 冻结，本片不得加）⇒ 连 `PgPool` / `PgConnection`
   这两个类型名都写不出来；`grep -rn "pub use sqlx" crates/` = 空，`mc-repos/src/lib.rs` 也没有
   重导出 ⇒ 没有旁路；
2. `SchedulerRepo`（本 crate 唯一能命名的库句柄）**没有 `pool()` / `db()` 访问器** ——
   只有 `new` / `connect` + 6 个租约方法 ⇒ 借不到 pool 去调 M5-6 的 `tick_candidates(&PgPool)`；
3. `mc-repos` 里**没有** `ListSchedulableAutopilotTriggers` / `AdvanceTriggerNextRun` /
   `TouchAutopilotTriggerFiredAt` 的对应函数（M5-1..M5-4 只交付了单机读面与 `update_trigger` 的
   通用补丁），而 `mc-repos` 不在本片写集内。

⇒ 数据面按上游本来就有单测接口这件事（`AutopilotScheduleDispatcher`）推广成两个**窄端口**：
`AutopilotSchedulePort`(5 方法) 与 `WakeupDispatchPort`(4 方法)，语义全部落在本片文件里，
实现由接线方注入（偏差 **D1**）。**生产实现目前缺位**，清单见 §3.4。

### 3.2 `register_all` 的签名（与 `docs/48` §7.3 承诺的差异）

`docs/48` §7.3 承诺「M5-8 只需加两行 `manager.register(job()?)?`」。**两行仍是两行**，
但签名多了端口包这一参：

```rust
pub fn register_all(manager: &mut Manager, ports: &JobPorts) -> SchedulerResult<()> {
    manager.register(autopilot::job(
        ports.autopilot_catalog.clone(),
        ports.autopilot_dispatch.clone(),
    ))?;
    manager.register(issue_wakeup::job(ports.wakeup.clone()))?;
    Ok(())
}
```

理由就是 §3.1：端口实例必须从外面造（`main.rs` 才有能力造），所以它必须作为参数进来。
`job()` 也相应返回 `JobSpec`（**不是** `Result`，偏差 D4）：上游也不返回错误，规格合法性统一由
`Manager::register` 的 `validate` 把关。

### 3.3 ready-to-apply 接线（**修正版**，替代 `docs/48` §7.1 的代码段）

前置（P0，需 owner）：`apps/mc-server/Cargo.toml` 加 `mc-scheduler` 边（`docs/48` §7.1 的
`mc-scheduler = { path = "../../crates/mc-scheduler" }`），并**同提交**更新 `Cargo.lock`；
若生产端口实现落在 `apps/mc-server`（推荐，见 §3.4），还需要 `mc-repos` / `mc-autopilot` 两条边。

```rust
    // 紧接现有「6. 装配 axum 路由」之后、`axum::serve` 之前：
    // 注册必须在 `spawn` 之前（`Manager::spawn` 消费 `self`，注册表在 spawn 时冻结）。
    let scheduler_ports = mc_scheduler::jobs::JobPorts::new(
        Arc::new(McAutopilotSchedulePort::new(db.clone())),   // §3.4 缺口 1
        Arc::new(AutopilotDispatcher::new(pool.clone())),      // 已可用：M5-4 的真实现
        Arc::new(McWakeupDispatchPort::new(db.clone(), realtime.clone())), // §3.4 缺口 2
    );
    let mut scheduler = mc_scheduler::Manager::new(
        mc_scheduler::SchedulerRepo::new(db.clone()),
        mc_scheduler::Options::default()
            .with_runner_id(format!("mc-server-{}", std::process::id())),
    );
    mc_scheduler::jobs::register_all(&mut scheduler, &scheduler_ports).context("register scheduler jobs")?;
    let scheduler_handle = scheduler.spawn();
```

```rust
    // 现有 graceful shutdown 处（`axum::serve(…).with_graceful_shutdown(…).await?` 之后）：
    scheduler_handle.shutdown().await;
    actors.shutdown().context("shutdown actors")?;
```

### 3.4 接线片必须补的两块（本片**没有**交付，如实登记）

1. **`AutopilotSchedulePort` 的生产实现** —— 5 条 SQL（trait 文档逐条列了口径，含
   `JOIN autopilot` 的过滤条件）；落地位置二选一：`apps/mc-server`（加 `mc-repos` 边）或
   `mc-scheduler`（加 `sqlx`，走 `docs/15` §8.4 仲裁）。
2. **`WakeupDispatchPort` 的生产实现** —— 7 步事务顺序写在 trait 文档上（凭据 overlay ⇒
   `BEGIN` + `lock_timeout` ⇒ `plan_dispatch` ⇒ 建队列行 / 合并证据 ⇒ `consume_dispatch` ⇒
   `COMMIT` ⇒ 提交后广播 `task.queued`）。第 7 步要 realtime 出口（`mc-ws`），所以落地位置
   只能是 `apps/mc-server`。

> 也就是说：**「上游两个 job 的业务语义 + 循环 + 预算 + 租约接线」本片交付完毕，
> 「最后 5 条 SQL + 7 步事务」留给接线片**。这不是遗漏，是依赖表与写集共同决定的分工。

## 4. 与上游/计划的偏差（逐条可查）

| # | 偏差 | 理由 |
| --- | --- | --- |
| **D1** | 数据面抽成 `AutopilotSchedulePort` / `WakeupDispatchPort` 两个注入端口，`job()` 与 `register_all` 因此带参 | §3.1 的三条硬约束（无 `sqlx` / 无 pool 访问器 / `mc-repos` 无对应查询）。**派发面不在此列**：`ScheduleDispatch` 有对 `AutopilotDispatcher` 的真实现，编译期受检 |
| **D2** | 两个 job 都用 `result_json` 回传小结（autopilot：`run_id`/`run_status` 或 `skipped_reason`；wakeup：6 个计数）；wakeup 的 `rows_affected = dispatched` | 上游 `Tick` / `autopilotHandler` 只回 error，而本地审计行要求 `result_json` 是合法 JSON。wakeup 的 `dispatched` 才是「真的写了队列行」的计数（`Settled/Waiting/Removed` 不是） |
| **D3** | cron / 时区解析失败 ⇒ `SchedulerError::Permanent{code:"invalid_cron"}`（上游是可重试） | 表达式存错不会自己变好：按 `MaxAttempts=3` 白烧 3 轮往返 + 在租约表留一串 `FAILED` 噪音。有意的**加法** |
| **D4** | `job()` 返回 `JobSpec`，不返回 `Result` | 上游同；合法性交给 `Manager::register` 的 `validate`（漏填时间预算会当场被拒） |
| **D5** | 批次预算靠内核 `run_timeout=45s` + 每规则 `PER_RULE_BUDGET=2s`，**没有**逐行 `ctx.Err()` 检查 | 上游的逐行检查是 Go context 的写法；本地对应物是「整个 handler 被 timeout 掉」，而 `Settled/Skipped` 的语义在每行自己的收尾里已经落库。**行为等价**：45s 到点后剩下的行不再处理 |
| **D6** | `run_tick_with_budgets(port, per_rule, outcome)` 把两个预算做成参数（`run_tick` 用真常量委托） | 否则「2s 单规则预算」这条 DoD 只能靠真等 2s 来验（慢且不稳）。生产路径仍是常量 |
| **D7** | autopilot handler 的逻辑体开成 `pub async fn handle_scope(catalog: &dyn …, dispatch: &dyn …, scope_id, plan_time)`，`schedule_handler` 只是 3 行适配器 | `HandlerInput` 里挂着 `Heartbeat`，而它只能由真库 `SchedulerRepo` 构造 ⇒ 若只暴露 `Handler`，「停用 / 暂停 / cron 坏掉」这三条分支就只能靠真库用例验（慢，且不在门禁 ⑤ 里）。`&dyn` 而非 `&Arc<dyn …>`：测试里 `Arc<Stub…>` 的强制转换更干净 |
| **D8** | 单文件测试拆成 `tests/common/mod.rs` + `jobs_autopilot.rs` + `jobs_issue_wakeup.rs` | 门 ⑩ 的单文件 800 行硬限（合并版 1386 行）。`common/` 不是 test target，夹具与桩端口只写一遍。先例：M5-4 同因拆分 |

## 5. 测试与门禁

### 5.1 布局与覆盖矩阵

| 用例 | 覆盖 |
| --- | --- |
| `jobs_autopilot.rs`（17 纯 + 2 真库） | **DoD ①** 两个不同时区的 trigger ⇒ 各自 `plan_time` 正确对齐（上海 `0 17 * * *` / UTC 各自 17:00）；**DoD ②** 过期 plan 不再派发（`is_autopilot_schedule_plan_stale` + 钩子不吐格）；**DoD ③** `advanced_next_run` 的下一格（含「时钟慢于 DB」的 `plan_time` 下限）；scope 提供者 / 快照一致性 / 锚点三选一 / 重试面原样回吐 / handler 五分支 / 规格逐字段 / `JsonObject` 转义 |
| `jobs_issue_wakeup.rs`（7 纯 + 1 真库） | 逐行派发顺序与计数（`Settled/Removed/Waiting/Dispatched`）/ 一条坏规则不阻断后续 / 失败 ⇒ `note_dispatch_failure` 且**仍** `touch_dispatch` / 错误聚合进 `Handler` / 规格逐字段 / **DoD ④** 真库：两行 `register_all` 装上两个 job、后台循环起停、一次 tick ⇒ 每条候选一次、同一 SUCCESS 桶换实例再抢 ⇒ `Conflicted` |
| `common/mod.rs` | 共享夹具（`trigger_row` / `autopilot_row` / `wakeup_row` / `snapshot` / `plans_now` / `repo` / `fresh_trigger`）+ 桩端口（`StubCatalog` / `StubDispatch` / `StubWakeup` / `OutcomeWakeup`） |

真库用例一律 `#[ignore]`，按本仓统一口径：拿不到 `MULTICA_TEST_DATABASE_URL` 就 panic，不会静默跳过。

### 5.2 门禁证据（本片 HEAD，base 已真合 `e796c5d`）

```console
# 真库（566 迁移，本地 multica_lum1571）
$ bash scripts/gates.sh --with-db
① fmt PASS  ② build PASS  ③ clippy PASS  ④ clippy-test-util PASS  ⑤ test PASS
⑥ db PASS (migrate=0,e2e=0)  ⑧ schema-drift PASS  ⑦ route-parity PASS
⑨ conformance PASS  ⑩ file-size PASS
overall: PASS — 10/10 gate(s) green in 77s
```

| 门 | 本轮读数 |
| --- | --- |
| ⑤ test | 69 target / **1354 passed** / 0 failed（含本片 38 条纯逻辑用例：autopilot 17 + wakeup 7 + 内核 lease 14；`cargo test --workspace` 的 `Doc-tests` 行不计入 target 数） |
| ⑥ db | `migrate=0,e2e=0`（**不收集 `mc-scheduler`**，`docs/48` §7.2）⇒ 本片的 10 条真库用例手工跑，见下 |
| ⑦ route-parity | `local 328 registered / implemented 262 real + 2 ph = 264/456 / known_gap 192 / unclaimed 0 / regression 0 / local_only 11` —— 本片 0 路由（未碰任何 `routes/**`） |
| ⑨ conformance | `pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306`（与 `docs/49` 逐字一致） |
| ⑩ file-size | `scanned=523 baseline=10 violations=0`（本片最长文件 `jobs_autopilot.rs` 630 行；`tests/common/mod.rs` 448） |

**真库原始输出**（`MULTICA_TEST_DATABASE_URL=postgres://…@localhost:5432/multica_lum1571`，
先 `DATABASE_URL=… cargo run -q -p mc-migrate -- run --dir migrations` ⇒ `applied 566 migration(s)`）：

```console
$ cargo test -p mc-scheduler -- --ignored
     Running tests/jobs_autopilot.rs
running 2 tests
test real_db_autopilot_dispatches_one_run_for_the_current_bucket ... ok
test real_db_two_managers_never_dispatch_the_same_bucket_twice ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 17 filtered out; finished in 0.11s
     Running tests/jobs_issue_wakeup.rs
running 1 test
test real_db_register_all_wires_both_jobs_and_the_loop_starts_and_stops ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.14s
     Running tests/lease_db.rs
running 7 tests
test failure_schedules_a_backoff_retry_that_blocks_early_reclaim ... ok
test manager_runs_a_claimed_job_exactly_once ... ok
test every_plan_returns_to_the_failed_bucket_when_its_backoff_has_burned ... ok
test permanent_failure_burns_the_retry_budget ... ok
test heartbeat_renewal_pushes_the_stale_window_forward ... ok
test shutdown_stops_the_loop_and_aborts_the_running_handler ... ok
test stale_lease_is_stolen_and_the_old_holder_gets_lease_lost ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.39s
```

**DoD ④ 的替代证据（P0 的直接后果）**：`apps/mc-server/src/main.rs` 加不上那两行（§3.1），
所以「服务起来/停掉」这条 DoD 用**等价物**兑现：真 `Manager` + `register_all` + `spawn()` + 真实
后台循环 + `shutdown()`，并断言登记表逐字等于 `["autopilot_schedule_dispatch", "issue_wakeup_dispatch"]`、
同名二次注册被拒、两个 job 都跑出 `SUCCESS` 审计行、`shutdown()` 在 5s 内返回。
**进程级证据仍然缺位**（没有真起 `mc-server`），如实登记在 §6。

## 6. 诚实清单（本片**没有**覆盖）

1. **没有跑起来的服务**：`main.rs` 未接线（P0）⇒ 没有进程级证据；证据止于 `mc-scheduler` + 真库。
2. **两个生产端口缺位**（§3.4）⇒ 真库用例走的是**桩端口**：租约、桶对齐、审计、起停是真的，
   但「端口背后那 5 条 SQL / 7 步事务」未被本片的任何用例执行过。
3. **没有跨进程并发**：并发 claim 的两方是同一进程里的两个 `Manager`（同连接池）。跨进程同步靠
   DB 唯一键，理论上与进程数无关，但**未在真两进程下重复验过**（与 `docs/48` §7.4 同一条）。
4. **没有「真起服务 + 真 tick」的端到端**：`wakeup` 侧第 7 步（提交后广播 `task.queued`）在本片
   根本没有代码路径（属端口实现），所以「UI 能看到队列行」这件事本片无法证明。
5. **`tick_candidates` 的收据清理只有一条链路**：清理与取行绑在同一个 M5-6 调用里（偏差 D5 的
   同源事实），本片没有独立验证「7 天前的收据真的被清掉」。
6. **未改 ⑦ 基线**：0 路由片，`docs/47` §47.3 的缺口归属板读数不变（`M5=1` 仍是 M5-5 在飞的那条）。
