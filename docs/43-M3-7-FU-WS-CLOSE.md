# M3-7 收尾：ws 三条遗留缺口（LUM-1506）

承接 `docs/38-M3-WS-TRANSPORT.md`（M3-7 主干交付）。本片只收口 M3-7 遗留的三条缺口，
不做新路由、不加迁移、不碰 `Cargo.lock`。

- **base**：`feat/multica-rs-initial` @ `abb208e`（PR #47 已合并）
- **分支**：`feat/multica-rs-m3c-wsclose`
- **上游落点**：`server/internal/handler/daemon_ws.go`、`server/internal/handler/agent_env.go`、
  `server/pkg/protocol/messages.go`、`server/internal/task/task.go`

## 1. 三条缺口的处理

### 1.1 `?runtime_id=` / `?runtime_ids=` 收窄（已接）

上游 `daemon_ws.go:120` `parseRuntimeIDs` + `buildDaemonWebSocketIdentity:53-66`：daemon 面连接
可以用查询参数把自己**声明**到某个 runtime 子集；不在该机器名下的 runtime 直接
**404 `runtime not found`**，且收窄结果会成为连接的 `identity.RuntimeIDs`，从而决定投递面。

本地落点：`crates/mc-http/src/routes/daemon/ws.rs`

| 函数 | 上游 | 语义 |
| --- | --- | --- |
| `requested_runtime_ids(&HashMap<String,String>)` | `parseRuntimeIDs` | `runtime_id` + `runtime_ids`（逗号表）合流，逐项 trim、去空、保序去重 |
| `narrow_runtime_ids(full, requested)` | `buildDaemonWebSocketIdentity` | 无参数 ⇒ daemon 名下全集；有参数 ⇒ 逐项校验，缺席即 404 `runtime not found` |

调用点 `lifecycle::ws`：全集仍由 `DaemonRepo::runtime_ids_for_daemon(workspace_id, daemon_id)`
给出，收窄后写进 `ClientIdentity.runtime_ids`（hub 的 `by_runtime` 索引就是投递面）。

### 1.2 `install_ws_handlers` 不再是死代码（已收口）

`routes/daemon/mod.rs::install_ws_handlers` 此前**无人调用**，真实装配写在
`lifecycle::ws` 里直接调 `ws::install`（两套等价逻辑，其中一套永远不跑）。本片按「重接线」
收口：`lifecycle::ws` 改为调 `super::install_ws_handlers(&state)`，`ws::install` 保持
`pub(crate)`，`mod.rs` 里的文档锚点（`/// 把 ws 面的两个 handler`）保留 —— 
`mod.rs` 的路由计数单测靠它切分 `router()` 体。

**同时修掉一个真实缺陷：幂等闸的作用域。** 原先 `install_ws_handlers` 与 `ws::install`
各持一个**进程级** `static INSTALLED: OnceLock<()>`。一个进程可以有多个 hub（每个 e2e 用例
一个），进程级闸会让第一个 hub 之后的全部**漏装**：连接升级成功，但每一帧 RPC 都回
503 `rpc handler unavailable`。现在闸按 hub 判（`rpc_handler().is_some()` &&
`heartbeat_handler().is_some()`）：`set_*_handler` 本来就是「覆盖」语义，重复装只是换掉等价
闭包，多一次 `Arc::clone`、不做分配。这条有回归证据：`tests/daemon/ws.rs` 的
`ws_handshake_rpc_and_claim_share_the_http_body` 在多 hub 的进程里由 FAILED 转绿。

### 1.3 用户面广播（`agent:status` 已接；另两条登记为待接）

`crates/mc-ws/src/frames.rs` 新增三条用户面载荷/帧构造函数（逐字对照上游 JSON tag）：

| 帧 | 上游载荷 | 本地构造 |
| --- | --- | --- |
| `chat:done` | `protocol/messages.go:278` `ChatDonePayload` | `ChatDonePayload` / `chat_done_frame` |
| `task:queued` | `task.go:7159` `taskEvent` 的 payload 键集 | `TaskQueuedPayload` / `task_queued_frame` |
| `agent:status` | `agent_env.go:272`（`{"agent": <脱敏 AgentResponse>}`） | `AgentStatusPayload` / `agent_status_frame` |

`crates/mc-ws/src/hub.rs` 新增 `notify_chat_done` / `notify_task_queued` / `notify_agent_status`。

**投递面（本片的核心设计点）**：上游把 daemon 面（`daemonws.Hub`）与用户面
（`events.Bus` + 工作区订阅者）分成两个传输层；本仓只有一条 `/api/daemon/ws` 连接面
（`docs/32` D-4），所以用户面通知必须自己把 **daemon 面连接排除掉**。做法是把
`notify_frame` 拆出 `notify_frame_filtered(index, key, data, event_id, allow)`：用户面三条
走 `Index::Workspace`（与上游同一维度），逐连接要求 `identity.user_id` 非空
（daemon 面 `mdt_` 连接的 `user_id` 是空串）且工作区在授权 scope 内。

