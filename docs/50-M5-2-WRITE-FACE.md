# 50 · M5-2 自动驾驶写面（LUM-1567）

C 波第二片：把上游 autopilot 的**写面**五条路由搬进 `mc-http`，连同它们背后的写 SQL
（`mc-repos`）、写面纯函数（`mc-autopilot`）与权限/指派判定。读面是 M5-1（`docs/46`），
触发器的写面归 M5-3（`docs/51`），投递与 webhook 入口归 M5-4 / M5-5。

上游基准：`louloulin/multica` @ `f41fae6`（与 `scripts/route_parity.py` 内嵌的同一 commit）。

## 0. 交付物

| 文件 | 行数 | 内容 |
| --- | --- | --- |
| `crates/mc-http/src/routes/autopilots/crud.rs` | 710 | 3 个 handler（create / update / delete）+ `router()` + 请求体 + 内嵌单测 5 |
| `crates/mc-http/src/routes/autopilots/assignee.rs` | 365 | 指派校验（`validateAutopilotAssigneeForSave` 等价物）+ 403/409 扁平体 + 内嵌单测 2 |
| `crates/mc-http/src/routes/autopilots/subscribers.rs` | 227 | 协作者两条路由 + 订阅者整表替换 + 内嵌单测 2 |
| `crates/mc-autopilot/src/write.rs` | 357 | 三态 patch 组装 / `substantive_change` / `rule_config_summary` / 模板校验 + 内嵌单测 8 |
| `crates/mc-autopilot/src/collaborator.rs` | 257 | 订阅者解析 / 加锁排序 / 候选投影 + 内嵌单测 9 |
| `crates/mc-repos/src/autopilot/write.rs` | 543 | 写 SQL：`upsert_patch`、`archive`、规则版本、订阅者/协作者、`FOR UPDATE` / `FOR SHARE` |
| `crates/mc-repos/src/autopilot/tests/write.rs` | 580 | repos 层真库 5 例（`#[ignore]`） |
| `crates/mc-http/tests/autopilots/crud.rs` | 700 | HTTP e2e 5 例（① ② ③ + 校验矩阵 + 删除） |
| `crates/mc-http/tests/autopilots/crud_access.rs` | 567 | HTTP e2e 4 例（④ 并发 ×2、⑤ 指派、⑥ 鉴权/协作者） |
| `crates/mc-http/tests/autopilots/crud_support.rs` | 241 | 上面两个文件的共享夹具（种子 / 探针），`pub(super)` |

`crates/mc-autopilot/src/error.rs` **未改动**：写面没有新错误变体，全部复用 M5-1 已有的
`AutopilotError::validation`（§36.6 的「私有错误只准尾部追加」约束因此没有触发）。

未触碰：`routes/autopilots/mod.rs`（M5-0 聚合壳）、`routes/mod.rs`、`routes/mount.rs`、
`mc-http/Cargo.toml`（`mc-http → mc-autopilot` 复用 M5-1 已加的那条边，`Cargo.lock` 无变化）。

### 0.1 e2e 测试为什么拆三个文件

初版是把 15 例写在一个 `crud.rs` 里，**1097 行** —— 直接撞 ⑦`file-size` 的 R7 800 行硬上限
（`scripts/file_size_check.py`，只扫 `git ls-files` 里的代码路径）。拆法：夹具单独成
`crud_support.rs`，两个用例文件各自保持在 800 行以内。

**`clippy::wildcard_imports` 是 pedantic**（门③/④ 带 `-D warnings`）⇒ 兄弟模块之间**不能**
`use super::crud_support::*;`，只能逐项列举（只有 `use super::*;` / `use crate::*;` 被豁免）。
新增夹具函数时必须同步改两个 import 列表。

## 1. 路由与注册键

| 上游 handler | 本地 handler | 注册键 |
| --- | --- | --- |
| `CreateAutopilot`（`handler/autopilot.go:808`） | `crud::create_autopilot` | `POST /api/autopilots` + `POST /api/autopilots/` |
| `UpdateAutopilot`（:1039） | `crud::update_autopilot` | `PATCH /api/autopilots/{id}` + 带尾斜杠 |
| `DeleteAutopilot`（:1339） | `crud::delete_autopilot` | `DELETE /api/autopilots/{id}` + 带尾斜杠 |
| `AddAutopilotCollaborator`（:1420） | `subscribers::add_autopilot_collaborator` | `POST /api/autopilots/{id}/collaborators` |
| `RemoveAutopilotCollaborator`（:1479） | `subscribers::remove_autopilot_collaborator` | `DELETE /api/autopilots/{id}/collaborators/{userId}` |

