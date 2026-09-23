# M3-4 runtime 台账（LUM-1427）：runtime-profile 6 条 + runtimes 台账 9 条

本文件是 M3-4 切片的落地记录。范围**只有**这 15 条路由（`runtime-profile` 台账 + 运行时实例
台账 + 用量聚合）；8 条异步往返（`/api/runtimes/{runtimeId}/{update,models,local-skills*}`）属 M3-7（W3c），
本片**不注册**。上游逐字清单是 `docs/fixtures/upstream-routes.tsv`（commit `f41fae6b`，472 行，
owner 列 `M3`）：台账面在 L325-341、profile 面在 L431-436；切片内的二分工
（本片 = 15 条同步台账面，M3-7 `LUM-1438` = 8 条异步往返，写在同一份
`crates/mc-http/src/routes/runtimes.rs` 上 ⇒ **M3-4 先合**）见 `docs/37-M3-W3C-PREFLIGHT.md`
§12.1 与 §3 的共享面表。

| 项 | 值 |
| --- | --- |
| issue | **LUM-1427**（parent epic LUM-1334） |
| 分支 | `agent/devbox5/1d45bdfbd5dc`（托管工作区分支）→ PR 目标 `feat/multica-rs-initial` |
| 基线 | `feat/multica-rs-initial` @ `617036e`（M3-0 空锚点 + M3-1/M3-2/M3-5 已合入） |
| 上游对照 | `louloulin/multica` @ `f41fae6b`：`server/internal/handler/{runtime,runtime_profile,runtime_blocking_agents,runtime_unusable_notice}.go`、`server/internal/service/runtime_teardown.go`、`server/pkg/db/queries/{runtime,runtime_profile,runtime_usage}.sql`、`server/cmd/server/router.go` L1680-1720 / L2266-2293 |
| 领域依赖 | M3-2（LUM-1408）交付的 `mc-runtime`（adapter / profile / quota / liveness）—— 本片**只消费** |
| 门禁 | `bash scripts/gates.sh --with-db` **10/10 全绿** |

## 1. 结论

1. **15 条路由全部从 gap 变为真实实现**，写在 M3-0 的空 router 锚点上。注册键 **18** 条
   （15 条上游键 + 3 条尾斜杠别名，见 §2）；`mount.rs` 只把 `mount_slice_runtime()` 从
   `not_implemented` 占位改成 `super::runtimes::router()`。
2. **分层不乱**：`mc-repos` **不**依赖 `mc-runtime`。仓库层只认表（`agent_runtime` /
   `runtime_profile` / `agent` / `agent_task_queue` / `task_usage*`），
   `omp → pi` 这类**协议族推导**留在 HTTP 路由层（§3）。
3. **门 ⑦ 与基线**（实测）：`local 174 registered`（基线 `156 → 174`）、
   `implemented 143 real + 10 placeholder = 153 / 456`、`known_gap 303`、
   `unclaimed 0`、`regression 0`、`local_only 11`；`OK: every upstream route is either
   implemented or owned`。基线文件在**本 PR 内**重生成（`--write-baseline`），
   依据是集成周期交接的授权（覆盖 issue 里「不要动 ⑦/⑨」的默认限制）。
   刷基线的理由不是「不刷会红」（⑦ 只对**丢**路由判红）而是**给新增的 18 条契约上锁**
   —— `docs/37-M3-W3C-PREFLIGHT.md` §12.1.3 的实验已证伪「不刷会红」。
   另注：⑦ 的 `real` 口径**偏高**（`routes/issues/mod.rs` 里仍有指向 501 `not_implemented`
   的注册被算成 `real`，§12.1.6）—— 本片这 15 条是真实现，handler 语义以 e2e / ⑨ 为准。
4. **门 ⑨ 无需重生成**：`crates/mc-conformance/report.json` 的 58 条 golden fixture 里
   没有 runtimes 家族的请求，`--no-db --check` 实测 `report matches`。
5. **测试 35 条**：`mc-repos` 10（纯函数 + 真库 `#[ignore]`）+ `mc-http` e2e 14（真库）
   + `mc-http` lib 内 11 条 `routes::runtimes::*` 纯函数（协议族 / 拒绝体键集 / 时间窗口）。
6. **两条删除路径语义不同，不能合并**：`DELETE`（严格）在有活跃 agent 时 **409** 并回
   结构化挡路清单；`POST …/unbind-agents-and-delete`（确认）按用户确认过的 agent 快照
   （`expected_active_agent_ids`）比对后才拆，快照不一致 → **409** `runtime_delete_plan_changed`。
   profile 派生出来的实例**不允许单独删**（§4.6）。

### 交付文件（行数实测，全部 ≤ 800 —— 门 ⑩）

