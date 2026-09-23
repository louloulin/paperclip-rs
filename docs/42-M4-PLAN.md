# docs/42-M4-PLAN.md — W4（组织与对话：chat / project / squad）切片计划

**依据**：`docs/plan1.md` §5 W4（chat 25 + project 10 + squad 10，4 周，验收门「域内 fixture 通过」）、
§3.3（crate 波次归属）、§6.1（并发切片模型）、§6.4（门禁清单）。
**写就基线**：本仓 trunk `feat/multica-rs-initial` @ `a51d523`（2026-09-23 08:30 cycle / LUM-1468）。
**上游冻结**：`f41fae6b08fb`（与 `docs/fixtures/upstream-routes.tsv`、`contracts/upstream-schema.sql` 同源；
`docs/37` §5.5 里写的 `90e0bdf` 是错的，见 `docs/37` §18 复核记录）。
**本文件性质**：立项文档，**不含任何代码/迁移改动**；M4 的实现由 §4 的切片各自完成。

---

## 0. 结论速览

| 项 | 实测值 | 来源 |
| --- | --- | --- |
| M4 路由 | **45**（chat 25 / projects 10 / squads 10），占上游 456 的 9.9% | `docs/fixtures/upstream-routes.tsv` owner 列 |
| 本地现状 | **0 条真实实现**；其中 **6 条**由 M0 的 501 占位"满足" | `python3 scripts/route_parity.py` |
| ⑦ 未实现（owner=M4） | **39** 条 | 同上（`owners.M4`） |
| 上游 handler 非测试 LOC | **≈ 6.6k**（chat 2988 / project 2023 / squad 1611）+ service 811 | §1.2 |
| SQL 查询面 | 7 个查询文件 / **137** 条 query / ≈ 2.2k 行（`chat.sql` 独占 77 条） | §1.3 |
| 需要的表 | **11 张，全部已在 `migrations/upstream/`** ⇒ **0 新建迁移** | §2 |
| ⑨ 契约证据 | M4 域现有 fixture **3 条且全 `unevaluable`**，可用 = **0**（抽取报告 19 个候选站点全 skip） | `contracts/golden/extraction-report.tsv`、`crates/mc-conformance/report.json` |
| 切片 | M4-0 anchor（0 路由）→ M4-0b 抽取器 I4（0 路由）→ M4-1 project(10) ∥ M4-2 squad(10) ∥ M4-3 chat 会话/消息(15) → M4-4 chat 任务/生成(10) → M4-INT 集成 | §4 |
| ⑦ 预测（落地后） | registered `195→189`（anchor 删 6 占位）`→234`（M4 全落）；implemented `162→201`（197 real + 4 placeholder）；known_gap `294→255` | §3.3 |
| 文档编号 | 本文件 `42`；M4 集成 `docs/43-M4-INTEGRATION.md`（`a51d523` 时 38–41 已占用，42/43 空） | §9 |
| 不碰 | `migrations/`、`scripts/`、根 `Cargo.toml`、`state.rs`（预计） | §6 |

**一句话**：M4 是"零依赖冲突、纯增量"的一波——表齐、crate 可空建、路由全新增（除 6 条占位原地替换），
但**没有一条现成 fixture 可回放**，所以 `plan1` W4 的验收门必须先补一个抽取器切片（M4-0b），否则"域内 fixture 通过"是一条无法执行的门。

---

## 1. 上游 M4 面测绘

### 1.1 45 条路由逐条（`router.go` 行号 → handler）

owner 列取自 `scripts/route-owners.tsv`（M4 = `/api/chat*` + `/api/projects*` + `/api/squads*`）。

#### chat（25 条，`router.go` L2334–L2374）

