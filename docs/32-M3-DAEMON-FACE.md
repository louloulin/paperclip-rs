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


---

## 14. M8-2（`LUM-1799`）：VCS provider 抽象 + 连接管理 + 入站 webhook（5 路由）的落点、偏离与门禁读数

`docs/61-M8-PLAN.md` §4.1 的 **stage 2 第二片**（`5` 路由 / 上游 `vcs.go` (336) + `vcs_webhook.go` (335)
+ `integrations/vcs/*` (649) = **1,320** 行）。本节是它在 `docs/32` 的**自己那一段**
（`docs/61` §3.3 / §6.5 第 7 条）。号段起手复核：`## 11.` = M8-0 anchor、`## 12.` = M8-1
⇒ 本节**原取** `## 13.`；合并进 base 时 `## 13.` 已被 M7-3（`LUM-1768`，PR #88）占用
（两片同为 `docs/32` 尾节追加 ⇒ cycle `LUM-1851` 按「两侧都保留」解，本片顺延为 `## 14.`）
（计划书写的「§9.12」是计划期占位号）。

**起手 base = `ca05edbd`**（= `5f1a34ec` + 合并 #85 / M8-1）。硬前置两条**当轮重验通过**：
M8-0（#83）与 M7-0 的 `crates/mc-secrets/src/secretbox.rs` 都在 base 里。

### 14.1 落点（写集逐字）

| 文件 | 角色 | 内容 |
| --- | --- | --- |
| `crates/mc-vcs/src/forgejo.rs` | **实现**（M8-2） | `ForgejoProvider{kind}`（**一个结构体两个 kind**：`forgejo` + `gitea`，上游 `init()` 逐字）的 `Provider` 六方法；`X-Gitea-Signature` HMAC-SHA256 验签（容忍 `sha256=` 前缀，空 secret 直接拒）；PR / commit-status 载荷解析（三段 owner 回落、`derivePRState` 的**顺序**、五态→三态）；`GET /api/v1/user` 的 token 校验。文件尾承接上游同 package 的 shared helpers：`normalize_instance_url` / `coalesce` / `decode_payload` |
| `crates/mc-vcs/src/gitlab.rs` | **实现**（M8-2） | `GitLabProvider`：`X-Gitlab-Event` 分类、`X-Gitlab-Token` **明文常量时间**比较、MR 载荷映射（`path_with_namespace` 拆子组、三段 draft 判据、`locked` 读作 open）、pipeline 载荷（合成 context `gitlab/pipeline`）、`GET /api/v4/user`；**手写公历算法**的 `normalize_gitlab_time`（见 13.2 D7） |
| `crates/mc-repos/src/vcs/connection.rs` | **实现**（M8-2） | `VcsConnectionRow`（**手写 `Debug` 脱敏两个密文列**）+ `VcsConnectionRepo`：`list_by_workspace` / `find_by_id`（不收窄 workspace，webhook 的唯一入口）/ `upsert`（`ON CONFLICT (workspace_id, instance_url)` 原地轮换）/ `rotate_webhook_secret`（`WHERE … AND workspace_id`）/ `delete`（**一条带 `target` CTE 的语句**，4 张表无 FK ⇒ 子行显式清） |
| `crates/mc-repos/src/vcs/pull_request.rs` | **实现**（M8-2，M8-4 只读） | `VcsPullRequestRow` 全列投影 + `upsert`（15 列逐列 `CASE WHEN EXCLUDED.pr_updated_at >= …` 守卫）+ `find` + `list_by_issue`（按**当前 head_sha** 聚合 passed/failed/pending）+ 关联账 `link_issue`（`preserve_close_intent` 冻结语义）/ `unlink_issue` / `list_issue_ids_for_head` |
| `crates/mc-repos/src/vcs/commit_status.rs` | **实现**（M8-2） | `VcsCommitStatusRow` + `VcsCommitStatusRepo::upsert`（`WHERE EXCLUDED.updated_at >= 现值` 单调守卫）/ `find` / `list_for_head` |
| `crates/mc-http/src/routes/vcs/dto.rs` | **实现**（M8-2） | `VcsConnectionResponse` / `VcsConnectResponse`（**手写 `Debug`** 脱敏一次性明文）/ `VcsConnectionsResponse`；`webhook_path` / `webhook_url` + `MULTICA_PUBLIC_URL` 接缝；`error_with_code`（本仓嵌套信封 + 上游稳定 code） |
| `crates/mc-http/src/routes/vcs/connections.rs` | **实现**（M8-2） | 4 条 workspace 路由 + `VcsScope`（member/admin 门）+ `provider_registry()`（**本片唯一的 registry 构造点**，webhook 复用）+ `seal_secret` / `open_secret`（`secretbox` + base64）+ `mint_webhook_secret`（32 随机字节 hex）+ `is_absolute_http_url` |
| `crates/mc-http/src/routes/vcs/webhook.rs` | **实现**（M8-2） | 公开路由 `POST /api/webhooks/vcs/:connectionId`：`DefaultBodyLimit(10 MiB)` + **扁平** `{"error":…}` 错误体 + 失败阶梯 404/400/413/500/401 + 两条镜像路径（PR / CI 状态）+ 广播 |
| `crates/mc-http/tests/vcs/{main,support,connections,webhook}.rs` | **新增测试**（M8-2） | 真库 + 离线实例替身的端到端 **17** 例 |
| `docs/32` §14（本节） | **文档**（M8-2） | 落点 / 偏离 / `DoD` 证据 / 门禁读数 |

**只读**（未改一个字节）：`crates/mc-secrets/src/secretbox.rs`（M7-0 建）、
`crates/mc-vcs/src/{provider,events,registry,signature}.rs`、`crates/mc-vcs/Cargo.toml`、
`crates/mc-http/src/{state.rs,state/integrations.rs,routes/{mod,mount}.rs,routes/vcs/mod.rs}`、
`crates/mc-repos/src/vcs/mod.rs`、`Cargo.toml` 根、`Cargo.lock`、`docs/fixtures/**`、`apps/**`。

**零注册编辑**：`routes/vcs/mod.rs`（anchor）已 `merge(connections::router()).merge(webhook::router())`
⇒ 本片只在两个子文件里注册 5 条键，**注册键集合**与上游字面量逐字一致（`dual-form required: 0`）。

### 14.2 偏离登记（M8-2-D1 … M8-2-D11）

| ID | 事项 | 计划书 / anchor 期 | 本片 | 理由 |
| --- | --- | --- | --- | --- |
| **M8-2-D1** | 「未配置」的状态码 | `docs/61` §2.5 的 VCS 行写 **503** | **403 + `vcs_not_configured`**（connect / rotate） | 上游 `ConnectVCS` / `RotateVCSConnectionWebhook` 走 `writeFeatureDisabled(...)`，其实现逐字是 `writeErrorCode(w, http.StatusForbidden, …)`；注释写明「被关掉的能力不是瞬时故障，回 503 会招来重试与告警噪音」。M8-1 的 `repositories` 遇上同一分歧做了同一选择（§12.2 D6） |
| **M8-2-D2** | `error_with_code` 的复用 | §12.5 第 3 条说 `routes/github/install.rs` 的同名函数是 `pub(crate)`，「直接用，不要再复制一份」 | **实测它是私有的**（`fn error_with_code`，无可见性标注）⇒ 本片在 `routes/vcs/dto.rs` 持自己的副本（`routes/agents.rs` 的注释逐字：「各切片各自持有本地副本，与本仓既有约定一致」） | 改 `install.rs` 的可见性 = 动 M8-1 的写集文件；本仓惯例本来就是每片一份 |
| **M8-2-D3** | 公开 webhook 的错误体形状 | 本仓标准是嵌套 `{"error":{"code","message"}}` | **扁平** `{"error": msg}` + 尾随 `\n` | 上游这一族用 `writeError`（`writeJSON` 还补尾随换行，注释逐字 "Match the trailing newline…"），provider 的投递 UI 按扁平体解析。与 `routes/webhooks/autopilots.rs`（M5-5）同一判断 |
| **M8-2-D4** | 超大 body 的语义 | 上游 `io.ReadAll(io.LimitReader(r.Body, 10<<20))` **静默截断** | `DefaultBodyLimit(10 MiB)` ⇒ **413** | `Bytes` 抽取器给不出「截断后的前缀」。上游截断后的 body 验签必然失败 ⇒ 得到 **401**。选 413 的理由：它比「看起来像签名不匹配」更容易被投递 UI 解释清楚；上界常量与上游逐字相等（`10 * 1024 * 1024`） |
| **M8-2-D5** | provider registry 的构造点 | anchor 的 `registry.rs` 是**值**（不是全局可变状态） | `mc_http::routes::vcs::connections::provider_registry()`：每请求构造（3 个零尺寸 `Arc`），`webhook.rs` 复用同一函数 | anchor 逐字要求「不用进程内全局 map（单测互相串扰）」。单一构造点避免两处各注一份而漂移 |
| **M8-2-D6** | `normalize_instance_url` / `coalesce` / `decode_payload` 的位置 | —— | 落在 `forgejo.rs`（**上游同 package 的 shared helpers 段所在文件**） | `provider.rs` / `signature.rs` / `registry.rs` 被 anchor 冻结；上游这两个 helper 就写在 `forgejo.go` 的文件尾。`gitlab.rs` 与路由层都引用同一份 |
| **M8-2-D7** | GitLab 时间戳归一化 | 上游 `normalizeGitLabTime` 用 Go `time` 的 layout 表 | **手写公历算法**（`days_from_civil` / `civil_from_days`，Howard Hinnant 公开算法）等价于 `time.Parse(...).UTC().Format(time.RFC3339Nano)` | `mc-vcs/Cargo.toml` 被 anchor 冻结（注释逐字「此后 M8-2 的写者不得再新增三方依赖」）而 `mc-vcs` **没有 `chrono`**；把方言泄进共享解析层则违反 `events.rs` 的「RFC3339 或空串」契约。与上游的两点差异：命名时区只认 `UTC`/`GMT`（GitLab 只发这两种或数字偏移）；偏移范围校验 `±23:59`（与 `time.Parse` 的拒绝一致） |
| **M8-2-D8** | 载荷解码的 `serde` 口径 | —— | **容器级** `#[serde(default)]` + `decode_payload` 的**对象形状前置** | Go 的 `json.Unmarshal` 对**缺字段**不报错（serde 默认要求字段存在 ⇒ 不加会**收窄**）；但容器级 default 有个副作用：serde 派生的 `visit_seq` 不再要求元素个数 ⇒ `[]` 会被解成零值，**而 Go 对数组进结构体是报错的** ⇒ 显式拦回。`null` 则手动折成 `Default`（Go 对 `null` 是 no-op） |
| **M8-2-D9** | 本片的范围：**只做镜像** | `docs/61` §1.6 的两列对照写「VCS：PR 镜像 + CI 状态镜像，**无**自动关联/关闭」 | webhook **只** upsert PR / CI 状态 + 广播；关联账的**写入原语与读面**仍按 anchor 的写者表交付（`link_issue` / `unlink_issue` / `list_by_issue` / `list_issue_ids_for_head`），由 M8-4 调用 | 自动关联依赖 `extractIdentifiers` / `lookupIssueByIdentifier` / `advanceIssueToDone`（上游住在 `github.go` L964–L1997 = **M8-4** 的写集）；在 M8-2 里再写一份标识符抽取就是两个写者各持一份策略。⇒ 本波结束后 `issue_vcs_pull_request` 在没有其它写者之前为空，**这是登记过的缺口，不是遗漏** |
| **M8-2-D10** | `GetIssueCombinedPullRequestCloseAggregate` | `pkg/db/queries/vcs.sql` 里的一条 | **不交付** | 它服务的是「自动推进 issue 到 done」的决策（= 关闭策略，M8-4 的 `closepolicy.rs`），且同时读 `github_pull_request` / `issue_pull_request` 两张 GitHub 表 ⇒ 落点应由那个写者决定。与 D9 同一条边界 |
| **M8-2-D11** | `rotate_webhook_secret` 无匹配行 | 上游 `:one` 拿不到行 ⇒ handler 报 **500** | 本仓回 **404**（跨 workspace 的行与不存在同判） | 路由层已经在同一 workspace 内确认过连接存在 ⇒ 0 行只可能是竞态/越权，404 比「服务器内部错误」更诚实。上游那 500 是 sqlc `:one` 的副产物，不是刻意契约 |

**另记两条文案债**（与 §12.2 D12 同性质，归 M8-7 收口）：
① `crates/mc-repos/src/vcs/mod.rs` 与三个子文件的模块头还写着「M8-2 待落地 / doc-only 桩」；
② `crates/mc-http/src/routes/vcs/mod.rs` 的「anchor 期：三个子文件全是空 `Router::new()`」已过期。
两者都在 anchor 冻结文件里，本片**不改**。

### 14.3 专属 `DoD` 的证据（`docs/61` §6.5 的 M8-2 行逐条）

| `DoD` 条目 | 证据（用例） |
| --- | --- |
| `Provider` trait 的**三个检验**（forgejo + gitlab + 未注册 provider 报可区分错误） | `forgejo.rs::register_populates_both_forgejo_and_gitea`（两个 kind 一份 wire 行为：同一 body 的解析结果逐字相等）、`gitlab.rs::register_makes_gitlab_resolvable_and_unknown_kind_errors`（`RegistryError::UnknownProvider(Forgejo)` + 错误文案 `vcs: no provider registered for kind \`forgejo\``，**不是 panic、不是静默 None**）、`connections.rs::connect_rejects_bad_requests_and_maps_outbound_failures`（未注册 / 未认识的 provider ⇒ 400 `unsupported provider`） |
| **三种签名方案各 1 正例 + 1 反例** | Forgejo/Gitea HMAC：`forgejo.rs::hmac_signature_accepts_correct_and_rejects_tampered`（正例裸 hex + `sha256=` 前缀；反例：差 1 位 / body 差 1 字节 / 换密钥 / 缺头 / 空 secret / 非法 hex）+ **真库 e2e** `tests/vcs/webhook.rs::forgejo_hmac_accepts_signed_frame_and_rejects_tampered_one`（三次反例之后仍然**只有 1 行**）；GitLab 明文：`gitlab.rs::plaintext_token_accepts_correct_and_rejects_others`（差 1 位 / 长一截 / 空 / 大小写）+ e2e `gitlab_plaintext_token_accepts_matching_and_rejects_others`。常量时间比较的共用原语在 anchor 的 `signature.rs`（本片只读） |
| `rotate-webhook`：旧 secret **立刻失效** / 新 secret **立刻生效** / 明文**只此一次** | `tests/vcs/connections.rs::rotate_returns_one_time_secret_and_nothing_else_does`（rotate 后库里解出来的就是**新** secret、`≠` 旧值；rotate 之后再读列表，**两个** secret 都不出现在响应里；跨 workspace 的连接 id ⇒ 404） |
| per-connection secret 与 PAT 落库 = `secretbox` **密文**（明文入库即失败）+ `Debug` 脱敏 | e2e `connect_forgejo_persists_ciphertext_only`：响应里没有 PAT（只有一次性 `webhook_secret`）、没有 `encrypted` 字样；库里两列 `≠` 明文且**能解回原值**；`connection.rs::debug_redacts_encrypted_columns`（`VcsConnectionRow` 的手写 `Debug`）+ `dto.rs::connection_response_never_carries_credential_columns`（响应 JSON 里一个字节的密文都不许出现）+ `dto.rs::connect_response_debug_redacts_the_one_time_secret` |
| 5 条路由的「**产品边界** vs **未配置**」两层语义逐端点 | `product_boundary_off_matrix`（GET 200 + 四格恒定 / connect 404 / rotate 404 / **DELETE 不看边界** 204）、`boundary_on_without_key_matrix`（GET 200 + `configured:false` / connect + rotate 403 `vcs_not_configured` / **一行都不落库**）、`authorization_matrix_per_endpoint`（member+guest 403 ×3 / outsider 404 / 无会话 401 / 非法 ws 400 / 非法 connection id 400 ×2 / member 列表 `can_manage:false` vs admin `true`）、`webhook.rs::missing_surface_and_unknown_connection_are_404`（边界关 / 密钥缺 / 连接不存在三条路径都是 **404 `unknown connection`**）、`undecryptable_connection_secret_is_500_and_writes_nothing` |
| **离线替身端到端**（每连接自带 `instance_url` 是天然接缝） | `connect_forgejo_persists_ciphertext_only`（路由 → 真 HTTP `/api/v1/user`（替身）→ `secretbox` 封装 → 真库 → 响应 + `webhook_url` 派生）与 `connect_gitlab_uses_api_v4`（`/api/v4/user`）；**入站**面用真实 wire 帧（真 HMAC / 真 `X-Gitlab-Token` 头 + 真 JSON），**不需要**替身。替身三条纪律：只替 platform wire（axum 起的假实例，不是假 service）、字段逐项断言、两个反例必测（验签失败不落库 / 重复投递只留一行） |
| 幂等与单调（`docs/61` §4.2 纪律 ③） | `pull_request_mirror_is_idempotent_and_monotonic`（同帧两次 1 行；陈旧帧不得回退 title/state/head_sha；`merged=true` 归一化）、`commit_status_mirror_is_context_keyed_and_monotonic`（context 是主键的一部分；陈旧重投递被守卫挡住；缺 sha/state 确认但忽略）、`gitlab_pipeline_uses_synthetic_context`、`unmodelled_event_is_acknowledged_without_writing`（202 且不落任何行；**仍然要验签**） |

### 14.4 门禁读数（逐字取自当轮日志；日志留档在 run workdir 的 `gates-m8-2.log` / `gates-clippy.log`）

```
bash scripts/gates.sh --with-db --db-url 'postgres://mc_lum1799:…@127.0.0.1:5432/mc_lum1799'
  ①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0 · ⑥db 0（migrate=0, e2e=0）
  ⑧schema-drift 0 · ⑦route-parity 0 · ⑨conformance 0 · ⑩file-size 0
  ⇒ overall: PASS — 10/10 gate(s) green in 346s
```

- ⑦（**本片会动读数**；`baseline 406` **不动**）：`upstream 456 (commit f41fae6b08fb) | local 416 registered`、
  `implemented 336 real + 4 placeholder = 340 / 456`、`known_gap 116`、`unclaimed 0`、`regression 0`、
  `local_only 9`、`gaps by owner: M9=33 M7=24 **M8=14** M3+=16 M2-A=13 M3=11 M10=5`（和 = 116 ✓）。
  **与 issue 描述「起手补充（11:00 cycle）」的片后预期逐字相同**：`local 411→416`、`implemented 335→340`、
  `known_gap 121→116`、`owners.M8 19→14`（−5 = 本片 5 条路由）；`--write-baseline` **未跑**（归 M8-7）。
- ⑦ 第二条（形态）：`slash_alias_audit.py --quiet` = **exit 0**（5 条均为上游 plain 注册，
  `dual-form required: 0`；M8 没有 allowlist 退路 ⇒ 三类缺陷都是硬失败）。
- ⑩：**0 违规**，`scripts/file_size_baseline.tsv` **未动**。本片最大文件在 `cargo fmt --all` **之后**量：
  `crates/mc-vcs/src/gitlab.rs` **788** 行（⚠️ 硬限 800，余量只有 12 行 —— 它承接了手写公历算法 +
  时间戳测试；后续要改它请先看这条）、`tests/vcs/connections.rs` 763、`mc-repos/vcs/pull_request.rs` 776、
  `forgejo.rs` 656、其余 ≤ 549。
- ⑨：`report matches crates/mc-conformance/report.json`（本片 0 fixture 改动 ⇒ 未漂移；M8 面的那条
  composio fixture 仍归 M8-6）。
- ⑧：schema-drift 绿（本片 **0 迁移**、0 表改动 ⇒ 与基线同形）。
- ⑥：`mc-migrate` 566 个迁移 + `--ignored` 全绿（含本片 **19** 例真库用例：`mc-repos::vcs` 2 例 db +
  `mc-http --test vcs` 17 例）。
- ⑤（`cargo test --workspace`，**不带**库变量）：全绿；本片在其中新增 **34** 例**零 DB** 用例
  （`mc-vcs` 单元 19：`forgejo.rs` 8 + `gitlab.rs` 11；`mc-repos::vcs` 单元 5；
  `mc-http` lib 的 `routes::vcs` 10）⇒ 连同真库 19 例，本片共 **53** 例新测试。
- **【lesson·磁盘满的两种表现】** 本片起手 `df` 16G 可用；一次 `--all-targets` 冷建 + 一次 17 例 e2e
  链接就打满到 **358MB**，报错形态是 `could not create incremental compilation session directory …:
  No space left on device`（**编译期**）与 `error: could not create directory …/.fingerprint`
  （**门 ⑨**）。同项目另一片（`lum-1767` / 其合并树复验 run `LUM-1847`）当时正在跑
  `CARGO_INCREMENTAL=0 gates.sh --with-db`，`target` 23G 且**有活进程**（`/proc/*/cwd` 逐 PID 命中）
  ⇒ 未动它。**本片自己的处置**：`rm -rf target/debug/incremental`（3.0G）→ 仍不够 →
  `cargo clean` + **`CARGO_INCREMENTAL=0` 全量重建**（target 5.3G，比之前的 11G 省一半；
  与本项目 cycle 的 `LUM-1847` 同一手法）。此后所有 cargo 调用都带 `CARGO_INCREMENTAL=0`
  （env 覆盖 `.cargo/config.toml` 的 `incremental = true`），门禁读数不受影响（同一脚本、同一批命令）。

### 14.5 交接（给 M8-3 / M8-4 / M8-6 / M8-7）

1. **M8-4 读这三处**：`mc_repos::vcs::pull_request`（`link_issue` 的 `preserve_close_intent` 冻结语义、
  `unlink_issue` 的「终态后不得调用」纪律、`list_by_issue` 的聚合、`list_issue_ids_for_head` 的扇出
  **都已就位并有真库用例**）、`mc_vcs::events::{PullRequestEvent,CIStatusEvent}`（归一化后的唯一形状；
  终态判据用 `PullRequestEvent::is_terminal()`，**不要**在 handler 里重写 action 集合）、
  `mc_repos::vcs::commit_status`（单调守卫的语义与「必须喂事件时间戳」这条前提）。
  你的两条路由（`POST /api/webhooks/github` / `GET /api/issues/:id/pull-requests`）需要
  跨 GitHub+VCS 的关闭聚合时，**新增**查询的落点请选 `mc-repos/src/github/pull_request.rs`
  或你自己的新文件（见 §14.2 D10 —— 本片刻意没写那条跨表 CTE）。
2. **M8-3 / M8-6**：本片**没有**碰 `routes/{mcp,composio}/**`、`mc-composio/**`、`state*.rs`、
  `routes/{mod,mount}.rs`、manifest ⇒ 与本片写集**零交集**。公开 webhook 的**扁平**错误体与
  `DefaultBodyLimit` 手法（`webhook.rs`）可直接照抄（composio 的 callback 是同类公开路由）。
3. **M8-7（INT）**：本片把 ⑦ 推成 `local 416 / implemented 340 / known_gap 116 / owners.M8 14`；
  下一次刷新（`--write-baseline`：`baseline 406 → ？`）归 M8-7。请一并收口两条文案债（§14.2 末）与
  **D9/D10 两条「登记过的不实现」**（VCS 侧自动关联/关闭、跨表关闭聚合），并在 INT 报告里复述
  「`issue_vcs_pull_request` 在本波结束时仍为空」这条已知缺口。

## 15. M7-4（`LUM-1769`）：slack 出站/回复/命令历史 + 安装与绑定面（4 路由）

**口径**：本节一切落点与偏离都在上游 `f41fae6b08fb` 与本片当轮 base 上核对。本片**起手 base**
是 `b21cf928`（= 合并 #88（M7-3）+ #87（M8-2）之后），提交后 `origin/feat/multica-rs-initial`
已前进到 **`43361e92`**（= `d1bd3db0`（#89 / M8-3）+ §92/§93 docs）⇒ 本片已 rebase 到它，
§15.3 的两个读数（本片起手 tree 与 rebase 后 tree）都给。

### 15.1 落点（逐字路径）

| 落点 | 内容 |
| --- | --- |
| `crates/mc-channel/src/slack/outbound.rs` | `chat.postMessage` 的 wire 形状、`MessageApi` 端口、`Sender`（mrkdwn → 分片 → 线程化 → 元数据）、进程内 API 基址接缝 |
| `crates/mc-channel/src/slack/replier.rs` | 判决 → 文案（7 个 `Outcome`）、绑定卡、issue 标题消毒、`BindingMinter` / `OutboundLedger` 端口 |
| `crates/mc-channel/src/slack/typing.rs` | 👀 反应的生命周期（加/摘 + 2 分钟年龄闸门）、`ReactionApi` / `InstallationConfigs` 端口 |
| `crates/mc-channel/src/slack/history.rs` + `slack/history/{reader,text,flatten,tests}.rs` | 频道目录 / 单线程两条读面、四道过滤、正文摊平与人名标签 |
| `crates/mc-channel/src/slack/slash.rs` | `/issue`（快速创建）+ `/new` / `/clear`（DM 会话控制）、`ControlStarter` 的 engine 端口实现 |
| `crates/mc-channel/src/slack/install.rs` | BYO 安装的三步校验、`InstallStore` / `InstallApi` 端口、list/get/revoke |
| `crates/mc-channel/src/slack/binding.rs` | 绑定令牌的铸造与原子兑换（`BindingStore` 端口） |
| `crates/mc-channel/src/slack/{mod.rs,socket.rs}` | 7 个新模块的可见性、`register_resolvers` 装配入口；`socket.rs` 的 `send` 由失败关闭改为委托 `Sender` |
| `crates/mc-http/src/routes/channels/slack.rs` + `slack/{store.rs,tests.rs}` | 4 条路由、鉴权层、未配置语义、wire DTO；三条上游查询的 PG 端口实现 |
| `crates/mc-http/tests/channels/{main.rs,support.rs,slack.rs}` | 4 条路由的真库端到端（门 ⑥，`#[ignore]`） |

### 15.2 偏离（D1…D12，逐条可核对）

- **D1 写集勘误（新增路径）**：`docs/60` §3.3 给本片的格子是 7 个 `.rs` + 1 个路由文件。
  起手补充已追加 `slack/mod.rs` 与条件性的 `slack/socket.rs`。本片**再追加**的都是**门 ⑩
  的 800 行硬限**逼出来的切分（与 M7-3 对 `{inbound,media,resolvers,socket}/tests.rs` 同款，
  不是拆凑数字）：7 个 `slack/*/tests.rs`、`slack/tests.rs` + `slack/tests/support.rs`、
  `slack/history/{reader,text,flatten}.rs`（上游 `history.go` 一个文件 737 行，移植后 1,374 行）、
  `routes/channels/slack/{store.rs,tests.rs}`、`crates/mc-http/tests/channels/**`。
  **`slack/socket/tests.rs` 改了一条断言**（M7-3 的 `send_is_fail_closed_until_m7_4` 断言的
  是错误文案里的 `"M7-4"`；接线点落地后文案不再指向未来 ⇒ 改成断言"未注入发送器时失败关闭"）。
- **D2 三条上游查询没有泛化仓储**：`ListChannelInstallationsByWorkspace`（含 revoked）、
  `UpsertChannelInstallation` + 死主回收 + 唯一冲突分类、`ConsumeChannelBindingToken` +
  成员闸门 + 建绑定（同事务）在 `mc-repos` 里不存在，而本片写集**不含**
  `crates/mc-repos/src/channel/**` ⇒ 它们以**端口实现**（`PgInstallStore` / `PgBindingStore`）
  的形态落在 `routes/channels/slack/store.rs`。**语义逐条照上游**（事务边界、冲突三分类、
  `ON CONFLICT … WHERE multica_user_id = EXCLUDED…` 的拒绝语义、成员不通过即回滚不烧令牌），
  而"adapter 不得直接写 DB"这条边界仍然是类型层面的事实（`mc-channel` 只拿到 trait）。
- **D3 同步接缝 + 脱离任务（优于上游）**：engine 的 `OutboundReplier::reply` /
  `TypingNotifier::{on_ingested,on_settled}` 是**同步**方法（调用点在 `tokio::spawn` 里），
  上游在这几处**阻塞着**发 HTTP。本仓的同步方法只推一个脱离任务，真正的工作在 async 的
  `reply_now` / `add` / `clear` 里 ⇒ 引擎调用点绝不阻塞在 Slack HTTP 上，且用例能直接
  `await` 完整路径（不必 sleep 等后台任务）。**语义等价**（上游 `Add` 的顺序：
  年龄闸门 → 解密 → 调用 → 记状态；本片逐条照搬，包括那条"先调用后记状态"的既有竞态）。
- **D4 令牌不进结构体字段**：上游的 `slackSender` 持一个绑好令牌的 `*slack.Client`；
  本仓的 `Sender` 是**无状态**的，令牌作为形参传入 ⇒ 同一个 sender 服务多个安装，
  且"令牌不进字段"在类型层面成立（凭据纪律第 1 条）。
- **D5 绑定令牌的随机源**：上游 `crypto/rand` 读 32 字节；本 crate 的依赖集里没有 `rand`
  （`docs/60` §3.1 冻结）⇒ 取两个 v4 UUID 拼成 32 字节（244 bit 熵，与"15 分钟单次令牌"
  同量级，base64url **线的长度也相同**）。**存储哈希与 TTL 逐字不变**（`sha256(raw)` 的 hex、
  15 分钟、库侧 `CHECK` 仍钉着上限）。
- **D6 history 读面的三处形态差异**：(a) 上游的 `ListChannelOutboundMessageIDsForContext`
  依赖 `channel_outbound_message.channel_context_revision` 列，**本仓该列不存在** ⇒
  `allowed_bot_messages` 用「按 `(binding_id, route_revision)` 列出的出站行」近似 ——
  粒度是**路由代际**而不是上下文代际，方向是**多**放行同代际内早前轮次的本 bot 消息
  （不会少放行，即**不会**把该看见的藏起来）；(b) Block Kit 的摊平走通用 JSON 遍历
  （按 `type` 分派：`section`/`header`/`context`/`markdown`/`rich_text`），语义逐字照搬、
  类型树不照搬；(c) 页面类型（`HistoryPage`/`HistoryMessage`/`HistoryOptions`）定义在
  adapter 内 —— 消费方是 `multica chat history` / `chat thread`，**不在 M7 写集内**
  ⇒ 与 R-M7-6（读侧有一条属 M4）同类，**登记为缺口**，不是"漏实现"。
- **D7 `/issue` 的答复走 `response_url`**（上游同）：`EphemeralResponder` 的默认实现 POST
  `{"response_type":"ephemeral", …}`；`response_url` 自带签名票据 ⇒ 与
  `apps.connections.open` 的 `wss://` 同一条纪律（不进日志、不进 `Debug`、错误文案不回显）。
- **D8 出站记账的**边界**：绑定卡这条路径（`NeedsBinding`）**不**写 `channel_outbound_message`
  —— 那一刻还没有会话绑定行，没有"这条出站属于谁"可记。上游 `postResult` 逐字同
  （`if r.ledger == nil || !res.ChannelBindingID.Valid { return nil }`）。有绑定的两条
  （`control_ack` / `issue_ack`）照记，且历史读面正是按它过滤控制回执。
