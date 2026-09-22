# 28 — W0① workspace 重构（glob members / lints 统一 / 依赖审计 / hakari 评估）

> 切片：**LUM-1389 / W0 ①**（`docs/plan1.md` §3.2 目录布局、§4.6 工具链、§5 W0 ①）
> 分支：`agent/devbox5/w0-1-workspace-refactor` → `feat/multica-rs-initial`（base `ce5d499`）
> 交付物：根 `Cargo.toml`、7 个成员 `Cargo.toml`、`tests/smoke.rs`（1 行）、`scripts/audit_workspace_deps.py`、本文件
> 结论：**四项全部落地；成员集合与重构前逐字一致（19 = 19）；七道门 exit 0；`Cargo.lock` 只少 1 行（可逐行解释）；`cargo-hakari` 结论 = 暂不引入（有实测数据 + 明确重启条件）**

---

## 1. 结论速览

| # | 任务 | 结论 | 证据 |
| --- | --- | --- | --- |
| 1 | `members` 改 glob | 已改 `["crates/*", "apps/*"]`；成员集合**逐字未变** | `cargo metadata --no-deps` name+manifest_path 集合对比 → `IDENTICAL`（19/19）§2.2 |
| 2 | 统一 `[lints]` | **无需改动**：19/19 成员早已 `[lints] workspace = true`，0 处成员级覆盖 | `audit_workspace_deps.py` → `E1=0 E2=0`；门 ③④ exit 0 |
| 3 | `[workspace.dependencies]` 审计 + 落地 | 17 处"成员直写 version"全部 hoist 继承；**删掉 1 个多余的 `tower04` 别名**；`Cargo.lock` 净减 1 行 | 审计 `A2=17→0 A3=1→0`；§4.3、§5.2 |
| 4 | `cargo-hakari` 评估 | **暂不引入**（当前抖动只有 3 个 crate / 12s，代价大于收益） | 实测 §6.2；重启条件：crate 数 > 40 |

成员数没有变化（19），没有任何 crate 新增/删除，没有业务逻辑改动。

---

## 2. `members` 改成 glob

### 2.1 改法

```toml
# 之前：19 个字面路径，每加一个 crate 都要改这一行（→ 所有切片在同一行冲突）
members = ["crates/mc-auth", "crates/mc-authz", …, "apps/mc-server", "apps/mc-cli"]

# 之后
members = ["crates/*", "apps/*"]
```

根 manifest 本来就没有 `exclude`（实测 `grep '^exclude' Cargo.toml` 为空），所以没有需要保留的排除项。

### 2.2 成员集合一致性验证（本任务的硬验收）

```bash
# 重构前
cargo metadata --no-deps --format-version 1 > /tmp/meta_before.json
# 重构后：比较 (name, manifest_path) 集合
```

结果：`before n=19, after n=19, IDENTICAL`，`only_before=[] only_after=[]`。
`workspace_members` 也都是 19 条。即 **glob 展开出的成员与原来的字面列表是同一批包、同一批路径**。

### 2.3 glob 的三条实测约束

在 `/tmp/globtest*` 用最小 workspace 实测（不是照搬手册）：

| 情形 | 结果 |
| --- | --- |
| `members = ["xtask"]`，但 `xtask/` 不存在 | `error: failed to load manifest for workspace member …/xtask` / `No such file or directory (os error 2)`，**exit 101** |
| `members = ["crates/*"]`，`crates/nested/` 被 glob 命中但没有 `Cargo.toml` | 同样 **exit 101**：`failed to load manifest for workspace member …/crates/nested`（`referenced via 'crates/*'`） |
| `members` 里同时写 `crates/*` 与 `crates/nested/*` | 仍然失败 —— `crates/*` 已经命中了 `crates/nested` 这个目录本身 |

因此 glob **只能用于"每个直接子目录都是一个 package"这一层的目录**。两条直接推论：

- **`xtask` 现在不能进列表**（目录还不存在）。等真的创建 `xtask/` 时，往 `members` 加一个字面 `"xtask"` 即可 —— 根 manifest 里已经写好这条注释，避免下一个人"顺手补上"把树弄红。
- 未来按 `plan1.md` §3.2 拆成 `crates/{platform,domain,infra,api}/` 时，**必须把 `crates/*` 换成逐层 glob**，不能并存：

  ```toml
  members = ["crates/platform/*", "crates/domain/*", "crates/infra/*", "crates/api/*", "apps/*", "tools/*"]
  ```

  这也是 §2.2 那种"成员集合对比"在**每次**调整 members 后都必须重跑的原因。

