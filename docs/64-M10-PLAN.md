# docs/64 — M10（W10 收尾/终波）切片计划：ops 探针（`/health*`、`/health/realtime`）+ `/api/config` + 性能 bench + 双跑对账 + 发布制品 + **停止条件操作化**（A 面 5 条 + 尾账 B 面 17 行）

> **本片（`LUM-2099`）的性质**：docs + fixture + 建 issue 片。**不改任何 `.rs`**、不动 `migrations/**`、不动 `Cargo.lock`、**禁跑 `--write-baseline`**（本波基线刷新归 M10-INT；唯一例外见 §9.1）。
> **写集**：`docs/64-M10-PLAN.md` + `docs/fixtures/m10-declared-routes.tsv`（逐字，就这两条）。其余全部只读。
> **起手 base**：`c23fcfad`（本次实测，`git rev-parse origin/feat/multica-rs-initial`，与立项描述逐字相符；`LUM-1785` 尚未落地、GH 0 open PR）。
> **上游只读副本**：`f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`（= `docs/fixtures/upstream-routes.tsv` 记的 commit，本轮克隆进**本 run 的 workdir**）。⑨ 的 fixture pin 是**另一个** commit（`90e0bdf830436b3981b32a7017e1c18d41c7cdea`，见 §1.6）。

---

## 0. 结论速览

1. **本波不是一个「5 条路由的普通波」**。`plan1.md` §5 给 W10 的内容是「前端兼容验证、性能 bench、双跑对账、发布（helm/镜像/CLI 分发）」，路由面只占其中一格。⇒ 裁定 **本波的切片划分依据是「交付物」而不是「路由」**（全仓唯一一个非路由驱动的波），并给出**例外纪律**：路由面 5 条 + 本仓最后一块尾账（`M3+` 17 行）作为 **A/B 两面**并列进本波（§9.1）。
2. **这 5 条不插进 M9 波**（不提前到 M9-0 之前/之中）：判据四条在 §9.2。⇒ `M10` 面与 M9 **零共享文件**，可完全并行。
3. 🔴 **⑨ 义务**：本波一动，就是本仓**单波最大的一次快照位移** —— `pass 14 → 32`（+18，M7-14 的 2.5 倍）、`unmounted 22 → 4`、`contract_equivalence_rate 0.038 → 0.088`。**`crates/mc-conformance/report.json` 的刷新归 M10-INT（`M10-9`）**，不归任何一个代码片。
4. 🔴 **停止条件已操作化**（§2.6 + §9.9）：**Tier-1 硬向量 12 条 + 一键复算 `scripts/stop_condition.sh` + 当轮差额清单**。当前差额（本次实测）：`known_gap 65`（`M9 33 / M3+ 16 / M3 11 / M10 5`）、`implemented 388 real + 3 ph`、`baseline 458`、`local_only 9`、⑨ `pass 14 / mismatch 23 / unmounted 22 / unevaluable 306`。
5. **两份尾账都有主**：`M3+`（16 缺口 + 1 占位 = 17 行）⇒ **本波 B 面 4 片**（§4.3）；`M3`（11 行，全是 `/api/cloud-runtime/*`）⇒ **M9 尾账片 `M9-11`**（`--parent` = M9 计划 `LUM-1814`，stage 2，硬前置 `M9-0`；本片**同时建 issue**，见 §5.2 的理由与 §7.3）。
6. **规模**：`1 anchor + 11 代码片/交付片 + 1 INT = 13 个 M10 issue`（+1 个 M9 尾账 issue），分 6 个 stage，并发 ≤3。

---

## 1. 上游面测绘（`f41fae6b08fb` + ⑨ 的 `90e0bdf`，本轮实测）

### 1.1 本波路由账（A 面 5 条，逐字）

| # | METHOD | PATH | 上游 `server/cmd/server/router.go` | owner 规则（`scripts/route-owners.tsv`） | 授权面 |
|---:|---|---|---:|---|---|
| 1 | GET | `/health` | `1399` `r.Get("/health", health.liveHandler)` | `^/(health\|healthz\|readyz)$` → M10「ops 探针」 | 公开（**无** `/api` 前缀、无会话） |
| 2 | GET | `/healthz` | `1401` `r.Get("/healthz", health.readyHandler)` | 同上 | 公开 |
| 3 | GET | `/readyz` | `1400` `r.Get("/readyz", health.readyHandler)` | 同上 | 公开 |
| 4 | GET | `/health/realtime` | `1412` `r.Get("/health/realtime", realtimeMetricsHandler(os.Getenv("REALTIME_METRICS_TOKEN")))` | `^/health/realtime$` → M10 | 公开**但自带上限**：有 token ⇒ `Bearer`；无 token ⇒ 仅 loopback（否则 **404**） |
| 5 | GET | `/api/config` | `1478` `r.Get("/api/config", h.GetConfig)` | `^/api/config$` → M10 | 公开（上游注释逐字：web 应用**登录前**就要读它，决定是否渲染 Google 登录/注册按钮） |

**册与实现的单位口径**：本表 5 行 = ⑦ 的 `known_gap` 里 `owner == M10` 的 5 条（逐条相等，§10 命令 1 复算）。
本波**没有**「已注册占位待升级」的行（与 M8 的 25 行 = 24 gap + 1 占位、M9 的 34 行 = 33 gap + 1 占位不同）⇒ **A 面的路由账就是 5 条**。

`docs/plan1.md` 对这一族**零覆盖**（立项当轮 `grep -n 'health\|/api/config\|readyz\|healthz' docs/plan1.md` 零命中）：§1.2 的路由计数行里根本没有这一族 ⇒ 本表是**第一次**给出它的三方对账（路由 ↔ 上游 handler 文件 ↔ 本地落点），落点见 §2 / §3。

### 1.2 上游文件与行数（非测试）+ **单位裁定**

| 上游文件 | 行数 | 承担的键 | 说明 |
|---|---:|---|---|
| `server/cmd/server/health.go` | **198** | `/health`、`/healthz`、`/readyz` | `liveHandler` + `readyHandler` + `readiness()`（含 3s 缓存）+ 两个响应结构 |
| `server/cmd/server/health_realtime.go` | **106** | `/health/realtime` | token / loopback 访问门 + 快照合并 |
| `server/internal/handler/config.go` | **223** | `/api/config` | `AppConfig` 17 字段 + `GetConfig` + `daemonSetupURLsFromEnv` + `isOfficialCloud*` |
| `server/internal/realtime/metrics.go` | 277（**只读引用**） | — | `Metrics.Snapshot()` 的 13 个顶层键 |
| `server/internal/daemonws/metrics.go` | 60（**只读引用**） | — | `Snapshot()` 的 14 个键，挂在快照的 `daemonws` 子键下 |
| 小计（真正要移植的 3 个文件） | **527** | 5 键 | 另加两个 metrics 文件的**输出形状**（不是下载） |

**单位裁定（与 `docs/60` §9 对渠道行、`docs/61` §9 对 `vcs/ghsnapshot/composio` 行、`docs/62` §9 对 W9 行的同类裁定一致）**：`plan1.md` 与 `route-owners.tsv` 里的数字单位一律是 **上游路由条数**（本波 5），
**不是**上游代码行数、不是本仓要写的行数。本波的实际体量分布是**反的**：路由 5 条（527 行上游）而交付物面（bench / 对账 / 发布 / 停止条件）没有路由。

### 1.3 本地现状与缺口（本次实测，base `c23fcfad`）

```
$ python3 scripts/route_parity.py --json
counts: upstream 456 | local 474 | implemented 391 (real 388 + placeholder 3) | known_gap 65
        unclaimed 0 | regressions 0 | local_only 9 (placeholder 2)
owners: {M9: 33, M3+: 16, M3: 11, M10: 5}          # 和 = 65 ✓
sources: routes_dir crates/mc-http/src (files_scanned 201), upstream_commit f41fae6b08fb, baseline_routes 458
```

| 本地文件 | 现状（本轮实测） | 与 M10 的关系 |
|---|---|---|
| `crates/mc-http/src/routes/health.rs` | **58 行**，3 个 handler（`health` / `db_health` / `placeholder`） | A 面主战场（M10-1/2 改写；`placeholder` 只被 `mount.rs:61` 一处调用 ⇒ §9.3 裁定删除） |
| `crates/mc-http/src/routes/mount.rs` | **476 行** | M10-0 anchor 接线 + 删 1 行幽灵占位 |
| `crates/mc-http/src/routes/mod.rs` | **138 行** | M10-0 +1 个 `pub mod` |
| `crates/mc-config/src/lib.rs` | **463 行**（`home_paths.rs` +148） | **职责是进程级 env 配置** ⇒ 与 `/api/config` **不是同一件事**（§2.4 划线） |
| `crates/mc-feature-flags/src/lib.rs` | **102 行**（通用目录，**零** `frontend_public_flags` 概念） | M10-4 加 `frontend.rs`（6 键发布规则） |
| `crates/mc-ws/src/hub/mod.rs` | 已交付 `connection_count()` / `runtime_connection_count()` / `workspace_connection_count()` / `user_connection_count()` | M10-3 的 `/health/realtime` 对应物基座（缺**计数器**：慢客户端驱逐 / 收发丢弃 / 按事件类型的 QPS） |
| `crates/mc-storage/src/{lib.rs,local.rs,s3.rs}` | 203 / 129 / 72 行；`Storage` 只有 provider 路由 + key 校验 + `sha256_etag` | `/api/config` 的 `cdn_domain` / `cdn_signed` **本地无对应物**（§2.2 第 1/2 行给了裁定） |
| `crates/mc-migrate/src/lib.rs` | 已交付 `Readiness{loaded,applied,pending,missing_tables}` + `verify()` + `missing_tables()` | M10-2 的 `/readyz` **直接复用**（不新写就绪判定） |
| `deploy/`、`bench/`、`benches/`、`mc-bench` | **本地全不存在**（`ls` 实测） | 发布面 / bench 面是**纯 greenfield** |
| `crates/mc-conformance/` | 已交付（`--golden <dir>` / `--write` / `--check` / `--no-db` / `--db-url`） | 对账面（M10-5）与 ⑨ 快照的工具本身就是它 |

**本地已有的两个自造探针（`local_only 9` 的成员）**：`GET /api/health`（`mount.rs:28`）、`GET /api/health/db`（`mount.rs:29`）；`GET /api/feature-flags`（`mount.rs:61`，`health::placeholder`，**上游根本没有这个键**）。三者的处置在 §9.3。

### 1.4 尾斜杠双形态：本波实测 **0 键**（本仓第二次）

```
$ python3 scripts/slash_alias_audit.py --declared docs/fixtures/m10-declared-routes.tsv
  declared 5 upstream key(s); dual-form required: 0 | single-form: 5
  => 0 defect(s) from findings, 0 warning(s)          # exit 0
```

| 波 | declared | dual-form required | 出口 |
|---|---:|---:|---|
| M4 | 45 | **15** | `FAIL: 15`（预测红是预期输出） |
| M5 | 29 | **7** | `FAIL: 7` |
| M6 | 57 | **5** | `FAIL: 5` |
| M7 | 24 | **0** | exit 0 |
| M8 | 25 | **0** | exit 0 |
| M9 | 34 | **3** | `FAIL: 3` |
| **M10（本片）** | **5** | **0** | **exit 0** |

原因：5 条上游全是 `r.Get("/a/b", h)` 的 **plain** 注册（不在 `r.Route(...) + Get("/")` 的 chi Mount 形态里）⇒ chi 只服务**无尾斜杠**那一种。
⇒ 纪律：M10-1..M10-4 **只注册上游字面量那一形态**；多注册一条尾斜杠形态就是 `EXTRA_ALIAS` 缺陷，本波 `docs/fixtures/slash-alias-allowlist.tsv` 是 **0 数据行**、无豁免退路。

### 1.5 公开面矩阵（5 条全是**无会话**键，但不是同一层）

| 键 | 上游中间件栈 | 本地必须复刻的**拒绝**语义 |
|---|---|---|
| `/health` | 全局（`RequestID` / `ClientMetadata` / `RequestLogger` / `Recoverer` / CSP / CORS） | 无：任何调用者都得到 200（上游 `liveHandler` 不触库） |
| `/healthz` / `/readyz` | 同上 | **503**：库不可达（`checks.db=error`）或迁移未齐（`checks.migrations=out_of_date`） |
| `/health/realtime` | 同上 + **handler 内自带门** | 有 token ⇒ 缺/错 `Bearer` = **401** + `WWW-Authenticate: Bearer realm="metrics"`；无 token ⇒ 非 loopback（**或**带 `X-Forwarded-*` 等转发头）= **404** |
| `/api/config` | 同上（公开块，`router.go:1478`） | 无：匿名 200（`config_test.go` 的 16 条 + `integration_test.go` 的 1 条全部断言 200） |

⚠️ 三条全在**根路径**、不在 `/api` 前缀下 ⇒ 不得塞进任何 `mount_slice_*` 的 `/api` 子树，也不得被既有的 `/api/*` 鉴权提取器拦住（本地 `AuthUser` 提取器按路由挂，不全局 ⇒ 天然满足）。

### 1.6 🔴 本片发现的独立口径项：**本仓有两条不同的上游 pin**

| 门 | 权威来源 | commit | 差 |
|---|---|---|---|
| ⑦ 路由对齐 | `docs/fixtures/upstream-routes.tsv` + `contracts/golden/PIN`？**否** | **`f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`** | — |
| ⑨ 契约等价 | `contracts/golden/PIN`（366 个 fixture 的 `source.commit`） | **`90e0bdf830436b3981b32a7017e1c18d41c7cdea`** | **+12 提交**（`git rev-list --count f41fae6b..90e0bdf` = 12） |