- **D9 解析器面的装配悬空**（登记缺口，**不**擅自扩写集）：宿主装配点是
  `apps/mc-server/src/channels.rs`（**anchor 写集**）。本片不碰它，改为提供**一个**入口
  `slack::SlackResolverWiring` + `slack::register_resolvers(router, wiring)`（出站回复器 +
  打字指示 + M7-3 的五个必填端口一次接好），由 INT / 后续锚点调一次。**工厂侧已闭环**：
  `socket::factory` 交给每个 channel 的 `send` 已接上真 `Sender::http()`，
  所以"出站面是否可用"不再有第二个答案。
- **D10 `MULTICA_APP_URL`（回落 `FRONTEND_ORIGIN`）的读取口在路由文件**
  （`routes::channels::slack::app_url`，`pub`）。理由同 D9：`mc-channel` 不得自己
  `std::env::var`，而唯一的 env 读取面是 `mc-http`。宿主装配出站回复器时要把**同一个值**
  交给 `SlackOutboundReplier`（上游注释逐字：`MULTICA_PUBLIC_URL` 是 API 主机，**不是**绑页主机）。
- **D11 平台替身的两个注入点**：(a) `mc_channel::slack::outbound::{set_api_base,reset_api_base}`
  （进程全局，与 M8-1 的 `set_github_api_base` 同款）；(b) 入站侧的 `SocketTransport` 端口
  （M7-3 已留）。本片的端到端回路**不用** (b)：它跑真 `tokio-tungstenite` 客户端对**本地 WS
  服务端**（"只替平台 wire，不替业务路径"，`docs/60` §4.2）。
- **D12 400 文案带上 Slack 自己的错误码**：上游用一串通用文案（"could not verify the Slack
  tokens — …"）+ 日志里的 `err.Error()`；本仓的 `InstallError::Api{step,code}` 把
  `auth.test` / `bots.info` / `apps.connections.open` 的**具体**原因（`invalid_auth` 一类）
  带进结构化错误码，供前端指路。**只带 Slack 的错误码**，不带令牌、不带 URL。

### 15.3 门禁读数（本片当轮实测）

```
$ bash scripts/gates.sh --with-db
①fmt ②build ③clippy ④clippy-test-util ⑤test ⑥db ⑦route-parity ⑧schema-drift ⑨conformance ⑩file-size
```

门 ⑦（`route_parity.py`）在**两棵树上各测一次**（本片的 `+4` 与预测逐字一致）：

```
# ① 本片起手 tree（base b21cf928 `+` 本片）：与 §6.1 / 派发描述的预测逐字相符
upstream 456 (commit f41fae6b08fb) | local 420 registered | baseline 406
  implemented  340 real +   4 placeholder =  344 / 456   known_gap  112   unclaimed    0   regression   0   local_only    9
  gaps by owner: M9=33  M7=20  M3+=16  M8=14  M2-A=13  M3=11  M10=5

# ② rebase 到当轮 base 43361e92（`d1bd3db0` #89 / M8-3 `+` §92/§93）之后的 tree
#    （同一个命令，同两棵树之差 = 本片的 4 条）
base 43361e92 : local 424 | implemented 348 | known_gap 108 | owners.M7 24 | baseline 406
本片 rebase 后: local 428 | implemented 352 | known_gap 104 | owners.M7 20 | baseline 406
  gaps by owner: M9=33  M7=20  M3+=16  M2-A=13  M3=11  M8=6  M10=5
```

⇒ 两个 tree 上**本片都是 `+4`**（四条路由：`GET`/`DELETE …/slack/installations[/…]`、
`POST …/slack/install/byo`、`POST /api/slack/binding/redeem`），且不变式
`implemented + known_gap == 456`、`regressions == 0`、`unclaimed == 0`、`local_only == 9`
全部成立；**`scripts/file_size_baseline.tsv` 未动**。门 ⑦ 的第二条（形态）实测 `0 defect(s)`、
exit 0（M7 没有 allowlist 退路）。

### 15.4 交接（给 M7-5 / M7-9 / M7-14 / M7-15 / M7-21）

1. **四片可照抄的四件事**（同构面）：`outbound.rs` 的「wire 形状 + `MessageApi` 端口 + 基址接缝」、
   `binding.rs` 的「铸哈希令牌 + 原子兑换 + 三个不透明失败」、`install.rs` 的「BYO 三步校验 +
   `InstallStore::persist` 的冲突三分类」、`routes/channels/<platform>.rs` 的「鉴权层 →
   **逐端点**未配置语义 → wire DTO」四段结构。**未配置语义按各平台的 ⑨ fixture 逐条复核**
   （slack 这份是：列表 200 空 + 两个 `false`，其余三条 403 `slack_not_configured`）。
2. **绑定兑换的幂等判据是 DB 的 `consumed_at` CAS**（`UPDATE … WHERE consumed_at IS NULL`），
   **不是**应用层读改 —— 两片都不要在应用层再加一把锁（那会与这条 CAS 语义重复）。
3. **M7-21（INT）**：本片把 ⑦ 推成 `owners.M7 24 → 20`（在当轮 base `43361e92` 上是
   `local 424 → 428 / implemented 348 → 352 / known_gap 108 → 104`）；
   下一次 `--write-baseline`（`baseline 406 → ？`）归 M7-21。请一并收口本片登记的三条缺口：
   **D6(c)**（history 的消费方 `chat history` 命令面）、**D9**（解析器面的宿主装配一次调用）、
   **D10**（`MULTICA_APP_URL` 交给回复器），并在 INT 报告里复述 D6(a) 那条
   `channel_context_revision` 列缺失（**本波结束时仍在**）。
4. **本片与 M7-5/9/14/15 的写集零交集**：只碰 `crates/mc-channel/src/slack/**`（本平台目录）、
   `crates/mc-http/src/routes/channels/slack*`（本平台文件 + 同名前缀的子模块目录）、
   `crates/mc-http/tests/channels/**`、`docs/32`。根 `Cargo.toml` / `Cargo.lock` **未动**
   （零新依赖、零新成员）。

### 15.5 lesson（本片新增）

- **【lesson·门 ⑩ 与"用例内联"是互斥的】** 本仓的写法是「用例写在被测文件末尾的
  `#[cfg(test)] mod tests`」，但一个 400 行的实现 + 400 行用例正好把 800 行用光。
  可复用的切法是 **`<file>.rs` + `<file>/tests.rs` 子目录**（`mod tests;` 会解析到它），
  它比"把两个模块塞进一个文件"更省事，也让"装置 / 断言"这条线显式化
  （本片的 `slack/tests.rs` 最终拆成 `tests.rs` + `tests/support.rs`）。
- **【lesson·`f64 → i64/u32` 是 pedantic 下的两类硬失败】** `as i64` 触发
  `cast_possible_truncation`、`as u32` 再触发 `cast_possible_sign_loss`。Slack 的
  `ts` 是 `"<秒>.<微秒>"`：**只取整数秒部分**（字符串切分 + `parse::<i64>`）就同时避开两类，
  代价只是亚秒精度（对"2 分钟"这类阈值无影响）。同理 `usize → i64` 用 `i64::try_from`。
- **【lesson·"替换别人文件里的一条断言"要明说】** D1 里那条
  `socket/tests.rs` 的断言改动是本片唯一一处**改 M7-3 的用例**：它的断言把
  "未接线"绑在了错误文案的 `"M7-4"` 上 —— 那种断言在**交付的那一刻**必然失效。
  写"指向未来"的错误文案时，用例应断言**语义**（"失败关闭"），而不是字面量里的票号。

## 16. M8-3（`LUM-1800`）：MCP 服务器库 + agent 绑定 + per-task overlay 纯函数（8 路由）的落点、偏离与门禁读数

> **号段说明**：计划书写的「`docs/32` §9.12」是计划期占位号（`## 9.` 是 M6-0 anchor、
> `## 11.` 是 M8-0）⇒ 按派发时的约定取 **`§16`**。`§15` **刻意留空**给并发片 M7-4
> （`LUM-1769`，slack 出站）—— 本片**不回填**、也不占用它的号。

### 16.1 落点（写集逐字）

| 文件 | 行数 | 内容 |
| --- | --: | --- |
| `crates/mc-core/src/mcp/overlay.rs` | 638 | anchor 建桩、本片原地填充：`merge_task_overlay`（上游 `mcp_overlay.go` 160 行）+ `resolve_agent_mcp_config` + `WorkspaceMcpBinding`（上游 `workspace_mcp.go` 的折叠段，含两个容器 `mcpServers` / `mcp` 的规范化）+ 25 条纯函数用例 |
| `crates/mc-repos/src/mcp/workspace_server.rs` | 487 | `workspace_mcp_server` 的 CRUD（4 条 ORM 语句 + 2 条锁语句）+ 条目名/形状校验 + 6 条单测 |
| `crates/mc-repos/src/mcp/agent_binding.rs` | 307 | `agent_mcp_server` 的绑定 / 开关 / 摘除 + claim 路径的 `list_enabled_for_agent` + 2 条单测 |
| `crates/mc-http/src/routes/mcp/workspace.rs` | 471 | 库面 **4** 条路由 + `McpServerResponse`（write-only DTO）+ `mcp_transport_of` + 5 条单测 |
| `crates/mc-http/src/routes/mcp/agent.rs` | 323 | agent 面 **4** 条路由（响应恒为更新后的绑定列表）+ 4 条单测 |
| `crates/mc-http/tests/mcp/{main,support,workspace,agent}.rs` | 26 / 400 / 405 / 503 | **新测试目标**：11 条真库 e2e（含 1 条并发栅栏） |

**零编辑（anchor 冻结，逐条实测未动）**：`crates/mc-core/src/mcp.rs`（`pub mod overlay;` 与
`merge_task_overlay` 的 re-export 都已在 anchor 落好 ⇒ 本片新增的两个符号走
`mc_core::mcp::overlay::…` 路径，**不需要也不得**改这个文件）、`routes/mcp/mod.rs`、
`routes/{mod.rs,mount.rs}`、`state.rs`、根 `Cargo.toml` / `Cargo.lock`、
`docs/fixtures/**`、`scripts/file_size_baseline.tsv`；**只读面**零触碰：
`crates/mc-mcp/**`、`crates/mc-daemon/src/mcp/**`、`crates/mc-repos/src/plugin/**`、
`crates/mc-http/src/routes/plugins/**`、`crates/mc-repos/src/task/**`。

**写集追加一个目录**：`crates/mc-http/tests/mcp/**`（DoD 要求「真库 CRUD + 校验反例」证据；
与 M8-1 的 `tests/github/`、M8-2 的 `tests/vcs/` 同款 —— 新路径、与任何片零交集）。

### 16.2 偏离登记（M8-3-D1 … M8-3-D10）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **M8-3-D1** | 「拒 agent actor」的两个分支**不可实现** | `routes/mcp/{workspace,agent}.rs` | 上游 `requireWorkspaceMcpWriter` / `requireAgentMcpWriter` 先判 `resolveActor(...) == "agent"` ⇒ 403（`agents cannot modify the workspace MCP servers` / `…assignments`）。本仓 mc-http 只有 `AuthUser`（会话头 / `X-Multica-User-Id`，**恒人类成员**）⇒ 没有 agent 身份的请求上下文。与 M2-E `properties.rs` 的同一登记同款。**保留的部分**：库面 owner/admin 门、agent 面 `canViewAgentSecrets` 门逐条在 |
| **M8-3-D2** | `McpOverlayError` 是**三值、不带载荷**的枚举 | `mc-core/src/mcp/overlay.rs` | 上游 `unmarshalServerMap` 会把出错的 server 名写进错误文案（`mcpServers.<name> must be a JSON object`）；anchor 冻结的签名只有三支 ⇒ 四类非法输入（agent 文档非对象 / overlay 非对象 / 容器非对象 / 条目非对象或空名）折进三支。**四种情形的调用侧动作完全相同**（保留 agent 配置 + 一条 warn），丢的只是文案细节。四格对照表写在模块头 |
| **M8-3-D3** | `merge_task_overlay` 的「原值 + error」形态 | 同上 | 上游返回 `(bytes, error)` 双值；本仓的入参是 `&Value`（调用方**始终持有**）⇒ 「失败时回 agent 原值」等价于「返回 `Err`，调用方继续用自己手上那份」。签名由 anchor 冻结，语义未变 |
| **M8-3-D4** | `UPDATE` 的真实库故障回 **500** | `routes/mcp/workspace.rs` | 上游把 `UpdateWorkspaceMcpServer` 的**任何** `:one` 错误都折成 404（sqlc 的副产物）。本仓 409 只留给真·唯一冲突（`23505`）、404 只留给真·未命中，其余 → `Error::Database`（500）。与 M8-2-D11（rotate 的 500 → 404）方向相反但同一条理由：状态码要诚实 |
| **M8-3-D5** | 404 / 400 的**文案** | 两个 route 文件 | 本仓 404 体是 `Error::NotFound` 的渲染（`not found: mcp server`），上游是自由文本（`MCP server not found`）；坏 uuid 是 M3-5 约定的 `<field> must be a valid uuid`，上游 `invalid <field>`。**状态码逐条一致**，只有文案不同（同 `docs/40` §5 的既有偏离） |
| **M8-3-D6** | 时间戳口径 | `McpServerResponse` | 上游 `timestampToString` 是 `time.RFC3339`（**无**小数秒），本仓统一 `to_rfc3339()`（带纳秒）—— 与 M8-2 的 `routes/vcs/dto.rs` 同一口径、同一理由（`mc_repos` 的行类型给的是 `DateTime<Utc>`） |
| **M8-3-D7** | 授权判定的**顺序** | `routes/mcp/agent.rs` | 上游先 `loadAgentForUser`（404）再判 workspace 成员；本仓 `AgentScope::resolve` 先解析 workspace（400 / 非成员 404）再 `load_agent`（404）。状态码组合相同，只是「workspace 不是我的」与「agent 不存在」同时成立时报哪个的优先级不同（M6-4 `/skills*` 同款） |
| **M8-3-D8** | 新符号的**可见路径** | `mc-core/src/mcp/overlay.rs` | `resolve_agent_mcp_config` / `WorkspaceMcpBinding` / `MCP_SERVER_CONTAINERS` 只能从 `mc_core::mcp::overlay::…` 取（`mcp.rs` 是 anchor 冻结文件，只 re-export 了 `merge_task_overlay` + `McpOverlayError`）。不改锚点文件的代价，换来「一个文件一个写者」成立 |
| **M8-3-D9** | **登记缺口**：claim 的 agent 数据块尚未实现 ⇒ `resolve_agent_mcp_config` / `list_enabled_for_agent` 本波**无调用点** | `mc-repos/src/mcp/agent_binding.rs`、`mc-core/src/mcp/overlay.rs` | 上游的消费者是 `daemon.go:2470-2505`（claim 时把绑定折进 `mcp_config`、再叠 overlay）。本仓 `routes/daemon/dto.rs` 的模块头逐字写着「`agent.*` / `repos` / `skills` 等由别的 builder 填（本切片不实现）」⇒ claim 载荷里**没有** `mcp_config` 这个字段。与 R-M8-9（3 处 enqueue 接线）同性质的**登记缺口，不是遗漏**：`runtime_mcp_config` 在本波结束后仍不带托管 MCP。接线点与顺序写在 §16.5 |
| **M8-3-D10** | 同一份数据**两个 transport 口径**（刻意） | `mc-repos/mcp/*.rs` vs `routes/mcp/workspace.rs` | 线格式 = 上游 `mcpTransportOf`：未知 `type` **原样透传**（`{"type":"websocket"}` ⇒ `websocket`），只有没声明 `type` 时才按 `command`/`url` 推断；领域层 = anchor 的 `McpTransport` 三值枚举：未知 `type` 会继续按 `command`/`url` 推断（⇒ `Http`）。**响应只用线格式那个函数**，两个 `transport()` 访问器的文档都逐字写了「不要拿它生成 wire 值」，并有一条用例把差异钉在同一处 |

### 16.3 专属 `DoD` 的证据（`docs/61` §6.5 的 M8-3 行逐条）

| `DoD` 条目 | 证据（用例 / 命令） |
| --- | --- |
| **8 条路由**全部注册、每条至少一条用例 | ⑦：`local 416 → 424`（+8），`owners.M8 14 → 6`；形态门 `slash_alias_audit.py` = **0 defect**（8 条全是上游 plain 注册，无 `dual-form` 需求）。单测：`routes::mcp::{workspace,agent}::tests::{router_builds_without_panicking,…}`；e2e：`tests/mcp/{workspace,agent}.rs` 的 11 条 |
| **write-only**：列表/详情响应里 `headers` / `env` 的值一个字节都不出现 | `tests/mcp/workspace.rs::library_crud_round_trip_never_echoes_the_entry`（在**原始 body** 上断言：create / list（admin+member+guest 三次）/ 均不含 `sk-live-…`，**也不含 URL** —— URL 本身就是凭据材料）；`tests/mcp/agent.rs::binding_add_toggle_remove_is_idempotent`（绑定列表同样两样都不含）；`routes/mcp/workspace.rs::response_never_carries_the_entry`（DTO 级） |
| **重名拒绝**（迁移 `316` 的唯一约束） | `tests/mcp/workspace.rs::library_rejects_duplicate_names`：create 撞名 ⇒ **409**、把另一条改名撞过去 ⇒ **409**、原地改自己（名字不变）⇒ 200 |
| agent 绑定的 `enabled` 开关**幂等** | `tests/mcp/agent.rs::binding_add_toggle_remove_is_idempotent`：`add` 两次 ⇒ `count_bindings == 1`；`enabled=false` 两次 ⇒ 仍 1 行、值仍 `false`（**绑定存活**，不删不插）；再 `true` ⇒ 回到 `true`；`DELETE` 两次 ⇒ 第二次 404；摘完库条目**仍在** |
| **overlay 合并纯函数**与既有合并语义逐条一致（同名覆盖、runtime 层做底） | `mc-core/src/mcp/overlay.rs` 的 **25** 条：`merge_*` 12 条（上游 `mcp_overlay_test.go` 逐条移植，含「两侧都坏报 agent 支」「非对象条目拒绝」「顶层键只从 agent 侧保留」）+ `resolve_*` 12 条（上游 `workspace_mcp_test.go` 逐条移植，含**遗留容器 `mcp` 折进 `mcpServers`** 的 OpenCode 回归点、同名 agent 胜出、`mcp` vs `mcpServers` 的优先级）+ 1 条 `Debug` 脱敏。**与 daemon 侧的关系**：daemon 的 `runtime×agent` 本地合并在 `mc-daemon/src/mcp/runtime.rs`（M6-9 交付，本片**只读**）；本文件是它**上游一层**（agent 已解析后的 `mcp_config` ← per-task overlay），两层不是同一份逻辑、也没有第二份实现 |
| **无需平台替身**：真库 CRUD + 校验反例 + overlay 纯函数用例 | 真库 11 条（`tests/mcp/`，门 ⑥ 拉起）+ 校验反例：空名 / 非法字符名（空格、点、斜杠、emoji）/ 非对象条目（`[]`、字符串、`null`、缺失）/ 空对象 / 重名 / 未知 transport **原样透传**（`library_rejects_bad_input_but_keeps_unknown_transports` —— 上游刻意让 transport 是自由字符串，本片**不**把它做成硬校验，理由逐字写在 `mcp_transport_of` 的文档里） |
| **授权面**：agent 4 条必须是 `loadAgentForUser` 语义，**不是**裸 workspace member | `tests/mcp/agent.rs::binding_authorization_is_load_agent_for_user`：agent owner（**member 角色**）四条全 200；admin 200；**另一个同 workspace 的 member** 403；guest 403；非成员 404；缺 workspace 400；无会话 401；别的 workspace 的 agent 404；`kind<>'user'` 的 agent 404。库面：`library_authorization_matrix_per_endpoint`（读 member/guest 200、写 403、非成员 404、`null` body 400） |
| **应用层栅栏**（两张表无 FK ⇒ 竞态只能自己防；上游有专门的竞态用例） | `tests/mcp/agent.rs::binding_add_cannot_land_after_the_server_delete_commits`：真持锁的 `FOR UPDATE` + 真 handler 并发 ⇒ 断言写入方确实**停在锁上**（`pg_stat_activity.wait_event_type='Lock'` 且 query 命中 `…FOR SHARE`）、删除提交后才返回且**不是 200**、库里 **0** 行孤儿绑定；仓储层：`create` 的 `FOR KEY SHARE`（上游 `LockWorkspaceForChatSessionCreate`）、`delete` 的 `FOR UPDATE`（上游 `LockWorkspaceMcpServerForUpdate`）、`add` 的 `FOR SHARE`（上游 `LockWorkspaceMcpServerForShare`） |

### 16.4 门禁读数（逐字取自当轮日志；日志留档在 run workdir 的 `gates-m8-3.log`）

```
bash scripts/gates.sh --with-db --db-url 'postgres://mc_lum1800:…@127.0.0.1:5432/mc_lum1800'
  ①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0 · ⑥db 0（migrate=0, e2e=0）
  ⑧schema-drift 0 · ⑦route-parity 0 · ⑨conformance 0 · ⑩file-size 0
  ⇒ overall: PASS — 10/10 gate(s) green in 133s
```

- ⚠️ **首跑是 9/10**：⑧ 报 `permission denied to create database` ⇒ **exit 2**（`gates.sh` 里 2 是
  「根本没法开跑」而不是门失败）。原因是本 run 新建的测试角色 `mc_lum1800` 没有 `CREATEDB`
  权限（⑧ 要建自己的 scratch 库 `schema_probe_w0b_drift_<pid>`）。`ALTER ROLE mc_lum1800 CREATEDB`
  后**单跑 ⑧ = 26s PASS**，随后**整套重跑 = 10/10 / 133s**（上表就是那一次）。
  ⇒ **lesson**：`--with-db` 之前先给角色 `CREATEDB`，否则会把「环境缺权限」误读成「本片红了」。
- ⑦（**本片会动读数**；`baseline 406` **不动**）：`upstream 456 (commit f41fae6b08fb) |
  local 424 registered`、`implemented 344 real + 4 placeholder = 348 / 456`、`known_gap 108`、
  `unclaimed 0`、`regression 0`、`local_only 9`、`gaps by owner: M9=33 M7=24 M3+=16 M2-A=13
  M3=11 **M8=6** M10=5`（和 = 108 ✓）。**与 issue 描述「起手补充（12:30 cycle）」的片后预期逐字相同**：
  `local 416→424`、`implemented 340→348`、`known_gap 116→108`、`owners.M8 14→6`（−8 = 本片 8 条路由）。
  `--write-baseline` **未跑**（唯一一次刷新归 M8-7 `LUM-1804`）。
- ⑦ 第二条（形态）：`slash_alias_audit.py` = `0 defect(s)`、exit 0（M8 `dual-form required: 0`，
  **没有** allowlist 退路）；命令输出的逐字一行：`registered upstream-key literals: 428`。
- ⑩：**0 违规**，`scripts/file_size_baseline.tsv` **未动**。本片最大文件在 `cargo fmt --all` **之后**量：
  `mc-core/src/mcp/overlay.rs` **638**、`tests/mcp/agent.rs` 503、`mc-repos/mcp/workspace_server.rs` 487、
  `routes/mcp/workspace.rs` 471 —— 全部 ≤800（余量最小的那条是 162 行）。
- ⑨：`report matches crates/mc-conformance/report.json`（本片 0 fixture 改动 ⇒ 未漂移）。
- ⑧：schema-drift 绿（本片 **0 迁移**、0 表改动 ⇒ 与基线同形）。
- ⑥：`mc-migrate` 566 个迁移 + `--ignored` 全绿，含本片 **11** 例真库 e2e（`mc-http --test mcp`）。
- ⑤（`cargo test --workspace`，**不带**库变量）：全绿。本片新增 **42** 条**零 DB** 单测
  （`mc-core::mcp::overlay` 25 + `mc-repos::mcp` 8 + `mc-http` lib 的 `routes::mcp` 9）
  ⇒ 连同真库 11 例，本片共 **53** 例新测试。
- **【lesson·磁盘】** 起手 `df` 26G 可用，本 run 只保留一份 `target/`（`CARGO_INCREMENTAL=0` 全量重建后
  5.1G，默认增量式要 11G）；三轮门禁跑完仍 20G+。与 M8-2 同手法：**所有 cargo 调用都带
  `CARGO_INCREMENTAL=0`**，读数不受影响（同一脚本、同一批命令）。

### 16.5 交接（给 M8-6 / M8-7 / 未来接 claim 的切片）

1. **M8-6（composio）**：per-task overlay 的**合并侧**已经就位 ——
   `mc_core::mcp::overlay::merge_task_overlay` 是唯一实现点，**不要**在 `mc-composio` 里写第二份；
   落库原语 `mc_repos::task::overlay::attach_runtime_mcp_overlay` 仍是 `todo!()`（R-M8-9），
   本片**没有**碰它（它在 anchor 的写集里，实现归尾账）。
2. **M8-7（INT）**：本片把 ⑦ 推成 `local 424 / implemented 348 / known_gap 108 / owners.M8 6`；
   下一次刷新（`--write-baseline`）归你。请一并：① 在缺口清单里复述 **D9**（claim 的 agent 数据块
   未实现 ⇒ 绑定折叠与 overlay 合并本波都没有调用点）；② 复核 M8 面剩余 `unevaluable`；
   ③ `mc-core/src/mcp.rs` 与 `mc-repos/src/mcp/mod.rs` 的模块头仍写着「M8-3 待落地 / doc-only 桩」，
   是 anchor 冻结文件里的**文案债**（本片不得改，与 §14.2 末两条同性质）。
3. **未来接 claim 的切片**：接线点是 `crates/mc-http/src/routes/daemon/dto.rs` 的 agent 数据块
   （目前**没有** `mcp_config` 字段）。按上游 `daemon.go:2466-2505` 的顺序：
   `list_enabled_for_agent(agent)` → `resolve_agent_mcp_config(&bindings, agent_mcp_config)`
   → `merge_task_overlay(&resolved, task.runtime_mcp_overlay)`；**两段都要 fail-soft**
   （`Err` ⇒ 用上一步的原值 + 一条 `warn`），**绝不**因为共享条目或 overlay 坏掉就 panic 或丢掉
   agent 自己保存的 servers —— 这是上游注释逐字点名的失败模式。
4. **给后续任何写 `workspace_mcp_server` / `agent_mcp_server` 的片**：`config` 是 **write-only**
   （响应永不带值，连 `url` 都不带）；绑定**以 id 为键**（改名不影响使用）；两张表**没有 FK**
   ⇒ 删除 / 建行的栅栏只能靠应用层的三条锁语句（`create` 的 `FOR KEY SHARE`、`delete` 的
   `FOR UPDATE`、`add` 的 `FOR SHARE`），**不要**绕过它们直接写 SQL。

## 17. M7-5（`LUM-1770`）：telegram 入站 + 安装与绑定面（4 路由）

> **号段说明**：`docs/60-M7-PLAN.md` §6.5 只在 M7-5 行写「偏离写进 `docs/32` 偏离表」，
> 没给号；按派发时的约定取**文件末尾的下一个号** `## 17.`（`## 15.` = M7-4、`## 16.` = M8-3）。
> 14:30 cycle（`LUM-1859`）的派发预飞也点名了「M7-5 → M7-6 的 `api.rs` 写集边界项，在飞片已自
> 登记 `docs/32` §17 勘误」——本节就是那份自登记。

**口径**：本节一切落点与偏离都在上游 `f41fae6b08fb` 与本片当轮 base 上核对。本片**起手 base**
是 `f1cd4bbc`（= `43361e92` + merge #90 / M7-4）；提交前 `origin/feat/multica-rs-initial`
已前进到 **`0b355b0d`**（= `cc6ef9ea`（§94 cycle）+ §95 cycle，**只动 `docs/37`**）⇒ 本片已
rebase 到它，§17.3 的两个读数（起手 tree 与 rebase 后 tree）都给。

### 17.1 落点（写集逐字）

| 落点 | 内容 |
| --- | --- |
| `crates/mc-channel/src/telegram/config.rs` | 安装配置 blob、`Credentials` / `PublicConfig`、`Decrypter`、`Sensitive`、`parse_bot_id` / `parse_stored_bot_id`、`decrypt_token` |
| `crates/mc-channel/src/telegram/inbound.rs` + `inbound/tests.rs` | Bot API **入站** wire 类型（`Update` / `Message` / `Chat` / `User` / `MessageEntity`）、`RawEvent`、`inbound_from_update` 一族、`message_key` / `parse_message_ref` |
| `crates/mc-channel/src/telegram/api.rs` + `api/tests.rs` | `TelegramApi` 端口（五个方法）+ `JsonBotApi`（`reqwest`）+ `ApiError`（含 `Conflict` / `retry_after` / `http_code`）+ 进程内基址接缝 |
| `crates/mc-channel/src/telegram/install.rs` + `install/tests.rs` | `InstallError` 十一变体 + `InstallStore` 端口 + `InstallService::{register,list,get_in_workspace,revoke}` + `classify_credential_verification_error` |
| `crates/mc-channel/src/telegram/binding.rs` + `binding/tests.rs` | 绑定令牌的铸造与原子兑换（`BindingStore` 端口）、`hash_binding_token`、`random_binding_token` |
| `crates/mc-channel/src/telegram/resolvers.rs` + `resolvers/tests.rs` | `InstallationRow`（不透明平台值）、`InstallationQueries` / `IdentityQueries` 端口 + 仓储实现、`session_routing`（纯函数）、`TelegramTypingNotifier`、`TelegramResolverSet` |
| `crates/mc-channel/src/telegram/replier.rs` + `replier/tests.rs` | 判决 → 文案、群聊**不**发 bearer 链接、`is_addressed_issue_command` / `dropped_reply_text`、`url_encode` |
| `crates/mc-channel/src/telegram/mod.rs` + `tests.rs` | **长轮询回路**（`getUpdates` + offset 推进 + 409/429/传输的三种处置）、`TelegramChannel`（`Channel` 五方法）、工厂、`register` / `register_with`、`spawn_detached` |
| `crates/mc-http/src/routes/channels/telegram.rs` | 4 条路由、鉴权层、未配置语义、wire DTO |
| `crates/mc-http/src/routes/channels/telegram/store.rs` | 三条上游查询的 PG 端口实现（`PgInstallStore` / `PgBindingStore`） |
| `crates/mc-http/src/routes/channels/telegram/tests.rs` | 不依赖库的 wire / 状态码 / 契约键用例 |
| `crates/mc-http/tests/channels/telegram.rs` | 4 条路由的真库端到端（门 ⑥，`#[ignore]`）+ Bot API 替身 |
| `crates/mc-http/tests/channels/main.rs` | 追加 `mod telegram;`（M7-4 已在此追加 `mod slack;`） |
| `crates/mc-conformance/report.json` | ⑨ 快照再生（唯一一条 fixture 状态变化，见 17.2 D11） |

**写集勘误（逐条登记，照 M7-2 / M7-3 / M7-4 先例）**：`docs/60` §3.3 给 M7-5 的格子是
`telegram/{inbound,resolvers,replier,install,binding,config}.rs` + `routes/channels/telegram.rs`；
派发时的「起手补充」已追加 `telegram/mod.rs`（新模块要可见、`register()` 要填、长轮询要有家）。
本片**再追加**的路径只有两类，理由**只有一条** = 门 ⑩ 的 **800 行硬限**（不是拆凑数字）：

- `telegram/api.rs` + `api/tests.rs`：上游 `api.go` 的传输面（见 D2）；
- 六个 `*/tests.rs`（`inbound` / `api` / `install` / `binding` / `resolvers` / `replier`）+ 根
  `tests.rs`：用例内联后分别 1118 / 920 / 917 / 680 / 1238 / 991 / 666 行 ⇒ 用例拆成子模块。
  `config.rs`（605 行，含内联用例）**未拆**。

