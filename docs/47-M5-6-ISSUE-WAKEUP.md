# M5-6：issue wakeup 用户面（LUM-1565）

把上游 issue-wakeup 用户面（8 条路由 + 服务层 + SQL）搬进本仓：`handler/issue_wakeup.go`(320) +
`handler/wakeup_actor.go`(63) + `service/issue_wakeup.go`(831) + `service/issue_wakeup_evidence.go`(135) +
`db/queries/wakeup.sql`(162 / 23 查询) + `db/queries/workspace_wakeup.sql`(70 / 1 查询) ≈ **1,581 行**。
其中 7 条 issue 子路由在 M5-0 之后是本仓的 **501 占位**（handler 名 `not_implemented`），本片把它们的**行为**
填成真实现，并新增第 8 条（`GET /api/issue-wakeup-summaries`，本片唯一真正新增注册键）。

- **base**：`feat/multica-rs-initial` @ `f6abce0`（起手 head 是 `542e833`，交付前 rebase 到最新 head）
- **记录号**：`docs/37` §36.5 的表分配（`46 = M5-1`、**`47 = M5-6`**、`48 = M5-7`）⇒ 本文件是 `docs/47`
- **分支**：`agent/devbox5/dbfe130cf0b8`
- **提交**：`687170c`（仓储）→ `049504d`（写路径错误分类 + cron/证据）→ `fa410ca`（服务层）→
  `c92c96c`（派发切片 + 单测）→ `877a5cc`（HTTP 层）→ `66761fd`（真库 e2e）→ `b389e08`（拆超限文件 + fmt）→
  `533a718`（门 ④ allow）
- **写集**：22 文件 / +5,902 −126（含 `docs/47` 本文件后共 23 文件 / +6,072 −126）。**未动**：`routes/mount.rs`、`routes/mod.rs`、`routes/issues/mod.rs`、
  各 `lib.rs`、`mc-core/src/wakeup.rs`、⑦ 基线、`docs/fixtures/**`、`scripts/**`。
  两个例外见 §4.5（`mc-http/Cargo.toml` 加一条 workspace 内依赖边）与 §4.6（`Cargo.lock` 一行）。

## 1. 交付面

