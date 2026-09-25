# 63 · M2-A 收尾：issue-views / pins / issue-view-preferences / assignee-frequency（LUM-1691）

M2-A（`LUM-1348`）把 `/api/issues*` + `/api/issue-statuses*` 交付了，但 `scripts/route-owners.tsv`
判给 `M2-A` 的 51 条键里还有 13 条**无任何 issue 认领、代码侧也无注册**。本片收掉其中 12 条
（第 13 条 `POST /api/issues/{id}/squad-evaluated` 归 `LUM-1793`，见 §7），并在 ⑦ 上把
`owners.M2-A` 从 13 压到 1。

**本片是纯代码片：0 迁移、0 新表、0 新依赖**（`Cargo.lock` 不动）。

## 0. 交付物

| 文件 | 类型 | 行为 |
| --- | --- | --- |
| `crates/mc-repos/src/issue_view.rs` | 新 | `IssueViewRepo`：`issue_view` CRUD（workspace 收窄 + 乐观并发）+ `issue_view_preference` 读写；纯函数 `validate_variant` / `preference_scope_id` / `is_json_object` |
| `crates/mc-repos/src/pin.rs` | 新 | `PinRepo`：`pinned_item` 增删查 + `position` 改写；纯函数 `is_valid_item_type` / `visible_without_view_capability` |
| `crates/mc-repos/src/stats.rs` | 新 | `StatsRepo`：`GET /api/assignee-frequency` 的两条聚合 SQL + `merge_frequencies` 合并/排序 |
| `crates/mc-repos/src/lib.rs` | 改 | 3 行 `pub mod`（按字母序插入既有列表：`issue_view` / `pin` / `stats`），**不动任何既有行** |
| `crates/mc-http/src/routes/issue_views.rs` | 新 | `/api/issue-views*`（5 条）+ `/api/assignee-frequency`（1 条）|
| `crates/mc-http/src/routes/issue_view_preferences.rs` | 新 | `/api/issue-view-preferences`（2 条）|
| `crates/mc-http/src/routes/pins.rs` | 新 | `/api/pins*`（4 条）|
| `crates/mc-http/src/routes/mod.rs` | 改 | 3 行 `pub mod`（追加在 M2-E 那组之后），**不动任何既有行** |
| `crates/mc-http/src/routes/mount.rs` | 改 | 尾部追加 1 行 `.merge(mount_slice_issue_view_pin())` + 1 个 `mount_slice_*` 函数（合并上面 3 个子 router），**不动任何既有行** |
| `crates/mc-repos/tests/issue_view_pin_stats.rs` | 新 | repo 层真库契约用例 6 条（§5.1）|
| `crates/mc-http/tests/issue_views.rs` | 新 | 路由 e2e 2 条（issue-views 全链 / preferences）|
| `crates/mc-http/tests/issue_pins.rs` | 新 | 路由 e2e 2 条（pins 全链 / assignee-frequency）|

**为什么是 3 个路由文件而不是 1 个**：合写是 803 行，撞 R7 单文件 800 行硬上限（门 ⑩）；
而拆法正好与上游的 handler 文件一一对应 —— `issue_view.go` / `issue_view_preference.go` /
`pin.go`（`assignee-frequency` 的 handler 在上游 `activity.go`，语义上属视图栏那一族，故并入
`issue_views.rs`）。测试同理拆成两个 target（合写 899 行）。

## 1. 路由与注册键（12 条）

上游行号取自 `docs/fixtures/upstream-routes.tsv`（commit `f41fae6b08fb`），形态判据取自
`server/cmd/server/router.go:2129-2145` 与 `:1952`。

