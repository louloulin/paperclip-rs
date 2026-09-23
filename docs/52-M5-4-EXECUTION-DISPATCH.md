# 52 · M5-4 自动驾驶执行 / 派发面（LUM-1569）

波次 C 第四片、也是 C 波最大一片：把上游 autopilot 的 **执行面**搬进 `mc-http`（手工触发 /
runs 读面 / 投递读面 / replay，共 6 条路由），并把它们背后的 **派发服务层**
（`service/autopilot.go` 的 dispatch 段 ≈2,600 行：建 issue + 派任务 / 只派任务 / 终态回写 /
准入闸 / 额度 / 跳过 / 分析 / 模板与工具）落成 `mc-autopilot::dispatch` 与 `mc-repos` 的 W 面 SQL。

- **记录号**：本片用 **`52`**（`docs/37` §44.6 的权威预约：`52`=M5-4 / `53`=M4-4-fu `LUM-1600` /
  `54`=M5-5 / `55`=M5-8）。M5-3 用了 `51` ⇒ 与本片相邻的文件连续。
- **上游基准**：`louloulin/multica` @ `f41fae6b`（与 `scripts/route_parity.py` 内嵌同一 commit）。
  上游 handler 全文在 `server/internal/handler/autopilot.go` 与 `.../webhook_delivery.go`，
  服务层在 `server/internal/service/autopilot.go`。
- **基线**：`origin/feat/multica-rs-initial` @ `bf1997b`（本分支已把它合进来，见 §6.2）。

## 0. 交付物

| 文件 | 行数（本片增量） | 内容 |
| --- | --- | --- |
| `crates/mc-autopilot/src/dispatch/mod.rs` | 748（+729） | 事务边界与编排：常量 / `ReasonCode`(17) / `DispatchRequest` / `DispatchOutcome` / `DispatchError` / `SideEffectError` / `AutopilotDispatcher`（含 `dispatch_for_plan` 的三段快路径）/ `is_run_complete` |
| `crates/mc-autopilot/src/dispatch/admission.rs` | 409（+409） | 准入闸 `should_skip_dispatch`（上游三结局）、`resolve_leader`、`format_admission_reason`、**额度化建 run** `create_run_with_quota`、`require_bound_runtime` |
| `crates/mc-autopilot/src/dispatch/attribution.rs` | 304（+304） | 归属解析 `resolve_run_attribution`（`direct_human` → `trigger_owner` → `rule_owner` → `owner_fallback` → `unattributed`）+ `AttributionBlocked` |
| `crates/mc-autopilot/src/dispatch/template.rs` | 234（+234） | 计划时间插值 `interpolate_template` / `format_run_timestamp` / `format_run_date` / `build_issue_description` / 时区解析 |
| `crates/mc-autopilot/src/dispatch/create_issue.rs` | 398（+398/−9） | ① 建 issue + 派任务（单事务：编号 → 重复守卫 → issue → 订阅者扇出 → run 回链 → 消费预留 → task） |
| `crates/mc-autopilot/src/dispatch/run_only.rs` | 178（+177/−6） | ② 只派任务（无 issue，`autopilot_run_id` 直连） |
| `crates/mc-autopilot/src/dispatch/sync.rs` | 358（+355/−6） | ③ 终态回写 `sync_from_issue_status` / `sync_from_task` / `sync_from_linked_issue_task` + `fail_run` / `settle_with_quota` / `publish_run_done` |
| `crates/mc-autopilot/src/dispatch/skip.rs` | 126（+126/−10） | 跳过落账 `record_skipped_run`（幂等：冲突回读既有 run） |
| `crates/mc-autopilot/src/dispatch/analytics.rs` | 128（+122） | 派发侧埋点（`run_started` / `run_completed` / `run_failed` / `run_skipped` / `issue_created_from_autopilot` / `run_duration_ms`） |
| `crates/mc-http/src/routes/autopilots/execution.rs` | 430（+411/−9） | 3 号 handler（#15/#16/#17）+ `router()` + `run_to_response(_slim)` / `quota_exceeded_response` / `parse_limit_offset`（后三者 `pub(super)`） |
| `crates/mc-http/src/routes/autopilots/delivery.rs` | 418（+400/−11） | 3 号 handler（#18/#19/#20）+ `router()` + `WebhookDeliveryResponse`（两个忠实构造器） |
| `crates/mc-repos/src/autopilot/run.rs` | 713（+713/−10） | `autopilot_run` 18 列读写 + 建 issue/任务的邻表 SQL（替掉 10 行 stub） |
| `crates/mc-repos/src/autopilot/run/issue_sql.rs` | 201（+201） | issue 链：`lock_duplicate_key` / `find_recent_duplicate_issue` / `next_issue_number` / `next_top_position` / `insert_issue` / `insert_issue_subscribers` |
| `crates/mc-repos/src/autopilot/run/lookup_sql.rs` | 229（+229） | 派发读面：`load_issue_origin` / `effective_issue_status` / `load_agent` / `load_trigger_timezone` / `load_active_rule_version` |
| `crates/mc-repos/src/autopilot/delivery.rs` | 224（+224/−10） | 投递读写：`list`（23 列瘦行）/ `get_in_workspace`（28 列）/ `find_replay` / `create_replay` + `signature_failed()` |
| `crates/mc-http/tests/autopilots/execution.rs` | 661（新） | 14 例：#15/#16/#17 的形态、鉴权分层、判负顺序、`limit`/`offset` 语义、跨 autopilot 的 run |
| `crates/mc-http/tests/autopilots/deliveries.rs` | 583（新） | 7 例：#18/#19 读面投影 + 跨 autopilot 的 delivery + 写权分层（夹具 `pub(crate)`） |
| `crates/mc-http/tests/autopilots/deliveries_replay.rs` | 280（新） | 2 例：#20 五道判负阶梯 + 202 / 幂等命中 |
| `crates/mc-http/tests/autopilots/dispatch.rs` | 547（新） | 6 例：服务层三块（create_issue / run_only / sync）+ `agent_runtime_required` 提前判定 |
| `crates/mc-http/tests/autopilots/main.rs` | 45（+10） | 4 行 `mod`（`deliveries` / `deliveries_replay` / `dispatch` / `execution`） |
| `crates/mc-repos/src/autopilot/tests/run.rs` | 453（新） | 2 例：幂等槽位、半成品回收（夹具 `pub(super)`） |
| `crates/mc-repos/src/autopilot/tests/run_settle.rs` | 421（新） | 4 例：终态结算额度、issue 链、任务栅栏、读面收窄 |
| `crates/mc-repos/src/autopilot/tests/run_delivery.rs` | 153（新） | 1 例：replay 行的唯一槽位与去重旁路 |
| `crates/mc-repos/src/autopilot/tests/mod.rs` | +3 | 三行 `mod` |

