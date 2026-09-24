# docs/44 — M5（W5 自动化：autopilot / issue wakeup / 调度器）切片计划

> 本文件是 **M5 波（W5）的派发前计划**：把上游 `internal/scheduler` + autopilot / issue-wakeup 面
> 切成可并行、可独立验收的切片，使 **M4 收口（只剩 `LUM-1475` M4-4 与 `LUM-1476` M4-INT）后可以立刻派 M5 代码切片**。
>
> | 项 | 值 |
> | --- | --- |
> | 本仓基线 | `feat/multica-rs-initial` @ **`850fc27`**（= M4-0b 合并提交 + `docs/37` §29） |
> | 上游实测基线 | `90e0bdf`（clone `/tmp/ups_multica`）；路由表头部记录的 commit 是 **`f41fae6b*`**（⑦ 用的就是它） |
> | 上游路由表 | `docs/fixtures/upstream-routes.tsv`：**owner=M5 共 29 条**（456 总数不变） |
> | 本波路由面 | **29 条**（autopilot 20 + issue wakeup 8 + autopilot webhook 1） |
> | 本波代码面 | 上游非测试 **≈9.5k 行**（handler 4,328 + service 3,597 + scheduler 1,621）+ SQL 1,190 行 / 92 查询 |
> | 切片数 | **10**（含 1 个 anchor + 1 个集成片），并发上限 **3** |
> | 新 crate | **2**：`mc-autopilot`（领域服务层）、`mc-scheduler`（通用调度内核） |
> | 迁移 | **0 个新迁移**（12 张表全部已在 `migrations/`） |
> | M5 集成落地文档 | `docs/45-M5-INTEGRATION.md`（M5-INT 写，编号已预留，不与之抢） |
>
> **阅读顺序**：§0 结论速览 → §4 切片表（派发用）→ §5 anchor 清单（先动的那一片）→ §7 晋升顺序。
> §1 是测绘证据，§6 是门禁口径，§8 是风险登记（每条都有对应的切片 DoD）。
>
> **上游版本口径（已核对）**：`docs/fixtures/upstream-routes.tsv` 记录的上游 commit 是
> `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`，而本波的行数/span 实测来自 `90e0bdf` 的 clone。
> 两版之间 **M5 相关的 18 个上游文件与 `router.go` 逐字节相同**（复算见 §10 命令 5b）
> ⇒ 本表的行数与路由集合对两个 commit 都成立，不存在口径错配。

---

## 0. 结论速览

| 片 | 内容 | 上游行数 | 路由 | 依赖 | 波次 |
| --- | --- | ---: | ---: | --- | --- |
| **M5-0** | anchor scaffold：类型重写 + 两个新 crate 骨架 + 路由/仓储骨架 + 基线刷新 + 删 501 占位 | ~0（类型来自迁移列） | 0 | 无 | A |
| **M5-1** | autopilot 读面（list / detail / cron-preview / usage）+ quota 模块 | ≈1,550 | 4 | M5-0 | B |
| **M5-2** | autopilot 写面（create / update / delete / collaborators×2） | ≈1,200 | 5 | M5-0（读 M5-1） | C |
| **M5-3** | trigger 面 + 凭据（rotate token / signing secret） | ≈1,040 | 5 | M5-0（读 M5-1） | C |
| **M5-4** | 执行面（trigger / runs / runs/:id / deliveries×2 / replay）+ dispatch 服务层 | ≈2,600 | 6 | M5-1（quota）+ M5-2 | C |
| **M5-5** | autopilot webhook 入口（无认证面：token / 签名 / 限流 / 幂等） | ≈1,700 | 1 | M5-3 + M5-4 | D |
| **M5-6** | issue wakeup 用户面（6 条 issue 子路由 + 2 条 workspace 级） | ≈1,600 | 8 | M5-0 | B |
| **M5-7** | 调度器内核（spec / manager / db_ops + 主循环接线） | 1,152 | 0 | M5-0 | B |
| **M5-8** | 调度 jobs（autopilot schedule / issue wakeup） | 469 | 0 | M5-7 + M5-4 + M5-6 | D |
| **M5-INT** | 集成：⑦/⑨ 基线一次性刷新 + 本文档 §11 落地记录 + `docs/45` | 0 | 0 | 全部 | E |

**本波最容易踩的三个坑（先看这三条）**：

1. **`mc-core` 的两个既有 stub 是错的，必须重写而不是扩充**（§5.1 / R10）：`autopilot.rs`(85) 与
   `wakeup.rs`(72) 的字段与 `042_autopilot` / `509_issue_wakeup` 的列**大面积不符**（连主键语义、
   枚举取值都不对）。沿用旧 stub 会写出「编译通过、恒 500」的实现。M5-0 逐字段对照后**重写**这两个文件。
2. **7 条 wakeup 路由现在被 ⑦ 记成「已实现」是假的**（§1.3 / R3）：`routes/issues/mod.rs` 用
   `not_implemented`（501）注册它们，而 `scripts/route_parity.py:106` 的占位识别正则是
   `\bplaceholder\b` —— 匹配不到 `not_implemented`，于是这 7 个键被计成 `implemented_real`。
   **任何 M5 进度汇报前必须先扣掉这 7 条**。
3. **尾斜杠双形态本波实测 7 键 / 5 缺陷**（§1.4，不许抄 M4 的 15）：三条 chi Mount 路径
   （`/api/autopilots/`、`/api/autopilots/{id}/`、`/api/autopilots/{id}/triggers/{triggerId}/`）
   共 7 个 method 键必须两形态一起注册，漏一个是 404 而不是 307。

---

## 1. 上游面测绘（全部为 `90e0bdf` 实测）

### 1.1 路由表（29 条 = autopilot 20 + wakeup 8 + webhook 1）

`server/cmd/server/router.go`：autopilot 子树 `L2101–L2123`、issue wakeup 子树 `L1986–L1991`
（挂在 `/api/issues/{id}` 下）、workspace 级两条 `L2319–L2320`、webhook 入口 `L1487`。

| # | METHOD PATH | 上游 handler | 上游 span | 片 |
| ---: | --- | --- | ---: | --- |
| 1 | `GET /api/autopilots/` | `ListAutopilots` | 84 | M5-1 |
| 2 | `GET /api/autopilots/cron-preview` | `CronPreview` | 31 (+8) | M5-1 |
| 3 | `GET /api/autopilots/usage` | `GetAutopilotQuotaUsage` | 33 | M5-1 |
| 4 | `GET /api/autopilots/{id}/` | `GetAutopilot` | 73 | M5-1 |
| 5 | `POST /api/autopilots/` | `CreateAutopilot` | 159 | M5-2 |
| 6 | `PATCH /api/autopilots/{id}/` | `UpdateAutopilot` | 260 | M5-2 |
| 7 | `DELETE /api/autopilots/{id}/` | `DeleteAutopilot` | 64 | M5-2 |
| 8 | `POST /api/autopilots/{id}/collaborators` | `AddAutopilotCollaborator` | 59 (+17) | M5-2 |
| 9 | `DELETE /api/autopilots/{id}/collaborators/{userId}` | `RemoveAutopilotCollaborator` | 35 | M5-2 |
| 10 | `POST /api/autopilots/{id}/triggers` | `CreateAutopilotTrigger` | 202 (+56) | M5-3 |
| 11 | `PATCH /api/autopilots/{id}/triggers/{triggerId}/` | `UpdateAutopilotTrigger` | 172 | M5-3 |
| 12 | `DELETE /api/autopilots/{id}/triggers/{triggerId}/` | `DeleteAutopilotTrigger` | 74 | M5-3 |
| 13 | `POST /api/autopilots/{id}/triggers/{triggerId}/rotate-webhook-token` | `RotateAutopilotTriggerWebhookToken` | 69 | M5-3 |
| 14 | `PUT /api/autopilots/{id}/triggers/{triggerId}/signing-secret` | `SetAutopilotTriggerSigningSecret` | 65 | M5-3 |
| 15 | `POST /api/autopilots/{id}/trigger` | `TriggerAutopilot` | 73 (+15) | M5-4 |
| 16 | `GET /api/autopilots/{id}/runs` | `ListAutopilotRuns` | 50 | M5-4 |
| 17 | `GET /api/autopilots/{id}/runs/{runId}` | `GetAutopilotRun` | 44 | M5-4 |
| 18 | `GET /api/autopilots/{id}/deliveries` | `ListAutopilotDeliveries` | 48 | M5-4 |
| 19 | `GET /api/autopilots/{id}/deliveries/{deliveryId}` | `GetAutopilotDelivery` | 28 | M5-4 |
| 20 | `POST /api/autopilots/{id}/deliveries/{deliveryId}/replay` | `ReplayAutopilotDelivery` | 117 | M5-4 |
| 21 | `POST /api/webhooks/autopilots/{token}` | `HandleAutopilotWebhook` | 298 | M5-5 |
| 22 | `GET /api/issues/{id}/wakeups` | `ListIssueWakeups` | 31 | M5-6 |
| 23 | `POST /api/issues/{id}/wakeups` | `CreateIssueWakeup` | 47 | M5-6 |
| 24 | `PUT /api/issues/{id}/wakeups/{wakeupID}` | `CreateIssueWakeup`（upsert 复用） | — | M5-6 |
| 25 | `POST /api/issues/{id}/wakeups/{wakeupID}/disable` | `DisableIssueWakeup` | 25 | M5-6 |
| 26 | `POST /api/issues/{id}/wakeups/{wakeupID}/enable` | `EnableIssueWakeup` | 31 | M5-6 |
| 27 | `PATCH /api/issues/{id}/wakeups/{wakeupID}/instruction` | `EditIssueWakeupInstruction` | 30 | M5-6 |
| 28 | `GET /api/issue-wakeups` | `ListWorkspaceWakeups` | 81 | M5-6 |
| 29 | `GET /api/issue-wakeup-summaries` | `ListWorkspaceWakeupSummaries` | 27 | M5-6 |

