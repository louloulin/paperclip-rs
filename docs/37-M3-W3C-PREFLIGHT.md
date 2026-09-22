# M3 W3c 预飞：daemon 面 / execenv+adapters 写集实测与 ws 传输层切片可行性

> 编制：编程助手devbox5（LUM-1437）｜实测基线：`feat/multica-rs-initial` @ `a09789d`（issue 指定的起点）；**当前 base head 已前进到 `7888cf3`，两者的数字必须在 §2.6 里分开读**｜协议权威：上游 `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`
> 本文件只回答「W3c 能不能切、怎么切、切之前必须先做什么」，**不含任何实现**。所有数字都由 §8 的命令实测得到；与 `docs/35`/`docs/36` 同构。

---

## 1. 结论

1. **锚点已接好，W3c 不需要碰 `mount.rs`** —— `/api/daemon*` 的 slice 在 `mount.rs:96` 已 merge，`mount_slice_daemon()`（`mount.rs:205`）已是 `super::daemon::router()`。实测把真实路径挂进 `routes/daemon.rs` 即可经全局 router 命中（§2.2）。这与 `docs/15` §7.2 的「4 个 route stub + 4 个 mount.rs merge」一致。
2. **W3c 有三个真 anchor 缺口**（§3.1）：`mc-repos/src/daemon.rs` 与其 `pub mod` **未预置**；`mc-http/Cargo.toml` 缺 `mc-daemon-proto`/`mc-task`（`mc-ws` 另缺 `mc-daemon-proto`/`uuid`）；文档里「需先删 M0 占位」的 3 条**都不在 W3c 写集**（占位属 M3-4/M3-5）。
3. **ws 传输层可切出一个 0 路由 0 DB 的预切片，且无需等任何人** —— 已据此预建 **LUM-1439**（`docs/37` §4 给出写集、依赖、反证）。它应排在 M3-7 之前，是 W3c 串行链上唯一「现在就能开工」的一段。
4. **M3-8 的 25 项里只有 16 项能由冻结规则定族**，其余 8 项（copilot/opencode/codearts/deveco/openclaw/dsh/grok/antigravity）必须逐项定并写依据；`docs/15` §6 的 8/8/9 批划分**不按协议族**，建议**保留批边界、批内按族分组**（§5.3）。
5. **门 ⑦ / ⑨ 都不会因为 W3c 写集而需要刷新** —— 实测：加真实路由后 ⑦ 仍 `exit 0`（`unclaimed 0`、`regression 0`，基线数字不动），⑨ 打印 `report matches crates/mc-conformance/report.json`（§2.2、§8）。
6. **基线口径**：issue 里写的 ⑦ `122/136/11`、⑨ `unmounted 6 / placeholder 0` **是对的**，但属于**当前 base head `7888cf3`**，不是 issue 指定的起点 `a09789d` —— 中间 `8895abe` 已把 W3b 的 anchor 预删落底（删 `/api/agents`+`/api/runtimes` 两条占位）并同步刷了 ⑦ 基线 + ⑨ 快照。**W3c 派发请按 `7888cf3` 的口径**（§2.6）。
7. **前置实况**：M3-1/M3-2/M3-3（W3a 三片）**已合**（实测均为 HEAD 祖先）；`W0-B2`（LUM-1387）仍 `todo` 但**有一个 active run**（状态与 run 不一致，§3.3）。

---

## 2. 锚点接线与真路径探针（实测）

### 2.1 `mount.rs` 现状（行号为本仓实测，非文档引用）

| 位置 | 内容 | 归属 |
| --- | --- | --- |
| `mount.rs:49` | `.route("/api/agents", get|post(health::placeholder))` | M3-5（**需先删**） |
| `mount.rs:53` | `.route("/api/runtimes", …)` | M3-4（**需先删**） |
| `mount.rs:57` | `.route("/api/chat/sessions", …)` | M4 |
| `mount.rs:61` / `:65` / `:69` / `:73` / `:77` | `/api/skills`、`/api/plugins`、`/api/autopilots`、`/api/squads`、`/api/projects` | M6/M5/M4 |
| `mount.rs:80` | `.route("/api/feature-flags", get(health::placeholder))` | M10 |
| `mount.rs:93` | `.merge(mount_slice_agent())` | M3-5 |
| **`mount.rs:96`** | **`.merge(mount_slice_daemon())`** | **M3-7（已接好）** |
| `mount.rs:187` / **`:205`** | `fn mount_slice_agent()` / **`fn mount_slice_daemon()`** → `super::daemon::router()` | — |

- `mount.rs` 里 `health::placeholder` 共 **9 处**（17 个 method 注册）；**没有一处属于 W3c**。
- **行号漂移**：`docs/15` §1.8 写「锚点在 `mount.rs` 44–47 / 50–53」，实测已是 49 / 53（上方 M0 注释与 M1 切片 merge 增加过行）。同理 `docs/15` §1.8 的 `routes/issues.rs:2213` 那个 501 stub 现已随 R7 拆到 `routes/issues/mod.rs:194`。

### 2.2 探针：真 daemon 路径能经全局 router 命中（临时文件，跑完即删）

配方：在 `crates/mc-http/src/routes/daemon.rs` 临时加两条路由（`GET /__m3probe/daemon` + 真实的 `POST /api/daemon/deregister`），在 `crates/mc-http/tests/` 加临时测试，跑完 `git checkout --` 还原。

```
running 5 tests
test anchor_daemon_slice_reachable_via_global_router ... ok
test control_unregistered_probe_path_is_404 ... ok
test daemon_real_path_is_mounted ... ok
test real_router_over_tcp_socket_roundtrips ... ok
test daemon_min_loop_register_claim_start_progress_complete ... ok
test result: ok. 5 passed; 0 failed
```

四条断言各自的意义：

