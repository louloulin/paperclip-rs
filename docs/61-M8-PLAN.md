# docs/61 — M8（W8 代码与制品面：GitHub App / VCS 连接 / MCP 服务器库 / composio）切片计划

**状态**：M8 计划片（`LUM-1796`）交付物。M8 代码切片已按本文建为 `LUM-1796` 的子 issue（**LUM-1797 … LUM-1804**，全部 `backlog`，见 §7.3），
**待 M6 收口（`LUM-1673` M6-8 合 + `LUM-1675` M6-INT 合）+ M7-0（`LUM-1765`）合入后晋升**。

**上游口径**：`multica` @ **`f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`**（= `docs/fixtures/upstream-routes.tsv` 记录的那个 commit）。
只读副本本次克隆在 **`LUM-1796` 自己的 workdir**（`up-m8/`，`git fetch --depth 1 origin <sha>` + `checkout FETCH_HEAD`，
因为上游 `main` 的 tip 是 `90e0bdf`、不是本表钉的那个 sha），**不依赖 `/tmp/ups*` 或别的 run 的 workdir**（`LUM-1674` 亲眼见过跨 workdir 副本被 GC）。

**本地基线**：`paperclip-rs` @ **`ec83f6e7`**（`origin/feat/multica-rs-initial`，= `LUM-1791` cycle 的 §78 docs 提交）。
本文所有 ⑦/⑨/⑩ 读数都在 `ec83f6e7` 上测得。

**文档编号**：`58` 已被 `LUM-1675`（M6-INT）预留、`59` = `docs/59-M2-E-LABEL-PROPERTY.md`、`60` = `docs/60-M7-PLAN.md`
⇒ 本文取 **`61`**（起手已 `git fetch origin feat/multica-rs-initial` + `ls docs/ | sort` 复核，实测最大号为 60）。

> 本文对 `docs/plan1.md` 有 **7 处口径修订**（§9），其中三处是**承重的**：
> ① `W8 after w5`（不是 after w7）**成立** ⇒ M8 与 M7 **并行**（§9.3 给了单值结论与槽位表）；
> ② `mcp-servers` 8 条的**账归 M8、能力面属 W6**（§9.4）；
> ③ `docs/15` 把 attachments 面判给「W8/M8」而 fixture 写 `M3+` ⇒ **维持 `M3+`**，附件面登记为 W8 尾账（§9.2）。
> 其余承接 `docs/57-M6-PLAN.md` / `docs/60-M7-PLAN.md` 的骨架与全部记法纪律。

---

## 0. 结论速览

| 项 | 结论 |
|---|---|
| 本波路由 | **25 条**（GitHub 7 + VCS 5 + MCP 8 + composio 5）。其中 **24 条是 `known_gap`**，1 条（`GET /api/issues/{id}/pull-requests`）**已在本地注册为 `not_implemented` 占位** |
| 本波上游体量 | **6,626 行 / 10 文件**（handler 7 文件 **3,780** 行 + `internal/integrations/{vcs,ghsnapshot,composio}` 9 个非测试 `.go` **2,846** 行；逐文件见 §1.2） |
| 本波新迁移 | **0**。14 张 M8 面表**全部已在** `migrations/upstream/`（上游 560 个 `.up.sql` ⊂ 本地 560 个，逐字，§6.4） |
| 新 crate | **3 个**：`mc-vcs`（`Provider` trait + forgejo/gitlab + 验签）、`mc-vcs-github`（GitHub App JWT + installation token 缓存 + REST/GraphQL + webhook + PR 镜像 + ghsnapshot 管道）、**`mc-composio`**（SDK 客户端 + 服务 + state HMAC + overlay 构建）。**不建 `mc-attachment`**（附件面维持 `M3+`，§9.2） |
| MCP 面 | **不新建 crate**：领域类型进 `mc-core/src/mcp{,.rs}`、仓储进 `mc-repos/src/mcp/`、HTTP 进 `mc-http/src/routes/mcp/`（理由与「不得重复实现」清单见 §2.3） |
| 切片数 | **1 anchor + 6 代码片 + 1 INT = 8 个 issue**，4 个 stage，stage 内并发 ≤ **3**；单片最大 **1,320** 上游行（≤3.5k，§4.1） |
| 尾斜杠双形态 | **0 键**。`slash_alias_audit.py --declared docs/fixtures/m8-declared-routes.tsv` 实测 `declared 25 / dual-form required: 0`，**exit 0**（对照：M4 15 / M5 7 / M6 5 / M7 0）。M8 **没有 allowlist 退路问题，也没有形态欠账** |
| ⑦ 目标 | anchor 后**逐字不变**（**M8-0 不刷基线**：0 路由、0 占位删除，只**原地搬运** 1 条占位）；全波落地后 `local 454 / implemented 378 real + 3 placeholder / known_gap 78 / owners.M8 0`，`implemented + known_gap == 456` 恒成立（§6.1） |
| ⑨ 目标 | 落在 M8 25 条路由上的 fixture **只有 1 条**（`integrations/TestComposioCallbackIsPublic_NoCookieNot401`，`unmounted`）—— **本波是六轮里 ⑨ 面最稀薄的一波**。承诺：这 1 条 `unmounted → pass`；其余面**上游没有可判 fixture** ⇒ 由本波自造离线 fixture 承担，且**不承诺**提高 `contract_equivalence_rate`（§6.2） |
| 最大风险 | R-M8-1 **平台不可真连**（GitHub/composio/自建 Git）⇒ 端到端证据靠本地替身（§4.2）；R-M8-2 **凭据与验签**面最大（4 类部署密钥 + 2 种 webhook 签名方案）；R-M8-8 无 `jsonwebtoken` 依赖，RS256 走 `ring`/`rsa`（§2.4） |

---

## 1. 上游面测绘（`f41fae6b08fb` 实测）

### 1.1 路由表（25 条，按**注册块 / 授权层**分 6 簇）

M8 与 M4…M7 的结构性差异：它的 25 条路由**不落在同一个子路由块里**，而是散在 6 个注册点，
其中 4 条在 **Auth 组之外**（公开块）、9 条在 workspace **admin** 组、3 条在 workspace **member** 组、
4 条在会话级 user 面、1 条在 issue 子路由、4 条在 agent 子路由。

| 簇 | router.go 行号 | 条数 | 授权层（逐字来自 router.go） |
|---|---|---:|---|
| 公开块（无 Multica 会话，凭 HMAC 验签） | 1490、1491、1500、1517 | 4 | 无 middleware；`HandleGitHubWebhook` HMAC-SHA256、`GitHubSetupCallback` state HMAC、`HandleVCSWebhook` 每连接签名、`ComposioCallback` state HMAC |
| workspace **member** 组（`RequireWorkspaceMemberFromURL`） | 1672、1676、1686 | 3 | GitHub installations 列表 / VCS connections 列表 / workspace MCP 库列表 |
| workspace **admin** 组（`RequireWorkspaceRoleFromURL owner\|admin`） | 1710–1712、1757–1759、1761–1763 | 9 | MCP 写 3 条 + GitHub connect/repos/delete 3 条 + VCS connect/rotate/delete 3 条 |
| 会话级 **user** 面（Auth 组内，无 workspace 上下文） | 1866–1869 | 4 | composio 的 4 条（连接属于用户，不属于 workspace） |
| issue 子路由（`/api/issues/{id}` 内，workspace member 组） | 2011 | 1 | `GET /pull-requests` |
| agent 子路由（`/api/agents/{id}` 内） | 2206–2209 | 4 | `loadAgentForUser`（agent owner 或 workspace owner/admin，与 M6-4 的 `/skills*` 同手法） |

逐条（`METHOD  PATH  router.go:行  owner`，与 `docs/fixtures/m8-declared-routes.tsv` 25/25 相等，复算见 §10 命令 1）：

| # | METHOD | PATH | router.go | 片 | 上游 handler |
|---:|---|---|---:|---|---|
| 1 | GET | `/api/github/setup` | 1491 | M8-1 | `GitHubSetupCallback`（`github.go:501`） |
| 2 | GET | `/api/workspaces/{id}/github/connect` | 1757 | M8-1 | `GitHubConnect`（`github.go:462`） |
| 3 | GET | `/api/workspaces/{id}/github/installations` | 1672 | M8-1 | `ListGitHubInstallations`（`github.go:712`） |
| 4 | GET | `/api/workspaces/{id}/github/installations/{installationId}/repositories` | 1758 | M8-1 | `ListGitHubInstallationRepositories`（`github.go:747`） |
| 5 | DELETE | `/api/workspaces/{id}/github/installations/{installationId}` | 1759 | M8-1 | `DeleteGitHubInstallation`（`github.go:938`） |
| 6 | POST | `/api/webhooks/github` | 1490 | M8-4 | `HandleGitHubWebhook`（`github.go:1056`） |
| 7 | GET | `/api/issues/{id}/pull-requests` | 2011 | M8-4 | `ListPullRequestsForIssue`（`github.go:964`） |
| 8 | GET | `/api/workspaces/{id}/vcs/connections` | 1676 | M8-2 | `ListVCSConnections`（`vcs.go:107`） |
| 9 | POST | `/api/workspaces/{id}/vcs/connections` | 1761 | M8-2 | `ConnectVCS`（`vcs.go:157`） |
| 10 | DELETE | `/api/workspaces/{id}/vcs/connections/{connectionId}` | 1763 | M8-2 | `DeleteVCSConnection`（`vcs.go:248`） |
| 11 | POST | `/api/workspaces/{id}/vcs/connections/{connectionId}/rotate-webhook` | 1762 | M8-2 | `RotateVCSConnectionWebhook`（`vcs.go:274`） |
| 12 | POST | `/api/webhooks/vcs/{connectionId}` | 1500 | M8-2 | `HandleVCSWebhook`（`vcs_webhook.go:91`） |
| 13 | GET | `/api/workspaces/{id}/mcp-servers` | 1686 | M8-3 | `ListWorkspaceMcpServers`（`workspace_mcp_api.go:87`） |
| 14 | POST | `/api/workspaces/{id}/mcp-servers` | 1710 | M8-3 | `CreateWorkspaceMcpServer`（同上 `:138`） |
| 15 | PUT | `/api/workspaces/{id}/mcp-servers/{serverId}` | 1711 | M8-3 | `UpdateWorkspaceMcpServer`（同上 `:207`） |
| 16 | DELETE | `/api/workspaces/{id}/mcp-servers/{serverId}` | 1712 | M8-3 | `DeleteWorkspaceMcpServer`（同上 `:259`） |
| 17 | GET | `/api/agents/{id}/mcp-servers` | 2206 | M8-3 | `ListAgentMcpServers`（同上 `:323`） |
| 18 | POST | `/api/agents/{id}/mcp-servers` | 2207 | M8-3 | `AddAgentMcpServer`（同上 `:380`） |
| 19 | PUT | `/api/agents/{id}/mcp-servers/{serverId}/enabled` | 2208 | M8-3 | `SetAgentMcpServerEnabled`（同上 `:444`） |
| 20 | DELETE | `/api/agents/{id}/mcp-servers/{serverId}` | 2209 | M8-3 | `RemoveAgentMcpServer`（同上 `:482`） |
| 21 | GET | `/api/integrations/composio/callback` | 1517 | M8-6 | `ComposioCallback`（`integrations_composio.go:112`） |
| 22 | POST | `/api/integrations/composio/connect/init` | 1866 | M8-6 | `ComposioConnectInit`（同上 `:67`） |
| 23 | GET | `/api/integrations/composio/toolkits` | 1867 | M8-6 | `ListComposioToolkits`（同上 `:174`） |
| 24 | GET | `/api/integrations/composio/connections` | 1868 | M8-6 | `ListComposioConnections`（同上 `:135`） |
| 25 | DELETE | `/api/integrations/composio/connections/{id}` | 1869 | M8-6 | `DeleteComposioConnection`（同上 `:203`） |

账（每行恰好一片）：M8-1 **5** + M8-2 **5** + M8-3 **8** + M8-4 **2** + M8-5 **0** + M8-6 **5** = **25** ✓

### 1.2 上游文件与行数（非测试）+ **单位裁定**

**裁定结论（§9.1 的口径修订）**：`plan1.md` §1.2 那行末尾的 `ghsnapshot 7 / composio 6 / vcs 4`
**单位是「该子目录下的一切文件数（递归，含 `_test.go`）」**——与 `docs/60` §9.1 对 `wecom 89 / …` 的裁定**同一口径**。
三条判据：

1. 实测 `find server/internal/integrations/<dir> -type f | wc -l` = ghsnapshot **7** / composio **6** / vcs **4**，
   **三个数与 `plan1.md` 逐字相符**（不像 `wecom` 有 89 vs 90 的偏差）⇒ 无需修订数值，只需钉单位。
2. LOC 口径差 2–3 个数量级（实测非测试 `.go` 行数 = 1,147 / 1,050 / 649，总和 2,846，不是 17）；路由数口径是 0（这三个目录**一条路由都不注册**）。
3. 同一个数字在 handler 侧就是另一口径：`internal/handler` 里 M8 的 7 个文件是 **3,780 行**（见下表），
   与「17 个文件」不可直接相加 ⇒ **报数必须两栏并列**（`一切文件数` 与 `非测试 .go 文件数 / 行数`）。