口径提示：路由表的权威副本是 `docs/fixtures/upstream-routes.tsv`（含 `router.go` 行号），
本表与其 **owner=M5 行集合逐条相等**（§10 命令 1）。

### 1.2 上游文件与行数（非测试）

| 层 | 文件 | 行数 | 归属 |
| --- | --- | ---: | --- |
| handler | `handler/autopilot.go` | **2,469** | M5 |
| handler | `handler/autopilot_webhook.go` | **1,010** | M5 |
| handler | `handler/webhook_delivery.go` | **411** | M5（deliveries 3 条路由） |
| handler | `handler/issue_wakeup.go` | **320** | M5 |
| handler | `handler/wakeup_actor.go` | **63** | M5 |
| handler | `handler/autopilot_cron_preview.go` | **55** | M5 |
| service | `service/autopilot.go` | **1,930** | M5（dispatch / sync / 模板 / 分析） |
| service | `service/issue_wakeup.go` | **831** | M5 |
| service | `service/autopilot_quota.go` | **413** | M5 |
| service | `service/autopilot_quota_notifications.go` | **197** | M5 |
| service | `service/issue_wakeup_evidence.go` | **135** | M5 |
| service | `service/autopilot_notification_recipient.go` | **91** | M5 |
| service | `service/cron.go` | **138** | M5（cron 解析 + 下次触发；§5.4 选型） |
| scheduler | `scheduler/manager.go` | **489** | M5-7 |
| scheduler | `scheduler/db_ops.go` | **402** | M5-7 |
| scheduler | `scheduler/spec.go` | **261** | M5-7 |
| scheduler | `scheduler/jobs_autopilot.go` | **448** | M5-8 |
| scheduler | `scheduler/jobs_issue_wakeup.go` | **21** | M5-8 |
| scheduler | `scheduler/jobs_plugin_hook.go` | 353 | **M6**（本波只交付它要的内核） |
| scheduler | `scheduler/jobs_task_usage.go` | 120 | **M3/M9**（同上） |
| SQL | `db/queries/autopilot.sql` | 810 / 58 查询 | M5 |
| SQL | `db/queries/wakeup.sql` | 162 / 23 查询 | M5 |
| SQL | `db/queries/autopilot_quota.sql` | 148 / 10 查询 | M5 |
| SQL | `db/queries/workspace_wakeup.sql` | 70 / 1 查询 | M5 |

**M5 归属行数** = handler 4,328 + service 3,597 + scheduler 1,621 = **9,546 行** + SQL 1,190 行。
（本 issue 描述里的「5.6k」= `2469+1010+2094` 的口径：漏计了 `webhook_delivery.go` / `issue_wakeup.go` /
`wakeup_actor.go` / `autopilot_cron_preview.go` 的 849 行 handler，且完全没算 3,597 行 service。
口径差异见 §9.1，切片划分按 9,546 行做。)

**12 张表全部已在 `migrations/`，本波 0 个新迁移**：`autopilot` / `autopilot_trigger` / `autopilot_run`
（`042_autopilot`）、`webhook_delivery`（`093_webhook_deliveries`）、`autopilot_subscriber`（`120`）、
`autopilot_collaborator`（`128`）、`autopilot_rule_version`（`186`）、`autopilot_quota_period` /
`autopilot_quota_reservation`（`352`）、`issue_wakeup` / `issue_wakeup_receipt`（`509`）、
调度租约表 `sys_cron_executions`（`113`）。

### 1.3 本地现状与缺口

| 面 | 现状 | 证据 |
| --- | --- | --- |
| 领域类型 | `mc-core/src/autopilot.rs`(85) / `wakeup.rs`(72) 两个 stub，**字段与迁移列不符**（R10） | §5.1 逐字段对照 |
| 仓储 | `mc-repos` 0 个 autopilot / wakeup / scheduler 模块 | `ls crates/mc-repos/src/` |
| 服务层 | 无（`mc-autopilot` 不存在） | — |
| 路由 | `routes/autopilots*` 不存在；仅 `mount.rs` 的 `GET|POST /api/autopilots` 501 占位（L56–L59） | `grep -n autopilot mount.rs` |
| 路由（wakeup，**假实现**） | `routes/issues/mod.rs` L172–L189 用 `not_implemented` 注册 7 条，⑦ 记成 `implemented_real` | `route_parity.py:106` 正则 `\bplaceholder\b` |
| 路由（缺失） | `GET /api/issue-wakeup-summaries` 未注册（唯一真正 known_gap 的 wakeup 键） | ⑦ `known_gap` |
| scheduler | 0（无 crate、无主循环接线） | — |
| ⑦ 读数 | upstream 456 / local 292 / implemented 235（real 231 + placeholder 4）/ known_gap 221 / owners.M5 = **20** | 无参 `route_parity.py --json` |
| ⑦ 形态 | `MISSING_ALIAS(4)`：`GET|POST /api/autopilots`（M5，allowlist）+ `GET|POST /api/skills`（M6） | 无参 `slash_alias_audit.py` |
| ⑨ 契约 | 365 fixture / pass 5 / mismatch 23 / unmounted 31 / unevaluable 306 / 契约率 **1.37%**（mounted 等价率 17.86%；offline 可判 59 条，其中 pass 5） | `crates/mc-conformance/report.json` |
| ⑨ autopilot 域 | `contracts/golden/autopilots/` **8 条，全部 `unevaluable`**（member actor 需真库种子） | §6.2 |

`owners.M5 = 20` vs 真实 29 的差额拆解：**−2** 是 `mount.rs` 的占位（本来就不是实现），
**−7** 是上面那 7 条 501 被误计（本波 M5-6 落地后自动修正）。这也说明「⑦ 的 owner 计数」
在 M5 上不可直接当进度用。

### 1.4 尾斜杠双形态：本波实测

命令（预测模式，故意 exit 1）：

```
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m5-declared-routes.tsv
```

实测输出：**declared 29 键；需双形态 7 / 单形态 22；缺陷 5（+2 已 allowlist）**。
必须两形态一起注册的 7 个键（逐键）：

| 上游形态 | 别名形态（必须补注册） | 片 |
| --- | --- | --- |
| `GET /api/autopilots/` | `GET /api/autopilots` | M5-1 |
| `POST /api/autopilots/` | `POST /api/autopilots` | M5-2 |
| `GET /api/autopilots/{id}/` | `GET /api/autopilots/{id}` | M5-1 |
| `PATCH /api/autopilots/{id}/` | `PATCH /api/autopilots/{id}` | M5-2 |
| `DELETE /api/autopilots/{id}/` | `DELETE /api/autopilots/{id}` | M5-2 |
| `PATCH /api/autopilots/{id}/triggers/{triggerId}/` | `PATCH /api/autopilots/{id}/triggers/{triggerId}` | M5-3 |
| `DELETE /api/autopilots/{id}/triggers/{triggerId}/` | `DELETE /api/autopilots/{id}/triggers/{triggerId}` | M5-3 |

其余 22 键**只注册一个形态**（多注册会由 ⑦ 的第二条命令报 `EXTRA_ALIAS` 告警）。特别是：
`/api/autopilots/{id}/trigger`、`/runs`、`/deliveries`、`/collaborators`、`/wakeups*`、
`/api/issue-wakeups` 都是上游 plain 子路由 ⇒ **不加别名**。
`/api/webhooks/autopilots/{token}` 是独立 plain 注册（webhook 面唯一入口，无认证，见 R5）。

---

## 2. 目标架构与落点（含取舍）

### 2.1 分层落点

```
上游                              本仓
─────────────────────────────────────────────────────────────────────────
handler/*.go                →   crates/mc-http/src/routes/autopilots/**
                                crates/mc-http/src/routes/webhooks/autopilots.rs
                                crates/mc-http/src/routes/issues/wakeups.rs   （M2-A 的 501 位置换）
                                crates/mc-http/src/routes/issue_wakeups.rs   （workspace 级 2 条）
service/*.go                →   crates/mc-autopilot/**        【新 crate】
db/queries/*.sql            →   crates/mc-repos/src/{autopilot/**,wakeup/**,scheduler.rs}
internal/scheduler/{spec,manager,db_ops}.go
                            →   crates/mc-scheduler/**        【新 crate】
internal/scheduler/jobs_*.go→   crates/mc-scheduler/src/jobs/**
调度主循环（main.go）        →   apps/mc-server/src/main.rs（一处 spawn，M5-7 写）
领域类型（迁移列）           →   crates/mc-core/src/{autopilot.rs,autopilot_quota.rs,wakeup.rs}
```

### 2.2 为什么要有 `mc-autopilot` 这个 crate（判据 + 被否决的备选）

