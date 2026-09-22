# 25 — W0-B：上游 schema 快照与 drift 对账（LUM-1386）

本文件是 `docs/plan1.md` **P4「schema-first，禁止临时造表」** 与 **§6.3 schema 对账** 的操作手册：
上游快照怎么生成、drift 怎么算、首次运行的真实计数是多少、偏离表怎么用、以后怎么进 CI。

**本切片只建机制与快照，不切换运行时实际应用的迁移集合。** 切换迁移集是紧随其后的 **W0-B2（LUM-1387）**。
本切片不改 `crates/**`、不动 `migrations/0001`–`0004`、不改 CI。

## 0. 为什么需要它（要杀掉的故障模式）

plan1 **D1「计划以为有表、实际没有」**已经发生过两次：`docs/10-M2-PLAN.md` 把
`issue_label` / `issue_to_label` 写成"已有"，实际 `migrations/` 里根本没有；`issue_properties`
同样。发现方式一直是 **agent 事后实测撞到**，代价是每个后来者重新推导同一个否定事实。

本切片把这件事变成一条命令：

```bash
python3 scripts/schema_drift.py --db-url "$MULTICA_TEST_DATABASE_URL" --json
```

`exit 0` = 本仓 schema 与上游快照的**每一处**差异都在 `contracts/schema-deviations.tsv` 里登记过
（有原因、有承接 issue）。差异**不登记**就是非 0 —— 这就是以后进 CI 的红灯。

## 1. 交付物

| 路径 | 性质 | 规模 | 说明 |
| --- | --- | --- | --- |
| `migrations/upstream/*.up.sql` | 上游逐字副本（只读） | 560 个文件 / 2.5 MB | 不改名、不重排、不改内容、不加注释；上游 560 个 `.down.sql` **不入库**（本切片范围外） |
| `migrations/upstream/PIN` | 溯源 | 22 行 | 上游 repo + 完整 commit + 生成时间 + 四条命令（clone / vendor / build / audit） |
| `migrations/upstream/MANIFEST.sha256` | 逐字校验 | 560 行 | `cd migrations/upstream && sha256sum -c MANIFEST.sha256` |
| `scripts/schema_snapshot.py` | 提取器（两侧共用） | 1093 行 | 从 `information_schema` + `pg_catalog` 取归一化对象快照（表/列/类型/可空/默认/PK/UNIQUE/FK/CHECK/索引/函数/触发器） |
| `scripts/build_upstream_schema.py` | 快照生成（跑一次） | 433 行 | 按 `PIN` 在独立 scratch 库重放 560 个上游迁移 → `pg_dump --schema-only` → `contracts/upstream-schema.{sql,json}` + `contracts/upstream-apply-exceptions.tsv`；依赖扩展/本机不支持的语句**显式记录并跳过** |
| `scripts/schema_drift.py` | 对账（进 CI 的那个） | 807 行 | 把本仓 `migrations/` 应用到第二个 scratch 库 → 与上游快照 diff → 查偏离表 → `exit 0/1/2` |
| `contracts/upstream-schema.json` | 比对基准（只读） | 720 KB / 2146 个对象 | CI **不重新生成**，只读 |
| `contracts/upstream-schema.sql` | 人类可读规格 | 190 KB | 文件头带上游 commit |
| `contracts/upstream-apply-exceptions.tsv` | 生成的例外清单 | 9 条 | 每条被跳过语句的文件/行/类别/原因/受影响对象 |
| `contracts/schema-deviations.tsv` | 偏离登记表（人手维护） | 767 行 | 见 §6 |

## 2. 上游快照

- 上游：`github.com/louloulin/multica` @ `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`（`main`）
- 实测规模：`server/migrations/*.up.sql` = **560** 个 → 应用后 **1133** 条语句成功，**9** 条按例外跳过
  （`contracts/upstream-apply-exceptions.tsv`），最终对象 **2146** 个，表集口径见 §2.2
- 快照绑定 **PostgreSQL 16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)**：`format_type()` / `pg_get_*def()` 的输出随
  大版本变化，换 PG 版本必须重新生成，不能沿用。

### 2.1 生成配方（重生成时照抄）

```bash
# ① 逐字入库（全 blob 浅克隆 —— 见 docs/20-UPSTREAM-ANALYSIS-RECIPE.md §4 的 74 分钟陷阱）
git clone --depth 1 --single-branch --branch main https://github.com/louloulin/multica upstream-multica
git -C upstream-multica checkout --detach f41fae6b08fb734afcbd13205c0b3203dd0bc9c6
cp upstream-multica/server/migrations/*.up.sql migrations/upstream/
(cd migrations/upstream && sha256sum *.up.sql > MANIFEST.sha256)   # 然后写 PIN

# ② 逐字自检（必须 560 行全 OK）
cd migrations/upstream && sha256sum -c MANIFEST.sha256 | grep -c ': OK'

# ③ 生成快照（自己建/自己 drop scratch 库 schema_probe_w0b_up）
MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
  python3 scripts/build_upstream_schema.py

# ④ 与上游 clone 字节级比对（可选，取信于 PIN 之外的第三只眼）
diff -rq upstream-multica/server/migrations migrations/upstream   # 只应报上游多出的 *.down.sql
```

