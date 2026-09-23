# 54 · M5-5 无认证 webhook 入站面（LUM-1570）

波次 D 第一片：把上游 `POST /api/webhooks/autopilots/{token}` 的**入站面**搬进本仓 —— 这是
M5 波次里**唯一无认证的路由**（凭证就在 URL 路径里，`docs/44` §4.2 第 5 条 / 风险 R5）。上游
`handler/autopilot_webhook.go`（1,010 行）的 12 步 + `handler/webhook_delivery_worker.go` 的
「认领一条 + 推到终态」落到 `mc_autopilot::webhook/**`（签名 / 限流 / 归一化 / 准入 / worker）
与 `mc_repos::autopilot::ingress`。

- **记录号**：本片用 **`54`**（`docs/37` §44.6 的权威预约：`52`=M5-4 / `53`=M4-4-fu `LUM-1600` /
  `54`=M5-5 / `55`=M5-8）。
- **上游基准**：`louloulin/multica` @ `f41fae6b`（与 `scripts/route_parity.py` 内嵌同一 commit）。
  参照文件：`server/internal/handler/autopilot_webhook.go`、`.../webhook_delivery_worker.go`、
  `.../webhook_rate_limiter.go`、`.../handler.go:515-600`（`writeJSON`/`writeError`）、
  `server/internal/service/autopilot.go:186-470`、`server/pkg/db/queries/webhook_delivery.sql`、
  `server/cmd/server/router.go:1487`。
- **基线**：`origin/feat/multica-rs-initial` @ `5b94d54`（C 波 #58 / #59 与 **M5-8 #60** 均已合）。
  本分支从 `d2fc6c9` 起，已把 base（含 M5-8 与 `docs/57` M6 计划）真合进来 —— M5-8 的写集是
  `crates/mc-scheduler/**`，与本片**零交集**，合并无冲突。
- **无新依赖**：`Cargo.toml` / `Cargo.lock` 一行未动（`docs/15` §8.4）—— HMAC 复用
  `mc_core::hash`，限流器复用 `std::sync::LazyLock` + `tokio::sync::Mutex`。

## 0. 交付物

| 文件 | 行数 | 内容 |
| --- | ---: | --- |
| `crates/mc-http/src/routes/webhooks/autopilots.rs` | 203（+180） | 路由 #21：`router()` + 抽取器编排 + 扁平错误体 `write_json` / `read_body` |
| `crates/mc-autopilot/src/webhook/mod.rs` | 474（+456） | 常量 / `SigStatus` / `IgnoredReason` / `RejectedReason` / `WebhookEnvelope` / `InboundOutcome` / `InboundRequest` / `WebhookIngress` / **路由私有** `WebhookError` |
| `crates/mc-autopilot/src/webhook/admission.rs` | 459（+458） | A 段：`handle_inbound`（12 步）+ `settle_inbound`（第 8–12 步）+ `resolve_webhook_target` + `admit_webhook_delivery` + `finalise_terminal` |
| `crates/mc-autopilot/src/webhook/worker.rs` | 597（新） | B 段：`process_next_delivery(_in_workspace)` + `complete_delivery` / `retry_or_fail` / `defer_delivery` / `repair_run_task_link` + `WebhookDispatch` |
| `crates/mc-autopilot/src/webhook/signature.rs` | 89（新） | `X-Hub-Signature-256` 校验（`sha256=` + hex，常量时间） |
| `crates/mc-autopilot/src/webhook/ratelimit.rs` | 350（新） | 三条进程级限流器（绝对 IP / IP 债 / 每 trigger）+ 7 例单测 |
| `crates/mc-autopilot/src/webhook/provider.rs` | 485（+474） | `WebhookHeaders` / `normalize_webhook_payload` / `extract_dedupe_key` / `infer_event` / 事件作用域 |
| `crates/mc-autopilot/src/webhook/provider/tests.rs` | 325（新） | 19 例单测（门 ⑩ 拆分，见 D19） |
| `crates/mc-repos/src/autopilot/ingress.rs` | 530（+512） | W 面 SQL：`find_webhook_trigger_by_token` / `create_delivery` / `acknowledge` / `claim_queued(_in_workspace)` / `defer_claimed` / `retry_claimed` / `complete_claimed` / `update_terminal` / `touch_last_fired_at` / `find_trigger_by_id` / `find_task_status_by_run` |
| `crates/mc-repos/src/autopilot/run.rs` | 714（+3/−3） | **跨写集修复**：`load_trigger_principal` 的 workspace 收窄（D11） |
| `crates/mc-http/tests/autopilots/webhook.rs` | 739（新） | 10 例入站 e2e（门 ⑩ 拆分，见 D19） |
| `crates/mc-http/tests/autopilots/webhook_support.rs` | 264（新） | 观测面 `Res` / `post` + 夹具（同上） |
| `crates/mc-http/tests/autopilots/webhook_worker.rs` | 649（新） | 9 例 worker e2e |
| `crates/mc-http/tests/autopilots/main.rs` | 51（+6） | 三行 `mod`（`webhook` / `webhook_support` / `webhook_worker`）+ 两行模块契约注释 |

