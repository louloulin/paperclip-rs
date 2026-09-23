# M5-INT：M5 集成收口（10/10 门禁 + ⑦ 基线 300→329 + ⑨ autopilot 域 8 条离开 `unevaluable`）

> 交付单 **LUM-1572**｜起手 base `0fd96b4`（= 合并 #62/M5-5 后）｜合并前真合到 **`e01c73a`**（05:00 cycle 的 docs-only 推送）
> 分支 `agent/devbox5/43dcd6d6a0ed`｜依据：`docs/44-M5-PLAN.md` §6/§11、`docs/49-M4-INTEGRATION.md`（M4-INT 先例）
> 口径声明：本文件所有 ⑦/⑨/⑩ 数字**逐字取自当轮 gate 日志与当轮 `--json` / `--db-url` 输出**，无一处手写或转抄切片自述。

## 0. 结论（四行）

1. `bash scripts/gates.sh --with-db` → **10/10 PASS**（内容首跑冷 target **448s**；交付树热 target 两次 **86s / 79s**，三次读数逐字相同，⑤ `1380 passed / 0 failed`、⑥ e2e `333 passed / 0 failed`）。
2. ⑦ 基线 **300 → 329**（`+29 / −0`）：本次吸收的 29 键**全部是 M5 注册键**（21 条 autopilot/webhook 路由 + 7 条尾斜杠别名键 + 1 条 `/api/issue-wakeup-summaries`，与 `docs/44` §6.1 的预测公式逐字相同）。⑩ 0 违规。
3. 与 `docs/44` §6.1 的预测（`local 319 / implemented 255 / known_gap 201`）差 **+10 / +10 / −10**，差额**全部**来自 **M4-4（`LUM-1475`，PR #51，merge `542e833`）的 10 条 chat 键** —— 它们在计划书写完之后才合入，与 M5 无关（锚点实测与 §6.1 的「M5-0 后」行逐字一致，见 §4.3）。
4. ⑨ autopilot 域 8 条在真库模式下 **`unevaluable` 0**（6 pass / 1 mismatch / 1 unmounted，契约等价率 6/8 = 75%，已接入路由等价率 6/7 = 85.7%）；008 按口径保留并记录原因（§5）。

## 1. 写集（本片只碰 3 个文件）

| 文件 | 动作 | 说明 |
| --- | --- | --- |
| `docs/fixtures/route-parity-baseline.json` | 改 | ⑦ 基线 `300 → 329`（+29 键，逐键见 §4.1） |
| `docs/56-M5-INTEGRATION.md` | 新增 | 本文件（号段更正：正文旧号 `docs/45` 已被 `45-M4-4-CHAT-DISPATCH.md` 占用） |
| `docs/44-M5-PLAN.md` | 改 | 追加 §11 落地记录（原占位） |
| `crates/mc-conformance/report.json` | **未改** | ⑨ 的 `--no-db --check` 仍 `report matches`（§5.3）⇒ 无 diff，列此以证「碰过但无 diff」 |
| `docs/fixtures/slash-alias-allowlist.tsv` | **未碰** | 只剩 M6 的 2 行（`GET\|POST /api/skills`），M5 残留已在 M5-0 清空（§2） |

**未碰**：任何 `crates/**` 源码、`Cargo.toml`/`Cargo.lock`、`migrations/**`、`scripts/**`、
`routes/mount.rs`、`routes/mod.rs`、`routes/issues/mod.rs`、各 `lib.rs`（= `docs/44` §3.1 的共享锚点，只有 M5-0 动过一次）。

## 2. M5 合并波台账（9 片 + 本片，全部已入 base）