| 断言 | 结论 |
| --- | --- |
| `anchor_daemon_slice_reachable_via_global_router` | `mount_slice_daemon()` 的 merge 真的生效（不是死代码） |
| `control_unregistered_probe_path_is_404` | 对照组：没注册的路径确实是 404 ⇒ 上一条不是「全 200」的假绿 |
| `daemon_real_path_is_mounted` | **`POST /api/daemon/deregister` 这条真实路径可以在 daemon.rs 里直接注册，不会与任何 M0 占位冲突**（该路径无占位） |
| `real_router_over_tcp_socket_roundtrips` | 走真实 TCP（`TcpListener` + `axum::serve`，M1 手法）能往返 ⇒ 端到端链路成立 |

**⑦（门 route-parity）在同一次探针下的实测**：

```
upstream 456 (commit f41fae6b08fb) | local 142 registered | baseline 139
  implemented  113 real +  13 placeholder =  126 / 456   known_gap  330   unclaimed    0   regression   0   local_only   13
OK: every upstream route is either implemented or owned
⑦_EXIT=0
```

对照基线（干净树）：`local 140 registered | implemented 112 real + 13 placeholder = 125 / 456 | known_gap 331 | local_only 12`，同样 `exit 0`。

⇒ **加路由只动 `local_only`（12→13）与 `known_gap`（331→330），`unclaimed`/`regression` 不动，⑦ 不需要刷基线**（基线只拦 unclaimed/regression）。

**⑨（门 conformance）在同一次探针下的实测**：

```
report matches crates/mc-conformance/report.json
⑨_EXIT=0
```

⇒ 挂上真实 daemon 路径**不会**造成 ⑨ 快照漂移。原因是唯一那条 daemon fixture 的 actor 是 `member`，而 stateless tier 只支持 `anonymous`（`mc-conformance/src/lib.rs:948` 的 `run_tier` + `Tier::Stateless::supports`），**不回放**就判 `unevaluable`；也就是说它测不了不是因为「路由没挂」，而是因为「没库」。**M3-7 不要跑 `--write-baseline` / `--write`。**

### 2.3 占位 handler 的真身：**200**，不是 501

`crates/mc-http/src/routes/health.rs:52-53` 的文档注释写「返回 501」，但函数体只有 `Json(...)`、**没有 `StatusCode`** ⇒ 实际返回 **HTTP 200**，body 为：

```json
{"code":"not_implemented","message":"this route is reserved; implementation lands in a later milestone"}
```

- `docs/15` 里「幽灵 501」的说法对这批 M0 占位**不准确**（它们是 200 + 标记字段）。
- 门 ⑦ 判定占位靠的是 **handler 名**（`route_parity.py` 的 `PLACEHOLDER_HANDLER = re.compile(r"\bplaceholder\b")`），与状态码无关。
- 对 W3c 的实务含义：**验收时不能靠「非 200」判断占位**，要看响应体 `code`。

### 2.4 预删配方（W3c 无预删项，但规则要留档）

**规则**：axum 0.7 对 **同 path+同 method 重复注册会 panic**；M0/M2 占位已经占了它们的路径，所以**任何切片实现一条已被占位注册的路径时，必须在同一个 commit 里删掉占位那几行**。

实测（临时测试，跑完即删）：

```
running 3 tests
test distinct_methods_on_same_path_ok ... ok        # 同 path 不同 method：允许
test duplicate_method_on_same_path_panics ... ok    # 同 path+method：panic
test duplicate_path_registration_panics ... ok      # 同 path：panic
test result: ok. 3 passed; 0 failed
```

`mount.rs:186-191` / `:196-203` 的注释已把这条写成切片纪律（引 `docs/15` §9.6.2）。

**查自己的预删项**：

```bash
grep -n 'health::placeholder' crates/mc-http/src/routes/mount.rs        # 9 处，逐个判断归属
grep -rn 'not_implemented' crates/mc-http/src/routes/                  # M2 那 6 条 stub 的现址
```

**W3c 的实测结论：预删项 0 个。** 9 处占位分别属 M3-4（`/api/runtimes`）、M3-5（`/api/agents`）、M4/M5/M6/M10；`/api/daemon*` 与 `/api/runtimes/{runtimeId}/*` 没有被占位注册（探针实测 `POST /api/daemon/deregister` 无冲突）。

### 2.5 基线口径：`a09789d`（issue 起点）vs `7888cf3`（当前 base head）

issue 让从 `a09789d` 起分支，所以本片所有实测都在 `a09789d` 上做；但 base 分支随后前进了两步：

```
a09789d  merge(#24) R7 拆分 + #19 校验                    ← issue 指定的起点（本片实测基线）
8895abe  chore(m3-anchor): W3b anchor 预删（删 /api/agents + /api/runtimes 占位，刷 ⑦ 基线 + ⑨ 快照）
7888cf3  docs(36): 03:30 cycle 落地记录（LUM-1435）        ← 当前 base head
```

两套数字（同一套命令，只是换 commit）：

| 口径 | ⑦ | ⑨ totals | `mount.rs` 占位数 |
| --- | --- | --- | --- |
| **`a09789d`（issue 起点，本片 §2.2/§2.3 实测）** | `local 140 / baseline 139 / implemented 125 (112 real+13 placeholder) / known_gap 331 / local_only 12 / unclaimed 0 / regression 0` | `pass 4 / mismatch 1 / **unmounted 5** / **placeholder 1** / unevaluable 47` | 9（含 `/api/agents`、`/api/runtimes`） |
| **`7888cf3`（当前 base head，临时 worktree 实测）** | `local 136 / baseline 136 / implemented 122 (112 real+10 placeholder) / known_gap 334 / local_only 11 / unclaimed 0 / regression 0` | `pass 4 / mismatch 1 / **unmounted 6** / **placeholder 0** / unevaluable 47` | 7（两条已被 `8895abe` 删除） |

