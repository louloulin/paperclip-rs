# 25 — W0-B schema 地基：上游 560 迁移逐字入库 + drift 对账

> 切片：**LUM-1386 / W0-B**（`docs/plan1.md` §5 W0 ③「schema 逐字复刻 + drift 对账」、P4「schema-first，禁止临时造表」、§6.3、§6.3.1、D1）
> 分支：`feat/multica-rs-w0b-schema` → `feat/multica-rs-initial`
> 交付物：`migrations/upstream/`（560 个 `*.up.sql` + `PIN` + `MANIFEST.sha256`）、`contracts/upstream-schema.{sql,json}`、
> `contracts/upstream-apply-exceptions.tsv`、`contracts/schema-deviations.tsv`、`scripts/{schema_snapshot,build_upstream_schema,schema_drift}.py`、本文件
> 结论：**对账本机 `exit 0`**（767 处差异全部登记 / 25 行登记表 0 stale）；**上游取放字节级可校验**（`sha256sum -c` 560/560 OK）；
> **本切片不切换运行时实际应用的迁移集合**（见 §8，切换是 W0-B2 = LUM-1387）
>
> **后续（2026-09-22）**：W0-B2 = **LUM-1387 已接管**——运行时集合换成 `upstream/` 560 + `compat/` 6，`missing` 从 460 降到 **0**，
> 登记表 45 行，见 `docs/26-W0-SCHEMA-SWITCHOVER.md`。本文件 §5 的计数是**切换前**（`0001`–`0004` 单集合）的实测，保留作对照。

---

## 1. 为什么做这件事（D1 事故的机制化）

`plan1` **D1「计划以为有表、实际没有」已经发生两次**，而且两次都是靠实现者手撞发现的：

* `docs/10-M2-PLAN.md` 把 `issue_label` / `issue_to_label` 写成「已有」；本切片实测：它们是**上游 `001_init.up.sql` 建的**，
  而本仓 `0001_init.up.sql`（`parse_filename` 解析出的数字版本同为 **1**）里 `grep -i label` 为空 ⇒ label 的 9 条路由无处落；
* `issue_properties`：上游 head 的**表名是 `issue_property`**（单数，由 `191_issue_properties.up.sql` 建）。本仓只有
  `issue.properties JSONB` 列（`migrations/0001_init.up.sql:153`）—— 缺的是表，不是列。

根因不是"谁看错了"，而是 **schema 只存在于人的记忆与手写文档里**。本切片把它换成两个可复算的东西：

```
一条命令：python3 scripts/schema_drift.py --quiet      # exit 0 = 每处差异都登记过；exit 1 = 红灯
一个登记表：contracts/schema-deviations.tsv             # 每处差异必须有 原因 + 承接 issue
```

本轮之后，"以为有表"这类问题在**写计划时**就能被 `contracts/upstream-schema.json` 直接证否（它就是从上游 560 个迁移跑出来的对象清单），
在**合入前**会被 drift 门禁拦住（§9 给出接线方式）。

---

## 2. 交付物一览

| 路径 | 是什么 | 生成方式 | CI / 运行时怎么用 |
| --- | --- | --- | --- |
| `migrations/upstream/*.up.sql`（560） | 上游 `server/migrations` 的**字节级复制**（不改名/不重排/不改内容/不加注释） | `cp` + `sha256sum`（见 §3） | **只读**。当前 runner 不读它（§8）；W0-B2 起成为应用集合的第一段 |
| `migrations/upstream/PIN` | 上游 commit `f41fae6b08fb…` + 生成时间 + 逐条命令 | 手工（每次 re-vendor 重写） | 快照构建脚本读它校验来源 |
| `migrations/upstream/MANIFEST.sha256` | 560 行 `sha256sum` 清单 | `sha256sum *.up.sql` | `sha256sum -c` 校验"逐字"；构建脚本在应用前先校验 |
| `contracts/upstream-schema.sql` | 跑完 560 个迁移后 `pg_dump --schema-only` 的快照（人读契约） | `scripts/build_upstream_schema.py` | **只读快照**，CI 不重新生成 |
| `contracts/upstream-schema.json` | 同一次运行的**对象级**快照（比较用；2146 个对象，无时间戳 ⇒ 可复现） | 同上 | `schema_drift.py` 的 baseline |
| `contracts/upstream-apply-exceptions.tsv` | 构建快照时**显式跳过**的 9 条语句（缺 `pg_bigm` / `pg_cron`） | 同上（生成物，勿手改） | `schema_drift.py` 读它生成 `apply-exception:` 差异 |
| `contracts/schema-deviations.tsv` | 偏离登记表：25 行，覆盖全部 767 处差异 | 骨架由 `--emit-deviations` 出，**原因与承接 issue 手工填** | `schema_drift.py` 的判据：未登记 → exit 1 |
| `scripts/schema_snapshot.py` | 共用的 scratch 库 + 迁移应用 + 对象抽取（`information_schema` + `pg_catalog`）+ 归一化 | 单文件库 | 两侧快照出自**同一个抽取器**，所以能对象对对象比 |
| `scripts/build_upstream_schema.py` | 上游快照生成器（`--check` 可只复核不写入） | — | 一次性 + 每次 re-vendor |
| `scripts/schema_drift.py` | 对账 + 登记表校验（`--json` / `--quiet` / `--emit-deviations` / `--apply-set`） | — | 门禁本体 |

