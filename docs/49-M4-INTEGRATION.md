# M4-INT：M4 集成收口（10/10 门禁 + ⑦ 基线 290→300 + ⑨ 快照 no-op）

> 交付单 **LUM-1476**｜起手 base `7f0db0c`（= 合并 #52 后的 `feat/multica-rs-initial`，含 `659f19e`）｜分支 `agent/devbox5/2dda446b1090`
> 依据：`docs/42-M4-PLAN.md` §5.2/§7.5、`docs/21-M2-INTEGRATION-RECIPE.md` §10.5、`docs/22-ROUTE-PARITY.md` §4.4
> 口径声明：本文件所有 ⑦/⑨/⑩ 数字**逐字取自当轮 gate 日志**（`gates.sh --with-db` 全量 `tee`），无一处手写。

## 0. 结论（三行）

1. `bash scripts/gates.sh --with-db` → **10/10 PASS**（含 ⑥db、⑧schema-drift），当轮耗时 81s。
2. ⑦ 基线 **290 → 300**：本次吸收 **M4-4 的 10 条 chat 键**，`+10 / −0`；**M4 上游 45 条全部 implemented**（`gaps by owner` 里没有 M4 ⇒ M4 board = 0）。
3. ⑨ 快照重生成 = **no-op**：`mc-conformance --write` 跑完 `crates/mc-conformance/report.json` **零字节变化**，`⑨ --check` 仍 `report matches`。

## 1. 写集（本片只碰 4 个文件）

| 文件 | 动作 | 说明 |
| --- | --- | --- |
| `docs/fixtures/route-parity-baseline.json` | 改 | ⑦ 基线 `290 → 300`（+10 键，见 §4） |
| `scripts/route-owners.tsv` | 改 | M4 三条规则清空（见 §6） |
| `docs/49-M4-INTEGRATION.md` | 新增 | 本文件 |
| `crates/mc-conformance/report.json` | **未变** | 重生成 no-op（见 §5），列此以证"碰过但无 diff" |

**未碰**：任何 `crates/**` 源码、`Cargo.toml`/`Cargo.lock`、`migrations/**`、`docs/fixtures/upstream-routes.tsv`、
`docs/fixtures/slash-alias-allowlist.tsv`、`scripts/file_size_baseline.tsv`、`routes/mount.rs`。
⇒ 与在飞的 M5-1（`crates/mc-autopilot/**` + `mc-http/src/routes/autopilots/**`）与 M5-6（`mc-http/src/routes/issues/**` + `mc-autopilot/src/wakeup/**`）**零文件交集**。

## 2. M4 合并波台账（六片，全部已入 base）

| 片 | issue | PR | merge commit | 内容 |
| --- | --- | --- | --- | --- |
| M4-0 anchor | LUM-1470 | #42 | `fd3c81c` | 3 空 crate + 10 repos 空模块 + 3 路由空切片 + 删 M0 六键 + ⑦ 基线 248→242 |
| M4-0b 抽取器 | LUM-1471 | #46 | `0bb888a` | 规则 I4（直调 handler 站点入库），fixture 58 → 365 |
| M4-1 project | LUM-1472 | #43 | `b3602dc` | `/api/projects*` 10 条 |
| M4-2 squad | LUM-1473 | #44 | `cf65ed3` | `/api/squads*` 10 条 |
| M4-3 chat 读面 | LUM-1474 | #45 | `7be91a6` | chat 读面 15 条 |
| M4-4 chat 派发面 | LUM-1475 | #51 | `542e833` | chat 写面 10 条（**本片要补的基线记忆就是这 10 条**） |
| M4-INT | **LUM-1476** | 本 PR | — | 门禁 + 基线刷新 + owner 行清空 + 本记录 |

⑦ 基线在 M4 波里的三次变动（`git show <rev>:docs/fixtures/route-parity-baseline.json` 实测键数）：