| 片 | issue | PR | merge commit | 本片文档 | 内容 |
| --- | --- | --- | --- | --- | --- |
| M5-0 anchor | `LUM-1563` | #50 | `e07e0f2` | （`docs/37` 各轮记录） | 3 个新 crate 骨架 + 重写 `mc-core` 两个 stub + 删 2 个 autopilot 占位 + 基线 `242 → 290` |
| M5-1 读面 | `LUM-1564` | #55 | `76db3eb` | `docs/46` | `GET /api/autopilots*` 4 路由 + quota 模块 + cron 基座 |
| M5-6 wakeup 用户面 | `LUM-1565` | #54 | `5b4f407` | `docs/47` | wakeup 8 路由（7 条从 501 转真实现）+ `+1` `issue-wakeup-summaries` |
| M5-7 租约内核 | `LUM-1566` | #52 | `659f19e` | `docs/48` | `mc-scheduler` 内核（spec/manager/db_ops），0 路由 |
| M5-2 写面 | `LUM-1567` | #56 | `415f194` | `docs/50` | autopilot 写面 5 路由 + 协作者/订阅者 |
| M5-3 trigger + 凭据 | `LUM-1568` | #57 | `22d7135` | `docs/51` | trigger 5 路由（含 token 轮换 / signing secret） |
| M5-4 执行/派发 | `LUM-1569` | #58 | `f80ad18` | `docs/52` | 执行面 6 路由 + dispatch 服务层 |
| M5-8 调度 jobs | `LUM-1571` | #60 | `eed6969` | `docs/55` | 两个 job（autopilot 调度 / wakeup 派发），0 路由 |
| M5-5 webhook 入站 | `LUM-1570` | #62 | `0fd96b4` | `docs/54` | 唯一无认证入口 `POST /api/webhooks/autopilots/:token` + worker |
| **M5-INT** | **`LUM-1572`** | 本 PR | — | 本文件 | 门禁 + 基线刷新 + ⑨ 复核 + 跨波登记 + §11 |

同一窗口内合入的**非 M5** 提交（列此以解释 §4.3 的差额）：

| 片 | PR | merge commit | 对 ⑦ 的影响 |
| --- | --- | --- | --- |
| M4-INT 集成收口 | #53 | `015ff2f` | 基线 `290 → 300`（吸收 M4-4 的 10 条 chat 键） |
| M4-4-fu 聊天派发广播 | #59 | `84ef946` | **0 条新路由**（实测 `git diff 84ef946^1 84ef946 -- crates/mc-http/src` 的 `.route(` 增行为 0） |
| M6 计划片 | #61 | `80d60a1` | 0（新增 `docs/57` + `docs/fixtures/m6-declared-routes.tsv` 台账，不动基线） |

**⑦ 基线在 M5 波里的三次变动**（`git show <rev>:docs/fixtures/route-parity-baseline.json` 实测键数）：

| 时点 | 键数 | 动作 |
| --- | --- | --- |
| `e07e0f2^`（M5-0 前） | 242 | 波前 |
| `e07e0f2`（M5-0，PR #50） | **290** | +50 / −2：吸收 M4-1/2/3 的 chat 20 / 项目 15 / squad 15，删 M0 的两条 autopilot 占位 |
| `015ff2f`（M4-INT，PR #53） | **300** | +10 / −0：补 M4-4（PR #51，merge `542e833`）的 10 条 chat 键 |
| **本片** | **329** | **+29 / −0**：M5 全部注册键（`local 329` 与基线同轮一致） |

## 3. 门禁读数（当轮 `gates.sh --with-db`，10/10）

真库：一次性角色 `mc_lum1572`（`CREATEDB`）+ 库 `multica_lum1572`，密码不入任何交付物；`mc-migrate` 走仓内 `migrations/`。
本轮跑了三次（门读数三次逐字相同）：内容首跑**冷 target 448s**；交付树（含本文档）**热 target 86s / 79s**。
下面是**交付轮（热，热后一次 = 提交前最后一次）**的 summary 段逐字：

```
  #  gate               exit   time  result
  ①  fmt                   0     2s  PASS
  ②  build                 0     0s  PASS
  ③  clippy                0     0s  PASS
  ④  clippy-test-util      0     1s  PASS
  ⑤  test                  0    33s  PASS
  ⑥  db                    0    12s  PASS  (migrate=0,e2e=0)
  ⑧  schema-drift          0    25s  PASS
  ⑦  route-parity          0     1s  PASS
  ⑨  conformance           0     5s  PASS
  ⑩  file-size             0     0s  PASS
-----------------------------------------------------------------------
  overall: PASS — 10/10 gate(s) green in 79s
```

