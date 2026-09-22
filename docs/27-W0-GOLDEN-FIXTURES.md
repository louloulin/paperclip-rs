# W0-⑤ golden fixture 抽取器 + 回放器（plan1 P5「用上游测试当 oracle」）

> **切片**：W0-C / LUM-1388。**文件范围**：`scripts/extract_upstream_fixtures.py`、`contracts/golden/**`、
> `crates/mc-conformance/**`、本文件。
> **上游 pin**：`github.com/louloulin/multica@90e0bdf830436b3981b32a7017e1c18d41c7cdea`（见 `contracts/golden/PIN`）。
> **plan1 对应**：§2 P5、§5 W0⑤、§8「契约等价率」。**M3 接入点**：`docs/15-M3-PLAN.md` §8.3.4 / §8.5。

## 0. 一句话

本仓是上游 `multica`（Go，41.4 万行测试）的 **Rust 重写**。文档里写「行为等价」是不可证伪的；
本切片把上游测试里**可判定**的请求/响应断言抽成语言无关的 golden fixture，再用
`tower::ServiceExt::oneshot` 打在本仓**真实 router** 上重放，于是「契约等价」变成一个
**可以按 fixture 数出来的比例**，而且分母分子都从同一份报告里数出来 —— 不可能靠遮掉难看的行变好看。

```text
上游 Go 测试 ──extract_upstream_fixtures.py──▶ contracts/golden/**.json ──mc-conformance──▶ 本仓 axum router
   (oracle 源)        (逐条记 skip 原因)              (schema v1 + PIN)          (五类结论 + 比值)
```

本切片**不含**为了过 fixture 而改业务代码：实测出的不等价只登记（§6），不在此处修。

---

## 1. 三份产物

| 产物 | 位置 | 谁写 | 可复现方式 |
| --- | --- | --- | --- |
| golden fixture | `contracts/golden/<domain>/<nnn>-<Test>-L<line>.json`（58 个） | 抽取器 | `python3 scripts/extract_upstream_fixtures.py --upstream <checkout> --check` |
| 抽取台账 | `contracts/golden/stats.json`、`contracts/golden/extraction-report.tsv`（595 行：54 `extracted` / 501 `skipped` / 40 `helper_site`） | 抽取器 | 同上 |
| 等价率报告 | `crates/mc-conformance/report.json`（**stateless 层**，确定性） | 回放器 | `cargo run -p mc-conformance -- --check crates/mc-conformance/report.json` |

`contracts/golden/PIN` 记录上游仓/commit/日期/抽取器路径/schema 版本/扫描目录 —— 换上游基线就是换这个文件，
其他一切由它派生。

---

## 2. fixture schema（v1，冻结）

一个 JSON 文件 = 一条可回放的断言。字段（`crates/mc-conformance/src/lib.rs::Fixture`）：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `schema_version` | int | 必须等于 `1`；与抽取器 `SCHEMA_VERSION` 对齐，不一致**加载即报错** |
| `id` | string | `<domain>/<Test>@<上游文件>:<行>#<用例序号>`，全局唯一、排序稳定 → 报告可字节复现 |
| `method` / `path` | string | path 必须绝对路径；`{name}` 形式的路由参数由 `path_params` 提供 |
| `path_params` | map | `{"testWorkspaceID": "$testWorkspaceID"}` 等 |
| `query` | map | 上游 URL 上的字面量 query |
| `headers` | map | 上游显式设置的请求头（身份头另有通道，见 §4.3） |
| `actor.kind` | enum | `anonymous` / `member`（+ `upstream_identity`：`X-Workspace-ID` 一类按上游原样转发） |
| `body` | JSON \| null | 请求体；不可解析时该 site 直接 skip，不猜 |
| `expect.status` | int | 上游断言的状态码 —— **唯一必填的期望值** |
| `expect.json_subset` | JSON | 响应 body 的递归子集断言（本轮全为空，见 §7.1） |
| `expect.headers` | map | 上游显式断言过的响应头（本轮全为空） |
| `source` | object | `file`/`line`/`test`/`site`/`via`(`router`\|`handler`)/`commit` —— 每条 fixture 都能回溯到上游代码行 |
| `extraction.notes` | array | 抽取期对这条用例做过的「翻译」，**必须能回答"为什么这样回放仍然等价"** |

