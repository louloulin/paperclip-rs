# 22 - 路由对账与盲区归属（一条命令）

> 立项：**LUM-1376**（tooling）。实测时间：**2026-09-22 20:20 CST**。
> 实测基线：本仓 `feat/multica-rs-initial` @ **`9c57592`**；上游 `louloulin/multica` main @
> **`f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`**（`server/cmd/server/router.go`）。
>
> 本文件有两类内容，读的时候不要混：
> - **§3 是实测快照**（数字会随切片并入而变化，集成时重跑即得新值）；
> - **§1–§2、§4–§7 是规程**（怎么跑、两侧数据从哪来、owner 怎么改、快照怎么刷），不随快照过期。

---

## 1. 一条命令

```bash
python3 scripts/route_parity.py                       # 板子：谁还没被认领
python3 scripts/route_parity.py --list-gaps           # 附：按 owner 分组的全部缺口路由
python3 scripts/route_parity.py --json                # 机读（CI / 看板消费）
python3 scripts/route_parity.py --quiet               # 只留计数行
python3 scripts/route_parity.py --write-baseline       # 本仓新增/退役路由后，接受当前
                                                      # 注册集合为新的"不得丢失"基线
python3 scripts/route_parity.py --no-baseline          # 只出板子，关掉丢失门禁
```

只依赖 Python 3 标准库（`argparse/glob/json/os/re/sys/collections/dataclasses`），**不联网**：上游那一半
是仓库里的快照文件 `docs/fixtures/upstream-routes.tsv`，本仓那一半是源码静态抽取。

退出码（可直接当门禁用）：

| 码 | 含义 |
| --- | --- |
| `0` | 每条未实现的上游路由都有 owner，没有路由从基线里消失，两侧都无 `(method, path)` 重复，所有本地注册都能解析 |
| `1` | 存在 **unclaimed**（未实现且无 owner 的盲区），或存在 **regression**（基线里有、源码里没了），或本地/上游重复注册，或有无法解析的本地注册 |
| `2` | 参数/文件错误（fixture 不存在、`--routes-dir` 不存在、`--write-baseline` 遇到无法解析的注册等） |

输出五组：`implemented`（上游∩本地）、`known_gap`（上游有、本地无，**按 owner 分组**）、
`unclaimed`（= `known_gap` 里 owner 为空的那部分，是真正的盲区板）、`regression`（基线里有、
现在源码里没了，见 §4.4）、`local_only`（本地有、上游无，是需要"保留还是退役"的自觉决定，不计入失败）。

## 2. 数据两侧分别从哪来

| 侧 | 来源 | 抽法 |
| --- | --- | --- |
| 本仓 | `crates/mc-http/src/**/*.rs`（`--routes-dir`） | 掩码掉注释/字符串后按括号配平扫 `.route(<字面量>, <method chain>)`，整个 `method(...)` 链都要，见 §2.1 |
| 上游 | `docs/fixtures/upstream-routes.tsv`（`--upstream`） | 已由 `scripts/gen_upstream_routes.py` 生成好，见 §5 |
| 基线 | `docs/fixtures/route-parity-baseline.json`（`--baseline`） | 上一次 `--write-baseline` 记下的本仓注册集合，只用来抓"路由消失"，见 §4.4 |

### 2.1 为什么必须整链抽取，而不是"抽路径去重"

只按 path 做 `uniq -d` 会产生大量**假重复**：同一个路径注册多个方法（`GET`+`POST`）或
`.route("/api/inbox", …)` 与 `.route("/api/inbox/", …)` 并存都是合法的。实测本仓 71 条里有 **16 组**
这样的碰撞（全部合法，见 §3.3）。对账的主键是 `(method, path)`；只有 `(method, path)` **完全相同**
才是真正的重复（axum 0.7 的 `.merge` 遇到它会直接 panic，见 `docs/21` §6）。

### 2.2 静态抽取的能力边界（看不到就**报错**，不猜）

