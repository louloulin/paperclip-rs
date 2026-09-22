# M2-D：issue table 查询面 + `/api/issues/limit-usage`（LUM-1355）

本文件记录 M2-D 切片的**实测产出**、**语义要点**与**与上游的有意偏离**。上游 = `louloulin/multica`
`main`（`server/internal/handler/{issue_table_query,issue_table_group,issue_table_rows,issue_table_facets,issue_limit}.go`
与 `router.go` 的注册点，2026-09-22 抓取）。

上游把"HTTP 解析 + WHERE 编译 + SQL 执行"放在同一个 handler 文件里（单文件 3000+ 行）；本仓按
**仓储层 / HTTP 层**拆开（§6），代价是所有偏离都必须写清（§4）。

## 1. 产出文件

| 文件 | 行数 | 内容 |
|---|---|---|
| `crates/mc-repos/src/issue_table/mod.rs` | 547 | 查询规格与结果类型（`TableGroupSpec` / `TableFilter` / `TableOrder` / `TableRow` / `Table*Page`）、常量；**不含 SQL**，也不依赖 sqlx builder |
| `crates/mc-repos/src/issue_table/sql.rs` | 618 | `$n` 参数绑定、WHERE 编译、分组/排序解析、keyset 谓词、行结构 |
| `crates/mc-repos/src/issue_table/repo.rs` | 396 | `IssueTableRepo`：`status_order` / `table_groups` / `table_rows` / `table_facets` |
| `crates/mc-repos/src/issue_table/tests.rs` | 415 | 6 条 PG 集成测试（从 `repo.rs` 拆出，保证单文件在 R7 的 800 行以内） |
| `crates/mc-http/src/routes/issue_table/mod.rs` | 509 | 4 条路由 + 4 个 handler + `authorize` + 响应 DTO + `TableError` |
| `crates/mc-http/src/routes/issue_table/spec.rs` | 790 | 请求 DTO + 400/409/422 判定 + `query_fingerprint` + 分组 key/value 映射 |
| `crates/mc-http/src/routes/issue_table/cursor.rs` | 145 | cursor 编解码（`CursorWire`） |
| `crates/mc-http/src/routes/issue_table/tests.rs` | 433 | 11 条纯函数单元测试（无数据库） |
| `crates/mc-http/tests/issue_table.rs` | 479 | 4 条 DB e2e（正路径 3 + 契约负路径 1） |

`mount.rs` / `routes/mod.rs` / `state.rs` / 各 `Cargo.toml` **零改动**：M2 anchor scaffold（LUM-1347）
预留的 `mount_slice_issue()` 已经指向 `super::issues::router()`，而本切片用
`issues::router()` 内部的 `.merge(super::issue_table::router())` 挂载（§6），因此不与 M2-A / M2-B / W0-1
的锚点文件产生第三方冲突。

## 2. 路由映射（4 条，逐条对齐上游）

| method | path | 上游 handler | 本仓 |
|---|---|---|---|
| POST | `/api/issues/table/groups` | `ListIssueTableGroups`（`issue_table_group.go:892`） | `issue_table::table_groups` |
| POST | `/api/issues/table/rows` | `ListIssueTableRows`（`issue_table_rows.go:253`） | `issue_table::table_rows` |
| POST | `/api/issues/table/facets` | `ListIssueTableFacets`（`issue_table_facets.go`） | `issue_table::table_facets` |
| GET | `/api/issues/limit-usage` | `GetIssueLimitUsage`（`issue_limit.go:17`） | `issue_table::limit_usage` |

四个端点都要求**登录用户 + workspace 成员**（`auth_user::AuthUser` +
`invitations::require_workspace_member`）：非成员返回 **404**（`not_found("workspace")`，避免泄露
workspace 是否存在，与 M1 切片一致），缺用户头 401，坏 workspace uuid 400。

> `/api/issues/limit-usage` 是**静态段**，必须与 `issues.rs` 的 `/api/issues/:id` 共存。
> axum（matchit 0.7）静态段优先于参数段，因此 "limit-usage" 不会被当成 issue identifier；
> `tests/issue_table.rs::table_facets_and_limit_usage` 与 `issues.rs` 的既有 e2e 共同守住这一点。