| # | 上游键 | router.go | 上游注册形态 | 本仓注册点 |
| --- | --- | --- | --- | --- |
| 1 | `GET /api/assignee-frequency` | 1952 | plain | `/api/assignee-frequency` |
| 2 | `GET /api/issue-view-preferences` | 2136 | plain | `/api/issue-view-preferences` |
| 3 | `PUT /api/issue-view-preferences` | 2137 | plain | 同上（同一条 route 的 `.put()`）|
| 4 | `GET /api/issue-views/` | 2139 | `Route("/api/issue-views") + Get("/")` ⇒ **两形态** | `/api/issue-views` + `/api/issue-views/` |
| 5 | `POST /api/issue-views/` | 2140 | 同上 ⇒ **两形态** | 同上 |
| 6 | `GET /api/issue-views/{id}/` | 2142 | `Route("/{id}") + Get("/")` ⇒ **两形态** | `/api/issue-views/:id` + `/api/issue-views/:id/` |
| 7 | `PATCH /api/issue-views/{id}/` | 2143 | 同上 ⇒ **两形态** | 同上 |
| 8 | `DELETE /api/issue-views/{id}/` | 2144 | 同上 ⇒ **两形态** | 同上 |
| 9 | `GET /api/pins/` | 2129 | `Route("/api/pins") + Get("/")` ⇒ **两形态** | `/api/pins` + `/api/pins/` |
| 10 | `POST /api/pins/` | 2130 | 同上 ⇒ **两形态** | 同上 |
| 11 | `PUT /api/pins/reorder` | 2131 | plain（`Route` 内的普通子路由）| `/api/pins/reorder` |
| 12 | `DELETE /api/pins/{itemType}/{itemId}` | 2132 | plain | `/api/pins/:item_type/:item_id` |

⇒ **12 条上游键 / 19 个注册点**（7 条要求双形态 ⇒ 多出 7 个尾斜杠别名；见 §5.2 的计数核对）。
**`route_parity.py` 的 `local` 数的是注册点，不是折叠后的键** —— 所以本片 +19 而不是 +12，
而 `implemented` / `known_gap` / `owners` 走折叠键 ⇒ 分别是 +12 / −12 / −12。
`slash_alias_audit.py` 的 `MISSING_ALIAS` / `MISSING_EXACT` 是硬失败，而本波
`docs/fixtures/slash-alias-allowlist.tsv` 是 **0 数据行**（全注释）⇒ 没有任何豁免退路；
本片为此在每个双形态组上都注册了两个形态，并用 `--no-allowlist` 严格模式验过（§5.2）。

## 2. 数据模型与迁移（为什么零迁移）

三张表**已经在 head schema 的上游迁移链上**（`migrations/upstream/`，560 个 `.up.sql`，
`MANIFEST.sha256` 可校验；本片实跑 `mc-migrate run` 后按 `information_schema` 逐列复核）：

| 表 | 来源迁移 | 关键约束（实测）|
| --- | --- | --- |
| `pinned_item` | `038_pinned_items` + **`270_pinned_item_view`** | `UNIQUE (workspace_id, user_id, item_type, item_id)`；`item_type CHECK IN ('issue','project','view')`（270 把 038 的两值扩到三值 —— **只看 038 会以为 `view` 不可钉**）；`idx_pinned_item_user_ws (workspace_id,user_id,position)` |
| `issue_view` | `265_issue_view` | `scope_type IN (workspace,my,project)`；三条 CHECK 钉住 `scope_id` / `scope_variant` / `my ⇒ private`；`query`/`display` 是 `jsonb` 且 `jsonb_typeof = 'object'`；**无外键** |
| `issue_view_preference` | `268_issue_view_preference` | 复合主键 `(workspace_id,user_id,scope_type,scope_id)`，`scope_id NOT NULL`（回填口径见 `preference_scope_id`）；**无外键** |

⇒ 本片 **0 迁移**，也**没有**新建 `migrations/compat/0006`。
`activity_log` / `issue` 是本片只读的既有表（`GET /api/assignee-frequency` 的数据源）。

两处「只读迁移会读错」的坑，写在这里避免下一个人再踩：

1. **`pinned_item.item_type` 的 CHECK 被 270 改过**。只 `grep 038` 会得到
   `CHECK (item_type IN ('issue','project'))` ⇒ 会误判「`view` 型 pin 不可能存在」，进而漏掉
   `ListPins` 的 `?include=view` 兼容闸门（§3.3）。
