# M4-4：chat 派发与生成面 10 条（LUM-1475）

承接 `docs/42-M4-PLAN.md` §1.1 的 #8–#10、#13–#15、#19–#20、#24–#25：把上游
`server/internal/handler/chat.go` / `chat_history.go` / `mika_onboarding.go` 的
**派发面与生成面**搬进本仓。不加迁移、不碰 `Cargo.toml` / `Cargo.lock`、不改 ⑦ 基线。

- **base**：`feat/multica-rs-initial` @ `2b71c01`（PR #50 已合并）
- **分支**：`agent/devbox5/lum1475-m4-4`
- **上游落点**：`server/internal/handler/chat.go`、`chat_history.go`、`mika_onboarding.go`、
  `service/task.go`、`db/queries/chat.sql`、`db/queries/agent.sql`
- **交付形态**：本节记的是**救援 + 收口**后的最终树（原始 run 静默死亡，见 §5）

## 1. 交付面

| 方法 | 路径 | 上游 handler | 层次 |
| --- | --- | --- | --- |
| POST | `/api/chat/sessions/:sessionId/messages` | `SendChatMessage`（`chat.go:832`） | `routes/chat/task/dispatch.rs`（355 行） |
| POST | `/api/chat/sessions/:sessionId/onboarding` | `StartMikaOnboarding`（`mika_onboarding.go:60`） | `dispatch.rs` |
| POST | `/api/chat/sessions/:sessionId/quick-actions/regenerate` | `RegenerateChatQuickActions`（`chat.go:1100`） | `routes/chat/task/quick_action.rs`（158 行） |
| GET | `/api/chat/sessions/:sessionId/pending-task` | `GetPendingChatTask`（`chat.go:1609`） | `routes/chat/task/queue.rs`（316 行） |
| DELETE | `/api/chat/sessions/:sessionId/queued-tasks` | `ClearQueuedChatTasks`（`chat.go:1758`） | `queue.rs` |
| POST | `/api/chat/sessions/:sessionId/queued-tasks/:taskId/prioritize` | `PrioritizeQueuedChatTask`（`chat.go:1673`） | `queue.rs` |
| GET | `/api/chat/pending-tasks` | `ListPendingChatTasks`（`chat.go:1477`） | `queue.rs` |
| GET | `/api/chat/pending-tasks/has-any` | `HasPendingChatTasks`（`chat.go:1565`） | `queue.rs` |
| GET | `/api/chat/history` | `GetChatChannelHistory`（`chat_history.go:55`） | `routes/chat/task/history.rs`（333 行） |
| GET | `/api/chat/thread` | `GetChatThread`（`chat_history.go:178`） | `history.rs` |

三层落点（每层只依赖上一层；`mc-chat` 与 `mc-repos` 之间**没有**依赖边）：

| 层 | 文件 | 内容 |
| --- | --- | --- |
| 纯领域（`mc-chat`） | `task.rs`（477）、`history.rs`（381）、`quick_action.rs`（192）、`onboarding.rs`（484）、`lib.rs` | DTO 形状、状态机取值、判据纯函数（`RegenerateRefusal` / `regenerable_target` / `is_regenerable_turn`、语言表、分页游标编解码） |
| 落库（`mc-repos`） | `chat_task.rs`、`chat_task/{queue,send,support,onboarding}.rs`、`chat_history.rs`、`chat_quick_action.rs` | 手写 SQL 照 `db/queries/*.sql` 原文；`agent_task_queue` 只读/只写 chat 相关列 |
| HTTP（`mc-http`） | `routes/chat/task.rs`（路由聚合）+ `task/{dispatch,queue,quick_action,history,support}.rs` | 门（认证/INVOKE/公开会话/归档）→ 解码 → 仓储 → 响应，判定顺序逐字照上游 |

写集 18 文件 / +4420 −84（含本片的 2 个救援提交）。**没有**新增 `#[ignore]` 真库测试
（见 §3 G9 —— 这是本片最大的交付质量缺口，已开 `LUM-1601` 收口）。

## 2. 三条跨波依赖的处置（`docs/42` §4.3）

1. **task 队列**（M3-3/M3-6，已合入）：`SendChatMessage` 复用 `agent_task_queue`；状态取值
   对齐迁移的 CHECK，不引入自造列。
