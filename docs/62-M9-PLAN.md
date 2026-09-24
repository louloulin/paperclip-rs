# docs/62 — M9（W9 商业面）切片计划：cloud-billing / cloud-subscriptions / dashboard / onboarding / feedback / contact-sales / notification-preferences / stripe webhook / mika / issue timeline（34 路由）

**状态**：M9 计划片（`LUM-1814`）交付物。M9 代码切片已按本文建为 `LUM-1814` 的**子 issue（M9-0 … M9-10，11 条，全部 `backlog`）**，
**待 M7 收口（`LUM-1786` M7-21 INT 合）+ M8 收口（`LUM-1804` M8-7 INT 合）后晋升**（依据见 §7）。

**上游口径**：`multica` @ **`f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`**（= `docs/fixtures/upstream-routes.tsv` 记录的那个 commit）。
只读副本本次克隆在 **`LUM-1814` 自己的 workdir**（`ups/up-m9/`，`git clone --depth 1 --branch main` + `git fetch --depth 1 origin <sha>` + `checkout FETCH_HEAD`；
上游 `main` 的 tip 是 `90e0bdf`，不是本表钉的 sha），**不依赖 `/tmp/ups*` 或别的 run 的 workdir**（`LUM-1674` 亲眼见过跨 workdir 副本被 GC）。

**本地基线**：`paperclip-rs` @ **`f73d916d`**（`origin/feat/multica-rs-initial`，= M6-INT `LUM-1675` 的合并提交；本片起手实测）。
本文所有 ⑦/⑨/⑩ 读数都在 `f73d916d` 上测得（复算命令见 §10）。

**文档编号**：`60` = `docs/60-M7-PLAN.md`、`61` = `docs/61-M8-PLAN.md`，两者均已合入 ⇒ 本文取 **`62`**
（起手已 `git fetch origin feat/multica-rs-initial` + `ls docs/ | sort` 复核，实测最大号为 61，`docs/fixtures/` 最大号为 `m8-declared-routes.tsv`）。

> 本文对 `docs/plan1.md` / `docs/15-M3-PLAN.md` / `docs/01-PLAN.md` 有 **9 处口径修订**（§9），其中**四处是承重的**：
> ① `plan1.md` §3.3 的 W9 **4 个 crate 收敛为 2 个**（`mc-cloud` + `mc-entitlement`，§9.3）；
> ② `mcp-servers` 式的「crate 账」不存在，但 **cloud-runtime 11 条（owner 单元格仍是 `M3`）的 crate 归属 = `mc-cloud`、波次账目 = 仍在 M3**（§9.2）；
> ③ **`plan1.md` §5 写的「billing(8) subscriptions(7)」单位是「路由条数」**，不是上游文件数；本波上游面 **4,711 行**（§9.1）；
> ④ **`docs/15` §9.1 裁决的 cloud-runtime 归属（M9）与 fixture 的 owner 单元格（M3）至今不一致**，本片**不改单元格**（⑦ 逐字不变是硬约束），改为登记**归属迁移建议 + 执行点**（§9.2）。
> 其余承接 `docs/57-M6-PLAN.md` / `docs/60-M7-PLAN.md` / `docs/61-M8-PLAN.md` 的骨架与全部记法纪律。

---

## 0. 结论速览

| 项 | 结论 |
|---|---|
| 本波路由 | **34 条**（`docs/fixtures/upstream-routes.tsv` 里 owner=M9 的行数）。其中 **33 条是 `known_gap`**，1 条（`GET /api/issues/{id}/timeline`）**已在本地注册为 `not_implemented` 占位** ⇒ 本波把它升级为真实现（注册键不变）。**不要**把 34 与 33 混用（§1.7） |
| 本波上游体量 | **4,711 行**（`internal/handler/*` **3,705** 行 + `internal/{cloudruntime,entitlement}` **1,006** 行，逐文件见 §1.2）。**不含** cloud-runtime 的 208 行（§9.2）、不含 `seatcapacity` 的 909 行（R-M9-6） |
| 本波新迁移 | **0**。M9 面 12 张表 + 6 个 user 列**全部已在** `migrations/upstream/`（§6.4）；billing / subscriptions / stripe **本地 0 张表**（纯出站代理，§9.4） |
| 新 crate | **2 个**：**`mc-cloud`**（唯一出站传输 + billing / subscriptions / stripe webhook 三个面 + `mc-cloud` 的 `Provider` 位）、**`mc-entitlement`**（远程策略客户端 + 有界缓存 + 单飞 + `Gate`/`Action` 类型）。**`plan1.md` §3.3 的 `mc-onboarding` / `mc-feedback` / `mc-dashboard` 三个 crate 不建**（判据与代价见 §2.2、§9.3） |
| 切片数 | **1 anchor + 8 代码片 + 1 INT = 10 个 issue**（+ 本文自身 = 11 条子 issue），**5 个 stage**，stage 内并发 ≤ **3**；单片最大 **995** 上游行（`M9-3`，≤3.5k 上限，§4.1） |
| 尾斜杠双形态 | **3 键**（`/api/notification-preferences/` 的 GET/PATCH/PUT）。`python3 scripts/slash_alias_audit.py --declared docs/fixtures/m9-declared-routes.tsv` 实测 `declared 34 / dual-form required: 3 / single-form: 31`、**`FAIL: 3`（exit 1）**——**这是预测值，不是缺陷**：它把「本波必须先补哪 3 个无斜杠形态」写成可执行的清单（对照：M4 15 / M5 7 / M6 5 / M7 0 / M8 0，§1.4） |
| ⑦ 目标 | anchor 后**逐字不变**（`454/378/78`，baseline 不动）——**M9-0 是本仓第三个不刷基线的 anchor**，机制同 M8-0：**1 条占位原地搬运**、0 新注册键；全波落地后 `local 487 / implemented 411（real 409 + ph 2）/ known_gap 45 / owners.M9 0`，`implemented + known_gap == 456` 恒成立（§6.1） |
| ⑨ 目标 | 落在 M9 34 条路由上的上游 fixture **共 20 条**（`unevaluable 17` + `unmounted 3`）。**17 条 actor=member ⇒ stateless 层结构上永远 `unevaluable`**（既有 lesson），**3 条 anonymous（都是 `POST /api/webhooks/stripe`，期望 401/403/429）是本波唯一可在无库模式下转绿的**；其余靠本波**自造离线 fixture**，且**不承诺**提高 `contract_equivalence_rate`（当前 0.0137，§6.2） |
| 最大风险 | R-M9-1 **`MULTICA_CLOUD_URL` 不可达 ⇒ 16 条代理路由全部只能拿到 403/502**（离线替身是唯一证据来源）；R-M9-2 **`RequireHumanActor` 等价物缺失**（本仓无「机器凭据一律 403」的中间件，而计费面**必须**有）；R-M9-3 **`state.rs` 第二次被 anchor 追加**（530 → 预计 ~600，逼近 ⑩ 硬限 800）；R-M9-4 **`mika` 与既有 agent/chat 面两处共享文件**（§9.1 的裁定把它压到单值） |

---

## 1. 上游面测绘（`f41fae6b08fb` 实测）

### 1.1 路由表（34 条，按**注册块 / 授权层**分 10 簇）

M9 与 M6/M7/M8 的结构性差异：M9 的 34 条**没有任何一条与别的波共享上游文件**，但它们散在 **10 个互不相关的注册块**里，
授权层有 **4 种**（公开 / 会话级 user / workspace member / workspace admin + 机器凭据闸）。

| 簇 | router.go 行 | 条数 | 注册块（逐字） | 授权层 |
|---|---|---:|---|---|
| A cloud-billing | 1913–1920 | 8 | `r.Route("/api/cloud-billing", …)` + `r.Use(handler.RequireHumanActor)` | Auth + **机器凭据 403** |
| B cloud-subscriptions | 1934–1943 | 7 | `r.Route("/api/cloud-subscriptions", …)` 两个 `r.Group` | Auth + 机器凭据 403 + **member**（读 2）/ **owner\|admin**（写 5） |
| C stripe webhook | 1505 | 1 | 公开块（`// Public API`） | **无**（凭签名，且签名校验在云侧） |
| D contact-sales | 1479 | 1 | 公开块 + `r.With(contactSalesRL)` | **无** + 限流 5/h |
| E onboarding | 1617–1627 | 5 | `// --- User-scoped routes ---`（Auth 组内） | Auth（user 级，无 workspace） |
| F feedback | 1634 | 1 | 同 E | Auth + handler 内 10/h |
| G notification-preferences | 2401–2403 | 3 | workspace-scoped `r.Group` 内 | Auth + **workspace member** |
| H dashboard | 2256–2261 | 6 | workspace-scoped `r.Group` 内 | Auth + **workspace member** |
| I mika | 2184 | 1 | `/api/agents` 子路由 | Auth + **workspace member** |
| J issue timeline | 1981 | 1 | `/api/issues/{id}` 子路由 | Auth + **workspace member** |

逐条（`METHOD PATH router.go:行 片`，与 `docs/fixtures/m9-declared-routes.tsv` 34/34 相等，复算见 §10 命令 1）：

| # | METHOD | PATH | router.go | 片 | 上游 handler（`file:行`） |
|---:|---|---|---:|---|---|
| 1 | POST | `/api/agents/mika` | 2184 | M9-7 | `CreateMikaAgent`（`mika_agent.go:82`） |
| 2 | GET | `/api/cloud-billing/balance` | 1913 | M9-1 | `GetCloudBillingBalance`（`cloud_billing.go:356`） |
| 3 | GET | `/api/cloud-billing/transactions` | 1914 | M9-1 | `ListCloudBillingTransactions`（`:366`） |
| 4 | GET | `/api/cloud-billing/batches` | 1915 | M9-1 | `ListCloudBillingBatches`（`:377`） |
| 5 | GET | `/api/cloud-billing/topups` | 1916 | M9-1 | `ListCloudBillingTopups`（`:385`） |
| 6 | GET | `/api/cloud-billing/price-tiers` | 1917 | M9-1 | `ListCloudBillingPriceTiers`（`:399`） |
| 7 | POST | `/api/cloud-billing/checkout-sessions` | 1918 | M9-1 | `CreateCloudBillingCheckoutSession`（`:410`） |
| 8 | GET | `/api/cloud-billing/checkout-sessions/{sessionId}` | 1919 | M9-1 | `GetCloudBillingCheckoutSession`（`:421`） |
| 9 | POST | `/api/cloud-billing/portal-sessions` | 1920 | M9-1 | `CreateCloudBillingPortalSession`（`:476`） |
| 10 | GET | `/api/cloud-subscriptions/summary` | 1934 | M9-2 | `GetCloudWorkspaceSubscriptionSummary`（`:169`） |
| 11 | GET | `/api/cloud-subscriptions/prices` | 1935 | M9-2 | `GetCloudWorkspaceSubscriptionPrices`（`:185`） |
| 12 | POST | `/api/cloud-subscriptions/checkout-sessions` | 1939 | M9-2 | `CreateCloudWorkspaceSubscriptionCheckout`（`:197`） |
| 13 | POST | `/api/cloud-subscriptions/seats/purchase-preview` | 1940 | M9-2 | `PreviewCloudWorkspaceSubscriptionSeatPurchase`（`:260`） |
| 14 | POST | `/api/cloud-subscriptions/seats/purchases` | 1941 | M9-2 | `PurchaseCloudWorkspaceSubscriptionSeats`（`:290`） |
| 15 | POST | `/api/cloud-subscriptions/seats/reconcile` | 1942 | M9-2 | `ReconcileCloudWorkspaceSubscriptionSeats`（`:249`） |
| 16 | POST | `/api/cloud-subscriptions/portal-sessions` | 1943 | M9-2 | `CreateCloudWorkspaceSubscriptionPortal`（`:341`） |
| 17 | POST | `/api/contact-sales` | 1479 | M9-5 | `CreateContactSales`（`contact_sales.go:121`） |
| 18 | GET | `/api/dashboard/usage/daily` | 2256 | M9-4 | `GetDashboardUsageDaily`（`dashboard.go:178`） |
| 19 | GET | `/api/dashboard/usage/by-agent` | 2257 | M9-4 | `GetDashboardUsageByAgent`（`:262`） |
| 20 | GET | `/api/dashboard/agent-runtime` | 2258 | M9-4 | `GetDashboardAgentRunTime`（`:384`） |
| 21 | GET | `/api/dashboard/runtime/daily` | 2259 | M9-4 | `GetDashboardRunTimeDaily`（`:474`） |
| 22 | GET | `/api/dashboard/failures/daily` | 2260 | M9-4 | `GetDashboardFailuresDaily`（`:541`） |
| 23 | GET | `/api/dashboard/failures/by-agent` | 2261 | M9-4 | `GetDashboardFailuresByAgent`（`:588`） |
| 24 | POST | `/api/feedback` | 1634 | M9-5 | `CreateFeedback`（`feedback.go:66`） |
| 25 | GET | `/api/issues/{id}/timeline` | 1981 | M9-8 | `ListTimeline`（`activity.go:149`） |
| 26 | PATCH | `/api/me/onboarding` | 1617 | M9-3 | `PatchOnboarding`（`onboarding.go:226`） |
| 27 | POST | `/api/me/onboarding/complete` | 1618 | M9-3 | `CompleteOnboarding`（`onboarding.go:69`） |
| 28 | POST | `/api/me/onboarding/cloud-waitlist` | 1619 | M9-3 | `JoinCloudWaitlist`（`onboarding.go:321`） |
| 29 | POST | `/api/me/onboarding/runtime-bootstrap` | 1626 | M9-3 | `BootstrapOnboardingRuntime`（`onboarding_shim.go:134`，**DEPRECATED**） |
| 30 | POST | `/api/me/onboarding/no-runtime-bootstrap` | 1627 | M9-3 | `BootstrapOnboardingNoRuntime`（`onboarding_shim.go:360`，**DEPRECATED**） |
| 31 | GET | `/api/notification-preferences/` | 2401 | M9-5 | `GetNotificationPreferences`（`notification_preference.go:36`） |
| 32 | PATCH | `/api/notification-preferences/` | 2402 | M9-5 | `PatchNotificationPreferences`（`:150`） |
| 33 | PUT | `/api/notification-preferences/` | 2403 | M9-5 | `UpdateNotificationPreferences`（`:124`） |
| 34 | POST | `/api/webhooks/stripe` | 1505 | M9-6 | `HandleCloudBillingStripeWebhook`（`cloud_billing.go:520`） |

账（每行恰好一片）：M9-1 **8** + M9-2 **7** + M9-3 **5** + M9-4 **6** + M9-5 **5** + M9-6 **1** + M9-7 **1** + M9-8 **1** = **34** ✓

### 1.2 上游文件与行数（非测试）+ **单位裁定**

**裁定结论（§9.1）**：`plan1.md` §5 的 W9 行写的 `cloud-runtime(11) billing(8) subscriptions(7) … dashboard(6)`
—— **括号里的单位是「上游路由条数」**（与 `docs/60` §9.1 对渠道行、`docs/61` §9.1 对 `vcs/ghsnapshot/composio` 行的同类裁定一致），
**不是文件数、不是行数、不是 handler 数**。三条判据：