抽取器只认字面量路径。以下写法会被记为 `!! unresolved registrations` 并让命令以 `1` 退出——这是
故意的：宁可让人来看一眼，也不要静默漏掉路由。当前基线实测为**空**。

| 写法 | 行为 |
| --- | --- |
| `.route(concat!("/api/", x), …)` / 任何非常量表达式路径 | unresolved |
| `.route(path, make_method_router())`（第二参数不是 `get/post/...` 调用链） | unresolved |
| `.route(path, …)` 参数个数不是 2 | unresolved |
| `.route` 之外的路由 API（`nest` / `nest_service` / `route_service` / `fallback_service`） | 不识别，以常量表 `OTHER_ROUTE_APIS` 记录为已知限制；本仓实测未使用 |
| `any(handler)` | 识别为 method `ANY`：该 path 上任意方法都算命中（本仓实测未使用） |
| `#[cfg(test)] mod tests` 内的 `.route` | 排除（测试用的临时路由不算对外契约） |
| path 参数写法 | `:id` 与 `{id}` 都归一为 `:param`（**只比位置，不比参数名**）；axum 0.7 必须写 `:id`，写 `{id}` 会被当字面量段（恒 404，`docs/09` §7.4） |
| 尾斜杠 | `(method, path)` 的**注册**键保留尾斜杠（`/api/inbox` ≠ `/api/inbox/`，axum 就是这么注册的）；两侧**对账**时才把尾斜杠折叠（chi 的 `/x` 与 `/x/` 是同一 handler），折叠结果以 `note:` 行明示 |

### 2.3 `placeholder` 不等于 implemented

M0/M1 留下的占位（`health::placeholder`，返回 501）也是"已注册"。计数行因此把
`implemented` 拆成 **real + placeholder** 两段；`local_only` 列表里也逐条标 `[placeholder]`。
只有 real 那部分才算真的实现了合同。

## 3. 实测快照（`9c57592` vs 上游 `f41fae6`）

### 3.1 板子

```
$ python3 scripts/route_parity.py
upstream 456 (commit f41fae6b08fb) | local 71 registered | baseline 71
  implemented   43 real +  13 placeholder =   56 / 456   known_gap  400   unclaimed    0   regression   0   local_only   14
  gaps by owner: M3=98  M6=55  M2-A=47  M4=39  M9=34  M5=27  M8=25  M7=24  M3+=17  M1=9  M2-E=9  M2-B=8  M10=5  M2-D=3
  note: same route with and without trailing slash (legal; folded in comparison): GET /api/inbox + /api/inbox/
  ...
OK: every upstream route is either implemented or owned        # exit 0
```

| 指标 | 值 | 说明 |
| --- | --- | --- |
| 上游 `(method, path)` | **456** | 含 `/v1` 插件面 9 条 × 2 前缀 = 18 条（常量路径被内联展开） |
| 本仓注册 | **71** | 58 个 `.route(` 调用展开而来；**重复 0**；基线 71（与注册数相等 = 无丢失） |
| 其中占位 | 19 | 13 条与上游重合，6 条只在本地（`auth/login`、`auth/session`、`runtimes` POST、`plugins` GET/POST、`feature-flags`） |
| implemented | **56** 条上游路由 = **43 real + 13 placeholder** | |
| known_gap | **400** | 全部有 owner |
| **unclaimed** | **0** | 这是本命令的门禁指标之一 |
| **regression** | **0** | 基线 71 条一条没少；有丢失时见 §4.4 |
| local_only | 14 = 8 real + 6 placeholder | 见 §3.4 |

`known_gap` 400 条的 owner 分布即 §4.3 的表；`--list-gaps` 会按 owner 逐条列出。

### 3.2 71 条 / 零重复（验收要求）

