# 59 · M2-E 标签 / 属性定义目录（LUM-1370）

M2 的**最后一块端到端前置**：上游的 **label 目录**（`/api/labels`）与 **custom property
定义目录**（`/api/properties`）。M2-A 只落了 issue-**从属**的
`/api/issues/{id}/labels`、`/api/issues/{id}/properties/{propertyId}`，而定义目录在
`docs/10` §0/§2 里从未出现（`docs/10` §5.2 记为「覆盖盲区」）⇒ 当时那 3 条 labels 路由是
**恒 501 的桩**、properties 的写面**只往 JSONB 里塞键值、没有类型契约**。本片把它们补成真实现。

- **记录号**：本片用 **`59`**（预约见 `docs/37` §44.6 的顺延：`57`=M6 计划 / `58` 未用 /
  `59`=M2-E）。`docs/10` §5.2 的「M2-E（LUM-1370，backlog）」就是本片。
- **上游基准**：`louloulin/multica` @ `f41fae6b`（与 `scripts/route_parity.py` 内嵌同一 commit）。
  参照文件：`server/internal/handler/label.go`（708 行）、
  `server/internal/handler/property.go`（860 行）、`server/internal/issueproperty/value.go`、
  `server/pkg/db/queries/issue_label.sql`、`.../issue_property.sql`、
  `server/cmd/server/router.go:2028-2050`（两块目录）+ `:181-185`（issue 侧 labels）。
- **基线**：`origin/feat/multica-rs-initial` @ **`8d33080`**（合并 `#64` 之后的 docs-only 直推）。
  本分支从 `8d33080` 起。
- **无新依赖、无新迁移**（见 §2）：`Cargo.toml` / `Cargo.lock` / `migrations/**` 一行未动。

## 0. 交付物

| 文件 | 行数 | 内容 |
| --- | ---: | --- |
| `crates/mc-repos/src/label.rs` | 676（新） | 常量 / `LabelRow` / `LabelListRow` / `NewLabel` / `LabelUpdate` / `AssignmentOutcome` / `parse_resource_type` / `validate_name` / `normalize_color` / `LabelRepo`（9 方法）+ 4 纯单测 + 2 真库用例 |
| `crates/mc-repos/src/property.rs` | 582（新） | `MAX_*` / `PROPERTY_TYPES` / `PROPERTY_ICONS` / `RESERVED_NAMES` / `ACTOR_KINDS` / 行与输入类型 / `PropertyError` / `PropertyRepo`（7 方法）/ advisory lock helper / 值面自由函数（`definition_for_value` / `resolve_actor_refs` / `definition_exists`） |
| `crates/mc-repos/src/property/validation.rs` | 424（新） | 名字 / 图标 / 类型 / 描述 / config 校验 + `parse_config` / `validate_value` / `actor_refs_in_value` / `bag_exceeds_limit` / `merge_bag` / `is_http_url`（**R7 拆出**，见 D17） |
| `crates/mc-repos/src/property/tests.rs` | 423（新） | 12 例纯单测（校验矩阵：9 类型 × 合法/非法值、保留名、图标、config、actor ref、null 字节） |
| `crates/mc-repos/src/property/db_tests.rs` | 412（新） | 4 例真库用例（**R7 拆出**，见 D17） |
| `crates/mc-repos/src/lib.rs` | +2 | 两行 `pub mod`（`label` / `property`） |
| `crates/mc-http/src/routes/labels.rs` | 438（新） | 目录面 **10** 注册键 + 5 handler；issue 侧 3 个 `pub(crate)` handler（`list_issue_labels` / `attach_label` / `detach_label`）+ `LabelResponse` / `IssueLabelsResponse` |
| `crates/mc-http/src/routes/properties.rs` | 376（新） | 目录面 **8** 注册键 + 4 handler + `PropertiesQuery` / `CreatePropertyRequest` / `UpdatePropertyRequest` / `PropertyResponse` |
| `crates/mc-http/src/routes/mod.rs` | +6 | 两行 `pub mod`（`labels` / `properties`）+ 4 行注释，插在 `pub mod webhooks;` 之后 |
| `crates/mc-http/src/routes/mount.rs` | +11 | 1 行 `.merge(mount_slice_label_property())`（插在 `mount_slice_subscriber()` 之后）+ `mount_slice_label_property()` 定义（追加在同族函数之后） |
| `crates/mc-http/src/routes/issues/mod.rs` | +20 / −4 | `/labels` 三键换真 handler（含**补上** `POST`）+ 模块文档 |
| `crates/mc-http/src/routes/issues/dto.rs` | +42 | `SetPropertyValueRequest`（三态，D9）+ 永久 `mod tests` 守卫 |
| `crates/mc-http/src/routes/issues/extras.rs` | +72 / −15 | `set_property` / `delete_property` 改成「定义面先行」 |
| `crates/mc-http/tests/label_property.rs` | 753（新） | 2 例 e2e（真库 + 真 router） |
| `crates/mc-http/tests/issues/auth.rs` | +5 / −4 | 501 canary 换端点（D18） |
| `crates/mc-http/tests/issues/reactions.rs` | +7 / −16 | properties 块改断言（D18） |

