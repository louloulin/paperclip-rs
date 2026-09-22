# W3a 预飞检查 —— M3-1 / M3-2 / M3-3 晋升前的实测核对

> **基线**：`feat/multica-rs-initial` @ `9d494c7`（2026-09-23 01:21 +0800）。
> **执行**：LUM-1417 autopilot cycle（2026-09-23 01:30 Asia/Shanghai）。
> **为什么只写文档不派发**：该 cycle 开始时并发槽 **3/3 占满**（LUM-1406 / LUM-1410 / LUM-1416，均 17:22:38Z 起跑），
> 按「一次最多三个任务」的约束不得再派发；本文件把**下一 cycle 的晋升决策所需的实测事实**一次收齐，
> 避免 M3-1/2/3 三片带着过期数字开工（计划里的 `AdapterRegistryStub` 计数与 stub 行号都已漂移，见 §3）。
>
> 本文件**不改** `docs/plan1.md` / `docs/15-M3-PLAN.md`：三个在飞切片按约定要回填 `plan1.md §8`，
> 并发期由它们写；此处只登记实测，不动共享文档。

---

## 1. 基线健康证据（可复算）

| 检查 | 结果 |
| --- | --- |
| GitHub Actions（`feat/multica-rs-initial` @ `9d494c7`） | run **35760112528** → `success`（三 job：`fast` / `db` / `contract`）<br>前三次：`2fd7530`→35758059922 ✅、`3ee2d14`→35755752778 ✅、`8ad10e5`→35751311958 ✅ |
| `python3 scripts/route_parity.py --quiet` | **exit 0**；456 upstream / **140** local registered；`implemented 125`（**112 real** + 13 placeholder）；`known_gap 331`；`unclaimed 0`；`regression 0`；`local_only 12` |
| 本仓 crate 数 | 18 个（`ls crates/`） |

> ⚠️ 立项 issue（LUM-1407/1408/1409）正文写的是「立项时 @`8ad10e5` base CI 绿（run `35751311958`）」——
> 该事实仍然成立，但那**不是最新基线**；晋升时请按当前 base 的 head（写作时 = `9d494c7`，run `35760112528`）复核。

---

## 2. 晋升前置核对：三片的 in-repo 前提**全部成立**

| 检查项 | 计划假设（`docs/15` §4 / §7.2） | 实测 @`9d494c7` | 结论 |
| --- | --- | --- | --- |
| 新 crate 名未被占用 | 5 个新 crate | `mc-daemon-proto` / `mc-runtime` / `mc-task` / `mc-agent` / `mc-daemon` **全部不存在** | ✅ |
| 根 manifest 不需要改 | `members = ["crates/*", "apps/*"]`（glob） | `Cargo.toml:8` 逐字如此；目录是**扁平** `crates/mc-*`（不是 plan1 §3.2 设想的 `crates/{platform,domain,…}/*`）⇒ 新 crate 放 `crates/mc-xxx/` 即可 | ✅ |
| 依赖已在 lock / workspace 依赖 | `proptest`（M3-3）、`tokio-tungstenite`（M3-7） | `Cargo.toml` 有 `proptest = "1"`；`Cargo.lock:4167` 有 `tokio-tungstenite`；`jsonrpsee` **不在** lock（M3 不需要，插件宿主属 M6） | ✅ |
| 三片不受 W0-B2（LUM-1387）阻塞 | 三片均 0 路由、不落库 | 交付物 = 3 个新 crate（+ `state.rs` 替换 + `routes/issues.rs` 只读），无 SQL、无迁移 | ✅ |
| 三片互不共享可写文件 | M3-2 是唯一写 `state.rs` 的切片 | `state.rs` / `mount.rs` / `mod.rs` / `Cargo.lock` 由 M3-0 scaffold 一次性预置 | ✅（前提：M3-0 先合入） |
| **硬前置 M3-0（LUM-1406）已合入 base** | 缺 scaffold 时三片互抢 `lib.rs`/`mod.rs`/`mount.rs`/`Cargo.lock` | 写作时 LUM-1406 **仍在运行**（`running` @17:22:38Z），**未合入** | ⏳ **唯一未满足项** |