⇒ **issue 里给出的预期值（122 / 136 / 11、unmounted 6 / placeholder 0）不是错的，而是属于 `7888cf3`**。本片没有改用后者的原因很实际：issue 指定从 `a09789d` 起分支，而预删本身**改变不了 W3c 的结论** ——

- W3c 的预删项在**两个口径下都是 0**（被删的两条属 M3-4/M3-5）；
- 锚点（`mount.rs:96` / `mount_slice_daemon()`）在**两个口径下都已接好**（`8895abe` 只删占位行，未动 merge）；
- ⑦/⑨ 的不变性结论在两个口径下同样成立（加真实路由只动 `local_only`/`known_gap`，⑨ 只把 `placeholder 1 → unmounted 1`）。

**给 M3-7 的口径纪律**：晋升与验收引用 ⑦/⑨ 数字时，**必须写清是哪一版 base**（`8895abe` 之后 baseline 从 139 变 136、⑨ 快照也变过）；`docs/15`/本篇 `a09789d` 的数字只在 `a09789d` 上成立。本片没有刷新任何基线/快照（那是 cycle/集成的活）。

### 2.5b 回退证据（预飞不留代码）

```
$ git status --porcelain
（空）
$ grep -c "__m3probe" crates/mc-http/src/routes/daemon.rs   # 无匹配
$ git diff --stat        # 无输出
```

`daemon.rs` 已还原为 scaffold 空切片（`pub fn router() -> Router<Arc<AppState>> { Router::new() }`）。

---

## 3. 写集矩阵与三个 anchor 缺口

### 3.1 缺口

| # | 缺口 | 实测证据 | 谁必须补 |
| --- | --- | --- | --- |
| **A** | `crates/mc-repos/src/daemon.rs` **不存在**，`lib.rs` **也没有** `pub mod daemon;` | `ls crates/mc-repos/src/` → `{agent,comment,inbox,invitation,issue,issue_status,issue_table,member,pat,runtime,share_link,subscriber,task,user,verification_code,workspace}.rs`；`lib.rs` 只声明了其中 13 个 | M3-7（**要改他人也会碰的共享 `lib.rs`** ⇒ 必须排在 W0-B2 之后） |
| **B** | 依赖未预声明：`mc-http/Cargo.toml` **没有** `mc-daemon-proto`、**没有** `mc-task`；`mc-ws/Cargo.toml` **没有** `mc-daemon-proto`、**没有** `uuid` | `grep -c 'mc-daemon-proto\|mc-task' crates/mc-http/Cargo.toml` → `0`；`mc-ws` 现有 deps = serde/serde_json/thiserror/anyhow/tracing/tokio/tokio-tungstenite/futures-util/axum/mc-realtime/mc-core | M3-7（或先由 ws 预切片吸收一部分） |
| **C** | 「需先删 M0 占位」的 3 条**不在 W3c**（且其中两条已由 base 的 `8895abe` 预删落底） | `a09789d` 上 `GET|POST /api/agents`（`mount.rs:49`→M3-5）、`GET /api/runtimes`（`:53`→M3-4）；`7888cf3` 上已删（占位只剩 7 处）；探针实测 daemon 路径在两个口径下都无占位冲突 | 无（W3c 预删项 0） |

> 依赖 delta 会写 `Cargo.lock`，而 `Cargo.lock` 是**共享面**（`docs/15` §7.5 规定 rebase 规则）。**把这次 delta 放在 ws 预切片（LUM-1439）里先落地**，M3-7 就只需 rebase，冲突面最小。

### 3.2 写集矩阵

| 切片 | 写集 | 不写（易误判） |
| --- | --- | --- |
| **LUM-1439** ws 传输层预切片 | `crates/mc-ws/src/{hub,identity,frames}.rs`、`crates/mc-ws/src/lib.rs`（仅 `pub mod`）、`crates/mc-ws/Cargo.toml`、`crates/mc-ws/tests/hub_*.rs`、`Cargo.lock`、`docs/38` | 不挂路由、不写 handler、不碰 `mc-ws` 既有 `/live-events` 语义、不碰 `mc-daemon-proto`、不碰基线/快照 |
| **LUM-1438** M3-7 daemon 面 | `crates/mc-http/src/routes/daemon.rs` → `routes/daemon/{register,heartbeat,claims,tasks,requests,gc}.rs`、`routes/runtimes.rs`（仅表 B 的 8 条）、`crates/mc-repos/src/daemon.rs`（新建）+ `lib.rs`（1 行）、`crates/mc-http/Cargo.toml`、`crates/mc-daemon/src/**`（客户端侧）、`crates/mc-http/tests/daemon_*.rs`、`Cargo.lock`、`docs/32` | 不碰 `mount.rs`（已接好）、不碰 ⑦ 基线、不碰 ⑨ 快照、不做 execenv/adapters、不做 cloud-runtime |
| **LUM-1440** M3-8 execenv | `crates/mc-daemon/src/execenv/**`、`crates/mc-daemon/src/lib.rs`（1 行）、`crates/mc-daemon/Cargo.toml`、`crates/mc-daemon/tests/execenv_*.rs`、`docs/33` | 同一 crate 与 M3-7 共享 `Cargo.toml`/`lib.rs` ⇒ **串行** |
| **LUM-1441/1442/1443** M3-8 a/b/c | `crates/mc-runtime/src/adapters/<key>/**`、`crates/mc-runtime/src/registry.rs`、`crates/mc-runtime/src/catalog.rs`、`crates/mc-runtime/src/adapters/mod.rs`、`crates/mc-runtime/tests/**`、`docs/33` | 0 路由；三批**串行**改同一批共享文件 |

### 3.3 共享面与并发位