---

## 3. 上游快照生成配方（一次性行为，需评审）

### 3.1 取上游字节（**别用 blob-less clone**）

```bash
# 全 blob 浅克隆（实测：96 MB；本机两次分别 5.4 s / 21.9 s，取决于网络）
git clone --depth 1 --single-branch --branch main https://github.com/louloulin/multica /tmp/upstream-multica
git -C /tmp/upstream-multica checkout --detach f41fae6b08fb734afcbd13205c0b3203dd0bc9c6

# 逐字入库（不改名、不重排、不改内容、不加注释），然后固化成清单
cp /tmp/upstream-multica/server/migrations/*.up.sql migrations/upstream/
(cd migrations/upstream && sha256sum *.up.sql > MANIFEST.sha256)
```

**"逐字"的两重证据**（不是"我看过了"）：

```bash
# (a) 清单自校验：560/560 OK，exit 0
$ cd migrations/upstream && sha256sum -c MANIFEST.sha256 > /tmp/mf.txt; echo $?   # → 0
$ grep -c ': OK' /tmp/mf.txt                                                     # → 560

# (b) 独立复核：按 PIN 抓回上游那一棵提交，与入库文件逐个 cmp（不依赖 MANIFEST）
$ git -C /tmp/verify-up fetch --depth 1 origin f41fae6b08fb734afcbd13205c0b3203dd0bc9c6
$ git -C /tmp/verify-up checkout FETCH_HEAD                                     # HEAD = f41fae6b08fb…
$ for f in migrations/upstream/*.up.sql; do \
      cmp -s "$f" "/tmp/verify-up/server/migrations/$(basename "$f")" || echo "DIFF $f"; done
compared=560 differing=0
```

> 复核当日上游 `main` 已经走到 `90e0bdf8…`，与 `PIN` 里的 `f41fae6b…` 不是同一个提交 ——
> 这正是 `PIN` 存在的理由：**字节来自哪个提交是记录，不是猜测**。上游那棵树里同时有 **560 个 `.down.sql`**，本切片不入库（§12）。

> ⚠️ **禁止** `git init` + `fetch --filter=blob:none` 之后在仓库里 `git grep`：缺失 blob 会触发逐对象懒取
> （约 1.4s/对象，单次 grep ETA ≈ 74 分钟），**已经卡死过两个切片**。完整排障与计时见
> `docs/20-UPSTREAM-ANALYSIS-RECIPE.md` §4。

**范围说明：上游 560 个 `.down.sql` 不在本切片范围**（不入库、不校验）。它们只在 W0-B2 做回滚路径时才有用，
届时按同一个 PIN 取即可（`PIN` 里的 `clone_command` 可原样复用）。

### 3.2 建快照

```bash
export MULTICA_TEST_DATABASE_URL=postgres://user:pw@127.0.0.1:5432/<任一已存在的库>
python3 scripts/build_upstream_schema.py            # 建 + 写 contracts/upstream-schema.{sql,json,exceptions}
python3 scripts/build_upstream_schema.py --check     # 只复核：重建到 scratch 库并与已入库文件逐字节比，不写
```

* `--db-url` 指向**已存在**的库，脚本在它旁边建 scratch 库 `schema_probe_w0b_up`（`--db-name` 可改），跑完自动 drop（`--keep-db` 保留）；
* 应用前先校验 `MANIFEST.sha256`（560/560），所以**快照不可能建立在半改过的迁移集上**；
* 跳过语句**绝不静默**：脚本先探测 `pg_available_extensions`，命中缺失扩展标记的语句在**执行前**跳过，逐条写进
  `contracts/upstream-apply-exceptions.tsv`；没预料到的跳过**直接失败**（除非 `--lenient`）；
* 退出码：`0` 快照写好或 `--check` 逐字节一致；`1` 有未预料跳过（快照不可信）；`2` 用法/环境错（URL、psql/pg_dump、清单不符、迁移失败）。

**本次实跑（2026-09-22，本机 PostgreSQL 16.15）**：

```
$ python3 scripts/build_upstream_schema.py --check
replaying 560 migrations into schema_probe_w0b_up (…)
check: 2146 objects (column 1291, constraint 420, function 26, index 276, table 116, trigger 17), 9 skip(s), 0 guard notice(s)
exit 0        # 与入库文件逐字节一致 ⇒ 快照可复现（耗时 1m24s）
```

`contracts/upstream-schema.sql` 文件头带 **上游 commit `f41fae6b08fb…`**、PG 版本、560 迁移 / 1142 语句 / 9 条记录跳过 /
对象计数，可直接人读核对。

---

## 4. drift 口径与归一化规则

### 4.1 两侧来源（同一抽取器，所以不是文本 diff）

```
side A（本仓）  migrations/*.up.sql  → scratch 库 → information_schema + pg_catalog → 对象快照
side B（上游）  contracts/upstream-schema.json （入库的 baseline；CI 永不重新生成）
```

`contracts/upstream-schema.sql` 是**给人读的规范**；**比较用的是 JSON**，这样比的是"对象对对象"——
表 / 列（带 `attnum`，所以**换列序也算差异**）/ 类型 / 可空性 / 默认值 / PK·UNIQUE·FK·CHECK（带 `convalidated`）/
独立索引（带 `valid`，半个 `CREATE INDEX CONCURRENTLY` 不会被当成成品）/ 函数 / 触发器。

