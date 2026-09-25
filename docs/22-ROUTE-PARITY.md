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

**判定口径（`LUM-1580` 起）**：handler 表达式的**函数名**含 `placeholder` 或 `not_implemented`
即判为占位 —— `scripts/route_parity.py` 的 `PLACEHOLDER_HANDLER = re.compile(r"\b(?:placeholder|not_implemented)\b")`。
本仓实测存在的两个族：

| handler | 响应 | 位置 |
| --- | --- | --- |
| `health::placeholder` | **200** + body `{"code":"not_implemented"}`（**不是** 501；`health.rs` 的函数注释写 501 是笔误） | `crates/mc-http/src/routes/health.rs` |
| `not_implemented` | 501 + body `error.code = "not_implemented"` | `crates/mc-http/src/routes/issues/mod.rs` |

M0/M1 留下的占位也是"已注册"。计数行因此把 `implemented` 拆成 **real + placeholder** 两段；
`local_only` 列表里也逐条标 `[placeholder]`。只有 real 那部分才算真的实现了合同。

**判空看名字，所以有一个已知边界（1 条，实测、非人工清单）**：**路由专属**的 501 stub
（函数名不含上述词）仍被计进 `implemented_real`。当前全仓唯一一条：
`POST /api/comments/{commentId}/sub-issues` → `crates/mc-http/src/routes/comments/mod.rs` 的
`create_comment_sub_issue`。复算（`crates/mc-http/src` 里 body 带 501/`not_implemented` 标记的
函数应恰好 3 个：`placeholder` / `not_implemented` / `create_comment_sub_issue`）：

```bash
python3 - <<'PY'
import re, glob, sys
sys.path.insert(0, 'scripts'); import route_parity as rp
for p in sorted(glob.glob('crates/mc-http/src/**/*.rs', recursive=True)):
    src = open(p, encoding='utf-8').read(); masked, _ = rp.mask_rust(src)
    for m in re.finditer(r'\bfn\s+(\w+)', masked):
        b = masked.find('{', m.end()); e = rp.matching_paren(masked, b)
        if b > 0 and re.search(r'NOT_IMPLEMENTED|"not_implemented"', re.sub(r'//[^\n]*', '', src[b:e])):
            print(m.group(1), p)
PY
```

⇒ 用 `implemented_real` 讲进度时，**扣掉 `implemented_placeholder` 之后还要再扣这 1 条**。

**保留了另一条修法**：不靠名字、按 handler 是否 501 占位判定（issue 备选 ②）能把上面那条也收进来，
但会**再改一次读数**（`implemented_real` 再 −1）⇒ 必须先落本片的口径，再单独派一片，否则本片 DoD 的
读数与预测表无法对齐。

**已过期的代码注释（本片未改，写集只含 `scripts/` + docs）**：`crates/mc-http/src/routes/issues/mod.rs:53`、
`crates/mc-http/src/routes/mount.rs:281`、`crates/mc-http/src/routes/issues/wakeups.rs:13` 仍写
「占位正则只认 `\bplaceholder\b`」—— 修完后这句不再成立，下次动这三个文件时顺手更正。

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

### 3.6 占位口径修订后的当轮读数（`LUM-1580`，base `fd6c4aa6`）

第七道门在 `LUM-1580` 之前把 `not_implemented` 算成真实现 ⇒ `implemented_real` 虚高。
同一条命令、同一棵树，唯一差异是 `PLACEHOLDER_HANDLER` 那一行：

| 读数 | 修订前（只认 `placeholder`） | 修订后 |
| --- | ---: | ---: |
| `local` | 405 | 405 |
| `implemented` | 329 | 329 |
| `implemented_real` | **329** | **325** |
| `implemented_placeholder` | 0 | **4** |
| `known_gap` | 127 | 127 |
| `unclaimed` / `regressions` | 0 / 0 | 0 / 0 |
| `local_only` | 9 | 9 |
| `local_only_placeholder` | 1 | 2 |

被改判的 4 条上游键（两次 `--json` 取差集，逐键可复算）：`GET /api/issues/{id}/attachments`（M3+）、
`GET /api/issues/{id}/pull-requests`（M8）、`GET /api/issues/{id}/timeline`（M9）、
`POST /api/issues/{id}/comments/trigger-preview`（M2-A）；另加 1 条 local-only
（`GET /api/issues/:id/quick-actions`）。`implemented + known_gap = 456` 全程成立。