| 共享面 | 持有者 | 对 W3c 的约束 |
| --- | --- | --- |
| `Cargo.lock` | 任何加依赖的切片 | 一次落地（建议放进 LUM-1439），其余 rebase（`docs/15` §7.5） |
| `crates/mc-repos/src/lib.rs` | W0-B2 / M3-4/5/6 | M3-7 的 +1 行必须排在 W0-B2 **之后** |
| `crates/mc-http/Cargo.toml` | M3-4/5/6/7 | 加 dep 时按 rebase 处理 |
| `docs/fixtures/route-parity-baseline.json` | M3 集成 cycle | 切片**不得** `--write-baseline`（实测 W3c 也不需要） |
| `crates/mc-conformance/report.json` | M3 集成 cycle | 切片**不得**重生成（实测 W3c 不需要） |
| `crates/mc-http/src/routes/runtimes.rs` | M3-4 与 M3-7 | 表 B 的 8 条与 M3-4 的 9 条同文件 ⇒ **M3-4 先合** |

**前置实况（实测）**：`M3-1`（freeze `0ed37b5` / commit `0f93c55`）、`M3-2`（`59b89e6`）、`M3-3`（`418c2b0`）**都是 HEAD 的祖先** ⇒ W3a 已全部落地。`W0-B2`（LUM-1387，`01a0c94e-0798-79c3-b176-78b4d38d980e`）状态 **`todo`**、assignee 空，但 `multica issue get` 显示**有 1 个 active run**（`01a0ca84-7670-74eb-8aab-e3dad1412d60`）⇒ **状态与 run 不一致，请 owner 确认**：若它已在跑，W3c 的晋升判据应按「run 完成并合入」而不是「状态变 in_review」。

### 3.4 文档编号登记（只登记，不改既有裁决）

`docs/15` §3 现登记的编号里 `27`/`31` 已被别的文档占用（`docs/27` = W0 golden fixtures 已在；`docs/39` 已被 M3-6 切片文本占用）。本片只**追加**两条：

- **`docs/37`** = 本文件（W3c 预飞）；
- **`docs/38`** = ws 传输层预切片文档（若 LUM-1439 派发）。

`32`（M3-7 daemon 面）/`33`（M3-8 adapters）/`34`（M3 集成手册）沿用 `docs/15` §3 原登记，未占用。

---

## 4. ws 传输层缺口与「0 路由 0 DB 预切片」可行性

### 4.1 两个 ws 不是一回事（最容易被合并误判）

| | 用户面实时通道 | **daemon 面 ws** |
| --- | --- | --- |
| 本仓现状 | `mc-ws/src/lib.rs`（102 行）：`/live-events`、`ClientMessage::{Ping,Resume,Pong}`；**未挂载**，`crates/mc-ws/tests/` 为空 | 无实现 |
| 上游 | `/ws`（`router.go:1424`，owner **M3+**） | `/api/daemon/ws`（`router.go:1526`，owner **M3**） |
| 语义 | 用户订阅事件 | per-`runtime_id` 连接注册、身份握手、心跳（服务端主动 ping）、重连/续传、扇出、RPC 请求-应答、eventID 去重、metrics |
| 归属 | M3+（本片**不动**其语义） | **M3-7** |

⇒ M3-7 写的是 `mc-ws` 里的**第二个** hub，不能把 `/live-events` 的 `ClientMessage` 当 daemon 帧用。

### 4.2 M3-7 必须自己实现的部分（上游实测，`hub.go` / `daemon_rpc.go`）

| 能力 | 上游位置 | 说明 |
| --- | --- | --- |
| 连接注册表 | `hub.go:312`（`type Hub`）、`:335`（`NewHub`）、`:439` 注册 | 按 `runtime_id` 索引，含 `byRuntime` 等映射 |
| 身份 | `hub.go:412` `HandleWebSocket(w, r, identity)` | identity **由调用方注入**（`ClientIdentity{DaemonID,UserID,PrimaryWorkspaceID,Capabilities,ClientVersion}` + `middleware.WithDaemonContext`） |
| 心跳/超时 | `hub.go:17-19`、`:945/947`、`:1129`、`:1138/1148` | `writeWait=10s`、`pongWait=60s`、`pingPeriod=54s`；**服务端主动 ping + 超时踢线** |
| in-flight 限流 | `hub.go:298-300`、`:436` | `maxInFlightRPCPerClient = 8`，`rpcSem` |
| 通知 API | `hub.go:446/452/458/466/474` | `NotifyTaskAvailable` / `NotifyRuntimeProfilesChanged` / `NotifyWorkspacesChanged` / `NotifyPendingWork` / `NotifyRuntimeGone` |
| 去重 | `hub.go:548` `invalidateRuntime` | 按 `eventID` 判 delivered/deduped |
| 精确投递 | `hub.go:594` `DeliverDaemonRuntime` | 按 runtime 投递 |
| RPC 分发 | `daemon_rpc.go:44-90` | `rpcResponseCapture`（内存 `http.ResponseWriter`）+ 派发到**同一个 HTTP handler**；错误文本 `unknown rpc method %q` |
| 状态映射 | `hub.go:1007-1040` | ≥8 ⇒ 429；handler 不可用 ⇒ 503；handler status <400 ⇒ 500；否则透传 |
| 帧参数 | `mc-daemon-proto/src/rpc.rs:58-73`（**已冻结**） | 上述 5 个数值常量 + 4 个状态码常量 |

### 4.3 可行性结论：**可行**（0 路由、0 DB、0 前置等待）

**为什么能切开**：hub 只依赖「连接 + 身份 + 帧」，**不解析 token、不查库**（identity 由调用方注入，上游同样如此）；44 条 HTTP 路由的业务类型不必出现在传输层 ⇒ 两者写集不重叠。唯一共享的是 `Cargo.toml`/`Cargo.lock`，可一次落地。

**预切片写集**：`crates/mc-ws/src/{hub,identity,frames}.rs` + `lib.rs`（`pub mod`）+ `Cargo.toml`（`mc-daemon-proto`、`uuid`）+ `crates/mc-ws/tests/hub_*.rs` + `Cargo.lock` + `docs/38`。

