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
