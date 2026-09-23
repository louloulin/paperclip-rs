# 26 — W0-B2 schema 切换：运行时应用集合换成「560 上游 + 6 compat 补丁」

> 切片：**LUM-1387 / W0-B2**（`docs/plan1.md` §5 W0 ③「schema 逐字复刻 + drift 对账」的后半、§6.3.1 接管契约 C1–C5、P4「schema-first，禁止临时造表」）
> 分支：`agent/devbox5/e3dad1412d60` → `feat/multica-rs-initial`
> 交付物：`crates/mc-db/src/migrate.rs`（runner 重写）、`crates/mc-migrate`（CLI）、`migrations/compat/`（6 个补丁）、
> `crates/mc-repos/src/*`（SQL 对齐）、`scripts/schema_drift.py`（应用集合可切换）、`contracts/schema-deviations.tsv`（45 行）、本文件
> 结论：**560 + 6 = 566 个迁移从零应用成功**（117 张表，账本 `version` 为 `text`）；
> **drift `missing = 0`、`exit 0`**（`apply-exception=9, differs=14, extra=22`；登记表 45 行、0 stale）；
> **门禁 `bash scripts/gates.sh --with-db` 10/10 绿**
> 上一片（LUM-1386 / `docs/25-W0-SCHEMA-DRIFT.md`）把上游 560 个迁移**逐字入库**但没有接线；本片接线。

---

## 1. 切换前后

| | 切换前（W0-B 之后、W0-B2 之前） | 切换后（本片） |
| --- | --- | --- |
| 运行时应用的迁移 | `migrations/0001…0004`（4 个文件，28 张表） | `migrations/**/*.up.sql` = `upstream/` 560 + `compat/` 6 = **566**（117 张表） |
| `migrations/upstream/` | 在库里，但 runner 的 `glob("*.up.sql")` **不递归** ⇒ 从未被应用 | 与 `compat/` 合成**一个**编号空间，按文件名 stem 排序应用 |
| 账本键 | `BIGINT`（`parse_filename` 解析出的数字版本） | **`text`** = 文件 stem（与上游 runner 一致，见 §2.2） |
| drift 的 `missing` | 460 | **0** |
| 本仓 `0001…0004` | 运行时集合 | **退役**（内容按语义拆进 `compat/535–537`，见 §4） |

`mc-repos` / `mc-http` 的 SQL 全部改到上游列名与类型上（§5、§6），**HTTP 契约零变化**：所有 Rust 结构体字段名与类型都不动，
`text`↔`uuid` 的差异在 SQL 里用 `::text` 投影 / `$n::uuid` 转换抹平。

---

## 2. 应用集合与 runner 语义

### 2.1 集合 = `migrations/**.up.sql`，按 stem 排序、同 stem 即错

`mc_db::Migrator::load_dirs(&[dir…])` 递归收集 `*.up.sql`，以 **文件 stem** 为版本键，按 stem 字典序排序；
两个文件同 stem 直接报错，不做「最后一个赢」：

```
duplicate migration version 001_init: .../upstream/001_init.up.sql, .../compat/001_init.up.sql — migration keys must be unique
```

这正是上游 runner 的口径。`--dir` 可重复 ⇒ 也能把集合拆成多个目录（现在不需要：`migrations/` 递归就够）。

### 2.2 runner 行为（对照上游 `server/cmd/migrate/main.go`）

* **逐条执行**：一个文件按语句切开（`split_sql_statements`，正确处理 `$$` 函数体、引号、注释），每条走 `sqlx::raw_sql`，
  **不套外层事务**——与上游一致，也让 `CREATE INDEX CONCURRENTLY` 这类语句可用；
* **先执行、后记账**：语句/文件失败 ⇒ 该文件**不写账本** ⇒ 整文件重跑。因此每个 compat 补丁必须**幂等**
  （`ADD COLUMN IF NOT EXISTS` / `SET DEFAULT` / `DROP CONSTRAINT IF EXISTS` + `ADD CONSTRAINT`）；