| # | 文件 | 行数 | 内容 | 门禁 |
| - | --- | ---: | --- | --- |
| ① | `crates/mc-repos/src/wakeup/mod.rs` | 486 | 行结构（`WakeupRow` / `IssueWakeupView` / `WakeupSummaryRow` / `WakeupReceiptRow`）+ 手写 `FromRow` + 错误分类（`WakeupRepoError`，按约束名/SQLSTATE 而非文案） | ③⑤ |
| ② | `crates/mc-repos/src/wakeup/issue.rs` | 551 | `issue_wakeup` 全部读写：`create` / `replace`（upsert）/ `edit_instruction` / `disable(_with_reason)` / `advance` / `note_failure` / `lock*` / `ready_wakeups` / `list_issue_wakeups` | ③⑤ |
| ③ | `crates/mc-repos/src/wakeup/receipt.rs` | 156 | `issue_wakeup_receipt`：`record`（按 `(wakeup_id,revision,event_key)` upsert）/ `list_pending_for_update` / `consume` / `discard_for_wakeup` / `delete_expired` | ③⑤ |
| ④ | `crates/mc-repos/src/wakeup/listing.rs` | 182 | workspace 级两查询（列表 JSON + summaries 聚合 `rank<=3`） | ③⑤ |
| ⑤ | `crates/mc-repos/src/wakeup/lookup.rs` | 170 | `member_role` / `agent_for_wakeup` / `invocation_targets` / `comment_exists` / `lock_workspace` / `lock_task_owner_rows` / `delete_wakeup_cascade` | ③⑤ |
| ⑥ | `crates/mc-repos/src/wakeup/tests.rs` | 361 | 10 条纯单测（错误分类 / 约束名解析） | ⑤ |
| ⑦ | `crates/mc-autopilot/src/wakeup/mod.rs` | 112 | 面内错误类型 `WakeupError`（**不写 `src/error.rs`**，M5-1 热点） | ③⑤ |
| ⑧ | `crates/mc-autopilot/src/wakeup/service.rs` | 767 | `validate`(108 行对照) / `authorize` / `can_member_invoke_agent` / `save`(221) / `enable` / `edit_instruction` / `disable` / `stop_closed_issue_wakeups` / `check_claim` | ③⑤ |
| ⑨ | `crates/mc-autopilot/src/wakeup/service/tests.rs` | 240 | 8 条服务层纯单测 | ⑤ |
| ⑩ | `crates/mc-autopilot/src/wakeup/schedule.rs` | 414 | 最小 5 字段 cron 解析 + `next_occurrence_after_utc`（含时区/DST/星期名/步长） | ⑤ |
| ⑪ | `crates/mc-autopilot/src/wakeup/evidence.rs` | 410 | 收据 → 证据渲染（`legacy` 降级、并发合并 `…`） | ⑤ |
| ⑫ | `crates/mc-autopilot/src/wakeup/dispatch.rs` | 332 | `plan_dispatch` / `consume_dispatch` / `tick_candidates` / `note_dispatch_failure`（**M5-8 复用面**） | ⑤ |
| ⑬ | `crates/mc-http/src/routes/issues/wakeups.rs` | 393 | 6 条 issue 级路由（7 键：PUT 复用 POST 体） | ③④ |
| ⑭ | `crates/mc-http/src/routes/issue_wakeups.rs` | 420 | 2 条 workspace 级路由 + 共享错误/权限帮助函数 | ③④ |
| ⑮ | `crates/mc-http/tests/issues/wakeups.rs` + `wakeups/{support,crud,listing,isolation}.rs` | 27+136+554+143+113 | 7 条真库 e2e（`#[ignore]`） | ⑥ |

本仓 src（不含测试）≈ 4,994 行 vs 上游 ≈ 1,581 行 = **3.16×**（高于 `docs/44` §5.3 的 1.7–2.0× 经验值：
本片把上游 SQL 全部手写、把 `Validate` 的四类 kind × mode 组合显式化成检查、并补了逐条注释）。
单文件最大 **767** 行（硬门 800，⑩ 绿）。

## 2. 上游映射

### 2.1 路由（8 条，全单形态）

| METHOD PATH | 上游 handler | Rust 落点 |
| --- | --- | --- |
| `GET /api/issues/{id}/wakeups` | `ListIssueWakeups` | `issues/wakeups.rs::list_issue_wakeups` |
| `POST /api/issues/{id}/wakeups` | `CreateIssueWakeup` → 201 | `create_issue_wakeup` |
| `PUT /api/issues/{id}/wakeups/{wakeupID}` | `CreateIssueWakeup`（**upsert 复用**）→ 200 | `upsert_issue_wakeup` |
| `POST .../{wakeupID}/disable` | `DisableIssueWakeup` → 204 | `disable_issue_wakeup` |
| `POST .../{wakeupID}/enable` | `EnableIssueWakeup` → 204 | `enable_issue_wakeup` |
| `PATCH .../{wakeupID}/instruction` | `EditIssueWakeupInstruction` → 204 | `edit_issue_wakeup_instruction` |
| `GET /api/issue-wakeups` | `ListWorkspaceWakeups` | `issue_wakeups.rs::list_workspace_wakeups_handler` |
| `GET /api/issue-wakeup-summaries` | `ListWorkspaceWakeupSummaries` | `list_workspace_wakeup_summaries_handler` |

路径参数名保持 `:wakeupId`；**无尾斜杠别名**（`slash_alias_audit.py` 在门 ⑦ 里一并跑绿）。

### 2.2 服务层逐函数