冷跑同段的耗时（作对照，证明热跑不是「跳过」而是缓存）：① 2s / ② 96s / ③ 66s / ④ 22s / ⑤ 37s / ⑥ 144s / ⑧ 37s / ⑦ 0s / ⑨ 44s / ⑩ 0s。
累计通过数（两轮的 ⑤/⑥ 段逐 target 汇总）：⑤ **97 个 target / 1380 passed / 0 failed**；⑥ **21 个 target / 333 passed / 0 failed**；全日志 `FAILED|panicked` 出现 **0 次**。

⑦ 门逐字（`=== [⑦] gate route-parity ===` 段）：

```
upstream 456 (commit f41fae6b08fb) | local 329 registered | baseline 329
  implemented  263 real +   2 placeholder =  265 / 456   known_gap  191   unclaimed    0   regression   0   local_only   11
OK: every upstream route is either implemented or owned
```

⑨ 门逐字（`--no-db --check crates/mc-conformance/report.json`）：

```
golden: contracts/golden  fixtures: 365
  pass 5  mismatch 23  unmounted 31  placeholder 0  unevaluable 306
  契约等价率 = 5/365 = 1.4%
  已接入路由等价率 = 5/28 = 17.9%
  离线可判定（anonymous）= 5/59 pass
```

⑩ 门逐字：`python3 scripts/file_size_check.py --quiet` → exit 0（0 违规，本片无新增超限文件）。

`known_gap` 的 owner 板（当轮 `--json` 实测）：`M6 55 / M9 33 / M7 24 / M8 24 / M3+ 16 / M2-A 14 / M3 11 / M2-E 9 / M10 5`
—— **没有 M5**（`owners.M5 = 0`，即 `docs/44` §6.1 的收口条件）；`duplicates.local = []`、`duplicates.upstream = []`。

## 4. ⑦ 基线刷新明细（逐键可核）

### 4.1 本次吸收的 29 键（`--write-baseline` 的 `+29 / −0`）

| 组 | 键数 | 键 |
| --- | ---: | --- |
| autopilot 读/写/执行/凭据 + webhook 入口 | 21 | `GET /api/autopilots`、`GET /api/autopilots/cron-preview`、`GET /api/autopilots/usage`、`GET /api/autopilots/:param`、`GET /api/autopilots/:param/runs`、`GET /api/autopilots/:param/runs/:param`、`GET /api/autopilots/:param/deliveries`、`GET /api/autopilots/:param/deliveries/:param`、`POST /api/autopilots`、`PATCH /api/autopilots/:param`、`DELETE /api/autopilots/:param`、`POST /api/autopilots/:param/collaborators`、`DELETE /api/autopilots/:param/collaborators/:param`、`POST /api/autopilots/:param/triggers`、`PATCH /api/autopilots/:param/triggers/:param`、`DELETE /api/autopilots/:param/triggers/:param`、`POST /api/autopilots/:param/triggers/:param/rotate-webhook-token`、`PUT /api/autopilots/:param/triggers/:param/signing-secret`、`POST /api/autopilots/:param/trigger`、`POST /api/autopilots/:param/deliveries/:param/replay`、`POST /api/webhooks/autopilots/:param` |
| 尾斜杠别名键（形态门已 0 defect） | 7 | `GET /api/autopilots/`、`GET /api/autopilots/:param/`、`POST /api/autopilots/`、`PATCH /api/autopilots/:param/`、`DELETE /api/autopilots/:param/`、`PATCH /api/autopilots/:param/triggers/:param/`、`DELETE /api/autopilots/:param/triggers/:param/` |
| wakeup 读面新增 | 1 | `GET /api/issue-wakeup-summaries`（另 7 条 wakeup 路由波前已注册，本波只把 501 换成真实现，**键数不变** —— 见 `docs/47` §8 的检测器口径说明） |

> **三个数字不同义**（`docs/44` §6.1 的提醒，本片复核过）：`upstream 456` 是上游路由表；`local 329` 是本地注册键；
> `route-parity-baseline.json` 记的是「**曾注册过**」的路由集合（波前 300）。刷新动作只动第三个，刷新后三者同轮一致（`baseline 329 == local 329`）。

### 4.2 `local_only 11` 与 M5 无关（逐键可核）