* **扩展门（gate）**：`operator_class_available` 探测本机有没有 `gin_bigm_ops`；`OPERATOR_CLASS_GATES` 目前只登记一条
  `446_issue_properties_bigm_index.up.sql` ⇒ 该文件**跳过 SQL 但仍然记账**（否则 `verify` 永远不 ready）。启动时校验每个门版本都在集合里存在，防止门指向不存在的文件；
* **并发安全**：整轮持 `pg_advisory_lock`（钉在同一条连接上，所有路径都显式 `pg_advisory_unlock`）；
* **账本**：`schema_migrations (version text primary key, applied_at timestamptz)`，与上游同名同形；
* **空集合**：只警告 + `Ok(0)`；目录不存在 ⇒ 报错（空集合会让「全部缺失」的结论毫无意义）。

### 2.3 CLI

```
multica-migrate run    --dir <DIR>（可重复，默认 migrations） [--json]
multica-migrate verify --dir <DIR> [--json]     # C5 readiness：所有 up 版本都已记账 + 必需表都在
multica-migrate status
multica-migrate doctor
```

`verify` 的输出就是 C5 的判据：

```
$ multica-migrate verify --dir migrations
loaded=566 applied=566 pending=0 missing_tables=[]
```

**没有实现 issue 正文提到的 `--upstream-dir`**：vendored 目录现在是 `migrations/` 的**子目录**，递归收集 + stem 合并已经覆盖
它，再加一个专用开关只会多一条能被写错的路径。`--dir` 可重复保留了「两个平级目录」的用法。
另外 `mc-migrate` 的 `DEFAULT_REQUIRED_TABLES` 里原来写的是本仓旧表名 `wakeup`，已改成上游的
`issue_wakeup` / `issue_wakeup_receipt`（否则 `verify` 永远缺表）。

---

## 3. 账本与老库：只支持重建（C1 / C3 / C5）

* **C1 迁移身份 = 文件 stem（`text`）**：`schema_migrations.version` 因此与上游同名同形；
* **C3 老库路径 = 重建，不做原地 rebaseline**：W0-B2 之前的开发库账本是 `BIGINT` 的主键，runner 检测到后**直接失败**：

  ```
  DbError::MigrationManifest: schema_migrations.version is bigint, but this runner keys migrations by
  file stem (text) — rebuild the database (see docs/26 §3) instead of migrating it in place
  ```

  理由：老库是 28 张本仓表，新集合是 116 张上游表，两者不是「同一套 schema 的两个版本」，逐表映射的收益全无——开发库是
  throwaway，重建是最短且不会留半迁移状态的路：

  ```bash
  dropdb <db> && createdb -O <role> <db>
  mc-migrate run --dir migrations
  ```

* **C5 readiness**：`verify` 要求**每个** up 版本都已记账且必需表都在（§2.3）。

---

## 4. `0001–0004` 的归宿（C4）：删除，内容按语义拆进 `compat/535–537`

`migrations/0001_init.up.sql` … `0004_reactions_and_subscribers.up.sql` 已删除。**只有真正本仓独有的语义**才进 compat，
其余一律改用上游对象（P4：不为让旧测试变绿而新建本地表）。

编号规则：上游最大号 534 ⇒ compat 从 **535** 起，沿用上游 3 位零填充（`migrations/upstream/` 的 70/71/99/146/147/148/280/372/373/374/380/381/405/406/433/434/435/436/507/508/517 **是空号，不可复用**，所以不能靠「填空号」插队）。
`--apply-set local` 下 `upstream/` 与 `compat/` 是同一个 stem 空间，数字一旦撞车 `load_dirs` 立刻报错。