**历史数字不要互相转抄**：`231→218`（13 条）是 `8521544` 时点的口径，`329→325`（4 条）是本片时点；
`docs/15` §9.7 的「23 条」更早（M3 6 + M5 7 + M2-A 4 + M2-D 3 + M8/M9/M3+ 各 1）。三个数字是同一件事的三时点。

**基线不动**：`docs/fixtures/route-parity-baseline.json` 记的是「曾经注册过」的键集合（= `local`），
与 handler 怎么分类无关 ⇒ 本片一次都没跑 `--write-baseline`（下一次归 M6-INT `LUM-1675`）。

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

### 4.5 M2 集成后的基线刷新（LUM-1354，2026-09-22 实测）

上面那份基线（71 条）是在 **T1 自己的基点 `9c57592`** 上生成的。M1-E（LUM-1362）在 T1 之后做了
**3 条有意退役 + 1 条改名**，所以五条切片并入 `feat/multica-rs-initial` 后，本命令会按 §4.4 报 4 条
`regression`（exit 1）：

| 基线里有 | 集成后现状 | 依据 |
| --- | --- | --- |
| `POST /api/auth/cli-token` | 改名为 `POST /api/cli-token` | 上游 `router.go:1628` 就是 `/api/cli-token`（`docs/fixtures/upstream-routes.tsv`） |
| `POST /api/auth/login`、`GET /api/auth/session` | 已删除 | M0 幽灵占位，上游无此二路由（`docs/17-M1-CONTRACT-GAPS.md` §3） |
| `POST /api/workspaces/:param/invitations` | 已删除 | 上游只有 `GET`(1667) + `DELETE`(1706)，没有 `POST` |

处理照 §4.4 表格的「有意退役」那一行：先删/改路由（已由 M1-E 完成并合入），再
`python3 scripts/route_parity.py --write-baseline` 接受新集合，把决定写进 diff。刷新后实测：

```
upstream 456 (commit f41fae6b08fb) | local 139 registered | baseline 139
  implemented  111 real +  13 placeholder =  124 / 456   known_gap  332   unclaimed    0   regression   0   local_only   12
  note: same route with and without trailing slash (legal; folded in comparison): ...
OK: every upstream route is either implemented or owned        # exit 0
```

基线文件 diff = **+72 新增 / −4 退役**，`old − new` 恰好等于上表 4 条 ⇒ **没有一条路由是在集成时被误删的**
（“新增路由 → 绿”这条也顺带得到一次真实复核）。集成期**唯一**的基线动作就是这个，
完整执行记录见 `docs/21-M2-INTEGRATION-RECIPE.md` §10.5。

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

## 7. M2-A 尾-补：`POST /api/issues/{id}/squad-evaluated`（LUM-1793）

### 7.1 归属：把一条「兜底命中」变成「显式裁决」

这条键是 `known_gap` 里**唯一**没有 issue 承接的一条，而且它的 owner 单元格不是任何切片
计划做的裁决 —— 它是本文件 §4.2 那张规则表的**兜底行**按 first-match-wins 命中的产物
（`docs/37-M3-W3C-PREFLIGHT.md` §78 的四路取证：fixture 单元格 → 规则表 → 代码侧
`grep` + `git log --all -S` → 全项目 issue 文本，四路全部落空）。

本片的处置**不是**改 fixture 的第 3 列，而是给规则表补一条**带理由的显式规则**，
插在兜底行 `^/api/issues` **之前**：

```
^/api/issues/[^/]+/squad-evaluated	M2-A	squad leader 判决记录（LUM-1793；handler 在 squad.go，键挂在 /api/issues/{id} 下）
```

理由（也就是这一行的「显式裁决」内容）：