合计 **16 文件 +4249 / −39**（`git diff --numstat 8d33080 -- crates`）。

**共享文件只做「插入新行」**（`LUM-1665` M6-0 anchor 同时写 `mount.rs` / `routes/mod.rs` /
`mc-repos/src/lib.rs` / `docs/fixtures/*`）：本片对这四个文件**没有修改任何已有行**，只在
`mount_slice_subscriber()` 之后、`pub mod webhooks;` 之后、`pub mod issue_table;` /
`pub mod project_resource;` 之后**各插入若干新行**（`git diff` 里只有 `+`，无 `−`）。
两片合并顺序 = anchor 先、本片后；万一 git 报冲突，**保留双方的新增行**即可。
`issues/mod.rs` / `issues/dto.rs` / `issues/extras.rs` 不在 anchor 写集内 ⇒ 自由编辑。

## 1. 路由与注册键

| # | 上游 handler | 本地 handler | 注册键（规范形态） | 成功码 |
| ---: | --- | --- | --- | ---: |
| 1 | `ListLabels`（`label.go:144`） | `routes/labels.rs::list_labels` | `GET /api/labels` | 200 |
| 2 | `CreateLabel`（`:192`） | `create_label` | `POST /api/labels` | **201** |
| 3 | `GetLabel`（`:166`） | `get_label` | `GET /api/labels/:id` | 200 |
| 4 | `UpdateLabel`（`:240`） | `update_label` | **`PUT`** `/api/labels/:id` | 200 |
| 5 | `DeleteLabel`（`:309`） | `delete_label` | `DELETE /api/labels/:id` | **204** |
| 6 | `ListProperties`（`property.go:421`） | `routes/properties.rs::list_properties` | `GET /api/properties` | 200 |
| 7 | `CreateProperty`（`:467`） | `create_property` | `POST /api/properties` | **201** |
| 8 | `GetProperty`（`:444`） | `get_property` | `GET /api/properties/:id` | 200 |
| 9 | `UpdateProperty`（`:544`） | `update_property` | **`PATCH`** `/api/properties/:id` | 200 |
| 10 | `ListLabelsForIssue`（`:395`） | `issues/mod.rs` → `super::labels::list_issue_labels` | `GET /api/issues/:id/labels` | 200 |
| 11 | `AttachLabel`（`:419`） | `attach_label` | `POST /api/issues/:id/labels` | 200 |
| 12 | `DetachLabel`（`:501`） | `detach_label` | `DELETE /api/issues/:id/labels/:labelId` | 200 |
| 13 | `SetIssueProperty`（`:675`） | `issues/extras.rs::set_property` | `PUT /api/issues/:id/properties/:propertyId` | 200 |
| 14 | `DeleteIssueProperty`（`:770`） | `delete_property` | `DELETE /api/issues/:id/properties/:propertyId` | 200 |

**注意方法逐字对齐上游**：label 的更新是 **`PUT`**（不是 PATCH）、property 定义是 **`PATCH`**。

**尾斜杠双形态（D2）**：上游 `/api/labels` / `/api/properties` 都是
`r.Route(...) + Get/Post("/")` 形态（chi `Mount` 同时服务 `<P>` 与 `<P>/`）⇒ 目录面
**每个方法注册两个形态**，方法集合逐字相同：

- `labels.rs::router()` 10 键 = `/api/labels` [GET, POST] + `/api/labels/` [GET, POST]
  + `/api/labels/:id` [GET, PUT, DELETE] + `/api/labels/:id/` [GET, PUT, DELETE]；