2. **三张表都没有外键是上游的刻意策略**（265/268 的注释逐字写明「No FKs by repository policy,
   lifecycle cleanup is handled in application transactions」）⇒ 删除必须由应用层收尾：
   `DeleteIssueView` 用数据修改型 CTE 顺手清扫指向该视图的 `pinned_item`；本片的 repo 测试与
   e2e 测试清理都必须**显式**删 `issue_view` / `issue_view_preference`（级联删不到）。

## 3. 逐状态码契约

### 3.1 `issue-views`（5 条）

| 方法/路径 | 成功 | 关键错误 |
| --- | --- | --- |
| `GET /api/issue-views` | 200（数组，按 `created_at ASC`，`LIMIT 200`）| 400 缺/非法 `scope_type`；404 非成员 |
| `POST /api/issue-views` | **201** | 400 名字为空或 >80 rune / 非法 `scope_type` / 非法 `visibility` / `query` 非对象或缺失 / `display` 显式 `null` / `scope_variant` 与 scope 不配 / 配额满 / `scope_type=project` 缺 `scope_id`；404 `project` 不在本 workspace |
| `GET /api/issue-views/{id}` | 200 | 404（不存在、跨 workspace、**不可读**三种合成一个 404）|
| `PATCH /api/issue-views/{id}` | 200（`revision` +1）| 400 缺 `expected_revision` / 名字超限 / 非法 `visibility` / `my` 视图改 `visibility` / `query`·`display` 显式 `null` 或非对象 / `scope_variant` 不配；**403** 无写权限；**404** 不可读；**409** `expected_revision` 落后 |
| `DELETE /api/issue-views/{id}` | **204** | 403 无写权限；404 不可读（删除顺手清该视图的 `view` 型 pin）|

要点：

- **读权限**：所有者 或 `visibility='workspace'`（`IssueViewRow::is_readable_by`）。不可读与不存在
  **都是 404** —— 私有视图的存在性不得泄漏（上游 `loadIssueViewForUser` 把两者合成一个分支）。
- **写权限**（上游 `canManageIssueView`）：所有者；或共享视图的 workspace `owner`/`admin`。
  这是 M2-A 面上**唯一**会返回 403 的地方。别人的私有视图在读取阶段就 404 了 ⇒ 管理员的权力
  碰不到它们。
- **`scope_type` 不可变**；`scope_variant` 可在同一 scope 内切（`my` ⇒ `assigned|created|involved|any`
  必填；`workspace|project` ⇒ `members|agents` 可选，缺失 / `""` / `"all"` 都归一成 NULL）。
- `my` 作用域的 `visibility` 被强写成 `private`（DB CHECK 也这么钉），`PATCH` 想改成
  `workspace` ⇒ 400 `my views are always private`。
- **配额**：每成员每 workspace 100 个（`CountIssueViewsByOwner >= 100` ⇒ 400
  `view limit reached for this workspace`）。

### 3.2 `issue-view-preferences`（2 条）

| 方法 | 成功 | 关键错误 |
| --- | --- | --- |
| `GET` | **200**（无记录时 `prefs={}`、`updated_at=""`）| 400 非法 `scope_type` / `project` 缺 `scope_id`；404 `project` 不存在、非成员 |
| `PUT` | 200（整文档覆盖，last-write-wins）| 400 同上 + `prefs` 显式 `null` 或非对象 |

- **无记录不是 404**：上游 `pgx.ErrNoRows` 分支返回 200 + 空文档。`updated_at` 在 Go 里没有
  `omitempty` ⇒ **空串照样出现在 JSON 里**，本仓用 `String`（不是 `Option`）保留这一形状。
- `scope_id` **永不为 NULL**：`workspace` → workspace id、`my` → user id、`project` → project id
  （且 project 必须先在本 workspace 存在，否则 404）。复合主键因此不依赖 NULL 比较。

### 3.3 `pins`（4 条）