`Fixture::verify()` 是**加载期**检查：版本、非空、path 绝对、每个 `{name}` 必须有 `path_params`。
坏 fixture 必须让命令失败，不能被静默跳过（`load_dir` 一个文件都不容错）。

例（一条真 fixture，`contracts/golden/health/001-TestHealth-L212.json`）：

```json
{
  "schema_version": 1,
  "id": "health/TestHealth@server/cmd/server/integration_test.go:212#1",
  "method": "GET", "path": "/health", "path_params": {}, "query": {}, "headers": {},
  "actor": { "kind": "anonymous" },
  "body": null,
  "expect": { "status": 200, "json_subset": {}, "headers": {} },
  "source": { "file": "server/cmd/server/integration_test.go", "line": 212, "test": "TestHealth",
              "site": "http.Get", "via": "router",
              "commit": "90e0bdf830436b3981b32a7017e1c18d41c7cdea" },
  "extraction": { "notes": [], "bindings": {} }
}
```

---

## 3. 抽取器（`scripts/extract_upstream_fixtures.py`，1863 行）

```bash
python3 scripts/extract_upstream_fixtures.py --upstream <checkout>            # 写入 contracts/golden
python3 scripts/extract_upstream_fixtures.py --upstream <checkout> --check    # 只比对，字节不同则 exit 1
# 可选：--out <dir>  --scan <子树…>（默认 server/internal/handler server/cmd/server）
```

### 3.1 什么叫「可判定」

一个上游测试只有当下面**每一条**都成立时才被抽成 fixture；任何一条不成立 → 记 skip 原因，**不静默丢**：

1. 请求经过 `site` 之一发出（见 3.2），且 method/path 是**字面量**；
2. 该测试对**状态码**有断言（`w.Code` / `resp.StatusCode` 比较）；
3. 路径与 query 里的符号能解析成字面量或 `$testUserID` / `$testWorkspaceID`（**唯一两个**可绑定符号）；
4. 请求体能解析成 JSON 字面量（或确实没有 body）；
5. 状态码断言唯一（同一请求被断言多次且值不同 → `ambiguous_status`）。

### 3.2 候选点与抽取率（`stats.json`，实测）

| candidate site | 条数 |
| --- | ---: |
| `testutil.Call(t, handler, request)` | 463 |
| `authRequest(t, method, path, body)` | 71 |
| `http.NewRequest(method, url, body)` | 16 |
| `http.Get(url)` | 5 |
| **合计** | **555** |

抽成 fixture：**54 site → 58 fixture**（表驱动用例一个 site 出多条）⇒ **抽取率 9.73%**。

### 3.3 skip 原因分布（501 条，逐条见 `extraction-report.tsv`，行内带 `file`/`line`/`test`/`site`/`reason`/`detail`）

| reason | 条数 | 含义 / 为什么现在抽不了 |
| --- | ---: | --- |
| `request_var_unresolved` | 295 | 请求是变量（helper 拼装、table 字段），静态解析不出字面量 |
| `no_status_assertion` | 89 | 只断言 body/DB，不比对状态码 |
| `body_unresolved` | 74 | body 里有不可静态求值的表达式 |
| `value_unresolved` | 30 | 路径/query 里是不可绑定符号（时间戳、随机 id、helper 返回值） |
| `ambiguous_status` | 12 | 同一请求多个不同状态断言（表驱动分支） |
| `path_not_literal` | 1 | 路径由变量拼接 |
| **合计** | **501** | 555 − 54 = 501 ✓（counts are exhaustive，脚本会自检） |

台账里另有 40 行 `helper_site`：上游测试调用到的请求 helper 定义本身（给 54 条 `extracted` 提供"请求是怎么造出来的"回溯链），**不产生 fixture**。