M4 的做法是「服务逻辑留在 `mc-http/src/routes/<domain>/*.rs`，crate 只放纯领域类型」
（`mc-project` 33 行 / `mc-squad` 256 行 / `mc-chat` 671 行，而 `routes/{projects,squads,chat}/**` 4,476 行）。
**M5 不沿用这个做法**，判据三条：

1. **行数**：M5 服务面 = `service/autopilot.go` 1,930 + `autopilot_quota.go` 413 + `quota_notifications.go` 197
   + `notification_recipient.go` 91 = **2,631 行**（外加 469 行调度 job）。塞进 `mc-http` 需要 3–4 个
   ~900 行的文件 ⇒ 直接撞 800 行硬门（`scripts/file_size_check.py`），且每次拆都要动共享 `mod.rs`。
2. **依赖方向**：`scheduler/jobs_autopilot.go` 的 handler 要调用 **同一个** `DispatchAutopilotForPlan`。
   若 dispatch 留在 `mc-http`，`mc-scheduler` 就得依赖 `mc-http` —— 把整个 axum 面拖进一个 tick 循环
   crate，方向是错的（调度器不该认识 HTTP 状态码和 `Json<...>` 提取器）。
3. **可测性**：上游这些逻辑的主测试是 `direct_handler` / 真库集成（`autopilot_quota_test.go` 1,045、
   `webhook_delivery_test.go` 1,007、`autopilot_schedule_job_test.go` 820），不经 HTTP。
   放在 `mc-autopilot` 可以直接用「真库事务 + typed 输入/输出」测，等价度更高。

**代价（已登记）**：多一个 manifest + `Cargo.lock` delta（由 M5-0 一次性加）；
handler 的鉴权（`memberCanWriteAutopilot` 等 285 行）**留在 `mc-http`**，
`mc-autopilot` 只接收已解析的 actor 与领域参数 —— 边界写死在 §2.4。

被否决的备选：① 全部塞 `mc-http` 路由（= M4 做法）——被判据 1/2 否决；
② 塞 `mc-repos` —— `mc-repos` 是纯 SQL 行映射（`project/` 只有 `search.rs` + `tests.rs`），
事务编排、幂等、通知不属它的职责；③ 塞 `mc-daemon` —— 见 §2.3。

### 2.3 调度器落点判据（`mc-scheduler` + 一个 spawn 点）

判据三条，逐条实测：

1. **谁持有 tokio 主循环** → `apps/mc-server/src/main.rs`（`#[tokio::main]`，200 行，装配
   `Db` → `AppState` → `axum::serve`）。`mc-daemon` 是**客户端/execenv 侧** crate，本仓没有 daemon
   二进制 ⇒ 调度器只能由 server 二进制启动（与上游 `main.go` 一致）。
2. **谁持有 DB 连接池与租约表** → server（`Db::connect` → `AppState`）；租约表 `sys_cron_executions`
   是 server 侧 `migrations/upstream/113_*`，与 daemon 协议无关。
3. **是否耦合 daemon 协议** → **否**。调度器把「该跑一次」变成写入 `agent_task_queue` 的一行，
   daemon 只是**认领**（M3 已合）。调度器不认识 daemon 的 HTTP 面 ⇒ 不放进 `mc-daemon`。

**结论**：新建 `mc-scheduler`（通用内核：`spec.rs` / `manager.rs` / `db_ops.rs` + `jobs/`），
在 `apps/mc-server/src/main.rs` 里 spawn 一次（M5-7 写 spawn 块，M5-8 往注册表加 2 行 ⇒ 串行边）。
依赖方向 `mc-scheduler → mc-autopilot → mc-repos → mc-db`，`crates/*` 是 glob 成员 ⇒ 根 `Cargo.toml` 不动。

**跨波登记**：本 crate 的内核同时服务 **M6**（`jobs_plugin_hook.go` 353 行）与 **M9/M3**
（`jobs_task_usage.go` 120 行）；本波只交付内核 + M5 两个 job，M6/M9 只加 `jobs/*.rs`。

### 2.4 边界契约（切片必须遵守，写进各片 DoD）

- **C1** `mc-autopilot` 不依赖 `mc-http`（不得出现 `axum`）；`mc-scheduler` 不依赖 `mc-http`。
- **C2** `mc-autopilot` 的公开函数只收 `&Db` / `&mut Transaction` + 领域入参 + actor 结构体，
  返回 `Result<T, AutopilotError>`；HTTP 状态码映射只在 `mc-http` 侧。
- **C3** 鉴权（谁能写、谁能管访问）在 `mc-http`；但**越权判定所需的行数据**由 `mc-repos` 提供。
- **C4** 凭据（webhook token、signing secret）只经 `mc-telemetry` 的
  `redact_str` / `redact_json` / `redact_log`（`crates/mc-telemetry/src/redact.rs`）出日志与响应；
  响应里只允许出现 `signingSecretHint`（上游 15 行）那类 hint。
- **C5** 一切时间基准取自 `SELECT now()`（上游 `NextOccurrenceAfterUTC` 的注释契约：
  两实例时钟偏移不得导致不同 `plan_time`）。

---

## 3. 写集与并发

### 3.1 共享锚点（只在 M5-0 动一次）

| 文件 | M5-0 的动作 | 之后谁都不动 |
| --- | --- | --- |
| `crates/mc-core/src/lib.rs` | +1 行（`pub mod autopilot_quota;`） | ✓ |
| `crates/mc-core/src/{autopilot.rs,autopilot_quota.rs,wakeup.rs}` | **重写/新建**（§5.1） | 除类型 bug 外不动 |
| `crates/mc-repos/src/lib.rs` | +3 行（`autopilot` / `wakeup` / `scheduler`） | ✓ |
| `crates/mc-http/src/routes/mod.rs` | +4 行（M5 模块块） | ✓ |
| `crates/mc-http/src/routes/mount.rs` | +`mount_slice_autopilot()` 并 merge；**删 2 行 501 占位** | ✓ |
| `crates/mc-http/src/routes/issues/mod.rs` | **删 7 个 501 `.route(...)` 块** + `.merge(wakeups::router())` | ✓ |
| `Cargo.lock` + 两个新 crate 的 `Cargo.toml` | 建 crate、加全部依赖（`cron`/`chrono-tz` 等，§5.4） | ✓ |
| `docs/fixtures/route-parity-baseline.json` | 刷基线（290） | 之后只在 M5-INT 再刷 |
| `docs/fixtures/slash-alias-allowlist.tsv` | 删 2 行 M5（`GET|POST /api/autopilots`） | ✓ |
| `apps/mc-server/src/main.rs` | ——（M5-7 写 spawn 块；M5-8 加 2 行注册） | 串行边 |

`routes/issues/mod.rs` 的先例已经写好（M3-6 移交段）：同 path+method 重复注册会让 axum 在
`Router::route` 处 **panic**，所以「删 501 + 挂子 router」必须在同一个 commit 里完成。

### 3.2 写集矩阵（一格一个文件，一格一个写者）

| 文件 | M5-1 | M5-2 | M5-3 | M5-4 | M5-5 | M5-6 | M5-7 | M5-8 |
| --- | :-: | :-: | :-: | :-: | :-: | :-: | :-: | :-: |
| `mc-core/src/autopilot*.rs` | — | — | 读 | 读 | 读 | — | — | 读 |
| `mc-core/src/wakeup.rs` | — | — | — | — | — | 读 | — | 读 |
| `mc-repos/src/autopilot/mod.rs`（行 + 共享 SELECT） | **W** | 读 | 读 | 读 | 读 | — | — | 读 |
| `mc-repos/src/autopilot/write.rs` | — | **W** | — | — | — | — | — | — |
| `mc-repos/src/autopilot/trigger.rs` | — | — | **W** | — | 读 | — | — | — |
| `mc-repos/src/autopilot/run.rs` | — | — | — | **W** | — | — | — | 读 |
| `mc-repos/src/autopilot/delivery.rs` | — | — | — | **W** | — | — | — | — |
| `mc-repos/src/autopilot/ingress.rs` | — | — | — | — | **W** | — | — | — |
| `mc-repos/src/autopilot/quota.rs` | **W** | — | — | 读 | — | — | — | — |
| `mc-repos/src/wakeup/**.rs` | — | — | — | — | — | **W** | 读 | 读 |
| `mc-repos/src/scheduler.rs` | — | — | — | — | — | — | **W** | — |
| `mc-http/…/autopilots/access.rs`、`dto.rs`、`list.rs` | **W** | 读 | 读 | 读 | 读 | — | — | — |
| `mc-http/…/autopilots/{crud,subscribers,assignee}.rs` | — | **W** | — | — | — | — | — | — |
| `mc-http/…/autopilots/{trigger,credentials}.rs` | — | — | **W** | — | — | — | — | — |
| `mc-http/…/autopilots/{execution,delivery}.rs` | — | — | — | **W** | — | — | — | — |
| `mc-http/…/webhooks/autopilots.rs` | — | — | — | — | **W** | — | — | — |
| `mc-http/…/issues/wakeups.rs` + `issue_wakeups.rs` | — | — | — | — | — | **W** | — | — |
| `mc-autopilot/src/{quota,notification}.rs` | **W** | 读 | — | 读 | — | — | — | — |
| `mc-autopilot/src/{write,collaborator}.rs` | — | **W** | 读 | — | — | — | — | — |
| `mc-autopilot/src/{trigger,credential}.rs` | — | — | **W** | — | 读 | — | — | — |
| `mc-autopilot/src/dispatch/**` | — | — | — | **W** | 读 | — | — | — |
| `mc-autopilot/src/webhook/**` | — | — | — | — | **W** | — | — | — |
| `mc-autopilot/src/wakeup/**` | — | — | — | — | — | **W** | — | — |
| `mc-scheduler/src/{spec,manager,db_ops}.rs` | — | — | — | — | — | — | **W** | 读 |
| `mc-scheduler/src/jobs/{autopilot,issue_wakeup}.rs` | — | — | — | — | — | — | — | **W** |
| `apps/mc-server/src/main.rs` | — | — | — | — | — | — | **W** | **W**（串行在后） |