⇒ 晋升动作 = 「等 M3-0 的 `merge(m3-0)` 落进 `feat/multica-rs-initial`」→ 一次把 LUM-1407/1408/1409 三件置 `todo`（正好占满 3 槽）。

---

## 3. 与 `docs/15-M3-PLAN.md` 的三处数字漂移（切片开工前请用**实测值**）

### 3.1 `AdapterRegistryStub` 的替换范围：20 行 / 10 文件 → **23 行 / 12 文件**

- 计划 §4 M3-2 与 LUM-1408 正文记的是 @`8ad10e5` 的实测：`20 行 / 10 文件`（「7 个调用点」= 7 个测试文件）。
- @`9d494c7` 实测：**23 处引用 / 12 个文件**（`grep -rc AdapterRegistryStub crates/ --include='*.rs'`，含定义 2 处）。
- 多出来的 2 个文件：
  - `crates/mc-http/tests/issue_table.rs`（M2-D 新增测试）；
  - **`crates/mc-conformance/src/harness.rs`（W0-C / LUM-1388 新增的消费者）** —— 这一处最要紧：
    它属于**契约门 ⑨ 的代码路径**，M3-2 若只按旧清单替换，`cargo build`/⑨ 门会直接红。
- 完整清单（12）：`mc-http/src/state.rs`（定义 3 处）、`mc-http/src/routes/auth.rs`、`mc-http/src/routes/inbox.rs`、
  `mc-conformance/src/harness.rs`，以及 9 个 `crates/mc-http/tests/*.rs`
  （`issues` / `issue_table` / `comments` / `inbox` / `pats` / `invitations` / `share_links` / `contract_gaps` …）。

### 3.2 `ConfigSnapshot {` 构造点：11 个文件 → **12 个文件**

- 计划 §7.2 第 6 项（scaffold 给 `ConfigSnapshot` 加 `#[derive(Default)]` 并把构造点收尾改成 `..Default::default()`）写「现状出现在 11 个文件里」。
- @`9d494c7` 实测：**12 个**——新增的同样是 `crates/mc-http/tests/issue_table.rs`。
- 另：`AdapterRegistryStub` 本身**已经**是 `#[derive(Default)]`（`state.rs:41`），M3-0 只需处理 `ConfigSnapshot`。

### 3.3 `routes/issues.rs` 的 501 stub：行号已漂移，**以路径为准**

- 计划 §5 M3-6 引「已在 `routes/issues.rs:91/123/124/125/126/133` 注册」；实测这些行号已不对（M2-C/M2-D 追加路由后整体下移）。
- @`9d494c7` 实测该文件（2227 行）共 **19 条 stub 路由 / 20 处方法注册**；全仓 `not_implemented` 代码出现 **28 处**。
- **M3-6 要接管的 6 条**（其余 stub 不属 M3）现行位置：

  | 路径 | 方法 | 现位置 |
  | --- | --- | ---: |
  | `/api/issues/preview-trigger` | POST | L90 |
  | `/api/issues/:id/active-task` | GET | L119 |
  | `/api/issues/:id/rerun` | POST | L120 |
  | `/api/issues/:id/task-runs` | GET | L121 |
  | `/api/issues/:id/usage` | GET | L122 |
  | `/api/issues/:id/tasks/:taskId/cancel` | POST | L129–132 |

- 其余 13 条 stub 的归属（**不是** M3，别在 M3 切片里顺手实现）：`timeline` / `attachments` / `pull-requests` / `labels` / `labels/:labelId` /
  `quick-actions` / `comments/trigger-preview` → M2 面（label/property 见 LUM-1370）；`wakeups` 系列 + `/api/issue-wakeups` → M5。

---

## 4. 下一 cycle 的晋升清单与队列（写作时快照）

**可晋升（硬前置 = M3-0 合入）**，三件同时晋升正好占满 3 槽：