1. 路由条数与 fixture 逐字相符：`/api/cloud-subscriptions` **7**、`/api/dashboard` **6**、`/api/cloud-billing` **8**、`/api/cloud-runtime` **11**（§10 命令 1）。
2. 行数口径差三个数量级（34 条路由对应上游 **4,711** 行，不是 32）；且**文件数口径也不成立**：`/api/dashboard` 的 6 条全在 **1 个文件**，而 `/api/cloud-billing` 的 8 条与 `/api/cloud-subscriptions` 的 7 条**共用同一个文件**
   ⇒ 报数必须**两栏并列**（`路由条数` 与 `非测试行数`）。

**M9 面的实测构成**（本片逐文件复算，命令见 §10 命令 2）：

| 上游文件 | 行数 | 片 | 说明 |
|---|---:|---|---|
| `internal/handler/cloud_billing.go`（L1–L37 文件头 + L356–L503） | 185 | M9-1 | owner-credit 8 条 + 复用 `proxyCloudRuntime`（在 `cloud_runtime.go`） |
| `internal/handler/cloud_billing.go`（L54–L355） | 302 | M9-2 | workspace subscription 7 条 + `requireCloudSubscriptionWorkspace`/`proxyCloudSubscription` |
| `internal/handler/cloud_billing.go`（L38–L52 + L504–L604） | 117 | M9-6 | `maxStripeWebhookBodySize`/`stripeSignatureHeader` + `HandleCloudBillingStripeWebhook` |
| `internal/handler/onboarding.go` | 372 | M9-3 | 3 条（`complete`/`patch`/`cloud-waitlist`）+ 问卷答案类型 |
| `internal/handler/onboarding_shim.go` | 623 | M9-3 | 2 条 DEPRECATED shim（helper agent + starter issue 的 provision 链） |
| `internal/handler/dashboard.go` | 655 | M9-4 | 6 条聚合 + `foldRestrictedAgents` 可见性折叠 |
| `internal/handler/notification_preference.go` | 172 | M9-5 | 3 条（同一路径三方法） |
| `internal/handler/feedback.go` | 177 | M9-5 | 1 条 + 10/h 限流 |
| `internal/handler/contact_sales.go` | 323 | M9-5 | 1 条 + 企业邮箱域名/枚举校验 |
| `internal/handler/mika_agent.go` | 328 | M9-7 | 1 条（get-or-create + 会话锁） |
| `internal/handler/activity.go`（**L63–L393**） | 331 | M9-8 | 3 个 helper + `ListTimeline` + 4 个映射/水合函数 |
| **handler 小计**（不含 `actor_guards`） | **3,585** | | |
| `internal/cloudruntime/client.go` | 255 | M9-0 | **唯一出站传输**（Fleet + Billing + Stripe 转发共用，`Request.Headers` 注释逐字点名 Stripe 直通） |
| `internal/entitlement/`（`client.go` 422 + `cache.go` 113 + `types.go` 128 + `doc.go` 10 + `entitlementtest/stub.go` 78） | 751 | M9-9 | 远程策略客户端 + 有界缓存 + 单飞 + 测试替身 |
| **包小计** | **1,006** | | |
| `internal/handler/actor_guards.go` | 120 | M9-0 | `RequireHumanActor`（**计费面唯一的机器凭据闸**，billing + subscriptions 两个簇共用） |
| **M9 全波合计** | **4,711** | | |

**`cloud_billing.go` 是唯一被三片共用的上游文件**：604 行按**函数定义行**切成三段（`L169` = subscriptions 首函数、`L356` = billing 首函数、`L520` = stripe 首函数）。
这不是「拆上游文件」，而是**本仓按本地文件分工**的必然结果（一个上游文件里的 16 条 handler 分属**两种授权层** + 一个**公开面**）。切点逐条可复算（§10 命令 2）。

**`activity.go` 的 M9 段是 L63–L393**：`L1–L62` 是包/导入/注释、**`L394–L461` 的 `GetAssigneeFrequency` 属 `M2-A`**（fixture owner=`M2-A`，`router.go:1952`）⇒ **M9-8 的写集不含 `/api/assignee-frequency`**（否则就是抢 `M2-A` 的账）。

**`internal/{feedback,dashboard,onboarding,contact_sales,billing,subscription}` 这些包**上游**不存在**（实测 `ls internal/`）——
商业面在上游全部是 `internal/handler/*.go` + 一张表，这正是 §2.2 把 `plan1.md` 的 4 个 crate 收敛成 2 个的**第一条硬证据**。

### 1.3 本地现状与缺口（`f73d916d` 实测）

```
$ python3 scripts/route_parity.py
upstream 456 (commit f41fae6b08fb) | local 406 registered | baseline 406
  implemented  326 real +   4 placeholder =  330 / 456   known_gap  126   unclaimed    0   regression   0   local_only    9
  gaps by owner: M9=33  M7=24  M8=24  M3+=16  M2-A=13  M3=11  M10=5
```

* **M9 的 34 条里，33 条是 `known_gap`**；第 25 行 `GET /api/issues/{id}/timeline` 已以 `not_implemented` 注册在
  `crates/mc-http/src/routes/issues/mod.rs:191`（返 501）⇒ 计入 `implemented_placeholder`，不占 `known_gap`。
* **M9 面在本地几乎零代码**：`grep -rli 'feedback|contact_sales|notification_preference|cloud-billing|cloud_subscriptions|stripe' crates/ --include=*.rs`
  **全部 0 命中**；`/api/me/onboarding`、`/api/notification-preferences`、`/api/dashboard/*` 三条前缀在 `routes/mount.rs` 里都不存在。
  ⇒ **M9 是从零起的一波**，但它有 **3 个已交付的接缝**必须复用而不是重写（§2.3）：
  ① `crates/mc-autopilot/src/quota.rs` 的 `QuotaPolicyProvider` + `install_policy_provider`（**M5-1 明确为 M9 留的 entitlement 接缝**）；
  ② `crates/mc-chat/src/onboarding.rs` + `crates/mc-repos/src/chat_task/onboarding.rs` + `crates/mc-http/src/routes/chat/task/dispatch.rs`（M4-4 的 Mika 引路）；
  ③ `crates/mc-autopilot/src/webhook/ratelimit.rs`（M5-5 的进程级滑动窗口限流器，三条闸的分工写在它的模块头）。
* **M9 面的表全部已在**：12 张 + 6 个 user 列（§6.4 逐条），**billing/subscriptions/stripe 面 0 张表**（§9.4）；唯一本地独有的列是 `"user".onboarding_state JSONB`（`migrations/compat/537_local_only_columns.up.sql:43`）。
* **local_only 9 与 M9 无关**（3 `/api/me/pats` + 4 健康/openapi + `/api/issues/:id/{reactions,quick-actions}` + 1 占位 `GET /api/feature-flags`）⇒ **M9 不新增 local_only**；门 ⑦ 的第二条命令（形态）本地**欠账为 0**（`docs/fixtures/slash-alias-allowlist.tsv` 当前为空）。

### 1.4 尾斜杠双形态：本波实测 **3 键**（本仓第二次非 0）

上游 chi 在「`r.Route(prefix, …)` + 子路由写 `"/"`」时**两种形态都服务**。M9 的 `/api/notification-preferences` 正是这个形状（`router.go:2400-2403`）⇒ 3 个方法各需要一个**无斜杠别名**：

```
$ python3 scripts/slash_alias_audit.py --declared docs/fixtures/m9-declared-routes.tsv
  declared 34 upstream key(s); dual-form required: 3 | single-form: 31
      DUAL   GET    /api/notification-preferences/   must register: /api/notification-preferences
      DUAL   PATCH  /api/notification-preferences/   must register: /api/notification-preferences
      DUAL   PUT    /api/notification-preferences/   must register: /api/notification-preferences
  MISSING_ALIAS (3): missing alias        # ⇒ 这是**预测**：只按上游字面量注册时会缺 3 个别名
  => 3 defect(s) from findings, 0 warning(s)      # exit 1
```

| 波 | 声明键 | `dual-form required` | 预测模式 exit | 退路 |
|---|---:|---:|---:|---|
| M4（`docs/42`） | 29 | 15 | 1（`FAIL: 15`） | 曾有 allowlist 6 行，M4-0 删毕 |
| M5（`docs/44`） | 29 | 7 | 1（`FAIL: 7`） | 曾有 allowlist 2 行，M5-0 删毕 |
| M6（`docs/57`） | 57 | 5 | 1（`FAIL: 5`） | M6-0 删毕 ⇒ 现在为空 |
| M7（`docs/60`） | 24 | 0 | 0 | 无欠账 |
| M8（`docs/61`） | 25 | 0 | 0 | 无欠账 |
| **M9（本文）** | **34** | **3** | **1（`FAIL: 3`）** | **`M9-5` 必须两形态一起注册**（无 allowlist 退路：门 ⑦ 会把残留的 allowlist 行当缺陷） |

⇒ M9 各片的形态纪律只有**两条**：① **`M9-5` 三个方法各注册两形态**（`/api/notification-preferences` 与 `/api/notification-preferences/`）；
② 其余 31 键**只按上游字面量注册那一形态**（补了另一种 = `EXTRA_ALIAS` 缺陷）。

### 1.5 授权面矩阵（4 种授权层 × 10 簇）+ **`RequireHumanActor` 是本波最大的新增安全面**

| 簇 | 会话 | workspace | 角色 | 机器凭据（`mat_` / `mcn_`） | 限流 | flag |
|---|---|---|---|---|---|---|
| A cloud-billing | 必须 | — | — | **403**（`RequireHumanActor`） | — | — |
| B cloud-subscriptions | 必须 | 必须 | 读 member / 写 owner\|admin | **403** | — | `billing_workspace_subscriptions`（写 5 条） |
| C stripe webhook | 无 | — | — | 无（无凭据） | **per-IP**（与 autopilot webhook 同一把） | cloud 未配置 ⇒ 403 |
| D contact-sales | 无 | — | — | 无 | 5/h（`RATE_LIMIT_CONTACT_SALES`） | — |
| E onboarding | 必须 | — | — | **允许**（user 级，非账户级） | — | — |
| F feedback | 必须 | — | — | 允许 | 10/h（handler 内） | — |
| G notification-preferences | 必须 | 必须 | member | 允许 | — | — |
| H dashboard | 必须 | 必须 | member | 允许 | — | — |
| I mika | 必须 | 必须 | member | 允许 | — | — |
| J issue timeline | 必须 | 必须 | member | 允许 | — | — |

**为什么 `RequireHumanActor` 是"最大的新增安全面"**：上游 `actor_guards.go:120` 的注释逐字写明理由 —— Auth 中间件把 **JWT cookie / `mul_` PAT / `mat_` 任务令牌 / `mcn_` 云节点 PAT** 四种凭据统一盖成 `X-User-ID`（机器凭据盖的是**属主的** user id），而**计费是账户级动作**：「一个跑着的 agent 读它属主的余额 / 开 checkout 会话，正是我们要防的横向移动」。
本仓**目前没有这条闸**（`grep -rn 'X-Actor-Source' crates/mc-http/src` 只有 4 处注释，**无实现**）⇒ `M9-0` 必须落这条中间件，`M9-1`/`M9-2` 的 15 条路由全部挂上，且**逐条**给「任务令牌 ⇒ 403」的反例用例。

### 1.6 出站代理面与三条公开路由

**16 条代理路由**（A 8 + B 7 + C 1）的共同事实（`cloud_billing.go` L16-36 设计注释逐字）：① 全部转发到 `MULTICA_CLOUD_URL`（`router.go:429`），与 `/api/cloud-runtime` **同一个云服务**（`:8080`），owner-credit 走 `proxyCloudRuntime`、workspace 面走更严的 `proxyCloudSubscription`；
② 转发时**注入 `X-User-ID`**，云侧仍是**最终授权方**（每次 mutation 前重新校验 membership）；③ Stripe webhook 是**唯一例外**（不注入 `X-User-ID`、**逐字**转发原始体）。

**C 的五段本地语义**（`cloud_billing.go:519-604`）：① cloud 未配置 ⇒ **403**；② per-IP 限流抢在读 body 前 ⇒ **429**（复用 autopilot webhook 同一把 limiter）；③ `Stripe-Signature` 缺失 ⇒ **401**（`Header.Values` 判**存在性** ⇒ 显式设成 `""` 也算缺失）；④ **签名校验在云侧**（本地不做 HMAC）；⑤ body 上限 1 MiB，**不 trim、不 JSON 校验**。

### 1.7 账的口径：**34 条 / 33 缺口 / 1 占位**

三条数，别混用（先例：`docs/61` §1.7 的 25/24）：

| 数 | 含义 | 来源 |
|---:|---|---|
| **34** | M9 名下**上游路由总数**（= `upstream-routes.tsv` 里 owner=M9 的行数）= **本表与 `m9-declared-routes.tsv` 的口径** | `awk -F'\t' '!/^#/ && $3=="M9"' docs/fixtures/upstream-routes.tsv \| wc -l` |
| **33** | 其中**尚未实现**的（⑦ 的 `owners.M9`）；`LUM-1814` 描述里写的「33 条」指这个 | `python3 scripts/route_parity.py` 的 `gaps by owner: … M9=33` |
| **1** | 已注册的占位（`GET /api/issues/{id}/timeline`，`issues/mod.rs:191`）—— 本波升级为真实现，**注册键不变** | `python3 scripts/route_parity.py --list-gaps` 的 `[M9]` 段不含它 |

### 1.8 已退役 / 不存在的路由：**本波有 2 条 DEPRECATED 活路由，0 条反向验收**

`runtime-bootstrap` / `no-runtime-bootstrap` 上游注释逐字标 **DEPRECATED**（桌面 < v3 的 rollout shim，`router.go:1620-1626`），
但它们**仍是活路由**（handler 在 `onboarding_shim.go`）⇒ **必须实现**，不做「必须 404」的反向验收。
`grep -n 'group-routes\|deprecated' docs/fixtures/upstream-routes.tsv` 无 M9 命中 ⇒ 本波不需要反向验收条目。

---

## 2. 目标架构与落点（含取舍）

### 2.1 分层落点

| 层 | 落点 | 内容 |
|---|---|---|
| 领域类型（跨 crate） | `crates/mc-core/src/{cloud.rs,onboarding.rs,notification.rs,dashboard.rs}`（anchor 建） | 订阅/计费响应形状、onboarding 档案与问卷答案、通知偏好分组词表、dashboard 行形状 |
| 出站传输（**唯一**） | **`crates/mc-cloud/`**（新 crate） | `transport.rs`（= `cloudruntime.Client`：`base_url` 可注入、`Headers` 直通、`Op` 计量标签）、`config.rs`、`error.rs`、`billing.rs`、`subscriptions.rs`、`webhook.rs` |
| 策略客户端 | **`crates/mc-entitlement/`**（新 crate） | `types.rs`（`Gate`/`Action`/`Reason`）、`cache.rs`（有界 + stale grace）、`client.rs`（单飞刷新 + 超时）、`stub.rs`（`entitlementtest` 等价物） |
| 组合根适配器 | `apps/mc-server/src/entitlement.rs`（anchor 建桩，M9-9 填） | 唯一实现 `mc_autopilot::quota::QuotaPolicyProvider` 的适配器 + `install_policy_provider` 调用 |
| PG 仓储 | `crates/mc-repos/src/{onboarding.rs,notification_preference.rs,feedback.rs,contact_sales.rs,dashboard.rs,timeline.rs,agent/mika.rs}` | 6 + 1 个新文件（各片自己的） |
| HTTP 面 | `crates/mc-http/src/routes/{cloud,dashboard,onboarding}/`（目录切片）+ 单文件切片 | 34 条路由，**一个子文件 = 一个写者** |
| 机器凭据闸 | `crates/mc-http/src/actor_guard.rs`（新文件，anchor 建） | `RequireHumanActor` 等价物（读 `X-Actor-Source`；**缺该 header 即视为人类**，逐字对齐上游） |

