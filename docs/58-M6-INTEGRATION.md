# docs/58 — M6-INT：M6 集成收口（10/10 门禁 + ⑦ 基线 344→406 + ⑨ M6 18 条离开 `unevaluable`）

> 交付单 **LUM-1675**（M6-10，0 代码片）｜起手 base **`94f3ecfc`**（= M6-8 / PR #78 的 merge commit）｜分支 `agent/devbox5/14ec3a6d1f09`
> 依据：`docs/57-M6-PLAN.md` §4.2 / §6 / §9.7 / §9.8 / §9.9 / §10、`docs/56-M5-INTEGRATION.md`（M5-INT 先例）
> 口径声明：本文件所有 ⑦/⑨/⑩/⑤/⑥ 数字**逐字取自当轮 gate 日志（`/tmp/gates1675_cold.log`）与当轮 `--json` / `--db-url` 输出**，
> 无一处转抄切片自述。逐片读数取自**在各自 merge commit 的 `crates/mc-http/src` 上重扫**（§4.2），不是抄各片描述。

## 0. 结论（五行）

1. `bash scripts/gates.sh --with-db` → **10/10 PASS / 388s**（首轮全量；逐门耗时见 §3）；⑤ `1826 passed / 0 failed / 201 ignored`（106 target）、⑥ `524 passed / 0 failed`（33 target）；全日志 `FAILED|panicked` **0 次**。
2. **⑦ 基线一次性刷新 `344 → 406`（`+62 / −0`）**，本波（M6-0…M6-9）的注册键 62 条 = **声明 57 条路由 + M6-2 那 5 个双形态键的第二形态**，逐键可核（§4）；刷新后 `local == baseline == 406`。
3. 末态验收向量与 `docs/57` §9.7 / §9.9 的预测**逐字相符、零差异**：`local 406 / implemented 330 = 326 real + 4 placeholder / known_gap 126 / owners.M6 0 / unclaimed 0 / regression 0 / local_only 9（占位 2）`，不变式 `330 + 126 = 456` ✓。**M6 路由面收口**。
4. ⑨：真库模式下 M6 相关 **20 条 fixture 全部离开 `unevaluable`**（`0 unevaluable`）—— 但**只有 5 条 `pass`，13 条 `mismatch`、1 条 `unmounted`**，两种非 pass 的机制已逐条定位（**都不是实现分叉**，§5.2）；`crates/mc-conformance/report.json` 的 **stateless 层逐字未变**（`--check` = `report matches`）⇒ **本片不重生成报告**。
5. 跨片缺口登记 **8 行**（§7）：**D8 掉棒项（`LUM-1745`）**、R-M6-1、R-M6-3（归 W8）、R-M6-13、⑨ 的 plugin 凭据缺口、M6D-10 的收敛点、一处**文档缺口**（M6-1/M6-4/M6-5 无 `docs/32` §9.x 落地记录），以及已因 `LUM-1659` 合入而**闭**的 R-M6-6。

## 1. 写集（本片只碰 4 个文件 + 1 处「计划占位」的收口）

| 文件 | 动作 | 说明 |
| --- | --- | --- |
| `docs/fixtures/route-parity-baseline.json` | **改** | ⑦ 基线 `344 → 406`（`+62 / −0`，逐键见 §4.1） |
| `docs/58-M6-INTEGRATION.md` | **新增** | 本文件（`docs/58` 号段由 `LUM-1675` 预留，`docs/56` = M5-INT） |
| `crates/mc-conformance/report.json` | **未改（0 diff）** | ⑨ 的 stateless 快照与当轮 `--no-db --check` 逐字一致（§5.3）⇒ 列此以证「碰过但无 diff」 |
| `docs/fixtures/slash-alias-allowlist.tsv` | **未改（0 diff）** | M6 的 2 行由 M6-0 在同一 PR 删除，本文件现在**只剩表头**（允许名单为空）；`slash_alias_audit.py` **0 defect** |
| `docs/57-M6-PLAN.md` | **改（§11 占位收口）** | ⚠️ **写集外的唯一一处**，理由见下 |

**关于第 5 个文件（有意为之，非顺手扩大改动面）**：`docs/57` §11 的标题逐字是
「**M6-INT 落地记录（占位，由 M6-10 填写）**」，占位正文写「待 M6-10 交付后填写：⑦/⑨/基线与 allowlist 的最终读数、跨片缺口、与本文预测的差异」——
即**计划本身把这一节指给了本片**。M5-INT 的先例同构（`docs/44` §11 原占位，由 `LUM-1572` 追加落地记录，见 `docs/56` §1 的写集表）。
本片只把该节替换成一段指路（指向本文件），**不重复正文**；`docs/**` 不在门 ⑩ 的扫描面内，且不动任何 `.rs` ⇒ 不影响任何门。
若评审认为该节应留空，删除它是**零风险的**（本文件 §0–§10 自足）。

**未碰**：任何 `crates/**` / `apps/**` 源码（`git diff --numstat HEAD -- crates apps` 为空）、`Cargo.toml` / `Cargo.lock`、
`migrations/**`、`scripts/**`（含 `route_parity.py` —— 分类口径已由 `LUM-1580` / #77 落定，本片不需要也不得改它）、
`scripts/file_size_baseline.tsv`（22 行，**逐字未变**）。

## 2. M6 合并波台账（10 片，全部已入 base，全部 `in_review`）

