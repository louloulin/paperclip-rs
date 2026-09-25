# M3-7 daemon 面（LUM-1438）：36 条 `/api/daemon/*` + 8 条 runtime 异步往返 + ws 接线 + `mc-daemon` 客户端

本文件是 M3 子波三 W3c 第一片（M3-7）的落地记录。范围 = `docs/15-M3-PLAN.md` §6 M3-7 与
`docs/37-M3-W3C-PREFLIGHT.md` §3.2 的写集：`docs/16-M3-DAEMON-PROTOCOL.md` §6.1 的 **36 条 daemon 路由**
+ §6.2 的 **8 条 runtime 异步往返**（用户面发起端 + 轮询端）+ **ws 服务端的接线** +
**`crates/mc-daemon` 客户端**（注册 / 心跳 / claim 的最小可用实现）。execenv、adapters、
cloud-runtime、计费与配额结算都不在本片（`docs/33-M3-ADAPTERS.md`）。

| 项 | 值 |
| --- | --- |
| issue | **LUM-1438**（parent epic LUM-1334） |
| 分支 | `agent/devbox5/bc22f48c54a3` → PR 目标 `feat/multica-rs-initial` |
| 基点 | `feat/multica-rs-initial` @ `62e7427`（含 W0-B2 schema 切换 #27、ws 传输层 #28、M3-4 #31、M3-5 #32、M3-6 #33、M3-8 批 1 #34、批 2 #38） |
| 上游对照 | `louloulin/multica` @ `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`：`server/internal/daemon/daemon.go`、`internal/daemonws/*`、`internal/handler/{daemon,daemon_ws,daemon_rpc,daemon_workspace,runtime_local_skills,runtime_models,runtime_update}.go`、`internal/middleware/daemon_auth.go`、`cmd/server/router.go` L1520–L1571 / L2265–L2281 |
| 契约来源 | `docs/16-M3-DAEMON-PROTOCOL.md`（§3 帧、§4 事件、§5 能力、§6 两张冻结表、§7 守卫别名）；`docs/fixtures/upstream-routes.tsv` |
| 门禁 | `bash scripts/gates.sh --with-db` **10/10 全绿**（实测读数见 §7） |

## 1. 结论

1. **44 条路由全部从 gap 变为真实实现**：`docs/16` §6.1 的 36 条（含 4 条 `*/result` 上报与
   `/api/daemon/claim` 历史别名）＋ §6.2 的 8 条用户面异步往返，共 44 条。本片**不重生成**
   ⑦ 基线与 ⑨ 快照，读数登记在 §7。
   路由逐条挂载在 `routes/daemon/mod.rs::router()`（35 条 `.route(` ⇒ 36 个 path/method 组合，
   `/tasks/:taskId/messages` 一条挂 GET+POST、`/api/daemon/claim` 是与 `/api/daemon/tasks/claim`
   同 handler 的历史别名），用户面 8 条挂载在 `routes/runtimes.rs::async_requests::router()`。
   **`mount.rs` 一行未动**：`mount_slice_daemon()` 的 merge 在预飞时已接好（`docs/37` §2.1）。
2. **本片修掉两处上游路径抄错**（原 attempt 的机械移植产物，会被 route_parity 判成「实现了别的路由」）：
   `GET /api/daemon/runtimes/:runtimeId/tasks/pending`（原写成 `…/tasks/:taskId/pending`，
   上游 `router.go:1547` 该段下没有 `{taskId}`）与
   `POST /api/daemon/runtimes/:runtimeId/recover-orphans`（原写成 `…/tasks/recover-orphans`，
   上游 `router.go:1570` 直接挂在 `runtimeId` 下）。两条 handler 本来就只取 `Path(runtime_id)`，
   改 router 即可；测试里的三处旧路径同步修正。
3. **ws 服务端的传输层不是本片写的**：hub / 索引 / 扇出 / 帧分派在预切片 **LUM-1439**（`docs/38`，
   已随 #28 合入）里交完，`crates/mc-ws` 本片**一行未动**。本片的 ws 义务是**接线**：
   `routes/daemon/ws.rs` 用 `OnceLock` 把 `Hub::set_heartbeat_handler` / `Hub::set_rpc_handler`
   装成 daemon 面的两个 handler（回调持 `Weak<AppState>`，不成环），并由
   `lifecycle::ws`（`GET /api/daemon/ws` 的升级 handler）在握手时调用。`dispatch_rpc` 与
   HTTP 腿共用 `claim_batch_core`（身份以 `DaemonAuth` 结构传入，**不接受任何可伪造的 header**），
   所以「ws RPC 的 body 与 HTTP 响应逐字节相同」是结构性事实而不是巧合（用例实测）。
4. **服务端 → runtime 的异步请求台账在进程内存**（四类请求：update / models / local-skills /
   local-skills-import，`daemon_requests.rs`）。上游同样是进程内 store（无 DB 表），本仓照搬
   ⇒ **不新增迁移**、不触碰 schema-drift 门；代价是单节点（偏离 **D-2**）。
5. **`mc-daemon` 客户端**（本片新增 5 个源文件 + 2 个测试文件，33 个测试）：注册 / 心跳 / claim /
   下线的**真实 HTTP 实现**（`reqwest`），加上一份「领到的任务不会被执行两次」的本地台账
   （`ClientState`：in-flight 去重、心跳水线、`runtime_gone` 去重）。**执行环境（execenv）不在本片**，
   属 M3-8 `LUM-1440`（与本 crate 共用 `lib.rs` / `Cargo.toml` ⇒ 串行）。
6. **测试**：`mc-http` 真库 e2e **22**（4 个模块：`loop_routes` 10 / `async_face` 7 / `gc` 3 /
   `ws` 2）+ `mc-http` 纯函数单测 **42**（`routes::daemon::*` 27 + `daemon_requests::*` 9 +
   `routes::runtimes::async_requests::*` 6）+ `mc-repos` 纯函数 4（`daemon::tests::*`，SQL 面由
   e2e 覆盖）+ `mc-daemon` **33**（`client_loop` 25 + `http_transport` 8）。ws 传输层自身的
   57 条属 LUM-1439。
7. **三处上游硬约束**是本片最容易踩的坑，都在代码注释与用例里显式触发：①`G_rt` / `G_task`
   只在**确认查无此行**时 404，其它 DB 故障必须 500（说成「已删除」会让 daemon 杀掉正在跑的 agent，
   `daemon.go:87-91`）；②`fail` 与 `pin-session` 成功**没有响应体**（204），不要「顺手补
   `{"status":"ok"}`」；③离线闸（503 `runtime is offline`）只有三处（models + 两个 local-skills 入队），
   **`update` 没有**。

### 交付文件（行数实测，全部 ≤ 800 —— 门 ⑩）

| 文件 | 行数 | 说明 |
| --- | --- | --- |
| `crates/mc-http/src/routes/daemon/mod.rs` | 201 | 聚合 + `router()` + `install_ws_handlers`（含「36 端点 = 35 条 `.route(`」的自证断言） |
| `crates/mc-http/src/routes/daemon/scope.rs` | 374 | `DaemonAuth` 提取器（`mdt_`/`mul_`/`mcn_`/dev-mode 四条分流）+ 三级门禁 + 身份→库内行 |
| `crates/mc-http/src/routes/daemon/lifecycle.rs` | 757 | register / deregister / heartbeat / ws / workspaces / repos / runtime-profiles |
| `crates/mc-http/src/routes/daemon/claims.rs` | 506 | `runtimes/:rid/tasks/claim`、`tasks/claim`（+ 别名）、prepare-lease、pending、recover-orphans、skill-bundles/resolve |
| `crates/mc-http/src/routes/daemon/tasks.rs` | 622 | status / start / wait-local-directory / progress / complete / fail / usage / cancel-ack / session / plugin-hooks / plugin-mcp credential |
| `crates/mc-http/src/routes/daemon/messages.rs` | 196 | 任务消息批量追加（POST）+ 增量读（GET，`since` 解析） |
| `crates/mc-http/src/routes/daemon/gc.rs` | 310 | 5 条 `gc-check`（批量 + 4 个单点探针） |
| `crates/mc-http/src/routes/daemon/skills.rs` | 253 | skill bundle 打包（含路径白名单 / 稳定哈希）+ 本地导入的 `target_skill_id` 口径 |
| `crates/mc-http/src/routes/daemon/requests.rs` | 733 | 4 条 `*/result` 上报 + `conflict` 非错误语义 |
| `crates/mc-http/src/routes/daemon/dto.rs` | 759 | 请求 / 响应 DTO（30+ 字段的任务投影、`type` 而非 `provider` 的 runtime 上报） |
| `crates/mc-http/src/routes/daemon/ws.rs` | 289 | hub 接线：心跳 handler + RPC handler（复用 `claim_batch_core`） |
| `crates/mc-http/src/routes/runtimes/async_requests.rs` | 445 | 表 B 8 条（发起端 + 轮询端）+ 6 条纯函数单测 |
| `crates/mc-http/src/daemon_requests.rs` | 744 | 四类请求的内存台账（`HasPending` 去重、`pending → running → 终态`、惰性超时） |
| `crates/mc-http/src/daemon_requests/wire.rs` | 227 | 台账的线上形状（`omitempty` 语义逐字段对齐） |
| `crates/mc-repos/src/daemon.rs` | 504 | 仓储根：`DaemonRepo` 装配 + repos 归一化 / `repos_version` / `issue_category` 纯函数 |
| `crates/mc-repos/src/daemon/{registry,tasks,skills,gc}.rs` | 528 + 577 + 213 + 108 | 注册 / claim 与生命周期写 / 本地 skill 导入 / 残留清扫 |
| `crates/mc-http/tests/daemon/{main,support,loop_routes,async_face,gc,ws}.rs` | 22 + 352 + 630 + 542 + 221 + 207 | 真库 e2e（22 条，全部 `#[ignore]`） |
| `crates/mc-daemon/src/{lib,wire,transport,state,client}.rs` | 44 + 215 + 280 + 227 + 481 | 客户端：wire DTO / HTTP 传输 / 台账 / 回路 |
| `crates/mc-daemon/tests/{client_loop,http_transport}.rs` | 658 + 399 | 脚本化传输 25 条 + 真 TCP 打桩 8 条 |

拆分动因都是门 ⑩（R7 单文件 800 行硬上限）；`lifecycle.rs` 757 与 `dto.rs` 759 是上限内的最大块，
`tasks.rs` 622 / `claims.rs` 506 / `requests.rs` 733 按域切开，`daemon_requests.rs` 744 与
`daemon_requests/wire.rs` 227 是「实现与线上形状」的切分。

## 2. 路由表与鉴权门

逐条的 method / path / 请求体 / 200 响应 / 幂等标签 / 错误码在 `docs/16` §6.1（36 条）与 §6.2（8 条）
里已冻结，本文件不重复；守卫别名（`G_ws` / `G_ws²` / `G_rt` / `G_task` / `G_rr` / `G_rc` / `G_ls` /
`G_plug`）的定义在 `docs/16` §7.1。下面只给**分组**与**本片实测的关键差异**：

| 组 | 条数 | 挂载点 | 门 |
| --- | --- | --- | --- |
| 生命周期与身份 | 7 | `register`、`deregister`、`heartbeat`、`ws`、`workspaces`、`workspaces/:id/repos`、`workspaces/:id/runtime-profiles` | `G_ws`（`ws` 另有升级前的身份判定） |
| claim 面 | 7 | `runtimes/:rid/tasks/claim`、`tasks/claim`、`claim`（别名）、`prepare-lease`、`tasks/pending`、`recover-orphans`、`skill-bundles/resolve` | `G_rt` / `G_task`；批量 claim 走 `G_ws²` 的**静默跳过**语义 |
| 任务生命周期 | 12 | `status`、`start`、`wait-local-directory`、`progress`、`complete`、`fail`、`usage`、`messages`（GET+POST）、`cancel-ack`、`session`、`plugin-hooks`、`plugin-mcp/:cid/credential` | `G_task`；插件两条再叠 `G_plug` |
| `*/result` 上报 | 4 | `runtimes/:rid/{update/:uid, models/:rid, local-skills/:rid, local-skills/import/:rid}/result` | `G_rt` + 台账里 request 必须存在（否则 404 `request not found`） |
| GC 探针 | 5 | `workspaces/:id/issues/gc-check`、`issues/:id/gc-check`、`chat-sessions/:id/gc-check`、`autopilot-runs/:id/gc-check`、`tasks/:id/gc-check` | `G_ws` / `G_task`；批量那条是 POST 但纯读 |
| 表 B：异步往返（用户面） | 8 | `/api/runtimes/:rid/{update, models, local-skills, local-skills/import}` 的 POST 发起端 + GET 轮询端 | `G_rr`（list 两条是 `G_rc`），import 两条是 `G_ls`（owner-only） |

**两条与上游逐字对齐、但很容易写反的门**（`runtime_local_skills.go` 实测）：

- **`local-skills` 的两条 list 路由不是 owner-only**：`InitiateListLocalSkills`（:594）与
  `GetLocalSkillListRequest`（:617）用的是 `requireRuntimeCapabilityReadAccess`（:552）＝「成员 + 可读」，
  只有**导入**那两条走 `requireRuntimeLocalSkillAccess`（:572）＝ owner-only。
  attempt 1 把 owner 门错误地铺到了四条上，本片按上游拆开（`runtimes/async_requests.rs` 模块头）。
- **离线闸只有三处**：`runtime_local_skills.go:601`（list 入队）、`:642`（import 入队）、
  `runtime_models.go:353`（models 入队）→ 503 `runtime is offline`。
  `InitiateUpdate`（`runtime_update.go:213`）**没有**这个闸 ⇒ 离线 runtime 的 update 入队是 **200**。

## 3. 上游语义逐条核过的点

1. **`G_rt` / `G_task` 只在 `isNotFound` 时 404**，其它 DB 故障 500；工作区不匹配一律 404 `not found`
   （不泄漏 id 存在性）。本仓把这条写成「仓储层只把 `None` 翻 404」的显式分支。
2. **心跳 ack 两种形状**：HTTP 面 = `{status} ∪ pending_*`（**故意不回** `runtime_id` 与
   `server_capabilities`，上游注释写明是冗余噪声），WS 面 = 完整 `DaemonHeartbeatAckPayload`。
   本仓 `lifecycle::heartbeat_ack()` 是唯一 ack 生产者，`http_ack_value()` 负责 HTTP 面的裁剪
   ——两条腿不会漂移。HTTP 面**不发** `runtime_gone`（那是 404 的职责）。
3. **批量心跳的 action 规划**：四类 `pending_*` 相互独立；`pending_local_skill_imports`（复数、批量）
   非空时**优先**，单数 `pending_local_skill_import` 被忽略；`runtime_gone` 先于 action 处理
   （去重 + 回收）。客户端侧 `plan_heartbeat_actions()` 逐条对齐。
4. **claim**：`daemon_id` 为空 → 400；与 token 绑定的机器名不一致 → **403 `daemon_id does not match
   token`**（头是凭据来源、体是客户端自述，不同即冒名）；`max_tasks < 0` → 400；`max_tasks == 0` →
   200 空列表；**不属于本机器的 runtime 静默跳过**（`G_ws²`），整批不因此 4xx。
   互斥靠 SQL 的 `FOR UPDATE SKIP LOCKED`（`mc-repos/src/daemon/tasks.rs:462`）＋
   `idx_one_pending_task_per_issue_agent_thread` 部分唯一索引 ⇒ 两条并发 claim 的并集恰好一条。
5. **`fail` 与 `pin-session` 成功是 204 空体**（上游 handler 末尾没有 `writeJSON`）。
   `pin-session` 的两字段都空 → 400 `session_id or work_dir required`；写回是 `COALESCE`（只填空槽、
   不覆盖），0 行受影响不是错误（任务可能已被取消）。
6. **`progress` 是覆盖语义**（step/total/summary 整体替换，不累积）；`usage` 是逐条 upsert，
   **单条失败只记日志、整体仍 200**，且只有**正数** `cost_usd_ticks` 才落库（0 = 不知道，
   负数 = 畸形上报）；`messages` 是追加，`since` 非法 → 400 `invalid since parameter`。
7. **`cancel-ack` 的 body 尽力而为**：解码失败不阻断（老 daemon 发 `{}`），四条写入合并成一条语句；
   但**落库失败必须 500**（这几个字段是已取消任务产出物的唯一指针，daemon 会重试）。
8. **`recover-orphans`** 只回 `{"orphaned","retried"}`；`retried` 恒 0（自动重试属 RuntimeSweeper 面）。
9. **`gc-check` 5 条**：批量那条 POST 纯读、`issue_ids` 有上限（超限 400 `too many issue_ids`）、
   `category` 只在**内建状态**出现；四条单点探针各回自己的最小投影。
10. **`*/result` 上报**：终态由 daemon 给，服务端不再改；`local-skills/import` 的 `conflict` 是**终态但
    不是错误**（`error` 保持为空，是否启用由**发起时**的 `supports_conflict` 决定）；
    服务端存的是**自己新建/覆盖的那行 skill**，所以线上回来的 `skill.id` **不是** daemon 上报的
    `s-1`，用户给的 `name` 胜出（用例把这条写成断言）。
11. **插件面恒禁用**：`plugin-hooks` 与 `plugin-mcp/:cid/credential` 都回 403
    `plugin_api_disabled`（上游 `requirePluginsV1` 在未装插件宿主时的同码；plugin 内容属 W6）。
12. **注册的 runtime 上报字段是 `type` 而不是 `provider`**（`RegisterRuntime` 的 JSON tag 实测），
    `kind` 在 Rust 侧带 `#[serde(rename = "type")]`；`deregister` 的 `offline_reasons` 是
    `map[string]json.RawMessage` 而不是数组。
13. **`ws` 升级前的身份**：`/api/daemon` 整组挂在 `DaemonAuth` 之下（`router.go:1520-1521`），
    无 `Authorization` 头时中间件先 401 ⇒ 上游 handler 里那条
    `400 runtime_ids or user identity required`（`daemon_ws.go:19-23`）在本地**走不到**，
    本地同样先 401（用例 `ws_without_identity_is_rejected_before_upgrade` 的判据）。

## 4. 模块地图与分层

```
router（daemon/mod.rs）
  ├─ scope::DaemonAuth        ← mdt_ (daemon token) / mul_ (PAT) / mcn_ (云 PAT，fail-closed) / dev-mode 头
  │     ├─ require_workspace_access   → G_ws
  │     ├─ require_runtime_access     → G_rt（只把 None 翻 404）
  │     └─ require_task_access        → G_task
  ├─ handler 组（lifecycle / claims / tasks / messages / gc / skills / requests）
  │     └─ DaemonRepo（mc-repos/src/daemon/{registry,tasks,skills,gc}.rs）→ PostgreSQL
  ├─ daemon_requests::RequestStore  ← 四类异步请求的进程内台账（无 DB 表）
  └─ ws::install → mc_ws::Hub        ← 传输层在 LUM-1439；本片只装 handler
```

- **没有新迁移**：本片零 schema 变更 ⇒ ⑧ schema-drift 门不动（`daemon_requests` 走内存，
  与上游一致）。
- **`AppState` 多两个字段**：`daemon_hub: Arc<mc_ws::hub::Hub>` 与
  `daemon_requests: Arc<RequestStore>`（`state.rs`）。与 `/live-events` 的 `realtime` 是**两个不同的
  hub**：用户面与 daemon 面的订阅集、帧类型、心跳都不同（`docs/37` §4.1）。
- **`crates/mc-daemon` 的分层**：`wire.rs`（线上 DTO）→ `transport.rs`（`DaemonTransport` trait +
  `HttpTransport`）→ `state.rs`（本地台账）→ `client.rs`（`DaemonClient` 回路）。trait 是给 ws RPC 腿
  留的缝：M3-8 的连接管理只需再加一个 impl，客户端逻辑不变。

## 5. 有意偏离（全部可核对）

| # | 偏离 | 位置 | 性质 |
| --- | --- | --- | --- |
| **D-1** | 无 `Authorization` 头时走 dev-mode 头身份（`X-Multica-User-Id` + 可选 `X-Daemon-Id`） | `routes/daemon/scope.rs:15,102`；`mc-daemon/src/transport.rs:143,185`；`tests/daemon/support.rs` | 仅测试与本机开发；`X-Daemon-Id` **只活在 HTTP 腿**，WS 的 RPC 分发不认它 |
| **D-2** | 异步请求台账在**进程内存**（单节点、无跨进程共享、无 Redis） | `src/daemon_requests.rs:16`（`state.rs` 装配） | 与上游同构，代价是多实例部署下请求不可见 |
| **D-3** | 时间戳 RFC3339 **秒**精度（Go 是 ns） | `src/daemon_requests/wire.rs:22`；`scope.rs:370` | 只影响同一秒内的先后可比性 |
| **D-4** | 无 membership / runtime 租约缓存，每次查库 | `scope.rs:29,285`；`routes/daemon/ws.rs:82` | 正确性优先；hub 侧不存租约，等价的门在更早一层 |
| **D-5** | 路径里 UUID 解析失败 → **400**（上游 `parseUUIDOrBadRequest` 之外的分支会 panic→500） | `scope.rs:353` | 只把 5xx 变 4xx，不放行任何请求 |
| **D-6** | `deregister` 逐条读 runtime（N+1） | `lifecycle.rs:440` | 语义一致；上游批量读一次 |
| **D-7** | **无 JWT 分支**（上游 `DaemonAuth` 最后那条 `jwt`） | `scope.rs:19` | M1 的 session 用 `X-Multica-Session` cookie，语义不同；dev-mode 兜底即可 |
| **D-8** | 错误 body 的 message 带 `mc-errors` Display 前缀，且是**嵌套信封** `{"error":{"code","message"}}` | `src/error.rs:22`（`respond_with`）；`tests/daemon/support.rs` 的 `PREFIXES` | 继承 M1/M2 全仓约定；`respond_with` 只覆写状态码、不分叉形状（离线闸的 503 用它把 422 口径改成上游契约的 503） |
| **D-9** | 客户端侧**重复定义** wire DTO（`mc-daemon/src/wire.rs` vs `mc-http` 的 `routes/daemon/dto.rs` vs `mc-daemon-proto`） | `mc-daemon/src/wire.rs:11` | 客户端不反向依赖服务端 crate；两侧漂移由本表与用例守着 |
| **D-10** | `crates/mc-daemon/Cargo.toml` 多一行 `reqwest = { workspace = true }` | `crates/mc-daemon/Cargo.toml:39` | `reqwest` 已是 workspace 依赖且在 `Cargo.lock` 里（mc-http 的出站调用用它）⇒ **无新包、无新 feature、无新版本**；锁文件唯一变化是本 crate `dependencies` 数组多一条（实测 +1 行）。预飞清单只算了 ws 侧，故登记 |
| **D-11** | WS 握手**不解析** `?runtime_id=` / `?runtime_ids=`：daemon token 的 runtime 集合**从库派生**（`runtime_ids_for_daemon`），用户身份连接不带 runtime | `lifecycle.rs::ws` | 上游该查询参数是权威输入（缺失时 400）；本仓放宽以兼容 dev-mode 身份连接。**新增能力时按上游补**（§8） |
| **D-12** | 测试文件布局与 `docs/37` §6.3 的清单不同：7 个预期文件名收敛为 4 个模块 + `main.rs`/`support.rs`（一个 e2e target） | `tests/daemon/*` | 只为少一份重复的 `support`；覆盖项逐条对得上（§6） |
| **D-13** | 插件面**恒禁用**（403 `plugin_api_disabled`）、skill/plugin **内容**不实现 | `routes/daemon/tasks.rs:562`；`skills.rs` | 范围依据 `docs/15` §1.5 注：M3-7 只做通道与降级，内容属 W6 |

