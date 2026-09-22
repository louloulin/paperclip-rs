# 30 — W0-D：⑧ `schema-drift` 门禁接线

> 切片：**LUM-1402 / W0-D**（补 `docs/25-W0-SCHEMA-DRIFT.md` §9 明写"本切片不做"的那个空档）
> 分支：`feat/multica-rs-w0d-drift-gate` → `feat/multica-rs-initial`（base `00a2a12`）
> 交付物：`scripts/gates.sh`（新增门 ⑧）、`.github/workflows/ci.yml`（`db` job +2 step）、本文件、
> `docs/24-W0-CI.md` §10、`docs/25-W0-SCHEMA-DRIFT.md` §9/§12 回填、`docs/plan1.md` §6.2 回填
> 结论：**⑧ 在本机对真 PG16 全绿（exit 0，1.3s）；0 / 1 / 2 三种退出码都实测过；CI 接线只调 `gates.sh`；
> Actions 上的实跑本切片**不等**（见 §6）**

---

## 1. 为什么做这件事（这个洞是怎么留下的）

`docs/25` 交付了 `scripts/schema_drift.py`（767 处差异的登记表 + `--quiet` 判据 + 0/1/2 退出码语义），
并在 §9 把"怎么接 CI"写成了两句话，**但 §12 明写本切片不改 CI**。同时 `docs/24` 的 `gates.sh`
只有 7 道门。两者拼起来的结果是：

**本仓唯一的 schema 契约机器，在 W0-B 合入之后的每一个小时里都没有被任何自动化跑过一次。**

它的失效方式很隐蔽：`contracts/schema-deviations.tsv` 过期（登记的行已经不再对应任何差异，即 `stale`）、
`migrations/*.sql` 被改写、`mc-repos` 里的 SQL 悄悄改表和上游漂开 —— 这些都不会让 7 道门里任何一道变红，
因为 ①–⑤ 只看"代码能不能编译/测试"，⑥ 只看"本仓 `0001`–`0004` 跑不跑得动"，⑦ 只看路由清单。
⑥ 从来不回答"计划里写的表到底有没有"。

---

## 2. 改了什么

### 2.1 `scripts/gates.sh`：门 ⑧

| 项 | 值 |
| --- | --- |
| 门名 / 编号 | `schema-drift` / ⑧ |
| 命令（脚本内的唯一实现） | `MULTICA_TEST_DATABASE_URL=<db-url> python3 scripts/schema_drift.py --quiet` |
| 打分变量 | `GATE_SCHEMA_DRIFT_EXIT=<code>`（取自脚本退出码，不改写） |
| 是否需要 cargo | 否（纯 Python） |
| 是否需要 PG | **是**（真库 URL；角色需 `CREATEDB`） |
| 默认集合 | 不在（**当时**默认跑 ①–⑤ + ⑦；⑨ 之后加入默认集合，见 §2.3） |
| `--with-db` | **在**（本切片时 `--with-db` = ①–⑤ + ⑥ + ⑧ + ⑦，共 8 门；现在含 ⑨，共 9 门，见 §2.3） |
| 缺库 URL 时 | `exit 2`，与 ⑥ 同一条前置检查（**绝不静默跳过**） |

> 汇总表里 ⑧ 会印在 ⑦ **之前**：`ALL_GATES` 的排列把两道**需要库**的门（⑥ ⑧）放在一起，
> 离线门 ⑦ 收尾；**编号是稳定标识，不表示执行顺序**（⑦ = route parity 的编号早已被 `docs/22`/`docs/24`
> 引用，不重编）。

三个刻意的设计决定：

1. **平时 `--quiet`，红了才补打完整报告。** 判据是退出码；但 ⑧ 绿的时候那份报告有 700+ 行差异明细
   （实测：`missing 460 / extra 140 / differs 158 / apply-exception 9`），塞进 CI 日志只会把真信号淹掉。
   一旦非 0，本门**再跑一遍不带 `--quiet`** 的，把未登记差异逐行打出来（负向实测见 §3.5）——
   红门必须自带诊断，否则等于让人重跑一遍。