`upstream-multica/` **不进提交**（放 workdir 外或写 `.gitignore`）。

### 2.2 表集口径（114 / 137 / 138 的由来）

560 个上游迁移里 `CREATE TABLE` 去重 **137** 张，其中 **24** 张被后续迁移 `DROP`（`344_plugin_v2_reset`
退役 12 张 plugin v1 表、usage dashboard/rollup 6 张，另有 `daemon_pairing_session` / `runtime_usage` 等）
⇒ **上游 head 最终表集 = 114 张**（Go runner 自建的 `schema_migrations` 一起算 = 115）。

`contracts/upstream-schema.sql` / `.json` 是**跑完 560 个迁移后 `pg_dump`** 得到的，天然是 114 口径。
只按 `CREATE TABLE` 文本计数会在 114/137/138 之间漂 —— **所有对外数字必须注明是哪一个口径**。

### 2.3 9 条 apply-exception（不许静默跳过）

| 类别 | 对象 | 文件:行 |
| --- | --- | --- |
| `extension-unavailable` | `extension:pg_bigm`、`index:idx_issue_title_bigm`、`idx_issue_description_bigm`、`idx_comment_content_bigm`、`idx_project_title_bigm`、`idx_project_description_bigm`、`idx_issue_properties_bigm` | `032:3`、`032:13`、`033:3`、`036:13`、`036:22`、`039:2`、`446:19` |
| `extension-unavailable` | `extension:pg_cron`、`statement:103_drop_legacy_daily_rollups.up.sql:101` | `076:24`、`103:101` |

本机与 CI 都没有 `pg_bigm` / `pg_cron`，上游用 `DO $$ ... EXCEPTION WHEN OTHERS THEN RAISE NOTICE ... $$`
自己包住了这些语句 —— 跳过是上游设计的降级路径，但**必须在偏离表里各登记一行**，所以
`contracts/upstream-apply-exceptions.tsv`（生成）与 `contracts/schema-deviations.tsv`（登记）之间有交叉校验：
**有跳过没登记 → 非 0；有登记的 `apply-exception:` 行却没跳过 → 也算 stale（error）**。

## 3. drift 口径与归一化规则

```
side A（本仓）  migrations/*.up.sql            → scratch 库 → information_schema + pg_catalog 快照
side B（上游）  contracts/upstream-schema.json （已入库；CI 永不重新生成）
```

两侧**用同一个提取器**（`scripts/schema_snapshot.py`），所以比对是**对象对对象**，而不是两份
`pg_dump` 文本的脆弱 diff。归一化写死在 `schema_snapshot.py::NORMALIZATION` 并在快照 `meta` 里留档：

| 项 | 规则 |
| --- | --- |
| schema | 只取 `public`；键不带限定名，定义里的 `public.` 前缀剥掉 |
| 类型别名 | `format_type(atttypid, atttypmod)` ⇒ `varchar(255) == character varying(255)` |
| 默认值 | `pg_get_expr(adbin, adrelid)`，空白折叠 |
| 约束背书的索引 | PK/UNIQUE/EXCLUDE 背后的索引**不算独立对象**（约束行已覆盖） |
| 扩展成员 | `pg_depend.deptype='e'` 的对象排除，扩展集合另记（本机：`pg_trgm` / `pgcrypto` / `plpgsql`） |
| 序列 | serial/identity 通过列的 `nextval()` 默认值可见，同时作为 sequence 对象存在 |
| 函数 | `pg_get_functiondef()`，剥 `public.`、规范化换行，函数体逐字 |
| 空白 | 定义内部连续空白折叠成一个空格 |
| 服务端 | PostgreSQL 16（**版本相关**，见 §2） |

四类差异（`scripts/schema_drift.py` 头部有同样的表述）：

| 类别 | 含义 |
| --- | --- |
| `missing:` | 上游有、本仓迁移没建出来 |
| `extra:` | 本仓建了、上游快照没有 |
| `differs:` | 同名对象定义不同。**列序（`attnum`）不同也算差异**——重排过的表是差异，不是契约变更 |
| `apply-exception:` | 上游本来会有，但本机因缺扩展**根本不可能存在**（来自 §2.3 的 9 条） |