拆完 `crates/mc-channel/src/telegram/**` 最大文件 **666 行**、`crates/mc-http/src/routes/channels/telegram/**`
最大 **367 行**、`crates/mc-http/tests/channels/telegram.rs` **695 行** ⇒ 全部 ≤ 800；
`scripts/file_size_baseline.tsv` **未动**（门 ⑩ 0 违规）。

### 17.2 偏离登记（M7-5-D1 … M7-5-D13）

| # | 偏离 | 处置与理由 |
| --- | --- | --- |
| **D1** | **未配置分支先于鉴权**：上游这四条 handler 的第一句都是 `if h.TelegramInstall == nil { …; return }`（**不读**身份），而本仓的 `AuthUser` 是提取器（在 handler 体之前跑） | 四条 handler 改用 `Option<AuthUser>`：**未配置**分支先返回（与上游同序），**已配置**分支再 `ok_or(unauthorized)` 要身份。这是 ⑨ 的 `workspaces/TestListTelegramInstallationsNotConfiguredReturnsEmpty`（actor `anonymous`、期望 **200**）**唯一**可达的形态 —— 若先鉴权，该 fixture 只会从 `unmounted` 变成 `mismatch`（更糟）。**登记为跨片待定**：lark 面（M7-14）有 **7** 条同形 fixture，建议照本条处理并在 INT 复核「未配置信息可被匿名读到」是否可接受 |
| **D2** | **`api.rs` 的写集边界**：`docs/60` §3.3 把 `api.rs` 记在 **M7-6** 名下，但 M7-5 的三处调用（入站 `getUpdates`、安装校验 `getMe` + `getWebhookInfo`、判决回复 `sendMessage`）都在这条传输上，而 **M7-6 的硬前置是 M7-5** | 依赖方向只能是「M7-5 落传输、M7-6 在同一文件里补出站流式那一半」（与 M7-3 先落 `slack/socket.rs`、M7-4 再接线 `send` 是同一条先例；anchor 的 `mc-channel/Cargo.toml` 注释已把出站 HTTP 的点名写成 "telegram `api.rs`"）。**交给 M7-6 的缺口逐条**：`editMessageText`、`sendMessage` 的 `parse_mode=HTML` 与 UTF-16 分片、429 的"一次重试"包装（上游 `sendMessageWithRetryAfter`）、`sender.rs` / `outbound.rs` / `delivery.rs` / `markdown.rs`。**14:30 cycle 的派发预飞已点名本条** |
| **D3** | **offset 不落地**：本地**不**存 `getUpdates` 的 offset（没有表、没有文件、没有结构字段） | 上游同：确认消费发生在 **Telegram 服务端**（`getUpdates?offset=N` 一发，`update_id < N` 即出队）。「重启不重复消费、不丢更新」= ① 每次 `connect` 从 **0** 开始（Telegram 重投全部未确认更新 ⇒ **不丢**）+ ② 批内先推进 offset 再逐条投递 + ③ engine 的 `(installation, message_id)` 去重吸收重投 ⇒ **不重复消费**。三条各有一条用例（`tests.rs` 的 `the_polling_loop_dispatches_and_advances_offset_after_each_batch` / `a_restart_replays_pending_updates_from_offset_zero`）；把 offset 落库反而会引入"落库成功但投递失败"的两难 |
| **D4** | **传输错误丢弃 cause**：`ApiError::Transport { method }` 不带 `reqwest::Error` | Bot API 的请求 URL **带 bot token**（`/bot<token>/<method>`），而 `reqwest` 的 `Display` 会印出整条 URL ⇒ 上游 `TestTransportErrorDoesNotExposeBotToken` 只保住"不回显令牌"那半，`errors.Is(err, transportErr)` 那半**主动放弃**（与 `slack::outbound::SlackApiError::Transport` 同款）。诊断信息改为方法名（`ApiError::method()`） |
| **D5** | **本地副本**：`Decrypter` / `Sensitive` / `url_encode` / `error_with_code` / `BindingMinter` / `BindingStore` 的端口形状与 slack 面**同名同形但各持一份** | 上游同样是 `slack/config.go` 与 `telegram/config.go` 各自声明 `credentials` / `Decrypter`，且两面的错误前缀（`slack:` / `telegram:`）与字段集不同（Slack 两个密文列、Telegram 一个）。收敛成共享件要动 M7-3 / M7-4 的**已合文件**（不在本片写集）⇒ 本片只新增。**登记为后续收敛项** |
| **D6** | **会话隔离键的分隔符**：上游 `chat_id:线程根`，本仓的通用策略（`BindingKeyPolicy::ChatIdPlusThreadRoot`）用 `#` | **隔离粒度完全一致**：`inbound_from_update` 只对 `is_topic_message`（= 论坛话题，只存在于超级群）写 `Source.thread_id` ⇒ 私聊 / 普通群聊的键都恰好是 chat id，论坛话题各自成一个会话 —— 与上游 `telegramSessionRouting` 逐条等价（`resolvers/tests.rs` 的 `the_runtime_policy_matches_the_upstream_routing_modulo_the_separator` 钉住"只差分隔符"）。这条是 **M7-3-D1** 已登记的跨片偏离（Slack 面同款） |
| **D7** | **绑定行的 `config` 列**：上游写 `{"chat_id": …}`，本仓通用实现写 `null` | 真实 chat id 在隔离键的前缀里（见 D6），读得回来；要落到列上需改 M7-2 的 `NewEnsureSession` 传参面（不属本片写集）。**M7-3-D3 同款** |
| **D8** | **`Channel::send` 是"最小可用"**：纯文本 `sendMessage`（话题 + 引用参数照传），**没有** Markdown→HTML、分片、流式编辑、投递状态机 | 上游这三块在 `sender.go` / `outbound.go` / `delivery.go`（M7-6 的写集）。本片给一条**真能发出去**的路径（判决回复正是走它），而不是交一个自称 `TEXT` 却发不出的半成品；`capabilities()` 仍照上游声明五个位（`MESSAGE_EDIT` 的实现归 M7-6） |
| **D9** | **三条平台文案落在 `replier.rs`**（`AGENT_OFFLINE_TEXT` / `AGENT_ARCHIVED_TEXT` / `UNSUPPORTED_TYPE_TEXT`；上游在 `sender.go`） | 上游这三个常量在 `sender.go`（M7-6 的文件），但 M7-5 的判决回复器与入站回路**都要**用它们。逐字照抄文案，M7-6 落 `sender.rs` 时**复用本文件的常量**，不要再抄一份 |
| **D10** | **解析器面的宿主装配仍悬空**：`TelegramResolverSet` 已齐（含出站回复器与打字指示），但 `apps/mc-server/src/channels.rs` 的一次装配调用**不在本片写集**（anchor 冻结） | 与 M7-4 的 D9 同款：本片提供 [`mc_channel::telegram::resolvers::TelegramResolverSet`] + `register` / `register_with` 两个入口，由 INT / 后续锚点调一次。**登记缺口**，不是漏实现 |
| **D11** | **⑨ 快照再生**：本片把 `crates/mc-conformance/report.json` 重新生成（`pass 5 → 6`、`unmounted 31 → 30`，`by_via.handler` 新增 `pass 1`） | 这是「fixture 从 `unmounted` 转 `pass`」这条 DoD 的**产物**（报告是 stateless 层快照，⑨ 用 `--check` 逐字比对）。**唯一**一条状态变化就是 `workspaces/TestListTelegramInstallationsNotConfiguredReturnsEmpty@server/internal/handler/telegram_test.go:21#27`（`unmounted → pass`），其余 364 条**逐字未动** |
| **D12** | **e2e 替身的注册形态**：Bot API 替身用 axum 的 `fallback` 按**方法名**分派，而不是按 `/bot<token>/<method>` 逐路径注册 | 令牌里带 `:`，而 matchit 0.7 的 `:` 是路径参数标记 ⇒ 把令牌写进路径段是不稳的。替身纪律（`docs/60` §4.2 第 1 条"只替平台 wire"）不受影响：断言链仍是"真 HTTP → 真 handler → 真 DB" |
| **D13** | **`getUpdates` 显式送 `offset`**：上游用 `json:"offset,omitempty"` ⇒ `offset = 0` 时**省略**该字段；本仓恒送 | 两者对 Bot API **同义**（缺省 = "从最早的未确认更新开始返回" = `offset 0`），且显式送让请求体可被替身逐字段断言（`api/tests.rs` 的 `get_updates_sends_the_upstream_wire_shapes`） |

**另有一条不构成偏离的**实现选择（记在这里免得被当漏项）：本片**没有**媒体解析器
（`TelegramResolverSet::with_media` 保留接口、默认 `None`）。上游 telegram 的媒体取回在
`outbound.go` / `delivery.go` 一侧（M7-6），入站侧只做**分类**（`classify_message`）与
"暂不支持"的礼貌告知 —— 与本仓 M7-3 的 slack 面（媒体解析归 M7-3）**不同**，这是上游的
真实切分，不是本片省事。

### 17.3 门禁读数（本片当轮实测）

```
①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0 · ⑦route-parity 0
· ⑨conformance 0 · ⑩file-size 0                                ⇒ 8/8 PASS / 48s
①–⑤ 0 · ⑥db 0（migrate=0, e2e=0）· ⑧schema-drift 0 · ⑦ 0 · ⑨ 0 · ⑩ 0  ⇒ 10/10 PASS / 230s
```

门 ⑦（`route_parity.py`）在**两棵树上各测一次**（本片的 `+4` 与预测逐字一致）：

```
# ① 本片起手 tree（base f1cd4bbc `+` 本片）
upstream 456 (commit f41fae6b08fb) | local 432 registered | baseline 406
  implemented  352 real +   4 placeholder =  356 / 456   known_gap  100   unclaimed 0   regression 0   local_only 9
  gaps by owner: M9=33  M3+=16  M7=16  M2-A=13  M3=11  M8=6  M10=5

# ② 起手 base 自身（同一个命令；起手时实测 —— 差值恒为 4）
base f1cd4bbc : local 428 | implemented 348 real + 4 ph = 352 | known_gap 104 | owners.M7 20 | baseline 406
```

⇒ 两个 tree 上**本片都是 `+4`**（四条路由：`GET` / `DELETE …/telegram/installations[/…]`、
`POST …/telegram/install`、`POST /api/telegram/binding/redeem`），且不变式
`implemented + known_gap == 456`、`regressions == 0`、`unclaimed == 0`、`local_only == 9`
全部成立；**`scripts/file_size_baseline.tsv` 未动**。门 ⑦ 的第二条（形态）实测
`0 defect(s)`、exit 0（M7 没有 allowlist 退路）。缺口板里**已无**任何 telegram 路由
（`owners.M7 20 → 16`，剩下的 16 条 = lark 5 + dingtalk 7 + wecom 4）。

门 ⑨（`--no-db --check`）与快照逐字相符：`pass 6 · mismatch 23 · unmounted 30 · placeholder 0
· unevaluable 306`（`offline_decidable 6/59`）。本片专属的那一条
（`workspaces/TestListTelegramInstallationsNotConfiguredReturnsEmpty`）**已 `pass`**（见 D11）。

用例账（全部实跑）：`cargo test -p mc-channel --lib` = **332 passed / 0 failed**（其中
`telegram::` **86** 条）；`cargo test -p mc-http --lib` = **352 passed / 0 failed**
（其中 `routes::channels::telegram::` **11** 条）；门 ⑥ 的
`cargo test -p mc-repos -p mc-http -p mc-scheduler -p mc-server --features mc-http/test-util
-- --ignored` = `e2e=0`（本片新增 **8** 条真库 e2e 全绿）。

### 17.4 交接（给 M7-6 / M7-21 INT）

1. **【给 M7-6（`LUM-1771`）· 最重要的一条】`api.rs` 已经存在**：`mc_channel::telegram::api`
   里有 `TelegramApi` 端口（`get_me` / `get_webhook_info` / `get_updates` / `send_message` /
   `send_chat_action`）、`JsonBotApi`（两个超时的 `reqwest` 客户端）、`ApiError`
   （`Conflict` / `retry_after()` / `http_code()`）与**进程内基址接缝**（`api_base` /
   `set_api_base` / `reset_api_base`）。**你的写集第一条就是"在同一个文件里补出站流式那一半"**
   （`edit_message_text` + `parse_mode=HTML` + 429 的一次重试包装 + `sender.rs` 的 UTF-16 分片）；
   `SendMessage` 里 `parse_mode` / `message_thread_id` / `reply_to_message_id` /
   `allow_sending_without_reply` 四个字段已经就位，wire 形态有逐字段用例（见 D2 / D13）。
2. **【给 M7-6】`Channel::send` 的替换点**：`telegram/mod.rs` 的 `send` 现在是纯文本
   `sendMessage`。把出站发送器接进来时**改这一处**，别在 `replier.rs` 与 `mod.rs` 各留一条路径
   （判决回复应继续走同一条发送器 —— 上游 `replier.go` 就是这么做的）。
3. **【给 M7-6】文案别抄第二份**：`AGENT_OFFLINE_TEXT` / `AGENT_ARCHIVED_TEXT` /
   `UNSUPPORTED_TYPE_TEXT` 已在 `replier.rs`（D9）；`ISSUE_DISPATCH_FAILED_TEXT` 与
   `ISSUE_ERROR_REPLY_TIMEOUT` 在 `mod.rs`。
4. **【给 M7-21 INT】需要你收口的三件**：① `owners.M7` 从 16 继续往下（本片已把 telegram 4 条清零）；
   ② ⑦ 基线 `--write-baseline` 406 → 全波落地后的值（本片**未**刷）；③ 在 INT 报告里复述
   **D1**（未配置分支先于鉴权：lark 面 7 条 fixture 同形，需统一口径）与 **D2**（`api.rs` 的
   写集边界，M7-6 已按同一文件扩展）。另有两条**本波结束时仍在**的登记项：D5（本地副本的收敛）
   与 D10（解析器面的宿主装配一次调用）。
5. **【给后续任何写 `telegram/` 的片】写者表**在 `telegram/mod.rs` 的模块文档里（M7-6 的五个文件
   已列出）。**不要**再改 `telegram/mod.rs` 的模块表之外的共享件（`routes/channels/mod.rs` /
   `mount.rs` / `state.rs` / `routes/auth.rs` 全部 anchor 冻结）。

### 17.5 lesson（本片新增）

- **【lesson·⑨ 的 `unmounted → pass` 会逼出"鉴权顺序"这个接口决定】** 上游那批 fixture 是
  **直接调 handler** 抽出来的（`site: direct_handler`），所以它看到的是 handler 的第一句
  （`== nil` 检查）；回放到本仓的 **router** 上就会撞 `AuthUser` 提取器的 401。`docs/60` §6.2
  只写"承诺 8 条 `unmounted → pass`"，没写这条后果。**判据**：凡"未配置语义"fixture，其
  handler 必须把"部署密钥存在"的判定放在**读身份之前**（`Option<AuthUser>` 就够了）；先全绿再
  回头改顺序，会发现 8 条里最多只能转成 `mismatch`（比 `unmounted` 更糟 —— 判为"实现错了"）。
- **【lesson·长轮询回路的用例可以完全"不睡真觉"】** 回路的时延只有三个来源：`retry_delay`
  （注入为 0）、Telegram 强制的 429 退避（唯一一处 1 秒，是协议下界）、以及"没有新消息"的挂起
  （用 `std::future::pending()` 表示，外层 `tokio::time::timeout` 收尾）。于是"offset 推进 /
  重启重投 / 409 致命 / 429 吸收 / 瞬态交给退避"五条判决全都能在**毫秒级**跑完，且**没有**一条
  依赖 sleep 的时序猜测。
- **【lesson·把"不落地"写成三条可观测判据，比写一句"重启安全"有用得多】** `offset` 不落地这类
  决定，容易在评审时被读成"漏实现"。本片把它拆成 ① 每次从 0 开始 ② 先推进后投递 ③ 引擎去重 ——
  每条各有用例，且第 ③ 条用"两次投递的 `message_id` 相同"证明去重键成立（而不是断言一句注释）。
- **【lesson·`#[cfg(test)] mod tests;` 的解析路径取决于宿主文件是 `x.rs` 还是 `x/mod.rs`】**
  `telegram/inbound.rs` 里的 `mod tests;` → `telegram/inbound/tests.rs`；而
  `telegram/mod.rs` 里的 → `telegram/tests.rs`（**不是** `telegram/mod/tests.rs`）。本片第一次
  就把它写错到 `mod/` 下，编译器只报"找不到文件"，改个目录名就过了。

## 18. M7-6（`LUM-1771`）：telegram 出站与投递（0 路由）

**口径**：本节一切落点与偏离都在上游 `f41fae6b08fb` 与本片当轮 base 上核对。本片**起手 base**
是 `a326ced7`（= 合并 #92（M7-5）之后 `origin/feat/multica-rs-initial` 的 tip —— M7-5 的合并提交
`a326ced77d1dbadde67aa7cebb8dedabf55a35cc` 带 `merge(m7): PR #92 …` 的标题，**不带** `merge #93`）。
提交时 origin tip 已前进到 **`0ec41345`**（§97/§98 cycle + 合并 #93 / M2-A 尾-补）⇒ 本片已 rebase 到它，
**写集零交集、无冲突**（进场的是 `mc-http/src/routes/{mod,mount}.rs` + 一个新 route 文件 + `mc-repos/src/{lib.rs,squad_evaluation.rs}`，
与本片 20 个改动文件无交集）。

### 18.1 落点（逐字路径）

| 落点 | 写者 | 内容 |
| --- | :-: | --- |
| `crates/mc-channel/src/telegram/markdown.rs` + `markdown/tests.rs` | M7-6 | 上游 `markdown.go`（119 行）：Markdown → Telegram **HTML parse mode**（逐行、代码块保真、非贪婪语义手写复刻） |
| `crates/mc-channel/src/telegram/sender.rs` + `sender/tests.rs` | M7-6 | 上游 `sender.go`（194 行）：UTF-16 码元分片（优先换行）、HTML 渲染、**HTML 被拒回落纯文本**、只第一片引用、`SendResult` 带末片 |
| `crates/mc-channel/src/telegram/delivery.rs` + `delivery/tests.rs` + `delivery/testing.rs` | M7-6 | 上游 `delivery.go`（457 行）：投递**所有权状态机**（租约 / 阶段 / 发送状态 / 结转 / 收口）+ `DeliveryStore` 端口 + `PgDeliveryStore` 适配；`testing.rs` 是 `pub(crate)` 的进程内替身（`outbound/tests.rs` 也用它） |
| `crates/mc-channel/src/telegram/outbound.rs` + `outbound/tests.rs` | M7-6 | 上游 `outbound.go`（1,633 行）的可判定面：占位消息一次、编辑阶梯、终态编辑/分片、失败告知、目标解析 |
| `crates/mc-channel/src/telegram/api.rs`（**扩展**） | M7-5 落 + **M7-6 补** | 补出站半边：`editMessageText`（端口 + `EditMessageText` 入参 + wire 形态）、`send_message_with_retry_after`（429 的**一次**重试，上游 `sendMessageWithRetryAfter`）。**既有五个方法的形态一字未动** |
| `crates/mc-channel/src/telegram/mod.rs`（**扩展**） | M7-5 落 + **M7-6 补** | 模块表 + `Channel::send` 的**单点替换**（走发送器；判决回复与 agent 答复同一条路径） |
| `crates/mc-http/tests/channels/telegram_round_trip.rs` + `main.rs`（一行 `mod`） | M7-6 | **端到端收发回路**（`docs/60` §4.2 的渠道门禁证据），真库 `#[ignore]` |
| `crates/mc-repos/src/channel/delivery.rs`（**追加**）+ `delivery/tests.rs`（新） | M7-6 追加 | 上游 `channel.sql` 里投递状态机的**十条语句**（见 D1）；内联 `db_tests` 原样搬到子目录 |

### 18.2 偏离（D1…D10，逐条可核对）

- **D1 写集勘误（三类，逐类说明）**：

  1. **新增路径**（都在 `docs/60` §3.3 的写集之外，但都是实现该片所必需的）：
     `telegram/{markdown,sender,delivery,outbound}.rs` 的 `*/tests.rs` 子目录、`delivery/testing.rs`、
     `crates/mc-http/tests/channels/telegram_round_trip.rs`（+ `main.rs` 一行 `mod`）。
     `mod.rs` 本身由**起手补充**追加（第二类漏项第 3 次：不写进去四个新文件根本不参与编译）。
     四个测试子目录是**门 ⑩ 的 800 行硬限**逼出来的切分（`outbound.rs` 首次成形 809 行 ⇒ 再压到 788；
     `delivery.rs` 764）。**未动** `scripts/file_size_baseline.tsv`。
  2. **追加到 M7-1 的已合并文件**：`crates/mc-repos/src/channel/delivery.rs` 追加**十条语句**
     （`get_reply_turn` / `acquire_reply_delivery` / `release_…` / `renew_…` / `mark_…_sending` /
     `record_…_placeholder` / `record_…_chunk` / `reset_…_send` / `mark_…_send_unknown` / `close_…_turn`）
     与两个入参结构（`NewReplyDeliveryAttempt` / `CloseReplyDeliveryTurn`）。**没有改 M7-1 的任何一条既有语句或断言** ——
     它的 `claim_reply_delivery` 与上游 `AcquireChannelReplyDelivery` 有三处语义差异（深度由调用方给且只前进、
     `phase` 守卫、不重置 `send_state`），所以新增的是**上游形态**的那一条而不是改既有那条（既有那条有自己的调用者与用例）。
     同时把内联 `mod db_tests` **原样**搬到 `crates/mc-repos/src/channel/delivery/tests.rs`（内联会把文件顶过 800）。
     理由：投递状态机要**真库**才算落成（DoD 明写"delivery 状态机有表驱动用例"），而 `mc-channel` 的依赖面被 anchor 冻结
     （没有 `sqlx`、没有 `mc-db`）⇒ PG 语句只能落在 `mc-repos`。
  3. **改 M7-5 的四个测试替身 + 一条断言**：`api.rs` 的 trait 新增 `edit_message_text` ⇒
     `install/tests.rs`、`replier/tests.rs`、`resolvers/tests.rs`、`telegram/tests.rs` 里的四个 `TelegramApi` 替身
     各补一个（失败关闭的）实现。另**一条断言改了**：`telegram/tests.rs` 的
     `send_uses_plain_text_with_thread_and_quote` → `send_goes_through_the_outbound_sender` ——
     出站替换点接上发送器之后，"纯文本 + 裸数字 id"按设计变成"HTML + 复合键"（上游 `sender.go` 的 `Send` 就是后者）。
     这是本片**唯一**改动的 M7-5 用例。
- **D2 没有事件总线 ⇒ 入口是显式调用（**登记为缺口**，不是漏实现）**：上游 `Outbound` 是 `events.Bus` 的订阅者
  （transcript / done / failed / cancelled 四类事件），并自带 terminal worker 池、重试最小堆与空闲清扫器来驱动它。
  本仓**没有**那条进程内总线（`mc-realtime` 是给前端的信封通道）⇒ 本片只落**状态机 + 一次一步**的入口
  （`push_partial` / `deliver_answer` / `deliver_failure_notice`，回报 `Step{Done,RetryAfter,Failed}`），
  由宿主按自己的节奏驱动。与 M7-4 对 `slack/outbound.go` 的处理**同一条先例**（本波两个出站片都没有总线可挂）。
  **宿主接线（`apps/mc-server`）不在本片写集** ⇒ 登记为缺口，与 M7-5 的解析器面装配同一类。
- **D3 目标解析 = 纯函数 + 调用方取数**：上游 `resolveTarget` 自己查 `channel_task_delivery` /
  `channel_installation` 并解密凭据。本仓的 adapter 不直接写 DB ⇒ 解析的**语义**落在
  `ReplyTarget::from_task_delivery`（纯函数，表驱动用例：渠道判别、config 里的 `chat_id` 优先、线程/引用零值），
  两次读库留在宿主 / 端到端用例那一侧（用例里就是**真库**那两次读）。
- **D4 没有 `regex` ⇒ 手写扫描器**：上游 `markdown.go` 用 7 条 `regexp`；本 crate 的依赖集在 M7-0 之后冻结
  （`docs/60` §2.2 不得新增三方依赖）⇒ 七条模式手写成扫描器，**非贪婪语义逐条复刻**（含"斜体那条模式的分组 1
  吃掉开星号前一个字符"这种会被写丢的细节），并用上游 `telegram_test.go` 的用例 + 边界用例（`****` / `*a**b*` /
  `a `` b` / 多字节）钉住。多字节上第一版真的踩过：`open + 3` 的字节下标落在 `中`（3 字节）中间 ⇒ panic。
- **D5 两处 message id 形态不同（逐字照上游）**：`Channel::send` 的 `SendResult` 是**复合键** `"chat:message"`
  （上游 `messageKey`），而 `channel_reply_delivery.message_id` 是**裸** platform id（`deliveryLease.messageID()`
  要把它 `ParseInt` 回 `int64`）。两条路径各有各的用例；**不要**把其中一处"统一"成另一处。
- **D6 `unknown` 那条写**不围栏在 owner 上（上游逐字）：持有者自己的请求挂死、租约过期之后，这条写仍然必须落地 ——
  否则后继者会读成"什么都没发过"并重发（那是重复投递的成因）。`mark_reply_delivery_send_unknown` 与
  `DeliveryLedger::record_send` 都按这条落，用例只断言"从 `in_flight` 才能转 `unknown`"。
- **D7 未接的字段 / 能力**：`send_state = 'in_flight'` 只在 claim 之后写（与上游同）；
  消息编辑的 `message_thread_id` 不在 `editMessageText` 的 wire 里（上游也没有）。
  `Capability::MESSAGE_EDIT` 的声明在 M7-5 就已给出，本片**补上了它的实现**（这才是那条位图成立的时刻）。
- **D8 单副本假设**在这里**不适用**：投递租约是**真库**的 CAS（`owner_token` / `owner_expires_at`），
  与 R-M7-1 的"无 Redis ⇒ 进程内租约"不同 —— 出站投递的跨副本一致性由 Postgres 承担，不需要 Redis 替身。
  本片因此没有可登记的 Redis 偏离。
- **D9 门 ⑩ 的两次拆分**：`crates/mc-repos/src/channel/delivery.rs`（内联用例外出）、
  `crates/mc-channel/src/telegram/outbound.rs`（首版 809 行 ⇒ 压到 788：删掉三个没被调用的门面函数 + 收文档）。
  `mc-http/tests/channels/telegram_round_trip.rs` 也按 `too_many_lines` 拆成三段函数（同一条链路的三段，不是三个用例）。
- **D10 429 的两条处置不同（逐字照上游）**：`sendMessage` 走 `send_message_with_retry_after`
  （**在同一处**按 Telegram 强制的退避睡一次再试）；`editMessageText` **不**在原地重试，而是回 `Step::RetryAfter`
  让调度器等（编辑跑在租约里，原地等会让调用越过租约）。两条都有用例（`sender/tests.rs` 的
  `a_rate_limit_is_retried_exactly_once`、`delivery/tests.rs` 的编辑阶梯表）。

### 18.3 门禁读数（本片当轮实测）

```
$ bash scripts/gates.sh --with-db
①fmt ②build ③clippy ④clippy-test-util ⑤test ⑥db ⑦route-parity ⑧schema-drift ⑨conformance ⑩file-size
```

```
$ bash scripts/gates.sh --with-db
①fmt ②build ③clippy ④clippy-test-util ⑤test ⑥db ⑦route-parity ⑧schema-drift ⑨conformance ⑩file-size
=> overall: PASS — 10/10 gate(s) green in 445s（本片树 @ 起手 base `a326ced7`，新文件已 `git add`）

# ⑥ 真库：migrate=0,e2e=0（新增：mc-repos 的 6 条投递状态机 PG 用例 + mc-http 的 1 条端到端回路）

# ⑦（0 路由 ⇒ 与起手预测**逐字相同**，不变式全部成立）
upstream 456 (commit f41fae6b08fb) | local 451 registered | baseline 406
  implemented  364 real +   4 placeholder =  368 / 456   known_gap   88   unclaimed    0   regression   0   local_only    9
  gaps by owner: M9=33  M3+=16  M7=16  M3=11  M8=6  M10=5  M2-A=1

# ⑦ 第二条（形态门）：M7 无 allowlist 退路，三类都是硬失败
$ python3 scripts/slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv
  declared 24 upstream key(s); dual-form required: 0 | single-form: 24
  shapes OK: every registered upstream key matches the form upstream serves
  => 0 defect(s) from findings, 0 warning(s)     # exit 0 ⇒ 未引入 MISSING_ALIAS / MISSING_EXACT / EXTRA_ALIAS

# ⑨（本片 0 路由 ⇒ **逐字不变**）
  pass 6  mismatch 23  unmounted 30  placeholder 0  unevaluable 306
  report matches crates/mc-conformance/report.json

# ⑩（新文件全部 ≤800；`scripts/file_size_baseline.tsv` **未动**）
$ python3 scripts/file_size_check.py --quiet   # exit 0
```

**rebase 到 `0ec41345` 之后**（交付树；在飞片 M2-A 尾-补已进场，它带 1 条路由）：同一套命令 **10/10 绿 / 499s**
（`migrate=0,e2e=0`），门 ⑦ 的差**全部来自那一片**，本片仍为 0 路由：

```
$ bash scripts/gates.sh --with-db            # rebase 后的交付树
upstream 456 (commit f41fae6b08fb) | local 452 registered | baseline 406
  implemented  365 real +   4 placeholder =  369 / 456   known_gap   87   unclaimed    0   regression   0   local_only    9
  gaps by owner: M9=33  M3+=16  M7=16  M3=11  M8=6  M10=5        # M2-A 线已收口，不再有 M2-A 行
=> overall: PASS — 10/10 gate(s) green in 499s
```

⇒ 本片**未刷基线**（`--write-baseline` 归 M7-21），本片自身的 `local / implemented / known_gap /
owners.M7` 增量为 **0**（451→452 那一条是 M2-A 尾-补的，与本片无关）—— 与派发描述里的预测一致。⚠️ 门 ⑩ 只扫 `git ls-files` ⇒ **新文件必须先 `git add`**
才受它约束（本片首次跑 ⑩ 时四个新模块还是 untracked，是"假绿"）。

### 18.4 交接（给 M7-21 / INT / 后续出站片）

1. **M7-21（`LUM-1786`）**：本片 **0 路由** ⇒ ⑦ 的 `local / implemented / known_gap / owners.M7` 全部**不动**
   （不变式 `implemented + known_gap == 456`、`regression == 0`）。请一并收口本片登记的三条缺口：
   **D2**（出站调度器的宿主接线，与 M7-5 的解析器面装配同一个落点 `apps/mc-server/src/channels.rs`）、
   **D3**（目标解析的两次读库）、以及 M7-5 留下的 **§17.4 第 1 条**（`register_with` 的宿主调用）。
2. **lark / dingtalk / wecom 的出站片可照抄的四件事**：`markdown.rs` 的"逐行 + 代码块保真 + 手写非贪婪"、
   `sender.rs` 的"分片 → 渲染 → 回落"三段、`delivery.rs` 的"**端口 + 真库 CAS + 三层守卫**"
   （`phase` / `attempt_depth` / `send_state`）、`outbound.rs` 的"**一步一回报**"（`Step` 三态）。
   四个平台的出站语义能共用后面两件，只有 wire 不同。