| # | 方法 | 路径 | 行 | handler | 切片 |
| --- | --- | --- | ---: | --- | --- |
| 1 | POST | `/api/chat/sessions/` | 2335 | `CreateChatSession` | M4-3 |
| 2 | GET | `/api/chat/sessions/` | 2336 | `ListChatSessions` | M4-3 |
| 3 | GET | `/api/chat/sessions/{sessionId}/` | 2338 | `GetChatSession` | M4-3 |
| 4 | PATCH | `/api/chat/sessions/{sessionId}/` | 2339 | `UpdateChatSession` | M4-3 |
| 5 | PATCH | `/api/chat/sessions/{sessionId}/pin` | 2340 | `SetChatSessionPinned` | M4-3 |
| 6 | PATCH | `/api/chat/sessions/{sessionId}/archive` | 2341 | `SetChatSessionArchived` | M4-3 |
| 7 | DELETE | `/api/chat/sessions/{sessionId}/` | 2342 | `DeleteChatSession` | M4-3 |
| 8 | POST | `/api/chat/sessions/{sessionId}/messages` | 2343 | `SendChatMessage` | M4-4 |
| 9 | POST | `/api/chat/sessions/{sessionId}/onboarding` | 2344 | `StartMikaOnboarding` | M4-4 |
| 10 | POST | `/api/chat/sessions/{sessionId}/quick-actions/regenerate` | 2347 | `RegenerateChatQuickActions` | M4-4 |
| 11 | GET | `/api/chat/sessions/{sessionId}/messages` | 2348 | `ListChatMessages` | M4-3 |
| 12 | GET | `/api/chat/sessions/{sessionId}/messages/page` | 2349 | `ListChatMessagesPage` | M4-3 |
| 13 | GET | `/api/chat/sessions/{sessionId}/pending-task` | 2350 | `GetPendingChatTask` | M4-4 |
| 14 | DELETE | `/api/chat/sessions/{sessionId}/queued-tasks` | 2351 | `ClearQueuedChatTasks` | M4-4 |
| 15 | POST | `/api/chat/sessions/{sessionId}/queued-tasks/{taskId}/prioritize` | 2352 | `PrioritizeQueuedChatTask` | M4-4 |
| 16 | POST | `/api/chat/sessions/{sessionId}/read` | 2353 | `MarkChatSessionRead` | M4-3 |
| 17 | GET | `/api/chat/sessions/{sessionId}/draft-restores` | 2356 | `ListChatDraftRestores` | M4-3 |
| 18 | DELETE | `/api/chat/sessions/{sessionId}/draft-restores/{restoreId}` | 2357 | `ConsumeChatDraftRestore` | M4-3 |
| 19 | GET | `/api/chat/pending-tasks` | 2360 | `ListPendingChatTasks` | M4-4 |
| 20 | GET | `/api/chat/pending-tasks/has-any` | 2361 | `HasPendingChatTasks` | M4-4 |
| 21 | GET | `/api/chat/pinned-agents` | 2364 | `ListChatPinnedAgents` | M4-3 |
| 22 | POST | `/api/chat/pinned-agents` | 2365 | `PinChatAgent` | M4-3 |
| 23 | DELETE | `/api/chat/pinned-agents/{agentId}` | 2366 | `UnpinChatAgent` | M4-3 |
| 24 | GET | `/api/chat/history` | 2373 | `GetChatChannelHistory` | M4-4 |
| 25 | GET | `/api/chat/thread` | 2374 | `GetChatThread` | M4-4 |

#### projects（10 条，`router.go` L2064–L2078）

| # | 方法 | 路径 | 行 | handler | 切片 |
| --- | --- | --- | ---: | --- | --- |
| 26 | GET | `/api/projects/search` | 2066 | `SearchProjects` | M4-1 |
| 27 | GET | `/api/projects/` | 2067 | `ListProjects` | M4-1 |
| 28 | POST | `/api/projects/` | 2068 | `CreateProject` | M4-1 |
| 29 | GET | `/api/projects/{id}/` | 2070 | `GetProject` | M4-1 |
| 30 | PUT | `/api/projects/{id}/` | 2071 | `UpdateProject` | M4-1 |
| 31 | DELETE | `/api/projects/{id}/` | 2072 | `DeleteProject` | M4-1 |
| 32 | GET | `/api/projects/{id}/resources` | 2073 | `ListProjectResources` | M4-1 |
| 33 | POST | `/api/projects/{id}/resources` | 2074 | `CreateProjectResource` | M4-1 |
| 34 | PUT | `/api/projects/{id}/resources/{resourceId}` | 2075 | `UpdateProjectResource` | M4-1 |
| 35 | DELETE | `/api/projects/{id}/resources/{resourceId}` | 2076 | `DeleteProjectResource` | M4-1 |

#### squads（10 条，`router.go` L2081–L2093）

| # | 方法 | 路径 | 行 | handler | 切片 |
| --- | --- | --- | ---: | --- | --- |
| 36 | GET | `/api/squads/` | 2082 | `ListSquads` | M4-2 |
| 37 | POST | `/api/squads/` | 2083 | `CreateSquad` | M4-2 |
| 38 | GET | `/api/squads/{id}/` | 2085 | `GetSquad` | M4-2 |
| 39 | PUT | `/api/squads/{id}/` | 2086 | `UpdateSquad` | M4-2 |
| 40 | DELETE | `/api/squads/{id}/` | 2087 | `DeleteSquad` | M4-2 |
| 41 | GET | `/api/squads/{id}/members` | 2088 | `ListSquadMembers` | M4-2 |
| 42 | GET | `/api/squads/{id}/members/status` | 2089 | `ListSquadMemberStatus` | M4-2 |
| 43 | POST | `/api/squads/{id}/members` | 2090 | `AddSquadMember` | M4-2 |
| 44 | DELETE | `/api/squads/{id}/members` | 2091 | `RemoveSquadMember` | M4-2 |
| 45 | PATCH | `/api/squads/{id}/members/role` | 2092 | `UpdateSquadMemberRole` | M4-2 |

> **形态纪律（本波最容易踩的一条）**：上表路径**逐字照抄**，含尾斜杠（`/api/projects/`、`/api/squads/{id}/`、
> `/api/chat/sessions/`）与**无**尾斜杠（`/api/chat/pending-tasks`、`/api/projects/search`）。
> 本仓 `mount.rs` L49/L65/L69 的 6 条 M0 占位用的是**无尾斜杠**形态（`/api/chat/sessions`），
> 与上游的**带尾斜杠**形态（`/api/chat/sessions/`）在 axum 里是**两个不同的注册键**
> ⇒ ① 不会 panic（真路由用带斜杠形态注册即可并存）；② 但占位会**永久留下 501 幽灵路由**，
> 而 ⑦ 的 `slash_aliases()` 折叠又会把它算成"已实现"，从报表上看不出来（`docs/15` §7.3 已记录这个陷阱）。
> **唯一的 panic 场景**是切片把真路由写成**无尾斜杠**形态 —— 那就与遗留占位同键、启动即 panic（`docs/15` §9.6.6）。
> 处置见 §5.2（anchor 预删 6 行 + 刷基线）。`/api/projects/search` 与 `/api/projects/{id}/` 不同键，无冲突。
> 路径参数一律写 `:id`（matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404，`docs/09` §7.4）。

