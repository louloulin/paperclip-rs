# 51 · M5-3 自动驾驶 trigger 写面 + 凭据（LUM-1568）

波次 C 第三片：把上游 autopilot 的 **trigger 写面**（create / update / delete）与
**两条凭据路由**（轮换 webhook token / 设置签名密钥）搬进 `mc-http`，连同它们用到的
纯函数（时区 / provider 闭集 / `event_filters` 校验与编码 / `next_run_at` 折算）与
仓储写点（`autopilot_trigger` 的 INSERT / UPDATE / DELETE / token 轮换 / secret 写入）。

上游基准：`louloulin/multica` @ `f41fae6b`（与 `scripts/route_parity.py` 内嵌同一 commit），
上游 handler 全文在 `server/internal/handler/autopilot.go`。

## 0. 交付物

| 文件 | 行数（本片增量） | 内容 |
| --- | --- | --- |
| `crates/mc-http/src/routes/autopilots/trigger.rs` | 627（+597/−3） | 3 号 handler（#10/#11/#12）+ `router()` + 请求私有 DTO + `decode_body` / `resolve_write_scope` / `load_bound_trigger_row`（后三者 `pub(super)`，被凭据面复用） |
| `crates/mc-http/src/routes/autopilots/credentials.rs` | 269（+247/−3） | 2 号 handler（#13/#14）+ `router()` + 唯一日志点 `log_credential_write` |
| `crates/mc-repos/src/autopilot/trigger.rs` | 385（+377/−2） | 写点：`create_trigger` / `update_trigger` / `delete_trigger` / `AutopilotTriggerRepo::set_signing_secret` / `rotate_webhook_token` / `generate_webhook_token` + `NewTrigger` / `TriggerPatch` |
| `crates/mc-autopilot/src/trigger.rs` | 261（+254/−12） | 纯函数：`Timezone`（IANA 校验）、`is_allowed_webhook_provider`、`validate_webhook_event_filters`、`encode_webhook_event_filters(_always)`、`next_run_at_for`、`TRIGGER_KIND_*` / `DEFAULT_TIMEZONE` |
| `crates/mc-autopilot/src/trigger/tests.rs` | 204（+204） | 上表纯函数的单测（边界：空时区 / 未知 IANA / `event_filters` 三态 / next-run 直算） |
| `crates/mc-autopilot/src/credential.rs` | 150（+141/−0） | `normalize_signing_secret`（三态 + 16 字节下限）、`redact_log_line`（→ `mc_telemetry::redact_log`）、`contains_credential` |
| `crates/mc-http/tests/autopilots/triggers.rs` | 397 | 路由形态 / 鉴权分层 / 共享工具宿主（`pub(crate)`） |
| `crates/mc-http/tests/autopilots/trigger_crud.rs` | 510 | 数据面：校验顺序、三态、`next_run_at` 直赋值保护、token 铸造 |
| `crates/mc-http/tests/autopilots/credentials.rs` | 175 | 凭据面：轮换只对 webhook、secret 只写不回显 |
| `crates/mc-http/tests/autopilots/main.rs` | +7 | 三行 `mod`（拆文件见 §6.1） |

合计 10 文件 **+2909 / −20**。写面 e2e 15 例（全部 `--ignored`，需真 PG）+ 纯函数单测随各自文件。

**未触碰**（C 波共享热点与 scaffold 约定）：`routes/autopilots/mod.rs`（M5-0 聚合壳，
`mount_slice_autopilot()` 已合入本片的 `router()`）、`routes/mount.rs`、`routes/mod.rs`、
任何 `lib.rs`、`Cargo.toml` / `Cargo.lock`（**无新依赖**）、
`crates/mc-http/tests/autopilots/support.rs`（M5-1 所有）、
`crates/mc-repos/src/autopilot/tests/mod.rs`（M5-1 的注册表，只声明 `mod quota;`）。

## 1. 路由与注册键