### 2.4 一个非目标

本切片只动"怎么列成员"，不动目录布局本身（`crates/` 下仍是扁平的 17 个 + `apps/` 2 个）。
分层目录（domain/infra/api）是 W2 之后的事，见 §2.3 的迁移写法。

---

## 3. `[lints]` 统一（本项零改动，但有证据）

任务要求"给每个成员加 `[lints] workspace = true`，例外要写清理由"。**实测结果是这项已经做完了**，
所以本切片在这项上没有产生 diff —— 但把判定做成了可复现的检查（`audit_workspace_deps.py` 的 E 段）：

```text
## E1 成员缺 [lints] workspace = true (0)
## E2 成员级 lints 覆盖（与 workspace = true 互斥） (0)
```

- E1 = 0：19/19 成员都有 `[lints] workspace = true`，无例外，因此没有"需要写理由的例外"。
- E2 = 0：没有成员自带 `[lints.*]` 表。cargo 里这两者互斥（`cannot override workspace lints`），
  所以 E2 非空时应该**先合并到根 `[workspace.lints]`**，而不是就地保留。

门 ③ `cargo clippy --workspace --all-targets -- -D warnings`（exit 0）与门 ④
`cargo clippy -p mc-http --all-targets --features mc-http/test-util -- -D warnings`（exit 0）
覆盖了两条 lint 路径，**没有一个新告警需要修**（pedantic 已在根统一开着）。

---

## 4. `[workspace.dependencies]` 审计 + 落地

### 4.1 审计工具与判定口径

`scripts/audit_workspace_deps.py`（只读、永远 exit 0、支持 `--json`）。判定口径写死在脚本 docstring 里：

| 段 | 含义 | 是否"该修" |
| --- | --- | --- |
| A1 | 成员直写 `version`，而 `[workspace.dependencies]` 已有同名条目 | 该修（改成 `workspace = true`） |
| A2 | 成员直写 `version`，workspace 层没有 | 该修（先 hoist 再引用） |
| A3 | 成员用 `package =` 改名引用（如 `tower04 = { package = "tower" }`） | 需单独决策（改名后不能直接继承） |
| B | `Cargo.lock` 里同名 crate 多版本 | 绝大多数是传递依赖，**不自动判红**，要 `cargo tree -i` 归因（§5） |
| C1 | **normal/build** 依赖上的 feature 分歧 | 该修 |
| C2 | **dev-dependencies** 上的额外 feature | 不修（`{ workspace = true, features = [...] }` 的正常用法，登记） |
| D | `[workspace.dependencies]` 里没有成员使用的条目 | 决策，不强制删 |
| E1/E2 | `[lints]` 统一性（§3） | 该修 |

**它不是门禁**，也没有加进 `scripts/gates.sh`：这类审计的"正确值"会随新 crate 持续变化，
做成硬门会让每个后续切片都卡在统计口径上。它是给切片作者和 review 用的工具。

### 4.2 修复前 / 修复后

```text
修复前: FINDINGS: A1=0 A2=17 A3=1 B=40 C1=0 C2=2 D=3   （无 E 段）
修复后: FINDINGS: A1=0 A2=0  A3=0 B=40 C1=0 C2=2 D=3 E1=0 E2=0
```

A1/A2/A3 清零，C1 本来就是 0（**没有一处真正的 feature 分歧**），B/D/C2 是登记项（§4.5、§5）。

### 4.3 变更清单

根 `[workspace.dependencies]` 新增 15 条（都是原来散在成员里的）：`url`、`http`、`http-body-util`、
`bytes`、`aes-gcm`、`zeroize`、`aws-config`、`aws-sdk-s3`、`aws-sdk-secretsmanager`、
`aws-credential-types`、`opentelemetry`、`opentelemetry_sdk`、`opentelemetry-otlp`、
`tracing-opentelemetry`、`tempfile`。

成员侧（7 个 manifest，共 18 处）：

| 成员 | 改动 |
| --- | --- |
| `crates/mc-config` | `url` → `{ workspace = true }` |
| `crates/mc-errors` | `http` → `{ workspace = true }` |
| `crates/mc-http` | `http-body-util` → `{ workspace = true }`；**删除 `tower04` 别名**（§5.2） |
| `crates/mc-secrets` | `zeroize`、`aes-gcm`、`aws-sdk-secretsmanager`(optional)、`aws-config`(optional)、`tempfile` → 继承 |
| `crates/mc-storage` | `bytes`、`aws-sdk-s3`(optional)、`aws-config`(optional)、`aws-credential-types`(optional)、`tempfile` → 继承 |
| `crates/mc-telemetry` | `opentelemetry`、`opentelemetry_sdk`、`opentelemetry-otlp`、`tracing-opentelemetry`（都 optional）→ 继承 |
| 根 `Cargo.toml` | 新条目 + 说明注释 |