```
$ python3 scripts/route_parity.py --json | python3 -c 'import json,sys; r=json.load(sys.stdin); print(r["counts"]["local"], r["duplicates"]["local"], r["counts"]["upstream"], r["duplicates"]["upstream"], r["counts"]["regressions"])'
71 [] 456 [] 0
```

**上游侧 456 条可独立复核**（不依赖生成器）：`router.go` 里 chi 方法注册
`r.X(...)` 438 条 + `r.With(mw).X(...)` 9 条 = **447**，加上
`registerPluginActionRoutes()`（`router.go:102-112`，9 条）在两个前缀下被各内联一次 = **447 + 9 = 456**。
`/api/labels`（5 条）与 `/api/properties`（4 条）——正是 LUM-1368 cycle 手工对账漏掉的两个目录——
在 fixture 里是 `M2-E`。

### 3.3 同 path 不同 method **不算**重复（验收要求）

按 path 去重会得到 16 组假阳性，按 `(method, path)` 去重是 0 组：

```
/api/agents                  GET + POST          /api/me                       GET + PATCH
/api/autopilots              GET + POST          /api/me/pats                  GET + POST
/api/chat/sessions           GET + POST          /api/plugins                  GET + POST
/api/projects                GET + POST          /api/runtimes                 GET + POST
/api/skills                  GET + POST          /api/squads                    GET + POST
/api/workspaces              GET + POST          /api/inbox                    GET /api/inbox + GET /api/inbox/  ← 尾斜杠
/api/workspaces/:param       GET + PATCH + DELETE
/api/workspaces/:param/{invitations,members,share-links}   GET + POST
```

`/api/workspaces/:id` 的 `GET`+`PATCH`+`DELETE` 是分三个 `.merge(...)` 子树注册的
（`crates/mc-http/src/routes/workspaces.rs:443` / `:454` / `:464`），正是"path-only 去重会误报"的典型。

### 3.4 local_only 14 条：三个**真实的**合同偏离 + 6 个占位

按 `docs/17-M1-CONTRACT-GAPS.md` 的决策，这些是本地自造路径 / 待退役路由，脚本只提示、不判失败：

| 本地路由 | 位置 | 性质 |
| --- | --- | --- |
| `POST /api/auth/cli-token` | `routes/auth.rs:55` | 上游是 `POST /api/cli-token`（D5） |
| `POST /api/workspaces/:id/invitations` | `routes/invitations.rs:37` | 上游无此路由（D3：**待删除**） |
| `GET/POST /api/me/pats`、`DELETE /api/me/pats/:id` | `routes/pats.rs:38-39` | 上游是 `/api/tokens*`（保留为带 `Deprecation` 头的 alias，D4） |
| `GET /api/health`、`/api/health/db`、`/api/openapi.json` | `routes/mount.rs:28-30` | 本仓自有的运维面（上游是 `/healthz`、`/readyz`、`/health` → 归 M10） |
| `POST /api/auth/login`、`GET /api/auth/session`、`POST /api/runtimes`、`GET/POST /api/plugins`、`GET /api/feature-flags` | `routes/mount.rs:36-78` | 占位（501），上游无对应 path |

### 3.5 顺手复核到的一个事实（M1 合同的落点）

`known_gap` 的 9 条 M1 路由里，有 8 条正是 **LUM-1362 / PR #5（`feat/multica-rs-m1e-contract-gaps`）**
新加的实现（`PUT /api/workspaces/{id}`、`/api/workspaces/{id}/members/{memberId}` 的 PATCH/DELETE、
`/api/tokens*`、`POST /api/cli-token`），它们在本 base `9c57592` 上仍是缺口——PR #5 合入后本表会自动
从 9 降到 1（只剩 `POST /auth/google`，Google 登录本仓未移植，仍记 M1）。这不影响本工具的正确性，
但说明**快照要随集成重跑**（§5 的刷新流程）。

## 4. owner 词表与规则表

### 4.1 词表（`docs/fixtures/upstream-routes.tsv` 第 3 列）