| # | 上游 handler（`handler/autopilot.go`） | 本地 handler | axum 注册键 |
| ---: | --- | --- | --- |
| 10 | `CreateAutopilotTrigger`（1514） | `trigger::create_autopilot_trigger` | `POST /api/autopilots/:id/triggers` |
| 11 | `UpdateAutopilotTrigger`（1876） | `trigger::update_autopilot_trigger` | `PATCH …/:triggerId` + `…/:triggerId/` |
| 12 | `DeleteAutopilotTrigger`（2055） | `trigger::delete_autopilot_trigger` | `DELETE …/:triggerId` + `…/:triggerId/` |
| 13 | `RotateAutopilotTriggerWebhookToken`（2140） | `credentials::rotate_webhook_token` | `POST …/:triggerId/rotate-webhook-token` |
| 14 | `SetAutopilotTriggerSigningSecret`（2231） | `credentials::set_signing_secret` | `PUT …/:triggerId/signing-secret` |

5 条上游路径 → **7 个注册键**：#11/#12 上游是 `Route(":triggerId") + Patch("/") / Delete("/")`
（`chi.Mount` 的根 `/` 同时服务两种请求），axum 0.7 必须各注册一次；
#10/#13/#14 上游是单形态 plain 子路由，**多注册一个尾斜杠就是 `EXTRA_ALIAS`**
（`scripts/slash_alias_audit.py` 会红）——`triggers::single_form_routes_reject_the_trailing_slash_alias`
用 `POST …/triggers/` → 404、`POST …/rotate-webhook-token/` → 404 钉住这一点。

静态段与参数段共处一层没有冲突：`…/:triggerId`、`…/:triggerId/rotate-webhook-token`、
`…/:triggerId/signing-secret` 同时注册能被 matchit 接受（单测
`trigger_routes_coexist_with_the_sibling_subrouters` 走的是**合并后的** `autopilots::router()`）。

状态码：create `201`（上游 `writeJSON(w, http.StatusCreated, resp)`；
`docs/44` §3.2 表里的 “202” 是 **span 行数**，不是状态码）· update `200` · delete `204` ·
rotate `200` · set-signing-secret `200`。

## 2. 逐路由契约

### 2.1 `POST /api/autopilots/:id/triggers` → `201`

请求体（私有结构体 `CreateAutopilotTriggerRequest`，**不放进 `autopilots/dto.rs`**）：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `kind` | `String`（`#[serde(default)]`） | 必填；`schedule` / `webhook` 之外的值先判 400 |
| `label` | `Option<String>` | 空 ⇒ 落 `NULL` |
| `cron_expression` / `timezone` | `Option<String>` | 只对 `schedule` 合法 |
| `event_filters` | `Vec<WebhookEventFilter>`（`null` ⇒ 空） | 只对 `webhook` 合法 |
| `provider` | `Option<String>` | 只对 `webhook` 合法；闭集 `{generic, github}` |
| `enabled` | `Option<bool>` + `#[allow(dead_code)]` | **上游 create 忽略该值**（新行恒 `true`），保留字段是为了让类型错的 `enabled` 也跟 Go 的 `json.Decode` 一样 400 |

校验顺序逐字对照上游（同一份坏输入只能给一个文案）：
`invalid request body`（400，`decode_body`）→ `kind is required` →
`kind must be schedule or webhook` → 分叉后的专有字段检查
（schedule：`cron_expression is required for schedule triggers` → cron 解析 →
`timezone`；webhook：`timezone is not valid for webhook triggers` /
`event_filters is only valid for webhook triggers` / `provider is only valid for webhook triggers` /
`provider must be generic or github`）。

- `next_run_at` = `mc_autopilot::trigger::next_run_at_for(cron, tz, now)`；永不触发的表达式落
  `NULL`（本地偏差，见 §5.3）。
- **timezone 落 `''` 而不是 `NULL`**：上游 `ptrToText(req.Timezone)` 把 `nil` 绑成 `""`，
  响应里的 `timezone` 因此是 `""`（webhook 行才是 `null`）。`Timezone::from_column(Some(""))`
  仍解成 UTC，所以 next-run 数学不变 —— 这是响应形状保真，不是语义改动。
