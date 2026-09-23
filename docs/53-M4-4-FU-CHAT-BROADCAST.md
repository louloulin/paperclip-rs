# 53 · M4-4-fu 聊天派发的提交后广播（LUM-1600）

波次 C 的补片（`docs/42` §4.3 的跨波降级预案兑现）：把 `docs/45` 登记的两条硬缺口
**G1**（四条用户面广播的调用点全未接；其中 `chat:message` / `chat:quick_actions` 的帧当时
**尚不存在**）与 **G2**（`clear_queued_chat_tasks` 提交后四步副作用）在本片收口。

上游基准：`louloulin/multica` @ `f41fae6b`（与 `scripts/route_parity.py` 内嵌同一 commit），
上游全文在 `server/internal/{handler/chat.go,mika_onboarding.go,service/task.go,
service/chat_quick_actions.go,pkg/protocol/{messages.go,events.go}}`。

本片**不新增路由**（⑦ 读数不变），只补「提交之后」那一段：三个用户面帧 + 五个调用点 +
四个 hub 通知方法。派发面的路由与鉴权门是 `docs/45` 的交付，本片一行未改。

## 0. 交付物

| 文件 | 行数（`git diff --numstat` 86116ae..HEAD） | 内容 |
| --- | --- | --- |
| `crates/mc-ws/src/frames/user_events.rs` | 187（+187/−0，新） | 三个用户面帧构造器 + 三个线上形状单测 |
| `crates/mc-ws/src/frames.rs` | 13（+13/−0） | `mod user_events;` + 再导出（帧面与 `docs/43` §1.3 的三条同址） |
| `crates/mc-ws/src/hub/{mod,user_face}.rs` | `hub.rs` → `hub/mod.rs`（+37/−74）+ `user_face.rs`（+135，新） | 用户面通知段整体搬进子模块并新增三条；`notify_tasks_finished`（按 runtime 去重）留在 `mod.rs` |
| `crates/mc-http/src/routes/chat/task/broadcast.rs` | 271（+271/−0，新） | **唯一与 hub 对话的落点**：载荷构建器 + 行/行外包装 + 单测 |
| `crates/mc-http/src/routes/chat/task/{dispatch,queue,quick_action}.rs` | +72/−18 | 五个调用点（顺序、位置、条件逐字） |
| `crates/mc-http/src/routes/chat/task.rs` | +6/−3 | 声明 `broadcast` 子模块 + 更新模块头的「未接」清单 |
| `crates/mc-repos/src/chat_task/{queue,support}.rs` | +27/−13 | `clear_queued_tasks` 改回返回被取消的行；`prioritize_queued_task` 的 `RETURNING` 增列 `agent_id` |
| `crates/mc-ws/tests/{hub_user_frames,hub_registry}.rs` | +202/−1 | 投递面（用户面 vs daemon 面 vs 别的工作区）+ 批量唤醒去重 |
| `crates/mc-http/tests/chat/broadcast.rs` | 545（+545/−0，新） | 真库 + 真 socket 的 e2e 三条（顺序栅栏 / 取消批次 / onboarding） |
| `crates/mc-http/tests/chat.rs` | +2/−0 | `#[path = "chat/broadcast.rs"] mod broadcast;` |

合计 15 文件 **+1497 / −109**（含 `hub.rs` 改名后的搬运行）。5 个提交，每个自洽可编译。

**未触碰**：任何 `Cargo.toml` / `Cargo.lock`（无新依赖）、`migrations/**`、
`crates/mc-daemon-proto/**`（三个帧的载荷类型与 kind 常量早已在那里冻结，本片只加薄构造器）、
`routes/mount.rs`、`routes/mod.rs`、⑦ 基线、`docs/fixtures/**`。

## 1. 五条调用点