### 1.2 handler / service 规模（上游 `f41fae6b`，非测试行数）

| 文件 | 行 | 域 |
| --- | ---: | --- |
| `server/internal/handler/chat.go` | 2105 | chat |
| `server/internal/handler/chat_history.go` | 418 | chat（history/thread） |
| `server/internal/handler/chat_title.go` | 296 | chat（标题异步生成） |
| `server/internal/handler/chat_pinned_agent.go` | 169 | chat |
| `server/internal/handler/squad.go` | 1243 | squad |
| `server/internal/handler/squad_briefing.go` | 368 | squad（briefing，路由在别处） |
| `server/internal/handler/project_resource.go` | 1061 | project |
| `server/internal/handler/project.go` | 962 | project |
| `server/internal/service/chat_quick_actions_generate.go` | 497 | chat（quick-actions 生成，走 daemon） |
| `server/internal/service/chat_quick_actions.go` | 293 | chat（quick-actions 建议） |
| `server/internal/service/squad_no_action.go` | 21 | squad |

**handler 合计 ≈ 6.6k 行；service 合计 811 行。** 对照本仓 §6.4 的 800 行/文件硬上限
（门 ⑩ `scripts/file_size_check.py`，`scripts/file_size_baseline.tsv` 只允许变短、新代码不得进基线）：
**M4 每个域都必须按子域拆文件**，照 M3-5/M3-6 的 `routes/agents/*`、`routes/tasks/*` 目录化写法。
单文件预算见 §4.2。

### 1.3 SQL 查询面（`server/pkg/db/queries/`，sqlc 源）

| 查询文件 | 行 | query 数 | 涉及表（`FROM/INTO/UPDATE/JOIN` 去重） |
| --- | ---: | ---: | --- |
| `chat.sql` | 1660 | 77 | `chat_session` `chat_message` `chat_draft_restore` `agent` `agent_builder_draft` `agent_task_queue` `channel_chat_context_generation` |
| `chat_pinned_agent.sql` | 34 | 6 | `chat_pinned_agent` |
| `quick_action.sql` | 77 | 8 | `quick_action` |
| `task_message.sql` | 98 | 5 | `task_message` |
| `squad.sql` | 170 | 22 | `squad` `squad_member` `agent` `agent_runtime` `agent_task_queue` `autopilot` `issue` |
| `project.sql` | 64 | 9 | `project` `issue` |
| `project_resource.sql` | 52 | 10 | `project_resource` |
| **合计** | **2155** | **137** | — |

> `chat.sql` 的 1660 行 / 77 条 query 是 M4 的**单点最重负载**：它包含消息分页游标、未读计数、
> 排队位置、draft-restore 的幂等消费、以及"同一 session 内 `latest_visible` / `pending` / `prioritized`"
> 这类带 `LATERAL` 与窗口函数的聚合。**拆文件时不能按行数随便切**——按 §4.2 的模块边界切，
> 每条 query 连同其 Rust 调用点整块搬运（`docs/15` §6 的"一 query 一函数"惯例）。

---

## 2. schema 对账：11 张表全在，M4 不写迁移

`grep -rl 'CREATE TABLE ... <t>' migrations/upstream/` 实测（`a51d523`）：

| 表 | CREATE 所在迁移 | 列数（近似） | 关联切片 |
| --- | --- | ---: | --- |
| `chat_session` | `033_chat.up.sql` | 10 | M4-3 |
| `chat_message` | `033_chat.up.sql` | 6 | M4-3 |
| `chat_draft_restore` | `182_chat_draft_restore.up.sql` | 6 | M4-3 |
| `chat_pinned_agent` | `152_chat_pinned_agent.up.sql` | 6 | M4-3 |
| `project` | `034_projects.up.sql` | 10 | M4-1 |
| `project_resource` | `065_project_resources.up.sql` | 9 | M4-1 |
| `squad` | `084_squad.up.sql` | 8 | M4-2 |
| `squad_member` | `084_squad.up.sql` | 6 | M4-2 |
| `quick_action` | `237_quick_action.up.sql` | 15 | M4-4 |
| `agent_task_queue` | M3 域（`contracts/upstream-schema.sql` 已在） | — | M4-4 只读 |
| `task_message` | M3 域（同上） | — | M4-4 只读 |

**11/11 存在，0 处 `DROP TABLE`** ⇒ 与 `docs/15` §7.4 的纪律一致：**M4 一个迁移文件都不新增**
（`migrations/compat/*` 编号由 W0-B2 独占）。若实现中真发现缺列，**回评该切片**并在 `docs/43` 记录，
不要自己加迁移。

---

## 3. 本地现状与占位账

### 3.1 一句话：M4 是空白域

