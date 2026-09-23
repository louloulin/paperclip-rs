# 46 · M5-1 自动驾驶读面（LUM-1564）

波次 B 第一片：把上游 autopilot 的**读面**四条路由搬进 `mc-http`，连同它读的那些行、
那几个纯函数（cron 网格 / 配额折算）和权限判定。写面（创建 / 编辑 / 触发 / 订阅 / 投递）
与 webhook 入口分别属 M5-2 / M5-3 / M5-4 / M5-5。

上游基准：`louloulin/multica` @ `f41fae6`（与 `scripts/route_parity.py` 内嵌的同一 commit）。

## 0. 交付物

| 文件 | 行数 | 内容 |
| --- | --- | --- |
| `crates/mc-http/src/routes/autopilots/list.rs` | 327 | 4 个 handler + `router()` + cron-preview 常量 |
| `crates/mc-http/src/routes/autopilots/dto.rs` | 283 | 上游 `autopilotToResponse` / `triggerToResponse` / 订阅者 / 协作者投影 + 信封 |
| `crates/mc-http/src/routes/autopilots/access.rs` | 190 | 写权限判定（所有权 ∨ 协作者）、成员校验、工作区内加载 |
| `crates/mc-autopilot/src/cron.rs` + `cron/tests.rs` | 578 + 312 | 5 段 cron 解析 + 时区 + 下一次网格（**手写，不引 crate**） |
| `crates/mc-autopilot/src/quota.rs` | 414 | 配额策略缝（`QuotaPolicyProvider`）+ 周期折算 |
| `crates/mc-autopilot/src/dto.rs` | 291 | 响应结构体（list/detail/trigger/订阅者/协作者/用量） |
| `crates/mc-autopilot/src/notification.rs` | 201 | 通知收件人投影 |
| `crates/mc-repos/src/autopilot/mod.rs`（读侧） | — | `list` / `get_in_workspace` / `list_triggers` / `list_subscribers(_for_autopilots)` / `list_collaborators` / `is_collaborator` / `member_role` |
| `crates/mc-repos/src/autopilot/quota.rs` | — | `ensure_period` / `get_period` / `reserve` / `consume` / `release` / `list_recoverable` / 计数累加 |
| `crates/mc-http/tests/autopilots/**` | 1326 | 真库 e2e 15 例 |
| `crates/mc-repos/src/autopilot/tests/**` | 276 | 配额真库 3 例 |

`routes/autopilots/mod.rs`（M5-0 的聚合壳）**未改**：四条路由经 `mount_slice_autopilot()`
合入，故无需新挂载点，`routes/mod.rs`、`routes/mount.rs` 全未触碰。

依赖边：`mc-http → mc-autopilot` 复用 M5-6 已加的那一条（`crates/mc-http/Cargo.toml:61`），
`Cargo.lock` 无需变更。合并 base 时这里曾出现**重复键**（两次独立新增同一条边 ⇒
`duplicate key` 让 ①-⑤/⑨ 全红），已收敛为一条 —— 后续切片若也要加这条边，
先 `grep mc-autopilot crates/mc-http/Cargo.toml`。

## 1. 路由与线上契约

| 上游 handler | 本地 handler | 注册键 |
| --- | --- | --- |
| `ListAutopilots`（`handler/autopilot.go:439-525`） | `list::list_autopilots` | `GET /api/autopilots` + `GET /api/autopilots/` |
| `GetAutopilot`（:523-596） | `list::get_autopilot` | `GET /api/autopilots/{id}` + `GET /api/autopilots/{id}/` |
| `CronPreview`（`handler/autopilot_cron_preview.go:1-55`） | `list::cron_preview` | `GET /api/autopilots/cron-preview` |
| `GetAutopilotQuotaUsage`（:2435-2469） | `list::get_quota_usage` | `GET /api/autopilots/usage` |