2. **⑧ 与 ⑥ 用同一个库 URL 是安全的。** ⑧ 自己建、自己删 scratch 库 `schema_probe_w0b_drift`
   （`--db-name` 可改），**不读**目标库里的表；⑥ 迁移的是 URL 指向的那个库。两者不共享对象。
3. **`usage()` 不再写死行号。** 原来 `--help` 打印 `sed -n '3,40p'`，加一道门就要去数行号，
   而且范围末尾会把 `set -u` 一起打进帮助文本。现改成 awk 读文件顶部的注释块（遇第一行非注释即停）。

### 2.2 `.github/workflows/ci.yml`：挂在 `db` job（不是 `contract`）

```
db job: ⑥ db  →  ⑧ deps (psql + python3)  →  ⑧ schema-drift
```

* 命令仍然只出现在 `gates.sh` 里：YAML 里那一步逐字是 `bash scripts/gates.sh --only schema-drift`，
  没有把 `python3 scripts/schema_drift.py …` 抄进 YAML（`docs/24` §2 的"唯一实现"约定）。
* **不能挂 `contract` job**：那道门是纯离线（只读 `docs/fixtures/`），而 ⑧ 要真 PostgreSQL。
* **deps 步的理由**：⑧ 需要 `psql` 客户端（它对着库 URL 建/删 scratch 库），而 ⑥ 只用 Rust 侧的 sqlx，
  所以 `db` job 原来完全没有 `psql`。ubuntu-latest 镜像自带 psql/python3，这一步只是把"镜像是精简版"
  的情形补上；**让 ⑧ 因为"环境没 psql"而红，会把环境问题错报成 schema 问题**。
  库里 `postgres:16` service 的 `POSTGRES_USER` 是超级用户 ⇒ `CREATEDB` 权限没问题。

### 2.3 门清单的后续变化：⑨ `conformance`（W0-E / LUM-1413，非本切片）

本文件交付时门清单是 **8 门**（`--list` 末行 = `route-parity`）。之后 **W0-E** 接上了
`docs/27-W0-GOLDEN-FIXTURES.md` §9 预留的契约门，清单变成 **9 门**（编号稳定、不重编）：

| 项 | 值 |
| --- | --- |
| 门名 / 编号 / 打分变量 | `conformance` / **⑨** / `GATE_CONFORMANCE_EXIT` |
| 命令 | `env -u MULTICA_TEST_DATABASE_URL -u MULTICA_DATABASE_URL cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json` |
| 是否需要 cargo / PG | 是 / **否**（stateless 层：不拨号、不建库） |
| 默认集合 | **在**（默认 = ①–⑤ + ⑦ + ⑨，共 7 门） |
| `--with-db` | **在**（①–⑨，共 9 门）⇒ 本文件 §3.4 那句"顺序为 ①–⑥⑧⑦、共 8 门"是**当时**的实测记录 |
| 位置 | **不改 ⑧ 的挂点**：⑨ 挂 `contract` job（那道 job 因此补了 rust 工具链），⑧ 仍在 `db` job |
| 与 ⑧ 的关系 | 无关，也不重叠：⑧ 盯 schema 差异登记表，⑨ 盯 golden fixture 回放结论（`report.json`） |

汇总表里 ⑨ 会印在 ⑦ **之后**（`ALL_GATES` 末尾），所以当前顺序是 `① ② ③ ④ ⑤ ⑥ ⑧ ⑦ ⑨` ——
再次强调：**编号是稳定标识，不表示执行顺序**。完整交付说明与实测输出见 `docs/24-W0-CI.md` §11。

---

## 3. 验证（本机实测，命令与输出）

### 3.1 语法与清单

```console
$ bash -n scripts/gates.sh && echo OK                                    # → OK
$ bash scripts/gates.sh --list
fmt / build / clippy / clippy-test-util / test / db / schema-drift / route-parity   # 8 行
$ bash scripts/gates.sh --help | tail -5        # 帮助块完整（awk 重写后不再截断）
```

### 3.2 前置条件：没有库 URL ⇒ exit 2（不是静默跳过）