报告里 missing/extra 表的**从属对象（列/约束/索引/触发器）折叠进表那一行**并计数
（`+19 owned objects (...)`)），否则 88 张表的列会把信号淹掉；`differs:` 逐对象一行。

## 4. 首次真实运行计数（2026-09-22，本机）

### 4.1 `--apply-set local`（默认，= CI 模式：只跑本仓 `0001`–`0004`）→ **exit 0**

```
baseline  contracts/upstream-schema.json — upstream f41fae6b08fb, 560 migrations, 2146 objects
apply set local — 4 file(s), 54 statements, 4 applied, 0 skipped
scratch   schema_probe_w0b_drift (dropped) on PostgreSQL 16.15 — 508 objects

counts    missing 460 | extra 140 | differs 158 | apply-exception 9   (= 767)
folded    930 column, 288 constraint, 166 index, 6 trigger 折叠进 missing/extra 表行
registry  767 row(s): 767 matched, 0 stale (warning), 0 stale (error)
OK — every difference is registered
```

- `--json` 关键字段：`ok: true`、`counts{missing:460, extra:140, differs:158, apply-exception:9}`、
  `position_only: 111`（111 条 `differs` 只是列序）、`unregistered: []`。
- 表级一致率：本仓建出 **26 / 114 ≈ 22.8%**（上游 head 最终表集）；对象级 508 / 2146 ≈ 23.7%。
- **767 条里 758 条是同一件事**——「本仓还没接管上游迁移集」，全部承接 **LUM-1387**；
  只有 9 条 `apply-exception` 是本切片登记的长期例外。

### 4.2 `--apply-set full`（探索模式：先上游 560 个、再本仓 4 个，**只是预告 W0-B2**）→ **exit 0**

```
apply set full — 564 file(s), 1196 statements, 564 applied, 61 skipped
scratch  schema_probe_w0b_drift (dropped) — 2204 objects（上游快照 2146）

counts    extra 22 | apply-exception 9   (= 31)      ← 从 767 掉到 31
registry  31 matched, 736 stale (warning)            ← 没被 full 模式命中的基线行只报警告，不报错
```

- 61 条 skip 里 **52 条是必然的撞车**（本仓 `0001` 再次 `CREATE TABLE` 上游已建的表）：`full` 模式按设计
  关掉 strict 判定，"记录 + 继续 + 用 diff 说话"。
- 剩 22 条 `extra` 是上游 head 没有、本仓迁移留下的对象（两张自造表 `plugin` / `wakeup`（各自带从属对象）
  与三个本地索引 `issue_workspace_assignee_idx` / `issue_workspace_status_position_idx` /
  `project_workspace_idx` 等）。
- **这条实测是 W0-B2 的验收预告**：切换应用集合后 drift 从 767 → 31，且现有登记表已能覆盖它。
  LUM-1387 收尾必须重跑 `--apply-set local`（切换后它才是"运行时真正跑的集合"），把归零的行删掉。

## 5. 迁移编号空间与合并顺序（决定，不实现）

`full` 模式按**先 `upstream/` 再本仓 `migrations/`** 的顺序应用；正式接管（W0-B2）按 plan1 §6.3.1 的
C1–C5 执行，编号空间如下：

| 目录 | 内容 | 排序键 | 编号空间 |
| --- | --- | --- | --- |
| `migrations/upstream/` | 上游 560 个迁移逐字 | **文件名词干**，`LC_ALL=C` 字典序 | `001`–`534`；21 个空号（70/71/99/146-148/280/372-374/380/381/405/406/433-436/507/508）；**30 个数字版本各带 2–4 个文件**（共 77 个），最大共号组 `109_*` 4 个文件 |
| `migrations/compat/` | 本仓特有补丁（W0-B2 起） | 同上 | **`535_` 起，保持 3 位零填充**；空号不可复用（复用会插进历史中间） |
| `migrations/`（现状 `0001`–`0004`） | 本仓手写迁移 | `mc-db` 的 **BIGINT 数值序** | **W0-B2 整体退役**：`0001_init` 与上游 `001_init` 词干完全相同，`0002/3/4` 与上游 `002/003/004` 撞号 |

合并枚举必须是"两个目录、按词干排序的一个序列"（按全路径排序会把 `compat/` 排到 `upstream/` 之前）。
编号 3 位零填充的硬约束：一旦出现 `1000_*`，字典序会把它排到 `535_*` **之前**。

## 6. 偏离表怎么用

```bash
# 机器只给骨架（原因/承接留空，故意的——防止把脚本输出直接粘回表里就算数）
python3 scripts/schema_drift.py --db-url "$MULTICA_TEST_DATABASE_URL" --emit-deviations

# 人类补 `原因` + `承接 issue`，粘进 contracts/schema-deviations.tsv
```