| 方法/路径 | 成功 | 关键错误 |
| --- | --- | --- |
| `GET /api/pins` | 200（`position ASC, created_at ASC`；默认**不含** `view` 行）| 404 非成员 |
| `POST /api/pins` | **201** | 400 非法 `item_type` / `item_id` 空 / `item_id` 非 uuid；**404** 被钉对象不在本 workspace；**409** 重复钉同一项 |
| `PUT /api/pins/reorder` | **204** | 400 body 非法或 `items[].id` 非 uuid；404 非成员 |
| `DELETE /api/pins/:itemType/:itemId` | **204**（**幂等**：没钉过也 204）| 400 `itemId` 非 uuid；404 非成员 |

- **`?include=view` 兼容闸门**：`ListPins` 默认丢掉 `item_type='view'` 的行，除非
  `include` 参数**子串**含 `view`（上游 `strings.Contains`，本仓照抄子串语义而不是等号）。
  理由见上游注释：老客户端把不认识的 `view` pin 当项目 pin 拉详情 → 404 → **永久自动取消钉住**
  ⇒ 只要它打开一次侧栏就能毁掉用户的视图 pin。
- **重复 pin = 409 而不是幂等**：`POST` 撞唯一约束（SQLSTATE `23505`）⇒ 409 `item already pinned`。
  **幂等的只有 `DELETE`**（上游 `DeletePin` 不看 rows affected）。任务描述里那条
  「幂等（重复 pin 不报错）」与上游实测不符，本片**按上游实现**（见 §4 第 1 条）。
- **`item_type` 不参与 `DELETE` 校验**（上游同款）：只按四元组删，`itemType` 传什么都 204。
- 被钉对象归属（上游 `CreatePin` 的 `switch`）：`issue` → 本 workspace 的 issue；`project` →
  `GetProjectInWorkspace`；`view` → **视图的读权限**（自己的或 workspace 共享的），
  别人的私有视图一律 404 —— 一次 pin 不能确认它的存在。

### 3.4 `assignee-frequency`（1 条）

`GET /api/assignee-frequency` → 200，`[{assignee_type, assignee_id, frequency}, …]`，
**频次降序**；空库 / 无数据 ⇒ `[]`（不是 `null`）。404 非成员。

两路数据源**逐字**照上游两条 SQL（`server/pkg/db/generated/{activity,issue}.sql.go`）：

```sql
-- 源 1（activity.sql / CountAssigneeChangesByActor）：本人改派过谁
SELECT details->>'to_type' AS assignee_type, details->>'to_id' AS assignee_id,
       COUNT(*)::bigint AS frequency
  FROM activity_log
 WHERE workspace_id = $1 AND actor_id = $2
   AND actor_type = 'member' AND action = 'assignee_changed'
   AND details->>'to_type' IS NOT NULL AND details->>'to_id' IS NOT NULL
 GROUP BY details->>'to_type', details->>'to_id'

-- 源 2（issue.sql / CountCreatedIssueAssignees）：本人建单时已带指派人
SELECT assignee_type, assignee_id, COUNT(*)::bigint AS frequency
  FROM issue
 WHERE workspace_id = $1 AND creator_id = $2
   AND creator_type = 'member'
   AND assignee_type IS NOT NULL AND assignee_id IS NOT NULL
 GROUP BY assignee_type, assignee_id
```

合并键是 `"type:id"` **字符串**（源 1 的 id 是 `details->>'to_id'` 的**文本**，不保证是 UUID；
上游扫进 `interface{}` 后按字符串用 ⇒ 本模块也不把它 UUID 化，否则历史脏值会被静默丢）。
四个窗口条件（`actor_type='member'`、`action='assignee_changed'`、`creator_type='member'`、
`details->>'to_id' IS NOT NULL`）在 repo 测试里各有一条反例钉住。

## 4. 与上游的偏差（逐条可查）