3. **写 `delivery` 面时先查这三件事**：(a) 取租约的 SQL 有**三条** `WHERE` 守卫（收口 / `phase` / 深度），
   少一条就会让迟到的旧尝试或流式帧重写用户正在读的内容；(b) `unknown` 那条写**不能**围栏在 owner 上；
   (c) 占位消息**不是进度** —— `message_id` 与 `chunks_sent` 是两个字段，混起来会静默截断回复。
4. **端到端回路的可复用件**：`crates/mc-http/tests/channels/telegram_round_trip.rs` 里的
   `BotState`（真方法名的 axum 替身 + 逐字段捕获）、`drive_inbound`（真 `TelegramChannel` + 真 engine `Router`
   + 五个真 PG 端口）、`drive_streaming_delivery`（真 `PgDeliveryStore` + `push_partial` / `deliver_answer`）。
   四平台的"每渠道至少一条真实收发回路"（`plan1.md` §5 W7）可以照这份骨架来。

### 18.5 lesson（本片新增）

- **【lesson·"上游一个文件 = 本仓一个文件"在出站面第一次失效】** `outbound.go` 1,633 行里真正**可判定**的部分
  （占位/编辑阶梯/分片/告知/目标解析）不到一半，其余是**事件总线驱动的基础设施**（worker 池、重试堆、清扫器、
  按 chat 的限速表）。本仓没有那条总线 ⇒ 照抄基础设施只会得到一堆没人调的代码。**判据**：先把上游按
  "状态机 / 调度基础设施 / wire"三分，只搬第一与第三类，第二类换成**一次一步的显式入口 + 三态回报**
  （`Step`），语义等价且可测（用例因此完全不睡真觉）。
- **【lesson·空值语义要在 wire 层断言"缺席"而不是"零"】** Telegram 的 `message_thread_id` / `reply_parameters`
  在**空**时的正确形态是**字段缺席**，不是 `0`。这一点只有帧级断言能钉住（`assert!(frame.get(…).is_none())`），
  Rust 侧的 `#[serde(skip_serializing_if = "…")]` 或 `if x != 0` 写错时，单元测试全绿而线上会被拒。
- **【lesson·真库用例要用"每次运行都不同"的路由键】** `channel_installation` 的唯一索引建在
  `(channel_type, config->>'app_id')` 上：写死 `123456` 的用例在上一轮**失败**（清场没跑到）之后会一直红，
  而报错是"duplicate key"，看起来像代码问题。**做法**：路由键取 run 级随机值（`fresh_bot_id()`），
  清场只当优化、不当正确性前提。
- **【lesson·"一条断言改了"要当成交付项写出来】** D1 第 3 条那条改动是本片唯一改 M7-5 用例的地方。
  它之所以必须改，是因为**替换点接线的时刻**本身就是形态变化的时刻：出站从"纯文本 + 裸 id"变成
  "HTML + 复合键"。写"将来会被替换"的代码时，用例应断言**语义**（"走发送器"），而不是断言**当年的形状**。
- **【lesson·`too_many_lines` 会挑出"剧本式用例"】** 端到端回路天生是线性的（造帧 → 入站 → 断言 → 出站 → 断言），
  pedantic 的 100 行上限把它挑了出来。**修法不是加 `#[allow]`**：把同一条链路的**段落**提成函数
  （`drive_inbound` / `drive_streaming_delivery`），用例本体只剩"接线 + 三处断言"，反而比原来更清楚。

## 19. M7-7（`LUM-1772`）：dingtalk 入站与 Stream 连接（0 路由）

**口径**：本节一切落点与偏离都在上游 `f41fae6b08fb` 与本片当轮 base 上核对。本片**起手 base**
是 `6ae471fe6272a8ce5b335aa74d049efb3c3aad26`（= 合并 PR #94 / M7-6 之后
`origin/feat/multica-rs-initial` 的 tip；派发描述的起手补充与 `git fetch` 后的实测一致）。
提交时 origin tip 已前进到 **`c16e00a6`**（§101 cycle 报告 + 合并面，**docs-only**）⇒ 与本片
24 个改动文件**零交集**；本片**未 rebase**（无必要），PR 以 `6ae471fe` 为 base。

### 19.1 落点（逐字路径）

| 落点 | 写者 | 上游 |
| --- | :-: | --- |
| `crates/mc-channel/src/dingtalk/mod.rs` | M7-0 建 / **M7-7 填** | `dingtalk_channel.go` 的连接那一半（`Connect` / 工厂 / 注册面） |
| `crates/mc-channel/src/dingtalk/jobs.rs`（新） | M7-7 | `dingtalk_channel.go` 的 `onMessage`（前置判决）+ `runInbound`（作业体） |
| `crates/mc-channel/src/dingtalk/stream.rs` + `stream/{endpoint,tungstenite}.rs` + `stream/tests.rs` | M7-7 | `ws_frame.go`（67）+ `ws_endpoint.go`（105）+ `ws_connector.go`（226） |
| `crates/mc-channel/src/dingtalk/inbound.rs` + `inbound/{card,quoted,mention}.rs` + `inbound/{tests,card/tests,quoted/tests}.rs` + `testdata/*.json` | M7-7 | `inbound.go`（827）+ `inbound_card.go`（108）+ 上游 `testdata/` 四份 golden |
| `crates/mc-channel/src/dingtalk/dispatch.rs` + `dispatch/tests.rs` | M7-7 | `dispatch.go`（202）+ `dingtalk_channel.go` 的 `dispatchSlot` 一族 |
| `crates/mc-channel/src/dingtalk/emotion.rs` + `emotion/tests.rs` | M7-7 | `emotion.go`（67）的平台契约那一半 |
| `crates/mc-channel/src/dingtalk/resolvers.rs` + `resolvers/tests.rs` | M7-7 | `resolvers.go`（533）的**判决**那一半 |
| `crates/mc-channel/src/dingtalk/tests.rs`（新） | M7-7 | 本文件自己的用例（工厂 / 生命周期 / 入站闭环） |

### 19.2 偏离（D1…D14，逐条可核对）

- **D1 写集勘误（三类）**：

  1. **`mod.rs` 本身**（起手补充第二类漏项，第 4 次）：不写进去五个新文件根本不参与编译；
  2. **新增路径**（`jobs.rs` / `stream/{endpoint,tungstenite}.rs` / `inbound/{card,quoted,mention}.rs` /
     九个 `*/tests.rs` / `testdata/*.json`）：**全部**是门 ⑩ 的 800 行硬限逼出来的切分，不是风格选择。
     切分边界都取上游自己的文件边界（`ws_endpoint.go` / `inbound_card.go` / `dispatch.go` 的槽那一族）
     或"连接怎么跑 / 一条回调怎么入库"这种单一接缝。首次成形时 `inbound.rs` 877 / `mod.rs` 896 /
     `stream.rs` 952 三处越限，拆完 **775 / 725 / 637**，全部 ≤800；**未动** `scripts/file_size_baseline.tsv`；
  3. **四份上游 golden 用 `include_str!` 嵌入**（`src/dingtalk/testdata/`）：编译期进二进制 ⇒
     用例不依赖运行目录（`git ls-files` 会收它们，门 ⑩ 只扫 `.rs`，不受影响）。
- **D2 出站位**失败关闭（实现归 **M7-8**）：本片**不**交"发不出去却自称 `TEXT`"的半成品，
  `Channel::send` 明说未接线（同 M7-3 → M7-4 的先例）。`Capability::TEXT | ATTACHMENT` 照上游声明，
  `ATTACHMENT` 的实现时刻在 M7-8。
- **D3 生命周期停机的判决换成 `Drop`**：上游在 `Connect` 的 `defer` 里记
  `stopDispatch = ctx.Err() != nil`（supervisor 取消子 ctx = 生命周期停机；自己返回 = 重连）。
  本仓的取消是 supervisor 的 `select!` **丢弃** `connect` 的 future（`engine/supervisor.rs:652`）
  ⇒ 同一个判决由 `RelinquishGuard` 在 `Drop` 里给出（正常返回时 `defuse`）。语义逐条对应，
  且两边都有用例（`a_gateway_redial_keeps_the_queue_alive` / `a_dropped_connect_future_is_a_lifecycle_stop`）。
  实测：`tokio::spawn(connect).abort()` 之后 `relinquished() == true`。
- **D4 队列跨重连复用**：上游 `dispatchSlot` + `dispatchSlotRegistry` 一族落在本片的 `dispatch.rs`
  （按 AppKey 一格；**已收口的槽不再复用**；`release` 按 `Arc` 比较，重连期重叠的旧代不得删掉
  已被换代接管的那一格）。三条都有用例。
- **D5 取消**不穿透工作业签名：上游把 `d.ctx` 派生的 ctx 交给 handler，硬取消时在飞作业**立刻**
  看到取消；本仓的作业签名（engine 的 `InboundHandler`）没有 ctx ⇒ 硬取消只丢**排队中**的，
  在飞作业跑完自己那一次（最长 `job_timeout` 120s）。收口预算取 **4s**（小于 supervisor 的
  `disconnect_timeout = 5s`）⇒ 收口先结束，supervisor 的超时不会先响。
- **D6 绑定行 `config` 列不写**：上游把 `{conversation_type, conversation_id, staff_id}` 写进
  `channel_chat_session_binding.config`；本仓的通用 binder 写 `null`（M7-2 的形态，动它要改
  M7-5/M7-6 的已合用例）。本片把**上游自己的兜底**（按 `channel_chat_id` 寻址）显式落成
  `outbound_target` 纯函数 ⇒ M7-8 拿到的是同一份判决，而不是"读到一个 null 就崩"。
- **D7 群清单 / bot 身份只落接缝**：上游 `groupPresenceObserver` 写 `dingtalk_bot_identity` /
  `dingtalk_group_presence` / `dingtalk_group_activity`（并调 M7-9 的 `BotNameResolver` 解析可读 bot 名）。
  三张表的**写语句**要落在 `crates/mc-repos/**`（**不在本片写集**）⇒ 本片落 `GroupPresenceObserver`
  接缝 + 诚实默认值 `NoGroupPresence`（什么都不写、不报错）。观察是**尽力而为**的：失败只记 warn，
  绝不改判决（上游逐字）—— 一条用例专门钉这条。
- **D8 bot 名来源是同步接缝**：上游 `BotNameResolver.Resolve` 是 async 且要打平台 API + 权限缓存。
  本片只定形状（`BotNameSource::bot_name(&self, app_key, conversation_id) -> Option<String>` +
  `NoBotName` = **失败关闭**：拿不到名字就一个提及都不剥）。实现归 M7-9 的 `bot_identity.go` 面。
- **D9 读截止 / 单所有者写 / ping 事件**：上游 `SetReadDeadline` 每次读之前设一次、pong 处理器再设一次；
  本仓把读做成一次 `timeout`，靠把 pong **显式上抛**（`WsEvent::Pong`）实现"每个事件刷新一次截止"
  （有空闲但在心跳的链路不超时）。ping 与读放进同一个 `select!`（一个连接一个所有者）⇒ 端口可以是
  `&mut self`，用例的替身也因此简单。`Ping` 事件不显式回写：`tokio-tungstenite` 自己会回
  （其文档逐字："the library will automatically reply to Ping frames with Pong frames"）。
- **D10 引导/拨号的错误形态**：上游把响应体原样拼进引导错误；本仓**只带状态码**（网关的错误体可能
  回声请求，而请求里就带 `AppSecret`）。拨号错误同样**不**原样透出（`connect_async` 的错里带完整
  URL，而那条 URL 自带一次性 Stream ticket）。两条各有一个反例用例（loopback 服务端的 401 体里
  就放着那个 secret；拨号失败的断言的则是"错误里没有 ticket"）。
- **D11 没有 `regex` ⇒ `webURLStart` 手写**：上游那条 `(?i)(?:https?://|www\.)` 在冻结依赖集的
  前提下写成"取最左匹配"的扫描器（`to_ascii_lowercase` 逐字节等长 ⇒ 下标可直接用于原串）。
  `web_url_spans` 是**跨片共享件**：M7-8 的 `markdown.rs` 要用它（上游放在 `markdown.go`）⇒
  请**复用**而不是再写一份（见 19.4）。
- **D12 凭据形态**：`StreamInstallConfig` 多一个**明文** `app_secret` 列（只给本地 / 用例；生产走
  `app_secret_encrypted`，且**必须有解密器**才装配）。`AppSecret` / `Decrypter` 是本 crate 的
  **第三份**同形件（`slack::config::Sensitive` / `telegram::config::Sensitive`）；收敛不在本片写集
  （要动 M7-3/4/5 的已合文件）⇒ 与 M7-9 的 `config.rs` 合并时一并处理。三个承载凭据的类型
  （`AppSecret` / `OpenConnectionRequest` / `StreamInstallConfig`）都**手写 `Debug`** + 三条
  "错误路径不回显凭据"用例。
- **D13 `/issue` 派发失败告知归 M7-8**：上游 `notifyIssueDispatchError` 要用 `sender` /
  `targetFromMessage` / `isAddressedIssueCommand`（全在 `outbound_send.go` / `reply_source.go`）⇒
  本片的作业体在 handler 失败时只记 warn，那条**用户可见的告知**由 M7-8 的回复器接管（登记，不静默）。
- **D14 本片**不**碰 `mc-repos/**`**：没有新 SQL、没有 schema 变更、没有新迁移（⑥ 门的读数与本片
  无关的部分照旧）。`dingtalk_*` 三张表的建表在前（`docs/60` §6.4），写入见 D7/D8。

### 19.3 门禁读数（本片当轮实测）

```
$ CARGO_INCREMENTAL=0 MULTICA_TEST_DATABASE_URL=<local-pg> bash scripts/gates.sh --with-db
①fmt ②build ③clippy ④clippy-test-util ⑤test ⑥db ⑦route-parity ⑧schema-drift ⑨conformance ⑩file-size
=> overall: PASS — 10/10 gate(s) green in 433s
   （⑥ 的读数：migrate=0,e2e=0；真库角色必须带 CREATEDB，否则 ⑧ 的探针库建不出来 ⇒ exit 2）

# ⑦（本片 0 路由 ⇒ 与起手预测**逐字相同**，不变式全部成立）
upstream 456 (commit f41fae6b08fb) | local 452 registered | baseline 406
  implemented  365 real +   4 placeholder =  369 / 456   known_gap   87   unclaimed    0   regression   0   local_only    9
  gaps by owner: M9=33  M3+=16  M7=16  M3=11  M8=6  M10=5

# ⑦ 第二条（形态门）：M7 无 allowlist 退路，三类都是硬失败
$ python3 scripts/slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv
  declared 24 upstream key(s); dual-form required: 0 | single-form: 24
  shapes OK: every registered upstream key matches the form upstream serves
  => 0 defect(s) from findings, 0 warning(s)      # exit 0 ⇒ 未引入 MISSING_ALIAS / MISSING_EXACT / EXTRA_ALIAS

# ⑨（本片 0 路由 ⇒ **逐字不变**）
  pass 6  mismatch 23  unmounted 30  placeholder 0  unevaluable 306
  report matches crates/mc-conformance/report.json

# 本片相关的 12 条 fixture（§6.2 点名的那个集合）
  12 条 = pass 1 + unmounted 7 + unevaluable 4      # 本片**未**增删任何一条

# ⑩（新文件全部 ≤800；`scripts/file_size_baseline.tsv` **未动**）
$ python3 scripts/file_size_check.py --quiet   # exit 0

# 单元测试：`cargo test -p mc-channel` ⇒ 471 passed（其中本片 95 条）
```

⚠️ **门 ⑩ 只扫 `git ls-files`** ⇒ 新文件必须**先 `git add`** 才受它约束（本片首次跑 ⑩ 时四个
新模块还是 untracked，是"假绿"）。

⚠️ **磁盘（本轮的真实事故，登记给后续片）**：本片与另一片（`lum-1801`）同时在跑，`/` 49G 被两个
`target/` 吃满（各 16–21G），`--with-db` 的第一次与第二次尝试都撞到 **ENOSPC**：③④ 与 ⑨ 报
`failed to create directory … target/debug/.fingerprint/…`，⑥ 报
`ld terminated with signal 7 [Bus error]`（链接器写不出输出），⑧ 报
`could not create directory "base/…": No space left on device`。**这些都不是代码红**。
自救三步：`rm -rf target/debug/incremental`（6.8G）、`rm -rf target/debug/deps/mc_conformance-* target/debug/deps/rustc*`、
以及给 cargo 加 `CARGO_INCREMENTAL=0`（不再生成 incremental）⇒ 第三次跑 **10/10 / 433s**。
判据留给下一片：**`df` 低于 ~8G 时别开 `--with-db`**（`--all-targets` 全量重建 + `-p mc-http
--features mc-http/test-util` 的第二份测试二进制各 ~7G）。

### 19.4 交接（给 M7-8 / M7-9 / M7-21 / 宿主）

1. **M7-8（dingtalk 出站/媒体/回复）**：
   (a) `Channel::send` 是**单点替换**（照 M7-6 的 `telegram/mod.rs` 先例：一个委托，不动别处）；
   (b) **复用** `inbound::card::web_url_spans` 与 `inbound::card::readable_quoted_text`（上游的
   `markdown.go` 要用前者，别再写一份扫描器）；
   (c) `emotion.rs` 的 `EmotionTransport` 端口是你的 `sender` 要实现的那一半（校验与 401 重试
   已经在 `set_emoji_reaction` 里，别重复实现）；
   (d) `resolvers::outbound_target` 是出站寻址的**唯一**判决点（含 D6 的兜底）；
   (e) 队列的 `drain_and_close` 是收口口（D3/D5）。
2. **M7-9（安装/凭据/群身份 + 7 条路由）**：
   (a) `config.rs` / `token.go` 接管 `StreamInstallConfig` 与 `Decrypter`（D12 的收敛一并做）；
   (b) `BotNameSource` / `GroupPresenceObserver` 的两个诚实默认值（`NoBotName` /
   `NoGroupPresence`）换成真实现 ⇒ 群里的 `@bot` 提及剥离与群清单写入才真正生效；
   (c) `group-routes` **必须保持 404**（`docs/60` §1.6）；
   (d) 本片在 `dingtalk/mod.rs` 追加的 `pub mod` 行与 `register_resolvers` 是接线点（后合者 rebase）。
3. **M7-21（`LUM-1786`）**：本片 **0 路由** ⇒ ⑦ 的 `local / implemented / known_gap / owners.M7`
   全部**不动**（不变式 `implemented + known_gap == 456`、`regression == 0`）；本片**未刷基线**
   （`--write-baseline` 归它）。请一并收口本片登记的缺口：D2（出站）、D7/D8（群清单 / bot 名来源）、
   D13（`/issue` 派发失败告知）、D14（`dingtalk_*` 写语句）。
4. **宿主（`apps/mc-server/src/channels.rs`，anchor 写集）**：`register_with(&registry, &DingTalkDeps)`
   （部署密钥 + `NoBotName` 的替换）与 `Dispatcher::drain_and_close` 的停机调用 —— 与 M7-5/M7-6 的
   两处装配缺口同一个落点。**停机顺序照旧**：先停渠道连接 → 再停调度器 → 最后停 actor。

### 19.5 本片的 lesson（写给后续的渠道片）

- **【lesson·"上游一个文件 = 本仓一个文件"在**连接面**同样会失效】** `ws_connector.go` 226 行里
  真正属于本仓的是"帧循环 + 三个收尾判决"；`gorilla` 的并发写、`WriteControl` 的语义、
  ctx 取消都**没有**对应物 ⇒ 换成"单所有者 `select!` + `WsEvent` 上抛 + `Drop` 判决"。
  判据：先问"上游这一行依赖的是**库**还是**平台**"；依赖库的那部分要按本仓的运行时重写，
  而不是照抄。
- **【lesson·`watch::Sender::send` 在没有接收者时会**不更新值**】** `Dispatcher` 的四个 `watch`
  通道（closed / cancelled / done / workers）第一版全被试出来：`send` 返回 `Err` 且值不变 ⇒
  "先置位、后来者才订阅"读到旧值（收口判决失效、worker 计数停在 0、`drain_and_close` 立刻宣布
  完成）。修法是**四条通道各留一个常驻接收者**（`channel_keepers`）。同一形状的第二处：
  "计数在起任务**之前**加"（否则收口会把刚落地的 enqueue 读成"没有在跑的"）。
- **【lesson·替身必须保真到"取消安全"】** 脚本化 socket 第一版是"pop 再 sleep"：帧循环在 ping
  那一支赢下 `select!` 时会**丢弃**正在等的读 future（真 socket 的 `poll_next` 是取消安全的），
  于是那个替身把事件提前吃掉、用例测出了一个真实实现不会有的行为（"socket closed"）。
  修法：事件带一个**只定一次**的截止时间，取消后下次继续等同一个时刻。
- **【lesson·`AppKey` / `AppSecret` 这类混排词必须进反引号】** pedantic 的 `doc_markdown` 会在
  新文件里逐个挑出来（本片 9 处）；同批还有 `doc_lazy_continuation`（列表项续行要缩进）。

> **号段勘误（cycle 合并 #95 时裁定）**：本节原取 `## 19.`，与同轮在飞的 M7-7（`LUM-1772`，PR #96）撞号；M7-7 的 `mod.rs` 头部已引用 `docs/32 §19`，故本节让号改为 `## 20.`（编号 `19.x → 20.x`），D 编号与内容一字未动。

## 20. M8-4（`LUM-1801`）：GitHub 入站 webhook + PR 镜像 + 自动关联/关闭 + issue↔PR 读面（2 路由）

> **号段说明**：计划书写的「`docs/32` §9.12」是计划期占位号（`## 9.` 是 M6-0 anchor、
> `## 11.` 是 M8-0）。**起手**实测最大 = `## 17.`（M7-5）⇒ 本片原取 **`## 18.`**；
> 但 `## 18.` 在本片在飞期间被 **M7-6（`LUM-1771`，已先合入 base）** 占用 ⇒
> 合并时**让号**改为 **`## 19.`**（M8-4 的 D 编号与内容不变，只有节号平移）。
> `## 11.` 是 M8-0）⇒ 按派发时「起手复核 `N+1`」的约定取 **`## 18.`**（起手实测最大 =
> `## 17.` = M7-5）。

### 20.1 落点（写集逐字）

| 文件 | 行数 | 内容 |
| --- | --: | --- |
| `crates/mc-vcs-github/src/payload.rs` | 510 | anchor 建桩、本片原地填充：`GithubEventKind`（三族 + **`Ping`**）+ 三个 wire 结构（`installation` / `pull_request` / CI）+ `coalesce` / `str_ptr_or_nil` / `parse_gh_time{,_required}` + 5 条单测 |
| `crates/mc-vcs-github/src/webhook.rs` | 172 | 原地上填：`verify_webhook_signature`（`hmac`/`sha2`/`hex` 直连边复刻上游三语义）+ `sign_webhook_body`（替身发帧用）+ `event_kind_for_request` + 4 条单测 |
| `crates/mc-vcs-github/src/links.rs` | 474 | 原地上填：`extract_identifiers` / `extract_closing_identifiers` / `issue_number_for_prefix` —— **手写扫描器**逐字复刻上游两条正则（理由与两条 Go/Rust 语义差见模块头）+ 14 条单测（`DoD` 点名的 **6 个边界**逐条） |
| `crates/mc-vcs-github/src/closepolicy.rs` | 241 | 原地上填：`CloseIntent` 三态 + `close_intent_policy` + `preserve_close_intent` + `CloseIntentPolicy`（`unrestricted`/`owner` + `permits` + `from_resolvers` + `for_bindings`）+ 6 条单测 |
| `crates/mc-vcs-github/src/mirror.rs` | 528 | **形状修订**（见 D5）：`MirrorRequest` 保留、新增 `identifiers` / `derived_state` / `mergeable_write` / `refresh_request` / `plan` → `MirrorPlan`；`derive_pr_state` / `derive_pr_mergeable_state` / `base_ref_changed` 逐字对齐上游 + 11 条单测 |
| `crates/mc-repos/src/github/check_suite.rs` | 273 | 原地上填：`GithubCheckSuiteRepo` 两条查询（`list_pr_numbers_by_head_sha` 上游逐字 SQL；`list_workspace_pr_numbers` 按载荷直接给的号）+ 1 条真库单测 |
| `crates/mc-repos/src/github/pending.rs` | 50 | **doc-only 结论**（见 D7）：实测上游 `f41fae6b` 上 `github_pending_check_suite` **零写者零读者**（Plan C 取代了迁移 `096` 描述的暂存-回放）⇒ 本波**不**造无调用方的仓储面 |
| `crates/mc-http/src/routes/github/webhook.rs` | 239 | 入口与装配：`router()`（1 个**新增**注册键）+ 端口注入槽 + `handle_github_webhook`（验签/分派/状态码）+ `flat_error` + 3 条单测 |
| `crates/mc-http/src/routes/github/webhook/installations.rs` | 188 | `installation` 族处理 + 3 条按 `installation_id` 的查询 |
| `crates/mc-http/src/routes/github/webhook/mirror.rs` | 567 | `pull_request` 族：扇出 + 投递级关闭裁决 + 逐 workspace 镜像 + 自动推进 + 跨 provider 关闭聚合 + 3 条共享读 + 2 条单测 |
| `crates/mc-http/src/routes/github/webhook/ci.rs` | 111 | 三族 CI 事件（纯触发器 → 入队） |
| `crates/mc-http/src/routes/github/issue_pr.rs` | 542 | 原地上填：`GET /api/issues/:id/pull-requests` 真实现（替换 501 占位）+ `PullRequestCard`（上游 31 字段形状）+ 3 条单测 |
| `crates/mc-http/tests/github_webhook/{main,support,webhook,ci,issue_pr}.rs` | 26 / 372 / 709 / 212 / 282 | **新测试目标**：14 条真库 e2e（三族事件 + 幂等 + 两个反例 + 端到端四条链 + 读面/越权/VCS 合流） |

**零编辑（anchor 冻结，逐条实测未动）**：`crates/mc-http/src/state.rs`、`routes/mod.rs`、`routes/mount.rs`、
`routes/github/mod.rs`、`routes/issues/**`、三个 `Cargo.toml`、`Cargo.lock`、`docs/61-M8-PLAN.md`、
`docs/fixtures/**`、`scripts/file_size_baseline.tsv`。
**只读面零触碰**：`mc-vcs-github/src/{app,token_cache,rest,dto}.rs`、`ghsnapshot/**`、`port.rs`、
`mc-repos/src/github/{installation,pull_request}.rs`、`mc-repos/src/vcs/pull_request.rs`、
`routes/github/{dto,install,setup}.rs`。

**写集追加一个目录**：`crates/mc-http/tests/github_webhook/**`（`DoD` 要求真库端到端证据；与 M8-1 的
`tests/github/`、M8-2 的 `tests/vcs/`、M8-3 的 `tests/mcp/` 同款 —— 新路径、与任何片零交集）。
**另加一个子目录**：`crates/mc-http/src/routes/github/webhook/`（三个子模块）。这不是新写者：这三个
文件只因 `webhook.rs` 的 `mod` 声明而存在，写者仍是本片；起因是 `webhook.rs` 单文件到了 **981 行**
（门 ⑩ 的 800 硬上限），按 `channels/slack.rs` + `channels/slack/store.rs` 的既有手法拆
（`webhook.rs` 里 `mod ci; mod installations; mod mirror;` ⇒ `routes/github/webhook/*.rs`）。

### 20.2 偏离登记（M8-4-D1 … M8-4-D10）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **D1** | body 超限 ⇒ **413 `payload too large`**（上游 `io.LimitReader` 是**静默截断**，截断后验签必然失败 ⇒ 上游实际回 401） | `routes/github/webhook.rs` 的 `GITHUB_WEBHOOK_MAX_BODY_BYTES` + 抽取器 | 与 M8-2 的 `vcs/webhook.rs` **同判同措辞**（那一片已经登记过同一处）。理由：`Bytes` 抽取器给不出「截断后的前缀」，而 413 比「看起来像签名不匹配」更容易被 provider 的投递 UI 解释清楚。**不是**语义宽松：两条路径都拒绝处理 |
| **D2** | 快照刷新端口落在**本片自己的文件**里的进程级注入槽（`set_pr_refresh_port` / `pr_refresh_port`，默认 `DisabledPrRefresh`），而不是 `AppState` 字段 | `routes/github/webhook.rs` | **本片发现的 anchor 缺口**：上游 `h.PRRefresh` 是 Handler 字段，本仓 `docs/61` §5 的 `state.rs` 行只加了 vcs/github/composio 三组密钥 ⇒ 请求面**没有**端口来源，而 `state.rs` 是 anchor 冻结文件。写法与纪律逐条复刻 M8-1 的 `GITHUB_API_BASE`（同款进程级可变状态、同款「生产装配点将来改成 state 字段」的注记、测试注入记录型端口）。**建议 M8-7/M8-5**：把端口正式挂到 `AppState`（需要一次 anchor 级改动），本槽降级为纯测试接缝 |
| **D3** | 6 条查询落在**本片自己的文件**里（`webhook/installations.rs` 3 条按 `installation_id`，`webhook/mirror.rs` 3 条：workspace settings / issue 存在性+整行 / 跨 provider 关闭聚合），而不是 `mc-repos` | 同上 | `docs/61` §3.3 逐字规定 M8-4 对 `mc-repos/src/github/{installation,pull_request}.rs` 是**只读**（那是 M8-1 的写集），而 `mc-repos/src/github/mod.rs` 是 anchor 冻结（加不了新模块）⇒ 本片所需的「按 installation_id 列出/删除/刷新」与「`GetIssueCombinedPullRequestCloseAggregate`」只能落在自己的文件里。M8-2 的 `vcs/pull_request.rs` 模块头已经把后者的落点判给 M8-4（「落点应由关闭策略的写者（M8-4）决定」）⇒ 本片按写集纪律选择了 HTTP 层。**建议 M8-7**：若要归位，统一挪进 `mc-repos` 并同步改本表 |
| **D4** | 三处**逐字对齐之外**的差异，每条都写进了对应函数的文档注释：① 入队**每个绑定一次**（上游一次，端口键含 `workspace_id`）；② CI 事件先查库把号映射回 `(workspace, 号)` 再入队（上游直接入队、未镜像的 PR 也入队；本仓未镜像的 PR 本来也没有可写快照的行）；③ CI 的 `status` 事件入队载荷**带上**解析出的 `head_sha`（端口有 `Option<sha>` 字段，上游没有） | `webhook/{mirror,ci}.rs` | 三条都是**端口形状**（anchor 冻结）与上游 Handler 形状的差，**不是**业务判定差。取「更严/更可观测」的那一侧并逐条登记（D4 一律是「窄化」：只会更少入队，不会更多） |
| **D5** | `mc-vcs-github/src/mirror.rs` **不从 `MirrorRequest` 直接做 I/O**：`pub async fn mirror_pull_request(..) -> Result<(), GithubError>` 改成纯函数 `plan(&self, resolved, auto_link, prefix) -> MirrorPlan`，I/O 编排留在 HTTP 层 | `mirror.rs` / `webhook/mirror.rs` | anchor 的签名要求本 crate 能持有 `Db`/仓储/`state.realtime`，但本 crate 的依赖边**没有** `mc-db`/`sqlx`/`mc-realtime`（`Cargo.toml` 注释逐字「此后 M8-1/4/5 的写者不得再新增三方依赖」）⇒ 只能这样切，而这样切还有个好处：**决策核变成可纯函数逐条钉住的**（11 条单测覆盖 claim/裸提及/掉链/冻结/不放行/自动关联关）。与 M8-1 把广播留在 HTTP 层同判例 |
| **D6** | 不接线 `StopClosedIssueWakeups` 与 `notifyParentOfChildDone`（上游 `advanceIssueToDone` 里的两段） | `webhook/mirror.rs::advance_issue_to_done` | 它们在 M5（wakeup 停机）与 M2（父 issue 完成通知）的模块里，接线需要一次跨波改动；本片只做「status → done + `issue:updated` 广播」。**登记为缺口**：合并的 PR 把 issue 推到 done 时，`agent_task_queue` 里为该 issue 排的 wakeup **不会**被取消，父 issue 也**不会**收到子完成通知 —— 两条都是可见行为，归 **M8-INT**（`LUM-1804`）或 M5/M2 的尾账 |
| **D7** | `mc-repos/src/github/pending.rs` 保持 **doc-only**（本波零实现），并把「为什么无事可做」写成三条实测判据 | `pending.rs` | anchor 的文件头假定 `check_suite` 事件会**写** check 行、乱序时先暂存；在钉住的上游 `f41fae6b` 上这假定**不成立**：三族 CI 事件一律走 `triggerPRRefreshFromCIEvent`（Plan C：「payload is never read for display」），而 `github_pull_request_check_{suite,run}` 的唯一写者是 `github_snapshot.sql.go`（**M8-5** 的 snapshot 管道）。若本片按 anchor 描述落 suite/check_run 的写面，就会成为与 M8-5 争同一批表的**第二个写者** |
| **D8** | `docs/61` §6.5 M8-4 行点名「`extractIdentifiers`/`extractClosingIdentifiers` 的 **6 个边界**」在计划书里没有逐个列出 ⇒ 本片把「大小写 / `#` 前缀 / 跨行 / 代码块内 / `owner/repo#n` / 去重」六个写进 `links.rs` 的模块文档与用例名，作为**可复核的口径固定** | `links.rs` | 上游**不**做 Markdown 解析（代码块里的标识符照样命中）是刻意的逐字对齐，这一条最容易被「顺手加个 fenced-code 过滤」改坏 ⇒ 单独一条用例 + 一条反例钉住 |
| **D9** | `link_issue` 的 `linked_by_type` 写 **`"system"`**（上游 `LinkIssueToPullRequest` 的 `strToText("system")`），而非本仓其它路径惯用的 `"webhook"` | `webhook/mirror.rs::apply_auto_link` | 逐字对齐上游 GitHub webhook 路径。**注意** M8-1 的 repo 单测用的是 `"webhook"` —— 那是**测试自己的取值**，不是生产口径；生产口径只有上游这一个 |
| **D10** | **写集之外**唯一一处改动：`crates/mc-http/tests/issues/auth.rs` 的 501 占位断言重新指向（`pull-requests` → `attachments`，+5 行注释） | `tests/issues/auth.rs:121-140` | 这条断言的**既有惯例**就是「不管哪个切片实现掉当前那条占位，就由它把断言指向下一条最远的缺口」（原注释逐字：「这条断言换过三次落点」，并逐次列出了前三片）。本片把 `GET /api/issues/:id/pull-requests` 从 501 换成真实现（`docs/61` §6.1 预测的 `implemented_placeholder 4→3` 正是这一条）⇒ 断言必然变红（门 ⑥ 实测 `left: 404, right: 501`），**不修就是留一条已知错误的测试**。取 `attachments` 的理由：`docs/61` §9.2 把它判给 `M3+` 并作为 W8 尾账登记 ⇒ 比其它候选耐久。**本片之外零改动**（`git diff` 只有这 13 + 3 个文件） |