**并发结论**：B 波 `M5-1 ∥ M5-6 ∥ M5-7` 零文件交集；C 波 `M5-2 ∥ M5-3 ∥ M5-4` 零文件交集
（M5-4 只 **读** `mc-repos/src/autopilot/mod.rs` 与 `mc-http/…/dto.rs`，这两者已在 B 波合并）；
D 波 `M5-5 ∥ M5-8` 零交集。**全波无「同一文件两个并发写者」**。

**补记（13:30 cycle / `LUM-1607` 实测，`docs/37` §36.6）**：上表的「零交集」对 `mc-autopilot` 的
**框架三文件**（`src/lib.rs` / `src/error.rs` / `src/dto.rs`）原本**没有行** —— anchor 把它们判给 M5-1
（见 `src/lib.rs` 的写者表），但同时写着「其余切片加自己的错误变体时只准加变体」⇒ `src/error.rs`
在 **C 波（`M5-2 ∥ M5-3 ∥ M5-4`，真 3 片并行）** 上是**多写者**：三片都要 400/403/404/409 语义，
最省事的写法就是各自往同一个 enum 尾部 + 同一个 `AutopilotError → ApiError` 映射里追加 ⇒ PR 层必然文本冲突。
**C 波派发时每片 DoD 必须写明两条**：① 私有错误/形状放**自己的文件**，只有跨切片共享的才进 `src/error.rs`；
② 确需进时**只在文件末尾追加**（变体与 match 臂），禁止重排/重命名/改既有行。
（`mc-http/…/autopilots/dto.rs` 判给 M5-1 单写者，**不需要**这条补丁；`src/lib.rs` 在 B 波后不再变。）

---

## 4. 切片表（派发用）

### 4.1 全景

| 片 | 上游组成（带 span） | 上游行数 | 路由 | 交付物 | 依赖 |
| --- | --- | ---: | ---: | --- | --- |
| M5-0 | 无（类型来自 12 张迁移列） | ~0 | 0 | §5 全部骨架 + 基线 | — |
| M5-1 | `autopilot.go` 共享件 29–438（`computeNextRun`75 / `collaboratorToEntry`81 / `autopilotToResponse`37 / `triggerToResponse`50 / `signingSecretHint`15 / `redactWebhookSecrets`19 / `broadcast…`9 / `webhookPathForToken`4 / `runToResponse`32 / `runToResponseSlim`88）+ `ListAutopilots`84 + `GetAutopilot`73 + `loadAutopilotInWorkspace`25 + 权限 157（`autopilotWriteByOwnership`14 / `memberCanWriteAutopilot`70 / `autopilotActingUserID`36 / `requireAutopilotActingMember`35）+ `CronPreview`31 + `GetAutopilotQuotaUsage`33 + `service/autopilot_quota.go`414 + `autopilot_quota_notifications.go`198 + `service/cron.go`138 + `autopilot_quota.sql` | ≈1,550 | 4 | 读面 + quota 模块 + cron 基座 | M5-0 |
| M5-2 | `CreateAutopilot`159 / `parseAutopilotSubscribers`33 / `lockAndValidateAutopilotSubscribers`39 / `UpdateAutopilot`260 / `autopilotRuleSubstantiveChange`13 / `recordAutopilotRuleVersion`4 / `parseAutopilotProjectID`23 / `DeleteAutopilot`64 / `AddAutopilotCollaborator`59 / `writeAutopilotCollaborators`17 / `RemoveAutopilotCollaborator`35 / `validateAutopilotAssigneeForSave`76 / `isValidAutopilotAssigneeType`19 / `service/autopilot_notification_recipient.go`91 + `autopilot.sql` 写查询 | ≈1,200 | 5 | 写面 + 协作者 + 订阅者 | M5-0 |
| M5-3 | `CreateAutopilotTrigger`202 / `createWebhookTriggerWithMintedToken`56 / `isAllowedWebhookProvider`9 / `UpdateAutopilotTrigger`172 / `DeleteAutopilotTrigger`74 / `RotateAutopilotTriggerWebhookToken`69 / `SetAutopilotTriggerSigningSecret`65 + `autopilot.sql` trigger 查询 | ≈1,040 | 5 | trigger CRUD + 凭据 | M5-0 |
| M5-4 | `TriggerAutopilot`73 / `requireAutopilotTriggerInvoker`15 / `ListAutopilotRuns`50 / `GetAutopilotRun`44 + `webhook_delivery.go` 全 411 + `service/autopilot.go` dispatch 段（`DispatchAutopilot`35 / `…Manual`12 / `…ManualWithKey`20 / `dispatchAutopilot`52 / `dispatchAutopilotRun`57 / `dispatchCreateIssue`199 / `notifyAutopilotSubscribersOnCreate`91 / `dispatchRunOnly`104 / `SyncRunFrom*`159 / `handleDispatchSkip`33 / `shouldSkipDispatch`98 / `recordSkippedRun`58 / `failRun`25 / `publishRunDone`13 / 分析 92 / 模板与工具 200） | ≈2,600 | 6 | 执行面 + dispatch | M5-1 + M5-2 |
| M5-5 | `autopilot_webhook.go` 全 1,010（`HandleAutopilotWebhook`298 / `persistInboundDelivery`52 / `finalise*`65 / 签名 36 / 事件过滤 129 / 限流 40 / provider 适配 145 / 其余工具）+ `service` 的 `AdmitAutopilotWebhookDelivery`68 / `recoverConcurrentWebhookAdmission`26 / `DispatchAutopilotForWebhookDelivery`43 / `ensureWebhookCreateIssueTask`61 / `repairAutopilotRunTaskLink`56 | ≈1,700 | 1 | webhook 入口 | M5-3 + M5-4 |
| M5-6 | `issue_wakeup.go`321 + `wakeup_actor.go`64 + `service/issue_wakeup.go`832（`Validate`108 / `save`221 / `dispatch`185 / `Tick`35 / 其余）+ `issue_wakeup_evidence.go`136 + `wakeup.sql`162/23q + `workspace_wakeup.sql`70/1q | ≈1,600 | 8 | wakeup 全用户面 | M5-0 |
| M5-7 | `scheduler/spec.go`262 + `manager.go`490 + `db_ops.go`403 + 主循环接线 | 1,152 | 0 | 租约内核 | M5-0 |
| M5-8 | `scheduler/jobs_autopilot.go`449 + `jobs_issue_wakeup.go`22 | 469 | 0 | 两个 job | M5-7 + M5-4 + M5-6 |
| M5-INT | — | 0 | 0 | ⑦/⑨ 刷新 + 落地文档 | 全部 |

### 4.2 逐片要点（评审用，不替代切片自己的 DoD）

- **M5-1**：`autopilotToResponse`(37) 的 JSON 契约是本波最容易被抄错的（`assignee_type` /
  `pause_reason` / `execution_mode` / `can_write` / `can_manage_access` / 列表专属
  `trigger_kinds` `next_run_at` `last_run_status`；`can_write` 的文档注释说明「不带 caller 时省略，
  客户端按 unknown 处理」，本地 DTO 必须区分 `Option<bool>` 与 `bool`）。列表查询的派生字段
  （启用 trigger 的 kinds / 最近 run 状态 / 下次触发）是 3 条额外 SQL，别漏。
  quota 侧：`AutopilotQuotaUsage`(40) 是 `usage` 路由的唯一契约来源；`QuotaEnabled()`(6) 依赖
  entitlement 平面（R7）。`service/cron.go`(138) 提供 `NextOccurrenceAfterUTC` /
  `NextOccurrencesAfterUTC`，是本波**唯一的 cron 解析点**（`cron-preview` 与 M5-7 的 plan_time 共用）。
- **M5-2**：`UpdateAutopilot`(260) 是三态补丁（缺失 / `null` / 有值）大户 + 规则版本落库
  （`autopilotRuleSubstantiveChange` 判「实质变更」）；`validateAutopilotAssigneeForSave`(76) 要区分
  `agent` / `squad`（`squad` → 运行期解析 `squad.leader_id`，见响应注释）。
  `lockAndValidateAutopilotSubscribers`(39) 必须**在同一事务内锁**（上游用 `FOR SHARE`/`FOR UPDATE` 语义，
  落地时以真库并发测试为准）。
- **M5-3**：凭据两条（`rotate-webhook-token` / `signing-secret`）是**写敏感值**的路由：
  `redactWebhookSecrets`(19) + `signingSecretHint`(15) 是响应契约（只出 hint），C4 强制走 redaction 通道。
  `createWebhookTriggerWithMintedToken`(56) 的 token 生成 + 唯一性冲突重试要落 `webhookPathForToken`(4) 的
  路径形态（与 M5-5 的 ingress 路径参数一致）。