## 3. 语义要点

### 3.1 `/groups`：分组计数

- **`total` 是全部匹配 issue 数**，不是页内之和：由 `SUM(issue_count) OVER ()` 在 ranked CTE 里
  一次算出（上游同），所以 `groups` 被 `page.limit` 截断之后 `total` 仍完整。
- **分组 key 带前缀**：`status:todo` / `priority:urgent` / `assignee:user:<uuid>` /
  `assignee:unassigned` / `project:<uuid>` / `project:none`（`sql.rs::group_key_of`）。响应里
  `key` 是上面这条字符串，`value` 是上游的对象形状
  （`{"kind":"status","status":"todo","actor":null}`）——由 `group_value()` 从 key 反推。
- **`include_empty`** 只对 `status` / `priority` 生效（`sql.rs::resolve_group`），实现是
  `expected`（`unnest(order_values)`）LEFT JOIN `actual`，再 UNION 补上"不在固定顺序里"的自定义
  key。`status` 的顺序来自 `issue_status` 目录 ∪ 7 个内置 key（`merge_status_order`：category
  open < closed → position → 内置优先 → 内置序 → key）。
- **分页按"分组"而不是"issue"**：`limit` 是分组数上限；SQL 取 `limit+1` 行，第 `limit+1` 行只用来
  生成 keyset cursor（`group_order, group_sort, group_value` 三元组），不返回给客户端。
- `group.kind=none` 在 `/groups` 上 **400**（上游 `group.kind must not be none`）。

### 3.2 `/rows`：行 + keyset 分页 + 层级

- **keyset 分页**：排序键 → `table_sort_key`（SQL 里 `(expr)::text`），cursor 携带
  `{sort_value, sort_is_null, row_created_at, row_id}`；`resolve_sort` 生成与上游一致的
  `ORDER BY`（可为空的时间列 `NULLS LAST`、`last_activity` 只按 `i.id` 打破平局、其余字段用
  `i.created_at, i.id` 兜底）。`limit+1` 行的最后一行用于 mint `next_cursor`。
- **`total` 只在"未分组根页头"出现**：仅 `cursor == nil && group.kind == none && parent_id == nil`
  才付全量 COUNT 的代价（上游 `issue_table_rows.go:458` 同）。DTO 里 `total: i64`（**不是**指针、
  也不是 `Option`），所以续页 / 分组页 / 子分支的 `total` 是 **0**，与上游零值行为逐字相同。
- **`branch_total`** = 本页实际行数（上游注释：`Current page size; retained for response
  compatibility`）。
- **层级**：`hierarchy.enabled=true` 时先物化 `membership`（当前分组内的成员集合），
  - `parent_id` 有值 → 该节点的**直接子**（且父必须在 membership 里）；
  - `parent_id` 为空 → 分支**根**（`parent_issue_id IS NULL` 或父不在 membership 里）；
  - `direct_child_count` 只在层级模式下真算（否则恒 `0::bigint`）。
  `parent_id` 不带 `hierarchy.enabled` → **400**（上游同）。

### 3.3 `/facets`：disjunctive 计数

- 每个 facet 一次独立查询（本仓实现；上游用 `GROUPING SETS` 批量扫描）。契约一致，代价是 N 次
  扫描，N ≤ `TABLE_MAX_FACETS = 32`。
- **disjunctive**：算 A 的取值分布时**去掉 A 自己的过滤**（`TableFilter::without_facet`）。例如
  `filters.statuses=["todo"]` 时，`status` facet 仍然列出 `done`（这样客户端才能从 done 切回去），
  而 `priority` facet 会按 `statuses=todo` 过滤。
- `include_total` 缺省 / `false` → `total = 0`（上游零值），只有显式 `true` 才付 `COUNT(*)`。
- 未指定 `facets` → 空数组（不报错），与上游零值一致。

### 3.4 过滤与作用域

- **`scope`**：`workspace`（缺省 / 空 kind）/ `project`（要求 `project_id`）/ `assignee` /
  `creator`（要求 `actor`）/ `my`（actor 取**登录用户**，`relation` ∈ assigned / created /
  involved / any）。