2. **ws 广播**（M3-7 / LUM-1438 ⇒ 帧与 hub 面由 **LUM-1506 / PR #49 交付**）：上游在提交后发
   `broadcastTaskEvent(EventTaskQueued)` + `NotifyTaskEnqueued` + `publishChat(EventChatMessage)`，
   `chat:done` / `chat:quick_actions` 同理。**本片一条都没接**（不建 notifier、不发事件），
   按 `docs/42` §4.3 的降级预案登记为缺口 ⇒ **`LUM-1600`**。
   ⚠️ 注意 `docs/43` 的 G1/G2 是**指名 M4-4** 接这两个调用点的：本片没做到，`LUM-1600` 是
   对它的显式接管，不是「另一个切片的事」。
3. **渠道集成**（M7）：`/api/chat/history` 与 `/api/chat/thread` 只落**非渠道**分支
   （上游 `h.SlackHistory == nil`）：history 读本会话转录，thread 回
   `writeNoChannelIntegration` 的固定响应。渠道阅读器（`ChannelOverview` / `Thread`）随 M7。

## 3. 未覆盖项与偏离（known_gap）

| # | 缺口 / 偏离 | 位置 | 归属 |
| --- | --- | --- | --- |
| G1 | **四条用户面广播的调用点全未接**：`chat:message`（*帧尚不存在*）、`task:queued`、`chat:done`、`chat:quick_actions`（*帧尚不存在*）。帧面/hub 通知面已在 PR #49 就绪 | `routes/chat/task/{dispatch,queue,quick_action,support}.rs` | **LUM-1600** |
| G2 | `clear_queued_chat_tasks` 提交后的**四步副作用**未接：`captureTaskCancelled` 埋点、`ReconcileAgentStatus`、`broadcastTaskEvent`、`notifyTasksFinished` | `mc-repos/src/chat_task/queue.rs` | **LUM-1600**（后两步）/ 埋点与状态汇总属 M6 |
| G3 | quick-actions **provider 未接入** ⇒ `QuickActions == nil \|\| !Enabled()` 恒真，202 成功与三个目标态 409（`NoTurn`/`Stale`/`Busy`）在路由上**不可达**；两条 SQL 真做但无测试 | `routes/chat/task/quick_action.rs` + `mc-repos/src/chat_quick_action.rs` | provider = M6/M7（生成侧 790 行）；测试 = LUM-1601 |
| G4 | 渠道阅读器缺：history 只有「本会话转录」一条路径，thread 恒回 `writeNoChannelIntegration` | `mc-repos/src/chat_history.rs`、`routes/chat/task/history.rs` | M7 |
| G5 | `AgentReadiness`（`agent_ready.go:132`）不做：**运行时不可用的发言照旧排队**，上游在 M6/M7 侧会拒 | `mc-repos/src/chat_task/send.rs` | M6/M7 |
| G6 | 标题派生（`chattitle.Derive`）的**异步替换**未接（CAS 写入在仓储里已做） | `mc-repos/src/chat_task/send.rs` | M6/M7 |
| G7 | `buildRuntimeMCPOverlay` / `applyAttributionFallback` 不可达 ⇒ `runtime_mcp_overlay` / `runtime_connected_apps` 恒 `NULL`，attribution 直接按 `DirectHumanRun` 写 `direct_human` / `chat` / 会话 id（`attribution_fail_closed` 分支对 `AuthUser` 永不触发） | `mc-repos/src/chat_task/send.rs` | M6/M7（Composio） |
| G8 | `prioritize` 成功后**不再回读**一次 `GetAgentTask`（上游会，用来回最新行）；响应由 CAS 的 `RETURNING` 直接给出 | `routes/chat/task/queue.rs` | 有意偏离（值等价） |
| G9 | **chat 面 16 个 `mc-repos` 模块的真库测试**：本片交付前全无（`chat_session`/`chat_message`/`chat_draft_restore`/`chat_pinned_agent`/`chat_task*`/`chat_history`/`chat_quick_action`），承载的是 4.4k+ 行手写 SQL。本片删除 3 处**假称有真库测试**的注释（原句照抄自 `chat_session.rs` 的样板） | `crates/mc-repos/src/chat_*.rs` | **LUM-1601 已交付 9/16（见 §7）**，余 7 个见 §7.5 |
| G10 | 错误信封偏离：上游 `writeFeatureDisabled` / `dispatchBlocked` 是**扁平**体，本仓部分收入嵌套信封（`{"error":{...}}`，照 `daemon/tasks.rs:567` 先例）；状态码与机器可读 `code` 不变 | `routes/chat/task/{quick_action,support}.rs` | 协议面既有偏离（`docs/32` D-4） |
| G11 | `dispatch_blocked` 在 `routes/chat/task/support.rs` 留了**第二份**逐字实现（`routes::tasks::rerun` 已有同名私有实现，跨模块走不到）⇒ 文案漂移时门 ⑦/⑨ 看不见 | `routes/chat/task/support.rs` | 技术债：提升为全仓共享 helper |
| G12 | `agent_task_queue` 的 `CHECK`（`migrations/0001_init.up.sql:230`）是**已知错误**、不是契约 ⇒ 状态取值以 `mc-task` 的常量与上游 `task.go` 为准 | `mc-repos/src/chat_task/*` | 既有事实（M3 已记录） |
| G13 | anchor scaffold 对 `quick_action` 表的判断**是错的**：那张表是 *issue* 快捷动作（`237_quick_action.up.sql`），与 chat 无关；chat 的 quick actions 存在 `chat_message.quick_actions` JSONB 列 | `mc-repos/src/chat_quick_action.rs` 模块头 | 需在 M5/M6 的 scaffold 修正（避免后续切片照抄错表） |
| G14 | `history`/`thread` 把 `task_context` 的**任何**失败（含 DB 错误）都折成 404，逐字照上游，以免出现上游没有的 500 | `routes/chat/task/history.rs` | 有意偏离（与上游一致） |