6 个注册键 / 4 条上游路径：`chi` 的 `Mount` + 子路由 `/` 同时服务带与不带尾斜杠两种请求，
`axum` 必须各注册一次，否则 `GET /api/autopilots/` 落到 router 默认 404。
`cron-preview` / `usage` 上游只有单形态（静态段优先于 `{id}` 匹配，注意注册顺序无关 —— axum 0.7
按静态优先匹配）。

### 1.1 `GET /api/autopilots`

```json
{"autopilots": [{ "id": "…", "status": "active", "trigger_kinds": ["webhook","schedule"],
                  "next_run_at": "2026-09-23T01:00:00+00:00", "last_run_status": "completed",
                  "subscribers": [{"user_type":"member","user_id":"…","created_at":"…"}] }],
 "total": 3}
```

- `status` 查询参数**没有 allowlist**：缺省 / 空串 ⇒ 排除 `archived`；其余按字面匹配
  （`?status=nonexistent` ⇒ `200` + 空数组，不是 400）。上游 `sqlc.narg('status')` 就是这个语义。
- `trigger_kinds` = **enabled** 触发的 `kind` 去重升序；`next_run_at` = enabled `schedule`
  触发的最小 `next_run_at`；`last_run_status` = 最近一次 run 的状态，没有 run 时上游写 `""`
  ⇒ 本地 `last_run_status_or_none()` 把它变 `None`（省掉 `""`）。
- **协作者集合不带工作区过滤**：上游 `ListAutopilotIDsForCollaborator(caller.UserID)` 是全局查，
  本地照抄（`list_autopilot_ids_for_collaborator`）。
- 订阅者**批量**查（一次 `ANY($1::uuid[])`），失败 ⇒ `500`（不 fail-open）；
  返回 `{user_type,user_id,created_at}`，空集合序列化成 `[]`（不是 `null`）。
- `total` = **本次返回条数**（上游无分页，也没有 `COUNT(*)` 第二查）。后续若要分页，
  这个字段的语义需与前端重新对齐 —— 见 §5.6。
- `can_manage_access` 在列表上**不出现**（只有详情面才知道调用者能不能管权限）。

### 1.2 `GET /api/autopilots/{id}`

```json
{"autopilot": {"…": "…", "can_write": true, "can_manage_access": false},
 "triggers": [{"kind":"webhook","webhook_token":"…","webhook_path":"/api/webhooks/autopilots/tok",
               "webhook_url":"https://…/api/webhooks/autopilots/tok","provider":"generic",
               "has_signing_secret":true,"signing_secret_hint":"…","event_filters":{…}}],
 "collaborators": [{"user_type":"member","user_id":"…","granted_by":"…","created_at":"…"}]}
```

- `can_write` / `can_manage_access` **恒出现**；`can_write=false` ⇒ webhook 三件套
  （`webhook_token` / `webhook_path` / `webhook_url`）置 `null`，但 `has_signing_secret` 与
  `signing_secret_hint` 保留（上游 `redactWebhookSecrets` 只清这三项，hint 不是密文）。
- `triggers` / `collaborators` 查询失败 ⇒ `[]`（**fail-open**，上游写 `nil`）；
  `triggers` 与 `collaborators` 之外的失败（autopilot 本体、订阅者）⇒ 500 / 404。
- `next_run_at` 序列化：`null` 或 RFC3339；`webhook_url` 无 `omitempty` ⇒ 为 `null` 也在体里。
- `event_filters` 为空时**整键省略**（`skip_serializing_if`），与上游 `omitempty` 一致。

### 1.3 `GET /api/autopilots/cron-preview`

```json
{"next_runs": ["2026-09-23T01:00:00Z", "2026-09-24T01:00:00Z", "2026-09-25T01:00:00Z"]}
```

- 固定 3 次（`previewCount = 3`），**严格递增**，`time.RFC3339`（秒精度、`Z`）。
- 校验顺序（逐字对齐上游，因此错误码可预测）：
  1. `expr` 为空 ⇒ `400 {"error":"expr is required","code":"invalid_cron"}`；
  2. `tz` 非法 ⇒ `400 {"code":"invalid_timezone"}`；
  3. 解析 / 求值失败 ⇒ `400 {"code":"invalid_cron"}`。
  故 `expr=&tz=bad` 报 `invalid_cron`（先看 expr），`expr=bad&tz=bad` 报 `invalid_timezone`。