### 4.2 四种差异

| 类别 | 含义 |
| --- | --- |
| `missing` | 上游有、本仓迁移不建 |
| `extra` | 本仓迁移建了、上游没有 |
| `differs` | 两边都有、定义不同 |
| `apply-exception` | 上游**本来会有**，但在本机快照里根本建不出来：来自需要 `pg_bigm` / `pg_cron` 的语句。这些行**完全来自** `contracts/upstream-apply-exceptions.tsv`（生成物），两边**硬校验锁死**：跳过语句没登记 → 红灯；登记了却没有对应跳过 → 也是红灯 |

### 4.3 折叠与 `position_only`

* **折叠**：`missing`/`extra` **表**自己的列/约束/索引/触发器不再逐条出报告，而是折进那张表的行（`+930 column` 这种标注）。
  理由：某张上游表缺了，它那几十列逐条列出来会把真正的信号埋掉（否则报告要多 1390 行）。
* **`position_only`**：`differs` 里只有 `attnum` 不同（同列、同类型、同默认值，仅位置不同）的列单独计数
  （本轮 111/138）。**仍然是差异**（上游后续 `ALTER` 加列造成的列序不同，快照记录位置是有意的），
  但报告把它拆出来，免得 111 条列序噪声盖住 27 条真实差异。

### 4.4 归一化规则（写死在 `scripts/schema_snapshot.py::NORMALIZATION`，两侧共用）

| 口径项 | 规则 |
| --- | --- |
| PG 版本 | **PostgreSQL 16**（`format_type()` / `pg_get_*def()` 的输出随版本变） |
| schema | 只比 `public`；对象键不带限定名，定义里的 `public.` 前缀剥掉 |
| 类型别名 | `format_type(atttypid, atttypmod)` ⇒ `varchar(255)` ≡ `character varying(255)`，**不做额外别名合并** |
| `DEFAULT` | `pg_get_expr(adbin, adrelid)`，空白折叠（所以 `''` 与 `''::text` **算不同**，虽然语义等价） |
| 序列 | serial/identity 通过 `nextval()` 默认值与序列对象体现 |
| 扩展成员 | `pg_depend.deptype='e'` 的对象排除（`pg_dump` 也这么做），改记扩展集合本身 |
| 约束底层索引 | PK/UNIQUE/EXCLUDE 的支撑索引不单列（约束行已覆盖） |
| 函数 | `pg_get_functiondef()`，`public.` 剥掉，行尾归一，**函数体逐字保留** |
| 空白 | 定义内连续空白折成一个空格 |
| 记账表 | 两侧都由 psql 直接应用 `*.up.sql`，**都不含** runner 自建的 `schema_migrations` ⇒ 对称、无需特判 |

### 4.5 对账环境

`scripts/schema_drift.py` 自己建 scratch 库（默认 `schema_probe_w0b_drift`，`--db-name` 可改），跑完自动 drop。

```bash
python3 scripts/schema_drift.py --db-url …            # 人读报告，exit 0/1/2
python3 scripts/schema_drift.py --json                 # 机器可读（以后进 CI 用这个）
python3 scripts/schema_drift.py --quiet                # 只留退出码（CI 用）
python3 scripts/schema_drift.py --emit-deviations      # 未登记差异的骨架行（原因/承接 issue 刻意留空）
python3 scripts/schema_drift.py --apply-set full       # 探索：先上游、再本仓（W0-B2 用，非门禁，宽松对待必然的撞表）
```

库 URL 由 `--db-url` 或 `MULTICA_TEST_DATABASE_URL` 提供，**绝不写死密码**，密码经 `PGPASSWORD` 传给 `psql` 子进程
（不进 argv，`ps` 看不到）。**退出码语义**：`0` = 每处差异都已登记；`1` = drift 未登记完 / 有未登记的跳过 / `apply-exception` 行 stale /
迁移意外失败；`2` = 用法或环境错（没给 URL、连不上、快照或登记表格式不对、文件缺失）——**`2` 不是"通过"**，是"这道门根本没跑"。

---

## 5. 首次运行的真实计数（2026-09-22 本机实测）

> ⚠️ **W0-B2（LUM-1387）之后这一节的数字已过期**，它量的是切换前的单集合（`apply set local — 4 file(s)`）。
> 现在的口径是 `apply set local — 566 file(s)`、`missing=0`、`extra=22`、`differs=14`、`apply-exception=9`（登记表 45 行），
> 见 `docs/26-W0-SCHEMA-SWITCHOVER.md` §1/§6.4。本节保留下来，是因为它是「本仓自造 schema 与上游差多远」的基线。

```
$ python3 scripts/schema_drift.py --db-url … --db-name w0b_drift_probe
baseline  contracts/upstream-schema.json — upstream f41fae6b08fb, 560 migrations, 2146 objects
apply set local — 4 file(s), 54 statements, 4 applied, 0 skipped
scratch   w0b_drift_probe (dropped) on PostgreSQL 16.15 (Ubuntu 16.15-0ubuntu0.24.04.1) — 508 objects
differences (767): …
by category: apply-exception=9, differs=158, extra=140, missing=460
            (111 of the differs rows are column order only …)
            (930 column, 288 constraint, 166 index, 6 trigger folded into the missing/extra table rows)
registry  25 row(s): 25 matched, 0 stale (warning), 0 stale (error)
OK — every difference is registered                       # exit 0
```