- `properties.rs::router()` 8 键 = `/api/properties` [GET, POST] + `/api/properties/`
  [GET, POST] + `/api/properties/:id` [GET, PATCH] + `/api/properties/:id/` [GET, PATCH]。

**issue 侧那 3 键是 plain 子路由**（`router.go:181-185` 的 `r.Get("/labels")`）⇒ 上游只有
**一个**形态，**不加**尾斜杠别名（否则门 ⑦ 的第二条命令 `slash_alias_audit.py` 告警）。
注册位置不动（就在 `issues/mod.rs` 的原行上原地换 handler，见 D18）。

**门 ⑦ 增量（唯一判据）**：

```
基线 8d33080：upstream 456 | local 329 registered | implemented 263 real + 2 placeholder = 265/456
              known_gap 191 | unclaimed 0 | regressions 0 | local_only 11 | slash_aliases 53
              owners: M2-E 9 / M2-A 14 / M6 55 / M9 33 / M7 24 / M8 24 / M3+ 16 / M3 11 / M10 5
本片 HEAD ：upstream 456 | local 348 registered | implemented 273 real + 2 placeholder = 275/456
              known_gap 181 | unclaimed 0 | regressions 0 | local_only 11 | slash_aliases 62
              owners: M2-E **0**（条目消失）/ M2-A 13 / 其余不变
```

- **+19 注册键 = 10 条上游规范形态 + 9 条尾斜杠别名**（`slash_aliases` 53 → 62 正好 +9）。
- **+10 implemented**：9 条 `owners.M2-E` 全部关掉，外加 `POST /api/issues/{id}/labels`
  （原来挂 `owners.M2-A`，M2-A 14 → 13）——即 M2-A 遗留的那条 labels 缺口也一并闭环。
- `known_gap 191 → 181`（−10），`regressions 0`、`unclaimed 0` 不变。
- 顺带确认：本片上界内**没有**新增 `local_only` 路由（`/api/labels` 等全部是上游已有路径）。

## 2. 数据模型与迁移（为什么零迁移）

`docs/10` §5.1（2026-09-22 18:00 快照，基于当时的 `fd6dfd6`）判定
`issue_label` / `issue_to_label` / `issue_properties` **不存在**、并禁止在 M2 切片里新开迁移号。
**该结论已被 W0-B2 的 upstream schema 镜像推翻**：本仓 `migrations/upstream/` 现已逐字包含
上游全部 566 条迁移 ⇒ 四张表/列**全部现成**，本片一行迁移都没写：

```text
issue_label(id, workspace_id FK→workspace ON DELETE CASCADE, name, color,
            created_at, updated_at, resource_type, description)      -- 001:75 + 059 + 162
  UNIQUE (workspace_id, resource_type, LOWER(name))                  -- 194
issue_to_label(issue_id, label_id)  -- 复合主键，两侧 ON DELETE CASCADE -- 001:82
issue_property(id, workspace_id, name, type, description, config JSONB DEFAULT '{}'
               CHECK jsonb_typeof = 'object', position FLOAT DEFAULT 0,
               archived_at, icon)                                    -- 191（+341 补 actor 类型）
  UNIQUE (workspace_id, LOWER(name))                                 -- 194
issue.properties JSONB NOT NULL DEFAULT '{}'                         -- 0001:153
  CHECK jsonb_typeof = 'object' + 16KB 上限                          -- 191
```

三个**必须知道**的 schema 事实（写在这里免得下一个人再踩）：

1. 上游表名是 **`issue_property`（单数）**，而迁移**文件**叫 `191_issue_properties.up.sql`；
2. `193` **删掉了 `issue_property` 的 workspace 外键** ⇒ 真库用例清场必须**显式删行**，
   不能靠 `DELETE FROM workspace` 级联；
3. 三张资源侧关联表（`issue_to_label` / `agent_to_label` / `skill_to_label`）**没有外键**
   （上游注释：避免未经评审的级联锁与审计行为）⇒ 删目录行时必须在**同一事务**里显式清关联（D14）。

## 3. 逐状态码契约

### 3.1 label 目录