`local_only 11` = 8 条真实现 + 3 条 placeholder：`GET /api/issues/:id/reactions`、`GET /api/issues/:id/quick-actions`、
`GET /api/health`、`GET /api/health/db`、`GET /api/openapi.json`、`GET|POST /api/me/pats`、`DELETE /api/me/pats/:id`、`POST /api/me/pats/:id/reveal`（真实现）
+ `GET|POST /api/plugins`、`GET /api/feature-flags`（placeholder，M6 面）。**M5 贡献 0 条 local_only。**

### 4.3 与 `docs/44` §6.1 预测的差额（+10 / +10 / −10）的归因

| 时点 | local | implemented | known_gap | 来源 |
| --- | ---: | ---: | ---: | --- |
| §6.1「现在」（计划落笔轮） | 292 | 235（real 231 + placeholder 4） | 221 | `docs/44` 预测行 |
| §6.1「M5-0 后」预测 | 290 | 233（real 231 + placeholder 2） | 223 | `docs/44` 预测行 |
| **实测（`../wt-anchor` = `e07e0f2` 的只读 worktree 上跑 `--json`）** | **290** | **233（real 231 + placeholder 2）** | **223** | 与预测行**逐字相同** |
| §6.1「M5-1..5 后」预测 | 319 | 255 | 201 | `docs/44` 预测行 |
| **实测（本片 gate 日志）** | **329** | **265（real 263 + placeholder 2）** | **191** | 本片 |

差额 `+10 / +10 / −10` 的完整来源：**M4-4（`LUM-1475`，PR #51，merge `542e833`）的 10 条 chat 键**
——`DELETE /api/chat/sessions/:param/queued-tasks`、`GET /api/chat/history`、`GET /api/chat/pending-tasks`、
`GET /api/chat/pending-tasks/has-any`、`GET /api/chat/sessions/:param/pending-task`、`GET /api/chat/thread`、
`POST /api/chat/sessions/:param/messages`、`POST /api/chat/sessions/:param/onboarding`、
`POST /api/chat/sessions/:param/queued-tasks/:param/prioritize`、`POST /api/chat/sessions/:param/quick-actions/regenerate`
（引入提交 `4dfc01a`，`git log -S` 可核）。

**为什么计划里没有它**：`docs/44` §6.1 的预测是在 M4-4 合入之前落笔的；M4-4 的 merge（`542e833`）晚于 M5-0 的锚点（`e07e0f2`），
基线由 M4-INT（`015ff2f`）补收（`290 → 300`）。
⇒ 这不是 M5 超交，也不是漏账；锚点读数与 §6.1 的锚点预测逐字一致这一点，把差额完全定位到了锚点之后的那 10 条 chat 键。

**算术自洽**：`implemented + known_gap = 265 + 191 = 456 = upstream`；三行的 `implemented + known_gap` 也都 = 456（235+221 / 233+223 / 255+201），
`regression = 0`（刷新基线后 `--declared` 与 `--no-baseline` 的集合差为 0）。

### 4.4 形态门（`slash_alias_audit.py`）

当轮 `--quiet`：**2 findings / 0 defect**，两条 finding 都是 **M6** 的 `GET|POST /api/skills`（即 allowlist 里仅存的两行），
`MISSING_ALIAS` 中 **M5 = 0** ⇒ `docs/44` §6.1 的形态门条件达成，`allowlist` 文件本片未碰。

## 5. ⑨ 复核：autopilot 域 8 条离开 `unevaluable`

### 5.1 逐条读数（本轮真库模式，`--filter autopilots`）

```
$ cargo run -q -p mc-conformance -- --filter autopilots --db-url "$MULTICA_TEST_DATABASE_URL" --json
  … stdout["totals"]：
  {"fixtures": 8, "pass": 6, "mismatch": 1, "unmounted": 1, "placeholder": 0, "unevaluable": 0,
   "by_actor": {"member": {"mismatch": 1, "pass": 6, "unmounted": 1}},
   "by_via": {"handler": {"mismatch": 1, "pass": 6, "unmounted": 1}},
   "tiers": {"database": {"mismatch": 1, "pass": 6, "unmounted": 1}}}
  … stdout["offline_decidable"]：{"fixtures": 0, "pass": 0}
  … stderr（种子身份每次运行现生成，此处为交付轮的值）：
database 层：8 条（种子身份 user=7862d6d4-234e-4a24-af5d-10ac7b9353b8 workspace=eea2eafa-a2a9-4aeb-90a5-90742e46e916）
```