| 补丁 | 内容 | 来源 |
| --- | --- | --- |
| `535_pat_revoked_at.up.sql` | `personal_access_token.revoked_at`；`token_prefix DEFAULT ''`；`issue_status.color DEFAULT '#6b7280'` | 复述退役的 `0002` |
| `536_auth_and_invitations.up.sql` | `verification_code.{consumed_at,purpose,user_id}`、`workspace_invitation.{token,accepted_at,revoked_at}` | 复述退役的 `0003` |
| `537_local_only_columns.up.sql` | `comment.routing_escalation`、`issue.{identifier,origin,origin_task_id,source_context_id,status_name}`、`member.updated_at`、`personal_access_token.{scopes,token_last4}`、`user.{email_verified_at,onboarding_state}`、`workspace.archived_at` | 本仓净新增列（上游确实没有） |
| `538_actor_type_vocabulary.up.sql` | 7 个 principal 词汇 CHECK 放宽（§6.1） | 两侧词汇取并集 |
| `539_status_and_role_vocabulary.up.sql` | `issue_status_category_check` + 3 个 role CHECK（§6.2） | 同上 |
| `540_comment_resolved_consistency.up.sql` | `comment_resolved_consistency` 的三元耦合放宽为「actor 两个字段同生同灭」（§6.3） | 本地 resolve 无 actor |

`issue_status.color` 的默认值取 `'#6b7280'` 而不是 `''`：上游有 `CHECK (color ~ '^#[0-9a-f]{6}$')`，`''` 会直接把
INSERT 打红——这是本片实测踩到的第一个坑。

**退役的本地表**：`plugin`、`wakeup`（旧名）不重建；上游对应物是 `plugin` 之外的 `issue_wakeup` / `issue_wakeup_receipt`。

---

## 5. `mc-repos` SQL 对齐：改名 / 别名 / 转型 / 双写

原则：**Rust 结构体字段名与类型一个都不动**（`mc-http` 的 DTO、`row.author_id.clone()` 这些调用点全部不动），
上游列与本仓列名/类型不一致的地方都在 SQL 里抹平。物理改名只发生在「上游那一列 `NOT NULL` 且无默认值、语义又完全相同」时。

| 上游 | 本仓字段 | 处理 |
| --- | --- | --- |
| `comment.content`（`NOT NULL`，无默认） | `body` | INSERT/UPDATE 写 `content`；读出 `content AS body` |
| `comment.author_id UUID` | `author_id: String` | 读出 `author_id::text AS author_id`；绑定 `$n::uuid` |
| `comment_reaction.actor_id` / `issue_reaction.actor_id` / `inbox_item.actor_id` / `issue.assignee_id` / `issue.creator_id` | 同上（`String`） | 同上（`inbox_item.actor_id` 还多一层 `COALESCE(…::text, '')`，因为它可空而本地不是 `Option`） |
| `inbox_item.recipient_id`（+ `recipient_type`） | `user_id` | 写 `recipient_id` + 固定 `recipient_type = 'user'`；读出 `recipient_id AS user_id` |
| `inbox_item.type` | `category` | 读出 `type AS category` |
| `workspace_invitation.invitee_email` / `inviter_id` | `email` / `invited_by_user_id` | 物理改名（`NOT NULL` 无默认 + 语义相同） |
| `verification_code.code` | `code_hash` | 物理改名（同上） |
| `issue_status.icon`（`NOT NULL DEFAULT ''`） | `Option<String>` | 读 `NULLIF(icon, '') AS icon`；写 `COALESCE($n::text, '')` |
| `issue_status.color` / `personal_access_token.token_prefix` | 本地省略 | compat `SET DEFAULT`（§4） |

**过滤侧保留 `::text`**：`LIST_WHERE` 的 `assignee_id::text = ANY($5::text[])`、`creator_id::text = $6::text`
——查询串来自 API，**任意字符串都必须只是「匹配不到」而不能 500**（`issue_table` 的 scope/group 谓词走的是内部 UUID，已改成直接绑 `uuid`）。

**双写**（上游那一列是权威，本仓列保留读兼容）：PAT `revoked` ↔ `revoked_at`、`verification_code.used` ↔ `consumed_at`、
`inbox_item.read`/`archived` ↔ `read_at`/`archived_at`、`workspace_invitation.status` ↔ 本地 `accepted_at`/`revoked_at`
（上游 revoke 是**硬删**，本地保留软撤：置 `revoked_at` + 把 `status` 镜像成 `'declined'`；上游 CHECK 里**没有** `'revoked'`）。

