# 停止条件（Tier-1 硬向量 12 条）的逐条来源、复算命令与残余登记

> 本文件是 `LUM-2110`（M10-8）的交付说明，配套交付物是 `scripts/stop_condition.sh`。
> 上游计划：`docs/64-M10-PLAN.md` §2.6 第 3 项（「一键复算」）+ §9.9（停止条件的操作化定义）。
> **0 路由 / 0 迁移 / 0 产品代码**：本片只新建两个文件（`scripts/stop_condition.sh`、`docs/65-STOP-CONDITION.md`），
> **不改任何既有文件**（写集审计第 ② 条；尤其**不许**改 `scripts/gates.sh` —— 门集合的稳定性是 `docs/64` §9.8 的纪律）。

---

## 1. 为什么要有这个文件

`LUM-1334` 的停止条件是一句自然语言：「把 paperclip-rs 打造 multica rust 版本为止」。
`docs/64` §9.9 把它拆成 12 条**可判定**的 Tier-1 硬向量。本文件回答三个问题：

1. 每条判据的**来源**是哪个脚本 / 哪一行（判据只有一个实现，不许有第二份）；
2. 未达标时的**处置**归哪个片（谁去清、能不能清）；
3. **当轮实测**的差额是多少（§4 —— 当轮重取，禁抄任何历史数字）。

脚本的设计约束，逐条钉死：

| 约束 | 理由 |
|---|---|
| **判据不复制**：12 条全部委托给既有脚本（`route_parity.py` / `slash_alias_audit.py` / `mc-conformance` / `schema_drift.py` / `file_size_check.py` / `gates.sh` / GitHub check-runs），本脚本只取数 + 对账 | 判定逻辑复制一份就有两处真相源；`docs/64` §9.8 要求门集合不许因新工具而漂移 |
| **退出码沿用 `gates.sh` 的逐字语义**：`0` = 全绿、`1` = 有判据不达标、**`2` = 没法开跑** | 「没法判定」既不是绿也不是红；`gates.sh` 对 ⑥/⑧ 缺库报的就是 2。`scripts/gates.sh:31-33` |
| **🔴 禁止为让 exit 变绿而刷新快照**：`crates/mc-conformance/report.json`、`docs/fixtures/route-parity-baseline.json`、`scripts/file_size_baseline.tsv` 的刷新权分别归 M10-9（`LUM-2111`）/ M10-0 / M10-9 | 「还剩多少」是**观测**，不是**可写**的。允许本脚本自己刷快照，它就变成一台「自己给自己判绿的机器」 |
| **不存在的对象按缺报**：T1-10 的 `image` 门与 T1-11 的 `image` job 在本仓**不存在** ⇒ 报 `SKIP-NO-GATE` / `FAIL` 并指名，**不改** `gates.sh`、**不改** CI workflow 去凑 | 见 §5 的「口径缺陷」。🔴 **`docs/37` §229.6 已订正下述两个前提**：门 `image`（`gates.sh --only image`）与 CI job `image` 已由 **M10-7（`LUM-2109`）交付**，两者**都存在**。T1-10b 现读作 `SKIP-NO-ASSET`（本机无 docker，三分档里「没法开跑」），T1-11 现读作 **PASS（CI 4/4 `success`）** |

---

## 2. 逐条判据：来源 → 期望 → 复算命令 → 处置

「格」是脚本实际打印的行；一条向量可能拆成多格（§2.0 说明为什么拆）。
**行号以 `scripts/stop_condition.sh` 在 `959a4002` 上的版本为准**（本片唯一的代码交付物）。

### 2.0 为什么 T1-1 拆成六格（`LUM-2110` §194 的承重订正）

`route_parity.py --json` 的 placeholder 分**两个互不相干的桶**：

| 字段 | 语义 | 本轮实测的键 |
|---|---|---|
| `implemented_placeholder` | **上游 456 条里已注册、handler 仍占位** | `POST /api/issues/{id}/comments/trigger-preview`（`router_line 1978`，owner `M2-A`） |
| `local_only_placeholder` | **上游没有、本仓自加且仍占位** | `GET /api/issues/:id/quick-actions`（`routes/issues/mod.rs:220`） |

⇒ 「还差 1 个 placeholder」这句话本身是**错的表述**（会让人以为只有一条欠账）。脚本必须分别打两行，
T1-1b 还必须**打印键名**，否则「还剩多少」这个问题就没有被回答。