| 触发 | 状态码 | body |
| --- | ---: | --- |
| `GET /api/labels` | 200 | `{"labels":[…],"total":n}`；`?resource_type=` 非法 → 400 |
| `POST /api/labels` | 201 | `LabelResponse` |
| `GET /api/labels/:id` | 200 / 404 | `LabelResponse` |
| `PUT /api/labels/:id` | 200 / 404 / 409 | `LabelResponse` |
| `DELETE /api/labels/:id` | **204** | 空体 |
| `GET /api/issues/:id/labels` | 200 / 404 | `{"labels":[…],"issue_revision":n}`（**恒**带 revision） |
| `POST /api/issues/:id/labels` | 200 / 400 / 404 | `{"labels":[…] [,"issue_revision":n]}` |
| `DELETE /api/issues/:id/labels/:labelId` | 200 / 400 / 404 | 同上 |

`LabelResponse` = `{id, workspace_id, resource_type, name, description, color, usage_count,
created_at, updated_at}`（字段名逐字对齐上游 `LabelResponse`）。

- `POST` 的 `label_id` 为空串 → 400 `label_id is required`（**在** workspace 解析之前）；
- attach/detach **幂等**：`changed == false`（重复挂/摘）时**不返回** `issue_revision` 字段
  （上游 `if attached.IssueRevision > 0`）——这是**可观测差异**，e2e 逐条断言了；
- 跨 workspace 的 `:labelId` → 404（不泄漏存在性）；`resource_type != "issue"` 的标签
  attach 到 issue → 404 `issue label not found` 同款语义；
- `usage_count` 口径 = 按 `resource_type` 在 issue/agent/skill 三张关联表间切换计数（D13）。

### 3.2 property 定义目录

| 触发 | 状态码 | body |
| --- | ---: | --- |
| `GET /api/properties` | 200 | `{"properties":[…],"total":n}`；`?include_archived=true` 才含归档 |
| `POST /api/properties` | 201 | `PropertyResponse` |
| `GET /api/properties/:id` | 200 / 404 | `PropertyResponse` |
| `PATCH /api/properties/:id` | 200 / 400 / 404 / **409** | `PropertyResponse` |
| 活跃定义到顶（20） | 400 | `a workspace cannot have more than 20 active properties; archive unused ones first` |
| 名字/类型/图标/config 非法 | 400 | 逐字对齐上游（`invalid type "x"; valid types: …` 等） |
| 删掉仍被引用的 select 选项 | **409** | 消息列出 `"选项名" (N issues)` |
| 重名（同 workspace，大小写不敏感） | 409 | `a label/property with that name already exists` 同款 |

- 定义**只归档不删除**（`archived: true`）；`type` **不可变**（改类型 = 归档旧的 + 建新的）；
- `archived_at` **恒**出现在响应里（未归档为 `null`）——与上游 `ArchivedAt *string` 无
  `omitempty` 一致（D19）；
- **写面只有 owner/admin**，读面无门（与上游一致；本仓补成员门见 D5）。

### 3.3 issue 侧的值面（`PUT|DELETE /api/issues/:id/properties/:propertyId`）

`set_property` 的执行顺序（**逐字对齐上游**，顺序本身是契约）：

1. `property id` 非 UUID → 400；body 不是 JSON → 400 `invalid request body`；
2. `resolve_workspace` + 成员门 → 404；issue 不在 workspace → 404；
3. **取定义**：不存在 → 404 `property not found`；已归档 → 400；
4. 缺 `value` 字段 → 400 `value is required`；显式 `null` → 400
   `value cannot be null (use DELETE to unset a property)`；
5. `validate_value`（按类型：text ≤2000 runes / url ≤2048 且 http(s) / date `YYYY-MM-DD` /
   select 必须在 options 内 / actor `"<kind>:<uuid>"`，kind ∈ `["member"]`…）→ 400；
6. actor 类值 `resolve_actor_refs`（成员不存在 → 400）；16KB 预检 → 400；
7. 写 JSONB（键 = 定义 UUID 文本）→ 200 `{"properties":{…},"issue_revision":n}`。

`delete_property`：非 UUID → 400；定义不存在 → 404；**已归档的定义仍允许删值**
（上游注释：cleanup 不能被阻塞）；响应**恒**带 `issue_revision`（D10）。

## 4. 与上游的偏差（逐条可查）