| 片 | issue | PR | merge commit | 路由增量（**实测**，§4.2） | 落地记录 |
| --- | --- | --- | --- | ---: | --- |
| M6-0 anchor | `LUM-1665` | #65 | `d62558b3` | **−4**（删 4 个 M0 占位键：基线 `329 → 325`） | `docs/32` §9.1–§9.5 |
| M6-1 契约与凭据 | `LUM-1666` | #67 | `aa3eb749` | 0（0 路由片） | **无 §9 记录**（§7 文档缺口） |
| M6-2 skill 读写 | `LUM-1667` | #69 | `fc4971c2` | **+17** = 12 路由 + 5 双形态键 | `docs/32` §9.6 |
| M6-3 skill 导入/刷新 | `LUM-1668` | #70 | `25592547` | **+2** | `docs/32` §9.7 |
| M6-4 skill 供给面 | `LUM-1669` | #71 | `1ca24762` | **+6** | **无 §9 记录**（§7 文档缺口） |
| M6-5 plugin 生命周期 | `LUM-1670` | #72 | `2e18514c` | **+13** | **无 §9 记录**（§7 文档缺口） |
| M6-6 运行时面 | `LUM-1671` | #73 | `6d9bc858` | **+4** | `docs/32` §9.8 |
| M6-7 公开 API/bridge/surface | `LUM-1672` | #74 | `81c58721` | **+19** | `docs/32` §9.9 |
| M6-9 daemon 执行面 | `LUM-1674` | #75 | `2394bfcc` | 0（0 路由片） | `docs/32` §9.10 |
| M6-8 hook + hook job | `LUM-1673` | #78 | **`94f3ecfc`** | **+1** | `docs/32` §9.11 |

**M6 波内的非 M6 干扰项（列此以解释 §4.2 的读数跳变，不是 M6 的账）**：

| 时点 | 提交 | 对 ⑦ 的影响 |
| --- | --- | --- |
| M6-0 之后、M6-1 之前 | `ee26c9c5`（#66，M2-E label/property 目录面） | `local 325 → 344`（+19），**基线同轮刷到 344** ⇒ M6-1 的 slices 读数从 344 起算 |
| M6-9 之后、M6-8 之前 | `fd6c4aa6`（#76 M7 计划）/ `956f387f`（#77 `LUM-1580` 门 ⑦ 占位正则） | **0 路由**；#77 只改 `implemented` 的 real/placeholder 拆分（`0 ph → 4 ph`，总数不变） |

⇒ **`--write-baseline` 的时点无关紧要（本片是唯一一次），但读数起算点只有 344**：`344 + 62 = 406`（§4）。

## 3. 门禁读数（当轮 `gates.sh --with-db`，10/10）

真库：一次性角色 `mc_lum1675`（`CREATEDB`）+ 库 `multica_lum1675`；`mc-migrate` 走仓内 `migrations/`。密码不入任何交付物。
首轮全量（冷 target / 快照刷新后的工作树）逐字：

```
  #  gate               exit   time  result
  ①  fmt                   0     1s  PASS
  ②  build                 0    71s  PASS
  ③  clippy                0    63s  PASS
  ④  clippy-test-util      0    26s  PASS
  ⑤  test                  0    34s  PASS
  ⑥  db                    0   161s  PASS  (migrate=0,e2e=0)
  ⑧  schema-drift          0    26s  PASS
  ⑦  route-parity          0     0s  PASS
  ⑨  conformance           0     6s  PASS
  ⑩  file-size             0     0s  PASS
-----------------------------------------------------------------------
  overall: PASS — 10/10 gate(s) green in 388s
```

**交付树（含本文件与 `docs/57` §11）热 target 复跑，逐字**：

```
  #  gate               exit   time  result
  ①  fmt                   0     2s  PASS
  ②  build                 0     1s  PASS
  ③  clippy                0     0s  PASS
  ④  clippy-test-util      0     0s  PASS
  ⑤  test                  0    36s  PASS
  ⑥  db                    0    51s  PASS  (migrate=0,e2e=0)
  ⑧  schema-drift          0    25s  PASS
  ⑦  route-parity          0     0s  PASS
  ⑨  conformance           0     5s  PASS
  ⑩  file-size             0     1s  PASS
-----------------------------------------------------------------------
  overall: PASS — 10/10 gate(s) green in 121s
```

两次跑的**通过数逐字相同**（⑤ `106 target / 1826 passed / 0 failed / 201 ignored`；⑥ `33 target / 524 passed / 0 failed`），
只差缓存命中后的耗时（388s → 121s）⇒ 热跑不是「跳过」，而是同一批断言的重放（与 M5-INT §3 的同款对照）。

- ⑤ `cargo test --workspace`：**106 target / 1826 passed / 0 failed / 201 ignored**（不带库变量，脚本用 `env -u` 显式剥掉）。
- ⑥ 真库 e2e：**33 target / 524 passed / 0 failed**，`migrate=0`。
- ⑧ `schema-drift`：0（自建带 PID 的 scratch 库，跑完即删）。
- ⑩：0 违规；`scripts/file_size_baseline.tsv` **22 行逐字未变**（只减不增 ⇒ 本波 0 新增 + 0 删行）。

⑦ 门逐字（`=== [⑦] gate route-parity ===` 段，**刷新之后**）：