### 2.2 为什么是**两个**新 crate（三条判据 + 被否决的备选）

**判据 1：是否被 daemon 与 http 同时依赖？** `mc-cloud` **否**（只有 HTTP 宿主），但它是**三个路由簇的唯一传输** ⇒ 不落 `mc-core`（带 reqwest）。`mc-entitlement` **否**，但消费者是**领域 crate**（`mc-autopilot` 的 quota 接缝、`mc-http` 的 `limit-usage`）
⇒ 它必须**比 `mc-http` 更靠下**且**不能**挂在 `mc-autopilot` 上（否则 `mc-cloud`/`mc-http` 反向依赖领域）。`mc-onboarding`/`mc-feedback`/`mc-dashboard` **否**（0 daemon 消费者、0 跨 crate 消费者）。

**判据 2：是否需要独立 trait 抽象？** `mc-cloud` **不需要**（单实现；离线替身是**平台侧** HTTP 服务，不是 Rust trait），但仍有 255 行传输 + 一套错误映射 ⇒ 独立 crate 的收益是**三片共用一个 base URL**。`mc-entitlement` **需要**（`Provider` trait + `entitlementtest` 替身，且被 3 处消费）。另三个**不需要**（无 trait、无多实现、无替身）。

**判据 3：是否共用凭据面？** `mc-cloud` 与 `mc-entitlement` 共用**同一个 env**（`MULTICA_CLOUD_URL`）**但不同路径** ⇒ 共用件只有「一个 base URL 解析」，不足以合并（entitlement 有缓存/单飞/超时三组独立语义）。`contact_sales`/`feedback`/`dashboard`/`onboarding` **零凭据**（无密钥、无签名、无出站）⇒ 凭据面**不构成**建这三个 crate 的理由。

**被否决的备选（各写代价）**：

| 备选 | 否决理由 |
|---|---|
| 照 `plan1.md` §3.3 建 **4 个** crate | **上游没有对应包**（实测 `ls internal/` 无 `feedback|dashboard|onboarding|contact_sales`），三者的全部内容 = handler 文件 + 单表读写 + 只读聚合 SQL。**代价**：+3 个 `Cargo.toml`、+3 条依赖边、+3 个 crate 级 clippy 目标 ⇒ 编译时间上升而**替换收益为 0**（无第二种实现、无 daemon 消费者） |
| 把 `mc-entitlement` 并进 `mc-cloud` 或塞进 `mc-authz` | 前者的消费者是 `mc-autopilot`/`mc-http` 而非 `mc-cloud` ⇒ 并进去等于让 `mc-autopilot` 依赖整个云代理面（**依赖面倒挂**）；后者是**无 IO 的纯策略** crate（依赖只有 `serde`/`serde_json`/`thiserror`/`tracing`/`mc-core`/`mc-errors`，**无 reqwest**）⇒ 塞进网络客户端会把 reqwest 拖进每一个 authz 消费者（§9.8） |
| 建 `mc-onboarding` 承载 Mika + onboarding | Mika 的服务端身份常量与 agent 行生命周期属 `mc-agent` 面；onboarding 的 5 条是**两个文件 995 行**的 handler + 6 个 user 列 ⇒ 落 `routes/onboarding/` + `mc-repos/src/onboarding.rs`（§9.1） |
| 建 `mc-seatcapacity`（上游 `internal/seatcapacity` 909 行） | 它**不注册任何路由**（fixture 里 0 行），挂在**邀请/加入**面，与 M9 的 34 条零交集 ⇒ 本波**不动**，登记为**未登记的缺口**（R-M9-6） |

**依赖方向（无环，anchor 一次性接好）**：
`mc-cloud` → `mc-core` / `mc-errors` / `mc-telemetry`（+ `reqwest` / `serde` / `serde_json` / `url` / `thiserror` / `tracing` / `tokio`）；
`mc-entitlement` → `mc-core` / `mc-errors` / `mc-telemetry`（+ `reqwest` / `serde` / `tokio`；**不依赖 `mc-autopilot`**，适配器在 `apps/mc-server`）；
`mc-http` → `mc-cloud` + `mc-entitlement`（各一条 `path` 边）；`apps/mc-server` → `mc-entitlement`（+ 既有 `mc-autopilot`，用于装 provider）。

### 2.3 「**不得重复实现**」清单（三个已交付接缝，逐字路径）

| 已交付面 | 是什么（实测） | M9 的关系 |
|---|---|---|
| `crates/mc-autopilot/src/quota.rs`（`QuotaPolicyProvider` trait :133 + `NoEntitlementPlane` :143 + `install_policy_provider` :162） | M5-1 落下的**entitlement 接缝**；模块头逐字：「R7 的原话是『本仓无 entitlement 平面 ⇒ quota 关闭』…… **M9/云侧装自己的实现即可让同一批调用点变成按工作区下发策略**」 | **接缝归 M9 用，不重写**：`M9-9` 只写**适配器**（`apps/mc-server/src/entitlement.rs`）+ `mc-entitlement` 客户端；**禁止**改 `quota.rs` 的 trait 形状、禁止在 `mc-http` 里写第二份策略判定 |
| `crates/mc-chat/src/onboarding.rs`（语言白名单/开场白/kickoff 文案）+ `crates/mc-repos/src/chat_task/onboarding.rs`（`start_mika_onboarding` 同事务写两行）+ `crates/mc-http/src/routes/chat/task/dispatch.rs` | M4-4 的 **Mika 引路**（`POST /api/chat/sessions/{id}/onboarding`，fixture owner=`M4`） | **只读**：`M9-3` 的 onboarding 状态机**不得**改这三处；`M9-7` 的 `POST /api/agents/mika` 要**复用** `get-or-create` 的会话锁语义（上游 `LockWorkspaceForChatSessionCreate`）与既有 `mc-repos/src/chat_session.rs` |
| `crates/mc-autopilot/src/webhook/ratelimit.rs`（`SlidingWindowLimiter` + 三条闸） | M5-5 的**进程级滑动窗口限流器**；模块头写死「本仓没有 Redis ⇒ 内存实现就是等价物」 | **复用**：`M9-6` 的 stripe webhook 与 `M9-5` 的 contact-sales 限流**必须**用同一个 `SlidingWindowLimiter`；**禁止**新写第二份限流器 |
| `crates/mc-repos/src/runtime/usage.rs`（322 行，`task_usage_hourly` 读写） | M3 的 usage rollup 面 | **只读**：`M9-4` 的 dashboard 6 条 SQL **只读** `task_usage_hourly` / `agent_task_queue`（§9.6），不重算 rollup、不写 `task_usage_dashboard_*` |

### 2.4 凭据、部署密钥与 redaction（**只有一组密钥，但它是"开关"**）

| 部署密钥 | 用途 | 落点 | 缺失语义 |
|---|---|---|---|
| **`MULTICA_CLOUD_URL`** | **唯一**的云服务基址（cloud-runtime 代理 + billing + subscriptions + **entitlement 策略** + Stripe 转发） | `crates/mc-http/src/state.rs` 的 `cloud` 字段（anchor 建，**在 `AppState::new` 内读 env，不新增参数**）；`mc-cloud::config` 只做「合法绝对 URL」校验 | **空 / 非法 ⇒ 整体 disabled**：16 条代理路由 403（`cloud_runtime_not_configured`）、stripe webhook 403、entitlement 平面 = `NoEntitlementPlane`（quota 恒 `off`）。非法 URL ⇒ 500 `cloud_runtime_misconfigured`（与上游 `writeCloudRuntimeError` 逐条对齐，§2.6） |
| `STRIPE_WEBHOOK_SECRET` | **不在本地**：签名校验在云侧 | **本地不读这个 env**（这是有意的等价，不是缺口 —— 见 §9.5） | 本地只判「`Stripe-Signature` 头**存在**」⇒ 401 |
| `RATE_LIMIT_CONTACT_SALES` | contact-sales 限流（默认 5/h） | `M9-5` 的 `SlidingWindowLimiter` 构造处 | 缺省 → 用默认值（`envPositiveInt` 语义） |

**redaction 是硬约束（照 `docs/33` §12.2 先例，与 `docs/60` §2.3 / `docs/61` §2.4 同四条判据）**：① 承载 base URL / 出站响应体 / `Idempotency-Key` 的类型**手写 `Debug`**（URL 的 userinfo/query/fragment 一律在校验阶段拒绝，逐字对齐上游 `entitlement/client.go:57` 的 `ErrInvalidConfig`）；② 任何 `tracing::*` **不得**插值原始出站响应体；
③ 新增「错误路径**不回显**云侧响应体」用例（`mc-cloud` 的 `error.rs` 级 + `M9-1` 的 handler 级各一条）；④ `mc_telemetry::redact::Redactor::is_sensitive` 需覆盖 `cloud_url` / `stripe_signature` / `idempotency_key`（anchor 一次性补齐并给用例）。

> **与 M6/M7/M8 的差异**：本波**没有**「每连接密文」「App 私钥 PEM」这类高价值凭据面 —— 唯一的秘密是云侧服务基址，而它**不含凭据**（`X-User-ID` 是身份、云侧持有 Stripe/策略密钥）⇒ redaction 面比别的波小，**不是遗漏**。

### 2.5 「未配置 / 未授权 / flag 关」三层语义（M9 版 R-M7-3，逐端点不同）

| 端点族 | 未配置（`MULTICA_CLOUD_URL` 空） | 未授权 | flag 关 |
|---|---|---|---|
| A billing 8 条 | **403** `cloud_runtime_not_configured` | 401（无会话）/ **403**（机器凭据） | — |
| B subscriptions 写 5 条 | 403 | 403（非 owner\|admin）/ 401 | **403** `workspace_subscriptions_disabled` |
| B subscriptions 读 2 条 | 403 | 401 / 403（非 member） | — （读面不 gate，上游逐字「Summary and prices are member-readable」） |
| C stripe webhook | **403**（**先于**限流与签名检查） | 401（缺签名） | — |
| D contact-sales | **无未配置语义**（纯本地落库） | 400（校验失败）/ 429 | — |
| E onboarding 5 条 | 无（纯本地） | 401 | — |
| F feedback | 无（纯本地） | 401 / 429 | — |
| G notification-preferences | 无（纯本地） | 401 / 403（非 member） | — |
| H dashboard | 无（纯本地只读） | 401 / 403 | — |
| I mika | **无**（agent 行是本地数据；但 `runtime_id` 不合法 ⇒ 400） | 401 / 403 | — |
| J issue timeline | 无（纯本地只读） | 401 / 403 / **404**（非本 workspace 的 issue） | — |

⇒ 各片 DoD 必须**逐端点**写这三格，**不许统一返回 403/503**（先例：`docs/61` §2.5）。

### 2.6 出站代理的错误映射（离线替身的验收锚点）

逐字对齐上游 `writeCloudRuntimeError`（`cloud_runtime.go:196`）+ `writeFeatureDisabled`（`handler.go:578`）：

| 上游情形 | 本地状态码 | 本地 code |
|---|---:|---|
| 未配置 / disabled | **403** | `cloud_runtime_not_configured` |
| base URL 非法 | **500** | `cloud_runtime_misconfigured` |
| `context.DeadlineExceeded` | **504** | `cloud runtime request timed out` |
| 其他（连接拒绝 / 5xx / 体超限） | **502** | `cloud runtime request failed` |

**离线替身接缝（决定 CI 可判性）**：`base_url` 必须**可注入**（上游 `cloudruntime.Config.BaseURL` 就是构造参数）⇒ 替身是「本地 HTTP 服务端」，不是 Rust trait 替身。这也是 `M9-1`/`M9-2`/`M9-6` 端到端证据的**唯一**来源（§4.2）。

### 2.7 边界契约（写进各片 DoD，逐条可测）

1. **只有一个传输**：`mc-http` 的任何 handler **不得**直接 `reqwest`；出站只能经 `mc_cloud::transport::Client`。
2. **机器凭据一律 403**：挂了 `RequireHumanActor` 的 15 条路由，`mat_` / `mcn_` 请求必须 403 且**不产生出站请求**（反例用例逐条）。
3. **webhook 原始体逐字**：stripe 转发**不得** `trim` / `json` 校验 / 重编码；证据 = 替身收到的**字节级**比对。
4. **不做本地验签**（有意等价，§9.5）：只判 `Stripe-Signature` 头存在性；**禁止**读 `STRIPE_WEBHOOK_SECRET` / 自建 HMAC。
5. **不做本地事件去重**（有意等价，§9.5）：幂等由云侧事件 id 负责；本波的「幂等」验收只覆盖「同一请求重投 ⇒ 转发两次、本地不落任何状态」。
6. **两形态键**：`/api/notification-preferences` 与 `/api/notification-preferences/` **都注册**（§1.4）。
7. **不改上游已交付面**：`mc-autopilot/src/quota.rs` 的 trait 形状、`mc-chat/src/onboarding.rs`、`mc-repos/src/runtime/usage.rs` 只读（§2.3）。
8. **`localhost` 之外无硬编码**：`MULTICA_CLOUD_URL` 是唯一基址来源，**禁止**在代码里写 `api.stripe.com` / 云域名。

---

## 3. 写集与并发

### 3.1 共享锚点（**只在 M9-0 动一次**，其余片只读）