合计 **24 文件 +8143 / −71**（`git diff --numstat origin/feat/multica-rs-initial`，含 3 个
新测试模块与 3 个门⑩ 拆分文件）。

**未触碰**（C 波共享热点与单写者规则）：`routes/autopilots/` 里 M5-1/M5-2/M5-3 的文件
（`mod.rs` / `dto.rs` / `list.rs` / `crud.rs` / `access.rs` / `assignee.rs` / `subscribers.rs` /
`trigger.rs` / `credentials.rs`）、`routes/mount.rs`、`routes/mod.rs`、任何 `lib.rs`、
`Cargo.toml` / `Cargo.lock`（**无新依赖**）、`mc-http/tests/autopilots/support.rs`（M5-1 所有）、
`mc-repos/src/autopilot/mod.rs`（M5-1 的模块表，M5-4 不能往里加 `pub mod`）。

## 1. 路由与注册键

| # | 上游 handler | 本地 handler | axum 注册键 | 成功码 |
| ---: | --- | --- | --- | --- |
| 15 | `TriggerAutopilot`（`autopilot.go:606`） | `execution::trigger_autopilot` | `POST /api/autopilots/:id/trigger` | `200` |
| 16 | `ListAutopilotRuns`（`autopilot.go:806`） | `execution::list_autopilot_runs` | `GET /api/autopilots/:id/runs` | `200` |
| 17 | `GetAutopilotRun`（`autopilot.go:866`） | `execution::get_autopilot_run` | `GET /api/autopilots/:id/runs/:runId` | `200` |
| 18 | `ListAutopilotDeliveries`（`webhook_delivery.go:161`） | `delivery::list_autopilot_deliveries` | `GET /api/autopilots/:id/deliveries` | `200` |
| 19 | `GetAutopilotDelivery`（`webhook_delivery.go:209`） | `delivery::get_autopilot_delivery` | `GET /api/autopilots/:id/deliveries/:deliveryId` | `200` |
| 20 | `ReplayAutopilotDelivery`（`webhook_delivery.go:225`） | `delivery::replay_autopilot_delivery` | `POST …/:deliveryId/replay` | `202` |