**依赖**：只依赖 **M3-1（协议冻结）**，实测已是 base 祖先 ⇒ **可立即派发**；不需要 W0-B2、不需要 M3-3、不需要 M3-7 先合。

**反证（什么情况下不成立）**：如果 hub 要自己查库校验 token、或要按 task 领域的类型做投递决策，就必须等 W0-B2 + M3-3，切分即失败。实测**两者都不需要**：上游把 identity 作为函数参数传入（`hub.go:412`），投递只带 `runtime_id` + 帧。

**已实测的可行性证据**（临时回话，跑完即删）：

```
running 6 tests
test rpc_roundtrip_matches_http_leg ... ok
test http_leg_bytes_equal_ws_rpc_body_bytes ... ok
test reconnect_after_close_works ... ok
test transport_auto_pongs_client_ping ... ok
test unknown_method_returns_404 ... ok
test concurrent_rpcs_are_correlated_by_request_id ... ok
test result: ok. 6 passed; 0 failed
```

其中 `http_leg_bytes_equal_ws_rpc_body_bytes` 是**跨端契约守门人**：手写 HTTP/1.1 走 `TcpStream` 拿到的响应体，与 ws RPC 帧的 `body` 字段**逐字节相同** —— 这正是 `docs/16` 与上游 `rpcResponseCapture`（`daemon_rpc.go:44`）所要求的「同一 handler、两个调用方」。另外 `transport_auto_pongs_client_ping` 证实了那条坑：**axum 会自动回 client 的 Ping**，所以服务端主动 ping 与 pongWait 踢线**必须自己写**。

### 4.4 交付序

```
LUM-1439（ws 传输层，现在可派，0 前置）
        ↓ 必须先合
W0-B2（LUM-1387，schema 切换，已在进行）
        ↓
LUM-1438（M3-7：44 条 + 消费 hub）
        ↓
LUM-1440（execenv，同 crate 串行） / LUM-1441 → 1442 → 1443（adapters 三批串行）
```

---

## 5. M3-8 分批可行性（execenv 99 文件 + 25 adapters）

### 5.1 25 项的 launch header 与定族实况（逐字实测自 `catalog.rs:131-157`）

| # | `AgentType` | key | launch header | 协议族 | 来源 |
| ---: | --- | --- | --- | --- | --- |
| 1 | `Claude` | `claude` | `claude (stream-json)` | `StreamJson` | 冻结规则 |
| 2 | `Codebuddy` | `codebuddy` | `codebuddy (stream-json)` | `StreamJson` | 冻结规则 |
| 3 | `Codex` | `codex` | `codex app-server` | `AppServer` | 冻结规则 |
| 4 | `Copilot` | `copilot` | `copilot (json)` | **待定** | 规则未覆盖 |
| 5 | `Opencode` | `opencode` | `opencode run (json)` | **待定** | 规则未覆盖 |
| 6 | `Codearts` | `codearts` | `codearts run (json)` | **待定** | 规则未覆盖 |
| 7 | `Deveco` | `deveco` | `deveco run (json)` | **待定** | 规则未覆盖 |
| 8 | `Openclaw` | `openclaw` | `openclaw agent (json)` | **待定** | 规则未覆盖 |
| 9 | `Hermes` | `hermes` | `hermes acp` | `Acp` | 冻结规则 |
| 10 | `Pi` | `pi` | `pi (json mode)` | `JsonLine` | **M3-2 已交付** |
| 11 | `Cursor` | `cursor` | `cursor-agent (stream-json)` | `StreamJson` | 冻结规则 |
| 12 | `Kimi` | `kimi` | `kimi acp` | `Acp` | 冻结规则 |
| 13 | `Reasonix` | `reasonix` | `reasonix acp` | `Acp` | 冻结规则 |
| 14 | `Dsh` | `dsh` | `dsh --profile multica (stdio)` | **待定** | 规则未覆盖 |
| 15 | `Kiro` | `kiro` | `kiro-cli acp` | `Acp` | 冻结规则 |
| 16 | `Antigravity` | `antigravity` | `agy -p (non-interactive)` | **待定** | 规则未覆盖 |
| 17 | `Qoder` | `qoder` | `qodercli --acp` | `Acp` | 冻结规则 |
| 18 | `QoderCliCn` | `qoderclicn` | `qoderclicn --acp` | `Acp` | 冻结规则 |
| 19 | `TraeCli` | `traecli` | `traecli acp serve` | `Acp` | 冻结规则 |
| 20 | `Grok` | `grok` | `grok agent stdio` | **待定** | 规则未覆盖 |
| 21 | `Qwen` | `qwen` | `qwen -p (stream-json)` | `StreamJson` | 冻结规则 |
| 22 | `QwenPaw` | `qwenpaw` | `qwenpaw acp` | `Acp` | 冻结规则 |
| 23 | `Mcode` | `mcode` | `mcode acp` | `Acp` | 冻结规则 |
| 24 | `Dim` | `dim` | `dim acp` | `Acp` | 冻结规则 |
| 25 | `Zeroclaw` | `zeroclaw` | `zeroclaw acp` | `Acp` | 冻结规则 |

直方图：**规则覆盖 16**（`Acp` 11 + `StreamJson` 4 + `AppServer` 1）、**已定 1**（`Pi`）、**待定 8**。

⇒ M3-8 除了「写 24 个 adapter」，还必须**为 8 项定族并写依据**（`adapter.rs:346-350` 的注释只给了三条映射）；`Opaque` 是默认兜底，不能当作「已定」。

### 5.2 已交付的共享设施（单位成本的答案）