净效果：**版本与 feature 的真相只有一个地方**（根 manifest），成员只声明"我要它 / 我要额外开哪些 feature"。

### 4.4 hoist 的两条硬约束（实测）

1. **`optional = true` 不能写在 `[workspace.dependencies]` 里**：

   ```text
   error: failed to parse manifest … `aws-config` is optional, but workspace dependencies cannot be optional
   exit 101
   ```

   正确写法是成员侧 `{ workspace = true, optional = true }`（已实测 exit 0）。

2. **改名引用（`package =`）无法直接继承**：A3 命中的就是 `tower04`，处理见 §5.2。

### 4.5 C2 与 D 为什么不修

- **C2 = 2**：`mc-core` 的 dev-dependency `tokio` 额外开 `+macros,rt`；`mc-http` 的 dev-dependency `tower` 额外开 `+util`。
  这是**故意的**：测试专用 feature 不该反向污染整个 workspace 的 `tokio`（那会让所有成员都编 macros）。
  `{ workspace = true, features = [...] }` 正是表达"继承版本 + 本处多开 feature"的写法。
- **D = 3**：`dashmap`、`mockall`、`proptest` 目前没有成员引用。保留的理由是它们已在
  `plan1.md` §4.6 的工具清单里（M2+ 的并发缓存 / mock / 属性测试），删掉只会在下个切片再加回来。
  审计把它们列出来，是为了让 reviewer 一眼看到"这 3 条是计划内预留，不是漏删"。

---

## 5. 重复版本（B = 40）

### 5.1 三类来源

`B=40` 是 `Cargo.lock` 里同名多版本的数量，**其中没有一条是本切片造成的**，按来源分三类：

| 来源 | 例子 | 能否在本仓消除 |
| --- | --- | --- |
| 可选 feature 拉进来的云 SDK / OTLP | `http 0.2.12/1.5.0`、`hyper 0.14.32/1`、`h2 0.3.27/0.4`、`tower 0.4.13/0.5.3` | 不能（除非升级/替换 aws-sdk、tonic，属独立决策） |
| 编译器/生态传递依赖 | `syn 2/3`、`thiserror 1/2`、`getrandom 0.2/0.4`、`rand 0.8/0.10`、`windows-*` 多代 | 不能（由上游决定） |
| **本仓直接声明** | 只有一处：`tower04`（§5.2） | **能 → 已消除** |

关键实测：**默认 feature 图里这些"重复版本"大多数根本不在图里**，它们只是 lock 的"可选/feature 相关条目"：

```console
$ cargo tree -i tower@0.4.13     # 默认图
error: package ID specification `tower@0.4.13` did not match any packages
# 同样：http@0.2.12 / hyper@0.14.32 / h2@0.3.27 在默认图里也不存在

$ cargo tree -i tower@0.4.13 --workspace --all-features
tower v0.4.13
└── tonic v0.12.3
    ├── opentelemetry-otlp v0.17.0
    │   └── mc-telemetry          # 只有开 otlp feature 才进来
```

所以"lock 里有 40 个重复版本"读成"默认构建会编两份"是错的；判断方法就是
`cargo tree -i <name>@<version>`：**能查到 = 真在图里；报 `did not match any packages` = 只在 lock 里**。

### 5.2 唯一一处可控重复：`tower 0.4` → 已消除

**发现**：根 `tests/smoke.rs` 用 `tower04::ServiceExt::oneshot`，`tower04` 是
`{ package = "tower", version = "0.4", features = ["util"] }` 的别名，注释写的理由是
"axum 0.7 基于 tower 0.4 的 Service trait"。实测**这个理由不成立**：

| 证据 | 命令 / 结果 |
| --- | --- |
| axum 0.7.9 依赖的是 **tower 0.5.3** | `cargo tree -p axum --depth 1` → `tower v0.5.3` |
| tower 0.4 与 0.5 **共用同一个 `tower-service 0.3.3`**（`Service` trait 的真正提供者） | `Cargo.lock`：`tower 0.4.13` 与 `tower 0.5.3` 的 dependencies 都含 `tower-service`（lock 里只有一个 0.3.3） |
| 同一个写法在其余 7 个测试文件里早就用 `tower`(0.5) 且全绿 | `crates/mc-http/tests/{comments,invitations,issues,contract_gaps,pats,share_links,inbox}.rs` 都是 `use tower::ServiceExt;` |