## 4. 门禁证据

`MULTICA_TEST_DATABASE_URL=postgres://mc_lum1563:…@127.0.0.1:5432/multica_lum1563 bash scripts/gates.sh --with-db`
（日志 `gates-1475-merged-1.log`，**合并后**的树；报文真库 566 迁移 / 无新增迁移）：

| 门 | 结果 | 读数 |
| --- | --- | --- |
| ① fmt | PASS | 0s |
| ② build | PASS | `--all-targets --locked`，72s |
| ③ clippy | PASS | `-D warnings`，26s（救援前 **101**） |
| ④ clippy-test-util | PASS | `-p mc-http --features mc-http/test-util`，18s |
| ⑤ test | PASS | workspace **1178 passed / 0 failed / 99 ignored**（31s） |
| ⑥ db | PASS | `migrate applied 0`；`--ignored` e2e **217 passed / 0 failed**（19 个 target，47s） |
| ⑧ schema-drift | PASS | 真库 vs `contracts/upstream-schema.sql`，29s |
| ⑦ route-parity | PASS | 见下 |
| ⑨ conformance | PASS | `pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306`，契约等价率 5/365 = 1.4%，已接入 5/28 = 17.9%（37s） |
| ⑩ file-size | PASS | 最长新文件 `mc-chat/src/onboarding.rs` 484 行；`tests/chat.rs` 796 行（≤800） |
| — | **overall** | **PASS — 10/10，261s** |

⑦ 读数（`upstream 456 @ f41fae6b08fb`）：

| 时点 | local 注册 | ⑦ 基线 | implemented | known_gap | unclaimed | regression | local_only |
| --- | --- | --- | --- | --- | --- | --- | --- |
| base `2b71c01`（§33 记录） | 290 | 290 | 231 real + 2 ph = 233 | 223 | 0 | 0 | 11 |
| 本片合并后 | **300** | 290 | **241 real + 2 ph = 243** | **213** | 0 | 0 | 11 |

Δ = local +10、implemented real **+10**、known_gap −10 —— 与本片 10 条路由逐条对上。
**不需要刷新 ⑦ 基线**（10 条上游键本就在基线里，本片把它们从 `known_gap` 移到
`implemented`）；`slash_alias_audit` 无 `EXTRA_ALIAS`（10 条全是 plain 子路由，只有无尾斜杠形态）。