| 格 | 判据 | 期望 | 来源 | 复算命令 | 未达标归谁 |
|---|---|---|---|---|---|
| **T1-1a** (`scripts/stop_condition.sh:141`) 🔴**`LUM-2482` 已订正** | `implemented == upstream`（= `real + placeholder`） | `456` | `route_parity.py:counts.implemented`（或 `implemented_real + implemented_placeholder`） | `python3 scripts/route_parity.py --json \| python3 -c "import json,sys;print(json.load(sys.stdin)['counts']['implemented'])"` | 原判据是 `implemented_real == 456`，而本仓**正确**终态就是 `455 real + 1 placeholder`（那 1 条被计划期裁定「不做」）⇒ 旧判据在任何**正确**实现下都恒 FAIL。改判后的含义：「每一条上游键都已被认领」；**认领得对不对由 T1-1b 管** |
| **T1-1b** (`:196`) 🔴**`LUM-2482` 已订正** | **占位键集合 == 人工裁定白名单**（集合相等，**非个数**） | 与白名单表逐键相等（并打印差异） | `implemented[].placeholder==true` 的 `(method, path)` 集合 vs 脚本内 `placeholder_adjudication.tsv` | 同上（键名取 `report['implemented']` 里 `placeholder==true` 的行） | 两个方向都判：实测 \ 白名单 ⇒ `UNADJUDICATED` FAIL（真欠账）；白名单 \ 实测 ⇒ `STALE-WHITELIST` FAIL（白名单过期，该划掉）。🔴 **白名单严禁自动生成**（从实测值反推 = 每次判「实测 == 实测」= 恒 PASS，判据归零）。改白名单必须先改 `docs/10` / `docs/12` 的裁定 |
| **T1-1c** (`:147`) | `known_gap == 0` | `0` | `counts.known_gap` | 同 T1-1a | 缺口按 `owners` 直方图分派；本轮 `0` ⇒ 连续收口 |
| **T1-1d** (`:149`) | `unclaimed == 0` | `0` | `counts.unclaimed` | 同上 | 无人认领的缺口 ⇒ 立项。**不变量，任何一轮红都是回归** |
| **T1-1e** (`:151`) | `regressions == 0` | `0` | `counts.regressions` | 同上 | 已实现键退回缺口 ⇒ 红。**不变量** |
| **T1-1f** (`:152`) | `local == baseline_routes` | 相等 | `counts.local` vs `sources.baseline_routes` | 同上 | 不相等 = 有键注册了却没进基线 ⇒ 刷基线（**M10-0 / M10-9**，且删键与刷新必须同一次提交，否则 `regressions` 判红） |
| **T1-2** (`:158`) | ⑦ `owners` 直方图**全空** | `{}` | `route_parity.py:owners` | `python3 scripts/route_parity.py --json \| python3 -c "import json,sys;print(json.load(sys.stdin)['owners'])"` | 非空 = 仍有归属中的尾账；按 owner 分派 |
| **T1-3** (`:167`) | `local_only` **逐条有理由** | 每条都能在登记表里查到（**双向**） | `route_parity.py:local_only` × 脚本内登记表（`scripts/stop_condition.sh:171-181`） | 见 §3 | 实测有而表里没有 = **未登记**（FAIL）；表里有而实测已消失 = `STALE`（FAIL）。本片**只登记、不删路由**（删 `/api/health` 的收益为 0，见 `docs/64` §9.3） |
| **T1-4** (`:257`) | ⑦b 形态：`slash_alias_audit.py` exit 0 且 defect 0 | `exit 0 ∧ defect 0 ∧ stale_allowlist 0` | `scripts/slash_alias_audit.py:findings[].allowlisted` | `python3 scripts/slash_alias_audit.py --json` | 名单外的 `MISSING_ALIAS` / `MISSING_EXACT` / `EXTRA_ALIAS` 直接判红 ⇒ 归该键的波次；名单内条目修好后**必须删行**（残留行会被当缺陷） |
| **T1-5** (`:274`) | ⑨ `--no-db`：`mismatch == 0 ∧ unmounted == 0` | `0 ∧ 0`，且 `--check report.json` 逐字一致 | `mc-conformance` 的 `totals`（`crates/mc-conformance/src/lib.rs:703-711`） | `bash scripts/stop_condition.sh`（或 `target/debug/mc-conformance --golden contracts/golden --no-db --json`） | 🔴 **本片只观测**：差额要靠实现路由收敛，**或**由 M10-9 刷快照。刷新权不在本片 |
| **T1-6** (`:346`) | ⑨ `--db-url`：`unevaluable == 0 ∧ mismatch == 0` | `0 ∧ 0` | 同上（带 database 层的回放） | `target/debug/mc-conformance --golden contracts/golden --db-url 'postgres://…' --json` | 未挂载 / 不可判定的 fixture 需要真实现或真 actor ⇒ 归各波次；**缺库时本条 `SKIP-NO-DB` 且整体 exit 2** |
| **T1-7** (`:365`) | ⑨ **判词自带凭据**（两档各自举证）：`pass` 有相符观测 ∧ `unevaluable` 无观测且理由点名前提/凭据 ∧ 声明=观测 ∧ 两个 rate **从原始终数导出** | 五项全真（**不再**要求 rate `== 1.0`） | `report.json` 逐行 `outcome` / `status_observed` / `status_expected` / `requires` / `detail` / `actor` ＋ `totals` ＋ 两个 rate（`crates/mc-conformance/src/report.rs`） | 同 T1-5（读同一份 JSON） | 「让 rate 变好」的实现族：假 `pass`、静默 `unevaluable`、放宽 `supports`、手写 rate ⇒ **均红**。旧口径不可满足：`contract` 的分母是全部 365 条，是「这一层能判多少」的函数（`docs/37` §218.5） |
| **T1-8** (`:380`) | ⑧ `schema_drift`：`missing == 0`（且 exit 0） | `counts.missing == 0 ∧ ok == true` | `scripts/schema_drift.py --json` 的 `counts` / `ok` | `MULTICA_TEST_DATABASE_URL='postgres://…' python3 scripts/schema_drift.py --json` | 未登记的漂移 ⇒ 登记进 `contracts/upstream-schema-deviations.tsv`（该文件**既有**，归 schema 面各波次）。**需库；缺库 `SKIP-NO-DB` + exit 2** |
| **T1-9** (`:419`) | ⑩ `file_size_check`：`violations == 0` | `0` | `scripts/file_size_check.py` 首行 | `python3 scripts/file_size_check.py \| head -1` | 新文件超 800 行 ⇒ 拆；清单内条目只允许变短。**`scripts/file_size_baseline.tsv` 的刷新权归 M10-9**（本片不动它） |
| **T1-10** (`:457`) | 门禁 `gates.sh --with-db` **10/10** | 10 道门全 `GATE_*_EXIT=0` | `scripts/gates.sh` 的 10 行 `GATE_<NAME>_EXIT=` | `bash scripts/gates.sh --with-db --db-url 'postgres://…'`（或 `--gates-log <已有的日志>`） | 逐门 `bash scripts/gates.sh --only <name>` 复现；红的门归对应面 |
| **T1-10b** (`:541`) 🔴**`LUM-2482` 已订正** | 门禁 `gates.sh --only image` 绿 | `exit 0` | `gates.sh --only image` 的退出码 | `bash scripts/gates.sh --only image` | 🔴 **三分档**（与 T1-10 同源的 0/1/2 语义）：`0 ⇒ PASS`、`1 ⇒ FAIL`、`2 ⇒ SKIP-NO-ASSET`（「没法开跑」，缺 docker/podman/buildah，与 ⑥/⑧ 缺库同档）。未给 `--gates-log` ⇒ 没跑过 ⇒ `SKIP-NO-ASSET`。原判据把初值 2 一律打成 FAIL，等于**把「没法开跑」记成「红」** |
| **T1-11** (`:501`) | CI 4 个 job 全绿 | `fast` / `db` / `contract` / `image` 全 `success` | GitHub check-runs API（`--sha` 指定 commit，默认 `HEAD`） | `bash scripts/stop_condition.sh --sha <commit>` | job 红 ⇒ 看该 commit 的 Actions 日志。🔴 **`docs/37` §229.6 已订正**：`image` job **存在**（M10-7 / `LUM-2109` 交付，`.github/workflows/ci.yml` 的 `image:` job），本轮 4/4 `success`。本行早先的「**不存在**」是 M10-7 之前的 relics，未随 M10-7 同步 |
| **T1-12** (`:540`) | 对账：`--golden contracts/golden-local` ⇒ `mismatch 0 ∧ unmounted 0` | 两个 golden-local 根都 `0 ∧ 0` | `scripts/mc_golden_local_check.sh` 的逐根读数行 | `bash scripts/mc_golden_local_check.sh` | 字段级契约不符 ⇒ 归该字段的实现面（`docs/64` §9.7） |