**M8 面的实测构成**（本片逐文件复算，命令见 §10 命令 2）：

| 上游文件 | 一切文件数 | 非测试 `.go` 行数 | 片 |
|---|---:|---:|---|
| `internal/handler/github.go`（L1–L963） | — | **963** | M8-1 |
| `internal/handler/github.go`（L964–L1997） | — | **1,034** | M8-4 |
| `internal/handler/vcs.go` | — | 336 | M8-2 |
| `internal/handler/vcs_webhook.go` | — | 335 | M8-2 |
| `internal/handler/workspace_mcp_api.go` | — | 530 | M8-3 |
| `internal/handler/workspace_mcp.go` | — | 193 | M8-3 |
| `internal/handler/mcp_overlay.go` | — | 160 | M8-3 |
| `internal/handler/integrations_composio.go` | — | 229 | M8-6 |
| **handler 小计** | — | **3,780** | |
| `internal/integrations/vcs/`（`vcs.go` 144 + `forgejo.go` 257 + `gitlab.go` 248） | **4** | 649 | M8-2 |
| `internal/integrations/ghsnapshot/`（`client.go` 295 + `refresh.go` 562 + `snapshot.go` 290） | **7** | 1,147 | M8-1（client） + M8-5（refresh/snapshot） |
| `internal/integrations/composio/`（`service.go` 709 + `dispatch.go` 249 + `state.go` 92） | **6** | 1,050 | M8-6 |
| **integrations 小计** | **17** | **2,846** | |
| **M8 全波合计** | — | **6,626** | |

**`github.go` 是唯一被两片共用的上游文件**（1,997 行按**函数区间**切：L1–L963 归 M8-1、L964–L1997 归 M8-4），
这不是「拆上游文件」，而是**本仓按本地文件分工**的必然结果（一个上游文件里的 handler 分属两条路由簇 +
两套授权层）。切分点钉在 `ListPullRequestsForIssue`（`github.go:964`）——它是本文件里**唯一**属于 M8-4 簇的只读 handler，
其后的 `broadcastPRSnapshotApplied`（1011）、`HandleGitHubWebhook`（1056）及 PR 镜像全部归 M8-4。
两片共用的响应映射（`githubInstallationToResponse`/`githubPullRequestToResponse`，L163–L330）落在 M8-1 的写集
（`crates/mc-http/src/routes/github/dto.rs`），**M8-4 只读**——记法纪律照 `docs/60` §3.3 的「写者 / 读者」两列。

**25 条路由 vs 6,626 行**：路由面只覆盖这个面最上面的一层（列表/连接/回调），真正的体量在三处：
① PR 镜像与自动关联/自动关闭（M8-4 的 1,034 行）；② GitHub 安装式凭据链与 GraphQL 快照管道（M8-1 + M8-5 的 2,000+ 行）；
③ VCS 的 per-provider 抽象与两种签名方案（M8-2 的 1,320 行）。⇒ `plan1.md` §5 W8 行写的
「上游对应 = `integrations/vcs`」**严重低估**（vcs 只是 3 文件 649 行），见 §9.6。

### 1.3 本地现状与缺口（`ec83f6e7` 实测）

```
$ python3 scripts/route_parity.py
upstream 456 (commit f41fae6b08fb) | local 405 registered | baseline 344
  implemented  325 real +   4 placeholder =  329 / 456   known_gap  127   unclaimed    0   regression   0   local_only    9
  gaps by owner: M9=33  M7=24  M8=24  M3+=16  M2-A=13  M3=11  M10=5  M6=1
```

* **M8 的 25 条里，24 条是 `known_gap`**（§1.1 表里除第 7 行外的全部）；第 7 行
  `GET /api/issues/{id}/pull-requests` 已以 `not_implemented` 注册在
  `crates/mc-http/src/routes/issues/mod.rs:193`（占位名 `not_implemented`，返 501）⇒ 它**计入 `implemented_placeholder`**，
  不占 `known_gap`。这正是「**24 还是 25**」这条口径的来源，见 §1.7。
* **M8 面在本地几乎零代码**：`grep -rln 'workspace_mcp_server\|agent_mcp_server\|WorkspaceMcpServer\|AgentMcpServer' crates/ apps/`
  **零命中**（目录 `mc-repos/src/{vcs,github,mcp,composio}` 都不存在）；`vcs` 只出现在 `mc-daemon-proto/src/events.rs`
  之类与文件存储无关的位置；`pull-requests` 只出现在 `routes/issues/mod.rs:193`（占位）与它的测试注释里。
  ⇒ **M8 是从零起的一波**，没有 M6 那样的「半交付面」要收编（唯一例外见 §2.3 的 MCP 交集清单）。
* `local_only 9` 与 M8 无关（3 条 `/api/me/pats` + 4 条健康/openapi + `/api/issues/:id/{reactions,quick-actions}` + 1 条占位
  `GET /api/feature-flags`）⇒ **M8 不新增 local_only**。
* 门 ⑦ 的第二条命令（形态）本地实测 `registered upstream-key literals: 409 / 0 defect(s)` ⇒ 本波起手**无形态欠账**，
  `docs/fixtures/slash-alias-allowlist.tsv` **当前为空**（M6-0 删完了最后 2 行）。

### 1.4 尾斜杠双形态：本波实测 **0 键**

上游 chi 只在「子路由写 `"/"`」时两种形态都服务（`Mount` 语义）。M8 的 25 条**全部是完整子路径的 plain 注册**：

```
$ python3 scripts/slash_alias_audit.py --declared docs/fixtures/m8-declared-routes.tsv
  declared 25 upstream key(s); dual-form required: 0 | single-form: 25
  shapes OK: every registered upstream key matches the form upstream serves
  => 0 defect(s) from findings, 0 warning(s)                       # exit 0
```

| 波 | 声明键 | `dual-form required` | 预测模式 exit | 退路 |
|---|---:|---:|---:|---|
| M4（`docs/42`） | 29 | 15 | 1（`FAIL: 15`） | 曾有 allowlist 6 行，M4-0 删毕 |
| M5（`docs/44`） | 29 | 7 | 1（`FAIL: 7`） | 曾有 allowlist 2 行，M5-0 删毕 |
| M6（`docs/57`） | 57 | 5 | 1（`FAIL: 5`） | M6-0 删毕 ⇒ 现在为空 |
| M7（`docs/60`） | 24 | 0 | **0** | 无欠账 |
| **M8（本文）** | **25** | **0** | **0** | **无欠账，无需 allowlist** |

⚠️ 注意 `composio` 那一簇：上游 `router.go:1863` 是 `r.Route("/api/integrations/composio", …)`（Mount），
但 4 个子路由写的是**完整子路径**（`r.Post("/connect/init", …)`），**不是** `Get("/")`
⇒ 不构成双形态键（`predict()` 只看 `upstream-routes.tsv` 里该 key 是否带尾斜杠，实测 4 条都不带）。
同理 `r.Route("/api/issues") + r.Get("/pull-requests")`、`r.Route("/api/agents") + r.Get("/mcp-servers")` 都是 plain 子路由。

⇒ M8 各片的形态纪律只有**一条**：**只按上游字面量注册那一形态，既不补尾斜杠形态、也不得漏成带斜杠形态**
（补了带斜杠形态 = `EXTRA_ALIAS` 缺陷：axum 会同时服务 `/x` 与 `/x/`，而上游 plain 注册只服务 `/x`）。

### 1.5 入站 webhook 的路由口径（**单列一节**，`plan1.md` §1.3 的复核项）

**结论：M8 的两条入站 webhook 都在 456 条表内、都注册在 `router.go`、都走同一棵 chi 路由树，
没有任何 `internal/integrations/**` 自带的 mux。** 三条判据：

1. `POST /api/webhooks/github`（`router.go:1490`）与 `POST /api/webhooks/vcs/{connectionId}`（`router.go:1500`）
   **字面写在 `router.go` 的「Public API」块里**（与 `POST /api/webhooks/autopilots/{token}`、`POST /api/webhooks/stripe`
   同一段），注释逐字说明「no Multica auth — authenticated via HMAC-SHA256 signature in the handler」。
2. 全仓扫 `internal/integrations/**`：**零** `http.NewServeMux` / `chi.NewRouter`（只有 `lark/*_test.go` 的测试替身用到 `NewServeMux`）；
   `internal/integrations/vcs` 只导出 `Provider` trait 与 `Provider.VerifySignature(...)`，**不注册任何路由**。
3. `plan1.md` §1.3 的「456 条，其中 `/api`+`/auth` 440 条」**成立**（16 条非 `/api`/`/auth`，与 `docs/60` §1.5 的点名逐条一致，
   本波复核后**新增点两条**：`POST /api/webhooks/github` 与 `POST /api/webhooks/vcs/{connectionId}` **都在 `/api` 一侧**，
   不落在那 16 条里）。

**后果（写进 §2.4 / §4.2 / §6）**：M8 的 4 条公开路由**不经过会话 middleware**，其凭据是：