6 条上游路径 → **6 个注册键**：上游这 6 条都是**单形态** plain 子路由（没有 `Route("…") + Post("/")`
那种挂法）⇒ 本地也不注册尾斜杠别名，多注册一个就是 `EXTRA_ALIAS`
（`scripts/slash_alias_audit.py` 会红）。`execution.rs` / `delivery.rs` 的 e2e 各有一条用例
（`single_form_routes_reject_the_trailing_slash_alias` / `method_set_and_path_form_are_exact`）
把 `POST /trigger/`、`GET /runs/`、`GET /deliveries/`、`…/replay/` 钉成 **404**。

静态段与参数段共处一层无冲突：`/:id/runs` 与 `/:id/runs/:runId`、
`/:id/deliveries` 与 `/:id/deliveries/:deliveryId(/replay)` 同时注册能被 matchit 0.7 接受
（路径参数一律 `:id` / `:runId` / `:deliveryId`，写成 `{id}` 会被当字面量段）。

三份 `router()` 由 M5-0 的聚合壳（`routes/autopilots/mod.rs::mount_slice_autopilot`）挂进
`autopilots::router()`；⑦ 的读数因此从 `262 real` 再上一层（见 §6.2）。

## 2. 逐路由契约

### 2.1 `POST /api/autopilots/:id/trigger` → `200`

**判负顺序是契约**（上游把授权放在状态检查**之前**，理由逐字：未授权调用者不该能分辨
「未激活」与「已激活」）：

| 序 | 闸 | 判负 |
| ---: | --- | --- |
| 1 | `resolve_write_scope`（成员 → 加载 → 写权） | 400（非 UUID / 缺工作区头）· 404（不存在 / 跨工作区）· 403（无写权） |
| 2 | `autopilot.status != "active"` | 400 `autopilot is not active` |
| 3 | `Idempotency-Key` 长度（先 `trim`） | 400 `Idempotency-Key is too long`（>255） |
| 4 | 派发（同调度面那条链，计划时间 = 现在） | 429（额度）/ 500（其余） |

- `Idempotency-Key` 缺失 ⇒ 本地生成 `req-<uuid>`（上游 `service.NewRequestIdempotencyKey()`）。
- **成功一律 200**，即使这次派发被准入闸判成 `skipped`：响应体是 run 的 wire 形状，
  `status="skipped"` + `reason_code`（如 `target_unavailable`）。UI 按这两个字段弹 toast。
- **写权闸复用 M5-3 的 `trigger::resolve_write_scope`**（`requireAutopilotTriggerInvoker` 刻意
  **不**叠加 `requireAutopilotWrite`：判的是同一个人，叠加等于要两个不相干的人）。
- **500 固定文案** `failed to trigger autopilot`（上游 MUL-6472 的理由：任何成员都能走到这里，
  而 `pgx` 的错误链带约束名与内部 id）⇒ 真实错误只进 `tracing::error!`（`tracing::error!` 不是可选项：
  这是本面唯一的排障入口）。
- 无额度平面时 `DispatchError::QuotaExceeded` 不可达 ⇒ 429 的形状由 **lib 单测**钉住（§6.1）。

### 2.2 `GET /api/autopilots/:id/runs` → `200`

响应 `{"runs":[…],"total":N}`，`total` = **本页长度**（上游就是 `len(resp)`，不是全表计数）。

- `limit` 默认 **20**、上限 **100**；`offset` 默认 0。`parse_limit_offset` 照抄
  Go `strconv.Atoi` 的**失败语义**：解析失败 / `limit <= 0` / `offset < 0` ⇒ 用默认值，
  不报 400（`limit_and_offset_follow_upstream_atoi_semantics` 钉住）。
- 投影用 `runToResponseSlim`：**丢掉 `trigger_payload`**。webhook 信封可达 256 KiB、
  默认 `limit=20` ⇒ 列表最坏 ~5 MiB 全是随即被 JSON 编码器丢掉的字节。详情才发全量载荷。