### 3.4 明确**不在**本轮范围的上游测试（不假装覆盖）

- **handler-direct + `httptest.ResponseRecorder`**：**1484 处**。上游在那里绕过了自己的 router，
  回放出的状态码差异可能是测试级原因而非契约差异，所以不抽（`stats.json::scope.not_extracted` 逐字记录这个数字与理由）。
- **daemon / ws 协议、SQL 期望结果、事件 envelope**：抽取器当前只扫两个 HTTP 目录。
  M3 需要协议 golden 时按 `docs/15-M3-PLAN.md` §8.5 扩 `--scan` 与抽取规则（**这是已知缺口，不是遗漏**）。

---

## 4. 回放器（`crates/mc-conformance`，1615 行 Rust，含 265 行测试）

### 4.1 用法

```bash
cargo run -p mc-conformance                                   # 两层回放，文本报告
cargo run -p mc-conformance -- --json                         # 同上，JSON
cargo run -p mc-conformance -- --list                         # 只列 fixture + 来源，不发请求
cargo run -p mc-conformance -- --filter issues/               # 只跑 id 含该子串的
cargo run -p mc-conformance -- --db-url postgres://…          # 追加 database 层（或 MULTICA_TEST_DATABASE_URL）
cargo run -p mc-conformance -- --no-db                        # 强制只跑 stateless 层
cargo run -p mc-conformance -- --write crates/mc-conformance/report.json
cargo run -p mc-conformance -- --check crates/mc-conformance/report.json    # 漂移 exit 1
cargo run -p mc-conformance -- --require-pass 40              # 低于阈值 exit 3（给 M3 用）
```

退出码：`0` 完成 / `1` 报告漂移 / `2` 用法或加载错误 / `3` `--require-pass` 未达标。

### 4.2 两层（`harness.rs`）

| 层 | 数据库 | 能判定什么 | 确定性 |
| --- | --- | --- | --- |
| **stateless** | `connect_lazy` 指向不可达端口（不拨号、不建库） | 匿名断言，主要是 401/404 一类 | **确定性** → CI 与 `--check` 用这层 |
| **database** | 真 PG + 跑 `migrations/` + 用 `POST /api/workspaces` 造一个种子 user/workspace | 需要身份的断言（本轮 47/58 条 `member`） | 每次跑新建种子身份 → 计数稳定、内容随库变化 |

两层都跑同一 fixture 时，汇总取**更强**的那层（`pass > mismatch > unmounted/placeholder/unevaluable`），
并在行里写明结论来自哪层。装配与 `apps/mc-server` 保持一致（同一个 `AppState` + `apply_default_middleware`），
所以测的是真实链路而不是裸 handler。

### 4.3 身份注入

本仓有两条身份通道：session 中间件读 `X-Multica-Session`，M1 dev-mode 的 `AuthUser` 提取器读
`X-Multica-User-Id`。回放器**两个都发**，让同一个 `member` fixture 同时覆盖两条链路。
抽取器只声明 `$testUserID` / `$testWorkspaceID` 两个可绑定符号；出现别的符号会在抽取期就 skip —— 回放期遇到未绑定符号直接 `unevaluable`，**绝不猜一个值**。

### 4.4 五类结论（不猜、不掩盖）

| outcome | 含义 | 算不算失败 |
| --- | --- | --- |
| `pass` | 状态码一致，且 `json_subset` 是响应 body 的子集 | — |
| `mismatch` | 打到**已实现**路由，但状态码/字段与上游断言不符 | **这才是缺陷** |
| `unmounted` | 本仓没有这条路由（404/405） | 属"未实现"，单独计数 |
| `placeholder` | 路由在，但是 M0 占位实现（`{"code":"not_implemented"}` / 501） | 属"未实现"，单独计数 |
| `unevaluable` | 本仓无法构造这次请求（缺绑定 / 缺库） | 不算失败，但必须出现在报告里 |

每个 fixture **恰好**一行结论（`assert_eq!(rows.len(), fixtures.len())`），且每行必须带非空 `detail`。