实测关系：`git merge-base --is-ancestor f41fae6b 90e0bdf` **成立**（前者是后者的祖先）。
对本波的实际影响（逐条实测）：`git diff f41fae6b..90e0bdf -- <M10 的 7 个上游文件>` **只改了两个文件**：
`server/internal/handler/config.go` **+7 行**（新增第 17 个字段 `issue_create_properties_supported`）与 `config_test.go` **+23 行**（新增 `TestGetConfigDeclaresIssueCreatePropertiesSupport`）。
⇒ **`/api/config` 的契约必须以 `90e0bdf` 为准**（⑨ 的 17 条 fixture 就是从它抽的），而**路由表/行号以 `f41fae6b` 为准**（本波 5 条的行号在两侧相同，实测）。
⇒ 纪律：M10-4 起手必须**两个 commit 都克隆**，字段表读 `90e0bdf`、行号读 `f41fae6b`；两者的差异要写进自己那一段登记。这是本仓**第一次**显式区分这两条 pin（此前各波都只引其中一个）。

### 1.7 账的口径：A 面 5 gap / B 面 17 行 = 16 gap + 1 占位

| owner | fixture 行数 | `known_gap` | 差 | 差在哪 |
|---|---:|---:|---:|---|
| **M10** | 5 | 5 | 0 | — |
| **M3+** | **17** | **16** | 1 | `GET /api/issues/{id}/attachments` **已在本地以 501 占位注册**（`crates/mc-http/src/routes/issues/mod.rs:2001` 对应的那一行，`docs/61` §9.2 已登记）⇒ 本波是**占位升级**而非缺口 |
| **M3** | 101 | **11** | 90 | 11 条缺口全是 `/api/cloud-runtime/*`（其余 90 条已实现）；crate 归属已在 `docs/62` §9.2 裁定为 `mc-cloud`、波次账目**仍写 M3** |

⇒ 本波（含 B 面）关掉 **21** 条 `known_gap`（A 5 + B 16）+ **1** 条占位升级；`known_gap 65 → 44`，留下 `M9 33 + M3 11` 给 W9 与其尾账。
**A 面 5 条上游 handler 文件、B 面 17 行的上游文件与行数**：B 面逐行清单见 §10 命令 2（由 `awk '$3=="M3+"'` 一键复算，不在此处抄表）。

---

## 2. 目标架构与落点（含取舍）

### 2.1 `/health` ≠ `/healthz` ≠ `/readyz` —— 三种语义，本地必须分开

上游给的是**两个** handler 挂在**三个**路径上，语义不同：

| 路径 | 上游 handler | 判据 | 本地落点 |
|---|---|---|---|
| `/health` | `liveHandler` | **liveness**：进程活着就 200，**不触库**；body `{status:"ok", pid, commit, started_at}`（后三个 omitempty） | `crates/mc-http/src/routes/probes/live.rs` |
| `/readyz` | `readyHandler` | **readiness**：`db.Ping` + **所有** up 版本都在 `schema_migrations` 里（不是"有最新一行"）⇒ 200 或 **503**；body `{status, checks:{db, migrations}}`，`migrations ∈ {ok, error, out_of_date, unknown}` | `crates/mc-http/src/routes/probes/ready.rs` |
| `/healthz` | `readyHandler`（**同一个**） | 同 `/readyz` | 同上（**同一个 handler 挂两个路径**，不是两份实现） |

**取舍**：本地现有的 `GET /api/health` 把 `db` 状态塞进一个**恒 200** 的响应（`health.rs:22-34` 的 `status` 硬编码 `"ok"`，`db` 字段才反映健康）。
⇒ 裁定：**不把它改造成 `/readyz`**（改它的语义会破坏既有 CLI 探针与 `mc-openapi` 的文档测试，见 §9.3），而是新增独立的 ready 面。
**但**：`/readyz` 的"迁移齐否"判定**必须复用** `mc-migrate::verify()`（`Readiness.pending` / `missing_tables`），**不许**在路由里重写一份 SQL —— 先例：上游 `readinessQuery` 是 `SELECT COUNT(*) FROM schema_migrations WHERE version = ANY($1)`，与本仓 `Migrator::pending()` 回答的是同一个问题。
**缓存**：上游 `readinessCacheTTL = 3s`（`sync.Map` + `atomic.Pointer`）⇒ 本地逐字复刻（3 秒 TTL + 单飞刷新），**不是**可选项：没有它，`/readyz` 每次调用都要 `Ping` + 迁移比对，运维探针 1s 一次就会把库打满。

### 2.2 🔴 `/api/config` 的契约（**以 `90e0bdf` 为准**，逐字段）

**先钉死一件事实**：⑨ 的 17 条 config fixture **全部只断言 `status: 200` + `json_subset: {}`**（本轮实测：17/17 的 `json_subset` 是空对象，`actor` 全 `anonymous`；16 条 `via=handler`、1 条 `via=router`）。
⇒ **⑨ 的 `unmounted → pass` 只证明"挂上了且返回 200 + 一个 JSON 对象"**，它**不**证明字段集正确。
⇒ 字段级契约的判据只有一处：**`config_test.go`（`90e0bdf`）里那 19 个测试的断言** + 本片自造的字段级 fixture（§2.6 / M10-5）。**不许**把 ⑨ 变绿当成 config 做完了。

**17 个字段的逐条来源与本地取值**（`AppConfig`，`90e0bdf` 逐字；`omitempty` 栏决定"缺省时键是否出现"）：

| # | JSON 键 | 上游取值 | 本地落点 / 取值裁定 | omitempty |
|---:|---|---|---|---|
| 1 | `cdn_domain` | `h.Storage.CdnDomain()` | 本仓 `mc-storage` **无 CDN 概念** ⇒ 新 env `MULTICA_CDN_DOMAIN`（缺省 `""`） | 否（**总出现**） |
| 2 | `cdn_signed` | `h.CFSigner != nil` | 本仓无 CloudFront ⇒ **恒 `false`**、键**不出现**（登记为已知差异：签名下载走 `mc-storage` 自己的 HMAC，M10-B1） | 是 |
| 3 | `allow_signup` | `ALLOW_SIGNUP != "false"` | 同名 env，逐字 | 否 |
| 4 | `google_client_id` | `GOOGLE_CLIENT_ID` | 同名 env（本仓 M1 已交付 `/auth/google`） | 是 |
| 5 | `workspace_creation_disabled` | `DISABLE_WORKSPACE_CREATION == "true"` | 同名 env，逐字（**不是** `envPositiveInt` 式宽松解析） | 是 |
| 6 | `daemon_server_url` | `MULTICA_DAEMON_SERVER_URL` → `MULTICA_PUBLIC_URL` → app_url，`normalizePublicURL` 去尾斜杠 | 同名三个 env + 同一归一化（**只在 app_url 非空时才可能非空**） | 是 |
| 7 | `daemon_app_url` | `MULTICA_APP_URL` → `FRONTEND_ORIGIN` | 同名两个 env；`multica.ai` 主机 ⇒ 两者**都置空**（`isOfficialCloudDaemonConfig`，逐字移植） | 是 |
| 8 | `vcs_integration_available` | `h.cfg.VCSIntegrationEnabled` | **已交付的接缝**：`crates/mc-http/src/state/integrations.rs` 的 `MULTICA_VCS_INTEGRATION_ENABLED`（M8-2 落的）⇒ **只读复用**，不新造开关 | 是 |
| 9 | `posthog_key` | `POSTHOG_API_KEY`（`ANALYTICS_DISABLED ∈ {true,1}` ⇒ 空） | 同名 env + 同一短路 | 否 |
| 10 | `posthog_host` | `POSTHOG_HOST`；空且 key 非空 ⇒ `https://us.i.posthog.com` | 同名 env + 同一缺省回填 | 否 |
| 11 | `analytics_environment` | `ANALYTICS_ENVIRONMENT` → `APP_ENV` → `"dev"`（归一化 `production/staging/dev`） | 同名 env + 同一归一化（`dev` 是**缺省值**，不是空串） | 否 |
| 12 | `feature_flags` | `EvaluateFrontendPublicFlags`（见下表） | `crates/mc-feature-flags/src/frontend.rs`（新）：**6 键**发布规则 | 是 |
| 13 | `local_worktree_supported` | 恒 `true`（"这是本 build 的属性"） | ✅ **实测成立**：`crates/mc-http/src/routes/projects/resource_ref.rs:106-119` 校验 `execution_mode ∈ {in_place, worktree}`，`:280-300` 的 `wants_worktree()` + `requireWorktreeCapableDaemon` 等价物给出 422 `daemon_version_unsupported` ⇒ 取 **`true`**，并**必须**配一条"未宣告能力的 daemon ⇒ 422"用例 | 否 |
| 14 | `agent_conversation_starters_supported` | 恒 `true` | ✅ **实测成立**：`crates/mc-repos/src/agent/` 持久化 `conversation_starters`（M3-5；`db_tests.rs` 有显式断言）⇒ 取 **`true`** + 一条持久化用例 | 否 |
| 15 | `issue_create_properties_supported` | 恒 `true`（**`90e0bdf` 新增**） | ❌ **实测不成立**：`CreateIssueRequest`（`crates/mc-http/src/routes/issues/dto.rs:242-262`）**没有** `properties` 字段 ⇒ serde 静默忽略该 bag（正是上游注释警告的那个失败模式）⇒ **取 `false`** 并登记为已知差异（§9.4 第 3 条）。**不**在本波补实现（那属 M2-A 面） | 否 |
| 16 | `comment_delete_keep_replies_supported` | 恒 `true` | ✅ **实测成立**：`DELETE /api/comments/{commentId}/keep-replies` **已注册**（`routes/comments/mod.rs`）+ `CommentRepo::soft_delete(id, keep_replies=true)` 保留回复（`crates/mc-repos/src/comment.rs:464-484`）⇒ 取 **`true`** + 一条"回复仍在"用例 | 否 |
| 17 | `server_version` | `h.cfg.ServerVersion`，**仅自建版**（`!isOfficialCloudDeployment()`） | crate 版本（`env!("CARGO_PKG_VERSION")`）+ 同一 `multica.ai` 抑制分支；dev 构建可为空 | 是 |

**`feature_flags` 的 6 键（`EvaluateFrontendPublicFlags` 逐字）**：

| 键 | 上游默认 | 规则 |
|---|---|---|
| `billing_workspace_subscriptions` | `false` | `frontendPublicFlags` 三键之一，走 provider 判定（缺省 `false`） |
| `composio_mcp_apps` | `false` | 同上 |
| `plugins_v1` | `false` | 同上（**dogfood 开关**，`config_test.go` 有独立测试） |
| `agents_agent_builder` | **恒 `true`** | 兼容键：已安装的桌面客户端依赖它 |
| `agents_skill_toggles` | **恒 `true`** | 兼容键（v0.4.0 客户端） |
| `settings_resource_labels` | **恒 `true`** | 兼容键（v0.4.0–v0.4.15 客户端 **fail-closed**） |
| ~~`desktop_hang_stack_capture`~~ | **必须不发布** | 上游注释逐字：发布它＝给已不能产生可用 stack 的机群重新打开调试器通道 |
| ~~`custom_issue_statuses`~~ 等 | **必须不发布** | 同上（`config_test.go` 逐条断言"不得出现"） |

**本地 flag 源（`0` 迁移的证据）**：上游 `featureflag.NewServiceFromEnv` = `EnvProvider("FF_")` + 可选 `MULTICA_FEATURE_FLAGS_FILE`（YAML 规则文件）+ 链式 provider；`IsEnabled(ctx, key, default)` 在无命中时返回 default。
**实测：上游 `migrations/` 里没有任何 feature flag 表**（`grep -rli 'create table.*feature' migrations/upstream/*.up.sql` 为空）⇒ 本仓 `/api/config` **不需要新迁移**（与 §6.4 的预测一致），实现只需 env 覆盖（`FF_<KEY>`，`flagKeyToEnv` 的转换：非字母数字 → `_`，字母转大写）+ 可选 YAML 文件（本仓 `serde_yaml` 已在 `[workspace.dependencies]`）。

**边界纪律**：`/api/config` 的字段值**只**来自 ① env ② crate 常量 ③ `mc-storage` / `mc-feature-flags` 的只读查询。**禁止**读任何用户/租户数据（上游注释逐字：「never user- or tenant-scoped data」）⇒ 一条 DoD：**匿名可读 + 不触库**（离线层必须能答）。

### 2.3 `/health/realtime` 的语义与可判定契约（不许写"返回个 ok 就行"）

上游它 probe 的是 **realtime hub 与 daemonws hub 的进程级计数器**，两个快照**合并**（`daemonws` 那个挂在自己的子键下）：

```
realtime.M.Snapshot()          → 13 个顶层键（13 个计数/映射）
  connects_total / disconnects_total / active_connections / slow_evictions_total
  messages_sent_total / messages_dropped_total / inbound_too_large_total
  events_sent_by_type{事件类型→计数} / subscribes_total{} / unsubscribes_total{}
  subscribe_denied_total{} / active_scope_rooms{} / redis{16 个键（含 streams{} 与 last_error）}
daemonws.M.Snapshot()          → 14 个键，整体挂在 snapshot["daemonws"] 之下
  connects_total / disconnects_total / active_connections / slow_evictions_total
  wakeup_{published_total,publish_errors,received_total,delivered_hit_total,delivered_miss_total}
  runtime_gone_{delivered_hit_total,delivered_miss_total,published_total,publish_errors,received_total}
```

**本地对应物**：`crates/mc-ws/src/hub/mod.rs`（`Hub` 已有连接计数四则）+ `crates/mc-realtime`（`WsState`）；**缺**的是**累计计数器**（慢客户端驱逐、收发丢弃、按事件类型发送、订阅/退订/拒绝、scope room 活跃数）⇒ M10-3 的写集包含 `crates/mc-ws/src/hub/metrics.rs`（新）+ `Hub` 的字段与自增点。
**Redis 子树**：本仓**无 Redis 依赖**（`Cargo.toml` 无 redis）⇒ 裁定 `redis` 子键**照发**但取**单副本缺省值**（`connected:false` / `node_id:""` / 计数 0 / `streams:{}` / `last_error:null`），并在登记里写明理由（与上游"无 Redis 时的单副本部署"同形）。**不许**删掉这个子键（客户端可能是按 key 存在性解析的）。