| # | fixture（上游站点） | 路由 | 期望 | 实测 | 判定 |
| --- | --- | --- | ---: | ---: | --- |
| 1 | `TestListAutopilots_DerivedFields` @`autopilot_list_test.go:77` | `GET /api/autopilots` | 200 | 200 | pass |
| 2 | `TestListAutopilots_DefaultExcludesArchived` @`:132` | `GET /api/autopilots` | 200 | 200 | pass |
| 3 | `TestListAutopilots_DefaultExcludesArchived` @`:149` | `GET /api/autopilots` | 200 | 200 | pass |
| 4 | `TestListAutopilots_SubscribersMatchDetail` @`:194` | `GET /api/autopilots` | 200 | 200 | pass |
| 5 | `TestAutopilotSubscriberReadFailureFailsClosed` @`:293` | `GET /api/autopilots` | 500 | **200** | **mismatch**（见 §5.2） |
| 6 | `TestAutopilotQuotaManualAndWebhookEnforcement` @`autopilot_quota_handler_test.go:76` | `GET /api/autopilots/usage` | 200 | 200 | pass |
| 7 | `TestCreateAutopilotRejectsMalformedAssigneeID` @`handler_test.go:1667` | `POST /api/autopilots` | 400 | 400 | pass |
| 8 | `TestUpdateAutopilotRejectsMalformedID` @`handler_test.go:1675` | `PUT /api/autopilots/not-a-uuid` | 400 | 405 | **unmounted**（见 §5.2） |

当轮报告自带的两个比率：`contract_equivalence_rate = 6/8 = 75%`、`mounted_equivalence_rate = 6/7 = 85.7%`；
`unevaluable = 0` ⇒ `docs/44` §6.2 的硬要求（这 8 条必须变成 pass 或 mismatch）**达成**。

### 5.2 两种非 pass 的性质（都不是「为了好看」处理掉的）

- **005 mismatch（唯一一条）**：上游那条用例是 `direct_handler` 站点，测试**注入了一个读失败**的订阅者仓储
  （`TestAutopilotSubscriberReadFailureFailsClosed`），断言 handler 走 fail-closed 的 500。本地重放是
  「router + 真库」，**没有故障注入通道** ⇒ 观测到的是正常路径的 200。
  实现侧该语义**已落地**（`crates/mc-http/src/routes/autopilots/list.rs:39` 的契约表 + `:176` 的顺序注释：
  订阅者读失败 `?` 传播 → `repo_err` → 500；`triggers` / `collaborators` 才是 fail open），但**这条路无法被现有 fixture 观测到**
  ⇒ 如实记 **mismatch + 原因**，并作为 M5 等价证据的**残余缺口**（等价证据靠本地 e2e，口径见 `docs/44` §6.2）。
- **008 unmounted（永久）**：`PUT /api/autopilots/not-a-uuid` 是**抽取产物缺陷**（上游该站点是 `PATCH`），
  按 `docs/44` §6.2 的口径**保留、不删 fixture、不为它加 `PUT` 路由**；真库重放里未挂载 ⇒ 405（harness 记 `unmounted`，不是 mismatch）。
  008 的 `PATCH` 正例在本地有真库 e2e（`crates/mc-http/tests/autopilots/crud.rs:157` 的 `patch_three_state_semantics`）。

### 5.3 `--no-db` 模式仍是 8/8 `unevaluable`（预期内，非回归）

```
$ cargo run -q -p mc-conformance -- --no-db --filter autopilots --json | … ["totals"]
{"fixtures": 8, "pass": 0, "mismatch": 0, "unmounted": 0, "placeholder": 0, "unevaluable": 8,
 "by_actor": {"member": {"unevaluable": 8}}, "by_via": {"handler": {"unevaluable": 8}}, "tiers": {"stateless": {"unevaluable": 8}}}
```

真库模式的同一条命令（本节上方）。

8 条全是 `member` actor ⇒ 需要真库种子身份，离线模式**结构上**判不了（`docs/44` §6.2 已把这一点写成验收口径：
「等 ⑨ 涨上来」不是 M5 的验收路径）。门 ⑨（无 db）的整仓读数因此不含 autopilot 域，见 §3。
`crates/mc-conformance/report.json` 与当轮 `--no-db --check` 逐字一致 ⇒ **本片不需要重生成报告**（无 diff）。