- **语法合法但永不触发**的表达式（如 `0 0 31 2 *`）⇒ `200 {"next_runs":[]}`，不是错误。
- 错误体是**扁平**的 `{"error","code"}`（上游 `writeCronPreviewError`），不是全仓 `ApiError`
  的 `{"error":{"code","message"}}` —— 客户端按 `code` 分支，故必须逐字保留。
- 时区缺省 `UTC`；`Asia/Shanghai` 的 `0 9 * * *` ⇒ UTC `01:00`（有测试钉住）。

### 1.4 `GET /api/autopilots/usage`

```json
{"action":"off","limit":null,"used":null,"reserved":null,"total":null,
 "period_start":null,"period_end":null,"reset_at":null,"reached":null,
 "policy_revision":null,"subscription_version":null,"blocked_counts":null}
```

- `action` 三态 `off` / `observe` / `enforce`：
  - `off`（默认，本仓没有 entitlement 面）⇒ **除 `action` 外全部 `null`**，`blocked_counts` 也是 `null`；
  - `observe` ⇒ `used + reserved`、`limit` 都给出，`reached` 仍为 `null`，`blocked_counts` ≥ `{}`；
  - `enforce` ⇒ `reached: true/false`。
- 周期行缺失 ⇒ `0/0` + `reached: false`（不是 404，也不是 500）；行里的 `blocked_counts`
  JSON 解不开 ⇒ 500（上游同）。
- 策略从哪来：`mc_autopilot::quota::QuotaPolicyProvider`（`policy(&self, workspace_id) -> Option<QuotaPolicy>`），
  默认实现 `NoEntitlementPlane` 恒返回 `None` ⇒ 读面就是 `off`。M9 用
  `install_policy_provider(...)`（`OnceLock`，**首次调用生效**）接上 Cloud 的 entitlement 面。
- `usage_from_period(&QuotaPolicy, Option<&QuotaPeriodRow>)` 是**纯函数**，三态形状由单测钉住；
  它不碰数据库，`QuotaRepo::usage_period`（`mc-repos`）只负责取行。

## 2. 权限模型（四条路由共用）

| 情形 | 结果 |
| --- | --- |
| 缺 `X-Multica-User-Id` | `401` |
| 缺 `X-Workspace-Id` | `400` |
| 非工作区成员 / id 属别的租户 | `404`（不是 403 —— 上游 `requireWorkspaceMember` 就是 404，不泄露存在性） |
| 成员 + 非 UUID 路径 id | `400` |
| 非成员 + 非 UUID 路径 id | `404`（成员校验在 UUID 解析**之前**） |
| 成员但无写权（详情面） | `200` + `can_write:false`（读面本身不拒） |

写判定（上游 `authenticateAutopilotWriteByOwnership`）：

```text
role ∈ {owner, admin}  ||  (created_by_type == "member" && created_by_id == caller)
```

详情面的 `can_write` 再加协作者：`write_by_ownership(…) || is_collaborator(ap, caller)`
（协作者查询报错 ⇒ `false`，**fail-closed**，与上游 `memberCanWriteAutopilot` 的 error 处理一致）。
`access::owns_for_write(created_by_type, created_by_id, role, user_id)` 是最底层谓词，
列表面（只有 `AutopilotListRow`）与详情面（完整行）都走它。

`load_in_workspace` 把「查不到 / 不在本工作区」都折成 `NotFound`（上游 `loadAutopilotInWorkspace`
把所有错误都映 404）；本地 DB 故障仍 `500`（见 §5.3）。

## 3. 上游映射