```
upstream 456 (commit f41fae6b08fb) | local 406 registered | baseline 406
  implemented  326 real +   4 placeholder =  330 / 456   known_gap  126   unclaimed    0   regression    0   local_only    9
OK: every upstream route is either implemented or owned
```

`known_gap` 的 owner 板（当轮 `--list-gaps` 实测）：`M9=33  M7=24  M8=24  M3+=16  M2-A=13  M3=11  M10=5`（和 = 126 ✓，**没有 `M6` 键** ⇒ `owners.M6 = 0`）。
`duplicates.local = []`、`duplicates.upstream = []`；`local_only 9` = 7 条真实现本地路由（`/api/health`、`/api/health/db`、`/api/openapi.json`、
`/api/issues/:id/reactions`、`/api/me/pats` 3 条）+ **2 条占位**（`GET /api/issues/:id/quick-actions`、`GET /api/feature-flags`）—— 本波**不动**（`docs/57` §9.7 的「末态 `local_only 9`（占位 2）」逐字一致）。

⑨ 门逐字（`--no-db --check crates/mc-conformance/report.json`）：

```
golden: contracts/golden  fixtures: 365
  pass 5  mismatch 23  unmounted 31  placeholder 0  unevaluable 306
  契约等价率 = 5/365 = 1.4%
  已接入路由等价率 = 5/28 = 17.9%
  离线可判定（anonymous）= 5/59 pass
report matches crates/mc-conformance/report.json
```

§3 的两个比率在**真库模式**下另有读数（§5.1）：`contract_equivalence_rate = 140/365 = 38.4%`、`mounted_equivalence_rate = 140/286 = 49.0%`（286 = pass + mismatch = 已挂载且可判的分母）。

⑩ 门逐字：`python3 scripts/file_size_check.py --quiet` → exit 0。

## 4. ⑦ 基线刷新明细（逐键 + 逐片，两路独立核算）

### 4.1 本次吸收的 62 键（`--write-baseline` 的 `+62 / −0`）

键集合差（`docs/fixtures/route-parity-baseline.json` 刷新前 vs 后，`set` 差）：

| 组 | 键数 | 键 |
| --- | ---: | --- |
| skill 读写（M6-2） | 17 | `GET /api/skills`、`GET /api/skills/`、`POST /api/skills`、`POST /api/skills/`、`GET\|PUT\|DELETE /api/skills/:param`、`GET\|PUT\|DELETE /api/skills/:param/`、`GET /api/skills/search`、`GET /api/skills/:param/files`、`PUT /api/skills/:param/files`、`DELETE /api/skills/:param/files/:param`、`GET /api/skills/:param/labels`、`POST /api/skills/:param/labels`、`DELETE /api/skills/:param/labels/:param` |
| skill 导入/刷新（M6-3） | 2 | `POST /api/skills/import`、`POST /api/skills/:param/refresh` |
| skill 供给面（M6-4） | 6 | `GET /api/agents/:param/skills`、`PUT /api/agents/:param/skills`、`POST /api/agents/:param/skills/add`、`PUT /api/agents/:param/skills/:param/enabled`、`DELETE /api/agents/:param/skills/:param`、`PUT /api/agents/:param/runtime-skills/enabled` |
| plugin 生命周期/包管理（M6-5） | 13 | `GET\|POST /api/workspaces/:param/plugins`、`DELETE /api/workspaces/:param/plugins/:param`、`POST /api/workspaces/:param/plugins/:param/{enable,disable,token}`、`DELETE /api/workspaces/:param/plugins/:param/token`、`PUT /api/workspaces/:param/plugins/:param/config`、`GET /api/workspaces/:param/plugins/packages`、`POST /api/workspaces/:param/plugins/packages`、`POST /api/workspaces/:param/plugins/packages/local`、`DELETE /api/workspaces/:param/plugins/packages/:param`、`POST /api/workspaces/:param/plugins/preview` |
| 运行时面（M6-6） | 4 | `GET /api/workspaces/:param/plugins/:param/invocations`、`GET\|PUT /api/workspaces/:param/plugins/:param/mcp/:param/tools`、`GET /api/workspaces/:param/plugins/:param/surfaces/:param/launch` |
| 公开 API + bridge + surface（M6-7） | 19 | `/v1/{context,issues/:param,issues/:param/comments,storage/:param,storage/:param/:param}`（5 读 + 5 写 = 10）+ `/api/plugin-bridge/v1/{context,issues/:param,issues/:param/comments,storage/:param,storage/:param/:param}`（同 10 中的 9，见下）+ `GET /plugin-surfaces/:param` |
| hook 引擎（M6-8） | 1 | `POST /api/plugin-bridge/v1/hooks/:param` |
| **合计** | **62** | `+62 / −0` |

> M6-7 的 19 条 = `/v1` 9 条（`context` / `issues` GET+PATCH / `issues/:param/comments` GET+POST / `storage` 2 形态 GET+PUT）
> **注册两次**（`/v1/*` 与 `/api/plugin-bridge/v1/*` 同一批 handler）+ `GET /plugin-surfaces/:param` 1 条
> —— 与 `docs/57` §9.2「路由账要算两份」逐字一致；其中 `/api/plugin-bridge/v1/hooks/:param` 归 M6-8 而不是 M6-7。