### 2.1 退出码与判定取值

| 判定 | 含义 | 影响退出码 |
|---|---|---|
| `PASS` | 判据达标 | — |
| `FAIL` | 判据不达标（「还剩多少」的一格） | 整体 `1` |
| `SKIP-NO-DB` | 缺库 / 库不可达 ⇒ **无法判定** | 整体 **`2`**（优先于 1） |
| `SKIP-NO-ASSET` | 缺前置资产（二进制 / 门禁日志 / API 凭据）⇒ 无法判定 | 整体 `1` |
| `SKIP-NO-GATE` | 判据指向的对象**在本仓不存在** | 整体 `1`（并使 T1 不可能全绿） |

`SKIP-NO-*` 三类**都不算绿** —— 「没法判定」被当成「绿」是本仓反复踩过的坑（见 `docs/64` §9.8）。
优先级 `2 > 1 > 0`：有 `SKIP-NO-DB` 时即便别的格全绿也报 2，因为「没法开跑」是最强的信号。

### 2.2 `T1-7` 的口径变更（`docs/37` §218 / `LUM-2503`，原写 §216，合并时顺延）

**旧口径**：`contract_equivalence_rate == 1.0 ∧ mounted_equivalence_rate == 1.0`。这条在**任何正确实现**下都不可满足：`contract` 的分母是**全部 365 条**（含 331 条需要真库/真凭据才能判定的结构性 `unevaluable`），所以它是「**这一层能判多少**」的函数，不是实现完成度，也不是正确性。当轮两层实测：stateless `0.093151` / `1.000000`（365/34/0/0/0/331）vs database `0.734247` / `0.807229`（365/268/64/3/0/30）—— 同一指标差 **7.9 倍**。