> 编号纪律：D-1～D-6 是 attempt 1 就写在代码里的号，本片**不改号**；`scope.rs` 里原本把
> 「无 JWT 分支」也写成 D-2（与 `daemon_requests.rs` 撞号），本片改为 **D-7**；D-8～D-13 是本片新增。
> 代码里引用本表的注释一律写 `docs/32` 或 `docs/32-M3-DAEMON-FACE.md`（attempt 1 有两处写成
> `docs/32-M3-7-DAEMON-ROUTES.md`，已修正）。

## 6. 测试

```bash
# 客户端（不需要库、不需要服务端；真 TCP 打桩在 127.0.0.1:0）
cargo test -p mc-daemon

# daemon 面纯函数（不需要库）
cargo test -p mc-http --lib --features mc-http/test-util routes::daemon daemon_requests

# 真库 e2e（PG 必须已 `mc-migrate run --dir migrations`）
MULTICA_TEST_DATABASE_URL='postgres://…' \
  cargo test -p mc-http --features mc-http/test-util --test daemon -- --ignored
```

| 面向 | 用例 | 覆盖的验收项（`docs/15` §6 M3-7 测试清单） |
| --- | --- | --- |
| `loop_routes.rs`（10） | `daemon_loop_end_to_end`（注册→心跳→claim→prepare-lease→start→progress→messages→usage→complete，逐步断言，含 **claim 幂等**）、`concurrent_claims_yield_the_task_exactly_once`（**并发 claim 只发一次**：两请求并集恰好一条 + 只有一枚 `task_token`）、`claim_validates_request_shape`、`claim_rejects_daemon_id_mismatch`、`claim_skips_runtimes_of_other_daemons`、`fail_task_marks_terminal_state`、`cancel_ack_accepts_empty_body`、`recover_orphans_reports_counts`、`unknown_runtime_and_task_are_404`、`spoofed_identity_headers_do_not_authenticate` | prepare-lease / progress / complete / fail / cancel-ack 各 ≥1；recover-orphans ≥1；**claim 幂等 + 并发** |
| `async_face.rs`（7） | update / models / local-skills / local-skills-import 四条完整回路（发起 → `*/result` → 轮询到终态）、`update_requires_target_version`、`local_skill_list_is_readable_but_import_is_owner_only`、`enqueue_on_offline_runtime_is_503` | 异步请求-应答 ≥1 条完整回路（实为 4 条）；两条门差异 |
| `gc.rs`（3） | `gc_check_covers_all_five_probes`、`gc_check_hides_other_workspaces`、`batch_gc_check_rejects_oversized_batch` | `gc-check` 5 条 ≥1 |
| `ws.rs`（2） | `ws_handshake_rpc_and_claim_share_the_http_body`（真 socket 101 + 未知 method 404 + ws body 与 HTTP body **逐字相同** + WS 腿真领到任务 + 拆线重连仍 101）、`ws_without_identity_is_rejected_before_upgrade` | ws 握手 ≥1、RPC ≥1、断线重连 ≥1（另见 `mc-ws/tests/hub_*.rs` 的 57 条） |
| `mc-daemon/client_loop.rs`（25） | 注册 wire 形状（`type` 非 `provider`）、`InvalidConfig` 守卫、心跳水线与 `omitempty`、四类 action 规划、404 → `runtime_gone`、5xx 保留 runtime 且可重试、`heartbeat_all`、claim 短路与去重、`finish_task` 释放槽位、deregister、`TransportError` 分类 | 客户端回路（`docs/15` §6「client」） |
| `mc-daemon/http_transport.rs`（8） | 真 TCP stub：dev 头 / Bearer + 能力集（含 `skill-bundles-v1`，**不含** WS-only 的 `claim-poll-hints-v1`）/ 错误信封 / 非 JSON 错误体 / 成功非 JSON → `Malformed` / 真 404 → `RuntimeGone` / 不可达 → 可重试 / 注册→心跳→claim 的顺序断言 | HTTP 腿真实可用（D-10 的兑现） |

> `crates/mc-ws` 的传输层用例（`hub_heartbeat` / `hub_limits` / `hub_reconnect` / `hub_registry` /
> `hub_rpc`，57 条，全部真 socket）属 **LUM-1439**，本片只消费。

## 7. 门禁

```bash
bash scripts/gates.sh --with-db      # 10/10
```

实测读数（本片）：

| 门 | 读数 |
| --- | --- |
| ① fmt / ② build `--locked` / ③ clippy `-D warnings` / ④ clippy `test-util` | 全绿 |
| ⑤ workspace test | 全绿（本片新增 `mc-daemon` 33 条） |
| ⑥ 真库（迁移 + `--ignored`） | `GATE_DB_MIGRATE_EXIT=0` / `GATE_DB_E2E_EXIT=0`；其中 `mc-http` 的 daemon e2e **22 passed / 0 failed** |
| ⑦ route-parity | `upstream 456 (f41fae6b08fb) | local 239 registered | baseline 184`；`implemented 196 real + 10 placeholder = 206 / 456 | known_gap 250 | unclaimed 0 | regression 0 | local_only 11`；slash-alias `0 defect` / 19 allowlisted |
| ⑧ schema-drift | 全绿（本片零迁移 ⇒ 期望不变） |
| ⑨ conformance | `report matches crates/mc-conformance/report.json`（本片**不**重生成快照） |
| ⑩ file-size | `0 violation(s)`（本片最大新文件 `daemon_requests.rs` 744 行） |

```
  overall: PASS — 10/10 gate(s) green
```

**两个会让本片在门 ②/③ 变红的坑（已修，后来者复用）**：

1. 新 e2e target `tests/daemon/main.rs` 必须带 `#![cfg(feature = "test-util")]`
   （与 `tests/{issues,runtimes,agents,tasks}/main.rs` 同款）。否则门 ②/③ 的
   `--all-targets`（**不加** feature）会因 `Db::from_pool` 不存在而编译失败。
2. 门 ④ 会真的编译这些测试文件 ⇒ `too_many_lines`（pedantic，100 行）在主链用例
   （`daemon_loop_end_to_end` 179、`gc_check_covers_all_five_probes` 107）上触发；
   本仓惯例是就地 `#[allow(clippy::too_many_lines)]` + 一句理由，不是拆函数。

运行前删掉 target 下的 `debug/incremental`（本次实测 9.5 GB）并设 `CARGO_INCREMENTAL=0`
可以避免“No space left on device”把门 ④/⑨ 误判成代码错误。

## 8. 交接与遗留

1. **给 M3-8（`LUM-1440` execenv / `LUM-1441`～`LUM-1443` adapters）**：本片**没有**实现任何执行环境
   与 adapter；`crates/mc-daemon/src/lib.rs` 与 `Cargo.toml` 由两片共享 ⇒ 按 `docs/15` §6 串行。
   客户端侧留的缝是 `DaemonTransport` trait：WS RPC 腿只需再加一个 impl。
2. **给 M4**：`/api/daemon/*` 的写面已经能改任务状态，但**不广播**任何用户面事件
   （`agent:status` / `task:*` 交给 M4 的 hub 接线）。
3. **遗留缺口（本片明确不做，按优先级）**：
   - **WS 查询参数**（D-11）：上游把 `?runtime_id=` / `?runtime_ids=` 当权威输入（缺失且无用户身份 →
     400、坏 uuid → 400、未知/越权 runtime → 404、逐行判 workspace 访问）。本仓放宽为库内派生。
   - **`?runtime_ids=` 的多值合并**（重复参数 + 逗号分隔 + 去重 trim）随上一条一起补。
   - **多实例部署**（D-2）：`RequestStore` 要换成共享后端（Redis / DB 表）才能横向扩。
   - **计费与配额结算**：`task_usage` 只有读面与 upsert，无结算（`docs/15` §0）。
   - **插件内容**（D-13）：通道在，内容属 W6。
4. **给集成周期**：⑦ 基线与 ⑨ 快照需重生成（本片 +44 real）；`docs/15-M3-PLAN.md` §1.5 的
   `plugin/skill 内容` 注记与 `docs/37` §6.3 的测试文件清单已按本片的实际布局对齐（D-12）。

## 9. M6-0 anchor（`LUM-1665`）：文件→写者表与偏离登记

`docs/57-M6-PLAN.md` §5 的「每文件预扩张清单」是**锚点文件集**；本节的表是它落地后的
**准确版**（含锚点期做的三处归位判断）。M6 后续切片按本表认领写集，**不得**编辑左列之外的
共享文件（`routes/mount.rs` / `routes/mod.rs` / 各 `lib.rs` / 根 `Cargo.toml` / `Cargo.lock` /
⑦ 基线 / `slash-alias-allowlist.tsv` 全部由锚点冻结）。

### 9.1 三个**锚点冻结**的聚合/共享文件（M6 各片只读，写各自子文件）

| 冻结文件 | 谁写 | 说明 |
| --- | :-: | --- |
| `crates/mc-http/src/routes/mount.rs` | anchor | 删 2 条 M0 占位 + 加 5 个 `mount_slice_*()`（全无状态、锚点期空 router） |
| `crates/mc-http/src/routes/mod.rs` | anchor | 声明 `skills` / `plugins` / `plugin_bridge` / `v1` / `surfaces` |
| `crates/mc-http/src/state.rs` | anchor | `PluginSecretKey` + `plugin_key` + `plugin_surface_origin` 读取口 |
| `crates/mc-http/src/routes/skills/mod.rs` | anchor | 14 键聚合 + 5 个尾斜杠双形态键登记 |
| `crates/mc-http/src/routes/plugins/mod.rs` | anchor | 17 键聚合（M6-5 13 + M6-6 4，**不含** M6-8 的 hook 路由） |
| `crates/mc-http/src/routes/v1/mod.rs` | anchor | 9 键聚合；`policy::apply(merged)` **只在合并点套一层** |
| `crates/mc-http/src/routes/plugin_bridge/mod.rs` | anchor | 10 键聚合（M6-7 的 9 + M6-8 的 1） |
| `crates/mc-skill` / `mc-plugin-host` / `mc-mcp` 的 `src/lib.rs` | anchor | 声明全部计划子模块（`pub mod …`），各片只填自己的文件 |
| `crates/mc-repos/src/lib.rs` | anchor | `pub mod plugin;` + `pub mod skill;` |
| `crates/mc-repos/src/{skill,plugin}/mod.rs` | anchor | 子模块声明 + 本仓约定（裸 `Uuid` / 手写 `FromRow` / `map_sqlx_err`） |

### 9.2 锚点期**归位判断**（三处，`docs/57` §5 原表未覆盖或写的是另一落点）

| 事项 | 原表 | 本锚点落点 | 理由 |
| --- | --- | --- | --- |
| `routes/agents.rs` + `routes/agents/dto.rs` + M6-4 的 `/api/agents/{id}/skills*` 6 条 | 未列 | **交给 M6-4 自己加** `mod skills;` + `merge`（本锚点**不**建 `routes/agents/skills.rs`） | `routes/agents.rs` 是 M2/M4 既成文件，锚点碰它会与「一个文件一个写者」冲突；M6-4 是该文件在 M6 内的**唯一**写者 |
| `POST /api/plugin-bridge/v1/hooks/{key}` | `routes/plugins/hooks_job.rs`（M6-8） | `routes/plugin_bridge/hooks.rs`（M6-8） | 路由前缀与文件所在目录一致，漏挂的风险最小；`hooks_job.rs` 保留为 **0 路由**的 job 粘合落点。注册键总数不变（57/57） |
| `mc-skill` 的计划子模块 `git.rs` | `mc-skill/src/git.rs`（M6-3） | `mc-skill/src/source.rs`（M6-3） | 上游**没有** git 克隆路径：`internal/handler/skill.go:943-945` 只有 `clawHub` / `skillsSh` / `github` 三个 **HTTP** 源（`detectImportSource` L950，`clawHubAPIBase = "https://clawhub.ai/api/v1"`）。名字改成 `source` 才不会误导实现者去写 `git clone` |
| `MULTICA_PLUGIN_SURFACE_ORIGIN` | §5 只点名 `plugin_key` | `state.rs` 同时落 `plugin_surface_origin` | `state.rs` 由锚点冻结；若留到 M6-6/M6-7，那两片必须回来改冻结文件（锚点存在的唯一理由就是消灭这种改动） |

### 9.3 `mc-core` 的**不做什么**（登记为偏离，避免两处定义漂移）

1. **`mc_core::plugin` 故意不定义 manifest 契约类型**（`PluginManifestV1` 等已随旧 stub 删除）。
   唯一真值在 `mc-plugin-host::manifest`（M6-1 写）。理由：`mc-core` 是所有 crate 的公共底座，
   契约校验需要 zip/json-schema 级依赖，塞进 `mc-core` 会把依赖面扩大一整圈。
2. 旧 stub 的字段（`slug` / `visibility` / `body` / `owner_id` / `display_name` / `status` /
   `install_order` / `package_path` / `config_revision` / `secret_revision` / `via_attribution`）
   **在真实上游表里一个都不存在** ⇒ 本次是**重写**而非扩张：`skill.rs` 43 → 221 行、
   `plugin.rs` 68 → 462 行，字段逐列对齐 `migrations/upstream/**`。
3. `PluginStatus` 是**派生**两态（`from_enabled(bool)`），不是列；`PluginScope` 是
   **透明 newtype**（不是 enum）—— 权威取值集在 `mc-plugin-host::capabilities`。
4. 上游 digest 列（`plugin_package_version.digest` / `plugin_package_file.sha256`）是**纯 hex**
   （`char_length = 64` 的 CHECK），而 bundle 的**线上**形态带 `sha256:` 前缀（`SkillRef.hash`）；
   这条口径写进 `mc-core` 的类型文档，避免实现者把前缀写进列里。

### 9.4 依赖与工具链偏离（版本按**声明的 MSRV `1.80`** 选，不是「最新」）

| 依赖 | 落点版本 | 为什么不是更高 | 影响 |
| --- | --- | --- | --- |
| `zip` | `2.x` | zip 6.0.0 MSRV **1.83**、zip 8.6.0 MSRV **1.88**，都高于本仓声明的 `rust-version = 1.80` | 只能用 2.x 的 API（`ZipArchive` / `read_to_string`）；M6-3/M6-5 解包时注意 |
| `tower_governor` | `0.4.x`（**键名必须写 `tower_governor`**） | 0.8 的 `axum` 特征拉 axum **0.8** + tonic 0.14 ⇒ lock 里出现第二个 axum 大版本，`Body` 类型不兼容（`E0277`） | 限流层只能用 0.4 的 `GovernorLayer`（M6-7）。⚠️ rsproxy 索引把包名规范化为**下划线**：manifest 写 `tower-governor` 会在 resolve 期报 `no matching package named 'tower-governor'` |
| `serde_yaml` | `0.9`（解析为 `0.9.34+deprecated`） | 上游仓库已归档，无 1.x；0.9 是唯一的 0.9/0.8 分界 | 只用于 skill frontmatter（M6-2） |
| axum feature | `multipart` | — | 插件包上传（M6-5）需要；已加在根 `Cargo.toml` 的 `axum` 上 |

### 9.5 本锚点的门禁读数（逐字取自当轮日志）

- `local 325 registered | baseline 325`、`implemented 263 real + 0 placeholder = 263 / 456`、
  `known_gap 193`、`unclaimed 0`、`regression 0`、`local_only 9`、`gaps by owner: M6=57`。
- ⑦ 的两条**预期变化**（不是回归）：
  1. 4 个 M0 占位键（`GET|POST /api/skills`、`GET|POST /api/plugins`）被删 ⇒ 基线
     `329 → 325`，`implemented` 从 `263+2` 变成 `263+0`；
  2. `slash_alias_audit.py --declared docs/fixtures/m6-declared-routes.tsv` 从 `3 defect` 变
     **`5 defect`**：删掉 2 行 allowlist 豁免后，`/api/skills` 与 `/api/skills/:param` 的
     双形态欠账**如实暴露**给 M6-2（这正是本锚点要的效果 —— `slash-alias-allowlist.tsv` 现已**空**）。
- `crates/mc-plugin-protocol/` 已删（343 行的 stdio JSON-RPC，**零代码依赖者**：
  唯一外部引用只剩 `README.md` / `AGENTS.md` / 历史文档）—— 上游 `pkg/plugincontract` 是
  **声明式 manifest/bundle/capabilities 校验器**，不是 JSON-RPC（`docs/57` §9.3）。

### 9.6 M6-2 skill 读写面（`LUM-1667`）的偏离登记

**落点**：12 条上游路由 / **17 个注册键**（12 + 5 个尾斜杠双形态＝`/api/skills`(GET,POST)、
`/api/skills/:id`(GET,PUT,DELETE)）。**写集**（相对 M6-0 冻结基线的增量）：

| 文件 | 行数 | 内容 |
| --- | --: | --- |
| `mc-skill/src/frontmatter.rs` | 406 | 不可失败的 frontmatter 解析（+16 用例） |
| `mc-skill/src/binary.rs` | 113 | `is_likely_binary_file_path`（60 条扩展名黑名单） |
| `mc-skill/src/reserved.rs` | 125 | `is_reserved_content_path` + Go `filepath.Clean` 移植 |
| `mc-repos/src/skill/read.rs` | 262 | `SkillRepo` + 4 个行结构 + 6 个 SELECT |
| `mc-repos/src/skill/write.rs` | 322 | 建/改/删 + 文件/标签写（含唯一冲突 → 409） |
| `mc-http/src/routes/skills/helpers.rs` | 793 | 共享件：投影 DTO / `SkillScope` / ClawHub 客户端 |
| `mc-http/src/routes/skills/{crud,files,labels}.rs` | 322/188/158 | 三个 `router()` |
| `mc-http/tests/skills/**` | 1298 | e2e：`main.rs` + `support.rs` + 3 个用例文件（12 例） |

**`mod.rs` / `lib.rs` 冻结口径成立**：`mc-skill/src/lib.rs` 与 `skill/mod.rs`、`routes/skills/mod.rs`
在 M6-0 就已声明全部子模块，本片**零编辑**（写集只有上面这些文件 + 新增 `tests/skills/`）。

#### 偏离（M2-D1～M2-D17）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **M2-D1** | `parse_skill_frontmatter(&str) -> Frontmatter` **不可失败**（无 `Result`） | `mc-skill/src/frontmatter.rs` | 上游 `ParseFrontmatter` 在「围栏在但 YAML 非法」时返回 error，但**本波 12 条路由没有一条读 frontmatter**（读它的是 M6-3 的导入）。「非法 YAML 该拒还是该降级」的自由度留给**唯一调用方**；桩注释「围栏存在但 YAML 非法 = Err」是过期信息 |
| **M2-D2** | 只落 `is_likely_binary_file_path`，**没有** `is_likely_binary_content` | `mc-skill/src/binary.rs` | 上游 `internal/skill/binary.go` **不存在** `IsLikelyBinaryContent`（plan 里的假想 API）。调用方只有 M6-3 的导入路径；`put_file`/`create`/`update` 与上游一样**没有**二进制闸（桩注释「binary 必须跳过」是过期信息） |
| **M2-D3** | 保留路径名比较是 **ASCII** `eq_ignore_ascii_case` | `mc-skill/src/reserved.rs` | Go `strings.EqualFold` 是 Unicode 全折叠（`SKİLL.md` 这类会折叠成 `SKILL.md`）。差异面只有 Unicode 折叠，M6-3 复用**同一函数**即口径一致 |
| **M2-D4** | `SkillRepo` 落在 `read.rs`（私有 `db` + `impl RepoWithDb`），外部路径是 `mc_repos::skill::read::SkillRepo` | `mc-repos/src/skill/read.rs` | `skill/mod.rs` 冻结且**没有 `pub use`**；`write.rs` 用 `impl SkillRepo` 续写（同一类型、两个文件） |
| **M2-D5** | 事务走 `self.db().pool().begin()` | `mc-repos/src/skill/write.rs` | `mc-db::Db` 只有 `.pool()`，没有 tx helper；与 `mc-repos` 其他模块写法一致 |
| **M2-D6** | `sanitize_null_bytes` 在 `write.rs` **本地复制 3 行**（`replace('\0', "")`） | `mc-repos/src/skill/write.rs` | `property/validation.rs:181` 的同名函数是 `pub(crate)` 且 `property.rs:246` 是**私有** `mod validation;` ⇒ 从 `crate::skill::write` **不可达**；语义与上游 `SanitizeTextForPostgres` 逐字等价 |
| **M2-D7** | 写路径**不做** frontmatter / 二进制校验 | `routes/skills/crud.rs`、`files.rs` | 上游 `CreateSkill` / `UpdateSkill` / `UpsertSkillFile` 也只做 `validateFilePath` + 保留路径 + null 字节；多一道闸就会拒掉上游收下的请求 |
| **M2-D8** | `load_skill` 把 `RepoError::Db` 折成 **500**（其余 → 404） | `helpers.rs::SkillScope::load_skill` | 上游把 `GetSkillInWorkspace` 的任何错误都折成 404；本仓区分真库故障，避免「库挂了」被伪装成「skill 不存在」 |
| **M2-D9** | 404 错误体：本仓 `{"error":{"code","message":"not found: skill"}}` vs 上游 `"skill not found"` | `helpers.rs` + `routes/agents.rs::not_found` | 契约的 5 个 golden 只比**状态码**（`json_subset: {}`）⇒ 不判红；测试断言裸资源名 `"skill"` / `"skill file"` / `"skill label"`（同 `tests/agents` 既有形态） |
| **M2-D10** | 保留路径过滤在**路由层**（`supported_files`），仓储的 `upsert_file` **无过滤** | `routes/skills/helpers.rs` | `skill_file` 列上没有 CHECK；上游也是在 handler 里 `IsReservedContentPath` 跳过。⇒ M6-3 若直接调 `upsert_file` 要自己挡 |
| **M2-D11** | `SkillScope` 持 `Arc<AppState>` 克隆；`require_can_manage` **懒查**角色 | `helpers.rs` | 上游只有 `canManageSkill` 查成员；list / get / list_files 三条**零角色查询**（提前查会把「非成员读列表」从 200 变 404） |
| **M2-D12** | `query_escape` / `path_escape` **手写**（不新增 `url` 依赖） | `helpers.rs` | 逐字对齐 Go `QueryEscape`（空格→`+`、`+`→`%2B`）与 `PathEscape`（空格→`%20`、`/`→`%2F`）的保留集；`mc-http` 的依赖面不变 |
| **M2-D13** | 请求体**只接受对象或字面 `null`**，数组/标量 → 400 `invalid request body` | `helpers.rs::decode_body` | 上游 `json.Decode` 进结构体对 `[]` 报错，而 `serde` 给结构体派生的 visitor **能吃数组**（`[]` = 一个字段都没给）⇒ 必须显式判。**这是门 ⑤ 抓出的真 bug**（原实现 `[]` 会静默变成「全缺省」） |
| **M2-D14** | 创建/更新响应里的 `files` 是**请求体顺序**，`GET` 才是 `ORDER BY path ASC` | `routes/skills/crud.rs` | 上游 `createSkillWithFilesInTx` 边 upsert 边 append（`req.Files == nil` 那条分支才回落到 list 顺序）。⚠️ 桩注释「`PUT /files` 是整批替换语义」是**错的**：它只 upsert **一个**文件（`CreateSkillFileRequest{Path,Content}`）；整批替换只发生在 `PUT /api/skills/{id}`。**这是门 ⑥ 抓出的真 bug**（原用例假定创建响应有序） |
| **M2-D15** | 标签面：字段名 `label_id`、**不发** `label:updated`、`usage_count` 恒 0 | `routes/skills/labels.rs` | ① 桩注释写的 `labelId` 是错的；② 上游 `AttachLabelToSkill`/`DetachLabelFromSkill` 会 publish 事件，本仓 M6-2 **不接事件总线** ⇒ 登记为**行为缺口**（hook/job 面 M6-8 若需要再补）；③ `usage_count` 上游 `labelToResponse` 本来也是 0 |
| **M2-D16** | UUID 错误文本沿用上游字段名（`skill id` / `label_id` / `label id` / `file id`），错误码走本仓 `mc-errors`（400 `validation`） | `helpers.rs`、`files.rs`、`labels.rs` | 上游 `parseUUIDOrBadRequest` 是 400 + 明文；本仓统一信封，状态码一致 |
| **M2-D17** | 工具函数改名为 `to_new_skill` / `with_files_dto(&SkillWithFiles)` | `routes/skills/*` | 只为过 `clippy::wrong_self_convention`（`into_*` 要求 `self`）与 `needless_pass_by_value`；plan 里的 `into_new_skill` 不落地 |