| 上游 | 本地 |
| --- | --- |
| `AutopilotResponse` / `autopilotToResponse`（29-120 / 185） | `mc_autopilot::dto::AutopilotResponse` + `routes::autopilots::dto::autopilot_to_response`（`assignee_type == ""` ⇒ `"agent"`） |
| `triggerToResponse`（autopilot.go 触发段） | `dto::trigger_to_response`，`webhook_url` 由 `mc_autopilot::dto::public_url()` 拼（无 `AppState` 里的 `cfg.PublicUrl`，取值走环境） |
| `collaboratorToEntry` / 订阅者投影 | `dto::collaborator_entry` / `dto::subscriber_entry` |
| `signingSecretHint`（162-360） | `mc_autopilot::dto::signing_secret_hint` |
| `redactWebhookSecrets` | `dto::redact_webhook_secrets` |
| `NextOccurrenceAfterUTC` / `NextOccurrencesAfterUTC` / `NextOccurrencesUTC`（`service/cron.go:26/43/61`） | `mc_autopilot::cron::{next_occurrence_after_utc, next_occurrences_after_utc, next_occurrences_between_utc}` |
| `service/autopilot_quota.go`（19/26/33/…） | `mc_autopilot::quota` + `mc-repos/src/autopilot/quota.rs` |
| `service/autopilot_notification_recipient.go`（91） | `mc_autopilot::notification` |

`cron` 是**手写** 5 段解析（`FIELD_COUNT = 5`，含 `*` `a-b` `a,b` `*/n` `a-b/n`、`sun=0/7`、
月份/星期名），搜窗口 `SEARCH_HORIZON_YEARS = 5`，两次触发间隔上限 `MAX_BETWEEN_OCCURRENCES = 1024`
（超限即认为「永不触发」⇒ `[]`）。不引 `cron` crate 的理由：上游用 `robfig/cron` 的
`SpecSchedule.Next` 语义（含 `dom/dow` 的 OR 规则与 Go 零值起算），外部 crate 的边界行为
（尤其「无匹配时的返回值」）对不齐，而这是**契约面**（编辑器靠它区分「表达式错」与「永不触发」）。

`floor_plan` / 网格起点必须是 **Go 零值**（`time.Time{}`）而不是 Unix 纪元：
`robfig/cron` 的 next 搜索从零值开始，用纪元起算会整体偏移 7200s（时区/闰秒口径），
`cron/tests.rs` 有对应用例。

## 4. 数据面

读侧 SQL 全部 `mc-repos/src/autopilot/mod.rs`：

- `list(ws, status)`：一查取全部行 + 三个相关子查询（`trigger_kinds` / `next_run_at` /
  `last_run_status`），`ORDER BY created_at DESC`。
- `list_subscribers_for_autopilots(&[Uuid])`：一次 `ANY($1::uuid[])`，`ORDER BY autopilot_id`，
  调用方按 `autopilot_id` 分组。**join `member` 过滤离职成员**（`m.workspace_id = a.workspace_id
  AND m.user_id = s.user_id`），与上游 `ListAutopilotSubscribers` 一致。
- `member_role(ws, user)`：`SELECT role FROM member`；无行 ⇒ `None` ⇒ 404。
- 配额表 `autopilot_quota_period` / `autopilot_quota_reservation` 来自 M5-0 的
  `migrations/upstream/352_autopilot_quota_execution.up.sql`（含 `compat/` 覆盖）。

## 5. 偏离与修正

### 5.1 acting user（有意简化）

上游 `autopilotActingUserID` / `resolveActor`（`handler.go:847`）会区分 **task token** 与
**用户 token**：守护进程/任务上下文里的「行为主体」可能是发起任务的用户而不是调用者。
本地读面一律取认证用户（`AuthUser::id()`，dev 模式读 `X-Multica-User-Id`）——task token 面
是 M3-7 守护进程的领地，等它落地后 `access::acting_user_id` 是唯一需要改的接缝。

### 5.2 非成员 404

DoD 的描述文字写「非成员 403」，实现按上游 `requireWorkspaceMember` 给 **404**
（不泄露资源存在性）。已由 `auth::non_member_gets_404_not_403` 钉住；若产品侧要求 403，
改动点是 `access::require_member`。

### 5.3 错误体文案

