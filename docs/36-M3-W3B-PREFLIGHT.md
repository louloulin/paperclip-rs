# W3b 预飞检查 —— M3-4 / M3-5 / M3-6 晋升前的实测核对

> - **编制**：LUM-1425（2026-09-23 02:30 CST autopilot cycle；docs-only，本文件不含实现代码）
> - **基线**：`feat/multica-rs-initial` @ **`0acbae0`**（= `d7639f0` octopus merge [PR #18 M3-0 anchor + PR #20 门 ⑩] + 门禁清单 docs。远端 tip 实测一致）
> - **上游权威**：`docs/fixtures/upstream-routes.tsv`（456 行，commit `f41fae6b`）+ `docs/15-M3-PLAN.md` §1（逐条行号）
> - **前作**：`docs/35-M3-W3A-PREFLIGHT.md`（W3a 口径）。冲突处以 `docs/plan1.md` 为准（LUM-1357 裁决）。
> - 本 cycle **没有派发任何切片**（并发 3/3 满载，见 §1）；产出是「W3b 能不能开、开之前 base 上必须先落什么」的实测结论。
> - **后续更新（2026-09-23 03:00 cycle / LUM-1430）**：§1 的两条结论**均已解除**（W3a 三片已合入 base `4a61450`、LUM-1387 与 LUM-1423 已晋升）。见文末 **§10**。

---

## 1. 本 cycle 的两个结论

1. **W3b 现在不能晋升**，有两条独立理由（§4）：并发位 3/3 全被 W3a 占着（`LUM-1407/1408/1409` 各有 1 个 `running` run；`multica daemon status` 的 `active_task_count` = 4 含本 cycle），且 W3b 的**硬前置 W0-B2（LUM-1387）**仍在 `backlog`、无 run。
2. **base 上需要先落一个 anchor commit**，把 M3-4 与 M3-5 会共同踩到的三处共享文件（`mount.rs` / ⑦ 基线 / ⑨ 快照）预删干净 —— 配方与实测输出见 §3。落它之后，W3b 三片的写集**两两不重叠**，可以像 W3a 一样三片并行。

---

## 2. M3-0 锚点接线实测：四个切片 router **确实**接在全局路由表上（探针证据）

**为什么必须探针**：M3-0 anchor 的四个切片都是**空 `Router::new()`**。空 router 的 `.merge()` 与「压根忘了 merge」在 ⑤/⑦/⑨ 三门上的观测完全一致（路由表逐字不变、parity 数字不变）。静态读 `mount.rs` 只能证明「源码里有那行 `merge`」，不能证明「接上了」。这是**只能证伪不能证实**的一类断言，所以本轮做了探针。

做法（只改四个切片文件，**`mount.rs` 零改动**）：

```bash
# 1) 四个切片各塞一条标记路由（临时）
#    routes/{agents,runtimes,tasks,daemon}.rs: Router::new().route("/__m3probe/<slice>", …)
git diff --stat        # → 4 files changed, 4 insertions(+), 4 deletions(-)   ← mount.rs 不在列
# 2) 从全局 router 打 oneshot
cargo test -p mc-http --test m3_probe_tmp --features test-util
```

实测结果（2026-09-23，暖 target 1m23s）：

```text
running 2 tests
test control_unregistered_probe_path_is_not_found ... ok
test m3_anchor_slices_reachable_through_global_router ... ok
test result: ok. 2 passed; 0 failed
```

- 四条 `/__m3probe/{agents,runtimes,tasks,daemon}` 全部 **200**，且 body 各自是 `slice:<name>`（证明是**对应切片**在应答，不是别的 router 兜的）；
- 对照组 `/__m3probe/nobody` = **404**（证明本用例真的能区分「接上了」与「没接上」，不是恒绿）;
- 只改了 4 个切片文件 ⇒ 同时验证了 scaffold 的承诺「**切片只需实现自己的 `router()`，不必改 `mount.rs`/`mod.rs`**」；
- 探针已撤：`grep -rn __m3probe --include=*.rs` = 0、`git status --porcelain` 为空、HEAD 仍是 `0acbae0`。

**反向探针（同一次实验，重要）**：把切片路由注册成 `GET /api/agents`（与 M0 占位同名同方法）后，`mc_http::router()` **构树即 panic**：

```text
panicked at axum-0.7.9/src/routing/path_router.rs:70:22:
Overlapping method route. Handler for `GET /api/agents` already exists
```

⇒ `docs/15` §9.6.6 的「**必须整块删占位**」不是经验之谈而是可执行硬约束：只删 `POST` 留 `GET` 一样会在启动时炸。

---

## 3. W3b anchor 预删配方（已在 `0acbae0` 上实测一遍，本轮已回退，未提交）

**为什么要在 base 上先删**：`/api/agents` 与 `/api/runtimes` 两条 M0 占位（各 `get().post()`）分别属于 M3-5 与 M3-4，**都必须删**；两个块在 `mount.rs` 里只隔 4 行 ⇒ 两片同改 `mount.rs`。而且删占位会让 ⑦ 报 regression、让 ⑨ 的计数漂移 ⇒ 两片还都要改 `docs/fixtures/route-parity-baseline.json` 与 `crates/mc-conformance/report.json`。**三个共享文件、两个写者**，正是 M3-0 scaffold 当初要消灭的模式。按同样的手法预落在 base，两片就只剩自己的文件。

实测（`0acbae0` + 只删 `mount.rs` 里两个占位块，8 行）：

| 步骤 | 实测结果 |
| --- | --- |
| ⑤ `cargo test --workspace` | **exit 0，0 failed**（41s 暖 target）⇒ 没有任何测试/代码依赖这两条占位（`grep -rn '"/api/agents\|/api/runtimes'` 在 `crates/ tests/` 里也是 0 命中） |
| ⑦ `python3 scripts/route_parity.py --json` | **4 条 regression**：`GET/POST /api/agents`、`GET/POST /api/runtimes`；`counts`：local 140→**136**、implemented 125→**122**、known_gap 331→**334**、local_only 12→**11**（`POST /api/runtimes` 上游无对应物） |
| ⑦ `--write-baseline` | 基线 **140 → 136** 条（−4 行）；随后 `--quiet` **exit 0**、regression 0 |
| ⑨ `mc-conformance --no-db --check` | **红**：`unmounted` 5→**6**、`placeholder` 1→**0**；唯一受影响 fixture = `agents/TestProtectedRoutesRequireAuth@server/cmd/server/integration_test.go:433#1`（`GET /api/agents`，anonymous/router）：`200 + placeholder 信封` → `404 + unmounted` |
| ⑨ `--write` 后 `--check` | **exit 0**（`report.json` 需一并提交，commit message 里逐条解释上面两个数字漂移） |
| ⑩ `file_size_check.py` | `mount.rs` 只变短，无影响 |

**⇒ anchor commit 的交付物 = 3 个文件**：`crates/mc-http/src/routes/mount.rs`（−8 行）、`docs/fixtures/route-parity-baseline.json`（−4 行）、`crates/mc-conformance/report.json`（⑨ 重生成）。做完之后 W3b 三片的写集里**不再出现** `mount.rs` / ⑦ 基线 / ⑨ 快照 / `Cargo.lock`。

**副产物（顺带证伪的一个疑问）**：切片把路由写成 `/api/agents` 还是 `/api/agents/` **都可以** —— ⑦ 对尾部斜杠是**折叠比较**：两种写法实测都是 `GET /api/agents/` = implemented、0 regression（`slash_aliases` 只是报告里的一条 note，不是必须登记的例外）。

---

## 4. W3b 的前置（逐条实测）

| 前置 | 当前状态（2026-09-23 02:30 CST） | 依据 |
| --- | --- | --- |
| M3-0 anchor（LUM-1406） | **已合 base**（四个空切片 + `routes/mod.rs` 声明 + `mc-repos/{agent,runtime,task}.rs` stub） | `0acbae0` 树 + §2 探针 |
| **W0-B2（LUM-1387）** | **backlog，无 run** | `multica issue runs LUM-1387 --active` = 0 |
| W3a：M3-1 / M3-2 / M3-3 | 各 1 个 `running` run ⇒ **并发 3/3 满** | `multica issue runs <id> --active` |
| ⑦ 基线 / ⑨ 快照 / ⑩ 基线 | 均绿（本轮实测 ⑦ exit 0、⑨ exit 0、⑤ exit 0） | §3 表格 |

**为什么 W3b 必须等 LUM-1387（两条，缺一条都够）**

1. **写集互斥是 LUM-1387 的明文范围限制**：「本切片与任何碰 `mc-repos/**` 或迁移文件的业务切片互斥……建议晋升时**同时只放它一个**」。而 M3-4/5/6 的主交付正是 `crates/mc-repos/src/{runtime,agent,task}.rs` 的 Pg 实现 + `mc-http/tests/*` 的 DB e2e。
2. **更硬的是 schema**：W3b 三个 repo 的 SQL 与 DB e2e 现在只能对着本仓手写的 `migrations/0001`（28 张表）写；W0-B2 之后运行库变成**上游 138 张表**。`docs/15` §2.2 已实测：本地 `0001` 那 4 张与 M3 同名的表（`agent` / `agent_runtime` / `agent_task_queue` / `runtime_profile`）**形状与上游不同**，且本仓自造的 7 列（`retry_count` / `source_task_id` 等）**一律不用**——真值是上游迁移 `022`/`055` 的列与约束。先做 W3b 等于把同一批 SQL 写两遍，第二遍还得重测。

**⇒ 晋升顺序**：`LUM-1387`（独占位）→ 合入后 **M3-4 / M3-5 / M3-6 三片同波**（写集两两不重叠，见 §6）→ 再等 M3-1 + M3-3 合入后 **M3-7**（W3c）。

---

## 5. W3b 三片各要覆盖的路由（实测复核自 `docs/fixtures/upstream-routes.tsv`）

| 分族 | 条数 | 归属 | 上游 `router.go` 行号 | 本地当前状态 |
| --- | ---: | --- | --- | --- |
| runtime-profiles（`/api/workspaces/{id}/runtime-profiles*`） | 6 | **M3-4** | L1680-1681, L1717-1720 | 0（全 gap） |
| runtimes 台账 | 9 | **M3-4** | L2266, 2268-2272, 2281, 2289, 2293 | `GET /api/runtimes` = M0 占位（§3 预删） |
| runtimes 异步往返（`Initiate*` → `…/result`） | 8 | M3-7 | L2273-2280 | 0 |
| agents（13 条 `/api/agents*`） | 13 | **M3-5** | L2178-2191, 2196-2198, 2215-2216 | `GET/POST /api/agents` = M0 占位（§3 预删） |
| workspace 级 agent 统计（`agent-task-snapshot` / `agent-activity-30d` / `agent-run-counts`） | 3 | **M3-5** | L2318, 2329, 2332 | 0 |
| agent-builder | 4 | **M3-6** | L2224-2226, 2229 | 0 |
| task / lifecycle / usage / retry | 11 | **M3-6** | L1635, 1970, 1992-1998, 2016-2017, 2314, 2324 | **6 条是 501 stub，须原地替换** |
| daemon | 36 | M3-7 | L1523-1571 | 0 |

合计 **90 条 = 15 + 16 + 15 + 44**（与 `docs/15` §0 的「101 − 11(cloud-runtime)」一致；逐条行号见 `docs/15` §1.1–§1.6）。

**M3-6 的 6 个 stub：行号已漂移，以路径为准**（`docs/15` §1.6/§1.8 记的是 L91/123/124/125/126/133；`@0acbae0` 实测）：

| 路径 | 方法 | `crates/mc-http/src/routes/issues.rs` 实测行 |
| --- | --- | --- |
| `/api/issues/preview-trigger` | POST | L90 |
| `/api/issues/:id/active-task` | GET | L119 |
| `/api/issues/:id/rerun` | POST | L120 |
| `/api/issues/:id/task-runs` | GET | L121 |
| `/api/issues/:id/usage` | GET | L122 |
| `/api/issues/:id/tasks/:taskId/cancel` | POST | L129-130 |

⇒ M3-6 **要改 `routes/issues.rs`**（原地替换 handler，不能新增路由：同 path+method 重复注册会 panic，§2 反向探针）。

**已知计数噪声（不是缺口）**：`route_parity` 的 `M3 gap`（实测 92 + `M3+` 16）里含 **11 条 `cloud-runtime`** —— fixture 的 owner 单元格与 `scripts/route-owners.tsv` 都误标 M3，LUM-1357 已裁决判给 W9/M9（`docs/15` §9.1），改动这两个文件超出单 commit 范围，登记给 M9 立项时处理。

---

## 6. 三片写集与冲突矩阵（晋升时按这张表派分支）

| 切片 | 分支 | 写集（新增/修改） | 与同波切片重叠 |
| --- | --- | --- | --- |
| M3-4 | `feat/multica-rs-m3b-runtime-profiles` | `crates/mc-http/src/routes/runtimes.rs`、`crates/mc-repos/src/runtime.rs`、`crates/mc-http/tests/runtimes*.rs`、`docs/…` | 无 |
| M3-5 | `feat/multica-rs-m3b-agents` | `crates/mc-http/src/routes/agents.rs`、`crates/mc-repos/src/agent.rs`、`crates/mc-http/tests/agents*.rs`、`docs/…` | 无 |
| M3-6 | `feat/multica-rs-m3b-task-queue` | `crates/mc-http/src/routes/tasks.rs`、`crates/mc-repos/src/task.rs`、`crates/mc-http/src/routes/issues.rs`（6 stub）、`crates/mc-http/tests/tasks*.rs`、`docs/…` | 无（与 **LUM-1423** 撞 `routes/issues.rs`，见下） |
| 共享 | —— | `mount.rs` / ⑦ `route-parity-baseline.json` / ⑨ `report.json` / `Cargo.lock` | **由 §3 预删 + 集成 cycle 统一持有**，切片不得碰 |

- `routes/mod.rs` 与 `mount.rs` 由 M3-0 scaffold 预先改好 ⇒ **三片都不需要动**（§2 探针已证）。
- `crates/mc-repos/src/lib.rs` 已声明 `pub mod {agent,runtime,task}` ⇒ 也不动。
- **唯一真实冲突点**：M3-6 与 **LUM-1423**（R7 补课，拆 `routes/issues.rs` 2394 行）都改 `routes/issues.rs` ⇒ **LUM-1423 必须先合**（它同时解锁 PR #19/LUM-1410）。LUM-1423 的边界（只碰 `routes/issues*` + `tests/issues*`，不碰 `mc-repos`/迁移）与 W3b 三片不重叠。
- ⑩ 门对三片都生效：`routes/*.rs` 与 `tests/*.rs` 单文件 ≤ 800 行 ⇒ daemon/task 面注定要按域拆目录（`docs/15` §7.7）。

---

## 7. 下一个 cycle 的动作队列

1. **W3a 三片交付后**（按 LUM-1421 的 octopus merge 先例一次落底）⇒ 空出 3 个并发位。
2. 空出后**第一位给 LUM-1387（独占）**；另两位给 LUM-1423（只碰 `routes/issues*`，与 1387 不重叠）+ 可选的 docs-only 片。
3. LUM-1387 合入后：落 §3 的 anchor 预删（若 1387 顺带做了更好）→ 晋升 M3-4/5/6 三片（**issue 已按本文件预建**：M3-4 = **LUM-1427**、M3-5 = **LUM-1428**、M3-6 = **LUM-1429**，均为 `backlog`、已挂 LUM-1334、已含路由清单/写集/禁区/晋升条件；晋升 = `multica issue status <id> todo`）。
4. M3-1 + M3-3 合入后才谈 W3c（M3-7 / M3-8）。
5. **LUM-1370（M2-E）的正文需要改** —— 见 §7.1。

### 7.1 LUM-1370「新增 `0005_labels_and_properties.up.sql`」的前提已失效（重要更正）

- 原描述任务清单第 1 项要求新建 `0005_*.up.sql` 来建 `issue_label` / `issue_to_label` / `issue_properties` 三张表。
- **事实**：W0-B 已把上游 560 个迁移入库，这三张表**上游本来就有**（`001_init.up.sql:75,82` 与 `191_issue_properties.up.sql`）。LUM-1368 当时的「本仓没有这两张表」结论针对的是**本仓手写 `0001`**，不是上游 schema —— 切库之后这三张表自然到位。
- 而且 LUM-1387 的接管契约 **C2** 规定 compat 补丁从 **`535_`** 起、3 位零填充（C1 把记账键改成词干字符串）：写 `0005_` 会按字典序插进历史中间，直接违反 C1/C2。
- ⇒ 晋升 M2-E 之前必须把描述改成「**只做** 9 条路由（`/api/labels` 5 + `/api/properties` 4）+ `mc-repos/src/{label,property}.rs` + 回填 M2-A 的 5 条 501」，删掉迁移项；顺序上排在 **LUM-1387 之后**（label/property 的 SQL 与 DB e2e 必须对着上游 schema 写）。

---

## 8. 复算命令（本文件每个数字的来源）

```bash
cd <workdir>/paperclip-rs && export PATH="$HOME/.cargo/bin:$PATH"

# §1 并发位
multica issue runs LUM-1407 --active --output json    # 同样查 1408/1409/1387
multica daemon status --output json                   # active_task_count

# §2 锚点探针（临时改 4 个切片文件后）
git diff --stat                                       # 期望：只有 routes/{agents,runtimes,tasks,daemon}.rs
cargo test -p mc-http --test m3_probe_tmp --features test-util     # 2 passed
grep -rn __m3probe --include=*.rs | wc -l             # 撤探针后必须为 0

# §3 预删配方（改完 mount.rs 后）
cargo test --workspace                                # ⑤ exit 0
python3 scripts/route_parity.py --json                # 4 条 regression
python3 scripts/route_parity.py --write-baseline && python3 scripts/route_parity.py --quiet
cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json   # 红：unmounted 5→6
cargo run -q -p mc-conformance -- --no-db --write crates/mc-conformance/report.json   # 再 --check = exit 0

# §5 路由账（owner=M3/M3+）
grep -v '^#' docs/fixtures/upstream-routes.tsv | awk -F'\t' '$3=="M3"||$3=="M3+"' | wc -l   # 118 行
grep -n 'not_implemented' crates/mc-http/src/routes/issues.rs                                # 6 个 M3 stub 行号
wc -l crates/mc-http/src/routes/issues.rs crates/mc-http/tests/issues.rs                     # 2227 / 1104（base）
```

---

## 9. 本 cycle 没做什么（明确边界）

- **没派发任何切片**（并发 3/3 满；唯一能做的是把后续片预建成 `backlog` = LUM-1427/1428/1429，实测 `runs --active` 均为 0，未触发任何 run）。
- **没在 base 上改任何代码**：§3 的预删配方只是**实测演练**，已用 `git checkout --` 完整回退（`git status --porcelain` 为空）。本 cycle 落到 base 的只有本文件。
- **没合 PR #19**（被门 ⑩ 判红，由 LUM-1423 解锁）、**没动 LUM-1387/1370/1423 的正文**（改正文属它们的晋升 cycle；本文件只把结论登记在这里）。
- **没碰 cloud-runtime 的 owner 误标**（`docs/15` §9.1 已裁决判给 M9，改动面超出本片）。

---

## 10. 03:00 cycle 落地记录（LUM-1430）—— §1 的两条结论均已解除

| 事实 | 实测（2026-09-23 03:07 CST） |
| --- | --- |
| W3a 三片合入 base | **octopus merge `4a61450`**：PR #21（M3-1 / LUM-1407）+ #22（M3-3 / LUM-1409）+ #23（M3-2 / LUM-1408），三 PR 均 `merged=True`；66 文件 +15821/−88 |
| 门禁 | `bash scripts/gates.sh --with-db` **10/10 绿，187s**：⑤ `458 passed / 0 failed`（65 suites）、⑥ `89 passed / 0 failed`（含 `--ignored`）、⑦ `implemented 125/456`＋`regression 0`＋`unclaimed 0`、⑨ `pass 4 / mismatch 1 / unmounted 5 / placeholder 1 / unevaluable 47`（快照逐字未变）、⑩ 绿 |
| 写集核对 | W3a 合入的 66 个文件里 `mc-repos/**` / `migrations/**` / `mc-db` / `mc-migrate` **0 命中** ⇒ 未侵占 LUM-1387 的独占写集，§4 的「W3b 必须等 W0-B2」顺序不变 |
| 并发 | 三片交付后槽位全空（`daemon active_task_count` 由 4 降到 1）；本 cycle 晋升 **LUM-1387（独占）** 与 **LUM-1423**（`runs --active` 各 1 个 `running`，19:07:53 起），**第三个槽位有意留空** —— 队列里其余待办（M2-E / M3-4/5/6）都要碰 `mc-repos/**`，与 LUM-1387 的独占限制互斥 |

### 10.1 LUM-1423 正文第 2 步的实测更正

`git merge-tree --write-tree 4a61450 db080ca`（`db080ca` = PR #19 head）：`crates/mc-http/tests/issues.rs` 与 `docs/14-M2-TABLE.md` **自动合并**；**`docs/24-W0-CI.md` 有 1 处冲突**（起因是 `f0de4f7` 门 ⑩ 改过同一文档的门禁表，**与 W3a 无关**）。已在晋升时把这条写进 LUM-1423 正文。

### 10.2 本 cycle 未做（边界）

- **§3 的 anchor 预删仍未落**（`mount.rs` 两条 M0 占位 + ⑦ 基线 + ⑨ 快照）：配方未变，留给 LUM-1387 合入后的 cycle。
- **没有**动 `docs/15` §8 的门禁数字；**没有**改 LUM-1370 的正文（它已由 LUM-1384 cycle 改成「不写 `0005`、前置 = W0-B2」）。
- octopus merge 用了 git 的默认 commit message（`Merge commit '…'`，没带 `merge(...)` 标题）—— 下次集成请用 `git merge --no-ff -m "…" <sha1> <sha2> <sha3>`。