**`skill_to_label` 无外键（迁移 `162`）** ⇒ 删 skill 时**必须**显式删关联行（repo `delete` 的
事务里做）；`skill_file` / `agent_skill` 有 `ON DELETE CASCADE`，不用管。

#### 本片纠正的过期文档（`mc-skill/src/lib.rs` 等仍是 M6-0 桩口径）

| 位置 | 桩写的 | 实际 |
| --- | --- | --- |
| `mc-skill/src/lib.rs` 头 | 「本 crate 现在没有任何公开类型」 | 本片落了三组 `pub fn`（frontmatter / binary / reserved） |
| `routes/skills/files.rs` 头 | 「PUT 是整批替换语义」+「binary 必须跳过」 | 单文件 upsert；二进制闸只在 M6-3 的导入路径 |
| `routes/skills/labels.rs` 头 | 请求体 `{"labelId": …}` | `{"label_id": …}` |
| `routes/skills/mod.rs` 头 | 「14 个注册键 = 12 + 2」 | **17**（12 + 5 双形态；`/api/skills/search` 无双形态） |
| `mc-skill/src/frontmatter.rs` 头 | 「围栏存在但 YAML 非法 = Err」 | 见 M2-D1（不可失败） |
| `mc-repos/src/skill/mod.rs` 头 | 「手写 `sqlx::FromRow`」 | 本片用裸 `Uuid` 字段 + `#[derive(FromRow)]` 行结构（`Id` 仍无 sqlx impl，故行结构不用 `Id`） |

#### 本片门禁读数（逐字取自当轮日志）

- ⑦：`upstream 456 | local 361 registered | baseline 344`、`implemented 285 real + 0 placeholder`、
  `known_gap 171`、`unclaimed 0`、**`regression 0`**、`local_only 9`（`+17` = 12 条上游路由 + 5 个双形态键）。
- `slash_alias_audit.py`：**0 defect**，且 `docs/fixtures/slash-alias-allowlist.tsv` **未新增行**（仍空）。
- ⑨ 契约：committed `report.json` **逐字未变**（5 个 skills fixture 都是 stateless 层 ⇒ `unevaluable`）；
  本地真库层复跑 `mc-conformance --filter skills --db-url …` ⇒ `fixtures 5 · pass 5 · mismatch 0 ·
  unevaluable 0`（`tiers.database.pass = 5`、`contract_equivalence_rate = 1.0`）——含 004 的**真实
  clawhub.ai 出站**（`q=react` ⇒ 200）。
- e2e：`tests/skills` **12 例全绿**（真库；`--test-threads=4`，22s）。

### 9.7 M6-3 skill 导入/刷新生态（`LUM-1668`）的落点与偏离登记

**落点**：2 条上游路由 / **2 个注册键**（`POST /api/skills/import`、`POST /api/skills/:id/refresh`；
两者上游都**没有**尾斜杠形态 ⇒ ⑦ `local` 只 +2）。**写集**（相对 M6-2 合并后基线的增量）：

| 文件 | 行数 | 内容 |
| --- | --: | --- |
| `mc-skill/src/source.rs` | 417 | 源判定：`detect_import_source` / `parse_clawhub_slug` / `parse_skills_sh_parts` / `parse_github_url`（**不是** `git.rs`，上游没有克隆路径） |
| `mc-skill/src/archive.rs` | 622 | multipart zip 解包：4 个上限（1 MiB/文件、8 MiB/包、256 文件、16 MiB 上传）+ `SKILL.md` frontmatter 取名 + 包装目录回落 |
| `mc-repos/src/skill/import.rs` | 504 | 整包一个事务：`create_imported` / `create_renamed_imported`（后缀 `2..52`）/ `overwrite_imported`（creator-only 与 creator-or-admin 两种策略）+ `ConflictStrategy` |
| `mc-http/src/routes/skills/import.rs` | 592 | `ImportSkill` 的形状编排（JSON/multipart 分支、四策略、结构化 vs 旧形态） |
| `mc-http/src/routes/skills/import/fetch.rs` | 571 | `ClawHub` / skills.sh 抓取 + 原始文件下载 + 端点覆写 + 错误分类（413/504/503/502） |
| `mc-http/src/routes/skills/import/github.rs` | 451 | `api.github.com` 仓库/引用/tree + raw 支撑文件下载 + `Bearer` 闸门 |
| `mc-http/src/routes/skills/import/github/tree.rs` | 363 | tree 遍历与「哪些条目算 skill 文件」的判定 |
| `mc-http/src/routes/skills/import/github{,/tree}/tests.rs` | 42/112 | 上面两块的单测 |
| `mc-http/src/routes/skills/refresh.rs` | 281 | `RefreshSkill`：`parse_skill_origin` + `fetch_imported_skill_from_origin` + `merge_skill_config_origin` |
| `mc-http/tests/skills/{import,refresh,zipfixture}.rs` | 612/353/108 | e2e：导入 8 例、刷新 3 例、手写 zip 夹具（store 法 + 自算 CRC32）+ 1 条夹具自检 |
| `mc-http/tests/skills/{main,support}.rs` | +7/+272 | 模块登记 + mock（`ClawHub` / GitHub api / GitHub raw / multipart 构造 / 端点覆写闸锁） |

**冻结点零编辑**：`routes/skills/mod.rs`（M6-0 已 `merge(import::router(), refresh::router())`）、
`routes/{mod,mount}.rs`、`state.rs`、`mc-skill/src/lib.rs`、`mc-repos/src/skill/mod.rs`、根
`Cargo.{toml,lock}`、⑦ 基线、`slash-alias-allowlist.tsv` **全部零改动**（本片未加任何依赖：
`reqwest`/`zip`/`serde_json` 都已在 `mc-http` 的清单里；`url` **没有**也**不新增**，见 M3-D7）。

**文件拆分登记**（`docs/57` §5 的落点已细化，`mod` 声明由各自父文件承担，`routes/skills/mod.rs` 仍零编辑）：
`routes/skills/import.rs` 内的 `mod fetch; mod github;` ⇒ `import/{fetch.rs, github.rs, github/tree.rs}`，
单测随文件（`github/tests.rs`、`github/tree/tests.rs`）；`routes/skills/refresh.rs` 保持单片（281 行）。

#### 偏离（M3-D1～M3-D12）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **M3-D1** | GitHub 抓取是**自建临时实现**（`SkillSourceFetcher` port + `HttpSkillSourceFetcher`）；**跳过**「tree truncated 再逐层 crawl」的兜底 | `import/github{,/tree}.rs` | 上游 `fetchFromGitHub` 对 `truncated` 的 tree 会回落到「按需列目录」的多次 API 调用。本仓**不落**这条兜底：`truncated=true` 或 tree 请求失败 ⇒ `ImportFailure::Unavailable`（**503**，可重试），**绝不落半份包**。行为差异面 = 「超大仓库」这一个分支；port 是本片的正式交付物（写进 issue 的 DoD），W8 的 GitHub 客户端就位后由它实现该 port |
| **M3-D2** | 源判定错误（`empty URL` / 不支持的域名 / 坏 slug）是 **400**，取件期错误才是 502/503/504/413 | `routes/skills/import.rs`、`import/fetch.rs` | 逐字对齐上游：`detectImportSource` 在**取件之前**返回错误，`ImportSkill` 用 `writeError(400, err)`；只有 `fetchFrom*` 的失败才走 `importFetchErrorResponse` |
| **M3-D3** | multipart 路径的上限违例是 **400**，URL 路径才是 **413** | `routes/skills/import.rs`、`mc-skill/src/archive.rs` | 上游 `importSkillFromArchive` 对 `ParseMultipartForm`/解包错误一律 `writeError(400, err.Error())`；413 只属于 URL 路径的 `errImportCapExceeded`。**两条路径的 cap 语义是「整包失败」**（不截断、不降级） |
| **M3-D4** | 错误体是**扁平** `{"error":"…"}`，且 refresh 的 404 文案带本仓前缀（`not found: skill`，上游是 `skill not found`） | `routes/skills/{import,refresh}.rs` | 与 M2-D9 同源：本仓 `mc_errors::Error::Display` 会加内部前缀，而本片两个端点选了「扁平体」这一种出口（`helpers::upstream_unavailable` 已是同款先例）。状态码逐条对齐（400/403/404/409/413/422/502/503/504）。**golden 契约只比状态码** ⇒ 不判红 |
| **M3-D5** | JSON 请求体加了 **1 MiB** 上限（上游不设限） | `import.rs::JSON_IMPORT_BODY_LIMIT` | 防无上限读内存；超限报 400 `invalid request body`。另：**16–25 MiB** 的归档体在到达 `Multipart` 之前被全局 `RequestBodyLimitLayer`（25 MiB）截断 ⇒ 400，**>25 MiB** ⇒ 413（层自己回）——上游只有 16 MiB 这一道 |
| **M3-D6** | 45s 总超时由 `tokio::time::timeout` 产生，折成 `ImportFailure::Timeout` | `import/fetch.rs` | 上游用 `context.WithTimeout` + `errors.Is(err, context.DeadlineExceeded)`。**状态码（504）与文案逐字不变**；单请求仍是 30s（上游 `http.Client.Timeout`） |
| **M3-D7** | `host_of(url)` 手写（**不新增 `url` 依赖**）；`Bearer` 令牌只在 host 是 `raw.githubusercontent.com`（或测试覆写的端点）时加 | `import/fetch.rs`、`import/github.rs` | 上游用 `net/url` 的 `URL.Hostname()` + `strings.EqualFold`。手写实现覆盖「scheme://host[:port]/…」这唯一形态（大小写不敏感、去端口）；**只影响 URL 解析边角**，不影响请求路径 |
| **M3-D8** | 保留路径（`SKILL.md` 等）过滤放在**请求映射**（`imported_skill_file_requests`），两条分支都过 | `routes/skills/import.rs` | 上游只在 `createSkillWithFilesInTx` 里 `IsReservedContentPath` 跳过，`overwriteSkillWithFiles` **不过滤**。本仓两条都过滤（**更严**：`SKILL.md` 永远只进 `skill.content`，不会额外进 `skill_file`） |
| **M3-D9** | `mc-skill` 内自带 `validate_archive_file_path`；`import.rs` 里 `sanitize_null_bytes` 是 3 行副本 | `mc-skill/src/archive.rs`、`routes/skills/import.rs` | 依赖方向冻结：`mc-skill` **不能**依赖 `mc-http`（`helpers::validate_file_path` 在 `pub(crate)` 且方向相反）。`sanitize_null_bytes` 同 M2-D6 的理由；`with_files_dto` 的 3 行副本是因为 `crud.rs` 那份是私有函数，而 `helpers.rs` 已 **793/800 行**（门 ⑩ 硬限内没有余量放共享件） |
| **M3-D10** | 归档/出网内容一律 `String::from_utf8_lossy`（非 UTF-8 字节被替换为 U+FFFD）；JSON 路径下的 body 也按 lossy 文本进库 | `mc-skill/src/archive.rs`、`import/fetch.rs` | `skill.content` / `skill_file.content` 是 `TEXT`，上游 Go 的 `string(bytes)` 保留原始字节（PG 在非 UTF-8 时会在写入期报错）。本仓选择「降级为 U+FFFD」而不是把整包判废（导入不该因为一个二进制附件而全废）；`is_likely_binary_file_path`（M2-D2）仍是调用方的可选闸 |
| **M3-D11** | `ImportError` / `ImportFailure` / `SourceError` 落在 **`mc-skill`**，不进 `mc-core` | `mc-skill/src/{archive,source}.rs` | `mc-core` 是公共底座，塞导入错误类型会把「zip/出网」的语义扩散到所有 crate（同 9.3 第 1 条的理由） |
| **M3-D12** | 不广播 WS 事件；测试用**进程级**端点覆写（`set_source_endpoints`，`test-util` 门控）代替上游的包级变量 | `import.rs`、`tests/skills/support.rs` | 事件总线缺口同 M2-D15（M6-8 的 hook/job 面若需要再补）。端点覆写是全局单值 ⇒ 用 `support::MOCK_LOCK`（`tokio::sync::Mutex`）串行所有 mock 用例；e2e 必须带 `--features mc-http/test-util` |

#### 本片纠正的过期口径（桩注释 / 文档）

| 位置 | 原写 | 实际 |
| --- | --- | --- |
| `routes/skills/import.rs`（M6-0 桩） | 请求体字段 `source` | 上游 json tag 是 **`url`**；`on_conflict` 可选 |
| `routes/skills/refresh.rs`（M6-0 桩） | 「没有 origin ⇒ 400」 | **422**（`errSkillNotRefreshable`）；且空 `source_url`、类型与 URL 不匹配、不可刷新类型（`archive`/`runtime_local`）**都折进同一句** |
| `routes/skills/import.rs`（M6-0 桩） | 「`PUT /files` 是整批替换」一类旧口径的连带假设 | 见 M2-D14；本片 `finishSkillImport` 的两条形态都按上游「整包 upsert + 删旧文件」 |
| `docs/57` §5 的 `mc-skill/src/git.rs` | 建 `git.rs` | 上游**没有** git 路径 ⇒ 文件叫 `source.rs`（9.2 第 3 行同一条） |
| 本 issue 的 DoD | 「⑨ 本片的 fixture 离开 `unevaluable`」 | **本片两个端点在上游 fixture 里一条都没有**（`report.json` 全文 grep `skills/import` = 0 命中）⇒ ⑨ 的**预期变化就是 0**，不是欠账 |

#### 本片门禁读数（逐字取自当轮日志，`fc4971c` + 本片工作树）

- `bash scripts/gates.sh --with-db` ⇒ **10/10**（交付树的最后一次全量热跑 **109s**：
  ①1s ②1s ③0s ④0s ⑤34s ⑥42s(migrate=0,e2e=0) ⑧25s ⑦0s ⑨5s ⑩1s；同一棵树的首次冷跑 **296s**，
  含 ⑥ 自己的一次 `mc-migrate run`）。
- ⑦：`upstream 456 | local 363 registered | baseline 344`、`implemented 287 real + 0 placeholder`、
  `known_gap 169`、`unclaimed 0`、**`regression 0`**、`local_only 9`（**+2**，基线**未刷**——
  刷新一次性归 M6-INT `LUM-1675`）。
- `slash_alias_audit.py`：**0 defect**（本片两条键都没有尾斜杠形态，也没有新增 allowlist 行）。
- ⑨：`crates/mc-conformance/report.json` **逐字未变**（`pass 5 / mismatch 23 / unmounted 31 /
  placeholder 0 / unevaluable 306`，`fixtures 365`）—— 见上表最后一行。
- ⑤ + ⑥ 合计：**2016 passed / 176 ignored**（⑤ 1600/176、⑥ 416/0；M6-2 自报基线 1556 + 405 ⇒
  +44/+11 = 本片新增单测 44 例 + e2e 11 例）。
- e2e：`tests/skills` **23 例全绿**（真库 `mc_lum1668`；`--test-threads=4`，21.4s），
  其中本片新增 11 例（导入 8 + 刷新 3）+ 1 条 zip 夹具自检（非 ignored）。

### 9.8 M6-6 插件运行时面（`LUM-1671`）的落点与偏离登记

**落点**：**4 个注册键**（`GET …/invocations`、`GET|PUT …/mcp/{hookKey}/tools`、
`GET …/surfaces/{surfaceKey}/launch`；都在 `routes/plugins/mod.rs` 里由 M6-0 冻结、逐字对齐
`docs/fixtures/upstream-routes.tsv:425-428`，**无**尾斜杠形态 ⇒ ⑦ `local` 只 +4）。
上游对照：`internal/handler/plugin_mcp.go`（159）+ `plugin_hook.go` 的 invocations 段（≈30）+
`service/plugin_mcp_transport.go` 前半（≈150）+ `plugin_surface.go` 的 launch 段（≈183）。

| 文件 | 行数 | 内容 |
| --- | --: | --- |
| `mc-http/src/routes/plugins/mcp.rs` | 608（非测试 447） | 3 条路由 + 门（`load_installation`）/ 发现（`discover_hook_tools`）/ 采纳钉定（`pin_tools`）/ DTO（`invocation_payload`、`mcp_tool_payload`）；5 条纯判定单测 |
| `mc-http/src/routes/plugins/surface_launch.rs` | 663（非测试 473） | 1 条路由 + origin 解析与专用判定 + `SurfaceLaunchClaims` / `mint_surface_launch` / `open_surface_launch_claims`（M6-7 复用）；6 条纯判定单测 |
| `mc-http/tests/plugins/{runtime,runtime_surface,runtime_support}.rs` | 579/314/133 | 真库 e2e 10 例（4 条路由各有用例）+ 共用夹具（拆三个文件是门 ⑩ 的 800 行硬限逼出来的） |
| `mc-repos/src/plugin/mcp_approval.rs` | +74 | `mcp_approvals()` 只读投影 + `secret_ciphertext` 窄读垫片（见 M6D-10）+ 1 条真库用例 |
| `mc-repos/src/plugin/invocation_read.rs` | ±1 | 夹具 `created_at` 绑定改 `$4::timestamptz`（见下「纠正」） |
| `mc-http/src/routes/plugins/install.rs` | +21/−8 | 四处可见性放开（见 M6D-11），无行为改动 |
| `mc-http/tests/plugins/{main,support}.rs` | +12/+18 | 模块登记 + `app_surface(db, key, origin)` 夹具（`plugin_surface_origin` 也由测试说了算） |

**冻结点零编辑**：`routes/plugins/mod.rs`、`routes/{mod,mount}.rs`、`state.rs`、
`mc-plugin-host/**`、`mc-mcp/**`、`routes/surfaces.rs`、根 `Cargo.{toml,lock}`、⑦ 基线、
`slash-alias-allowlist.tsv` **全部零改动**（本片**未加任何依赖**：`mc-mcp` / `mc-plugin-host`
两条边由 M6-0 anchor 就位）。

#### 偏离（M6D-1～M6D-12）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **M6D-1** | `plugin_surfaces_not_configured` 是 **503**，上游是 **403** | `surface_launch.rs::surfaces_not_configured` | 上游 `writeFeatureDisabled` 落 `writeErrorCode(w, http.StatusForbidden, …)`（`handler.go:578`）。本仓口径取自 **M6-0 anchor 自己写的 `state.rs` 文档**（两处）、`routes/surfaces.rs` 的桩、`plugin_bridge/hooks.rs` 的 `plugin_disabled`（同款 503）以及本 issue 的 DoD —— 四处一致 ⇒ 按 503 落地。**码与文案逐字不变**，只有状态码这一位不同；⑨ 的 365 条 fixture 里没有任何一条打这个端点（全文 grep = 0 命中）⇒ 不判红 |
| **M6D-2** | `mcp_approvals` 存**窄形态**（`name` + `schema_digest`），不是上游整个 `remotemcp.Tool` | `mcp.rs::pin_tools` | 迁移 `369` 的注释与 `mc_core::plugin::PluginApprovedTool` 的头表写的就是这个形状；少掉的 `description` / `inputSchema` 只用于展示，而比对（本文件唯一关心的语义）只用这两个字段。**不新增列、不改迁移** |
| **M6D-3** | 暴露 `?limit=&offset=`（上游 `LIMIT 100` 硬编码、**无查询参数**） | `mcp.rs::page_params`、`mc-repos/src/plugin/invocation_read.rs` | 本 issue 的 DoD 要求「空页 / 越界 offset」两条边界可测 ⇒ 必须有分页能力。缺省 `100` / `0` 与上游行为逐字相同，上限 **500**（上游没有参数也就没有上限；加了参数就必须有）。查询串**手解**而不是 `Query<T>`：后者的拒绝体是 axum 的纯文本，会与插件面的 `{"error":{"code","message"}}` 信封不一致；拼错/非法值回缺省而**不是** 400（分页是能力，不是拒服务的理由） |
| **M6D-4** | 单条 `UPDATE`（`mcp_approvals \|\| jsonb_build_object($2,$3::jsonb)` / `- $2`）代替上游「读-改-写 + 整块 `json.Marshal`」 | `mc-repos/src/plugin/mcp_approval.rs::set_hook_approval` | 读-改-写让两个管理员**同时**批准**不同** hook 时可能丢掉一个（read→write 窗口内的更新被整块覆盖）；单条语句语义完全等价（整块替换该 hook 的值、删掉该 hook 的键），却天然没有这个窗口。`updated_at = now()` 与上游 `SetPluginMCPApprovals` 逐字相同；`RETURNING` 让「安装已消失」照上游落成 `NotFound`。⚠️ 与 M6D-2 合并看：**整块写入仍是 per-hook 的**，其它 hook 一个字节都不动 |
| **M6D-5** | 排序在 `created_at DESC` 之后**追加 `id DESC`** 作次键 | `mc-repos/src/plugin/invocation_read.rs::list` | `created_at` 默认 `now()`，同事务插入的行时间戳**完全相同**，只按它排会让本片新增的 `OFFSET` 在并列行上跳过或重复（上游没有分页所以看不见这个问题） |
| **M6D-6** | `parse_plugin_surface_origin` **手写**，不新增 `url` 依赖 | `surface_launch.rs` | 依赖边冻结在 M6-0 anchor（「此后 M6 各切片不再改本 manifest、`Cargo.lock` 只在 anchor 重生成」）。两处比 Go `url.Parse` **更严**：空端口（`https://host:`）被拒；主机名按字符表校验（含 IPv6 方括号形态） |
| **M6D-7** | 「专用 origin」的候选退化为「本进程 host（配了端口就连端口）」+「本次请求的 `Host` 头」 | `surface_launch.rs::surface_origin_is_dedicated` | 上游候选是 `cfg.PublicURL` / `cfg.AppURL` / `cfg.AttachmentFrameAncestors`；本仓**没有**这三个配置面（`ConfigSnapshot` 只有 `host`/`port`，`mc_config::ServerConfig::external_url` 没有被 anchor 接进 `AppState`，而 `state.rs` 是冻结文件）。**更严**的一面：同名的任意端口都算非专用、未写明端口折成 scheme 默认端口（Go 比 `Host` 字符串，不折）。**更宽**的一面：经别名（CNAME / 另一个 host）访问的 app origin 认不出来 |
| **M6D-8** | `SurfaceLaunchClaims` / `mint_surface_launch` / `open_surface_launch_claims` **落在本片**（不是 M6-7） | `surface_launch.rs` | 上游 `pluginSurfaceLaunchClaims` 的类型与校验在 `plugin_surface.go` 里签发与承载**共用一份**。本仓由 M6-6 出、M6-7 **复用**（`routes/surfaces.rs` 在同一个 crate 里，`pub` 可见），避免两份 claims 契约漂移 —— `mc-plugin-host::credentials` 的注释原写「claims 的类型与校验归 M6-7」**已由本条更正**。承载段（Host 边界校验、cookie/Authorization 拒绝、CSP、HTML 文档渲染）**完全**归 M6-7，本片一行不写 |
| **M6D-9** | dev-origin 策略在 **route 层**读 `MULTICA_PLUGIN_DEV_ORIGINS` / `MULTICA_PLUGIN_DEV_CA` | `mcp.rs::endpoint_policy` | `mc_mcp::devorigin` 按自己的头注「env 名字只导出、读值集中到入口层」；而 `AppState` 没有该字段、`state.rs` 又是冻结文件 ⇒ 入口层就是本 route。与 `packages.rs` 逐请求读 `MULTICA_PLUGIN_DIR` 同款先例。**未设置 ⇒ 空白名单 / 无额外 CA**，与上游「没配就是没有」同判 |
| **M6D-10** | 三处**窄读垫片**落在 `mc-repos/src/plugin/mcp_approval.rs`（不是 `installation.rs` / `package.rs`） | `installation_for_workspace` / `package_file_sha256` / `secret_ciphertext` | 那两个文件归 M6-5（本片不能改），而 `plugin_secret` 在整个 `plugin/*` 模块里**只有写侧**（`upsert_secret_tx` / `delete_*`）。窄读只取「装了什么版本、管理员同意了什麼、manifest 快照、开关、一个文件的 sha256、一个 secret 的密文」——**故意不取** `config` / `token_hash`。收敛点（合并进 M6-5 的仓储）登记给 M6-INT（`LUM-1675`） |
| **M6D-11** | `install.rs` 四处可见性放开：`parse_installation_manifest`（新抽）/ `installation_payload` / `installation_repo` / `deployment_key` → `pub(super)` | `routes/plugins/install.rs` | 本片要复用 ①「manifest 不可读」那一句文案 ②安装行 DTO（`PUT tools` 的响应就是上游 `pluginInstallationPayload(updated)`） ③部署密钥的**唯一转写点**。各写一份等于把同一句话/同一个转写点变成两处。**无行为改动**（`installation_manifest` 改为转调 `parse_installation_manifest`，行为逐字相同） |
| **M6D-12** | `workspace_mcp_api.go` 的台账段与 `mcp_overlay.go` **不在本片** | —— | 本 issue 的动作清单把两者列为参考面，但 `/api/workspaces/:id/mcp-servers` 四条（`router.go:1686/1710-1712`）在 `docs/fixtures/upstream-routes.tsv:406-409` 里归属 **M8**；`mcp_overlay.go` 是 **per-task agent** overlay（`mcp_config` 合并），也是 M8 面。本片只做 `plugin_*` 的 3 条 + surface 的 1 条（`docs/57` §8 R-M6-5 已把「只做 workspace 级」写死） |