本片新增的测试证据：

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 纯领域判据（`mc-chat`） | `cargo test -p mc-chat` | 26 passed（`task` 8 / `history` 6 / `quick_action` 4 / `onboarding` 8） |
| 路由内联判据（`mc-http`） | `cargo test -p mc-http --lib routes::chat::task` | 4 passed（`quick_action` 2 / `support` 2） |
| 10 条路由**存在性守卫**（无需 DB） | `cargo test -p mc-http --test chat --features test-util m4_4` | 1 passed —— 8 条 `AuthUser` 面缺头 401 / 缺 workspace 400、`history`/`thread` 缺 actor 凭证 **403**；路径写错即掉 404 |
| router 构造（matchit 冲突回归） | 同上（`lazy_app()` 装配全量 router） | 构造成功 —— 修掉 `:id` / `:sessionId` 冲突前的 `panicked: insertion failed due to conflict` 不再出现 |

## 5. 救援记（本片为什么有两个 `fix` 之外的提交）

原始 run（`01a0cdf9-2e48`）12:18:02Z 结束、**零 comment、零产物**：worktree 里躺着 ~3.5k 行
未提交改动，分支未推送。救援动作（全部在最终树里）：

1. 先把编译/格式化过关的 3.5k 行**提交**（`4dfc01a`，18 文件 +4408 −84），再动它。
2. 跑合并树门禁 ⇒ **失败**：`GATE_CLIPPY_EXIT=101` + conformance/测试阶段
   `router()` panic。根因两条，都修在 `aa31dba`：
   - **matchit 0.7 路由参数名冲突**：M4-4 写 `:id`，M4-1..M4-3 的 chat 面写 `:sessionId`，
     同一位置两个参数名 ⇒ `Router::new()` 直接 panic（门 ⑤⑥⑨ 全红）。统一为 `:sessionId`
     并加回归守卫（§4 最后一行）。
   - **clippy 26 条**：`format_push_string` ×4、`doc_markdown`（反引号不配对）、
     `unnecessary raw-string hashes` ×4、常量断言、`usize→i64` 强转、`single_match`、
     大型枚举变体（`Box`）、`items_after_statements` 等。
3. `docs/45`（本文件）+ 路由守卫测试 + 3 处**假真库测试声明**的更正（G9）。

## 6. 交接

- **LUM-1601**（G9）：chat 面 `mc-repos` 模块的真库测试与共享 fixture —— **已交付**，见 §7。
- 其余 G4–G7、G13 随 M7 / M6 与 scaffold 修正推进。
- 本片的 `LUM-1475` 交付后停在 `in_review`（`done` 留给人工）。

## 7. 续记：LUM-1601（G9）chat 面真库测试

- **base**：`feat/multica-rs-initial` @ `0fd96b4`（PR #62 已合并）
- **分支**：`lum-1601-chat-repo-tests`
- **改动面**：11 个源码文件（3 改：`chat_task.rs` / `chat_history.rs` / `chat_quick_action.rs`；
  8 新增测试文件）**零生产 SQL 改动** + 本文件（§3 G9 行与 §7）

G9 记的 chat 面里，本片给**手写 SQL 最密的 9 个模块**补上真库测试：`chat_task/{send,queue,onboarding}`、
`chat_history`、`chat_quick_action`、`chat_session`、`chat_message`、`chat_pinned_agent`、
`chat_draft_restore`。**新增 39 个 `#[ignore]` 测试**（`mc-repos` 的 `--ignored` 用例 108 → 147）。

### 7.1 现场与摆放

共享 fixture 落在 `crates/mc-repos/src/chat_task/tests/mod.rs`：一个最小现场（workspace + user +
runtime + agent + chat_session）+ 只做 INSERT 的裸句柄（`TaskSeed` / `MessageSeed` / `RawTask` /
`raw_session` / `new_project` / `new_agent` / `new_draft_restore`）+ `teardown`（删 workspace，
靠外键级联清干净）。照 `task/tests` / `autopilot/tests` 的先例：`MULTICA_TEST_DATABASE_URL`
没设 ⇒ 打印 skip 静默返回（门 ⑤ 不带库跑 `cargo test`，不能让整套变红）；
**设了就绝不静默跳过** —— 连不上、或只是一个空串，都在 `setup` 里 `expect` 失败（即 issue 要求 3
的「不允许静默跳过」落点；现场每个用例自建自删、id 随机，同库并发跑互不干扰）。