| 时点 | 键数 | 动作 |
| --- | --- | --- |
| `ab76e72`（M3 尾斜杠收口） | 248 | 波前 |
| `6711ea9`（M4-0 anchor） | **242** | −6：删 M0 三条幽灵占位（6 键） |
| `b09ac47`（M5-0 占位预删，PR #50） | **290** | +50 / −2：吸收 M4-1/2/3 的 chat 20/项目 15/squad 15，删 M0 两条 autopilot 占位 |
| 本片 | **300** | +10 / −0：补 M4-4 的 10 条 chat 键（M5-0 刷新时 M4-4 尚未合入） |

## 3. 门禁读数（当轮 `gates.sh --with-db`，10/10）

同一棵树（本片写集定稿后）复跑多次：`81s / 90s / 139s`，**十道门的 exit 码与 ⑤⑥⑦⑨ 的计数逐字相同**；耗时随同机并发切片浮动，下表为其中一轮：

```
  #  gate               exit   time  result
  ①  fmt                   0     1s  PASS
  ②  build                 0     1s  PASS
  ③  clippy                0     0s  PASS
  ④  clippy-test-util      0     0s  PASS
  ⑤  test                  0    31s  PASS
  ⑥  db                    0    24s  PASS  (migrate=0,e2e=0)
  ⑧  schema-drift          0    28s  PASS
  ⑦  route-parity          0     0s  PASS
  ⑨  conformance           0     5s  PASS
  ⑩  file-size             0     0s  PASS
  overall: PASS — 10/10 gate(s) green in 90s
```

- ⑤ `env -u MULTICA_TEST_DATABASE_URL … cargo test --workspace`：**1192 passed / 0 failed / 110 ignored**（94 个 test target）。
- ⑥ 真库：`mc-migrate run` + `cargo test -p mc-repos -p mc-http --features mc-http/test-util -- --ignored` → **221 passed / 0 failed / 0 ignored**。
- ⑦ 逐字：`upstream 456 (commit f41fae6b08fb) | local 300 registered | baseline 300` / `implemented 241 real + 2 placeholder = 243 / 456  known_gap 213  unclaimed 0  regression 0  local_only 11` / `OK: every upstream route is either implemented or owned`。
- ⑨ 逐字：`golden: contracts/golden  fixtures: 365` / `pass 5  mismatch 23  unmounted 31  placeholder 0  unevaluable 306` / `契约等价率 = 5/365 = 1.4%` / `已接入路由等价率 = 5/28 = 17.9%` / `离线可判定（anonymous）= 5/59 pass` / `report matches crates/mc-conformance/report.json`。
- ⑩ `python3 scripts/file_size_check.py --quiet` exit 0（`docs/**` 不在 ⑩ 的扫描面内，本文件不影响该门）。

冷/热对比（三轮 `--with-db`）：首轮 **423s**（`target` 从零到 13G），复跑 **81s / 90s / 139s**（热）。三轮 ⑦ 读数唯一差异是 `baseline 290 → 300`（即本片刷新本身），其余计数逐字相同。

> 口径说明：本表取自**写集定稿那棵树**上的门禁运行；本文件（`docs/**`）后续若有文字修订，不进入任何门的判据面 —— ⑦ 只读 `crates/mc-http/src` + `docs/fixtures/*`、⑨ 只读 `contracts/golden` + `crates/mc-conformance/report.json`、⑩ 显式跳过 `docs/**`、②–⑥⑧ 面由 `crates/**`/`Cargo.*`/`migrations/**` 决定。因此文档文字修订后用 `--only fmt,route-parity,conformance,file-size` 复验即可。

## 4. ⑦ 基线刷新明细（逐键可核）

命令：`python3 scripts/route_parity.py --write-baseline`

- 刷新前（首轮门禁读数）：`local 300 registered | baseline 290`
- 刷新后（`--write-baseline` 自己打印）：`local 300 registered | baseline 300`，`regression 0`
- 文件 diff：`1 file changed, 10 insertions(+)`，**0 删除**

本次**吸收**的 10 键（`old → new` 集合差，逐键）：