### 5.1 三组计数（**表 / 列** 口径）

| 组 | 表 | 列 | 约束 | 索引 | 函数 | 触发器 | 小计 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `missing`（上游有本仓无） | **90** | **167** | 56 | 110 | 26 | 11 | 460 |
| `extra`（本仓有上游无） | **2** | **86** | 31 | 21 | 0 | 0 | 140 |
| `differs`（同名定义不同） | 0 | **138**（111 列序 + 27 真实） | 20 | 0 | 0 | 0 | 158 |
| `apply-exception` | — | — | — | 6 | — | — | 9（+2 扩展 +1 语句） |
| **合计** | | | | | | | **767** |

* `missing` 的 **167 列**是"**两侧都有的表**上缺的列"（22 张表）；**90 张上游表自带的 902 列**被折进表行了（见 §4.3）。
* 本仓侧快照 **508 个对象**（28 张表 + 手写的 21 个独立索引 + 各表的列与约束；无函数、无触发器），
  上游侧 **2146 个对象**（table 116 / column 1291 / constraint 420 / index 276 / function 26 / trigger 17）。
* **覆盖率 = 26/116 ≈ 22%**（26 张同名表；本仓 28 张 = 26 共有 + `plugin` + `wakeup`）。

### 5.2 逐表分布（`missing`/`extra` 的列与约束）

`missing column`（167 / 22 张表）：
`agent_task_queue 46、agent 13、runtime_profile 11、chat_message 11、autopilot 10、chat_session 8、agent_runtime 7、channel_installation 7、comment 7、inbox_item 7、issue 5、project 5、project_resource 5、issue_status 4、workspace 4、skill 3、squad 3、user 3、workspace_invitation 3、issue_subscriber 2、verification_code 2、personal_access_token 1`

`missing constraint`（56 / 18 张表）：`issue 10、agent 8、agent_task_queue 6、issue_status 6、autopilot 4、runtime_profile 3、channel_installation 2、chat_session 2、comment 2、inbox_item 2、project 2、project_resource 2、skill 2、agent_runtime 1、chat_message 1、issue_subscriber 1、squad 1、workspace_invitation 1`

`extra column`（86 / 21 张表）：`autopilot 9、agent_task_queue 7、workspace_invitation 7、agent_runtime 6、chat_message 6、skill 6、issue 5、channel_installation 4、chat_session 4、inbox_item 4、project 4、runtime_profile 4、user 4、verification_code 4、personal_access_token 3、comment 2、squad 2、workspace 2、agent 1、member 1、project_resource 1`

`extra constraint`（31 / 17 张表）、`extra index`（21 条，逐条）与 `missing index`（110 条）清单用一条命令复算（§11）；
**本仓自造命名与上游只差命名的索引**例如 `issue_project_idx`（上游 `idx_issue_project`）、`idx_share_link_code`（上游 `workspace_share_link_code_uidx`）。

### 5.3 27 处**真实**列差异（其余 111 处只是列序）

（下表逐条来自 `--json` 的 `detail`，未做归并；一条列可以同时有类型/默认值/可空性/列序多种差异）

| # | 表.列 | 差异（本仓 → 上游） |
| ---: | --- | --- |
| 1 | `comment.author_id` | 类型 `text` → `uuid`（+ 列序） |
| 2 | `issue.creator_id` | 类型 `text` → `uuid`（+ 列序） |
| 3 | `issue.assignee_id` | 类型 `text` → `uuid`（+ 列序） |
| 4 | `inbox_item.actor_id` | 类型 `text` → `uuid`；可空性 `NOT NULL` → 可空（+ 列序） |
| 5 | `issue_reaction.actor_id` | 类型 `text` → `uuid` |
| 6 | `comment_reaction.actor_id` | 类型 `text` → `uuid` |
| 7 | `chat_message.elapsed_ms` | 类型 `integer` → `bigint`（+ 列序） |
| 8 | `user.starter_content_state` | 类型 `jsonb` → `text` |
| 9 | `user.language` | 类型 `text` → `character varying(20)`；`DEFAULT ''` → `NULL::character varying`（+ 列序） |
| 10 | `agent.max_concurrent_tasks` | `DEFAULT 1` → `6`（+ 列序） |
| 11 | `agent.visibility` | `DEFAULT 'workspace'` → `'private'`（+ 列序） |
| 12 | `agent_runtime.visibility` | `DEFAULT 'workspace'` → `'private'`（+ 列序） |
| 13 | `issue.number` | `DEFAULT ''` → `0`（+ 列序） |
| 14 | `workspace_invitation.expires_at` | `DEFAULT ''` → `(now() + '7 days'::interval)`（+ 列序） |
| 15 | `agent.description` | `DEFAULT ''` → `''::text`；可空 → `NOT NULL`（+ 列序） |
| 16 | `chat_session.title` | `DEFAULT ''` → `''::text`；可空 → `NOT NULL`（+ 列序） |
| 17 | `issue_status.icon` | `DEFAULT ''` → `''::text`；可空 → `NOT NULL`（+ 列序） |
| 18 | `skill.description` | `DEFAULT ''` → `''::text`；可空 → `NOT NULL`（+ 列序） |
| 19 | `squad.description` | `DEFAULT ''` → `''::text`；可空 → `NOT NULL`（+ 列序） |
| 20 | `squad.instructions` | `DEFAULT ''` → `''::text`；可空 → `NOT NULL`（+ 列序） |
| 21 | `user.profile_description` | `DEFAULT ''` → `''::text`；可空 → `NOT NULL`（+ 列序） |
| 22 | `personal_access_token.token_prefix` | `DEFAULT ''::text` → `''`（方向相反，同为渲染差异）（+ 列序） |
| 23 | `chat_session.agent_id` | 可空 → `NOT NULL`（+ 列序） |
| 24 | `verification_code.email` | 可空 → `NOT NULL`（+ 列序） |
| 25 | `agent_task_queue.issue_id` | `NOT NULL` → 可空（+ 列序） |
| 26 | `inbox_item.actor_type` | `NOT NULL` → 可空（+ 列序） |
| 27 | `personal_access_token.expires_at` | `NOT NULL` → 可空 |