### 4.5 测试（`cargo test -p mc-conformance`）

| 用例 | 默认 | 断言（锁的是协议，不是产品状态） |
| --- | --- | --- |
| `fixture_set_is_self_consistent` | ✅ 离线 | 全部 fixture 可加载；每条要么能 `plan`（且 URI 里不留 `{` 占位符）要么给出非空原因；报告行数与 fixture 数一一对应 |
| `stateless_tier_decides_every_anonymous_fixture` | ✅ | 11 条 `anonymous` **一条也不许** `unevaluable`，且必须落到某个明确结论；`member` 必须明确报"需要 database 层"而不是伪装成 mismatch；`pass ≥ 4`（受保护路由 401 的地板值，低于它说明鉴权链断了）；同层跑两次报告**字节一致**（`--check` 的前提） |
| `json_subset_is_checked_against_a_live_route` | `--ignored` | 手工 `/api/health` fixture 命中 subset → `pass`；把 subset 换成不存在的字段 → 必须 `mismatch`；另断言数组按下标、对象递归的边界 |
| `database_tier_replays_every_fixture` | `--ignored`（要真库） | 58 条全部被判定、**零 `unevaluable`**、每行 `detail` 非空；并用 `judge()` 自检"418 → mismatch" |

---

## 5. 指标定义（plan1 §8「契约等价率」）

```text
契约等价率        = pass / fixtures                      （分母 = 抽取出的 fixture 数）
已接入路由等价率  = pass / (pass + mismatch)              （只问"实现了的路由对不对"）
离线可判定        = actor = anonymous 的 fixture 数 / 其中 pass 数
```

这三个数都在同一次回放里算出来（`Report::from_rows`），**没有第二个计数来源**。
plan1 §8 写「上游用例数」：本轮把分母解释为**抽出的 fixture 数**（`docs/15-M3-PLAN.md` §8.3.4 明确要求
"分母 = W0-C 抽出的 golden fixture 用例数"），因为上游可判定的用例总数无法穷举 —— 555 个候选点里
501 个当前不可静态解析，1484 个 recorder 点根本不在范围。**所以契约等价率永远必须和抽取率一起读**：

### 5.1 本轮实测（base `8ad10e5`，2026-09-22）

| 层 | pass | mismatch | unmounted | placeholder | unevaluable | 契约等价率 | 已接入路由等价率 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| stateless（确定性，CI 口径） | 4 | 1 | 5 | 1 | 47 | **4/58 = 6.9%** | 4/5 = 80.0% |
| database（本机真库口径 = 两者取强者 = 报告合并列） | 40 | 4 | 8 | 6 | 0 | **40/58 = 69.0%** | **40/44 = 90.9%** |

另：`offline_decidable = 4/11` pass。

**怎么读这三个数**：

- stateless 的 6.9% **不是**「代码差」：47/58 条 fixture 需要真库（`member` 身份），stateless 层把它们判 `unevaluable`。
  确定性是它存在的理由（CI 与 `--check`），要的是"没漂移"，不是"比率高"。
- **M3 集成门该报 database 层的 69.0%**（40/58）。plan1 §8 给 W3 的目标是 40% ⇒ 24 条；本轮 base 已达 40 条，
  但这个余量会在抽取器扩面（§3.4 的两大缺口）后重新摊薄 —— 目标是**基线上涨**，不是数字好看。
- 90.9% 的「已接入路由等价率」说明分子分母口径干净：唯一不等的 4 条全部在上表 §6 逐条登记过。

---

## 6. 本轮非等价清单（只登记，不在本切片修）

### 6.1 `mismatch` 4 条