| 公开路由 | 凭据来源 | 失败语义 |
|---|---|---|
| `POST /api/webhooks/github` | `GITHUB_WEBHOOK_SECRET` + `X-Hub-Signature-256`（HMAC-SHA256） | 缺密钥/验签失败 ⇒ 401（**不是 404**：路由必须在，鉴权在 handler 内） |
| `GET /api/github/setup` | `GITHUB_WEBHOOK_SECRET` 签的 state（`<workspaceID>.<nonce>.<sigHex>`，HMAC-SHA256） | state 不合法 ⇒ 400/401；`GITHUB_APP_SLUG` 或 secret 缺失 ⇒ 「未配置」语义（**逐端点**，见 §2.4） |
| `POST /api/webhooks/vcs/{connectionId}` | 路径里的 `connectionId` 决定 workspace/provider/**解密密钥**，再用该连接的 webhook secret 验签（Forgejo/Gitea = HMAC 头；GitLab = `X-Gitlab-Token` **明文比较**） | 连接不存在/密钥缺失/验签失败 ⇒ 401/404（上游语义逐条对齐） |
| `GET /api/integrations/composio/callback` | `COMPOSIO_STATE_SECRET`（或由 `JWT_SECRET` 派生）签的 `state`；**明确从会话之外取身份** | state 不合法 ⇒ **401**（这正是 ⑨ 唯一那条 M8 fixture 断言的语义，见 §6.2） |

### 1.6 GitHub App 安装式 vs VCS connection：**不是同一抽象**（§2.2 判据 2 的证据）

| 维度 | GitHub App（`github.go` + `ghsnapshot`） | VCS connection（`integrations/vcs`） |
|---|---|---|
| 凭据模型 | App 私钥（PEM）→ 签 App JWT（RS256）→ 换 **installation token**（短时、按安装、需刷新缓存） | 每 workspace **一条连接**存 provider + instanceURL + **用户 PAT**（`secretbox` 密文） |
| 出站选路 | 固定 `https://api.github.com`（`github.go:36` 的**可写包级变量**，注释逐字「Mutable so tests can…」） | 每连接自带 `instance_url`（`NormalizeInstanceURL`）⇒ 天然可替换 |
| 入站验签 | 单一 `GITHUB_WEBHOOK_SECRET` + `X-Hub-Signature-256`，事件族 installation/pull_request/check_suite | **per-connection** secret（可轮换，`rotate-webhook`）+ **两种**签名方案（HMAC / 明文 token） |
| 事件→领域映射 | PR 镜像 + **自动关联 issue**（identifier/closing keyword）+ **自动关闭 issue** + CI 事件触发快照刷新 | PR 镜像 + CI 状态镜像（`vcs_commit_status`），**无**自动关联/关闭 |
| 抽象载体 | 无 trait（单实现，直连 REST + GraphQL） | **有** `Provider` trait（`Kind/EventKind/VerifySignature/ParsePullRequest/ParseCIStatus/ValidateToken`）+ `registry` |

⇒ **判据 2（是否需要 per-provider trait 抽象）在 VCS 侧成立、在 GitHub 侧不成立**（GitHub 是单实现，
上游自己也没把它塞进 `integrations/vcs` 的 registry）。这就是 §2.2 把两个 crate 分开的**唯一硬理由**。

### 1.7 账的口径：**25 条 / 24 缺口 / +9 条 attachments（不并入）**

三条不同的数，别混用：

| 数 | 含义 | 来源 |
|---:|---|---|
| **25** | M8 名下**上游路由总数**（= `upstream-routes.tsv` 里 owner=M8 的行数） | `awk -F'\t' '!/^#/ && $3=="M8"' docs/fixtures/upstream-routes.tsv \| wc -l` |
| **24** | 其中**尚未实现**的（⑦ 的 `owners.M8`）；`LUM-1796` 描述里写的「24 条」指这个 | `python3 scripts/route_parity.py` 的 `gaps by owner: … M8=24` |
| **+9** | attachments 面（`docs/15` §580/§581 判给「W8/M8」）—— **本波不并入**，裁定的依据与影响见 §9.2 | §9.2 的 9 行逐字清单 |

`docs/fixtures/m8-declared-routes.tsv` 照 M7 的构造配方取**第一条口径（25 行）**，
并要求 `diff` 与 fixture 的 owner=M8 行集合为空（§10 命令 1）——这既是形态预判的输入，
也是「本片没有偷偷改账」的证据。

### 1.8 已退役 / 不存在的路由：**本波为空**

M7 有一条「必须 404」的反向验收（`dingtalk/group-routes`）。M8 复核后**没有同类项**：`GET /api/issues/{id}/pull-requests` 是**真路由**（只是本地还是占位），
`grep -n 'group-routes\|deprecated' docs/fixtures/upstream-routes.tsv` 无 M8 命中 ⇒ 本波不需要反向验收条目。

---

## 2. 目标架构与落点（含取舍）

### 2.1 分层落点

| 层 | 落点 | 内容 |
|---|---|---|
| 领域类型（跨 crate） | `crates/mc-core/src/{vcs.rs,github.rs,mcp.rs,composio.rs}`（anchor 建） | `VcsProviderKind`、`VcsConnection`、`PullRequestSnapshot`、`IssuePrLink`、`WorkspaceMcpServer`/`McpTransport`/`McpBinding`、`ComposioConnection` |
| VCS provider 抽象 | **`crates/mc-vcs/`**（新 crate） | `Provider` trait（6 方法）+ registry + `EventKind`/`PullRequestEvent`/`CIStatusEvent` + `VerifySignature` 两种方案 + `forgejo.rs` / `gitlab.rs` |
| GitHub App + 快照管道 | **`crates/mc-vcs-github/`**（新 crate） | `app.rs`（RS256 App JWT）、`token_cache.rs`（installation token 缓存 + 单飞刷新）、`rest.rs`（分页/仓库/撤销）、`dto.rs`、`payload.rs`/`webhook.rs`（验签 + 事件分派）、`mirror.rs`/`closepolicy.rs`/`links.rs`、`ghsnapshot/{client,snapshot,refresh}.rs`、`port.rs`（`PrRefreshPort`） |
| composio | **`crates/mc-composio/`**（新 crate） | `client.rs`（HTTP + `x-api-key`）、`service.rs`（connect/callback/list/toolkits/disconnect）、`state.rs`（HMAC state）、`catalog.rs`（toolkit/auth-config 解析）、`overlay.rs`（per-task MCP overlay 构建） |
| 密钥端口 | `crates/mc-secrets/src/secretbox.rs`（**M7-0 建，M8-1 只读**） | `Box::{new,seal,open}`（`nonce(12)‖ct‖tag`）+ `load_key(env)`。M8 **不新增第二份实现** |
| PG 仓储 | `crates/mc-repos/src/{vcs,github,mcp,composio}/`（anchor 建 `mod.rs`，各片填子文件） | 14 张表按面分文件（§3.3） |
| HTTP 面 | `crates/mc-http/src/routes/{github,vcs,mcp,composio}/`（anchor 建聚合 `mod.rs` + 空子 router） | 25 条路由，**一个子文件 = 一个写者** |
| 后台宿主 | `apps/mc-server/src/integrations.rs`（anchor 建，M8-5 填） | `ghsnapshot::Manager::start`（worker 池 + TTL sweeper）+ graceful shutdown 句柄 |

### 2.2 为什么是**三个**新 crate（issue 的三条判据 + 被否决的备选）

**判据 1：是否被 daemon 与 http 同时依赖？**

* `mc-vcs` —— **否**（只有 HTTP 面用它；每连接签名与事件解析都是请求内的事）。⇒ 不落 `mc-core`。
* `mc-vcs-github` —— **是（两个宿主，不是两个 crate）**：`ghsnapshot::Manager` 是**长期后台 worker**
  （上游 `handler.go:513-519` 在 `NewHandler` 里 `h.PRRefresh = ghsnapshot.NewManager(...)`，注释要求
  `h.PRRefresh.Start(ctx)`「launch its worker pool + TTL sweeper」），宿主必须是 `apps/mc-server`；
  而 `Client`/`payload`/`mirror` 是请求内的事。⇒ 同一 crate、**两个宿主**（http + app），靠 `port.rs` 的 trait 分界。
* `mc-composio` —— **是**：上游 `router.go:1252` 逐字 `h.TaskService.Composio = svc`，即 composio 服务**同时**被
  ① composio 的 5 条 HTTP 路由、② task 派发服务（`service/task.go` 的 `Enqueue*` 用它算 per-task MCP overlay）使用。
  本地对应物：`mc-repos/src/task/queries.rs:469` 已经在 **SELECT** `runtime_mcp_overlay`，
  而 `mc-repos/src/chat_task/send.rs:30` 的注释写着「`runtime_mcp_overlay` / `runtime_connected_apps` **恒 `NULL`**」
  ⇒ **读侧已接、写侧没有生产者**。生产者必须在 `mc-http` 与派发层都能拿到 ⇒ 独立 crate。

**判据 2：是否需要 per-provider trait 抽象？** —— **VCS 侧是硬需求，GitHub 侧不是**（§1.6 的五行对照）：
上游 `integrations/vcs` 有 `Provider` 接口 + `registry`（`register()` 在 `init()` 里），
而 GitHub 是 `handler/github.go` 里的单实现、直连 REST/GraphQL。⇒ **crate 数 ≥ 2**。

**判据 3：凭据与验签是否共用？** —— **部分共用，但共用件已在 `mc-secrets`**：
四类部署密钥（`GITHUB_APP_PRIVATE_KEY` / `GITHUB_WEBHOOK_SECRET` / `MULTICA_VCS_SECRET_KEY` / `COMPOSIO_API_KEY`
+ `COMPOSIO_STATE_SECRET`）都经 `mc_secrets::secretbox` 或直接读 env，**验签算法却分三种**
（GitHub HMAC-SHA256、Forgejo HMAC-SHA256、GitLab 明文比较）⇒ 共用件不构成「合并成一个 crate」的理由。

**被否决的备选（各写代价）**：

| 备选 | 否决理由 |
|---|---|
| 只建 `mc-vcs`（GitHub 作为它的第 3 个 provider） | GitHub 的 `installationToken` 是一次**带缓存的 OAuth 交换**（`client.go:132/162`），不是 `ValidateToken` 那种「拿 token 试一次」；强行并入会让 `Provider` trait 长出 `Refreshable Token` 这条只有 1/3 实现者用得上的方法（接口污染）。且 `ghsnapshot` 的 852 行 GraphQL 管道与 PR 镜像（1,034 行）本就不是 provider 抽象的一部分 |
| 把 `mc-vcs-github` 拆成 `mc-vcs-github` + `mc-ghsnapshot` | `ghsnapshot::Client` 的 installation token 缓存与 `github.go` 的 `signGitHubAppJWT`/`fetchInstallationAccount` 是**同一套** App 凭据链（两处都在读 `GITHUB_APP_ID`/`GITHUB_APP_PRIVATE_KEY`，实测 `ghsnapshot/client.go:88-89`）⇒ 拆开会立刻出现第二份 JWT 签名与第二份 token 缓存。**这是本文与 `plan1.md` §3.3 的唯一 crate 数差异**（§9.5） |
| 不建 `mc-composio`，塞进 `mc-http` | 违反判据 1 的第二半：生产者要被派发层拿到，而 `mc-http` 不该被派发层依赖（依赖方向会反向）。另外 composio 有 1,050 行服务代码 + HMAC state，`mc-http` 已 40 个路由模块 |
| 复用 `mc-feature-flags` / `mc-storage` | 与 M8 无关；composio 的「启用」是 feature flag **与** 密钥的组合（`router.go:1216-1219` 两层都过才装配），flag 由既有的 `mc-feature-flags` 提供，**不新建** |
| 建 `mc-attachment`（`plan1.md` §3.3 列在 W8） | 附件面 9 条路由的 owner 是 `M3+`（fixture 权威）⇒ 本波**不动**它，登记为 W8 尾账（§9.2）。建一个没有路由、没有验收面的 crate 是纯粹的浪费 |

**依赖方向（无环，anchor 一次性接好）**：
`mc-vcs` → `mc-core` / `mc-errors` / `mc-secrets` / `mc-telemetry`（+ `reqwest` / `hmac` / `sha2` / `hex` / `url`）；
`mc-vcs-github` → `mc-core` / `mc-errors` / `mc-repos` / `mc-telemetry`（+ `reqwest` / `hmac` / `sha2` / `hex` / `base64` / **`ring` 或 `rsa`**）；
`mc-composio` → `mc-core` / `mc-errors` / `mc-repos` / `mc-telemetry`（+ `reqwest` / `hmac` / `sha2` / `base64` / `url`）；
`mc-http` → 三个新 crate（各一条 `path` 边）；`apps/mc-server` → `mc-vcs-github` + `mc-composio`（各一条 `path` 边）。

### 2.3 MCP 面：**不新建 crate** + 与 M6 已交付面的「**不得重复实现**」清单

`mcp-servers` 的 8 条路由是 **workspace MCP 服务器库**（`workspace_mcp_server` / `agent_mcp_server` 两张表）的 CRUD +
agent 绑定开关，外加 `mcp_overlay.go` 的 per-task overlay 合并。**它不是 remote MCP 客户端**，与 M6 交付的四个面**正交**：

| M6 已交付面（本地，实测存在） | 是什么 | M8 的关系 |
|---|---|---|
| `crates/mc-mcp/src/{client,types,oauth,devorigin}.rs` | remote MCP **客户端**（JSON-RPC over HTTP + OAuth，`pkg/remotemcp`） | **零交集**。`mc-mcp/src/lib.rs` 逐字写着「不做 MCP 服务端」「不碰数据库」⇒ M8 不得往里加库/仓储代码 |
| `crates/mc-repos/src/plugin/mcp_approval.rs` | 插件远程 MCP 的**工具采纳记录**（`plugin_installation.mcp_approvals`） | **零交集**（不同表、不同生命周期：一个是插件调用授权，一个是 workspace 服务器库） |
| `crates/mc-daemon/src/mcp/{runtime.rs,hook.rs,broker/*}` | **daemon 侧**运行时 MCP：读 provider 原生配置 + 本地合并 + 去敏 inventory | **读侧消费者**：M8 产出的 `runtime_mcp_overlay` 就是「上行注入」到这里的输入；M8 **不改** daemon 文件 |
| `crates/mc-http/src/routes/plugins/mcp.rs` | 插件 MCP 的 3 条路由（M6-6） | **零交集**，但它的文件头（`mcp.rs:39`）**已经逐字记下**：「`mcp_overlay.go` 是 per-task agent overlay（**M8 面**），同理不接」⇒ **本波的主权有 M6 自己写的凭证** |

**裁定（§9.4）**：`mcp-servers` 8 条**账与实现都归 M8**（fixture owner 权威，且不改 owner 单元格），
**不迁移到 W6 尾账**；实现方式是「**只补 HTTP + 存储 + overlay 纯函数**，复用既有面」：

1. **复用** `mc-core` 的 agent 配置形状与 `mc-daemon/src/mcp/runtime.rs` 的合并语义（daemon 侧**已实现**「runtime 层做底、agent 层同名覆盖」）；
2. **新增**（本波独有的东西）：`workspace_mcp_server` / `agent_mcp_server` 的仓储与 8 条路由，
   以及 `mcp_overlay.go` 的 **per-task overlay 合并**（纯函数，落 `crates/mc-core/src/mcp/overlay.rs`）；
3. **禁止**：在 `mc-mcp` 里加库/仓储、在 `mc-daemon/src/mcp/**` 里加 HTTP 语义、复制 daemon 侧的去敏逻辑。

### 2.4 凭据、部署密钥与 redaction（**四类密钥逐条**）

| 部署密钥 | 用途 | 落点 | 缺失语义 |
|---|---|---|---|
| `GITHUB_APP_SLUG` + `GITHUB_WEBHOOK_SECRET` | 安装引导 URL + **webhook 验签** + **state 签名**（上游刻意复用同一个 secret，`github.go:364-366` 注释逐字「so operators only need to configure one value」） | `crates/mc-http/src/state.rs` 的 `github` 字段（anchor 建，仅在 `AppState::new` 构造体内读 env） | `isGitHubConfigured()==false` ⇒ connect 端点给「未配置」语义；webhook 401 |
| `GITHUB_APP_ID` + `GITHUB_APP_PRIVATE_KEY`（PEM） | App JWT（RS256）→ installation token | 同上 + `mc-vcs-github::app` | **与上两条独立**（`isGitHubRepositoryBrowseConfigured()`）⇒ 「能连接」≠「能浏览仓库」，逐端点语义 |
| `MULTICA_VCS_SECRET_KEY`（base64 32B） | `secretbox` 封装每连接的 PAT + webhook secret | `mc_secrets::secretbox`（**M7-0 建的**）+ `mc-http/src/state.rs` | `isVCSConfigured()==false` ⇒ connect 503（**绝不落明文**）；另有 `MULTICA_VCS_INTEGRATION_ENABLED`（**产品边界**，自建版才有，云端关掉）⇒ 两个开关语义独立 |
| `COMPOSIO_API_KEY` + `COMPOSIO_STATE_SECRET`\|`JWT_SECRET` + `COMPOSIO_CALLBACK_BASE_URL`\|`MULTICA_PUBLIC_URL` | SDK 鉴权 + state HMAC + 回调地址 | `mc-composio::service` + `mc-http/src/state.rs` | **三个条件缺一即整体不装配**（`router.go:1220-1256` 的 switch），4 条会话路由 503 |
| MCP 服务器条目的 `headers`/`env` 值 | 第三方凭证 | `workspace_mcp_server.config`（**write-only**） | 上游注释逐字：「The payload is names and transports only; the stored entries are **write-only**」⇒ 响应**永不**含值字段 |

**redaction 是硬约束（照 `docs/33` §12.2 先例，与 `docs/60` §2.3 同四条判据）**：

1. 承载密钥/密文/明文 secret 的类型**手写 `Debug`**，输出 `<redacted>`（照 `PluginSecretKey` 的 `crates/mc-http/src/state.rs:246` 先例）；
2. 任何 `tracing::*` 调用**不得**插值这些字段（clippy 抓不到，靠测试）；
3. 新增「错误路径**不回显**凭据」用例（GitHub/VCS/composio 各一条：`github_errors_never_echo_app_key` 等）；
4. `mc_telemetry::redact::Redactor::is_sensitive` 的 `SENSITIVE_KEYS` 需覆盖 `*token`/`*secret`/`app_private_key`/`x-api-key`
   （anchor 一次性补齐并给用例）。

**RS256 与 `Cargo.lock`（R-M8-8）**：本仓 **没有 `jsonwebtoken`**（`grep -c '^name = "jsonwebtoken"$' Cargo.lock` = 0），
但 `ring 0.17.14` 与 `rsa 0.9.10` **都已在 lock 里**（传递依赖）⇒ 裁定：**anchor 一次性选定其中一个**
（`ring` 的 `RSA_PKCS1_SHA256` 更贴近上游 `golang-jwt` 的 PKCS#1 v1.5），
**不新增外部包**，只新增一条 `[workspace.dependencies]` 直连边；选择理由与实测写进 `docs/32` §9.12。

### 2.5 「未配置 / 未授权」语义**逐端点不同**（M8 版 R-M7-3）

M7 的同类风险是「5 个渠道密钥缺失 ⇒ 各端点语义不同」；M8 的风险面更大，因为**授权层就有 5 种**：

| 端点族 | 未配置（缺密钥） | 未授权（会话/角色/agent 权限） |
|---|---|---|
| `GET /api/workspaces/{id}/github/connect` | 200 + `configured:false`（前端据此隐藏按钮） | 403（非 owner/admin） |
| `GET /api/github/setup` | 400/401（state 签不出来） | 无（公开路由） |
| `POST /api/webhooks/github` | 401（密钥空 ⇒ 验签必失败） | 无 |
| `GET /api/workspaces/{id}/github/installations` | 200 + 空数组（member 可见） | 401/403 |
| VCS 4 条写面 | `isVCSAvailable()==false` ⇒ 403/404（**产品边界**）；`isVCSConfigured()==false` ⇒ 503 | 403（非 owner/admin） |
| `POST /api/webhooks/vcs/{connectionId}` | 连接不存在 ⇒ 404；`VCSSecretBox` 空 ⇒ 503 | 无 |
| MCP 8 条 | **无未配置语义**（本地库，不依赖外部密钥） | member/admin / `loadAgentForUser` |
| composio 5 条 | 503（缺 `COMPOSIO_API_KEY` / flag 关 / 缺 state secret / 缺回调基址，**四种**） | 401（匿名） / 403 |

⇒ 各片 DoD 必须**逐端点**写「未配置 + 未授权」两格，**不许统一返回 503/401**。

### 2.6 后台宿主：`ghsnapshot::Manager` 在 `apps/mc-server`（不是 `mc-http`）

* 上游 `handler.go:513-519`：`NewHandler` 里造 `Manager`，由 `cmd/server/main.go` 调 `h.PRRefresh.Start(ctx)`，
  Manager 自带 **worker 池 + TTL sweeper + 限流暂停 + 单地址串行**（`refresh.go` 的 `release/finish/deferActive/scheduleRetry/scheduleChase`）。
* 本仓对应物 = `apps/mc-server/src/integrations.rs`（与 M5-9 的 `scheduler::start`、M7-0 的 `channels::start` 同造型），
  `main.rs` 新增一步调用，并进**停机链**：`先停渠道连接 → 再停 PR 刷新 → 再停调度器 → 最后停 actor`。
* **为什么不在 `mc-http`**：`mc-http` 是薄适配层（`plan1.md` §3.1），且 worker 的重试/退避/限流不属于请求面。

### 2.7 边界契约（写进各片 DoD，逐条可测）

1. **`mc-vcs` 不知道 GitHub**：`Provider` trait 的 6 个方法不得出现 `github` 字样；GitHub 不注册进那个 registry。
2. **凭据只经 `secretbox` 或 env**：任何 handler/DTO**不得**有 `String` 明文 secret 的 `Debug`/`Display`/日志插值（§2.4 四条）。
3. **webhook 验签必须常量时间**：HMAC 比较用 `hmac::Mac::verify_slice`（或 `subtle` 等价物），GitLab 的 `X-Gitlab-Token` 明文比较也要用常量时间比较函数 —— 反例测试：签名差 1 位必失败、正确签名必通过。
4. **公开路由的鉴权在 handler 内**：4 条公开路由**不得**挂会话 middleware，也**不得**因为缺会话就返回 401（除非验签真失败）。
5. **MCP 条目 write-only**：响应 DTO **不含** `headers`/`env` 的值。
6. **不改 `Route 456` 的形态**：只按上游字面量注册（§1.4）。
7. **不实现已退役路由**：本波为空（§1.8），但 M8-4 必须**保留** `pull-requests` 路由存在（占位 → 真实现，不能删）。

---

## 3. 写集与并发

### 3.1 共享锚点（**只在 M8-0 动一次**，其余片只读）

| 共享件 | 动作 |
|---|---|
| `Cargo.toml`（根 `[workspace.dependencies]`） | **最多新增一条**：`ring` 或 `rsa`（§2.4，二者都已在 lock 里，不引入新包）。members 是 `crates/*` glob ⇒ **members 行不动** |
| `Cargo.lock` | 只重新生成（**只有 anchor 能改**）：新增 3 个 workspace 成员 + 1 条直连边 |
| `crates/{mc-vcs,mc-vcs-github,mc-composio}/Cargo.toml` + `src/lib.rs` | **建骨架**（`pub mod` 树 + trait/类型位 + 空实现），依赖边一次接好 |
| `crates/mc-http/Cargo.toml` | 加 3 条 `path` 边（`mc-vcs` / `mc-vcs-github` / `mc-composio`） |
| `apps/mc-server/Cargo.toml` | 加 2 条 `path` 边（`mc-vcs-github` / `mc-composio`） |
| `crates/mc-http/src/routes/{mod.rs,mount.rs}` | 加 4 个 `pub mod` + `mount_slice_code_artifacts()`（**anchor 期 4 个子 router 全空** ⇒ 注册键不变） |
| `crates/mc-http/src/state.rs` | 加 `vcs` / `github` / `composio` 三组字段（**在 `AppState::new` 内读 env，不新增参数**）+ 唯一出口 |
| `crates/mc-http/src/routes/auth.rs` | 测试里唯一的 `AppState { … }` 字面量补字段 |
| `crates/mc-http/src/routes/issues/mod.rs` | **搬运**（不是删除）`/api/issues/:id/pull-requests` 那一行 501 占位 → `routes/github/issue_pr.rs`（handler 名仍是 `not_implemented`）⇒ **注册键集合逐字不变** |
| `crates/mc-core/src/{vcs.rs,github.rs,mcp.rs,composio.rs}` + `crates/mc-core/src/lib.rs` | 领域类型位 + 4 行 `pub mod` |
| `crates/mc-repos/src/{lib.rs,vcs/mod.rs,github/mod.rs,mcp/mod.rs,composio/mod.rs,task/mod.rs}` | 模块树 + `pub use`（各面文件由各片新建） |
| `apps/mc-server/src/{main.rs,integrations.rs}` | 新文件 + `main.rs` 一次调用 + 停机链一行 |
| `docs/fixtures/route-parity-baseline.json` | **本 anchor 不动**（0 路由、0 占位删除）⇒ 归 **M8-7（INT）** 刷新 |
| `docs/32` §9.12 偏离表 | 按本文 §9 + R-M8-1…9 追加（**anchor 一次落，后续片不回来改**；号段起手复核） |
| `docs/fixtures/slash-alias-allowlist.tsv` | **不动**（本波 0 欠账，§1.4） |

### 3.2 与 **M6 / M7 热点**的交集（逐文件；M6/M7 收口后这些是**只读面**）

| 热点文件 | 谁要碰 | 结论 |
|---|---|---|
| `crates/mc-http/src/state.rs` | **M7-0** 加渠道密钥字段、**M8-0** 加 vcs/github/composio 字段 | **同文件、不同块** ⇒ **两个 anchor 不得同飞**（§7） |
| `crates/mc-http/src/routes/mod.rs` | M7-0 +1 `pub mod channels`、M8-0 +4 `pub mod` | 同上 |
| `crates/mc-http/src/routes/mount.rs` | M7-0 +1 `mount_slice_channel()`、M8-0 +1 `mount_slice_code_artifacts()` | 同上 |
| `crates/mc-core/src/lib.rs` / `crates/mc-repos/src/lib.rs` | M7-0 +channel 模块、M8-0 +4 模块 | 同上 |
| `crates/mc-http/src/routes/auth.rs` | 两个 anchor 各补字段 | 同上（该文件是全仓唯一的 `AppState` 字面量构造点） |
| `apps/mc-server/src/main.rs` | M7-0 +渠道停机步、M8-0 +PR 刷新停机步 | 同上 |
| `Cargo.lock` | 两个 anchor 各重新生成 | 同上（**必须串行**，否则 lock 三向冲突） |
| `crates/mc-secrets/src/secretbox.rs` | **M7-0 建**、**M8-1 只读** | 不同波次、**零写集交集**；M8 **不建第二份** |
| `crates/mc-http/src/routes/channels/**` / `crates/mc-channel/**` / `crates/mc-repos/src/channel/**` / `crates/mc-core/src/channel*` / `apps/mc-server/src/channels.rs` | M7 各片 | **与 M8 零交集**（M8 只碰 `routes/{github,vcs,mcp,composio}`、`mc-vcs*`、`mc-composio`、`mc-repos/src/{vcs,github,mcp,composio}`、`mc-core/src/{vcs,github,mcp,composio}`） |
| `crates/mc-repos/src/task/store.rs`（782 行）/ `task/queries.rs` | **M8 不碰**：overlay 走**新文件** `crates/mc-repos/src/task/overlay.rs`（anchor 建 `task/mod.rs` 的 `pub mod`），用一条独立 `UPDATE` 写 `runtime_mcp_overlay`，**不改 `NewTask` 字面量**（否则会连锁改所有调用点，见 R-M8-9） | 零交集 |
| `crates/mc-mcp/**`、`crates/mc-repos/src/plugin/**`、`crates/mc-daemon/src/mcp/**`、`crates/mc-http/src/routes/plugins/**` | M6 已交付 | **只读**；§2.3 的「不得重复实现」清单是硬约束 |
| `docs/32-M3-DAEMON-FACE.md` | M7 各片的 §9.x + M8-0 的 §9.12 | **唯一共享的文档写点**；建议 M8-0 与 M7 各片**不同轮**落笔（否则人工解冲突）。降级为低危 |

### 3.3 写集（**一格 = 一个本地文件 = 一个写者**；逐字路径，禁 glob/花括号）

| 本地文件（逐字） | 唯一写者 | 读者 |
|---|---|---|
| `crates/mc-vcs/src/{lib.rs,provider.rs,events.rs,registry.rs,signature.rs}` | M8-0 | M8-2 |
| `crates/mc-vcs/src/{forgejo.rs,gitlab.rs}`（anchor 建桩，切片原地填充） | M8-2 | — |
| `crates/mc-vcs-github/src/{lib.rs,port.rs,ghsnapshot/mod.rs}` | M8-0 | M8-1/4/5 |
| `crates/mc-vcs-github/src/{app.rs,token_cache.rs,rest.rs,dto.rs,ghsnapshot/client.rs}` | M8-1 | M8-4 / M8-5 |
| `crates/mc-vcs-github/src/{payload.rs,webhook.rs,mirror.rs,closepolicy.rs,links.rs}` | M8-4 | — |
| `crates/mc-vcs-github/src/ghsnapshot/{snapshot.rs,refresh.rs}` | M8-5 | — |
| `crates/mc-composio/src/{lib.rs,client.rs}` | M8-0 | M8-6 |
| `crates/mc-composio/src/{service.rs,state.rs,catalog.rs,overlay.rs}` | M8-6 | — |
| `crates/mc-core/src/{vcs.rs,github.rs,composio.rs}` | M8-0 | 各片 |
| `crates/mc-core/src/mcp.rs` | M8-0 | M8-3 |
| `crates/mc-core/src/mcp/overlay.rs` | M8-3 | M8-4（PR 卡片注入？否——只读契约） |
| `crates/mc-repos/src/vcs/{mod.rs,connection.rs,pull_request.rs,commit_status.rs}` | `mod.rs` = M8-0；三个子文件 = M8-2 | — |
| `crates/mc-repos/src/github/{mod.rs,installation.rs,pull_request.rs,check_suite.rs,pending.rs}` | `mod.rs` = M8-0；`installation.rs`/`pull_request.rs` = M8-1；`check_suite.rs`/`pending.rs` = M8-4 | M8-1 读 M8-4 的？否——**同向**：M8-4 读 M8-1 的两个；M8-5 读 M8-1 的 `pull_request.rs` |
| `crates/mc-repos/src/mcp/{mod.rs,workspace_server.rs,agent_binding.rs}` | `mod.rs` = M8-0；两个子文件 = M8-3 | M8-3 |
| `crates/mc-repos/src/composio/{mod.rs,connection.rs}` | `mod.rs` = M8-0；`connection.rs` = M8-6 | — |
| `crates/mc-repos/src/task/overlay.rs` | M8-0（建桩：`attach_runtime_mcp_overlay` 的签名 + `todo!()`） | M8-3 只读；实现归 M8-INT 之后的尾账（R-M8-9） |
| `crates/mc-http/src/routes/{github,vcs,mcp,composio}/mod.rs` | M8-0（聚合 4 个子 router，anchor 期全空） | — |
| `crates/mc-http/src/routes/github/{install.rs,setup.rs,dto.rs}` | M8-1 | M8-4 |
| `crates/mc-http/src/routes/github/{webhook.rs,issue_pr.rs}` | M8-4 | — |
| `crates/mc-http/src/routes/vcs/{connections.rs,webhook.rs,dto.rs}` | M8-2 | — |
| `crates/mc-http/src/routes/mcp/{workspace.rs,agent.rs}` | M8-3 | — |
| `crates/mc-http/src/routes/composio/{callback.rs,connect.rs,catalog.rs}` | M8-6 | — |
| `crates/mc-http/src/state.rs` / `routes/mod.rs` / `routes/mount.rs` / `routes/auth.rs` / `routes/issues/mod.rs` / `Cargo.toml` | M8-0（此后**冻结**） | 各片只读 |
| `apps/mc-server/src/integrations.rs` | M8-0（建桩）→ **M8-5 填** | — |
| `apps/mc-server/src/{main.rs,Cargo.toml}` / `Cargo.lock` | M8-0 | — |
| `docs/61-M8-PLAN.md` / `docs/fixtures/m8-declared-routes.tsv` | **本片（`LUM-1796`）** | 各片只读 |
| `docs/32` §9.12 | M8-0 | 各片追加自己的编号段落（低危，见 §3.2） |

> **同 stage 内两片可以读同一张 DB 表，但必须走各自的本地文件**。
> **记法纪律（承接 `docs/57` §3.2 / `docs/60` §3.3）**：写集一律写**逐字路径**，禁 glob / 花括号 / 「某某段」。

---

## 4. 切片表（派发用）

### 4.1 全景（**1 anchor + 6 代码片 + 1 INT = 8 个 issue**）

| # | 切片 | 路由 | 上游行数 | 上游组成 | stage | 硬前置 |
|---|---|---:|---:|---|---|---|
| M8-0 | anchor（骨架 + 契约位 + 密钥端口接线 + 宿主位 + 占位搬运） | 0 | — | — | 1 | **M6 全合**（`LUM-1673`+`LUM-1675`）→ **M7-0（`LUM-1765`）合入** |
| M8-1 | GitHub App 安装/回调/仓库浏览（含 App JWT 与 installation token 缓存、ghsnapshot 客户端） | 5 | **1,258** | `github.go` L1–L963 (963) + `ghsnapshot/client.go` (295) | 2 | M8-0 |
| M8-2 | VCS provider 抽象 + 连接管理 + 入站 webhook | 5 | **1,320** | `vcs.go` (336) + `vcs_webhook.go` (335) + `integrations/vcs/*` (649) | 2 | M8-0 |
| M8-3 | MCP 服务器库 + agent 绑定 + per-task overlay 纯函数 | 8 | **883** | `workspace_mcp_api.go` (530) + `workspace_mcp.go` (193) + `mcp_overlay.go` (160) | 2 | M8-0 |
| M8-4 | GitHub 入站 webhook + PR 镜像 + 自动关联/关闭 + issue↔PR 读面 | 2 | **1,034** | `github.go` L964–L1997 | 3 | M8-0/1（读 `dto.rs`） |
| M8-5 | ghsnapshot 快照管道（GraphQL 解析 + worker/限流/退避 + 宿主） | 0 | **852** | `ghsnapshot/snapshot.go` (290) + `ghsnapshot/refresh.go` (562) | 3 | M8-0/1（读 `ghsnapshot/client.rs`） |
| M8-6 | composio（SDK 客户端 + 服务 + state HMAC + toolkit 目录 + overlay 构建） | 5 | **1,279** | `integrations_composio.go` (229) + `integrations/composio/*` (1,050) | 3 | M8-0 |
| M8-7 | INT（集成、快照刷新与缺口登记） | 0 | — | — | 4 | 全波 |

**路由账**：5 + 5 + 8 + 2 + 0 + 5 = **25** ✓
**行数账**：1,258 + 1,320 + 883 + 1,034 + 852 + 1,279 = **6,626** ✓（每片 ≤ **3,500**，最大 M8-2 = 1,320）
**缺口账**：本波关掉 24 条 `known_gap`（5+5+8+1+0+5）＋把 1 条占位换成真实现（`pull-requests`）✓

> ★ **M8 是本仓六轮里唯一「每片都远低于 3.5k 上限」的一波**（最大片 1,320 行，是 M7 最大片的 38%）。
> 原因不是 M8 小，而是它的上游面**天然分成四个互不相关的子系统**（GitHub / VCS / MCP / composio）。
> ⇒ 切片粒度按**子系统 × 生命周期**切（请求内 vs 后台 worker），不按行数凑。

### 4.2 离线替身方案（**每片端到端证据的承担者**）

`plan1.md` §5 W8 行的门禁逐字是「**PR/分支快照 fixture**」。GitHub / composio / 自建 Git 实例**都无法在 CI 真连**，
判据必须**离线可复现**：**本地平台替身 + 真实 wire 帧 + 真库**。好消息：**上游自己已经留好了接缝**：

| 面 | 承担端到端证据的片 | 替身接缝（上游证据） | 替身纪律 |
|---|---|---|---|
| GitHub App 安装/浏览 | **M8-1** | `github.go:34-36` 的 `var githubAPIBase = "https://api.github.com"`，注释逐字「**Mutable so tests can**…」 | 本地 HTTP 服务端按 REST 形状答 `/app/installations/{id}`、`/app/installations/{id}/access_tokens`、`/installation/repositories`；断言链 = 路由 → App JWT 生成 → token 交换 → 分页列表 → 真库 |
| GitHub 入站 webhook + PR 镜像 | **M8-4** | 同上 + `verifyWebhookSignature`（`github.go:1096`） | 本地发**真实 HMAC-SHA256** 头的帧（installation/pull_request/check_suite 三族），断言 PR 行 + issue 关联 + 自动关闭 + 快照入队 4 个结果 |
| ghsnapshot 管道 | **M8-5** | `ghsnapshot/client.go:64-65` 的 `apiBase` 字段 + `defaultAPIBase`（`client.go:40`） | 本地 GraphQL 服务端答 `statusCheckRollup`；断言 `PRSnapshot::Decided` / 限流暂停 / 退避时间序列（注入 `Now`） |
| VCS 连接 + webhook | **M8-2** | 每连接自带 `instance_url`（`vcs.go:53` 的 `NormalizeInstanceURL`） | 本地 Forgejo/GitLab 替身（`/api/v1/user`、`/api/v4/user`）；三种签名方案各一条正例 + 一条反例 |
| MCP 库 | **M8-3** | **无需平台替身**（全部是本地库语义） | 证据 = 真库 CRUD + 校验反例（非法 transport / 重名 / write-only 断言）+ overlay 合并的纯函数用例 |
| composio | **M8-6** | `pkg/composio` 的 `Options{APIKey}`（本地 API base 可注入） | 本地 HTTP 替身：toolkits / auth_configs / connected_accounts；断言 state HMAC 四反例（篡改、过期、重放、错密钥） |

**替身三条纪律（与 `docs/60` §4.2 同款，写进上表各片 DoD）**：
① **只替平台 wire，不替业务路径**（替身是「假 GitHub」，不是「假 service」）；
② 帧/响应**逐字段**比对（入站断言归一化结果、出站断言替身收到的原始请求头与体）；
③ **两个反例必测**（验签失败 ⇒ 401 且**不落库**；重复投递 ⇒ 幂等且不重复插行）。

### 4.3 波次（并发 ≤3；stage 内三片可并行，上一 stage 未合不进下一 stage）

```
stage 1  M8-0
stage 2  M8-1 ∥ M8-2 ∥ M8-3
stage 3  M8-4 ∥ M8-5 ∥ M8-6
stage 4  M8-7
```

**串行链（必须写清，避免同 stage 内抢同一文件）**：

1. **`M8-1 → M8-4`**：`crates/mc-http/src/routes/github/dto.rs`（响应映射）与
   `crates/mc-vcs-github/src/ghsnapshot/client.rs` 由 M8-1 写、M8-4 只读 ⇒ M8-4 进 stage 3。
2. **`M8-1 → M8-5`**：`ghsnapshot/client.rs` 的 `Client` 是 M8-5 的 `FetchPRSnapshot` 的唯一注入点 ⇒ 同上。
3. **`M8-0 → 全部`**：`state.rs` / `mount.rs` / 4 个聚合 `mod.rs` / 3 个 crate 骨架 / `port.rs` 的 trait 全在 anchor。
4. **`M8-2` 内部**：`forgejo.rs` / `gitlab.rs` 由 M8-2 自己写；它与 M8-1/M8-3 **零文件交集**，所以三片可同 stage。

> 与 M7 的 9 个 stage 相比 M8 只有 4 个 stage：**这不是「M8 更小」而是「M8 的依赖链更浅」**——
> 它的四个子系统彼此独立，唯一的中枢是 anchor（trait/port/state）。这条差异也是 §7 并行裁定的依据之一。

---

## 5. M8-0 anchor：逐文件预扩展清单

| 文件 | 动作 | 关键点 |
|---|---|---|
| `crates/mc-vcs/Cargo.toml` | 新建 | 依赖：`mc-core` / `mc-errors` / `mc-secrets` / `mc-telemetry` + `reqwest` / `hmac` / `sha2` / `hex` / `url` / `serde` / `serde_json` / `thiserror` / `tracing` / `async-trait` / `tokio`。**零新外部包** |
| `crates/mc-vcs-github/Cargo.toml` | 新建 | 追加 `mc-repos` + `base64` + **`ring`（或 `rsa`）**；**这是本 anchor 唯一可能新增的依赖边**（§2.4 R-M8-8） |
| `crates/mc-composio/Cargo.toml` | 新建 | 追加 `mc-repos` + `base64` / `url` |
| `crates/mc-vcs/src/lib.rs` | 新建骨架 | `pub mod {provider,events,registry,signature,forgejo,gitlab};`，只放 trait/类型位与**空实现** |
| `crates/mc-vcs/src/provider.rs` | 建（类型位） | `Provider` trait 6 方法（`kind` / `event_kind` / `verify_signature` / `parse_pull_request` / `parse_ci_status` / `validate_token`）+ `ErrUnauthorized`。**不写任何 provider 分支** |
| `crates/mc-vcs/src/{forgejo.rs,gitlab.rs}` | 建**桩** | 各 1 个 `pub fn register()` 空体（M8-2 原地填充；anchor 之后不再有第二个写者） |
| `crates/mc-vcs-github/src/{lib.rs,port.rs}` | 新建骨架 + **port 先定** | `port.rs`: `PrRefreshPort` trait（`enqueue` / `maybe_enqueue_on_view` / `enabled`）+ `GithubAppConfig` 形状。实现在 M8-5 |
| `crates/mc-vcs-github/src/{app.rs,token_cache.rs,rest.rs,dto.rs,payload.rs,webhook.rs,mirror.rs,closepolicy.rs,links.rs}` | 建**桩** | 每文件只放签名与 `todo!()` 位（**anchor 不实现**）；`dto.rs` 的响应类型可以是完整形状（切片只填函数体） |
| `crates/mc-vcs-github/src/ghsnapshot/{mod.rs,client.rs,snapshot.rs,refresh.rs}` | 建**桩** | `client.rs` 放 `Client` 的字段与 `api_base` 接缝（**这是离线替身的关键**） |
| `crates/mc-composio/src/{lib.rs,client.rs,service.rs,state.rs,catalog.rs,overlay.rs}` | 建骨架/桩 | `client.rs` 放 `api_base` 接缝；`overlay.rs` 放 `build_task_overlay` 签名 |
| `crates/mc-core/src/{vcs.rs,github.rs,mcp.rs,composio.rs}` | 新建（**完整类型形状**） | `VcsProviderKind`/`VcsConnection`、`PullRequestSnapshot`/`IssuePrLink`、`WorkspaceMcpServer`/`McpTransport`/`McpBinding`、`ComposioConnection`。**不动既有枚举** |
| `crates/mc-core/src/lib.rs` | +4 行 `pub mod` | — |
| `crates/mc-repos/src/{vcs,github,mcp,composio}/mod.rs` | 建模块树 + `pub use` | `crates/mc-repos/src/lib.rs` +4 行 |
| `crates/mc-repos/src/task/mod.rs` | +1 行 `pub mod overlay;` | `task/overlay.rs` 建桩（`attach_runtime_mcp_overlay` 签名 + `todo!()`） |
| `crates/mc-http/src/routes/{github,vcs,mcp,composio}/mod.rs` | 建（聚合各自子 router） | 子文件 anchor 期是**空 `Router::new()`** ⇒ 写者各写自己的 |
| `crates/mc-http/src/routes/github/issue_pr.rs` | 建 + **承接搬运过来的 501 占位** | handler 名保持 `not_implemented`（`crate::routes::issues::not_implemented` 已是 `pub(crate)`）⇒ ⑦ 的 `implemented_placeholder` **不变** |
| `crates/mc-http/src/routes/mod.rs` | +4 `pub mod` | — |
| `crates/mc-http/src/routes/mount.rs` | +`mount_slice_code_artifacts()` + `.merge(...)` 一行 | **anchor 期 4 个子 router 全空 + 占位搬运 ⇒ 注册键逐字不变** ⇒ ⑦ 无变化 |
| `crates/mc-http/src/state.rs` | 加 `vcs` / `github` / `composio` 三组字段 + 唯一出口 | **不新增 `AppState::new` 参数**；在构造体内读 5 个 env（`PluginSecretKey::from_env` 先例）；`github` 的 App 私钥用 `secrecy` 风格的手写 `Debug` |
| `crates/mc-http/src/routes/auth.rs` | 测试 `AppState { … }` 字面量补字段 | 全仓唯一字面量构造点 |
| `apps/mc-server/src/integrations.rs` | 新建 | `pub struct IntegrationHandles` + `pub async fn start(...)`（**anchor 期空跑**：无密钥 ⇒ 返回空 handle） |
| `apps/mc-server/src/main.rs` | +`mod integrations;` + 调用 + 停机链一行 | 停机顺序：**先停渠道连接 → 再停 PR 刷新 → 再停调度器 → 最后停 actor** |
| `apps/mc-server/Cargo.toml` / `crates/mc-http/Cargo.toml` | 加 `path` 边 | — |
| `Cargo.lock` | 重新生成 | 3 个成员 + ≤1 条外部直连边（§2.4） |
| `docs/32` §9.12 | 追加偏离与口径修订 | 承接 §9 + R-M8-1…R-M8-9，**anchor 一次落** |
| `docs/fixtures/route-parity-baseline.json` | **不动** | M8-0 零路由删除；刷新归 M8-7 |

> 纪律（与 M5-0 / M6-0 / M7-0 相同）：anchor **不实现任何路由逻辑**、**不写任何平台 wire 代码** ——
> 只固定 trait、module 边界、密钥端口、宿主位与账本。anchor 的测试只有三类：
> 编译（`cargo check --workspace --all-targets`）、门 ⑦/⑩ 读数、`provider`/`port` trait 的**形状用例**
> （`Arc<dyn Provider>` 能构造、`Arc<dyn PrRefreshPort>` 默认实现返回 `None`、`AppState` 的 5 个 env 缺省不 panic）。

---

## 6. 门禁与验收

### 6.1 门 ⑦（路由对齐）

| 时点 | local | implemented | known_gap | owners.M8 | baseline | 备注 |
|---|---:|---:|---:|---:|---:|---|
| **`ec83f6e7`（本片起手，实测）** | **405** | **329**（325 real + 4 ph） | **127** | **24** | 344 | local_only 9 |
| `LUM-1675`（M6-INT）后（**预测**） | 406 | 330（326+4） | 126 | 24 | 344 | M6-8 的 `POST /api/plugin-bridge/v1/hooks/{key}` 落地 |
| M7 全波（24 条）后（**预测**） | 430 | 354（350+4） | 102 | 24 | 344 | +24 |
| **M8-0 后** | **430** | **354**（350+4） | **102** | **24** | **344（不动）** | anchor 0 路由、0 占位删除、**1 条占位原地搬运** ⇒ 本波是**第二个不刷基线的 anchor** |
| M8-1 后 | 435 | 359（355+4） | 97 | 19 | 344 | +5 |
| M8-2 后 | 440 | 364（360+4） | 92 | 14 | 344 | +5 |
| M8-3 后 | 448 | 372（368+4） | 84 | 6 | 344 | +8 |
| M8-4 后 | 449 | 373（370+**3**） | 83 | **5** | 344 | +1 新注册；同时 `pull-requests` 占位 → 真实现（real +1 / ph −1） |
| M8-5 后 | 449 | 373（370+3） | 83 | 5 | 344 | 0 路由 |
| M8-6 后 | **454** | **378**（375+3） | **78** | **0** | 344 | +5 |
| **M8-7（INT）后** | 454 | 378 | 78 | 0 | **454** | `--write-baseline` 344 → 454 |

不变式（每片自检）：`implemented + known_gap == 456`、`regressions == 0`、`unclaimed == 0`、`local_only == 9`（M8 不新增 local_only）。
**只有 M8-1/2/3/4/6 五片会动读数**；M8-0、M8-5、M8-7（除基线）必须**逐字不变**。

> ⚠️ 上表 M6-INT / M7 两行是**预测**（它们的片还没落地）。M8 各片起手**必须重取当轮 base sha 与实测读数**，
> 不许直接抄本表（`docs/37` §46 的 lesson：口径类片合入的瞬间，所有引用旧口径的表都变成过期文书）。

### 6.2 门 ⑨（契约等价）——M8 相关 fixture 现状与目标

```
$ cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json
totals: fixtures 365 · pass 5 · mismatch 23 · unmounted 31 · unevaluable 306
```

按「路径落在 M8 的 25 条路由上」过滤（复算见 §10 命令 4）：**只有 1 条**。

| fixture | 路由 | 现状 | 期望 | 由哪片转绿 |
|---|---|---|---|---|
| `integrations/TestComposioCallbackIsPublic_NoCookieNot401@server/cmd/server/composio_callback_public_test.go:25#1` | `GET /api/integrations/composio/callback` | `unmounted`（`status_observed` 404，`status_expected` **401**） | **401**（匿名 + 错 state 时**不是** 404，也不是「缺 cookie 就 401」） | **M8-6** |

⇒ 本波**承诺 1 条 `unmounted → pass`**。**其余 24 条路由在上游 fixture 语料里没有对应项**
（`report.json` 的 `domain` 直方图里根本没有 `github`/`vcs`/`mcp`/`pull_requests`；
`integrations` 域只有这 1 条）⇒ 本波**不承诺**提高 `contract_equivalence_rate`（当前 0.0137），
也**不制造** `unevaluable` 条目。

**取而代之的验收判据**：本波每片**自造离线 fixture** 落到 `contracts/golden/`（不计入上游 365 的 pass 率，
避免污染契约等价率），覆盖四类**离线可判**语义（§7 的 DoD 逐片点名）：

| 类 | 条数 | 内容 |
|---|---:|---|
| 未配置 / 未授权矩阵 | 12 | §2.5 的「未配置 + 未授权」两格，逐端点 |
| 凭据与验签反例 | 8 | GitHub HMAC 差 1 位 / VCS 三方案各 1 反例 / composio state 四反例 |
| write-only 与 redaction | 5 | MCP 响应不含值字段；错误路径不回显密钥；`Debug` 脱敏 |
| 幂等与竞态 | 4 | webhook 重复投递、revoke-webhook 轮换、token 缓存单飞、快照重入 |

### 6.3 门 ⑩（文件大小）——预飞

门 ⑩ 只扫 `git ls-files` 的代码文件（`docs/**` 不查），规则「只减不增」，清单外硬限 **800 行**。
M8 写集里**没有任何文件**在 `scripts/file_size_baseline.tsv`（该表只剩 10 个条目）⇒ **全部走 800 行硬限**。预飞：

| 本地文件 | 当前 | 计划 | 风险与对策 |
|---|---:|---|---|
| `crates/mc-http/src/state.rs` | 530 | ≈690（M7-0 与 M8-0 **各加一块**） | **中危**：两次 anchor 共加 ~160 行 ⇒ 若 M7-0 后已 >620，M8-0 必须把「读 env + 构造」下放到 `crates/mc-http/src/state/integrations.rs`（新文件，anchor 建） |
| `crates/mc-repos/src/task/store.rs` | 782 | **不动**（R-M8-9 的独立 `UPDATE` 走 `task/overlay.rs`） | 安全（避免踩线） |
| `crates/mc-http/src/routes/mount.rs` | 341 | ≈368 | 安全 |
| `crates/mc-http/src/routes/mod.rs` | 89 | ≈97 | 安全 |
| `crates/mc-http/src/routes/issues/mod.rs` | 253 | 252（**减 1 行**：占位搬运出去） | 安全（且方向是**减少**） |
| `apps/mc-server/src/main.rs` | 213 | ≈235 | 安全 |
| `crates/mc-http/src/routes/github/webhook.rs` | — | 400–700 | 上游 `HandleGitHubWebhook` + 事件分派共 ~500 行 ⇒ 按「验签 / 事件分派 / 每事件处理」拆到 `payload.rs`/`webhook.rs`/`mirror.rs` |
| `crates/mc-vcs-github/src/mirror.rs` | — | 600–800 | 上游 `mirrorPullRequestForWorkspace` 单函数 ~200 行 + 关联/关闭 ~300 行 ⇒ 与 `links.rs`/`closepolicy.rs` 分文件 |
| `crates/mc-vcs-github/src/ghsnapshot/refresh.rs` | — | 500–700 | 上游 562 行 ⇒ 与 `snapshot.rs`（290）分文件（已按此设计） |
| `crates/mc-composio/src/service.rs` | — | 600–800 | 上游 709 行 ⇒ 按「连接生命周期 / toolkit 目录 / auth-config 解析」拆到 `catalog.rs` |
| `crates/mc-http/src/routes/mcp/workspace.rs` | — | 400–600 | 上游 530 行含 8 条 handler ⇒ 与 `agent.rs` 分文件，且 DTO 内联（不另建文件） |

⇒ **四片必须在实现前先拆分**：M8-4（`mirror.rs`）、M8-5（`refresh.rs`）、M8-6（`service.rs`）、M8-1（`rest.rs`）。
拆分是**回归上游结构**（上游 `*_test.go` 与实现文件同目录分文件），不是凑门。

### 6.4 迁移条数 = **0**

14 张 M8 面表**全部已在** `migrations/upstream/`（复算见 §10 命令 5；上游 `server/migrations/*.up.sql` = **560 个**，本地 `migrations/upstream/*.up.sql` = **560 个**）：

| 族 | 表 | 张数 |
|---|---|---:|
| GitHub App / PR 快照 | `github_installation`、`github_pending_installation`、`github_pull_request`、`github_pull_request_check_suite`、`github_pull_request_check_run`、`github_pending_check_suite`、`issue_pull_request` | 7 |
| VCS（token 型自建 Git） | `vcs_connection`、`vcs_pull_request`、`issue_vcs_pull_request`、`vcs_commit_status` | 4 |
| MCP 服务器库 | `workspace_mcp_server`、`agent_mcp_server` | 2 |
| composio | `user_composio_connection` | 1 |

⇒ 本波**不写任何 `migrations/**`**，也不刷 `contracts/upstream-apply-exceptions.tsv`。门 ⑧（schema-drift）在 INT 片跑一次即可。

> 注：`plugin_remote_mcp_oauth_state` / `plugin_remote_mcp_secret` 也是 MCP 相关表，但它们是 **M6 的插件远程 MCP 面**
> （`plugin_*` 前缀 + `mc-repos/src/plugin/` 已有读写），**不在 M8 写集内**（§2.3 的禁改清单）。

### 6.5 每片 DoD（通用 + 专属）

**通用（每片都跑，命令见 §10 命令 6）**

1. `bash scripts/gates.sh` **8/8 绿**；触碰 DB 的片（M8-1/2/3/4/5/6 全部）追加 `--with-db` **10/10**。
2. ⑦ 读数与 §6.1 该片行一致（`implemented + known_gap == 456`、`regressions == 0`、`local_only == 9`）。
3. 形态门：本片不得引入 `MISSING_ALIAS` / `MISSING_EXACT` / `EXTRA_ALIAS`（M8 无欠账 ⇒ 三类都是硬失败）。
4. ⑩：新文件 ≤800 行；`scripts/file_size_baseline.tsv` 不动或缩小。
5. 每条路由至少一条测试（handler 级或 e2e），且**不用** `health::placeholder`。
6. 凭据面：手写 `Debug` 脱敏 + 「错误路径不回显凭据」用例（§2.4）。
7. 偏离（无 Redis 的单副本假设 / 未接线项 / 替身范围）必须写进 `docs/32` §9.12 的**自己那一段**（编号起手复核）。

**专属**

| 片 | 专属验收 |
|---|---|
| M8-0 | ⑦ 读数逐字不变（`430/354/102`，baseline 344 不动）；`cargo metadata` 通过且 3 个 crate 是成员；`pull-requests` 占位**搬运后注册键不变**；`AppState` 的 5 个 env 缺省不 panic；RS256 依赖边选定并实测签发-验证往返（§2.4） |
| M8-1 | 5 条路由的「未配置 + 未授权」矩阵（§2.5）；App JWT 签发/过期/时钟偏移；installation token 缓存 + **单飞刷新**（并发 8 个请求只换 1 次 token）；仓库分页（`parseGitHubPageParam` 边界）；**离线替身端到端**（§4.2） |
| M8-2 | `Provider` trait 三实现检验（forgejo/gitlab 两实现 + 未注册 provider 报错）；**三种签名方案各 1 正例 + 1 反例**；`rotate-webhook` 旧 secret 立刻失效、新 secret 立刻生效；per-connection secret 落库 = `secretbox` **密文**（明文入库即失败）；5 条路由的「产品边界 vs 未配置」两层语义 |
| M8-3 | 8 条路由；MCP 条目 **write-only** 断言（响应 JSON 里 `headers`/`env` 的值一个字节都不出现）；重名拒绝（`316` 的唯一约束）；agent 绑定的 `enabled` 开关幂等；**overlay 合并纯函数**与既有的合并语义逐条一致（`docs/32` §9.10 的对照表） |
| M8-4 | **离线替身端到端**（§4.2）；三族 webhook 事件（installation / pull_request / check_suite）各一条；PR 自动关联 identifier 的 6 个边界（`extractIdentifiers`/`extractClosingIdentifiers`）；自动关闭的三态策略（`closeIntentPolicy`）；`pull-requests` 从 501 → 真实现且**路由仍在**；幂等（同一 webhook 重投 2 次只插 1 行 PR） |
| M8-5 | **离线 GraphQL 替身**；`PRSnapshot::Decided` 的三态；限流暂停（`RateLimitError` ⇒ `rateLimitPause`）；退避时间序列（注入 `Now`）；worker 池的并发上限与 TTL sweep；**停机链**（shutdown 后 worker 在 N 秒内退出） |
| M8-6 | **⑨ 那 1 条 fixture 转 pass**（匿名 + 错 state ⇒ **401**）；composio 四种「未配置」语义（§2.5）；state HMAC 四反例（篡改 / 过期 / 重放 / 错密钥）；toolkit 目录的动态解析（auth-config 未配 ⇒ 该 toolkit 不出现）；**离线替身端到端**（connect → callback → 落库 → toolkits） |
| M8-7 | 快照三件套刷新（⑦/⑨/⑩）+ `--write-baseline`（344 → 454）+ 缺口登记；**无代码改动**；登记项至少含：附件面 9 条（W8 尾账，§9.2）、composio overlay 的 3 处 enqueue 接线（R-M8-9）、M8 面剩余 `unevaluable` 复核 |

---

## 7. 晋升顺序与前置（含 **M7 ∥ M8 槽位分配表**）

### 7.1 硬前置链

1. **M6 全合**：`LUM-1673`（M6-8）与 `LUM-1675`（M6-INT）落地 —— 理由与 `docs/60` §7.1 相同（共享受护文件 + 基线刷新归 INT）。
2. **`LUM-1765`（M7-0 anchor）合入 —— 这是 M8-0 的**额外**硬前置**，三条理由：
   (a) 两个 anchor 争同一批共享文件（`state.rs` / `routes/{mod,mount}.rs` / `mc-core/src/lib.rs` / `mc-repos/src/lib.rs` /
   `routes/auth.rs` / `apps/mc-server/src/main.rs` / `Cargo.lock`，§3.2 逐文件）；
   (b) **`crates/mc-secrets/src/secretbox.rs` 由 M7-0 建**，M8-2 的 VCS 密文面**只读复用**，M8 **不得**建第二份；
   (c) M8-0 要读**刷新后**的 base sha 与读数（`docs/37` §46 的 lesson）。
3. **M8-1 是 M8-4 / M8-5 的前置**（`dto.rs` 与 `ghsnapshot/client.rs`）。
4. **M8-7 是全波收口**。

### 7.2 单值结论：**M7 ∥ M8 并行**（不是「M7 独占 3 槽」）

**依据四条，逐条可复算**：

1. **DAG 成立**：`plan1.md` §5 的 gantt 写 `W8 after w5`（`w7 after w6` 是**另一条分支**）。W5（M5 波）**已收口**
   （`owners.M5 = 0`）⇒ W8 的前置条件**现在就已满足**，不存在「等 W7」的硬依赖。
2. **零产物依赖**：M8 的 6 个代码片**不消费任何 M7 产物**（逐文件证据：M7 写 `mc-channel` / `routes/channels` /
   `mc-repos/src/channel` / `mc-core/src/channel*`；M8 写 `mc-vcs*` / `mc-composio` /
   `routes/{github,vcs,mcp,composio}` / `mc-repos/src/{vcs,github,mcp,composio}` —— **交集为 ∅**，唯一例外
   `mc-secrets/src/secretbox.rs` 是 **M7-0 建、M8 只读**）。渠道面与代码制品面在**领域上无关**
   （一个是出站长连接 + 安装绑定，一个是入站 webhook + PR 快照 + 工具集成）。
3. **关键路径说**：`plan1.md` §5 的 DAG 里 `W9 after w8`（W9 商业面是 W8 的后继），而**没有任何波次以 W7 为硬前置**
   ⇒ **W8 在关键路径上、W7 不在**。让 M8 等到 W7 之后，等于把关键路径上的 3 周推到第 13 轮。
4. **槽位预算**：M8 8 个 issue（anchor 1 + 代码 6 + INT 1）、M7 22 个。3 槽下界 = `ceil(30/3) = 10` 轮；
   **串行两波 = M7 的 9 个 stage + M8 的 4 个 stage = 13 轮**（两波之间还有一次基线/口径交接）。
   ⇒ 并行**净省约 3 轮**，且省下的正是 M8 的 4 个 stage。

**槽位分配（单值）**：**M7 保 2 槽、M8 保 1 槽**；M7 的**空槽轮**（它的 stage 8 只有 2 片、stage 9 只有 1 片）
M8 可临时占用到 2–3 槽。

| 轮 | M7（保 2 槽） | M8（保 1 槽） | 说明 |
|---|---|---|---|
| R1 | M7-0 | — | M8-0 被前置拦住（§7.1.2） |
| R2 | M7-1 ∥ M7-2 | **M8-0** | M7-3 依赖 M7-1/2 ⇒ 第 3 槽给 M8-0；**M8-0 落 `docs/32` §9.12 时避开 M7 各片的 §9.x 同轮**（§3.2 的低危项） |
| R3 | M7-3 ∥ M7-4 | **M8-1** | M8-1 与 M7-3/4 零文件交集 |
| R4 | M7-5 ∥ M7-6 | **M8-2** | — |
| R5 | M7-7 ∥ M7-8 | **M8-3** | — |
| R6 | M7-9 ∥ M7-10 | **M8-4** | M8-4 的硬前置（M8-1 已合）满足 |
| R7 | M7-11 ∥ M7-12 | **M8-5** | — |
| R8 | M7-13 ∥ M7-14 | **M8-6** | — |
| R9 | M7-15 ∥ M7-16 ∥ **M8-7** | — | M8 收口（INT 与 M7 的两片可同轮，三条零交集） |
| R10–R11 | M7-17…M7-21 | — | M8 已完结，M7 回满 3 槽 |

> ⚠️ 上表是**基线排法**，不是硬约束：真实派发时以「**谁有空位谁起、M8 每轮至少 1 槽**」为准；
> 若 M7 某轮只有 1–2 片可派（依赖未满足），M8 可临时占 2 槽。
> **唯一硬规则**：M8-0 与 M7-0 **不得同飞**（同一批共享文件 + `Cargo.lock`）。

### 7.3 子 issue 一览（全部 `backlog`，`--parent LUM-1796`，stage 与 §4.1 一一对应）

| # | 切片 | 子 issue | stage | 路由 | 上游行数 |
|---|---|---|---|---:|---:|
| 1 | M8-0 anchor | **LUM-1797** | 1 | 0 | — |
| 2 | M8-1 GitHub App 安装/浏览 | **LUM-1798** | 2 | 5 | 1,258 |
| 3 | M8-2 VCS provider + 连接 + webhook | **LUM-1799** | 2 | 5 | 1,320 |
| 4 | M8-3 MCP 库 + agent 绑定 + overlay | **LUM-1800** | 2 | 8 | 883 |
| 5 | M8-4 GitHub webhook + PR 镜像 | **LUM-1801** | 3 | 2 | 1,034 |
| 6 | M8-5 ghsnapshot 管道 | **LUM-1802** | 3 | 0 | 852 |
| 7 | M8-6 composio | **LUM-1803** | 3 | 5 | 1,279 |
| 8 | M8-7 INT | **LUM-1804** | 4 | 0 | — |

（路由账 5+5+8+2+0+5 = 25 ✓；stage 分布 1/3/3/1 = 8）

> 晋升规则与 M5/M6/M7 相同：`backlog → todo` 才起跑；同 stage 内三片可并行；上一 stage 未合不进下一 stage。
> ⚠️ **派发提示**：M8-0 已有 assignee（本 agent）⇒ 一条 `status todo` 即起 run；
> M8-1…M8-7 **无 assignee** ⇒ 必须 `assign --to-id 3c6087f9-f768-45a0-9b07-979f7d4fabf5`（照 `docs/60` §7.4 的教训）。

---

## 8. 风险登记（每条对应一个 DoD 或一个「登记不实现」的决定）

| ID | 风险 | 缓解 / 决定 |
|---|---|---|
| **R-M8-1** | **平台不可在 CI 真连**（GitHub API/GraphQL、composio、自建 Git）⇒ 「PR/分支快照 fixture」门禁可能被"假绿"绕过 | §4.2 的替身三条纪律 + 上游**自带的接缝**（`githubAPIBase` 包级变量、`ghsnapshot.apiBase` 字段、VCS 的 per-connection `instance_url`、composio 的 API base）；6 片 DoD 逐条点名承担者 |
| **R-M8-2** | **凭据面最大**：4 类部署密钥 + 3 种密钥形态（env 明文 / secretbox 密文 / App 私钥 PEM） | §2.4 四类密钥逐条 + redaction 四条判据 + 「写只写、读不回明文」断言；VCS 的每连接 secret 明文入库即失败 |
| **R-M8-3** | **installation token 刷新竞态**：并发请求同时发现过期 ⇒ 打爆 token 端点、或互相覆盖缓存 | `token_cache.rs` 的**单飞**（`tokio::sync::Mutex` / `OnceCell`）+ 过期余量（提前 N 秒）+ DoD 的「并发 8 请求只换 1 次」用例 |
| **R-M8-4** | **webhook 重放与验签**：GitHub HMAC-SHA256、Forgejo HMAC、GitLab **明文 token 比较**（若用 `==` 则非常量时间） | §2.7 第 3 条（常量时间比较）+ 每方案 1 反例 + 幂等（同 payload 重投只插 1 行）；**不做**时间戳窗口（上游也没做，登记为有意的等价而非缺口） |
| **R-M8-5** | **ghsnapshot 限流与退避**：GraphQL 配额耗尽会让 PR 卡片永久停更；worker 是**跨副本**语义的（上游无 Redis 时靠单副本假设） | `RateLimitError` ⇒ `rateLimitPause`/`deferActive`/`scheduleRetry` 三级；与 M7 的 R-M7-1 **同源**：本仓无 Redis ⇒ **单副本部署契约**（登记在 `docs/32` §9.12） |
| **R-M8-6** | **MCP 条目是 write-only 的**，但 `workspace_mcp_server.config` 是 JSONB ⇒ 很容易在列表响应里把 `headers`/`env` 原样吐出去 | §2.7 第 5 条 + M8-3 的 DoD（响应 JSON 里值字段一个字节都不出现）+ 一条反例测试 |
| **R-M8-7** | **`state.rs` 逼近 ⑩ 硬限**：它是唯一被 M7-0 与 M8-0 **两个** anchor 追加的文件（当前 530 行） | §6.3 预飞：两次 anchor 共加 ~160 行；若 M7-0 后 >620，M8-0 把「读 env + 构造」下放到 `crates/mc-http/src/state/integrations.rs`（新文件） |
| **R-M8-8** | **无 `jsonwebtoken` 依赖**：GitHub App JWT 要 RS256（PKCS#1 v1.5）签名 | 裁定用**已在 `Cargo.lock` 里**的 `ring 0.17.14`（或 `rsa 0.9.10`），**不新增外部包**；anchor 一次性加直连边并在 `docs/32` §9.12 登记选择与实测（§2.4） |
| **R-M8-9** | **composio overlay 只交付到「可注入」为止**：本地没有上游 `service/task.go` 那样的**中心 enqueue 函数**，task 行由 3 处 INSERT 分散创建（`mc-repos/src/task/store.rs:216`、`chat_task/send.rs:136`、`autopilot/run.rs:575`），上游的 overlay 注入点在本仓**没有对应物** | 裁定：M8-3 交付 `mc-core/src/mcp/overlay.rs` 的**纯函数** + `mc-repos/src/task/overlay.rs` 的 `attach_runtime_mcp_overlay` **写入原语**；M8-6 交付 composio 服务侧的 session URL 获取；**「3 处 enqueue 接线」不在本波写集内**（会连锁改 `NewTask` 的每个字面量构造点），由 M8-7 登记为**明确尾账**并给出 file:line 清单 ⇒ 「`runtime_mcp_overlay` 恒 NULL」在本波结束后**仍然成立**，这是**登记过的缺口**，不是遗漏 |
| **R-M8-10** | **`docs/32` 是 M7/M8 唯一共享的文档写点**（M7 各片追加 §9.x，M8-0 追加 §9.12） | 排法上避开同轮（§7.2 的 R2 注）；若真撞上，由后合者 rebase 并保留两段（纯文档，无编译风险） |

---

## 9. 与 `docs/plan1.md` / `docs/15` 的差异（口径修订，逐条）

### 9.1 口径修订一：§1.2 那行 `ghsnapshot 7 / composio 6 / vcs 4` 的单位是「**一切文件数**」

* 原文：`internal/integrations | 153 | 48,907 | wecom 89 / dingtalk 76 / lark 72 / channel 42 / slack 32 / telegram 23 / ghsnapshot 7 / composio 6 / vcs 4`
* 实测：三个数**逐字相符**（`find <dir> -type f | wc -l` = 7 / 6 / 4），单位是「递归的一切文件数（含 `_test.go`）」
  —— 与 `docs/60` §9.1 对同行的裁定**同一口径**。非测试 `.go` 口径是 **3 / 3 / 3 文件 / 1,147 / 1,050 / 649 行**。
* **无数据修订，只有单位钉定**：M8 报数一律两栏并列（「一切文件数 17」+「非测试 9 文件 / 2,846 行」），**不引用**「17」当行数。

### 9.2 口径修订二：**attachments 面维持 `M3+`，登记为 W8 尾账**（文档与 fixture 不一致）

* 冲突：`docs/15-M3-PLAN.md` §580/§581 把附件面 8 行判给「**W8 / M8**（制品与存储）」；
  但 `docs/fixtures/upstream-routes.tsv` 里这些行的 owner 单元格是 **`M3+`**，且是**显式规则**（不是兜底命中）：
  `^/api/attachments → M3+`、`^/api/upload-file → M3+`、`^/api/issues/[^/]+/attachments → M3+`、`^/uploads/ → M3+`、`^/api/avatars/ → M3+`。
* **裁定：维持 `M3+`，本波不并入**。三条理由：
  (a) **改 owner 单元格就会改 ⑦ 读数**（`owners.M8` 24→33、`M3+` 16→7），而本片的硬约束是「交付前后 ⑦ 逐字不变」；
  (b) owner 列是**⑦ 的唯一权威**（`docs/15` §536 的纪律：「**不许**为了好看去改 owner 单元格凑账」）；
  (c) 附件面的能力载体（对象存储 + 签名 URL）与 M8 的四个子系统**零共用**（`mc-storage` 已存在且 M8 不碰它）。
* **影响（对 M8 切片表）**：M8 的账 = **25 条**，切片表**不含**附件面；`plan1.md` §3.3 列的 **`mc-attachment` 本波不建**。
* **登记（尾账，交给 M8-7）**：附件面 **9 条**（比 `docs/15` §580/§581 的 8 条**多 1**：
  两节都漏了 **`DELETE /api/attachments/{id}`**，`router.go:2156`）—— 逐条见 `docs/fixtures/m8-declared-routes.tsv` 的表头注释。
  推荐处理：单独立项（`W8 尾账`），**不塞进 M8 切片**（沿用「无主/未排期缺口 = 立 issue 登记」的先例）。

### 9.3 口径修订三：**`W8 after w5` 成立** ⇒ M7 ∥ M8 并行（§7.2 是单值结论）

* `plan1.md` §5 的 gantt 写 `W8 after w5`，而 §5 的表格把 W8 的上游写成 `integrations/vcs` —— **两处都不错**，但都没说
  「W8 与 W7 能不能并行」。本片补上：**能，而且应该**（四条依据见 §7.2）。
* 附带修订：`plan1.md` §5 未点出 **W8 在关键路径上**（`W9 after w8`）而 W7 不在 ⇒ 派发时应**优先保障 W8 的槽位**。
* 这是本文对 §5 的**唯一实质性排期补充**。

### 9.4 口径修订四：**`mcp-servers` 8 条的账归 M8、能力面属 W6**

* 事实三条：(a) fixture owner = `M8`（显式规则 `^/api/agents/[^/]+/mcp-servers`、`^/api/workspaces/[^/]+/mcp-servers`）；
  (b) `docs/15` §9.5 把 agent 面的 4 条判给 M8（「其余 12 条已由 fixture 划给 M6/M7/M8/M9」）；
  (c) **`crates/mc-http/src/routes/plugins/mcp.rs:39` 逐字写着**「`mcp_overlay.go` 是 per-task agent overlay（**M8 面**），同理不接」
  —— 这是 M6 自己留下的主权凭证。
* **裁定**：**账与实现都归 M8，不迁移 owner**；实现方式是「消费既有 `ResolvedMcpConfig`/daemon 侧合并语义 + 只补 HTTP/存储/overlay 纯函数」（§2.3）。
* 对 ⑦ owner 板的影响：**无**（不迁移即不改读数）。

### 9.5 口径修订五：`plan1.md` §3.3 的 W8 行 **3 个 crate → 实测 3 个 + 1 不建**

* 原文：`W8 | mc-vcs mc-vcs-github mc-attachment`。
* 实测：(a) `mc-attachment` **本波不建**（§9.2）；(b) **新增 `mc-composio`**（判据 1 的第二半：它有两个消费者，§2.2）。
* 净变化：**crate 数不变（3）**，但集合不同 ⇒ 派发时不得按原文找 `mc-attachment`。

### 9.6 口径修订六：§5 W8 行的「上游对应 = `integrations/vcs`」**严重低估**

* 实测：`integrations/vcs` 只有 **4 个文件 / 649 行（非测试 3 文件）**；M8 的上游面是 **6,626 行**
  （handler 3,780 + `{vcs,ghsnapshot,composio}` 2,846）—— 真值在 **`internal/handler`**（`github.go` 一个文件就 1,997 行，
  比整个 `integrations/vcs` 大 3 倍）。
* 这也是 `plan1.md` §5 的 W8 工时「3 周」需要**按本表重估**的理由（本片不给出新工时，只给出可复算的行数与切片数）。

### 9.7 与 M7 计划（`docs/60`）的结构一致性

承接 `docs/60` 的骨架（§0 速览 / §1 测绘 / §2 架构 / §3 写集 / §4 切片 / §5 anchor / §6 门禁 / §7 晋升 / §8 风险 / §9 差异 / §10 复算），
并沿用其四条纪律：**写集逐字路径**、**`unevaluable` 不许改写成 `pass`**、**落笔前 `git fetch` 复核号段**、**base sha 起手重取**。
**与 M6/M7 的三处结构差异**：

1. **⑨ 面几乎为空**（M7 有 12 条落在本波路由上的 fixture，M8 只有 **1 条**）⇒ M8 的验收重心从「转多少条 fixture」移到「自造离线 fixture + 未配置/未授权矩阵」。
2. **anchor 是第二个不刷基线的**（M7-0 是第一个）——但机制不同：M7-0 是**零删除**，M8-0 是**零净变化**（1 条占位原地搬运，注册键与占位计数都不变）。
3. **stage 更浅**（4 个 vs M7 的 9 个）⇒ 同一轮里可以更早把 M8 的片灌满槽位（§7.2）。

---

## 10. 复算命令（全部只读，可在任意 workdir 复现）

```bash
# 前置：本片的上游只读副本（钉住 commit f41fae6b08fb；上游 main tip 是 90e0bdf，必须显式取该 sha）
#   UP_ROOT=<本 run 的 workdir>；上游 = $UP_ROOT/up-m8，本地 = $UP_ROOT/paperclip-rs
#   纪律：副本只克隆进**本 run 的 workdir**，不依赖 /tmp 或别的 run 的 workdir（会被 GC 掉）
cd <workdir> && git clone --depth 1 --single-branch --branch main https://github.com/louloulin/multica up-m8
cd up-m8 && git fetch --depth 1 origin f41fae6b08fb734afcbd13205c0b3203dd0bc9c6 && git checkout -q FETCH_HEAD
git log --oneline -1        # ⇒ f41fae6 fix(cursor): own Windows background shells…
cd ../paperclip-rs && git fetch origin feat/multica-rs-initial && git log --oneline -1   # ⇒ ec83f6e7

# 1. 路由表：本文 §1.1 与上游 owner=M8 行集合相等（应无输出）
diff <(awk -F'\t' '!/^#/ && $3=="M8"{print $1"\t"$2}' docs/fixtures/upstream-routes.tsv | sort) \
     <(grep -v '^#' docs/fixtures/m8-declared-routes.tsv | tail -n +2 | sort)
awk -F'\t' '!/^#/ && $3=="M8"' docs/fixtures/upstream-routes.tsv | wc -l      # ⇒ 25
# 1b. M8 面没有散落在别的 owner（应只输出 M8）——注意 `M3+` 的 attachments 面**不属于** M8（§9.2）
grep -iE '/github|/vcs/|mcp-servers|composio|webhooks/(github|vcs)' docs/fixtures/upstream-routes.tsv | awk -F'\t' '{print $3}' | sort -u

# 2. 上游文件与行数（§1.2）
UP=<clone>/server
wc -l $UP/internal/handler/{github,vcs,vcs_webhook,integrations_composio,workspace_mcp_api,workspace_mcp,mcp_overlay}.go   # ⇒ 1997/336/335/229/530/193/160 = 3780
for d in vcs ghsnapshot composio; do
  printf "%-12s files_all=%-3s go_nontest=%-3s loc_nontest=%s\n" "$d" \
    "$(find $UP/internal/integrations/$d -type f | wc -l)" \
    "$(find $UP/internal/integrations/$d -name '*.go' ! -name '*_test.go' | wc -l)" \
    "$(find $UP/internal/integrations/$d -name '*.go' ! -name '*_test.go' -exec cat {} + | wc -l)"
done   # ⇒ vcs 4/3/649 · ghsnapshot 7/3/1147 · composio 6/3/1050
# github.go 的两段切点（§1.2）：L963 是 M8-1 的末行，L964 = ListPullRequestsForIssue 起
sed -n '963p;964p' $UP/internal/handler/github.go

# 3. 形态门（预测模式：M8 期望 `0 defect(s)` 且 **exit 0**）
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m8-declared-routes.tsv
python3 scripts/slash_alias_audit.py                    # 本地实况：0 defect

# 4. ⑦ 读数（§1.3 / §6.1）
python3 scripts/route_parity.py
python3 scripts/route_parity.py --list-gaps | awk '/^    \[M8\]/,/^    \[M9\]/'      # ⇒ 24 条
python3 scripts/route_parity.py --json | python3 -c 'import json,sys;d=json.load(sys.stdin);print(d["counts"],d["owners"])'

# 5. ⑨ M8 相关 fixture（§6.2；应 **1 条** unmounted）
python3 - <<'PY'
import json, collections
d = json.load(open('crates/mc-conformance/report.json'))['fixtures']
decl = [l.rstrip('\n').split('\t') for l in open('docs/fixtures/m8-declared-routes.tsv') if l.strip() and not l.startswith('#')][1:]
segs = lambda p: [s for s in p.strip().rstrip('/').split('/')]
keys = [(m, segs(p)) for m, p in decl]
def hit(m, p):
    ps = segs(p)
    return any(m == dm and len(ds) == len(ps) and all(a.startswith('{') or a.startswith(':') or a == b for a, b in zip(ds, ps)) for dm, ds in keys)
rows = [f for f in d if hit(f['method'], f['path'])]
print(len(rows), dict(collections.Counter(f['outcome'] for f in rows)))
PY

# 6. 14 张 M8 面表是否都在（§6.4；每行应输出 1，缺 CREATE 的表数为 0）
for t in github_installation github_pending_installation github_pull_request \
         github_pull_request_check_suite github_pull_request_check_run github_pending_check_suite \
         issue_pull_request vcs_connection vcs_pull_request issue_vcs_pull_request vcs_commit_status \
         workspace_mcp_server agent_mcp_server user_composio_connection; do
  printf "%-38s %s\n" "$t" "$(grep -rliE "CREATE TABLE (IF NOT EXISTS )?\"?$t\"?[ (]" migrations/upstream/*.sql | wc -l)"
done
ls migrations/upstream/*.up.sql | wc -l                                   # ⇒ 560（上游 server/migrations/*.up.sql 也是 560）

# 7. 门 ⑩ 预飞：M8 写集是否已有人被列入白名单（应无输出）
grep -E 'mc-vcs|mc-composio|routes/(github|vcs|mcp|composio)|mc-core/src/(vcs|github|mcp|composio)|state\.rs' scripts/file_size_baseline.tsv
wc -l crates/mc-http/src/state.rs crates/mc-http/src/routes/{mount.rs,mod.rs} crates/mc-repos/src/task/store.rs

# 8. M6/M7 交集核对（§3.2；M8 侧写集应零命中 channel 面）
grep -rn 'channel' crates/mc-vcs/src crates/mc-composio/src crates/mc-http/src/routes/vcs 2>/dev/null   # 建完后应为空

# 9. 全量门禁（每片交付前；M8 各片全部触碰 DB ⇒ 一律带 --with-db）
bash scripts/gates.sh --with-db
```

---

## 11. M8-INT 落地记录（占位，由 M8-7 填写）

（待 M8 波次收口后由 `M8-7` 填写：⑦/⑨/⑩ 快照刷新读数、baseline 344→454 的实测、
本波实际未落地的项（至少含 §9.2 的附件面 9 条尾账与 R-M8-9 的 3 处 enqueue 接线）、
以及 M8 面剩余 `unevaluable` fixture 的复核结论。）