```console
$ env -u MULTICA_TEST_DATABASE_URL bash scripts/gates.sh --only schema-drift
error: the 'db' / 'schema-drift' gates need a database URL
  pass --db-url 'postgres://user:pw@127.0.0.1:5432/<db>' or set MULTICA_TEST_DATABASE_URL
  (both gates create what they need: ⑥ migrates that DB, ⑧ creates and drops its own scratch DB)
$ echo $?                                                                 # → 2
```

### 3.3 对真 PG16 全绿

库 URL 指向本机 PostgreSQL 16.15 的 `mc_gate_base`（角色 `mc_gate` 带 `CREATEDB`）：

```console
$ bash scripts/gates.sh --only schema-drift --db-url "$MULTICA_TEST_DATABASE_URL"

=== [⑧] gate schema-drift ===
$ MULTICA_TEST_DATABASE_URL=<db-url> python3 scripts/schema_drift.py --quiet
GATE_SCHEMA_DRIFT_EXIT=0

============================= GATE SUMMARY =============================
  #  gate               exit   time  result
  ⑧  schema-drift          0     1s  PASS
  (not selected: fmt build clippy clippy-test-util test db route-parity)
  overall: PASS — 1/1 gate(s) green in 1s
$ echo $?                                                                 # → 0
```

### 3.4 `--with-db` 的选中集合确实含 ⑧（只验"选择逻辑"，不做冷编译）

把 `HOME` 指向一个 `.cargo/bin/cargo` 是 echo+exit 0 的假目录，让 8 门在 1 秒内全部走完
（这一步**只**证明"⑧ 在 `--with-db` 的集合里、顺序为 ①–⑥⑧⑦、汇总表统计到 8 门"，
不替代任何真实门禁证据）：

```console
  ①  fmt    ②  build    ③  clippy    ④  clippy-test-util    ⑤  test
  ⑥  db  PASS (migrate=0,e2e=0)     ⑧  schema-drift  PASS     ⑦  route-parity  PASS
  overall: PASS — 8/8 gate(s) green in 1s
```

### 3.5 负向 ①：未登记的差异 ⇒ 门红，且打印定位行

临时往 `migrations/` 塞一条 `0005_gate_probe.up.sql`（`CREATE TABLE gate_probe_bogus`，**不提交**）：

```console
  extra            table       gate_probe_bogus              (+1 column, 1 constraint)
unregistered differences (1) — run this to get skeleton rows:
  extra     table  gate_probe_bogus
GATE_SCHEMA_DRIFT_EXIT=1
  overall: FAIL — 0/1 gate(s) green in 2s (rerun the red gate(s) with --only)
$ echo $?                                                                 # → 1
$ git status --porcelain                                                  # 只剩我改的 scripts/gates.sh
```

这里顺带量出了 ⑧ 的**判定边界**（写进 §7）：它抓到了新增的 `extra table`，
而 `missing`/`extra column` 等 11 行是 `*` glob 登记的（见 §7），同类新增差异会被吞掉。

### 3.6 负向 ②：库不存在 ⇒ 脚本 exit 2 ⇒ 门记 FAIL（不是 PASS）

```console
$ bash scripts/gates.sh --only schema-drift --db-url 'postgres://mc_gate:…@127.0.0.1:5432/does_not_exist_db'
error: psql failed (2) on 'DROP DATABASE IF EXISTS "schema_probe_w0b_drift"': … FATAL: database "does_not_exist_db" does not exist
GATE_SCHEMA_DRIFT_EXIT=2
  overall: FAIL — 0/1 gate(s) green in 0s
```

### 3.7 CI 文件：解析 + 结构断言

```console
$ python3 - <<'PY'   # PyYAML 解析 + 断言
…   # jobs: fast, db, contract；db.steps 顺序 ⑥ → ⑧deps → ⑧gate；⑧ 的 run 逐字 == 'bash scripts/gates.sh --only schema-drift'
PY
order OK: ⑥ → ⑧ deps → ⑧ gate
```

> PyYAML 的坑同 `docs/24` §5：裸键 `on:` 会被解析成布尔 `True`，别拿 key 列表去判 `"on" in doc`。

### 3.8 改动面