### 20.3 专属 DoD 逐条证据（`docs/61` §6.5 的 M8-4 行）

| DoD 条目 | 证据 |
| --- | --- |
| **离线替身端到端**：断言链 = webhook → PR 行 → issue 关联 → 自动关闭 → **快照入队**，中间零 mock | `tests/github_webhook/webhook.rs::pull_request_webhook_drives_the_whole_chain` —— 一帧带**真实 HMAC-SHA256 头**的载荷（`sign_webhook_body` 与被测代码共用同一份 HMAC）走完五环：① `github_pull_request` 行（`state='merged'` / `head_sha` / 统计 / 作者）；② 两行 `issue_pull_request`（`close_intent` **一真一假**）；③ `#8` 被推进到 `done` 而 `#7` 留在 `todo`（逐字验证闸门第 ③ 条）；④ 记录型端口收到 `reason=Webhook` + `head_sha` 的入队；⑤ `pull_request:updated` 与 `issue:updated` 两条广播。**这条用例同时也是「离线替身」的形态说明**：本面的方向是「GitHub 打我们」⇒ 替身是**发帧的那一侧**，不需要本地 HTTP 服务端 |
| **三族事件各一条** | `installation`：`installation_deleted_drops_every_binding_and_broadcasts_per_workspace` + `installation_created_stores_pending_then_refreshes_the_bound_account`（含 `deleted` / `suspend` / `created` / `new_permissions_accepted` / 未建模 action 五种 action）；`pull_request`：上面的端到端 + `redelivering…` + `a_passing_mention…` + `ambiguous…`；`check_suite`：`ci.rs` 的两条（`check_suite` 直给号 + `status` 走 SHA 回查 + `check_run` 同族同路） |
| **幂等**：同一 webhook 重投 2 次**只插 1 行** PR | `redelivering_the_same_webhook_inserts_one_pr_row_and_keeps_one_link`：两帧同载荷 ⇒ `pr_count == 1`、关联账 `vec![(issue, false)]` 恰好一行、两帧各入队一次（幂等在**行**上，不在入队上 —— 上游同判） |
| **两个反例**：验签失败 ⇒ 401 且**不落库**；缺密钥 ⇒ 拒收 | `bad_signature_is_401_and_persists_nothing`（错密钥 + 无签名头两种 ⇒ 401 + `pr_count == 0` + 零关联账）；`missing_webhook_secret_is_404_rather_than_a_permissive_accept`（`None` 与 `""` 两种 ⇒ 404 `not found`，且**带合法签名也不放行**） |
| **自动关联 identifier 的 6 个边界** | `links.rs` 的 14 条单测逐条：`boundary_case_is_folded_to_uppercase`（大小写）/ `boundary_hash_prefix_matches`（`#` 前缀）/ `boundary_cross_line_matches`（跨行）/ `boundary_inside_code_block_still_matches`（代码块内**仍命中**）/ `boundary_owner_repo_hash_form_matches`（`owner/repo#n`）/ `boundary_dedup_keeps_first_occurrence_order_across_parts`（去重保序），外加 `prefix_is_capped_at_ten_characters`、`identifier_needs_a_non_word_left_edge_and_a_digit_right_edge`、`issue_number_for_prefix_boundaries` |
| **自动关闭的三态**（`closeIntentPolicy.permits`） | `closepolicy.rs`：`CloseIntent` 三态（`three_state_close_intent`）+ `preserve_close_intent`（`preserve_close_intent_freezes_after_terminal_events`）+ `permits` 的四条（零值全拒 / 单绑定无限制 / 只放行记录的 owner / 歧义不放行）。真库侧：`ambiguous_closing_identifier_is_withheld_across_bound_workspaces` —— 两个 workspace（**同前缀**、同 installation）都能解析 `Closes <P>-7` ⇒ 两边都**照建关联账但不带 `close_intent`**，于是两边都**不推进**（`#6804` 的 fail-closed） |
| **`pull-requests` 从 501 → 真实现且路由仍在** | `issue_pr.rs::read_face_returns_the_linked_card_and_triggers_a_page_view_refresh` 第一条断言就是 `assert_ne!(status, StatusCode::NOT_IMPLEMENTED)`；⑦ 读数同时给出双向证据：`local 452→453`（新增 webhook 键）而 `implemented_placeholder 4→3`（占位换真实现） |
| **公开路由不得挂会话 middleware / 不得因缺会话 401** | 本面两族用例的帧**从不带** `x-multica-user-id`（`signed_webhook_request` 只放 `x-github-event` 与签名头）—— 全部 202/200/401/404 都在**无会话**下取得；401 只由**验签失败**触发 |
| ⑩：新文件 ≤800 行 | 见 18.4；`scripts/file_size_baseline.tsv` **未动** |
| 凭据面：手写 `Debug` 脱敏 + 「错误路径不回显凭据」 | 本片**不新增**凭据类型（App 私钥 / installation token 在 M8-1 的 `app.rs` / `token_cache.rs`，本片只读其不透明接口）。本面对应的一条是**广播不得回显管理手柄**：`installation_deleted…` 断言广播载荷里不出现数字 `installation_id`（只出现内部行 id），`installation_id` 本身是 GitHub 的公开数字标识、但对非 admin 成员是管理手柄（上游逐字口径） |

### 20.4 门禁读数（逐字取自当轮日志；日志留档在 run workdir 的 `gates-m8-4b.log`）

```
bash scripts/gates.sh --with-db --db-url 'postgres://mc_lum1801:…@127.0.0.1:5432/multica_lum1801'
  ①fmt 0 · ②build 0 · ③clippy 0 · ④clippy-test-util 0 · ⑤test 0 · ⑥db 0（migrate=0, e2e=0）
  ⑧schema-drift 0 · ⑦route-parity 0 · ⑨conformance 0 · ⑩file-size 0
  ⇒ overall: PASS — 10/10 gate(s) green in 360s
```

- ⚠️ **首跑是 9/10**（`gates-m8-4.log`）：⑥ 的 e2e 报 `left: 404, right: 501`，红在
  `tests/issues/auth.rs::issue_auth_workspace_and_not_implemented` —— 那条断言断的正是本片
  实现掉的 `GET /api/issues/:id/pull-requests` 占位。按该断言自己的既有惯例重新指向
  `attachments`（见 **D10**）后**整套重跑 = 10/10 / 360s**（上表就是那一次）。
  ⇒ **lesson**：`implemented_placeholder` 那条读数每减 1，就有一条**别人写的**测试会变红；
  「占位搬运」只保证了注册键不变，**没有**保证断言不变 —— 派发时就该把这条写进写集
  （本片是第一次踩到；M8-5/M8-6 若也实现占位，请同样先 `grep` 一遍
  `NOT_IMPLEMENTED` 与 `not_implemented` 的测试断言）。
- ⑦（**本片会动读数**；`baseline 406` **不动**）：`upstream 456 (commit f41fae6b08fb) |
  local 453 registered`、`implemented 367 real + 3 placeholder = 370 / 456`、`known_gap 86`、
  `unclaimed 0`、`regression 0`、`local_only 9`、`gaps by owner: M9=33 M3+=16 M7=16 M3=11
  M10=5 **M8=5**`（和 = 86 ✓；`implemented + known_gap = 456` ✓）。
  **与「起手补充」的实测起手值逐差相等**：`local 452→453`（+1 = 新增 `webhooks/github`）、
  `implemented 369→370`（**real 365→367**：新增 1 条 + 占位换真实现 1 条；**placeholder 4→3**）、
  `known_gap 87→86`、`owners.M8 6→5`。`--write-baseline` **未跑**（唯一一次刷新归 M8-7 `LUM-1804`）。
- ⑦ 第二条（形态）：`slash_alias_audit.py` = `0 defect(s)`、exit 0（M8 `dual-form required: 0`，
  **没有** allowlist 退路）—— 本片两条键都是上游 plain 注册（`r.Post` / `r.Get`），只注册字面量形态。
- ⑩：**0 违规**，`scripts/file_size_baseline.tsv` **未动**。本片最大文件在 `cargo fmt --all`
  **之后**量：`tests/github_webhook/webhook.rs` **709**、`webhook/mirror.rs` 567、
  `issue_pr.rs` 542、`mc-vcs-github/src/mirror.rs` 528、`payload.rs` 510（余量最小的那条是 91 行）。
  ⚠️ **一个实测坑**：门 ⑩ 走 `git ls-files` ⇒ **未 `git add` 的新文件根本不被扫**。本片第一次
  跑 ⑩ 报「0 违规」是在新文件还没入索引时拿到的 —— 那条读数是**假绿**。`git add -A` 后重跑
  才是真读数（本节的数就是重跑后的）。
- ⑨：`report matches crates/mc-conformance/report.json`（本片 0 fixture 改动 ⇒ 未漂移）。
- ⑧：schema-drift 绿（本片 **0 迁移**、0 表改动 ⇒ 与基线同形）。
- ⑥：`mc-migrate` 566 个迁移 + `--ignored` 全绿，含本片 **14** 例真库 e2e
  （`mc-http --test github_webhook`：`14 passed; 0 failed`）与 `mc-repos` 的那 1 例
  （`github::check_suite::tests::head_sha_lookup_is_scoped_to_installation_and_repo ... ok`）。
- ⑤（`cargo test --workspace`，**不带**库变量）：全绿。本片新增 **47** 条**零 DB** 单测
  （`mc-vcs-github` 37 = payload 5 + webhook 4 + links 13 + closepolicy 5 + mirror 10；
  `mc-repos` 2 = check_suite 1 + pending 1；`mc-http` lib 8 = `routes::github::webhook` 3 +
  `routes::github::issue_pr` 3 + `webhook::mirror` 2）⇒ 连同真库 **15** 例（14 + 1），
  本片共 **62** 例新测试。
- **【lesson·三处 clippy 硬线（本片实测，全部在测试/新建文件上）】** ① 新 lint
  `assigning_clones`：`x = y.clone()` 要写 `x.clone_from(&y)`（本片 8 处，全在 DTO 映射）；
  ② `ref_option`：`fn f(v: &Option<T>)` 要改成 `fn f(v: Option<&str>)`（本片 1 处）；
  ③ `items_after_statements`：测试函数体里不能定义 `struct`/`impl`（提到模块层）。另两条：
  `needless_pass_by_value`（`serde_json::Value` 形参用 `impl Into<Value>` 最省改）与
  `too_many_lines`（100 行线，本片把 `mirror_pull_request_for_workspace` 拆出
  `apply_auto_link`、把超长的 CI 用例拆成两条）。
- **【lesson·门 ⑩ 的 800 行线会逼出「目录切片」】** `routes/github/webhook.rs` 一度到 **981 行**
  ⇒ 按 `channels/slack.rs` + `channels/slack/store.rs` 的既有手法拆成
  `webhook.rs` + `webhook/{installations,mirror,ci}.rs`（`foo.rs` 里 `mod bar;` ⇒ `foo/bar.rs`，
  **不是** `foo/mod/bar.rs` —— 与 §17.5 最后一条 lesson 同一形状）。测试侧同理：`tests/github_webhook/`
  从 4 个文件拆到 5 个（`webhook.rs` 711 → 709 + 新 `ci.rs` 205）。
- **【lesson·同 workdir 的并发片会争 cargo 的 build-dir 锁】** 本轮首次跑门禁时 ③ 卡在
  `Blocking waiting for file lock on build directory`：另一个切片的 run（`lum-1894-…`）在同时
  编译。**判据**：门禁日志出现这一行**不是**本片红灯，但不能据此提前报数 —— `ps` 看着自己的
  `gates.sh` PID 还活着就等它。本片两次全量跑：9/10 / 695s（含等待）与 10/10 / 360s。

#### 20.4.1 合并树上的重跑（base 前进到 `5b1028e1` 之后）

```
bash scripts/gates.sh --with-db --db-url 'postgres://mc_lum1801:…@127.0.0.1:5432/multica_lum1801'
  ①3s · ②182s · ③102s · ④65s · ⑤82s · ⑥250s · ⑧35s · ⑦1s · ⑨74s · ⑩2s
  ⇒ overall: PASS — 10/10 gate(s) green in 796s
```

- **合并**：`origin/feat/multica-rs-initial` 在飞期间前进到 `5b1028e1`（M7-6 的 `56a63d88` +
  三轮 cycle docs）⇒ 就地真合并。**一处冲突**：`docs/32` —— M7-6 先占了 `## 18.`，
  本片让号改 `## 19.`（**D 编号与内容一字未动，只有节号平移**；本片代码注释里的
  `docs/32` §18.x 自引用同步改为 §19.x）。除本片 19 个文件外**零改动**
  （`git diff --stat origin/feat/multica-rs-initial` 逐条比对过；M7-6 的
  `mc-channel/src/telegram/**` 与 `mc-repos/src/channel/**` 与本片 base 逐字相同）。
- ⚠️ **首跑 8/10（`gates-m8-4c.log`），两条红**，**都不是代码缺陷**，而是**磁盘耗尽**：
  ④ 报 `couldn't create a temp dir: No space left on device (os error 28)`；
  ⑥ 的红是 `telegram::revoke_is_admin_only_and_workspace_scoped` 拿到
  `409 telegram_bot_owned_by_another_workspace` —— **复用同一个测试库**跨多轮跑留下的行。
  **处置**：① 删掉本 workdir 的 `target/`（当时 19G，磁盘 100% / 仅剩 142M）；
  ② `DROP DATABASE` + 重建 + 让门 ⑥ 自己迁移 ⇒ 干净库；③ 重跑 = 上表的 10/10。
- **【lesson·同 workdir 的 target 是 ENOSPC 的第一来源】** 本片 target 单轮涨到 `19–21G`，
  把工作区磁盘顶到 **100%**。⇒ 与 `CARGO_INCREMENTAL=0` **同一理由**，本轮门禁额外带
  `CARGO_PROFILE_DEV_DEBUG=0`（只去掉 debug info，**不改任何门禁命令、不影响任何门禁的
  判定**：fmt/build/clippy/test/route-parity/schema-drift/conformance/file-size 都不读它，
  只有 panic 的 backtrace 会少符号）。**这是一条偏离，登记在案**：`gates.sh` 的默认环境没有
  这个变量，本节的读数是在「增量关闭 + debug info 关闭」下达成的 —— 与 §16.4 的
  `CARGO_INCREMENTAL=0` 读数同性质。
- **【lesson·测试库要每轮重建，别复用】** 本片第一次全量跑（10/10）与合并后重跑用的是
  **同一个** `multica_lum1801`；第二轮起 telegram 面就出现「bot 已连到别的 workspace」的
  409 —— 那是**上一轮遗留行**，与代码无关。⇒ 门 ⑥ 自己会 `mc-migrate run`，但**不会**清库；
  凡是「唯一约束 + 跨 workspace 归属」这类断言，跨轮复用同一个库就会假红。

### 20.5 交接（给 M8-5 / M8-7 / 未来碰这条路的切片）

1. **M8-5（ghsnapshot 管道）—— 本片给你留了一个**装配点**，请接上**：`routes/github/webhook.rs`
   的 `set_pr_refresh_port(Arc<dyn PrRefreshPort>)` 就是 `h.PRRefresh` 的等价物；M8-5 实现
   `impl PrRefreshPort for Manager` 之后，在 `apps/mc-server` 的装配里调用它（**在** `router()`
   之前），webhook 与页面访问两条入队路径就都活了。**不要**在 `mc-http` 里造第二份 worker。
   另外：**不要**碰 `github_pull_request_check_{suite,run}` 的写面 —— 那是你的
   （`github_snapshot.sql.go` 的对应物），本片刻意一行没写（D7）。
2. **M8-7（INT）**：本片把 ⑦ 推成 `local 453 / implemented 370 (367 real + 3 placeholder) /
   `known_gap 86 / owners.M8 5`；下一次 `--write-baseline`（406 → …）归你。请一并：① 把
   **D2**（端口注入槽）登记为「anchor 缺口的临时处置」并决定是否正式挂到 `AppState`；
   ② 把 **D3** 的 6 条查询要么归位到 `mc-repos`、要么在写集表里显式认下它们的当前位置；
   ③ 把 **D6**（wakeup 停机 + 父 issue 通知未接线）与 **D7**（`github_pending_check_suite`
   无归属）写进缺口清单；④ `mc-vcs-github/src/{lib,mirror}.rs` 与 `mc-repos/src/github/{mod,
   check_suite,pending}.rs` 的模块头里还有「M8-4 待落地 / doc-only 桩」的**文案债**（anchor
   冻结文件，本片不得改，与 §14.2 / §16.5 第 3 条同性质）。
3. **给后续任何写 GitHub 面的片**：三个**顺序契约**不要动 —— ① 投递级关闭裁决必须在任何
   workspace 关联**之前**算完（`resolve_close_intent_policy`），镜像那趟只能收紧它；
   ② 推进闸门必须读**持久化之后**的聚合（跨 GitHub + VCS 两张关联账），不是本次载荷；
   ③ 裸提及在 PR 仍可编辑时**要掉链**、终态之后**不得**掉链（`preserveCloseIntent` 的
   `Action != "closed"` 那一半最容易被读反）。
4. **给后续任何写 `routes/github/webhook.rs` 的片**：本文件已按门 ⑩ 拆成
   `webhook.rs` + `webhook/{installations,mirror,ci}.rs`（`channels/slack.rs` 的同款手法）。
   新增事件族请**新开子模块**，不要把 `webhook.rs` 顶回 800 行以上（它现在 232 行，入口与
   装配是它唯一的职责）。

---

## 21. M8-5（`LUM-1802`）：ghsnapshot 快照管道（0 路由）

> **号段说明**：计划书写的「`docs/32` §9.12」是计划期占位号（`## 9.` 是 M6-0 anchor、
> `## 11.` 是 M8-0）。**起手**实测最大 = `## 20.`（M8-4）⇒ 本片取 **`## 21.`**
> （`LUM-1773`/M7-8 同轮在飞；若它先落 `21.` 则由后合者让号平移，D 编号与内容不动 ——
> 与 M8-4 让号同一手法）。
>
> 本片**0 路由**（它服务的读面是 M8-4 的 `GET /api/issues/{id}/pull-requests`）⇒ ⑦ 读数
> 交付前后**逐字不变**。

### 21.1 落点（写集逐字 + 按门 ⑩ 拆出的子文件）

| 文件 | 行数 | 内容 |
| --- | --: | --- |
| `crates/mc-vcs-github/src/ghsnapshot/snapshot.rs` | 477 | 原地上填：`PR_SNAPSHOT_QUERY`（上游 `snapshot.go:27` 逐字）+ `PrSnapshot` / `SnapshotCheck` + `decided()`（三态）+ `parse_pr_snapshot{,_page}` + `fetch_pr_snapshot`（游标分页 + 三道守卫）+ `normalize_node` / `normalize_run_status` / `normalize_status_state` |
| `crates/mc-vcs-github/src/ghsnapshot/snapshot/tests.rs` | 282 | **新文件**（`mod tests;`）：纯函数用例 9 条（含 `Decided` 六例、`null` rollup、映射表逐条、`normalizeNode` 两形态） |
| `crates/mc-vcs-github/src/ghsnapshot/refresh.rs` | 762 | 原地上填：`Address` / `Job` / `State` / `Inner` / `Manager`（worker 池 + 单地址串行 + 三道退避 + TTL sweep + 停机）+ 时钟/抖动/定时器/抓取器缝 |
| `crates/mc-vcs-github/src/ghsnapshot/refresh/ports.rs` | 300 | **新文件**（`mod ports;` + `pub use`）：`SnapshotStore` / `StoreError` / `ResolvedTarget` / `PrRowRef` / `Timer` / `TokioTimer` / `SnapshotFetcher` / `HttpSnapshotFetcher` / `Tuning` / `ManagerOptions`（**契约面**，见 21.1 的拆分理由） |
| `crates/mc-vcs-github/src/ghsnapshot/refresh/tests.rs` | 608 | **新文件**（`mod tests;`）：管道用例 15 条（去重/单地址串行/并发上限/三级退避/退避序列/sweep/停机/端口） |
| `crates/mc-vcs-github/src/ghsnapshot/refresh/tests/wire.rs` | 360 | **新文件**（`mod wire;`）：**离线 GraphQL 替身**上的 7 条（分页到完 / 三道守卫 / null rollup / `Retry-After` / 出站逐字段 / 端到端） |
| `crates/mc-vcs-github/src/ghsnapshot/refresh/test_support.rs` | 762 | **新文件**（`mod test_support;`）：**手写 HTTP/1.1 替身**（`TcpListener`）+ `FixedStore` + `RecordingTimer` + 三种抓取器替身 + 假时钟 |
| `crates/mc-vcs-github/src/port.rs` | 207（+42） | `impl PrRefreshPort for Manager`（anchor 指定的落点）+ 三条语义注记 |
| `apps/mc-server/src/integrations.rs` | 463 | 原地上填：装配（`start` / `start_with`）+ `McSnapshotStore`（**直连 SQL 四段**，逐字对照 `pkg/db/queries/github_snapshot.sql`）+ 停机（收 worker + 关池） |
| `apps/mc-server/src/integrations/tests.rs` | 490 | **新文件**（`mod tests;`）：装配判据 4 条（零数据库）+ **真库 7 条**（守卫/批替换/null rollup/扇出/解析/sweep 筛法/sweep 游标） |

**零编辑（anchor 冻结，逐条实测未动）**：`crates/mc-vcs-github/Cargo.toml`、`apps/mc-server/Cargo.toml`、
`apps/mc-server/src/main.rs`、`Cargo.lock`、`crates/mc-http/src/state*.rs`、`routes/{mod,mount}.rs`、
`ghsnapshot/{mod.rs,client.rs}`、`mc-repos/src/github/**`、`docs/61-M8-PLAN.md`、`docs/fixtures/**`、
`scripts/file_size_baseline.tsv`、`docs/fixtures/route-parity-baseline.json`。
**只读面零触碰**：`mc-http` 的 webhook/issue_pr（本片只**调用**它的 `set_pr_refresh_port` 端口注入槽）。

#### 21.1.1 为什么新增 6 个文件（门 ⑩ 的 800 行线逼出的三处切分）

**判据**：门 ⑩ 是硬线（800 行/文件），而本片的上游体量是 **852 行 Go**（`snapshot.go` 290 +
`refresh.go` 562），加上本仓密度更高的文档与用例，单文件必然越线。三处按**职责**切，全部沿用
M8-4 的既有手法（§20.1 的 `webhook.rs` → `webhook/*.rs`）：

1. `snapshot.rs` 一度 **717 行**（用例与实现同文件）⇒ 用例移到 `snapshot/tests.rs`（9 条纯函数），
   实现留在 `snapshot.rs`；跑真 wire 的那几条**不放这里**（替身只写一份，见第 3 条）。
2. `refresh.rs` 一度 **1040 行** ⇒ 按「**契约面 vs 实现面**」切：`refresh/ports.rs` 放端口
   （trait）+ 形状类型 + 调参 + 注入缝，`refresh.rs` 放 worker 池 / 限流 / 退避 / 停机。
   对外路径**逐字不变**（`refresh.rs` 里 `pub use ports::{…}` 把 12 个名字重导一遍）。
3. `refresh/tests.rs` 一度 **888 行** ⇒ 真 wire 的 7 条移到 `refresh/tests/wire.rs`
   （替身道具住在 `test_support.rs`，两处共用一份）。

**新增文件全部由**自己父模块的 `mod` 声明**引入**（`refresh.rs` 声明 `mod ports;` /
`mod test_support;` / `mod tests;`，`tests.rs` 声明 `mod wire;`）—— **没有**碰任何 anchor 冻结的
`mod.rs`（`ghsnapshot/mod.rs` 的 `pub mod refresh;` 与 `pub use refresh::Manager;` 一字未动）。

#### 21.1.2 一处**签名修订**（D1 的落点）

anchor 的 `ghsnapshot/mod.rs` 逐字 `pub use snapshot::parse_pr_snapshot;` —— 这个名字**保留**，
但它的签名从 `(&Value) -> Result<PullRequestSnapshot, GithubError>` 改成
`(&Value) -> Result<PrSnapshot, GithubError>`（理由见 D1）。`Parser` 名字没变 ⇒ 冻结文件不需要动。

### 21.2 偏离登记（M8-5-D1 … M8-5-D9）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **D1** | `parse_pr_snapshot` 的**返回类型**从领域类型 `mc_core::github::PullRequestSnapshot` 改成上游同形的 `PrSnapshot` | `snapshot.rs` | anchor 那个签名**无法实现**：领域类型要求 `workspace_id` / `repo_owner` / `repo_name` / `pr_number` / `fetched_at`，这五项**都不在 GraphQL 载荷里**；而迁移 `222` 要写的 `api_mergeable` / `api_merge_state_status` / `checks_rollup_state` 它也不携带。锚点期该函数**零调用方**（`grep -rn parse_pr_snapshot` 实测只有桩与 `pub use` 两处）⇒ 无消费者受影响。**名字与 `pub use` 逐字不变** |
| **D2** | 上游 `Enqueue(installationID, owner, repo, number)` 的定位键是**地址**；本仓端口（anchor 冻结形状）的定位键是 `(workspace_id, owner, repo, number)` ⇒ `installation_id` 只能在 **worker 里**从 PR 行解析（一次索引查询） | `refresh.rs` / `port.rs` | 端口不含 installation id，而 `enqueue` 不得阻塞 ACK 路径（上游「入队与执行分离」）。**执行侧的单地址串行与上游逐字相同**（`active` / `in_flight` / `trailing` 三个集合都按地址键控，`attempts` / `rate_until` 同理）。代价：同一地址的**重复请求**会各做一次解析查询（去重只保证「解析 + 抓取」一次）。M8-4 的 D4 已把「每个绑定各入队一次」登记为同一处口径 |
| **D3** | 页面访问的 **view TTL 判定**（上游由 handler 传入 `fetchedAt` / `hasFetched`）挪到解析阶段做 | `refresh.rs::resolve_job` | 端口形状不带这两个字段（M8-4 的 `issue_pr.rs` 只传 `PrRefreshRequest`），而 passage「TTL 内不抓」的语义必须保留 ⇒ 解析时顺带读一次 `snapshot_fetched_at`（同一个解析查询的第三列）。副产物：`maybe_enqueue_on_view` 的返回值语义收窄为「**受理入队**」（上游是「现在真去抓」），已在 `port.rs` 的 impl 上逐字注明 |
| **D4** | `Manager::start` 改**同步**（anchor 桩写的是 `async fn start`）；`Manager::new` 多一个 `store` 实参 | `refresh.rs` | `start` 只 spawn worker + sweeper，**没有可 `await` 的东西**；而 anchor **冻结**的 `apps/mc-server/src/integrations.rs::start(keys)` 是同步函数（`main.rs` 的注释逐字「接线后这里换成真调用，**签名不变**」）⇒ 异步的 `start` 在同步装配点里只能再 spawn 一层，把启动错误变成不可见的日志。`store` 实参是宿主注入点（见 D7） |
| **D5** | 宿主**自建一个懒拨号的连接池**（`Config::from_env().database.url` + `Db::connect_lazy`，上限 8、`min_connections = 0`，停机时 `close()`） | `apps/mc-server/src/integrations.rs` | 冻结的 `integrations::start(keys)` 拿不到 `main.rs` 的 `Db`，而 `main.rs` 是本片的**只读**文件。三条出路里选了「自建小池」而不是「不装配存储（登记成缺口）」：后者会让「配了密钥但快照永远不落库」变成静默失效。生产路径上配置已被 `main` 解析成功过 ⇒ 这里必然拿得到同一个 URL。**代价**：多一个（空闲时零 TCP 的）池。**建议 M8-7**：在 `main.rs` 里把 `&db` 传进来（一行），删掉本池与该常量 |
| **D6** | `pull_request:updated` 广播**不接线**（`ManagerOptions::on_applied` 缝留出，默认 `None`） | `refresh.rs` / `integrations.rs` | 上游 `Manager.onApplied` 让「快照真的写进去了」广播一条事件；宿主同样拿不到 `realtime` 句柄（与 D5 同一处约束）。**可见后果**：页面访问触发的刷新写进库了，但客户端不会收到推送（下次打开卡片才看到新值）。这是**登记过的缺口**，不是遗漏；接线点是一行（`with_options` 的 `on_applied`） |
| **D7** | 存储端口的**生产实现落在 `apps/mc-server`**（直连 SQL），不在 `mc-repos` | `apps/mc-server/src/integrations.rs` | `mc-vcs-github` 的依赖边被 anchor 冻结（`Cargo.toml` 逐字「此后 M8-1/4/5 的写者不得再新增三方依赖」）⇒ 它没有 `sqlx` / `mc-db`；而 `mc-repos/src/github/{pull_request,check_suite}.rs` 属 M8-1/M8-4、`mc-repos/src/github/mod.rs` 是 anchor 冻结 ⇒ 四段 SQL 只能落宿主。**这正是 M5-9 的既有手法**（`scheduler/*_port.rs` 三个生产端口同样是「在本 crate 里直打 SQL」，理由逐字相同）。**建议 M8-7**：若要归位，统一挪进 `mc-repos` 并同步改 §3.3 的写集表 |
| **D8** | `aggregateChecksConclusion` **不在本片重复实现** | —（已在 `routes/github/issue_pr.rs`） | `docs/61` §6.5 的 M8-5 行点名「`normalizeRunStatus` / `normalizeStatusState` / `aggregateChecksConclusion` 的映射表逐条对照上游」：前两条是本片的（`snapshot.rs`，各有用例），第三条是 `handler/github.go:299` 的**计数 → 粗粒度结论**映射，M8-1/M8-4 已经落成 `routes/github/issue_pr.rs` 的 `rollup_to_conclusion` / `aggregate_checks_conclusion`（同一文件还有卡片的三态派生）⇒ 按 §2.3 的「不得重复实现」纪律**不搬第二份**。本片只把**逐 check 行**写进库（`github_pull_request_check_run`），聚合留给读面 |
| **D9** | 新增 6 个文件（3 个测试文件 + `ports.rs` + `tests/wire.rs` + `snapshot/tests.rs`） | 见 21.1 | 门 ⑩ 的硬线逼出的切分（21.1.1 的三条判据）；全部由自家父模块声明，不碰任何冻结 `mod.rs`。与 M8-4 的 `webhook/{installations,mirror,ci}.rs` 同款 |