**三种「数」不同义（`docs/44` §6.1 的提醒，本片复核过）**：`upstream 456` = 上游路由表；`local 406` = 本地**逐条注册路径**（同一路由的两种尾斜杠形态各算一条）；
`baseline` 记的是「**曾注册过**」的集合。刷新动作只动第三个，刷新后与前两者**同轮一致**（`baseline 406 == local 406`，`local < upstream` 因为 M7/M8/M9 面尚未注册）。

### 4.2 逐片路由增量（**独立重扫**，不是抄各片自述）

方法：对每个 M6 merge commit，`git archive <merge> crates/mc-http/src | tar -x` 到独立目录，再
`python3 scripts/route_parity.py --routes-dir <该目录>/crates/mc-http/src --no-baseline --json` 取 `counts.local`。
这是**同一把尺子**在 10 个时点上量同一条轴，因此逐片 ⊿ 与 §4.1 的键集合差可交叉验证：

| 时点 | commit | `local` | ⊿ | 声明（`docs/57` §9.7） | 一致？ |
| --- | --- | ---: | ---: | --- | --- |
| M6-0 合入 | `d62558b3` | 325 | −4 | −4（删 4 个 M0 占位） | ✓ |
| M6-1 合入（M2-E #66 已在其中） | `aa3eb749` | 344 | +19 | 0（+19 = M2-E） | ✓ |
| M6-2 | `fc4971c2` | 361 | **+17** | +17（12 路由 + 5 双形态） | ✓ |
| M6-3 | `25592547` | 363 | **+2** | +2 | ✓ |
| M6-4 | `1ca24762` | 369 | **+6** | +6 | ✓ |
| M6-5 | `2e18514c` | 382 | **+13** | +13 | ✓ |
| M6-6 | `6d9bc858` | 386 | **+4** | +4 | ✓ |
| M6-7 | `81c58721` | 405 | **+19** | +19 | ✓ |
| M6-9（在 M6-8 之前合入） | `2394bfcc` | 405 | 0 | 0 | ✓ |
| M6-8（本片起手 base） | `94f3ecfc` | **406** | **+1** | +1 | ✓ |

⇒ **记账闭合（三条独立算得同一个数）**：① 键集合差 = **62**；② 逐片 ⊿ 之和 = `17+2+6+13+4+19+1 = 62`；
③ 声明侧 `docs/fixtures/m6-declared-routes.tsv` = **57 条路由** + M6-2 的 **5** 个双形态键 = 62。**零 slack、零重复认领、无一键归两片**（§6 的写集交叉检查是另一半证据）。

### 4.3 基线的三次变动（M6 窗口）

| 时点 | 键数 | 动作 |
| --- | ---: | --- |
| M6-0 之前 | 329 | 波前 |
| `d62558b3`（M6-0，PR #65） | **325** | −4：删 `mount.rs` 的 4 个 M0 占位键（`GET\|POST /api/skills`、`GET\|POST /api/plugins`） |
| `ee26c9c5`（#66，M2-E） | **344** | +19：M2-E 的 label/property 目录路由（**非 M6**） |
| **本片（M6-INT）** | **406** | **+62 / −0**：M6 全波（§4.1） |

## 5. ⑨ 复核：M6 相关 20 条 fixture 的最终分类

### 5.1 逐条读数（真库模式，当轮 `--db-url "$MULTICA_TEST_DATABASE_URL" --json`）

```
database 层：365 条（种子身份 user=de12aca4-…-ee8208f8dd58 workspace=9b628b53-…-f79b563246e2）
totals：{"fixtures": 365, "pass": 140, "mismatch": 146, "unmounted": 64, "placeholder": 0, "unevaluable": 15}
contract_equivalence_rate = 0.3836（140/365）   mounted_equivalence_rate = 0.4895（140/(140+146)）
```

M6 相关 20 条（过滤口径 = `docs/57` §10 命令 7，逐字复现）：

| # | fixture（上游站点） | 路由 | 期望 | 实测 | 判定 | 归属 |
| --- | --- | --- | ---: | ---: | --- | --- |
| 1–5 | `skills/TestListSkills_OmitsContent`、`TestListSkills_IncludesAttachedLabelsAndOmitsContent`、`TestSearchSkillsEmptyQueryReturns400`、`TestSearchSkillsReturnsNormalizedClawHubCandidates`、`TestGetSkill_MalformedUUIDReturns400` | `GET /api/skills`、`GET /api/skills/search`、`GET /api/skills/not-a-uuid` | 200/200/400/200/400 | 同 | **pass** ×5 | M6-2 |
| 6–12 | `agents/TestListAgentSkills_OmitsContent`、`TestSetAgentSkillsRejectsMalformedSkillID`、`TestAddAgentSkillsRejectsMalformedSkillID`、`TestUpdateAgent_PreservesSkillsInResponse`（GET+PUT）、`TestArchiveRestoreAgent_PreservesSkillsInResponse`（×2） | `/api/agents/{id}/skills*`、`/api/agents/{id}`（archive/restore） | 200/400/400/200/200/200/200 | **404** ×7 | **mismatch** ×7（§5.2 A） | M6-4 |
| 13 | `context/TestPluginActionRequiresTheFeatureFlag` | `GET /v1/context` | 403 | **401** | **mismatch**（§5.2 B） | M6-7 |
| 14–18 | `issues/TestPluginInstallTokenEnforcesGrantedScope`、`TestPluginInstallTokenRunsIssueCommentWorkflow`（×4） | `/v1/issues/{ref}` GET+PATCH、`/v1/issues/{ref}/comments` GET+POST | 403/200/200/201/200 | **401** ×5 | **mismatch** ×5（§5.2 B） | M6-7 |
| 19 | `labels/TestListSkills_IncludesAttachedLabelsAndOmitsContent` | `POST /api/labels` | 201 | 201 | **pass** | **M2**（不承诺，联动登记） |
| 20 | `config/TestGetConfigExposesEnabledPluginsV1Flag` | `GET /api/config` | 200 | 404 | **unmounted**（路由未注册） | **M10**（不承诺，联动登记） |

