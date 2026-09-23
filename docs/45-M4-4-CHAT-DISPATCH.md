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
| G9 | **chat 面 16 个 `mc-repos` 模块全无 `#[ignore]` 真库测试**（`chat_session`/`chat_message`/`chat_draft_restore`/`chat_pinned_agent`/`chat_task*`/`chat_history`/`chat_quick_action`），承载的是 4.4k+ 行手写 SQL。本片删除 3 处**假称有真库测试**的注释（原句照抄自 `chat_session.rs` 的样板） | `crates/mc-repos/src/chat_*.rs` | **LUM-1601** |
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

- **LUM-1600**（G1/G2）：接四条 ws 广播调用点 + queue 面四步副作用 —— 帧面/hub 面已就绪，
  只剩调用点，是最短的一块硬缺口。
- **LUM-1601**（G9）：chat 面 16 个 `mc-repos` 模块的真库测试与共享 fixture。
- 其余 G4–G7、G13 随 M7 / M6 与 scaffold 修正推进。
- 本片的 `LUM-1475` 交付后停在 `in_review`（`done` 留给人工）。