汇总（**互有重叠**）：类型 9 处（6 个 `text`→`uuid` + `integer`→`bigint` + `jsonb`→`text` + `text`→`varchar(20)`）、
默认值 14 处（其中 **6 处语义不同**：`1`→`6`、`'workspace'`→`'private'` ×2、`''`→`0`、`''`→`now()+7 days`、`''`→`NULL::character varying`；
**8 处只是 `''` vs `''::text` 渲染**）、可空性 13 处（9 处本仓可空 → 上游 `NOT NULL`，4 处反向）。

> `''` 与 `''::text` 语义等价，但 §4.4 的口径按 `pg_get_expr` 文本比，所以它们出现在 `differs` 里。
> **真正会在切换时炸的是那 6 个 `text`→`uuid`**：`crates/**` 里所有读写这些列的 SQL/`FromRow` 都要跟着改 —— 这正是
> W0-B2（LUM-1387）的范围，本切片刻意不碰（见 §8）。

### 5.4 表集口径更正：上游 head 是 **116**，不是 114

原 issue 与 `plan1` §6.3.1 写"CREATE 去重 137 − 24 个 DROP ⇒ head 114"。本切片用**两种独立方法**复算，都是 **116**：

```bash
# 方法一：按文件名序模拟 CREATE/DROP（去 SQL 注释 + 去 $…$ 块 + 处理 "user" 引号）
#   → CREATE 去重 137；DROP 语句 24 条；其中 lark_inbound_message_dedup 与 plugin_installation
#     是「先 drop、后 re-create」⇒ 净退役 21 ⇒ head = 137 − 21 = 116
# 方法二：560 个迁移跑完后的 pg_dump / 对象快照 ⇒ table 116（§3.2）
#   两种方法的表集差集为空（互证）：live − snapshot = ∅，snapshot − live = ∅
```

差 2 的原因：**24 条 `DROP TABLE` 里有 2 张后来又被重新建出**，
所以"24 个 DROP"≠"净退役 24 张"。drift 门禁与"schema 一致率"一律以 **head 最终集 116** 为分母（覆盖率 26/116 ≈ 22%）。
另注：`schema_migrations` 是 Go runner 自建的，**不在** 116 里（快照由 psql 应用 `*.up.sql` 得到，两侧对称，见 §4.4）。

### 5.5 9 条 `apply-exception`（本机环境限制，不是上游缺陷）

| 对象 | 来源 | 原因 |
| --- | --- | --- |
| `extension:pg_bigm` | `032_issue_search_index.up.sql:3` | 本机 PG 16 无 `pg_bigm` |
| `index:idx_issue_title_bigm` / `idx_issue_description_bigm` | `032_issue_search_index.up.sql:13` | 同上（bigram GIN 索引） |
| `index:idx_comment_content_bigm` | `033_comment_search_index.up.sql:3` | 同上 |
| `index:idx_project_title_bigm` / `idx_project_description_bigm` | `039_project_search_index.up.sql:2` | 同上 |
| `index:idx_issue_properties_bigm` | `446_issue_properties_bigm_index.up.sql:19` | 同上（且是 `CREATE INDEX CONCURRENTLY`，事务里跑不了） |
| `extension:pg_cron` | `076_task_usage_pgcron_extension.up.sql:24` | 本机 PG 16 无 `pg_cron` |
| `statement:103_drop_legacy_daily_rollups.up.sql:101` | 同上文件第 101 行 | 只在 `pg_cron` 存在时调 `cron.unschedule(...)`；不建对象，只清理旧定时任务 |

装上这两个扩展后重跑 §3.2 即可让它们消失（届时登记表里对应的行要**同一个 commit** 一并删掉，见 §6）。

---

## 6. 偏离登记表用法（`contracts/schema-deviations.tsv`）

5 列 TSV（`对象 / 类型 / 差异摘要 / 原因 / 承接 issue`），`#` 注释可穿插：