`crates/` 现有 23 个 crate，**没有** `mc-chat` / `mc-project` / `mc-squad`；
`crates/mc-repos/src/lib.rs` 的 16 个 `pub mod` 里**没有**任何 M4 域模块；
`crates/mc-http/src/routes/` 下**没有** chat/projects/squads 文件。**M4 从零起。**

### 3.2 6 条 501 占位（`mount.rs` L47–L72）

| 占位键（现注册形态，`mount.rs` L49/L53/L57/L61/L65/L69） | 命中上游路由 | 后果 |
| --- | --- | --- |
| `GET /api/chat/sessions`（**无**尾斜杠） | #2 | 恒 501；⑦ 折叠后把它报成 `GET /api/chat/sessions/` 的"已实现" |
| `POST /api/chat/sessions` | #1 | 同上 |
| `GET /api/projects` | #27 | 同上 |
| `POST /api/projects` | #28 | 同上 |
| `GET /api/squads` | #36 | 同上 |
| `POST /api/squads` | #37 | 同上 |

这 6 条同时出现在 `route_parity.py` 的 `implemented`（10 条 placeholder 的一部分）与
⑦ 基线里 ⇒ 它们既是"幽灵路由"又是"假实现"。处置见 §5.2（**anchor 预删**，照 `docs/36` §3 的 W3b 配方）。

### 3.3 ⑦ 口径算术（预测值，复算命令见 §10）

| 阶段 | registered | implemented（real + placeholder） | known_gap |
| --- | ---: | ---: | ---: |
| 现在 `a51d523` | 195 | 162（152 + 10） | 294 |
| M4-0 anchor 后（删 6 占位 + 刷基线） | 189 | 156（152 + 4） | 300 |
| M4 全落（45 条真实路由） | **234** | **201（197 + 4）** | **255** |

> 剩下的 4 条 placeholder = `/api/autopilots/` ×2（M5）+ `/api/skills/` ×2（M6），不归 M4。
> 算术自检：`implemented + known_gap = 456` ✓。

---

## 4. 切片划分（4 片 + 2 个前置 + 1 个集成）

### 4.1 全景

| 切片 | 内容 | 路由 | 依赖 | 优先级 |
| --- | --- | --- | --- | --- |
| **M4-0** | anchor scaffold（§5） | 0 | 无（可直接派） | none |
| **M4-0b** | 抽取器 I4 规则（§6） | 0 | 无（纯 `scripts/` + fixtures） | medium |
| **M4-1** | project + project resources | 10（#26–35） | M2-A（已合） | medium |
| **M4-2** | squad | 10（#36–45） | M3-4/M3-5（已合） | medium |
| **M4-3** | chat 会话 / 消息读面 / 快捷栏 / draft-restore | 15（#1–7,11,12,16–18,21–23） | M4-0；读 `agent` 域 | high |
| **M4-4** | chat 派发与生成面（发消息、排队、quick-actions、onboarding、history/thread） | 10（#8–10,13–15,19,20,24,25） | M4-3 + **M3-3/M3-6 已合** + **M3-7 ws 广播** | high |
| **M4-INT** | 集成 cycle：门禁 + ⑦ 基线一次性刷新 + ⑨ 等价率 + `docs/43` | 0 | M4-1..M4-4 全合 | medium |

### 4.2 写集矩阵（一文件一写者）

**anchor 预建、此后任何切片不再改的文件**（照 `docs/15` §7.1/§7.2 的"预扩展"手法）：

| 文件 | anchor 动作 | 谁读它 |
| --- | --- | --- |
| `crates/mc-chat/` `crates/mc-project/` `crates/mc-squad/`（各 `Cargo.toml` + `src/lib.rs`） | 空 crate + 依赖预声明 + `Cargo.lock` delta | M4-1..4 |
| `crates/mc-repos/src/lib.rs` | 追加 10 行 `pub mod`（§5.1） | 全部 |
| `crates/mc-http/src/routes/mod.rs` | 追加 `pub mod chat; pub mod projects; pub mod squads;` | 全部 |
| `crates/mc-http/src/routes/chat/mod.rs` | 空子模块聚合（`session`/`message`/`bar`/`task`） | M4-3/M4-4 **只写自己的子文件** |
| `crates/mc-http/src/routes/mount.rs` | 3 个 `mount_slice_*` + 3 行 `.merge` + **删 6 条占位** | 全部 |
| `docs/fixtures/route-parity-baseline.json` | anchor 刷到 189（+ ⑨ 快照重生成） | 集成再刷一次 |

**每片独占的写集**：

| 切片 | crate 源 | repos 模块 | routes 文件 | 测试 |
| --- | --- | --- | --- | --- |
| M4-1 | `mc-project/src/*` | `project.rs` `project_resource.rs` | `routes/projects.rs`（>800 行则拆 `routes/projects/*`，anchor 已建 `projects.rs` 首层） | `crates/mc-http/tests/projects*.rs` |
| M4-2 | `mc-squad/src/*` | `squad.rs` | `routes/squads.rs`（同上） | `crates/mc-http/tests/squads*.rs` |
| M4-3 | `mc-chat/src/{session,message,pinned,draft}.rs` | `chat_session.rs` `chat_message.rs` `chat_pinned_agent.rs` `chat_draft_restore.rs` | `routes/chat/{session,message,bar}.rs` | `crates/mc-http/tests/chat*.rs` |
| M4-4 | `mc-chat/src/{task,history,quick_action}.rs` | `chat_task.rs` `chat_history.rs` `chat_quick_action.rs` | `routes/chat/task.rs`（含 history/thread） | `crates/mc-http/tests/chat_task*.rs` |
| M4-0b | — | — | — | `contracts/golden/**` + `extraction-report.tsv` |