合计 **15 文件 +5366 / −34**（`git diff --numstat 5b94d54`；含 `docs/54` 本文 258 行，
`Cargo.toml` / `Cargo.lock` 零改动）。

**未触碰**（单写者规则）：`webhooks/mod.rs`（M5-0 anchor）、`routes/mount.rs`、`routes/mod.rs`、
`mc-http/src/state.rs`（`AppState` 共享 anchor ⇒ 限流器状态放模块内进程级静态）、
`mc-autopilot/src/error.rs`（跨切片热点 ⇒ `WebhookError` 落在 `webhook/mod.rs`）、
`tests/autopilots/support.rs`（M5-1 所有）、任何 `lib.rs`、`Cargo.toml` / `Cargo.lock`。
**唯一例外**是 `mc-repos/src/autopilot/run.rs` 的 3 行修复（D11）。

## 1. 路由与注册键

| # | 上游 handler | 本地 handler | axum 注册键 | 成功码 |
| ---: | --- | --- | --- | --- |
| 21 | `HandleAutopilotWebhook`（`autopilot_webhook.go:347`） | `webhooks::autopilots::handle_autopilot_webhook` | `POST /api/webhooks/autopilots/:token` | `200` |

- **单形态**：`router.go:1487` 是 plain 路由 ⇒ 0 条尾斜杠别名（`slash_alias_audit` 干净）。
- 路径参数写 `:token`（matchit 0.7 会把 `{token}` 当**字面量段**：编译过、恒 404）。
- body 上限挂在子 router 上：`.layer(DefaultBodyLimit::max(MAX_WEBHOOK_BODY_BYTES))`
  （`256 * 1024`，上游 `maxWebhookBodyBytes`）。
- **无认证中间件**：签名里没有 `AuthUser`，工作区作用域**只**来自 token 反查 trigger 后的
  `autopilot.workspace_id`；客户端给的 `X-Workspace-ID` 一概不读。

**门 ⑦ 增量（唯一判据）**：

```
基线 5b94d54（= d2fc6c9 + M5-8 + M6 计划，均为 code 面正交）：
      local 328 registered | implemented 262 real + 2 placeholder = 264/456 | known_gap 192
本片 HEAD  ：local 329 registered | implemented 263 real + 2 placeholder = 265/456 | known_gap 191
⇒ +1 注册键 / +1 implemented / −1 known_gap，unclaimed 0 / regression 0 / local_only 11（不变）
```

## 2. 逐状态码契约