上游挂载点是 `server/cmd/server/router.go:2100-2127` 的 `r.Route("/api/autopilots", …)`：
`chi` 的 `Route` + 子路由 `"/"` 同时吃带/不带尾斜杠两种请求，`axum` 必须各注册一次，
否则 `/api/autopilots/` 会落到 router 默认 404（`slash_alias_audit.py` 判 `MISSING_ALIAS`）。
协作者两条上游就是单形态（`r.Post("/collaborators", …)` / `r.Delete("/collaborators/{userId}", …)`），
本地照抄，不加尾斜杠别名。

**共 8 个注册键 / 5 条上游路径。** 与 M5-1 一样，三条主路由用
`.route(uri, patch(h).delete(h))` 的**双形态**写法 —— 但注意 `get` 绝不能这样合并
（M5-1 的读面路由已在同路径注册，重复注册同一 method 会在 router 构建期 panic；
`crud::router()` 的 `aggregate_router_has_no_conflicting_routes` 单测钉住这一点）。

## 2. 线上契约

### 2.1 `POST /api/autopilots` ⇒ `201`

响应是 `autopilotToResponse`（`handler/autopilot.go:185`）的**写面形态**：16 个基础键 + `subscribers`。
`trigger_kinds` / `next_run_at` / `last_run_status` 是**列表专属派生列**，写面**不出现**；
`subscribers` **恒出现**（空集合序列化成 `[]`，不是 `null`，也不是省略）。

校验顺序（逐字复刻上游，顺序本身是契约 —— 先撞哪条报错要一致）：

1. body 不是 JSON 对象 ⇒ `400 invalid request body`（`null` 是**合法**的零值体，等价 `{}`）；
2. `title is required` → `assignee_id is required` → `execution_mode is required`
   → `execution_mode must be create_issue or run_only` → `{{…}}` 模板校验
   （`unknown template variable "nope"; supported: {{date}}`）；
3. **成员门槛**（`require_member`，非成员 404）；
4. `assignee_id` 解析 → `assignee_type` 缺省 `agent` / 只准 `agent|squad`；
5. `project_id` 必须在**本 workspace**（否则 `project_id must reference a project in this workspace`）；
6. `subscribers` 解析（`subscribers[0] …` 逐项报错）。
7. 事务：锁（见 §3.5）→ `INSERT autopilot` → 订阅者 → **规则版本 v1**。

### 2.2 `PATCH /api/autopilots/{id}` ⇒ `200`

**三态**逐列判定，判定依据是「键在不在」而**不是** `Option` 是否为 `None`
（`Option` 分不清「缺失」与「显式 null」，而这两者在 `assignee_id` 上是两条分支）：

| 列 | 键不出现 | 键 = `null` | 键 = 值 |
| --- | --- | --- | --- |
| `title` | 不改 | 不改 | 覆盖（**不**发版，见 §3.2） |
| `description` | 不改 | 不改 | 覆盖（**发版**） |
| `assignee_type` / `assignee_id` | 不改 | 400 `assignee_id cannot be null` | 覆盖 + 重新校验（**发版**） |
| `status` | 不改 | 不改 | 覆盖（**发版**）；置 `active` 时清 `pause_reason` |
| `execution_mode` | 不改 | 不改 | 覆盖（**发版**） |
| `issue_title_template` | 不改 | 不改 | 覆盖（**发版**） |
| `project_id` | 不改 | **清空** | 覆盖（**不**发版） |
| `pause_reason` | 不改 | 不改 | 覆盖 |
| `subscribers` | 不改 | 不改 | **整表替换**（见 §3.3） |

`description` 这一行是**核对上游后的修正**：上游 `UpdateAutopilot`（:1084）
写的是 `if _, ok := rawFields["description"]; ok { params.Description = ptrToText(req.Description) }`
—— 键在、值为字符串时**是可观测的覆盖**，并且 `description` 在
`autopilotRuleSubstantiveChange`（:1299）的六列里 ⇒ 还发版。详见 §5.1。

乐观并发：`lock_autopilot_for_update` 拿到行后比对 `updated_at`，与请求携带的 `updated_at`
不一致 ⇒ **409 `autopilot_update_conflict`**（扁平体，见 §2.5）。