> **为什么 chat 拆两片**：`chat.go` 2105 行 + `chat.sql` 1660 行是单片预算的 2–3 倍；且两半的**外部依赖不同**
> —— 读面（会话/消息/快捷栏）只依赖已合的 M2/M3 域，写面（发消息/排队/quick-actions）依赖 task 队列与
> **M3-7 的 `chat:done` / task-queued ws 广播**（上游 `chat.go:1728` `h.TaskService.BroadcastTaskQueued`）。
> 拆开后 M4-3 可以在 M3 收尾期间就开跑，M4-4 卡在 M3-7 合并之后（§7）。
> 若 anchor 预拆后实测两片仍互相踩（例如 `mc-chat/src/lib.rs` 需要改动），**降级为单片 chat**（25 条一次落），
> 在 PR 里说明并同步本表。

### 4.3 M4-4 的三个跨波依赖（必须登记）

1. **task 队列**：`SendChatMessage` → `EnqueueChatTask` → `agent_task_queue`（M3-3 领域层 + M3-6 用户面已合）。
   `pending-tasks` / `pending-task` / `queued-tasks{clear,prioritize}` 全部读写这张表。
2. **ws 广播**：`BroadcastTaskQueued`（`chat.go:1728`）与 `chat:done` 事件属 M3-7 的 notifier。
3. **渠道集成（M7）**：`/api/chat/history` + `/api/chat/thread`（`chat_history.go`）用
   `X-Task-ID` 头 + `GetChannelChatSessionBindingBySessionAny` + `channel.HistoryOptions`（slack/lark 分支）。
   **本波只落"非渠道分支"**（无绑定时按上游 `writeNoChannelIntegration` 的响应），渠道分支随 M7 补齐，
   并在 `docs/43` 的 known_gap 里显式登记。

---

## 5. M4-0：anchor scaffold（一次性，先于一切切片）

照 `docs/15` §7.2（M3-0 / LUM-1406）、`docs/36` §3（W3b 预删 / LUM-1435）的成品配方。

### 5.1 预建清单

1. **三个空 crate**：`mc-chat`、`mc-project`、`mc-squad`（根 manifest 已是 glob members
   `crates/*`/`apps/*` ⇒ **不改根 `Cargo.toml`**）。依赖在 anchor 一次预声明并落 `Cargo.lock`：
   `serde` / `serde_json` / `uuid` / `chrono` / `thiserror` / `tracing` / `sqlx`（如需）+ 内部 `mc-core` / `mc-db` / `mc-errors`。
   **此后 M4 切片不得新增三方依赖**（要加走 `docs/15` §8.4 仲裁，由集成方加）。
2. **10 个 repos 空模块 + 10 行 `pub mod`**（`lib.rs` 只此一次改动）：
   `chat_draft_restore` `chat_history` `chat_message` `chat_pinned_agent` `chat_quick_action` `chat_session` `chat_task`
   `project` `project_resource` `squad`。
3. **路由空切片**：`routes/projects.rs`、`routes/squads.rs`、`routes/chat/mod.rs`（内部 4 个空子模块
   `session.rs` `message.rs` `bar.rs` `task.rs`，各自 `pub fn router() -> Router<Arc<AppState>> { Router::new() }`），
   `routes/mod.rs` 追加 3 行 `pub mod`。
4. **`mount.rs`**：3 个 `mount_slice_{project,squad,chat}()` + 3 行 `.merge(...)`。
5. **不建表、不写迁移、不刷新 ⑦ 基线**（基线刷新是 §5.2 的事，与本条的"加空切片"分开）。

### 5.2 占位预删 + 基线/快照刷新（anchor 第二 commit）

- 删除 `mount.rs` 里 §3.2 的 **6 行占位**（`/api/chat/sessions/`、`/api/projects/`、`/api/squads/` 的 `get+post`）。
- 理由：不删则三条 M4 首路由**无法注册**（同 path+method 重复注册 panic）；留给切片删则**三片都要改
  `mount.rs` + ⑦ 基线 + ⑨ 快照**（三个共享文件、三个写者 ⇒ 必然 3-way 冲突）。这是 W3b 已经实测过的取舍。
- 键形态：删掉的是 6 个**无尾斜杠**键（基线文件里就是 `GET /api/chat/sessions` 这类字符串），
  切片随后注册的 45 条是**上游原形**（多为带尾斜杠）—— 两组是不同注册键，不会互相覆盖。
- 随之：`python3 scripts/route_parity.py --write-baseline`（195→189）+ 重生成 ⑨ 快照
  （`cargo test -p mc-conformance -- --nocapture` 生成 `crates/mc-conformance/report.json`）。
  **只有 anchor 与 M4-INT 可以刷 ⑦ 基线**；四个切片一律不刷（`docs/15` §7.3 纪律）。