| 文件 | 行数 | 说明 |
| --- | --- | --- |
| `crates/mc-repos/src/runtime.rs` | 52 | 模块根：thin hub（`mod` + `pub use` + 列常量） |
| `crates/mc-repos/src/runtime/ledger.rs` | 467 | 台账读 / 改名 / 可见性 / 活跃 agent 快照 |
| `crates/mc-repos/src/runtime/profiles.rs` | 438 | profile CRUD + 级联删除判定 |
| `crates/mc-repos/src/runtime/teardown.rs` | 401 | 拆除事务（严格删除 + 解绑删除） |
| `crates/mc-repos/src/runtime/usage.rs` | 322 | 四个用量聚合查询 |
| `crates/mc-repos/src/runtime/tests.rs` | 728 | PG 集成测试（9 条，8 条 `#[ignore]`） |
| `crates/mc-repos/src/runtime/tests/scope.rs` | 80 | 可见性/`usable_by` 真库测试 |
| `crates/mc-http/src/routes/runtimes.rs` | 116 | 模块文档 + `router()`（18 键） |
| `crates/mc-http/src/routes/runtimes/access.rs` | 206 | 成员/角色校验、runtime 载入、`decode_body` |
| `crates/mc-http/src/routes/runtimes/dto.rs` | 310 | 响应 DTO + 请求体 |
| `crates/mc-http/src/routes/runtimes/refusals.rs` | 528 | 三类 409 拒绝体 |
| `crates/mc-http/src/routes/runtimes/profiles.rs` | 319 | 6 条 profile 路由 |
| `crates/mc-http/src/routes/runtimes/ledger.rs` | 400 | list / PATCH / DELETE / unbind 删除 |
| `crates/mc-http/src/routes/runtimes/usage.rs` | 297 | 4 条用量路由 + `days`/`tz` 解析 |
| `crates/mc-http/src/routes/runtimes/protocol.rs` | 81 | `protocol_family` / `runtime_type` / `launch_header` |
| `crates/mc-http/tests/runtimes/support.rs` | 428 | e2e 脚手架（连接 / `AppState` / 种子 / 请求） |
| `crates/mc-http/tests/runtimes/profiles.rs` | 450 | 5 条 profile e2e |
| `crates/mc-http/tests/runtimes/ledger.rs` | 775 | 7 条台账 e2e（含两条删除路径 + 别名） |
| `crates/mc-http/tests/runtimes/usage.rs` | 243 | 2 条用量 e2e |
| `docs/fixtures/route-parity-baseline.json` | 181 | ⑦ 基线 156 → 174 |

## 2. 运行时与测试约定

**鉴权**：沿用 M1/M2 的 dev-mode 约定 —— 当前用户来自 `X-Multica-User-Id`（`AuthUser` 提取器），
workspace 来自 `X-Workspace-ID` header 或 `?workspace_id=`（`issues/context.rs::resolve_workspace`）。
**不用** `middleware::authn` 的 `require_*` 守卫：那套认 `x-multica-session` / cookie，
与本仓既有 e2e 约定分叉，且会把「非成员 404」变成 401。profile 路由的 workspace 来自**路径**
`:id`（不是 header），成员校验先于 profile 查询，所以「别人的 workspace + 存在的 profileId」
同样是 404 而不是数据泄露。

**状态码**：未认证 **401**、非成员 / 资源不可见 **404**、角色不足 **403**、重名 **409**、
活跃 agent 挡删除 **409**。profile DELETE 成功 **204**；runtime DELETE 成功 **200
`{"status":"ok"}`**（上游如此，不是 204）。

**尾斜杠**：上游是 chi 的 `Route("/api/runtimes") + Get("/")` 写法，客户端带不带斜杠都能命中。
`matchit 0.7.3` 把尾斜杠当**有效段** —— 树上只有 `/api/runtimes/` 时 `at("/api/runtimes")`
返回 `Err(MissingTrailingSlash)`，`axum-0.7.9/src/routing/path_router.rs` 把它并入 `Err(...)`
⇒ **404**（不是 307 重定向）。因此这里对 `GET /api/runtimes` 与
`PATCH|DELETE /api/runtimes/:runtimeId` 各注册两种写法（`inbox.rs` 同款），
`tests/runtimes/ledger.rs::trailing_slash_and_bare_paths_hit_the_same_handlers` 是唯一能看见
这个故障的测试（⑦ 会折叠两种写法，看不见）。profile 路径**没有**别名：
上游 `Get("/runtime-profiles")` 挂在 `/{id}` 子路由上，是精确路径。