**新口径**：问**判词有没有自带凭据**（五条，见上表 `T1-7` 行）。它不写死任何当轮数字、不自动生成白名单，并且对「这条判据在什么实现下会 FAIL」有正面回答：假 `pass`、静默 `unevaluable`、放之四海皆准的理由、放宽 `supports`（声明≠观测）、手写/改分母的 rate —— 五类各自对应五条里的一条。

**为什么不是「把当前值写死」**：写死 `0.093151` 会同时删掉鉴别力（实现退化成 `mismatch` 也照样绿）并对下一次正常收敛产生假红。新口径判的是「结论与它的凭据是否对得上」，与具体数值无关。

---

## 3. `local_only` 登记表（T1-3 的 8 条，逐条来源）

登记表在脚本里（`scripts/stop_condition.sh:171-181` 的 `local_only_registry.tsv` here-doc），
**不在** `docs/32`（那是既有文件，本片的写集审计第 ② 条禁止改）。逐条：

| ID | 键 | 理由来源 | 本轮状态 |
|---|---|---|---|
| `M3-LOCAL-01` | `GET /api/issues/:id/reactions` | `docs/15-M3-PLAN.md:213` 逐字「本地自造（`local_only`，M3 不动）」 | REGISTERED |
| `M3PLUS-LOCAL-02` | `GET /api/issues/:id/quick-actions` | `docs/15-M3-PLAN.md:587` 逐字「与上游 `GET /api/quick-actions/` 不是同一条」；🔴 它是 `crates/mc-http/tests/issues/auth.rs:141` 那条**耐久 501 断言**的第六个落点 ⇒ 禁删禁实现 | REGISTERED **[placeholder]** |
| `OPS-LOCAL-03` | `GET /api/health` | `docs/64` §9.3 裁定「保留（不收敛）」：服务+DB 综合语义，被 `apps/mc-cli/src/main.rs` 当探针、被 `mc-openapi` 文档测试断言、被 `mc-conformance` 自造 fixture 断言 | REGISTERED |
| `OPS-LOCAL-04` | `GET /api/health/db` | 同上（DB 专用探针） | REGISTERED |
| `OPS-LOCAL-05` | `GET /api/openapi.json` | `docs/22-ROUTE-PARITY.md` §3.4「本仓自有的运维面」 | REGISTERED |
| `PATS-LOCAL-06/07/08` | `GET/POST /api/me/pats`、`DELETE /api/me/pats/:id` | `docs/17-M1-CONTRACT-GAPS.md` §D4：主路径已迁 `/api/tokens`，本键保留一个发布周期作为 **deprecated alias**（响应带 `Deprecation` 头，`crates/mc-http/src/routes/pats.rs:14,90`） | REGISTERED |

**登记 = 合规**：§9.3 的原话是「`local_only` 是**登记项**……『有主』即合规」。压 `local_only` 到更小
要改 4 处引用者而**收益为零**（`local_only` 不进任何门的分子分母）。

---

## 4. 当轮实测读数（本片交付时，base `959a4002`）