### 21.3 专属 `DoD` 逐条证据（`docs/61` §6.5 的 M8-5 行）

| `DoD` 条目 | 证据 |
| --- | --- |
| **离线 GraphQL 替身**（`docs/61` §4.2 的名点：本地服务端答 `statusCheckRollup`） | `refresh/test_support.rs` 的 `HttpDouble` 是一台**真的 HTTP/1.1 服务端**（手写在 `TcpListener` 上：本 crate 加不了测试用 web 框架，依赖边冻结）；`github_double` 只替**平台 wire** 的两个端点（`/app/installations/{id}/access_tokens` 回 201 真形状 token、`/graphql` 由用例决定），其余 404。断言链 = 真 `reqwest` → 真 App JWT 签名 → 真 token 交换 → **真分页循环** → 归一化。7 条 wire 用例见 `refresh/tests/wire.rs` |
| **替身纪律之二：出站请求逐字段** | `offline_double_asserts_outbound_headers_and_query_body`：`POST /app/installations/7/access_tokens` 路径逐字 + `Authorization: Bearer <app jwt>` + 体 `{"permissions":{"metadata":"read"}}`；`POST /graphql` 的 `accept` / `x-github-api-version` / `content-type` / `Authorization: Bearer ghs_installation_token`（**必须用换来的 token**）+ 查询文本含 `statusCheckRollup` 与 `contexts(first:100,after:$cursor)` + 四个变量 + **首页游标必须是 `null`**（上游逐字） |
| **`PRSnapshot::Decided` 的三态**（在跑 / 已决 / 无检查） | `snapshot/tests.rs::decided_mapping_table_matches_upstream` 六例逐条（上游 `TestSnapshotDecided` 逐字：clean passed / conflicting / mergeable unknown / rollup pending / running context / **no checks but mergeable = 已决**）+ `null_rollup_means_no_checks_and_never_passed`（验收判据 5：`null` **绝不**当作通过） |
| **`normalizeRunStatus` / `normalizeStatusState` 映射表逐条** | `normalize_run_status_table`（`COMPLETED` / `IN_PROGRESS` / 四种「还在跑」兜底）、`normalize_status_state_table`（五值，含 `EXPECTED` 与未知同判）、`normalize_node_flattens_both_union_members`（两形态 + 未知 `__typename` 与非对象节点被跳过 + `conclusion: null` ⇒ `None`）。（第三条 `aggregateChecksConclusion` 见 D8） |
| **分页到完（验收判据 2）+ 三道守卫** | wire：`graphql_double_paginates_contexts_to_completion`（跨两页 4 条 context，含 `StatusContext` 折成同一形状）；`graphql_double_rejects_head_change_during_pagination`（第 2 次就发现不一致、不做第三次）；`graphql_double_rejects_pagination_beyond_the_page_limit`（病态游标被 `MAX_SNAPSHOT_CONTEXT_PAGES` 截断，调用次数逐字相等）；纯函数侧还有 `rollup_only_reads_the_first_commit_node`（上游 `Nodes[0]`）与 `malformed_payloads_are_rejected_without_echoing_body`（**不回显**载荷） |
| **限流三级：`RateLimitError` ⇒ `rate_limit_pause` / `defer_active` / `schedule_retry`** | ① `rate_limited_fetch_records_installation_pause_without_direct_retry`（只记 installation 级截止、**零**定时器 ⇒ 不建无界重试环，交回 TTL sweep）；② `rate_limited_installation_does_not_occupy_workers`（concurrency=2 的池被暂停租户的两件占满时，另一租户照样被抓；两件都交给定时器，延迟逐字 `3600s`）；③ `extend_rate_limit_never_shortens_and_is_installation_scoped`（只延不缩 + 按 installation 隔离 + 过期自动清账）；④ wire 侧 `graphql_double_surfaces_secondary_rate_limit_with_retry_after`（`403` + `retry-after: 90` ⇒ `RateLimited{90}`） |
| **退避时间序列可测（注入 `Now`，不许 sleep 真实时间）** | `RecordingTimer` 捕获延迟序列并手动触发；`chase_backoff_sequence_is_bounded` 断言 `30s → 60s → 120s → 300s → 300s`（用尽后停末项）且到 `max_chase_attempts = 5` **不再排**；假时钟 `FakeClock` 推进 120s 验证限流截止自动清账。全用例零真实等待 |
| **worker 池并发上限** | `worker_pool_caps_concurrency`（concurrency=2、5 个地址、抓取器观测同时在飞数）：`max_live <= 2` 且 `>= 2`（两个 worker 真的并行） |
| **单地址串行（同一 `(installation, owner, repo, number)` 不得并发刷新）** | `same_address_is_fetched_serially_with_one_trailing_replay`（闸门抓取器制造确定性时序）：在飞期间入队 ⇒ 只留 **trailing** 边、`max_live == 1`、回放**恰好一次**（`calls == 2`）；`enqueue_address_coalesces_and_marks_active` 与 `enqueue_coalesces_identical_requests` 钉住入队去重 |
| **TTL sweep（单地址串行 + 游标）** | 零数据库侧 `sweep_once_enqueues_stale_addresses_and_advances_the_cursor`（陈旧阈值 = `now - sweep_ttl`、首批零值游标、一轮有界 200、游标前进到上一批末地址）+ `sweep_failure_is_survivable`；**真库侧** `sweep_excludes_fresh_decided_and_closed_rows`（六行夹具逐条：新鲜 / 已决 / rollup 在跑 / 另一 installation 未抓过 / 已合并 / 有未完成 check_run）与 `sweep_cursor_rotates_and_is_bounded`（`max_rows=1` 截断 + 游标之后的行排前、其余回绕） |
| **停机链（N 秒内 worker 退出）** | `shutdown_stops_workers_within_the_grace_window`：`concurrency=2` 起 worker 后 `shutdown()` 在 500ms 宽限期内返回（实测毫秒级）、worker 句柄已全部收口、之后不再抓取、`active` 记账清零、第二次 `shutdown` 立刻返回（幂等）；`enqueue_after_shutdown_is_a_no_op` 钉住「停机后入队是 no-op」。`main.rs` 的顺序（先渠道 → **再 PR 刷新** → 再调度器 → 最后 actor）一行未动（anchor 已排好） |
| **真库写面（本片的新代码是 `github_pull_request_check_run` 的唯一写者）** | 7 条真库用例（`apps/mc-server/src/integrations/tests.rs`，`#[ignore]`、门 ⑥ 会跑）：head-SHA 守卫的**写成功**与**作废**两半、批次替换（2 行 → 1 行）、`null` rollup ⇒ `checks_rollup_state` 为 `NULL` 且零行、跨 workspace 扇出、`resolve_installation`（含 `None` ⇒ 不是错误）、sweep 两态 |
| **未配置 / 诚实退化（上游验收判据 4）** | `disabled_manager_is_inert`（缺 App 私钥 ⇒ `enabled()==false`、`start` 不起 worker、`enqueue*` 是 no-op、不 panic）+ `apps/mc-server` 的三条装配判据（无凭据不装配 / 有凭据无库 URL 明说未接线 / 有凭据+URL 真接线且停机干净） |
| **凭据纪律** | 本片**不新增**任何凭据类型（App 私钥/token 在 M8-1 的 `app.rs` / `token_cache.rs`）。两处自查：`db_error` **只**转发 `sqlx` 的原因（不回显 URL 与参数）、`integrations.rs` 的错误日志**不**插值 URL；`Manager` 的 `Debug` 只 expose 「配没配 + 并发数 + started」；抓取失败日志只打 `owner/repo/number/error`（`GithubError` 本就只带原因名与状态码） |
| ⑦：**0 路由 ⇒ 交付前后逐字不变** | 见 21.4（与「起手补充（rev 3）」的钉死值逐字相等） |
| ⑩：新文件 ≤ 800 行 | 见 21.4 与 21.1.1（三处切分就是为了它）；`scripts/file_size_baseline.tsv` **未动** |

### 21.4 门禁读数（逐字取自当轮日志；日志留档在 run workdir 的 `gates-m8-5*.log`）

```
bash scripts/gates.sh --with-db --db-url 'postgres://mc_lum1802:…@127.0.0.1:5432/multica_lum1802'
  ①fmt 0(2s) · ②build 0(3s) · ③clippy 0(1s) · ④clippy-test-util 0(1s) · ⑤test 0(41s)
  ⑥db 0(57s；migrate=0, e2e=0) · ⑧schema-drift 0(29s) · ⑦route-parity 0 · ⑨conformance 0(6s) · ⑩file-size 0
  ⇒ overall: PASS — 10/10 gate(s) green in 140s
```

⚠️ **首跑是 9/10**（`gates-m8-5.log`，450s）：③ 红在两条真库用例的 `too_many_lines`
（102 / 112 行，`apps/mc-server/src/integrations/tests.rs`）—— 按「夹具 + 两条用例」拆开
（判据见 §21.4 的 lesson）后**整套重跑 = 上表的 10/10 / 140s**。⑥ 的明细：
`mc-server` 的 `--ignored` 目标 `10 passed; 6 filtered out`（本片 **7** 例 + M5-9 的 **3** 例，
逐条 ok 见日志）。

- ⑦（**本片不动读数**；`baseline 406` **不动**）：`upstream 456 (commit f41fae6b08fb) |
  local 453 registered`、`implemented 367 real + 3 placeholder = 370 / 456`、`known_gap 86`、
  `unclaimed 0`、`regression 0`、`local_only 9`、`gaps by owner: M9=33 M3+=16 M7=16 M3=11
  M10=5 **M8=5**`（和 = 86 ✓）—— 与「起手补充（rev 3）」的钉死值**逐字相等**（0 路由）。
  `--write-baseline` **未跑**（唯一一次刷新归 M8-7 `LUM-1804`）。
- ⑦ 第二条（形态）：`slash_alias_audit.py` = `0 defect(s)`、exit 0（本片 0 新注册键）。
- ⑩：**0 违规**，`scripts/file_size_baseline.tsv` **未动**。本片最大文件在 `cargo fmt --all`
  **之后**量：`refresh.rs` 762、`test_support.rs` 762、`tests.rs` 608、`integrations/tests.rs` 490、
  `snapshot.rs` 477、`integrations.rs` 463、`tests/wire.rs` 360、`ports.rs` 300、`snapshot/tests.rs` 282。
  ⚠️ 门 ⑩ 走 `git ls-files` ⇒ 新文件**先 `git add`**（§20.4 的同一条实测坑）。
- ⑨：`report matches crates/mc-conformance/report.json`（本片 **0 fixture 改动** ⇒ 未漂移）。
- ⑧：schema-drift 绿（本片 **0 迁移**、0 表结构改动 ⇒ 与基线同形）。
- ⑥：`mc-migrate` 566 个迁移 + `--ignored` 全绿，含本片 **7** 例真库用例
  （`mc-server`：守卫写/作废、批次替换、null rollup、扇出、解析、sweep 筛法/游标 —— 逐条 ok）。
- ⑤（`cargo test --workspace`，**不带**库变量）：全绿。本片新增 **31** 条零 DB 单测
  （`mc-vcs-github`：`snapshot` 9 + `refresh` 15 + `refresh::wire` 7）+ `mc-server` 的 **4** 条装配判据
  ⇒ 连同真库 **7** 例，本片共 **42** 例新测试（`mc-vcs-github` 的 lib 用例从 63 → **94 passed**
  / 0 failed，日志第 3315 行；`mc-server` 的零 DB 目标从 12 → **16**，其中本片 4 条）。
- **[lesson·门 ⑩ 的 800 行线对「上游 852 行 + 本仓文档密度」是**必然**触发，不是意外]** 本片三处
  越线（1040 / 888 / 717）在**写之前**就由体量判死 ⇒ 正确做法是**先按职责规划文件切分**，而不是
  写完再拆。切分轴取「契约面（trait + 形状 + 调参）vs 实现面（worker/限流/退避/停机）」与
  「纯函数用例 vs 真 wire 用例」——两者都让**读的人先看到契约**。
- **[lesson·`cargo clippy -p <crate>` 与门 ③ 的 `--workspace` 不等价]** 本片首跑 9/10，红在 ③ 的
  **`too_many_lines`**（两条真库用例 102/112 行）—— 而单 crate 的 `cargo clippy -p mc-server` 当时
  报绿（增量指纹把它跳过了）。**判据**：交付前**必须**跑 `--workspace`（或直接 `gates.sh`），
  单 crate 的绿不能当选票。
- **[lesson·`#[async_trait]` 的宏再导出]** 端口定义方（`mc-vcs-github`）有 `async-trait` 依赖边，
  而宿主（`apps/mc-server`）的 manifest 冻结 ⇒ 由定义方 `pub use async_trait::async_trait;`
  带出宏，实现方 `use mc_vcs_github::ghsnapshot::refresh::async_trait;` 即可（不必为测试加依赖边）。
  注意：同一模块里**不要**再写一行私有的 `use async_trait::async_trait;`（两者会撞名）。
- **[lesson·`Db::connect_lazy` 是同步的，可以放进同步装配点]** D5 的自建池之所以能成立，全靠
  `mc_db::Db::connect_lazy(url, max, min) -> Result<Db>` **不拨号**：装配点（同步）只构造池，
  真正的连接发生在 worker 的第一次查询。（`Db::connect` 是 async ⇒ 那条路会反过来逼 `main.rs` 改签名。）

### 21.5 交接（给 M8-6 / M8-7 / 未来碰这条路的切片）

1. **M8-7（INT）** 请收口四件事：① **D5** —— 决定是否把 `&db` 从 `main.rs` 传进来（一行），
   删掉本宿主的自建池；② **D6** —— 决定是否在 `main.rs` 里把 `realtime` 句柄交给
   `ManagerOptions::on_applied`（一行，之后页面访问触发的刷新才有推送）；③ **D7** —— 决定四条
   SQL 是否归位到 `mc-repos`（要同时改 §3.3 的写集表）；④ 缺口清单：`github_pending_check_suite`
   仍无归属（M8-4 的 D7）、`mc-repos/src/github/{check_suite,pending}.rs` 与 `ghsnapshot/refresh.rs`
   里仍有「M8-5 待落地」类的**文案债**（anchor 冻结文件，本片不得改）。
2. **M8-6（composio）**：与本片**零文件交集**（本片写 `ghsnapshot/**` + `port.rs` 的 impl +
   `apps/mc-server/src/integrations.rs`）。若 M8-6 也要在 `apps/mc-server` 起后台宿主，请**新开文件**
   （`src/composio.rs`），别往 `integrations.rs` 上加 —— 那里已经是「GitHub 快照宿主」的单一职责。
3. **未来任何碰 `ghsnapshot/**` 的切片**：`refresh.rs` 是 762 行、`test_support.rs` 也是 762 行
   （都离 800 很近）⇒ 新增用例请按现有轴分流（纯函数 → `snapshot/tests.rs`；管道 → `refresh/tests.rs`；
   真 wire → `refresh/tests/wire.rs`），**不要**把任一个顶回越线。
4. **未来任何改 `routes/github/webhook.rs` 的切片**：本片只**调用**它的
   `set_pr_refresh_port`（进程级注入槽，按请求读 ⇒ `main.rs` 先建 router、后起宿主也生效）。
   若要把端口正式挂到 `AppState`（M8-4 的 D2 建议），那是**一次 anchor 级改动**，请与 D5/D6 一起做。

---

## 22. M7-8（`LUM-1773`）：dingtalk 出站 / 媒体 / 回复 / 回执（0 路由）

> **号段说明**：计划期与派发补充都写「取实测最大 +1」；**起手**实测最大 = `## 20.`（M8-4）⇒
> 原取 `## 21.`，但 `## 21.` 被同轮在飞的 M8-5（`LUM-1802`，PR #97）先落 ⇒ 本片**让号**为
> **`## 22.`**（D 编号与内容不动，与 M8-4 让号同一手法）。

**口径**：本节一切落点与偏离都在上游 `f41fae6b08fb` 与本片实测树上核对。

- **起手 base**：`a20f69a271ca73b12c8b3e6a19e427f46ce39e32`（= 派发描述 rev 3 钉的值；
  `git fetch` 后实测一致）。
- **提交 / 推送时 origin tip = `3797b940`**（`a20f69a2` → M8-5 PR #97 合并 `b76b9b32` →
  §103/§104/§105 cycle docs）。与 `a20f69a2` 的逐文件差集里**代码面零交集**
  （M8-5 只动 `crates/mc-vcs-github/**` + `apps/mc-server/src/{integrations.rs,integrations/tests.rs}`），
  但 **`docs/32` 有 +159 行交集**（§21 号段）⇒ 本片**已 rebase** 到 `3797b940`（一次干净 rebase，
  无冲突），PR 以 `3797b940` 为 base。**门禁在 rebase 后的树上重跑**（读数见 §22.3）。

### 22.1 落点（逐字路径）

| 落点 | 写者 | 上游 |
| --- | :-: | --- |
| `dingtalk/mod.rs` | M7-0 建 / M7-7 填 / **M7-8 追加** | 本片只加 5 行 `pub mod`、出站端口接线、`Channel::send` 的发送体；**未动** `register` / `register_with` / `register_resolvers` |
| `dingtalk/outbound.rs` + `outbound/{target,openapi,credentials,quote,source}.rs` | M7-8 | `outbound.go`（313）+ `outbound_send.go`（238）+ `outbound_quote.go`（51）+ `reply_source.go`（127）+ `client.go`/`token.go` 的出站两条 |
| `dingtalk/outbound/tests.rs` + `outbound/tests/http.rs` | M7-8 | 本片自己的用例（纯函数 / 端口替身 ∥ 真 HTTP 端到端） |
| `dingtalk/markdown.rs` + `markdown/tests.rs` | M7-8 | `markdown.go`（286）+ `outbound_send.go` 的 `escapeMarkdown*` / `prependMarkdownQuote` |
| `dingtalk/media.rs` + `media/{guard,tests}.rs` | M7-8 | `media.go`（385） |
| `dingtalk/ack.rs` + `ack/{batch,tests}.rs` | M7-8 | `ack.go`（220）+ `ack_batch.go`（160） |
| `dingtalk/replier.rs` + `replier/tests.rs` | M7-8 | `replier.go`（342） |

### 22.2 偏离（D1…D12，逐条可核对）

- **D1 写集勘误（两类，逐条）**：

  1. **`dingtalk/mod.rs`**：不追加 5 行 `pub mod` 那五个新文件根本不参与编译（派发补充已点名，
     这是**第二类漏项第 5 次**）；本片对它只做三件事 —— 追加 `pub mod ack/dispatch…` 里自己那 5 行、
     把 `resolve_app_secret` 收成 `pub(crate)`（出站复用**同一条**凭据判据，见 D4）、
     给 `DingTalkChannel` 挂出站端口并实现 `Channel::send`。三个注册入口**一字未动**。
  2. **新增路径**：`outbound/{target,openapi,credentials,quote,source}.rs`、
     `outbound/tests/http.rs`、`media/guard.rs`、`ack/batch.rs`，以及 7 份 `*/tests.rs`。
     **全部**是门 ⑩ 的 800 行硬限逼出来的切分，边界取**上游文件边界**
     （`outbound_send.go` 的目标+分片 / `client.go`+`token.go` 的两条调用 / `config.go` 的
     `decodeCredentials` / `outbound_quote.go` / `reply_source.go` / `media.go` 的出口守卫 /
     `ack_batch.go`）。首版成形时越限的文件：`outbound.rs` 1377、`outbound/tests.rs` 1094、
     `media.rs` 885、`mod.rs` 844、`ack.rs` 804 ⇒ 拆完最大 788（`mod.rs`），全部 ≤800；
     **未动** `scripts/file_size_baseline.tsv`（仍 22 行）。
- **D2 出站端口由**工厂**注入，构造器不隐式造 HTTP 客户端**：`DingTalkChannel::new` 不注入
  ⇒ `Channel::send` **失败关闭**（M7-7 的用例 `send_is_fail_closed_and_capabilities_match_upstream`
  仍绿）；工厂（`factory_with_slots`）注入 [`outbound::OpenApiTransport`] 的生产实现
  （`HttpOpenApi`）与 `robot_code` ⇒ 宿主径上真的发得出去。两条路各有用例
  （`the_factory_wires_outbound_so_channel_send_posts_a_group_frame` / 上引）。
- **D3 令牌缓存与 `postJSON` 落在 `outbound/openapi.rs`（不是 M7-9 的 `client.rs`）**：
  M7-8 是出站的唯一写者，而 M7-9 才落完整的 `Client`（安装 / 吊销面也要它）⇒ 本片抽成端口
  [`OpenApiTransport`] + 生产实现 `HttpOpenApi`，**每条发送语义（校验 / 分片 /
  401 ⇒ 作废令牌 + 重试一次）在 `Sender` 里**，与实现解耦。M7-9 换实现即可，语义一行不动。
- **D4 凭据解码复用 M7-7 的判据**：`app_secret_encrypted` 的三条分支（密文优先 / 明文 / 都缺）
  只在 `mod.rs` 的 `resolve_app_secret` 有一份（上游 `config.go` 的 `decodeCredentials` 也只有一份）
  ⇒ 本片把它 `pub(crate)` 并在 `outbound/credentials.rs` 里薄包一层。`Decrypter` 的**收敛**
  （三份同形：slack / telegram / dingtalk）仍归 M7-9 的 `config.rs` —— **不静默略过**。
- **D5 出口守卫的两处形态差异（`media/guard.rs`）**：
  1. **没有 `http.DetectContentType`**：只认入站白名单那五种的**魔数**（更严，不会更松）；
  2. **`reqwest` 的 `dns_resolver` 只对域名生效**（host 已是 IP 字面量时它根本不解析）⇒
     字面量地址那一步由 `guard_download_url` 自立判据，域名那一步才交给
     `PublicOnlyResolver`（解析出来的**任一**答案非公网即整体拒）。两处合起来与上游
     `publicDownloadDialer` 的保证同义（DNS 重绑定改不了连接去向）。另：上游要在重定向回调里
     删 `Referer`（`net/http` 会把带票据的查询串带进下一跳），而 **`reqwest` 从不自动加它**
     ⇒ 这条保证由构造给出，没有可删的东西（同样登记，不假装做过）。
- **D6 端到端回路的**入站**那一半复用 M7-7 的替身**：`tokio-tungstenite` 在本仓**没有**
  `handshake` feature（M7-0 把依赖面一次定死）⇒ 起不了真实 WS 服务端。本片因此：
  **入站** = M7-7 的四份上游 golden + 脚本化内存 socket（`stream/tests.rs` / `inbound/tests.rs`），
  **出站** = **真** `reqwest` → **真** loopback HTTP 服务端（`outbound/tests/http.rs`），
  断言替身**实际收到**的原始帧。也就是说"端到端"是**金色入站 ⇒ 真出站帧**，而不是
  "真 WS 服务端 ⇒ 真出站" —— 这条差异写在用例文件的模块文档里，不在 PR 描述里美化。
- **D7 出站**不**做媒体上传（方向是单向的）**：`DingTalk` 的出站机器人消息只有
  `sampleMarkdown` 一种 `msgKey`（上游 `outbound_send.go` 一个字面量），**没有**图片 / 文件
  发送端点 ⇒ "媒体单向一并覆盖"指的是 **平台 → 本仓对象存储** 这一个方向（上游 `media.go`
  同样只有这一向）。三类出站形态的**真实**构成是：**文本**、**引用**（群发的 Markdown 引用块）、
  **私聊**（`oToMessages/batchSend`，**不**带引用）；"互动卡片"是**入站**形态
  （`quoted_interactive_card` golden），它进的是出站引用块 —— 用例逐条钉住这四种组合。
- **D8 顺序取回代替并发取回（`media.rs`）**：上游 `errgroup.SetLimit(2)`；本仓的每次取回自己就是
  一个线程 + current-thread 运行时（同步端口，见 D9）⇒ 再做池化只会成倍复制运行时，而内存上界
  反而更宽。改成**顺序**取回：语义不变（每张图互不牵连），上界从"最多 2 × 10 MiB"**收紧**到
  "最多 1 × 10 MiB"。`MEDIA_FETCH_CONCURRENCY` 保留，文档说明它不再当并发度用。
- **D9 同步端口 + 独立线程运行时**：engine 的 `MediaResolver` 是**同步**签名（M7-1 定死的契约），
  而 `reqwest` 没开 `blocking` feature ⇒ `ThreadedFetcher` / `block_on_engine` 在独立线程上跑
  current-thread 运行时（与 `slack::media::ThreadedFetcher` 同款）。真实代价：每个媒体对象
  多一线程。
- **D10 端口化三处**（adapter 不直接写 DB，`docs/60` §2.6 第 1 条）：`MediaStorage`（对象存储，
  与 `slack::media::MediaStorage` 同形**但不复用** —— §2.2 明确要 adapter 之间零互相依赖）、
  `ReactionInputQueries`（读 `chat_message`）、`ReactionSender`（贴 / 撤表情）。
  收敛进 engine 归 **M7-21**。
- **D11 没有进程内事件总线 ⇒ 三个入口是显式调用**（同 M7-6 对 telegram `outbound.go` 的先例）：
  `OutboundDelivery`（`EventChatDone` 那条路）、`AckNotifier::on_reply_delivered`、
  `AckNotifier::agent_archived_now`。"谁在什么时候调它们"是宿主的装配面。
- **D12 reply_source 的**写入点**仍缺（交接项，逐条登记）**：上游在 resolver 的 append 成功之后
  调 `rememberReplySource`（`resolvers.go:391/486`）。本片给出 `ReplySourceCache` 与
  `AckNotifier::remember_source`，但 `resolvers.rs`（M7-7 的只读面）**不**写它 ⇒
  **接线点**（宿主 / M7-9 的 binder 路径）必须在 M7-9 / M7-21 补上，否则 Done 永远贴不出来
  （其余回执不受影响）。**同样缺**：`with_replier` / `with_media` / `with_typing` 三个装配调用。

### 22.3 门禁与读数（**rebase 后的树上实测**）

```
⑦  upstream 456 | local 453 | baseline 406
    implemented 367 real + 3 placeholder = 370 / 456 | known_gap 86 | unclaimed 0 | regression 0 | local_only 9
    gaps by owner: M9=33  M3+=16  M7=16  M3=11  M10=5  M8=5      ← 与起手**逐字相同**（0 路由 ⇒ 不变量）
形态门  declared 24 / dual-form 0 / 0 defect（exit 0）；local 实况 0 defect（exit 0）
⑩     新文件最大 788（mod.rs）；scripts/file_size_baseline.tsv **未动**
⑨     report matches crates/mc-conformance/report.json（0 改动 ⇒ 5s 热跑）
门禁   `bash scripts/gates.sh --with-db` = **10/10 绿（145s，热）**；`cargo test -p mc-channel --lib` = 551 passed
       （其中 `dingtalk::` 175 = M7-7 的 95 + 本片 80：markdown 13 / outbound 23 / media 16 / ack 15 / replier 13）
```

> **门 ⑥ 的**测试库复用**假红（已知，本轮第三次实测）**：同一库上连跑两次 `--with-db` ⇒ ⑥ 报
> `e2e=101`（telegram 面的 `409 telegram_bot_owned_by_another_workspace`：上一轮遗留行）。
> **DROP DATABASE 重建后同一条命令全绿**（⑨ 单跑 ⑥ 105s / 全量 145s）。判据：⑥ 红了先看是不是
> **同库第二跑**，不要先去怀疑代码。
>
> **⚠️ 交给下一轮的观察项（本片**无法**归因，逐字记下）**：本片在 rebase 后的树上跑过 **5 次**
> `--with-db`（每次**都**先 `DROP DATABASE` + `CREATE DATABASE`），其中 **2 次** ⑥ 报 `e2e=101`
> （每次都**未落盘**完整输出 ⇒ **失败用例名未知**，只有 `GATE_DB_E2E_EXIT=101`；两次都在 148s/185s，
> 失败后**同库**单跑 ⑥ 立刻绿），**3 次全绿**（54s / 163s / 249s 全量 10/10）。
> 单独跑 ⑥ 的等价命令（`cargo test -p mc-repos -p mc-http -p mc-scheduler -p mc-server
> --features mc-http/test-util -- --ignored`）**4 次全绿**。判据：**本片 0 路由、0 DB 改动、0
> `#[ignore]` 用例**（新增的 80 条用例全在 `mc-channel` 的纯内存路径上）⇒ 与这 2 次红**无因果**；
> 下一次请**带 `> log` 落盘**再跑 `--with-db`，把失败的用例名钉住（大概率是 M8-5 合并进来的
> `apps/mc-server` DB 面与 `mc-http` 渠道面共享 `channel_installation` 行的顺序竞争）。

### 22.4 本片的 lesson（写给后续的渠道片）

- **【lesson·拆分是**机械**的，但"先写再拆"会**丢东西**】** 为了过门 ⑩ 我用脚本按行号搬块，
  一次搬迁把"两段之间的整块（`sendTarget` + 分片 + 引用占位）"连同 `#[cfg(test)] mod tests;`
  的声明一起搬丢/漏搬（前者要凭上下文重写，后者让 15 条用例**静默不编译** —— `test result: 551`
  掉到 536 才被发现）。**判据**：① 按**符号边界**搬，不按行号；② 每搬一次先 `cargo test`
  数**用例条数**（总数是最便宜的回归探针），再 `git diff --stat` 上看文件行数；③ 文件末尾的
  `#[cfg(test)] mod tests;` 属于"必须留在父文件"的那一类。