| 上游 | Rust | 说明 |
| --- | --- | --- |
| `Validate`(108) | `service::validate` | 四类 kind 的必填/互斥：`at` XOR `after_seconds`、`every` 需 `interval_seconds>0`、`cron` 需 5 字段；`mode=once` 需一次性触发面 |
| `save`(221) | `service::save` | upsert：新建走 `create`（`revision` 取 DB 默认 1），改走 `replace`（`revision+1`、`enabled=true`、清 `disabled_at`/`last_task_id`/`last_error`） |
| `dispatch`(185) | `dispatch::plan_dispatch` / `consume_dispatch` | 事件去重 + 认领 + `next_fire_at` 推进；返回值收敛为 `DispatchPlan{Settled,Removed,Waiting,Dispatch}` |
| `Tick`(35) | `dispatch::tick_candidates` | 只取 `ready_wakeups`（`enabled AND next_fire_at<=now()`，批 100），调用方在 M5-8 |
| `CheckClaim`(32) | `service::check_claim` | 读 `mc-task` 认领闸（`context->>'wakeup_revision'` vs `issue_wakeup.revision`） |
| `EditInstruction`(30) | `service::edit_instruction` | `expected_instruction` 乐观锁；**不 bump revision** |
| `Disable`(25) / `Enable`(31) | `service::disable` / `enable` | `enable` 带 `rearm`/`at` 时若已启用 ⇒ `ErrWakeupConflict`；两者都不动已有收据 |
| `CanMemberInvokeAgent` | `service::can_member_invoke_agent` | 逐字复刻上游三层判定（role → owner → `permission_mode` + invocation targets） |
| `StopClosedIssueWakeups`(:783) | `service::stop_closed_issue_wakeups` | 非活跃 issue ⇒ `disable_issue_wakeups` + `cancel_unstarted_issue_wakeup_tasks` |
| `issue_wakeup_evidence.go`(135) | `wakeup::evidence` | `stable` / `legacy` 双形态渲染 + 合并截断 |
| `wakeup.sql`(23 查询) / `workspace_wakeup.sql`(1) | `mc-repos/src/wakeup/**` | 全部手写 SQL，JSONB 直接进出 |

### 2.3 类型与常量

`WakeupRow` 字段名 = 列名（`issue_wakeup` 26 列，与 `migrations/upstream/509` 实测列一致；`WakeupRow` 另带
`WakeupReceiptRow` 10 列）。容量常量沿用 `mc-core/src/wakeup.rs`（M5-0 落定）：
`MAX_ENABLED_WAKEUPS_PER_ISSUE = 32`、`MAX_ENABLED_WAKEUPS_PER_WORKSPACE = 1000`（触发 DB 触发器 ⇒ 映射 400
`wakeup_capacity_exceeded`）。请求体上限逐字对齐上游：创建 **32,768** / enable **1,024** / instruction
**160,000**；列表 `limit ∈ 1..=100`（默认 50）、`offset ∈ 0..=1_000_000`、`search` ≤ 256。

## 3. 契约

- **`kind ∈ {event,at,every,cron}` 与 `mode ∈ {once,continuous}` 正交**：不是单一 `source` 枚举；
  `GET` 回读的 `mode` 与 `next_fire_at` 形态由两者共同决定（`event`/`once` 无 `next_fire_at`）。
- **revision 配对**：`issue_wakeup.revision` ↔ `issue_wakeup_receipt.revision`。`save` 的 upsert 递增 revision ⇒
  旧 revision 的**待处理收据全部作废**（`mark_stale_revision_processed`，软消费：写 `processed_at` 而**不删行**）。
- **`dispatch` + `CheckClaim` 是共用契约**：M5-8 的 `mc-scheduler/src/jobs/issue_wakeup.rs` 只调用
  `tick_candidates` / `plan_dispatch` / `consume_dispatch`，不重新定义去重语义。