### 2.3 `DELETE /api/autopilots/{id}` ⇒ `204`（无 body）

**删除 = 归档，执行历史全留**。上游 `DeleteAutopilot`（:1339-1394）只做
`ArchiveAutopilot` + `recordAutopilotRuleVersion`，注释原文是
「preserving runs, tasks, webhook deliveries, subscribers, and collaborators as execution history」；
本地 `archive` 的 SQL 是 `UPDATE autopilot SET status = $2, pause_reason = NULL, updated_at = now()`。

- 子行（`autopilot_trigger` / `autopilot_subscriber` / `autopilot_collaborator` / `autopilot_run` /
  `webhook_delivery`）**一行不删**，由 `delete_archives_and_preserves_history` 逐表数行钉住。
- 归档**也是实质状态变更**（`status` 在六列里）⇒ 每次 DELETE 追加一条
  `status='archived'` 的规则版本，发布者 = 删除者。
- **没有「已归档」护栏**：对已归档的 autopilot 再 DELETE 仍 `204` 并**再**记一条版本
  （上游同样如此）。这不是幂等语义上的「无副作用」，而是「重复归档各留一笔」。
- 列表按 `status <> 'archived'` 隐藏，详情 `GET` 仍 `200`。

> DoD 原文写的是「`DeleteAutopilot` 的关联行清理（trigger / collaborator / subscriber）」，
> 与钉住的上游实现**相反**。取证与结论见 §5.2。

### 2.4 协作者两条

| 路由 | 语义 |
| --- | --- |
| `POST …/collaborators` | body 只有 `user_id`（`user_type` **恒为 `member`**，不可指定）；成功 `201` + 列表形状 `{"collaborators":[…]}` |
| `DELETE …/collaborators/{userId}` | `200` + 列表形状；**删不存在的授权也 `200`**（上游无「授权行必须存在」检查，DELETE 0 行即幂等成功） |

`user_id` 校验三连：缺失 `user_id is required` → 非 UUID `user_id must be a valid uuid`
→ 非本 workspace 成员 `user_id must be a member of this workspace`。
**DELETE 路径的 `userId` 文案不同**：`user id must be a valid uuid`（带空格）—— 逐字保留两份。

两条路由**没有事务**（上游亦然）：单次 `isWorkspaceEntity` 判定 + 单条
`INSERT … ON CONFLICT DO NOTHING` / `DELETE`，因此并发授权天然不互相踩（`ON CONFLICT` 幂等），
由 `concurrent_collaborator_grants_all_land` 钉住。

`can_manage_access` = **只有所有权腿**（创建者 ∨ workspace admin），协作者**不能**再授权
（上游 MUL-3807 明确不留自委派）；`can_write` = 所有权腿 ∨ 协作者腿。

### 2.5 错误体形状

| 场景 | 形状 |
| --- | --- |
| 403（无写权 / 不能管权限） | **扁平** `{"error","code"}`，恰好 2 键，`code = autopilot_forbidden` |
| 409（乐观冲突） | **扁平**，`code = autopilot_update_conflict` |
| 400 / 404 / 401 | **嵌套** `{"error":{"code","message"}}` |
| 201 / 200 / 204 | 正常响应体；204 **无 body** |

400 的 `message` 一律带 `mc-errors` 的 Display 前缀 `validation error: `
（`crates/mc-errors/src/lib.rs:19`）—— 断言文案时这层框架包装不能漏：
`validation error: title is required`、`validation error: autopilot id must be a valid uuid`。
403 的扁平 `error` 是**裸句**（`only the autopilot creator or a workspace admin…`），
只有 2 个键，因此 e2e 里用 `flat_error(&body)` 单独读。

## 3. 语义要点

### 3.1 写权限三条腿

`access::member_can_write(...)` = 创建者 ∨ workspace admin（`role_may_write`）∨ 协作者行存在。
判定顺序在 `update_autopilot` / `delete_autopilot` 里是：
**成员门槛 → 加载行（含 workspace 作用域）→ 写权判定 → 解析 body → 事务**
⇒ 无写权者改一个空 body 也只会拿到 403，**不会**先看到字段校验错误。

### 3.2 规则版本：只有「实质变更」才发版