| 设施 | 位置 | 复用方式 |
| --- | --- | --- |
| `ProtocolFamily` / `AdapterCapabilities` / `RuntimeAdapter` trait | `mc-runtime/src/adapter/{traits.rs,handle.rs}`、`adapter.rs`（690 行） | M3-8 只实现，不改契约 |
| `AgentType::ALL: [Self; 25]` + `as_str` | `catalog.rs:65` | 注册与断言的数据源 |
| `AgentType → ProtocolFamily` 映射 | **不存在**（`grep` 实测） | **M3-8 必补**（§5.4） |
| 一致性套件宏 `adapter_conformance!` | `conformance.rs:525`，**内含 8 个 `#[tokio::test]`** | 每个 adapter 一行接入，8 个测试白送 |
| `FakeCli`（`version/replay/failing/sleeping/replay-sleep` 脚本） | `conformance.rs:88` | 免装真二进制 |
| pi-local 参考实现 | `adapters/pi_local/{args,run,stream,sanitize,mod}.rs`（共 2422 行）+ `crate::adapter_conformance!(PiLocal)`（`mod.rs:589`） | 新 adapter 的模板 |
| 注册表 `with_builtin_adapters` | `registry.rs:50`（现状**只有 `PiLocal`**） | M3-8 逐批加条目 |

⇒ **单个 adapter 的边际成本 ≈「一份 args/run/stream + 注册 + 定族」**，8/8/9 的批划分在**工作量**上可行（每批含 1 个新协议族的首个实现时偏重）。

### 5.3 批划分与建议（保留 `docs/15` 的 8/8/9，批内按族分组）

| 批 | `docs/15` §6 名单 | 族构成 | 备注 |
| --- | --- | --- | --- |
| 批 1（LUM-1441） | claude, codex, copilot, opencode, codebuddy, codearts, deveco, **pi** | `StreamJson`×2 + `AppServer`×1 + 待定×4 + **已完成×1** | `pi` 已由 M3-2 交付 ⇒ **实际新增 7**。批内建议顺序：`StreamJson`（claude, codebuddy）→ `AppServer`（codex）→ 待定 4 |
| 批 2（LUM-1442） | cursor, kimi, kiro, antigravity, qoder, qoderclicn, traecli, grok | `Acp`×5 + `StreamJson`×1 + 待定×2 | 先做 `Acp` 骨架（5 项共享）再填差异 |
| 批 3（LUM-1443） | qwen, qwenpaw, openclaw, hermes, reasonix, dsh, dim, mcode, zeroclaw | `Acp`×6 + `StreamJson`×1 + 待定×2 | 收口：断言 25 项全部有 adapter + 确定族 |

**建议（咨询性，不改变 `docs/15` §6 的裁决）**：批边界不动（它是权威），但**批内先做同族的第一项、再批量填同族**，可把复制粘贴面压到最小 —— 批 1 的「4 个待定」与批 3 的「6 个 `Acp`」是最值得这么做的两处。

### 5.4 两个缺口

1. **`AgentType → ProtocolFamily` 映射函数不存在**（`grep` 实测；`registry.rs` 只到 adapter 实例，`AdapterCapabilities.protocol` 由各 adapter 自述）⇒ M3-7 的 hub 能力协商、M3-4 的 runtime 台账若要按族决策，都会依赖这张表。**建议在批 1 落地**（含未定项的显式 `Opaque` + 注释「待 8b/8c 定」）。
2. **`#[ignore]` 无「缺二进制」先例**：实测全仓 `#[ignore]` 理由**全是** DB 相关（58 条 `requires MULTICA_TEST_DATABASE_URL`、31 条 `needs a real PostgreSQL via MULTICA_TEST_DATABASE_URL`、3 条中文 DB 理由、1 条 `needs MULTICA_TEST_DATABASE_URL`），**`mc-runtime` 自身 0 条**。所以「真二进制缺失就 skip」这条约定在 M3-8 是**首例**：要用就得在 `docs/33` 明写（并解释为什么 `FakeCli` 覆盖不了该用例），否则后人会把 `#[ignore]` 当 DB 专用标记。

### 5.5 上游 clone 漂移（数字引用纪律）

| 对象 | `docs/15` 的引用 | 本机实测 | 说明 |
| --- | --- | --- | --- |
| 上游 commit | `f41fae6b`（协议冻结 SHA） | 本机 clone 为 `90e0bdf`（浅克隆，**无** `f41fae6b`） | 一切**契约**数字以 `docs/16` / `mc-daemon-proto` 的冻结值为准 |
| `handler/daemon.go` | 6056 行 | **10583 行** | clone 是移动靶；行号只能当线索 |
| `internal/daemonws/`（非测试） | 1370 行 | **1370**（hub 1154 + metrics 60 + notifier 156）✓ | 与 `docs/15` 一致 |
| `handler/daemon_ws.go` / `daemon_rpc.go` / `daemon_workspace.go` | — | 127 / 90 / 87 ✓ | — |
| `internal/daemon/execenv/` | 99 文件 | **99** ✓ | M3-8 execenv 的体量依据 |

⇒ 引用上游行号时**必须连同 clone commit 一起写**（§8 给了复算方式）。

---

## 6. daemon 回路 e2e 配方实测（M3-7 的验收硬项）

### 6.1 配方（M3-7 照抄）

1. **stub daemon**：`TcpListener::bind("127.0.0.1:0")` + `tokio::spawn(async move { axum::serve(listener, app).await })`（M1 手法，现成范例：`crates/mc-http/src/routes/auth.rs:1466`）。
2. **被测面**：用 `mc_http::routes::router(state)`（全局 router，**不是**单独拼 daemon slice）⇒ 同时验证 merge 生效与中间件链。
3. **驱动**：`reqwest`（已是 `mc-http` 常规依赖）按顺序打 5 个真实路径：`POST /api/daemon/register` → `POST /api/daemon/tasks/claim` → `POST /api/daemon/tasks/{taskId}/start` → `POST /api/daemon/tasks/{taskId}/progress` → `POST /api/daemon/tasks/{taskId}/complete`，并**断言调用顺序**。
4. **无需改 `Cargo.toml`**（探针全程未动依赖）。