- **`filters.assignees` 的三态**（`Option<Vec<..>>`）：`null` = 不过滤；`[]` = 上游"显式空数组 =
  匹配 0 行"；非空 = 命中集合。这个区别也进指纹（`explicit_empty_assignees`），否则 `null` 与
  `[]` 会共享同一 cursor。
- **`include_no_assignee` / `include_no_project`** 把"未指派 / 无 project"并进结果集。
- **`search`**：空白切割后**每个词都要命中 `LOWER(title)`**（`ILIKE` 语义用 `LIKE` + 小写 +
  `escape_like`），**OR** `number` 命中（`ABC-45` 与裸数字都支持，`parse_query_number`）；只搜
  `title`，不搜 description（与上游同）。
- **`include_sub_issues`**：`Some(false)` 时排除有 parent 的 issue；`None` / `true` 不过滤。
- **`date`**：`field` ∈ `created_at` / `updated_at`，`start` / `end` 必填且必须 RFC3339。

### 3.5 指纹与 cursor

- `query_fingerprint` = `sha256:<hex>`，覆盖 workspace + scope + filters + search + sort（含
  **归一化后的**默认方向）与 `explicit_empty_assignees`；数组一律排序去重、`search` 去空白，
  因此"同一查询不同书写顺序"得到同一指纹（上游 `canonicalIssueTableFingerprint` 的意图）。
- cursor 是**纯 keyset 载荷**，`v` / `query`(指纹) / `group_key` / `parent_id` / 组游标三元组 /
  行游标四元组；服务端在翻页时用 `CursorWire::matches` 校验
  `(指纹, group_key, parent_id)` 三者任一不符 → **409 `cursor_query_mismatch`**。
- **客户端不得解析 cursor**（不透明），因此本仓用 hex 编码 JSON 而不是上游的
  `base64.RawURLEncoding`（§4.8）。

### 3.6 请求体与页大小

- 请求体上限 **1 MiB**（上游 `http.MaxBytesReader(w, r.Body, 1<<20)`）；超限与 JSON 语法错误
  都归到 400。
- **未知字段 → 400**：所有 DTO 都带 `#[serde(deny_unknown_fields)]`，镜像上游的
  `DisallowUnknownFields`。
- `page.limit`：缺省 / 0 → **50**，范围 **[1, 100]**；越界 400。`TABLE_DEFAULT_PAGE_SIZE` /
  `TABLE_MAX_PAGE_SIZE` 与上游常量同名同值。
- `facets` 数量 > 32 → 400。

## 4. 与上游的有意偏离（请 master 逐条确认）

**仓储层**（`crates/mc-repos/src/issue_table/mod.rs` 模块注释同款清单）：

1. **不开启 `REPEATABLE READ READ ONLY` 快照事务**：上游三个查询共用同一只读快照事务；本仓
   `Db` 没有 `TxStarter` 等价物，三个入口各自查询。影响是"同一请求内的多个查询可能看到不同时间
   点"（facets 的 N 次扫描之间理论上可被并发写打断）。**不改变**任何单次查询的结果。
2. **角色列是 `TEXT` 而不是 `uuid`**：本仓 0001 的 `issue.assignee_id` / `creator_id` 是 `TEXT`
   （`assignee_type` + id 一起表达 actor）。谓词因此用 `::text` 比较，非法 uuid 由 **HTTP 层**
   拦成 400（仓储层不解析）。
3. **`assignee_type` 取值是 `user|agent|squad|autopilot`**（本仓 0001 的 CHECK），上游是
   `member|agent|squad`。HTTP 层把上游写法的 `member` 归一化成 `user`（`normalize_actor_type`），
   与 M2-A / M2-B 同款处理；`scope.kind=my` 因此固定用 `'user'`。