`substantive_change`（`mc-autopilot/src/write.rs:145`）逐字对齐上游
`autopilotRuleSubstantiveChange`（:1299）的 **6 列**：`assignee_type` / `assignee_id` /
`status` / `execution_mode` / `description` / `issue_title_template`。
`title` 与 `project_id` **不在**其中（上游注释把它们算「cosmetic / routing」——
但注释同时把 `description` / `issue_title_template` 也叫 cosmetic，**与代码矛盾**，见 §5.1）。

版本体是 `rule_config_summary`（`write.rs:160`）＝
`{assignee_type, assignee_id(规范 UUID 串), status, execution_mode}`，**只增不改**：
`autopilot_rule_version` 无 UPDATE/DELETE 路径，e2e 逐条比对历史快照未被改写。

### 3.3 订阅者是「整表替换 + 排序加锁」

上游 `lockAndValidateAutopilotSubscribers`（:1000）的本地等价物在 `crud.rs:274`：

1. 按 `(workspace_id, user_id)` 取 **advisory lock**，**按规范 UUID 升序**逐个加锁
   （`collaborator::ordered_for_locking`）—— 升序是防死锁的关键，不是风格问题；
2. 锁内校验每个候选都是 workspace 成员（否则 `subscribers[i] is not a member …`）；
3. `LOCK` 成员行（`LockActiveMember`）之后才写。

⇒ 两个并发请求替换同一 autopilot 的订阅者时，后者看到的是前者**已提交**的集合，
不会出现「A 的成员被 B 的全量替换悄悄抹掉」的畸形中间态。

### 3.4 指派校验（`validateAutopilotAssigneeForSave` 的等价物）

| 情形 | 状态码 | 文案 |
| --- | --- | --- |
| agent 跨 workspace / 不存在 | 400 | `assignee must be a valid agent in this workspace` |
| agent 已归档 | 422 | `assignee agent is archived; pick a different agent` |
| agent 没有 runtime | 422 | `assignee agent needs a runtime before this autopilot can be active` |
| squad 已归档 | 422 | `squad is archived; pick a different squad` |
| squad 队长已归档 | 422 | `squad leader is archived; …` |
| 队长没有 runtime | 422 | `squad leader needs a runtime before this autopilot can be active` |
| 队长是 `private` 且调用者不是它的 owner | 403 | `cannot assign autopilot to squad with private leader` |

锁的形状：agent 走 `lock_agent_for_autopilot_assignment`（限定 `kind = 'user'` + workspace），
squad 走 `lock_squad_for_autopilot_assignment`，两者都 `FOR SHARE`。
`private` 队长的判定（`leader_is_invocable`）：owner ∨ (`public_to` + allowlist) ——
**`private` 连 admin 都拒**（上游 MUL-3963）。

### 3.5 锁序（必须一致，否则死锁）

① 订阅者 advisory lock（按规范 UUID 升序）
→ ② 成员行 `FOR SHARE`
→ ③ 指派对象 `FOR SHARE`（squad 则先锁 squad 再锁队长 agent）
→ ④ `autopilot` 行 `FOR UPDATE`
→ ⑤ 写。

`updated_at` 的乐观冲突比对排在 **④ 之后、⑤ 之前**：先锁住再比，避免「比完就变」。

## 4. 测试

三档，共 **+40 例**（其中需要真库的 14 例一律 `#[ignore]`）：

| 层次 | 文件 | 例数 | 覆盖 |
| --- | --- | --- | --- |
| 纯函数单测 | `mc-autopilot/src/write.rs`、`collaborator.rs` | 17 | 三态判定、模板校验、实质变更六列、加锁升序 |
| 路由内嵌单测 | `autopilots/{crud,assignee,subscribers}.rs` | 9 | `sent()` 的缺失 vs 显式 null、`null` 零值体、router 无冲突注册 |
| repos 真库 | `mc-repos/src/autopilot/tests/write.rs` | 5 | patch 逐列、归档清 `pause_reason` + 版本只增、指派锁（kind/归档）、订阅者与协作者幂等、实质编辑动触发器发布者 |
| HTTP e2e 真库 | `tests/autopilots/{crud,crud_access}.rs` | 9 | 见下表 |

### 4.1 DoD 对照