---

## 6. 词汇表：放宽上游 CHECK，而不是在 SQL 里搬运映射

本仓的 principal 词汇是 `user`（`docs/11-M2-ISSUE.md` 与 `mc-http/tests/{comments,issues}.rs` 把它当**契约断言**），
上游是 `member`。两条路：SQL 侧逐处 `'user'⇄'member'` 映射（约 26 处、且方向错一次就静默写错），或把上游 CHECK 放宽成并集。
本片选后者：**库同时接受两套词汇，本仓继续写 `'user'`（不要写 `'member'`）**。代价是 DB 层不再能挡住混写，收益是零映射、零契约改动。

### 6.1 `538`：actor / recipient / assignee / creator 类型（7 条）

| 约束 | 放宽后 |
| --- | --- |
| `comment.comment_author_type_check` | `user, member, agent, system, plugin, squad, autopilot` |
| `comment_reaction.comment_reaction_actor_type_check` | `user, member, agent` |
| `issue_reaction.issue_reaction_actor_type_check` | `user, member, agent` |
| `inbox_item.inbox_item_recipient_type_check` | `user, member, agent` |
| `issue.issue_assignee_type_check` | `user, member, agent, squad, autopilot` |
| `issue.issue_creator_type_check` | `user, member, agent, system` |
| `issue_subscriber.issue_subscriber_user_type_check` | `user, member, agent` |

本仓不写 `activity_log` / `attachment` / `project` / `quick_action` / `squad_member` / `autopilot*` / `issue_view` ⇒ 那些 CHECK 不动。

### 6.2 `539`：状态类别与角色

`issue_status_category_check` → `open, unstarted, started, done, closed`（本仓 `mc_core::status::StatusCategory` 有 `open`/`closed`）；
`workspace_invitation_role_check`、`workspace_share_link_role_check`、`member.member_role_check` →
`owner, admin, member, guest`（本仓 `WorkspaceRole` 含 `owner`/`guest`，`routes/invitations.rs` 还会写 `guest`）。

### 6.3 `540`：`comment_resolved_consistency` 放松

上游要求 `resolved_at` / `resolved_by_id` / `resolved_by_type` 三者同生同灭；本仓 `CommentRepo::resolve/unresolve` 是**没有 actor 的布尔翻转**
（DTO 不暴露 `resolved_by_*`），所以放宽为 `CHECK ((resolved_by_type IS NULL) = (resolved_by_id IS NULL))`——actor 仍然要么全有要么全无。

### 6.4 登记

以上 12 条约束差异 + 2 条列差异（`issue_status.color`、`personal_access_token.token_prefix`）+ 22 条本仓净新增列/
约束（`extra`）+ 9 条 `apply-exception` = `contracts/schema-deviations.tsv` 的 **45 行**，每行都有原因与承接 issue
（本片 36 行，W0-B 的 9 条 `apply-exception` 沿用）。drift 输出：

```
by category: apply-exception=9, differs=14, extra=22
registry  45 row(s): 45 matched, 0 stale (warning), 0 stale (error)
OK — every difference is registered
```

---

## 7. 继承来的收紧：`issue_status` 显示名唯一

上游有 `idx_issue_status_workspace_name_active UNIQUE (workspace_id, lower(name)) WHERE archived_at IS NULL`，本仓 `0001` 没有。
于是 `create` 撞显示名不再是「建两行同名状态」，而是 `409 Conflict`（`map_sqlx_err`）。这是**新增的约束**，不是偏离，
所以不在登记表里；`mc-repos` 的建状态测试已按上游口径调整（同名冲突用例改用不同显示名 + 说明注释）。

---

## 8. 已知缺口与越界最小改动