| 触发 | 状态码 | body（**全部**尾随 `\n` + `content-type: application/json`） |
| --- | ---: | --- |
| 接受（准入成功） | 200 | `{"status":"accepted","delivery_id","run_id","autopilot_id","trigger_id"}` |
| 准入闸跳过（无目标 agent/squad） | 200 | 同上 + `"reason"`（run 落 `skipped`，投递仍 `queued` 等 B 段收口） |
| 忽略（停用 / 归档 / 暂停） | 200 | `{"status":"ignored","delivery_id","reason"}`（`reason_code` 为 NULL，理由在 `error` 列） |
| 忽略（事件作用域不匹配） | 200 | `{"status":"ignored","delivery_id","event","reason":"event_filtered"}` |
| 忽略（配额耗尽） | 200 | `{"status":"ignored","delivery_id","reason_code":"quota_exceeded"}`（**无** `reason`） |
| 去重命中（同 `dedupe_key`） | 200 | `{"status":"duplicate","delivery_id"[,"run_id"]}`，只自增 `attempt_count` |
| body 不是 JSON 对象/数组 / 空的 | 400 | `{"error":"empty body"}` / `{"error":"invalid json: …"}` / `{"error":"body must be a JSON object or array"}` |
| 签名缺失 / 非法 | 401 | `{"status":"rejected","delivery_id","reason"}`（投递 `status='rejected'`） |
| 空 token / 未知 token / 轮换掉的旧 token / autopilot 行缺失 / workspace 交叉失败 | 404 | `{"error":"webhook not found"}`（**同形，不泄漏存在性**） |
| body 超 `256 KiB` | 413 | `{"error":"payload too large"}` |
| 两道 IP 闸任一拒绝 | 429 | `{"error":"rate limit exceeded"}` + `Retry-After`（`ceil` 且**至少 1**） |
| 库错 / 准入失败（非配额） | 500 | `{"error":"internal error"}` / `{"error":"failed to admit autopilot"}` |

- `Content-Length` 显式给出，尾随 `\n` 与上游 `writeJSON` 逐字一致（上游注释：*Match the trailing
  newline that json.Encoder.Encode historically appended*）。落库的 `response_body` 是**不含**尾随 `\n`
  的那份 JSON。
- **凭据不回显**：响应体里没有 token，也没有签名密钥；`WebhookError` 的三条 5xx 是**固定文案**，
  真实原因只进 `tracing`（`WebhookError::Invalid` 是唯一回显字符串的变体 —— 那里回显的是调用方
  自己的 body 解析错误，上游同样回显）。

## 3. 两段式（A 段同步准入 / B 段 worker 收口）

上游把「准入」和「执行」写在同一个 service 里，本地必须拆开：`AutopilotDispatcher::dispatch()`
会真的建 issue / 任务（M5-4 的写集），入站 handler 不能同步跑它（上游也是「入站只准入，
worker 才执行」）。

```
A 段（同步，webhook/admission.rs）        B 段（worker，webhook/worker.rs）
限流两闸 → token→trigger → body 归一化 →   claim（SKIP LOCKED + 2 分钟租约）
签名 → INSERT queued → 状态/作用域闸 →      → 每 trigger 预算闸 → 归属交叉校验
admit（去重回读 / 跳过落账 / 额度化建 run）  → 归一化存量 body → admit/派发 → 收口终态
→ Acknowledge（只写响应字段，**留 queued**）
```

`create_run_with_quota` 在 A 段就把 run 建好（`issue_created`，或 `run_only` 时 `running`），
并写 `autopilot_run.webhook_delivery_id`；B 段只做「派发 + 收口」，**不会**再建第二条 run。

| B 段形态 | delivery 终态 | run |
| --- | --- | --- |
| 派发成功 | `dispatched` | `running`（`run_only`：任务挂 `autopilot_run_id`） |
| 准入闸跳过 | `dispatched` | A 段建的 `skipped`（**复用**） |
| 暂停 / 归档 / 停用 | `ignored` | 无 |
| 归属交叉校验失败 | `failed` | 无（行本身脏了，重试没意义） |
| 存量 body 归一化失败 | `failed` | 无（同上，**不重试**） |
| 既有 run 是 `failed` | `failed` | 沿用既有 run |
| 每 trigger 预算用尽 | `queued`（`DeferClaimed`） | 无 |