> 纪律：以下数字是**当轮跑出来的**，不是抄 `docs/64` §9.9 的立项当轮快照（那是 `c23fcfad` 的读数）。
> base sha 当轮重取：`git fetch origin feat/multica-rs-initial && git rev-parse origin/feat/multica-rs-initial` ⇒ **`959a4002830e6fa07c35a4df1512ded87d3b1d8d`**。

### 4.1 ⑦ / ⑧ / ⑩（零编译，秒级）

```text
upstream 456 (commit f41fae6b08fb734afcbd13205c0b3203dd0bc9c6) | local 546 registered | baseline 546
implemented 456 = 455 real + 1 placeholder | known_gap 0 | unclaimed 0 | regressions 0
local_only 8 (含 1 placeholder) | owners {} | ok=true
GATE_ROUTE_PARITY_EXIT=0   GATE_SLASH_ALIAS_EXIT=0   GATE_FILE_SIZE_EXIT=0
```

⇒ 与 §196 起手补充在 `959a4002` 上的读数**逐字相同**（「0 路由片合入后八个数不变」第 6 次复现）。

### 4.2 ⑨（本片**自己**实测；`docs/64` §9.9 的 `pass 14 / unmounted 22 / unevaluable 306` 是 `c23fcfad` 的旧快照）

离线层（`--no-db`，365 条）：

```text
fixtures 365 | pass 33 | mismatch 25 | unmounted 1 | placeholder 0 | unevaluable 306
contract_equivalence_rate 0.090411 | mounted_equivalence_rate 0.568966
--check crates/mc-conformance/report.json exit 0（与已提交快照逐字一致）
```

真库层（`--db-url`，同 365 条 + database 层）：

```text
fixtures 365 | pass 187 | mismatch 159 | unmounted 4 | placeholder 0 | unevaluable 15
```

`golden-local`（本仓自造的字段级对账，两个根）：

```text
contracts/golden-local/default  fixtures=10 pass=10 mismatch=0 unmounted=0 placeholder=0 unevaluable=0
contracts/golden-local/token    fixtures=3  pass=3  mismatch=0 unmounted=0 placeholder=0 unevaluable=0
```

#### 4.2.1 订正（`82584529` / LUM-2487 实测）—— 上面两块都是**旧基线**的快照

上面 4.2 的两组数字**都不是当前 `82584529` 的读数**（离线层那份 `pass 33 / mismatch 25 /
unmounted 1 / unevaluable 306` 尤其旧：当前已提交 `report.json` 是 `pass 34 / mismatch 0 /
unmounted 0 / unevaluable 331`）。保留原文是因为它记录的是**当时**的判定能力，不是错抄。

`82584529` 上本片（daemon_token 签发+登记）当场实测：

```text
离线层 --no-db --check report.json   exit 0，report.json 逐字未动（0 路由片）
真库层 --db-url <fresh db>  fixtures 365 | pass 196 | mismatch 136 | unmounted 3 | placeholder 0 | unevaluable 30
  by_actor daemon: 9 pass + 9 mismatch + 2 unevaluable   ← 本片从 20 unevaluable 变过来的
```

🔴 **T1-6 仍是 FAIL，且本片让它 FAIL 得更有信息量**：20 条 daemon 里 18 条已从「不可判定」变成
**真判定**（9 `pass` / 9 `mismatch`），余 2 条挂着 `db_fault_injection`（上游 mockDB 的替身）——
那 2 条是**真·恒不可判定**，本仓真池说不失败就不失败。
⇒ 报 T1-6 的数时必须写清**取自离线层还是真库层**（两者差一个量级），详见 `docs/37` §205.4–205.5。

### 4.3 ⑧（需库）

```text
apply-exception=9, differs=14, extra=22 （missing=0, ok=True, exit 0）
```

⇒ `missing == 0` ⇒ **T1-8 PASS**。「差异全部已登记」与「没有缺表」是两件事，判据取后者。

### 4.4 ⑥ 门禁（本片**自己**跑的 `gates.sh --with-db`）

```text
gates.sh --with-db  ⇒  11 条 GATE_*_EXIT 全 0  ⇒  T1-10 PASS
```

（是 11 条而不是 10 条：门 `db` 会打 `GATE_DB_MIGRATE_EXIT` / `GATE_DB_E2E_EXIT` / `GATE_DB_EXIT` 三行，
合并判据取 `GATE_DB_EXIT`。脚本判的是**没有一条非 0**，不是数行数。）