全仓 `Error::NotFound` 渲染成 `not found: autopilot`，上游是 `autopilot not found`。
契约是 **HTTP status + `code`**（`not_found` / `forbidden` / `unauthorized` / `invalid_request`
/ `database_error`），文案不逐字对齐；同理 500 的 `"failed to list autopilots"` 变成
`database_error`（`repo_err`）。`loadAutopilotInWorkspace` 的「一切皆 404」也被收窄：
DB 故障是 500，只有「查不到」是 404（否则真库故障会伪装成 404）。

### 5.4 时间序列化

- 行里的 `created_at` / `updated_at` / `next_run_at` 走 `mc_core::Timestamp::as_iso()`
  ⇒ `+00:00` 后缀（同一时刻，格式不同）。
- `cron-preview` 的 `next_runs` 特意用 `to_rfc3339_opts(SecondsFormat::Secs, true)`
  ⇒ 与 Go `time.RFC3339` 逐字节一致的 `…Z`。理由：这个字段的前端消费方是表达式编辑器，
  它展示的是「下一次触发时刻」的字符串，最不该有格式歧义。

### 5.5 真库测试抓到的两处 bug（已修）

新写的读面 e2e 一跑就红，暴露的是 **M5-0 落下的 JOIN 歧义列**（同一类，两处）：

1. `crates/mc-repos/src/autopilot/mod.rs`：`AUTOPILOT_SUBSCRIBER_COLUMNS` 不带限定，
   而两条订阅者查询都 `JOIN member`（`member` 也有 `user_id`）⇒
   `column reference "user_id" is ambiguous`，列表与详情**双双 500**。修法是按上游
   `SELECT s.*` 的等价形式把列清单限定成 `s.…`（`FromRow` 按列名匹配，限定不影响映射）。
2. `crates/mc-repos/src/autopilot/quota.rs`：`list_recoverable` 展开列清单后 `JOIN autopilot_run`
   （两边都有 `id` / `created_at`）⇒ `column reference "id" is ambiguous`。上游本就是
   `SELECT r.*`，改回 `r.*`（同时避免再写一份重复列清单）。

两条都补了回归测试（`mc-repos/src/autopilot/tests/quota.rs` + `mc-http/tests/autopilots/read.rs`）。
教训：**列清单常量在 JOIN 上下文里必须限定**；只有单表查询才能用裸列名。

### 5.6 其它有意为之

- 列表 `total` = 返回条数（不是 `COUNT(*)`）；分页属后续切片。
- `provider` 缺省 `"generic"`；`assignee_type == ""` ⇒ `"agent"`。
- 详情 `triggers` 里 `event_filters` JSONB 解不开时**丢掉该字段**而不是 500（上游同）。
- `can_write` 在全仓是 `Option<bool>`（调用者未知时才省略）；读面恒有成员调用者，
  故线上总是 `true`/`false`，`None` 只在 DTO 单测里出现。
- `#[allow(clippy::too_many_arguments)]`：`reserve` 与上游 sqlc params struct 字段一一对应，
  与仓内既有惯例一致（`mc-repos/src/daemon/*`、`mc-runtime/src/adapters/*`）。

## 6. 测试

| 目标 | 例数 | 跑法 |
| --- | --- | --- |
| `mc-autopilot` 单测 | 27 | `cargo test -p mc-autopilot --locked` |
| `mc-http` 读面 e2e | 15 | `MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --test autopilots --features test-util -- --ignored` |
| `mc-repos` 配额真库 | 3 | `MULTICA_TEST_DATABASE_URL=… cargo test -p mc-repos --lib -- --ignored autopilot::` |

e2e 分五个文件（`tests/autopilots/{main,support,read,auth,cron,usage}.rs`）：每个
`tests/*.rs`（含 `main.rs` 形态的目录）是**独立二进制/独立进程**，既绕开单文件 800 行上限，
也让真库夹具互不干扰。

- `read.rs`（5）：列表排除 `archived` + 三列派生值；`can_write` 覆盖「角色 / 创建者 / 协作者」
  三条路径；详情给写者带凭据、给非写者脱敏；空派生列省略但 `subscribers` 仍为 `[]`。