- 认领 → 每 trigger 限流（60/min）拒绝 ⇒ `defer_claimed(available_at = now + retry_after)`，
  **不自增** `dispatch_attempts`；其余失败走 `retry_or_fail`（`1 << min(attempts, 6)` 退避，
  第 5 次落 `failed`）。
- 租约竞态：worker 侧的每条变更 SQL 都带 `lease_token`，`ErrNoRows` 折叠成「租约丢了」+
  debug（上游 `handleWebhookLeaseMutation` 的等价物）。
- **两个计数器不要混**：`attempt_count` 是入站去重命中计数（DEFAULT 1，只有 duplicate 分支 +1）；
  `dispatch_attempts` 是派发尝试（DEFAULT 0，`complete_claimed` **无条件** +1，`defer_claimed` 不加）。

## 4. 与上游的偏差（逐条可查）

代码里以 `docs/54 D<n>` 互相引用；本表是权威编号。

| # | 偏差 | 判据 / 理由 |
| ---: | --- | --- |
| **D1** | 错误体是**扁平** `{"error":"…"}` + 尾随 `\n`，不复用本仓 `crate::error::ApiError`（嵌套体） | provider 的投递 UI（GitHub/GitLab）按扁平体解析；上游 `writeError` 就是 `map[string]string`。route 内手写 `write_json` |
| **D2** | **无可信代理支持**：`MULTICA_TRUSTED_PROXIES` 不实现，转发头一律不读，IP 只取 socket peer | 限流只是**防爆**而非鉴权，伪造 `X-Forwarded-For` 只会影响自己的桶；多读一个头反而扩大攻击面。`ConnectInfo` 缺失时记 warn 后按「无 IP」继续 |
| **D3** | 限流器是**进程内**静态（每副本一份），不是共享计数器 | 无 Redis（本波禁新依赖）；上游未配 Redis 时同样是每进程内存。多副本 ⇒ 限额≈副本数×标称值 |
| **D4** | 限流器两处**加固**（上游没有）：键空间上限 `8 192` 后 fail-open + warn；`Mutex` 中毒走 `into_inner()` fail-open | 键是**攻击者可控**的（IP / trigger id）⇒ 无界 map 是内存放大面；限流器自己 panic 不该把整条路由变成 500 |
| **D5** | 签名校验两处刻意差异：① 入站 hex 先归一小写再比（上游 `hex.DecodeString` 两种大小写都收，而 `hmac_sha256_verify` 逐字节比 hex）；② 加一道 `is_ascii_hexdigit` 预检 | 逐字照抄会在「客户端送大写 hex」时误判 `invalid`。前缀 `sha256=` 仍**大小写敏感** |
| **D6** | `WebhookEnvelope.event_payload` 是 `serde_json::Value`（上游 `json.RawMessage` 原样字节） | 本地 jsonb 列按 `Value` 读写是本仓既有约定（`#[sqlx(flatten)]` 在本仓 sqlx 0.8 下不可用，见 D10）。字节级重序列化对 webhook 载荷无语义影响 |
| **D7** | 信封序列化失败折成 `WebhookError::Internal`（500 `internal error`） | 结构里只有 `String` / `Value`，正常不可能失败；留一条不 panic 的兜底路径 |
| **D8** | **不含轮询循环**：1 s ticker / `Notify` / 4 并发调度**在本仓尚无 owner** | 本片只交「认领一条 + 推到终态」这一步（`process_next_delivery[_in_workspace]`）。`docs/44` 的文件→切片表里**没有** `handler/webhook_delivery_worker.go`（M5-5 的 span 只到 service 侧那三处），而已合的 M5-8 是 `mc-scheduler` 的两个 job（与本面无关）⇒ 循环 + 通知触发需要 M5-INT（`LUM-1572`）定归属或新开切片。当前 `process_next_delivery*` **无生产调用点** |
| **D9** | 新增 **workspace 收窄的认领变体**：`claim_queued_in_workspace`（repo）+ `process_next_delivery_in_workspace`（service）；生产路径仍是全局 `claim_queued` | 认领是**整库**的（上游 worker 是单例），而同一 test binary 内的用例并发跑 ⇒ 全局认领会抢走邻例刚落的 `queued` 行。变体与全局版 SQL 逐字相同，只多 ` AND workspace_id = $1` |
| **D10** | `find_webhook_trigger_by_token` 必须写 `a.workspace_id AS autopilot_workspace_id` 并**手写** `sqlx::FromRow` | PG 结果列名取裸列名 ⇒ `SELECT a.workspace_id` 的列名是 `workspace_id`，`try_get("autopilot_workspace_id")` 运行期报 `no column found for name: autopilot_workspace_id`（编译期看不出来，真库实测）。`#[sqlx(flatten)]` 在本仓 sqlx 0.8 下不可用 |
| **D11** | **跨写集修复**：`mc-repos/src/autopilot/run.rs::load_trigger_principal` 的 SQL 改成上游形态（JOIN `autopilot` 取 `a.workspace_id`），±3 行 | 原实现写 `t.workspace_id`（`autopilot_trigger` **没有**该列，上游也没有）⇒ 真库必炸 `column t.workspace_id does not exist`。M5-4 的直调路径没走到这一读，webhook 面第一次走到就暴露。**只改这一条只读查询，绑定参数与语义不变** |
| **D12** | 不移植 `SyncRunFromTask` 的**重放**（只读 task 状态做判断） | 那条重放会反向改 run 状态，属 M5-4 的终态回写面（`dispatch/sync.rs`）；本片只需要「task 是否已终态」。多读两列（`find_task_status_by_run`）比复制一份状态机安全 |
| **D13** | 不移植 `UpdateWebhookDeliveryDispatched` / `finaliseDeliveryWithRun` | **上游死代码**：全仓 grep 只有定义、无调用点（`autopilot_webhook.go:887`）⇒ 本地不落这条语句，带 run 的终态只走 `complete_claimed` |
| **D14** | 事件作用域不匹配 ⇒ `status='ignored'` + `reason='event_filtered'`，**不是** `dispatched` | DoD 文案写「不产生 run（但 delivery 仍记 `dispatched`）」，与上游 `finaliseDeliveryTerminal(…ignored…, "event_filtered")` **冲突** ⇒ 取上游。理由在 `error` 列，`reason_code` NULL |
| **D15** | 准入失败（非配额）⇒ 500 `{"error":"failed to admit autopilot"}`，投递**留在 `queued`**、不写终态 | 上游 `AdmitAutopilotWebhookDelivery` 的库错也是 500；留在 `queued` 让 worker 稍后重试准入（本片无循环 ⇒ 等 D8 那条循环落地后的 sweep）。用独立变体（而不是 `Internal`）是为了让 5xx 也能区分「内部错」与「准入错」而不泄漏细节 |
| **D16** | `ensureWebhookCreateIssueTask` 未移植（降级为 `tracing::debug` + 复用既有 run） | 本地「建 issue / 挂 run / 建 task」是**同一个事务**（M5-4 `create_issue.rs`）⇒ 上游那个「run 建好但 task 缺失」的崩溃窗口不存在 |
| **D17** | `repair_run_task_link` 只补**链接**，不重放终态 | 终态回写属 M5-4 `sync.rs`；本片只做「run 缺 `agent_task_id` 时按 `autopilot_run_id` 回链」 |
| **D18** | `mc-repos` 侧新增两个**只读自由函数**（`find_trigger_by_id` / `find_task_status_by_run`） | `trigger.rs::get_by_id` 是**需要 `Db` 的 repo 方法**，而 `mc-autopilot` 只持有 `PgPool`（无 `mc-db` 依赖）⇒ 构造不出来。自由函数 + `map_sqlx_err` 与既有 `ingress.rs` 同手法 |
| **D19** | **R7 文件拆分**（门 ⑩ 800 行硬上限）：`webhook/admission.rs` 拆出 `webhook/worker.rs`；`webhook/provider.rs` 拆出 `provider/tests.rs`（`#[cfg(test)] mod tests;`）；测试 target 拆成 `webhook.rs` / `webhook_support.rs` / `webhook_worker.rs` | 拆前分别 1003 / 803 / 987 行。同一 `impl WebhookIngress` 跨兄弟文件 ⇒ 跨文件私有项提为 `pub(super)`（`AdmitRefusal` / `admit_webhook_delivery`）；`WebhookIngress` 字段不受影响（结构体定义在 `webhook/mod.rs`，是两个文件的父模块）。**多一个 `mod` 行**（`main.rs`）是机械结果 |
| **D20** | `base_content_type`：没有分号时**不 trim**（`"  "` 原样入库） | 逐字照抄上游（只在 `strings.Index(…, ";") >= 0` 分支里 `TrimSpace`）。「顺手 trim」会悄悄改掉信封里的 `request.contentType` |
| **D21** | body 上限的**判定点提前**：axum 抽取器先读 body（超限 → 413），再进 service 的限流与 token 反查 | 上游顺序是「限流 → token → body」。`DefaultBodyLimit` 只能作用在 body 抽取器上（`Bytes`），而 `http_body_util` 在本 crate 只是 dev-dependency ⇒ `src/` 里拿不到 `LengthLimitError` 去手写流式读。后果只影响**优先级**：超限请求遇到未知 token 时回 413（不是 404）、遇到限流耗尽时回 413（不是 429）。响应形状与「不落库」判据不变 |
| **D22** | worker 侧重建的 `WebhookHeaders`（`from_selected`）**不含**签名头 ⇒ `x_hub_signature_256` 恒 `None`；worker **不**重跑签名校验 | 上游 `headersFromSelected` 同样只装落库的那 8 个头（签名在入站已判过，`status='rejected'` 的投递根本进不了 worker） |
| **D23** | `reason_code` **只有配额路径**写（`quota_exceeded`）；其余终态（rejected / ignored-*）理由文本进 `error` 列、`reason_code` NULL | 上游 `finaliseDeliveryTerminal` 的 `reasonCode` 是**可变参数**，只有配额调用点传值 |