- **时间基准一律 `SELECT now()`**（`lookup::transaction_now`），不在应用侧取时钟，避免多实例时钟漂移。
- **HTTP 层做错误映射**：`WakeupHttpError::Coded{status,code,message}` 只用于需要自定义 code 的两条
  （`wakeup_capacity_exceeded` 400 / `wakeup_source_busy` 409），其余走既有 `ApiError`。

## 4. 与上游的偏差（逐条）

1. **cron 解析自己实现**：M5-3（`trigger.rs`）未合入，本片在 `schedule.rs` 里给最小 5 字段解析器
   （`*`/`a-b`/`*/n`/`a,b`/月份与星期名/`0|7=Sun`），支持时区与 DST 跳跃（跳过不存在的本地时刻）。
2. **错误信封形状不同**：本仓 `ApiError` 是 `{"error":{"code","message"}}`，`message` 带 code 标签前缀
   （如 `"conflict: wakeup changed; refresh and retry"`）；上游是扁平 `{"error":msg,"code":code}` 且 message 无前缀。
   状态码与 code 语义对齐，**e2e 断言用 `.ends_with(上游原文)` + `error.code`**。
3. **非成员状态码 = 403**（按 issue 的 DoD ⑦）；上游 `requireWorkspaceMember` 给 **404**。
   成员判定**先于** issue 加载 ⇒ 跨 workspace / 不存在的 issue 仍是 **404**（两义性保留）。
4. **没有任务事件广播**：上游 `Disable` 里的 `broadcastTaskEvent` 在本仓无对应面（WS 扇出在 M3-7 拆出），
   改为把待取消的队列行交给 M5-8（`cancel_unstarted_wakeup_tasks` 已就位）。
5. **`mc-http/Cargo.toml` 加一条依赖边** `mc-autopilot = { path = "../mc-autopilot" }`：HTTP 层要调服务层，
   而本仓 `Cargo.toml` 不在「不得再动」清单里的原因是它**不在共享锚点列表**（`routes/mount.rs` 等）；该边是
   workspace 内路径依赖，不引入新第三方包。
6. **`Cargo.lock` 一行**（`mc-http` 的 deps 数组加 `"mc-autopilot"`）：门 ② 与 CI 都是
   `cargo build --workspace --all-targets --locked` ⇒ 缺这行会**直接失败**；M5 后续切片若也要这条边，
   合并时同位置冲突取任一侧即可（重复插入会被 cargo 去重）。
7. **agent actor 分支本地不可达**：`X-Actor-Source: task_token` / `X-Agent-ID` 这套上游 `resolveActor` 走的是
   daemon 鉴权面；本仓 `/api/issues/*` 只有 `AuthUser`（`x-multica-user-id`）一条入口 ⇒ `resolveActor` 恒为
   `("member", user_id)`，`wakeupSourceTaskID` 恒 `None`，上游那条「需要人类发起来源」的 403 分支在此不可达。

## 5. 真库 e2e（DoD 7 场景 ⇒ 7 用例全绿）

`crates/mc-http/tests/issues/wakeups/**`，`MULTICA_TEST_DATABASE_URL` + `--ignored --test-threads=1`：

| DoD | 用例 | 断言要点 |
| --- | --- | --- |
| ① | `crud::wakeup_kinds_create_and_readback` | 四种 kind 各建一条 + 列表回读（`mode` / `next_fire_at` 形态） |
| ② | `crud::wakeup_mode_is_orthogonal_to_kind` | 同 kind 下 `once` vs `continuous` 的差异 |
| ③ | `crud::wakeup_upsert_bumps_revision_and_drops_receipts` | PUT 复用 POST 体、`revision` 递增、**旧 receipt 待处理数 1 → 0**（查 `processed_at IS NULL`） |
| ④ | `crud::wakeup_disable_then_enable` | `disable` → `enabled=false` / `disabled_at`；`enable` 回真 |
| ⑤ | `crud::wakeup_instruction_edit` | 204 + `expected_instruction` 冲突 409 + 体上限 400 |
| ⑥ | `listing::workspace_list_and_summaries` | `GET /api/issue-wakeups`（`items`/`total`/`counts`/`agents`）与 summaries 聚合口径 |
| ⑦ | `isolation::wakeup_membership_and_workspace_isolation` | 非成员 403；跨 workspace 404；header 优先于 `?workspace_slug` |