4. **不支持的维度显式 422，绝不编造计数**：`label` / `property` / `parent` / `status_category` /
   `compound` 分组与 `working_agents` / `property` facet → 422 `unsupported_group`；
   `filters` 的 `project_statuses` 非空 / `label_ids` 非空 / `properties` 非空 / `working_only=true` /
   `working_issue_ids` 非 null → 422 `unsupported_filter`。**空数组 / 空对象 / `null` 等价于"未提供"**
   （上游也不会为它们加谓词），所以"客户端把全部字段都发一遍"的常规请求不会被误拒。
   根因：本仓没有 `issue_label` / `issue_to_label` / `issue_properties` 表（缺口实测见
   `docs/10-M2-PLAN.md` §5、承接项 LUM-1370 + W0-B2）。
5. **没有 `squad_member` 表** → `scope.relation=involved` 只覆盖"agent 归属"与"squad leader"
   两条支路（上游还有 squad member / 订阅者等支路）。
6. **`priority` 分组 / `priority` facet / `priority` 排序是本仓新增维度**（上游没有）：本仓
   `issue.priority` 是一等列且是 CHECK 约束，做成维度比"猜上游以后会加"更诚实。响应里多一个
   `value.priority` 字段；`priority` 分组的固定顺序 = 5 个 CHECK 值
   `urgent > high > medium > low > none`。
7. **facet 逐个查询**而非上游的 `GROUPING SETS`：契约（计数与取值集合）一致，差异只在 SQL 形状
   与扫描次数（§3.3）。

**HTTP 层**（`crates/mc-http/src/routes/issue_table/mod.rs` 模块注释同款清单）：

8. **cursor 是 hex 编码的 JSON**，上游是 `base64.RawURLEncoding`：沿用 M2-C `routes/inbox.rs`
   的先例，避免给 mc-http 增依赖。cursor 对客户端不透明（上游文档也不承诺稳定编码），因此这不是
   契约破坏；**已知非等价登记**：若上游客户端缓存了旧 cursor 再打到本仓，会得到 400
   `invalid cursor`（一次性、可恢复）。
9. **`scope.kind=my` 的 actor 取当前登录用户**，请求里带的 `actor` 被忽略（上游从 session 取）。
10. **`GET /api/issues/limit-usage` 恒 204**：本仓没有 entitlement / Cloud 订阅源，等价于上游
    `policy.Action != ActionEnforce` 的分支（"未强制限额，无 usage 可报"），而不是伪造一个
    `{"used":…,"limit":…}`。前端拿 204 应理解为"不展示限额 UI"。
11. **400 的错误体用本仓形状** `{"error":{"code","message"}}`（`mc-errors` 的约定）；**409 / 422
    用上游的扁平形状**（`{"error":"cursor_query_mismatch",…}` / `{"error":"unsupported_group","code","message"}`）。
    这是刻意的：409/422 的 `error` 字段是上游客户端**已消费**的枚举值，形状不能动；400 的通用形状
    由 M1/M2-A 已确立，也不在本切片改动。
12. **`group.kind=status_category` 的 `category_format` 字段被接受但忽略**（`GroupDto.category_format`
    带 `#[allow(dead_code)]`）：上游已安装客户端会带上它，拒收会误伤整个请求；该维度本身走
    §4.4 的 422 路径。

## 5. 排序与分组维度（支持面）

**`query.sort.field`**（`TableSortField::parse`；空 = `position`）：

| field | 默认方向 | 空值 | 打破平局 |
|---|---|---|---|
| `position` | asc | 不适用 | `created_at, id` |
| `title` | asc | 不适用 | `created_at, id` |
| `created_at` / `updated_at` | asc | 不适用 | `created_at, id` |
| `last_activity` | **desc** | `NULLS LAST` | **只按 `id`** |
| `start_date` / `due_date` | asc | `NULLS LAST` | `created_at, id` |
| `status` | asc | 不适用 | 按 workspace status 目录 rank |
| `priority` | asc | 不适用 | 按 5 个 CHECK 值 rank |

上游的 `property:<id>` 之类属性排序**不支持**：`TableSortField::parse` 返回 `None` → **400**
`invalid query.sort.field`（上游会把它翻译成 `issue_properties` 上的 join，本仓没有该表；见 §4.4）。
非法方向同样 400。