- **M5-4**：本波最大的一刀（≈2.6k）。三块必须分开测：① `dispatchCreateIssue`(199) 建 issue + 派任务；
  ② `dispatchRunOnly`(104) 只派任务；③ `SyncRunFrom*`(159) 把任务终态回写 run。
  `shouldSkipDispatch`(98) + `recordSkippedRun`(58) 是「重复抑制 / 并发策略（skip|queue|replace）」的
  真值来源，是 ⑨ 里 `TestDispatchAutopilotForPlanIsIdempotent` 那一族的上游对应物。
  **边界**：若发现 `agent_task_queue` 的 queued 行不会被执行（daemon 面未接线），登记为跨波缺口，
  **不要在 M5 里实现 daemon**（R9）。
- **M5-5**：唯一无认证入口（R5）。必须落地：token → trigger 解析、provider 签名校验
  （`verifyWebhookSignatureForProvider`18 + `verifyHubSignature`18）、事件过滤（`validateWebhookEventFilters`17 +
  `webhookEventAllowedByTriggerScope`45 + `webhookActionCandidates`45）、幂等（`extractDedupeKey`23 +
  `dedupe_key` 部分唯一索引）、限流（`writeWebhookRateLimit`22 + `clientIPForRateLimit`22 +
  `remoteAddrHost`17 + `parseNetIPAddr`11 + `addrInPrefixes`9）、body 规范化（`normalizeWebhookPayload`61 +
  `stripBOM`18 + `inferEvent`31）。`delivery.status` 语义按 `093_webhook_deliveries.up.sql`
  的注释（`queued` / `dispatched` / …；被 admission 跳过的 run 仍算 `dispatched`，
  skipped 记在 `autopilot_run.status`）。
- **M5-6**：8 条路由里有 7 条要**从 `routes/issues/mod.rs` 的 501 换成真实现**（M5-0 已把注册搬进
  `issues/wakeups.rs`，本片只填实现）。`service/issue_wakeup.go` 的 `Validate`(108) 与 `save`(221)
  是 upsert + revision 语义（`issue_wakeup.revision` 与 `issue_wakeup_receipt.revision` 配对：
  revision 变更后旧 receipt 失效）；`dispatch`(185) + `CheckClaim`(32) 是「事件去重 + 认领」，
  与 M5-8 的 job 共用。`kind ∈ {event,at,every,cron}` + `mode ∈ {once,continuous}` 是**两个正交维度**
  （旧 stub 把它们压成了一个 `source` 枚举 —— 见 R10）。
- **M5-7**：租约内核，`sys_cron_executions` 是唯一同步点（R2）。`spec.go` 的 `String()` 两个
  （24 + 132 行的格式化/解析）、`validate`(34)、`retryDelay`(16)、`FloorPlan`(8) 是 plan_time 取整契约；
  `manager.go` 的 `plansForTick`(95) + `runClaimed`(116) + `runHeartbeats`(33) + `classifyError`(23)
  是主循环；`db_ops.go` 的 `tryClaim`(110) + `markStaleAsFailed`(26) + `finishFailure`(46) +
  `RetryEligible`(18) 是租约安全面。**本片不注册任何 job**（注册表空转）⇒ 可独立验收。
- **M5-8**：两个 job 各 ~1 个 handler，但 `jobs_autopilot.go` 的 `autopilotScopes`(66) +
  `autopilotPlansForScope`(84) + `isAutopilotSchedulePlanStale`(9) + `advancedNextRun`(15) 才是难点
  （按 workspace × 时区分桶算 plan_time，且要处理「慢 tick 后 plan 已过期」）。
  `jobs_issue_wakeup.go` 只有 22 行（`Tick` 的薄壳）⇒ 真正的活在上游 `service/issue_wakeup.go`。
- **M5-INT**：⑦/⑨ 一次性刷新（本文档 §6 给预测值，实际必须以当轮 gate 日志为准）、
  `docs/44` 追加 §11 落地记录、写 `docs/45-M5-INTEGRATION.md`。

### 4.3 波次（并发 ≤3）

```
A: M5-0                     [1]
B: M5-1 ∥ M5-6 ∥ M5-7       [3]
C: M5-2 ∥ M5-3 ∥ M5-4       [3]
D: M5-5 ∥ M5-8              [2]
E: M5-INT                   [1]
```

串行边（都不是「等一个 PR 合完」的长链，而是**读对方已合并文件**）：M5-2/3/4 ← M5-1；
M5-5 ← M5-3+M5-4；M5-8 ← M5-7+M5-4+M5-6。**B 波三片是本波的吞吐关键**：M5-1 只放 4 条路由
但交付 dto/access/quota 三个共享面，所以它是 C 波的前置，不能延后。

---

## 5. M5-0 anchor：逐文件预扩展清单

anchor 的定位与 M4-0 相同：**只建骨架 + 刷基线，不放业务逻辑**（业务行数计入对应切片）。

### 5.1 领域类型：**重写**（不是扩充）

| 文件 | 现状 | 动作 |
| --- | --- | --- |
| `crates/mc-core/src/autopilot.rs` | 85 行，字段与 `042_autopilot` / `093` / `120` / `128` / `186` / `352` 不符 | **重写**为 ≥5 个类型组（Autopilot / Trigger / Run / Delivery / Collaborator+Subscriber / RuleVersion） |
| `crates/mc-core/src/autopilot_quota.rs` | 不存在 | 新建（period / reservation / usage / 决策枚举） |
| `crates/mc-core/src/wakeup.rs` | 72 行，把 kind/mode 压成 `source` 枚举 | **重写**为上游 `509_issue_wakeup` 的 kind/mode/event_types/filters/revision 语义 |

**逐字段对照（R10 的证据，落地时按迁移列写死）——`autopilot` 表**：

| 上游列（`042` + 迁移追加） | 本地 stub 字段 | 判定 |
| --- | --- | --- |
| `id` / `workspace_id` / `project_id` / `description` | 有 | ✓ |
| `title` | `name` | ✗ 改名 + 语义 |
| `assignee_type` / `assignee_id` | `squad_id` | ✗ agent/squad 二态被压成 squad |
| `priority` | 无 | ✗ 缺 |
| `status ∈ {active,paused,archived}` + `pause_reason` | `enabled: bool` | ✗ 二态 vs 三态 + 原因 |
| `execution_mode ∈ {create_issue,run_only}` | 无 | ✗ 缺（决定派单分支） |
| `issue_title_template` | 无 | ✗ 缺 |
| `concurrency_policy ∈ {skip,queue,replace}` | 无 | ✗ 缺（`shouldSkipDispatch` 的真值） |
| `created_by_type` / `created_by_id` | 无 | ✗ 缺（越权判定要用） |
| `last_run_at` | 无 | ✗ 缺 |
| `created_at` / `updated_at` | 有 | ✓ |
| —— | `trigger` / `cron_expression` / `webhook_url` / `trigger_event_filters` | ✗ **不属于本表**（属 `autopilot_trigger`） |
| —— | `rule_version` / `rule` | ✗ **不属于本表**（属 `autopilot_rule_version`） |

**`issue_wakeup` 表**（`509`）：上游列 = `id / workspace_id / issue_id / agent_id / created_by /
source_task_id / parent_comment_id / instruction / kind(4) / mode(2) / event_types[] / filter_agent_id /
filter_task_id / interval_seconds / cron_expression / timezone / next_fire_at / enabled / disabled_at /
revision / last_task_id / last_error / created_at / updated_at`。
本地 stub = `id / workspace_id / issue_id / **source** / **event_type** / **due_at** / **actor_type** /
**actor_id** / **status** / **run_id** / **receipt_id** / **coalesced_count** / created_at / updated_at`
⇒ 12 个字段里 6 个是上游没有的（`run_id` / `receipt_id` / `coalesced_count` 的语义在
`issue_wakeup_receipt` 表），且缺 9 个上游列。**这是本波第一优先级的重写**。

### 5.2 新 crate 骨架

`crates/mc-autopilot/`：`Cargo.toml`（依赖 `mc-core` / `mc-repos` / `mc-telemetry` / `mc-realtime` /
`serde` / `serde_json` / `sqlx` / `chrono` / `chrono-tz` / `uuid` / `tracing` / `thiserror`）、
`src/lib.rs`、`src/error.rs`、`src/dto.rs`、`src/quota.rs`、`src/notification.rs`、`src/write.rs`、
`src/collaborator.rs`、`src/trigger.rs`、`src/credential.rs`、
`src/dispatch/{mod,create_issue,run_only,sync,skip,analytics}.rs`、
`src/webhook/{mod,signature,ratelimit,admission,provider}.rs`、
`src/wakeup/{mod,service,evidence}.rs`。

`crates/mc-scheduler/`：`Cargo.toml`（依赖 `mc-core` / `mc-repos` / `mc-autopilot` / `tokio`(rt,time) /
`chrono` / `uuid` / `tracing` / `thiserror`）、`src/lib.rs`、`src/error.rs`、`src/spec.rs`、
`src/manager.rs`、`src/db_ops.rs`、`src/jobs/{mod,autopilot,issue_wakeup}.rs`。

### 5.3 仓储 / 路由骨架（**全部空 router，只建文件**）