- **读面不加写权闸**：任何工作区成员都能看到 runs（与 M5-1 的读面同口径）。

### 2.3 `GET /api/autopilots/:id/runs/:runId` → `200`

- run 的归属**经 autopilot 重查一遍**：`run_sql::get` 后必须 `run.autopilot_id == autopilot.id`，
  否则与「不存在」折成同一个 **404 `not found: run`**（上游注释逐字：ID 猜中也不能读到别人的 run）。
- 非 UUID 的 `:runId` ⇒ **400**（`parse_uuid(&run_id, "run id")`，在 autopilot 加载**之后**判）。
- 详情发全量：`trigger_payload` / `result` 是 `jsonb`，本地行结构已解成
  `Option<Value>`，NULL 保持 `null`。

### 2.4 `GET /api/autopilots/:id/deliveries` → `200`

响应 `{"deliveries":[…],"total":N}`（同样 `total = len`）。投影 = `slimDeliveryToResponse`：
**三个详情专属字段整个缺席**（不是 `null`）——`raw_body`（≤256 KiB/行）、`selected_headers`、
`response_body`。本地两个行结构已经把这个差别钉在 SQL 层：瘦行 23 列
（`WEBHOOK_DELIVERY_SLIM_COLUMNS`）/ 全行 28 列。

### 2.5 `GET /api/autopilots/:id/deliveries/:deliveryId` → `200`

- 共用 `load_delivery_for_autopilot`：**跨 autopilot 的 `deliveryId` 一律 404**
  （`delivery_from_another_autopilot_is_not_found` 用「同工作区的另一个 autopilot」证明
  404 不是靠工作区头得到的）。
- 详情取整行 ⇒ 三个专属字段回归，`WebhookDeliveryResponse::from_full(row, true)`。

### 2.6 `POST /api/autopilots/:id/deliveries/:deliveryId/replay` → `202`

写权闸 + `resolve_write_scope`（replay 产生一条新投递 = 一次新的「让 agent 干活」，
比读面高一格，上游 `requireAutopilotWrite`）。判负阶梯**逐字对照上游顺序**：

| 序 | 闸 | 判负 |
| ---: | --- | --- |
| 0 | 成员 / 工作区 / 加载 / 写权 | 400 · 404 · 403 |
| 1 | `signature_failed()` | 400 `cannot replay a delivery that failed signature verification` |
| 2 | `raw_body` 缺失或空 | 400 `original delivery has no raw body to replay` |
| 3 | `autopilot.status != "active"` | 400 `autopilot is not active` |
| 4 | trigger 存在 | 404 `not found: trigger` |
| 5 | `trigger.enabled` | 400 `trigger is disabled` |
| 6 | `raw_body` 仍是合法 JSON | 400 `stored body no longer parses: …` |
| 7 | `Idempotency-Key` 长度 | 400 `Idempotency-Key is too long`（>255；缺失 ⇒ `replay-<uuid>`） |

- **成功与幂等命中都是 202**（`Accepted`，不是 201）：上游把它当「请求已受理，durable worker
  负责真正的派发」。响应体是**详情形态**（上游两条出口都是 `deliveryToResponse(replay, true)`）。
- **幂等**：同一 `(原投递, Idempotency-Key)` 已有 replay ⇒ 回读那一行、**不建第二行**；
  并发下唯一索引抢先（`RepoError::Conflict`）⇒ 同样回读，语义与幂等命中相同。
- **原投递行不被改写**（`replay_is_202_and_idempotent_per_key` 在三次调用后复查原行字段）。
- replay 行**不带 `dedupe_key`**（上游刻意让重放绕开 provider 去重），初始 `status='queued'`。
- 500 固定文案 `failed to create replay delivery`（与 #15 同口径：约束名不透给成员）。

## 3. 派发服务层

### 3.1 三种结局

| 结局 | 触发 | 落账 |
| --- | --- | --- |
| **`skipped`** | `should_skip_dispatch` 判负（主体消失 / agent 归档 / squad 归档 / **agent 无 runtime**） | `record_skipped_run`：新建 run（`status='skipped'` + `reason_code`）、**不占额度**、发 `autopilot:run_done` |
| **新建 run + task** | 准入通过，走 `create_issue` 或 `run_only` 分支 | `status='issue_created'`（建 issue 线）/ `status='running'`（run_only 线）+ `agent_task_queue` 行 |
| **`reused`** | 幂等命中（同 `(trigger, planned_at)` / 同 `webhook_delivery` / 同 `quota_reservation`） | 回读既有 run，`DispatchOutcome.reused = true`，不新建 |