* **对象**：精确对象键，或 glob（实践里就是 `<表>.*`，用来一次覆盖一张表的列/约束差异）；
* **类型**：`table | column | constraint | index | function | trigger | type | view | extension | statement`；
* **差异摘要**：必须以 `missing:` / `extra:` / `differs:` / `apply-exception:` 开头（工具只校验前缀，冒号后的文字原样记录）；
* **原因**：**为什么这处差异可接受**（不是"它长什么样"）；
* **承接 issue**：谁负责关掉它。空 → 直接报错（"没有承接人的差异永远不会被关掉"）。

规矩：

1. **新增偏离必须与产生它的改动同一 commit 登记**。也就是：动了 `crates/**` 的 SQL / 加了 compat 补丁 / 重新 vendor 上游，
   都要在同一次提交里更新本表；否则 `schema_drift.py` 立刻 exit 1，红灯属于这次改动，不属于下一个人。
2. `--emit-deviations` 给出的骨架**刻意把原因与承接 issue 留空**，所以把它的输出粘贴回去**永远不可能**通过校验。
3. **stale 行**：匹配不到任何差异的行 → 普通类别是**警告**（表里留了过期条目），
   `apply-exception:` 类别是**错误**（它必须与 `contracts/upstream-apply-exceptions.tsv` 严格一一对应）。
   本轮 25 行**全部命中、0 stale**。
4. **不许为了让脚本变绿**而放宽 `原因` 的语义，也不许把某张表的差异塞进"批量理由"里假装登记过 ——
   批量行只用于**同一根因、同一承接 issue** 的成组差异。

### 6.1 本轮登记的 25 行（`--json` 的 `registry.coverage` 实测）

| 行 | 对象 | 类型 | 类别 | 覆盖 |
| --- | --- | --- | --- | ---: |
| §1 | `issue_label` | table | missing | 1 |
| §1 | `issue_to_label` | table | missing | 1 |
| §1 | `issue_property`（**注意是单数**） | table | missing | 1 |
| §2 | `plugin` | table | extra | 1 |
| §2 | `wakeup` | table | extra | 1 |
| §3 | `*` | table | missing | 90 |
| §3 | `*` | column | missing | 167 |
| §3 | `*` | constraint | missing | 56 |
| §3 | `*` | index | missing | 110 |
| §3 | `*` | function | missing | 26 |
| §3 | `*` | trigger | missing | 11 |
| §3 | `*` | column | extra | 86 |
| §3 | `*` | constraint | extra | 31 |
| §3 | `*` | index | extra | 21 |
| §3 | `*` | column | differs | 138 |
| §3 | `*` | constraint | differs | 20 |
| §4 | `pg_bigm` / `pg_cron`（2 行） | extension | apply-exception | 1 + 1 |
| §4 | 6 个 `*_bigm` 索引（6 行） | index | apply-exception | 各 1 |
| §4 | `103_drop_legacy_daily_rollups.up.sql:101` | statement | apply-exception | 1 |

**承接 issue**：除 `apply-exception` 的 9 行归本切片（LUM-1386，环境限制、长期登记）外，
其余 16 行全部归 **LUM-1387（W0-B2：切换应用集合 = 上游逐字集 + compat 补丁）** —— 切换后这些差异按定义消失。
`issue_label` / `issue_to_label` / `issue_property` 与 `plugin` / `wakeup` 这 5 行**单独成行**，
刻意不被 §3 的批量行吞掉（它们是本 issue 点名的 D1 源头与本仓自造表）。

### 6.2 反例实测（登记表的"拒绝"行为，本机实跑）

光说规则不算数，下面是三个**故意改坏**的登记表副本（放在仓库外，用完即弃）的真实输出 —— 它们定义了“什么不能通过”：

```bash
# 反例 A：把 issue_label 行的「原因」清空
$ python3 scripts/schema_drift.py --deviations neg-a.tsv --db-url …
error: neg-a.tsv:29: 原因 is empty — a registered difference needs a reason a reviewer can disagree with
exit 2

# 反例 B：凭空追一条没有对应跳过语句的 apply-exception 行（ghost_idx）
$ python3 scripts/schema_drift.py --deviations neg-b.tsv --db-url …
registry  26 row(s): 25 matched, 0 stale (warning), 1 stale (error)
  ERROR stale apply-exception row at line 66: ghost_idx (index)
exit 1

# 反例 C：把 plugin 行的「承接 issue」清空（同时故意给一个不存在的库 URL）
$ python3 scripts/schema_drift.py --deviations neg-c.tsv --db-url postgres://…/definitely_not_there
error: neg-c.tsv:36: 承接 issue is empty — a registered difference with no owner never gets closed
exit 2
```

两个细节值得记住：**原因/承接 issue 为空是"格式错"（exit 2）**，在碰数据库**之前**就被拒（反例 C 的假 URL 根本没被访问）；
而"凭空登记"是**内容错**（exit 1，stale error）—— 也就是说，把 `--emit-deviations` 的空白骨架直接粘回去**走不到比对那一步**。

---

## 7. 迁移编号空间与合并顺序（决定）

背景硬约束见 `plan1` §6.3.1（C1–C4）：上游 runner 与本仓 runner 在**身份（TEXT 词干 vs `i64`）**、
**顺序（全路径字典序 vs 数值序）**、**记账表形态**三处不兼容；上游 560 个迁移只有 **513 个互不相同的数字版本**
（30 个数字各带 2–4 个文件，共 77 个文件；最多的是 `032_` / `060_` / `109_` / `120_` 各 4 个）。
⇒ 合并序列**必须按文件名词干排序**，而 `compat/` 的目录名会把它排到 `upstream/` 之前 —— 所以**顺序由编号决定，不由目录名决定**。