**可判定响应契约（状态码 + 字段）**：

| 情形 | 状态码 | body / 头 |
|---|---|---|
| `REALTIME_METRICS_TOKEN` 已设 + 正确 `Authorization: Bearer <token>` | **200** | 13 个顶层键 + `daemonws{14}`；`Content-Type: application/json`；`Cache-Control: no-store` |
| 已设 token + 缺失/错误 token | **401** | `WWW-Authenticate: Bearer realm="metrics"`，body `unauthorized`（上游 `http.Error` ⇒ 纯文本 + 换行） |
| 未设 token + **loopback** 且**无**转发头 | **200** | 同上 |
| 未设 token + 非 loopback，或**任一** `X-Forwarded-*`/`Forwarded` 存在（哪怕来源是 127.0.0.1） | **404** | 空（"不向远程扫描器宣告它的存在"） |

⇒ 四条各一条用例（第 3/4 条用 `tower::ServiceExt::oneshot` + 伪造 `RemoteAddr`/头，本仓既有手法）。

### 2.4 🔴 `mc-config` 与 `/api/config` 的边界（禁止混成一节）

| 维度 | `crates/mc-config`（**已存在**，463 行） | `GET /api/config`（**本波新建**） |
|---|---|---|
| 消费者 | **进程自己**（启动装配：`AppState::new`、`mc-migrate`、日志、存储/密钥选择） | **浏览器前端**（登录前） |
| 内容 | `server`/`database`/`auth`/`storage`/`secrets`/`instance`/`feature_flags`/`runtime`/`channel`/`plugin` 十段**强类型**配置 | 17 个**扁平**的公开字段（白名单，`AppConfig`） |
| 安全面 | **可以**含密（DB URL、密钥、AWS 前缀）⇒ 永不外发 | **只允许**匿名安全字段（上游注释：never user/tenant-scoped） |
| 与 env 的关系 | **权威解析者**（`MULTICA_*` + `PAPERCLIP_*` 兼容别名） | **消费者之一**：能复用 `Config` 的段落就直接读（如 `server.external_url`），**不能**复用的（`ALLOW_SIGNUP` / `POSTHOG_*` / `CDN` / 能力声明）由本波新读**同一批 env** |
| 代码落点 | `crates/mc-config/src/{lib.rs,home_paths.rs}` | `crates/mc-http/src/routes/config.rs`（**不改 `mc-config` 的任何既有节**；若必须加字段，加**新节**并登记） |

⇒ **两条硬纪律**：① `/api/config` **不得** `serde` 序列化 `mc_config::Config`（那会把 DB URL 与密钥发出去）；② 任何新增 env **必须**在 `mc-config` 里登记或明确标注"仅 `/api/config` 读"（本波采用后者的清单形式，写进 M10-4 的 DoD）。

### 2.5 性能 bench 的落点与工具

**选库闸门（`plan1.md` §4）**：crates.io 有活跃维护且 >1M 月下载 ⇒ **必须用现成的**。
实测（crates.io API，本轮）：`criterion` newest **0.8.2**（`rust_version = 1.86`）、**0.7.0（`rust_version = 1.80`）**、0.6.0（1.80）、0.5.1（无声明）；总下载 **289,529,678**、近 90 天 **59,680,685** ⇒ 闸门通过。
⇒ **裁定：`criterion = "0.7"`**，理由：本仓 `rust-version = "1.80"`（`[workspace.package]`）⇒ **0.8.x 的 MSRV 1.86 不可用**，0.7.0 恰好等于本仓上限（与 M6-0 对 `zip`/`serde_yaml`、M8-0 对 `ring` 的"按声明的 MSRV 选版本"同一纪律，先例 `docs/32` §9.4）。

**落点**：新 crate `crates/mc-bench`（`plan1.md` §3.3 给 W10 的 crate 就是 `mc-bench` + `mc-conformance`）：

```
crates/mc-bench/Cargo.toml            # [[bench]] name=… harness=false test=false
crates/mc-bench/src/lib.rs            # 数据集播种 + 连接池/语句缓存口径 + 报告序列化
crates/mc-bench/benches/issue_list.rs # 热点 ①：issue 列表（limit=50 + 排序 + 过滤）
crates/mc-bench/benches/facets.rs     # 热点 ②：facets 聚合
crates/mc-bench/benches/inbox_cursor.rs # 热点 ③：inbox 游标（keyset 翻页）
```

**三条纪律**（写进 M10-6 的 DoD）：

1. `[[bench]]` **必须** `test = false`（否则门 ⑤ `cargo test --workspace` 会把整个 benchmark 当测试跑，把 40 分钟的 CI 拖成小时级；criterion 的 `harness = false` 只解决"不吞 main"，不解决 cargo 的 target 选择）。
2. **无库即红**：bench 需要真库，缺 `MULTICA_TEST_DATABASE_URL` 时 **panic 退出**，**不许静默跳过**（本仓已有两次"绿是空跑"的教训，见 `docs/24`）。
3. **判据化阈值（禁"同量级"这类不可判定措辞）**：
   * 口径：同一 workdir、同一 PG（`:5432`）、同一**固定数据集**（播种是 bench 的一部分、有确定行数）；每档 `warm_up_time = 3s`、`measurement_time = 10s`，取 criterion 的 `p50/p95/p99`（`--sample-size 100`）。
   * **相对阈值**：`p95(本片) ≤ 1.25 × p95(基线)`，基线 = **本波起手时**在 `base` 分支上跑一次的结果，落到 `docs/fixtures/bench-baseline.json`（**判据只有一处**，且可机器比对）。
   * **绝对上界**（防"基线本身就很慢"）：`issue_list p95 ≤ 5 ms`、`facets p95 ≤ 20 ms`、`inbox_cursor p95 ≤ 2 ms`（10k issue / 5k comment 数据集、32 vCPU、本地 PG 无并发）。上界是**本波写死的第一版**，若起手实测基线远超它，M10-6 **必须**先改这份预算并写理由，不许默默放宽。
   * **R10（上游有 prepared statement cache）**：`Cargo.toml` 的连接参数**必须**显式 `.statement_cache_capacity(n>0)`（sqlx 缺省已是 100，但必须**显式**），且 M10-6 交付一条**可判**用例：同一语句在同一连接上第二次执行**不再** `Parse`（用 `pg_stat_statements` 或 sqlx 的 `prepare` 计数断言）。

**与 Go 版比**：**不做**（没有 Go 工具链，实测 `which go` 为空）⇒ 登记为"不做 + 理由"，由相对阈值替代（§9.6）。

### 2.6 契约对账（双跑对账的**离线**证据形态）与停止条件

**为什么不能"人工比对"**：上游 Go 服务在本仓环境**起不来**（无 Go 工具链、无前端）；CI 也没有 Go 服务。⇒ 裁定：**双跑对账 = 与"冻结的上游期望"逐字段对账**，由三件套承担（§9.7）：

1. **上游冻结面**：`contracts/golden/**`（365 条，pin `90e0bdf`）—— 已有，**不改**（它的 `json_subset` 大多为空，是抽取器的既有形态）。
2. **本仓自造字段级 fixture**：新目录 `contracts/golden-local/**`（**不进** `contracts/golden/` ⇒ 不改 ⑨ 的 365/14/23 分母），用**同一条抽取器的格式**（`schema_version 1` + `path` + `actor` + `expect.status` + `expect.json_subset`**非空**）；
   由 `cargo run -q -p mc-conformance -- --no-db --golden contracts/golden-local` 回放 ⇒ 判据 = `mismatch 0 ∧ unmounted 0`（**这是唯一一处把 config / probes 的字段级契约变成机器判据的地方**）。
3. **一键复算**：`scripts/stop_condition.sh`（新，M10-8）把 §9.9 的 Tier-1 向量逐项算出并给出 exit code。

**停止条件的三层**（详见 §9.9）：

| Tier | 内容 | 谁来判 |
|---|---|---|
| **T1** | 12 条硬向量（⑦/⑦b/⑨/⑩/⑧/门禁/CI/对账）全绿 | `scripts/stop_condition.sh` exit 0 |
| **T2** | T1 未达标项的**逐条登记**（id + 理由 + 归属片）；登记表冻结 ⇒ 停止条件是"已登记例外的冻结"而不是"永远不满足" | M10-9 的 INT 报告 + `docs/32` §9.x |
| **T3** | 人工验收（真实前端 E2E、真实镜像部署）**明确不做**（无资产、不可判定）⇒ 写进 §9.8 的"不做"清单 | — |

---

## 3. 写集与并发

### 3.1 共享锚点（**只在 M10-0 动一次**，其余片只读）

| 共享件 | 动作 |
|---|---|
| `crates/mc-http/src/routes/mod.rs` | +1 行 `pub mod probes;` +1 行 `pub mod config;`（**追加段**，不动既有行） |
| `crates/mc-http/src/routes/mount.rs` | +`mount_slice_probes()`（一个函数 + 一行 `.merge(...)`）；**删** `/api/feature-flags` 那一行（§9.3） |
| `crates/mc-http/src/routes/probes/mod.rs` | **新建**：聚合 3 个子 router（anchor 期 3 个子 router 全空 `Router::new()` ⇒ **零注册键**） |
| `crates/mc-http/src/routes/probes/{live.rs,ready.rs,realtime.rs}` | 建**桩**（签名 + 空 `Router::new()`，各片原地填充） |
| `crates/mc-http/src/routes/config.rs` | 建**桩**（`get_config` 签名 + `AppConfig` 17 字段的**完整形状**；实现归 M10-4 原地填充） |
| `crates/mc-http/src/routes/health.rs` | **改**：删 `placeholder`（唯一调用点随幽灵占位一起删）；`/api/health`、`/api/health/db` **逐字不动** |
| `docs/fixtures/route-parity-baseline.json` | **本 anchor 刷新一次**（§9.1 的唯一例外：删键 ⇒ `regressions` 判红，删除与刷新必须同一次提交；先例 M4-0 / M6-0） |
| `docs/32-M3-DAEMON-FACE.md` §9.**14** | M10-0 追加「M10 的文件→写者表 + 偏离登记」；号段**起手复核**（当前末号 `## 37.`；§9.x 末号 `9.11`，M8-0 预定 `9.12`、M9-0 预定 `9.13`） |
| `Cargo.toml` / `Cargo.lock` | **本波只被 M10-6 碰一次**（`mc-bench` 新成员 + `criterion` dev-dep）；`members` 是 `crates/*` glob ⇒ **不加字面路径** |

> **anchor 不注册占位（本波独有纪律，必须写进 M10-0 的 DoD）**：这 5 条是**上游键**，若 anchor 先注册 501 占位，⑦ 会立刻把它算成 `implemented_placeholder`（`owners.M10 → 0` 假绿），而 ⑨ 会从 `unmounted` 变成 **`mismatch`**（期望 200、得到 501 ⇒ `mismatch 23 → 41`）。
> 这与 M6-0 删幽灵占位的方向相反、与 M8-0「只搬运占位、不新建占位」的理由相同：**anchor 期注册键集合只减 1、不增**（§6 的 M10-0 行）。

### 3.2 与相邻波（M9 / M3 尾账）的交集（逐文件）

| 热点文件 | 谁要碰 | 结论 |
|---|---|---|
| `crates/mc-http/src/{state.rs,routes/{mod,mount}.rs}` / `Cargo.lock` | M9-0（cloud/entitlement 接线）∥ **M10-0** | **不得同飞**（同文件不同块）⇒ §7 的槽位纪律 |
| `crates/mc-http/src/routes/issues/mod.rs` | **M10-B1**（`/api/issues/:id/attachments` 占位 → 真实现） | M2-A 的尾账（`LUM-1691`/`LUM-1793`）**已合**（`docs/63`）⇒ 无在飞写者；写集必须**钉到那一个 `.route(...)` 行**，其余行只读 |
| `crates/mc-http/src/routes/projects/resource_ref.rs` | **只读**（`local_worktree_supported` 的证据） | M10-4 只做断言，**不改** |
| `crates/mc-http/src/routes/comments/mod.rs` | **只读**（`keep_replies` 的证据） | 同上 |
| `crates/mc-repos/src/{attachment.rs?}`（**不存在**⇒ M10-B1 新建）/ `crates/mc-repos/src/issue.rs` | M10-B1 写新文件；`issue.rs` **只读**（占位升级走路由层） | 单写者 |
| `crates/mc-ws/src/hub/*` | **M10-3**（加计数器） | 与 M9/A 面零交集；与 B 面的 `/ws`（M10-B4）**同 crate 不同文件**（B4 只读 hub 的公开 API、写 `routes/ws.rs`）⇒ 排在不同 stage（§4.4） |
| `crates/mc-cloud/**` / `routes/cloud/**` | **M9-11**（`/api/cloud-runtime/*`） | 与 M10 **零交集**（本波不引 `mc-cloud`） |
| `docs/32-M3-DAEMON-FACE.md` §9.x | M10-0（§9.14）∥ M9-0（§9.13） | 避免同轮（纯文档，低危） |
| `docs/fixtures/route-parity-baseline.json` | M10-0（删键例外）∥ M10-9（INT 收口）∥ M9-10（M9 的 INT） | **必须串行**（同一文件、整文件重写） |
| `contracts/golden/**` | **无人** | M10-5 只写 `contracts/golden-local/**`（新目录） |

### 3.3 写集（**一格 = 一个本地文件 = 一个写者**；逐字路径，禁 glob / 花括号）