## 6. 门 ⑩ 与「一个文件一个写者」交叉检查

- ⑩ **0 违规**；本片只改 docs 与基线 fixture（`docs/**` 不在 ⑩ 的扫描面内）。
- 交叉方法：对每个切片用 `git diff --name-only <merge>^1 <merge>`（= 该片相对当轮 base 的**真实贡献面**，
  比 `git log --name-only` 更准确：后者会把「合基」带进来的上游文件也算给该片）。
  9 片合计 **197 个文件次**（70+22+13+11+25+15+23+11+7），去重后 132 个文件；被 **>1 片**触碰的 **59 个**，三类：

| 类 | 数量 | 例 | 判定 |
| --- | ---: | --- | --- |
| ① 锚点骨架填充 | **53** | `crates/mc-autopilot/src/**` + `crates/mc-http/src/routes/autopilots/**`（33 个）、`crates/mc-repos/src/{autopilot,scheduler,wakeup}/*.rs`、`crates/mc-scheduler/src/*.rs`、`routes/{webhooks/autopilots,issue_wakeups,issues/wakeups}.rs`（20 个）；每个的写者集合实测都是 `{M5-0, 本片}` 这一对 | 设计如此（`docs/44` §5.3）：锚点只建**骨架 / 空模块 / 类型**，实现留给切片自己的格子 |
| ② 追加型注册表 | **5** | `crates/mc-http/tests/autopilots/main.rs`（M5-1..M5-5 各加自己的 `mod` 行，实测 **18 行 `mod`，`sort \| uniq -d` 无重复**）、`crates/mc-repos/src/autopilot/tests/mod.rs`、`crates/mc-scheduler/src/jobs/mod.rs`（M5-0 建两个 `pub mod`、M5-7 加 `register_all` 空实现、M5-8 填两个 job —— **串行边**，非并发写）、`crates/mc-http/Cargo.toml` 与 `Cargo.lock`（M5-1/M5-6 各加一条边） | 追加语义，**无重复键 / 无重复定义**；实测 `Cargo.lock` 里 `mc-autopilot`、`mc-scheduler` 各只 1 个 `[[package]]`；②③⑦ 全绿是其反证 |
| ③ 唯一一处**跨片改语义** | **1** | `crates/mc-repos/src/autopilot/run.rs`（写者 `{M5-0, M5-4, M5-5}`）：M5-5 在 M5-4 的文件里改了 **3 行** —— `load_trigger_principal` 原本 `WHERE t.workspace_id = $3`（`autopilot_trigger` **无此列**）改成 `JOIN autopilot a ON a.id = t.autopilot_id … a.workspace_id = $3` | **必需修复**（`autopilot_trigger` 没有 `workspace_id` 列，原写法在真库下必炸），非残留；记在此以免下次审计把它当违规 |

⇒ **无「同一文件两个写者」残留**：59 个交叉里 58 个是①②类的结构性共享，1 个是上述带原因的修复。

## 7. 跨波缺口登记（R7 / R9 / R8 / 调度内核共享，本片集中一段）