| # | 上游（commit `f41fae6b`） | 本地落点 | 发出的帧 | 顺序约束 |
| ---: | --- | --- | --- | --- |
| 1 | `service/task.go:2493-2494`（`SendDirectChatMessage` 提交后：`broadcastTaskEvent(EventTaskQueued)` → `NotifyTaskEnqueued` → `publishChat(EventChatMessage)`） | `dispatch.rs::send_chat_message` | `task:queued` → `daemon:task_available` → `chat:message` | **气泡最后**：客户端不能先看到消息、再等状态胶囊出现 |
| 2 | `handler/chat.go:1728`（`PrioritizeQueuedChatTask` 提交后**只**发 `BroadcastTaskQueued`，无唤醒） | `queue.rs::prioritize_queued_chat_task` | `task:queued`（`status="queued"`） | 无 |
| 3 | `handler/chat.go:1758` + `service/task.go:2985`（`ClearQueuedChatTasks` 提交后四步） | `queue.rs::clear_queued_chat_tasks` | 逐条 `task:cancelled` → 一次合并唤醒 | 先逐条广播、后唤醒（队列视图先更新） |
| 4 | `mika_onboarding.go:181`（服务端开场白 `publishChat`，载荷**不带** `TaskID`） | `dispatch.rs::start_mika_onboarding` | `chat:message`（`role="assistant"`） | kickoff 行**永不**广播 |
| 5 | `service/task.go:7307`（`broadcastChatDone`，含 quick_actions 投影） | —— **无调用点** | `chat:done` | 见 §4 D1（本地还没有聊天完成路径） |

三条纪律（写在 `broadcast.rs` 模块头，是**本片的设计主张**）：

1. **只有一个地方碰 hub**：`routes/chat/task/broadcast.rs` 是全仓唯一调
   `state.daemon_hub.notify_chat_*` / `notify_task_*` / `notify_tasks_finished` 的文件。
   三个 handler 只负责「在正确位置、按正确顺序、带正确条件」调它。`queue.rs` 连
   `daemon_hub` 字段都不出现（`task_queued_for` 是行外版本，供 `prioritize` 的 CAS
   `RETURNING` 行使用 —— 那一行不是 `ChatTaskRow`）。
2. **尽力而为**：所有调用只拿到 `DeliveryOutcome`，没有错误通道 ⇒ **不改状态码**。
   与上游一致（上游的 `notify*` 也不回写响应），也与既有先例一致
   （`routes/agents/env.rs:214` 把 `notify_agent_status` 的返回值直接丢掉）。
3. **提交后不回读**：载荷全部取自事务 `RETURNING` 的行 —— 为此 `clear_queued_tasks`
   改回返回 `Vec<ChatTaskRow>`（`docs/45` 时代只回计数），`prioritize_queued_task` 的
   `RETURNING` 增列 `agent_id`。**没有**为广播多发一次 SELECT。

`chat` 任务的 `issue_id` 恒 `""`（`agent_task_queue.issue_id` 对 chat 是 NULL，
上游 `taskEvent` 写 `task.IssueID` 的空值）—— 键**在**、值空，不是省略。

## 2. 三个帧的线上契约

键序 = **字典序**（`fn frame` → `Message::new` → `serde_json::Value`，见 `docs/16` §11.5），
`task_id` / `failed` 两个 `omitempty` 的**缺席**与 `quick_actions: []` 的**在场**都是客户端
分支的依据，逐个帧逐字钉在 `frames/user_events.rs` 与两个投递面测试里。

| 帧 | 载荷 | 形状要点 |
| --- | --- | --- |
| `chat:message` | `protocol/messages.go:235` | `task_id` `omitempty`（onboarding 开场白没有任务 ⇒ 键缺席）；`created_at` 秒精度（`timestampToString`），**与 201 响应同一份** |
| `chat:quick_actions` | `messages.go:186` | `quick_actions` **恒在**（收敛失败时是 `[]`，不是省略，这是解开客户端骨架屏的信号）；`failed` 只在 `failed:true` 时出现；元素 `primary` 同样 `omitempty` |
| `task:cancelled` | 复用 `taskEvent` 键集（`service/task.go:7159`） | 与 `task:queued` 只差 `status`；`chat_session_id` 只在 `task.ChatSessionID.Valid` 时出现 |
| `daemon:task_available` | `notifyTaskAvailable` | `task_id` **`omitempty`**：带真 id = 「去认领这一条」，**空 id = 「队列里可能还有活儿」**（`notifyRuntimeMayHaveWork` 的 hint 形态） |

`daemon:task_available` 的这条语义差是本片实现中被测试逼出来的一个事实：批量唤醒
（`notify_tasks_finished`）走的是空 id 分支，线上帧里**没有** `task_id` 键，
`crates/mc-ws/tests/hub_registry.rs` 的 `batch_wake_dedupes_runtimes_and_skips_empty_ids`
与 `mc-http` 的取消用例各钉一次（两条路径：hub 单测 + 真库 e2e）。

## 3. 投递面：谁听得见（本片的核心正确性）

用户面通知的受众判定完全复用 `docs/43` §1.3 建立的过滤（`hub/user_face.rs` 的
`notify_workspace_users`）：`Index::Workspace` 维度 + 逐连接要求 `identity.user_id` 非空 +
`identity.allows_workspace(ws)`。**空工作区 ⇒ `miss()`，一帧不发**（上游
`notifyWorkspaceFrame` 的 `""` 分支）。