| 本地文件（逐字） | 唯一写者 |
|---|---|
| `crates/mc-http/src/routes/mod.rs` | M10-0 |
| `crates/mc-http/src/routes/mount.rs` | M10-0 |
| `crates/mc-http/src/routes/health.rs` | M10-0 |
| `crates/mc-http/src/routes/probes/mod.rs` | M10-0 |
| `crates/mc-http/src/routes/probes/live.rs` | M10-0 建桩 → **M10-1** 填充 |
| `crates/mc-http/src/routes/probes/ready.rs` | M10-0 建桩 → **M10-2** 填充 |
| `crates/mc-http/src/routes/probes/realtime.rs` | M10-0 建桩 → **M10-3** 填充 |
| `crates/mc-http/src/routes/config.rs` | M10-0 建桩 → **M10-4** 填充 |
| `crates/mc-feature-flags/src/{lib.rs,frontend.rs}` | **M10-4**（`frontend.rs` 新建；`lib.rs` 只加 1 行 `pub mod frontend;`） |
| `crates/mc-ws/src/{lib.rs,hub/mod.rs,hub/metrics.rs}` | **M10-3**（`metrics.rs` 新建；`lib.rs` +1 行；`hub/mod.rs` 加字段与自增点） |
| `crates/mc-http/src/routes/attachments/mod.rs` | M10-B1（新建，聚合） |
| `crates/mc-http/src/routes/attachments/{read,download,delete}.rs` | M10-B1（三个新文件：读 / 内容与签名下载 / 删除） |
| `crates/mc-http/src/routes/issues/mod.rs` | **M10-B1**（**仅** `/api/issues/:id/attachments` 那一行：`not_implemented` → `super::attachments::router()` 的 handler；其余行只读） |
| `crates/mc-repos/src/attachment.rs` | M10-B1（新建：`attachment` 行的读/删/签名） |
| `crates/mc-repos/src/lib.rs` | **M10-B1**（+1 行 `pub mod attachment;`） |
| `crates/mc-http/src/routes/uploads.rs` | M10-B2（新建：`POST /api/upload-file` + `GET /uploads/*`） |
| `crates/mc-http/src/routes/quick_actions/{mod.rs,list.rs,lifecycle.rs,invoke.rs}` | M10-B3（四个新文件：目录 CRUD / 生命周期 / issue 侧 render+run） |
| `crates/mc-http/src/routes/avatars.rs` | M10-B4（新建：`GET /api/avatars/:sig/*`） |
| `crates/mc-http/src/routes/ws.rs` | M10-B4（新建：`GET /ws`，复用已交付 `mc-ws` 的 hub） |
| `crates/mc-http/src/routes/comments/sub_issues.rs` | M10-B4（新建：`GET /api/comments/:commentId/sub-issue-preview`） |
| `crates/mc-bench/{Cargo.toml,src/lib.rs,benches/issue_list.rs,benches/facets.rs,benches/inbox_cursor.rs}` | M10-6 |
| `Cargo.toml` / `Cargo.lock` | M10-6（唯一一次） |
| `contracts/golden-local/config/001-*.json` … `contracts/golden-local/probes/*.json` | M10-5（新目录） |
| `scripts/mc_golden_local_check.sh` | M10-5（新建：包一层 `--golden contracts/golden-local` 并断言 `mismatch 0 / unmounted 0`） |
| `deploy/Dockerfile` | M10-7（新建） |
| `scripts/gates.sh` / `.github/workflows/ci.yml` | M10-7（`image` 门 + CI 第 4 个 job；**不动默认门集合**） |
| `scripts/stop_condition.sh` / `docs/65-STOP-CONDITION.md` | M10-8 |
| `docs/32-M3-DAEMON-FACE.md` §9.14 | M10-0（一次落，其余片只读） |
| `docs/fixtures/route-parity-baseline.json` | M10-0（删键例外）→ M10-9（INT 收口） |
| `crates/mc-conformance/report.json` | **M10-9**（唯一刷新者；`--write`） |
| `docs/64-M10-PLAN.md` / `docs/fixtures/m10-declared-routes.tsv` | **本片（`LUM-2099`）**，此后只读 |

> **记法纪律（承接 `docs/57` §3.2 / `docs/60` §3.3 / `docs/61` §3.3 / `docs/62` §3.3）**：写集一律写**逐字路径**，禁 glob / 花括号 / 「某某段」。
> **写集审计两类（每片 DoD 必填）**：① 本片**新建**的文件在不在 anchor 骨架里（不在 ⇒ 说明为何不抢别人的行）；② **为让新文件可见要改哪个既有文件**（`mod.rs` / `mount.rs` / `lib.rs` ……）—— 第二类是本仓历史**第 16 类漏项**的高发点（`docs/37` §47 起）。

### 3.4 同 stage 零交集矩阵（逐片核对）

| stage | 片 | 路由侧 | crate / 仓储侧 | 交集 |
|---|---|---|---|---|
| 1 | M10-0 | `mount.rs` / `routes/mod.rs` / `health.rs` / 4 个桩 | — | — |
| 2 | M10-1 ∥ M10-2 ∥ M10-3 | `probes/live.rs` ∥ `probes/ready.rs` ∥ `probes/{realtime.rs}` + `mc-ws/hub/*` | — ∥ — ∥ `mc-ws` | **∅** |
| 3 | M10-4 ∥ M10-B1 ∥ M10-6 | `config.rs`+`mc-feature-flags` ∥ `attachments/*`+`mc-repos/attachment.rs` ∥ `mc-bench/*` | 三个互不相交的 crate 面 | **∅** |
| 4 | M10-5 ∥ M10-B2 ∥ M10-B3 | `contracts/golden-local` ∥ `uploads.rs` ∥ `quick_actions/*` | — | **∅**（B2 只读 B1 已交的 `mc-repos/attachment.rs`） |
| 5 | M10-7 ∥ M10-8 ∥ M10-B4 | `deploy/Dockerfile`+`gates.sh`+`ci.yml` ∥ `stop_condition.sh` ∥ `avatars.rs`/`ws.rs`/`comments/sub_issues.rs` | — | **∅**（B2 只读 B1 的仓储；B4 只读 hub） |
| 6 | M10-9 | — | — | 只读 + 快照 |

---

## 4. 切片表（派发用）

### 4.1 A 面（W10 收尾：5 路由 + 交付物）—— 1 anchor + 8 片

| # | 切片 | 路由 | stage | 硬前置 |
|---|---|---:|---|---|
| **M10-0** | anchor：`probes/` + `config.rs` 骨架接线、删 `/api/feature-flags` 幽灵占位、基线例外刷新、§9.14 登记 | 0（**−1**） | 1 | 无（可立即起） |
| **M10-1** | `/health` live 探针（`status/pid/commit/started_at`，不触库） | 1 | 2 | M10-0 |
| **M10-2** | `/healthz` + `/readyz` ready 探针（3s 缓存 + `mc-migrate::verify` + 503 三态） | 2 | 2 | M10-0 |
| **M10-3** | `/health/realtime`（快照 13+14 字段 + token/loopback 四态门 + `mc-ws` 计数器） | 1 | 2 | M10-0 |
| **M10-4** | `/api/config`（17 字段 + 6 flag + 4 能力声明逐条实测） | 1 | 3 | M10-0 |
| **M10-5** | 契约对账：`contracts/golden-local/**` 字段级 fixture + 对账脚本 | 0 | 4 | M10-1 / M10-2 / M10-3 / M10-4 |
| **M10-6** | 性能 bench：`crates/mc-bench`（criterion 0.7 + 3 热点 + R10 语句缓存） | 0 | 3 | 无（需真库；**不新增门**） |
| **M10-7** | 发布面：`deploy/Dockerfile` + 门 `image`（非默认集合）+ CI 第 4 job | 0 | 5 | M10-0（`gates.sh` 追加） |
| **M10-8** | 停止条件操作化：`scripts/stop_condition.sh` + `docs/65-STOP-CONDITION.md` | 0 | 5 | M10-1…M10-7 |
| **M10-9** | INT：快照三件套刷新（⑦ 基线 / ⑨ report.json / ⑩）+ 终态向量判定 + 缺口登记 | 0 | 6 | 全波 |

### 4.2 B 面（尾账收口 `M3+`：17 行 = 16 缺口 + 1 占位升级）—— 4 片

| # | 切片 | 路由行数 | 其中缺口 | stage | 硬前置 |
|---|---|---:|---:|---|---|
| **M10-B1** | 附件面：`/api/attachments/{id}` GET/DELETE + `/content` + `/download` + `/signed-download` + `/api/issues/{id}/attachments`（占位升级，**6 行**） | 6 | 5 | 3 | M10-0 |
| **M10-B2** | 上传与静态分发：`POST /api/upload-file` + `GET /uploads/*`（**3 行**：含 B1 的 DELETE 归属复核） | 3 | 3 | 4 | M10-B1 |
| **M10-B3** | quick-actions：目录 4 条（`GET|POST /api/quick-actions/`、`PATCH|DELETE /api/quick-actions/{id}/`）+ issue 侧 2 条（`render` / `run`）（**6 行**） | 6 | 6 | 4 | M10-0 |
| **M10-B4** | 其余：`GET /api/avatars/{sig}/*` + `GET /ws` + `GET /api/comments/{commentId}/sub-issue-preview`（**2 行 + 1 行**） | 3 | 2 | 5 | M10-0 |

> **B 面的路由账**：`6 + 3 + 6 + 3 = 18`？**不是** —— `DELETE /api/attachments/{id}` 只算**一次**（在 B1），B2 的行是 `POST /api/upload-file` + `GET /uploads/*` **2 行**。
> 逐行归属（复算见 §10 命令 2）：**B1 6 行**（`/api/attachments/{id}` GET、`DELETE /api/attachments/{id}`、`.../{id}/content`、`.../{id}/download`、`.../{id}/signed-download`、`/api/issues/{id}/attachments`）· **B2 2 行**（`/api/upload-file`、`/uploads/*`）· **B3 6 行**（`/api/quick-actions/` GET/POST、`/api/quick-actions/{id}/` PATCH/DELETE、`/api/issues/{id}/quick-actions/{qaId}/render|run`）· **B4 3 行**（`/api/avatars/{sig}/*`、`/ws`、`/api/comments/{commentId}/sub-issue-preview`）⇒ **6+2+6+3 = 17 ✓**；
> 缺口 = **16**（17 行 − B1 的 1 条占位升级）✓；⑦ 的 `owners.M3+` 从 16 走到 **0**。

**规模账**：A 面 10 个 issue + B 面 4 个 + INT（含在 A 面表里）= **13 个 M10 issue**；路由 5 + 17 = **22 行**；缺口 5 + 16 = **21**；占位升级 1。

### 4.3 波次（并发 ≤3；同 stage 三片可并行，上一 stage 未合不进下一 stage）

```
stage 1  M10-0
stage 2  M10-1 ∥ M10-2 ∥ M10-3
stage 3  M10-4 ∥ M10-B1 ∥ M10-6
stage 4  M10-5 ∥ M10-B2 ∥ M10-B3
stage 5  M10-7 ∥ M10-8 ∥ M10-B4
stage 6  M10-9（INT）
```

**串行链（必须写清，避免同 stage 内抢同一文件）**：

1. **`M10-0 → 全部`**：`routes/{mod,mount}.rs`、`probes/*` 桩、`config.rs` 桩、`health.rs`、基线、§9.14。
2. **`M10-0 → M10-3` 之外**：`mc-ws` 只被 M10-3 写；`/ws`（M10-B4）只读 hub 的公开 API ⇒ 排 stage 5（与 M10-3 隔 3 轮）。
3. **`M10-B1 → M10-B2`**：同一批 `attachment` 行与同一份 `mc-storage` 出口；B2 复用 B1 的签名/键校验工具。
4. **`Cargo.toml`/`Cargo.lock` 只被 M10-6 写一次** ⇒ M10-6 与 M10-0 **不同 stage**（stage 3 vs 1），与 M9-0/deps 面**不得同轮**。
5. **`report.json` / `route-parity-baseline.json` 只被 M10-9 写**（除 M10-0 的删键例外）⇒ M10-9 是唯一收口。
6. **`gates.sh` / `ci.yml` 只被 M10-7 写** ⇒ 与任何改 `gates.sh` 的动作（无）零冲突；`image` 门**不进默认集合**（§9.8）。

---

## 5. 与 M9 / M3 尾账的交界

### 5.1 本波 A 面与 M9 波：**零共享文件**

| 面 | M9 | M10 |
|---|---|---|
| 路由文件 | `routes/{cloud,dashboard,onboarding}/*`、`timeline.rs`、`issue_table/mod.rs` | `routes/{probes/*,config.rs,attachments/*,uploads.rs,quick_actions/*,avatars.rs,ws.rs}` |
| crate | `mc-cloud`、`mc-entitlement`（新） | `mc-feature-flags`（改）、`mc-ws`（改）、`mc-bench`（新） |
| 共享件 | `state.rs` / `routes/{mod,mount}.rs` / `Cargo.lock`（M9-0 一次） | 同两个文件（**M10-0 一次**）+ `Cargo.lock`（M10-6 一次） |

⇒ **`M9-0` 与 `M10-0` 不得同飞**（同两个文件 + `Cargo.lock`）；**`M10-6` 与 `M9-0` 也不得同飞**（`Cargo.lock`）。
⇒ 其余片可完全并行（本波有 6 个 stage，M9 有 5 个 ⇒ 两波并行时槽位怎么分见 §7.2）。

### 5.2 🔴 `/api/cloud-runtime/*` 11 条（owner 单元格 = `M3`）的归属裁定