| # | 项 | 现状与证据 | 归属 | 本片处置 |
| --- | --- | --- | --- | --- |
| **R7** | entitlement 平面（quota 上限 / 周期边界 / `QuotaEnabled()`） | `mc-autopilot/src/quota.rs` 已落 `QuotaPolicyProvider` trait + `install_policy_provider`（进程内装一次），默认「无平面 ⇒ off」等价分支；`mc-core/src/autopilot_quota.rs` 把 `policy_revision`/`subscription_version` 做成调用方传入。**生产实现缺位**（没人装 provider ⇒ 线上恒 off 形态） | **M9**（Cloud 订阅面） | 登记；M5-1 已按上游 off 形态验过 `usage` 路由 |
| **R9** | daemon 执行面（`agent_task_queue` 的 `queued` 行没有执行者） | M5-4 的派发只到「落 `queued` 行」；`docs/52` §7 与 `docs/44` §4.2 都写明「不在 M5 实现 daemon」。M3-7（`LUM-1438`）的 daemon 循环与本波的 worker 循环**至今无 owner** | 跨波（M3-7 / M6-9 daemon 面） | 登记；M5-4 的终态回写（`SyncRunFrom*`）待 daemon 面落地后才有真路径 |
| **R8** | realtime 广播口径（会不会长成第二套命名） | 全仓唯一出口是 `mc_realtime::EventEnvelope`；M5 的**唯一**发点在同波 `mc-autopilot/src/dispatch/{mod,sync,skip,create_issue}.rs`：`resource = "workspace"`（订阅单位是 workspace，`autopilot_id` 在载荷里）+ `event_type = "autopilot:run_start" / "autopilot:run_done"`，载荷带 `autopilot_id`/`run_id`/`status`/`reason_code`。**M5-1/2/3 的 CRUD 事件（上游 `EventAutopilotCreated/Updated/Deleted`）没接**（`docs/50` §5.3 登记）；wakeup 的 `task.queued` 要等接线片。**没有第二套命名**：chat 面（含 M4-4-fu）同样只注册路由、未发 WS（`crates/mc-http/src/routes/chat/session.rs:34-35` 的「3. **未接**（跨波依赖）」清单把「WS 广播（M3-7）」列在未接里） | 后续 WS 扇出片 + M5-9 接线片 | 登记；**口径以 M5-4 为准**（`resource="workspace"` + `事件名`），后续片不得自造频道名 |
| **R6/R12** | 调度内核共享给 M6 + M9/M3 | `mc-scheduler` 内核（`manager`/`db_ops`/`spec`）**零 SQL**：SQL 全在 `mc-repos/src/scheduler.rs`；job 只加 `jobs/*.rs` 并往 `register_all` 加注册行 ⇒ M6（`jobs_plugin_hook.go`353）与 M9/M3（`jobs_task_usage.go`120）**不需要再碰 SQL**。**但**：`register_all(manager, &JobPorts)` 需要调用方提供 `AutopilotSchedulePort` / `WakeupDispatchPort` 的生产实现 | M6 / M9 各加 job 文件；生产端口实现归接线片 | 登记（`docs/48` §4、`docs/55` §3.4） |
| **P0** | **`apps/mc-server/Cargo.toml` 缺 `mc-scheduler` 依赖边**（+ `Cargo.lock`），且门 ⑥ 未收集 `-p mc-scheduler` | `docs/48` §7.1/§7.2 已实测：`apps/mc-server/Cargo.toml` 有 13 条 `mc-*` path 依赖但无 `mc-scheduler`；门 ② 跑 `--locked` ⇒ 硬写 spawn 块会当场红。挡着 **M5-9（`LUM-1659` 接线片）** 与 M6 的注册点。owner（`LUM-1628` §4）至今 0 回复 | **`LUM-1659`（M5-9）** — 本片**不接线、不再 @**（按起手令） | 登记为**M5 唯一未收口的项**（代码片全合、门禁全绿，但生产启动路径上 `mc-scheduler` 仍是死的） |
| **D8** | webhook worker 的轮询循环无 owner（1 s ticker + `Notify` + 4 并发） | `docs/54` §6.1 把归属**判给 M5-INT**；契约已定：`WebhookIngress::process_next_delivery()`（`Ok(None)`=队空 / `Ok(Some)`=收口一条 / `Err`=认领期基础设施错） | **本片裁定：归 `LUM-1659`（M5-9）** —— 它是同时持有「`main.rs` 接线」+「两个端口生产实现」的唯一落点；若该片长期停 `backlog`，则由 **M6-9（daemon 执行面）** 接收 | 裁定并登记（见下） |
| **文档** | `docs/48` §7.1 的 ready-to-apply 接线代码段**已过期** | 该段写的是 `register_all(&mut scheduler)`；M5-8 落地后签名是 `register_all(&mut manager, &JobPorts)`，且两个端口**没有生产实现**（`docs/55` §3.4 列了 5 条 SQL + 7 步事务） | `LUM-1659` | 登记：接线片按 M5-8 的签名写，别照抄 §7.1 |

## 8. 与计划预测的偏差（逐项）