| # | issue | 切片 | 分支 | 0 路由 |
| --- | --- | --- | --- | --- |
| 1 | LUM-1407 | M3-1 daemon 协议冻结 | `feat/multica-rs-m3a-daemon-proto` | ✅ |
| 2 | LUM-1408 | M3-2 runtime 抽象 + pi-local adapter | `feat/multica-rs-m3a-runtime-adapter` | ✅ |
| 3 | LUM-1409 | M3-3 task 领域层状态机 | `feat/multica-rs-m3a-task-domain` | ✅ |

**本轮不进（登记原因，避免下一 cycle 误抢）**：

| issue | 为什么等 |
| --- | --- |
| LUM-1370（M2-E label/properties） | 要写 `crates/mc-http/src/routes/issues.rs`（与 M3-6 冲突）与 `mc-repos`，且 D1 的 label 两表缺口未决 |
| LUM-1387（W0-B2 schema 切换） | 独占 `mc-repos/**` + `migrations/compat/`；前置「W0-B 已落」成立，但语义是**全仓 SQL 一次切换**，适合单独一 cycle、不与路由切片并行 |
| LUM-1416（R7 单文件 800 行门） | 本轮已在跑 |

**W0-B2 的遗留前提（复核实测）**：`crates/mc-migrate/src/lib.rs:52` 的 `DEFAULT_REQUIRED_TABLES` 里**仍是 `"wakeup"`**
（上游是 `issue_wakeup` / `issue_wakeup_receipt`）⇒ LUM-1387 必须同时改这一行，否则切换后 `mc-migrate` verify 立刻红灯。

---

## 5. 复算命令（本文件所有数字的来源）

```bash
cd <paperclip-rs 检出目录>            # base = feat/multica-rs-initial @ 9d494c7
git fetch origin feat/multica-rs-initial && git log --oneline -1 origin/feat/multica-rs-initial

# §1 base CI（换 token：printf 'protocol=https\nhost=github.com\n\n' | git credential fill）
curl -s -u "x-access-token:$TOK" \
  "https://api.github.com/repos/louloulin/paperclip-rs/actions/runs?branch=feat/multica-rs-initial&per_page=6" \
  | python3 -c 'import json,sys;[print(r["id"],r["head_sha"][:8],r["conclusion"]) for r in json.load(sys.stdin)["workflow_runs"]]'
python3 scripts/route_parity.py --quiet        # → exit 0；140 local / 125 implemented(112 real) / known_gap 331

# §2 前提
for c in mc-daemon-proto mc-runtime mc-task mc-agent mc-daemon; do [ -d crates/$c ] && echo "$c EXISTS" || echo "$c absent"; done
sed -n '8p' Cargo.toml                          # members = ["crates/*", "apps/*"]
grep -n '^proptest' Cargo.toml; grep -n 'name = "tokio-tungstenite"' Cargo.lock

# §3.1 / §3.2 沙箱计数
grep -rc AdapterRegistryStub crates/ --include='*.rs' | grep -v ':0' | awk -F: '{s+=$2} END {print s" refs / "NR" files"}'
grep -rln 'AdapterRegistryStub' crates/ --include='*.rs' | sort
grep -rln 'ConfigSnapshot {' crates/ --include='*.rs' | wc -l
sed -n '35,55p' crates/mc-http/src/state.rs

# §3.3 stub 清点
grep -n not_implemented crates/mc-http/src/routes/issues.rs
grep -rn not_implemented crates/ --include='*.rs' | wc -l

# §4 W0-B2 遗留前提
sed -n '52,66p' crates/mc-migrate/src/lib.rs
```

---

## 6. 本 cycle 没做什么（明确边界）

- 未派发任何任务（3/3 槽满，`multica issue runs <各 in_progress issue> --active` 实测）。
- 未改任何代码、迁移、`Cargo.toml`/`Cargo.lock`、`mount.rs`/`state.rs`（在飞切片的写集）。
- 未改 `docs/plan1.md`、`docs/15-M3-PLAN.md`（同上：切片按约定回填；本文件只登记实测）。
- 未刷新 `docs/fixtures/route-parity-baseline.json`（§7.3 规定由集成任务统一刷）。