⇒ **M6 面的 18 条：`unevaluable` 0 / pass 5 / mismatch 13**。`docs/57` §6.2 的硬要求（「每片的 fixture 从 `unevaluable` 变 pass **或按偏离表判 mismatch 并登记理由**」）**达成**；
`unevaluable` 的绝对数下降幅度 = 真库模式下 M6 面 **19 → 0**（stateless 层不变，见 §5.3）。

### 5.2 两种非 pass 的机制（逐条定位；**都不是实现分叉**）

**A. agents 7 条 → 404：fixture 路径里的 agent id 是上游测试库的**硬编码行**，而 ⑨ 的种子**只建 user + workspace**。**
- 证据 ①：fixture 的 `path` 逐字是 `/api/agents/1c331d0b-94fd-412a-a7cc-6a209add28f1…`；种子后的真库里 `SELECT count(*) FROM agent WHERE id='1c331d0b-…'` = **0**（该库 `agent` 表当时共 10 行，全部是各片 e2e 自建的行）。
- 证据 ②：`crates/mc-conformance/src/lib.rs:263-264` 的 `Bindings::resolve` **只认两个占位符** `$testUserID` / `$testWorkspaceID` ⇒ 路径里写死的上游行 id **不做重绑定**（skills 那 5 条能 pass，正因为它们的路径/查询串走这两个占位符：detail 里逐字记着 `query: workspace_id=9b628b53-…`）。
- 证据 ③（语义未丢）：同一批语义有本地真库 e2e —— `crates/mc-http/tests/agents/crud.rs:664` 的 `get_archive_restore_responses_carry_skills`（与 fixture `TestArchiveRestoreAgent_PreservesSkillsInResponse` 同名同义，`:704` 断言响应里 skill 名 = 自建 probe）。
- ⇒ 性质 = **harness 的种子/重绑定缺口**（要 `$testAgentID` 一类的行级种子，或让 fixture 自带建行步骤），归属 **⑨ 工具面（跨波）**，不在本片写集。

**B. plugin 6 条（`/v1` 面）→ 401：fixture 走 `direct_handler`、上游用的是**安装令牌**，抽取时 `Authorization` 头没被带出来。**
- 证据 ①：这 6 条的 `via = handler`（不是 `router`），`detail` 只记状态不记头；期望值 403/200/201 在上游是**装着 `mpi_` 令牌**跑出来的。
- 证据 ②：本地凭据门是 `crates/mc-http/src/routes/v1/policy.rs:253` 的 `plugin_bearer_required`（401 + `plugin_bearer_required`），`/v1` 面的 9 条请求**必须**是插件凭据（`policy.rs:29` 的注释逐字）。
- 证据 ③：上游自己的 `PluginBearerOnly` 在同样「无令牌」的 replay 下也会给 401 ⇒ 不是本地分叉。
- ⇒ 性质 = **fixture 抽取丢凭据**，已在 `docs/32` §9.9「登记的已知缺口（给 M6-INT）」第 1 条登记；收敛动作 = 在 `mc-conformance` 的 harness 里为 plugin 面伪造一枚真实安装令牌（**本片写集外**，登记不实现）。

> **一件不肯抹平的事**：本片**不承诺**「契约等价率」—— 20 条里 6 pass 的分子对不上 `docs/57` §6.2 的隐式期待（该节只写「离开 `unevaluable`」，没写「必须 pass」）。
> `unevaluable → pass` 的 5 条全在 M6-2；13 条 `mismatch` 的每一条都在上表点了名与机制，**没有任何一条被算成 pass**。

### 5.3 `--no-db` 模式仍是同一批 `unevaluable`（预期内，非回归）

stateless 层的这 20 条 = **19 `unevaluable` + 1 `unmounted`**（只有 `config` 那条路径在离线也能判成「无路由」）——
与 base `94f3ecfc` **逐字相同**。原因：这 19 条的 actor 全是 `member`（skills 5 条也是），而 stateless 层只认 `anonymous`（`harness.rs:66` 只建 user/workspace，没有库）。
⇒ `crates/mc-conformance/report.json` 是 **stateless 层快照**，结构上**不可能**因 M6 而变 ⇒ 门 ⑨ 的 `--check` 报 `report matches`，**本片不需要重生成**（0 diff）。
`docs/57` §6.2 的整仓读数（`pass 5 / mismatch 23 / unmounted 31 / unevaluable 306`）因此逐字不变。

## 6. 门 ⑩ 与「一个文件一个写者」交叉检查

- ⑩ **0 违规**；本片只改 docs 与基线 fixture（`docs/**` 不在 ⑩ 的扫描面内）；`scripts/file_size_baseline.tsv` **22 行未变**。
- 交叉方法：对每个 M6 片用 `git diff --name-only <merge>^1 <merge>`（= 该片相对当轮 base 的**真实贡献面**）。
  10 片合计 **252 个文件次**，去重后 **187 个文件**；被 **>1 片**触碰的 **57 个**（其中 56 个非 docs）：