| 共享件 | 动作 |
|---|---|
| `Cargo.toml`（根 workspace） | `members` 是 `crates/*` glob ⇒ **members 行不动**；`[workspace.dependencies]` **必须**复核是否已有 `reqwest`（M7/M8 已引）⇒ 预期**最多 +0 条** |
| `Cargo.lock` | 只重新生成（**只有 anchor 能改**）：新增 2 个 workspace 成员 |
| `crates/mc-cloud/{Cargo.toml,src/lib.rs,src/config.rs,src/error.rs,src/transport.rs}` | **新建**；**transport 由 anchor 完整实现**（先例：M7-0 完整实现了 `mc-secrets/src/secretbox.rs`）—— 它是 3 个路由簇的唯一使能件 |
| `crates/mc-cloud/src/{billing.rs,subscriptions.rs,webhook.rs}` | 建**桩**（签名 + `todo!()` 位，各片原地填充） |
| `crates/mc-entitlement/{Cargo.toml,src/lib.rs,src/types.rs}` | 新建骨架 + **完整类型形状**（`Gate`/`Action`/`Reason`/`Policy`） |
| `crates/mc-entitlement/src/{client.rs,cache.rs,stub.rs}` | 建**桩**（`Provider` trait 位 + `todo!()`；实现归 M9-9） |
| `crates/mc-http/Cargo.toml` | 加 2 条 `path` 边（`mc-cloud` / `mc-entitlement`） |
| `apps/mc-server/Cargo.toml` | 加 1 条 `path` 边（`mc-entitlement`） |
| `crates/mc-http/src/routes/{mod.rs,mount.rs}` | +6 个 `pub mod` + `mount_slice_commercial()`（**anchor 期 6 个子 router 全空** ⇒ 注册键不变） |
| `crates/mc-http/src/state.rs`（+ 必要时 `src/state/cloud.rs`） | 加 `cloud` / `entitlement` 两组字段（**在 `AppState::new` 构造体内读 env**） |
| `crates/mc-http/src/routes/auth.rs` | 测试里唯一的 `AppState { … }` 字面量补字段 |
| `crates/mc-http/src/actor_guard.rs` | **新建**：`RequireHumanActor` 等价物 + `X-Actor-Source` 的解析口径用例 |
| `crates/mc-http/src/routes/issues/mod.rs` | **搬运**（不是删除）`/api/issues/:id/timeline` 那一行 501 占位 → `routes/timeline.rs`（handler 名仍是 `not_implemented`）⇒ **注册键集合逐字不变** |
| `crates/mc-core/src/{cloud.rs,onboarding.rs,notification.rs,dashboard.rs}` + `src/lib.rs` | 类型位 + 4 行 `pub mod` |
| `crates/mc-repos/src/{lib.rs,onboarding.rs,notification_preference.rs,feedback.rs,contact_sales.rs,dashboard.rs,timeline.rs,agent/mika.rs}` | `lib.rs` +7 行 + `agent/mod.rs` +1 行；**7 个新文件锚点只建桩**（签名 + `todo!()`） |
| `crates/mc-http/src/routes/{cloud,dashboard,onboarding}/mod.rs` + `{notification_preferences.rs,feedback.rs,contact_sales.rs,timeline.rs}` | 建（聚合各自子 router / 空 `Router::new()`） |
| `apps/mc-server/src/{main.rs,entitlement.rs}` | 新文件 + `main.rs` 一次调用（**anchor 期空跑**：无 `MULTICA_CLOUD_URL` ⇒ 不装平面） |
| `docs/32-M3-DAEMON-FACE.md` §9.**13** | 按本文 §9 + R-M9-1…7 追加（**anchor 一次落，后续片不回来改**；号段起手复核：当前最大 §9.11，M8-0 已预留 §9.12） |
| `docs/fixtures/route-parity-baseline.json` | **本 anchor 不动**（0 路由、0 占位删除、1 条占位**原地搬运**）⇒ 归 **M9-10（INT）** 刷新 |
| `docs/fixtures/slash-alias-allowlist.tsv` | **不动**（本波 3 个双形态键由 `M9-5` 直接两形态注册，**不进 allowlist**） |

### 3.2 与**相邻波（M7 / M8）**的交集（逐文件）

| 热点文件 | 谁要碰 | 结论 |
|---|---|---|
| `crates/mc-http/src/state.rs` | M7-0（渠道密钥）、M8-0（vcs/github/composio）、**M9-0（cloud/entitlement）** | **三个 anchor 不得同飞**（同文件、不同块）⇒ §7 的串行链 |
| `crates/mc-http/src/routes/{mod.rs,mount.rs}` | 同上三个 anchor | 同上 |
| `crates/mc-core/src/lib.rs` / `crates/mc-repos/src/lib.rs` | 同上三个 anchor | 同上 |
| `apps/mc-server/src/main.rs` | 同上三个 anchor | 同上 |
| `Cargo.lock` | 同上三个 anchor | **必须串行** |
| `crates/mc-repos/src/runtime/usage.rs` | **M3 已交付**（`task_usage_hourly`） | **只读**（§2.3） |
| `crates/mc-autopilot/src/quota.rs` | **M5-1 已交付**（entitlement 接缝） | **只读**：M9-9 只写 `apps/mc-server/src/entitlement.rs` |
| `crates/mc-http/src/routes/issue_table/mod.rs`（509 行） | **M2-D 已交付**；**M9-9 要改 `limit_usage`**（当前恒 204） | **跨波写者、单一写者**：M2-D 已合、无在飞片 ⇒ 安全；但**写集必须逐字声明**这一行区间 |
| `crates/mc-http/src/routes/issues/mod.rs` | M8-0 与 M9-0 各搬运 1 条占位（`pull-requests` / `timeline`） | **两个 anchor 不得同飞**（同上） |
| `docs/32-M3-DAEMON-FACE.md` | M7 各片 §9.x + M8-0 §9.12 + **M9-0 §9.13** | 排法上避开同轮（低危，纯文档） |
| `crates/mc-http/src/routes/webhooks/autopilots.rs` | **M5-5 已交付**（限流器调用点） | **只读**；M9-6 复用 `mc-autopilot::webhook::ratelimit` 的**库**，不碰这个路由文件 |

### 3.3 写集（**一格 = 一个本地文件 = 一个写者**；逐字路径，禁 glob / 花括号）

| 本地文件（逐字） | 唯一写者 | 读者 |
|---|---|---|
| `crates/mc-cloud/src/{lib.rs,config.rs,error.rs,transport.rs}` | M9-0 | M9-1/2/6 |
| `crates/mc-cloud/src/{billing.rs,subscriptions.rs,webhook.rs}` | M9-1 / M9-2 / M9-6（一人一个文件） | — |
| `crates/mc-entitlement/src/{lib.rs,types.rs}` | M9-0 | M9-9 |
| `crates/mc-entitlement/src/{client.rs,cache.rs,stub.rs}` | M9-9 | — |
| `crates/mc-core/src/{cloud.rs,onboarding.rs,notification.rs,dashboard.rs}` | M9-0 | 各片 |
| `crates/mc-core/src/lib.rs` / `crates/mc-repos/src/lib.rs` / `crates/mc-repos/src/agent/mod.rs` | M9-0 | 各片 |
| `crates/mc-repos/src/{onboarding.rs,notification_preference.rs,feedback.rs,contact_sales.rs,dashboard.rs,timeline.rs,agent/mika.rs}` | 7 个桩由 M9-0 建；填充：`onboarding.rs`=M9-3、`notification_preference.rs`/`feedback.rs`/`contact_sales.rs`=M9-5、`dashboard.rs`=M9-4、`agent/mika.rs`=M9-7、`timeline.rs`=M9-8 | — |
| `crates/mc-http/src/actor_guard.rs` | M9-0 | M9-1/2 |
| `crates/mc-http/src/routes/cloud/mod.rs` | M9-0 | — |
| `crates/mc-http/src/routes/cloud/{billing.rs,subscriptions.rs,webhook.rs}` | M9-1 / M9-2 / M9-6 | — |
| `crates/mc-http/src/routes/onboarding/{mod.rs,profile.rs,shim.rs,cloud_waitlist.rs}` | M9-3（`mod.rs` 由 M9-0 建空壳） | — |
| `crates/mc-http/src/routes/dashboard/{mod.rs,usage.rs,runtime.rs,failures.rs}` | M9-4（`mod.rs` 由 M9-0 建空壳） | — |
| `crates/mc-http/src/routes/{notification_preferences.rs,feedback.rs,contact_sales.rs}` | M9-5 | — |
| `crates/mc-http/src/routes/agents/mika.rs` | M9-7 | — |
| `crates/mc-http/src/routes/timeline.rs` | M9-8 | — |
| `crates/mc-http/src/routes/issue_table/mod.rs` | **M9-9**（**仅** `limit_usage` 一个函数体；其余行只读） | — |
| `apps/mc-server/src/entitlement.rs` | M9-9（桩由 M9-0 建） | — |
| `crates/mc-http/src/{state.rs,routes/mod.rs,routes/mount.rs,routes/auth.rs,routes/issues/mod.rs,Cargo.toml}` / `apps/mc-server/src/{main.rs,Cargo.toml}` / `Cargo.toml` / `Cargo.lock` | M9-0（此后**冻结**） | 各片只读 |
| `docs/32-M3-DAEMON-FACE.md` §9.13 / `docs/fixtures/route-parity-baseline.json` / `docs/fixtures/slash-alias-allowlist.tsv` | M9-0 / M9-10 / **不动** | §9.13 = 承接 §9 + R-M9-1…7；baseline 归 INT；allowlist 不进（§1.4） |
| `docs/32-M3-DAEMON-FACE.md` §9.13 | M9-0 | 各片只读 |
| `docs/62-M9-PLAN.md` / `docs/fixtures/m9-declared-routes.tsv` | **本片（`LUM-1814`）** | 各片只读 |

> **记法纪律（承接 `docs/57` §3.2 / `docs/60` §3.3 / `docs/61` §3.3）**：写集一律写**逐字路径**，禁 glob / 花括号 / 「某某段」。

### 3.4 同 stage 零交集矩阵（逐片核对）

| stage | 片 | 路由文件 | 仓储文件 | crate 文件 | 交集 |
|---|---|---:|---|---|---|
| 1 | M9-0 | 6 个空聚合 + 1 条搬运 | 7 个桩 | `mc-cloud` 四件 + `mc-entitlement` 三件 | — |
| 2 | M9-1 / M9-2 / M9-3 | `cloud/billing.rs` ∥ `cloud/subscriptions.rs` ∥ `onboarding/*` | — | `billing.rs` ∥ `subscriptions.rs` ∥ — | **∅** |
| 3 | M9-4 / M9-5 / M9-6 | `dashboard/*` ∥ 3 个单文件 ∥ `cloud/webhook.rs` | `dashboard.rs` ∥ 3 个单文件 ∥ — | — ∥ — ∥ `webhook.rs` | **∅** |
| 4 | M9-7 / M9-8 / M9-9 | `agents/mika.rs` ∥ `timeline.rs` ∥ `issue_table/mod.rs`（1 函数） | `agent/mika.rs` ∥ `timeline.rs` ∥ — | — ∥ — ∥ `mc-entitlement/src/*` + `apps/mc-server/src/entitlement.rs` | **∅** |
| 5 | M9-10 | — | — | — | 只读 + 快照 |

---

## 4. 切片表（派发用）

### 4.1 全景（**1 anchor + 8 代码片 + 1 INT = 10 个 issue**）

| # | 切片 | 路由 | 上游行数 | 上游组成 | stage | 硬前置 |
|---|---|---:|---:|---|---|---|
| M9-0 | anchor（骨架 + **出站传输** + 机器凭据闸 + 密钥端口 + 占位搬运） | 0 | 375 | `cloudruntime/client.go` 255 + `actor_guards.go` 120 | 1 | **M7 全合 + M8 全合**（§7.1） |
| M9-1 | cloud-billing 8 条（owner-credit 出站代理 + 机器凭据闸） | 8 | 185 | `cloud_billing.go` L1–L37 + L356–L503 | 2 | M9-0 |
| M9-2 | cloud-subscriptions 7 条（workspace 级代理 + 角色 + rollout flag + 幂等键） | 7 | 302 | `cloud_billing.go` L54–L355 | 2 | M9-0 |
| M9-3 | onboarding 5 条（档案/问卷/完成 + cloud waitlist + 2 条 DEPRECATED shim） | 5 | **995** | `onboarding.go` 372 + `onboarding_shim.go` 623 | 2 | M9-0 |
| M9-4 | dashboard 6 条（usage/runtime/failures 三个只读聚合 + 可见性折叠 + tz/days/project 口径） | 6 | 655 | `dashboard.go` | 3 | M9-0 |
| M9-5 | notification-preferences 3 条（**双形态键**）+ feedback 1 条 + contact-sales 1 条 | 5 | 672 | `notification_preference.go` 172 + `feedback.go` 177 + `contact_sales.go` 323 | 3 | M9-0 |
| M9-6 | stripe webhook 1 条（限流 + 缺签名 401 + 原始体转发） | 1 | 117 | `cloud_billing.go` L38–L52 + L504–L604 | 3 | M9-0 |
| M9-7 | mika 1 条（内置 agent 供给 + get-or-create onboarding 会话） | 1 | 328 | `mika_agent.go` | 4 | M9-0 |
| M9-8 | issue timeline 1 条（comments + activity_log 合并 + keyset + 截断语义） | 1 | 331 | `activity.go` L63–L393 | 4 | M9-0 |
| M9-9 | entitlement 平面接线（**0 路由**）+ 套餐/配额矩阵测试 | 0 | 751 | `internal/entitlement/*` 751（另加本地 `issue_table/mod.rs` 的 `limit_usage` 改造，不计入上游体量） | 4 | M9-0（+ 只读 `mc-autopilot/src/quota.rs`） |
| M9-10 | INT（集成、快照刷新与缺口登记） | 0 | — | — | 5 | 全波 |

**路由账**：8 + 7 + 5 + 6 + 5 + 1 + 1 + 1 + 0 = **34** ✓
**行数账**：375 + 185 + 302 + 995 + 655 + 672 + 117 + 328 + 331 + 751 = **4,711** ✓（与 §1.2 逐字相符）
**缺口账**：本波关掉 **33** 条 `known_gap`（8+7+5+6+5+1+1+1+0 = 34 条路由里，第 25 行是**占位升级**而非缺口）✓

> ★ **本波的最大单片是 `M9-3`（995 行）**，仍只有上限（3.5k）的 28%。原因同 M8：M9 的上游面**天然分成互不相关的十个簇**
> ⇒ 切片粒度按**子系统 × 授权层 × 生命周期**切（出站代理 vs 本地落库 vs 只读聚合 vs 策略客户端），不按行数凑。

### 4.2 离线替身方案（**每片端到端证据的承担者**）

`plan1.md` §5 W9 行的门禁逐字是「**套餐/配额矩阵测试**」。而 16 条代理路由**在 CI 无法真连云侧**、Stripe **无法真投递** ⇒ 判据必须**离线可复现**：**本地平台替身 + 真实 wire 帧 + 真库**。好消息：上游自己留好了接缝：

| 面 | 承担端到端证据的片 | 替身接缝（上游证据） | 替身纪律 |
|---|---|---|---|
| billing 8 条 | **M9-1** | `cloudruntime.Config.BaseURL` 是**构造参数**（`client.go:31`）⇒ 本地可注入 | 本地 HTTP 替身答 `/api/v1/billing/*`；断言链 = 路由 → 身份头（`X-User-ID`）→ 云侧路径 → 响应逐字透传 → 真库（0 行，纯代理） |
| subscriptions 7 条 | **M9-2** | 同上 + `proxyCloudSubscription` | 替身断言**注入体**（`workspace_id` 必须是中间件解析的那个，客户端不能走私）+ `Idempotency-Key` 长度上限（255 / 200 两档）+ flag 关 ⇒ 403 |
| stripe webhook | **M9-6** | 同上（`Request.Headers` 直通） | 替身断言**字节级**原始体 + `Stripe-Signature` + `Content-Type` 逐字；三段本地语义（403/429/401）各一条 |
| dashboard 6 条 | **M9-4** | **无需平台替身**（全部是本地只读聚合） | 证据 = 真库造行（`task_usage_hourly` + `agent_task_queue`）⇒ 6 个响应逐字段；tz 边界（`Asia/Shanghai` vs UTC）+ `days` 上限 365 + 两半 cutoff 口径（N+1 vs N）各一条 |
| onboarding 5 条 | **M9-3** | **无需平台替身** | 证据 = 真库（user 列）+ 状态机反例（重复 complete 幂等 / 问卷缺字段 400）+ shim 的 provision 链（helper agent + starter issue）**逐行断言** |
| notification-prefs / feedback / contact-sales | **M9-5** | **无需平台替身**；contact-sales 的"公开面"用**无会话**请求验 | 证据 = 真库 + 校验反例（非企业邮箱 / `company_size` 枚举外 / 分组键不合法 / 分组值不合法）+ 限流 429 各一条 + **双形态键各一条** |
| mika | **M9-7** | **无需平台替身** | 证据 = 真库 + 幂等反例（并发两次只建 1 个 agent + 1 个会话）+ 「客户端不能铸造 `system_key`」反例 |
| timeline | **M9-8** | **无需平台替身** | 证据 = 真库（comments + `activity_log` 两类行）+ keyset 边界 + **两侧独立截断**（上游注释逐字：不 clamp 到同一 floor）+ 响应头 `X-Timeline-Truncated` |
| entitlement | **M9-9** | **本地 stub 平面**（`entitlementtest.Stub` 等价物） | 证据 = 套餐/配额矩阵（`off`/`observe`/`enforce` × 2 个 gate × 缓存新鲜/陈旧/不可达）+ 「装平面后 `usage` 与 `limit-usage` 读同一份策略」断言 |