| # | 偏差 | 性质 | 为什么 |
| --- | --- | --- | --- |
| 1 | 任务描述要求「重复 pin 不报错（幂等）」；**本片按上游返回 409** `item already pinned` | **有意不照描述** | `server/internal/handler/pin.go` 的 `isUniqueViolation` 分支逐字返回 409；本仓的裁决规则是「逐字照上游」。上游真正幂等的是 `DELETE`（重复 unpin 仍 204），本片照做并有 e2e 断言 |
| 2 | `reorder` 先把全部 `items[].id` 解析完再落写；上游边解析边写 | **本片收紧**（唯一的收紧）| 上游循环里 `parseUUIDOrBadRequest` 与 `UPDATE` 交错 ⇒ 中途遇到非法 id 时前面几项**已经落库**（半截排序 + 400）。本片把解析收在前，让 400 不产生任何写入。已注册为偏差，e2e 有断言 |
| 3 | 无 realtime 发布 | **能力缺口**（§6）| 上游 `CreatePin`/`DeletePin`/`ReorderPins` 各 `h.publish` 一条事件；M2 面整波没有 realtime 通道 |
| 4 | 时间戳格式：上游 `time.RFC3339`（UTC → `…Z`），本仓 `DateTime::to_rfc3339()`（UTC → `…+00:00`）| 全仓既有约定 | `labels.rs` / `inbox.rs` / `invitations.rs` / `auth.rs` 全部这样；本片不单开第二种格式 |
| 5 | 错误体文案：上游自由文本（`issue not found` / `item already pinned`）| 全仓既有约定 + 一处新增 | 本仓统一 `mc_errors::Error`（`not_found: issue`）；**例外**：`409 item already pinned` 与两处 `400` 文案（`item_type must be …` / `view limit reached …` / `name must be between 1 and 80 characters` / `view was modified by someone else`）逐字保留上游英文 —— 它们进了 `route-parity` 的对外契约面，且客户端会按文案提示 |
| 6 | 同频次时的排序 | **本片固定**（不是收紧，是把随机值钉死）| 上游 `sort.Slice` **非稳定** + 合并源是 Go map ⇒ 同频次相对顺序上游自己也不确定。本模块在频次相同按 `(type, id)` 升序，让 fixture 可断言 |
| 7 | `POST /api/issue-views/{id}/…` 的 body 上限 | 等价实现 | 上游 `http.MaxBytesReader` + `Decode` 失败 ⇒ 400 `invalid request body`；本仓在解码前判长度 ⇒ 同样 400（**不是** axum 默认的 413）|

## 5. 测试与门禁

### 5.1 布局

| 文件 | 用例 | 覆盖 |
| --- | --- | --- |
| `crates/mc-repos/tests/issue_view_pin_stats.rs` | 6 条（`#[ignore]`，需 `MULTICA_TEST_DATABASE_URL`）| ①issue_view CRUD + 跨 workspace 404 + 乐观并发（旧 revision → `None`）②preference 无记录 = `None`、整文档覆盖、**每用户**与**每 scope** 各自独立 ③pin 追加位置 + 重复 → `Conflict` + **删除幂等**（1 行 / 0 行都不报错）+ 每用户隔离 ④reorder 只改 `position` 且**不动 `created_at`** ⑤删视图顺手清扫 `view` pin（两个用户的 pin 都清、非 view 的 pin 不误伤）⑥assignee-frequency 两路合并 + 排序 + 六条反例句（别人改派 / 非 `assignee_changed` / `actor_type<>member` / `details` 缺 `to_id` / 别人建单 / 无指派人的单）|
| `crates/mc-http/tests/issue_views.rs` | 2 条 e2e | issue-views 全链（**两形态各打一发** + 404×3 种 + 403 + 409 + 400×5）+ preferences（无记录 200 + 回填 + round trip + 覆盖 + 400×3 + 404）|
| `crates/mc-http/tests/issue_pins.rs` | 2 条 e2e | pins 全链（两形态 + `include=view` 闸门 + 409 + 400×2 + 404 + reorder 顺序 + 幂等 unpin + 跨 workspace 空列表）+ assignee-frequency（空 `[]` + 合并计数 + 别人空）|
| 三个 repo 模块 + 三个路由文件的 `#[cfg(test)]` | 21 条纯单测 | `item_type` 词表 / view pin 可见性 / 视图名 rune 计数 / `scope_variant` 配对 / `scope_id` 回填 / 只认 JSON 对象 / 读权限 / 频次合并与排序（含非 UUID 的 `to_id` 透传）/ **「键缺失 vs 显式 `null`」** / 超限 body → 400 / 3 个 router 的 build（同 path+method 重复注册会 panic）|