| DoD | 用例 |
| --- | --- |
| ① create → detail 字段 == `autopilotToResponse` | `crud::create_shape_matches_detail_and_rule_v1` |
| ② 三态 patch | `crud::patch_three_state_semantics` + `crud::create_and_patch_validation_messages` |
| ③ 规则版本 append-only | `crud::rule_versions_append_only_on_substantive_edits` |
| ④ 并发写订阅者不互相踩 | `crud_access::subscriber_replace_is_atomic_under_concurrency` |
| ④ 并发授权不丢 | `crud_access::concurrent_collaborator_grants_all_land` |
| ⑤ squad / agent 指派校验 | `crud_access::assignee_validation_for_agent_and_squad` |
| ⑥ 非成员 / 无写权 / 跨 workspace + 协作者生命周期 | `crud_access::access_refusals_and_collaborator_lifecycle` |
| 删除语义（归档 + 留史 + 列表隐藏 + 重复删幂等） | `crud::delete_archives_and_preserves_history` |

⚠️ ① 的断言是**逐键**比对，不是整body 相等：详情信封比 create 多 `can_write` /
`can_manage_access` 两个键（读面才有），整body 相等会假红。

⚠️ ⑥ 的两个用词按 M5-1 已确认口径：**非成员是 404**（不是 DoD 写的 403），
**成员但无写权是 403 扁平体**。`docs/46` §5.2 已记录同一约定，本片沿用。

### 4.2 真库测试的形态（值得后续片照抄）

- **并发用 `tokio::join!` 真并发**，不是「先 A 后 B」的形状：`subscriber_replace_is_atomic_under_concurrency`
  同时发两条整表替换（成员集合不同），断言最终集合是**其中一条**的完整快照 ——
  出现混合体就说明锁/事务漏了。`futures_join` 小工具（`crud_access.rs`）就地展开四个 future。
- **每条用例自建 workspace 自清场**：`crud_support::cleanup_all` 按
  `autopilot → squad → agent → agent_runtime → workspace/user` 顺序删。
  M5-1 的 `support::cleanup` 不删 agent/squad/runtime，而这些行对 workspace 有外键 ⇒
  `DELETE FROM workspace` 会**静默失败**留下一地孤儿（后续用例如撞名就会莫名其妙地绿/红）。
- 断言只看**真库读数**（`autopilot_state` / `child_counts` / `rule_versions`），
  响应体只用来钉线上契约。
- 并发断言不能写「恰好 1 行」：协作者列表响应是**整张**列表，并发期间别人的授权可能已落库
  ⇒ 断言「包含本次授权的行」+ 终态数行。

### 4.3 门禁

本片工作树（含下面的夹具修正）跑全门：

```
MULTICA_TEST_DATABASE_URL='postgres://…/multica_lum1567' bash scripts/gates.sh --with-db
```

| 门 | 结果 | 读数 |
| --- | --- | --- |
| ① fmt | PASS | `cargo fmt --all --check` |
| ② build | PASS | `cargo build --locked --workspace --all-targets`（`Cargo.lock` 无变化） |
| ③ clippy | PASS | `-D warnings`（pedantic） |
| ④ clippy-test-util | PASS | `-p mc-http --all-targets --features mc-http/test-util` |
| ⑤ test | PASS | **1281 passed / 0 failed**（含本片 26 例内嵌单测） |
| ⑥ db | PASS | **260 passed / 0 failed**（`--ignored`，真 PG：本片 repos 5 + e2e 9 + M5-1 的 15…） |
| ⑦ route-parity | PASS | `upstream 456` / `local 315` / `baseline 300`，`implemented 253 = 251 real + 2 placeholder`，`unclaimed 0`、`regression 0`、`local_only 11` |
| ⑧ schema-drift | PASS | 与 `migrations/` 无漂移 |
| ⑨ conformance | PASS | `--no-db --check report.json` |
| ⑩ file-size | PASS | 800 行上限：本片最大文件 710 行（`crud.rs`） |

整体 **10/10 green / 159s**（热 `target/`）。

> ⑤ 的 **1281** 是全仓未忽略用例总数（不是本片的）——本片的 26 例内嵌单测在其中；
> ⑦ 里 5 条新路由把 `implemented real` 从 246 推到 251（多段路径参数按 1 条计）。

## 5. 偏离与修正

### 5.1 `description` 的三态：锚点的「不可观测」结论是错的（已改代码，不是改文档）

片前锚点把 `UpdateAutopilot` 的 `description` 判成「上游不可观测 ⇒ 不实现」。
核对钉住的上游（`handler/autopilot.go:1084`）后确认：