⚠️ **本 device 的磁盘纪律（本片实测，两次撞 `avail = 0`）**：`cargo build --workspace --all-targets` 的
`target/debug/deps` 峰值 **≈27G**，而本盘 49G 里 OS + 其他 workdir 已占 ≈20G ⇒ 跑 `--with-db` 全量门禁
**必须**：`export CARGO_INCREMENTAL=0`（省 ≈12G 的 `incremental/`）、中途 `rm -rf target/debug/incremental`、
以及**只保留一套 feature 变体的产物**（被 kill 的那一轮会留下 ≈6.1G 孤儿二进制，按 mtime 删）。
`docs/37` §184 的「整跑一次 + 按 `--only` 逐门补」在这里是**必需**而不是可选。


### 4.5 🔴 本片验收：**脚本现在的输出就是「还剩多少」的机器化答案**

base `959a4002`，一条命令（门禁段用本片自己跑出来的 `gates.sh --with-db` 日志对账，不重复编译）：

```bash
bash scripts/stop_condition.sh \
  --db-url "$DB_URL" --gates-log /tmp/gates_evidence.log --sha 959a4002830e6fa07c35a4df1512ded87d3b1d8d
```

```text
  T1-1a  FAIL     implemented_real == 456          got=455   差 1 条（= T1-1b 那条）
  T1-1b  FAIL     implemented_placeholder == 0     got=1     POST /api/issues/{id}/comments/trigger-preview (owner=M2-A, router_line 1978)
  T1-1c  PASS     known_gap == 0                   got=0
  T1-1d  PASS     unclaimed == 0                   got=0
  T1-1e  PASS     regressions == 0                 got=0
  T1-1f  PASS     local == baseline_routes         546 == 546
  T1-2   PASS     ⑦ owners 直方图                  {}
  T1-3   PASS     ⑦ local_only 逐条登记             8/8 registered（占位 1 条单列）
  T1-4   PASS     ⑦b slash_alias_audit            0 finding(s), 0 defect(s), registered=541, stale_allowlist=0
  T1-5   FAIL     ⑨ --no-db                        fixtures 365 pass 33 mismatch 25 unmounted 1 unevaluable 306
  T1-7   FAIL     ⑨ 两个 rate                      contract 0.090411 ∧ mounted 0.568966
  T1-6   FAIL     ⑨ --db-url                       fixtures 365 pass 187 mismatch 159 unmounted 4 unevaluable 15
  T1-8   PASS     ⑧ schema_drift                   missing=0, ok=True（differs=14 extra=22 apply-exception=9）
  T1-9   PASS     ⑩ file_size_check                scanned=1285 baseline=10 violations=0
  T1-10  PASS     门禁 gates.sh 10/10              11 条 GATE_*_EXIT 全 0
  T1-10b SKIP-NO-GATE 门禁 gates.sh --only image    gate 'image' does not exist
  T1-11  FAIL     CI 4 job 全绿                     base 的 3 个 job（fast/db/contract）全 success，image job 不存在
  T1-12  PASS     golden-local 对账                default 10/10 ∧ token 3/3，mismatch=unmounted=0
  ---
  pass=11 fail=6 skip-no-db=0 skip-no-asset=0 skip-no-gate=1 total=18
  exit=1
  failing_ids: T1-1a T1-1b T1-5 T1-7 T1-6 T1-11
```

⇒ **停止条件距离全绿还差 6 格**，逐格归属见 §6。**T1-1a / T1-1b 的 FAIL 是本片的正确产出**，
> 🔴 **历史快照，已被 `LUM-2482` 推翻**（`docs/37` §204）：上面这段 `T1-1a FAIL 差 1 条` / `T1-1b FAIL got=1`
> / `T1-10b SKIP-NO-GATE` 是 **M10-8 当时的实测**，保留作历史。当年的结论「这三格的 FAIL 是正确产出」
> **不再成立**——那三条判据写错了：`implemented_real == 456` 在任何**正确**实现下都恒 FAIL（那 1 条占位
> 是 `docs/10:101` + `docs/12:111` 逐字裁定的不做项），`T1-10b` 则把「没法开跑」（exit 2）记成红。
> 现状：T1-1a/1b/10b 已改为「`implemented == upstream`」+「占位集合 == 裁定白名单」+「0/1/2 三分档」，
> 本仓实跑 `T1-1a PASS / T1-1b PASS / T1-10b SKIP-NO-ASSET`。
不是缺陷：本片交付的是「还剩多少」的机器化答案，不是把差额清零（§6 列了每格归谁、为什么不能在
M10-8 内清）。且在 `image` 门 / `image` job 建出来之前，**T1 根本不可能 exit 0** —— 这是判据与资产
之间的差额，需要 M10-7 的 owner 裁决（§5）。