#### 本片纠正的过期口径

| 位置 | 原写 | 实际 |
| --- | --- | --- |
| `mc-plugin-host/src/credentials.rs`（M6-1 注释） | 「claims 的类型与校验归 M6-7」 | 归 M6-6（签发侧；`docs/57:519` 也是这么分的），M6-7 复用 `open_surface_launch_claims` —— 见 M6D-8 |
| `docs/57` §7 M6-7 行（`:520`） | surface token 的「篡改/过期/错域三种拒绝」是 **M6-7** 的 DoD | 本 issue 的 DoD 把它给了 M6-6。实际：三条拒绝在**本片**的 `surface_launch.rs` `mod tests` 里逐条断言（篡改 = GCM 认证失败；过期 = `expires_at <= now`；错域 = 另一把部署密钥、或同一把密钥但**没有**域分离标签的那个盒子）；M6-7 的承载段只多 Host 边界与凭据拒绝 ⇒ 两处描述都已满足，**不重复实现** |
| `mc-repos/src/plugin/invocation_read.rs`（M6-6 抢救产物） | 夹具把 `created_at` 当 `&str` 绑进 `VALUES (…, $4)` | **PG 会报 42804**（`column "created_at" is of type timestamp with time zone but expression is of type text`）—— 该文件的 3 条用例是本片**第一次**接真库跑，一跑就红。改为 `$4::timestamptz`。**这就是「新增测试必须接真库跑过再推」那条纪律的又一例**（M6-5 栽过同款） |
| 桩注释的行预算 | `mcp.rs` ≤380 / `surface_launch.rs` ≤220 | 非测试 447 / 473。超出的是落地说明（4 条 / 4 条）+ 门·发现·采纳三段的分层注释（上游对应函数各自有一段「为什么」）。门 ⑩ 的 800 行硬限内（608 / 663），**不为凑数字砍注释** |

#### 本片门禁读数（逐字取自当轮日志；`bash scripts/gates.sh --with-db`）

- **10/10 green，144s**（热跑；①2s ②6s ③12s ④0s ⑤35s ⑥58s ⑧26s ⑦0s ⑨5s ⑩0s）。
  ⚠️ 同一棵树的首轮全量跑**先红了一次**，红的**不是代码**：`/` 只剩 5.0G，`cargo` 在
  `mc-conformance` 的指纹目录上报 `ENOSPC`，于是 ②③④⑤⑥⑨ 一起「红」。回收自己 workdir 的
  `target/debug/incremental`（1.7G）后重跑即全绿 —— **ENOSPC 会伪装成门禁红**（`docs/37` 已记过）。
  逐门 `--only` 跑完再全量复跑的读数与上表一致。
- ⑦：`upstream 456 | local 386 registered | baseline 344`、`implemented 310 real + 0 placeholder`、
  `known_gap 146`、`unclaimed 0`、**`regression 0`**、`local_only 9`；`owners.M6` **24 → 20**
  （本片 +4 逐条落在 M6 名下）。**基线未刷** —— 一次性刷新归 M6-INT（`LUM-1675`）。
- `slash_alias_audit.py`：**0 defect**（本片 4 条键都没有尾斜杠形态，也没有新增 allowlist 行）。
- ⑤ + ⑥：**2119 passed / 198 ignored**（⑤ 1648/198、⑥ 471/0；本片新增 ⑤ **11** 例纯判定单测
  → `mcp.rs` 5 + `surface_launch.rs` 6、⑥ **17** 例 = `tests/plugins` 的 e2e **10** 例
  （`runtime.rs` 6 + `runtime_surface.rs` 4）+ `mc-repos::plugin` 的 7 例真库用例）。
- ⑨：`crates/mc-conformance/report.json` **逐字未变**（`pass 5 / mismatch 23 / unmounted 31 /
  placeholder 0 / unevaluable 306`，`fixtures 365`）—— 本片 4 条端点在上游 fixture 里
  **一条都没有**（`runtime.rs` 的 30 例是自建真库 e2e）⇒ ⑨ 的**预期变化就是 0**，不是欠账。

### 9.9 M6-7 公开 Action API + bridge + surface（`LUM-1672`）的落点与偏离登记

**写集**（与 issue 的枚举一字不差）：`routes/v1/{context,issues,storage,policy}.rs`、
`routes/plugin_bridge/{context,issues,storage}.rs`、`routes/surfaces.rs`、
`mc-repos/src/plugin/storage.rs`，外加 `routes/v1/policy/caller.rs`（门 ⑩ 的拆分，见 D1）与
`crates/mc-http/tests/public_api/**`（e2e）。**冻结面零编辑**：`routes/{mount,mod}.rs`、
`routes/v1/mod.rs`、`routes/plugin_bridge/{mod,hooks}.rs`、`routes/plugins/**`、`state.rs`、
根 `Cargo.{toml,lock}`、⑦ 基线、⑨ `report.json`、`slash-alias-allowlist.tsv`。

#### 偏离（M6-7-D1～D8）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **M6-7-D1** | `policy.rs` 拆成 `policy.rs` + `policy/caller.rs` | `routes/v1/policy/` | 门 ⑩ 单文件 800 行硬上限（原 837 行）。两者同属 `routes::v1::policy` 模块树，调用方仍写 `policy::resolve_caller(...)`；`v1/mod.rs`（锚点冻结）不需要 `mod` 声明（父模块 `policy.rs` 自己声明） |
| **M6-7-D2** | 中间件层必须用 **`route_layer`**，不是 `layer` | `routes/v1/policy.rs` | `Router::layer` 连 **fallback** 一起包 ⇒ 未匹配的路径（尾斜杠别名、纯 404）被凭据门提前变成 **401**。实测把 `tests/autopilots/{deliveries,execution,triggers}` 的 3 条「单形态路由必须 404」用例打红；同一处也让 ⑨ 的两条 anonymous fixture 从 `unmounted` 翻成 `pass`（假绿）。`route_layer` 只包已声明的路由，行为与上游「中间件挂在路由组上」一致 |
| **M6-7-D3** | `plugins_v1` 的本地口径沿用 M6-5 的**反转**（未登记 = 开启） | `policy/caller.rs` | 同 M6-5-D1 的理由：本仓 `FeatureFlagCatalog` 无持久化后端（唯一的 `register` 调用点在测试里），照抄上游默认 `false` 会让 `/v1` 面**永远** 403。`contracts/golden/context/001` 在上游是**测试里显式关掉开关**拿到的 403 ⇒ 本仓以「显式登记 false ⇒ 403」逐字满足（`tests/public_api/guard.rs` 同名用例） |
| **M6-7-D4** | 回调令牌表（`CallbackTokens`）落在 `routes/v1/policy.rs` 的**进程内静态** | `policy.rs::callback_tokens()` | `state.rs`（锚点冻结）没有这个字段，而**两片**需要同一张表（M6-8 的 hook 派发签发、本片解析）。⚠️ **M6-8 必须复用 `mc_http::routes::v1::policy::callback_tokens()`**，不要再 new 一张 |
| **M6-7-D5** | 安装令牌的 `token_hash` 读口用直查 | `policy/caller.rs::authenticate_install_token` | `mc-repos/src/plugin/installation.rs` 是 M6-5 的写集（没有按 token_hash 的读口，且本片不得编辑它）⇒ 复用那个文件**公开的** `installation::COLUMNS` 直查一次，列投影仍只有一份 |
| **M6-7-D6** | `comment.via_plugin_id` 用一条后置 `UPDATE` 补写；`comment.type` 用一次后置查询补齐 | `routes/v1/issues.rs` | M2 的 `NewComment` 没有 `via_plugin_id`、`CommentRow` 的列投影没有 `type`（`mc-repos/src/comment.rs` 是本片写集外）⇒ 复用 `CommentRepo::create`（DoD 第 4 条：不新写评论服务）之后补一条单列 UPDATE / 一次 `SELECT id, type`。跨片缺口：M2 补齐列投影后应删掉这两段 |
| **M6-7-D7** | 公开契约的 `author_type` 归一：`user`/`member` ⇒ **`member`** | `routes/v1/issues.rs::public_author_type` | M2 对「人」写 `'user'`（迁移 `538` 的 CHECK 同时放行 `user`/`member`），上游写 `'member'`。公开契约只有一份拼写，折成上游的；写侧仍是 M2 的口径 |
| **M6-7-D8** | `ServePluginSurface` 的「未配置」是 **404**（不是 503）；Host 边界判定在 handler 内 | `routes/surfaces.rs` | ① M6-0 桩注释写「origin/密钥缺 ⇒ 503 `plugin_surfaces_not_configured`」，那是 **launch 段**（M6-6）的错误码；上游 serve 段对 origin 解析失败一律 `http.NotFound`。② 上游把 `PluginSurfaceHostBoundary` 挂在**整台 router** 上；本仓的挂载点在锚点冻结的 `mount.rs` 里 ⇒ 由 handler 内同一个判据承担，差别只在「内容主机上的 `/api/health` 会落到全局 router」这一条（M6-INT 可决定是否把该中间件提到 `main.rs`） |

#### 跨片接口（M6-8 / M6-INT 请照用）

- `mc_http::routes::v1::policy::callback_tokens()` —— 进程内回调令牌表（M6-8 签发侧的唯一入口）。
- `mc_http::routes::surfaces::SURFACE_LAUNCH_TTL_SECS` = 120 —— 「2 分钟 TTL」的唯一真值。
- `mc_http::routes::surfaces::SurfaceLaunchClaims`（`pub(crate)`）—— 令牌声明的**唯一**形状；
  M6-6 的 launch handler 序列化它（`workspace_id` / `installation_id` / `version_id` /
  `surface_key` / `digest` / `challenge` / `expires_at`）。
- `mc_repos::plugin::storage::{StorageRepo, StorageError, SCOPE_*, MAX_*}` —— 存储面唯一实现点。

#### 本片门禁读数（逐字取自当轮日志）

- `bash scripts/gates.sh --with-db` ⇒ **10/10**（`383s`；①1s ②92s ③37s ④29s ⑤52s ⑥138s ⑧25s ⑦1s ⑨8s ⑩0s）。
- ⑦：`upstream 456 | local 401 registered | baseline 344`、`implemented 325 real + 0 placeholder`、
  `known_gap 131`、`unclaimed 0`、**`regression 0`**、`local_only 9`（基线**未刷** —— 归 M6-INT）。
  本片 **+19**（382 → 401），`owners.M6` **24 → 5**。
- ⑨：`crates/mc-conformance/report.json` **逐字未变**（`pass 5 / mismatch 23 / unmounted 31 /
  placeholder 0 / unevaluable 306`，`fixtures 365`）。本片的 6 条 plugin fixture（`context/001` +
  `issues/09x`×5）actor 全是 **member** ⇒ stateless 层结构性不可判（`Tier::Stateless.supports`
  只认 anonymous），committed 报告不可能变；DB 层实测它们已从 `unmounted` 变**已判**（`401`，
  见下）。
- e2e：`tests/public_api` **40 例全绿**（真库 `mc_lum1672`）；`--features mc-http/test-util`。

#### 登记的已知缺口（给 M6-INT）

1. **⑨ 的 6 条 plugin fixture 在 DB 层判 `mismatch`（不是 `pass`）**：它们由上游**直接调 handler**
   的测试抽取而来（`site: direct_handler`），replay 走的是 router 且 fixture 里**没有**
   `Authorization` 头（上游用的是安装令牌）⇒ `/v1` 的凭据门答 **401**（`plugin_bearer_required`）。
   上游自己的 `PluginBearerOnly` 在同样的 replay 下也会给 401 —— 这不是本片的实现分叉，而是
   fixture 抽取丢掉了凭据。要让它变 `pass`，需要在 `mc-conformance` 的 harness 里为 plugin 面
   伪造一枚真实令牌（**本片写集外** ⇒ 登记，不静默）。
2. 桥面与公开面**不共享**限流桶（`tower_governor` 的桶按 router 实例分片；桥面的层挂在
   `plugin_bridge/{context,issues,storage}.rs` 三处 ⇒ 同前缀下每面各自计数）。上游两档也区分
   （`user_default` vs `plugin_strict`），故行为面等价；仅登记实现细节。

### 9.10 M6-9 daemon 侧 skill/MCP 执行面（`LUM-1674`）的落点与偏离登记

> 记录号说明：00:30 cycle（`LUM-1755`）派发时定的是 **§9.9**，但本片落笔前 `git fetch` 复核发现
> 该号段已被并行合并的 M6-7（`LUM-1672` / PR #74）占用 ⇒ **顺延为 §9.10**（`docs/57` 的
> 「落笔前先复核号段」纪律生效）。

**落点**：**0 个注册键**（0 路由片）—— 上游 `internal/daemon/` 的六个文件 + `execenv/` 的六个
注入口，全部是**本机执行面**：没有 handler、没有路由、没有迁移、没有 DB。上游对照：
`local_skills.go`（726）+ `skill_cache.go`（192）+ `slash_skill.go`（35）+
`runtime_mcp.go`（578）+ `remote_mcp_broker.go`（475）+ `plugin_hook_mcp.go`（246）+ `execenv/`
六个文件（845）= **3,097 行**（全为 `90e0bdf` 实测，非测试）。

| 文件（全部新建） | 行数 | 内容 |
| --- | --: | --- |
| `mc-daemon/src/skill/mod.rs` | 645 | 发现根/摘要/bundle/cache-ref 的 DTO、`path.Clean` 逐行移植、key 规范化、缓存段安全化、支持文件路径白名单、home 折算 |
| `mc-daemon/src/skill/local.rs` | 531 | provider → 用户级 skill 根（22 个 provider + omp 描述符行）、递归枚举（深度 4 / 符号链接不跟 / visited 去环 / 按 key 去重）、支持文件收集（1 MiB · 256 条 · 8 MiB 三道闸 + 二进制/UTF-8/NUL 判据）、列表-装载一致性 |
| `mc-daemon/src/skill/cache.rs` | 605 | `<root>/<ws>/<source>/<id>/<hash>/bundle.json` 的原子读写 + per-ref 锁 + **校验调 `mc_core::skill::build_manifest`** |
| `mc-daemon/src/skill/slash.rs` | 217 | `[/label](slash://skill/<id>)` 提取（**手写扫描器**，无 `regex` 边）+ 按 id 去重 |
| `mc-daemon/src/mcp/mod.rs` | 230 | JSON-RPC 信封、两处错误码表、per-task 随机 token、`PluginHookTool` |
| `mc-daemon/src/mcp/runtime.rs` | 608 | JSONC `strip` 逐行移植、各 provider 的 MCP 配置路径/格式、去敏 inventory、嵌套键查找、传输档归类、runtime×agent **本地**合并 |
| `mc-daemon/src/mcp/broker.rs` | 683 | 任务期 broker：端点解析（`mc-mcp`）、凭据解析器、`127.0.0.1:<随机端口>/<随机 token>` 真监听、闸的顺序、代理核心 |
| `mc-daemon/src/mcp/broker/{http,protocol,tests}.rs` | 75/149/361 | HTTP 出口形状 / 纯协议件（SSE 解码、`tools/list` 过滤、配置合并）/ 用例（门 ⑩ 逼出来的拆分） |
| `mc-daemon/src/mcp/hook.rs` | 702 | 把插件 hook 合成 MCP server：`initialize`/`notifications`/`tools/list`/`tools/call`、工具错误（非协议错误）、真监听 |
| `mc-daemon/src/mcp/runtime/tests.rs` | 217 | 同上（用例与实现分开） |
| `mc-daemon/src/execenv/cursor_mcp.rs` | 718 | `.cursor/mcp.json`、Cursor 数据目录的 approvals（**字节级**：stdio/remote 两种规范化形状 + 字段序）、`.workspace-trusted`、`mcp-auth.json` 播种（链接优先） |
| `mc-daemon/src/execenv/runtime_skill_policy.rs` | 424 | claude 的 runtime-skill settings（overrides + **permission deny 两通道**）、codex 的 `[[skills.config]]` 追加、`cleanRuntimeSkillKey`（与 `normalize_local_skill_key` **口径不同**，见 M6D-8） |
| `mc-daemon/src/execenv/codex_user_skills.rs` | 330 | 用户 `~/.codex/skills` → 每任务 `CODEX_HOME/skills` 的**链接**（不拷贝）、workspace skill 优先、只摘自己的链接 |
| `mc-daemon/src/execenv/skill_visibility.rs` | 257 | 模型可见清单（名字换成盘上 slug）、批次内 slug 去重、`disable-model-invocation` 判定 |
| `mc-daemon/src/execenv/codex_skill_strip.rs` | 185 | 剥掉 `[[skills.config]]`（Codex CLI 0.114 的 `missing field path` 会让它拒启动） |
| `mc-daemon/src/execenv/omp_mcp.rs` | 184 | omp 的 `.omp/mcp.json` 注入（拒绝覆盖用户文件） |
| `mc-daemon/src/execenv/sidecar.rs` | 411 | 本 slice 需要的写闸：拒绝覆盖的写入/目录创建、slug 候选序列、frontmatter 切片（见 M6D-6） |
| `mc-daemon/src/lib.rs` | 59（+9） | **只加** `pub mod mcp; pub mod skill;` 与一段模块文档（08:30 cycle 的写集修订） |
| `mc-daemon/src/execenv/mod.rs` | 88（+10） | **只加** 7 行 `pub mod`（既有 `{guard,lock,path,temp}.rs` **一行未改**） |
| `mc-daemon/Cargo.toml` + `Cargo.lock` | +14 / +2 | 两条 `path` 边（见 M6D-1） |

**冻结点零编辑**：路由表 / `route-parity-baseline.json` / `slash-alias-allowlist.tsv` /
`mc-http/**` / `mc-skill/**` / `mc-mcp/**` / `mc-plugin-host/**` / `mc-repos/**` 全部零改动。

#### 两条专属 DoD 的证据

1. **bundle 缓存校验用的是同一个函数**（issue 原文：*有断言证明是同一个函数，而非「同结果的新实现」*）：
   `cache.rs::validate_skill_bundle` 里只有一处 digest 来源 —— `mc_core::skill::build_manifest`
   （`crates/mc-core/src/skill.rs:256`），与 `crates/mc-http/src/routes/daemon/skills.rs:164` 的
   `build_agent_bundle` 是**同一个符号**。用例
   `skill::cache::tests::validate_accepts_exactly_the_mc_core_digest` 把它钉在两处：
   ① 金标 `sha256:e1fb47095775209b81c5d404e36f5b2285ac7744cb39960cc761cf211543d79d`
   （`build_manifest` 在 `a.md="a" / b.md="bb"` 上的实测值），②「换一个 64 位 hex 就判不过」。
   本文件**没有**任何自算哈希的代码路径。
2. **broker 的 pinned tools 拒绝用例**（issue 原文：*声明与实际不符 ⇒ 拒绝*）：
   `mcp::broker::tests::pinned_tools_reject_missing_and_drifted_tools` 三条断言 ——
   **缺工具** ⇒ `approved tool "read" is missing`、**schema 漂移** ⇒ `schema drifted`、
   **远端新增**工具 ⇒ 放行（没批准也不会被 `tools/call` 放过去）。比对本身调
   `mc_mcp::client::validate_pinned_tools`（M6-1），本 crate **没有**第二份比对逻辑。