```
DELETE /api/chat/sessions/:param/queued-tasks
GET    /api/chat/history
GET    /api/chat/pending-tasks
GET    /api/chat/pending-tasks/has-any
GET    /api/chat/sessions/:param/pending-task
GET    /api/chat/thread
POST   /api/chat/sessions/:param/messages
POST   /api/chat/sessions/:param/onboarding
POST   /api/chat/sessions/:param/queued-tasks/:param/prioritize
POST   /api/chat/sessions/:param/quick-actions/regenerate
```

本次**退役/删除**的键：**0 条**。
⇒ 这 10 条全部属 M4-4（PR #51，合并于 `542e833`）的 chat 派发面，与派发单预测的键集合逐条一致；
刷新只把"删除记忆"补回来（`regression` 只对"基线里有、当刻树里没有"报警），**不改变** ⑦ 的 `implemented/known_gap` 任何计数（`implemented 243 / known_gap 213` 刷新前后逐字相同）。

**为什么之前是 290**：M5-0（PR #50，`b09ac47`）在 **M4-4 合入之前**刷新基线，其吸收面不含 M4-4 的 10 键；
13:00 cycle（LUM-1602）在 `docs/37` §35 已把这个"no-op"误判更正为 `290 → 300`，本片实测与该预测逐字吻合。

## 5. ⑨ 快照重生成：no-op（证据）

```bash
env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
  cargo run -q -p mc-conformance -- --no-db --write crates/mc-conformance/report.json
```

- 输出与 ⑨ 门禁同一份读数（`pass 5 / mismatch 23 / unmounted 31 / placeholder 0 / unevaluable 306`，等价率 1.4% / 17.9%）。
- 跑完 `git status --short` 只有 `docs/fixtures/route-parity-baseline.json` 一行 ⇒ `crates/mc-conformance/report.json` **未变化**。
- 当刻契约等价率：**stateless 层 5/365 = 1.4%**（已接入路由 5/28 = 17.9%；离线可判定 5/59）。数字与 `docs/15` §10.4 的 M3 口径同源，**不**与真库层（`docs/27` §5.1）混用。

## 6. `route-owners.tsv` 的 M4 行清空

- **路径更正**：计划文档写的是 `docs/fixtures/route-owners.tsv`，该文件不存在；规则表实际在 **`scripts/route-owners.tsv`**（`docs/22` §4.2 与 `gen_upstream_routes.py --owner-rules` 都指向它）。
- 动作：删掉 `^/api/chat/`、`^/api/projects`、`^/api/squads` **三条 M4 规则**（其余 77 条不动）。
- 为什么安全（两层证据）：
  1. 规则表**只填空**：`docs/fixtures/upstream-routes.tsv` 里 M4 前缀的 **45 条路由全部已有显式 owner（空 owner 0 条）**，显式值优先于规则 ⇒ 删规则**不可能**把任何一条打回 `unclaimed`；`⑨`/`⑦` 都不读这个文件。
  2. 实测：删规则前后各跑一次 `route_parity.py --quiet`，读数**逐字未变**（`local 300 / baseline 300 / implemented 243 / known_gap 213 / unclaimed 0 / regression 0 / local_only 11`）。
- 语义：M4 已收口（M4 域 45/45 implemented，board gap = 0），规则行不应再"认领"未来新增的 `/api/chat|projects|squads` 路由 —— 删掉后，上游若在这些前缀下新增路由而无人认领，⑦ 会以 `unclaimed ≠ 0` **红**（可被看见），这正是规则表存在的意义。

## 7. `known_gap` 与占位归属（当刻 `--json` 实测）

`known_gap 213` 按 owner（`gaps by owner` 逐字）：

```
M6=55  M9=33  M7=24  M8=24  M5=22  M3+=16  M2-A=14  M3=11  M2-E=9  M10=5      # 合计 213
```

`implemented 243` 按 owner（`route_parity.py --json` 的 `implemented` 集合）：M3 90 / M1 34 / **M4 45** / M2-A 37 / M2-C 14 / M2-B 8 / M5 7 / M2-D 3 / M3+ 1 / M8 1 / M9 1 / M6 2。