---

## 5. 🔴 口径缺陷：T1-10 / T1-11 有两格指向**不存在的对象**

`docs/64` §9.9 的 T1-10 后半分句与 T1-11 都要求镜像面，但本仓**没有镜像门、没有镜像 job**：

| 事实 | 证据（当轮实测） |
|---|---|
| `gates.sh` 的 `ALL_GATES` **恰 10 个**：`fmt build clippy clippy-test-util test db schema-drift route-parity conformance file-size` | `scripts/gates.sh:81`；`bash scripts/gates.sh --list` |
| `.github/workflows/ci.yml` **恰 3 个 job**：`fast` / `db` / `contract` | `.github/workflows/ci.yml:38,79,133`；base 的 check-runs 也是 3 个 |

⇒ 照 `docs/64` §9.9 的字面实现必然红。本片的处理（**不改** `gates.sh`、**不改** CI workflow）：

* `T1-10b` 判 `SKIP-NO-GATE`，细节打印 `ALL_GATES` 的逐字清单 ⇒ 一眼看出是「对象不存在」而不是「门红」；
* `T1-11` 判 `FAIL`，细节点名缺失的 job 名（CI 的 check-run `name` 是显示名「fast — fmt / build / …」，
  所以脚本按 ` — ` 前的**短名**比对，否则 3 个绿 job 会被误判成「一个都没有」）。

**建门 / 建 job 归 M10-7（`LUM-2109`，发布面）**：本机 `docker` / `podman` / `buildah` **三者皆无**，
`deploy/` 在 base **不存在** ⇒ 该片需 owner 裁决（装容器运行时 / 改 DoD / 明确不做）。
在这之前，**停止条件不可能 exit 0** —— 这是判据与资产之间的差额，不是本片的实现缺陷。

---

## 6. Tier-2 残余登记（`docs/64` §9.9 要求的「未达标项逐条登记」）

| 残余项 | 当前值 | 归属 | 为什么不能在 M10-8 内清 |
|---|---|---|---|
| `implemented_placeholder` 1 条（`POST /api/issues/{id}/comments/trigger-preview`） | 1 | **未立项**（`docs/10` §2 记「M2-B 明确不做」） | 计划期裁定；要清必须改 `docs/10` 的裁定并重挑耐久断言落点 |
| `local_only` 8 条 | 8 | 已登记（§3） | 「有主即合规」；压它收益为 0 |
| ⑨ `mismatch 25 / unmounted 1`（离线层） | 26 | **M10-9**（`LUM-2111`）刷快照 / 各波次收敛实现 | 快照刷新权归 INT 面（两个 INT 不得同轮） |
| ⑨ `mismatch 159 / unevaluable 15`（真库层） | 174 | 同上 | 同上 |
| ⑨ 两个 rate `0.090411 / 0.568966` | — | 同上 | 同上 |
| 门 `image` / CI job `image` 不存在 | 2 格 | **M10-7**（`LUM-2109`） | 🔴 **已于 M10-7 交付**（`docs/37` §229.6 订正）：两者**都存在**，T1-10b 现为 `SKIP-NO-ASSET`、T1-11 现为 PASS。本行保留是因为**它记录的是当初的缺口归属**，不是当前读数 |
| `helm` / 打包分发 | 明确不做 | — | `docs/64` §9.8 已裁定 |
| 真实前端 E2E / 真实镜像部署 | 明确不做 | — | `docs/64` §9.8 已裁定（Tier-3 人工验收） |

---

## 7. 复算（本片交付的验收命令）