**替身三条纪律（与 `docs/60` §4.2 / `docs/61` §4.2 同款，写进上表各片 DoD）**：① **只替平台 wire，不替业务路径**（替身是「假云侧」）；② 帧/响应**逐字段**比对（出站断言替身收到的原始请求头与体）；③ **反例必测**（未配置 ⇒ 403 且**不发出站请求**；机器凭据 ⇒ 403 且**不发出站请求**）。

### 4.3 波次（并发 ≤3；stage 内三片可并行，上一 stage 未合不进下一 stage）

```
stage 1  M9-0
stage 2  M9-1 ∥ M9-2 ∥ M9-3
stage 3  M9-4 ∥ M9-5 ∥ M9-6
stage 4  M9-7 ∥ M9-8 ∥ M9-9
stage 5  M9-10
```

**串行链（必须写清，避免同 stage 内抢同一文件）**：

1. **`M9-0 → 全部`**：`state.rs` / `mount.rs` / 6 个聚合 `mod.rs` / 两个 crate 骨架 / `actor_guard.rs` / 7 个仓储桩全在 anchor。
2. **`M9-0 → M9-1/2/6`**：`mc-cloud/src/transport.rs` 是**同一份**（三片只读）；三片各写自己的 `mc-cloud/src/*.rs` 与 `routes/cloud/*.rs` ⇒ **stage 2/3 内零交集**。
3. **`M9-0 → M9-9`**：`mc-entitlement/src/{lib.rs,types.rs}` 由 anchor 定形；`client.rs`/`cache.rs`/`stub.rs` 归 M9-9。
4. **`M9-9` 是唯一改**「**别人已交付文件**」**的片**（`crates/mc-http/src/routes/issue_table/mod.rs` 的 `limit_usage`）⇒ 它的 stage 4 位置是有意的：
   等 M2-A 的尾账（`LUM-1691` / `LUM-1793`）**先落地**，避免与 M2 面同轮改同一文件。
5. **`M9-10` 是全波收口**（只有它碰 `route-parity-baseline.json`）。

---

## 5. M9-0 anchor：逐文件预扩展清单

| 文件 | 动作 | 关键点 |
|---|---|---|
| `crates/mc-cloud/{Cargo.toml,src/lib.rs,src/config.rs,src/error.rs,src/transport.rs}` | **新建并实现** | `Config{base_url,timeout,http_client,recorder}`、`Request{method,path,body,user_id,request_id,op,headers}`、`Client::{new,enabled,do}` + 四条错误映射（§2.6）。**这是 anchor 唯一"实现"的部分**（先例：M7-0 实现了 `secretbox.rs`）；依赖 `mc-core`/`mc-errors`/`mc-telemetry` + `reqwest`/`serde`/`serde_json`/`url`/`thiserror`/`tracing`/`tokio`，**零新外部包**（`reqwest` 已被 M7/M8 引入） |
| `crates/mc-cloud/src/{billing.rs,subscriptions.rs,webhook.rs}` | 建**桩** | 只放各面出站路径常量与响应类型位（各片原地填充） |
| `crates/mc-entitlement/{Cargo.toml,src/lib.rs,src/types.rs}` | 新建骨架 + **完整类型形状** | `Gate{IssueCount,AutopilotRuns}`、`Action{Off,Observe,Enforce}`、`Reason{…}`、`Policy`、`Provider` trait。**不动既有枚举** |
| `crates/mc-entitlement/src/{client.rs,cache.rs,stub.rs}` | 建**桩** | `todo!()` 位；`cache.rs` 放有界上限常量（10,000 / stale grace 15m / 失败重试 5s / TTL 上限 5m / 响应体 64KiB，逐字来自上游 `client.go:32-39`） |
| `crates/mc-http/src/actor_guard.rs` | **新建并实现** | `RequireHumanActor`：读 `X-Actor-Source`，`task_token` / `cloud_pat` ⇒ 403（`this endpoint is only available to human actors`）；**缺 header ⇒ 人类**（逐字对齐上游 `actor_guards.go`） |
| `crates/mc-http/src/state.rs`（或 `src/state/cloud.rs`） | 加 `cloud` / `entitlement` 两组字段 | **不新增 `AppState::new` 参数**；在构造体内读 `MULTICA_CLOUD_URL`；`CloudConfig` 手写 `Debug`（§2.4） |
| `crates/mc-http/src/routes/{mod.rs,mount.rs}` | +6 `pub mod` + `mount_slice_commercial()` + 一行 `.merge(...)` | **anchor 期 6 个子 router 全空 + 占位搬运 ⇒ 注册键逐字不变** |
| `crates/mc-http/src/routes/cloud/mod.rs` / `dashboard/mod.rs` / `onboarding/mod.rs` | 建（聚合各自子 router） | 子文件 anchor 期是**空 `Router::new()`** |
| `crates/mc-http/src/routes/{notification_preferences.rs,feedback.rs,contact_sales.rs,timeline.rs}` | 建 + 空 router；`timeline.rs` **承接搬运过来的 501 占位** | handler 名保持 `not_implemented`（已是 `pub(crate)`）⇒ ⑦ 的 `implemented_placeholder` **不变** |
| `crates/mc-http/src/routes/{agents/mika.rs,issue_table/mod.rs}` | **不建**（M9-7 / M9-9 自建/自改） | anchor **不碰** `issue_table/mod.rs`（避免与 M2 面抢文件） |
| `crates/mc-core/src/{cloud.rs,onboarding.rs,notification.rs,dashboard.rs}` | 新建（**完整类型形状**） | 订阅/计费 DTO 形状、onboarding 档案、通知偏好分组词表（7 组 + 2 值，逐字来自上游 `validNotifGroups`/`validNotifValues`）、dashboard 行形状 |
| `crates/mc-repos/src/{onboarding.rs,notification_preference.rs,feedback.rs,contact_sales.rs,dashboard.rs,timeline.rs,agent/mika.rs}` | 建**桩** | 每个文件只放签名 + `todo!()`；`lib.rs` +7 行、`agent/mod.rs` +1 行 |
| `apps/mc-server/src/entitlement.rs` | 新建 | `pub async fn start(...)`（**anchor 期空跑**：无 `MULTICA_CLOUD_URL` ⇒ 不装平面、返回 `None`） |
| `apps/mc-server/src/main.rs` | +`mod entitlement;` + 调用一行 | 装机点放在 `AppState::new` 之后、`scheduler::start` 之前 |
| `crates/mc-http/Cargo.toml` / `apps/mc-server/{Cargo.toml}` | 加 `path` 边 | 2 + 1 条 |
| `Cargo.lock` | 重新生成 | 2 个成员、0 条新外部包 |
| `docs/32` §9.13 | 追加偏离与口径修订 | 承接 §9 + R-M9-1…R-M9-7，**anchor 一次落** |
| `docs/fixtures/route-parity-baseline.json` | **不动** | 刷新归 M9-10 |

> 纪律（与 M5-0 / M6-0 / M7-0 / M8-0 相同）：anchor **不实现任何路由逻辑**（唯一例外是**出站传输**与**机器凭据闸** —— 二者是 3 个路由簇的使能件，没有它们任何一片都无法离线测试）；
anchor 的测试只有四类：编译（`cargo check --workspace --all-targets`）、门 ⑦/⑩ 读数、`Provider`/`Transport` 的**形状用例**（`Client::new` 对非法 URL 报错、`enabled()==false` 时不发出站）、**`actor_guard` 的 3 条口径用例**（无 header ⇒ 通过；`task_token` ⇒ 403；`cloud_pat` ⇒ 403）。

---

## 6. 门禁与验收

### 6.1 门 ⑦（路由对齐）

| 时点 | local | implemented | known_gap | owners.M9 | baseline | 备注 |
|---|---:|---:|---:|---:|---:|---|
| **`f73d916d`（本片起手，实测）** | **406** | **330**（326 real + 4 ph） | **126** | **33** | 406 | local_only 9 |
| M7 全波后（**预测**） | 430 | 354（350+4） | 102 | 33 | 406 | +24 |
| M8 全波后（**预测**） | **454** | **378**（375 real + 3 ph） | **78** | 33 | 406 | +24（含 1 条占位升级） |
| **M9-0 后** | **454** | **378** | **78** | **33** | **406（不动）** | anchor 0 路由、0 占位删除、**1 条占位原地搬运** ⇒ 本仓**第三个不刷基线的 anchor** |
| M9-1 后 | 462 | 386 | 70 | 25 | 406 | +8 |
| M9-2 后 | 469 | 393 | 63 | 18 | 406 | +7 |
| M9-3 后 | 474 | 398 | 58 | 13 | 406 | +5 |
| M9-4 后 | 480 | 404 | 52 | 7 | 406 | +6 |
| M9-5 后 | 485 | 409 | 47 | 2 | 406 | +5（含 3 个**双形态**键，注册键数按 axum 实际注册计） |
| M9-6 后 | 486 | 410 | 46 | 1 | 406 | +1 |
| M9-7 后 | 487 | 411 | 45 | **0** | 406 | +1 ⇒ **M9 的缺口清零** |
| M9-8 后 | 487 | **411**（real **409** + ph **2**） | 45 | 0 | 406 | 0 新注册；`timeline` 占位 → 真实现（real +1 / ph −1） |
| M9-9 后 | 487 | 411（409+2） | 45 | 0 | 406 | 0 路由 |
| **M9-10（INT）后** | 487 | 411 | 45 | 0 | **487** | `--write-baseline` 406 → 487 |

不变式（每片自检）：`implemented + known_gap == 456`、`regressions == 0`、`unclaimed == 0`、`local_only == 9`（M9 不新增 local_only）。
**只有 M9-1/2/3/4/5/6/7 七片会动读数**；M9-0、M9-8（除占位计数的 real/ph 拆分）、M9-9、M9-10（除基线）必须**逐字不变**。

> ⚠️ 上表 M7/M8 两行是**预测**（它们的片还没落地）。M9 各片起手**必须重取当轮 base sha 与实测读数**，不许直接抄本表（`docs/37` §46 的 lesson：口径类片合入的瞬间，所有引用旧口径的表都变成过期文书）。

### 6.2 门 ⑨（契约等价）——M9 相关 fixture 现状与目标

```
$ cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json
totals: fixtures 365 · pass 5 · mismatch 23 · unmounted 31 · unevaluable 306
```

按「路径落在 M9 的 34 条路由上」过滤（复算见 §10 命令 4）：**共 20 条**，分布 `unevaluable 17 / unmounted 3`：

| 组 | 条数 | 现状 | 期望 | 由哪片转绿 |
|---|---:|---|---|---|
| `POST /api/webhooks/stripe`（3 条，actor=**anonymous**） | 3 | `unmounted`（本地根本没挂这条路由 ⇒ 404） | **401**（缺签名）/ **403**（cloud 未配置）/ **429**（限流） | **M9-6** |
| `cloud_billing` / `cloud_subscriptions` / `dashboard` / `feedback`（17 条，actor=**member**） | 17 | `unevaluable` | **结构上无法在 `--no-db` 层判定**（stateless 层只认 anonymous，见既有 lesson）⇒ 真证据必须 `--db-url` 跑 | 各片在自己 DoD 里给**真库读数** |

⇒ 本波**承诺 3 条 `unmounted → pass`**（唯一在无库模式下可判的一簇），
**不承诺**提高 `contract_equivalence_rate`（当前 0.0137），也**不制造**新的 `unevaluable` 条目。
17 条 member fixture 的**逐条点名**（片 → fixture id）写在各片 DoD；M9-10 给 `--db-url` 全量跑后的逐条结论。

**取而代之的验收判据**：本波每片**自造离线 fixture** 落到 `contracts/golden/`（不计入上游 365 的 pass 率，避免污染契约等价率），
覆盖五类**离线可判**语义：① 未配置/未授权/flag 关三层矩阵（§2.5，逐端点）；② 机器凭据 403 反例（15 条路由）；
③ 出站替身的字节级直通（billing/subscriptions/stripe）；④ 只读聚合的口径边界（tz / days / 两半 cutoff / 可见性折叠）；
⑤ 状态机与幂等（onboarding 重复 complete、mika 并发 get-or-create、timeline 两侧独立截断）。

### 6.3 门 ⑩（文件大小）——预飞

门 ⑩ 只扫 `git ls-files` 的代码文件（`docs/**` 不查），规则「只减不增」，清单外硬限 **800 行**。M9 写集里**没有任何文件**在 `scripts/file_size_baseline.tsv`（该表 10 个条目全是 M1-M5 的存量）⇒ **全部走 800 行硬限**。预飞：

| 本地文件 | 当前 | 计划 | 风险与对策 |
|---|---:|---|---|
| `crates/mc-http/src/state.rs` | 530 | ≈600（M9-0 加 cloud/entitlement 两组） | **中危**（R-M9-3）：M7-0 与 M8-0 可能已各加一块 ⇒ 起手先 `wc -l`；若 >620，M9-0 把「读 env + 构造」下放到 `crates/mc-http/src/state/cloud.rs`（新文件，anchor 建） |
| `crates/mc-http/src/routes/mount.rs` | 341 | ≈380 | 安全 |
| `crates/mc-http/src/routes/mod.rs` | 89 | ≈101 | 安全 |
| `crates/mc-http/src/routes/issues/mod.rs` | 253 | 252（**减 1 行**：占位搬运出去） | 安全（方向是**减少**） |
| `crates/mc-http/src/routes/issue_table/mod.rs` | **509** | ≈520（M9-9 只改 `limit_usage` 函数体） | 安全，但它是 **M2-D 的文件** ⇒ M9-9 的写集必须钉在**函数级** |
| `apps/mc-server/src/main.rs` | 213 | ≈235 | 安全 |
| `crates/mc-cloud/src/transport.rs` | — | 255（上游 1:1） | 安全 |
| `crates/mc-cloud/src/subscriptions.rs` | — | 200–350 | 安全 |
| `crates/mc-entitlement/src/client.rs` | — | 350–450（上游 422） | 安全（若超 450 拆 `refresh.rs`） |
| `crates/mc-repos/src/dashboard.rs` | — | 300–450（6 条 SQL） | 安全 |
| `crates/mc-http/src/routes/dashboard/{usage,runtime,failures}.rs` | — | 各 200–350（上游 1 个文件 655 行 ⇒ **拆 3 个**） | 安全 |
| `crates/mc-http/src/routes/onboarding/shim.rs` | — | 400–650（上游 623 行） | **中危**：若超 700，按「provision 链 / 文案常量」再拆 `shim_content.rs` |
| `crates/mc-repos/src/onboarding.rs` | — | 150–250 | 安全 |