```go
if _, ok := rawFields["description"]; ok {
    params.Description = ptrToText(req.Description)
}
```

`ptrToText(nil)` → `NULL`、`ptrToText(&"x")` → `'x'` ⇒ **键在、给字符串就是可观测覆盖**，
且 `description` 在六列实质判据里 ⇒ 还发版。这是**上游注释与代码矛盾**的第二处
（注释把 `description` / `issue_title_template` 归为 cosmetic，代码把它们算 substantive）。
处置：**改代码**（本地原先漏了这条分支），并按代码为准 —— `substantive_change` 已经是 6 列，
SQL 的 `COALESCE` 天然给出「键在给出值就覆盖」的正确行为。

### 5.2 DoD 的删除语义与上游相反（未按 DoD 实现）

DoD 写「`DeleteAutopilot` 的关联行清理（trigger / collaborator / subscriber）」。
实测钉住的上游：

- `DeleteAutopilot`（:1339-1394）：只有 `ArchiveAutopilot` + `recordAutopilotRuleVersion`，
  注释明说保留 runs / tasks / webhook deliveries / subscribers / collaborators 作为执行历史；
- `DeleteAutopilotCollaboratorsForAutopilot`（`pkg/db/queries/autopilot.sql:796`）存在但
  **零调用**（死代码）；`DeleteAutopilotSubscribersForAutopilot` 只在 `UpdateAutopilot` 里被调
  （:1247，整表替换用）；根本没有 `DeleteAutopilotTriggers` 查询。

⇒ 本地实现**照上游**（归档，不删子行），并由 e2e 逐表数行钉住。
若产品侧真要「删除即清空子行」，那是对上游契约的**新增语义**，需要另行确认 —— 不是本片能顺手做的。

### 5.3 实时事件面（未实现，记录为缺口）

上游在五条路由上都发事件：`publish(EventAutopilotCreated)`（:948）、
`EventAutopilotUpdated`（:1273、协作者两条 :1469/:1506）、
`EventAutopilotDeleted`（:1393）。本仓没有实时广播平面（M5-1 读面同样未接，
`docs/46` §5 未覆盖这一条）⇒ 本片**不发事件**，缺口记录在此，
接缝点是各 handler 事务提交之后（`mc-http` 里已有 workspace 级广播的地方可复用）。

### 5.4 门⑤ 抓到的第三处：抢救代码里有一个「分支不可达」的单测（改夹具，不是改实现）

第一次跑全门时 ⑤ 红：`mc-autopilot` 的
`collaborator::tests::user_type_must_be_member_and_error_names_the_index`
期望 `subscribers[1].user_type must be 'member'`，实际拿到
`subscribers[0].user_id must be a valid uuid`。

根因是**夹具**：`parse_subscribers` 是逐项 `user_type → user_id 非空 → UUID 解析 → 去重` 的
（与上游 `parseAutopilotSubscribers`、`handler/autopilot.go:967-994` 的顺序逐行对照过），
而夹具把第 0 项的 `user_id` 写成 `"u"` ⇒ 第 0 项就在 UUID 解析上返回，永远走不到第 1 项的
`user_type` 分支。修法是夹具用**合法 UUID**（实现一行未改）。

顺带说明抢救代码的编译/单测状态：这次门⑤ 是 `mc-autopilot` 内嵌单测**第一次运行**
（抢救 commit 只保证了 `cargo check`）—— 全仓只有这一例红，其余 69 例绿。

### 5.5 建档号

`docs/NN` 空号：`46=M5-1 / 47=M5-6 / 48=M5-7 / 49=M4-INT` 已用 ⇒ M5-2 取 **50**
（`51=M5-3`、`52=M5-4` 已由 cycle 记录预留，下一片 M5-5 取 53）。本片代码里 3 个文件原先引用
`docs/47`（误取了 M5-6 的号）已全部改为 `docs/50`。

## 6. 遗留

1. **`apps/mc-server` 缺 `mc-scheduler` 依赖边 + 门⑥ 不含 `-p mc-scheduler`**（P0，仍未获批，跨片）。
2. ⑦ 基线（`route-parity` 快照）归 M5-INT 一次性刷（`LUM-1572`）。
3. 触发器写面（`POST/PATCH/DELETE /triggers` + webhook token 轮换 / signing secret）属 M5-3；
   订阅者的「创建者自动成为订阅者」策略在上游由触发器/运行路径补，本片不动。