#### 偏离（M6D-1～M6D-17）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **M6D-1** | `crates/mc-daemon/Cargo.toml` **加两条 `path` 边**（`mc-mcp`、`mc-skill`）+ `Cargo.lock` **+2 行** | `mc-daemon/Cargo.toml` | 00:30 cycle 预飞的「零 manifest / 零 lock 编辑」**只覆盖 bundle hash 走 `mc-core` 这一条**，没有把动作清单第 5 条（*消费 M6-1 的 `mc-mcp`*）对回 manifest。不接边的代价：broker 要自建 JSON-RPC 客户端与钉定比对、本地 skill 发现要自抄 frontmatter 解析与二进制判定 —— 恰是本波「一处真值」纪律要避免的两份实现。两条边都指向**已在 workspace 与 lock 里的**成员 ⇒ 锁文件无新 package 条目（与 D-10 的 `reqwest` 同款：只多依赖数组里的两行）。**M6-INT 若要收紧，删这两行 + 本片对应的 `use` 即可**（影响面已隔离在 `skill/local.rs` / `mcp/{runtime,broker}.rs`） |
| **M6D-2** | **TOML 未接**：codex 的 `config.toml` 读不了 | `mcp/runtime.rs::unmarshal_runtime_mcp_config` | 需要 TOML 解析器，而 `toml` 不在 workspace 依赖表里（M6-0 anchor 冻结「此后 M6 各切片不得再新增三方依赖」）。⇒ 返回**可区分**的 `McpConfigError::TomlUnsupported`（不是静默空表：那会把「解析不了」谎报成「没有 server」）。**写侧不受影响**：`codex_skill_strip` / `runtime_skill_policy` 只**追加** TOML 文本 |
| **M6D-3** | **claude 插件段未接**（插件 skill 根 + 插件贡献的 MCP server） | `skill/local.rs`、`mcp/runtime.rs` | 上游要 `listEnabledClaudePlugins` / `readClaudePluginManifest` / `claudePluginComponentPaths`（`claude_plugins.go`，**不在本片写集**）。⇒ `root="plugin"` 的 skill 根与 `Claude Plugin · <name>` 来源的 MCP 条目本片不产出。前缀逻辑本身已落并有单测（`LocalSkillRoot::plugin` 的 `<plugin>:` 键前缀） |
| **M6D-4** | `hermes` 的根解析未接 ⇒ 该 provider 报**不支持** | `skill/local.rs::provider_root` | 上游走 `execenv.ResolveHermesProfile`（Hermes home 解析，随 provider 配置切片进来）。**不**退回硬编码 `~/.hermes` —— 上游注释点名那样会在 Windows 上漏掉 Hermes 真正加载的全部 skill（GH #8310） |
| **M6D-5** | 内置 runtime 描述符表只落 `omp` 一行 | `skill/local.rs::BUILTIN_USER_SKILLS_DIRS` | 上游查 `agent.BuiltinRuntimeByID`，该注册表当前**只有一行**（`omp` ⇒ `.omp/agent/skills`）。注册表本身归 agent 切片；一行硬编码把「今天的行为」逐字固定住 |
| **M6D-6** | 新增 `execenv/sidecar.rs`（本片 execenv 的**第 7 个**文件），且 `sidecar_manifest.go` 的账本 / `CleanupSidecars` / `ensureSkillFrontmatter` **不做** | `execenv/sidecar.rs` | 本片的六个注入口共用三件**语义独立**的东西：拒绝覆盖的写入（上游 `errPathPreExists`）、slug 候选序列（`skillSlugCandidate` / `allocateCollisionFreeSkillDir`）、frontmatter 切片（`frontmatterParts`）—— 后两者上游自己就为「两个调用方必须同意」抽出来共用。**因此本片的 sidecar 写入不记账**（只用于随 env root 一起被 GC 的产物，如 `cursor-data/`）；合并回 `sidecar_manifest.go` 的 converge 点登记给任务准备切片 / M6-INT |
| **M6D-7** | `disable-model-invocation` 是**一个键的窄口径读取**，不用 YAML 解析器 | `execenv/skill_visibility.rs` | `mc-daemon` 这一侧没有 YAML 解析器（`serde_yaml` 不是它的依赖，M6D-1 也只接了 `mc-skill`/`mc-mcp`）。窄口径覆盖：顶格键 + 标量 `true`/`false`/`"true"`（大小写不敏感、容忍行尾注释）。嵌套或锚点等写法**不认** —— 失败方向是「仍然列出该 skill」，与上游的 `switch` 默认分支一致 |
| **M6D-8** | `cleanRuntimeSkillKey` 与 `normalize_local_skill_key` **口径不同**（照抄上游） | `execenv/runtime_skill_policy.rs` | 前者只拒 `.` / 绝对路径 / 恰好 `..` / `../` 前缀（`..foo` **合法**），后者按 `strings.HasPrefix(cleaned, "..")` 把 `..foo` 一并拒掉。两处都逐字照抄并各有单测钉住差异 —— 合并成一个函数会让**其中一处**与上游分叉 |
| **M6D-9** | `SkillForEnv {name, content}` 是 `SkillContextForEnv` 的**子集** | `skill/mod.rs` | 三个消费者（`runtime_skill_policy` / `codex_user_skills` / `skill_visibility`）只读 `Name`（slug 与占位判定）与 `Content`（解 frontmatter）。其余列随 task-context 切片进来后应换成它的投影 |
| **M6D-10** | `random_broker_token` 用两个 `Uuid::new_v4()` 拼 48 位 hex | `mcp/mod.rs` | 上游 `crypto/rand.Read(24)`；本 crate 没有 `rand` 边。`uuid` 的 v4 同样取自 OS CSPRNG（122 位/次）⇒ 熵不低于上游，**形态逐字相同**（48 位小写 hex）。这个 token 是路径上的访问控制 |
| **M6D-11** | `go_quote` 是 `strconv.Quote` 的**窄口径** | `execenv/runtime_skill_policy.rs` | 覆盖 `"` / `\` / `\n` / `\r` / `\t` 与其余控制字符（`\uXXXX`）；非 ASCII 按 UTF-8 原样写（与 Go 一致）。Go 的 `\u` 转义全集本波不追 |
| **M6D-12** | 目录链接只在 unix 实现 | `execenv/codex_user_skills.rs::create_dir_link` | 上游 Windows 建 junction（`codex_home_link_windows.go`）；本片在非 unix 平台返回**可区分**错误。Windows 面整波为登记缺口（`docs/33` §p3） |
| **M6D-13** | broker **没有** `ReadHeaderTimeout` 的等价物 | `mcp/broker.rs` | 上游给 `http.Server` 设 5s 读头超时；axum/hyper 要装 `tower-http` 的 timeout 层，而它不在 `mc-daemon` 的依赖表里（M6D-1 只接了两条 `path` 边）。整条请求仍被「出网调用超时 + 连接生命周期」约束 |
| **M6D-14** | broker 的 per-call 日志只有 `task_id` / `installation_id` / `contribution` / `tool` | `mcp/broker.rs::handle` | 上游那行还有 `duration_ms` 与 `result_class`。两者都只影响可观测性：拒绝类结果已经通过 JSON-RPC 错误码回到调用方。日志级别用 `debug`（上游是 `Info`），避免每次工具调用都在 daemon 日志里刷一行 |
| **M6D-15** | bundle 校验对**未知 `source`** 收紧为「判不过」 | `skill/cache.rs::parse_source` | `ManifestInput::source` 是封闭枚举（`SkillSource` 三常量），而上游是把**任意字符串**喂进哈希。差异只出现在「源不是三个常量之一」这一种输入上，且方向是**收紧**（那种 ref 本来也不该命中缓存） |
| **M6D-16** | `prepare_omp_mcp_config` 要求 `work_dir` 是**绝对路径** | `execenv/omp_mcp.rs` | 上游直接 `filepath.Join(workDir, ".omp")`：相对 `workDir` 会把 sidecar 落到 daemon 的 cwd，而不是用户的任务目录。与 `cursor_project_root` 一样先解析再写 |
| **M6D-17** | Cursor approvals 的字节口径：U+2028/U+2029 **原样**写出 | `execenv/cursor_mcp.rs` | Go 的 `json.Encoder` 会把这两个码位转义成 `\u2028`/`\u2029`（即使 `SetEscapeHTML(false)`），`serde_json` 不转。它们出现在 MCP 服务器配置里属极端情形；**其余**（字段序、`omitempty` 的缺省、不转义 HTML）都已逐字对齐，并有单测钉住 |

#### 门禁读数（逐字取自当轮日志；合并 #74 之后的 base `633660ee`）

- `bash scripts/gates.sh`：**8/8 绿**（热 target 77s；起手冷跑 324s，③ 因 17 条 pedantic 先红，
  修完复跑全绿）。⑦ 的第二条命令 `slash_alias_audit.py --quiet` 一并 exit 0。
- ⑤：**1810 passed / 0 failed / 198 ignored** —— base 的 1673/198 之外**恰好 +137**，即本片新增的
  137 条 `mc-daemon` 单测（全部为纯判定 / 真实文件系统 / 本机 socket，**0 条** `#[ignore]`）。
  ⑥/⑧ **未跑**：本片不碰库（无 DB 代码、无迁移）⇒ 按 issue 的口径只需 8/8。
- ⑦：**读数逐字不变**（0 路由片）：`local 405 / baseline 344 / implemented 329 real + 0 placeholder /
  known_gap 127 / unclaimed 0 / regression 0 / local_only 9`，`owners.M6 1`（M6-9 不消费任何
  `M6` 缺口键）。基线**未刷** —— 一次性刷新归 M6-INT（`LUM-1675`，目标末态 `406 / 330 / 126 / owners.M6 0`）。
- ⑨：`crates/mc-conformance/report.json` **逐字未变**（`pass 5 / mismatch 23 / unmounted 31 /
  placeholder 0 / unevaluable 306`，`fixtures 365`）—— 本片**不是 HTTP 面**，⑨ 的预期变化就是 0。
- ⑩：本片 21 个 `.rs` 新文件里最大 **718 行**（`cursor_mcp.rs`），全部 ≤800；
  `scripts/file_size_baseline.tsv` **未动**（只减不增）。

#### 登记的已知缺口（给 M6-INT / 后续切片）

1. `mc-daemon/Cargo.toml` 的两条 `path` 边（M6D-1）若被判定越出写集，收敛动作是：删边 +
   把 `mc_skill::{frontmatter,binary}` 的调用换成 `mc-core` 侧的共享实现（需要先把它下沉到
   `mc-core`，与 M6-4 的 bundle hash 同款处置），broker 的钉定比对则必须搬进 `mc-core` 或由
   `mc-mcp` 暴露 —— **不要**在 `mc-daemon` 里复制第二份。
2. `execenv/sidecar.rs`（M6D-6）应在任务准备切片落地 `sidecar_manifest.go` 后合并回去，
   届时本片六个注入口改为接收 manifest 参数（写入记账 + 可回收）。
3. `mcp/runtime.rs` 的 TOML 面（M6D-2）与 `claude` 插件段（M6D-3）是**两个独立的**补齐项：
   前者只差一条依赖边（或一个窄 TOML 读取器），后者要等 `claude_plugins.go` 落地。

### 9.11 M6-8 hook 引擎 + MCP 传输 + hook job（`LUM-1673`）的落点与偏离登记

> 记录号说明：派发时定的是 **§9.10**（当时 `git fetch` 复核的尾号是 §9.9），但落笔后合并 base 时
> 发现该号段已被并行合并的 M6-9（`LUM-1674`）占用 ⇒ **顺延为 §9.11**（与 M6-6/M6-7 那次
> 「两侧都保留、后到者顺延」同一先例；`docs/57` 的「落笔前先复核号段」纪律在 base 移动时要**重跑一次**）。

**落点**：上游 **1 条注册键**（`POST /api/plugin-bridge/v1/hooks/{key}`，`router.go:1598`）+
**0 路由**的 job 粘合（`scheduler/jobs_plugin_hook.go`353）。上游体量 ≈2,082 行，本片按
「路由面 + job 面 + 引擎面」拆成本地 4 组文件。

**写集**（相对 `81c58721` 的增量，逐字路径）：

| 文件 | 性质 | 内容 |
| --- | :-: | --- |
| `crates/mc-http/src/routes/plugin_bridge/hooks.rs` | 改（44 → 54） | **唯一注册键**的挂载点（`policy::apply_bridge` + `post(hooks_job::invoke_bridge_hook)`） |
| `crates/mc-http/src/routes/plugins/hooks_job.rs` | 改（35 → 568） | 模块根：`HookInvocation` / `HookCallResult` / `HookError` / `HookRuntime` / 引擎四道前置 / 出站目的地判据 / `build_hook_headers` / `schedule_delivery_id` / `router()` |
| `crates/mc-http/src/routes/plugins/hooks_job/{bridge,outbound,schedule,wire,tests}.rs` | **新建** | 门 ⑩ 的拆分（见 D1）：桥面 handler / 出站发送 / job 粘合 + 日程对齐 / wire 结构与触发器折点 / 单元用例 |
| `crates/mc-repos/src/plugin/hook.rs` | 改（21 → 701） | `plugin_hook_schedule` 与 `plugin_invocation` 的写侧 + `reconcile_tx` / `set_enabled_tx`（12 列投影、三连守卫、按尝试计数的限流/熔断读口、TTL 清扫） |
| `crates/mc-scheduler/src/jobs/plugin_hook.rs` | **新建**（461） | job 本体：`SCOPE_KIND` / scope 枚举 / 计划折叠 / 分页枚举 / handler / 规格 |
| `crates/mc-scheduler/src/jobs/mod.rs` | 改（204 → 220） | `pub mod plugin_hook;` + `JobPorts` 第 4 个端口 + 第 3 行 `register`（见 D5） |
| `apps/mc-server/src/scheduler/hook_port.rs` | **新建**（127） | `PluginHookPort` 的生产实现（薄适配层，把活交给 `mc-http` 的引擎） |
| `apps/mc-server/src/scheduler/mod.rs` | 改 | `build()` 多传一个端口实参（**只此一处**；`main.rs` 一行未动） |
| `crates/mc-http/src/routes/plugins/install/{lifecycle,settings}.rs` | 改 | 安装/升级/启停三处调用日程对齐（见 D9，回填 M6-5-D2 的跨片缺口） |
| `crates/mc-scheduler/tests/jobs_plugin_hook.rs`、`crates/mc-http/tests/plugins/{hooks,hooks_job}.rs` | **新建** | 用例（纯逻辑 16 + 真库 14） |
| `crates/mc-scheduler/tests/{common/mod,jobs_issue_wakeup}.rs`、`crates/mc-http/tests/plugins/{main,support}.rs` | 改 | 夹具（见 D4/D8） |

**冻结面只读**：`routes/{mount,mod}.rs`、各 `routes/**/mod.rs`（含 `plugins/mod.rs` 的
`.merge(hooks_job::router())`）、`state.rs`、根 `Cargo.{toml,lock}`、`apps/mc-server/src/main.rs`、
`Cargo.toml`（任何 crate）、⑦ 基线、⑨ `report.json`、`slash-alias-allowlist.tsv` —— 逐条 `git diff` 空。

#### 偏离（M6-8-D1～D10）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **M6-8-D1** | `hooks_job.rs` 拆成 6 个文件 | `routes/plugins/hooks_job/**` | 门 ⑩ 单文件 800 行硬上限（一次成文的引擎 + job + 桥面 handler + 用例 = 1,389 行）。`routes/plugins/mod.rs` 是 anchor 冻结面（**不能**加 `mod`）⇒ 只能靠**子模块目录**（与 `install.rs` / `install/` 同款）：`hooks_job.rs` 声明 `mod {bridge,outbound,schedule,wire}`，公开路径不变 |
| **M6-8-D2** | 路由实现在 `hooks_job::bridge`，注册在 `plugin_bridge/hooks.rs` | 两处 | anchor 的 §9.2 归位（注册点按路径前缀）与「引擎与错误映射全在 `routes::plugins` 下」两条约束同时满足：bridge 文件只挂载（M6-7 的 `context/issues/storage` 同款），实现留在插件面文件树里。**注册键总数不变（57 条 / 本仓 406 条）** |
| **M6-8-D3** | 部署密钥缺失时的 `plugin_disabled` 是 **503**，但 `plugin_invocation.status` 按上游分类 = `failed` | `hooks_job::HookError::disabled` | 503 沿用 `M6D-1` 的四处一致口径；**调用记录**不跟着本地状态码走 —— 上游把「签名密钥缺失」归 `PluginErrorUnavailable` ⇒ `failed`（`refused` 只给 Forbidden/Quota/Incompatible）。两者分开登记，免得下一次有人「统一」成一种 |
| **M6-8-D4** | 写集追加 6 个**非本片文件**（逐条给理由） | 见下 | ① `tests/jobs_issue_wakeup.rs` + `tests/common/mod.rs`：`JobPorts` 变四实参（D5）必须改 M5 的构造点；② `tests/plugins/{main,support}.rs`：e2e 分片声明 + `build_state` 提为 `pub(crate)`（job 数据面用例需要 `Arc<AppState>`，而不是 `Router`）；③ `install/{lifecycle,settings}.rs`：D9 的回填；④ `apps/mc-server/src/scheduler/mod.rs`：`build()` 的端口装配（issue 已授权「只改 `build()`」） |
| **M6-8-D5** | `JobPorts::new` 取**四实参**（不是 builder、不是 `Option`） | `mc-scheduler/src/jobs/mod.rs` | issue 的裁定是「builder」或「`new` 加一参」二选一，并明确**禁止** `Option<Arc<dyn …>> + 默认 None`（那会让 job 静默不注册）。取后者：注册无条件（`register_all` 第 3 行），代价是 M5 的用例多传一个实参（已在 D4 登记，`register_all` 的登记表断言同步改成三个 job） |
| **M6-8-D6** | 引擎的上下文是 `HookRuntime`，不是 `AppState` | `hooks_job.rs` | `apps/mc-server/src/main.rs`（M5-9）调的是 `scheduler::start(&db, daemon_hub)` 且**一行不改** ⇒ 端口实现只有 `Db`。把引擎的依赖面收窄成 `{db, plugin_key, feature_flags}` 后，路由侧 `HookRuntime::from_state(&state)`、job 侧 `HookRuntime::standalone(db)` 都能构造它 |
| **M6-8-D7** | **未做**的四面（全部登记，不静默） | 见下 | ① `event` 触发的分发器（上游 `plugin_event_dispatch.go` 323 + `plugin_event_bridge.go` 108）：它要挂到事件总线上，而总线的落点（`mc-realtime` / M2 的资源服务）不在本片写集内 ⇒ `plugin_invocation.trigger='event'` 目前**没有生产者**；② MCP 传输段（上游 `plugin_mcp_transport.go` ≈150）：本地对 `transport.type="mcp"` 的 hook **逐字照抄上游**拒绝（422 `hook transport "mcp" is not supported yet`），真正要动的文件（`mc-mcp` / `plugins/mcp.rs`）分属 M6-1 / M6-6；③ `agent` 触发（`POST /api/daemon/tasks/:id/plugin-hooks`）：那条键归 M3 的 daemon 面，本仓至今恒 403（插件面恒禁用）；④ 调用记录的 TTL 清扫的上游宿主是 event dispatcher 的计时器 ⇒ 本地只落了查询（`delete_expired`），没有计时器；`CallbackBaseURL` 同理（上游配置项，本地无此配置 ⇒ 出站体里恒不出现 `callback_url`） |
| **M6-8-D8** | 夹具的 2 处改动 | `tests/plugins/support.rs` | ① `build_state` / `build_state_with_origin` 提为 `pub(crate)` 并加 `state_for`；② 新增 `cleanup_schedules`：`plugin_hook_schedule` **没有外键**（`399` 的注释写明关系由应用拥有），共享的 `cleanup` 够不着它 ⇒ 不显式删就会攒孤儿行。两处都只影响测试 |
| **M6-8-D9** | 日程投影对齐的**三处调用点**回填（M6-5-D2 的跨片缺口） | `install/{lifecycle,settings}.rs` | 上游在安装/升级/启停里调 `reconcilePluginHookSchedules` / `setPluginHookSchedulesEnabled`；M6-5 把该表整体划给 M6-8（`docs/32` §9.6 的 D2「M6-8 落地后需回填」）⇒ 本片补上，且**与安装行同事务**。不补的话 job 有消费者没生产者 = 死代码 |
| **M6-8-D10** | `delivery_id` **不是**行级唯一键；幂等由 `sys_cron_executions` 保证 | `plugin/invocation` 写侧 + job | anchor 的桩注释写「同一次计划投递重复执行时用 `delivery_id` 去重」—— 那句话指的是**投递**（同一格不双跑），不是**行**。上游一次计划投递重试三次就写三行（`attempt` 递增），`delivery_id` 让接收方认得出它们是同一次投递；行级唯一会吃掉重试行 ⇒ 限流/熔断计数与「这个端点为什么在失败」都会失真。本片取上游语义，幂等屏障是内核的唯一键 `(job_name, scope_kind, scope_id, plan_time)` |

#### 跨片接口（M6-INT / 后续波请照用）

- `mc_http::routes::plugins::hooks_job::{HookRuntime, HookError, invoke_hook, build_hook_headers,
  schedule_delivery_id, dispatch_scheduled_hook, advance_schedule_next_run, list_enabled_schedules,
  load_schedule}` —— hook 面的**唯一**实现点（路由、job、端口三方共用）。
- `mc_repos::plugin::hook::{HookScheduleRepo, reconcile_tx, set_enabled_tx, HookScheduleInput,
  SCHEDULE_COLUMNS}` —— `plugin_hook_schedule` 的**唯一**写者（M6-5 只读）。
- `mc_scheduler::jobs::plugin_hook::{JOB_NAME, SCOPE_KIND, PluginHookPort, ScheduleOutcome}` ——
  内核侧的契约；`apps/mc-server/src/scheduler/hook_port.rs` 是生产实现。
- **`plugin_invocation` 的读面**仍是 M6-6 的 `mc_repos::plugin::invocation_read`（本片只写）。

#### 本片门禁读数（逐字取自当轮日志）

- `bash scripts/gates.sh --with-db` ⇒ **10/10 / 161s**（起手 base `81c58721`：
  ①2s ②1s ③10s ④8s ⑤34s ⑥76s ⑧24s ⑦1s ⑨5s ⑩0s；`overall: PASS`）；
  **合并 base `ec83f6e7` 后重跑 ⇒ 10/10 / 211s**（本片的证据取后者：它才是 PR 落地树）。
- ⑦（**起手 base `81c58721` 的实测**）：`upstream 456 | local 406 registered | baseline 344`、
  `implemented 330 real + 0 placeholder`、`known_gap 126`、`unclaimed 0`、**`regression 0`**、
  `local_only 9`；`owners` 表 `{M9 33 / M7 24 / M8 24 / M3+ 16 / M2-A 13 / M3 11 / M10 5}`
  （和 = 126✓，**无 `M6` 键**）。
  ⚠️ **`real + placeholder` 的拆分随 base 变化，总数不变**：起手时基线上还没有 `#77`
  （`LUM-1580` 把门 ⑦ 的占位判据从「字面 `placeholder`」放宽到 `placeholder|not_implemented`，
  `scripts/route_parity.py` 的 `PLACEHOLDER_HANDLER`）⇒ 上表的 `0 placeholder` 是**旧正则**下的读数。
  **合并 base（`ec83f6e7`）后同一棵树逐字实测 = `implemented 326 real + 4 placeholder = 330`**
  （4 条占位键的 owner 都不是 M6：`attachments` / `pull-requests` / `timeline` /
  `comments/trigger-preview`）。口径与读数见 `docs/22-ROUTE-PARITY.md` §2.3 / §3.6 与
  `docs/57` §9.9（14:00 cycle 已就地订正）。
  **合并后的门禁（`b0c053a0`，base `ec83f6e7`）⇒ 仍 10/10 / 211s**：
  `local 406 / implemented 326 real + 4 placeholder = 330 / known_gap 126 / unclaimed 0 /
  regression 0 / local_only 9`，`owners` 表同前（**无 `M6`**）。
  本片 **+1**（405 → 406），**`owners.M6 = 0`** —— M6 的路由面收口。
  ⚠️ 基线 **344 不刷**：唯一一次 `--write-baseline` 归 M6-INT（`LUM-1675`）。
- ⑨：`crates/mc-conformance/report.json` **逐字未变**（本片 0 条 fixture 归属变化 —— M6-8 的
  那条键没有 fixture）。
- ⑤：`cargo test --workspace` ⇒ **2213 passed / 0 failed / 201 ignored**。
- ⑥：真库 `mc_lum1673`（`CREATEDB`，模板 `CREATE ROLE mc_lum1673 LOGIN CREATEDB …`）⇒
  `migrate=0 e2e=0`（**524 passed / 0 failed**）—— 含本片新增的 16 个真库用例
  （mc-http 的 11 个 hook 用例、mc-scheduler 的 `real_db_the_loop_delivers_each_schedule_bucket_exactly_once`、
  mc-repos 的 2 个 `plugin::hook::db_tests`）。
- **幂等证据**（DoD）：`real_db_the_loop_delivers_each_schedule_bucket_exactly_once` 用
  **50ms tick / 5 分钟计划格**的循环跑满 ~0.5s（十几次 tick）⇒ 桩端口只被派发**一次**，
  再补一条 `db_ops::try_claim` 对同一个 SUCCESS 桶的确定性探针（`Conflicted`）。
  幂等屏障是内核的唯一键 `(job_name, scope_kind, scope_id, plan_time)`，不是本片的代码。

#### 登记的已知缺口（给 M6-INT）

1. **`event` 触发的生产者缺位**（D7 ①）：`plugin_invocation.trigger='event'` 只有表没有调用点。
   接法：在事务提交后的事件出口（M2/M3 的资源服务）调 `hooks_job::invoke_hook`，并把
   `trigger=Event` + `event_type` 传进去；限流、熔断、`net:` 判据、调用记录都已就绪。
2. **MCP 传输段未接**（D7 ②）：`transport.type="mcp"` 的 hook 目前按上游拒绝（422）。
   上游那条路径要 `mc-mcp` 的 client，而那是 M6-1 / M6-6 的文件。
3. **⑥ 的 ⑨ 面**：本片没有新增 fixture，所以 `report.json` 不需要动；若 M6-INT 想给
   `plugin-bridge/v1/hooks` 加契约快照，需先在上游侧有对应 fixture（目前 0 条）。
4. **两个 `plugins_v1` 判定点**：路由侧走 `policy::plugins_v1_enabled(state)`，job 侧走
   `HookRuntime::standalone(db)` 里的**空**目录（未登记 = 开启）。生产进程的开关目录由
   `main.rs` 的 `AppState` 持有，而调度循环拿不到它（D6）⇒ 若将来要给 hook job 单独关开关，
   需要把目录也传给 `scheduler::start`（那要动 `main.rs`，本片不改）。