⇒ **三片必须在实现前先拆分**：M9-3（`shim.rs`）、M9-4（`dashboard/{usage,runtime,failures}.rs`）、M9-6（`cloud/webhook.rs` 与 `mc-cloud/src/webhook.rs` 分工）。拆分是**回归上游结构**（上游 `cloud_billing.go` 一个文件里三种授权层），不是凑门。

### 6.4 迁移条数 = **0**

M9 面**12 张表 + 6 个 user 列全部已在** `migrations/upstream/`（复算见 §10 命令 5）：

| 族 | 对象 | 张/列数 | 迁移 |
|---|---|---:|---|
| 通知偏好 | `notification_preference` | 1 | `064` |
| 反馈 / 销售 | `feedback`、`contact_sales_inquiry` | 2 | `057`、`098` |
| 用量 rollup | `task_usage_hourly`（101）、`task_usage_dashboard_daily` + `task_usage_dashboard_rollup_state`（084，**legacy**）、`client_usage_daily`（207）、`runtime_usage`（013） | 5 | `013/084/101/207` |
| 任务 / 活动 | `agent_task_queue`、`activity_log` | 2 | `001` |
| user onboarding 列 | `onboarded_at`（050）、`onboarding_questionnaire`（051/094）、`cloud_waitlist_email`/`cloud_waitlist_reason`（052）、`starter_content_state`（054）、`onboarding_runtime_choice`（098） | 6 列 | `050/051/052/054/094/098` |
| 席位容量（**非 M9 路由面**） | `seat_capacity_outbox` + 索引族 | 1 + 4 索引 | `415`–`419` |
| 计费 / 订阅 / Stripe | **无表** | **0** | —（§9.4） |

⇒ 本波**不写任何 `migrations/**`**，也不刷 `contracts/upstream-apply-exceptions.tsv`。门 ⑧（schema-drift）在 INT 片跑一次即可。

### 6.5 每片 DoD（通用 + 专属）

**通用（每片都跑，命令见 §10 命令 6）**

1. `bash scripts/gates.sh` **8/8 绿**；触碰 DB 的片（M9-1…M9-9 全部）追加 `--with-db` **10/10**。
2. ⑦ 读数与 §6.1 该片行一致（`implemented + known_gap == 456`、`regressions == 0`、`local_only == 9`）。
3. 形态门：本片不得引入 `MISSING_ALIAS` / `MISSING_EXACT` / `EXTRA_ALIAS`（`M9-5` 的 3 个双形态键是**必须补**的，补完才算绿）。
4. ⑩：新文件 ≤800 行；`scripts/file_size_baseline.tsv` **不动**（M9 写集不在名单内）。
5. 每条路由至少一条测试（handler 级或 e2e），且**不用** `health::placeholder`。
6. 出站面（M9-1/2/6）：`base_url` 可注入的**离线替身**端到端 + 字节级/字段级比对 + 「未配置 ⇒ 403 且不发出站」反例；凭据面：`Debug` 脱敏 + 「错误路径不回显云侧响应体」用例（§2.4）。
7. 偏离（无 Redis 的单副本假设 / 未接线项 / 替身范围）必须写进 `docs/32` §9.13 的**自己那一段**（编号起手复核）。

**专属**

| 片 | 专属验收 |
|---|---|
| M9-0 | ⑦ 读数**逐字不变**（`454/378/78`，baseline 406 不动）；`cargo metadata` 通过且 2 个 crate 是成员；`timeline` 占位**搬运后注册键不变**；`actor_guard` 三态用例全绿；`Client::new` 对非法 URL（含 userinfo / query / fragment）报错且 `enabled()==false` 时不发出站；`AppState` 缺 `MULTICA_CLOUD_URL` 不 panic 且 `Deb` 输出不含 URL 凭据 |
| M9-1 | 8 条路由的三层语义（403/401/403-机器凭据）；**15 条**机器凭据反例的其中 8 条；`checkout-sessions/{sessionId}` 的路径参数透传；**离线替身端到端**（§4.2） |
| M9-2 | 7 条路由：读 2 条 member 可读、写 5 条非 owner\|admin ⇒ 403；flag 关 ⇒ 403 `workspace_subscriptions_disabled`；`workspace_id` 由中间件注入（客户端走私 ⇒ 被覆盖）；`Idempotency-Key` 255/200 两档上限；座位购买的 `expected_current_seats` / `expected_purchase_version` 透传 |
| M9-3 | 5 条路由；**问卷状态机**（`complete` 幂等、缺 `role`/`use_case` 的 `400`）；`cloud-waitlist` 与 `052` 的两列**对齐断言**（写入后直读列）；2 条 DEPRECATED shim 的 provision 链逐行断言（helper agent + starter issue）；**不改** `mc-chat/src/onboarding.rs`（§2.3 的禁改清单） |
| M9-4 | 6 条路由；**只读 `task_usage_hourly` / `agent_task_queue`**（断言不 `SELECT` 任何 `task_usage_dashboard_*`）；tz 口径（`Asia/Shanghai` vs UTC 各一条）；`days` 默认 30 / 上限 365 / 非法值 400；两半 cutoff（N+1 vs N）各一条；`foldRestrictedAgents` 的私有 agent 折叠（`__restricted_agents__` 哨兵 + 合并不丢总额） |
| M9-5 | 5 条路由；**3 个双形态键**（`/api/notification-preferences` 与 `/api/notification-preferences/` 各一条）；分组词表 7 组 × 2 值的正反例；feedback 的 `has_images` 标记与 10/h 限流 429；contact-sales 的**无会话**可访问 + 企业邮箱域名拒绝 + `company_size` 枚举 + 5/h 限流 429；**复用** M5-5 的 `SlidingWindowLimiter`（不新写） |
| M9-6 | **⑨ 那 3 条 `unmounted → pass`**（401/403/429）；三段本地语义的**顺序**（403 先于 429 先于 401）；1 MiB 体上限 ⇒ 413；原始体**字节级**直通（替身比对）；`Header.Values` 语义（显式 `""` 也算缺失）；`X-User-ID` **不注入** |
| M9-7 | 1 条路由；并发 get-or-create（2 个并发请求 ⇒ 1 agent + 1 会话）；`kind`/`system_key` **不可由客户端铸造**（多传字段被忽略）；`language` 白名单外的值 ⇒ 400；`post` 幂等（同 workspace 第二次调用返回既有 agent） |
| M9-8 | 1 条路由；comments + `activity_log` 合并的**顺序与去重**；keyset（`before`/`after`/`around`/`limit`）四参边界；**两侧独立截断不 clamp**（上游注释逐字）+ `X-Timeline-Truncated` 响应头；非本 workspace 的 issue ⇒ 404；**不碰** `GetAssigneeFrequency`（`M2-A` 的账） |
| M9-9 | **套餐/配额矩阵**：3 个 `Action` × 2 个 `Gate` × 3 个缓存态（新鲜/陈旧/不可达）逐格；装平面后 `GET /api/autopilots/usage` 与 `GET /api/issues/limit-usage` **读同一份策略**（断言不出现第二份判定）；`limit_usage` 从 204 → 有 usage 的行；**不改** `mc-autopilot/src/quota.rs` 的 trait 形状（§2.3） |
| M9-10 | 快照三件套刷新（⑦/⑨/⑩）+ `--write-baseline`（406 → 487）+ 缺口登记；**无新功能代码**；登记项至少含：cloud-runtime 11 条的 owner 迁移建议（§9.2）、`internal/seatcapacity` 909 行的未登记缺口（R-M9-6）、`activity_log` 写入面覆盖率（§9.7）、`mc-dashboard` 等三 crate 不建的结论（§9.3）、`task_usage_dashboard_*` legacy 表的存留口径（§9.6） |

---

## 7. 晋升顺序与前置（含**三波槽位分配**）

### 7.1 硬前置链

1. **M7 全合**：`LUM-1786`（M7-21 INT）落地 —— 理由与 `docs/61` §7.1 相同（共享受护文件 + 基线刷新归 INT）。
2. **M8 全合**：`LUM-1804`（M8-7 INT）落地 —— **M9-0 与 M8-0 争同一批共享文件**
   （`state.rs` / `routes/{mod,mount}.rs` / `mc-core/src/lib.rs` / `mc-repos/src/lib.rs` / `routes/issues/mod.rs`（两条占位搬运）/
   `routes/auth.rs` / `apps/mc-server/src/main.rs` / `Cargo.lock`，§3.2 逐文件）。
3. **`M9-0` 是全部代码片的前置**，**`M9-1/2/6` 还需要 `mc-cloud/src/transport.rs`**（在 anchor 内）。
4. **`M9-9` 需要 M2-A 的尾账先落地**（`LUM-1691` / `LUM-1793`：它们是 `issue_table` 面的邻居，且都还没跑）⇒ M9-9 排在 stage 4。
5. **`M9-10` 是全波收口**。

### 7.2 单值结论：**M7 → M8 → M9 串行起跑；M9 起跑后与 M10 尾账并行**

**依据四条**：① **DAG 成立**——`plan1.md` §5 的 gantt 逐字写 `W9 商业面 :w9, after w8, 4` ⇒ W9 以 W8 为硬前置，与 W7 无依赖但**共享 3 个锚点**（§3.2）；
② **产物依赖为零**（M7 写 `mc-channel*`/`routes/channels`、M8 写 `mc-vcs*`/`mc-composio`、M9 写 `mc-cloud`/`mc-entitlement`/`routes/{cloud,dashboard,onboarding}`）⇒ **唯一交集是共享文件的一次性接线** ⇒ 必须串行 `M9-0`、代码片可并行；
③ **槽位预算**：M9 10 个 issue，3 槽下界 `ceil(10/3)=4` 轮（本文排 5 个 stage），而 M7 剩 22 片（保 2 槽）、M8 剩 8 片（保 1 槽）；
④ **基线交接**：`plan1.md` §6.3 的纪律（口径类片合入即让引用旧口径的表过期）⇒ **每个 INT 各刷一次基线**，M9 的 INT 是**第三次**（M6-INT 406 → M7-INT → M8-INT → **M9-INT 487**）⇒ 三波按 INT 顺序交接。

**槽位分配（单值）**：

| 轮 | M7（保 2 槽） | M8（保 1 槽，空槽时 M9 借） | M9（本波） | 说明 |
|---|---|---|---|---|
| R1–R11 | M7-0…M7-21 | — | — | M9-0 被 M8-0 的共享文件前置拦住 |
| R12 | — | M8-0…M8-6（+ M8-7 INT） | — | 同理 |
| **R13** | — | — | **M9-0** | 三波唯一必须串行的接线点 |
| **R14** | ——（M7 已完结） | ——（M8 已完结） | **M9-1 ∥ M9-2 ∥ M9-3** | M9 回满 3 槽 |
| **R15** | — | — | **M9-4 ∥ M9-5 ∥ M9-6** | — |
| **R16** | — | — | **M9-7 ∥ M9-8 ∥ M9-9** | M9-9 的 M2-A 前置在此时应已满足（否则降为 2 槽） |
| **R17** | — | — | **M9-10（INT）** | 收口 |

> ⚠️ 上表是**基线排法**，不是硬约束：真实派发以「谁有空位谁起」为准。
> **唯一硬规则**：`M9-0` 与 `M7-0`/`M8-0` **不得同飞**（同一批共享文件 + `Cargo.lock`）；`M9-9` 与 M2-A 的尾账**不得同飞**（同一文件）。

### 7.3 子 issue 一览（全部 `backlog`，`--parent LUM-1814`，stage 与 §4.1 一一对应）

| # | 切片 | 子 issue | stage | 路由 | 上游行数 |
|---|---|---|---:|---:|---:|
| 1 | M9-0 anchor | **`LUM-1815`**（`01a0d569-b078-7512-9b67-c4d4644941b3`） | 1 | 0 | 375 |
| 2 | M9-1 cloud-billing | **`LUM-1816`**（`01a0d569-b0cc-77e0-a25e-9c647db7fbbf`） | 2 | 8 | 185 |
| 3 | M9-2 cloud-subscriptions | **`LUM-1817`**（`01a0d569-b11f-7e65-a70e-1b5c7499b118`） | 2 | 7 | 302 |
| 4 | M9-3 onboarding | **`LUM-1818`**（`01a0d569-b169-7442-9867-63137fab0d43`） | 2 | 5 | 995 |
| 5 | M9-4 dashboard | **`LUM-1819`**（`01a0d569-ca02-7952-93eb-c6086e45524f`） | 3 | 6 | 655 |
| 6 | M9-5 notification-preferences + feedback + contact-sales | **`LUM-1820`**（`01a0d569-ca4b-76c0-b810-daca4017e0f6`） | 3 | 5 | 672 |
| 7 | M9-6 stripe webhook | **`LUM-1821`**（`01a0d569-ca98-76b0-8edb-f116f9f54a30`） | 3 | 1 | 117 |
| 8 | M9-7 mika | **`LUM-1822`**（`01a0d569-cae8-7b3b-9d63-73c48129b02e`） | 4 | 1 | 328 |
| 9 | M9-8 issue timeline | **`LUM-1823`**（`01a0d569-cb2e-775b-95be-358718f610d8`） | 4 | 1 | 331 |
| 10 | M9-9 entitlement 接线 | **`LUM-1824`**（`01a0d569-cb78-753d-8699-01ad7db51168`） | 4 | 0 | 751 |
| 11 | M9-10 INT | **`LUM-1825`**（`01a0d569-cbc3-72c0-b55f-78a586a8ea4c`） | 5 | 0 | — |

（路由账 8+7+5+6+5+1+1+1+0 = 34 ✓；stage 分布 1/3/3/3/1 = 11）

> 晋升规则与 M5/M6/M7/M8 相同：`backlog → todo` 才起跑；同 stage 内三片可并行；上一 stage 未合不进下一 stage。
> ⚠️ **派发提示**：本波 11 条**全部无 assignee** ⇒ 每次晋升都必须 `assign --to-id 3c6087f9-f768-45a0-9b07-979f7d4fabf5`
> （照 `docs/60` §7.4 的教训），并 `--no-start` 记录（避免与 status 变更重复起 run）。

---

## 8. 风险登记（每条对应一个 DoD 或一个「登记不实现」的决定）