| owner | 含义 |
| --- | --- |
| `M1` | M1 切片（基础 + 认证/工作区/PAT），见 `docs/05`–`docs/09` |
| `M2-A` … `M2-E` | M2 五个切片（issue 从属面 / 评论 / inbox+订阅 / 表格视图 / 定义目录），见 `docs/10` §2 |
| `M3` … `M10` | 后续 milestone，见 `docs/01` §6 的 milestone 表 |
| `M3+` | `docs/10` 明确写"M3+，未立项"的部分（quick-actions、`/ws`、`/uploads/*`、`/api/avatars/*` 等）：**还没有 issue**，登记在此就是为了下一次立项时有据可查 |
| `n/a` | 明确**不移植**（有意为之，不是漏项） |

**空 owner = 未认领盲区** → 命令以 `1` 退出。这是本工具存在的意义：任何人新增/上游新增了路由，
只要没人认领，板子就会红。

### 4.2 规则表 `scripts/route-owners.tsv`

`正则<TAB>owner<TAB>理由`，**首次匹配生效**，共 83 条。它只在生成快照时**填空**（见 §5），
不会覆盖 fixture 里已有的 owner 值。

改 owner 的两种方式，按场景选：

- **只改个别路由** → 直接改 `docs/fixtures/upstream-routes.tsv` 第 3 列。此后重新生成也不会被覆盖
  （生成器只填空白），diff 里一眼能看到是谁改的。
- **改一类路由** → 改 `scripts/route-owners.tsv` 对应规则的理由/归属。规则改完重生成，**只影响
  仍是空白**的格子；已被显式写过的格子不变，需要同步时手动改。

### 4.3 当前 owner 分布（`known_gap` 400 条）

| owner | 条数 | owner | 条数 | owner | 条数 |
| --- | --- | --- | --- | --- | --- |
| M3 | 98 | M5 | 27 | M2-E | 9 |
| M6 | 55 | M8 | 25 | M2-B | 8 |
| M2-A | 47 | M7 | 24 | M10 | 5 |
| M4 | 39 | M3+ | 17 | M2-D | 3 |
| M9 | 34 | M1 | 9 | | |

### 4.4 基线：已实现的路由不得悄悄消失（`--write-baseline`）

`unclaimed` 只能回答"还没做的上游路由有没有主"，回答不了"**已经做了的**路由是不是被删了"：
一条已实现的上游路由被删掉后，fixture 只知道它归某个 milestone（owner 非空），于是它退化成
`known_gap`，命令仍然是绿的 —— 这就是一次静默的合同丢失。所以本仓侧还有一份自己的记忆：
`docs/fixtures/route-parity-baseline.json`（`--write-baseline` 生成，git 里版本化）。

| 行为 | 结果 |
| --- | --- |
| 新增路由 | **绿**（基线只做子集检查，本项目的日常就是加路由） |
| 删除/改名一条已注册的路由 | **红**（`regression` 列出 `METHOD path`，exit 1） |
| 有意退役路由（如 D3 的 `POST /api/workspaces/:id/invitations`） | 先删路由，再 `--write-baseline` 接受新集合，把"退役"这个决定写进 diff |
| 只想看板、不要丢失门禁 | `--no-baseline` |

基线用**不折叠尾斜杠**的注册键（`/x` 与 `/x/` 在 axum 里是两个不同的注册），因为这里问的是
"源码里这条注册还在不在"，而不是"两侧语义是否等价"。

负例自证（LUM-1376 验收，临时删 `workspaces.rs` 的 `GET /api/workspaces/:id`，不提交）：