这不是优化而是正确性：本仓只有一条 `/api/daemon/ws` 连接面（`docs/32` D-4），
daemon 面 `mdt_` 连接的 `workspace_id` 与用户连接**相同**、只有 `user_id` 是空串。
不排掉的话 `chat:message` 的**正文**会顺着工作区索引投给同一工作区的 daemon 连接。

本片为这一条补了两层证据：

| 层 | 位置 | 断言 |
| --- | --- | --- |
| hub 单测（真 socket，无库） | `crates/mc-ws/tests/hub_user_frames.rs::dispatch_family_frames_share_the_user_face_filter` | 三条帧各一次：用户连接逐字收到、同工作区 daemon 面连接静默、别的工作区用户静默；并断言 `ws-1` 索引里**确实有两条**连接（排除靠 `user_id` 空，不是靠 not-in-index ⇒ 防假绿） |
| 真库 e2e（真 socket + 真 router） | `crates/mc-http/tests/chat/broadcast.rs` | HTTP 写入（`POST …/messages`）之后：用户 socket 收到 `task:queued` → `chat:message`，daemon socket 只收到一条带真 task id 的唤醒 |

`mc-http` 的 e2e 必须是真 socket：`tower::ServiceExt::oneshot` 拿到的是**未升级**的响应，
WS 读泵不会跑（`tests/daemon/ws.rs` 的首段注释已说明同一件事）；也必须真库：载荷取自
事务 `RETURNING` 的行、受众取自 hub 的工作区索引。

## 4. 偏离与未接（登记）

| # | 项 | 位置 | 归属 |
| --- | --- | --- | --- |
| D1 | **`chat:done` 仍无调用点**（`docs/45` G1 的残留）：本地没有聊天**完成**路径 —— 没有「任务跑完 → 写 assistant 消息 + 投影 quick_actions」的服务层（上游在 `task.go:7307` 的 agent 结果收敛里） | `mc-repos/src/chat_task/**`（无该写点） | M6（agent 结果收敛）+ `LUM-1601`（真库测试） |
| D2 | **`chat:quick_actions` 帧有落点、无调用点**（`docs/45` G1 + G3 的残留）：provider 未接 ⇒ `routes/chat/task/quick_action.rs` 唯一的可达出口是 403，202 分支不可达。帧与入口都已就绪并标了 `#[allow(dead_code)]` + 注释（**不要**再建第二条广播路径） | `routes/chat/task/quick_action.rs` | provider = M6/M7 |
| D3 | **四步副作用只做了后两步**（`docs/45` G2 的前半）：`captureTaskCancelled`（埋点）与 `ReconcileAgentStatus`（agent 状态汇总）在本地没有对应物 ⇒ 跳过 | `mc-repos/src/chat_task/queue.rs` | M6（埋点 + 状态汇总） |
| D4 | `notifyRuntimeMayHaveWork` 里的 `EmptyClaim.Bump`（`service/task.go:7105`）**未做**：全仓没有 `EmptyClaim` 这类空转计数 | `crates/mc-ws/src/hub/mod.rs::notify_tasks_finished` | 无归属（本仓无此机制；若后续加空转退避再补） |
| D5 | `notify_tasks_finished` 的入参口径与上游不同：上游吃 `[]Task`，本地吃 **runtime id 列表**（handler 手上已有行，投影在 `broadcast.rs` 做）⇒ hub 面不需要知道任务结构 | `hub/mod.rs` | 有意偏离（值等价：同样跳空、同样按 runtime 去重） |
| D6 | `prioritize` 提交后**不回读**（承 `docs/45` G8）：`task:queued` 的载荷取自 CAS `RETURNING` 行 | `routes/chat/task/queue.rs` | 有意偏离（值等价） |
| D7 | 拆分（R7 800 行上限）：`mc-ws/src/hub.rs` → `hub/{mod,user_face}.rs`、新增 `frames/user_events.rs`；`mc-http/tests/chat.rs` 因此增 2 行到 798（**不进基线**） | 见 §0 | 技术债已还，无残留 |

`docs/45` 的 G1/G2 在本片**部分**收口：G1 的四条里 `chat:message` / `task:queued` /
`task:cancelled` 三条已接、`chat:done` 转 D1；G2 的四步里后两步已接、前两步转 D3。
`docs/45` §3 的原表格保留不动（历史读数），以本文件的 D1–D3 为当前口径。