测试内种子：`seed_workspace` + `agent_runtime` + `agent`（`owner_id` = 测试用户、`permission_mode='public_to'`），
因为 `authorize` 要求发起人能 invoke 目标 agent。**未覆盖**：`filter_actor_type=agent` 的调用者形态（见 §4.7）。

## 6. 门禁读数（当轮 `gates-m5-6.log`，`grep` 所得）

```
① fmt 0 · ② build 0 · ③ clippy 0 · ④ clippy-test-util 0 · ⑤ test 0
⑥ db 0 (migrate=0, e2e=0, 121s) · ⑦ route-parity 0 · ⑨ conformance 0 · ⑩ file-size 0
  ⇒ overall: PASS — 10/10 gate(s) green in 309s   （`bash scripts/gates.sh --with-db`）
```

- ⑦：`upstream 456 | local 301 registered | baseline 290`；
  `implemented 242 real + 2 placeholder = 244 / 456，known_gap 212，unclaimed 0，regression 0，local_only 11`。
  相对 base `542e833`（`local 300 / 241 real / known_gap 213`）增量 = **+1 注册键 / +1 real / known_gap −1**，
  即 `GET /api/issue-wakeup-summaries`。**7 条 issue 子路由的键数与 `implemented` 计数都不变**
  （它们此前已被 ⑦ 记为 `implemented_real`，原因见 §8）。
- ⑥ 里本片相关读数：`wakeups::` 7 条 e2e 全过；`mc-autopilot` 的 `wakeup::` 单测 36 条、`mc-repos` 的
  `wakeup::tests::` 10 条全过（同一轮 `--ignored` 与单元跑都绿）。
- ⑤/门 ② 是 `--locked`：本片 `Cargo.lock` 一行增量（§4.6）就是为它准备的。

## 7. 未覆盖 / 交给后续片

1. **`Tick` 的调用方**（`mc-scheduler/src/jobs/issue_wakeup.rs` 接线 + 真库 7 例）⇒ **M5-8**。
2. **`broadcastTaskEvent` 面**（§4.4）⇒ M3-7 之后的 WS 扇出片；本片只提供待取消行。
3. **`CreateWakeupTask` / 真正起任务**：本片只到「认领 + 落收据」，任务创建仍在 `mc-task` 侧。
4. **`mode=once` 的连续触发**：`advance` 会按 `mode` 决定是否 `enabled=false`，但「一次性 wakeup 触发后的
   通知可见性」需 M5-8 的真库案例确认。
5. **`LUM-1580`（⑦ 检测器正则修复）不得与本片并行合并**：本片 §8 的账目依赖当前检测器口径。

## 8. 数字纪律：为什么不能用 ⑦ 的 owner 计数当本片进度

⑦ 的占位检测器只认 `\bplaceholder\b`（`scripts/route_parity.py`），而 M5-0 生成的 7 条 issue-wakeup 占位
handler 名是 **`not_implemented`**（不含 `placeholder` 字样）⇒ 这 7 键在本片动手**之前**就已经被记成
`implemented_real`。所以：

- 本片把 7 条从「返回 501」改成真实现，**⑦ 的 `implemented` 计数不变**（键数也不变）；
- 唯一会动的账是**人工维护的「全仓假实现」台账**：本片 **−7**（13 → 6）；
- 本片对 ⑦ 的可见增量只有 **+1**（`GET /api/issue-wakeup-summaries` 从 `known_gap` 转 `real`）。

检测器缺陷本身登记在 **R3**，修复片是 `LUM-1580`（不得与 M5-8/M5-INT 并行合并）。