| # | 偏差 | 判据 / 理由 |
| ---: | --- | --- |
| **D1** | **零迁移**：四张表/列直接复用 `migrations/upstream/**` | `docs/10` §5.1 的「表不存在」是 W0-B2 之前的快照，现已过时（§2）。本片同时更正了 `docs/10` §5.1 / §5.2 与 `docs/11` §6 |
| **D2** | **尾斜杠双形态只给目录面**；issue 侧 3 键单形态 | 上游目录面是 chi `Mount`（两形态都服务），issue 侧是 plain `r.Get("/labels")`。门 ⑦ 的 `slash_alias_audit.py` 把「上游没有的别名」当缺陷 ⇒ 双形态必须与上游形态**逐一对应**，不能一刀切 |
| **D3** | 错误体文案不同（**状态码一致**）：404 = `not found: label` / `not found: property`，400 = `validation error: {上游原文}`，409 = 本仓 `Conflict` 渲染 | 本仓 `mc_errors::Error::{NotFound,Validation,Conflict}` 是**全仓统一**渲染（`Error::Validation` 的 Display 带 `validation error: ` 前缀），没有「原样字符串」变体。用户可见的**状态码**逐条一致，这是本片与既有切片的共同约定 |
| **D4** | 路径参数非法时的消息形态：上游 `invalid property id` / `invalid label id`；本仓共享 helper `parse_target_id` 出 `property id must be a uuid` | 复用 `issues/context.rs::parse_target_id`（M2-A 起 20+ 个端点同款）。只改文案、不改状态码 |
| **D5** | 补成员门：`require_workspace_member`（目录读 + issue 侧全部）/ `require_workspace_admin`（property 定义写） | 上游 workspace 来自 session（`h.resolveWorkspaceID(r)`），**没有**「非成员」这个状态；本仓 workspace 从 query/header 显式解析 ⇒ 必须自己关门。非成员 → 404（与 issue 面一致），角色不足 → 403 `workspace admin role required` |
| **D6** | `actor == "agent"` 的 403 `agents cannot manage property definitions` **不可实现** | 本仓 mc-http 的请求身份只有 `X-Multica-User-Id`（`AuthUser`），没有 agent 主体 ⇒ 无法区分「agent 发的请求」。登记为**能力缺口**，见 §6.3 |
| **D7** | 值面无**单事务**：`definition_for_value`（读定义）与 `IssueRepo::set_property`（写 JSONB）是**两次独立**往返，上游在 `prop:<id>` advisory lock + 一个事务内一气呵成 | 本仓 `IssueRepo::set_property` 是既有的纯 JSONB setter（M2-A 交付、被 metadata/reactions 面共用），把它包进「先读定义再写」的事务需要改 `mc-repos` 的**公共**签名（跨切片写集）。**后果**：定义在「读取后、写入前」被归档的那条窄窗口内，值仍会被写入（上游被锁挡住）。窗口内不产生新数据面，只是多一个已归档定义的旧值 ⇒ 登记为已知偏差，是否合事务留给后续 cycle |
| **D8** | 16KB 上限**预检**在路由层（`bag_exceeds_limit`），DB 的 `issue_properties_size_limit` CHECK 仍是底线 | 上游 `isCheckViolation(err)` 把 CHECK 违反翻成 400；本仓不复用这条路径（CHECK 违反在 `mc-repos` 里会落成 500）⇒ 先算一遍「合并后的袋子是否超限」，超了就直接 400 上游文案 |
| **D9** | `SetPropertyValueRequest.value` 是 `Option<Option<JsonValue>>`（+ `deserialize_some`） | 上游解码到 `map[string]any` 后靠 `v, ok := m["value"]` 区分「字段缺失」（→ `value is required`）与「显式 null」（→ `value cannot be null…`）。serde 默认把两者都折成 `None` ⇒ 必须自定义。永久单测（`dto.rs::tests`）守住这条语义 |
| **D10** | `delete_property` 删值后**多一次** `repo.get` 取 `issue_revision` | 上游 `DELETE` 返回整行 issue（`updated.Revision`）；本仓 `delete_property` 只回 JSONB 袋子 ⇒ 补一条只读查询换响应形状一致 |
| **D11** | 活跃上限 20 的**计数口径**与并发防护：`archived_at IS NULL` 计数 + `props:<ws>` advisory xact lock 下 read-then-write | 逐条照上游 F5（上游也是锁内 count）。真库并发用例 `db_active_cap_is_20_and_holds_under_concurrency` 实证：两个并发 create 只有一个成功，失败方拿 `PropertyError::ActiveCap(20)`，终态 active 恰好 20 |
| **D12** | 值校验函数住在 `mc-repos::property`，**调用点**在 mc-http 路由层 | 上游 `ValidateValue` 在 `internal/issueproperty`（纯函数包），HTTP handler 调它。本仓分层等价：`mc-repos` 不依赖 HTTP，路由层做编排 |
| **D13** | `usage_count` 用 `CASE l.resource_type WHEN 'issue'/'agent'/'skill' THEN (SELECT COUNT(*) …) ELSE 0 END` | 与上游 `issue_label.sql:3-8` 逐字同构（三张关联表分别是 `issue_to_label` / `agent_to_label` / `skill_to_label`） |
| **D14** | 删目录行时**同一事务**里先清关联再删行；`DELETE … RETURNING id` 区分 404 与基础设施错 | 三张关联表没有外键（§2 第 3 条）⇒ 不清就是孤儿行。用 `RETURNING` 而不是「先 SELECT 再 DELETE」是为了不留 TOCTOU 预检 |
| **D15** | 颜色正则 `^#?[0-9a-fA-F]{6}$` + 归一为小写 `#rrggbb`；**不动** 6 位限制 | 上游注释写明这是 **LOAD-BEARING INVARIANT**：前端 `LabelChip` 直接把它当 `backgroundColor` 用，放宽即 inline-style 注入面 |
| **D16** | `validate_name` 的判定顺序：**先查控制字符**，再 `trim`，再长度，再保留名 | 逐字照上游（`label.go:124` / `property.go:189`）⇒ `"\t"` 报的是「不能含控制字符」而不是 `name is required`。保留名按**规范化形态**比较（`Priority` / ` due date ` 也拒） |
| **D17** | **R7 文件拆分**（门 ⑩ 800 行）：`property.rs`(1006) → `property/validation.rs`(424)；`property/tests.rs`(837) → `property/db_tests.rs`(412) | 拆分手法：① 校验纯函数拆兄弟文件 + `pub use self::validation::*;` 重导出（调用方 `use mc_repos::property::validate_value` 不受影响），跨文件私有项提 `pub(crate)`；② 真库用例从 `tests.rs` 拆成兄弟测试模块 `db_tests.rs`（`property.rs` 多一行 `#[cfg(test)] mod db_tests;`），共享夹具 `select_config()` 提为 `pub(super)`。**注意**：门 ⑩ 只扫 `git ls-files` 里**已跟踪**的文件 ⇒ 新增文件必须先 `git add` 再跑门，否则 837 行会**假绿** |
| **D18** | 两条既有测试因**行为变更**而改：`tests/issues/auth.rs` 的 501 canary 从 `GET /api/issues/:id/labels` 挪到 `GET /api/issues/:id/pull-requests`（M9 仍缺）；`tests/issues/reactions.rs` 的 properties 块改断言「非 UUID property id → 400 `validation_error`」 | 那两条 labels 路由以前是**恒 501**，本片换成了真实现 ⇒ canary 必须换到另一个仍缺的端点上，否则它会开始失败。**这不是放宽断言**：canary 的语义（「未实现端点恒 501」）原样保留 |
| **D19** | `PropertyResponse.archived_at: Option<String>` **不带** `skip_serializing_if` | 与上游 `ArchivedAt *string` 无 `omitempty` 一致（未归档 = `null` 出现在 JSON 里）。同表的其他字段也逐字对齐上游 `propertyToResponse`/`propertyListRowToResponse` |
| **D20** | 未知/跨 workspace 的定义 id 在**删值**时 → 404，但**已归档**的定义允许删值 | 上游 `DeleteIssueProperty` 先 `GetIssueProperty` 判 404，然后**不**判归档（注释：cleanup must never be blocked） |