### 3.2 三个执行分支（`docs/44` §4.2 要求分开测）

1. **`create_issue.rs`**（上游 `dispatchCreateIssue` 681）：一个 tx 里依次做
   `next_issue_number` → 重复守卫（`lock_duplicate_key` + 60s 窗口）→ `insert_issue`
   （`origin_type='autopilot'` + `origin_id` **与** 本地 `origin='autopilot_run'` 双写）→
   订阅者扇出 → `update_issue_created` 回链 → 消费预留额度 → `create_task`。
   提交后做分析埋点 + `notifyAutopilotSubscribersOnCreate` + info 日志。
2. **`run_only.rs`**（上游 `dispatchRunOnly` 981）：不建 issue，任务直接挂
   `agent_task_queue.autopilot_run_id`（`issue_id = NULL`）⇒ 终态由 `sync_from_task` 收口。
3. **`sync.rs`**（上游 `SyncRunFromIssue` 1085 / `SyncRunFromTask` 1135 /
   `SyncRunFromLinkedIssueTask` 1195）：把 issue 状态或任务终态折算成 run 的终态。
   **所有终态转换都走额度化的 `update_terminal_with_quota`**：`completed` ⇒ `consume=true`，
   `failed` / `skipped` ⇒ `consume=false`（上游 `autopilot_quota.go:248` 的唯一收口点）。

三块在 `mc-http` 的 `dispatch.rs` 里各有真库用例（`create_issue_dispatch_links_the_issue_and_the_task` /
`run_only_dispatch_enqueues_a_task_without_an_issue` / `sync_from_task_settles_the_run_once` /
`sync_from_issue_status_settles_the_create_issue_run` / `sync_from_linked_issue_task_waits_for_active_tasks`）。

### 3.3 准入闸与它的两条已知缺口

`should_skip_dispatch` 交付为**上游真正存在的那三种结局**（本地没有
`concurrency_policy`，见 §5.1）。上游另有两道闸本波**不可达**：

- `AgentReadiness`（runtime 可用性探测，属 M6/M7）⇒ 简化为「leader 解析得出来 + agent 未归档 +
  squad 未归档」；**唯一例外**是 `agent.runtime_id IS NULL`：`agent_task_queue` 的 CHECK
  `agent_task_queue_active_requires_runtime`（`runtime_id IS NOT NULL OR completed_at IS NOT NULL`）
  让这条路径在本地**写不进去**，于是由 `admission::require_bound_runtime` 提前判成上游那条
  `agent_runtime_required`（`AgentReadiness` `agent_ready.go:143` 的真实分支）——
  即「上游会返回的错误码」与「本地 schema 强制的行为」在这一点上重合，遂照上游落地。
  `unbound_agent_is_skipped_before_the_task_insert` 钉住它是 **`Skipped` 而不是 500**。
- `autopilotAdmitInvoke`（squad 私有 leader 的调用授权）⇒ run_only 线按「准入已过即放行」处理。

### 3.4 额度与跳过

- **跳过不占额度**：`record_skipped_run` 落 `quota_reservation_id = NULL`。
- **取消/失败要还额度**：`settle_with_quota(consume=false)` → `release`；
  `recover_partial_run` 也释放（`recover_partial_run_frees_the_slot_and_releases_the_reservation`）。
- `get_reservation_by_key` 过滤 `state <> 'released'` ⇒ 释放后读回 `None`；
  测试因此同时 `SELECT state` 直接确认 `'released'`。

### 3.5 事件与分析

`autopilot:run_start` / `autopilot:run_done`（`EVENT_AUTOPILOT_RUN_*`）经
`mc_realtime::RealtimeHandle` 广播，载荷带 `autopilot_id` / `run_id` / `status` / `reason_code`；
埋点 6 条（`run_started` / `run_completed` / `run_failed` / `run_skipped` /
`issue_created_from_autopilot` / `run_duration_ms`）。

## 4. 权限链