**这 57 个里唯一一个 docs 文件**是 `docs/32-M3-DAEMON-FACE.md`（7 片各自的 §9.x 追加段，写者 `{M6-0,2,3,6,7,8,9}`）—— 追加语义，见下②类。其余 56 个（**50 个含 M6-0 + 6 个不含**）分类如下：

| 类 | 数量 | 例 | 判定 |
| --- | ---: | --- | --- |
| ① 锚点骨架填充（写者集合 = `{M6-0, 某一片}`） | **49** | `crates/mc-skill/src/*`、`crates/mc-repos/src/skill/*`、`crates/mc-repos/src/plugin/*`、`crates/mc-plugin-host/src/*`、`crates/mc-mcp/src/*`、`crates/mc-openapi/src/v1.rs`、`crates/mc-http/src/routes/{skills,plugins,v1,plugin_bridge}/**`、`crates/mc-core/src/skill.rs` | 设计如此（`docs/57` §3.1）：anchor 只建骨架/类型/桩，实现留各片自己的格子 |
| ② 追加型注册表 / 台账 | **5** | `docs/32`（7 片各追加 §9.x）、`crates/mc-http/tests/skills/{main,support}.rs`（M6-2 建、M6-3 追加）、`crates/mc-http/tests/plugins/{main,support}.rs`（M6-5 建，M6-6/M6-8 各追加自己的 `mod` 行）；实测无重复 `mod` / 无重复定义 | 追加语义，**无重复键**；②③④⑤全绿是其反证 |
| ③ 跨片可见性放开（唯一一处 3 写者代码文件） | **1** | `crates/mc-http/src/routes/plugins/install.rs`（写者 `{M6-0, M6-5, M6-6}`）：M6-6 把 `installation_repo` / `deployment_key` / `installation_manifest` 提为 `pub(super)` 并新抽 `parse_installation_manifest` —— `docs/32` §9.8 的 **M6D-11** 逐字登记（无行为改动） | 设计内的复用点收敛 |
| ④ **串行**跨片改语义（一个片对，两处） | **2** | `crates/mc-http/src/routes/plugins/install/{lifecycle,settings}.rs`（写者 `{M6-5, M6-8}`）：M6-8 在安装/升级/启停的**同一个事务**里各插 4–5 行 `hooks_job::{reconcile_schedules_tx, set_schedules_enabled_tx}` —— **两处都在代码注释里点名**「缺口由 `docs/32` §9.6 的 M6-5-D2 指派给本片」「与启停同一个事务」 | **必需接线**（日程投影必须与安装/启停同事务），且 M6-5 → M6-8 是**串行**合入（非并发写）；记此以免下次审计当违规 |

（`Cargo.lock` 只有 M6-0 一个写者 —— 它一次性重生成，此后各片只加自己 crate 的 `[[package]]`，故不出现在多写者表里。）

⇒ **无「同一文件两个并发写者」残留**：56 个非 docs 交叉里 **53** 个是①/②类的结构性共享（49 + 4），1 个是③类（有据的可见性放开），2 个是④类（带出处与理由的串行改动）；第 57 个交叉是 `docs/32` 的 7 片追加段（②类，非代码）。


## 7. 跨片缺口登记（8 行，一处收口）