| 项 | `docs/44` 预测 | 实测 | 差异与原因 |
| --- | --- | --- | --- |
| ⑦ `local` | 319 | **329** | +10 = M4-4 的 chat 键（§4.3） |
| ⑦ `implemented` | 255 | **265**（real 263 + placeholder 2） | +10 同上 |
| ⑦ `known_gap` | 201 | **191** | −10 同上；`implemented + known_gap = 456` 守住 |
| ⑦ 锚点行 | 290 / 233 / 223 | **290 / 233 / 223** | 逐字一致 ⇒ 差额全在锚点之后 |
| ⑦ 基线 | 刷新 → 319 | **329**（`+29 / −0`） | 与 `local` 同轮一致（刷新口径正确） |
| ⑦ 形态门 | M5 `MISSING_ALIAS` = 0，allowlist 剩 2 行 M6 | **0 defect**；allowlist 2 行（M6） | 一致 |
| ⑨ autopilot 8 条 | 不得再有 `unevaluable` | **0 unevaluable**（6 pass / 1 mismatch / 1 unmounted） | 一致；1 mismatch 的性质见 §5.2 |
| ⑩ 文件大小 | 不需要切片中途重拆（锚点预拆足够） | **0 违规** | 一致 |
| 门槛 | 10/10 `--with-db` | **10/10**（冷 448s / 热 86s、79s） | 一致 |

## 9. 复现命令（逐字）

```bash
# 起手（本轮实测 base = e01c73a = 0fd96b4 + 05:00 cycle 的 docs-only 推送）
git fetch && git checkout -B agent/devbox5/43dcd6d6a0ed origin/feat/multica-rs-initial

# ① ⑦ 基线一次性刷新（先备份，再核对 +29/−0）
cp docs/fixtures/route-parity-baseline.json ../baseline.before.json
python3 scripts/route_parity.py --write-baseline
python3 scripts/route_parity.py --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["counts"])'

# ② 形态门（M5 相关必须是 0；allowlist 只剩 2 行 M6）
python3 scripts/slash_alias_audit.py --quiet; cat docs/fixtures/slash-alias-allowlist.tsv

# ③ 真库（一次性角色需 CREATEDB；密码不入任何交付物）
sudo -n -u postgres psql -c "CREATE ROLE mc_lum1572 LOGIN PASSWORD '…' CREATEDB"
sudo -n -u postgres psql -c "CREATE DATABASE multica_lum1572 OWNER mc_lum1572"
export MULTICA_TEST_DATABASE_URL='postgres://mc_lum1572:…@127.0.0.1:5432/multica_lum1572'
bash scripts/gates.sh --with-db

# ④ ⑨ autopilot 域 8 条：两种模式（离线 8/8 unevaluable 是预期；真库必须 0）
cargo run -q -p mc-conformance -- --no-db --filter autopilots --json
cargo run -q -p mc-conformance -- --filter autopilots --db-url "$MULTICA_TEST_DATABASE_URL" --json

# ⑤ 锚点行复核（差额归因，只读；worktree 不污染工作树）
git worktree add ../wt-anchor e07e0f2 && (cd ../wt-anchor && python3 scripts/route_parity.py --json) > ../anchor.json

# ⑥ 「一个文件一个写者」交叉（本片的审计口径）
for c in e07e0f2 5b4f407 659f19e 76db3eb 415f194 22d7135 f80ad18 eed6969 0fd96b4; do
  git diff --name-only $c^1 $c > ../files-$c.txt
done
```

## 10. 收口状态与后续

- **M5 代码片全部合并**（M5-0…M5-8，9 个 PR 全部入 base），`owners.M5 = 0`，⑦ 基线同轮刷新 ⇒ M5 波**代码面收口**。
- **M5 唯一未收口项** = §7 的 P0（`apps/mc-server` 缺 `mc-scheduler` 边）：**不挡**本片的门禁（门 ⑥ 全绿、内核真库用例有独立命令），
  但它挡着 M5-9（`LUM-1659`）与 M6 的注册点 ⇒ 交接给接线片，本片按令不接线。
- **后续片**：`LUM-1659`（M5-9 接线，含 §7 的 D8 裁定）→ M6 波（计划见 `docs/57`，基线预测 `docs/57` §6.1）。
  本片**不动** `scripts/route_parity.py` 的占位检测器（R3 ⇒ `LUM-1580`），因此汇报 M6 波进度时按 `docs/47` §8 的口径扣掉假实现。