**事实三条**：(a) `docs/62` §9.2 已裁定「**crate 归属 = `mc-cloud`；波次账目 = 仍在 M3**」，并把 owner 单元格的迁移**执行点**定在 **M9-10（INT）**；(b) 本地 **零实现**（11/11 在 `known_gap`，本次实测）；(c) 上游 `internal/cloudruntime/client.go`（255 行）+ `internal/handler/cloud_runtime.go`（**208 行**）就是 **M9-1/2/6 用的同一份出站传输**（`cloud_billing.go:16` 逐字「Fleet and Billing share `:8080`」）。

**裁定（本片）**：**归 M9 波次 ⇒ 新立 1 片 `M9-11`，`--parent` = M9 计划 `LUM-1814`，`--stage 2`（与 M9-1/2/3 并行），硬前置 `M9-0`。**
四条判据：① **crate 与传输都是 M9-0 的产物** ⇒ 放 M10 会制造一条"M10 依赖 M9-0"的跨波硬前置，而 M10 的其余 12 片一条都不需要 `mc-cloud`；② **产物零重叠**（`mc-cloud/src/runtime.rs` + `routes/cloud_runtime.rs` 与 M9-1/2/6 的 `{billing,subscriptions,webhook}.rs` 互不相交）⇒ 可与 M9 stage 2 并行，不拖长 M9 的墙钟；③ **owner 单元格迁移本来就归 M9-10**（`docs/62` §9.2）⇒ 账与片落在同一波次，收口动作一处完成；④ **不给 M10 添一个"非收尾"的路由面**（M10 已经因为 `M3+` 尾账带了 17 行，再加 11 条 cloud-runtime 会让"终波"混入第二个域）。

**本片已建该 issue**（不留给"将来某轮谁想起来"）：`--parent 01a0c737…`（`LUM-1814`）、`--status backlog`、`--stage 2`，正文含**逐字写集 + 硬前置 + ⑦ 预测 + 写集审计两类**。见 §7.3 / §8.2。
**若 M9-0 尚未起手**：要求 M9-0 在其写集里**预声明 2 行**（`crates/mc-cloud/src/runtime.rs` 的 `pub mod` + `routes/cloud/mod.rs` 的 `pub mod runtime;`）—— 这是一条**追加请求**，不阻塞（若 M9-0 已落地，`M9-11` 自己加那 2 行并登记为对冻结文件的最小破例）。

### 5.3 与 `docs/61` §9.2（附件面登记为「W8 尾账」）的关系

`docs/61` §9.2 把附件面 9 条（含 `/uploads/*`、`/api/avatars/*`）裁定为「**W8 尾账**」并**维持 owner 单元格 `M3+`**。W8 已收口（M8-7 INT 合于 2026-09-25）而尾账未派 ⇒ 本波**接手**：B 面正是这 17 行的落点（其中附件 6 行 + 上传/分发 2 行）。**不改 owner 单元格**（`M3+` 保持不变）⇒ ⑦ 的 `owners` 直方图在 B 面逐步清零，口径逐字一致。

---

## 6. 门禁与验收

### 6.1 门 ⑦（路由对齐）—— 逐时点预测

| 时点 | local | implemented（real + ph） | known_gap | owners | baseline | local_only | 备注 |
|---|---:|---|---:|---|---:|---:|---|
| **base `c23fcfad`（本片实测）** | **474** | 391（388 + 3） | **65** | M9 33 / M3+ 16 / M3 11 / M10 5 | 458 | 9（2 ph） | 本片起手 |
| **M10-0 后** | **473** | 391（388 + 3） | 65 | 不变 | **457** | **8（1 ph）** | **−1**（删幽灵占位）+ 基线 458→457；**无新增键、无新占位** |
| M10-1 后 | 474 | 392（389 + 3） | 64 | M10 **4** | 457 | 8 | +1 |
| M10-2 后 | 476 | 394（391 + 3） | 62 | M10 **2** | 457 | 8 | +2 |
| M10-3 后 | 477 | 395（392 + 3） | 61 | M10 **1** | 457 | 8 | +1（**无 fixture** ⇒ ⑨ 不动） |
| M10-4 后 | 478 | 396（393 + 3） | 60 | **M10 0** | 457 | 8 | +1；**⑨ pass 14→32** |
| M10-B1 后 | 483 | 401（399 + 2） | 55 | M3+ **11** | 457 | 8 | +5 新键 + 1 占位升级（real +6 / ph −1） |
| M10-B2 后 | 486 | 404（402 + 2） | 52 | M3+ **8** | 457 | 8 | +3 |
| M10-B3 后 | 492 | 410（408 + 2） | 46 | M3+ **2** | 457 | 8 | +6 |
| M10-B4 后 | 494 | 412（410 + 2） | 44 | **M3+ 0** | 457 | 8 | +2 |
| **M10-9（INT）后** | **494** | **412（410 + 2）** | **44** | **M9 33 / M3 11** | **494** | 8（1 ph） | `--write-baseline` 457 → 494 |

**不变式（每片自检）**：`implemented + known_gap == 456`、`unclaimed == 0`、`regressions == 0`、`local_only` 单调不增（本波只减 1）。
**账目核对**：本波关 **21** 条缺口（A 5 + B 16）+ **1** 条占位升级 ⇒ `known_gap 65 → 44` ✓（`M9 33 + M3 11`）。
**只有 M10-1/2/3/4 + B1/B2/B3/B4 八片会动 `local`/`implemented`**；M10-0（除 −1 与基线）、M10-5/6/7/8（0 路由）、M10-9（除基线）**绝不新增注册键**。

> ⚠️ 上表是**预测**（本片不写代码）。每片起手**必须重取当轮 base sha 与实测读数**，不许直接抄本表（`docs/37` §46 的 lesson：口径类片合入的瞬间，引用旧口径的表全部过期）。
> ⚠️ 若 `M9-11` 先落地，`owners.M9` 会变（`M9 33 → M9 22`），但 `local`/`implemented`/`known_gap` 的增量与上表**逐字相加**（`M9-11` +11 键）⇒ INT 以**当轮实测**为准。

### 6.2 门 ⑨（契约等价）—— 本波是**单波最大位移**

```
$ cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json   # 起手：绿（逐字节）
totals: fixtures 365 · pass 14 · mismatch 23 · unmounted 22 · placeholder 0 · unevaluable 306
contract_equivalence_rate 0.038356 · mounted_equivalence_rate 0.378378 · offline_decidable {fixtures 59, pass 14}
```

（本轮实测：`--write` 到临时文件后与提交版 **sha256 逐字节相同** = `888c6a27…` ⇒ 起手 ⑨ 绿且可复算。）

**M10 相关 fixture（18 条，逐字实测）**：

| 路径 | 条数 | `actor` | `via` | 现状 | 期望 | 转绿片 |
|---|---:|---|---|---|---|---|
| `/api/config` | **17** | 全 `anonymous` | 16 `handler` + 1 `router` | `unmounted`（本地 404） | 200 + JSON 对象 | **M10-4** |
| `/health` | **1** | `anonymous` | `router` | `unmounted`（`no route: 404 with empty body (axum fallback), expected 200`） | 200 + JSON 对象 | **M10-1** |
| `/healthz` / `/readyz` / `/health/realtime` | **0** | — | — | — | — | 无 fixture（**M10-2/3 的证据必须自造**：§2.6 的 `golden-local`） |

| 时点 | pass | mismatch | unmounted | unevaluable | contract rate | mounted rate |
|---|---:|---:|---:|---:|---:|---:|
| base（实测） | 14 | 23 | 22 | 306 | 0.038356 | 0.378378 |
| M10-1 后 | 15 | 23 | 21 | 306 | 0.041096 | 0.394737 |
| M10-4 后 | **32** | 23 | **4** | 306 | **0.087671** | **0.581818** |
| M10-B1…B4 后 | 32 | 23 | 4 | 306 | 同上 | 同上 |

* `mounted_equivalence_rate = pass / (pass + mismatch)`（**不含 `unmounted`**，`crates/mc-conformance/src/lib.rs:781-786` 实测）⇒ 32/55 = 0.581818。
* `pass` 最终 **14 → 32**（+18）＝ M7-14（+7）的 **2.5 倍**；`unmounted 22 → 4`（剩下的 4 = 3 条 `POST /api/webhooks/stripe`〔M9-6〕 + 1 条 `/users/me`〔M1 面，非本波〕）。
* **B 面不动 ⑨**：`M3+` 的 17 行里只有 **2 条** fixture（`DELETE /api/attachments/{uuid}` 404、`GET /api/attachments/{uuid}/download` 200），且**两条都是 `actor=member`** ⇒ 在 `--no-db` 层恒 `unevaluable`（实测）。
* ⇒ **`crates/mc-conformance/report.json` 的刷新归 M10-9**（先例：M8-6 的 composio 那条 `unmounted → pass`），**在计划里就指定归属**，不等某一轮门 ⑨ 红了再归因。

### 6.3 门 ⑩（文件大小）—— 预飞

门 ⑩ 只扫 `git ls-files` 的**代码**路径：`crates/**/*.rs`、`apps/**/*.rs`、`scripts/**/*.{py,sh}`、`.github/workflows/*.yml`（`scripts/file_size_check.py` 的 `SCOPE` 实测；**`docs/` 与 `contracts/` 不在内** ⇒ 本片与 `deploy/Dockerfile`、`contracts/golden-local/**` **都不受此门**）。
本轮实测：`file_size_check: limit=800 scanned=1147 baseline=10 violations=0`。

| 本波要新建/改的**在扫描范围内的**文件 | 预估 | 风险与对策 |
|---|---:|---|
| `crates/mc-http/src/routes/probes/realtime.rs` | ~250 | 安全（快照构造 + 门） |
| `crates/mc-http/src/routes/probes/ready.rs` | ~250 | 安全（缓存 + 三态） |
| `crates/mc-http/src/routes/config.rs` | ~350 | 安全（17 字段装配 + flag） |
| `crates/mc-http/src/routes/attachments/{read,download,delete}.rs` | 各 ~200–450 | 安全；若 `read.rs` >700，按"元数据 / 下载 URL"再拆 |
| `crates/mc-http/src/routes/quick_actions/{list,lifecycle,invoke}.rs` | 各 ~200–450 | 安全（上游 `quick_action.go` 一个文件，按生命周期拆 3 个） |
| `crates/mc-repos/src/attachment.rs` | ~300–500 | 安全 |
| `crates/mc-bench/benches/*.rs` | 各 ~150–250 | 安全 |
| `scripts/stop_condition.sh` | ~200–350 | 安全（**在扫描范围内**：`scripts/**/*.sh`） |
| `.github/workflows/ci.yml` | 153 → ~175 | 安全 |
| `crates/mc-ws/src/hub/mod.rs` | 现 >600？⇒ **起手先 `wc -l`** | **中危**：计数器若塞进 `hub/mod.rs` 会逼近 800 ⇒ 计数器一律落**新文件** `hub/metrics.rs`，`hub/mod.rs` 只加字段与自增点 |
| `crates/mc-http/src/routes/mount.rs` | 476 → ~500 | 安全 |
| `crates/mc-http/src/routes/issues/mod.rs` | 现 >800？⇒ 起手 `wc -l` | 安全（B1 只**改一行**，方向是减少或持平） |

`scripts/file_size_baseline.tsv`（10 条存量）**本波不动**（没有任何一片被列入白名单）。

### 6.4 迁移条数 = **0**（本波 A/B 两面）

| 面 | 需要新表/列？ | 证据 |
|---|---|---|
| `/api/config` | **0** | `AppConfig` 17 字段全部来自 env + crate 常量 + `mc-storage`/`mc-feature-flags` 只读查询；上游 feature flag **没有表**（`EnvProvider("FF_")` + 可选 YAML 文件，`featureflag/config.go:136-161`）⇒ 本地也不需要 |
| `/health*` | **0** | readiness 读**已有**的 `schema_migrations`（`mc-migrate::verify`） |
| `/health/realtime` | **0** | 进程内计数器 |
| `M3+` 17 行 | **0** | `attachment` / `comment` / `quick_action` / `user.avatar` 等表**已在** `migrations/upstream/`（B 面各片起手必须逐表复算并写进自己 DoD） |
| bench | **0** | 只读既有表（播种用既有 schema） |

⇒ 本波**不写任何 `migrations/**`**、不刷 `contracts/upstream-apply-exceptions.tsv`；门 ⑧ 在 M10-9 跑一次（`--with-db`）。

### 6.5 每片 DoD（通用 + 专属）

**通用（每片都跑，命令见 §10 命令 7）**

1. `bash scripts/gates.sh` **8/8 绿**；碰 DB 的片（B1/B2/B3/B4、M10-2、M10-6）追加 `--with-db` **10/10**。
2. ⑦ 读数与 §6.1 该片行一致（`implemented + known_gap == 456`、`regressions == 0`、`local_only` 不增）。
3. 形态门：**不得**引入 `MISSING_ALIAS` / `MISSING_EXACT` / `EXTRA_ALIAS`（本波 `dual-form required: 0` ⇒ 只注册上游字面量形态）。
4. ⑩：新文件 ≤800 行；`scripts/file_size_baseline.tsv` 不动。
5. 每条路由至少一条用例，且**不用** `health::placeholder`（本波 anchor 已删该函数 ⇒ 新片没有退路）。
6. **写集审计两类**：① 新建文件是否在 anchor 骨架内；② 为让新文件可见改了哪个既有文件（逐字列出行号）。
7. 偏离（未接线项 / 已知差异 / "不做"项）写进 `docs/32` §9.14 的**自己那一段**（号段起手复核）。

**专属**