---

## 10. M7-0 anchor（`LUM-1765`）：文件→写者表与偏离登记

`docs/60-M7-PLAN.md` §5 的「每文件预扩展清单」是**锚点文件集**；本节的表是它落地后的**准确版**
（含锚点期做的六处归位判断）。M7 后续切片按本表认领写集，**不得**编辑左列之外的共享文件
（`routes/mount.rs` / `routes/mod.rs` / `routes/channels/mod.rs` / `state.rs` / `routes/auth.rs` /
根 `Cargo.toml` / `Cargo.lock` / 各 `lib.rs` / `mc-core/src/channel{,.rs 的子模块}` / `mc-secrets` /
`apps/mc-server/src/{main.rs,channels.rs}` / ⑦ 基线 / `slash-alias-allowlist.tsv` 全部由锚点冻结）。

### 10.1 锚点冻结的聚合/共享文件（M7 各片只读，写各自子文件）

| 冻结文件 | 谁写 | 说明 |
| --- | :-: | --- |
| `crates/mc-http/src/routes/mount.rs` | anchor | 加 `mount_slice_channel()` + `.merge(...)` 一行；锚点期**零注册键** |
| `crates/mc-http/src/routes/mod.rs` | anchor | 声明 `channels`（**一个**面：24 条路由） |
| `crates/mc-http/src/routes/channels/mod.rs` | anchor | 24 条路由账（逐条带 `router.go` 行号）+ 5 个子模块声明 + 两个结构决策 |
| `crates/mc-http/src/state.rs` | anchor | `ChannelKeys`（五个 `MULTICA_<CHANNEL>_SECRET_KEY` 的**唯一**读取口）+ `AppState::new` 内读 env |
| `crates/mc-http/src/routes/auth.rs` | anchor | 测试里唯一的 `AppState { … }` 字面量补 `channel_keys`（**压缩注释回基线**，见 10.2 第 4 条） |
| `crates/mc-core/src/channel.rs` + `channel/{installation,message,binding,install_session}.rs` | anchor | 跨 crate 的领域层：三个字符串口径 / `Installation` / 归一化消息信封 / `BindingToken` / `InstallSession` |
| `crates/mc-channel/**`（`lib.rs` + 4 个类型文件 + `engine/{mod,router,supervisor,resolvers}.rs` + 5 个 `mod.rs`） | anchor | 运行时骨架；五个平台文件 anchor 期是**空** `register()` |
| `crates/mc-repos/src/lib.rs` + `channel/mod.rs` | anchor | 模块树 + 22 张表 → 8 文件的归属表 |
| `crates/mc-secrets/src/{lib.rs,secretbox.rs}` | anchor | 部署密钥封装盒（`cipher.rs` **不动**） |
| `apps/mc-server/src/channels.rs` + `main.rs` | anchor | 长连接宿主位 + 停机链的一行（**先停渠道 → 再停调度器 → 最后停 actor**） |
| `Cargo.lock` | anchor | 只重新生成：**+1 个成员 crate、0 个新外部包**（见 10.5） |

### 10.2 锚点期**归位判断**（六处，`docs/60` §5 原表未覆盖或写法不同）

| 事项 | 原表 | 本锚点落点 | 理由 |
| --- | --- | --- | --- |
| `engine/{session,batcher,lease,commands}.rs` 的 `pub mod` 声明 | 四文件归 **M7-2** | anchor **不**预声明它们（本片写集不含那四个文件）；M7-2 落自己的文件时自行在 `engine/mod.rs` 追加四行 `pub mod …;` | M7-1 与 M7-2 同 stage：先起跑的那片追加、另一片 rebase。若 anchor 预建那四个文件（空桩）就**越出本片写集**，且会在写集交叉审计里记成 `{M7-2}` 的文件被 anchor 创建 |
| `Registry` 的三条语义（last-writer-wins / 忽略空 key / `ErrUnknownType`） | 「anchor 建类型位、实现归 M7-1」 | anchor **已实现**（纯数据结构），`Channel` trait 的**实现**仍全归 M7-1 | 装配点是**进程启动路径**（`channels.rs` 调 `register`）：一个 `todo!()` 会让服务器起不来。这是 anchor 期唯一"提前实现"的地方，且不含任何平台分支 |
| 24 条 workspace 级路由的挂载法 | 未写 | **全路径注册**（`/api/workspaces/:id/<平台>/…`）+ 顶层 `merge`，**不** `nest` 进 `workspaces.rs` | 上游在同一个 `r.Route("/{id}", …)` 块内部注册；本仓若 `nest` 到 `workspaces.rs` 会与那里**已有**的 `/api/workspaces/:id` 抢同一挂载点（axum 0.7 嵌套与既有路径重叠会 panic） |
| `mc-telemetry::redact::SENSITIVE_KEYS` 是否补渠道键名 | §2.3 第 4 条说「不足则 anchor 补齐」 | anchor **不动** `crates/mc-telemetry/src/redact.rs`（**不在本片写集**），把裁定写在这里 | 逐条实测：渠道凭据的键名（`app_secret` / `bot_token` / `app_token` / `corpsecret` / `tenant_access_token` / `webhook_secret`）**都已被** `secret` / `token` / `secret` 子串命中；未命中的两个是 `app_key` 与 `encrypt_key` —— 前者是**客户端标识**（上游只封装 `app_secret`，明文标识进日志是**想要的**），后者在本波五个平台里**没有对应字段**（wecom 的凭据列叫 `secret`）。⇒ 当前覆盖**足够**，不加（加了会把调试信息也抹掉） |
| `engine` 的取消语义 | 上游靠 `context.Context` | anchor 的 `Channel::connect(&self)` **不带**取消令牌：取消 = supervisor 对承载 `connect` 的 tokio 任务 `abort` + 随后 `disconnect` | `docs/60` §5 的依赖表里**没有** `tokio-util`（锚点把依赖面一次定死）；契约不变（取消不是错误，`connect` 返回 `Ok(())`），M7-1 负责把它写成用例 |
| `tests/` 里 `AppState` 字面量（`routes/auth.rs`） | 「唯一字面量构造点补字段」 | 补 `channel_keys` 的同时**压缩同文件的注释**，把行数**压回基线 1704** | 门 ⑩ 的基线 `crates/mc-http/src/routes/auth.rs 1704` **只允许变短**；M6-0 的先例是同一手法（当时把 M3 的 4 行注释压成 1 行）。⇒ **任何后续切片都不要再往 `auth.rs` 加行**（新字段由 anchor 加，加不下就压缩注释） |

### 10.3 `mc-core` 的**不做什么**（登记为偏离，避免两处定义漂移）

1. **三个字符串口径单列成函数，不许各片内联**：`ChannelKind::as_str()`（路由前缀 / `issue.origin`
   = `lark`）、`ChannelKind::storage_str()`（`channel_*` 表的 `channel_type` 列 = **`feishu`**）、
   `ChannelKind::secret_key_env()`（env 名）。上游把 Lark/Feishu 存成 `feishu` 而 HTTP 面写 `lark`；
   20 个切片各自 `if kind == Lark { "feishu" }` 就是同一张表二十个真值。
   **登记为锚点新发现的跨波口径**（`docs/60` 未提）。
2. **不定义平台 wire 类型**：没有 Slack 信封、Lark 帧、DingTalk Stream、WeCom aibot、Telegram
   update 的 Rust 结构 —— 那是各 adapter 的文件。
3. **遗留 `ChannelInstallation` 的 shape 本片不动**（§5 的硬要求）：它的 `external_id` /
   `display_name` / `enabled` 三列在真实 `channel_installation` 表里**不存在**；M7 的真值是
   `channel::Installation`（逐列对齐迁移 `124`）。收敛（删或改名）**不在 M7 写集内**。
4. **`InboundMessage` 保留上游的四个独立语义开关**（`has_selected_context` / `force_fresh` /
   `skip_agent_run` / `addressed_to_bot`）：它们不是可以打包的状态位，所以对该结构的
   `clippy::struct_excessive_bools` 是**显式豁免**（理由写在源码里，不是为了躲 lint）。
5. **`BindingToken.raw` 与 `token_hash` 都不进序列化输出**：明文令牌只在 mint 的返回值里出现
   一次，哈希是服务端比对用的内部值 —— 少一个出口少一个泄露面。

### 10.4 依赖与工具链偏离

| 事项 | 落法 | 为什么 |
| --- | --- | --- |
| 根 `Cargo.toml` | **一行未改**（`members = ["crates/*", "apps/*"]` 是 glob） | 新 crate 只加目录；M7 各片不再碰根 manifest |
| `mc-channel` 的三方依赖 | 全部用 workspace 既有版本（`tokio` / `async-trait` / `serde` / `serde_json` / `reqwest` / `tokio-tungstenite` / `futures-util` / `thiserror` / `tracing` / `chrono` / `uuid` / `base64` / `hex` / `sha2` / `hmac`） | **无新外部包**；裁剪/加法都留到真需要时由集成方仲裁（`docs/15` §8.4） |
| 新增两条 `path` 边 | `mc-http → mc-channel`、`apps/mc-server → mc-channel` | 与 M5-9 给 `mc-scheduler` 补边同一手法：让渠道运行时**真的被链进二进制** |
| 不引 Redis | 进程内租约 / 去重 / 安装会话 / 重投递（R-M7-1） | 见 10.6 的 R-M7-1 |
| 不引 `tokio-util` | 取消走 `abort`（10.2 第 5 条） | 依赖面一次定死 |

### 10.5 本锚点的门禁读数（逐字取自当轮日志）

```
①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0 · ⑥db 0(migrate=0,e2e=0)
⑧schema-drift 0 · ⑦route-parity 0 · ⑨conformance 0 · ⑩file-size 0      ⇒ 10/10 PASS / 251s
```

- ⑦（**当场实测，本片不刷基线**）：`upstream 456 (commit f41fae6b08fb) | local 406 registered |
  baseline 406`、`implemented 326 real + 4 placeholder = 330 / 456`、`known_gap 126`、
  `unclaimed 0`、`regression 0`、`local_only 9`、`gaps by owner: M9=33 M7=24 M8=24 M3+=16
  M2-A=13 M3=11 M10=5` —— 与 `docs/60` §6.1 的 M7-0 行**逐字相同**（本 anchor 是五轮里
  **第一个零删除的 anchor**：渠道面在 `mount.rs` 没有任何 M0 占位可删 ⇒ 基线不动）。
- ⑦ 第二条（形态）：`slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv`
  = `declared 24 | dual-form required: 0 | 0 defect(s)`、exit 0（M7 **没有** allowlist 退路）。
- ⑩：0 违规；`scripts/file_size_baseline.tsv` **未动**（`routes/auth.rs` 压回 1704 = 基线）。
- ⑨：`report matches crates/mc-conformance/report.json`（本片零 fixture 改动 ⇒ 未漂移）。
- `cargo metadata`：workspace 成员 **33**，`mc-channel` 在内。
- `Cargo.lock`：`+31 行` = **一个** `[[package]] mc-channel` + 两条依赖边（`mc-http` /
  `mc-server`）；**零新外部包**。

### 10.6 风险登记（承接 `docs/60` §8；R-M7-1…R-M7-5 由**本 anchor 一次落**）

| ID | 内容 | 锚点落法 / 归属 |
| --- | --- | --- |
| **R-M7-1** | 无 Redis：上游 4 处跨副本协调（WS 租约 CAS / 入站去重 / 安装会话 / 出站重投递） | 进程内替身 + **单副本部署契约**；上游四处都有"无 Redis"的等价降级路径（`router.go:742` 有明文 warn）⇒ 换部署形态而非伪造行为。**本波不引入 Redis**；端口（`LeaseStore`）已由 anchor 定死，实现归 M7-2 |
| **R-M7-2** | 5 个平台 API 无法在 CI 真连 ⇒ "真实收发回路"门禁可能被假绿绕过 | 替身三条纪律（只替 wire 不替业务 / 帧逐字段断言 / 两个反例必测）写进 M7-4/6/8/13/19 的 DoD（`docs/60` §4.2） |
| **R-M7-3** | 5 个部署密钥的"未配置"语义**按端点而异**（lark 列表 200 空 + `install_supported:false`，wecom 端点 503） | 路由**仍然存在**、逐 fixture 对齐；**禁止统一 503**。anchor 把"判据 = 部署密钥存在"的装配点钉在 `apps/mc-server/src/channels.rs` |
| **R-M7-4** | `secretbox` 会有**两份**实现 | M6 面（`mc-plugin-host::credentials`，已合、冻结）与 M7 面（`mc-secrets::secretbox`，本片落）。**真实方位**（订正 `docs/60` §2.3 的措辞）：方向是 `mc-plugin-host → mc-secrets` ⇒ `mc-secrets` **不能**复用它（会成环），反过来 M6 面也不必改。两者都是 AES-256-GCM + `nonce‖ct‖tag`，逐字同形；收敛票**不在 M7 写集内** |
| **R-M7-5** | lark **两套表并存**（泛化 `channel_*` + 遗留 `lark_*`） | 22 张表的落点表里两套并列（`mc-repos/src/channel/mod.rs`）；M7-14 的 DoD 明写"不得合并" |

**锚点新登记（`docs/60` 未列，编号顺延）：**

| ID | 内容 | 处理 |
| --- | --- | --- |
| **R-M7-10** | **`channel_type` 的双口径**：`channel_*` 列存 `feishu`（上游 `channel.TypeFeishu`），而路由前缀与 `issue.origin` 是 `lark` | 已由 anchor **收敛成两个函数**（`ChannelKind::{as_str,storage_str}` + `from_storage_str`），有 3 条用例；M7 各片**不得**内联字面量。**不改 `ChannelKind::as_str`**（M4 的 `IssueOrigin` 与 24 条路由已按 `lark` 落） |
| **R-M7-11** | `mc-plugin-host::credentials::SecretBox::seal(plaintext, rng)` 需要外部熵源，而 M7 面（`mc-secrets::secretbox`）的 `seal` 自己取随机 nonce、另给 `seal_with_nonce` 做可复现向量 | 两份实现**签名不同**是**故意的**：M7 面要给各 adapter 一个"拿来就用"的封装口（平台帧里没有 rng 参数），可复现性走 `seal_with_nonce`。收敛时统一签名即可（R-M7-4 的同一票） |

### 10.7 交接给 M7 各片的三条硬约束（逐条可测）

1. **`routes/channels/mod.rs` 的路由账是唯一真值**：24 条逐条带 `router.go` 行号；各片只填自己
   那一份文件（`slack.rs` 4 / `telegram.rs` 4 / `dingtalk.rs` 7 / `lark.rs` 5 / `wecom.rs` 4），
   **不新增文件、不改 `mod.rs`、不改 `mount.rs`**。
2. **形态只有一种**：`dual-form required: 0` ⇒ 只注册上游字面量那一形态；补尾斜杠 = `EXTRA_ALIAS`，
   漏 = `MISSING_EXACT`，**没有 allowlist 退路**。路径参数写 `:name`（`{name}` 恒 404）。
3. **`group-routes` 必须保持不存在**（`GET /api/workspaces/:id/dingtalk/group-routes` 返回 404）——
   M7 波唯一一条**反向**验收（`docs/60` §1.6），另一条 `GET /api/agents/:id/dingtalk/groups`
   挂在既有 agents 子路由内部、由 M7-9 注册（不在 `mount.rs` 的合并点）。

## 11. M8-0 anchor（`LUM-1797`）：文件→写者表与偏离登记

`docs/61-M8-PLAN.md` §5 的「每文件预扩展清单」是**锚点文件集**；本节的表是它落地后的**准确版**
（含锚点期做的七处归位判断）。M8 后续切片按本表认领写集，**不得**编辑左列之外的共享文件
（`Cargo.toml` 根 / `Cargo.lock` / 各 `lib.rs` / `routes/{mod,mount}.rs` / `routes/{github,vcs,mcp,composio}/mod.rs` /
`state.rs` + `state/integrations.rs` / `routes/auth.rs` / `routes/issues/mod.rs` /
`apps/mc-server/src/{main.rs,integrations.rs}` / ⑦ 基线 / `slash-alias-allowlist.tsv` 全部由锚点冻结）。

> **号段复核（正文里的「§9.12」是计划期占位号）**：`docs/32` 的 `## 9.` 是 M6-0 anchor
> （`LUM-1665`）、`## 10.` 是 M7-0 anchor（`LUM-1765`）⇒ 本节取 **`## 11.`**。这正是
> `docs/61` §5 要求「号段起手复核」的原因：计划文书写的号在 M7-0 合并后已过期。

### 11.1 锚点冻结的聚合/共享文件（M8 各片只读，写各自子文件）

| 冻结文件 | 谁写 | 说明 |
| --- | :-: | --- |
| `crates/mc-http/src/routes/mount.rs` | anchor | 加 `mount_slice_code_artifacts()` + `.merge(...)` 一行；锚点期**只贡献 1 条注册键**（搬运的 501 占位） |
| `crates/mc-http/src/routes/mod.rs` | anchor | 声明 `github` / `vcs` / `mcp` / `composio` **四个**面 |
| `crates/mc-http/src/routes/{github,vcs,mcp,composio}/mod.rs` | anchor | 四份路由账（逐条带 `router.go` 行号）+ 子模块声明 + 子 router 的 `merge` 点 |
| `crates/mc-http/src/state.rs` | anchor | `github_keys` / `vcs_keys` / `composio_keys` 三字段 + `AppState::new` 内读 env（**不新增参数**） |
| `crates/mc-http/src/state/integrations.rs` | anchor | 三组部署密钥的**拆分读取口**（R-M8-7：`state.rs` 已超 620 行阈值） |
| `crates/mc-http/src/routes/auth.rs` | anchor | 测试里唯一的 `AppState { … }` 字面量补三字段（**压缩注释回基线 1704**，见 11.2 第 5 条） |
| `crates/mc-http/src/routes/issues/mod.rs` | anchor | **搬运**（不是删除）`/api/issues/:id/pull-requests` 的 501 占位 → `routes/github/issue_pr.rs` |
| `crates/mc-core/src/{vcs,github,mcp,composio}.rs` + `mcp/overlay.rs` | anchor | 跨 crate 的领域层：4 个模块的**完整类型形状** + overlay 纯函数签名 |
| `crates/mc-vcs/**` / `crates/mc-vcs-github/**` / `crates/mc-composio/**` | anchor | 三个新 crate 的骨架（`port.rs` / `app.rs` 的 RS256 是**完整实现**，其余是签名 + `todo!()`） |
| `crates/mc-repos/src/lib.rs` + `{vcs,github,mcp,composio}/mod.rs` + `task/{mod.rs,overlay.rs}` | anchor | 模块树 + 14 张表 → 子文件的归属表 + overlay 写入原语桩 |
| `apps/mc-server/src/integrations.rs` + `main.rs` | anchor | PR 刷新宿主位 + 停机链的一行（**先停渠道 → 再停 PR 刷新 → 再停调度器 → 最后停 actor**） |
| `Cargo.toml`（根）+ `Cargo.lock` | anchor | 唯一新增外部直连边 = `ring`（已在 lock 里）；lock 只重新生成（见 11.5） |

### 11.2 锚点期**归位判断**（七处，`docs/61` §5 原表未覆盖或写法不同）

| 事项 | 原表 | 本锚点落点 | 理由 |
| --- | --- | --- | --- |
| `crates/mc-core/src/mcp/overlay.rs` | 写者记 **M8-3**（§3.3） | anchor **建桩**（`merge_task_overlay` 签名 + `todo!()`），M8-3 原地填充 | `mcp.rs` 是 anchor 冻结的文件而 overlay 是它的子模块 ⇒ 若 anchor 不声明 `pub mod overlay;` 并落桩文件，crate **编译不过**；若留给 M8-3 声明，M8-3 就得改 anchor 冻结文件。M7-0 对 `engine/{router,supervisor,resolvers}.rs` 是同一处理（本片是它的第二次应用） |
| `crates/mc-repos/src/{vcs,github,mcp,composio}/` 的子文件（`connection.rs` 等 10 个） | 各面切片写者（§3.3） | anchor 建为 **doc-only 桩**（只 `pub mod` + 表说明，无代码） | 同上：`mod.rs` 声明子模块就必须有文件。逐字复刻 M7-0 对 `mc-repos/src/channel/` 8 个文件的做法（每个 13 行 doc-only） |
| `crates/mc-http/src/routes/{github,vcs,mcp,composio}/` 的 12 个子文件 | 各面切片写者（§3.3） | anchor 建为**空 `Router::new()` 桩**（`github/issue_pr.rs` 例外：承接搬运的占位） | 同上的模块树闭合；且这正是 `mount.rs` 「anchor 期零新注册键」的形态证据 |
| `mc-telemetry::redact::SENSITIVE_KEYS` 是否补 `*token` / `*secret` / `app_private_key` / `x-api-key` | §2.4 第 4 条说「anchor 一次性补齐」 | anchor **不动** `crates/mc-telemetry/src/redact.rs`（**不在本片写集**），把裁定写在这里 | 逐条实测：现有 `SENSITIVE_KEYS` 已覆盖 `token` / `secret` / `key` 子串 ⇒ `app_private_key`（含 `key`）、`x-api-key`（含 `key`）、`*token` / `*secret` 全部命中。补 `app_private_key` 会把 `app_key` 这类**客户端标识**也抹掉（上游刻意把标识明文入日志） |
| 根 `Cargo.toml` 的 `ring` 直连边 | §3.1 / §5 说「最多新增一条」 | 落 `[workspace.dependencies]` 的 `ring = "0.17"`，由 `mc-vcs-github` 以 `{ workspace = true }` 消费 | R-M8-8：`ring 0.17.14` **已在** `Cargo.lock`（传递依赖）⇒ **零新外部包**；与 M7-0 把依赖面一次定死同款 |
| `crates/mc-http/src/state.rs` 的行预算 | §6.3 预飞给两条分支（>620 则拆 `state/integrations.rs`） | 实测起手 `state.rs` = **640 行**（M7-0 后） ⇒ **拆**：读 env + 构造下放 `state/integrations.rs`，`state.rs` 只加三字段 + 三行构造 | R-M8-7 的预设阈值（620）**已被触发**；不拆则 `state.rs` 逼近门 ⑩ 的 800 行上限且会把余量吃光 |
| `tests/` 里 `AppState` 字面量（`routes/auth.rs`） | 「唯一字面量构造点补字段」 | 补三字段的同时**压缩同文件的注释**（`google_login` 的偏离登记 3 行 → 2 行），把行数**压回基线 1704** | 门 ⑩ 的基线 `crates/mc-http/src/routes/auth.rs 1704` **只允许变短**（M6-0 / M7-0 的同一手法）。⇒ **任何后续切片都不要再往 `auth.rs` 加行** |

### 11.3 `mc-core` 的三个**不做什么**（登记为偏离，避免两处定义漂移）

1. **`vcs` 与 `github` 两套并列，不合并**（`docs/61` §1.6）：`github_pull_request` 没有
   `connection_id` / `additions` 而有 `installation_id` 与快照管道；把 `VcsProviderKind` 扩成含
   GitHub 的枚举会让 `Provider` trait 长出只有 1/3 实现者用得上的方法。
2. **`from_str` 刻意不实现 `std::str::FromStr`**：上游语义是「未知值 ⇒ 不认识的 kind」，
   必须显式处理；`FromStr` 的错误类型会诱导调用侧用 `?` 掩盖它。6 个 `from_str` 都带
   `#[allow(clippy::should_implement_trait)]`（理由写在源码里，不是为了躲 lint）。
3. **`WorkspaceMcpServer` 的 `config` 手写脱敏 `Debug`**：它是含第三方凭证的 JSONB
   （`headers` / `env` 的值）⇒ `Debug` 只列 transport 与 `created_by`，**绝不**整体打印；
   `finish()` 用到全部字段以满足 `clippy::missing_fields_in_debug`。