| fixture | 路由 | 期望 → 实测 | 根因 | 去向 |
| --- | --- | --- | --- | --- |
| `issues/TestCreateIssueRejectsMalformedAttachmentIDBeforeWrite@…handler_test.go:1454#25` | `POST /api/issues` | 400 → **201** | 创建 issue 时不校验 `attachment_ids` 的 UUID 合法性 | **LUM-1410**（已建，backlog，未指派） |
| `issues/TestCreateIssueRejectsNonexistentMemberAssignee@…:1372#21` | `POST /api/issues` | 400 → **201** | 不校验 `assignee_id` 指向的成员是否存在 | **LUM-1410** |
| `issues/TestCreateIssueRejectsNonexistentAgentAssignee@…:1384#22` | `POST /api/issues` | 400 → **201** | 不校验 `assignee_id` 指向的 agent 是否存在 | **LUM-1410** |
| `auth/TestGoogleLoginSuccessfulExistingUser@…auth_google_error_code_test.go:289#1` | `POST /auth/google` | 200 → **403** | 上游用例用**假 verifier**（stub）签发 id_token；本仓走真 Google verifier，测试用的字面量 token 必然 403 | 不是路由缺陷，是**回放器限制**：要判定这类用例需在 harness 注入 verifier stub（改 `mc-http` 可测性接口，超出 W0-C 范围）。登记为已知边界：该 fixture 在报告里固定是 `mismatch`，读报告时按本条解释。 |

### 6.2 `unmounted` 8 条（路由未实现，owner 取自 `scripts/route-owners.tsv`）

| 路由 | owner |
| --- | --- |
| `GET /api/config` ×2 | M10（UI 兼容面） |
| `GET /health` | M10（ops 探针） |
| `POST /api/daemon/deregister` | M3（daemon pair 协议） |
| `GET /api/integrations/composio/callback` | M8 |
| `POST /api/pins` | M2-A |
| `GET /api/workspaces/{id}/dingtalk/groups` | M7 |
| `GET /users/me` | `route-owners.tsv` 未命中（按 first-match 规则见 `docs/22-ROUTE-PARITY.md`） |

### 6.3 `placeholder` 6 条（M0 占位实现）

| 路由 | 期望 → 实测 | 去向 |
| --- | --- | --- |
| `GET /api/agents` | 401 → 200（占位直接返 200） | M3（`docs/15-M3-PLAN.md` M3-5 明确要**删除**该占位） |
| `POST /api/agents` | 400 → 200 | M3 |
| `POST /api/autopilots` | 400 → 200 | M5 |
| `POST /api/projects` ×3 | 201 → 200 | M4 |

> 占位不是"失败"也不是"通过"：它证明**路由键存在**（`scripts/route_parity.py` 会把它算成已实现），
> 但行为不等价。这正是为什么本报告要把 `placeholder` 与 `mismatch` 分开计数 —— 否则"占位路由满足了上游路由"
> 的假象会让 route-parity 与契约等价率同时虚高。

---

## 7. 已知限制（不掩盖的缺口）

1. **`json_subset` 本轮全空（58/58）** —— 也就是说当前证明的是**状态码等价**，字段级等价只证明了机器可用。
   原因：只保留了"上游对 body 字段做字面量断言"的用例，全仓只有 3 条满足，且这 3 条都打在**未接入**的路由上
   （`/api/config`、`/users/me`、`/auth/google`）⇒ 加进去只是把 `mismatch` 从 3 变 6，不增加信息量。
   机器本身由 `json_subset_is_checked_against_a_live_route`（正向命中 + 反向必须 mismatch）覆盖。
   扩面条件：M2 面路由接入后，先把上游对 body 的断言（当前记为 `body_unresolved`）解析进 subset。
2. **抽取率 9.73% 是"HTTP 层两个目录内的静态可判定率"**，不是"覆盖了 9.73% 的上游测试"。分子分母见 §3.2；
   `--scan` 扩到 daemon/ws 后这个数会变。
3. **database 层是本地证据，CI 不依赖**：CI 只跑 stateless 层（无库）与 `--ignored` 的真库用例（⑧ 门已建库）。
   `report.json` 因此固定为 stateless 快照。
4. **`report.json` 不是门禁**：本切片没有改 `scripts/gates.sh`（那是 W0-A/CI 的文件，且并发切片在改它）。
   接线建议见 §9。当前它的用途是"证据 + 让你能 `--check` 发现漂移"。