**与上游逐字一致、因此不登记**（列出来是为了将来别误改）：去重键抽取（`github` ⇒
`X-GitHub-Delivery`，其余 `Idempotency-Key`，再兜底 `X-GitHub-Delivery`，空串 ⇒ 无，取前 trim）、
事件推断与 `splitWebhookEvent` 的 provider 识别集、`webhookActionCandidates` 的候选顺序、
事件作用域判据（无过滤器 ⇒ 放行；**malformed JSON ⇒ fail-closed**）、
签名/读体/404 的全部文案、`Retry-After = ceil(…, min 1)`、worker 退避 `1 << min(a, 6)`、
最大尝试 5 次、租约 2 分钟。

## 5. 测试与门禁

### 5.1 布局

| 层 | 文件 | 例数 |
| --- | --- | ---: |
| 纯函数单测（无 DB） | `mc-autopilot/src/webhook/ratelimit.rs` | 7 |
| 纯函数单测（无 DB） | `mc-autopilot/src/webhook/provider/tests.rs` | 19 |
| 入站 e2e（真库 + 真 router） | `mc-http/tests/autopilots/webhook.rs` | 10 |
| worker e2e（真库 + 真 SQL） | `mc-http/tests/autopilots/webhook_worker.rs` | 9 |

入站 10 例覆盖 DoD 三硬判据 + 其余契约：