这不是优化而是正确性：不排掉的话 `chat:done` 的正文会顺着工作区索引投给同一工作区的
daemon 面连接。证据：`crates/mc-ws/tests/hub_user_frames.rs`（同工作区的 daemon 面连接
收不到、另一个工作区的用户收不到、纯 daemon 连接下三条通知都是 miss）与
`tests/daemon/ws.rs::env_update_broadcasts_agent_status_to_user_connections_only`
（HTTP 写入 → 用户 socket 收到帧；daemon 面连接用「顺序栅栏」证明没收到）。

## 2. 已接的端到端链路

`PUT /api/agents/:id/env` → `agent:status` 帧（`crates/mc-http/src/routes/agents/env.rs`）：
上游 `agent_env.go:266-272` 在提交后广播。本地在 `update_custom_env_audited` 提交并重读出
行（`has_custom_env` / `custom_env_key_count` 是本次写入后的真值）后，用
`AgentDto::from_row` 投影成**脱敏**载荷（env 值从不进帧）再交给 hub。广播失败不影响响应：
它是「尽力而为的唤醒通道」，客户端仍以 HTTP 面为准。

## 3. 未覆盖项与偏离（后续切片必须接手）

| # | 缺口 / 偏离 | 位置 | 归属 |
| --- | --- | --- | --- |
| G1 | `chat:done` 的**调用点**未接：上游在任务完成事务提交后发（`task.go:7307` `broadcastChatDone`），本地 chat 完成路径属 M4-3/M4-4 写集（`routes/chat/**` 是本片禁写区）⇒ 本片只交付帧与 hub 通知面 | `crates/mc-ws/src/{frames,hub}.rs` | M4-4 |
| G2 | `task:queued` 的**调用点**未接：上游 `task.go:2733` `BroadcastTaskQueued` 在队列写入后发；本地入队路径在 `routes/tasks.rs` / daemon claim 面 | 同上 | M4-4 |
| G3 | 上游 `broadcastChatDone` 的 `quick_actions` 投影（`[]ChatQuickAction`）本地未实现，载荷类型已留出该字段（`Vec<Value>`） | `ChatDonePayload::quick_actions` | M4-4 |
| G4 | 上游把 `TaskID` / `ChatSessionID` 放在**信封**上；本地帧面只有 `{type, payload}` 一层 ⇒ 这两个值落在载荷里（值相同，位置不同） | `TaskQueuedPayload` | 协议面既有偏离（`docs/32` D-4） |
| G5 | **重复出现的** `?runtime_id=A&runtime_id=B`：上游逐个合并，本地取 `Query<HashMap>` 只能留住最后一个。逗号形态 `?runtime_ids=A,B` 逐字一致（daemon 升级用的正是它） | `requested_runtime_ids` | 有意取舍；若要上游逐字一致需改 `RawQuery` 解析 |
| G6 | **用户身份连接**带 `runtime_id` 收窄：本地一律 404 fail-closed。上游会查用户可见的 runtime 集合后放行；本地的 `ClientIdentity.runtime_ids` 只由 `mdt_` token 填，没有等价的查询 | `lifecycle::ws` 的 `DaemonActor::User` 分支 | M4 或后续切片 |
| G7 | `AgentDto.skills` 在广播载荷里恒为空（本地没有上游 `attachAgentSkills` 的等价物）；`invocation_targets` 本片不查（省一次 DB 往返） | `routes/agents/env.rs` | M4-5（agents 收口） |
| G8 | 用户面事件缺 `events.Bus` 那样的**跨进程**扇出（上游经 Redis relay 回环）：本地 hub 只有进程内投递 | `crates/mc-ws/src/hub.rs` | 多实例部署前必须补（同 `docs/38` 的 relay 缺口） |

## 4. 证据

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 帧形状（含 `omitempty` 缺席语义） | `cargo test -p mc-ws` | 见 PR 描述（`user_facing_frames_match_upstream_wire_shape`） |
| 用户面投递面过滤 | `cargo test -p mc-ws --test hub_user_frames` | 2 passed |
| 收窄校验（纯函数） | `cargo test -p mc-http --lib routes::daemon::ws` | 7 passed |
| 收窄 + agent:status 真 socket（真库） | `MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --features mc-http/test-util --test daemon -- --ignored ws::` | 5 passed |
| 离线门禁 | `bash scripts/gates.sh` | 见 PR 描述 |