断言分布：`tests/{send,queue,onboarding,session,message,pin,draft}.rs` 七个子模块 +
`chat_history.rs` / `chat_quick_action.rs` 的内联 `mod tests`。后四个模块（`chat_session` /
`chat_message` / `chat_pinned_agent` / `chat_draft_restore`）的用例没有内联进各自文件，两个原因：
真库现场只此一份；`chat_session.rs` 已 797 行、贴着门 ⑩ 的 800 行上限（内联会把热点文件顶穿）。

**排序确定性**：同一毫秒内插入的 `Uuid::now_v7()` 之间没有单调性，所有多行用例都显式给
`created_at`，或在比较前先排序集合 —— 不让「同毫秒并列」变成偶发红。

### 7.2 覆盖到的判据（按风险）

| 模块 | 用例 | 验的是 |
| --- | --- | --- |
| `chat_task/send.rs` | 10 | 锁顺序（先 `chat_session` 再 `agent`，用「持 agent 行锁 + 探 `chat_session … NOWAIT` 的 55P03」反证）；owner-row fence 阻塞（持 workspace/runtime 行锁 ⇒ send 卡住，放开才过）；**rebind 后回读**（任务拿 agent 当前 runtime，不是会话快照）；步骤 7/8 两处 CAS/采纳；附件绑定的四条拒绝理由；`runtime_mcp_overlay` 恒 NULL；媒体轮用附件名当标题；标题 CAS 已占用时不改；同会话第二次发送仍 `queued` 且不重写标题 |
| `chat_task/queue.rs` | 10 | `prioritize` 的 CAS 与三种结局（提升/无活跃回复/目标已非 queued）＋ 409 前置的 agent 锁失败；`clear` 保留可见头、取消其余、重锚下一个头、渠道输入转 stopped、直连输入删行；两处列表的排序与租户/创建者/可见 agent 过滤；缺会话时 noop |
| `chat_task/onboarding.rs` | 4 | kickoff 与开场行**相差恰好 1µs**（真库时间戳语义）；任何用户消息之后 `AlreadyStarted`；归档/缺失会话的结局；档案 + workspace 名的读取 |
| `chat_session.rs` | 4 | `create` / `create_explicit` 的 `runtime_id` 子查询回填与「显式创建」戳；四步事务与两个 404 分支**回滚不留行**；列表可见性（`explicitly_created_at` 或非渠道消息）、未读计数、归档强制 0、pin 压过最近活动；`update_project(_locked)` 的父锁与租户护栏；`delete_cascade` 三步事务与 `delete` / `lock_for_delete` 幂等 |
| `chat_message.rs` | 2 | `list_for_session` 升序 + `VISIBLE_HEAD_FILTER`（排队后续轮的输入隐藏、渠道记录不进用户面）；`list_page` 的 `(created_at, id)` 元组游标在**时间戳并列**时不丢不重 |
| `chat_history.rs` | 5 | 转录分页走完整条无缺口/重复；游标必须带**纳秒**（响应只到秒 ⇒ 秒级截断会漏行）；`onboarding_kickoff` 与渠道记录按可见头隐藏；渠道上下文按 `channel_context_revision` 过滤（含回填前的 NULL 行）；单行读的 None/Some |
| `chat_quick_action.rs` | 2 | 「最近可重生成回复」只看任务归属的 assistant 行；忙碌判据数**后台**行（`pending` 列表把它们藏了） |
| `chat_pinned_agent.rs` | 1 | `COALESCE(MAX(position), 0)` ⇒ 第一条 `1.0`；重复 pin 经 `ON CONFLICT DO UPDATE` 幂等且**保持原槽位**；列表 `position ASC, created_at ASC`；每用户私有 + 租户隔离 |
| `chat_draft_restore.rs` | 1 | 列表按 `created_at ASC`；`consume` 的 `chat_session_id` 是**授权**条件（错会话 ⇒ 0 行且草稿仍在）；`prune_by_session` 只清自己那一份 |

### 7.3 以 SQL 为准的四条事实更正

1. **`LUM-1601` 的 issue 说「幂等（同 client id）」在本仓没有对应物**。`send_direct_chat_message`
   的事务里根本没有 client id 参数；真正的幂等/唯一性面是另外三处：步骤 7 的
   `AdoptOrphanOnboardingKickoff`（孤立 kickoff 只被采纳一次）、步骤 8 的
   `InitializeChatSessionTitle` CAS（标题只写一次）、步骤 5 的 owner-row fence
   （`lock_task_owner_rows` 的 `FOR KEY SHARE`）。用例按这三处写。