**`group.kind`**：`none`（仅 `/rows`）| `status` | `priority`（本仓新增）| `assignee` | `project`。
**`facets[].kind`**：`status` | `priority`（本仓新增）| `assignee` | `creator` | `project`。
其余取值一律 422 `unsupported_group`（`group_kind_unsupported` / `facet_kind_unsupported`），
带 `property_id` 的 facet 也是 422。

**固定值的 wire 形式**：`assignee:unassigned` ↔ `GROUP_VALUE_UNASSIGNED = "__unassigned__"`；
`project:none` ↔ `GROUP_VALUE_NO_PROJECT = "__no_project__"`；facet 的无值桶用
`FACET_VALUE_NONE = "__none__"`。

## 6. 文件布局与路由挂载（为什么这样拆）

- **仓储层 / HTTP 层的边界**：仓储层只接受"**已校验**的规格 → SQL → 行"；HTTP 概念
  （DTO 字段名、`deny_unknown_fields`、字符前缀 `status:` 的解析、指纹、cursor 编解码、
  400/409/422 的分类）全在 `mc-http`。上游把两者写在同一个 handler 文件里，本仓拆开的理由是
  **R7（单文件 800 行）**：直接把上游三个 handler 合起来会得到一个 3000+ 行文件，且 M2-A
  （`routes/issues.rs`）的文件已经超标，不能再往里面堆。
- **`issue_table` 四文件**（`mod.rs` / `sql.rs` / `repo.rs` / `tests.rs`）同样受 R7 约束：规格与结果类型、
  SQL 文本、执行、测试各占一份，四份分别 547 / 618 / 396 / 415 行。`sql.rs` 里的运行时参数绑定用
  `Param` enum + `QueryBuilder` 手工维护 `$n` 序号，**不引入** `sqlx::QueryBuilder` 的
  `Any` 泛型（与 M2-A 的"构建期不需要数据库"约定一致：`query_as` + 运行时 `.bind()`，
  不用 compile-time 宏）。

### 6.4 R7 单文件上限：本切片的超标已消除（LUM-1416）

本切片（M2-D）交出的 `crates/mc-http/src/routes/issue_table.rs` 当时是 **1789 行**，超出
`docs/plan1.md` R7 的"单文件 800 行硬上限"；原因是一次性把上游同一份 handler 文件里的
"DTO + 校验 + 指纹 + cursor + 四个 handler + 单元测试"搬了过来，没有再切分。
**该超标已由 LUM-1416 拆分消除**（纯移动、零行为变化），四份的实测行数：

| 文件 | 行数 | 内容 |
| --- | ---: | --- |
| `routes/issue_table/mod.rs` | 509 | 模块 doc + `pub fn router()` + 4 个 handler + `authorize` / `table_repo` + 响应 DTO + `TableError` |
| `routes/issue_table/spec.rs` | 790 | 请求 DTO + `decode_body` + `build_*` 校验 + `query_fingerprint` + 分组 key/value 映射 |
| `routes/issue_table/cursor.rs` | 145 | `CursorWire` + `decode_cursor` |
| `routes/issue_table/tests.rs` | 433 | 原有单元测试（11 条纯函数测试） |

与拆分前登记的“建议的切分”的差异（见本文 `git log` 里的初版 §6.4）：**响应 DTO 与 `TableError`（错误→HTTP 形状）落在 `mod.rs`**
而不是全部挤进 `spec.rs`（`spec.rs` 若把响应 DTO 也算上会越过 800）；`spec.rs` 自己多了
`group_identity` / `group_value` / `parse_group_key`（上游 `issue_table_group.go` 那一半）。
文件从 `routes/issue_table.rs` 变成目录模块，对 `routes/mod.rs` 的 `pub mod issue_table;`
与 `super::` 路径透明，路由挂载点（`issues::router()` 末尾的 `.merge`）未动。