**改法**：删掉成员与根 manifest 里的 `tower04` 声明（2 处）+ `tests/smoke.rs` 一行
`use tower04::ServiceExt;` → `use tower::ServiceExt;`。`mc-http` 的 dev-dependencies 里本来就有
`tower = { workspace = true, features = ["util"] }`，什么都不用加。

**验证**（不是"编译过了"就完事 —— 该测试的真实分支需要数据库才走）：

```console
$ MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --test smoke
test full_smoke ... ok
test workspace_member_http_e2e ... ok     # 真的走完了 oneshot 链路
test result: ok. 2 passed; 0 failed
```

**`Cargo.lock` 变化：净减 1 行**：

```diff
@@ mc-http dependencies
- "tower 0.4.13",
  "tower 0.5.3",
```

`tower 0.4.13` 这个 **package 条目仍然留在 lock 里**——因为 `mc-telemetry` 的可选 `otlp`
（`opentelemetry-otlp → tonic 0.12.3`）还需要它。也就是说：**默认构建图从此少一个 tower 大版本；
`--all-features` 下仍会有两份，那是 tonic 带来的，不在本切片范围**。

### 5.3 关于 `http 0.2 / hyper 0.14 / h2 0.3`

它们由 `aws-smithy-http-client`（`aws-sdk-s3` / `aws-sdk-secretsmanager` 的依赖）引入，
而这两个 SDK 在 `mc-storage` / `mc-secrets` 里是 `optional`（`s3` / `aws` feature，`default = []`）。
所以：**默认构建不编它们；开了 `--all-features` 才会两代并存**。要收敛只能等 SDK 换代或换 SDK，
属独立决策（与 `axum 0.8` / `sqlx 0.9` 同级）。

---

## 6. `cargo-hakari` 评估

### 6.1 它解决什么

`cargo-hakari` 生成一个 `workspace-hack` crate，把整个 workspace 的 feature 组合"钉死"成一份，
用来消除**同一个依赖在不同 feature 组合下反复重编**的抖动（例如 `cargo build -p A` → `cargo test --workspace`
来回切时全量重编）。`plan1.md` §4.6 的原话是"**crate 数 >40 时启用**"。

### 6.2 实测抖动（本仓当前 19 个 crate）

在同一台机器、同一个 warm `target/` 上依次切换构建目标，数 `Compiling` 行 + 计时：

| 步骤 | 命令 | 重编 crate 数 | 耗时 |
| --- | --- | ---: | ---: |
| A | `cargo build --workspace --all-targets --locked`（warm） | 1 | 8s |
| B | `cargo build -p mc-http --all-targets`（切到子集） | **3** | 12s |
| C | 切回 `--workspace` | 0 | 1s |
| D | `cargo build -p mc-http --all-targets --features mc-http/test-util` | 1 | 24s |
| E | 再切回 `--workspace` | 0 | 1s |

参照量：本仓**冷启动完整构建是 265 个编译单元**（门 ②，`/tmp/gates_w0_1.log`）。
即最坏情况下 hakari 能省掉的抖动是 **3 个 crate（≈1.1% 的图）**，而且切回来是 0 重编
（resolver = "2" + 统一走 `--workspace` 的收益已经拿到了）。

### 6.3 代价

1. 生成一个 `workspace-hack` crate —— 与本切片"不新增 crate"的约束直接冲突；放进 `crates/` 会被
   `crates/*` glob 自动纳入（**这本身是个好处，也是**`members` 改 glob 之后**才成立的事实**），但它会出现在
   `cargo metadata` 的成员列表里，未来 §2.2 那种"集合对比"就要每次都把它算进去。
2. **每台开发机 + CI 都要装 `cargo hakari`**（version-pinned），否则 `cargo hakari verify` / CI 步骤直接失败；
   这是给整条流水线加一个新的外部工具依赖（门禁脆弱点）。
3. 依赖变更后必须重跑 `cargo hakari generate` + `manage-deps`，且 `workspace-hack` 的 diff 会周期性
   出现在无关 PR 里，review 噪音。
4. 收益上限由 §6.2 决定：当前只有秒级抖动，而它带来的复杂度是长期性的。

### 6.4 结论与重启条件

**结论：暂不引入。** 理由是**实测收益小、代价长期**，而不是"官方不推荐"。