```console
$ git show --stat
 scripts/gates.sh                |  ▲ 门 ⑧ + usage() 重写
 .github/workflows/ci.yml        |  +2 step
 docs/30-W0-DRIFT-GATE.md        |  新增
 docs/24-W0-CI.md / 25 / plan1   |  回填
```

`crates/**`、`apps/**`、`migrations/**`、`contracts/**`、`Cargo.*` **零改动** —— 本切片是接线，不是 schema 切换。

---

## 4. 期望数字（⑧ 在这次接线后的"全绿长什么样"）

```console
$ python3 scripts/schema_drift.py --json --db-url "$MULTICA_TEST_DATABASE_URL" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["counts"])'
{'missing': 460, 'extra': 140, 'differs': 158, 'apply-exception': 9}     # 合计 767
registry  25 row(s): 25 matched, 0 stale (warning), 0 stale (error)
OK — every difference is registered        # exit 0
```

* ⑧ 自身耗时 **~1.3s**（建 scratch 库 → 应用上游 560 + 本仓 4 个 `.up.sql` → 快照 → 比 → drop）。
* 8 门全绿的其余数字与 `docs/24` §4 逐字相同（本切片不动 ①–⑦ 的命令与期望值）——
  ⑨ 加入后 `--with-db` 是 9 门，多出的那一行是 `⑨ conformance 0s PASS`（见 §2.3）。
* `767` 是**切换前**的数字：W0-B2（LUM-1387）采用上游迁移集合、收窄登记表之后，这个数字会大幅下降；
  ⑧ 的判据不变（`exit 0` = 每一处差异都已登记）。

---

## 5. 与 ⑥ 的语义分工（别把两门当重复）

| | ⑥ `db` | ⑧ `schema-drift` |
| --- | --- | --- |
| 问的问题 | 迁移**跑得动**吗？跑完 e2e **过得了**吗？ | 跑出来的 schema **还是不是上游那份**？ |
| 对象 | 本仓 `0001`–`0004` + `mc-repos`/`mc-http` 的 DB 测试 | 上游 560 个 `.up.sql` 的快照 vs 本仓应用集合的快照 |
| 参考物 | 无（只看自己） | `contracts/upstream-schema.json` + `contracts/schema-deviations.tsv` |
| 盲区 | 表/列/约束到底有没有、对不对（**它从来不回答这个**） | 运行时行为（数据、权限、RLS、触发器语义是否被应用） |

---

## 6. 诚实说明：Actions 是否实跑过

与 `docs/24` §6 同一口径：**本切片交付的是"本机实测全绿 + YAML 结构校验"，不是一次绿色的 Actions 跑。**
Actions 上的实跑属"我完成动作后由外部系统触发"的事件，本切片不等它（交付验收看本地 exit code 与
`--only <gate>` 的接线正确性）。三 job 的 YAML 未做其他改动，触发分支仍是 `main` + `feat/multica-rs-initial`。

---

## 7. 已知限制 / 未决

1. **⑧ 只能证明"差异已登记"，不能证明"登记得对"。** 尤其是
   `contracts/schema-deviations.tsv` 里有 **11 行 `*` glob**（`missing` 的 table/column/constraint/index/
   function/trigger + `extra` 的 column/constraint/index + `differs` 的 column/constraint）——
   这些 category 下**将来新出现的**差异会被自动"登记"，⑧ 不会响。§3.5 的负向探针量出了这条边界：
   新增 `extra table` 会红，新增 `missing column` 不会红。W0-B2 收窄登记表时，必须把这 11 行换成显式行
   （这是 LUM-1387 的验收项之一）。
2. **⑧ 只比 `public` schema 的对象定义**，不比数据、不比权限、不比 RLS、不比触发器的运行时行为。
3. **⑧ 依赖 `psql` 客户端与 `CREATEDB` 权限**；不满足时门**红**（exit 2 → FAIL），不会静默跳过 ——
   所以"⑧ 绿"的前提是它真的跑起来了，而不是被环境挡在门外。
4. 上游快照自带 9 条 `apply-exception`（本机无 `pg_bigm` / `pg_cron`，见 `docs/25` §6）；
   ⑧ 沿用同一口径（这 9 条是**登记在案**的差异，不是未知漂移）。