1. 未知 / 轮换掉的 / 空 token 三种形态**逐字同形** 404 且**一行不落**；
2. body 超上限 → 413，且**落库前**就返回；
3. 空 body / 坏 JSON / 标量 body → 400 且不落库；
4. 签名缺失 / 非法 → 401 `rejected`（`signature_status='invalid'`）；
5. 合法签名**大小写两种 hex** 都接受（D5）；
6. 同 `dedupe_key` 二次入站 → 200 duplicate，只自增 `attempt_count`，**不建第二条 run**；
7. 事件作用域不匹配 → 200 ignored，**不建 run**（D14）；
8. 停用 trigger / 非 active autopilot → 200 ignored（三种 reason 逐条断言）；
9. 用尽坏凭据债 → 429 + `Retry-After ≥ 1`；
10. 响应体与投递行里**都不含** token 与签名密钥（凭据不回显）。

worker 9 例逐条对应 §3 的收口表：

1. `worker_dispatches_a_queued_delivery_and_marks_it_dispatched`（`run_only` 派发成功 + run 回链 +
   `last_fired_at`）；
2. `a_skipped_run_still_settles_the_delivery_as_dispatched`（**复用**入站建的 `skipped` run）；
3. `worker_rechecks_mutable_state_only_when_no_run_exists`（崩溃窗口 + 暂停 ⇒ 收口 `ignored`）；
4. `worker_fails_the_delivery_on_an_ownership_mismatch`（**不重试**）；
5. `worker_fails_the_delivery_when_the_stored_body_cannot_be_normalized`（**不重试**）；
6. `worker_settles_the_delivery_as_failed_when_the_run_is_failed`；
7. `worker_defers_when_the_per_trigger_budget_is_spent`（`queued` + `available_at` 前移，**不**计尝试）；
8. `retry_sql_bumps_the_attempt_counter_and_keeps_the_response_fields`（`retry_claimed` 的 SQL 语义：
   计数 +1、租约释放、`available_at` 前移）；