### 11.4 依赖与工具链偏离

| 事项 | 落法 | 为什么 |
| --- | --- | --- |
| 根 `Cargo.toml` | **只加一行** `ring = "0.17"`（`members` 是 glob，不动） | R-M8-8；`ring` 已在 lock |
| `mc-vcs` 的三方依赖 | 全部 workspace 既有版本（`tokio` / `async-trait` / `serde` / `serde_json` / `thiserror` / `tracing` / `reqwest` / `http` / `url` / `hmac` / `sha2` / `hex`） | 零新外部包；锚点把依赖面一次定死 |
| `mc-vcs-github` | 追加 `mc-repos` / `base64` / `ring` | 同上（`ring` 是唯一新增直连边） |
| `mc-composio` | 追加 `mc-repos` / `base64` / `url` / `hex` | 同上 |
| 新增两条 `path` 边（app 侧） | `apps/mc-server → mc-vcs-github`、`apps/mc-server → mc-composio` | 与 M5-9 / M7-0 同手法：让宿主**真的被链进二进制** |
| 新增三条 `path` 边（http 侧） | `mc-http → mc-vcs` / `mc-vcs-github` / `mc-composio` | 否则 M8-1..M8-6 会各自回来改共享 manifest |
| `mc-vcs-github → mc-vcs` **不建** | 无这条边；`mirror.rs` 用本 crate 的 `payload::PullRequestWebhookPayload` | 保持 `docs/61` §2.2 声明的依赖方向（GitHub 面不依赖 VCS provider 抽象） |
| PKCS#1 PEM（`BEGIN RSA PRIVATE KEY`）**不收** | `app::pkcs8_der_from_pem` 只认 PKCS#8（`BEGIN PRIVATE KEY`） | `ring` 的 `RsaKeyPair::from_pkcs8` 只吃 PKCS#8；转 PKCS#1 需要 `rsa` crate，而本片刻意只接**一条**直连边。上游 `jwt.ParseRSAPrivateKeyFromPEM` 两种都收 ⇒ 这是**登记过的收缩** |

### 11.5 本锚点的门禁读数（逐字取自当轮日志）

```
①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0 · ⑦route-parity 0
⑨conformance 0 · ⑩file-size 0                              ⇒ 8/8 PASS / 122s
（⑥db / ⑧schema-drift 未跑：本机无 PostgreSQL 服务；本片 0 迁移、0 DB 触碰 ⇒ 归 M8-7 跑一次）
```

- ⑦（**当场实测，本片不刷基线**）：`upstream 456 (commit f41fae6b08fb) | local 406 registered |
  baseline 406`、`implemented 326 real + 4 placeholder = 330 / 456`、`known_gap 126`、
  `unclaimed 0`、`regression 0`、`local_only 9`、`gaps by owner: M9=33 M7=24 M8=24 M3+=16
  M2-A=13 M3=11 M10=5` —— 与 `docs/61` §6.1 的 M8-0 行**逐字相同**（这是**第二个不刷 ⑦ 基线的
  anchor**：本波**没有** M0 占位可删，唯一动作是 1 条 501 占位**原地搬运** ⇒ 注册键集合与
  `implemented_placeholder` 计数都不变，机制与 M7-0 的「零删除」不同）。
- ⑦ 第二条（形态）：`slash_alias_audit.py --quiet` = exit 0（M8 `dual-form required: 0`，
  **没有** allowlist 退路）。
- ⑩：0 违规；`scripts/file_size_baseline.tsv` **未动**（`routes/auth.rs` 压回 1704 = 基线）。
- ⑨：`report matches crates/mc-conformance/report.json`（本片零 fixture 改动 ⇒ 未漂移）。
- `cargo metadata`：workspace 成员 **36**（M7-0 时 33 ⇒ **+3**），三个新 crate 全在内。
- `Cargo.lock`：`+77 行` = 三个 `[[package]]`（`mc-vcs` / `mc-vcs-github` / `mc-composio`）
  + 依赖边；**零新外部包**（`ring` 原本就在 lock 里，只多一条 `mc-vcs-github` → `ring` 的边）。
- 本片自测（anchor 的三类测试）：`mc-vcs` 的 `signature` 三条（HMAC 往返 + 差分反例 +
  常量时间比较）、`mc-vcs-github` 的 `app` 三条（**RS256 签发→验证往返** + JWT 三段/claims +
  非 PKCS#8 拒绝）、`port` 三条、`ghsnapshot::client` 一条、`rest`/`dto` 各一条、
  `mc-composio` 的 client/service/state 三条、`mc-http::state::integrations` 五条、
  `mc-server::integrations` 两条 —— **全部通过**。

### 11.6 风险登记（承接 `docs/61` §8；R-M8-1…R-M8-10 由**本 anchor 一次落**）

| ID | 内容 | 锚点落法 / 归属 |
| --- | --- | --- |
| **R-M8-1** | 平台不可真连（GitHub API/GraphQL、composio、自建 Git） | 替身接缝已由 anchor **物理落定**：`mc_vcs_github::rest::GithubClient::api_base`、`ghsnapshot::Client::with_api_base`、`mc_composio::ComposioClient::with_api_base`、VCS 的 per-connection `instance_url`；替身三条纪律写进各片 DoD（`docs/61` §4.2） |
| **R-M8-2** | 凭据面最大（4 类部署密钥 + 3 种密钥形态） | 读取口**唯一**：`state::integrations::{GithubKeys,VcsKeys,ComposioKeys}`；三者的 `Debug` 全部手写脱敏 + 5 条用例断言「值不出现」；VCS 明文入库即失败（M8-2 的 DoD） |
| **R-M8-3** | installation token 刷新竞态 | `token_cache.rs` 的 `InstallationTokenCache` + `get_or_fetch`（单飞）已定形状；实现归 M8-1（并发 8 请求只换 1 次的 DoD） |
| **R-M8-4** | webhook 重放与验签（三种方案） | 常量时间原语已落 `mc_vcs::signature`（`verify_hmac_sha256_hex` / `verify_plaintext_token` / `constant_time_eq`），带差分反例用例；GitHub 的 `webhook.rs` 验签归 M8-4。**不做**时间戳窗口（上游也没做，等价而非缺口） |
| **R-M8-5** | ghsnapshot 限流/退避；worker 跨副本语义 | 与 R-M7-1 同源：本仓无 Redis ⇒ **单副本部署契约**；三级退避（`rateLimitPause` / `deferActive` / `scheduleRetry`）归 M8-5 |
| **R-M8-6** | MCP 条目 write-only，但 `config` 是 JSONB | 领域层 `WorkspaceMcpServer` 的 `Debug` 已脱敏；`McpTransport` 三值校验给 M8-3 一个单一判据（响应不含值字段是 M8-3 的 DoD） |
| **R-M8-7** | `state.rs` 逼近 ⑩ 硬限 | **已触发并处理**：起手 640 行 > 620 ⇒ 拆 `state/integrations.rs`（见 11.2 第 6 条）；`state.rs` 收尾 665 行 |
| **R-M8-8** | 无 `jsonwebtoken`，RS256 要选实现 | **已裁定并实测**：选 `ring 0.17`（`RSA_PKCS1_SHA256`），零新外部包，签发→验证往返 + 3 条反例全部通过（见 11.4 / 11.5） |
| **R-M8-9** | composio overlay 只交付到「可注入」 | `mc-core/src/mcp/overlay.rs`（合并纯函数，M8-3）+ `mc-repos/src/task/overlay.rs`（写入原语，anchor 建桩）；「3 处 enqueue 接线」**不在本波写集**，由 M8-7 登记尾账 ⇒ `runtime_mcp_overlay` 恒 `NULL` 在本波后**仍然成立**（登记过的缺口） |
| **R-M8-10** | `docs/32` 是 M7/M8 唯一共享文档写点 | M7-0 已合 ⇒ 本节（`## 11.`）与 M7 各片的 `§9.x` / `§10.x` 不同段；同轮若有冲突由后合者 rebase 保留两段 |

### 11.7 交接给 M8 各片的四条硬约束（逐条可测）

1. **四份路由账是唯一真值**：`routes/{github,vcs,mcp,composio}/mod.rs` 各自逐条带 `router.go`
   行号；各片只填自己那一份**子文件**，**不新增文件、不改 `mod.rs`、不改 `mount.rs`**。
2. **形态只有一种**：`dual-form required: 0` ⇒ 只注册上游字面量那一形态；补尾斜杠 = `EXTRA_ALIAS`，
   漏 = `MISSING_EXACT`，**没有 allowlist 退路**。路径参数写 `:name`（`{name}` 恒 404）。
3. **`pull-requests` 路由必须保持存在**（M8-4 把 `github/issue_pr.rs` 的 501 占位换成真实现，
   **不能删**）；公开块 4 条（`github/setup` / `webhooks/github` / `webhooks/vcs/:connectionId` /
   `composio/callback`）**不得**挂会话 middleware（`docs/61` §2.7 第 4 条）。
4. **凭据只经 `secretbox` 或 `state::integrations`**：任何 handler / DTO 不得有明文 secret 的
   `Debug` / `Display` / 日志插值；其余面（`mc-mcp` / `mc-daemon/src/mcp/**` /
   `mc-repos/src/plugin/**`）是**只读**的，不得重复实现（`docs/61` §2.3）。

---

## 12. M8-1（`LUM-1798`）：GitHub App 安装/回调/仓库浏览面（5 路由）的落点、偏离与门禁读数

`docs/61-M8-PLAN.md` §4.1 的 **stage 2 第一片**（`5` 路由 / 上游 `github.go` L1–L963 +
`ghsnapshot/client.go`）。本节是它在 `docs/32` 的**自己那一段**（`docs/61` §3.3 / §6.5 第 7 条；
号段起手复核照 §11 开头那段 —— 计划书写的「§9.12」是计划期占位号，M8 面实际落在 `## 11.` 之后
⇒ 本节取 `## 12.`）。

### 12.1 落点（写集逐字）

| 文件 | 角色 | 内容 |
| --- | --- | --- |
| `crates/mc-vcs-github/src/rest.rs` | **实现**（M8-1） | `GithubClient`：App JWT 换 installation token（**期望 201**）、`GET /app/installations/{id}`（**永不失败**，失败回落 `unknown`/`User` 占位）、`GET /installation/repositories`（分页算术 = 上游 `page*per_page < total_count`）、`DELETE /installation/token`（**尽力而为**，5s 超时）、通用 `graph_ql`；`retry_after_secs`（`Retry-After` → `X-RateLimit-Reset` → 60s，钳 `[1s,5m]`）；响应体上限 `4 MiB`（上游 `githubAPIResponseLimit`） |
| `crates/mc-vcs-github/src/token_cache.rs` | **实现**（M8-1） | `InstallationTokenCache` + `get_or_fetch` 的**单飞**（每 installation 一把 `tokio::sync::Mutex` + 二次检查），续期余量 5 分钟；见 D2 |
| `crates/mc-vcs-github/src/dto.rs` | **实现**（M8-1） | `GithubPageParam::parse`（上游 `parseGitHubPageParam` 的逐字边界）、`Github{Installation,Installations,Connect,Repository,Repositories}Response`；见 D3 / D4 |
| `crates/mc-vcs-github/src/ghsnapshot/client.rs` | **实现**（M8-1） | `Client`：`sign_app_jwt`（RS256，`iat-60s` / `exp+9min`）、`installation_token`（**缓存 + 单飞**）、`graph_ql`、`revoke_token`；`api_base` 接缝保留；见 D1 |
| `crates/mc-repos/src/github/installation.rs` | **实现**（M8-1） | `github_installation` 的 upsert / 列 / 取 / 删 + `github_pending_installation` 的取 / 删 / upsert；见 D9 |
| `crates/mc-repos/src/github/pull_request.rs` | **实现**（M8-1，M8-4 只读） | `github_pull_request` 全列投影 + `find` / `upsert`（`mergeable_state` **三态**）+ `issue_pull_request` 的 `link`（`close_intent` 两种保持语义）/ `unlink` / `list_issue_ids_for_pull_request` / `list_by_issue`（按 `snapshot_head_sha` 聚合 check）；见 D11 |
| `crates/mc-http/src/routes/github/install.rs` | **实现**（M8-1） | `connect` / `installations` / `repositories` / `delete` 四条 handler + `GithubScope`（member 门）+ `error_with_code` + `github_api_base` 接缝 + 百分号编码；见 D6 / D7 |
| `crates/mc-http/src/routes/github/setup.rs` | **实现**（M8-1） | 公开回调（**6 个失败分支** → 302 + `&github_error=<kind>`）、state 的签/验（**手写 HMAC-SHA256**，见 D8）、`github_settings_url`、`frontend_origin_from_env`；同时是 `install.rs` 的 state 依赖（`sign_state_for_return` / `is_allowed_return_to` / `RETURN_TO_GITHUB`） |
| `crates/mc-http/src/routes/github/dto.rs` | **再导出面**（M8-1） | 只把 `mc_vcs_github::dto` 的类型转出给 HTTP 层 |
| `crates/mc-http/tests/github/{main,support,install,setup}.rs` | **新增测试**（M8-1） | 真库 + 离线替身的端到端（16 例） |
| `crates/mc-vcs-github/tests/rest_stub.rs` | **新增测试**（M8-1） | 零依赖的 HTTP/1.1 替身 + 真实 wire 断言（10 例，含并发单飞） |

**只读**（未改一个字节）：`crates/mc-secrets/src/secretbox.rs`、`state.rs` / `state/integrations.rs`、
`routes/{mod,mount}.rs`、`routes/github/mod.rs`、`crates/mc-mcp/**`、`crates/mc-daemon/src/mcp/**`、
`crates/mc-repos/src/plugin/**`、`Cargo.toml` 根、`Cargo.lock`、`docs/fixtures/**`、`apps/**`。

### 12.2 偏离登记（M8-1-D1 … M8-1-D12）

| ID | 事项 | anchor 期 / 计划书 | 本片 | 理由 |
| --- | --- | --- | --- | --- |
| **M8-1-D1** | `ghsnapshot/client.rs` 的 `fetch_pr_snapshot` 桩 | 签名 + `todo!()` | **删除**，换成通用 `Client::graph_ql(installation_id, query, variables, now_unix)` | GraphQL 的**查询文本**在上游住在 `snapshot.go`（= 本仓 M8-5 的 `ghsnapshot/snapshot.rs`）。把它抄进 `client.rs` 等于两个写者各持一份查询；上游分层就是 `snapshot.go` 调 `client.graphQL(...)`。`snapshot.rs` 的 `parse_pr_snapshot(&Value)` 桩签名**未动** ⇒ M8-5 只需自己驱动分页 |
| **M8-1-D2** | `token_cache::get_or_fetch` 的签名 | `(cache, id, now) -> Result<InstallationToken>`（**无 fetch 回调**） | `(cache, id, now, fetch: FnOnce() -> Future<...>)` | anchor 的签名拿不到「怎么换 token」⇒ 无法实现单飞。单飞用「每 installation 一把 `tokio::sync::Mutex` + 等锁后二次检查」，**不新增依赖**（上游用 `golang.org/x/sync/singleflight`）；`fetch` 是 `FnOnce` ⇒ 只在真的要发请求时调用一次 |
| **M8-1-D3** | 三个响应 DTO 的字段名 | anchor 暂定形状（`install_url: Option<String>`、无 `workspace_id` / 有 `connected_by_id` / `installation_id: i64`、repository 的 `name`+`owner`） | 逐条对齐**上游字面量**：`url: String`（未配置 = 空串）、加 `workspace_id`、`installation_id: Option<i64>`（**非 admin 整个字段缺席**，不是 `null`）、去掉上游没有的 `connected_by_id`、repository 八字段（`id/full_name/html_url/clone_url/description/private/archived/default_branch`）+ `GithubRepositoriesResponse` 信封 | `docs/61` §1.2 要求这两条响应映射与上游逐字段对应；`installation_id` 是 Connect/Disconnect 的**管理手柄**，上游对非 admin 整段省略 |
| **M8-1-D4** | `GithubPageParam::parse` 的**失败语义** | anchor 契约写「非法 / 缺失 / 越界都归一到安全值」，签名不可失败 | 改 `Result<Self, GithubPageParamError>`：缺省取默认（`page=1` / `per_page=100`），非法 / 越界 ⇒ **400**（`invalid page` / `invalid per_page`） | 上游 `parseGitHubPageParam` 对这两种情形一律 `writeError(400, "invalid "+name)`。静默归一会把客户端的**分页 bug** 变成沉默的错页（等价性优先于「宽容」） |
| **M8-1-D5** | `ExchangedInstallationToken` 的 `Debug` | **派生**（会打印 token） | 手写脱敏（只留 `<redacted>` + `expires_at`） | anchor 期误派生与 `docs/61` §2.4 第 1 条直接冲突；本片补 `exchanged_token_debug_never_echoes_the_token` 用例 |
| **M8-1-D6** | setup 回调的「未配置 / state 非法」状态码 | `docs/61` §2.5 写「400/401」 | **302 + `&github_error=<kind>`**（`missing_params` / `invalid_state` / `bad_installation_id` / `bad_workspace` / `persist_failed`），成功 `&github_connected=1` | 上游 `GitHubSetupCallback` 在**任何**失败分支都 `http.Redirect(..., StatusFound)`（回错误码页面会把用户卡在 GitHub 那侧）。⚠️ 用 `(StatusCode::FOUND, Location)` 手写而不是 `axum::response::Redirect::to`（后者是 **303**）。`repositories` 的未配置语义同时钉为 **403** `github_repository_browsing_not_configured`（上游 `writeFeatureDisabled` 故意不是 503） |
| **M8-1-D7** | `githubAPIBase` 的等价物位置 | 计划书说「`GithubKeys` 里没有，按 §4.2 注入」 | 落 `mc_http::routes::github::install::{github_api_base,set_github_api_base,reset_github_api_base}`（进程级 `Mutex<Option<String>>`，缺省 `https://api.github.com`） | `state::integrations::GithubKeys` 是 anchor 冻结的四字段（**没有** api_base），而离线替身必须有唯一接缝。用 `Mutex` 而非 `OnceLock`：同一测试二进制里既跑「缺省 base」又跑「注入 base」的用例。测试侧用 `STUB_LOCK` 串行（与 `tests/skills/import.rs` 的 `MOCK_LOCK` 同款） |
| **M8-1-D8** | state 的 HMAC-SHA256 | 直接用 `hmac` crate | `setup.rs` 里用 `sha2` 按 **RFC 2104 手写**（ipad/opad），常量时间比较也手写 | `mc-http` **没有** `hmac` 依赖，而 manifest 在 M8-0 之后冻结（`docs/61` §3.1：M8 各代码片不得改 manifest / lock）。正确性用 **RFC 4231 的官方向量**（Test Case 1/2/6）钉住 |
| **M8-1-D9** | `github_pending_installation` 的时间列 | anchor 的子文件文档写 `created_at` | **`received_at`**（迁移 `120` 逐字），`account_type` 是 `NOT NULL DEFAULT 'User'` + CHECK | 第一版照 anchor 的措辞写成 `created_at`，实测 `get_pending` 直接报 `column "created_at" does not exist` ⇒ **全部** setup 回调变 `persist_failed`（e2e 立刻抓到）。`upsert_pending` 把 `None` 折成 `'User'`：显式绑 `NULL` 会**违反** NOT NULL（默认值只在列缺席时生效） |
| **M8-1-D10** | 删除路径是否撤销 GitHub 侧凭据 | `docs/61` §2.5 的 delete 行未说 | **不调 GitHub**（只删本仓的行）⇒「撤销 installation token 失败不回滚删除」在本片**恒真** | 上游 `DeleteGitHubInstallation` 只跑一条 `DELETE ... WHERE id=$1 AND workspace_id=$2`。`revoke_installation_token` 只出现在 browse 路径的 `defer` 位置（best-effort，失败不影响响应） |
| **M8-1-D11** | `mc-repos/src/github/pull_request.rs` 的范围 | 计划书把该文件记在 M8-1 写集，M8-4 只读 | 本片**一次给全** M8-4 需要的读取面（全列投影 + `find`/`upsert`/`link`/`unlink`/`list_issue_ids`/`list_by_issue`） | 该文件的写者只有 M8-1（单写者纪律）⇒ 若只留桩，M8-4 的两条路由**无处安放**查询。仍属 M8-4 的：`check_suite.rs` / `pending.rs`（两张 check 表的写面）与 `routes/github/issue_pr.rs` / `webhook.rs` 的 handler |
| **M8-1-D12** | `crates/mc-repos/src/github/mod.rs` 的模块头 | 仍写着「四个子模块都是 doc-only 桩」 | **不改**（文案债） | 该文件是 anchor 冻结的；改它等于本片动了冻结面。登记在此，归 M8-7（INT）在收口轮一并订正 |

### 12.3 专属 `DoD` 的证据（`docs/61` §6.5 的 M8-1 行逐条）

| `DoD` 条目 | 证据（用例 / 命令） |
| --- | --- |
| 5 条路由的**「未配置 + 未授权」矩阵逐端点** | `crates/mc-http/tests/github/install.rs`：`connect_is_admin_only_and_unconfigured_is_200_not_503`（admin 200 / member+guest 403 / outsider 404 / 非法 ws 400 / 非法 return_to 400 / **未配置 200 + `configured:false`**）、`installations_are_member_visible_with_role_gated_management_handle`（三档角色 + `installation_id` 按角色缺席）、`repositories_require_admin_and_a_configured_app`（**403 + 稳定 code**）、`delete_installation_is_admin_only_and_idempotent`；公开回调的六分支见 `setup.rs` 的 `setup_callback_redirects_every_failure_with_an_error_flag` |
| App JWT：**签发-验证往返 / `exp` 边界 / 时钟偏移** | `mc-vcs-github/src/app.rs` 的三条（anchor 落）+ `tests/rest_stub.rs::client_signs_app_jwt_and_uses_bearer_for_graphql`（替身收到的那枚 Bearer 被**当场用公钥验签**，并断言 `iss`） |
| installation token 缓存 + **单飞**（并发 8 ⇒ 只换 1 次） | `token_cache.rs::eight_concurrent_callers_mint_exactly_one_token`（计数器 + 30ms 真 `await` 打开并发窗口）与 `tests/rest_stub.rs::eight_concurrent_requests_mint_exactly_one_installation_token`（**替身台账**只记到 1 次 `POST .../access_tokens`）；暖缓存路径见 `warm_cache_skips_the_second_token_exchange` |
| 仓库**分页边界**（上游 `parseGitHubPageParam`） | `dto.rs::page_param_defaults_and_boundaries`（缺省 / 空 / trim / 四个上下界 / `1.5` / `1e3` / 两错先报 page）+ `install.rs::repositories_reject_invalid_page_params_with_400`（六个非法 query 逐条 400，且发生在**任何出站调用之前**）+ `tests/rest_stub.rs::repository_pagination_follows_upstream_arithmetic` |
| **离线替身端到端**（路由 → App JWT → token 交换 → 分页列表 → 真库） | `install.rs::offline_stub_end_to_end_connect_callback_browse_delete`（connect 取 state → 公开回调落库 → 列表 → 100 条分页 + `next_page=2` → 末页 `next_page=null` → 删除 204）；替身只替**平台 wire**（axum 起的 `/app/installations/{id}`、`/access_tokens`、`/installation/repositories`、`/installation/token`），中间零 mock |
| 凭据：手写脱敏 + 「错误路径不回显」+ redaction | `rest.rs::exchanged_token_debug_never_echoes_the_token`、`ghsnapshot/client.rs::malformed_private_key_never_leaks_key_material`、`tests/rest_stub.rs::exchange_rejects_401_and_non_201_without_echoing_the_body`（**响应体逐字不出现在错误里**）、`installations` 用例断言 member 响应里 `installation_id` **缺席**。`mc_telemetry::redact` 的覆盖裁定沿用 §11.2 第 4 条（现有 `SENSITIVE_KEYS` 已覆盖 `key`/`token`/`secret` 子串） |
| 广播面 | `delete_publishes_a_broadcast_event`（`github_installation:deleted` + payload `{id}`）；setup 成功路径发 `github_installation:created`，载荷是**最弱角色视图**（不带 `installation_id`，上游注释逐字） |