| 问题 | 答案 |
| --- | --- |
| 为什么归 `M2-A` 而不是新 owner | handler 在 `squad.go`，但**注册键**挂在 `/api/issues/{id}` 下，且落库写的是 M2-A 面已经在读的 `activity_log`（`GET /api/assignee-frequency` 的数据源之一）⇒ 与 M2-A 尾片（LUM-1691）同一族 |
| 为什么补规则而不是改单元格 | `docs/15-M3-PLAN.md` §536 禁的是**为凑数**改 owner；显式规则让「谁负责、凭什么」对**下一次重生成**也成立（fixture 的显式单元格只保护自己那一行，规则还保护同一类键） |
| 会不会顺手改到别人 | 实测：只对 `squad-evaluated` 生效。`/api/issues/{id}/timeline`(M9)、`/attachments`(M3+)、`/pull-requests`(M8)、`/wakeups`(M5)、`/api/issues/table/*`(M2-D) 与兜底键 `/api/issues/{id}` 的 first-match **逐条不变**（`docs/63` §6 第 4 条留下的那条键就此关闭） |
| fixture 要重生成吗 | **不需要**。快照只填**空白** owner 单元格，这一格本来就有 `M2-A`（生成时命中的是兜底行，结果相同）⇒ `docs/fixtures/upstream-routes.tsv` 一个字节未动，`gen_upstream_routes.py --check` 的漂移与本事无关 |

### 7.2 路由清单（1 条键 / 1 个注册点）

| 上游键 | `router.go` | handler | 上游注册形态 | 本仓注册点 |
| --- | --- | --- | --- | --- |
| `POST /api/issues/{id}/squad-evaluated` | 2097 | `squad.go:976 RecordSquadLeaderEvaluation` | **plain**（`r.Post`，不是 `Route(…)+Post("/")`） | `crates/mc-http/src/routes/squad_evaluations.rs` → `/api/issues/:id/squad-evaluated` |

**只注册一个形态**：上游这一条是 `r.Post("/api/issues/{id}/squad-evaluated", …)`，不走 chi 的
`Mount` ⇒ 补一条尾斜杠形态是 `EXTRA_ALIAS` 缺陷、漏字面量是 `MISSING_EXACT`，两类都是硬失败，
而本波 `docs/fixtures/slash-alias-allowlist.tsv` 是 0 数据行、**没有豁免退路**。落地后用
`python3 scripts/slash_alias_audit.py --no-allowlist` 验过：`shapes OK`、`0 defect(s)`。

**落点纪律**：`mount.rs` 尾部追加 1 行 `.merge(mount_slice_squad_evaluation())` + 1 个
`mount_slice_*` 函数，`routes/mod.rs` 追加 1 行 `pub mod squad_evaluations;` —— 两边都是
**纯追加**，`issues/` 目录里的既有文件一个未动（与 `LUM-1691` 的追加段同款边界）。

### 7.3 ⑦ 计数（本片实测，base `dacad392`）

| 读数 | 本片前 | 本片后 | Δ |
| --- | ---: | ---: | ---: |
| `local` | 447 | **448** | +1 |
| `implemented` | 364（360 real + 4 ph） | **365**（361 real + 4 ph） | +1 |
| `known_gap` | 92 | **91** | −1 |
| `owners.M2-A` | **1** | **0**（该分组从板子上消失） | −1 |
| `unclaimed` / `regression` / `local_only` | 0 / 0 / 9 | 0 / 0 / 9 | 0 |

不变式 `implemented + known_gap = 456` 成立；`slash_alias_audit.py` 的上游键字面量 451 → **452**。
**`local` 只 +1**（1 条键 = 1 个注册点）：这条键不要求双形态，与 §2.1 的「`local` 数注册点」一致。

⚠️ **本片未跑 `--write-baseline`**（`baseline` 保持 406，文件未动）：基线刷新只归
M7-21 `LUM-1786` / M8-7 `LUM-1804`。

### 7.4 与上游的偏离（逐条可查）