| ID | 风险 | 缓解 / 决定 |
|---|---|---|
| **R-M9-1** | **`MULTICA_CLOUD_URL` 在 CI 不可达** ⇒ 16 条代理路由 + 1 条 webhook 转发**只能拿到 403/502**，「套餐/配额矩阵测试」门禁可能被"假绿"绕过 | §4.2 的替身三条纪律 + 上游**自带的接缝**（`BaseURL` 是构造参数）；三片 DoD 逐条点名承担者；**未配置 ⇒ 403 且不发出站**作为硬反例 |
| **R-M9-2** | **`RequireHumanActor` 等价物在本仓不存在**（实测只有注释提到 `X-Actor-Source`）⇒ 15 条账户级路由会**默认可被任务令牌调用**（横向移动面） | `M9-0` 落 `actor_guard.rs` + 三态用例；`M9-1`/`M9-2` 全部挂载 + 逐条 403 反例；**没有这条闸就不许合并 M9-1/2** |
| **R-M9-3** | **`state.rs` 第二次被 anchor 追加**（530 行，M7-0/M8-0 可能已各加一块）⇒ 逼近 ⑩ 硬限 800 | §6.3 预飞：起手 `wc -l`；>620 则把「读 env + 构造」下放到 `crates/mc-http/src/state/cloud.rs`（先例：R-M8-7） |
| **R-M9-4** | **timeline 的 activity 半边几乎无写入面**：本地 `activity_log` **只有 1 个写者**（`crates/mc-repos/src/agent/env.rs:80`），上游也只有 3 处 `CreateActivity`（`agent_env.go:156/244`、`squad.go:1102`）⇒ 真实现上线后**多数 issue 上 activity 半边是空的** | 不是 M9 的缺口而是**既有面的覆盖率事实**：`M9-8` 的 DoD 用**真库造行**断言合并/keyset/截断三个语义（不依赖别的波次补写者）；覆盖率缺口由 M9-10 **登记**（含 file:line 清单） |
| **R-M9-5** | **`M9-5` 的 3 个双形态键是"必须补"，而门 ⑦ 对 `EXTRA_ALIAS` 是硬失败** ⇒ 补多补少都红 | §1.4 的实测表（`declared 34 / dual-form required: 3`）是**唯一判据**；`M9-5` 起手跑 `--declared` 预测模式、收尾跑无参模式（本地实况 0 defect） |
| **R-M9-6** | **`internal/seatcapacity`（909 行）是未登记的缺口**：本地**零实现**，而它挂在邀请/加入面，不进任何波次的路由账 | **本波不动**（0 条路由、与 34 条零交集）⇒ 由 M9-10 **登记**为「无主缺口」（先例：`docs/61` §9.2 的附件面 9 条）。**不塞进 M9 切片** |
| **R-M9-7** | **`mc-cloud` 只有 1 个宿主（http）但被 3 片同时写** ⇒ 传输的改动会同时影响 3 片 | `transport.rs` 的**唯一写者是 anchor**，此后冻结；三片只写自己的 `mc-cloud/src/*.rs`；任何传输改动回到 M9-10 登记（§3.1） |
| **R-M9-8** | **`docs/32` §9.13 是 M7/M8/M9 三个 anchor 的唯一共享文档写点** | 排法上避开同轮（§7.2 的 R13）；若真撞上，后合者 rebase 并保留两段（纯文档，无编译风险）。先例：R-M8-10 |
| **R-M9-9** | **`limit_usage` 从 204 变为有值**是一次**行为变更**（客户端可能依赖 204） | `M9-9` 的 DoD 要求：`MULTICA_CLOUD_URL` 缺省时**仍返回 204**（等价于上游「gate 关掉」分支）⇒ 变更只发生在**装了平面**的部署上；矩阵必覆盖「未装 ⇒ 204」 |

---

## 9. 与 `docs/plan1.md` / `docs/15-M3-PLAN.md` / `docs/01-PLAN.md` 的差异（口径修订，逐条）

### 9.1 口径修订一：§5 W9 行括号里的数是**路由条数**；`docs/01` §6 的 Mika/activity log 归属**维持 M9**

* 原文（`plan1.md` §5）：`W9 商业面 | cloud-runtime(11) billing(8) subscriptions(7) entitlement + onboarding + feedback + dashboard(6) | internal/entitlement 等 | 4 周`。
* 实测：三条括号数与 fixture **逐字相符**，单位是**上游路由条数**（§1.2 的三条判据）。**无数据修订，只有单位钉定** +
  **新增上游体量实数**：M9 的 34 条路由对应上游 **4,711 行**（handler 3,705，含 `actor_guards.go` 120 与 `activity.go` 的 M9 段 331 + 包 1,006）。
* `docs/01-PLAN.md:213` 逐字把 **M9** 定义为「Activity / Onboarding / Cloud …… activity log + onboarding + **Mika** + cloud billing + waitlist + feedback + contact sales + instance telemetry」
  ⇒ **`/api/issues/{id}/timeline`（activity log）与 `POST /api/agents/mika` 的 M9 归属成立**（三处一致：`docs/01` §6 + fixture owner 单元格 + `scripts/route-owners.tsv:37/38`）。
* **`POST /api/agents/mika` 的裁定（本文第 1 条必须回答的问题）**：语义上是「内置 agent 供给 + onboarding 会话入口」—— 既不是商业面（**零** cloud transport / entitlement / Stripe / 凭据依赖，实测 `mika_agent.go` 的 import 只有 `pgx`/`service`/`protocol`/`db`/`logger`/`metrics`），也不是「第三方集成面」（它 `POST` 的是**本工作区自己的** agent 行）；但它**确由 M9 的 onboarding 故事触达**（handler 返回值里就带着 `onboarding_session`，`mika_agent.go:61-71`），且 `docs/01:92` 早已把「Onboarding / Starter content / Mika agent」划成一组。
  ⇒ **裁定：账与实现都留在 M9（`M9-7`），不迁移 owner 单元格**（迁移无收益：这条路由**一旦实现就不再是任何 owner 的缺口**，`owners.*` 直方图只统计 `known_gap`；改单元格却会动 ⑦ 读数，超出 docs-only 片的硬约束）。
  **"不许两边都挂"的落实**：`M9-7` 是这条路由的**唯一**实现者；`mc-agent` 面（W3）**不得**再立 mika 片；**不建 `mc-mika` crate**（`docs/01:92` 的 `mc-mika` 收敛掉，理由同 §9.3）。

### 9.2 口径修订二：`/api/cloud-runtime` 11 条 —— **crate 归属 = `mc-cloud`；波次账目 = 仍在 M3**

* 事实三条：(a) `docs/15` §9.1（LUM-1357）**已裁决** cloud-runtime 11 条判给 **W9/M9** 并逐字登记「登记给 M9 立项时处理」（`docs/36` §125 / `docs/37` §6976 / `docs/49` §138 三处重复）；(b) `upstream-routes.tsv` 与 `route-owners.tsv` 的 owner 单元格**至今仍是 `M3`**；(c) 它们与 `mc-cloud` 的其余三个面**共用同一个出站传输**（`cloud_billing.go:16-20` 逐字：「Fleet and Billing share `:8080`」）。
* **裁定（两条，分别回答 crate 与波次）**：① **crate 归属 = 是** —— `mc-cloud` 的 scope **包含** cloud-runtime 面（`crates/mc-cloud/src/runtime.rs`，`transport.rs` 复用）；上游 `internal/cloudruntime/client.go`（255 行）本来就是**两者的同一份客户端**，拆成两处会立刻出现第二份 base-URL 解析与错误映射。
  ② **波次账目 = 否** —— 本片**不改 owner 单元格**（硬约束：交付前后 ⑦ 逐字不变，同 `docs/61` §9.2 对附件面的处理），§4.1 的切片表**不含**它们。
* **归属迁移建议（登记，不在本片执行）**：
  ```
  # 迁移动作（15 行改动，两个文件）
  scripts/route-owners.tsv:21   ^/api/cloud-runtime  M3 → M9      # 现文：「远程 runtime 节点池（执行后端，非计费）」
  docs/fixtures/upstream-routes.tsv  11 行（$3 由 M3 → M9）
  # 预期 ⑦ 影响（键集合与 baseline 均不变，只有 owner 直方图变）
  owners.M3  11 → 0      owners.M9  33 → 44      local/implemented/known_gap/baseline/local_only/regression/unclaimed 全部不变
  ```
  **执行点 = `M9-10`（INT）**：理由三条 —— (i) 它是本波唯一有权刷 ⑦ 与登记口径的片（先例：M8-7）；
  (ii) 届时若 M3 的 cloud-runtime 尾账仍未派发，由 INT 一并把 owner 迁过来，正是「crate 已收编 ⇒ 账目归位」的收口动作；
  (iii) 若 M3 尾账**已派发/在飞**，则 INT **只登记不迁移**（避免与在飞片抢同一批行）。
* **跨波写者与串行关系（若将来由 M3 尾账实现）**：该片与 `M9-0` **不得同飞** —— 它们会同时写 `crates/mc-http/src/state.rs`、
  `routes/{mod,mount}.rs`、`crates/mc-cloud/src/{lib.rs,transport.rs}` 与 `Cargo.lock`。**安全顺序**：`M9-0` 先落 `mc-cloud` 骨架与传输 ⇒ 尾账片**只读**传输、只填 `routes/cloud_runtime.rs` 与 `mc-cloud/src/runtime.rs`。

### 9.3 口径修订三：`plan1.md` §3.3 的 W9 **4 个 crate → 实测 2 个**

* 原文：`W9 | mc-cloud(runtime/billing/subscriptions/entitlement) mc-onboarding mc-feedback mc-dashboard | 商业面`。
* 实测：**不建** `mc-onboarding` / `mc-feedback` / `mc-dashboard`（判据与代价见 §2.2 的备选表）；**改设 `mc-entitlement` 为独立 crate**（原文把 entitlement 塞在 `mc-cloud` 里，实测它有两个跨 crate 消费者 ⇒ 塞进去会让 `mc-autopilot` 反向依赖整个云代理面）。
* 净变化：**4 → 2**（`mc-cloud` + `mc-entitlement`），且 `mc-cloud` 的 scope **增加** cloud-runtime（§9.2）。
* 附带：`docs/01-PLAN.md:92` 的 `mc-mika` 也**不建**（§9.1）。

### 9.4 口径修订四：billing / subscriptions / stripe **本地 0 张表** ⇒ 纯出站代理（不是"表缺失"）

* 事实（双侧实测）：本地 `migrations/upstream/` 里**没有** `cloud_billing_*` / `cloud_subscription*` / `checkout_session` / `topup` / `stripe_*`（⇒ 0），**上游侧同样为 0**。
* ⇒ **裁定：不是"本地表缺失"，而是上游把计费状态放在 `multica-cloud` 自己的库里**（`cloud_billing.go:16` 逐字：「proxy to the same multica-cloud HTTP service」；`summary` 的注释逐字：「Cloud serves this from its own database **without calling Stripe**」）⇒ 本地**不需要**新迁移，**需要**的是**离线替身**（§4.2）。
* 逐条迁移清单（回答"哪几片需要新迁移"）：**全部 8 个代码片 = 0 新迁移**；唯一涉及列语义的是 `M9-3`（读写 `050/051/052/054/094/098` 的 6 个**已存在**的 user 列）。
* `plan1.md` §1.5 的 **114 张 head 最终集口径复核**：本波**不新增表** ⇒ 该口径**不受影响**（`§1.5` 的「138 = 137 + `schema_migrations`，其中 24 张被 DROP ⇒ head 114」是按上游迁移集算的，M9 不改它）。

### 9.5 口径修订五：`POST /api/webhooks/stripe` 的**验签在云侧**、**幂等也在云侧**（本地只有两件事）

* 逐字实测（`cloud_billing.go:519-604`）：本地**只**做 ① per-IP 限流（429）② `Stripe-Signature` 头存在性（401）③ 1 MiB 体上限 + **原始体逐字转发**；注释三处点名：签名校验在云侧、`X-User-ID` **不注入**、body **不许**任何转换。
* ⇒ **裁定**：本地**不实现** HMAC 验签、**不读取** `STRIPE_WEBHOOK_SECRET`、**不做**事件 id 去重（幂等由云侧事件表负责）—— 这是**有意的等价**，登记为「不做」（先例：`docs/61` R-M8-4 的「不做时间戳窗口」），**不是缺口**。
* 承担端到端证据的片 = **`M9-6`**（§4.2）；⑨ 那 3 条 anonymous fixture 就是它的判据（§6.2）。
* 限流用**哪一条闸**：上游复用 `h.WebhookIPRateLimiter`（`handler.go:492-494` 三条装配之一）；
  本地对应物 = `crates/mc-autopilot/src/webhook/ratelimit.rs` 的三条闸（绝对 IP 600/60s / 坏凭据 IP 30/60s / trigger 60/60s）
  ⇒ **`M9-6` 起手必须逐字复核是哪一条**（上游字段名与本地三条闸不是一一对应），并写进自己的 DoD。**不许**新写限流器。

### 9.6 口径修订六：`/api/dashboard/*` 6 条**只读** `task_usage_hourly` + `agent_task_queue`，**不读** `task_usage_dashboard_*`

* 事实（逐条实测 6 个 SQL 的 `FROM`/`JOIN`，见 §10 命令 3）：
  `usage/daily` 与 `usage/by-agent` ⇒ `FROM task_usage_hourly`；`agent-runtime`/`runtime/daily`/`failures/daily`/`failures/by-agent` ⇒ `FROM agent_task_queue atq JOIN agent a … LEFT JOIN issue i …`。
  **6 条里 0 条读 `task_usage_dashboard_daily`** —— 那两张表（`084_task_usage_dashboard_rollup.up.sql`）的 rollup 管道在 `101`/`103` 的 hourly 化里被**取代**了（`103_drop_legacy_daily_rollups.up.sql` 逐字「drop legacy daily rollups」）。
* ⇒ **裁定：只读既有 aggregate，不自建聚合**。逐条给表与口径：

| 路由 | 表 | 口径要点 |
|---|---|---|
| `usage/daily` | `task_usage_hourly` | `SUM` 四类 token + `cost_usd_ticks` + `task_count`，按 `DATE(bucket_hour AT TIME ZONE tz)` + `LOWER(provider)` + `model` 分组；`uncosted_*` 用 `COALESCE(uncosted_x, x)` |
| `usage/by-agent` | `task_usage_hourly` | 同上，分组换成 `agent_id` |
| `agent-runtime` | `agent_task_queue` + `agent` + `issue` | `SUM(EXTRACT(EPOCH FROM (completed_at - started_at)))` + task 计数 + 「是否计过费」（`EXISTS (SELECT 1 FROM task_usage …)`） |
| `runtime/daily` | 同上 | 按 `DATE(completed_at AT TIME ZONE tz)` 分组 |
| `failures/daily` / `failures/by-agent` | 同上 | 按 `failure_reason` 计数（含"从未 started"的任务） |

* **可复用的 M3 已交付面（禁止重复实现）**：`crates/mc-repos/src/runtime/usage.rs`（322 行，`task_usage_hourly` 的读写）、`crates/mc-repos/src/task/*`（`agent_task_queue` 的既有访问）、`crates/mc-http/src/routes/runtimes/usage.rs`（既有 per-runtime usage 口径 —— `dashboard` 的 `provider`/`model` 维度**故意**与它一致）。
* **可见性折叠**：3 条 per-agent 路由必须实现 `foldRestrictedAgents`（私有 agent 折叠到哨兵 `__restricted_agents__` 且**合并不丢总额**）—— 上游注释逐字「client-side filtering is decoration: one curl bypasses it」⇒ 本地**必须在服务端折叠**。
* **`client_usage_daily` / `runtime_usage` 不在这 6 条里**（前者属 `POST /api/client-usage` = `M3+`，后者属 `/api/runtimes/{id}/usage*` = M3 已交付）⇒ **M9-4 不碰它们**。