| # | 成员闸 | autopilot 加载 | 写权闸 | 失败码 |
| ---: | --- | --- | --- | --- |
| 15 | ✔ | ✔ | ✔ `requireAutopilotTriggerInvoker` | 400 / 403 / 404 |
| 16 | ✔ | ✔ | ✘ | 400 / 404 |
| 17 | ✔ | ✔ | ✘ | 400 / 404 |
| 18 | ✔ | ✔ | ✘ | 400 / 404 |
| 19 | ✔ | ✔ | ✘ | 400 / 404 |
| 20 | ✔ | ✔ | ✔ `requireAutopilotWrite` | 400 / 403 / 404 |

- 403 的**判定口径不同**：#15 判「是不是创建者 / 工作区管理员 / 被授权协作人」
  （`access.rs::require_write` → `resolve_write_scope`），#20 判「有没有写权」。
  两条都落在本地同一句 `insufficient permission for this autopilot`（§5.12）。
- 缺 `x-multica-user-id` ⇒ **401**（AuthUser 提取器）；缺/非法 `x-workspace-id` ⇒ **400**；
  6 条路由各有 e2e 用例（`missing_user_header_is_401_on_every_execution_route` 等）。

## 5. 与上游的偏差（逐条都可查）

1. **没有 `concurrency_policy`**（迁移 `043` 已 DROP 该列）⇒ 上游 `shouldSkipDispatch` 里的
   并发策略分支不存在，本片交付的正是上游真正存在的那三种结局（§3.1）。
2. **`AgentReadiness` / `autopilotAdmitInvoke` 未实现**（runtime 就绪面与私有调用闸属 M6/M7）；
   唯一例外是 `agent.runtime_id IS NULL` ⇒ `agent_runtime_required`（§3.3，上游真实分支）。
3. **`create_issue` 的入队时机**：上游在 issue 提交**之后**由 issue 事件链
   `EnqueueTaskForIssue` 入队；本地没有那条监听链（daemon loop 属 M3-7）⇒ 在**同一 tx** 内建任务，
   但**保持上游的挂法**：只链 `issue_id`，`autopilot_run_id` 留 NULL（否则 `sync_from_task` 会替
   `sync_from_linked_issue_task` 抢收）。**run 与 task 不互链**（`run.task_id` 亦为 NULL）＝上游形态。
4. **归属判定提前到 tx 之前**：上游在入队时才解析归属，归属不可问责会留下一个没有任务的 issue；
   本地在 tx 前解析 ⇒ 拒绝即整体回滚（不产生孤儿 issue）。
5. **额度消费点只消费已有预留**：上游 tx 内 `settleAutopilotQuota(consume=true)`；
   本地同样在 tx 内，但 `quota_reservation_id` 为 NULL 时什么都不做（无额度平面时即此路径）。
6. **`run_only` 的归属多一级降级**：上游只用 `triggerOwnerAttribution`；本地两线共用
   `resolve_run_attribution` ⇒ 多出 `rule_owner` 一级。仍**绝不留 NULL source**
   （`unattributed` 只在 fail-open 工作区落），语义是上游的超集。
7. **不发 `NotifyTaskEnqueued` / `task:queued`**：本地没有 daemon 唤醒面 ⇒ `queued` 行由
   M3-7/M5-5 之后的集成面取走（**跨波缺口**，`docs/44` R9 的口径：登记，不在本波实现 daemon）。
   `SyncRunFrom*` 同理只交付「可调用的服务 API + 真库用例」，生产触发链归 M5-7/M5-8。
8. **replay 不重新归一化事件名**：上游从 `raw_body` 重推 `event`（`normalizeWebhookPayload` +
   事件推断），那对函数属 M5-5 的入站面 ⇒ 本地沿用**原投递的 `event`**，但保留
   「`raw_body` 必须仍是合法 JSON」的校验以维持上游的 400 契约。
9. **replay 不唤醒 worker**：上游最后 `WebhookDeliveryWorker.Notify()`；本地只把行写进 `queued`，
   等 M5-5 的投递 worker 下次轮询（`known_gap`，与 M4-4 对同类「唤醒 daemon」调用一致）。