| 片 | 专属验收 |
|---|---|
| M10-0 | ⑦ **逐字**：`local 474 → 473`、`implemented 391` 不变、`known_gap 65` 不变、`local_only 9 → 8`、`baseline 458 → 457`；**`implemented_placeholder` 不变（3）**；`cargo metadata` 通过；`health::placeholder` 删除后**无 dead_code**（门 ③ `-D warnings` 绿）；§9.14 落地；**不得**注册任何 5 键中的占位（否则 ⑨ 会 `mismatch`） |
| M10-1 | `/health` 200 + `{status:"ok"}` + `pid`/`commit`/`started_at`（后三者 omitempty）；**不触库**（无 `MULTICA_TEST_DATABASE_URL` 也可 200）；`started_at` 是**进程启动时间**（不是请求时间，用于"回答者是我刚起的那个进程"判定，上游注释逐字）；**⑨ `health/001` 转 pass** |
| M10-2 | `/healthz` 与 `/readyz` **同一 handler**；三态：库不可达 ⇒ **503** `checks.db=error` + `migrations=unknown`；迁移未齐 ⇒ **503** `migrations=out_of_date`（**乱序补丁漏记账**的用例必须造出来：版本号低于已应用者）；齐 ⇒ 200 `{status:"ok",checks:{db:"ok",migrations:"ok"}}`；**3 秒缓存 + 单飞**（两次并发只 `Ping` 一次，用例断言）；复用 `mc-migrate::verify`（禁止自写 SQL） |
| M10-3 | 快照 **13 顶层键 + `daemonws{14}`** 逐字（键名一字不差）；`redis` 子树**照发**单副本缺省（§2.3 裁定）；**四态访问门**（200/401+`WWW-Authenticate`/loopback 200/转发头或非 loopback ⇒ **404**）；`Cache-Control: no-store`；计数器在 `mc-ws/src/hub/metrics.rs`，`hub/mod.rs` 只加字段与自增点；**至少 3 个计数器**有"跑一次真实连接→值增长"的用例 |
| M10-4 | 17 字段逐条对齐 `90e0bdf`（`omitempty` 行为也要对齐）；6 个 flag 键**逐字**（含"3 个兼容键恒 true"与"2 类键**不得出现**"）；4 个能力声明**逐条实测**（3 真 1 假，见 §2.2）；匿名可读 + **不触库**；`isOfficialCloudDaemonConfig`（`multica.ai`）抑制分支 + `normalizePublicURL` 去尾斜杠；**⑨ 17 条 fixture 转 pass**；**不得**序列化 `mc_config::Config` |
| M10-5 | `contracts/golden-local/**` 用**抽取器同款格式**（`schema_version 1`）且 `json_subset` **非空**；覆盖：config 17 字段的**关键子集**（≥6 条）、`/health` 1 条、`/healthz`+`/readyz` 的三态 2 条、`/health/realtime` 四态 4 条；回放命令 `--golden contracts/golden-local` ⇒ `mismatch 0 ∧ unmounted 0`；**不动 `contracts/golden/**`**（365 的分母不准变） |
| M10-6 | `criterion = "0.7"`（MSRV 1.80 的**唯一**可选档，§2.5）；`[[bench]]` 必须 `test = false`；3 个热点 bench 可跑；**无库即红**；`docs/fixtures/bench-baseline.json` 由**起手段**先落；`statement_cache_capacity` **显式** + 一条"第二次执行不再 Parse"的用例；`Cargo.lock` **只被本片改一次** |
| M10-7 | `deploy/Dockerfile` 多阶段（builder + slim runtime、非 root、`ENTRYPOINT` = `mc-server`）；门 `image` **只进 `ALL_GATES`、不进默认集合**（`bash scripts/gates.sh` 仍 8/8、`--with-db` 仍 10/10）；CI 新增 job `image` 且**只调** `bash scripts/gates.sh --only image`；本地无 docker（实测）⇒ 证据 = **CI job 的绿**；helm / npm / homebrew **明确不做**（§9.8） |
| M10-8 | `scripts/stop_condition.sh`：逐项打印 `T1-n` 的**期望值 / 实测值 / PASS-FAIL**，末尾 JSON（可机器解析），exit 0 仅当 T1 全绿；**不改任何既有门**；`docs/65-STOP-CONDITION.md` = 向运维/后续 wave 解释每条判据的**判据来源**（命令 + 期望） |
| M10-B1 | 6 行逐条；`/api/issues/{id}/attachments` 占位升级（`real +1 / ph −1`，`local` 不变）；签名下载（`/signed-download`）用 `mc-storage` 的 HMAC（**不引入 CloudFront**，与 §2.2 第 2 行一致）；跨 workspace ⇒ 404；`cdn_signed=false` 时 `/api/config` 的键**不出现**（与本片互为断言） |
| M10-B2 | `POST /api/upload-file`（multipart + 大小上限 + 类型白名单）；`GET /uploads/*`（**路径穿越**必须拒：`..` / 绝对路径 / 符号链接逃逸，三条反例）；与 B1 共用 `mc-storage` 出口与键校验 |
| M10-B3 | 6 行逐条；`/api/quick-actions/` 4 条**双形态**？**否** —— 上游 `r.Get("/")` 在 `r.Route("/api/quick-actions", …)` 里 ⇒ **本波 `dual-form required: 0` 只覆盖 M10 的 5 条**；`M3+` 的 `GET|POST /api/quick-actions/`、`PATCH|DELETE /api/quick-actions/{id}/` **带尾斜杠**（`docs/fixtures/upstream-routes.tsv` 逐字）⇒ 这 6 行必须**按上游字面量**注册（带斜杠的就是带斜杠），并**再跑一次** `slash_alias_audit.py` 确认没有 `MISSING_EXACT`；`render`/`run` 是 issue 侧 plain 注册 |
| M10-B4 | `/api/avatars/{sig}/*`（签名校验 + 非签名 ⇒ 403）；`GET /ws`（**复用已交付 `mc-ws` hub**：token 认证 → `handle_websocket`，不得新写 hub）；`sub-issue-preview`（human-only + 权限折叠） |
| M10-9（INT） | 三件套刷新（`route-parity-baseline.json` 457 → **494**、`report.json` `--write`、⑩ 复算）；终态向量 T1 的**当轮**逐项报告；**全部残余项逐条登记**（含 §9.9 的差额清单、§9.3 的 local_only 8 条、2 个字段级差异、`/users/me` 那条 unmounted、bench 基线与阈值、helm/镜像的"不做"）；**无新功能代码** |

> ⚠️ **M10-B3 的一条形态纪律必须写进它的 DoD**：本波的 `m10-declared-routes.tsv` **只声明 M10 的 5 条**（`dual-form required: 0`）；**`M3+` 的 17 行不在该表内** ⇒ B 面各片**必须**用 `docs/fixtures/upstream-routes.tsv` 的 `M3+` 行自行做形态预判（`GET|POST /api/quick-actions/`、`PATCH|DELETE /api/quick-actions/{id}/` 上游**带**尾斜杠 ⇒ 本地必须注册**带**尾斜杠那一形态），这是"预测表覆盖不到的地方"的**标准做法**（先例：M8 `issue_pr` 的 1 行）。

---

## 7. 排期与硬前置链

### 7.1 硬前置链

1. **`M10-0` 是所有 M10 片的硬前置**（共享 `routes/{mod,mount}.rs`）。
2. **`M10-1/2/3` 只依赖 M10-0**；`M10-4` 额外依赖 `mc-feature-flags` 的 `frontend.rs`（在 M10-4 自己的写集里）。
3. **`M10-5` 依赖 M10-1…M10-4**（对账对象是它们）；**`M10-8` 依赖 M10-1…M10-7**（向量里含 bench/发布项）。
4. **`M10-6` 无前置**（新 crate + dev-dep）⇒ 可最早跑，但 `Cargo.lock` 与 `M9-0` 不得同轮。
5. **`M10-B1 → M10-B2`**（同一批 `attachment` 与 `mc-storage` 出口）。
6. **`M10-9` 是全波收口**（唯一写 `report.json` 与基线者）。
7. **`M9-11`（cloud-runtime 11）的硬前置 = `M9-0`**（`mc-cloud/src/transport.rs`）⇒ 与 M10 的任何片**零依赖**。

### 7.2 槽位分配（并发 3；两波并行时）

| 轮 | M9 波（保 1–2 槽） | **M10 波（本波）** | 说明 |
|---|---|---|---|
| R1 | — | **M10-0** | 与 `M9-0` **不得同轮**（同 `routes/{mod,mount}.rs` + `Cargo.lock`） |
| R2 | `M9-0`（若 R1 空着） | **M10-1 ∥ M10-2 ∥ M10-3** | 与 M9-0 无共享文件（`probes/*` + `mc-ws`）⇒ 可同轮 |
| R3 | M9-1 ∥ M9-2 ∥ `M9-11` | **M10-4 ∥ M10-B1 ∥ M10-6** | M10-6 改 `Cargo.lock` ⇒ **不与 M9-0 同轮**（M9-0 在 R2） |
| R4 | M9-3 ∥ M9-4 ∥ M9-5 | **M10-5 ∥ M10-B2 ∥ M10-B3** | — |
| R5 | M9-6 ∥ M9-7 ∥ M9-8 | **M10-7 ∥ M10-8 ∥ M10-B4** | — |
| R6 | M9-9 ∥ M9-10 | **M10-9（INT）** | 两个 INT **不得同轮**（同刷基线文件）；若撞轮，后起者让位（先例：`LUM-2092` 的让位定式） |

> ⚠️ 上表是**基线排法**：真实派发以「谁有空位谁起」为准（`docs/64` §37 §? 的空位纪律：**空位只认当轮当场读数**）。
> **唯一硬规则**：`M10-0` / `M9-0` / `M10-6` 三者**两两不得同轮**（共享 `routes/{mod,mount}.rs` 或 `Cargo.lock`）；两个 INT 不得同轮（同一基线文件）。

### 7.3 子 issue 一览（全部 `backlog`；M10 的 `--parent` = `LUM-2099`，M9-11 的 `--parent` = `LUM-1814`）

| # | 切片 | 子 issue | parent | stage | 路由 | 面 |
|---|---|---|---:|---|---:|---|
| 1 | M10-0 anchor | **`LUM-2102`**（`01a0db32-f5a1-76aa-848e-446db25c2158`） | `LUM-2099` | 1 | 0（−1） | A |
| 2 | M10-1 live 探针 | **`LUM-2103`**（`01a0db32-f60b-73e8-b636-ee6bc791fcdf`） | `LUM-2099` | 2 | 1 | A |
| 3 | M10-2 ready 探针 | **`LUM-2104`**（`01a0db32-f676-7399-8975-9e319f853e66`） | `LUM-2099` | 2 | 2 | A |
| 4 | M10-3 realtime 指标探针 | **`LUM-2105`**（`01a0db32-f6d2-784b-ab09-3d4ab251dcc9`） | `LUM-2099` | 2 | 1 | A |
| 5 | M10-4 `/api/config` | **`LUM-2106`**（`01a0db32-f72d-7b2d-bed3-99d3de9492e7`） | `LUM-2099` | 3 | 1 | A |
| 6 | M10-5 契约对账 | **`LUM-2107`**（`01a0db33-163a-762a-b377-7e7f623117a1`） | `LUM-2099` | 4 | 0 | A |
| 7 | M10-6 性能 bench | **`LUM-2108`**（`01a0db33-1692-740d-9b10-a2a7ff6cde08`） | `LUM-2099` | 3 | 0 | A |
| 8 | M10-7 发布面 | **`LUM-2109`**（`01a0db33-16e6-7005-b618-0f278cced93f`） | `LUM-2099` | 5 | 0 | A |
| 9 | M10-8 停止条件 | **`LUM-2110`**（`01a0db33-1732-78d6-b0d6-c42c229f0e89`） | `LUM-2099` | 5 | 0 | A |
| 10 | M10-9 INT | **`LUM-2111`**（`01a0db33-178c-7091-ac20-6caa3dd47b89`） | `LUM-2099` | 6 | 0 | A |
| 11 | M10-B1 附件面 | **`LUM-2112`**（`01a0db33-17eb-7301-939d-715beb1ff907`） | `LUM-2099` | 3 | 6 | B |
| 12 | M10-B2 上传/分发 | **`LUM-2113`**（`01a0db33-183d-7870-becf-d4e9bfa32c00`） | `LUM-2099` | 4 | 2 | B |
| 13 | M10-B3 quick-actions | **`LUM-2114`**（`01a0db33-188c-7166-ad5c-89071b2de4d6`） | `LUM-2099` | 4 | 6 | B |
| 14 | M10-B4 avatars + /ws + preview | **`LUM-2115`**（`01a0db33-18dc-79ea-a5b2-4029d7809698`） | `LUM-2099` | 5 | 3 | B |
| 15 | **M9-11 cloud-runtime**（尾账，§5.2） | **`LUM-2116`**（`01a0db33-1928-741b-9df1-cd2247e4d90f`） | `LUM-1814` | 2 | 11 | M9 尾 |

（路由账：A 5 + B 17 = 22；stage 分布 `1 / 3 / 3 / 3 / 3 / 1` = 14 个 M10 issue；另加 1 个 M9 尾账 issue。）

> 晋升规则与 M5…M9 相同：`backlog → todo` 才起跑；同 stage 内可并行；上一 stage 未合不进下一 stage。
> ⚠️ **派发提示**：15 个 issue **全部无 assignee** ⇒ 每次晋升都必须 `assign --to-id 3c6087f9-f768-45a0-9b07-979f7d4fabf5`（照 `docs/60` §7.4 的教训），并 `--no-start` 记录（避免与 status 变更重复起 run）。

---

## 8. 子 issue 一览（逐条要点）

> 每条子 issue 的正文里都带：**逐字写集** + **硬前置** + **`--parent`** + **`--stage`** + **⑦/⑨ 预测** + **写集审计两类**。下列是"一句话要点 + 该片最容易被跳过的那一条"。

### 8.1 A 面（10 条）