### 5.3 anchor 验收

```bash
bash scripts/gates.sh --only route-parity      # 期望 exit 0，registered 189、regression 0
bash scripts/gates.sh --only conformance       # ⑨ 快照与 report.json 一致
bash scripts/gates.sh --only file-size         # ⑩ 新文件全部远低于 800
cargo check --workspace --all-targets          # 空 crate/空切片编译通过
```

---

## 6. 契约证据：M4 现在 0 条 fixture（**本波最大风险**）

### 6.1 实测缺口

- `contracts/golden/extraction-report.tsv`：全仓 596 个站点 → **54 条 fixture / 501 skip / 40 helper_site**；
  其中命中 M4 域测试文件的站点 **19 个，全部 skip**（18 条 `request_var_unresolved`，1 条 `no_status_assertion`）。
- `contracts/golden/projects/` 里的 3 条 fixture **不是** project 契约测试——它们来自
  `handler_test.go:693/751/759` 的"子 issue 继承父 project"用例，用 `POST /api/projects` 只作**装置**；
  而且 ⑨ 回放里这 3 条的状态全是 `unevaluable`（需要真库装置）。
  ⇒ **M4 域可用 fixture = 0。**
- ⑨ 全局现状（`crates/mc-conformance/report.json`，`a51d523` 实测）：fixture **58** 条 →
  `pass 5 / mismatch 1 / unmounted 5 / unevaluable 47`；**契约等价率 8.62%**，可离线判定 11 条。
  M4 域那 3 条全在 `unevaluable` 里。
- 后果：`plan1` §5 W4 写明的验收门「**域内 fixture 通过**」**当前不可执行**（没有域内可判定 fixture 可跑）。

### 6.2 为什么抽不出来（上游测试的写法）

M4 域的上游测试**绝大多数绕过 chi 路由、直接调用 handler**：

```go
req := newRequest("POST", "/api/chat-sessions/"+sessionID+"/messages", map[string]any{"content": content})
req = withURLParam(req, "sessionId", sessionID)
req = withChatTestWorkspaceCtx(t, req)
w := httptest.NewRecorder()
testHandler.SendChatMessage(w, req)          // ← 直接调 handler，不是 testutil.Call(...).Want(...)
if w.Code != http.StatusCreated { t.Fatalf(...) }   // ← 断言在 if，不在 .Want(...)
```

抽取器现有 I1/I2 规则只认 `testutil.Call(...).Want(status)`
（见 `scripts/extract_upstream_fixtures.py` 的 "Supported idioms"），因此这类站点一律
`request_var_unresolved`。注意 **`/api/chat-sessions/...` 是历史路径**（现行 router 只有
`/api/chat/sessions/...`；`/api/chat-sessions/{id}/gc-check` 属 M3 daemon 面），fixture 的路径必须
按**路由模板**归一，不能照抄字面量。

### 6.3 M4-0b：抽取器规则 I4（建议先做，收益远超 M4）

新规则（bounded 版）：

1. 识别 `testHandler.<Handler>(w, req)` / `h.<Handler>(w, req)` 直接调用站点；
2. 回溯 `req` 的 `httptest.NewRequest|newRequest(方法, 路径, body)`；
3. 应用后续 `withURLParam(req, k, v)` 把 `:param` 替换为占位符（与 I1 同法）；
4. 断言来源扩展为 `if w.Code != http.Status<X>` / `w.Code != <int>`；
5. **归一**：字面量路径若不在 `docs/fixtures/upstream-routes.tsv`，尝试 `-`→`/` 的兼容映射后仍不匹配则
   记 `path_not_registered`（不静默丢弃）；
6. handler 名 → 路由键的映射表由 `upstream-routes.tsv` + `router.go` 的 `h.<Handler>` 提取生成，
   落到 `docs/fixtures/handler-routes.tsv`（新文件，可复算）。

**可抽取池（实测）**：

| 范围 | 站点数（`testHandler.X(`） | 其中含状态断言 | 文件数 |
| --- | ---: | ---: | ---: |
| M4 域（chat/project/squad） | **223** | 197 | 26 |
| 全仓 handler 测试 | **1346** | — | 175 / 301 |

⇒ 这一片的**边际收益是 M5（autopilot）之后所有剩余波次的公共基础设施**，不只是 M4 用的。
建议 M4-0b 先以「三个最小文件」立标杆（`chat_pending_tasks_test.go`、`chat_history_test.go`、
`squad_member_status_test.go`，实测 36 站点 / 23 断言），跑通后再放量。

### 6.4 回退方案（若 M4-0b 超预算）

M4 各切片自带 axum 级集成测试（照 `crates/mc-http/tests/agents/*`、`tests/tasks/*` 的写法），
并在 PR 与 `docs/43` 里写明「域内 fixture 通过」这条门**降级为切片自证 + 上游用例逐条对照**，
把差额登记为 known_gap。**不允许**为了让门变绿而删门。

---

## 7. 晋升顺序与前置条件（并发 ≤ 3）