- **【lesson·"上游这一行依赖的是**库**还是**平台**"这条判据在出站面同样成立】** 上游 `net/http`
  的三件事（`CheckRedirect` 里的 `Referer` 删除、`DialContext` 里的地址校验、`DetectContentType`）
  在 `reqwest` 里**两件没有对应物、一件更严**：`Referer` 根本不会加（保证更强，但要**写明**
  它是由构造给出的）、`dns_resolver` 不管 IP 字面量（必须自立判据）、魔数嗅探按白名单收窄。
  照抄"上游这里删了 Referer"会写出一次**空操作**并留下假的安心感。
- **【lesson·同步端口上的"每个对象一个线程 + 运行时"是**真实**代价】** `MediaResolver` 的同步
  签名把 `errgroup` 的并发语义换成了顺序取回：内存上界更紧是对的，但要**登记**（D8），
  否则下一位会以为"上游并发 2、本仓并发 2"。
- **【lesson·"绿了"要连**跑法**一起记】** ⑤ 与 ⑥ 连跑的 `--with-db` 与"只跑 ⑥"是**两种**跑法：
  本片 2/5 与 4/4 的差异就出在这里 ⇒ 报"门禁 10/10"时必须同时给出**跑法 + 测试库状态**
  （是否重建），否则下一位复现不出来。
- **【lesson·`mod.rs` 是热点：先追加、再改，且**只**碰自己的行】** 本片对 `mod.rs` 的三处改动
  （5 行 `pub mod`、`resolve_app_secret` 的可见性、`Channel::send` 的发送体）都在**已有函数体之外**
  或"明确归本片"的那个函数里；三个注册入口与 `register*` 一行未动 ⇒ 与 M7-9（同文件追加 `pub mod`）
  的合并面只剩"两段 `pub mod` 相邻"这一点，rebase 即可。

---

## 23. M8-6（`LUM-1803`）：composio（SDK 客户端 + 服务 + state HMAC + toolkit 目录 + overlay 构建；5 路由）

> **号段说明**：计划书写的「`docs/32` §9.12」是计划期占位号（`## 9.` 是 M6-0 anchor、
> `## 11.` 是 M8-0）。**起手**实测最大 = `## 21.`（M8-5）⇒ 本片原取 `## 22.`，但 `## 22.`
> 被同轮在飞的 **M7-8（`LUM-1773`，PR #98）先落**（它的 §22 已在 base `0d4510e4` 里）⇒
> 本片**让号**为 **`## 23.`**（D 编号与内容不动，与 M8-4/M8-5/M7-8 让号同一手法）。
>
> 本片**是 M8 的最后一个代码片**：`owners.M8` 的 5 条（= composio 的 5 条）交付后归零。

### 23.1 落点（写集逐字 + 按门 ⑩ 拆出的两个子文件）

| 文件 | 行数 | 内容 |
| --- | --: | --- |
| `crates/mc-composio/src/client.rs` | 720 | 原地上填 anchor 桩：`DEFAULT_API_BASE`（修正为上游的 `…/api/v3.1`）、`AuthConfig` / `ToolkitEntry` / `ConnectedAccount` / `CreatedLink` / `ToolRouterSession` 五个 wire 形状 + **7 个端点**（toolkits 分页 / auth_configs 分页 / link / connected_accounts / revoke / delete / session）+ 唯一出站口（`get_json` / `post_json` / `post_empty` / `delete_empty` / `send` / `check_status`） |
| `crates/mc-composio/src/client/tests.rs` | 146 | **新文件**（`mod tests;`）：Debug 脱敏、默认 base/UA/超时、URL 拼接与百分号编码、`auth_headers` 的 bearer 面、状态码映射、7 条 wire 形状的容错反序列化、link/session 的出站体 |
| `crates/mc-composio/src/state.rs` | 538 | 原地上填：`StateClaims`（`u/t/a/e/n`）+ `sign` / `verify`（**常量时间**验签、`>` 边界、nonce 台账）+ `fresh_nonce`（`std` 熵 + 进程内计数；**不引** `rand`） |
| `crates/mc-composio/src/catalog.rs` | 389 | 原地上填：`visible_toolkits`（**纯函数**：auth-config 未配的 toolkit 整条丢弃）+ `best_auth_config_for_each_toolkit` / `better_auth_config`（自定义胜托管、同类间新的胜）+ `AuthConfigDirectory`（TTL 缓存 + 刷新失败发陈旧快照） |
| `crates/mc-composio/src/service.rs` | 639 | 原地上填：`ComposioError`（10 支）、`ComposioConfig`（+`state_ttl_secs`）、`ConnectionView` / `ToolkitView` / `McpSession` / `CallbackOutcome`（四态）+ `begin_connect` / `complete_callback` / `list_connections` / `disconnect` / `list_toolkits` / `create_mcp_session` + `verify_account_ownership`（**fail closed**） |
| `crates/mc-composio/src/service/tests.rs` | 238 | **新文件**（`mod tests;`）：四条件、`missing()` 逐条、未配置不 panic、回调 URL、state 四态、store 缺失、错误不回声凭据 |
| `crates/mc-composio/src/overlay.rs` | 180 | 原地上填：`build_task_overlay`（**纯函数**：三道闸门 + `{"mcpServers":{"composio":{"type":"http","url":…}}}`）+ 与 `mc-core::mcp::overlay::merge_task_overlay` 的合并对接用例 |
| `crates/mc-repos/src/composio/connection.rs` | 333 | 原地上填 doc-only 桩：`ComposioConnectionRow`（11 列 + `to_domain`）、`NewComposioConnection`、`ComposioConnectionRepo::{upsert,list_active,get,mark_revoked}`（**每条**带 `user_id` 收窄；SQL 提成 `upsert_sql()` / `list_active_sql()` / `get_sql()` / `MARK_REVOKED_SQL` 以便单测断言真语句） |
| `crates/mc-http/src/routes/composio/connect.rs` | 298 | 原地上填：`POST /connect/init` + 本面的**公共件**（`service_from` / `error_with_code` / `not_configured` / `upstream_error` / `error_response` / `set_composio_api_base`） |
| `crates/mc-http/src/routes/composio/catalog.rs` | 293 | 原地上填：`GET /toolkits` + `GET /connections` + `DELETE /connections/:id` + 两个响应 DTO（`omitempty` 与 `connectable` 恒真的 wire 契约） |
| `crates/mc-http/src/routes/composio/callback.rs` | 300 | 原地上填：`GET /callback`（**公开**）+ `app_url_from_env`（`MULTICA_APP_URL` → `FRONTEND_ORIGIN` → 相对路径）+ 302 重定向构造 |
| `crates/mc-http/tests/composio/{main,support,gating,callback,flows}.rs` | 19/594/181/223/461 | **新目录**（5 文件）：替身（按 `x-api-key` 分派，一个 base 服务整个二进制）+ 15 条真库用例 |

**零编辑（anchor 冻结，逐条实测未动）**：`crates/mc-composio/{Cargo.toml,src/lib.rs}`（**lib.rs 一行未动** ——
handler 直接走 `mc_composio::catalog::…` / `mc_composio::state::…` 模块路径，不需要新 re-export）、
`crates/mc-repos/src/composio/mod.rs`、`crates/mc-http/src/routes/composio/mod.rs`、
`crates/mc-http/src/{state.rs,state/integrations.rs,routes/mod.rs,routes/mount.rs}`、
`crates/mc-core/src/**`、`Cargo.toml` / `Cargo.lock`、`apps/mc-server/**`、
`docs/fixtures/**`、`scripts/file_size_baseline.tsv`、`docs/fixtures/route-parity-baseline.json`。
**只读面零触碰**：`mc-repos/src/task/**`、`crates/mc-chat/src/task.rs`、`crates/mc-autopilot/src/dispatch/**`
（本片**不接线**，R-M8-9 的尾账原样留给 M8-7）。

#### 23.1.1 门 ⑩ 逼出的**两处**切分（都在 `mc-composio` 自己的文件里）

**判据**：门 ⑩ 是硬线（800 行/文件）。首版实测 `client.rs` **864** / `service.rs` **869** 越线
（用例与实现同文件）⇒ 两处按 M8-5 的既有手法（§21.1.1 第 1 条）把 **`#[cfg(test)] mod tests`**
移到 `client/tests.rs` / `service/tests.rs`：

| 文件 | 拆前 | 拆后 |
| --- | --: | --: |
| `client.rs` | 864 | **720**（用例 146 移出） |
| `service.rs` | 869 | **639**（用例 238 移出） |

**新增文件由自己父模块的 `mod tests;` 声明**（`client.rs` / `service.rs` 各一行）——

**没有**碰任何 anchor 冻结的 `mod.rs`，也**没有**动 `lib.rs`（`pub mod client;` 与
`pub mod service;` 逐字不变；Rust 2018 的路径规则让 `src/client.rs` + `src/client/tests.rs` 共存）。
⚠️ 门 ⑩ 只扫 `git ls-files` ⇒ 两个新文件**先 `git add`**（§20.4 / §21.4 的同一条实测坑）。

### 23.2 偏离登记（M8-6-D1 … M8-6-D13）

| # | 偏离 | 位置 | 性质与理由 |
| --- | --- | --- | --- |
| **D1** | **`client.rs` 由 M8-6 落地**（issue 的「只读」表把它列在只读面） | `client.rs` | 它里面**每一个**方法的 `todo!()` 都逐字写着「实现归 M8-6」（`list_toolkits` 的 `todo!` 文案就是 `GET {api_base}/toolkits（docs/61 §4.2 的 M8-6 行）`）。anchor 已合（`LUM-1797` 终态）⇒ **不存在并发写者**，而留着 `todo!()` 会让一个公开 API 在生产路径上 panic。issue 自己为 `lib.rs` 立了同款判例（「要在 PR 里逐行说明」）⇒ 本片照同一条纪律登记：**只填桩、不改 anchor 钉死的形状**（`new` / `with_api_base` / `enabled` / `api_base` / `Debug` 逐字保留） |
| **D2** | `DEFAULT_API_BASE` 从 anchor 的 `…/api/v3` 改成上游的 `…/api/v3.1` | `client.rs` | anchor 的注释逐字「若上游 SDK 以后改端点，改这一处」；上游 `pkg/composio.DefaultBaseURL` 实测 = `https://backend.composio.dev/api/v3.1`。**唯一**改动值是这一行，且有 `default_base_matches_the_upstream_sdk` 钉住 |
| **D3** | `StateSigner::sign` 的签名从 `(subject: &str, now_unix: i64) -> String` 改成 `(&StateClaims) -> Result<String, StateError>`；`verify` 从 `-> Result<String, _>` 改成 `-> Result<StateClaims, _>` | `state.rs` | anchor 那个形状**无法承载契约**：`CompleteCallback` 需要 `ToolkitSlug`（重定向）与 `AuthConfigID`（fail-closed 的跨 toolkit 绑定），只绑一个 `subject` 做不到。两个方法在 anchor 期**零调用方**（`grep -rn 'StateSigner::'` 实测只有桩与 `pub use`）⇒ 无消费者受影响。`StateError` 的四支与 `Debug` 脱敏**逐字保留** |
| **D4** | state 载荷**多一个 nonce**（上游只有 4 个字段），重放靠**进程级**台账 | `state.rs` | anchor 的 `StateError::Replayed` 与文件头逐字写着「重放靠 nonce（进程内台账，无 Redis ⇒ 单副本部署契约）」⇒ 这是 anchor 指定的契约，上游只用短 `exp` 限重放。⚠️ 台账**必须**是进程级：`ComposioService` 按请求构造（D8），台账若跟着实例，同一份 state 的第二次到达会落到新实例上、永远判不出重放（`the_replay_ledger_is_process_wide_not_per_signer` 钉住这一点）。**代价**：多副本部署时重放窗口跨副本失效 ⇒ 单副本部署契约（与 R-M7-1/R-M8-5 同源） |
| **D5** | 「未配置」回 **403**，不是计划书写的 503 | `routes/composio/connect.rs` | 与 M8-1 的 `repositories`、M8-2 的 VCS 写面**同一个发现**：上游的实现是 `writeFeatureDisabled(...)` = `writeErrorCode(w, http.StatusForbidden, …)`，上游 `integrations_composio.go` 的文件头逐字写着「A missing deployment capability is a **non-retryable 403**, not a transient 503.」（`router.go` 注释里残留的 503 是过期文案）。四个条件逐条可测（`gating.rs` 的四象限），只是状态码照上游。**同一波里不允许出现两种「未配置」状态码** |
| **D6** | 公开回调的顺序**反过来**：先验 state，再判配置；「state 不合法」⇒ **401**（上游是 302 失败重定向） | `routes/composio/callback.rs` | ① 「先 state 后配置」是 `docs/61` §6.5 的 M8-6 行逐字要求（「**公开回调仍须按 state 判**」），也是 ⑨ 那条夹具的前提：它跑在「没配 `COMPOSIO_API_KEY`」的环境里，而夹具的 `status_expected` 是 **401**（上游 handler 在那种环境回 403，夹具的注释说「the precise non-401 code is incidental」—— 两条语义都要做对：没配 key ⇒ state 必然验不过 ⇒ 401；配齐 key + 坏 state ⇒ 也是 401）；② 本仓把「state 坏」判成 **401**（`docs/61` §1.5 / §6.5 的本地契约「不合法 ⇒ 401，不是 404」），四类原因**不**外传（只进日志） |
| **D7** | state **合法**但部署未装配 ⇒ **403**（`CallbackOutcome::NotConfigured`）；其余「state 合法但没成」一律 **302 失败重定向**（含上游不可达） | `callback.rs` / `service.rs` | 上游把**所有**失败都折成 302（注释逐字：the user is never left on a blank API response）⇒ 本片只在「验过 state 之后仍然没这个能力」这一格回 403（复用 D5 的同一格，与 4 条会话路由一致），其余照上游 302。**没有**为上游故障单独开 502（那会把浏览器停在 JSON 上）；故障只影响日志级别 |
| **D8** | 服务**按请求**构造（`service_from(&state)`），不是进程级单例 | `routes/composio/connect.rs` | 与 M8-1 的 `GithubClient::new(github_api_base())` 同款：服务只持有「配置 + 连接池 + 签发器 + auth-config 目录」，没有跨请求的**语义**状态（重放台账在 D4 里已经提到进程级）。**代价**：`AuthConfigDirectory` 的 TTL 缓存只在一次请求内生效（每个请求各解析一次 `/auth_configs`）——语义不变（可见性判据每次都重新解析，上游注释逐字「enabling a toolkit is a dashboard action」），只是多一次上游往返。**登记为 M8-7 尾账**：跨请求缓存需要一个进程级服务槽，而它必须与「`api_base` 注入面」「测试隔离」一起解决 |
| **D9** | `ComposioConfig` 多一个 `state_ttl_secs`（测试注入用；`None` ⇒ `DEFAULT_STATE_TTL_SECS` = 上游 5 分钟） | `service.rs` | anchor 的五个字段与 `is_configured` / `missing` **逐字保留**（`missing()` 的 4 条不许动，`config_requires_all_four_conditions` 仍在）。没有它就只能用真时钟测「过期」那一格 |
| **D10** | `ComposioError` 从 4 支扩到 10 支（+`ToolkitNotSupported` / `ConnectNotSuccessful` / `ConnectionNotFound` / `AccountVerification` / `Upstream{status,context}` / `Store` / `StoreMissing`），并派生 `Clone/PartialEq/Eq` | `service.rs` | 四支（`Transport` / `Unauthorized` / `NotConfigured` / `Malformed`）逐字保留；其余逐条对应上游的导出错误（`ErrToolkitNotSupported` / `ErrConnectNotSuccessful` / `ErrConnectionNotFound` / `ErrAccountVerification`）与本仓的仓储面。**`Upstream` 只带状态码与调用点**：不带响应体、不带 message、不带 URL（`docs/61` §2.4 的 redaction 第 3 条） |
| **D11** | 新增 `create_mcp_session`（服务）与 `auth_headers`（客户端） | `service.rs` / `client.rs` | 上游 `CreateMCPSession` 是 composio 服务「per-user MCP session helper」的正式一员，也是 R-M8-9 尾账要接的那一端：本片把它交付到「**可注入**」（`create_mcp_session` 拿 URL → `build_task_overlay` 造载荷），**不**接 3 处 enqueue（仍是 R-M8-9 的尾账）。`auth_headers` 是 bearer 的**唯一**出口（返回 `Vec<(String,String)>`，不进 `Debug`） |
| **D12** | overlay 载荷里**没有** `headers`（上游 `composioMCPServer` 带 `x-api-key`） | `overlay.rs` | 两个硬约束撞在一起：① `mc-core/src/mcp/overlay.rs` 的合并契约逐字「任何不在 `mcpServers` 下的顶层键只从 agent 侧保留……overlay 今天只携带 server 条目，不得悄悄引入别的顶层键」；② anchor 冻结的 `build_task_overlay(toolkit_slug, session_url, composio_user_id)` 签名**不带** bearer。⇒ 条目的 `x-api-key` 由**接线侧**从 `client.auth_headers()` 取并补进 server 条目（M8-7 尾账）。形状已与 M8-3 的测试夹具逐字一致（`{"mcpServers":{"composio":{"type":"http","url":…}}}`） |
| **D13** | 新增写集外的**测试目录** `crates/mc-http/tests/composio/**`（5 文件）+ 刷新 `crates/mc-conformance/report.json` | 测试 / ⑨ | 测试目录与 `tests/vcs/`、`tests/github/` 同款（新目录，零交集，无需改 manifest）。`report.json` 是**门 ⑨ 的判据文件**（`--check` 逐字节比对）：本片把 ⑨ 唯一那条 M8 fixture 从 `unmounted` 转成 `pass`，不刷它 ⑨ 必红（与 `57798b75`(M7-5)、`e5fe07bc` 同款处置） |

### 23.3 专属 `DoD` 逐条证据（`docs/61` §6.5 的 M8-6 行）

| `DoD` 条目 | 证据 |
| --- | --- |
| **⑨ 那 1 条 fixture 转 pass**（匿名 + 错 state ⇒ **401**） | `report.json` 逐字：`integrations/TestComposioCallbackIsPublic_NoCookieNot401@…:25#1` ⇒ `outcome: pass`、`status_observed: 401`（交付前 = `unmounted` / `status_observed: 404`）。⑨ 总量 `pass 6 → 7` / `unmounted 30 → 29`。路由侧另有 `gating.rs` 的两条：**没配 key** 与 **配齐 key** 两种环境**逐字同为 401** + `the_public_callback_never_falls_into_the_session_gate`（带一个非法用户头也必须给 composio 自己的 401，证明这条路由真的不在会话门内） |
| **四种「未配置」语义逐条** | `gating.rs::every_unconfigured_condition_gates_all_four_session_routes`：`flag 关` / `缺 COMPOSIO_API_KEY` / `缺 state secret` / `缺回调基址` × 4 条会话路由 = **16 格**，逐格断言 403 + `composio_not_configured`（**不是** 401/503，也不是统一的 401）。⚠️ 状态码照上游是 403 而非计划书的 503（D5） |
| **state HMAC 四反例**（篡改 / 过期 / 重放 / 错密钥） | `state.rs` 的单测：`tampered_payload_is_rejected`（改 `u` / 改 `t` 两例）、`tampered_signature_is_rejected_bit_for_bit`（签名差 1 位 + 非 base64 ⇒ `Malformed`）、`expired_state_is_rejected_and_the_boundary_is_inclusive`（`>` 边界：等号有效、+1 秒 Expired，且**过期优先于重放**）、`replayed_state_is_rejected_once`、`wrong_secret_is_rejected`、`malformed_tokens_are_rejected_before_any_comparison`（8 种坏形态）、`the_replay_ledger_is_process_wide_not_per_signer`、`empty_secret_signs_nothing`。路由侧 `callback.rs::broken_states_are_unauthorized` 把 bogus/tampered/expired/empty 四态打通到 401 |
| **toolkit 目录动态解析**（auth-config 未配 ⇒ 不出现） | 纯函数：`catalog.rs::unconfigured_toolkits_are_dropped_entirely` / `narrowing_keeps_order_dedupes_and_lowercases_slugs` / `empty_available_set_yields_an_empty_catalog` / `a_toolkit_with_no_candidates_is_invisible_even_if_ids_are_available`；归约：`custom_auth_config_beats_managed_and_newer_beats_older` / `disabled_and_malformed_configs_are_skipped`。路由侧：`flows.rs::connect_then_callback_then_persist_then_toolkits` 在**真替身**上断言目录只剩 `notion`（`github` 的那条 auth config 是 `DISABLED` ⇒ 整条不出现），且 `toolkits_report_upstream_failures_instead_of_an_empty_catalog` 钉住「解析失败 ⇒ 502，**不**回落成空目录」（上游注释逐字） |
| **离线替身端到端**（connect → callback → 落库 → toolkits，中间零 mock） | `flows.rs::connect_then_callback_then_persist_then_toolkits`：`POST /connect/init`（路由）→ 替身（**真 HTTP**，按 `x-api-key` 分派）收到 `/connected_accounts/link` → 从**真响应体**里取回 `callback_url` 里的 signed state（不是测试自己签的）→ `GET /callback` → 落库 → `GET /connections` / `GET /toolkits`。替身纪律逐条：① 只替平台 wire（替身是「假 Composio」，不是「假 service」）；② 出站逐字段断言（路径 + `x-api-key` 头 + `callback_url` 前缀 `{回调基址}{CALLBACK_PATH}?state=` + `auth_config_id=ac_notion_custom` + `user_id` 不变量）；③ 两个反例必测（同 state 重投 ⇒ 401 且不重复插行；第二次 DELETE ⇒ 204 且**不再**打上游） |
| **`overlay.rs` 交付到「可注入」为止 + 一条纯函数用例** | `overlay.rs::builds_the_claude_style_server_entry` / `the_payload_has_exactly_one_top_level_key`（顶层只有 `mcpServers`；条目只有 `type`/`url`）/ `every_blank_input_means_no_overlay`（三道闸门）/ `the_session_url_is_trimmed_but_otherwise_verbatim` / `an_agents_own_entry_named_composio_is_meant_to_be_overridden`（与 `mc_core::mcp::overlay::merge_task_overlay` 真合并一次）。**取数口的真链**在 `flows.rs::the_service_can_turn_live_connections_into_an_overlay`：真库连接 → `create_mcp_session`（替身回 `https://mcp.example/s/sess_1`，断言 `toolkits.enable` 与 `connected_accounts` 的钉法）→ `build_task_overlay`。**3 处 enqueue 接线不属本片**（R-M8-9：`runtime_mcp_overlay` 在本波结束后仍恒 `NULL`，登记过的缺口） |
| **「未配置 + 未授权」两格逐端点** | 未配置：见上（16 格）；未授权：`gating.rs::the_four_session_routes_are_session_gated_even_when_configured`（4 条匿名 ⇒ 401，且 body 不泄漏配置状态）+ `authentication_precedes_the_configuration_check`（**匿名 + 未配置 ⇒ 401**，与上游 middleware 的顺序一致）+ `flows.rs::connect_init_rejects_unknown_toolkits_and_bad_bodies`（toolkit 不支持 ⇒ 400；body 坏 / 缺 slug ⇒ 400；上游 500 ⇒ 502）+ `disconnect_is_idempotent_and_hides_foreign_connections`（不属于调用者的 id ⇒ 404，与「不存在」**同判**，不泄漏存在性；非 uuid ⇒ 400） |
| **凭据纪律**（手写 `Debug` 脱敏 + 错误路径不回显凭据） | 三个承载密钥的类型全部手写 `Debug`：`ComposioClient`（`api_key` ⇒ `<redacted>`）、`ComposioConfig`（只暴露 `configured` / `missing` / `api_base`）、`StateSigner`（`secret` ⇒ `<redacted>` + 台账条目数）。错误值不回显：`errors_never_echo_credentials`（`ak_test` / `state-secret` 都不出现在 `Transport`/`Upstream`/`Store` 的 Display 与两个 `Debug` 里）+ `check_status_maps_401_and_others_without_echoing_anything`（**只**带状态码与调用点）+ `malformed` 分支的文案里**没有**响应体。库里那 11 列全是不透明标识（bearer 只在会话 URL 路径上） |
| ⑦：5 条路由 = `owners.M8` **精确归零** | 见 23.4（`local 453 → 458`、`implemented 370 → 375`、`known_gap 86 → 81`、`owners.M8 5 → 0`、`baseline 406` **不动**） |
| ⑩：新文件 ≤ 800 行 | 见 23.4 与 23.1.1（两处切分就是为了它）；`scripts/file_size_baseline.tsv` **未动** |

### 23.4 门禁读数（逐字取自当轮日志；日志留档在 run workdir 的 `gates-m8-6.log`）

```text
bash scripts/gates.sh --with-db --db-url 'postgres://mc_lum1803:…@127.0.0.1:5432/mc_lum1803'
  ①fmt 0(3s) · ②build 0(101s) · ③clippy 0(35s) · ④clippy-test-util 0(32s) · ⑤test 0(45s)
  ⑥db 0(112s；migrate=0, e2e=0) · ⑧schema-drift 0(28s) · ⑦route-parity 0(0s) · ⑨conformance 0(56s) · ⑩file-size 0(0s)
  ⇒ overall: PASS — 10/10 gate(s) green in 412s
```

（单跑，`CARGO_INCREMENTAL=0`；起手磁盘 7.0G 可用 —— 门 ② 一度把它压到 4.2G，随后平台 GC 回收
15G 至 19G。**首跑即 10/10**，没有 ENOSPC、没有需要修的门。）

⚠️ **上表是本片分支树上的读数。** 交付时 `origin/feat/multica-rs-initial` 已前进到 `0d4510e4`
（M7-8 的 PR #98 合并 + §107 cycle docs）⇒ 本片**合并**该 tip（`git merge`，只 `docs/32` 一处冲突，
按「先到者保号」把本节让号为 `## 23.`）并在**合并树上整套重跑**：

```text
bash scripts/gates.sh --with-db（合并树 5ef58d5d，与上面同一条命令、同一个库 mc_lum1803；日志 gates-m8-6-merged.log）
  ①fmt 0(2s) · ②build 0(85s) · ③clippy 0(28s) · ④clippy-test-util 0(51s) · ⑤test 0(45s)
  ⑥db 0(233s；migrate=0, e2e=0) · ⑧schema-drift 0(25s) · ⑦route-parity 0(1s) · ⑨conformance 0(56s) · ⑩file-size 0(0s)
  ⇒ overall: PASS — 10/10 gate(s) green in 526s
```

⑦ 在合并树上**逐字相同**（`local 458 / implemented 375 (372+3) / known_gap 81 / owners.M8 0` —— M7-8
是 0 路由），⑨ 的 `report matches` 也相同 ⇒ **合并没有引入任何读数漂移**（§21.4 的「合并树重跑」
同款纪律；区别是那次跑在片自身上、这次是**先合 base 再跑**）。

- ⑦（**逐字实测**；`baseline 406` **不动**，本片**不跑** `--write-baseline` —— 唯一一次刷新归 M8-7）：

  | 口径 | 片前（起手实测） | 片后（交付） |
  | --- | ---: | ---: |
  | `local` | 453 | **458**（+5） |
  | `implemented` | 370 = 367 real + 3 ph | **375 = 372 real + 3 ph**（+5 real） |
  | `known_gap` | 86 | **81** |
  | `owners.M8` | **5** | **0**（精确归零） |
  | `baseline` | 406 | **406（不动）** |

  不变式逐条成立：`implemented + known_gap == 456`（375 + 81 = 456 ✓）、`regression 0`、`unclaimed 0`、
  `local_only 9`（本片**不新增** local_only）。`gaps by owner` 的**交付后**逐字：
  `M9=33 M3+=16 M7=16 M3=11 M10=5`（**M8 已从该行消失** = 0；和 = 81 ✓）。
- ⑦ 第二条（形态）：`slash_alias_audit.py` = `0 defect(s)`、exit 0；`NOT REGISTERED = 0`、
  `registered upstream-key literals = 462 / 0 defect(s)`（本片 5 条**只按上游字面量注册**，
  既没补尾斜杠形态也没漏成带斜杠形态 —— `dual-form required: 0`）。
- ⑨：`report matches crates/mc-conformance/report.json`。**唯一那条 M8 fixture 已转绿**：
  `integrations/TestComposioCallbackIsPublic_NoCookieNot401@server/cmd/server/composio_callback_public_test.go:25#1`
  ⇒ `unmounted → pass`、`status_observed: 404 → 401`；总量 `pass 6 → 7`、`unmounted 30 → 29`、
  `mismatch 23` / `unevaluable 306` 不变（**没有**制造新的 `unevaluable`）。`report.json` 的 diff
  只有 15 行上下（三处计数 + 那一条 fixture 的 `outcome`/`status_observed`/`detail`）。
- ⑩：**0 违规**，`scripts/file_size_baseline.tsv` **未动**。本片最大的四个文件：
  `client.rs` 720、`service.rs` 639、`tests/composio/support.rs` 594、`state.rs` 538
  （其余 ≤ 461）。两处越线（864 / 869）按 23.1.1 切开后全部落回线内。
- ⑧：schema-drift 绿（本片 **0 迁移**、0 表结构改动；`user_composio_connection` 是迁移 `127` 里就有的表）。
- ⑥：`mc-migrate` 566 个迁移 + `--ignored` 全绿。本片新增的真库目标 = `tests/composio/main.rs`
  **15 passed / 0 failed**（1.89s），逐条覆盖上面 23.3 的每一格。
- ⑤（`cargo test --workspace`，**不带**库变量）：全绿。本片新增 **82** 条测试，分布：
  `mc-composio` lib **49**（`client/tests.rs` 8 + `state` 15 + `catalog` 12 + `service/tests.rs` 10 + `overlay` 5；
  lib 总数 1 → **50 passed**）、`mc-repos` lib **4**（`composio/connection.rs`）、
  `mc-http` lib **14**（`routes/composio/{connect,catalog,callback}.rs` 的 4+4+6）、
  `mc-http` 的 `tests/composio` **15**（真库，见 ⑥）。**零 mock**：所有出站都打在真 HTTP 替身上。

- **[lesson·`#[tokio::test]` 的 runtime 生命周期会杀死 `tokio::spawn` 出来的替身]** 替身服务最初用
  `tokio::spawn(axum::serve(...))` 挂在**用例自己的 runtime** 上 —— 每个 `#[tokio::test]`
  各建一个 runtime，**用例一结束就被丢掉**，于是服务任务跟着死，后续用例全部变成
  `error sending request`（表现为「全链路 502」，非常像业务 bug）。修法：替身跑在**自己的 OS 线程 +
  自己的 multi-thread runtime** 上（`std::net::TcpListener` 同步 bind 拿到端口 → `thread::spawn`
  里 `runtime.block_on(serve(TcpListener::from_std(..)))`）⇒ 服务活到进程结束。
  **判据**：任何「跨用例复用」的进程级替身都必须与用例的 runtime 解耦。