- `mc-repos/src/`：`autopilot/{mod.rs,write.rs,trigger.rs,run.rs,delivery.rs,ingress.rs,quota.rs}`、
  `wakeup/{mod.rs,issue.rs,receipt.rs}`、`scheduler.rs`，并在 `lib.rs` +3 行。
  （按 §3.2 的「一格一写者」拆文件，**就是为了让 C 波三片能真并行**。）
- `mc-http/src/routes/`：`autopilots/{mod.rs,access.rs,list.rs,dto.rs,crud.rs,subscribers.rs,assignee.rs,
  trigger.rs,credentials.rs,execution.rs,delivery.rs}`、`webhooks/{mod.rs,autopilots.rs}`、
  `issue_wakeups.rs`、`issues/wakeups.rs`；`routes/mod.rs` +4 行；`mount.rs` 挂
  `mount_slice_autopilot()` 并**删掉 2 行 `/api/autopilots` 501 占位**（真实路由与占位同 path+method
  会 panic）。
- `routes/issues/mod.rs`：删 7 个 501 `.route(...)` 块（L172–L189）→ `.merge(wakeups::router())`。
  注意保留 `:wakeupId` 的命名（与其它 issue 子路由一致；⑦ 归一化后与上游 `{wakeupID}` 等价）。

### 5.4 anchor 的技术选型（必须实测后再定，不许凭印象）

1. **cron 解析**：上游是 `robfig/cron` 且 `cron.NewParser(Minute|Hour|Dom|Month|Dow)` ——
   **5 字段、无秒**（`service/cron.go:12`）。Rust 侧要在 `cron` crate 与手写之间选：必须先验证
   5 字段语义、`Dom`/`Dow` 的 OR 行为、以及「无下次触发」（`0 0 30 2 *`）的返回形态，再定。
   **选定后只在 anchor 加依赖**（否则每个切片都动 `Cargo.lock`）。
2. **时区**：`autopilot_trigger.timezone` + `issue_wakeup.timezone` 需要 `chrono-tz`（IANA）。
   anchor 一次引入，并在 `mc-core` 暴露一个 `Timezone` 校验（上游 `resolveAutopilotTriggerTimezone`29）。
3. **scheduler 的 tokio 形态**：`manager.go` 是「每 tick 一次 `Run(ctx)`」，本地对应
   `tokio::spawn` + `tokio::time::interval` + `CancellationToken`（graceful shutdown 要接
   `apps/mc-server/src/main.rs` 的 `shutdown_signal`）。anchor 只声明接口，M5-7 落地。

---

## 6. 门禁与验收

### 6.1 门 ⑦（路由对齐）——预测值与本波目标

`bash scripts/gates.sh` 的第 ⑦ 项（`route_parity.py` / `slash_alias_audit.py`）。

| 时点 | local（注册键） | implemented（含 placeholder） | known_gap | 说明 |
| --- | ---: | ---: | ---: | --- |
| 现在 | 292 | 235（real 231 + placeholder 4） | 221 | 本表的「现在」行是**本文档落笔轮的实测**（`bash scripts/gates.sh` **8/8 绿**，232s；
⑨ 报告与 `crates/mc-conformance/report.json` 字节一致）；其余行是预测，**实现时一律重测**。
placeholder 的 4 个具体键已核对：`GET|POST /api/autopilots/` 与 `GET|POST /api/skills/`。
| M5-0 后 | **290** | **233**（real 231 + placeholder 2） | **223** | 删 2 个 autopilot 占位；7 条 wakeup 501 仍误计在 real 里 |
| M5-6 后 | 290 | 233 | 223 | 7 条误计**不变**（本波不修检测器，见 R3），但**变成真实现** |
| M5-1..M5-5 后 | **319** | **255**（real 253 + placeholder 2） | **201** | +28 autopilot/webhook 注册键（21 路由 + 7 别名）+1 summaries |
| M5-INT | 刷新基线 → 319（另：`route-parity-baseline.json` 当前记 242，**它是「曾经注册过」的记忆**，与 `local`/`implemented` 三个数字不同义，刷新时不要混用） | 255 | 201 | ⑦ 基线文件与读数必须同轮一致 |

算术自洽：`implemented + known_gap = 456`（235+221 / 233+223 / 255+201）。
**形态门**：M5 各片合入后，`slash_alias_audit.py` 的 M5 相关 `MISSING_ALIAS` 必须为 0，
且 anchor 已删掉 `slash-alias-allowlist.tsv` 里那 2 行 M5（剩 `GET|POST /api/skills` 2 行给 M6）。

### 6.2 门 ⑨（契约等价）——现有 8 条 fixture **不够**（本 issue 必答项）

现状：`contracts/golden/autopilots/` 8 条，**全部 `unevaluable`**（member actor 需真库种子）：

| fixture | 路由 | 来源 |
| --- | --- | --- |
| 001–005 | `GET /api/autopilots` | `autopilot_list_test.go:77/132/149/194/293`（direct_handler） |
| 006 | `GET /api/autopilots/usage` | `autopilot_quota_handler_test.go:76` |
| 007 | `POST /api/autopilots` | `handler_test.go:1667`（`testutil.Call`） |
| 008 | `PUT /api/autopilots/not-a-uuid` | `handler_test.go:1675` — **method 不匹配上游**（上游是 `PATCH`）⇒ 永久 `unmounted` |

结论：**不够，且差距很大**。理由三条：

1. **覆盖**：8 条只碰 2/29 条 M5 路由（`GET /api/autopilots` 5 条、`GET /api/autopilots/usage` 1 条，
   另两条是 POST/负例）。**27 条路由零 fixture**，其中包含全部写面、全部凭据面、webhook 入口、runs/deliveries。
2. **抽取漏斗**：M5 域上游测试站点实测 **158 站点 / 24 文件**，只抽出 6 条（+2 条来自 `handler_test.go`）：
   129 条 `skipped`（`body_unresolved` 36 / `value_unresolved` 34 / `no_status_assertion` 31 /
   `request_var_unresolved` 18 / `ambiguous_status` 5 / `path_not_registered` 4 / `path_not_literal` 1）
   + 23 条 `helper_site`。⇒ 想靠「重跑抽取」补满 M5 是不现实的（**抽取器的天花板已经量过**）。
3. **008 是抽取产物缺陷**，不是「待实现的缺口」：处理口径 = 保留、标 `expected_unmounted` +
   原因（method 与上游路由表不符），**不许**为了让数字好看而删 fixture 或加 `PUT` 路由。

**补救口径（写进各片 DoD）**：M5 的等价证据靠 **本地 e2e**（路由级 + 真库），不靠 fixture 数量：
（注：⑨ 的 `--no-db` 报告里 `offline_decidable` 只有 **59 / 365** 条，其中 pass 5 ⇒ 本波即使全绿，
契约率也只能靠真库模式提升，这也说明「等 ⑨ 涨上来」不是 M5 的验收路径。）

- 路由级 `crates/mc-http/tests/autopilots/**`（无 DB，断言 401/403/404/405/双形态与 501 消失）；
- 真库 `crates/mc-repos/src/autopilot/tests/**`（事务、幂等、并发策略、quota 预留/结算）；
- 调度器 `crates/mc-scheduler/tests/**`（租约：并发 claim 只一个成功 / 租约被偷后写终态匹配 0 行 /
  stale 转 FAILED / retry 退避）+ 一条真库 lease 测试；
- ⑨ 那 8 条在 `--with-db` 模式下**必须从 `unevaluable` 变成 pass 或 mismatch**（不许还是 unevaluable）；
- **通过数必须以当轮 gate 日志的 `grep` 为准**（教训：`7be91a6` 的提交信息写「7 条 e2e」实为 5 条）。

### 6.3 门 ⑩（文件大小）——预飞：本波哪些文件会撞 800 行

`scripts/file_size_check.py` + 门 ⑩。anchor 一次性把**会被撞的文件拆好**，避免切片中途重拆：

| 落地文件 | 上游行数 | 预估 Rust 行 | 判定 / 动作 |
| --- | ---: | ---: | --- |
| `mc-http/…/autopilots/crud.rs` | 483（create+update+delete） | ~650 | 拆出 `subscribers.rs`(183) / `assignee.rs`(135) ⇒ **anchor 已拆** |
| `mc-http/…/autopilots/trigger.rs` | 448（create+update+delete） | ~600 | 凭据两条拆到 `credentials.rs`(168) |
| `mc-autopilot/src/dispatch/*.rs` | ~1,500 | ~1,900 | 拆 6 文件（§5.2）：单文件最大 `create_issue.rs` ≈ 300 |
| `mc-autopilot/src/webhook/*.rs` | 1,010 | ~1,300 | 拆 5 文件：`mod.rs`(ingress) ≈ 400 |
| `mc-autopilot/src/wakeup/service.rs` | 831 | ~1,050 | 拆 `mod.rs`(Validate/save) ≈ 550 + `dispatch.rs`(Tick/dispatch) ≈ 450 |
| `mc-scheduler/src/manager.rs` | 490 | ~600 | 单文件，OK |
| `mc-scheduler/src/db_ops.rs` | 403 | ~520 | 单文件，OK |
| `mc-repos/src/autopilot/mod.rs` | —— | ~450 | 行类型 + 共享 SELECT，OK |
| `mc-core/src/autopilot.rs` | —— | ~340 | OK（重写后） |
| 既有文件改动 | —— | —— | `routes/issues/mod.rs` 234 → ~216；`mount.rs` 237 → ~226；`routes/mod.rs` 59 → 63 |