1. **M4-0 anchor**（必须最先，单分支）→ 合入 `feat/multica-rs-initial`。
2. **M4-0b**（可与 M4-1/2 并行；只写 `scripts/` + `contracts/golden/`，与任何切片无文件交集）。
3. 三槽并行：**M4-1 project ∥ M4-2 squad ∥ M4-3 chat 读面**。
   - 与 M3 收尾（LUM-1438 daemon / LUM-1442 批 2 / LUM-1443 批 3 / LUM-1440 execenv）**写集零交集**
     （后者在 `routes/daemon*`、`mc-runtime/adapters/*`、`mc-ws/*`），可安全并存；
   - 唯一共享物是 `Cargo.lock`（anchor 一次落定后不再动）与 `docs/37`（集成文档，M4 切片只读）。
4. **M4-4**：前置 = **M3-7（LUM-1438）已合入**（ws 广播）+ M4-3 已合。若 M3-7 长期未合，
   可先落"不发广播"的版本并把广播登记为 follow-up（在 PR 里显式说明），**不要**自己实现 notifier。
5. **M4-INT**：`docs/43-M4-INTEGRATION.md`（照 `docs/21-M2-INTEGRATION-RECIPE.md` 配方）
   + 一次性 `route_parity.py --write-baseline`（→234）+ ⑨ 等价率重算 + `docs/fixtures/route-owners.tsv`
   的 M4 行清空。

---

## 8. 门禁与验收

### 8.1 逐片交付门（统一这一条命令）

```bash
bash scripts/gates.sh              # ①fmt ②build ③clippy ④clippy-test-util ⑤test ⑦route-parity ⑨conformance ⑩file-size
bash scripts/gates.sh --with-db    # 追加 ⑥db ⑧schema-drift（需 MULTICA_TEST_DATABASE_URL；无 URL 直接 exit 1）
```

- 切片 PR **不得**刷 `docs/fixtures/route-parity-baseline.json`；⑦ 的"新增路由"对基线是**单调增**，
  只有**删**路由才判 regression（本波已由 anchor 先删并刷基线）。
- ⑩：新文件 ≤800 行，**M4 新代码不得进 `scripts/file_size_baseline.tsv`**。
- ⑧：M4 不写迁移 ⇒ schema-drift 应恒绿；若红，说明有人误加迁移，回滚而非改对账脚本。

### 8.2 逐片验收清单（写进每个切片 issue）

- [ ] 路由逐条与 §1.1 的**方法+路径（含尾斜杠）**逐字一致；
- [ ] ⑥ 真库下跑通（`--with-db`），无 skip；
- [ ] 上游该域的 golden fixture（M4-0b 产出）全绿，或按 §6.4 记账；
- [ ] 写集严格落在 §4.2 的表内，未碰 anchor 文件与其他切片的文件；
- [ ] `docs/43`（或本文件）补记实测偏差。

---

## 9. 编号预占与文档落点

| 号 | 用途 | 状态 |
| --- | --- | --- |
| `docs/42` | 本文件（M4 计划） | 本切片落 |
| `docs/43` | M4 集成记录（M4-INT） | 预占 |
| `docs/44`+ | M5（autopilot）/ 后续波次 | 未占（截至 `a51d523`：38–41 已被 M3 占用） |

`docs/37`（W3c 预飞）与 `docs/15`（M3 计划）**本切片一律不动**；M4 的修订建议在 §11 之后
由 M4-INT 统一并入 `docs/15` 或独立的 `docs/43`。

---

## 10. 复算命令（本文件所有数字的可验来源）