10. **replay 行的 id 是 v4**（上游 `dbid.NewV7()`）：与 `mc-repos` 全部写面同口径。
11. **`autopilot_run` 上没有 `idempotency_key` 列**：幂等由三处唯一索引承担 ——
    `uq_autopilot_run_trigger_planned`（计划线）、`uq_autopilot_run_webhook_delivery`（webhook 线）、
    `autopilot_quota_reservation.idempotency_key`（手动带键线，**需要装额度平面**）。
    ⇒ 无额度平面时手工 `Idempotency-Key` 实际是 no-op（第二次同键调用会新建 run），
    这是本地与上游最容易被误读的一处（`run.rs` 的
    `create_run_rejects_a_second_run_in_the_same_idempotency_slot` 证明槽位语义本身是好的，
    缺的只是手动线的那张**唯一索引**）。
12. **403 四种上游码塌成一种**：`autopilot_trigger_forbidden` / `autopilot_trigger_no_originator` /
    `autopilot_forbidden` / `autopilot_no_originator` 在本地都是 `forbidden`
    （`mc_errors::Error` 没有自定义码变体，M5-2/M5-3 同口径）；正文带 `thiserror` 前缀
    `forbidden: …`（`docs/40` §5 的全仓偏差）。
13. **500 的两种折法（有意不对称）**：#15 与 #20 的两条**写路径**保留上游固定文案
    （不把约束名/内部 id 透给成员）；读面（#16–#19）走全仓 `repo_err`（`database_error`）。
14. **#15 的 429 不是标准错误体**：`{reason_code, used, reserved, limit, reset_at}` +
    `Retry-After`（秒，向上取整到 ≥1）逐字保留 —— 前端按 `reason_code == "quota_exceeded"` 分支，
    折成 `ApiError` 会破坏契约。
15. **`limit`/`offset` 用 Go `strconv.Atoi` 的失败语义**（非法值落默认、不报 400，
    §2.2）；`offset > i32::MAX` 之类的极端值落在 `parse::<i32>` 失败分支，同样落默认。
16. **`sync_from_task` 没有终态守卫**：上游 `SyncRunFromTask` 就是一条裸 UPDATE ⇒ 重复回调会
    覆盖既有终态，本地不额外加锁（`sync_from_task_settles_the_run_once` 断言的是「一次调用落一次
    终态」，不是「幂等」）。
17. **派发服务层的真库测试落 `mc-http` 的 test target**：`mc-autopilot` **没有** `tokio` 依赖
    （`docs/44` §5.2 的锚点禁止新增第三方依赖），而 `#[tokio::test]` 需要直接依赖 ⇒ 三块服务层
    用例挂在 `crates/mc-http/tests/autopilots/dispatch.rs`（该 target 本来就有 tokio + 真库脚手架）。
18. **`issue` 的 `origin_type='autopilot'` 之外的来源不加处理**：非 autopilot 来源的 issue
    （`origin_type` NULL / 其它）在 `effective_issue_status` 路径上按「无来源」处理，
    不会误收口成 autopilot run 的终态。
19. **门⑩ 的拆文件**（新增 3 个测试文件）：`deliveries.rs` 833→583（拆出写面 `deliveries_replay.rs` 280）、
    `mc-repos/tests/run.rs` 1007→453（拆出 `run_settle.rs` 421 / `run_delivery.rs` 153）。
    **新文件不能进 `scripts/file_size_baseline.tsv`** ⇒ 超过 800 行只能拆，不能钉基线（§6.1）。

## 6. 测试与门禁

### 6.1 测试布局

`crates/mc-http/tests/autopilots/` 追加 4 个文件（`main.rs` 四行 `mod`）。**为什么不是 1 个**：
门 ⑩ 单文件 800 行硬上限（合并写 2031 行），且 `support.rs` 是 M5-1 所有（不改）。
拆法是**按读面/写面**而不是按行数切：`deliveries.rs`（#18/#19 读面 + `pub(crate)` 夹具
`DeliverySpec` / `seed_delivery` / `unique_webhook_token`）∥ `deliveries_replay.rs`
（#20 判负阶梯 + 幂等）；`dispatch.rs` 自带宽化的种子（`seed_dispatchable_autopilot` /
`seed_agent` / `seed_unbound_agent`）。

本片新增 e2e **29 例**（该 target 合计 68 例）：`execution` 14、`deliveries` 7、
`deliveries_replay` 2、`dispatch` 6，全部 `#[ignore = "requires PostgreSQL (…)" ]`。
`mc-repos` 侧新增真库语义 **7 例**（`autopilot::tests::{run,run_settle,run_delivery}`，
该模块合计 15 例）。另有 `execution.rs` 的 4 条 lib 单测（429 形状 + `Retry-After` 下限）。