| # | 偏离 | 性质 | 为什么 |
| --- | --- | --- | --- |
| D1 | **错误信封**：本仓 `{"error":{code,message}}`（且 message 带 thiserror 前缀，如 `validation error: …`），上游是扁平 `{"error": msg}` | 全仓既有约定 | 状态码与英文文案**逐字**对齐（`outcome must be 'action', 'no_action', or 'failed'` / `task does not belong to issue` / `only the squad leader agent can record evaluations` / `task is not a squad leader task` / `leader task has no squad_id` / `squad not found` / `failed to record evaluation`），只有信封是本地形状 |
| D2 | **actor 解析只有一条分支**：上游 `resolveActor`（`handler.go:847`）是三条（`X-Actor-Source: task_token` 盖章 / `X-Agent-ID`+`X-Task-ID` 自校验 / 其余 = member），本仓只实现**第二条** | **能力缺口**（见 §7.5） | 本仓 `/api/issues*` 面**没有** task-token 中间件（`AuthUser` 恒人类成员）。第一条分支在上游成立靠的是 Auth/DaemonAuth 中间件「剥掉客户端头再盖章」；本地没有那层，照抄等于给**任何成员**一个自封 agent 的开关 —— 比第二条（会自己查 agent 行 + task 行校验）**更弱** ⇒ 有意不实现 |
| D3 | **无 realtime 发布**：上游成功后 `h.publish(EventActivityCreated, …)`（`squad.go:1120`；事件常量本地已有：`mc-daemon-proto/src/events.rs:163`） | **能力缺口**（与 `docs/63` §6 第 1 条同款） | M2 面整波没有 realtime 发布通道，本片不单开一条依赖边。**接线点**：落库成功后、201 之前；投递面 = `workspace_id` + `"agent"` + **调用者（= task 的 agent）** id；payload = `{"issue_id", "entry": {type:"activity", id, actor_type:"agent", actor_id, action, details, created_at}}` |
| D4 | **抑制查询无消费者**：判决行的 `actor_id` 照上游放 **`task.agent_id`**（不是 `squad.leader_id`），但 `HasSquadLeaderNoActionEvaluationForTask`（上游 `activity.sql:35`）对应的 service 面**未移植** ⇒ 「`no_action` 抑制 leader 评论」本地不生效 | **能力缺口** | 上游靠它避免 leader 用评论代替判决；本仓没有那个消费者。**但列的取值不能省**：局部索引 `089_squad_no_action_activity_index`（`(issue_id, actor_id, details->>'task_id')` WHERE `actor_type='agent' AND action='squad_leader_evaluated' AND details->>'outcome'='no_action'`）**已在 head schema 上**，放成 leader id 会让将来的消费者查不到那一行。repo 测试用照抄索引谓词的 `EXISTS` 查询钉住了这一列 |
| D5 | **task 行投影收窄**：上游 `GetAgentTaskInWorkspace` 是 `SELECT atq.*`（49 列），本仓只取 handler 读的 5 列（`id` / `agent_id` / `issue_id` / `is_leader_task` / `squad_id`） | 等价实现 | 谓词（`atq.id = $1 AND a.workspace_id = $2`）与 `JOIN agent` 逐字相同 —— 那个 join 才是租户闸门，收窄的只是投影宽度 |
| D6 | **500 不泄漏 DB 文案**：上游写不透明文案，本仓**有意不用** `Error::Database(message)`（后者会把 SQL 细节放进响应体） | **本片收紧**（唯一一处收紧） | 保持 `failed to record evaluation` 这个语义；DB 故障只进日志 |
| D7 | 时间戳格式：上游 `time.RFC3339`（UTC → `…Z`），本仓 `DateTime::to_rfc3339()`（UTC → `…+00:00`） | 全仓既有约定 | `labels.rs` / `inbox.rs` / `pins.rs` 等全部这样，本片不单开第二种格式 |

**权限粒度**（问题描述点名要登记的一项）：本地这一端是 **dev-mode 语义** ——
`X-Agent-ID` + `X-Task-ID` 都是**客户端可写**的头，只要调用者是 workspace 成员就能造出
一对合法的（上游注释自己写明这张回退「**不是**安全边界」：两个 id 都能从
`GET /api/issues/{id}/task-runs` 读到）。本片**保持闸门 1 / 闸门 2 的判定与顺序**
（它们决定的是「谁**有资格**写这条判决」，而不是「谁**是**那个 agent」），
`order` 用例把顺序这条安全判据钉住了：越过闸门 1 之前，任何拒绝都不回显 task 派生的 issue id。

### 7.5 本地做不到的那件事（如实登记，不要读成「已实现」）

**本仓没有密码学意义的 agent 身份边界。** 上游那条端点的安全模型是
「中间件剥头 + 盖章 + 活体 leader 判定」三层；本地只有第三层，前两层属 M3-7 的
daemon / agent-run 面（`docs/41-M3-6-TASK-QUEUE.md` 的 originator / task-token 平面）。
⇒ 这一端在本地与上游的 **dev-mode 回退分支同级**：可测、语义正确、但不是防伪的。
等 daemon 面的 task-token 中间件落地后，只需把 `resolve_agent_actor` 换成真正实现
（读服务端盖的章），**handler 里的 13 步判定一行都不用改**。