| 编号段 | 目录 | 来源 | 规则 |
| --- | --- | --- | --- |
| `001` – `534` | `migrations/upstream/` | 上游逐字 | **513 个互异数字 / 560 个文件**；同数字内按词干字典序；不动一个字节 |
| `535_` 起（保持 **3 位零填充**） | `migrations/compat/` | 本仓 | 本地迁移退役后**仍有价值**的补丁在这里重述；必须晚于上游最大编号 `534` |
| `0001` – `0004` | **退役** | 本仓 | `0001_init` 与上游 `001_init` 数字版本同为 `1`；`0002/0003/0004` 又撞上游 `002/003/004` ⇒ 整体退役（C4） |
| 空号（**21 个，不可复用**） | — | — | `70, 71, 99, 146, 147, 148, 280, 372, 373, 374, 380, 381, 405, 406, 433, 434, 435, 436, 507, 508, **517**` |

> 更正：`plan1` §6.3.1 C2 的空号清单列了 20 个，**漏了 `517`**（实测 1..534 内空号共 21 个）。复用任何空号都会让补丁插到历史中间。
> 为什么必须 3 位零填充：一旦出现 `1000_*`，字典序会把它排到 `535_*` **之前**。

**本切片的决定**：`migrations/upstream/` **只入库、不参与当前应用集合**（§8）；`migrations/compat/` 目录**本切片不建**
（它属于 W0-B2 的实现，本切片不预先造空目录）。

---

## 8. 本切片**不切换**运行时实际应用的迁移集合（明确声明）

本切片交付的是**机制与快照**，不是接管。三条可验证的证据：

1. `migrations/upstream/` 是 `migrations/` 的**子目录**，而本仓 runner 枚举迁移用的是
   `crates/mc-db/src/migrate.rs::load_dir` → `std::fs::read_dir(root)`（**非递归**）+ `if !path.is_file() { continue; }` ⇒
   子目录被跳过，560 个上游文件对当前 runner **不可见**；
2. 门禁 ⑥（`bash scripts/gates.sh --only db`）跑的仍是 `mc-migrate run --dir migrations` ⇒ 应用的还是 `0001`–`0004`
   （drift 报告里的 `apply set local — 4 file(s), 54 statements` 就是这件事的实测）；
3. `crates/**` 与 `migrations/0001`–`0004` 在本切片**一个字节都没改**（`git show --stat` 只有新增文件 + `contracts/schema-deviations.tsv`）。

接管的实现（TEXT 键、词干排序、两目录合并、存量库再基线、`DEFAULT_REQUIRED_TABLES` 改 `issue_wakeup*`）
全部留给 **W0-B2 = LUM-1387**；本切片为它准备好探索模式与 767 处差异的初始登记。

**2026-09-22 已接管（LUM-1387 / W0-B2，`docs/26-W0-SCHEMA-SWITCHOVER.md`）**：`load_dir` 换成递归的 `load_dirs`（TEXT 键 = 文件 stem、
词干排序、同 stem 报错），`mc-migrate` 的 `--dir` 可重复，`DEFAULT_REQUIRED_TABLES` 里的 `wakeup` 改成 `issue_wakeup`/`issue_wakeup_receipt`，
旧 `BIGINT` 账本库**报错要求重建**（不做原地 rebaseline）；`--apply-set full` 换成 `--apply-set upstream`（只跑 vendored 集合，
用来观察 compat 补丁关掉了哪些缺口，`local` 现在就是 560+6 的真实集合）。上面 §8 的 1/2/3 三条因此全部不再成立。

---

## 9. 以后怎么接 CI（本切片**不改** CI，只写清接法）

> **已由 W0-D / LUM-1402 执行（2026-09-22）**：门 ⑧ `schema-drift` 已进 `scripts/gates.sh` 的 `ALL_GATES`
> 与 `--with-db` 集合，并挂在 `ci.yml` 的 `db` job。本节以下内容保留为**当时的设计稿**，
> 落地后的实现细节、实测输出与限制见 `docs/30-W0-DRIFT-GATE.md`。两处与下述草稿的差异：
> ① 门里用 `--quiet`（判据是退出码），**红了才补跑一遍不带 `--quiet`** 的打印未登记差异（绿时那份报告 767 行）；
> ② `db` job 在 ⑧ 之前多一个 `⑧ deps — psql client + python3` 步（⑥ 只用 sqlx，不会把 `psql` 装出来）。

门禁本体已经能 `--quiet` 判退出码，接线是两句话的事，但**本切片不做**（`plan1` §5 W0 ④ 路由对账入 CI 之后）：

```bash
# 1) scripts/gates.sh：加一道门（名字建议 schema-drift），并进 ALL_GATES
#    python3 scripts/schema_drift.py --quiet     # 需要 MULTICA_TEST_DATABASE_URL
# 2) .github/workflows/ci.yml：挂在 db job（它有 services.postgres），加一个 step
#    - name: "⑧ schema drift — python3 scripts/schema_drift.py"
#      run: bash scripts/gates.sh --only schema-drift
```