```
$ python3 scripts/route_parity.py --quiet
upstream 456 (commit f41fae6b08fb) | local 70 registered | baseline 71
  implemented   42 real +  13 placeholder =   55 / 456   known_gap  401   unclaimed    0   regression   1   local_only   14
  !! 1 route(s) present in the baseline are gone from the source (a lost contract, not a gap):
     GET    /api/workspaces/:param
     fix by restoring the route; if the removal is deliberate, accept it with `python3 scripts/route_parity.py --write-baseline`
FAIL                                              # exit 1
$ git checkout -- crates/mc-http/src/routes/workspaces.rs    # 恢复
$ python3 scripts/route_parity.py --quiet
upstream 456 (commit f41fae6b08fb) | local 71 registered | baseline 71
  implemented   43 real +  13 placeholder =   56 / 456   known_gap  400   unclaimed    0   regression   0   local_only   14
OK: every upstream route is either implemented or owned    # exit 0
```

同一次运行里那一条也以 `known_gap owner=M1 router.go:1664` 出现（`known_gap` 400→401，M1 计数 9→10），
即"报成 known_gap"和"退出码非零"两份证据对得上。

## 5. 刷新上游快照（上游 main 变了 / 本轮集成后）

```bash
# 1) 浅克隆（约 5s；配方见 docs/20 §1）。切勿在 --filter=blob:none 的 blobless 仓库里跑 git grep
git clone --depth 1 --single-branch --branch main https://github.com/louloulin/multica /tmp/up-multica

# 2) 重新生成 fixture（owner 列会被保留；--commit 记录快照 sha）
python3 scripts/gen_upstream_routes.py \
  --router  /tmp/up-multica/server/cmd/server/router.go \
  --const-file /tmp/up-multica/server/pkg/publicapi/v1/routes.go \
  --owner-rules scripts/route-owners.tsv \
  --out docs/fixtures/upstream-routes.tsv \
  --commit "$(git -C /tmp/up-multica rev-parse HEAD)"

# 3) 幂等/漂移检查（不改文件；fixture 与当前上游不一致时 exit 1）
python3 scripts/gen_upstream_routes.py \
  --router  /tmp/up-multica/server/cmd/server/router.go \
  --const-file /tmp/up-multica/server/pkg/publicapi/v1/routes.go \
  --owner-rules scripts/route-owners.tsv \
  --out docs/fixtures/upstream-routes.tsv \
  --commit "$(git -C /tmp/up-multica rev-parse HEAD)" --check

# 4) 对账
python3 scripts/route_parity.py
```

生成器会展开 `r.Route("<prefix>", …)` / `r.Group(…)` 前缀、内联 `registerPluginActionRoutes(...)`
（`/v1` 与 `/api/plugin-bridge/v1` 两个前缀各 9 条），并把 `publicapiv1.*` 常量从
`--const-file` 解析成字面量；新出现的路由若匹配不到 `route-owners.tsv` 任何规则，owner 就留空 →
第 4 步立刻以 `1` 退出，提示人来决定归属。fixture 头部记录了生成命令、上游 sha、克隆配方与 owner 语义。

本切片**新增/退役了本仓路由**时，上游快照不用动，但要接受新的本仓基线（否则第 4 步会按 §4.4 报
`regression`）：

```bash
python3 scripts/route_parity.py --write-baseline   # 有意增删本仓路由后跑一次；diff 里只有 baseline 文件变
```

## 6. 明确不做的事

- **不比对行为**：本工具只比 `(method, path)` 的存在性。请求/响应体、状态码、权限、错误码这些合同
  差异属于 e2e 契约测试（`crates/mc-http/tests/` + `docs/17`），不要用本工具替代。
- **不做运行时验证**：纯静态。真正"路由能不能被 axum 装进 Router"的证据是 build/`cargo test`
  （重复 `(method, path)` 会在 `Router::merge` 处 panic，见 `docs/21` §6）。
- **不判断优先级**：owner 只回答"谁负责"，不回答"何时做"；排期看 `docs/01` §6 与各 issue。
- **基线只记"注册集合"**：它不知道 handler 是真是占位、是否还在返回 501；那属于占位清偿的看板，
  不在本工具里（本工具只把 `placeholder` 标出来，见 §2.3）。