- `auth.rs`（5）：四条路由 × 缺 user 头（401）/ 缺 workspace 头（400）；非成员 404；跨工作区 404；
  非 UUID 400。
- `cron.rs`（4）：3 条严格递增的 `Z` 串；时区换算与缺省 UTC；4 组错误码 + 扁平体；
  「永不触发」⇒ 空数组。
- `usage.rs`（1）：用一个**按工作区分片**的 `QuotaPolicyProvider`（`HashMap<Uuid,QuotaPolicy>`）
  把 off / observe / enforce / 无周期行四种形态塞进一个二进制，且不污染同进程其它用例。
- `tests/autopilots/support.rs`：真库夹具（`workspace` / `user` / `autopilot` 四张表 /
  触发 / 订阅者 / 协作者 / run / 配额周期）；`MULTICA_TEST_DATABASE_URL` 缺省静默跳过，
  **设了却连不上直接 panic**（库坏了必须红，跳过会把空跑伪装成绿）。

测试设计上的两条教训（值得后续片复用）：

1. `autopilot_trigger.webhook_token` 是**全局唯一**列 ⇒ 测试要每次生成新 token
   （`format!("tok_…{}", Uuid::new_v4().simple())`），否则第二次跑必撞 23505。
2. `list_recoverable` 是**全局**扫掠（上游不带 workspace 参数，由后台 sweeper 跑）
   ⇒ 断言必须按 `workspace_id` 收窄，不能断言「全局为空」。

## 7. 明确不做的部分

- `ListWorkspaceManagerNotificationRecipients`（`autopilot_notification_recipient.go` 的另一半）
  只有一个 M9/Cloud 调用方，本片不移植（`mc-autopilot/src/notification.rs` 只保留读面需要的投影）。
- `admit` 的**事务编排**（同事务建 `autopilot_run` + 回填 `quota_reservation_id`）属 M5-4；
  本片只提供仓储函数与纯决策函数。
- webhook 入口 `POST /api/webhooks/autopilots/{token}`（router.go:1487，无鉴权）属 M5-2。
- 唤醒 / 调度循环（`mc-daemon`）属 M5-6（已交付 `docs/47`）与 M5-7（`docs/48`）。
- ⑦ 基线（300）**本片不刷新**：按 R13，基线刷新由 M5-INT 收口时一次做（后刷者重扫）。

## 8. 门禁读数（`--with-db`）

日志：`gates-m5-1.log`（`bash scripts/gates.sh --with-db`，须带 `MULTICA_TEST_DATABASE_URL`）。

- **10/10 全绿**（176s）：① fmt ② build`--locked` ③ clippy`-D warnings` ④ clippy-test-util
  ⑤ test ⑥ db（migrate + 真库 e2e）⑧ schema-drift ⑦ route-parity+alias ⑨ conformance ⑩ file-size。
- ⑤ `1255 passed / 0 failed / 113 ignored`；⑥ 真库 `246 passed / 0 failed`
  （其中本片新增：mc-http `tests/autopilots` **15/15**、mc-repos `autopilot::tests::quota` **3/3**）。
- ⑧ schema-drift 绿 ⇒ 迁移与真库结构一致（配额两表在 `migrations/upstream/352_*.up.sql`）。
- ⑦ `upstream 456 (f41fae6) / local 307 registered / baseline 300`，`implemented 246 real + 2 placeholder`、
  `known_gap 208`、`unclaimed 0`、`regression 0`、`local_only 11`（本片 **+6 键**，基线未动）。
- 红过的两次都不是噪声：一次是合并 base 时的**重复依赖边**（§0），一次是新测试抓到的
  **JOIN 歧义列**（§5.5）—— 都已修复并补了回归测试。

## 9. 后续接缝

- `access::acting_user_id`：接 task token / 守护进程主体（M3-7）。
- `install_policy_provider`：接 Cloud entitlement 面（M9）。
- 列表分页与 `total` 语义（见 §5.6）。
- ⑦ 基线刷新（M5-INT）。