**预飞结论**：本波**不需要**在切片中途重拆文件（前提是 anchor 按上表建骨架）；
唯一需要切片自己盯的是 `mc-autopilot/src/dispatch/*` 与 `webhook/*`（M5-4 / M5-5 的第一动作应是
`wc -l` 自查，而不是最后才发现）。

### 6.4 每片 DoD（通用 + 专属）

**通用**（每片交付前必须贴出命令与输出片段）：

1. `bash scripts/gates.sh` **全绿**；有路由/迁移/DB 变更的片追加 `--with-db` 10/10（真库）。
2. `PATH="$HOME/.cargo/bin:$PATH" cargo fmt --all --check` + `cargo clippy --all-targets -- -D warnings`。
3. 门 ⑩ 不新增超限文件；⑦ 增量与 §6.1 表一致（形态 0 defect）。
4. 每片自带 e2e（§6.2 的三类之一以上），**不允许 0 测试**。
5. PR 目标分支 = `feat/multica-rs-initial`；提交信息里的数字必须来自当轮日志（不沿用自述）。

**专属**：M5-1 cron-preview 的 5 字段语义 + 无下次触发用例；M5-2 三态补丁 + squad 指派；
M5-3 **响应/日志不含完整 secret**（C4）；M5-4 幂等（同 plan 二次 dispatch 不产生第二条 run）+
并发策略三态；M5-5 无效 token / 超限 429 / 重复 dedupe_key 幂等三条；M5-6 revision 语义 +
receipt 失效；M5-7 租约四条（§6.2）；M5-8 plan_time 分桶与过期 plan。

---

## 7. 晋升顺序（M5 可以立刻派发的前提）

1. **M4 收口**：`LUM-1475`（M4-4）与 `LUM-1476`（M4-INT）落地。M5 **技术上**不依赖它们
   （本波只用 M2 的 issue/project 类型与 M3 的任务队列），但 M4-INT 会刷新 ⑦/⑨ 与 `docs/43`，
   M5-0 也要刷同样的基线 ⇒ **顺序执行避免两次基线互相覆盖**。
2. **M5-0 单独先合**（它同时改 `mount.rs` / `issues/mod.rs` / 基线，是全波唯一的共享写者）。
3. **B 波**：`LUM-1506`（ws 收口）与 `LUM-1440`（execenv）是 M4 的在飞片，与 M5 无交集；
   M5-1 ∥ M5-6 ∥ M5-7 可立即占满 3 个并发位。
4. **C 波**（M5-2 ∥ M5-3 ∥ M5-4）在 B 波合完后派；**D 波**（M5-5 ∥ M5-8）在 C 波合完后派；
   最后 M5-INT。
5. **派发前必量磁盘**：一次冷构建峰值 ≈7.4G（本机口径），<12G 可用不派；
   回收判据 = run 终态 + `readlink /proc/*/cwd` 无进程 + 只删 `target/`。
6. **backlog 子 issue 的建立顺序** = M5-0~M5-8 + M5-INT（10 条），`--status backlog` 停放，
   按波次逐条 `backlog → todo`（一次最多 3 条）。

### 7.1 已建子 issue（stage 与 §4.3 波次一一对应，全部 `backlog`）

| stage（波） | issue | 片 |
| --- | --- | --- |
| 1（A） | `LUM-1563` | M5-0 anchor |
| 2（B） | `LUM-1564` / `LUM-1565` / `LUM-1566` | M5-1 / M5-6 / M5-7 |
| 3（C） | `LUM-1567` / `LUM-1568` / `LUM-1569` | M5-2 / M5-3 / M5-4 |
| 4（D） | `LUM-1570` / `LUM-1571` | M5-5 / M5-8 |
| 5（E） | `LUM-1572` | M5-INT |

子 issue 的 parent 是本计划 issue（`LUM-1561`）。阶段屏障（stage barrier）在**整段子 issue 终态**时唤醒 parent
⇒ 编排 cycle 只需在每个 stage 完成后把下一段 `backlog → todo` 即可，不必逐片盯。

---

## 8. 风险登记（每条都对应一个切片的 DoD 或一个「登记不实现」的决定）

| # | 风险 | 处置 |
| --- | --- | --- |
| **R10** | `mc-core` 两个 stub 与迁移列大面积不符；沿用即写错实现 | M5-0 **重写**（§5.1 逐字段对照）；各片 DoD 要求「类型来自迁移列，不得来自旧 stub」 |
| **R3** | 7 条 wakeup 501 被 ⑦ 记成 `implemented_real`（检测器只认 `\bplaceholder\b`） | 本波**不修** `route_parity.py`（改检测器会动门禁语义，属独立 issue `LUM-1580`）；M5-6 落地后自动成真；汇报时扣掉。**全仓口径是 13 条**（M5 7 + M2-A 3 + M8/M9/M3+ 各 1，base 逐键实测见 `docs/37` §32.4）——讲全仓进度时扣 13，讲本波时扣 7。**已修（`LUM-1580`，2026-09-24）**：正则改为同时识别 `placeholder` 与 `not_implemented`，口径见 `docs/22` §2.3、当轮读数见 §3.6 |
| **R1** | 凭据（webhook token / signing secret）泄漏到响应或日志 | C4：走 `mc-telemetry` redaction；M5-3/M5-5 DoD 各加一条「不含完整 secret」断言 |
| **R5** | `/api/webhooks/autopilots/{token}` 是**无认证**入口：token 爆破、签名伪造、重放、大 body | M5-5 必须落地限流 + 签名校验 + dedupe 幂等 + body 上限；三条 e2e |
| **R2** | 调度器 exactly-once：并发 claim、租约被偷、stale、retry 退避 | M5-7 四条租约测试（§6.2）；`sys_cron_executions.lease_token` 轮换语义不得简化 |
| **R7** | quota 依赖 `entitlement.Gate`（cloud 订阅面，M9） | `QuotaEnabled()` 走「无 entitlement 平面 ⇒ quota 关闭」的等价分支；gate 留成 trait 供 M9 接；登记跨波依赖 |
| **R6** | cron 5 字段语义 + IANA 时区 + plan_time 取整 | anchor 实测后一次性引入 `cron` / `chrono-tz`（§5.4），只动一次 `Cargo.lock` |
| **R8** | `publishRunDone` / `broadcastAutopilotTriggerResponse` 需要 realtime 广播 | 复用 `mc-realtime::RealtimeHandle`（`main.rs` 已 start）；频道命名先与 M4 chat 广播口径对齐，避免第二套命名 |
| **R9** | dispatch 的终态回写依赖任务队列被真正执行（daemon 面） | M5-4 边界：发现 queued 行不执行 ⇒ **登记跨波缺口**，不在 M5 实现 daemon |
| **R4** | `routes/issues/mod.rs` / `mount.rs` 是 M1/M2 的落地文件，M5 也要动 | 只在 M5-0 动一次（删 501 + 挂子 router），之后 M5 切片不再碰；后续切片要动需 rebase 仲裁 |
| **R11** | 上游 `webhook_delivery` 表同时服务 deliveries 读面（M5-4）与 ingress（M5-5） | 按 §3.2 拆两个文件（`run/delivery.rs` vs `ingress.rs`）分离写权；表级语义耦合在 §4.2 登记 |
| **R12** | 上游 `notifyAutopilotSubscribersOnCreate`(91) + `autopilot_notification_recipient.go`(91) 涉及通知面 | 本波只做「订阅者解析 + 收件人解析 + 落库/inbox 项」；**实际投递**（邮件/IM）属 M7/M9，登记不在本波 |

---

## 9. 与 `docs/plan1.md` §5 / §8 的差异（必须修订项）

### 9.1 口径修订

1. **W5 的代码量**：plan1 §5 的行 471 只写了「autopilot(20) + wakeup + cron + analytics ←
   `internal/scheduler`」；本 issue 描述的 5.6k = handler+handler+scheduler 的算法。
   实测 **9,546 行**（+1,190 SQL），差异来源见 §1.2。**切片划分按 9,546 行做**，
   W5 工期在 plan1 §3.3（「3 周」）上按此口径重估（本波切成 10 片、5 波）。
2. **W5 的 crate 表**：plan1 §3.3 行 314 写 `mc-autopilot`(wakeup/cron) `mc-analytics`。
   修订为：`mc-autopilot`（本波建）+ **`mc-scheduler`**（plan1 没列；调度内核是 M5/M6/M9 共享面，
   必须独立成 crate，否则 M6 的 plugin hook job 要依赖 M9 或 M5 的 crate 内部）；
   `mc-analytics` **本波不建**：W5 的 "analytics" 实际落在 quota/usage（M5-1）+ 调度 job（M5-8），
   而真正的分析面（`/api/dashboard` 等）属 W9 `mc-dashboard`（plan1 行 314 的 W9 已列）⇒ 登记口径修订。
3. **W5 的依赖**：plan1 §6 的甘特把 W5 标成 `after w4`。实测 M5 **不依赖 M4 的任何代码**
   （只用 M2 issue/project 类型 + M3 任务队列）；`after w4` 是**并发位与基线的排期约束**，
   不是技术依赖。登记以免后续以为「M4 不完工 M5 动不了」。