### 12.4 门禁读数（逐字取自当轮日志；日志留档在 run workdir 的 `gates-m8-1*.log`）

**两轮跑完（同一棵树：第二轮只重跑被磁盘打断的三道门；此后只追加本节文档）**：

```
第一轮：①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0 · ⑦route-parity 0 · ⑩file-size 0
        （⑥/⑧/⑨ 因 **磁盘满** 红：`No space left on device`，见下）
第二轮（`--only db,schema-drift,conformance`）：⑥db 0（migrate=0, e2e=0）· ⑧schema-drift 0 · ⑨conformance 0
                                                              ⇒ 合计 **10/10 PASS**
```

- **【lesson·门 ⑥/⑨ 的「红」可能是磁盘满，不是代码错】** 第一轮 ⑥ 的 e2e 与 ⑨ 都报
  `No space left on device`（⑨ 连 `target/debug/.fingerprint` 都建不出来），⑧ 的 scratch 建库也失败
  （`could not create directory "base/…"`）。判据：`df -h /` 100%、`du -sh target` **18G**。
  唯一回收动作 = `rm -rf target/debug/incremental`（**8.5G**，本 run 自己的 target）⇒ 腾出 8.1G 后
  三门一次全绿。同 run 的 `up-m8` 只读副本 98M、**未**动；同项目另一在飞片（`lum-1767`，16G）**未**动
  （它当时有活进程：`/proc/5111/cwd` 命中其 workdir）。
- ⑦（**本片会动读数**；`baseline 406 不动`）：`upstream 456 (commit f41fae6b08fb) | local 411 registered`、
  `implemented 331 real + 4 placeholder = 335 / 456`、`known_gap 121`、`unclaimed 0`、`regression 0`、
  `local_only 9`、`gaps by owner: M9=33 M7=24 **M8=19** M3+=16 M2-A=13 M3=11 M10=5`
  —— 与 issue 描述「起手补充（二）」的**片后预期**（`local 411 / implemented 335 (331+4) / known_gap 121`）
  逐字相同；`owners.M8 24 → 19`（−5 = 本片 5 条路由）。
- ⑦ 第二条（形态）：`slash_alias_audit.py --quiet` = **exit 0**（本片 5 条均为上游 plain 注册，
  `dual-form required: 0`；没有 allowlist 退路 ⇒ 三类缺陷都是硬失败）。
- ⑩：**0 违规**；`scripts/file_size_baseline.tsv` **未动**。本片最大新文件
  `crates/mc-http/tests/github/install.rs` 687 行、`crates/mc-vcs-github/tests/rest_stub.rs` 666 行
  （均在 800 硬限内；`cargo fmt --all` **之后**量的数）。
- ⑨：`report matches crates/mc-conformance/report.json`（本片 0 fixture 改动 ⇒ 未漂移；M8 面那条
  composio fixture 仍归 M8-6）。
- ⑧：schema-drift 绿（本片 **0 迁移**、0 表改动 ⇒ 与基线同形）。
- ⑥：`mc-migrate` 566 个迁移（第二轮 `applied 0` = 已迁移）+ `--ignored` 全绿；其中本片新增
  真库用例 **21** 例（`mc-repos::github` 5 + `mc-http --test github` 16）。
- ⑤（`cargo test --workspace`，**不带**库变量）：全绿；本片在其中新增 **37** 例**零 DB** 用例
  （`mc-vcs-github` 单元 24 中的 16 例新用例 + `tests/rest_stub.rs` 集成 10 + `mc-http` 模块内纯函数 10 +
  `mc-repos::github` 的 1 例纯函数投影）⇒ 连同真库 21 例，本片共 **58** 例新测试（逐文件计数：
  `rest.rs` 4 / `dto.rs` 4 / `token_cache.rs` +4 / `ghsnapshot/client.rs` +4 / `rest_stub.rs` 10 /
  `routes/github/install.rs` 2 / `routes/github/setup.rs` 8 / `repos/github/installation.rs` 4 /
  `repos/github/pull_request.rs` 2 / `tests/github/install.rs` 10 / `tests/github/setup.rs` 6）。

### 12.5 交接（给 M8-2 / M8-4 / M8-5 / M8-6 / M8-7）

1. **M8-4 读这三处**：`mc_vcs_github::dto::{GithubInstallationResponse,GithubInstallationRow→}`
   （`dto.rs` 的 installation 侧已对齐上游；**PR 侧仍是 anchor 暂定形状**，扩展（snapshot / checks /
   additions 三组字段）由 M8-4 决定，但**必须**改 `mc-vcs-github/src/dto.rs` 而不是 `mc-http` 的再导出面）、
   `mc_vcs_github::rest`（`explode`-free：`GithubClient` 的 `exchange_installation_token` +
   `revoke_installation_token` 可直接复用）、`mc_repos::github::pull_request`（读取面与 upsert 已就位，
   见 D11）。**webhook 的验签**用 `mc_http::routes::github::setup::{hmac_sha256, constant_time_eq}`
   （同一份手写实现，别再写第二份）。
2. **M8-5 读 `ghsnapshot/client.rs`**：`Client::{sign_app_jwt, installation_token, graph_ql}` 已实现；
   `installation_token` **自带缓存 + 单飞**（`with_token_cache` 可注入余量），`graph_ql` 返回信封里的
   `data`，`RateLimited` 由 `GithubError` 统一表达 ⇒ `snapshot.rs` 只需驱动 `$cursor` 分页与归一化
   （`parse_pr_snapshot(&Value)` 桩签名未动）。
3. **M8-2 / M8-6**：本片**没有**碰 `routes/{vcs,mcp,composio}/**`、`mc-vcs/**`、`mc-composio/**`、
   `state.rs`、`requests/mod.rs`、`mount.rs`、manifest —— 三片的写集与 M8-1 **零交集**（`docs/61` §3.3）。
   唯一共享点：`install.rs` 的 `error_with_code` / `percent_encode_*` 是 `pub(crate)`，若你的面也要
   「403 + 稳定 code」的响应体，**直接用**（不要再复制一份 envelope）。
4. **M8-7（INT）**：本片把 ⑦ 推成 `local 411 / implemented 335 / known_gap 121 / owners.M8 19`；
   `--write-baseline` **仍未到**（归 M8-7）。D12 的文案债与 §11.6 的 R-M8-9（enqueue 尾账）一并收口。

## 13. M7-3（`LUM-1768`）：slack 入站回路的落点、偏离与门禁读数

`docs/60-M7-PLAN.md` §4.1 的 **stage 2 第三片**（`0` 路由 / 上游 `slack/{slack_channel,inbound,resolvers,media_ingest,config,mrkdwn,channel}.go` = 1,786 行）。
本节是它在 `docs/32` 的**自己那一段**（`docs/60` §6.5 第 7 条；号段照 M8-1 的先例顺延 —— `docs/60`
计划期写的「§10.x」是占位号，M7 面实际在 `## 10.`，本片取**文件末尾的下一个号** `## 13.`）。

### 13.1 落点（写集逐字）

| 文件 | 角色 | 内容 |
| --- | --- | --- |
| `crates/mc-channel/src/slack/config.rs` | **实现**（M7-3） | `InstallConfig` / `Credentials` / `Decrypter` / `SlackDeps`；`decode_credentials`（**唯一**解释点）、`decode_public_config`（解不开回零值，不报错）、`install_team_id`（**不**回退 `app_id`，与 `decode_credentials` 的 team 回退**故意**不同）、`installation_serves_team`、`decrypt_token`（MIME 折行 base64 容忍）、`encode_ciphertext` |
| `crates/mc-channel/src/slack/inbound.rs`（+ `inbound/tests.rs`） | **实现**（M7-3） | `RawEvent` / `RawFile` / `SlackFile` / `EventBody` / `EventsApiEvent`；`raw_files_from`、`parse_events_api`、`inbound_from_event`（`message` / `app_mention` 两种内部类型）、`slack_chat_type`、`is_ingestable_subtype`、`MentionRe`（`<@U\|name>` 手写匹配器，本 crate 不引 `regex`） |
| `crates/mc-channel/src/slack/socket.rs`（+ `socket/tests.rs`） | **实现**（M7-3） | `SocketFrame` / `parse_socket_frame` / `ack_json` / `FrameAction` / `dispatch_frame`；`SocketSession` + `SocketTransport` 端口、`TungsteniteTransport`（`apps.connections.open` 引导 + `tokio-tungstenite`）、`SlackChannel`（`Channel` 五方法）、`factory` / `factory_with_decrypter` |
| `crates/mc-channel/src/slack/media.rs`（+ `media/tests.rs`） | **实现**（M7-3） | `is_slack_file_host` / `is_fetchable_slack_file_url` / `validate_download_url`；`MediaStorage` + `MediaFetcher` 端口、`ThreadedFetcher`（默认实现）、`SlackMediaResolver`（`has_media` / `resolve_media` / `ingest_one`）；`slack_file_content_type`（HTML 登录页不是文件）、`slack_media_kind`、`slack_media_object_key`（按 chat message 派生）、`slack_file_name` / `clean_file_name` / `media_extension` / `safe_media_segment` |
| `crates/mc-channel/src/slack/mrkdwn.rs` | **实现**（M7-3） | `format_mrkdwn` + 十个手写扫描器（`find_fenced` / `find_inline_code` / `find_md_link` / `find_slack_entity` / `find_blockquote` / `find_header` / `find_italic` / `find_delimited`）+ `Placeholders`（逆序还原） |
| `crates/mc-channel/src/slack/resolvers.rs`（+ `resolvers/tests.rs`） | **实现**（M7-3） | `InstallationRow`（不透明平台值）；`InstallationQueries` / `IdentityQueries` 端口 + 仓储实现；`SlackInstallationResolver` / `SlackIdentityResolver` / `SlackSessionBinder`；`session_routing`（纯函数）；`SlackResolverSet::{new,from_repos,with_media,with_replier,with_typing,into_engine_set}` |
| `crates/mc-channel/src/slack/mod.rs` | **实现**（M7-3） | 五个 `pub mod` + `pub mod socket` + 两个注册入口：`register`（**失败关闭**解密器 + 一条 warn）、`register_with`（宿主把 `ChannelKeys` 交进来） |

**写集勘误（**逐条登记**，照 M7-2 的先例 `engine/{commands,session}/tests.rs`）**：`docs/60` §3.3 给
M7-3 的五格是 `slack/{inbound,resolvers,media,mrkdwn,config}.rs`。本片**追加**四个路径，理由**只有一条**
= 门 ⑩ 的 **800 行硬限**（不是凑数）：

- `slack/socket.rs`：`inbound.rs` 的**代码面**（不含用例）已 ~940 行 ⇒ 按「归一化 / 传输」拆开，
  切点正好是上游 `inbound.go` 与 `slack_channel.go` 的边界；两文件相互 `pub` 引用，无环（`inbound`
  不引用 `socket`；`socket` 引用 `inbound::{inbound_from_event,parse_events_api,TYPE_SLACK}`）。
- `slack/{inbound,media,resolvers,socket}/tests.rs`：四文件用例内联后分别 1516 / 1242 / 1121 / 835 行
  ⇒ 用例拆成子模块。`config.rs`（618）与 `mrkdwn.rs`（646）**未拆**。

拆完 `crates/mc-channel/src/slack/**` 最大文件 **656 行**，`scripts/file_size_baseline.tsv` **未动**。

### 13.2 偏离登记（M7-3-D1 … M7-3-D6）

| # | 偏离 | 处置与理由 |
| --- | --- | --- |
| **M7-3-D1** | **会话隔离键的分隔符**：上游 `chat_id:线程根`，本仓的通用策略（`BindingKeyPolicy::ChatIdPlusThreadRoot`）用 `#` 分隔 | **隔离粒度完全一致**（一个频道里两个 `@bot` 线程 = 两个会话，`resolvers/tests.rs::session_routing_table` 钉住）；只有键的**字面形态**不同。出站（M7-4）取真实 channel id 时按分隔符前的**前缀**取。**不**改 M7-2 的通用策略（它在 `engine/session.rs`，是各适配器共用的） |
| **M7-3-D2** | **DM（p2p）线程内的回复落点**：上游把 DM 的回复送进线程（`ReplyThread = ThreadID`），本仓落回 DM 顶层 | 同一个字段既当**隔离键**又当**回复线程**，而 p2p 的隔离键**必须**是 chat id（否则同一条 DM 里的线程消息会被拆成两个会话，agent 直接丢上下文）。两害相权取"会话不分裂"；影响面 = DM 内**线程**回复的落点，DM 顶层回复不受影响 |
| **M7-3-D3** | **绑定行的 `config` 列**：上游写 `{"channel_id": …}`（复合键下出站要知道真实 channel id），本仓通用实现写 `null` | 真实 channel id 在隔离键的**前缀**里（见 D1），读得回来。若要落到列上，需改 M7-2 的 `NewEnsureSession` 传参面（不属于本片写集） |
| **M7-3-D4** | **跨安装身份复用**：上游有 `FindReusableChannelUserBinding`（同一个 Slack 工作区里第二个 app 不必重新提示绑定，MUL-3911），`mc-repos/src/channel/binding.rs` **没有**这条查询 | 本片只实现「按 `(installation, 平台用户 id)` 绑定」的主路径。**不是静默略过**：没有它只影响"第二个 app 的第二条消息要重新走一次绑定卡"，不影响安全与数据正确性。落地归**拥有 binding 仓储的那一片**（M7-1 的 `binding.rs` 已合、加查询要另开票） |
| **M7-3-D5** | **媒体端口的同步形态**：`MediaResolver::resolve_media` 是本仓的**同步**签名（M7-1 定的契约；上游是 ctx-async），而 workspace 的 `reqwest` 没开 `blocking` feature 且 M7 不许新增依赖 / feature | 默认取回器 `ThreadedFetcher` 在**独立线程**上跑一个 current-thread 运行时（对任何调用上下文都成立：不要求 multi-thread runtime、在运行时 worker 上不会 panic）；对象存储与意图账本是 `MediaStorage` / `MediaIntentLedger` 端口，宿主用自己的桥接实现（`mc-storage` 的 `StorageProvider::put` 是 async）。同时**没有 deadline 上下文**：上游按"剩余预算 ÷ 剩余文件数"分摊，本片每个文件用固定 `FILE_FETCH_TIMEOUT`，而"预算耗尽"由 Router 的 `resolve_remote` 门槛 + `MAX_FILES_PER_MESSAGE` 兜底（语义不变：预算耗尽 ⇒ **不**起新下载） |
| **M7-3-D6** | **`Channel::send` 失败关闭**：上游 `/slack/outbound` 面（`channel.go` + `replier.go`）属 **M7-4** | 本片返回 `ChannelError::Transport{"outbound (chat.postMessage) is not wired yet — lands in M7-4"}`，而不是交一个发不出去却自称 `TEXT` 的半成品。选 `Transport` 是因为它表达的正是"这条出站链路还不存在"，且 `send` **不在** supervisor 的退避路径上（不会被误重试）。`capabilities()` 仍照上游声明 `TEXT | THREAD_REPLY`（位图是**声明**，实现归 M7-4） |

**另有一条不构成偏离的**实现选择（记在这里免得被当漏项）：`register()` 的签名（`&Registry` +
`&ChannelDeps`）里没有部署密钥的位置，而密钥的**唯一读取口**是 `mc_http::state::ChannelKeys`
（`mc-channel` 不得自己 `std::env::var`，`docs/60` §3.1）。所以本片给两个入口：
`register`（宿主当前调用的那个）用**失败关闭**解密器 + 一条 `warn`（绝不把密文当明文），
`register_with` 是接线好的入口（`SlackDeps::with_secret_box`）。宿主装配点
`apps/mc-server/src/channels.rs` 属 anchor 写集 ⇒ 接线动作归引入它的那一片。

### 13.3 专属 `DoD` 的证据（`docs/60` §6.5 的 M7-3 行逐条）

| 专属验收 | 证据（`cargo test -p mc-channel --lib` 里的用例名） |
| --- | --- |
| Socket Mode 信封帧解析（`events_api`） | `slack::socket::tests::{socket_frames_parse, frame_actions}`（四种信封 + 四类畸形帧 + ACK 线形态逐字 `{"envelope_id":"e1"}`） |
| 事件去重 | 走 engine 的共享两阶段端口（本片复用 M7-2 的 `ChannelDeduper`）；`resolvers/tests.rs::unknown_installation_is_dropped_without_an_error` 钉住"判决不是错误"的那一半 |
| 未绑定发件人 ⇒ **回绑定卡**（nil error，不是失败） | 端到端：**真信封帧** → adapter 自己的翻译 → `SlackResolverSet` → `Router::route` ⇒ `Ok(())`、`replier` 收到 `Outcome::NeedsBinding`、审计一行 `unbound_user`。用例：`slack::resolvers::tests::unbound_sender_gets_a_binding_card_and_no_error` |
| `mrkdwn` 转换一组用例 | `slack::mrkdwn::tests::*`（11 条）：上游 `mrkdwn_test.go` 的 15 条表 + 栅栏保护的 2 条 + 手写扫描器的边界（贪婪 `\s+`、`#{1,6}` 上界、斜体最小匹配、链接里一层平衡括号） |
| 媒体引用抽取一组用例 | `slack::media::tests::*` 与 `slack::inbound::tests::file_share_keeps_only_fetchable_files`：可取文件的筛选（无 URL / 站外被丢）、`has_media` 纯解码、HappyPath（意图行**先于**上传）、非 Slack 主机**不取不传**、HTML 登录页不当文件、对账器接管的 key 跳过、一个失败不阻断其余、文件名回落（路径穿越 / 全点 / 空）、超限在**落意图行之前**被拒 |
| 入站是 push（adapter 自己跑接收循环 + 注入的 `InboundHandler`）| `slack::socket::tests::connect_acks_before_dispatching_and_survives_bad_frames`（替身传输喂脚本帧：ACK 只对 `events_api` 发且**先于**投递、坏帧不致命、流结束 ⇒ 链路错误）+ `disconnect_frame_asks_for_a_reconnect` |
| 凭据面（§2.3 四条判据） | `slack::config::tests::debug_never_echoes_token_material`、`fail_closed_decrypter_refuses_instead_of_passing_ciphertext_through`、`slack::socket::tests::channel_debug_redacts_tokens`、`slack::media::tests::blocked_host_never_reaches_the_network`；承载凭据的类型全部**手写 `Debug`**（`Credentials` / `InstallConfig` / `Decrypter` / `SlackDeps` / `Sensitive` / `SlackChannel`），错误变体只带长度 / 字段名 / 来源 |

`cargo test -p mc-channel --lib` = **162 passed / 0 failed**（其中 `slack::` 新增 52 条）。

### 13.4 门禁读数（逐字取自当轮日志）

**两轮跑完（同一棵树；第二轮只重跑被磁盘打断的门）**：

```
第一轮（`bash scripts/gates.sh`，无 DB）：①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0
                                          · ⑦route-parity 0 · ⑨conformance 0 · ⑩file-size 0 ⇒ **8/8 PASS / 307s**
第二轮（`bash scripts/gates.sh --with-db`）：①–⑤ 0 · ⑥db 0（migrate=0, e2e=0）· ⑧schema-drift 0
                                          · ⑦ 0 · ⑨ 0 · ⑩ 0 ⇒ **10/10 PASS / 175s**
```

- **【lesson·门 ⑥ 的「红」又是磁盘满，不是代码错】** 第二次 `--with-db` 起初报
  `migrate=0,e2e=101`；单跑那条 e2e 命令看到的是 `ld terminated with signal 7 [Bus error]` +
  `could not write output … No space left on device`（`df -h /` = 100%、本 run 的 `target` **19G**、
  其中 `target/debug/incremental` **7.7G**）。唯一回收动作 = `rm -rf target/debug/incremental`
  （**本 run 自己的** target）⇒ 腾出 7.4G 后 10/10 一次全绿。同项目的另一在飞片（`lum-1799`，15G）
  **未动**（判据：当时无任何进程的 `/proc/*/cwd` 落在它的 workdir，但"run 终态 ∧ 交付在远端 ∧
  `git status` 空"三条没做 ⇒ 不整删别人的 target）。
- ⑦（**本片 0 路由 ⇒ 读数逐字不变**；`baseline 406 不动`）：`upstream 456 (commit f41fae6b08fb) |
  local 411 registered | baseline 406`、`implemented 331 real + 4 placeholder = 335 / 456`、
  `known_gap 121`、`unclaimed 0`、`regression 0`、`local_only 9`、
  `gaps by owner: M9=33 M7=24 M8=19 M3+=16 M2-A=13 M3=11 M10=5`（和 = 121 ✓）
  —— 与 issue「起手补充（11:00 cycle）」的当轮实测**逐字相同**（`owners.M7` 仍 24：本片不认领路由）。
- ⑦ 第二条（形态门）：`slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv` =
  `declared 24 upstream key(s); dual-form required: 0 | single-form: 24` ⇒ **0 defect(s)**、exit 0；
  本地实况同命令 `registered upstream-key literals: 415` ⇒ **0 defect(s)**、exit 0
  （**未**加任何 allowlist 行，M7 无形态欠账）。
- ⑨（与当轮 `report.json` 逐字比对）：`report matches crates/mc-conformance/report.json`，exit 0
  （本片 0 路由 ⇒ 快照不该变，事实也是没变）。
- ⑩：0 违规；`scripts/file_size_baseline.tsv` / `docs/fixtures/route-parity-baseline.json` /
  `docs/fixtures/slash-alias-allowlist.tsv` **三者都未动**（`--write-baseline` **禁跑**，唯一一次刷新归 M7-21 `LUM-1786`）。

### 13.5 交接（给 M7-4 / 宿主装配点 / M7-21）

1. **M7-4（slack 出站 + 安装与绑定面）**：
   - `Channel::send` 目前失败关闭（M7-3-D6）—— 实现 `slack/{outbound.rs,replier.rs}` 后请把
     `SlackChannel::send` 改成真投递（或在 `outbound.rs` 里给出 sender 并由 `socket.rs` 委托），
     并把 `capabilities()` 的位图与真实能力对齐；
   - `/issue`、`/new`、`/clear` 的**斜杠命令**帧已经**收到并 ACK**（`SocketFrame::SlashCommand`），
     但处理被丢弃（`FrameAction::Ignore`）⇒ 你只需在 `dispatch_frame` 的 `SlashCommand` 分支接上
     处理器，**不必**再动传输 / ACK 顺序（ACK 已是"先于处理"）；
   - `InstallationRow`（`ResolvedInstallation.platform` 上的不透明值）里有 `config`，出站可以直接
     用它解出 bot token（`decode_credentials`），**不用**再查一次库；
   - 会话隔离键的分隔符是 `#`（M7-3-D1），取真实 channel id 用前缀。
2. **宿主装配点（`apps/mc-server/src/channels.rs`，anchor 写集）**：Slack 的接线动作是
   `mc_channel::slack::register_with(&registry, &SlackDeps::with_secret_box(key))`，其中
   `key = ChannelKeys::get(ChannelKind::Slack)`；现在的 `register(&registry, &deps)` 会打一条 warn
   并**拒装配**带密文的安装（失败关闭）。同理，`ChannelDeps` 里还缺 `InstallationStore` /
   `LeaseStore` 的生产实现（归 M7-1/M7-2 的端口实现），在那之前 `start()` 仍是 `deps=None` 的空跑。
3. **M7-21（INT）**：本片**未**动任何基线（`route-parity-baseline.json` / `file_size_baseline.tsv` /
   `slash-alias-allowlist.tsv`）；本片对 ⑦ 的贡献是 **0**（0 路由、0 占位删除）⇒ `--write-baseline`
   仍归你（`docs/60` §6.1 的 M7-21 行）。