## 5. 测试与门禁

### 5.1 布局

| 层 | 文件 | 例数 |
| --- | --- | ---: |
| 纯单测（无 DB） | `mc-repos/src/label.rs` | 4 |
| 纯单测（无 DB） | `mc-repos/src/property/tests.rs` | 12 |
| 纯单测（无 DB，dto 三态守卫） | `mc-http/src/routes/issues/dto.rs` | 1 |
| 真库（repo 层，`#[ignore]`） | `mc-repos/src/label.rs` | 2 |
| 真库（repo 层，`#[ignore]`） | `mc-repos/src/property/db_tests.rs` | 4 |
| e2e（真库 + 真 router，`#[ignore]`） | `mc-http/tests/label_property.rs` | 2 |

**4 例 repo 真库用例**：`db_label_crud_round_trip_and_unique_conflict`、
`db_attach_detach_is_idempotent_and_workspace_scoped`、
`db_property_create_list_update_archive`、`db_active_cap_is_20_and_holds_under_concurrency`、
`db_removing_in_use_option_conflicts`、`db_value_face_bridging`
（后 4 例在 `db_tests.rs`）。

**2 例 e2e**（真库 + 真 router，含认证/成员门/跨 workspace）：

1. `label_catalog_lifecycle_and_issue_assignment`：目录 CRUD（201/200/204）→ 重名 409 →
   `?resource_type` 非法 400 → 颜色非法 400 → attach 幂等（第二次无 `issue_revision`）→
   `usage_count` 计数 → detach → 跨 workspace 404 → **空 `label_id` 400**；