* **`446_issue_properties_bigm_index`**：本机没有 `pg_bigm` ⇒ 该索引不建（`apply-exception`）；生产若装了扩展，门会自动放行；
* **`assignee_id` 形态校验已由 LUM-1410 落地**：`routes/issues/helpers.rs::validate_assignee_target` 移植了上游
  `validateAssigneePair`（形态非法 → 400，且在写库之前）；本片在 rebase 后跑门 ⑥ 时发现其中 `squad` 分支读的是旧列
  `leader_agent_id`（上游 `leader_id`）⇒ 见下方越界条目；
* **⑧ drift 的固定名 scratch 库在并发下会假红 —— 已修（LUM-1463）**：`schema_snapshot.ScratchDatabase` 进入时 `DROP DATABASE IF EXISTS
  schema_probe_w0b_drift` + `CREATE`，两个 drift 进程同时跑会互相把对方的 scratch 库删掉 ⇒ 对端读到半成品 schema 而报“未登记差异”。
  本片当时遇到的症状就是 exit 1（单独重跑 ⑧ 与整轮 `--with-db` 都是 exit 0，已 10/10）；LUM-1463 把并发复现出来，拿到两种更早的症状：
  后到者在 `CREATE DATABASE` 上撞重名（0.6s exit 2），先到者在 `002_agent_config.up.sql` 上撞 `FATAL: database … does not exist`（24s exit 2）。
  **修法**：只改默认值——`scripts/schema_drift.py::DEFAULT_DB_NAME = f"schema_probe_w0b_drift_{os.getpid()}"`（`--db-name` 仍可显式覆盖；
  该文件 797 → 799 行 ≤ 800，**没碰**已在 800 行上限的 `schema_snapshot.py`，也没给它加白名单）。实测修前 2/2 红、修后 2/2 绿（`docs/37` §17.1）；
* **越界最小改动**（都不在 `mc-repos/**`、`migrations/**`、`mc-db`、`mc-migrate` 写集内，但被上游 `NOT NULL`/类型逼出来，
  已逐条在 PR 里说明）：
  * `crates/mc-http/src/routes/auth.rs`：`#[ignore]` 测试夹具的 `verification_code.code_hash` → `code`；
  * `crates/mc-http/src/routes/issues/helpers.rs`：指派 `squad` 的存在性校验读的是本仓旧列 `leader_agent_id`，
    上游叫 **`leader_id`** ⇒ 修成上游列名（否则 `assignee_type: "squad"` 的校验直接 500；base 上刚落地的
    LUM-1410/LUM-1423 代码写于切换前）；
  * `crates/mc-http/tests/{comments,inbox}.rs`：夹具 `INSERT` 的 `creator_id` 补 `$n::uuid`；
  * `crates/mc-repos/src/issue_status.rs`：同名冲突用例改名（§7）；
  * `scripts/file_size_baseline.tsv`：删掉 `scripts/schema_drift.py 807` 一行——该脚本被**压到 797 行 ≤ 800**，
    按门 ⑩ 的规则必须从白名单移除（这也是本片唯一碰 `scripts/` 白名单的地方）。

---

## 9. 复现命令

```bash
# 1) 从零应用（566 个迁移；账本是 text）
dropdb <db> && createdb -O <role> <db>
MULTICA_DATABASE_URL=postgres://role:pw@127.0.0.1:5432/<db> mc-migrate run --dir migrations
MULTICA_DATABASE_URL=... mc-migrate verify --dir migrations   # loaded=566 applied=566 pending=0 missing_tables=[]

# 2) drift：missing 必须为 0，且 exit 0
python3 scripts/schema_drift.py --db-url postgres://role:pw@127.0.0.1:5432/<db>
python3 scripts/schema_drift.py --apply-set upstream --db-url ...   # 探索：只看上游裸集合（缺 compat 时的原始缺口）

# 3) 门禁（10/10）
bash scripts/gates.sh --with-db --db-url postgres://role:pw@127.0.0.1:5432/<gate-db>
```

本片实测（本机 PG16，`:5432`）：`mc-repos` + `mc-http`（lib + 全部集成，含 `--include-ignored`）**218 通过 / 0 失败**；
门禁 10/10（⑧ drift exit 0、⑩ file-size 0，`gate4_lum1387`）。