- **[lesson·进程级台账与「按请求构造的服务」是**结构性**配套]** 重放台账若留在 `StateSigner` 的字段里
  （看起来更内聚），配上「服务按请求构造」就等于**完全没有重放防护**（每次到达都是新实例）。
  本片把台账提到 `mc-composio::state` 的进程级静态，并**专门写了一条反例用例**
  （`the_replay_ledger_is_process_wide_not_per_signer`）把这个陷阱钉死。**同类判据**：任何
  「一次性 token / 幂等键 / 限流窗」的实现，先问「它的生命周期是否 ≥ 保护对象的生命周期」。
- **[lesson·一个进程级注入槽 + 一把带身份的 key = 零竞态的替身]** M8-1/M8-2 的 `set_*_api_base`
  是按用例 set/reset（并行用例之间有窗口）。本片改成：base **只注入一次**（`OnceCell`），
  替身**按请求的 `x-api-key` 分派行为**，而 key 里嵌着**本用例自己的 user id**
  （`ak_<variant>_<uuid>`）⇒ 替身既能答出「谁在调」也能答出「该走哪个分支」，用例之间
  **零共享可变状态**、可以任意并行。代价是替身要多一次 key 解析。
- **[lesson·门 ⑩ 对「上游 1,050 行 Go + 本仓文档密度」同样是必然触发]** 与 §21.4 同款：本片两处越线
  （864 / 869）在**写之前**就该由体量判死。**本仓的成熟手法**是「`#[cfg(test)] mod tests` 拆到
  `src/<mod>/tests.rs`」—— 它比拆实现更便宜，因为 Rust 2018 的路径规则允许 `src/client.rs` 与
  `src/client/tests.rs` 共存，**连 `mod.rs` 都不用碰**（§23.1.1）。
- **[lesson·上游注释与上游实现打架时，以**实现**为准并登记]** `router.go` 的注释写
  「composio 的 4 条返回 503」，而 `integrations_composio.go` 的文件头写「a non-retryable 403,
  not a transient 503」、代码是 `writeFeatureDisabled` = 403 —— 计划书 `docs/61` §2.5 抄的是前者。
  M8-1/M8-2/M8-6 三片独立撞上同一格并做了同一选择（照实现 + 登记 D5）。**判据**：`docs/61` 里
  逐端点的状态码格子在派发前值得用上游源码复核一遍。

### 23.5 交接（给 M8-7 / 未来碰这条路的切片）

1. **M8-7（INT）请收口六件事**：
   1. **R-M8-9 尾账（本片最大的缺口）**：`attach_runtime_mcp_overlay`（`mc-repos/src/task/overlay.rs`）
       仍是 `todo!()`，3 处 enqueue 接线点逐字 =
       `mc-repos/src/task/store.rs:216`、`mc-repos/src/chat_task/send.rs:136`、
       `mc-autopilot/src/autopilot/run.rs:575`。本片把**取数口**（`create_mcp_session`）与**构建**
       （`build_task_overlay`）都交付到了「可注入」，接线本身只在那一处。接上之前
       `runtime_mcp_overlay` **恒 `NULL`**（登记过的缺口，不是遗漏）。
   2. **D12**：接线时把 `x-api-key` 从 `ComposioClient::auth_headers()` 取出来补进 overlay 的
       server 条目（合并契约不允许**顶层**多键，但 server **条目**里加 `headers` 是允许的 ——
       上游 `composioMCPServer` 就是这么带的）。不补的话，daemon 侧的 MCP 客户端拿不到认证头。
   3. **D8**：是否把 auth-config 目录缓存做成**跨请求**（需要进程级服务槽；必须同时解决
       `api_base` 注入面与测试隔离，本片刻意不做）。若不做，请在 INT 的缺口清单里写明
       「每个 settings 请求各解析一次 `/auth_configs`」。
   4. **D5 的收口**：`owners.M8 = 0` 之后的 ⑦ 基线刷新（`--write-baseline` **406 → 458**）是
       M8-7 的**唯一**一次刷新职责（本片未跑）。若 M8-7 决定把「未配置」改回 503，必须
       **三片一起改**（M8-1 的 repositories / M8-2 的 VCS / 本片的 composio），否则同一波里
       会同时存在 403 与 503 两种语义。
   5. **文案债（1 行）**：`crates/mc-composio/src/lib.rs` 的文件头仍写着
       「`service.rs` / `state.rs` / `catalog.rs` / `overlay.rs` 是**桩**（签名 + `todo!()`）」——
       那是 anchor 冻结文件的过期文案（本片按 §21.5 的先例**不**改冻结文件，只登记）。
       顺手改这一行即可（`lib.rs` 的 `pub mod` / `pub use` 一行都不用动）。
   6. **缺口清单**：`ComposioKeys` 仍没有独立的 **frontend base**（回调重定向走
       `MULTICA_APP_URL` → `FRONTEND_ORIGIN` → 相对路径，与上游 `appURLFromEnv()` 逐字一致）；
       `user_composio_connection` 的 `expired` 状态**没有任何写入者**（上游也没有 —— 过期靠
       Composio 侧，本仓只写 `active` / `revoked`）。
2. **未来任何碰 composio 的切片**：`client.rs` 720 / `service.rs` 639 都离 800 还有余量，但
   `tests/composio/support.rs` 594 已经接近 —— 新增真库用例请按现有轴分流
   （门面矩阵 → `gating.rs`；回调四态 → `callback.rs`；链路与目录 → `flows.rs`；替身道具 → `support.rs`）。
   ⚠️ 替身是**一个 base 服务整个二进制**：新分支请走**新的 `x-api-key` variant**，不要按用例
   set/reset base（那会把 §23.4 那条 lesson 的坑重新挖出来）。
3. **未来任何碰 `mc-composio::state` 的切片**：nonce 台账是**进程级**的（D4），TTL 是
   上游的 5 分钟；改台账粒度前先读 `the_replay_ledger_is_process_wide_not_per_signer` 的注释。

## 24. M7-9（`LUM-1774`）：dingtalk 安装 / 凭据 / 群身份 + agent 群面（7 路由）

> **号段说明**：派发描述 rev 5 写「取 `## 23.`」，**rev 6 已作废并改取 `## 24.`**（`## 23.` 被
> M8-6 / `LUM-1803` 的 PR #99 占；`docs/37` §108 的 lesson「号段不能在 base 上取末号 + 1」）。
> 本节落笔时 `grep -n '^## ' docs/32-M3-DAEMON-FACE.md | tail -1` = `## 23. M8-6（LUM-1803）：composio…`
> （已合进 base）⇒ 本节取 **`## 24.`**。

**口径**：本节一切落点与偏离都在上游 `f41fae6b08fb` 与**本片实测树**上核对。

- **起手 base**：`2d3a5d33`（= 派发描述 rev 5/rev 6 钉的合并点 / PR #98 的 merge）。
- **提交时 origin tip = `1edb3d83`**：`2d3a5d33` → M8-6 PR #99 合并 `f0326bce` → §108/§109/§110
  cycle docs。逐文件差集与本片 21 个文件**零交集**（M8-6 只动 `mc-composio/**` + `routes/composio/**`
  + `tests/composio/**` + `docs/32` §23 + `docs/37` + `report.json`）⇒ 本片**已 rebase** 到
  `1edb3d83`（一次干净 rebase，无冲突），PR 以 `1edb3d83` 为 base。**门禁在 rebase 后的树上跑**
  （读数见 §24.3）。
- ⚠️ **`report.json` 的基线随 rebase 一起前进**：M8-6 的 5 条 composio 路由改变了 stateless 快照
  ⇒ 本片**不**重刷快照（门 ⑨ 的 `--check` 对着 rebase 后的那份跑）。

### 24.1 落点（逐字路径）

| 落点 | 写者 | 上游 |
| --- | :-: | --- |
| `crates/mc-channel/src/dingtalk/mod.rs` | M7-0 建 / M7-7 填 / M7-8 追加 / **M7-9 追加 5 行 `pub mod`** | 本片**只**追加 `binding / client / config / group_identity / install` 五行（788 → **793** ≤ 800）；三个注册入口与 `register*` **一行未动** |
| `dingtalk/config.rs` + `config/tests.rs` | M7-9 | `config.go`（150）的**写侧与公开投影**：`InstallConfig::byo`、`encode_ciphertext` / `decode_ciphertext`（`StdEncoding` + `stripWhitespace`）、`decode_public_config`、`credentials_from_config` |
| `dingtalk/client.rs` + `client/tests.rs` | M7-9 | `client.go`（125）+ `token.go`（201）+ `bot_identity.go` 的**传输**那一半：`Client`（令牌缓存 + 折叠铸造 + `post_json` + 401 ⇒ 作废 + 重试一次）、`message_file_download_url`、`bot_name_in_group`（按 `robotCode` **精确匹配**） |
| `dingtalk/install.rs` + `install/tests.rs` | M7-9 | `install.go`（285）+ `byo_install.go`（100）：`InstallError` 十字变体、`InstallStore` 端口、`InstallService::{list,get_in_workspace,revoke,register_byo}` |
| `dingtalk/binding.rs` + `binding/tests.rs` | M7-9 | `binding.go`（178）：令牌铸造 / 原子兑换（`BindingStore` 端口）、`hash_binding_token`、`random_binding_token` |
| `dingtalk/group_identity.rs` + `group_identity/bot_name.rs` + `group_identity/tests.rs` | M7-9 | `bot_identity.go`（250）+ `handler/dingtalk.go` 的 `listDingTalkGroups` 一族：清单装配（分组 / 排序 / 分页 / 可见性）、查询计划（四条 400 分支）、`BotNameResolver`（三类缓存）、`PresenceObserver` |
| `crates/mc-http/src/routes/channels/dingtalk.rs` | M7-9 | `handler/dingtalk.go`（782）：**7 条路由** + 七个 handler + 端口装配（[`install_service`] / [`group_inventory_store`] / [`resolver_set`]） |
| `routes/channels/dingtalk/{dto,scope}.rs` | M7-9 | 上面两条的**门 ⑩ 切分**：wire DTO / 鉴权与群清单取数 |
| `routes/channels/dingtalk/store.rs` + `store/groups.rs` + `tests.rs` | M7-9 | 六条上游查询的 PG 端口实现（安装 / 绑定面 ∥ 群面）+ 不依赖库的 wire / 状态码用例 |
| `crates/mc-http/tests/channels/dingtalk.rs` + `dingtalk/support.rs` | M7-9 | 7 条路由的**真库**端到端（门 ⑥，`#[ignore]`）+ 装置（`accessToken` 替身、`AppKey` 生成、群观察行） |
| `crates/mc-http/tests/channels/main.rs` | M7-4 建 / **M7-9 追加 1 行 `mod dingtalk;`** | 与 M7-5 追加 `mod telegram;` 同款 |
| `crates/mc-conformance/report.json` | **本片不动** | M8-6 的 5 条路由已经改过它（rebase 带来）；本片 7 条路由的 stateless 层结论**不**因它们而变（见 §24.3 的 ⑨） |

**写集勘误（三类，逐条）**：`docs/60` §3.3 给本片的格子是
`dingtalk/{install,binding,client,config,group_identity}.rs` + `routes/channels/dingtalk.rs`；
派发补充（第二类第 5 次）已追加 `dingtalk/mod.rs`。本片**再追加**的路径只有三类：

1. **`*/tests.rs` 子模块**（`config/` `client/` `install/` `binding/` `group_identity/` +
   `routes/channels/dingtalk/tests.rs` + `tests/channels/dingtalk/support.rs`）：同目录先例
   `slack/*/tests.rs` / `telegram/*/tests.rs`；
2. **门 ⑩ 的 800 行硬限逼出来的切分**：首版成形时 `group_identity.rs` **924**、
   `routes/channels/dingtalk.rs` **1053**、`store.rs` **979**、`tests/channels/dingtalk.rs` **823**
   四文件越限 ⇒ 按**单一接缝**切开：读面（清单装配）∥ 写面 + 平台身份查询
   （`group_identity/bot_name.rs`）、wire ∥ 鉴权/取数 ∥ handler（`{dto,scope}.rs`）、
   安装/绑定 ∥ 群（`store/groups.rs`）、装置 ∥ 用例（`dingtalk/support.rs`）。拆完最大 **793**
   （`mod.rs`），全部 ≤ 800；**未动** `scripts/file_size_baseline.tsv`（仍 22 行）；
3. **`tests/channels/main.rs` +1 行**（`mod dingtalk;`）：M7-5 追加 `mod telegram;` 的同一手法。

### 24.2 偏离（D1…D10，逐条可核对）

- **D1 写集勘误**：见上（三类，逐条点名）。`mod.rs` 的 5 行只追加、不改别人的行、不重排
  `register` / `register_with` / `register_resolvers`。
- **D2 `Decrypter` 的类型留在 `mod.rs`（只追加 5 行的硬预算）**：M7-7 的 `pub struct Decrypter` /
  `StreamInstallConfig` / `resolve_app_secret` 是**已经合入**的接口面，而本片对 `mod.rs` 只有
  **7 行余量**（788 + 5 = 793）⇒ 不搬类型。于是凭据面是**三分法**：**类型**在 `mod.rs`、
  **唯一判据**在 `outbound::credentials::decode_credentials`（M7-8 已合）、**写侧 + 公开投影**
  在 `config.rs`（`config::credentials_from_config` 再导出前者，于是 route 层的唯一入口叫
  `config::…`，实现只有一份）。**收敛票归 M7-21**，与 `docs/32` §22 的 D4 同一张票。
- **D3 agent 级那条路由的落法**（派发补充的「写集标注勘误」逐字）：`GET /api/agents/:id/dingtalk/groups`
  用**完整路径**注册在**本片自己的** `routes/channels/dingtalk.rs` 里，靠 `channels/mod.rs:91`
  的 `.merge(dingtalk::router())` 上台 —— **不** nest 进 `routes/agents.rs`（照抄上游的 nest 会与
  该文件既有挂载点抢同一前缀，axum 0.7 嵌套重叠会 panic）。同形先例：`routes/mcp/agent.rs`
  的 4 条 `/api/agents/:id/mcp-servers*`（M8-3，`agents.rs` 一行没动）。⚠️ 本文件 anchor 期的
  头注表格第 7 行写「（挂在既有 `/api/agents/{id}` 子路由内部）」是**上游 Go 的注册位置**，
  不是本仓的落法 ⇒ 已在 `dingtalk.rs` 的模块文档里显式勘误。
- **D4 `persistInstall` 收成一个端口方法；"死主"判据在 `DELETE` 谓词里**：上游那一个事务的五步
  （advisory lock → 回收死主占的 `(dingtalk, app_id)` 槽 → 读当前机器人身份 → **换机器人时退休
  旧行** → upsert）在 `PgInstallStore::persist` 里逐条照抄。两条**不简化**的地方：① 回收的判据
  写在 `DELETE` 的 `WHERE` 里（不是前置 `SELECT`）⇒ READ COMMITTED 下执行时重查（EvalPlanQual）
  关掉"读-再删"的 TOCTOU；② 依赖行清理跟着**实际被删掉的 id**（`dead` / `retired` CTE）走 ⇒ 只对
  本语句真的删掉的那一行跑（14 张依赖表逐条点名，含 `dingtalk_*` 三张与 `detached_audit`）。
- **D5 `dingtalk_*` 三张表的读写语句落在 `store/groups.rs`**：`crates/mc-repos/src/channel/**`
  对本片**只读**（写集表）⇒ 它们以端口实现的形态落在 `mc-http`（与 M7-4 / M7-5 的三条上游查询
  同手法）。**M7-7 的两个诚实默认值在这里换成真实现**：`NoGroupPresence` → `PresenceObserver`
  （写 `dingtalk_group_presence` + `dingtalk_bot_identity`，**尽力而为**：失败只记 warn、绝不改判决）、
  `NoBotName` → `BotNameResolver`（三类缓存：成功 1h / 其它失败 1min / **权限拒绝 30s 且跨群共享**）。
  `resolver_set(state)` 是这套装配的唯一入口（宿主在 `apps/mc-server/src/channels.rs` 调一次，
  那一步是 anchor 写集 ⇒ 登记缺口）。
- **D6 随机性**：上游 `crypto/rand` 读 32 字节；本 crate 的依赖集里**没有** `rand`
  （`docs/60` §3.1 冻结）⇒ 取两个 v4 UUID（各 122 bit 熵）拼成 32 字节 —— 熵量级同级、**线长相同**，
  只是那 32 字节不是均匀分布。与 M7-5 的 `random_binding_token` 同一处取舍。
- **D7 未配置分支先于鉴权（跨片约定，**逐端点对齐**）**：上游 `ListDingTalkInstallations` /
  `ListDingTalkGroups` 的第一句都是 `if h.DingTalkInstall == nil { 200 空; return }` ⇒ 本仓也用
  `Option<AuthUser>` 让**未配置**分支先返回。**agent 级那条是唯一的例外**：上游把
  `loadAgentForUser` + `canAccessPrivateAgent` 放在 nil 判断**之前** ⇒ 认不出的 agent 是 404、
  看得见但未配置才是 200 空（顺序**逐字**照抄，有用例钉住）。这条顺序是 ⑨ 的
  `TestListTelegramInstallationsNotConfiguredReturnsEmpty` 那一族（M7-5 的 D1）**唯一**可达的形态。
- **D8 客户端两份实现并存**：`outbound/openapi.rs` 的 `HttpOpenApi`（M7-8 已合、冻结）**保留**；
  本片的 `Client` 是**完整实现**（多出安装校验与 bot 名查询两条调用），并且**同样** `impl
  OpenApiTransport` ⇒ 宿主在 `DingTalkDeps::with_outbound` 处换成 `Arc::new(client::Client::new())`
  即可（`docs/32` §22 的 D3 交接项逐字：**换实现即可，语义一行不动**）。两条语义上有专门用例：
  `minting_caches_per_app_key_and_never_repeats_a_hit` / `a_401_on_a_lookup_invalidates_and_retries_exactly_once`。
  `envelope_error` 是 **本地副本**（平台错误体可能回声请求体 ⇒ 只取 `code`；与 M7-8 同款）。
- **D9 `url_encode` 空格 → `+`**：上游 `dingtalk/replier.go:195` 用 `url.QueryEscape` ⇒ 本文件的
  绑定链接转义与 `telegram` / `slack` 两侧**同一份实现**（各自持有一份，本仓既有约定）；
  测试同时钉住 `+/=` 与空格两种字形。
- **D10 群观察的写入语义（`ON CONFLICT` 的逐条对齐）**：标题 / `bot_name` / `bot_identity_issue`
  只在**新值非空**时覆盖（旧值保留）；`first_seen_at` 取更早（由 DB 的默认值 + 触发器兜住）；
  每次观察 `last_active_at = now()` 且 `mention_count + 1`；`workspace_id` 由**安装行**推导
  （不信调用方给的）。`dingtalk_bot_identity` 的写**独立成一条命令**（上游逐字给的理由是
  mixed-version 触发器要能观察到已完成的那条命令）。

### 24.3 门禁与读数（**rebase 后的树上实测**）

```
⑦  upstream 456 (f41fae6b08fb) | local 458 → 465 registered | baseline 406（不刷）
   起手 1edb3d83 : implemented 372 real + 3 placeholder = 375 / 456 | known_gap 81 | unclaimed 0 | regression 0 | local_only 9
                  gaps by owner: M9=33  M3+=16  M7=16  M3=11  M10=5
   本片落地树     : implemented 379 real + 3 placeholder = 382 / 456 | known_gap 74 | unclaimed 0 | regression 0 | local_only 9
                  gaps by owner: M9=33  M3+=16  M3=11  M7=9  M10=5     ← Δ=+7，owners.M7 16 → 9（与 rev 5/6 的预测一致）
形态门  declared 24 / dual-form 0 / 0 defect（exit 0）；local 实况 0 defect（exit 0）
⑩     新文件最大 793（dingtalk/mod.rs）；scripts/file_size_baseline.tsv **未动**（仍 22 行）
⑨     `pass 7 / mismatch 23 / unmounted 29 / placeholder 0 / unevaluable 306`（rebase 后的快照，
      含 M8-6 的 2 条）+ `report matches crates/mc-conformance/report.json` ⇒ **本片不刷快照**
```

**⑨ 的真库层（本片的专属验收：4 条 `unevaluable` 必须转 evaluable 并给实测结论）**

命令：`cargo run -q -p mc-conformance -- --filter DingTalk --db-url <mc_lum1774>`（`--json`，**不**
`--write`）⇒ `fixtures 5 · pass 3 · mismatch 2 · unmounted 0 · placeholder 0 · **unevaluable 0**`。

| fixture | 路由 | 期望 | 真库层实测 | 结论与归因 |
| --- | --- | ---: | --- | --- |
| `…/integration_test.go:786#6` | `GET …/dingtalk/group-routes`（**已退役**） | 404 | **404 pass** | 反向验收成立（§24.2 的 D3 / `docs/60` §1.6） |
| `…/integration_test.go:792#7` | `GET …/{testWorkspaceID}/dingtalk/groups` | 200 | **200 pass** | 未配置 → 200 空（`group_discovery_supported:true`） |
| `…/handler/dingtalk_test.go:642#11` | `GET …/{id}/dingtalk/groups` | 200 | **200 pass** | 同上（该 fixture 走 `handler` 面，回放时也过 router 中间件） |
| `…/integration_test.go:811#5` | `GET /api/agents/:id/dingtalk/groups` | 200 | **404 mismatch** | 归因 = **回放数据**：fixture 的 agent id 是上游测试**当场创建**的行，回放库的种子 workspace 里没有它 ⇒ 上游在同样前提下也是 404（v5 → v6 都只承诺"转 evaluable + 给结论"，**不**承诺 pass） |
| `…/integration_test.go:977#8` | `GET /api/workspaces/d1474000-…/dingtalk/groups` | 404 | **200 mismatch** | 归因 = **未配置分支的先后**（D7）：本条 fixture 的前提是"集成**已配置**"（那时 `workspaceMember` 先跑 ⇒ 非成员 404）；本仓回放环境的落库密钥未配置，而上游在未配置时**也**先回 200 空 ⇒ 不是实现缺陷。⑨ 的 `d1474000-…` 与种子 workspace 不同 ⇒ 若改成"先鉴权"这一条会 pass、但 lark/wecom 那一族（7+4 条未配置 fixture）会全线变红 —— 取舍见 D7 |

> **纪律check**：这两条 mismatch **没有被改写成 pass**（`docs/57` §6.2 / `docs/60` §6.2 的明文要求），
> 也**没有**为了它们去改实现顺序 —— 反例（`#5` 的 agent 门 / `#8` 的未配置优先）都在 `tests.rs`
> 里有**正面**用例钉住：`the_agent_level_gate_runs_before_the_unconfigured_branch`。

**用例账（全部实跑）**

- `cargo test -p mc-channel --lib dingtalk::` ⇒ **220 passed / 0 failed**（M7-7 的 95 + M7-8 的 80 +
  **本片 45**）；`cargo test -p mc-http --lib routes::channels::dingtalk` ⇒ **11 passed**；
- 门 ⑥ 的真库 e2e：`crates/mc-http/tests/channels/dingtalk.rs` **7 例全绿**（未配置矩阵 /
  反向验收 404 / **agent scope 矩阵 4 格 + `public_to` 一格** / BYO 往返 + 密文落库 + 冲突分类 /
  群清单 + 摘群 + 非活跃分页 / agent 级只回自己的 agent / 绑定兑换四判决）。

**门禁汇总（rebase 后实测，逐 gate 的 `GATE_*_EXIT`）**

```
bash scripts/gates.sh --with-db   →  overall: FAIL — 9/10 gate(s) green in 582s
①fmt 0   ②build 0   ③clippy 0   ④clippy-test-util 0   ⑤test **101**   ⑥db 0（migrate=0, e2e=0）
⑦route-parity 0   ⑧schema-drift 0   ⑨conformance 0（63s）   ⑩file-size 0（1s）

bash scripts/gates.sh --only test →  overall: PASS — 1/1 gate(s) green in 48s
                                    GATE_TEST_EXIT=0，126 个测试二进制全部 `test result: ok`
```

> ⚠️ **⑤ 第一次红的归因（**不是**本片的回归，且**不是**随机噪声）**：红在
> `crates/mc-composio/src/state.rs:390` 的 `state::tests::tampered_signature_is_rejected_bit_for_bit`
> —— **M8-6（PR #99，已合）** 的用例，与本片 21 个文件**零交集**。
>
> **触发条件**：32 字节 HMAC 的 `base64url_nopad` 恰 **43** 字符（末字符只承载 **4** 个有效 bit
> ⇒ 取值恰 16 种）；该用例取签名的**末字符**做 `A→B`。当它本就是 `'A'` 时，`'B'`(索引 1) 令低
> 2 个 bit 非零 ⇒ base64 的 `URL_SAFE_NO_PAD`（`decode_allow_trailing_bits: false`）报
> `InvalidLastSymbol`，于是 `state.rs:213` 的 `.map_err(|_| Malformed)` **先于** 216 行的常量时间
> 比较命中 ⇒ 返回 `Malformed` 而断言写的是 `Err(Tampered)` ⇒ **必然失败**（末字符不是 `'A'` 时
> 才绿）。本机实测：20000 次随机签名里末字符 = `'A'` 的 **1184 次（5.9%）**；该用例 12 次单跑
> **失败 1 次**。⇒ **~6% 概率**的数据相关 flake（按 `docs/37` §109「⑥ 的红有四种来源」的同一思路
> 分类结清：**不是**我的改动、**不是**磁盘、**不是**复用库）。
>
> **修法（一行，归 mc-composio 的写者 / M8-7 INT）**：翻**非末位**字符（例如把 `flipped.pop()`
> 换成对首个字符取反），而不是翻末字符。本片**不改**别人的文件（一格一写者的纪律）；⑤ 单跑
> 复现为绿（48s / 126 target）。
>
> **更正记录**：本节初稿把机制写成"末字符 `A`/`B` 解码出同一份字节、签名照旧成立"——那是
> **宽松解码器**（如 Python 的 `base64.urlsafe_b64decode`）的语义；本仓走的是 Rust `base64` 的
> **严格**引擎（拒绝非规范尾比特）⇒ 如实是上面那条 `Malformed` 路径。同轮在飞的 **M8-7
> （`LUM-1804`）** 独立测到同一统计（3/60）并给出同一条机理，两条记录互相印证；本节按它更正。

### 24.4 交接（给 M7-14 / M7-15 / M7-21 INT / 宿主）

1. **给 M7-14（lark，5 路由）与 M7-15（wecom，4 路由）**：本片把"未配置语义**逐端点对齐**"
   的两种形态都跑通了 —— 列表/群清单 = **200 空**（D7 的顺序理由逐字可复用）；撤销 / BYO /
   兑换 = **403 `<platform>_not_configured`**。**不要**统一成 503。另外三件可直接抄的：
   ① `PgInstallStore::persist` 的五步（advisory lock → 死主回收 → 身份读 → 换机器人退休 → upsert）；
   ② 绑定面的"三段一个事务 + 令牌表按 `channel_type` 收窄"；③ `tests/channels/dingtalk/support.rs`
   这套"装置与用例分开 + 每轮新 `AppKey`"的写法（固定 `app_id` 会让重复运行互相占 `(type, app_id)` 槽）。
2. **给 M7-21（`LUM-1786`）· 需要你收口的三件**：
   ① `owners.M7` 从 9 继续往下（余下 9 条 = lark 5 + wecom 4；**本片 7 条已清零**）；
   ② ⑦ 基线 `--write-baseline` **406 → 全波落地值**（本片**未**刷；rebase 后的快照是 M8-6 的那份）；
   ③ 在 INT 报告里复述 §24.3 的两条 mismatch 归因（**回放数据**与**未配置优先**）与 D7 的取舍。
3. **给宿主（`apps/mc-server/src/channels.rs`，anchor 写集）**：本片新增**一个**装配入口
   `mc_http::routes::channels::dingtalk::resolver_set(&state)`（`Option<DingTalkResolverSet>`，
   `None` = 未配置 ⇒ 该平台整体不装配）。它一次把 M7-7 的两个诚实默认值换成真实现
   （`PresenceObserver` + `BotNameResolver`），并返回可直接交给
   `mc_channel::dingtalk::register_resolvers(&router, set)` 的集合。**仍缺**的装配项（逐条登记，
   与 M7-7/M7-8 的清单相加）：`register_with(&registry, &DingTalkDeps)`（`with_decrypter` /
   `with_bot_names` / `with_outbound`）、`with_replier` / `with_media` / `with_typing`、
   `ReplySourceCache` 的写入点。
4. **给后续任何写 `dingtalk/` 的片**：本片对 `dingtalk/mod.rs` **只追加了 5 行 `pub mod`**
   （788 → 793，余量 **7** 行）⇒ 再往里加装配代码会撞门 ⑩；`Channel::send` 的单点替换仍是
   `mod.rs` 里那一个委托。SQL 一律落在 `routes/channels/dingtalk/**`（`mc-repos/src/channel/**`
   对本波只读）。

### 24.5 本片的 lesson

- **【lesson·门 ⑩ 的硬限会在**同一片里**逼出三处切分，而"先写后拆"要按**单一接缝**切】**
  本片首版四文件越限（924 / 1053 / 979 / 823）⇒ 全部按"读面 ∥ 写面"、"wire ∥ 鉴权 ∥ handler"、
  "安装/绑定 ∥ 群"、"装置 ∥ 用例"四条**语义接缝**切开，而不是按行数均分。判据：**每一处切分
  都要能一句话说清"这两半各自回答什么问题"**；说不清就说明该文件其实只有一个关注点，应当先去
  掉重复（本片顺带用"抽 `fx.request(...)` 助手"消掉了 4 处七行样板）。
- **【lesson·`clippy` 的 pedantic 在**新文件**上会挑三类"结构"而不是"风格"】** 本片踩到四条：
  `too_many_arguments`（上游一条 SQL 的七个 `sqlc.arg` 收成 `PresenceQuery`）、
  `needless_pass_by_value`（只读遍历的参数要按值收下并 `for … in vec` 消费掉）、
  `assigning_clones`（`x = y.clone()` 要写成 `x.clone_from(&y)`）、
  `implicit_hasher`（收 `HashSet` 的参数要泛化 `S: BuildHasher` —— 连带**测试**里要写
  `assemble_inventory::<RandomState>(…)`，否则类型推不出来）。**这四条都不该开豁免**：它们各自
  指向一处真的可以更好的形状。
- **【lesson·"门 ⑥ 的红"要先分类，别先怀疑代码】** 本轮 ⑥ 第一次红是 **ENOSPC**
  （`couldn't create a temp dir` + `ld terminated with signal 7 [Bus error]` —— 链接器写不出输出），
  不是用例失败：本片与另一个 workdir 的 `target/` 同时在长，`/` 只剩 4.9G。教训与 `docs/32` §22
  的观察项合并成一条可执行判据：**`df` 低于 ~10G 不要开 `--with-db`**；`--with-db` 前后各看一次
  `df`，并把"测试库是否重建 / 是否同库第二跑"写进报数（`docs/37` §109 的"⑥ 的红有四种来源"
  在本片又一次成立）。
- **【lesson·号段要在"全局最大"上取，而不是 base 的末号 + 1】** 派发 rev 5 说"取 `## 23.`"，
  5 分钟后被 rev 6 推翻（`## 23.` 被 PR #99 的在飞分支占了）。base **看不见**"已开 PR / 在飞未
  提交"的占号 ⇒ 起手要同时看**在飞分支的 workdir**（`git -C <workdir> log --oneline -1` + 它的
  `docs/32` 末号）。这条已记 `docs/37` §108，本片是它的第一个受益者（改号成本 = 0，因为还没动 docs）。