### 9.7 口径修订七：`user.onboarding_state` **不存在**（实测），存在的是 6 个别的列

* 原文（`LUM-1814` 描述 §3）：本地有「`user.onboarding_state`（`050/051/052/053/094/098`、`057`、`064`、`415`–`419`）」。
* 实测（逐迁移核对）：`onboarding_state` **不是任何上游迁移建的列**；本地**确实有**这一列，但它是**本地独有列**（`migrations/compat/537_local_only_columns.up.sql:43`，同文件注释逐字：「`"user".email_verified_at` / `onboarding_state`：**上游未建模**（本地引导流程读它们）」）。
  上游的真实列是：`onboarded_at`（`050`）、`onboarding_questionnaire`（`051` + `094` 的 v2 回填）、`cloud_waitlist_email`/`cloud_waitlist_reason`（`052`）、`starter_content_state`（`054` + `095` 回填）、`onboarding_runtime_choice`（`098`）；`053` 是 **DROP** 掉废弃的 `onboarding_current_step`。
* ⇒ **裁定**：`M9-3` 的写集**以 6 个上游列为准**（`onboarding_questionnaire` 是问卷的唯一载体）；`"user".onboarding_state`（本地独有）**只读不改**（起手已实测：`crates/mc-repos/src/user.rs:104/118/150` 在读它）。
* **`cloud-waitlist` 与 `052` 的对齐断言方式**：`M9-3` 的用例必须**写入后直读两列**（不经 API 回显），即 `SELECT cloud_waitlist_email, cloud_waitlist_reason FROM "user" WHERE id = $1` 与请求体逐字比对 —— 因为 `cloud_waitlist_*` 是**唯一**的上游载体，回显路径存在「API 自己拼出来」的假绿风险。
* **与既有 onboarding 实现的交集（逐字路径，回答原文 §7）**：

| 交集文件（逐字） | 既有内容 | M9 的关系 |
|---|---|---|
| `crates/mc-chat/src/onboarding.rs` | M4-4：Mika 语言白名单 / 开场白 / kickoff / `QuestionnaireAnswers` | **只读** |
| `crates/mc-repos/src/chat_task/onboarding.rs` | M4-4：`start_mika_onboarding` 同事务写 kickoff + opening | **只读**；`M9-7` 复用其「取或建会话」语义 |
| `crates/mc-http/src/routes/chat/task/dispatch.rs` | M4-4：kickoff 派发 | **只读** |
| `crates/mc-repos/src/user.rs` | 既有 user 列读写（含 `onboarding_state` 本地列） | **只读**；新查询写 `crates/mc-repos/src/onboarding.rs` |
| `crates/mc-ws/src/hub/user_face.rs` / `crates/mc-ws/src/frames/user_events.rs` | onboarding 相关 WS 事件 | **只读**（本波不发新事件） |

⇒ 明令：**只补 HTTP 面与状态机缺口，不重写既有 rev/状态机**。

### 9.8 口径修订八：entitlement 的落点是**独立 crate + 组合根适配器**（不落 `mc-authz`、不并进 `mc-cloud`）

* 事实三条：(a) 上游 `internal/entitlement` = 5 文件 / 751 行（`client.go` 422 + `cache.go` 113 + `types.go` 128 + `doc.go` 10 + `entitlementtest/stub.go` 78）；
  (b) 它被 **4 处**消费（`handler.go:26` 装到 `h.Entitlements`/`TaskService`/`IssueService`/`AutopilotService`；`service/{autopilot,issue,task}.go` 与 `handler/issue_limit.go` 读策略）；
  (c) **本仓已有一个为此准备的接缝**：`crates/mc-autopilot/src/quota.rs` 的 `QuotaPolicyProvider` + `install_policy_provider`，模块头逐字写着「M9/云侧装自己的实现即可……」。
* ⇒ **裁定：建 `mc-entitlement` crate**（类型 + 有界缓存 + 单飞 + `Provider` + stub），**适配器落在组合根** `apps/mc-server/src/entitlement.rs`（它实现 `mc_autopilot::quota::QuotaPolicyProvider` 并调 `install_policy_provider`）。
  两条判据：① **独立 trait 抽象成立**（`Provider` + stub 替身，且有三个不同消费点）；② **不落 `mc-authz`**（该 crate 是**无 IO 的纯策略** crate：`Cargo.toml` 的依赖只有 `serde`/`serde_json`/`thiserror`/`tracing`/`mc-core`/`mc-errors`，**无 reqwest** —— 塞进网络客户端会破坏这个不变量）；
  ③ **不并进 `mc-cloud`**（会让 `mc-autopilot` 反向依赖整个云代理面）。
* **「套餐/配额矩阵测试」的离线可复现判据**（回答原文 §8 的要求）：`M9-9` 必须用**本地 stub 平面**（`mc-entitlement/src/stub.rs`）
  跑出 **3 个 `Action` × 2 个 `Gate` × 3 个缓存态 = 18 格**矩阵，并断言 ③「装平面后 `GET /api/autopilots/usage` 与
  `GET /api/issues/limit-usage` 读**同一份**策略」（同一格不得出现两个不同结论）；
  另加一格「未装平面 ⇒ `limit-usage` 仍 204、`usage` 仍 `{"action":"off"}`」（回归保护，R-M9-9）。

### 9.9 与 M7/M8 计划的结构一致性

承接 `docs/60` / `docs/61` 的骨架（§0 速览 / §1 测绘 / §2 架构 / §3 写集 / §4 切片 / §5 anchor / §6 门禁 / §7 晋升 / §8 风险 / §9 差异 / §10 复算），
并沿用四条纪律：**写集逐字路径**、**`unevaluable` 不许改写成 `pass`**、**落笔前 `git fetch` 复核号段**、**base sha 起手重取**。
**与 M6/M7/M8 的四处结构差异**：

1. **第一个"多 crate 收敛"的波**（`plan1` 的 4 → 2，§9.3）——M6 是 4 → 4、M8 是 3 → 3+1 不建。
2. **第一个含"必须补形态别名"的波**（3 键，`FAIL: 3`），前两波（M7/M8）都是 0（§1.4）。
3. **第一个含"跨波改别人已交付文件"的波**（`M9-9` 改 `routes/issue_table/mod.rs` 的 `limit_usage`，§3.2）⇒ stage 位置是被前置约束推定的。
4. **stage 更浅但更宽**（5 个 stage、三片 ×3 轮）⇒ 起跑后 3 轮即可清空 M9 的缺口（§7.2）。

---

## 10. 复算命令（全部只读，可在任意 workdir 复现）

```bash
# 前置：本片的上游只读副本（钉住 commit f41fae6b08fb；上游 main tip 是 90e0bdf，必须显式取该 sha）
#   UP_ROOT=<本 run 的 workdir>；上游 = $UP_ROOT/ups/up-m9，本地 = $UP_ROOT/paperclip-rs
#   纪律：副本只克隆进**本 run 的 workdir**，不依赖 /tmp 或别的 run 的 workdir（会被 GC 掉）
cd <workdir> && mkdir -p ups && cd ups && git clone --depth 1 --single-branch --branch main https://github.com/louloulin/multica up-m9
cd up-m9 && git fetch --depth 1 origin f41fae6b08fb734afcbd13205c0b3203dd0bc9c6 && git checkout -q FETCH_HEAD && git log --oneline -1   # ⇒ f41fae6
cd ../../paperclip-rs && git fetch origin feat/multica-rs-initial && git log --oneline -1   # ⇒ f73d916d

# 1. 路由表（§1.1/§1.7）；diff 与 M9 行数、gaps、cloud-runtime 的 M3 残留（§9.2）
diff <(awk -F'\t' '!/^#/ && $3=="M9"{print $1"\t"$2}' docs/fixtures/upstream-routes.tsv | sort) \
     <(grep -v '^#' docs/fixtures/m9-declared-routes.tsv | tail -n +2 | sort)                  # ⇒ 无输出
awk -F'\t' '!/^#/ && $3=="M9"' docs/fixtures/upstream-routes.tsv | wc -l                          # ⇒ 34
python3 scripts/route_parity.py --list-gaps | awk '/^    \[M9\]/,/^    \[M7\]/' | head -3       # ⇒ [M9] 33
awk -F'\t' '!/^#/ && $3=="M3" && $2 ~ /^\/api\/cloud-runtime/' docs/fixtures/upstream-routes.tsv | wc -l   # ⇒ 11

# 2. 上游文件与行数（§1.2）
UP=<clone>/server
wc -l $UP/internal/handler/{mika_agent,cloud_billing,contact_sales,dashboard,feedback,activity,onboarding,onboarding_shim,notification_preference,actor_guards}.go
# ⇒ 328 604 323 655 177 461 372 623 172 120
wc -l $UP/internal/cloudruntime/client.go                                                       # ⇒ 255
find $UP/internal/entitlement -name '*.go' ! -name '*_test.go' -exec wc -l {} + | tail -1       # ⇒ 751
# cloud_billing.go 的三个切点：L169 = subscriptions 首函数、L356 = billing 首函数、L520 = stripe 首函数
grep -n '^func (h \*Handler) \(GetCloudWorkspaceSubscriptionSummary\|GetCloudBillingBalance\|HandleCloudBillingStripeWebhook\)' $UP/internal/handler/cloud_billing.go
# activity.go 的 M9 段边界（L63–L393；L394 起是 M2-A 的 GetAssigneeFrequency）
grep -n '^func ' $UP/internal/handler/activity.go | sed -n '3p;4p;$p'
ls -d $UP/internal/{feedback,dashboard,onboarding,contact_sales,billing,subscription}* 2>&1 | head -3   # ⇒ No such file（§2.2 第一条硬证据）

# 3. dashboard 6 条的真实数据源（§9.6；应全部是 task_usage_hourly 或 agent_task_queue）
for q in ListDashboardUsageDaily ListDashboardUsageByAgent ListDashboardAgentRunTime ListDashboardRunTimeDaily ListDashboardFailuresDaily ListDashboardFailuresByAgent; do
  f=$(grep -rln "name: $q " $UP/pkg/db/queries/*.sql); printf "%-34s %s\n" "$q" "$(awk "/name: $q /,/;\$/" $f | grep -oE 'FROM [a-z_]+' | tr '\n' ' ')"
done
grep -rn 'task_usage_dashboard_daily' $UP/pkg/db/queries/*.sql | wc -l                          # ⇒ 0
grep -rln 'task_usage_dashboard' $UP/migrations/*.up.sql                                        # ⇒ 084（legacy）+ 103（drop）

# 4. ⑨ M9 相关 fixture（§6.2；应 20 条 = unevaluable 17 + unmounted 3）
python3 - <<'PY'
import json, collections
d = json.load(open('crates/mc-conformance/report.json'))['fixtures']
decl = [l.rstrip('\n').split('\t') for l in open('docs/fixtures/upstream-routes.tsv') if l.strip() and not l.startswith('#')]
keys = [(r[0], [s for s in r[1].strip().rstrip('/').split('/')]) for r in decl if r[2]=='M9']
ps = lambda p: [s for s in p.strip().rstrip('/').split('/')]
hit = lambda m, p: any(m == dm and len(ds) == len(ps(p)) and all(a.startswith('{') or a.startswith(':') or a == b for a, b in zip(ds, ps(p))) for dm, ds in keys)
rows = [f for f in d if hit(f['method'], f['path'])]
print(len(rows), dict(collections.Counter(f['outcome'] for f in rows)))
print([(f['method'], f['path'], f['status_expected']) for f in rows if f['outcome']=='unmounted'])
PY

# 5. M9 面的表与列是否都在（§6.4；每行应输出 1）
for t in notification_preference feedback contact_sales_inquiry task_usage_hourly task_usage_dashboard_daily \
         task_usage_dashboard_rollup_state client_usage_daily runtime_usage activity_log; do
  printf "%-40s %s\n" "$t" "$(grep -rliE "CREATE TABLE (IF NOT EXISTS )?\"?$t\"?[ (]" migrations/upstream/*.up.sql | wc -l)"; done
grep -rliE 'CREATE TABLE (IF NOT EXISTS )?"?(cloud_billing|cloud_subscription|checkout_session|topup|stripe_event)' migrations/upstream/*.up.sql | wc -l   # ⇒ 0（§9.4）
ls migrations/upstream/{050_add_onboarded_at_to_users,051_add_onboarding_state_to_users,052_add_cloud_waitlist_to_users,054_add_starter_content_state_to_users,094_onboarding_questionnaire_v2,098_user_onboarding_runtime_choice}.up.sql
ls migrations/upstream/*.up.sql | wc -l                                                          # ⇒ 560（上游也是 560）
grep -rn 'onboarding_state' migrations/compat/537_local_only_columns.up.sql                      # ⇒ 本地独有列（§9.7）

# 6. 形态门（预测模式：`declared 34 / dual-form required: 3`，**exit 1** 是预测不是缺陷，§1.4）
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m9-declared-routes.tsv
python3 scripts/slash_alias_audit.py                    # 本地实况：起手时 0 defect

# 7. 门 ⑩ 预飞（§6.3）：M9 写集应无人被列入白名单 + 关键文件当前行数
grep -E 'mc-cloud|mc-entitlement|routes/(cloud|dashboard|onboarding)|notification_preferences|feedback|contact_sales|dashboard\.rs|timeline' scripts/file_size_baseline.tsv
wc -l crates/mc-http/src/state.rs crates/mc-http/src/routes/{mount.rs,mod.rs,issues/mod.rs,issue_table/mod.rs} apps/mc-server/src/main.rs

# 8. 已交付接缝核对（§2.3）与「本仓没有 RequireHumanActor」（M9-0 要新建，R-M9-2）
grep -n 'install_policy_provider\|pub trait QuotaPolicyProvider' crates/mc-autopilot/src/quota.rs
grep -n 'start_mika_onboarding' crates/mc-repos/src/chat_task/onboarding.rs
grep -n 'pub struct SlidingWindowLimiter' crates/mc-autopilot/src/webhook/ratelimit.rs
grep -rn 'func.*RequireHumanActor\|X-Actor-Source' crates/mc-http/src | wc -l                      # ⇒ 起手时只有注释命中

# 9. 全量门禁（每片交付前；M9 各片全部触碰 DB ⇒ 一律带 --with-db）
bash scripts/gates.sh --with-db
```

---

## 11. M9-INT 落地记录（占位，由 M9-10 填写）

（待 M9 收口后由 `M9-10` 填写：⑦/⑨/⑩ 快照刷新读数、baseline 406→487 的实测、本波实际未落地的项（至少含 §9.2 的 cloud-runtime 归属迁移结论、R-M9-6 的 `seatcapacity` 909 行未登记缺口、R-M9-4 的 `activity_log` 写入面覆盖率、§9.3 的三个 crate 不建结论、§9.6 的 legacy 表存留口径）、以及 20 条 fixture 在 `--db-url` 全量跑后的逐条结论。）