9. `claim_skips_deliveries_that_are_not_due_yet`（未到期行不认领 ⇒ 队列空）。

租约竞态（`lease_token` 不匹配 ⇒ `ErrNoRows` 折叠成幂等无操作）是**代码路径**，本片没有为它
单独造 e2e（要精确制造「认领后被别人改租约」需要第二条连接做时序注入，收益低于成本）。

### 5.2 门禁证据（本片 HEAD）

```
$ MULTICA_TEST_DATABASE_URL="$(cat ~/.mc_lum1570_dburl)" bash scripts/gates.sh --with-db
① fmt 2s  PASS   ② build 3s   PASS   ③ clippy 2s PASS   ④ clippy-test-util 0s PASS
⑤ test 33s PASS  ⑥ db 12s PASS (migrate=0,e2e=0)   ⑧ schema-drift 25s PASS
⑦ route-parity 0s PASS  ⑨ conformance 5s PASS  ⑩ file-size 0s PASS
overall: PASS — 10/10 gate(s) green in 82s
```

上面的读数是 **真实合并树**（base `5b94d54`，含 M5-8）上的那一轮；上一轮未合 base 时同树为
191s，①—⑩ 同样全绿。合并后再单独复核两项：

```
⑦  upstream 456 (commit f41fae6b08fb) | local 329 registered | baseline 300
    implemented 263 real + 2 placeholder = 265 / 456   known_gap 191   unclaimed 0   regression 0   local_only 11
⑨  report matches crates/mc-conformance/report.json
    totals = fixtures 365 / pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306
```

- ⑥ 是**真库**：migrate 566 条迁移 + 全部 `#[ignore]` e2e（`autopilots` target 跑 108 例 /
  84 例被 `--ignored` 过滤；`mc-repos` 侧同样全绿）。
- ⑦：`upstream 456 (commit f41fae6b08fb) | local 329 registered | baseline 300`，
  `implemented 263 real + 2 placeholder = 265 / 456   known_gap 191   unclaimed 0   regression 0
  local_only 11`，`OK: every upstream route is either implemented or owned`。
  ⇒ 本片落地后 ⑦ 缺口归属板上 **`owners.M5` 由 1 变 0**（M5 最后一条未实路由就是本路由）。