| # | 项 | 现状与证据 | 归属 / 处置 |
| --- | --- | --- | --- |
| **D8** | **webhook 投递 worker 的轮询循环**（1s ticker + `Notify` + 4 并发）无 owner、**无生产调用点** | ① `crates/mc-autopilot/src/webhook/worker.rs:216` 自陈「本片只提供这一步（`process_next_delivery`），**不提供轮询循环**」；② `grep -rn "process_next_delivery" crates apps` = 只有定义与文档注释，**0 个生产调用点**；③ `apps/mc-server/src/main.rs:162` 只有 `scheduler::start(...)`，无 webhook worker | **已开片 `LUM-1745`**（`backlog`、`high`）。裁定链见 `docs/57` §9.8；**排期约束**：`Notify` 句柄大概率落 `crates/mc-http/src/state.rs`（M6 冻结/热区）⇒ M6 收口后（**现在**）可起手 |
| **R-M6-6** | M6-8 的 hook job 无处运行 | **已闭**：`LUM-1659`（M5-9）合入（#68 / merge `5e7032a6`），`apps/mc-server/src/scheduler/{hook_port,schedule_port,wakeup_port}.rs` 是生产端口实现，`main.rs:162` 有 `scheduler::start` | 不登记为缺口；`docs/57` §8 R-M6-6 的缓解条件已满足 |
| **R-M6-1** | 插件包 JS 校验用**窄口径词法扫描器**，可能放过藏在词法陷阱里的 top-level `import` | `crates/mc-plugin-host/src/bundle.rs`（`SurfaceModuleSyntax`，:192）；上游用 `tdewolff/parse/v2` 走 AST。影响面 = 浏览器里 surface 运行失败（iframe + CSP，**不是越权**） | **无 owner**（跨波）；升级判据 = 实测出现漏判 ⇒ 引 `oxc_parser` 并重开一片。本片只登记 |
| **R-M6-3** | skill 导入自建 GitHub 抓取，与 W8 的 GitHub 面重复 | port 已定义：`crates/mc-http/src/routes/skills/import/fetch.rs:256` 的 `SkillSourceFetcher` + `:265` 的 `HttpSkillSourceFetcher`（默认 `reqwest` 实现，`:218` 装配） | **归 W8**（`docs/60`/`docs/61` 波）：W8 落地后由其实现该 port，本片只登记；**不要**再建第二个 GitHub 抽象层 |
| **R-M6-13** | 回调令牌 `mpc_` 是**进程内存 + 到期前可多次调用**的语义，多实例部署下会**提前 403** | ① 语义与「可重复调用」有测试：`crates/mc-http/tests/public_api/context.rs:33` 的 `callback_token_context_can_be_called_twice`；② 表是进程内单例：`crates/mc-http/src/routes/v1/policy.rs:85` 的 `callback_tokens()`（`docs/32` §9.9 **M6-7-D4**、§9.11 的跨片接口都点名「不要再 new 一张」）；③ 多实例行为：`crates/mc-plugin-host/src/token.rs` 的「不做什么」逐字写「不做 token 的跨进程共享（回调令牌是进程内的，多实例部署下由 M6-7 的 sticky/回源策略处理，本波不扩面）」 | **已知行为，登记不实现**：要跨进程共享（Redis / DB 表）或 sticky 路由才能扩面；`docs/57` §8 R-M6-13 的处置逐字如此 |
| **⑨-p1** | `/v1` 面 6 条 fixture 恒判 `mismatch`（401） | §5.2 B | **⑨ 工具面**：需在 `mc-conformance` 的 harness 里为 plugin 面伪造真实安装令牌（**超出本片写集**） |
| **M6D-10** | `mc-repos/src/plugin/mcp_approval.rs` 的**三处窄读垫片**（`installation_for_workspace` / `package_file_sha256` / `secret_ciphertext`）本该住 M6-5 的 `installation.rs` / `package.rs` | `docs/32` §9.8 M6D-10 逐字把收敛点**登记给 `LUM-1675`**；该文件注释亦有出处 | **登记为收敛项**（不实现）：合并进 M6-5 的仓储 ⇒ 需重开一片（本片 0 代码、不顺手改） |
| **文档** | **M6-1 / M6-4 / M6-5 无 `docs/32` §9.x 落地记录** | `git diff --name-only <merge>^1 <merge> \| grep '^docs/'` 对 `aa3eb749` / `1ca24762` / `2e18514c` **均为空**（M6-2/3/6/7/8/9 都有） | 本片**只登记**：三片的偏离与跨片接口目前只存在于 PR / issue 文本里；若后续波要引用它们（例如 M6-5 的 token 轮换口径），先读该片 PR #67/#71/#72 |

> M6-9 / M6-8 各自在 `docs/32` §9.10 / §9.11 末尾还留了「登记的已知缺口（给 M6-INT / 后续切片）」共 **7 条**（`mc-daemon` 的两条 `path` 依赖边、
> `execenv/sidecar.rs` 的回并、`mcp/runtime.rs` 的 TOML 面、`claude` 插件段、`event` 触发生产者缺位、MCP 传输段未接、hook job 的开关目录）
> —— 它们**已在源文档逐条有 owner 与收敛动作**，本片不重复搬运，只在此点名出处（`docs/32` §9.10 / §9.11）。

## 8. 与计划预测的偏差（逐项，一处不抹平）

| 项 | 预测出处 | 预测 | 实测 | 差异 |
| --- | --- | --- | --- | --- |
| ⑦ `local` | `docs/57` §9.7/§9.9 | 406 | **406** | 0 |
| ⑦ `implemented` | 同上 | `330 = 326 real + 4 placeholder` | **`330 = 326 real + 4 placeholder`** | **0（逐字）** |
| ⑦ `known_gap` | 同上 | 126 | **126** | 0 |
| ⑦ `owners.M6` | 同上 | 0 | **0（owner 板上无 `M6` 键）** | 0 |
| ⑦ `unclaimed`/`regression` | 同上 | 0 / 0 | **0 / 0** | 0 |
| ⑦ `local_only` | 同上 | 9（占位 2） | **9（占位 2）** | 0 |
| ⑦ 不变式 | `implemented + known_gap == 456` | 456 | **330 + 126 = 456** | ✓ |
| ⑦ 基线刷新 | 本片唯一一次 | `344 → 406` | **`344 → 406`（`+62/−0`）** | 0 |
| ⑦ 逐片 ⊿ | §9.7 的 ⊿ 列（+17/+2/+6/+13/+4/+19/+1） | 62 | **62（独立重扫，§4.2）** | 0 |
| ⑦ 形态门 | §6.2 / §8 | M6 `MISSING_ALIAS` = 0，allowlist **空** | **0 defect**；allowlist 只剩表头 | 0 |
| ⑨ M6 面 | 动作 2 | 20 条中 **18 条**离开 `unevaluable` | **18/18 离开**（真库模式）；但拆分为 **pass 5 / mismatch 13** | **预测未细分** ⇒ 按 §5.2 逐条机制登记，**不承诺等价率** |
| ⑨ 报告快照 | 动作 1 | 刷新 | **0 diff**（`--check` = `report matches`） | 无差异，「刷新」= 证明未漂移 |
| ⑩ 文件大小 | §6.3 | 0 新增超限、基线清单只减不增 | **0 违规；`file_size_baseline.tsv` 22 行未变** | 0 |
| 门槛 | `docs/57` §6.4 M6-10 行 | `--with-db` 10/10 | **10/10 / 388s**（交付树热复跑 10/10 / **121s**，通过数逐字相同） | ✓ |
| 写集 | 本 issue | 4 个文件 | 4 个 + `docs/57` §11 占位收口（§1 已声明理由） | **+1（有意）** |