| 片 | 一句话 | 最容易被跳过的那条 |
|---|---|---|
| M10-0 | anchor：4 个桩 + 删幽灵占位 + 基线例外 + §9.14 | **不得**给 5 条上游键注册占位（否则 ⑨ 从 `unmounted` 变 `mismatch`，`23 → 41`） |
| M10-1 | `/health` live，不触库 | `started_at` 必须是**进程启动时间**（上游用它判"回答者是不是我刚起的进程"） |
| M10-2 | `/healthz`+`/readyz`，同一 handler | **乱序补丁漏记账**的反例用例（只比"有最新一行"会漏） |
| M10-3 | `/health/realtime` + 访问门 + `mc-ws` 计数器 | `redis` 子树**照发**单副本缺省（不许删键）；`X-Forwarded-*` 存在即按代理处理 ⇒ 404 |
| M10-4 | `/api/config` 17 字段 + 6 flag + 4 能力声明 | 4 个能力声明**逐条实测**（3 真 1 假，§2.2 第 13–16 行）——**不许**照抄上游的 `true` |
| M10-5 | `contracts/golden-local/**` 字段级对账 | ⑨ 的 17 条 fixture 的 `json_subset` 是**空的** ⇒ 不补字段级对账 = 契约没人验 |
| M10-6 | `mc-bench` + criterion 0.7 + 3 热点 | `[[bench]] test = false`；`statement_cache_capacity` **显式**（R10） |
| M10-7 | `deploy/Dockerfile` + 门 `image` + CI job | 门 `image` **不进默认集合**（否则全仓 "8/8" 变成 "9/9"，几十份文档要改） |
| M10-8 | `scripts/stop_condition.sh` + `docs/65` | 向量里的**每一个**期望值都要能一键复算（禁手抄） |
| M10-9 | INT：三件套 + 终态判定 + 登记 | `report.json` 与基线**只有它**能写；残余项**逐条**登记 |

### 8.2 B 面 + M9 尾账（5 条）

| 片 | 一句话 | 最容易被跳过的那条 |
|---|---|---|
| M10-B1 | 附件读/删/下载/签名下载 + `issue attachments` 占位升级 | 占位升级是 `real +1 / ph −1`（`local` 不变）——**不是**新增键 |
| M10-B2 | `POST /api/upload-file` + `GET /uploads/*` | `/uploads/*` 的**路径穿越**三条反例 |
| M10-B3 | quick-actions 6 行 | 上游这 4 条**带尾斜杠**（`/api/quick-actions/`、`/api/quick-actions/{id}/`）⇒ 形态预判要自己从 `upstream-routes.tsv` 做（`m10-declared-routes.tsv` 只覆盖 M10 的 5 条） |
| M10-B4 | avatars + `/ws` + sub-issue-preview | `/ws` 必须**复用**已交付的 `mc-ws` hub，不许新写 |
| **M9-11** | cloud-runtime 11 条（`mc-cloud` 出站代理）+ owner 单元格迁移 | 它**不是** M10 的子 issue（`--parent` = `LUM-1814`）；`docs/62` §9.2 已把 crate 与迁移执行点都定好，本片只是把它**建出来** |

---

## 9. 差异与口径修订（逐条回答立项描述里的 10 个必答问题）

### 9.1 口径修订一：**W10 的"主体"是交付物，不是路由** —— 裁定切片划分依据 + 例外纪律

* 事实：路由面 5 条（§1.1），而 `plan1.md` §5 给 W10 的四件事（前端兼容验证 / 性能 bench / 双跑对账 / 发布）**一条路由都不占**；`plan1.md` §3.3 给 W10 的 crate 是 `mc-bench`（新建）+ `mc-conformance`（已存在）。
* **裁定**：本波的切片划分依据 = **交付物**（每个交付物一片，路由只是 A 面里的一格）。这是全仓**唯一**一个非路由驱动的波。
* **例外纪律（三条）**：① **路由面仍按路由切片**（`/health` / `/healthz`+`/readyz` / `/health/realtime` / `/api/config` 四片），理由是四条键的上游文件与语义各不相同（§1.2）；② **非路由片一律给"0 路由 + 可执行判据"**（bench = 相对阈值 + 报告文件；对账 = `--golden contracts/golden-local` 的 `mismatch 0`；发布 = 门 `image`；停止条件 = `stop_condition.sh` exit 0）—— **不接受"人工验证"**；③ **本仓最后一块尾账（`M3+` 17 行）并入本波 B 面**（§9.10），否则终态向量会挂着一个无人认领的 owner。
* **唯一一次基线破例**：M10-0 删 `/api/feature-flags`（§9.3）⇒ 该键在 `route-parity-baseline.json` 里，删除会让 `regressions` 判红（`route_parity.py:539` = `set(baseline) - live`）⇒ **删除与 `--write-baseline` 必须同一次提交**（先例：M4-0 删 6 键、M6-0 删 4 键）。本片（docs 片）**不跑** `--write-baseline`；M10-9（INT）再刷一次收口（457 → 494）。

### 9.2 口径修订二：🔴 **这 5 条不插进 M9 波**（本片最重要的裁定）

**结论：不插。** 四条判据：

1. **不阻塞 M9 起跑**：M9-0 的硬前置是「M7 全合 + M8 全合」（`docs/62` §7.1），与 M10 的 5 条**无关**；反过来，`/health*` + `/api/config` 也不需要 M9 的任何产物。
2. **与 M9 的共享件冲突**：M9-0 要动 `crates/mc-http/src/state.rs`（+cloud/entitlement 两组字段）、`routes/{mod,mount}.rs`、`Cargo.lock`；M10-0 也要动 `routes/{mod,mount}.rs` ⇒ **两者不得同飞**。若把 5 条插进 M9，就会在 M9 波内制造第二个 anchor 级写者，把"三片并行"退化成"anchor 串行"。
3. **`report.json` 归谁刷**：只有一处能刷（否则两个 INT 撞同一文件）。`docs/62` §7.3 已把 M9 的刷新定在 `M9-10`；本波定在 `M10-9`。若 5 条归 M9，⑨ 的 18 条位移会落在 **M9 的 INT** 里，而本波声明"本波是单波最大位移"就成了一句空话（口径与事实不符）。
4. **是否让 `owners.M10` 提前归零**：会——插进 M9 的代价是 `owners.M10` 在 W9 期间归零（报表好看），收益为零（`owners` 直方图只统计 `known_gap`，提前归零不减少任何工作量），而**代价**是 W9 的墙钟被自己的 anchor 串行拖长（M9 有 11 片、5 个 stage，插入 5 条要多 1 个 anchor 轮）。

⇒ 排期上两者**并行**（§7.2）：M10-0 与 M9-0 错轮，其余片完全并行。

### 9.3 口径修订三：本地 `/api/health` + `/api/health/db` **保留**；`/api/feature-flags` **删除**（`local_only 9 → 8`）

| 键 | 本地现状 | 裁定 | 判据 |
|---|---|---|---|
| `GET /api/health` | `mount.rs:28`，真实现（含 DB 状态字段）；被 **`apps/mc-cli/src/main.rs:42`** 当探针用；被 `mc-openapi` 的文档测试断言；被 `mc-conformance/tests/golden.rs:167` 的自造 fixture 断言 | **保留（不收敛）** | ① 它**不是**上游 `/health` 的别名：语义是"服务 + DB 综合"，而上游 `/health` 是**纯 liveness**（不触库）；把它改成上游语义会**破坏 CLI 探针**与两个既有测试；② 收敛的收益 = `local_only 9 → 7`，但 `local_only` 是**登记项**（`route_parity.py:20` 逐字：`registered here, absent upstream (informational)`）、**不进任何门的分子分母**；③ 代价 = 牵连 4 处（CLI / openapi / conformance 自造 fixture / 台账）而**收益为零** |
| `GET /api/health/db` | 同上，DB 专用探针 | **保留** | 同上 |
| `GET /api/feature-flags` | `mount.rs:61` = `health::placeholder`（**501**），上游**根本没有这个路由**（`grep -c 'feature-flags' docs/fixtures/upstream-routes.tsv` = **0**） | **删除**（M10-0 预删 + 同提交刷基线） | ① 它是**幽灵路由**（501 + 永远 `not_implemented`），而它想表达的语义（UI 读 flag）**由 `/api/config` 的 `feature_flags` 字段正式承担**（§2.2）⇒ 留着就是第二个真相源；② 唯一引用者 `mount.rs:61`，删后 `health::placeholder` 变成死代码 ⇒ 一并删（否则门 ③ `-D warnings` 红）；③ 先例一致：M4-0 删 6 个、M6-0 删 4 个自造占位 |

**对两个数的影响（必须记对）**：`local_only 9 → 8`、`local_only_placeholder 2 → 1`；**`unclaimed` 不变（0）**、`implemented`/`known_gap` 不变（该键不在上游 456 里）；`baseline 458 → 457`（同提交刷新）。
⇒ **不压 `local_only` 到 7** —— 保留的两个键是**有意的本地能力**，登记在 §9.9 的 `local_only` 登记表里（"有主"即合规）。

### 9.4 口径修订四：`GET /api/config` 的契约（17 字段，**以 `90e0bdf` 为准**）+ 0 迁移

* 逐字字段集、来源与本地取值 = §2.2 的表（17 个键 + 6 个 flag 键 + 2 类"不得发布"的键）。
* **对照 17 条 fixture**：它们的 `json_subset` **全部为空**（实测）⇒ 逐字字段集**不能**从 fixture 推出来，只能从 `config_test.go`（`90e0bdf`）的断言推出来 —— 这是本片发现的一个**判据缺口**（0 条 fixture 覆盖字段值）⇒ 由 M10-5 的 `contracts/golden-local/**` 补（§9.7）。
* **三个能力声明的实测裁定（不许照抄上游 `true`）**：`local_worktree_supported = true`（证据：`projects/resource_ref.rs:106-119` + `:280-300` 的能力门）、`agent_conversation_starters_supported = true`（证据：`mc-repos/src/agent/` 的持久化 + `db_tests` 断言）、`comment_delete_keep_replies_supported = true`（证据：`/keep-replies` 路由**已注册** + `soft_delete(id, keep_replies=true)`）；**`issue_create_properties_supported = false`**（证据：`CreateIssueRequest` 无 `properties` 字段 ⇒ serde 静默忽略 ⇒ 正是上游注释警告的失败模式）⇒ **登记为已知差异**（补实现属 M2-A 面，本波不做；客户端 `fail-closed`，故 `false` 是**安全**的一侧）。
* **迁移 = 0**（§6.4 给了逐面证据）。
* **`/api/config` 与 `mc-config` 的边界** = §2.4（两条硬纪律：不序列化 `Config`、新增 env 必须登记）。

### 9.5 口径修订五：`/health/realtime` 的语义是**进程级计数器快照**，判据是"字段名 + 状态码"

* 上游 probe 的是 `realtime.M.Snapshot()`（13 个顶层键）+ `daemonws.M.Snapshot()`（14 个键，挂在 `daemonws` 子键下）；**本地对应物**是 `crates/mc-ws` 的 hub 计数（已交付四则）+ 本波补的累计计数器（`hub/metrics.rs`）。
* **可判定响应契约** = §2.3 的四态表（200 / 401+`WWW-Authenticate` / loopback 200 / 代理或非 loopback 404）+ 字段名逐字 + `Cache-Control: no-store`。**"返回个 ok 就行"被明确否决**：它以字段名与状态码为判据。
* **`redis` 子树**：本仓无 Redis ⇒ 照发单副本缺省（§2.3），**不删键**。

### 9.6 口径修订六：性能 bench 用 `criterion 0.7`；阈值是**相对 + 绝对**两条；"与 Go 版同量级"被替换

* `plan1.md` §4 的选库闸门（crates.io 活跃 + >1M 月下载）⇒ `criterion`（总下载 2.90 亿、近 90 天 5,968 万）；**版本必须 0.7**（0.8.2 的 `rust_version = 1.86` > 本仓 `1.80`）。
* `plan1.md` §3.6 的三条热点（issue 列表 / facets / inbox 游标）+ `R10`（Go 有 prepared statement cache，`sqlx` 需显式持久化）⇒ 最小 bench 清单 + **显式** `statement_cache_capacity` + "第二次执行不再 Parse"用例。
* 🔴 **"与 Go 版并行双跑：同请求同响应"（`plan1.md` §5 的 W10 门禁）在本仓不可执行**（无 Go 工具链：`which go` 为空；无前端资产）⇒ **替换为**：① 本片 §2.5 的相对/绝对阈值；② §9.7 的冻结 golden 逐字段对账。**登记为口径修订**，不是静默放弃。

### 9.7 口径修订七：双跑对账 = **冻结 golden 逐字段对账**（离线、可判定）

* **不采用**"离线替身 + 结构对账"这种含糊形态，也**不允许**"人工比对"。
* 三件套：① `contracts/golden/**`（365 条上游冻结面，pin `90e0bdf`，**不改**）；② `contracts/golden-local/**`（本波自造的**字段级** fixture，`json_subset` 非空；**不进** `contracts/golden/` ⇒ ⑨ 的 365 分母不变）；③ `scripts/mc_golden_local_check.sh`（包一层回放并断言 `mismatch 0 ∧ unmounted 0`）。
* DoD 可判定：`cargo run -q -p mc-conformance -- --no-db --golden contracts/golden-local` 的退出码 + `mismatch/unmounted` 数。

### 9.8 口径修订八：发布制品的适用面 —— **Dockerfile 做（由 CI 判）；helm / 打包分发明确不做**