4. **调度内核的跨波归属**：`jobs_plugin_hook.go`(353, M6) 与 `jobs_task_usage.go`(120, M3/M9)
   在本波只交付**内核**，不交付这两个 job ⇒ 这两波的切片必须知道「内核已在 M5 备好」。

### 9.2 与 `docs/42-M4-PLAN.md` 的结构一致性

本文档沿用 docs/42 的章节骨架（§0 结论速览 → §1 测绘 → §2 落点取舍 → §3 写集 → §4 切片 →
§5 anchor → §6 门禁 → §7 晋升 → §8 风险 → §9 差异 → §10 复算命令），并把 docs/42 §6 的
「⑦ 预测表 / ⑨ 补救口径 / ⑩ 预飞」三项原样保留（M4 的教训：⑨ 的 fixture 数不能当等价证据）。

---

## 10. 复算命令（全部只读，可在任意 workdir 复现）

```bash
# 1. 路由表：本计划 §1.1 与上游 owner=M5 行集合相等（应无输出）
awk -F'\t' '!/^#/ && $3=="M5"{print $1"\t"$2}' docs/fixtures/upstream-routes.tsv | sort \
  | diff - <(grep -v '^#' docs/fixtures/m5-declared-routes.tsv | tail -n +2 | sort)

# 2. 形态门（预测模式：期望 "FAIL: 7 …"，即 7 键需双形态；exit 1 是预期）
#    注意前置条件：**anchor 删掉 2 行 M5 allowlist 之前**实测是 "FAIL: 5"（7 键需双形态，其中 2 键在
#    docs/fixtures/slash-alias-allowlist.tsv 里被豁免）；删掉后才变 7。见 docs/37 §32.3 命令 2。
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m5-declared-routes.tsv

# 3. 本地形态现状（0 defect；M5 的 2 行 allowlist 在 anchor 后应消失）
python3 scripts/slash_alias_audit.py

# 4. ⑦ 读数（local / implemented / known_gap / owners.M5；implemented+known_gap 必须 = 456）
python3 scripts/route_parity.py --json | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["counts"])'

# 5. 上游行数（§1.2 表；需要 /tmp/ups_multica @ 90e0bdf）
cd /tmp/ups_multica/server && wc -l internal/handler/{autopilot,autopilot_webhook,webhook_delivery,issue_wakeup,wakeup_actor,autopilot_cron_preview}.go \
  internal/service/{autopilot,issue_wakeup,autopilot_quota,autopilot_quota_notifications,issue_wakeup_evidence,autopilot_notification_recipient,cron}.go \
  internal/scheduler/{spec,manager,db_ops,jobs_autopilot,jobs_issue_wakeup}.go

# 5b. 上游版本口径：路由表的 commit 与实测 commit 上，M5 文件是否一致（应逐行 same / 空 diff）
cd /tmp/ups_multica && timeout 90 git fetch --depth=1 origin f41fae6b08fb734afcbd13205c0b3203dd0bc9c6
for f in internal/handler/{autopilot,autopilot_webhook,webhook_delivery,issue_wakeup,wakeup_actor,autopilot_cron_preview}.go; do
  diff <(git show HEAD:server/$f) <(git show FETCH_HEAD:server/$f) >/dev/null && echo "same $f" || echo "DIFF $f"; done
diff <(git show HEAD:server/cmd/server/router.go) <(git show FETCH_HEAD:server/cmd/server/router.go)   # ⇒ 空

# 6. 12 张表是否都在（应全部有输出；本波 0 新迁移）
cd - >/dev/null && grep -l 'CREATE TABLE' migrations/upstream/{042_autopilot,093_webhook_deliveries,113_sys_cron_executions,120_autopilot_subscriber,128_autopilot_collaborator,186_autopilot_rule_version,352_autopilot_quota_execution,509_issue_wakeup}.up.sql

# 7. ⑨ 契约（autopilot 域 8 条；--with-db 模式下这 8 条不得再有 unevaluable）
PATH="$HOME/.cargo/bin:$PATH" cargo run -q -p mc-conformance -- --no-db --filter autopilots --list
PATH="$HOME/.cargo/bin:$PATH" cargo run -q -p mc-conformance -- --no-db --filter autopilots --json \
  | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["totals"])'
# 真库模式（这 8 条必须离开 unevaluable；数值以当轮输出为准，不从本文档转抄）
PATH="$HOME/.cargo/bin:$PATH" cargo run -q -p mc-conformance -- --filter autopilots \
  --db-url "$MULTICA_TEST_DATABASE_URL" --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["totals"])'

# 8. 全量门禁（切片交付前；有 DB 变更的片追加 --with-db）
bash scripts/gates.sh            # 期望全绿
bash scripts/gates.sh --with-db  # 期望 10/10
```

> **数字纪律**（M4 两轮踩过）：本文档所有「上游行数 / span」都可用命令 5 复算；
> ⑦ 的读数一律以**当轮** `gates.sh` 日志为准，不从本文档或切片自述转抄。

---

## 11. M5-INT 落地记录（`LUM-1572`，起手 base `0fd96b4` → 真合到 `e01c73a`）

详见 **`docs/56-M5-INTEGRATION.md`**（本号段原计划的 `docs/45` 已被 `45-M4-4-CHAT-DISPATCH.md` 占用 ⇒ 改用 `56`）。

**逐片 PR / 合并提交**（全部入 base，`owners.M5 = 0`）：

| 片 | issue | PR | merge |
| --- | --- | --- | --- |
| M5-0 | `LUM-1563` | #50 | `e07e0f2` |
| M5-1 | `LUM-1564` | #55 | `76db3eb` |
| M5-6 | `LUM-1565` | #54 | `5b4f407` |
| M5-7 | `LUM-1566` | #52 | `659f19e` |
| M5-2 | `LUM-1567` | #56 | `415f194` |
| M5-3 | `LUM-1568` | #57 | `22d7135` |
| M5-4 | `LUM-1569` | #58 | `f80ad18` |
| M5-8 | `LUM-1571` | #60 | `eed6969` |
| M5-5 | `LUM-1570` | #62 | `0fd96b4` |
| **M5-INT** | **`LUM-1572`** | 本片 | — |

**⑦ 实测（本轮 `gates.sh --with-db` 日志，10/10 PASS；冷 target 448s / 热 target 86s、79s）**：
`upstream 456 | local 329 registered | baseline 300 → 329（+29 / −0）`；
`implemented 263 real + 2 placeholder = 265 / 456  known_gap 191  unclaimed 0  regression 0  local_only 11`；
`implemented + known_gap = 456`、`owners.M5 = 0`、⑩ 0 违规、`slash_alias_audit` 0 defect（allowlist 仅剩 M6 那 2 行）。

**与 §6.1 预测的差异（逐项，原因已定位）**：预测 `local 319 / implemented 255 / known_gap 201`，实测 **329 / 265 / 191** ——
差额 `+10 / +10 / −10` **全部**来自 **M4-4（PR #51，merge `542e833`）的 10 条 chat 键**（它晚于 M5-0 锚点合入，基线由 M4-INT `015ff2f` 补收 290→300），**不是 M5 超交**：
锚点实测（`e07e0f2` 只读 worktree）= `290 / 233（real 231 + placeholder 2）/ 223`，与 §6.1 的「M5-0 后」行**逐字一致**。
§6.1 的「刷新基线 → 319」因此按实测改为 **329**（刷新后 `baseline == local`）。

**⑨ 复核（本 issue 必答项，§6.2）**：真库模式下 autopilot 域 8 条 **`unevaluable` 0**（6 pass / 1 mismatch / 1 unmounted，契约率 6/8 = 75%，已接入路由率 6/7 = 85.7%）；
005（`TestAutopilotSubscriberReadFailureFailsClosed`）是 `direct_handler` + 故障注入站点，router+真库重放不可达 ⇒ 记 mismatch + 原因（实现侧 fail-closed 已落地）；
008（`PUT /api/autopilots/not-a-uuid`）按 §6.2 口径**保留、不加 `PUT` 路由**，harness 记 `unmounted`；
`--no-db` 模式仍是 8/8 `unevaluable`（结构上判不了，非回归）。`crates/mc-conformance/report.json` 无 diff。

**跨波缺口（本片集中登记，详见 `docs/56` §7）**：R7 entitlement 平面（`QuotaPolicyProvider` 已留口，生产实现归 M9）、
R9 daemon 执行面（`queued` 行仍无执行者）、R8 realtime 广播口径（唯一发点是 M5-4 的 `resource="workspace"` + `autopilot:run_*`，CRUD 事件未接）、
调度内核共享给 M6/M9（`register_all(manager, &JobPorts)` 需要两个生产端口实现 —— §7.1 的旧代码段已过期）、
**P0 = `apps/mc-server/Cargo.toml` 缺 `mc-scheduler` 边 + 门 ⑥ 未收集 `-p mc-scheduler`（本片按令不接线，归 `LUM-1659`）**，
webhook worker 轮询循环（`docs/54` §6.1）由本片**裁定归 `LUM-1659`**（备选 M6-9）。

**本片写集**：`docs/fixtures/route-parity-baseline.json`（刷新）+ `docs/56-M5-INTEGRATION.md`（新增）+ 本节；未碰任何 `crates/**`、`Cargo.toml`/`Cargo.lock`、`migrations/**`、`scripts/**`。