- webhook 分支走 `create_webhook_trigger_with_minted_token` 的语义：
  `generate_webhook_token()` = `"awt_" + 32 随机字节的 URL-safe 无填充 base64`（**47 字符**），
  唯一冲突重试 `WEBHOOK_TOKEN_ATTEMPTS = 3` 次，每次一个事务；耗尽 ⇒ 500。
  路径形态由 `mc_autopilot::dto::webhook_path_for_token` 定 = `/api/webhooks/autopilots/{token}`，
  **与 M5-5 的 ingress 入口逐字一致**（M5-5 必须复用同一个函数，不要另写一份）。

### 2.2 `PATCH /api/autopilots/:id/triggers/:triggerId[/]` → `200`

- **`event_filters` 三态**：缺键 / 显式 `null` ⇒ 不动；`[]` ⇒ 清空（库里落 `[]`，响应省略字段 =
  接受全部事件）；`[…]` ⇒ 替换。`Option<Vec<_>>` 的 `None`/`Some(empty)` 必须分开 —— 靠
  `null_as_default` 反序列化器把 `null` 折成 `None`。
- **`next_run_at` 是 SQL 直赋值列**（`= $n`，不是 `COALESCE`），所以 handler 先播种
  `prev.next_run_at`，只在「schedule 且 cron 非空」时重算 —— 否则「只改 label」会把这一列抹成
  `NULL`。`trigger_crud::update_keeps_untouched_columns_and_honours_the_tri_state` 钉住这条。
- 跨 kind 字段一律 400（`cron_expression is only valid for schedule triggers` /
  `timezone is only valid for schedule triggers`）；`kind` 本身**不可改**。

### 2.3 `DELETE /api/autopilots/:id/triggers/:triggerId[/]` → `204`

顺序：工作区 → 成员 → autopilot → 写权限 → trigger 解析与绑定检查 → 事务删除。
**trigger 必须属于该 autopilot**（`trigger.AutopilotID != autopilotUUID` ⇒ 404 `trigger not found`，
与不存在不可区分）。

### 2.4 `POST …/:triggerId/rotate-webhook-token` → `200`

- 只对 `kind = webhook` 有效：否则 400 `trigger is not a webhook trigger`。
- **这是全片唯一回显明文 token 的点**（不回显就没人拿得到入口 URL）；读面（M5-1 的
  `trigger_to_response`）照旧只出 `webhook_path` / `has_signing_secret` / hint。
- 同样 3 次唯一冲突重试；耗尽 ⇒ 500 `failed to rotate webhook token`。

### 2.5 `PUT …/:triggerId/signing-secret` → `200`

`{"signing_secret": "…"}`：`""` / 全空白 ⇒ 清除（`NULL`，退回「只验 bearer token」）、
非空且 `< 16` **字节** ⇒ 400 `signing_secret must be at least 16 characters`、
`>= 16` ⇒ 落 trim 后的值（首尾空白不进 HMAC 计算）。长度口径是**字节**（Go 的 `len(string)`），
`credential::length_is_measured_in_bytes_like_go` 用 6 个汉字 = 18 字节钉住 —— 别顺手换成
`chars().count()`。响应只出 `has_signing_secret` + hint。

## 3. 凭据纪律（本片的 DoD）

- **响应面**：hint = 末 4 位（`signingSecretHint` 语义，M5-1 已落在 `dto.rs`，
  本片**不复制第二份**）。测试用 `credential::contains_credential(text, secret)` 断言
  「整份响应体里找不到明文」：空凭据恒 `false` —— `str::contains("")` 为真，
  朴素 `contains` 会把「本轮没有凭据」误判成泄露。