```bash
# 0) 取当轮 base（禁抄本文件里的 sha）
git fetch origin feat/multica-rs-initial && git rev-parse origin/feat/multica-rs-initial

# 1) 起一个测试库（⑥/⑧ 都要）
sudo -u postgres psql -c "CREATE ROLE mc_x LOGIN PASSWORD 'x' CREATEDB;" \
                       -c "CREATE DATABASE multica_x OWNER mc_x;"
export DB_URL='postgres://mc_x:x@127.0.0.1:5432/multica_x'

# 2) 一键复算 Tier-1 全部 18 格（含 gates.sh --with-db 的 10 道门；很贵，几十分钟）
bash scripts/stop_condition.sh --db-url "$DB_URL" --json-out /tmp/stop.json
echo "exit=$?"      # 0 = T1 全绿 / 1 = 有差额 / 2 = 没法开跑

# 3) 便宜版：跳过最贵的门禁段，只判定其余各格
bash scripts/stop_condition.sh --db-url "$DB_URL" --skip-gates

# 4) 门禁已经跑过：不重跑，用日志对账（避免重复几十分钟的编译）
bash scripts/gates.sh --with-db --db-url "$DB_URL" > /tmp/gates.log 2>&1
bash scripts/stop_condition.sh --db-url "$DB_URL" --gates-log /tmp/gates.log

# 5) T1-11 查别的 commit（例如 PR head 而不是 base）
bash scripts/stop_condition.sh --skip-gates --sha <commit>

# 6) 逐格复算（本文件 §2 的「复算命令」列）
python3 scripts/route_parity.py --json | python3 -c "import json,sys;print(json.load(sys.stdin)['counts'])"
python3 scripts/slash_alias_audit.py --json
python3 scripts/file_size_check.py | head -1
MULTICA_TEST_DATABASE_URL="$DB_URL" python3 scripts/schema_drift.py --json | head -40
bash scripts/mc_golden_local_check.sh
```

机器可读产物：`--json-out` 落盘的 JSON，形状是

```json
{"schema": "stop-condition/1", "sha": "...", "exit_code": 1,
 "total": 18, "pass": 10, "fail": 6, "skip_no_db": 0, "skip_no_asset": 0, "skip_no_gate": 1,
 "failing_ids": ["T1-1a", "..."], "undecidable_ids": [],
 "checks": [{"id": "T1-1a", "criterion": "...", "expected": "456", "observed": "455",
             "verdict": "FAIL", "detail": "差 1 条：..."}]}
```

**「还剩多少」的机器化答案 = `failing_ids` + 每格的 `detail`**（每一格都指名到键 / 计数 / 差额）。

---

## 8. 本片的偏离登记（通用 DoD 第 7 条）

`docs/64` §通用纪律第 7 条要求把偏离写进 `docs/32-M3-DAEMON-FACE.md` §9.14 的「自己那一段」。
**本片不写那个文件**：写集审计第 ② 条逐字限定「新建 `scripts/stop_condition.sh` + `docs/65-STOP-CONDITION.md`，
**不改任何既有文件**」，而 `docs/32` 是既有文件（且它的号段已从 §9.14 走到 **§9.19 / `## 39.`**，
issue 描述里记的「当前末号 §9.11」已过期）。⇒ 偏离登记落在**本文件**（§5 与 §6），两处口径以本文件为准。
登记三条：

1. **T1-1 拆成 6 格、T1-1b 必须打印键名** —— 依据 `LUM-2110` 描述 §194 的承重订正（两个 placeholder 桶互不相干）。
2. **T1-10b / T1-11 按缺报而不是假装判据成立** —— 依据写集审计第 ② 条（不许改 `gates.sh` / CI）。
3. **`local_only` 登记表放在脚本内而不是 `docs/32`** —— 同一条写集约束；判据是**双向**的（实测 ⊆ 表 且 表 ⊆ 实测），
   所以表行失效会自己暴露成 `STALE`，不依赖人去同步两份文档。
4. 🔴 **`LUM-2482` 的两条偏离登记**（依据 `docs/37` §204 / issue 描述 §201.4 + §201.6）：
   ① **T1-1a/1b 的判据本身被改写**（不只是加登记）——旧判据在任何正确实现下恒 FAIL，等于没有鉴别力；
      新判据 = 「`implemented == upstream`」+「占位键集合 == **人工**裁定白名单」。白名单写在脚本内
      （与 `local_only` 登记表同款做法），**严禁自动生成**。改白名单 = 改一条人工裁定，权归改 `docs/10`/`docs/12` 的人。
   ② **T1-10b 由二档改三分档**（`0 PASS / 1 FAIL / 2 SKIP-NO-ASSET`），理由同①：一个数字两套语义。
      未改 `gates.sh`、未改 CI、未改 `route_parity.py`、未刷任何 baseline、未用 `--test-threads=1`「修绿」。

> 🔴 **本节登记的通用教训（`LUM-2482` 的真正承重）**：
> **收口判据本身也是需要被收口的代码。** `known_gap == 0` 已经连续十余轮为 0，但一份没人核对的
> 判据可以在每轮 `exit 1` 的输出里只占一行、与真缺陷混在一起。
> **每条判据都要能回答「它在什么实现下会 FAIL」**——答不上来的（通常是「要求一个已被人工裁定不做的
> 东西为零」「把没法开跑记成红」）多半就属于这一类。判读纪律：**超时 ≠ 失败 ≠ 全绿**。