### 6.2 实测输出

```
test daemon_min_loop_register_claim_start_progress_complete ... ok
```

探针里记录并断言的顺序为 `["register", "claim", "start", "progress", "complete"]`（顺序错即失败）。全套探针 5 passed / 0 failed，见 §2.2。

### 6.3 M3-7 要固化的测试文件（本片只给清单，不实现）

| 文件 | 内容 |
| --- | --- |
| `crates/mc-http/tests/daemon_register.rs` | 注册/注销/身份校验/重复注册 |
| `crates/mc-http/tests/daemon_heartbeat.rs` | 心跳 HTTP 面 ack（`{status} ∪ pending_*`，**与 ws 面不同**，`docs/16` §10.1） |
| `crates/mc-http/tests/daemon_claims.rs` | claim / `tasks/claim` / `prepare-lease` / `pending`；`idx_one_pending_task_per_issue` 冲突 ⇒ **409** |
| `crates/mc-http/tests/daemon_task_events.rs` | start / progress / complete / fail / cancel-ack / messages / session / wait-local-directory / usage |
| `crates/mc-http/tests/daemon_requests.rs` | 表 B 的 8 条异步往返 + 4 条 `*/result` 回填 |
| `crates/mc-http/tests/daemon_gc.rs` | 5 条 `gc-check` + `recover-orphans` |
| `crates/mc-http/tests/daemon_min_loop.rs` | §6.1 的最小回路（**验收硬项**） |
| `crates/mc-ws/tests/hub_*.rs` | 见 LUM-1439（预切片交付；M3-7 只消费） |

### 6.4 ws RPC 复用 HTTP handler 的实测

见 §4.3：`http_leg_bytes_equal_ws_rpc_body_bytes`（HTTP 响应体 == ws RPC `body`，逐字节）、`unknown_method_returns_404`、`concurrent_rpcs_are_correlated_by_request_id`、`reconnect_after_close_works`、`transport_auto_pongs_client_ping`。

---

## 7. 预建 backlog issue 清单

全部挂在 epic **LUM-1334**（`01a0c737-bc08-7c1e-a417-e4b562774c20`）下，`status=backlog`、`priority=high`、assignee = 本 agent（与 W3b 预建的 M3-4/5/6 同构），**创建时不启动 run**。

| issue | 内容 | 写集主体 | 前置（晋升判据） | 文档 |
| --- | --- | --- | --- | --- |
| **LUM-1439** | ws 传输层预切片（0 路由 0 DB） | `mc-ws/src/{hub,identity,frames}.rs` + tests + Cargo | **无**（M3-1 已合）⇒ 可立即派 | `docs/38` |
| **LUM-1438** | M3-7 daemon 面 44 条 + ws 服务端 | `routes/daemon/*` + `mc-repos/src/daemon.rs` + `mc-ws` 消费 + `mc-daemon` 客户端 | W0-B2 合 + M3-1/3 合（已满足）+ **M3-4 先合** + LUM-1439 先合；开工前 rebase 到当日 base head（口径见 §2.5） | `docs/32` |
| **LUM-1440** | M3-8-p0 execenv（99 文件） | `mc-daemon/src/execenv/**` | LUM-1438 合（同 crate 串行） | `docs/33` |
| **LUM-1441** | M3-8-a adapters 批 1（新增 7） | `mc-runtime/src/adapters/**` + registry + 族映射 | LUM-1438 合 | `docs/33` |
| **LUM-1442** | M3-8-b adapters 批 2（8 项） | 同上（串行） | LUM-1441 合 | `docs/33` |
| **LUM-1443** | M3-8-c adapters 批 3（9 项，收口 25） | 同上（串行） | LUM-1442 合 | `docs/33` |

每个 issue 正文都带：路由清单（含上游行号）/ 写集 / 前置 / 上游硬约束 / 测试清单 / 门禁与交付证据 / 范围限制 / 晋升条件 / 分支与基线。

**推荐派发序**：`LUM-1439` → `LUM-1438` → （`LUM-1440` 与 `LUM-1441` 可并行）→ `LUM-1442` → `LUM-1443`。

---

## 8. 复算命令（每条结论都能重跑）