上限现在**有机器执行**：`scripts/file_size_check.py`（存量违规钉在
`scripts/file_size_baseline.tsv`，只减不增）+ `scripts/gates.sh` 的门 ⑩ `file-size`
（进默认集合、CI 在 `fast` job，见 `docs/24-W0-CI.md` §12）。本文件拆分后已从该基线里消失；
全仓剩下 14 个存量违规（`routes/issues.rs` 2227、`mc-repos/src/issue.rs` 1950、`routes/auth.rs` 1704、…）
已逐行登记，它们只允许变短。
- **挂载点**：`issues::router()` 末尾 `.merge(super::issue_table::router())`。选择 merge 而不是
  新开 `mount_slice_*` 的理由：这四个路由的逻辑归属就是 issue 资源（上游也在同一份 handler 里
  注册），而且 `mount.rs` / `routes/mod.rs` 是 M2-A / M2-B / M2-C / W0-1 多分支的共享锚点文件，
  不碰它们能让本切片与其它在飞分支零冲突。
- **测试布局**：仓储层的 DB 测试放在 `issue_table/tests.rs`（`mod.rs` 里 `#[cfg(test)] mod tests;`，
  与 `crate::issue` 的 `db_tests` 一致：`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）；HTTP e2e 放在
  `crates/mc-http/tests/issue_table.rs`（与 `tests/issues.rs` / `tests/inbox.rs` 一致）。两者都
  **不**接进 `scripts/gates.sh` 的默认集合（默认集合离线可跑），由 `--with-db` 的 `db` 门覆盖。

## 7. 验证证据

```text
# 1) 编译 + lint（全 workspace 的 pedantic 门）
cargo clippy --workspace --all-targets --features mc-http/test-util -- -D warnings   # 0 warning

# 2) 仓储层 6 条 PG 测试
MULTICA_TEST_DATABASE_URL=… cargo test -p mc-repos --lib -- --ignored issue_table
# test result: ok. 6 passed; 0 failed; 0 ignored
#   db_table_groups_counts_by_status / db_table_groups_paginates_with_cursor
#   db_table_rows_keyset_pagination / db_table_rows_filtered_by_group_key
#   db_table_facets_disjunctive_counts / db_table_filters_by_assignee_scope

# 3) HTTP e2e 4 条
MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --test issue_table --features test-util -- --ignored
# test result: ok. 4 passed; 0 failed; 0 ignored
#   table_groups_counts_by_status / table_rows_paginates_and_rejects_cursor_mismatch
#   table_facets_and_limit_usage / table_contract_errors

# 4) 单元测试（拆分后 11 条纯函数测试全部保留）
env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL cargo test -p mc-http --lib -- issue_table
# test result: ok. 11 passed; 0 failed; 0 ignored

# 5) 真库门 + ⑩ 尺寸门（`--with-db` = 10 门；⑥ 需要真 PG）
MULTICA_TEST_DATABASE_URL=… bash scripts/gates.sh --with-db
#   ① ② ③ ④ ⑤ ⑥ ⑦ ⑨ ⑩ → 10/10 green；⑥ 里 issue_table 的 6 条仓储测试 + 4 条 e2e 逐条 ok
```

e2e 覆盖到的契约点：`total` 语义（分组全额 vs 续页 0）、`value` 对象形状、`include_empty` 补齐
7 个内置状态、keyset 翻页无重无漏、`409 cursor_query_mismatch`、disjunctive facet、`limit-usage`
204、未知字段 400、`label` 分组 422、`label_ids` 过滤 422、`group.kind=none` 在 `/groups` 400、
`page.limit=101` 400、非成员 404。

## 8. 交接与后续

- **`/api/issues` 的 `POST` 校验缺口**（assignee 存在性、attachment uuid）不属于本切片，已登记为
  **LUM-1410**。
- **label / property 维度**依赖上游 schema（`issue_label` / `issue_to_label` / `issue_properties`），
  当前实现是**显式 422**。承接：LUM-1370（label 维度）+ W0-B2（schema 切换，与 M3-0 互斥，见
  `docs/25-W0-SCHEMA-DRIFT.md`）。
- **快照一致性**（§4.1）如果 M3 引入 `TxStarter` 等价物，可以一次性把三个入口包进
  `REPEATABLE READ READ ONLY`，本切片不阻塞。
- **`facet` 批量扫描**（§4.7）在 facet 数量上限 32 的前提下不是瓶颈；若 M3 之后需要，可换成
  `GROUPING SETS` 而不改契约。