```bash
# ① M4 路由 45 条（owner 列）
awk -F'\t' '!/^#/ && $3=="M4"' docs/fixtures/upstream-routes.tsv | wc -l

# ② ⑦ 现状：owner=M4 未实现 39 / 占位 10 / registered 195
python3 scripts/route_parity.py --json | python3 -c \
  'import json,sys;r=json.load(sys.stdin);print(r["owners"]["M4"],r["counts"])'

# ③ 上游 handler/service 规模（需上游检出；见 docs/20 的 full-blob shallow clone 配方）
for f in chat.go chat_history.go chat_title.go chat_pinned_agent.go squad.go squad_briefing.go \
         project.go project_resource.go; do wc -l server/internal/handler/$f; done

# ④ SQL 查询面
for f in chat chat_pinned_agent quick_action task_message squad project project_resource; do
  printf "%-20s %5s %3s\n" "$f" "$(wc -l < server/pkg/db/queries/$f.sql)" \
    "$(grep -c '^-- name:' server/pkg/db/queries/$f.sql)"; done

# ⑤ 表存在性（应 11/11 命中，0 处 DROP）
for t in chat_session chat_message chat_draft_restore chat_pinned_agent project project_resource \
         squad squad_member quick_action; do
  echo "$t $(grep -rl "CREATE TABLE.*$t" migrations/upstream/ | wc -l)"; done

# ⑥ fixture 缺口（M4 域 19 行全 skip；⑨ 里那 3 条 project fixture 全 unevaluable）
awk -F'\t' 'NR>1 && $1 ~ /chat|project|squad/{c[$5]++} END{for(k in c) print k,c[k]}' \
  contracts/golden/extraction-report.tsv
python3 -c "import json;r=json.load(open('crates/mc-conformance/report.json'));\
print(r['totals']);print('contract=%.4f'%r['contract_equivalence_rate'])"

# ⑦ 抽取池（直接调 handler 的站点数）
grep -rc 'testHandler\.[A-Z][A-Za-z]*(' server/internal/handler/*chat*_test.go \
  server/internal/handler/*project*_test.go server/internal/handler/*squad*_test.go | \
  awk -F: '{s+=$2} END{print s}'

# ⑧ 规则 I4（§6.3）落地后的复算 —— 上一条 ⑥ 的「M4 域 0 条 fixture」已被本组取代：
#    M4 域实测 242 站点 / 30 文件，其中 48 抽出为 fixture、194 逐条记账（不许静默丢弃）。
#    （<checkout> = 与 contracts/golden/PIN 同 commit 的上游检出，见 docs/20；
#      scratch/ 是本仓未入库的临时目录，docstring 里也用它做示例。）
python3 scripts/extract_upstream_fixtures.py --upstream <checkout> --out scratch/golden-i4
#    I4 站点总数（`candidate_sites.by_kind.direct_handler`）与产出的域分布
python3 -c "import json;d=json.load(open('scratch/golden-i4/stats.json'));\
print(d['candidate_sites']);print(d['fixtures']['by_domain'])"
#    M4 域逐行结论（reason 机器可查；基线是 19 行 / 6 文件 / 0 抽出）
awk -F'\t' 'NR>1 && $1 ~ /chat|project|squad/{c[$5" "$6]++} END{for(k in c) print c[k], k}' \
  scratch/golden-i4/extraction-report.tsv
#    §6.3 的 36 个标杆站点（chat_history 16 / chat_pending_tasks 19 / squad_member_status 1）
awk -F'\t' 'NR==1 || $1 ~ /(chat_history|chat_pending_tasks|squad_member_status)_test.go/' \
  scratch/golden-i4/extraction-report.tsv

# ⑨ handler → 路由键索引（新文件，规则 I4 第 6 步；由 router.go 的 h.<Handler> + 路由表复算）
python3 scripts/upstream_handler_index.py --upstream <checkout> --write-handler-routes
python3 scripts/upstream_handler_index.py --upstream <checkout> --check-handler-routes

# ⑩ 门 ⑨（conformance/stateless）层新 fixture 的落点分布 ——「unevaluable 有原因」是协议，不是可删项：
python3 -c "import json;r=json.load(open('crates/mc-conformance/report.json'));\
print(r['totals']);print('contract=%.4f'%r['contract_equivalence_rate'])"
#    fixture 树必须可字节复算（⑨ 门的前置），且 58 条旧 fixture 形状不变：
python3 scripts/extract_upstream_fixtures.py --upstream <checkout> --out contracts/golden --check
```

---

## 11. 对 `plan1` W4 的修订（必须一并生效）

1. **依赖口径**：`plan1` 的甘特图写「W4 after W2」，但实测 chat 的 10 条路由（M4-4）读写
   `agent_task_queue` 并依赖 task-queued / `chat:done` 的 **ws 广播**（M3-7）⇒ W4 的**写面**实际
   依赖 W3 收尾。W4 的**读面 + project + squad**（35 条）与 W3 无关，可提前开跑。本文件按此拆片。
2. **验收门供给方**：`plan1` W4 的「域内 fixture 通过」在当前抽取器下**没有 fixture 可跑**；
   必须新增 M4-0b（§6）。这不是 M4 独有——全仓 1346 个直接调 handler 的站点同样抽不出来，
   是 W5–W9 的公共前置。
3. **规模微调**：`plan1` §3.3 的 M4 crate 清单为 `mc-chat`/`mc-project`/`mc-squad`（3 个）；
   本文件沿用，不新增 crate（`chat_task` 等子域留在 `mc-chat` 内的模块里，避免 crate 数膨胀到 48 上限之上）。
4. **工时**：4 周（`plan1`）在本仓 3 槽并发 + M3 收尾并行的现实下，拆成 4 片 + 2 前置 + 1 集成，
   每片 1–3 天量级；**不改变** `plan1` 的总工时口径。

---

## 12. 风险登记

| # | 风险 | 触发条件 | 对策 |
| --- | --- | --- | --- |
| R1 | `chat.sql` 77 条 query 的语义漂移（分页游标、未读、排队位置） | M4-3/M4-4 分头照抄 | M4-0b 的 fixture 做回归；无 fixture 时逐 query 对照上游 SQL 原文 |
| R2 | 6 条占位未预删 ⇒ 首路由注册 panic | anchor 未落就派切片 | anchor 独立 issue + 独立分支，先合再派 |
| R3 | M4-4 卡 M3-7 | M3-7 延期 | 先落非广播版本 + follow-up 登记（§7.4） |
| R4 | history/thread 的渠道分支不可实现 | M7 未到 | 只落非渠道分支 + known_gap（§4.3） |
| R5 | 文件超 800 行 | 照搬上游大文件 | anchor 已按子域拆文件；⑩ 门拦 |
| R6 | 三片同改 `mount.rs`/⑦ 基线/⑨ 快照 | anchor 未预删 | anchor 一次删除 + 刷基线（W3b 已验证） |
| R7 | fixture 抽取器 I4 规则被路径归一卡住 | `-`→`/` 映射不全 | `path_not_registered` 显式记账；先做 §6.3 的三个标杆文件 |