2. `property_definition_and_value_bridging`：定义 CRUD（201/200）→ `type` 不可变 →
   重名 409 → 删引用中选项 409 → 值面 10 条判定（404 未知定义 / 400 已归档 / 400 类型不符 /
   400 `value is required` / 400 `null` / select 必须命中 options / actor 引用解析 /
   16KB / 删除值带 `issue_revision` / 非 UUID 400）。

**两条已知的测试侧坑**（生产路径正确，`mc-repos` 真库用例已修）：

- 值袋的键必须是**定义 UUID**（`def.id().to_string()`），不是选项 id —— `usage_count` 用
  `jsonb_exists(properties, p.id::text)` 普查，键错会让「删除引用中选项」的 409 判定失灵；
- 「活跃上限 20 且并发安全」的用例必须**只留一个空位**（归档 1 条，不是归档全部），
  否则两个并发 create 都合法 ⇒ 用例恒绿而失去意义。

### 5.2 门禁证据（本片 HEAD）

```
$ MULTICA_TEST_DATABASE_URL="$(cat ~/.mc_lum1370_dburl)" bash scripts/gates.sh --with-db
① fmt 2s PASS  ② build 39s PASS  ③ clippy 18s PASS  ④ clippy-test-util 18s PASS
⑤ test 33s PASS  ⑥ db 86s PASS (migrate=0,e2e=0)  ⑧ schema-drift 24s PASS
⑦ route-parity 0s PASS  ⑨ conformance 36s PASS  ⑩ file-size 0s PASS
overall: PASS — 10/10 gate(s) green in 78s   （②③④ 是热缓存那一轮的读数）
```

- ⑥ 是**真库**：migrate 566 条迁移 + 全部 `#[ignore]` e2e；本片直接跑的读数 ——
  `mc-repos --lib -- --ignored` **153 passed**、`mc-http --test issues -- --ignored`
  **14 passed**、`mc-http --test label_property -- --ignored` **2 passed**；不带 DB 门 ⑤
  `mc-repos --lib` **100 passed / 153 ignored**。
- ⑧ **需要角色有 `CREATEDB`**（`schema-drift` 会建临时库 `schema_probe_*`）：本片第一次跑
  是 `exit 2 permission denied to create database`，`ALTER ROLE mc_lum1370 CREATEDB` 后 24s 全绿。
  **下一个人 provision 测试角色时请一并带上 `CREATEDB`**，否则 ⑧ 会假红。
- ⑦：见 §1 的增量块（`local 348 / implemented 275 / known_gap 181 / owners.M2-E 0`）。
- ⑩：改动文件最大 753 行（`tests/label_property.rs`）< 800；拆分明细见 D17。
- ③/④ 的 `-D warnings` 全过（`#[allow]` 只用在：`validate_value` 的 `too_many_lines`、
  两例 e2e 的 `too_many_lines`、日期字面量的 `unreadable_literal`）。

### 5.3 本地跑法（DB 夹具）