**唯一需要「解释」而不是「相等」的两处**：⑨ 的 20 条拆分（§5.2，两类机制都点名到文件:行）与 §1 的第 5 个文件（计划自己的占位）。

## 9. 复现命令（逐字）

```bash
# 起手（base = M6-8 的 merge commit）
git fetch && git checkout -B agent/devbox5/14ec3a6d1f09 origin/feat/multica-rs-initial   # 94f3ecfc

# ① ⑦ 基线一次性刷新（先备份，再核对 +62/−0 与键集合）
cp docs/fixtures/route-parity-baseline.json ../baseline.before.json
python3 scripts/route_parity.py --write-baseline
python3 - <<'PY'
import json
b=json.load(open("../baseline.before.json"))["routes"]; a=json.load(open("docs/fixtures/route-parity-baseline.json"))["routes"]
print(len(b),"->",len(a),"| +",len(set(a)-set(b)),"| -",len(set(b)-set(a)))
PY
python3 scripts/route_parity.py --quiet && python3 scripts/route_parity.py --list-gaps | head -4

# ② 逐片路由增量（独立重扫；本文件 §4.2）
for pair in M6-0:d62558b3 M6-2:fc4971c2 M6-5:2e18514c M6-7:81c58721 M6-8:94f3ecfc; do
  k=${pair%%:*}; c=${pair##*:}; d=/tmp/m6probe/$k; mkdir -p $d
  (cd $d && git archive $c crates/mc-http/src | tar -x)
  printf "%s %s local=%s\n" "$k" "$c" "$(python3 scripts/route_parity.py --routes-dir $d/crates/mc-http/src --no-baseline --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["counts"]["local"])')"
done

# ③ 真库（一次性角色需 CREATEDB；密码不入任何交付物）
sudo -n -u postgres psql -c "CREATE ROLE mc_lum1675 LOGIN PASSWORD '…' CREATEDB"
sudo -n -u postgres psql -c "CREATE DATABASE multica_lum1675 OWNER mc_lum1675"
export MULTICA_TEST_DATABASE_URL='postgres://mc_lum1675:…@127.0.0.1:5432/multica_lum1675'
bash scripts/gates.sh --with-db            # 期望 10/10

# ④ ⑨ M6 相关 20 条：两种模式（离线 19 unevaluable 是预期；真库必须 0 unevaluable）
cargo run -q -p mc-conformance -- --no-db --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["totals"])'
cargo run -q -p mc-conformance -- --db-url "$MULTICA_TEST_DATABASE_URL" --json > /tmp/conf_db.json
python3 - <<'PY'
import json
db=json.load(open("/tmp/conf_db.json"))
sel=[f for f in db["fixtures"] if "/api/skills" in (f["path"] or "") or "plugin" in (f["path"] or "")
     or (f["path"] or "").startswith("/v1") or "skill" in f["id"].lower() or "plugin" in f["id"].lower()]
from collections import Counter
print(len(sel), Counter(f["outcome"] for f in sel))
PY
psql "$MULTICA_TEST_DATABASE_URL" -tAc "select count(*) from agent where id='1c331d0b-94fd-412a-a7cc-6a209add28f1'"   # 0 ⇒ §5.2 A

# ⑤ 写集交叉（本文件 §6）
for c in d62558b3 aa3eb749 fc4971c2 25592547 1ca24762 2e18514c 6d9bc858 81c58721 94f3ecfc 2394bfcc; do
  git diff --name-only $c^1 $c > /tmp/m6files/$(git log -1 --format=%h $c).txt
done
cat /tmp/m6files/*.txt | sort | uniq -c | sort -rn | awk '$1>1'

# ⑥ D8 的「无生产调用点」（本文件 §7）
grep -rn "process_next_delivery" crates apps | grep -v '//' | grep -v 'mc-autopilot/src/webhook/worker.rs'
```

## 10. 收口状态与后续

- **M6 代码面收口**：M6-0…M6-9 十片全部合入 base（§2），`owners.M6 = 0`，⑦ 基线同轮刷新到 `406` ⇒ **M6 波路由面收口**。
  十个 M6 子 issue 全部停在 `in_review`（按本仓惯例 `done` 归人工）。
- **M6 波无未收口的硬前置**：`docs/57` §7 里唯一挂起的 `LUM-1659`（M5-9，调度器接线）已随 #68 入 base，R-M6-6 闭（§7）。
- **交接给后续波的 8 行缺口**见 §7（7 行未闭 + R-M6-6 已闭）；其中**只有 D8（`LUM-1745`）是「无人接」的掉棒项**，其余 6 组都已有 owner 或已按「登记不实现」处置。
- **快照的归口**：本片是全波**唯一**一次 `route_parity.py --write-baseline`。后续波（M7 / M8）各自的下一次刷新归**各自的 INT 片**
  （`docs/60` / `docs/61` 的计划已按同口径写）；M6 之后**不得**再出现「某片顺手刷基线」。
- **磁盘**：本片 `target/` 用后按惯例整删（冷建 388s / 热重跑见 §3 的耗时表）；起手 29G 可用。