**门 ⑤ / ⑥ 分工**：⑤ `cargo test --workspace` **不带** `MULTICA_TEST_DATABASE_URL`
（`smoke` 会重跑迁移）；⑥ 用 `MULTICA_DATABASE_URL` 跑 `mc-migrate run --dir migrations`
再用 `MULTICA_TEST_DATABASE_URL` 跑 `--ignored` 真库测试。本片 e2e 全部落在 ⑥：

```text
MULTICA_TEST_DATABASE_URL=postgres://… \
  cargo test -p mc-http --test runtimes --features test-util -- --ignored
```

未设置数据库变量 → 每条用例 `return`（普通 `cargo test` 不红）；**设了却连不上 → panic**
（库坏了不许静默假装绿）。

**测试造数约束**（上游不变量，fixture 里最容易踩）：
`agent_task_queue_active_requires_runtime`（`runtime_id IS NOT NULL OR completed_at IS NOT NULL`）
⇒ 在飞行的任务必须有真 runtime；`agent_task_queue.issue_id` 是 NOT NULL
⇒ 种一条任务要顺带建占位 issue；`agent_runtime` 唯一键是
`(workspace_id, daemon_id, provider)` ⇒ 「同一台机器上的多行」要用不同 provider；
「另一个 workspace 的成员」必须真建第二个 workspace（`member.workspace_id` 有外键）。

## 3. 分层与 `protocol_family` 推导

`mc-runtime`（M3-2）提供 adapter / profile / quota / liveness 抽象，本片**只消费**它产出的
表与行结构，不新增依赖边：`mc-repos` 不认识 `mc-runtime`（`cargo tree` 上仍无这条边）。
`runtime_profile.protocol_family` 的取值必须来自 `agent.AgentType` 的白名单，
而「`omp` 在协议层就是 `pi`」这类映射属于**传输层事实**，所以放在
`crates/mc-http/src/routes/runtimes/protocol.rs`：

- `runtime_protocol_family(runtime_type)`：先过 `AgentType::parse` 的 25 项白名单，
  再把 `omp` 归一成 `pi`；白名单外的值 ⇒ `None`（400）。
- `profile_runtime_type(runtime_type, protocol_family)`：`runtime_type` 为空时回落成
  `protocol_family`（上游 `CreateRuntimeProfile` 同款）。
- `launch_header(provider)`：上游 `launchHeaders` 的**用户可见启动骨架**逐字复刻
  （`claude (stream-json)` / `codex app-server` / `omp (json mode)` …），
  是 `AgentRuntimeDto.launch_header` 的取值；未知 provider 回空串（不编造）。

## 4. 有意偏离（逐条，附理由）

1. **错误体形状**：本仓标准体是嵌套 `{"error":{"code","message"}}`（`mc-errors`），
   上游是扁平 `{"error":"msg"}`。**例外**：前端要按 `code` 分支的六个 409 用扁平体，键集逐字对齐
   上游（`error` · `code` · 结构化伴随字段）：
   `runtime_has_active_agents`（+`active_agents` · `active_agent_count` · `active_agents_truncated`）、
   `runtime_delete_plan_changed`、`runtime_delete_not_drained`、`runtime_delete_workspace_mismatch`、
   `runtime_profile_has_active_agents`、`runtime_profile_instance_delete_unsupported`
   （+`profile_id` · `profile_name` · `runtime_status` · `active_agent_count` · `auto_cleanup_after_days`）。
   其余一律走嵌套体。这是全仓既有约定（M2 起），改它会动所有切片，本片只在
   `docs/40-M3-5-AGENTS.md` §5 与本文登记。
2. **`Json<T>` vs `Bytes`**：`axum::Json` 对类型不符回 **422**（上游 `json.Decode` 回 400）。
   本片所有写路由统一走 `access.rs::decode_body`（`Bytes` + `serde_json::from_value`），
   body 非法 ⇒ **400 `invalid request body`**。上游 `UpdateAgentRuntime` 的文案是
   `invalid JSON body`，本仓不逐字复刻文案（契约面是状态码）。
3. **非法 UUID 一律 400**：`runtime_id must be a uuid` / `profile id must be a uuid`，
   在**读资源之前**返回（不泄露存在性）。上游用 `parseUUIDOrBadRequest` 同款做法。
4. **`PATCH /api/runtimes/:id` 无斜杠别名**：见 §2，这是**新增**的注册键（上游只有
   `Route("/api/runtimes/{runtimeId}")` 的单键），为了让 chi 客户端的两种写法都命中。
5. **删除成功的体**：runtime 严格删除成功回 **200 `{"status":"ok"}`**（上游 `writeJSON` 同款），
   profile 删除回 **204**（上游 `w.WriteHeader(http.StatusNoContent)`）。