**重启条件（任一满足就重新评估）**：
- workspace crate 数超过 **40**（`plan1.md` §4.6 的原定阈值）；或
- 出现真实的"切 feature 全量重编"证据（用 §6.2 的同一张表复测，若单次抖动 > 50 个 crate 或 > 60s，就值得）。

复测脚本（原样照抄即可）：

```bash
run(){ label="$1"; shift; t0=$(date +%s); n=$("$@" 2>&1 | grep -c '^ *Compiling'); t1=$(date +%s); echo "$label: $n crate, $((t1-t0))s"; }
run "A) --workspace" cargo build --workspace --all-targets --locked
run "B) -p mc-http"  cargo build -p mc-http --all-targets
run "C) 切回"        cargo build --workspace --all-targets --locked
```

---

## 7. 验证

七道门（`scripts/gates.sh --with-db`，命令的唯一实现在 `gates.sh`，见 `docs/24-W0-CI.md`）：

```text
  #  gate               exit   time  result
  ①  fmt                   0     0s  PASS
  ②  build                 0    14s  PASS
  ③  clippy                0     6s  PASS
  ④  clippy-test-util      0     6s  PASS
  ⑤  test                  0     9s  PASS
  ⑥  db                    0    21s  PASS  (migrate=0,e2e=0)
  ⑦  route-parity          0     0s  PASS
  overall: PASS — 7/7 gate(s) green in 56s
```

计数与重构前基线（空 `target/` 冷跑 179s）**逐条一致**，说明改 manifest 没有改变任何编译/测试语义：

| 指标 | 基线（重构前） | 本次（重构后） |
| --- | --- | --- |
| 门 ⑤ 测试 target 数 / passed / failed / ignored | 45 / 233 / 0 / 43 | 45 / 233 / 0 / 43 |
| 门 ⑥ DB e2e passed（12 个 target） | 77 / 0 failed | 77 / 0 failed |
| 门 ⑦ route-parity | `upstream 456 (commit f41fae6b08fb)` / `local 139` | 同 |

工作区成员：`before n=19, after n=19, IDENTICAL`（§2.2）。
`Cargo.lock`：仅 §5.2 的 1 行删除。

---

## 8. 复现命令

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_INCREMENTAL=0                 # 磁盘紧张时必开：incremental 目录会撑爆（实测 os error 28）

# 1) 成员集合对比（改 members 后必跑）
cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys;print(sorted(p["manifest_path"] for p in json.load(sys.stdin)["packages"]))'

# 2) 依赖审计（只读，永远 exit 0）
python3 scripts/audit_workspace_deps.py
python3 scripts/audit_workspace_deps.py --json | python3 -m json.tool

# 3) 判"重复版本是否真在编译图里"
cargo tree -i tower@0.4.13                # 报 did not match ⇒ 默认图不含

# 4) 七道门（⑥ 需要真 PG；库必须已存在，schema 由 gates.sh 自己迁移）
MULTICA_TEST_DATABASE_URL='postgres://user:pass@127.0.0.1:5432/multica_test' bash scripts/gates.sh --with-db

# 5) 本次改动过的那个 e2e 单独真跑（⑤ 会静默跳过，见 §9）
MULTICA_TEST_DATABASE_URL='postgres://…' cargo test -p mc-http --test smoke
```

---

## 9. 顺带发现（不在本切片范围，留给后续）

`tests/smoke.rs` 的 `workspace_member_http_e2e` 是**需要数据库的用例**，但它（a）在门 ⑤ 里因为
`gates.sh` 用 `env -u MULTICA_TEST_DATABASE_URL` 剥掉了库变量而**提前 return**（打印 skip 后算 pass），
（b）不是 `#[ignore]`，所以在门 ⑥ 的 `-- --ignored` 下被 `2 filtered out`。**结果：它的断言体在 CI 里从不执行**
（只有本文件 §5.2 那种手工带库单跑才会真的走完）。

这正是 `plan1.md` §4.6 想用 `testcontainers` 根治的"DB 测试静默跳过"。
修法有两种（都超出本切片的文件范围：`scripts/gates.sh` 属 W0-A 的 CI 切片）：

- 把该用例改成 `#[ignore]`（让门 ⑥ 覆盖它，语义也诚实）；或
- 门 ⑤ 不再全局剥库变量，改为在测试内部显式判断。

已在本切片的 issue 评论里记一条，建议由 CI/测试基建切片统一处理（避免和 `docs/24` 的命令矩阵打架）。