- **剩余占位归属**：`implemented` 里的 2 条 placeholder = `GET /api/skills/` + `POST /api/skills/` ⇒ 归 **M6**（`docs/44-M5-PLAN.md` 的 M5 面 0 占位）；另有 3 条本仓自造占位（`GET|POST /api/plugins`、`GET /api/feature-flags`）落在 `local_only`（上游无同路径路由，非合同面）。
- `local_only 11`（逐条，取自当轮门禁日志）：`GET|POST /api/issues/:id/reactions` 之外的完整列表 = `GET /api/issues/:id/reactions`、`GET /api/issues/:id/quick-actions`、`GET /api/health`、`GET /api/health/db`、`GET /api/openapi.json`、`GET|POST /api/plugins`、`GET /api/feature-flags`、`GET|POST /api/me/pats`、`DELETE /api/me/pats/:id`。
- `M3` 的 11 条 `known_gap` 含 11 条 `cloud-runtime`（**计数噪声**，`docs/15` §9.1 已裁决判给 M9，改动 fixture 单元格不在本片范围 —— `unclaimed` 仍为 0）。

## 8. 与计划文档的偏差（实收 vs 预测）

| 计划说法 | 当刻实测 | 处置 |
| --- | --- | --- |
| `docs/42` §5.2：M4-INT 一次性 `--write-baseline`（→234） | 实际 `290 → 300` | 该预测基于 M4-0 时点（242/248 量级）；此后 M3 尾斜杠收口、M5-0 刷新已各吸收一批 ⇒ **以当刻实测为准**，本文件不沿用预测 |
| `docs/42` §7.5：记录文件写 `docs/43-M4-INTEGRATION.md` | `43` 已被 M3-7-fu 占用 | 用当刻空号 **`docs/49`**（`44`=M5 计划、`45`=M4-4、`46`/`47`/`48`=M5-1/M5-6/M5-7 已占） |
| `docs/42` §7.5：清 `docs/fixtures/route-owners.tsv` 的 M4 行 | 该路径不存在 | 实际 `scripts/route-owners.tsv`（§6） |
| `docs/37` §33.3/§34.3：M4-INT 刷新是 no-op | 错 | 已在 `docs/37` §35 更正为 `290 → 300`，本片实测复现 |
| ⑨ 快照"重生成" | no-op（零 diff） | M5-7 之前的各片已把 report.json 带到当刻状态；本片只留证据（§5） |

## 9. R13 与后续

- **R13**：本片（基线刷新）**不得**与 `LUM-1572`（M5-INT 基线刷新）/ `LUM-1580`（⑦ 占位正则修复）批进**同一次**合并；后合者必须按**当刻** `local` 实测值重刷（`--write-baseline` 只吸收当刻树里注册过的键）。
- 若 M5-INT 先刷新：本片以「无键可加 ⇒ 只留证据」收口（本轮已把 M4-4 的 10 键补回，故该情形下 M5-INT 的刷新对 M4 域是 no-op）。
- ⑦/⑨ 的下一次刷新归属：M5 各片（autopilot / wakeup / scheduler / issue 面）合入后由 **M5-INT（LUM-1572）** 一次性吸收，切片不得各自刷新。

## 10. 复现命令（逐字）

```bash
# 真库（一次性库/角色，角色需 CREATEDB；密码不入任何交付物）
MULTICA_TEST_DATABASE_URL='postgres://mc_lum1476:***@127.0.0.1:5432/multica_lum1476' \
  bash scripts/gates.sh --with-db 2>&1 | tee gates.log

python3 scripts/route_parity.py --write-baseline              # 290 → 300
python3 scripts/route_parity.py --quiet                       # exit 0，regression 0

env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL \
  cargo run -q -p mc-conformance -- --no-db --write crates/mc-conformance/report.json   # no-op

# 逐键核对本次吸收面（对照提交前的基线副本）
python3 - <<'PY'
import json
a = set(json.load(open('baseline-before.json'))['routes'])
b = set(json.load(open('docs/fixtures/route-parity-baseline.json'))['routes'])
print('ADDED', len(b - a)); print('\n'.join(sorted(b - a)))
print('REMOVED', len(a - b)); print('\n'.join(sorted(a - b)))
PY
```