`429` 的形状为什么走单测而不是 e2e：装额度平面是**进程级先到先得**
（`mc_autopilot::quota::install_policy_provider` 的 `OnceLock`），而 `autopilots` 是**一个**
测试二进制 ⇒ 在这里装平面会把 M5-1 的 `usage.rs`（断言 `off`/`observe`/`enforce`）挤掉，
而 `usage.rs` 是 M5-1 的文件、不能改。

**一处踩过的坑（值得后来者照抄结论）**：`idx_autopilot_trigger_webhook_token` 是**全局**唯一索引，
而门 ⑥ 的 e2e **并发**跑（`cargo test … -- --ignored`，没有 `--test-threads=1`）——
6 条用例共用固定 `webhook_token='awt_e2e'` 会随机 23505。修法是每用例一个唯一 token
（`unique_webhook_token()`），而不是把 e2e 改成单线程。

### 6.2 门禁证据（本片 HEAD）

```bash
# 真库（566 迁移）
sudo -n -u postgres psql -c "CREATE ROLE mc_lum1569 LOGIN CREATEDB PASSWORD '…'"
sudo -n -u postgres psql -c "CREATE DATABASE multica_lum1569 OWNER mc_lum1569"
MULTICA_DATABASE_URL=… cargo run -q -p mc-migrate -- run --dir migrations   # applied 566

MULTICA_TEST_DATABASE_URL=… bash scripts/gates.sh --with-db --db-url …
#   → 10/10 PASS，**76s**（热 target；冷 build 的那次是 248s）：①fmt ②build ③clippy
#     ④clippy-test-util ⑤test ⑥db（migrate=0,e2e=0）⑧schema-drift ⑦route-parity
#     ⑨conformance ⑩file-size
```

- **⑤** `cargo test --workspace`（脚本用 `env -u` 剥掉库变量）：95 个 suite 合计
  **1323 passed / 0 failed**，本片的 29 条 e2e 在这里是 `ignored`。
- **⑥** 真库 target 合计 **311 passed / 0 failed**；其中 `autopilots` target
  **68 passed / 0 failed**（**并发**，见 §6.1 的坑）、`mc-repos` lib
  **108 passed / 0 failed**（含本片 7 例，84 条非 ignored 被 `--ignored` 过滤掉）。
- **⑦** `upstream 456 (commit f41fae6b08fb) | local 328 registered | baseline 300`；
  `implemented 262 real + 2 placeholder = 264/456`、`known_gap 192`、
  **`unclaimed 0` / `regression 0`** / `local_only 11`。`slash_alias_audit.py` 无新增缺陷
  （本片 6 条全单形态，故不新增任何别名键）。
- **⑩** 0 violation（拆分后的 6 个文件都在 800 行以内，且未动基线清单）。
- **分支** `agent/devbox5/262d8d1d79ef` 已把 `origin/feat/multica-rs-initial` @ `bf1997b`
  合进来（merge `8c3b7dc`）⇒ 上述门禁是在**合并树**上跑的，PR diff 只剩本片文件。

## 7. 交接

- **M5-5（webhook ingress）**：replay 写下的 `queued` 行需要 worker 取走（§5.9）；
  ingress 侧的事件归一化是它的事（§5.8），别再实现一遍。
- **M5-7 / M5-8**：`SyncRunFrom*` 只有服务 API（§5.7），生产触发链（任务终态回调 / issue 状态变化）
  与「唤醒 daemon 执行 `queued` 行」是它们的交付内容；M5-8 的调度 job 按 `planned_at` 分桶，
  本片的 `planned_at` 已随 run 写入。
- **M5-INT（`LUM-1572`）**：⑦ 的基线（`local 328 / baseline 300`）仍是一次性刷新项
  （`local_only 11` 里有本片新增的注册键）——按 `docs/37` 的口径归 M5-INT，不在本片动基线。
- **P0（仍待 owner）**：`apps/mc-server` 缺 `mc-scheduler` 依赖边（`docs/48` §7.1）。
  本片不受它阻塞（门 ⑥ 在本片全绿），但它仍挡着 M5-8。
- **记录号**：`52` 已用 ⇒ 下一片（M5-5）用 `54`（`53` 属 M4-4-fu，§44.6）。