2. **`clear_queued_tasks` 的「保留可见头」不保护后台重生成行**。head CTE 滤掉
   `regenerate_quick_actions_for IS NOT NULL`，而 cancel 的谓词只有
   `status = 'queued' AND id IS DISTINCT FROM head` ⇒ 后台行会被**一起取消**（它既不是可见头，
   也就不可能被保留）。
3. **`VISIBLE_HEAD_FILTER` 只隐藏「排队中」任务的输入**。任务一旦翻到 `dispatched` / `running`，
   它的输入行不再隐藏；`task_id IS NULL` 的用户行恒可见；可见头本身当然也不隐藏（它就是这一轮的正文）。
4. **`delete_cascade` 的剪枝语句只按 `chat_session_id` 过滤**（不看 workspace，上游同款）
   ⇒ 一次**跨租户**且失败的删除调用也会把草稿剪掉（父行还在）。这不是笔误，是上游语义，
   已写成断言固化。

### 7.4 门禁证据

真库 `postgres://mc_lum1601:…@127.0.0.1:5432/multica_lum1601`（独立角色/库，避开并发 run 的现场）。

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 本片新用例 | `cargo test -p mc-repos --lib -- --ignored` | **147 passed / 0 failed**（本片前 108 ⇒ 新增 39） |
| 门 ⑥ 真库 e2e 计数 | `bash scripts/gates.sh --with-db --db-url …` | `--ignored` **372 passed / 0 failed**（本片前 **333** ⇒ **+39**；`mc-repos` 108 → **147**，`mc-http` 221 不变） |
| 全部门 | 同上（日志 `gates-1601-final.log`） | **PASS — 10/10，92s**；⑥ `migrate applied 0`（566 迁移文件全已应用、无新迁移） |
| ⑦/⑨/⑩ | 同上 | ⑦ `local 329 / baseline 300 / implemented 263 real + 2 ph / known_gap 191 / regression 0`；⑨ `pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306`（本片**零路由改动** ⇒ 与 base 同读数）；⑩ 最长新文件 `tests/send.rs` 776 行、`session.rs` 506 行（≤800） |

### 7.5 残留（G9 的剩余面）

`mc-repos` 的 chat 面实测是 **11 个文件**（G9 原文写「16 个模块」，是 issue 侧的过计）：7 个顶层
`chat_*.rs` + `chat_task.rs` + `chat_task/{send,queue,support,onboarding}.rs`。本片对这 11 个文件的
每个公开方法都至少有一条真库断言（`send` 1 / `onboarding` 4 / `queue` 5 / `session` 17（含 `create`）/
`message` 2 / `history` 4 / `quick_action` 2 / `pinned_agent` 4 / `draft_restore` 3），
包括 `session_has_public_user_message` / `session_is_channel_backed` / `has_pending_tasks_by_creator`
这类容易被漏掉的辅助读。

没覆盖的是三类**非模块粒度**的面：

| 面 | 为什么没盖 | 归属 |
| --- | --- | --- |
| `chat_task/support.rs` 的共享 SQL 片段与常量（`VISIBLE_HEAD_ORDER` / `PRIORITY_CHAT` / DTO） | 它们是拼进其它语句的字符串与返回结构，只能经调用方执行路径间接打到（本片已打到） | 随 M6/M7 改这些 SQL 时同步补 |
| **真并发互抢**（两条 `send` 同时争同一个会话头） | 本片只覆盖**确定性交错**（fence 阻塞、锁顺序探针、CAS 前置态），真竞态需要可控的两连接交错，重复跑有抖动风险 | 出现相关回归再补 |
| 渠道阅读器（`chat_history` 的 M7 面）与 quick-actions provider | §3 G4 / G3 明确不在本片 | M7 / M6 |

另：`derive_title` 在仓储侧仍是注入的 `fn(&str) -> String`（§3 G6 的异步替换未接）⇒ 用例里用的是
**与产品同源**的替身（`TITLE_LIMIT = 30`、首行非空、折叠空白、超出取 29 个 rune + `…`），
markdown fence / 链接的解装饰未复现。