两个设计约束（别踩）：

* **不能挂 `contract` job**：那道门是纯离线（只读 `docs/fixtures/`），而 drift 需要真 PostgreSQL；
* **库要给"空库"**：`schema_drift.py` 自己建 scratch 库、自己不依赖目标库的表，
  但若指向的库 URL 所在 server 没有 `CREATEDB` 权限就直接 exit 2（**不会静默跳过**）。

同时注意与门禁 ⑥ 的**语义分工**：⑥ 验"本仓迁移能跑 + e2e 能过"，⑧ 验"跑出来的 schema 还是上游的 schema"。
⑥ 从来不回答"计划里写的表到底有没有"，那正是 ⑧ 存在的理由。

---

## 10. 已知限制 / 未决

1. **`pg_bigm` / `pg_cron` 缺失**⇒ 9 条 `apply-exception` 长期存在；这是环境事实，不是可修复的缺陷。
2. **两侧都在本机 PG 16.15 上取**：上游生产库的版本/扩展集合若不同（例如装了 `pg_bigm`），
   快照的扩展相关部分会不同；`contracts/upstream-schema.sql` 头部已写死 PG 版本与"缺失扩展"。
3. **上游会继续演进**：`contracts/*` 是**快照**，不是跟踪。re-vendor 流程 = 更新 `PIN` + `MANIFEST.sha256` →
   `build_upstream_schema.py` → `--check` 复核 → 按新差异更新 `contracts/schema-deviations.tsv`（同一 commit）。
4. **`--apply-set full` 不是门禁**：它容忍"本仓 0001 与上游 001 撞表"这类必然碰撞，只用于 W0-B2 探索；
   `compat/` 落地后需要在 `schema_drift.py` 里把合并顺序从"目录名"改成"词干"（同 §7 的表）。
5. **本仓快照仍是"迁移能建出的对象"**：不含 runner 记账表、不含运行时 DDL（例如 M2 切片手工加的列）。
   后者如果发生，会以 `extra` 出现在报告里 —— 这是**故意的**：临时造表必须在同一 commit 登记。

---

## 11. 复算命令（一条一条抄，不要凭记忆）

```bash
# 上游字节"逐字"校验（560/560）
cd migrations/upstream && sha256sum -c MANIFEST.sha256 && ls *.up.sql | wc -l     # → 560
cd - && head -20 contracts/upstream-schema.sql                                    # → 上游 commit f41fae6b08fb…

# 快照可复现（重建到 scratch 库，逐字节比，不写文件）
python3 scripts/build_upstream_schema.py --check --db-url "$MULTICA_TEST_DATABASE_URL"

# drift：门禁判据（exit 0 = 全部登记）+ 机器可读计数
python3 scripts/schema_drift.py --quiet  --db-url "$MULTICA_TEST_DATABASE_URL"   # → exit 0
python3 scripts/schema_drift.py --json   --db-url "$MULTICA_TEST_DATABASE_URL" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d["counts"], len(d["unregistered"]))'

# 表集口径：CREATE 137 / DROP 顺序模拟 → head 116（见 §5.4）
python3 - <<'PY'
import re, glob, json
strip = lambda t: re.sub(r'\$[A-Za-z_]*\$.*?\$[A-Za-z_]*\$', ' ', re.sub(r'--[^\n]*', '', t), flags=re.S)
live, created = set(), set()
for f in sorted(glob.glob('migrations/upstream/*.up.sql')):
    for m in re.finditer(r'\b(CREATE|DROP)\s+TABLE\s+(IF\s+(NOT\s+)?EXISTS\s+)?([^;]*)', strip(open(f).read()), re.I):
        for q, u in re.findall(r'"([^"]+)"|\b([a-zA-Z_][a-zA-Z0-9_]*)\b', m.group(4).split('(')[0]):
            n = q or u
            if n.lower() in ('public', 'only'): continue
            (live.add(n), created.add(n)) if m.group(1).upper() == 'CREATE' else live.discard(n)
snap = {o['key'] for o in json.load(open('contracts/upstream-schema.json'))['objects'] if o['kind'] == 'table'}
print(len(created), len(live), len(snap), live == snap)   # → 137 116 116 True
PY

# 迁移编号空间：513 个互异数字 / 30 个数字带多文件 / 21 个空号
ls migrations/upstream/*.up.sql | sed 's#.*/##; s#_#\t#' | cut -f1 | sort -u | wc -l   # → 513
```

---

## 12. 本切片明确**未做**（避免误以为地基已完备）

* **不切换应用集合**：`crates/mc-db`、`crates/mc-migrate`、`mc-repos` 里的 SQL 一行没动 → W0-B2（LUM-1387）；
* **不改 CI**：只写了接法（§9），`gates.sh` 与 `ci.yml` 未加门禁
  → **已在 W0-D / LUM-1402 补齐**（门 ⑧，见 §9 顶部标注与 `docs/30`）；本行原文保留为 W0-B 交付时的状态。
* **不入库 560 个 `.down.sql`**（回滚路径属 W0-B2）；
* **不建 `migrations/compat/`**、不写任何 compat 补丁；
* 不实现路由、不建 `mc-source-context`、不碰 `docs/fixtures/`（route parity 属 T1 / W0-A）。