## 5. 门禁证据

`MULTICA_TEST_DATABASE_URL=postgres://mc_lum1600:…@127.0.0.1:5432/multica_lum1600 bash scripts/gates.sh --with-db`
（日志 `gates-lum1600-1.log`，**本片分支最终树**（含本文件）；报文真库 566 迁移 / **无新增迁移**，
`migrate applied 0` 幂等）：

| 门 | 结果 | 读数 |
| --- | --- | --- |
| ① fmt | PASS | `cargo fmt --all --check`，2s（首轮红：`tests/chat.rs` 的两个 `#[path] mod` 未按 rustfmt 排序 ⇒ `ee43c5f`） |
| ② build | PASS | `--all-targets --locked` |
| ③ clippy | PASS | `--workspace --all-targets -- -D warnings`（首轮红：`hub_user_frames` 的新用例 104 行 > 100 ⇒ 抽 `assert_user_face_only`） |
| ④ clippy-test-util | PASS | `-p mc-http --features mc-http/test-util` |
| ⑤ test | PASS | workspace **1319 passed / 0 failed**（95 个 target 组，33s，**不带**库变量） |
| ⑥ db | PASS | `migrate applied 0`（幂等）；`--ignored` e2e **278 passed / 0 failed**（21 个 target，含本片新增 3 条） |
| ⑧ schema-drift | PASS | 真库 vs `contracts/upstream-schema.sql`，27s |
| ⑦ route-parity | PASS | `local 322 registered / baseline 300 / implemented 256 real + 2 ph = 258 / known_gap 198 / unclaimed 0 / regression 0 / local_only 11` —— **与本片 base 逐字相同**（无路由改动）；`slash_alias_audit` 无新欠账 |
| ⑨ conformance | PASS | `pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306`，契约等价率 5/365 = 1.4%（与 `docs/49` 逐字相同） |
| ⑩ file-size | PASS | 最长新文件 `tests/chat/broadcast.rs` 545 行；`hub/mod.rs` 638 / `tests/chat.rs` 798（≤800），**基线无新增条目** |
| — | **overall** | **PASS — 10/10** |

**⑥ 的 e2e 计数是 +3 的硬证据**：base（`86116ae`）的 ⑥ 读数是 **275 passed**，
本片 **278** —— 差值就是 `crates/mc-http/tests/chat/broadcast.rs` 的三条，无其它 target 变动。

本片新增的测试证据：

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 帧形状（无库） | `cargo test -p mc-ws --lib` | 23 passed（含 3 条新帧的键序/omitempty） |
| 投递面（真 socket，无库） | `cargo test -p mc-ws` | 9 个 target 组全绿（`hub_user_frames` 3 / `hub_registry` 9 含新用例） |
| 提交后广播（**真库 + 真 socket**） | `MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --test chat --features test-util broadcast -- --ignored` | **3 passed**：`send_orders_user_frames_and_keeps_daemon_face_out` / `clear_queued_broadcasts_cancel_then_wakes_without_a_task_id` / `onboarding_opening_is_broadcast_without_a_task_id` |
| 载荷构建器（无库） | `cargo test -p mc-http --lib routes::chat::task` | 含 `broadcast` 的 2 条键集单测 |

取消用例里有两个**容易搞错**的事实，各自被一条断言钉住：

- `clear` 只取消「排队追问」、**保住可见头**（`queue.rs` 的 `head` CTE）⇒ 用例必须发
  **两条**消息；只发一条时被取消集合是空的（首轮实现就踩了这个坑，帧等不到是正确行为）。
  库里落点也断言了：追问 `cancelled`、头仍 `queued`。
- 再清一次是空批次 ⇒ 两条连接都静默（不是「重复广播」）。

## 6. 交接

- **`LUM-1601`**（`docs/45` G9）：chat 面 16 个 `mc-repos` 模块的真库测试与共享夹具 ——
  本片的 e2e 只覆盖**广播**这一段，仓储面（4.4k+ 行手写 SQL）仍然没有 `#[ignore]` 测试。
- **M6/M7**：`chat:done` 的写点（D1）、quick-actions provider（D2）、埋点与 agent 状态汇总（D3）。
- 本片交付后 `LUM-1600` 停在 `in_review`（`done` 留给人工）；父片 `LUM-1475` 保持 `in_review`。
- 下一片若要在 `routes/chat/**` 加第二个广播调用点：**改 `broadcast.rs`，不要在各 handler
  里直接调 `daemon_hub`** —— 顺序与过滤的全部知识都在那一个文件里。