```
# 一次性：角色 mc_lum1370（CREATE + CREATEDB）/ 库 multica_lum1370（本机 PG16:5432）
MULTICA_DATABASE_URL="$(cat ~/.mc_lum1370_dburl)" cargo run -q -p mc-migrate -- run --dir migrations
MULTICA_TEST_DATABASE_URL="$(cat ~/.mc_lum1370_dburl)" \
  cargo test -p mc-http --features test-util --test label_property -- --ignored --test-threads=1
```

未设 `MULTICA_TEST_DATABASE_URL` ⇒ 每例打印 skip 并 `return`（不算绿）。

## 6. 对外可观测差异（错误体 / 状态码 / 不可实现项）

本节是 §4 的**对外契约**子集（代码里以 `docs/59` §6 引用）。

### 6.1 状态码：与上游逐条一致

14 条路由的成功码（201 / 204 / 200）与全部错误码（400 / 403 / 404 / 409）都与上游一致。
**没有**使用 `Error::Unprocessable`（422）——上游这里全是 400。

### 6.2 响应体文案：三处系统性差异

1. **400**：本仓 `Error::Validation` 渲染为 `{"error":"validation error: {上游原文}"}`
   ⇒ 每条 400 都比上游多一个 `validation error: ` 前缀（内层文案逐字一致）；
2. **404**：`{"error":"not found: label"}` / `not found: property`（上游 `label not found` /
   `property not found`）；
3. **路径参数**：`{"error":"validation error: property id must be a uuid"}`
   （上游 `invalid property id`）。

以上三条是**全仓既有**约定（M2-A 起所有切片同款），不是本片引入的；列出来是为了让验收
脚本按「状态码 + 内层文案」匹配，不要按整串匹配。

### 6.3 能力缺口（**本片做不到**，如实登记）

| 缺口 | 上游行为 | 本仓现状 |
| --- | --- | --- |
| agent 主体管理 property 定义 | `403 agents cannot manage property definitions` | 请求身份只有 `X-Multica-User-Id`（无 agent 主体）⇒ 无法区分，D6 |
| agent/skill↔label 挂摘（`/api/agents/{id}/labels`、`/api/skills/{id}/labels` 等 6 条） | `label.go:576-708` | **不在本片范围**（门 ⑦ 里挂在 M6/M9 owner 下）；`resource_type` 机制（`issue|agent|skill`）与 `usage_count` 三表口径**已经就绪**，落地时只需再接 3 组 handler |
| 值写与定义读取的单事务原子性 | advisory lock + 单事务 | 两次独立往返（D7），窄竞态窗口已登记 |

## 7. 交接

1. **`docs/10` §5.1 / §5.2、`docs/11` §6 的「表不存在 / 留 501」已更正**（本片同批提交）——
   后续切片不要再引用那三条旧结论。
2. **`LUM-1580`（门 ⑦ 缺陷，仍开）**：`implemented_placeholder == 2` 的读数是**假的** ——
   parity 脚本只用 `\bplaceholder\b` 正则识别占位实现，而本仓的桩写的是 `not_implemented`，
   于是被算进 `implemented_real`。本片闭环的 3 条 labels 路由正是这类桩，等于「桩变真实现」
   但读数上 `implemented_real` 的增量和 `placeholder` 的减量**都不体现**。已按 `docs/44` §R3
   登记，不在本片修复（改脚本属 `LUM-1580`）。
3. **M6-0（`LUM-1665`）合并顺序**：anchor 先、本片后。本片对 `mount.rs` / `routes/mod.rs` /
   `mc-repos/src/lib.rs` 只有**新增行**（`git diff` 无 `−` 行，插入点见 §0 注），
   `docs/fixtures/route-parity-baseline.json` 与 `slash-alias-allowlist.tsv` 一行未改
   （`baseline_routes` 仍是 329，anchor 会整份重写它）。
4. **M6 的 skill 面可以直接复用**：`LabelRepo`（`resource_type = "skill"`）与 `PropertyRepo`
   的价值在 M6-2（skill 读写）会再遇到；`/api/skills/{id}/labels` 那 3 条只需在
   `issues/mod.rs` 之外新开一个 slice router。
5. **值面合事务**若将来要做：`IssueRepo::set_property` 需要一个「在给定事务里执行」的变体
   （现在它是池直连），`PropertyRepo::definition_for_value` 同样。两处都在 `mc-repos`，
   改动面跨 M2/M3 写集 ⇒ 建议单开一片并带上真库竞态用例。