```bash
export PATH="$HOME/.cargo/bin:$PATH"                 # 本机 rustup stable 1.98.1

# ── 0. 基线与前置 ────────────────────────────────────────────────
git log -1 --format='%H %s'                          # a09789d …（issue 起点）
# 口径对照（§2.5）：base head 已到 7888cf3；临时 worktree 实测另一套数字
git fetch -q origin feat/multica-rs-initial && git rev-parse --short origin/feat/multica-rs-initial
git log --oneline a09789d..origin/feat/multica-rs-initial | cat   # 8895abe / 7888cf3
git worktree add -q --detach /tmp/base1437 origin/feat/multica-rs-initial
( cd /tmp/base1437 && python3 scripts/route_parity.py | head -2 )   # local 136 / implemented 122 (112+10)
git worktree remove /tmp/base1437 --force
git merge-base --is-ancestor 0ed37b5 HEAD && echo "M3-1 landed"
git merge-base --is-ancestor 59b89e6 HEAD && echo "M3-2 landed"
git merge-base --is-ancestor 418c2b0 HEAD && echo "M3-3 landed"

# ── 2.1 锚点行号 ────────────────────────────────────────────────
grep -n '^\s*"/api/' crates/mc-http/src/routes/mount.rs          # 49/53/57/61/65/69/73/77
grep -n 'feature-flags\|merge(mount_slice_daemon\|fn mount_slice_daemon' crates/mc-http/src/routes/mount.rs
grep -c 'health::placeholder' crates/mc-http/src/routes/mount.rs # 9
sed -n '52,60p' crates/mc-http/src/routes/health.rs              # 注释写 501，实现是 200

# ── 2.2 门 ⑦（基线 + 计数）────────────────────────────────────────
python3 scripts/route_parity.py ; echo "⑦_EXIT=$?"
python3 scripts/route_parity.py --json > /tmp/parity.json
python3 -c "import json;d=json.load(open('/tmp/parity.json'));print(d['counts']);print(len(d['known_gap']),len(d['unclaimed']),len(d['regressions']),len(d['local_only']))"

# ── 2.2 门 ⑨（不变性）────────────────────────────────────────────
bash scripts/gates.sh                                # ①–⑤+⑦+⑨+⑩ = 8/8，约 140s（暖 target）
grep -n 'report matches\|已接入路由等价率\|契约等价率\|pass ' <(bash scripts/gates.sh 2>&1) | head

# ── 2.2 探针（临时文件，跑完必须删干净）──────────────────────────
#   crates/mc-http/src/routes/daemon.rs 加 2 条路由 + tests/m3_probe_tmp.rs
#   crates/mc-ws/tests/m3c_ws_rpc_rehearsal_tmp.rs
cargo test -p mc-http --test m3_probe_tmp
cargo test -p mc-ws  --test m3c_ws_rpc_rehearsal_tmp
rm -f crates/mc-http/tests/m3_probe_tmp.rs crates/mc-ws/tests/m3c_ws_rpc_rehearsal_tmp.rs
git checkout -- crates/mc-http/src/routes/daemon.rs
git status --porcelain                                # 必须只剩 docs/

# ── 2.4 预删/冲突失败模式 ───────────────────────────────────────
#   临时 tests/m3c_panictmp.rs：同 path+method 注册应 panic，不同 method 不 panic
cargo test -p mc-http --test m3c_panictmp ; rm -f crates/mc-http/tests/m3c_panictmp.rs

# ── 3.1 缺口 A/B ────────────────────────────────────────────────
ls crates/mc-repos/src/                                # 无 daemon.rs
grep -n 'pub mod' crates/mc-repos/src/lib.rs           # 无 pub mod daemon
grep -c 'mc-daemon-proto\|mc-task' crates/mc-http/Cargo.toml   # 0
grep -n 'mc-daemon-proto\|uuid' crates/mc-ws/Cargo.toml        # 无匹配

# ── 4.2 上游 hub 事实（clone 路径与本机 commit 一并记录）─────────
U=/home/devbox/multica_workspaces/lumos-659117e3ca3d/lum-1355-0d4fadfb4836/workdir/upstream-multica
git -C "$U" log -1 --format='%H'                       # 90e0bdf（非 f41fae6b）
sed -n '17,19p;298,300p;412p;1007,1040p' "$U/server/internal/daemonws/hub.go"
grep -n 'maxInFlightRPCPerClient\|pingPeriod\|pongWait\|writeWait' "$U/server/internal/daemonws/hub.go"
wc -l "$U/server/internal/handler/daemon.go" "$U/server/internal/daemonws/"*.go "$U/server/internal/handler/daemon_rpc.go"
ls "$U/server/internal/daemon/execenv/" | wc -l        # 99

# ── 5.1 25 项 launch header 与族直方图 ──────────────────────────
sed -n '60,70p;125,160p' crates/mc-runtime/src/catalog.rs
grep -n 'pub const ALL' -A 30 crates/mc-runtime/src/catalog.rs | grep -c 'Self::'   # 25
grep -rn 'fn .*[Ff]amily' crates/mc-runtime/src/       # 无 AgentType→ProtocolFamily 映射

# ── 5.2 一致性套件与 FakeCli 单位成本 ────────────────────────────
grep -n 'macro_rules! adapter_conformance\|struct FakeCli\|#\[tokio::test\]' crates/mc-runtime/src/conformance.rs
grep -c 'adapter_conformance!' crates/mc-runtime/src/adapters/pi_local/mod.rs   # 1
grep -c 'with_builtin_adapters' -A 6 crates/mc-runtime/src/registry.rs          # 现状仅 PiLocal

# ── 5.4 #[ignore] 先例调查 ──────────────────────────────────────
grep -rc '#\[ignore' crates/ --include=*.rs | grep -v ':0'
grep -rh -A1 '#\[ignore' crates/ --include=*.rs | grep -o '"[^"]*"' | sort | uniq -c | sort -rn

# ── 7. 预建 issue ───────────────────────────────────────────────
multica issue children 01a0c737-bc08-7c1e-a417-e4b562774c20 --output json | python3 -c "import json,sys;d=json.load(sys.stdin);[print(c['status'],c['id'][:8],c['title']) for c in d['unstaged'] if 'M3-' in c['title']]"

# ── 3.3 W0-B2 状态 vs run ───────────────────────────────────────
multica issue get 01a0c94e-0798-79c3-b176-78b4d38d980e --output json | python3 -c "import json,sys;d=json.load(sys.stdin);i=d.get('issue',d);print(i['status'],i.get('assignee_id'))"
multica issue runs 01a0c94e-0798-79c3-b176-78b4d38d980e --output json 2>/dev/null | head -40
```

---

## 9. 本片没做什么（边界）

- **没有实现任何东西**：`crates/**` 与 `migrations/**` 零改动；所有探针/回话的临时文件已删（§2.5），`git status --porcelain` 只剩 `docs/`。
- **没有改 `docs/15` 的任何裁决**：只追加 `docs/37`/`docs/38` 两条编号登记（§3.4）。
- **没有改门禁**：⑦ 基线 / ⑨ 快照 / `scripts/**` 未动（实测不需要动，§2.2）。
- **没有派发**：6 个 issue 全部 `backlog`，创建时不启动 run；晋升判据写在各自正文里。
- **没有做 M3-4/5/6（W3b）的事**：M0 占位删除、501 stub 原地替换都在那边；本片只给出「W3c 无预删项」的实测结论。
- **没有验真库**：本片是文档切片，⑥/⑧ 两门未跑；M3-7 的 `--with-db` 验收在其自己的 issue 里。