- ⑨：`report matches crates/mc-conformance/report.json`，totals **与基线一致**
  （`pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306`）—— 本路由**零 fixture**
  （`contracts/golden/autopilots/` 8 条只碰 2 条路由），等价证据全靠上面的 19 例 e2e。
- ⑩：改动文件最大 739 行（`tests/autopilots/webhook.rs`）< 800。
  注意 ⑩ 只扫 `git ls-files` 里**已跟踪**的路径 ⇒ 新增文件必须先 `git add` 再跑门，否则会漏判。
- ③/④ 的 `-D warnings` 全过（`#[allow]` 用在本片内部：`settle_inbound` 的
  `too_many_arguments` + `too_many_lines`、`process_next_delivery_scoped` 的 `too_many_lines`、
  `repair_run_task_link` 的 `type_complexity` + `result_large_err`）。

### 5.3 本地跑法（DB 夹具）

```
# 一次性：角色 mc_lum1570 / 库 multica_lum1570（本机 PG16:5432）
MULTICA_DATABASE_URL="$(cat ~/.mc_lum1570_dburl)" cargo run -q -p mc-migrate -- run --dir migrations
MULTICA_TEST_DATABASE_URL="$(cat ~/.mc_lum1570_dburl)" \
  cargo test -p mc-http --test autopilots --features test-util -- webhook:: --ignored
```

未设 `MULTICA_TEST_DATABASE_URL` ⇒ 每例打印 skip 并 `return`（不算绿）；
**设了但连不上 ⇒ panic**（坏库必须红，不能静默绿）。门 ⑤ 不带 DB URL，门 ⑨ 用
`env -u` + `--no-db` 双重保险。

## 6. 交接

1. **worker 轮询循环仍无 owner**（D8）：1 s ticker + `Notify` + 4 并发，循环体直接调
   `WebhookIngress::process_next_delivery()`（本片已定契约：`Ok(None)` = 队列空 / `Ok(Some(row))` =
   收口一条 / `Err` = 认领期基础设施错误）。已合的 **M5-8（`LUM-1571`）是 `mc-scheduler` 的两个 job**
   （autopilot schedule / issue wakeup），**不含**这条循环；`docs/44` 的文件→切片表也没给它分配写者
   ⇒ 需 M5-INT（`LUM-1572`）定归属。
2. **生产链路还差「通知 worker」这一步**：入站 `Acknowledge` 后投递留在 `queued`，
   现在没有任何东西会把它捡起来（上游是 `Notify` 触发 + 1 s ticker 兼底）。
3. **DoD 文案与上游冲突一条**（D14）：事件作用域不匹配记 `ignored` 而非 `dispatched`；
   如需改成 DoD 口径，改 `settle_inbound` 的那一处调用即可，但会与上游偏离。
4. `find_task_status_by_run` / `find_trigger_by_id` 是 webhook worker 专用读；M5-4 的
   `SyncRunFromTask` 若将来要移植，可直接复用前者。
5. 无 DB 单测只有 `ratelimit` / `provider` 两层；`signature.rs` 的三种输入形态现在只有
   e2e 覆盖（真库）—— 若想让它进 ⑤（无 DB 门），需要给 `mc-autopilot` 加一个不依赖
   `PgPool` 的入口。
6. 上游 `docs/44` §342 把 M5-5 的 span 写成含 `ensureWebhookCreateIssueTask`(61) 与
   `repairAutopilotRunTaskLink`(56)；本片分别按 **D16**（事务已合并 ⇒ 那个崩溃窗口不存在）与
   **D17**（只补链接）降级 —— 两者的**完整**移植（含 `SyncRunFromTask` 重放）仍属 M5-4 的终态回写面，
   若验收要求逐行对齐，请在 M5-INT 上重新裁定。