- 五列 TSV：`对象 / 类型 / 差异摘要 / 原因 / 承接 issue`；`#` 注释行随便写。
- `对象` 支持 glob（如 `issue.*` 覆盖一张表的所有列），匹配规则 = 精确键或 `fnmatch`。
- `差异摘要` 必须以 `missing:` / `extra:` / `differs:` / `apply-exception:` 之一开头，其余文本随你写
  （本表的行把脚本给的 detail 抄了进去，便于评审）。
- `原因` 与 `承接 issue` **都不能空**：加载器直接报错（"a reason a reviewer can disagree with" /
  "a difference with no owner never gets closed"）。
- **新增偏离必须与造出它的那个 commit 一起登记**，否则 CI 红灯。
- 反向检查：登记了却没命中的行会报 **stale** —— `apply-exception:` 是 **error**（说明该跳过已经不发生，
  例如以后装了 `pg_bigm`），其余只有 **warning**（`--apply-set full` 下 736 条基线行变 stale 就是这一类）。
- 本表当前的 **767 行是"W0-B2 之前的基线"**，不是长期豁免：LUM-1387 收尾必须重跑脚本、删掉归零的行，
  只留下真正需要长期豁免的那些。红灯依然锋利——**任何新出现的对象键都不在表里，立刻非 0**。

## 7. 与本切片边界

- **不切换应用集合**：`migrations/upstream/` 里 560 个文件当前**不被任何 Rust 代码读取**——
  `mc-db/src/migrate.rs` 的 `read_dir(root)` **非递归**且按 `NNN_<name>.up.sql` 解析成 `i64`，
  `crates/mc-http/tests/{pats,contract_gaps}.rs` 用的也是同一个加载器。所以本切片对运行时零影响，
  切换是 LUM-1387 的事。
- **不接 CI**：`scripts/schema_drift.py` 需要真库，等 W0-A 的 `scripts/gates.sh`（PR #9）落地后，
  以 `--only schema-drift` 的形式挂进 `db` job：`python3 scripts/schema_drift.py --db-url "$URL" --json`。
  CI **只读** `contracts/upstream-schema.json`，不重新生成快照。
- **不改 CI、不改 `crates/**`、不动 `migrations/0001`–`0004`**。

## 8. 复算命令（本文数字的唯一来源）

```bash
export MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test

# 逐字自检：560
ls migrations/upstream/*.up.sql | wc -l
(cd migrations/upstream && sha256sum -c MANIFEST.sha256 | grep -c ': OK')

# 对账（CI 模式）：期望 exit 0
python3 scripts/schema_drift.py --db-url "$MULTICA_TEST_DATABASE_URL" --json | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["ok"], d["counts"], d["registry"]["rows"], len(d["unregistered"]))'

# W0-B2 预告：期望 exit 0，counts 只剩 extra/apply-exception
python3 scripts/schema_drift.py --apply-set full --db-url "$MULTICA_TEST_DATABASE_URL" --json | \
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["ok"], d["counts"], d["repo"]["statements"], len(d["skipped"]))'

# 骨架行（只打印未登记的差异）
python3 scripts/schema_drift.py --db-url "$MULTICA_TEST_DATABASE_URL" --emit-deviations

# 上游快照重生成（会自己建/自己 drop scratch 库 schema_probe_w0b_up）
python3 scripts/build_upstream_schema.py --help
```

## 9. 已知限制

1. **快照绑定 PG 16.15**：`format_type` / `pg_get_*def()` 的输出跨大版本会变。换版本 → 重新生成快照 +
   重跑 drift（同时 9 条 `apply-exception` 可能要重新判定）。
2. **9 条例外是环境固有的**：装了 `pg_bigm` / `pg_cron` 的镜像上，这些行会变成 `stale_errors`（红灯），
   那时必须删行并让 bigram 索引真正进入快照——这是**有意为之**，不是 bug。
3. **767 行基线有保质期**：见 §6，LUM-1387 收尾必须 prune。
4. **`full` 模式的 52 条撞车 skip 不进 `upstream-apply-exceptions.tsv`**：那张表是 local 口径快照的产物；
   `full` 只是探索模式，唯一用途是给 W0-B2 预告 diff 规模。
5. **上游演进（plan1 R6）**：上游发版后要重新 PIN + 重新生成快照 + 重跑 drift，漂移会以差值形式出现。
6. scratch 库 `schema_probe_w0b_drift` / `schema_probe_w0b_up` 默认用完 **drop**（`--keep-db` 可保留用于排查）；
   本机 5432 上有别的切片在用的库（`multica_test` 等），**不要动别人的库、不要删行**。