| `plan1.md` §5 的项 | 上游对应 | 裁定 | 验证方式 |
|---|---|---|---|
| 镜像 | 上游根 `Dockerfile` + `Dockerfile.web` + `docker-compose*` | **做**：`deploy/Dockerfile`（多阶段、非 root、`mc-server` 入口） | **本机无 docker**（`which docker` 为空，实测）⇒ 证据 = **CI 新 job `image`**（`bash scripts/gates.sh --only image` 的唯一调用者，runner 有 docker）；门 `image` **不进默认集合**（`bash scripts/gates.sh` 仍 8/8、`--with-db` 仍 10/10） |
| helm | `deploy/helm/multica/**`（8 文件 713 行；`backend.yaml` 描述的是 **Go 镜像**、`frontend.yaml` 描述 **web Pod**、`postgres.yaml` 起 PG） | **不做**（显式） | 理由：① 逐字搬运会产出**一份无人能验证**的制品（本仓无前端 Pod 的镜像、无 Go 镜像）；② 停止条件的向量（⑦/⑨/⑩/⑧/门禁/CI）**不包含**它；③ `deploy/helm` 作为**只读参考**登记在 §10 命令 9 |
| CLI 分发 | 上游 `cmd/multica`（安装器/自更新） | **部分做**：本仓**已有** `apps/mc-cli`（62 行）⇒ 本波只要求 `cargo build --release --locked -p mc-cli` + 一条冒烟（`mc-cli` 的探针指向 `/api/health`） | **不做**打包渠道（homebrew / npm / install.sh / 自更新）——登记为"不做 + 理由：本仓无分发基础设施，这些渠道不可在本仓验证" |
| `migrations/upstream/` 的发布形态 | — | **不单独打包**：随二进制仓发行，用法逐字 `mc-migrate run --dir migrations/upstream --dir migrations/compat` | 门 ⑥（`--with-db`）已经是这条命令的机器化 |

⇒ **"不做"清单**（显式，不留白）：helm chart、前端镜像、npm/homebrew/install.sh 分发、自更新、真实前端 E2E、真实云侧双跑。

### 9.9 🔴 口径修订九：**停止条件的操作化定义**（本片的头号交付）

**Tier-1 硬向量（12 条，全部可一键复算）**

| ID | 判据 | 期望 | 命令 |
|---|---|---|---|
| T1-1 | ⑦ `counts` | `upstream 456 ∧ implemented_real 456 ∧ implemented_placeholder 0 ∧ known_gap 0 ∧ unclaimed 0 ∧ regressions 0 ∧ local == baseline_routes` | `route_parity.py --json` |
| T1-2 | ⑦ `owners` | **直方图为空**（每个 owner 计数 0） | 同上 |
| T1-3 | ⑦ `local_only` | 每个成员都在 §9.3/T1-3 的**登记表**里（当前 8 条，逐条有理由） | 同上 |
| T1-4 | ⑦b 形态 | `slash_alias_audit.py` exit 0 且 `defect(s) == 0` | `slash_alias_audit.py` |
| T1-5 | ⑨（`--no-db`） | `mismatch == 0 ∧ unmounted == 0` | `mc-conformance --no-db --check report.json` |
| T1-6 | ⑨（`--db-url`） | `unevaluable == 0 ∧ mismatch == 0` | `mc-conformance --db-url … --check` |
| T1-7 | ⑨ 两个 rate | `contract_equivalence_rate == 1.0 ∧ mounted_equivalence_rate == 1.0` | report.json |
| T1-8 | ⑧ schema_drift | `missing == 0`（exit 0） | `schema_drift.py --quiet`（需库） |
| T1-9 | ⑩ file-size | `violations == 0`（清单外 ≤800、清单内只减） | `file_size_check.py --quiet` |
| T1-10 | 门禁 | `gates.sh --with-db` **10/10** 且 `gates.sh --only image` 绿 | 两条命令 |
| T1-11 | CI | 4 个 job（fast / db / contract / image）全绿 | PR 的 CI |
| T1-12 | 对账 | `--golden contracts/golden-local` ⇒ `mismatch 0 ∧ unmounted 0` | `mc_golden_local_check.sh` |

**一键复算**：`bash scripts/stop_condition.sh`（M10-8 交付；逐项打印期望/实测/PASS-FAIL + 末尾 JSON；exit 0 仅当 T1 全绿）。
**当前差额清单（本片实测，base `c23fcfad`）**：

```
⑦ local 474 | baseline 458 | implemented 391 = 388 real + 3 placeholder | known_gap 65 | unclaimed 0 | regression 0 | local_only 9 (2 ph)
   owners = { M9 33, M3+ 16, M3 11, M10 5 }                 # 和 = 65 ✓
⑨ fixtures 365 | pass 14 | mismatch 23 | unmounted 22 | placeholder 0 | unevaluable 306
   contract_equivalence_rate 0.038356 | mounted_equivalence_rate 0.378378 | offline_decidable {59, 14}
⑩ scanned 1147 | baseline 10 | violations 0
⑧ schema_drift: 本片未跑（需库；`--with-db` 归各片/M10-9）
⑥ 门禁: 本片跑 `bash scripts/gates.sh`（8/8，见交付说明）
```

⇒ **停止条件的"剩余距离"** = 本表 + §6.1 的预测逐步清零到 T1-1…T1-12 全绿；**残余例外**（`issue_create_properties_supported=false`、`cdn_signed` 恒 false、`local_only 8`、`/users/me` 那条 unmounted、helm/镜像的"不做"）**必须逐条落在 Tier-2 登记表**里，T2 冻结 = 停止条件可判定。

### 9.10 口径修订十：`M3`(11) / `M3+`(16) 两个尾账的归属 —— **两条都不得再挂"无人认领"**

| 尾账 | 归属 | 理由 | 执行点 |
|---|---|---|---|
| `M3+`（17 行 = 16 缺口 + 1 占位） | **本波 B 面 4 片**（M10-B1…B4） | 它是本仓最后一块没人排期的缺口；`docs/61` §9.2 已把它登记为「W8 尾账」而 W8 已收口；把 16 条缺口另立一个波只会多一轮 anchor + INT + 基线交接，收益为零 | 本片**已建**（stage 3/4/5） |
| `M3`（11 行，全是 `/api/cloud-runtime/*`） | **M9 波次的新片 `M9-11`**（`--parent LUM-1814`，stage 2） | `docs/62` §9.2 已裁定 crate 归属 = `mc-cloud`、owner 迁移执行点 = `M9-10`；上游 `cloud_runtime.go`（208 行）用的是 M9-0 建的同一份 `mc-cloud` 传输 ⇒ 放 M9 波次 = **零新依赖、零新 crate、可与 M9 stage 2 并行** | 本片**已建**；`docs/62` §9.2 的迁移动作（`route-owners.tsv` + `upstream-routes.tsv` 的 11 行 owner `M3 → M9`）由 **M9-10** 执行（`docs/62` 已有裁决，本片不改那两个文件） |

### 9.11 口径修订十一（本片新增发现）：**本仓有两条不同的上游 pin**（⑦ `f41fae6b08fb` / ⑨ `90e0bdf`，差 12 提交）

* 逐字证据见 §1.6。对本波的唯一实质影响：`config.go` 在 `90e0bdf` 多了第 17 个字段 `issue_create_properties_supported`。
* ⇒ 纪律：**读契约字段用 `90e0bdf`，读路由行号用 `f41fae6b08fb`**；两者的差异写进实现片的登记段。**不得**用"两个 pin 是同一个"这个默认假设（此前各波的文档都只引其中一个，本片是第一次显式区分）。

---

## 10. 复算命令（全部只读；下游只读副本只克隆进**本 run 的 workdir**）

```bash
# 前置：本片的上游只读副本（§1.6 的**两个** pin：路由 f41fae6b08fb / fixture 90e0bdf）
#   UP_ROOT=<本 run 的 workdir>；上游 = $UP_ROOT/upstream-ref；本地 = $UP_ROOT/paperclip-rs
cd <workdir> && git clone https://github.com/loulouin/multica upstream-ref   # 仓库地址以项目资源为准
cd upstream-ref && git log --oneline -1
git cat-file -t f41fae6b08fb734afcbd13205c0b3203dd0bc9c6     # ⇒ commit
git rev-list --count f41fae6b08fb734afcbd13205c0b3203dd0bc9c6..90e0bdf830436b3981b32a7017e1c18d41c7cdea   # ⇒ 12
cd ../paperclip-rs && git fetch origin feat/multica-rs-initial && git rev-parse origin/feat/multica-rs-initial   # ⇒ c23fcfad（本片起手）

# 1. 路由表（§1.1/§1.7）
diff <(awk -F'\t' '!/^#/ && $3=="M10"{print $1"\t"$2}' docs/fixtures/upstream-routes.tsv | sort) \
     <(grep -v '^#' docs/fixtures/m10-declared-routes.tsv | tail -n +2 | sort)          # ⇒ 空
awk -F'\t' '!/^#/ && $3=="M10"' docs/fixtures/upstream-routes.tsv | wc -l                # ⇒ 5
awk -F'\t' '!/^#/ && $3=="M3+"' docs/fixtures/upstream-routes.tsv | wc -l                # ⇒ 17
python3 - <<'PY'
import json; d=json.load(open('docs/fixtures/route-parity-baseline.json'))['routes']
print(len(d), 'GET /api/feature-flags' in d)                                             # ⇒ 458 True（§9.3 的删键例外）
PY

# 2. 两个尾账的行与缺口（§1.7/§4.2）
for o in M3 M3+ M10; do printf "%-4s rows=%s\n" "$o" "$(awk -F'\t' -v o=$o '!/^#/ && $3==o' docs/fixtures/upstream-routes.tsv | wc -l)"; done
python3 scripts/route_parity.py --json > /tmp/rp.json
python3 - <<'PY'
import json,collections; d=json.load(open('/tmp/rp.json'))
print(d['counts']); print(d['owners'])
for ow in ('M3','M3+','M10'):
    print(ow, [(x['method'],x['path']) for x in d['known_gap'] if x.get('owner')==ow])
PY

# 3. 上游面测绘与行数（§1.2/§1.6；在 upstream-ref 里跑）
UP=server
wc -l $UP/cmd/server/{health.go,health_realtime.go} $UP/internal/handler/config.go \
      $UP/internal/realtime/metrics.go $UP/internal/daemonws/metrics.go                   # ⇒ 198 106 223 277 60
grep -n 'r.Get("/health\|r.Get("/readyz\|r.Get("/healthz\|r.Get("/health/realtime\|r.Get("/api/config' $UP/cmd/server/router.go
#   ⇒ 1399 / 1400 / 1401 / 1412 / 1478
git diff --stat f41fae6b08fb734afcbd13205c0b3203dd0bc9c6..90e0bdf830436b3981b32a7017e1c18d41c7cdea -- \
  server/internal/handler/config.go server/internal/handler/config_test.go server/cmd/server/router.go
#   ⇒ 只改 config.go(+7) 与 config_test.go(+23)（§1.6）
grep -c 'feature-flags' docs/fixtures/upstream-routes.tsv                                 # ⇒ 0（§9.3 删键判据）
grep -rli 'create table.*feature' migrations/upstream/*.up.sql | wc -l                    # ⇒ 0（§6.4 的 0 迁移）

# 4. ⑨ 的 18 条 fixture 与"json_subset 全空"（§6.2/§9.7）
python3 - <<'PY'
import json,glob,collections
rows=[json.load(open(f)) for f in glob.glob('contracts/golden/**/*.json',recursive=True)]
m10=[r for r in rows if isinstance(r,dict) and r.get('path') in
     ('/api/config','/health','/healthz','/readyz','/health/realtime')]
print(collections.Counter((r['path'],r['actor']['kind']) for r in m10))                   # ⇒ /api/config 17 anonymous ; /health 1 anonymous
print(collections.Counter(bool(r['expect']['json_subset']) for r in m10))                 # ⇒ {False: 18}
PY

# 5. 形态门（§1.4）：本波 0 缺陷 + 六波对照
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m10-declared-routes.tsv      # declared 5 / dual 0 / exit 0
for m in m4 m5 m6 m7 m8 m9; do printf "%-3s " $m; python3 scripts/slash_alias_audit.py --declared docs/fixtures/$m-declared-routes.tsv | grep -E 'dual-form|shapes OK'; done
python3 scripts/slash_alias_audit.py                                                       # 本地实况：0 defect

# 6. ⑩ 预飞（§6.3）
python3 scripts/file_size_check.py | head -1                                               # ⇒ scanned=1147 baseline=10 violations=0
grep -nE 'mc-ws/src/hub|routes/mount.rs|routes/issues/mod.rs' scripts/file_size_baseline.tsv   # ⇒ 空（本波写集不在白名单）
wc -l crates/mc-http/src/routes/{mount.rs,mod.rs,issues/mod.rs,health.rs} crates/mc-ws/src/hub/mod.rs

# 7. 门禁（通用；§6.5）
bash scripts/gates.sh                        # 8/8（本片）
bash scripts/gates.sh --with-db --db-url 'postgres://…@127.0.0.1:5432/<db>'   # 10/10（碰库的片）

# 8. 停止条件的当前差额（§9.9）：T1-1…T1-4、T1-9 可离线复算；T1-5 需先 build
cargo run -q -p mc-conformance -- --no-db --write /tmp/report.json && cmp /tmp/report.json crates/mc-conformance/report.json
python3 -c "import json;d=json.load(open('/tmp/report.json'));print(d['totals']['pass'],d['totals']['unmounted'],d['contract_equivalence_rate'],d['mounted_equivalence_rate'])"

# 9. 发布面参考（§9.8；只读，不改）
wc -l deploy/helm/multica/{Chart.yaml,values.yaml} deploy/helm/multica/templates/*.yaml    # ⇒ 16 216 + 481 = 713
which docker go                                                                            # ⇒ 两者皆空（本片实测）
```

---

**引用与下游**：本片是 `docs/60`（M7）/`docs/61`（M8）/`docs/62`（M9）三份计划的**直接后继**，体例（§0 速览 → §1 测绘 → §2 落点 → §3 写集 → §4 切片 → §5 交界 → §6 门禁预测 → §7 排期 → §8 子 issue → §9 口径修订 → §10 复算）与它们逐一对应。
新增的**独立机制**三件：`contracts/golden-local/**`（字段级对账）、`scripts/stop_condition.sh`（终态一键复算）、门 `image`（非默认集合的发布制品门）。