- **日志面**：`crates/mc-http` **没有** `mc-telemetry` 依赖边，而加这条边会动 `Cargo.lock`
  （闸门全带 `--locked`）。所以凭据路由的唯一日志点收口在
  `mc_autopilot::credential::redact_log_line`（内部就是 `mc_telemetry::redact_log`），
  HTTP 层只写 `tracing::info!(line = %redact_log_line(...), "…")`。
  字段名刻意避开 `token` / `secret` 字样：`Redactor::is_sensitive` 判的是
  **键名包含**，叫 `signing_configured` 才不会被抹成 `[REDACTED]` 而失去观测价值。
- ⚠️ `redact_str` 只认 `key=value` / `"key": "value"` / `key: value` 三种形状，
  **裸值不会被抹**（`redact_log_line` 的文档里有两个单测钉住这个已知限制）。
  因此「不要把凭据值放进日志」仍是调用方的责任，redaction 只是兜底。

## 4. 权限链

本地不引入 `RequireWorkspaceMember` 中间件，而在 handler 里显式串起来
（`trigger::resolve_write_scope`，M5-1 的 `access` 复用，不复制判定）：

| 判负 | 状态 | 上游文案 |
| --- | --- | --- |
| 缺 `X-Multica-User-Id` | 401 | `unauthorized` |
| 工作区头缺失 / 非 UUID | 400 | `invalid workspace id` |
| 非成员 | **404** | `not found: workspace`（掩盖存在性，与「工作区不存在」不可区分） |
| autopilot id 非 UUID | 400 | `autopilot id must be a valid uuid` |
| autopilot 不存在 | 404 | `autopilot not found` |
| 是成员但无写权限 | 403 | `insufficient permission for this autopilot` |
| trigger 非 UUID | 400 | `trigger id must be a valid uuid` |
| trigger 不属于本 autopilot / 不存在 | 404 | `trigger not found` |

`resolve_write_scope` 把「工作区解析 + 成员判定 + autopilot 加载」合并成一次调用：上游
对应的是「中间件 + `GetAutopilotInWorkspace`」两段，合并后仍保留可区分的判负
（例如 autopilot id 与 trigger id 同时非法时给的是 `autopilot id must be a valid uuid`）。

## 5. 与上游的偏差（逐条都可查）

1. **无 WebSocket 广播**（`squads.rs` 先例）：写面改变不推 `autopilot:*` 事件。
2. **403 文案带 `thiserror` 前缀**：线上正文是 `forbidden: insufficient permission for this autopilot`，
   上游是裸 `insufficient permission for this autopilot`（`docs/40` §5 的全仓偏差；
   测试用 `upstream_message` 剥前缀断言上游文案，另在个别用例里显式钉一次带前缀的原文）。
3. **`next_run_at` 永不触发时是 `NULL`**（上游落零值 `timestamptz`）。
4. **跳过 `autopilot_rule_version` 写入与 `SetAutopilotTriggerPublisher` 重打**：
   版本表写点属 M5-2（`autopilot/write.rs`），C 波并行 ⇒ 在 `update_trigger` /
   `delete_trigger` 调用处留了标记注释，未留死代码。后果：编辑触发器不会重打
   `published_by_id`，M5-4 的派发归因（`source=trigger_owner`）需据此复核。
5. **`parse_uuid` 文案**是 `"{field} must be a valid uuid"`（上游 `"invalid {field}"`）。
6. **`CronError::InvalidTimezone`** 少了被包裹的原因（上游带 `%w`）。
7. **rotate 的 `failed to generate webhook token` 分支不可达**（本地 mint 不会失败）。
8. **日志经 `redact_log_line` 间接一层**（§3），不是直接在 handler 里 `format!`。
9. **未落 `crates/mc-repos/src/autopilot/tests/trigger.rs` 真库模块**：注册表是 M5-1 的文件
   （不在本片写集），SQL 正确性由真库 e2e 覆盖（INSERT/UPDATE/DELETE 全部走 HTTP 面）。
10. **500 一律折成 `repo_err(e, "trigger")`**（`database_error` 码），不是上游逐字的
    `failed to create/update/delete trigger`；只有「token 铸造重试耗尽」这一条没有底层 DB 错误，
    才用 `Error::Internal` 带自述文案。