`tests/issue_views.rs` 与 `tests/issue_pins.rs` 各自带一份夹具（workspace + owner + member +
outsider + 第二个 workspace，且 **owner 在第二个 workspace 也是成员**）⇒ 那里断言出来的 404 /
空列表是**租户隔离**的结果，不是「因为不是成员所以看不见」这种弱结论。

### 5.2 门禁证据

环境：本机 PG16（`127.0.0.1:5432`），一次性角色 `mc_lum1691`（`LOGIN CREATEDB`）+ 库
`multica_lum1691`（`mc-migrate run --dir migrations` 应用 566 个迁移）。

- ⑦ 计数（`python3 scripts/route_parity.py`）。本片 rebase 到当时远端头（含 PR #90 / M7-4）后的实读：
  `local 447`、`implemented 364`（`360 real + 4 placeholder`）、`known_gap 92`、
  `unclaimed 0`、`regression 0`、`local_only 9`；
  `gaps by owner: M9=33 M7=20 M3+=16 M3=11 M8=6 M10=5 **M2-A=1**`。
  相对**同一 merge-base**的差值：`local +19`、`implemented +12`、`known_gap −12`、
  `owners.M2-A 13 → 1`（基线 `local 428 / implemented 352 / known_gap 104` —— 立案时写的
  `424 / 348 / 108` 是更早的 base，**别拿来对账**）。不变式 `implemented + known_gap = 456` 保持。
  **`local` 的 +19 是 12 条键 / 19 个注册点的结果**，不是 +12：`local` 数的是 axum 的
  *注册点*（`/x` 与 `/x/` 是两条），而 7 条双形态键各多一个尾斜杠别名。
- ⑦ 第二条命令：`python3 scripts/slash_alias_audit.py --no-allowlist`（严格模式）
  ⇒ `shapes OK`、`0 defect(s)`、`0 warning(s)`。
- ⑩：`python3 scripts/file_size_check.py --quiet` ⇒ exit 0（合写版 803 / 899 行是真撞限了，
  拆成 3 个路由文件 + 2 个 e2e target 后全部 ≤800）。
- 完整 10 门（`bash scripts/gates.sh --with-db`，**rebase 后的本片 HEAD**）：**10/10 全绿 / 511s**

  | # | gate | exit | time |
  | --- | --- | --- | --- |
  | ① | fmt | 0 | 2s |
  | ② | build | 0 | 108s |
  | ③ | clippy | 0 | 34s |
  | ④ | clippy-test-util | 0 | 28s |
  | ⑤ | test | 0 | 37s |
  | ⑥ | db（`migrate=0,e2e=0`）| 0 | 200s |
  | ⑧ | schema-drift | 0 | 37s |
  | ⑦ | route-parity | 0 | 0s |
  | ⑨ | conformance | 0 | 65s |
  | ⑩ | file-size | 0 | 0s |

  ⚠️ **磁盘会伪装成门红**：本片在本轮实测到两条同形记录 —— 磁盘只剩 ~2.4G 时
  ④/⑤/⑥/⑧ **同时**红（④⑤ 是 `cargo` 写工件失败的 101，⑧ 是建 scratch 库失败的 **exit 2**，
  即「根本没法开跑」而不是代码红）。清 `target/debug/incremental`（当时 15G）后门口逐条回绿。
  门红先看 `df -h /`。
- **未跑** `route_parity.py --write-baseline`（`baseline` 保持 406；基线刷新只归 M7-21
  `LUM-1786` / M8-7 `LUM-1804`），也未改 `docs/fixtures/route-parity-baseline.json` 与
  `docs/fixtures/slash-alias-allowlist.tsv`（后者仍是 0 数据行 —— 本片**没有**新增任何豁免）。

### 5.3 本地跑法（DB 夹具）