6. **profile 实例的预检在事务外**：上游在 `DeleteAgentRuntime` 里先
   `runtimeLiveProfile` + `profileInstanceDeleteRefusal` 再开事务；本片同样在路由层先判
   （**先在** `canEditRuntime` 之后、`delete_strict` 之前），而活跃 agent 的判定在
   `mc-repos::runtime::teardown::delete_strict` 的**事务内**（`LockAgentRuntime` →
   再查一次活跃 agent，避免 TOCTOU）。因此「precheck 用的是事务外的快照」是本片的
   已知偏离；它与上游一样是 fail-closed（事务内二次判定同样拒绝）。
   附带的错误类型偏离：普通路径按仓约定走 `crate::workspace::map_sqlx_err`
   （`ledger` / `profiles` / `usage`），但**拆除事务**必须把
   「活跃 agent 快照 / 计划漂移 / 未排空 / 跨 workspace 绑定」四类业务失败与真正的
   DB 错误分开，`RepoError{NotFound,Conflict,Db}` 表达不了 ⇒ `teardown.rs` 自带
   `DeleteRuntimeError` / `TeardownError`（各自的 `Db(String)` 才是真 DB 错误）。
7. **`apply_to_machine` 的作用域**：普通成员只能重命名自己在同一 `daemon_id` 上的行
   （`owner_filter = member.user_id`），owner/admin 重命名该 daemon 上的全部行
   （`owner_filter = None`）。上游 `UpdateAgentRuntime` L489-630 同款。
   `set_custom_name_by_daemon` 的 SQL **只按 `daemon_id`** 过滤（不按 provider），
   所以「一台机器上的多行」会被一起改名。
8. **没有 WS 广播**：上游拆完 runtime 会 `PublishRuntimeTeardown`（每个解绑 agent 一条
   `agent:status`、每个暂停的 autopilot 一条 `autopilot:updated`、取消的任务走
   `BroadcastCancelledTasks`、最后 `daemon:register` 带 `{"action":"delete"}` 让前端重取运行时列表）。
   本仓的 `mc-routes` 尚未接这套 publish 面（M3-5 agent 面同样没有），
   因此前端要等下一次轮询/刷新。这是**跨切片缺口**，登记在案而不是本片私补。
9. **用量时区**：`?tz=`（`chrono-tz`）→ 冷读 `user.timezone` → `"UTC"`，**从不报错**
   （上游 `resolveViewingTZ` 同款）。`days=N` 生成 **N+1** 个日历桶（本地午夜对齐，
   上游 `sinceFromDays` 故意留的余量，不要收紧）；`days=bad` 静默回落默认窗口
   （`/usage` 90 天，`by-agent` / `by-hour` 30 天，`activity` 无窗口）。
10. **`created_by` 恒为 null**：本仓 repo 层 INSERT 不写 `created_by`
    （上游写入创建者）。DTO 保留该键以免前端按字段存在性分支。
11. **`visibility` 硬编码 `'workspace'`**：`runtime_profile` 的 INSERT 不接收可见性参数，
    与上游建表默认一致；DTO 仍按下发。
12. **`runtime_profile` 没有 workspace 外键**（迁移 120 的应用层策略），
    所以测试清理必须显式 `DELETE FROM runtime_profile WHERE workspace_id = …`。

## 5. 覆盖缺口与遗留

- **本片不注册的 8 条上游路由**（`docs/fixtures/upstream-routes.tsv` L330-338，
  切片内归 M3-7 / `LUM-1438`）：`POST /api/runtimes/{runtimeId}/update`,
  `GET …/update/{updateId}`、`POST …/models`、`GET …/models/{requestId}`、
  `POST …/local-skills`、`GET …/local-skills/{requestId}`、`POST …/local-skills/import`、
  `GET …/local-skills/import/{requestId}` —— 它们会发起异步往返
  （`Initiate*`），需要的 task token / daemon 回调面在 M3-7。⑦ 里它们仍是 `known_gap`。
- **③④ 门对测试也生效**：`--all-targets` + pedantic=warn ⇒ e2e 里超过 100 行的
  「按调用顺序平铺」断言函数要显式 `#[allow(clippy::too_many_lines)]`
  （本仓既有约定，`tests/agents/*` 同款）。
- **⑩ 只扫 `git ls-files`**：未 `git add` 的新文件不参与判定，
  所以本地 `file_size_check` 绿 ≠ 过门。本片所有文件在 `git add -A` 之后重跑 ⑩ 才作数。
- **上游 `runtime.go` 里还有本片未消费的辅助面**：`runtime_unusable_notice.go`
  与 `runtime_blocking_agents.go` 的「为什么不能启动」提示目前只用于 409 拒绝体的
  `blocker_class` 字段（`user` / `mika` / `agent_builder` / `other_system`），
  完整的 unusable-notice 文案属 daemon 注册面（M3-7）。