5. **`insta` 未引入**（plan1 §4 曾列 `insta` 作 P5 执行器）：本轮的断言是"状态码 + JSON 子集"，
   不需要快照 redaction/评审工作流，`assert_eq!` + 稳定序列化已足够；引入它只会增加 `Cargo.lock` 面
   （本切片只新增 `mc-conformance` 一个 package 条目）。若将来做"整段响应快照"再评估。
6. **一条 fixture 只有一条主断言**：上游一个用例里多条断言 → 多条 fixture（id 里的 `#N`），
   所以"fixture 数"与"上游用例数"不是 1:1，读比率时不要混。

---

## 8. 复现（逐字命令与实测输出）

```bash
# 0) 上游基线（PIN 里的 commit）
git clone https://github.com/louloulin/multica ../upstream-multica    # 已 pin 90e0bdf8

# 1) 抽取器自证：树必须字节复现
$ python3 scripts/extract_upstream_fixtures.py --upstream ../upstream-multica --check
ok: 58 fixtures reproduce byte-identically          # 45.7s

# 2) 回放器：stateless 层（确定性，CI 口径）
$ cargo run -q -p mc-conformance
  pass 4  mismatch 1  unmounted 5  placeholder 1  unevaluable 47
  契约等价率 = 4/58 = 6.9%

# 3) 报告不漂移
$ cargo run -q -p mc-conformance -- --check crates/mc-conformance/report.json
report matches crates/mc-conformance/report.json

# 4) 真库层（本机 PG16；CI 由 ⑧ 门建库）
$ PGPASSWORD=multica psql -h 127.0.0.1 -U multica -d multica_conformance -c '\dt' | tail -1
(29 rows)                                            # 迁移已就位
$ cargo run -q -p mc-conformance -- --db-url postgres://multica:multica@127.0.0.1:5432/multica_conformance
  pass 40  mismatch 4  unmounted 8  placeholder 6  unevaluable 0
  契约等价率 = 40/58 = 69.0%
  已接入路由等价率 = 40/44 = 90.9%

# 5) 测试 + 门禁
$ cargo test -p mc-conformance                       # 2 passed
$ cargo test -p mc-conformance -- --ignored          # 2 passed（含 58 条真库回放）
$ cargo clippy -p mc-conformance --all-targets -- -D warnings   # clean
$ cargo fmt --all --check                            # clean
```

---

## 9. 后续（不在本切片做）

1. **⑨ 契约门**（建议名字）：把 stateless 层接进 `scripts/gates.sh` ——
   `cargo run -p mc-conformance -- --check crates/mc-conformance/report.json`（漂移即红）。
   所有者是 W0-A/CI 切片（`scripts/gates.sh`、`.github/workflows/ci.yml`），本切片不碰。
   **注意**：接门那一刻起，任何让 stateless 结论变化的改动（例如 `/api/config` 接入 M10）都必须同步刷新 `report.json` ——
   这是设计上的"强制对账"。
2. **M2 面抽取扩面**：把 `body_unresolved`（74 条）里"上游断言了 body 字段"的用例解析成 `json_subset`；
   优先 M2-A/M2-D 的 issue/comment 路由，因为它们已经有真库层可判定性。
3. **M3 协议 golden**：`--scan` 扩到 `internal/daemon`/`daemonws`，为 `docs/15-M3-PLAN.md` §8.5 的
   协议 golden JSON 提供同一抽取管线（保持"一份台账、一套 skip 原因"）。
4. **`/health` 路由缺口**：上游 `GET /health` 属 M10，本仓只有 `/api/health`。已在此登记（§6.2）。
5. **文档编号碰撞**：`docs/15-M3-PLAN.md:387` 把 `docs/27-*` 预留给了 `27-M3-AGENTS.md`，本文件按 issue
   （LUM-1388）要求占用 `docs/27-W0-GOLDEN-FIXTURES.md` ⇒ **M3-5 落地时请改用 `docs/31-M3-AGENTS.md`**
   （`docs/31` 当前空闲），本切片不改 `docs/15`（不在文件范围内）。