11. **`mc-errors` 前缀**（同 2）：`NotFound` → `not found: …`、`Validation` → `validation error: …`。
12. **schedule 行的 `timezone` 落 `''`**（§2.1），webhook 行落 `NULL` —— 与上游 `triggerToResponse`
    的 `""` / `null` 一致。

## 6. 测试与门禁

### 6.1 测试布局

`crates/mc-http/tests/autopilots/` 追加 3 个文件（`main.rs` 三行 `mod`）。**为什么不是 1 个文件**：
门 ⑩ 单文件 800 行硬上限，合并写是 1053 行；且 `support.rs` 是 M5-1 所有（不改），
共享工具（`WRITE_ROUTES` / `route_of` / `probe_status` / `upstream_message`）因此挂在
`triggers.rs` 里以 `pub(crate)` 暴露给同 target 的另两个文件。
`probe_status` 是本地请求构造器：`support::{call_no_user, call_no_workspace}` 是 **GET-only**，
而写面的 401/400 必须在**同方法**上验证（用 GET 打只注册了 POST 的路由只会得到 405）。

15 例覆盖：五条路由的 401/400/405、单形态拒尾斜杠、404/403 分层、跨 autopilot 的 trigger 404、
非 UUID 路径参数的上游顺序、create 校验顺序、schedule 的 `next_run_at` 与 `timezone` 形状、
webhook 的 token 铸造与 `webhook_path`、PATCH 三态与未触及列、DELETE 双形态、轮换只对 webhook、
secret 只写不回显。

### 6.2 门禁证据（本片 HEAD）

```bash
# 真库（566 迁移）
sudo -n -u postgres psql -c "CREATE ROLE mc_lum1568 LOGIN CREATEDB PASSWORD '…'"
sudo -n -u postgres psql -c "CREATE DATABASE multica_lum1568 OWNER mc_lum1568"
MULTICA_DATABASE_URL=… cargo run -q -p mc-migrate -- run --dir migrations   # applied 566
MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --test autopilots --features test-util -- --ignored
#   → 30 passed（M5-1 的 15 + 本片 15）

MULTICA_TEST_DATABASE_URL=… bash scripts/gates.sh --with-db --db-url …
#   → 10/10 PASS，200s：①fmt ②build ③clippy ④clippy-test-util ⑤test
#     ⑥db（migrate=0,e2e=0）⑦route-parity ⑧schema-drift ⑨conformance ⑩file-size
```

`⑦` 的两条命令都绿：`route_parity.py` 的折叠提示里出现
`PATCH /api/autopilots/:param/triggers/:param + …/` 与 `DELETE …` 两对（本片新增的 7 个注册键），
`unclaimed 0 / regression 0`；`slash_alias_audit.py` 只剩 M6 的两条 allowlist 欠账（`/api/skills`）。

## 7. 交接

- **M5-4（派发/执行）**：本片未落 `GetAutopilotTriggerForAutopilot` 与
  `autopilot_rule_version` 重打（§5.4），派发侧要自己按 `trigger.autopilot_id` 校验。
- **M5-5（webhook ingress）**：必须复用 `mc_autopilot::dto::webhook_path_for_token` 的形态
  （`/api/webhooks/autopilots/{token}`）与 `mc-repos` 的 token 生成器；
  `GetWebhookTriggerByToken` 按 `docs/44` §3.2 归 M5-5，本片**故意未实现**。
- **`get_by_id` 语义**：等价上游 `GetAutopilotTrigger`（`WHERE id = $1`，**不带** autopilot 绑定），
  调用方必须自己比对 `row.autopilot_id`（本片统一走 `load_bound_trigger_row`）。
- **C 波共享文件纪律**（`docs/44` §4.2 补记）：`mc-autopilot/src/{trigger,credential}.rs`、
  `mc-repos/src/autopilot/trigger.rs` 是多写者文件；本片的改动是**追加**（新函数落在文件末尾 /
  新模块落新文件），未改既有行的语义。后续片继续追加即可，冲突应停在文本层。