```bash
# 一次性
sudo -n -u postgres psql -c "CREATE ROLE mc_lum1691 LOGIN CREATEDB PASSWORD '<local-pw>'"
sudo -n -u postgres createdb -O mc_lum1691 multica_lum1691
export PATH="$HOME/.cargo/bin:$PATH"
export MULTICA_TEST_DATABASE_URL='postgres://mc_lum1691:<local-pw>@127.0.0.1:5432/multica_lum1691'
cargo run -p mc-migrate -- run --dir migrations        # ⑥/⑧ 都要求表已建

# 本片三个面
cargo test -p mc-repos --test issue_view_pin_stats -- --ignored --test-threads=1
cargo test -p mc-http --test issue_views --test issue_pins \
  --features mc-http/test-util -- --ignored --test-threads=1

# 全门
bash scripts/gates.sh --with-db
```

⚠️ 角色必须带 `CREATEDB`：门 ⑧（`schema_drift.py`）要建自己的 scratch 库
`schema_probe_w0b_drift_<pid>`，缺权限时脚本 **exit 2**，而 `gates.sh` 会把 2 记成 FAIL
（那是「根本没法开跑」，不是代码红 —— 见 `docs/30-W0-DRIFT-GATE.md`）。

## 6. 对外可观测差异 / 能力缺口（如实登记）

1. **无 realtime 发布（D1）**：上游 `pin.created` / `pin.deleted` / `pin.reordered` 三条事件本片
   不发。M2 面（issue / comment / inbox / label / property）整波都没有 realtime 通道，本片不单开
   一条依赖边。**接线点**：三个 pin handler 的写成功分支之后（`create_pin` 的 201 之前、
   `delete_pin` / `reorder_pins` 的 204 之前），事件体与上游一致 ——
   `pin.created` ⇒ `{"pin": <PinnedItemResponse>}`；`pin.deleted` ⇒
   `{"item_type": …, "item_id": …}`；`pin.reordered` ⇒ `{"items": <ReorderPinsRequest.items>}`；
   投递面统一为 `workspace_id` + `"member"` + 调用者 id。多端同步目前**只能靠重取列表**。
2. **`GET /api/pins` 的 `include` 参数是子串判定**（`?include=view`、`?include=xviewx` 都算命中）。
   这是**照上游**（`strings.Contains`），不是实现偷懒 —— 不要「顺手」改成等号。
3. **`issue_view` / `issue_view_preference` 没有外键**（上游策略）：本片没有补外键，也没有新增
   迁移。因此 **member 被移除 / project 被删除时的收尾仍缺**：上游另有
   `DeletePrivateIssueViewsByOwner` 与 `DeleteIssueViewsByProjectScope` 两条清理 SQL，本片
   **未实现也无调用点**（本仓 M1 的成员移除与 M4 的 project 删除路径都还没接这两个钩子）——
   与既有 R-M8-9 同性质，登记待后续切片处理；当前后果是「离开 workspace 的成员留下的私有视图
   仍占配额」（`count_by_owner` 只按 workspace+owner 数），**不会**泄漏内容（读路径按
   workspace 收窄）。
4. **`POST /api/issues/{id}/squad-evaluated` 不在本片**：它是 `M2-A` 线上剩下的最后一条键，
   但它的 owner 单元格是 `scripts/route-owners.tsv` 兜底行 `^/api/issues → M2-A` 的产物
   （不是立项裁决），且与本片同写 `mount.rs` / `routes/mod.rs` 的同一追加段 ⇒ 由 cycle 单独立
   issue `LUM-1793`（`backlog`）在本片**合入之后**单独跑。合入本片后
   `owners.M2-A` 预期 = **1**（就是它）。

## 7. 交接

- 合入本片后 `owners.M2-A 13 → 1`；剩下那 1 条 = `LUM-1793`（`POST /api/issues/{id}/squad-evaluated`）。
- 门 ⑦ 基线文件**本片不刷**（`baseline` 保持 406）。基线刷新只归 M7-21 `LUM-1786` / M8-7 `LUM-1804`。
- §6 第 1 条（realtime 三条事件）与第 3 条（两条清理 SQL 的接线）是本片**有意留下的**缺口，
  已在上面写清接线点；下一个碰 pin / view 面的切片应当顺手接上，而不是重新发现。
