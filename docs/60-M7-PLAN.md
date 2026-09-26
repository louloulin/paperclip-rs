# docs/60 — M7（W7 渠道面：slack / lark / dingtalk / wecom / telegram 的安装与绑定）切片计划

**状态**：M7 计划片（`LUM-1764`）交付物。M7 代码切片已按本文建为 `LUM-1764` 的子 issue（全部 `backlog`，见 §7），
**待 M6 收口（`LUM-1673` M6-8 合 + `LUM-1675` M6-INT 合）+ 并发位空出后晋升**。

**上游口径**：`multica` @ `f41fae6b08fb734afcbd13205c0b3203dd0bc9c6`（= `docs/fixtures/upstream-routes.tsv` 记录的那个 commit；
只读副本本次克隆在 LUM-1764 自己的 workdir 内，**不依赖 `/tmp/ups*` 或别的 run 的 workdir** —— `LUM-1674` 本轮亲眼见过跨 workdir 副本整个目录被 GC 掉）。

**本地基线**：`paperclip-rs` @ **`2394bfcc`**（`origin/feat/multica-rs-initial`，= PR #75 / M6-9 `LUM-1674` 的合并提交）。
本文所有 ⑦/⑨/⑩ 读数都在 `2394bfcc` 上测得。

**文档编号**：`58` 已被 `LUM-1675`（M6-INT）预留、`59` 是 `docs/59-M2-E-LABEL-PROPERTY.md` ⇒ 本文取 **`60`**（起手已 `git fetch` + `ls docs/ | sort` 复核）。

> 本文的口径只改 `docs/plan1.md` 两处（见 §9）：**§1.2 那行 `wecom 89 / dingtalk 76 / …` 的单位是「一切文件数」而不是 LOC/路由数**、
> **§5 W7 行的 `internal/integrations`(48.9k) 全部落在一个新 crate 而不是 6 个 adapter 各自成波**。
> 其余承接 `docs/57-M6-PLAN.md` 的骨架与全部记法纪律。

---

## 0. 结论速览

| 项 | 结论 |
|---|---|
| 本波路由 | **24 条**（lark 5 + slack 4 + wecom 4 + dingtalk 7 + telegram 4；其中 5 条是 `*/binding/redeem`，1 条是 agent 级 dingtalk 群） |
| 本波上游体量 | **48,366 行 / 149 文件**（`internal/integrations/{channel,slack,telegram,dingtalk,lark,wecom}` **46,198 行 / 144 文件** + `internal/handler/{slack,lark,dingtalk,telegram,wecom_web}.go` **2,168 行 / 5 文件**；均为非测试口径，逐文件见 §1.2） |
| 本波新迁移 | **0**。22 张渠道表**全部已在** `migrations/upstream/`（62 个渠道相关迁移文件，§6.4） |
| 新 crate | **`mc-channel`**（`Channel` trait + `Registry` + 5 adapter + engine/supervisor + wire 客户端）。领域类型进已存在的 `mc-core/src/channel.rs`；仓储进 `mc-repos/src/channel/`；**密钥端口进 `mc-secrets/src/secretbox.rs`**；HTTP 面进 `mc-http/src/routes/channels/`（5 文件 = 5 写者）；长连接宿主进 `apps/mc-server/src/channels.rs` |
| 切片数 | **1 anchor + 20 代码片 + 1 INT = 22 个 issue**，9 个 stage，stage 内并发 ≤ **3**；单片最大 **3,437** 上游行（≤3.5k，§4.1） |
| 尾斜杠双形态 | **0 键**。`slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv` 实测 `declared 24 / dual-form required: 0 / 0 defect(s)`，**exit 0**（对照：M4 15 / M5 7 / M6 5）。M7 **没有 allowlist 退路问题，也没有形态欠账** |
| 关键依赖 | M7-1（契约层 + engine 路由/监管）是全部 20 片的硬前置；M7-2（engine 会话/租约）是**所有**入站片的硬前置；每渠道的「入站片」先于「出站片」 |
| ⑦ 目标 | anchor 后不变（**M7-0 不刷基线**：0 路由、0 占位删除）；全波落地后 `local 430 / implemented 354 real / known_gap 102 / owners.M7 0`，`implemented + known_gap == 456` 恒成立 |
| ⑨ 目标 | 落在 M7 24 条路由上的 fixture **12 条**（8 `unmounted` + 4 `unevaluable`）；本波承诺 8 条 `unmounted → pass`，4 条只承诺「转 evaluable 并给出实测结论」（§6.2） |
| 最大风险 | R-M7-1 **无 Redis**（上游 4 处 Redis 用于跨副本协调）；R-M7-2 每渠道 API 无法在 CI 真连 ⇒ 端到端验收靠**平台替身**（§4.2）；R-M7-3 5 个部署密钥的**未配置语义**被 ⑨ 的 7 条 lark fixture 逐条钉住 |

---

## 1. 上游面测绘（`f41fae6b08fb` 实测）

### 1.1 路由表（24 条，按 `router.go` 行号分三簇）

| 簇 | 行号 | 条数 | 说明 |
|---|---|---|---|
| workspace 级安装面 | L1783–L1791（lark）、L1801–L1809（slack/wecom）、L1814–L1818（dingtalk）、L1825–L1830（telegram） | 18 | 全在既有 `/api/workspaces/{id}` 子路由**内部**用完整子路径注册（plain，不是 Mount）⇒ 5 组注册位置**彼此不相邻**（同一 `r.Route("/{id}", …)` 块内的 5 个 `r.Group`） |
| user 级绑定兑换 | L1841、L1847、L1850、L1854、L1858 | 5 | **无 workspace 前缀**：redeemer 在拥有 workspace 上下文**之前**就打它，会话身份 + token 里的外部 user id 合成 `*_user_binding` 行（上游注释逐条写明该理由） |
| agent 级群绑定 | L2192 | 1 | `GET /api/agents/{id}/dingtalk/groups`，挂在既有 `/api/agents/{id}` 子路由内部（与 M6-4 的 `/skills*` 同手法） |

逐条（`METHOD  PATH  router.go:行`，与 `docs/fixtures/m7-declared-routes.tsv` 24/24 相等，复算见 §10 命令 1）：

| # | METHOD | PATH | router.go | 片 |
|---:|---|---|---:|---|
| 1 | GET | `/api/workspaces/{id}/lark/installations` | 1783 | M7-14 |
| 2 | DELETE | `/api/workspaces/{id}/lark/installations/{installationId}` | 1784 | M7-14 |
| 3 | POST | `/api/workspaces/{id}/lark/install/begin` | 1790 | M7-14 |
| 4 | GET | `/api/workspaces/{id}/lark/install/{sessionId}/status` | 1791 | M7-14 |
| 5 | GET | `/api/workspaces/{id}/slack/installations` | 1801 | M7-4 |
| 6 | DELETE | `/api/workspaces/{id}/slack/installations/{installationId}` | 1806 | M7-4 |
| 7 | POST | `/api/workspaces/{id}/slack/install/byo` | 1807 | M7-4 |
| 8 | GET | `/api/workspaces/{id}/wecom/installations` | 1802 | M7-15 |
| 9 | DELETE | `/api/workspaces/{id}/wecom/installations/{installationId}` | 1808 | M7-15 |
| 10 | POST | `/api/workspaces/{id}/wecom/install/byo` | 1809 | M7-15 |
| 11 | GET | `/api/workspaces/{id}/dingtalk/installations` | 1814 | M7-9 |
| 12 | GET | `/api/workspaces/{id}/dingtalk/groups` | 1815 | M7-9 |
| 13 | DELETE | `/api/workspaces/{id}/dingtalk/installations/{installationId}/groups/{conversationId}` | 1816 | M7-9 |
| 14 | DELETE | `/api/workspaces/{id}/dingtalk/installations/{installationId}` | 1817 | M7-9 |
| 15 | POST | `/api/workspaces/{id}/dingtalk/install/byo` | 1818 | M7-9 |
| 16 | GET | `/api/workspaces/{id}/telegram/installations` | 1825 | M7-5 |
| 17 | DELETE | `/api/workspaces/{id}/telegram/installations/{installationId}` | 1829 | M7-5 |
| 18 | POST | `/api/workspaces/{id}/telegram/install` | 1830 | M7-5 |
| 19 | POST | `/api/lark/binding/redeem` | 1841 | M7-14 |
| 20 | POST | `/api/slack/binding/redeem` | 1847 | M7-4 |
| 21 | POST | `/api/dingtalk/binding/redeem` | 1850 | M7-9 |
| 22 | POST | `/api/wecom/binding/redeem` | 1854 | M7-15 |
| 23 | POST | `/api/telegram/binding/redeem` | 1858 | M7-5 |
| 24 | GET | `/api/agents/{id}/dingtalk/groups` | 2192 | M7-9 |

账（每行恰好一片）：M7-4 **4** + M7-5 **4** + M7-9 **7** + M7-14 **5** + M7-15 **4** = **24** ✓

### 1.2 上游文件与行数（非测试）+ **`plan1.md` §1.2 那行的单位裁定**

**裁定结论（§9 的核心口径修订）**：`plan1.md` §1.2 的 `wecom 89 / dingtalk 76 / lark 72 / channel 42 / slack 32 / telegram 23 / ghsnapshot 7 / composio 6 / vcs 4`
**单位是「该子目录下的一切文件数（递归，含 `_test.go`、含 `testdata/*.json`）」** —— 不是 LOC、不是路由数、不是 handler 数。三条判据：

1. **只有「一切文件数」能让这 9 个数相互自洽**：实测 `find <dir> -type f | wc -l` = channel 42 / composio 6 / dingtalk 76 / ghsnapshot 7 / lark 72 / slack 32 / telegram 23 / vcs 4 / **wecom 90**。
   LOC 口径与它们差着 2–3 个数量级（总和 112,682 行，不是 351）；路由数口径是 0–7（M7 全波才 24 条）；handler 数口径在 `internal/handler` 里（5 个文件）。
2. **同一张表里的 `153 / 48,907` 是另一个口径**（`*.go` 非测试）：实测 `153` 文件 / `49,044` 行。⇒ **一行里混了两个单位**：
   子目录那串是「一切文件」，总量那串是「非测试 `.go` 文件 / 非测试 `.go` LOC」。这是 `plan1.md` §1.2 的**排版歧义**，不是数据错误。
3. **`plan1.md` 的 `wecom 89` 与实测差 1**（实测 **90**）。其余 8 个数逐字相符。求和：plan1 的 9 个数 = **351**，实测 = **352**。
   LOC 那侧也差 137 行（49,044 − 48,907 = 137 ≈ 三个 `doc.go` 的 138 行：`lark/doc.go` 62 + `channel/doc.go` 45 + `channel/engine/doc.go` 31 ⇒ 极可能是写表时剔了 `doc.go`）。
   **两处偏差都不改变任何结论**，但本波报数一律用**自己实测**的两个数（下面第二张表），不引用 plan1 的 351/48,907。

**M7 面的实测构成**（本片逐文件复算，命令见 §10 命令 2）：

| 子目录 | 一切文件数（plan1 口径） | 非测试 `.go` 文件数 | 非测试 `.go` 行数 |
|---|---:|---:|---:|
| `channel/`（含 `engine/`） | 42 | 21 | 5,315 |
| `slack/` | 32 | 16 | 4,490 |
| `telegram/` | 23 | 12 | 4,504 |
| `dingtalk/` | 76 | 25 | 5,910 |
| `lark/` | 72 | 37 | 10,957 |
| `wecom/` | **90** | 33 | 15,022 |
| **渠道面小计** | **335** | **144** | **46,198** |
| `internal/handler/{slack,lark,dingtalk,telegram,wecom_web}.go` | — | 5 | 2,168 |
| **M7 全波合计** | — | **149** | **48,366** |

> 对照：`internal/integrations` 非测试 `.go` 共 **153** 文件 / **49,044** 行 ⇒ 减去渠道面的 144 / 46,198，余 **9 文件 / 2,846 行** = `composio`(3/1,050) + `ghsnapshot`(3/1,147) + `vcs`(3/649)，
> 三者分别归 **W9 / W8 / W8**（与 `plan1.md` §5 的波次一致），**不在 M7 写集内**。

**24 条路由 vs 4.6 万行**：24 条只是**安装/绑定管理面**（create/list/revoke + 扫码会话 + 令牌兑换）。渠道的**消息 I/O 一条都不是 HTTP 路由**（§1.5），
其余体量分布在每个 adapter 自带的 inbound 回路 / outbound 发送 / 媒体下载上传 / 打字指示 / 命令与卡片 / 解签 / 退避重连上 —— 这也是
`plan1.md` §5 W7 行的验收门禁写成「每渠道至少 1 条**真实收发回路**」而不是「24 条路由绿」的原因。

### 1.3 本地现状与缺口（`2394bfcc` 实测）

```
$ python3 scripts/route_parity.py
upstream 456 (commit f41fae6b08fb) | local 405 registered | baseline 344
  implemented  329 real +   0 placeholder =  329 / 456   known_gap  127   unclaimed    0   regression   0   local_only    9
  gaps by owner: M9=33  M7=24  M8=24  M3+=16  M2-A=13  M3=11  M10=5  M6=1
```

* **M7 的 24 条全是 `known_gap`：0 条实现、0 条 501 占位。** `grep -rn -E "slack|dingtalk|wecom|telegram|lark|feishu" crates/ apps/ --include=*.rs`
  在路由/仓储层**零命中**，只有 `crates/mc-core/src/channel.rs`（85 行：`ChannelKind` 六值 + `ChannelInstallation`）与
  `crates/mc-core/src/issue.rs`（`IssueOrigin::{SlackChat,LarkChat,DingTalkChat,WeComChat,TelegramChat}` 五个枚举值）——
  **这两处是 M2/M4 期顺手落的领域枚举，不是渠道运行时**。
* `local_only 9` 与 M7 无关（3 条 `/api/me/pats`、4 条健康/openapi、`/api/issues/:id/{reactions,quick-actions}`、1 条占位 `GET /api/feature-flags`）⇒ M7 **不新增 local_only**。
* 门 ⑦ 的第二条命令（形态）本地实测 `registered upstream-key literals: 409 / 0 defect(s)` ⇒ 本波起手**无形态欠账**，`docs/fixtures/slash-alias-allowlist.tsv` **已空**（M6-0 删完了最后 2 行）。
* **`mount.rs` 里没有渠道相关的 M0 占位**（M0 只造过 auth / issues / comments / inbox / skills / plugins / agents / runtimes / projects / squads / chat / autopilots）
  ⇒ **M7-0 是五轮 anchor 里第一个「零删除」的 anchor**：不删路由 ⇒ 不刷 ⑦ 基线（§6.1）。

**渠道面的对账断言（可复算，§10 命令 3）**：`docs/fixtures/upstream-routes.tsv` 中路径含渠道字样的数据行
= slack 4 / lark 5 / dingtalk 7 / wecom 4 / telegram 4 = **24**，且**每一行的 owner 都是 `M7`**（`awk -F'\t' '{print $3}' | sort -u` ⇒ 只剩 `M7`）
⇒ **渠道面没有散落在别的 owner**。但**读侧有一条是别人的**，必须写清：`GET /api/chat/history`（`router.go:2373`，owner=**M4**）是渠道对话历史的读入口，
归 M4 已落地；M7 只提供它读的数据（`channel_chat_session_binding` 等表），**不重实现它**（见 §8 R-M7-6）。

### 1.4 尾斜杠双形态：本波实测 **0 键**

上游 chi 只在「子路由写 `"/"`」时两种形态都服务（`Mount` 语义）。渠道面 24 条**全部是完整子路径的 plain 注册**
（`r.Get("/lark/installations", …)`、`r.Post("/api/lark/binding/redeem", …)`），所以：

```
$ python3 scripts/slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv
  declared 24 upstream key(s); dual-form required: 0 | single-form: 24
  shapes OK: every registered upstream key matches the form upstream serves
  => 0 defect(s) from findings, 0 warning(s)                       # exit 0
```

| 波 | 声明键 | `dual-form required` | 预测模式 exit | 退路 |
|---|---:|---:|---:|---|
| M4（`docs/42`） | 29 | 15 | 1（`FAIL: 15`） | 曾有 allowlist 6 行，M4-0 删毕 |
| M5（`docs/44`） | 29 | 7 | 1（`FAIL: 7`） | 曾有 allowlist 2 行，M5-0 删毕 |
| M6（`docs/57`） | 57 | 5 | 1（`FAIL: 5`） | M6-0 删毕 ⇒ 现在为空 |
| **M7（本文）** | **24** | **0** | **0** | **无欠账，无需 allowlist** |

⇒ M7 各片的形态纪律只有**一条**：**只按上游字面量注册那一形态，既不补尾斜杠形态、也不得漏成带斜杠形态**。
（补了带斜杠形态 = `EXTRA_ALIAS` 缺陷：axum 会同时服务 `/x` 与 `/x/`，而上游 plain 注册只服务 `/x`。）

### 1.5 渠道入站**不在** 456 条路由表里（单列一节，§1.3 的 `plan1.md` 复核项）

`plan1.md` §1.3 写「456 条，其中 `/api`+`/auth` **440** 条」。本片复核 440 = 456 − **16**，并逐条点出那 16 条非 `/api`/`/auth` 路由：

| # | METHOD | PATH | owner |
|---:|---|---|---|
| 1–4 | GET | `/health`、`/healthz`、`/readyz`、`/health/realtime` | M10 |
| 5 | GET | `/plugin-surfaces/{token}` | M6 |
| 6 | GET | `/uploads/*` | M3+ |
| 7 | GET | `/ws` | M3+ |
| 8–16 | GET/POST/PATCH/PUT/DELETE | `/v1/context`、`/v1/issues/{issue_ref}`(GET/PATCH)、`/v1/issues/{issue_ref}/comments`(GET/POST)、`/v1/storage/{scope}`(GET)、`/v1/storage/{scope}/{key}`(GET/PUT/DELETE) | M6 |

**渠道入站 = 0 条。** 五个平台**全部是出站长连接**（服务端主动拨出、平台在连接上推事件），没有任何 webhook / OAuth-callback 路由：

| 渠道 | 入站传输（上游实现点） | 证据（非测试源码） |
|---|---|---|
| slack | **per-installation Socket Mode**（WebSocket，`xapp-` app token 授权） | `slack/byo_install.go:40,168`（`StartSocketModeContext`） |
| lark | 自建 **WS 长连接**：`POST /callback/ws/endpoint` 引导 + gorilla/websocket | `lark/ws_endpoint.go:30,135`、`lark/ws_connector.go:23` |
| dingtalk | 自建 **Stream WebSocket**（每个 BYO installation 一条） | `dingtalk/ws_connector.go:12,48` |
| wecom | 智能机器人 **aibot WebSocket**（per-installation supervisor） | `wecom/credentials.go:4`、`wecom/credential_probe.go:174` |
| telegram | **`getUpdates` 长轮询**（50s server-side hold） | `telegram/api.go:15,24,224` |

三点后果，全部写进 §2/§4/§6：

1. `router.go` 里 1796–1800 行那段注释（「The OAuth callback itself is a public route … registered outside this workspace group」）**在 `f41fae6b08fb` 上是陈旧注释**：
   实测 `grep -n 'r\.\(Get\|Post\)' router.go` 全表**没有**任何 slack/lark/wecom/dingtalk/telegram 的 callback 路由，`grep -rn "oauth/callback" server/` 只命中 composio（M6 之外的另一波）。
   上游代码里唯一提到 callback 的地方是 `lark/types.go:55` / `ws_endpoint.go` 的**出站**引导 URL `/callback/ws/endpoint`（是服务端**发往** lark 的地址，不是本服务暴露的路由）。
2. **本波没有 webhook 验签面**（§8 的「入站验签与重放」风险因此**不是** HTTP 签名问题，而是**长连接帧的会话/租约/去重**问题：`channel_inbound_message_dedup` 表 + `ws_lease_token` 列）。
3. 长连接的生命周期（连接 / 退避重连 / 租约 / 优雅停机）**必须有自己的运行时宿主**（§2.4），不能寄生在 HTTP router 上。

### 1.6 已退役的渠道路由（**必须 404**，不许"顺手补上"）

`GET /api/workspaces/{id}/dingtalk/group-routes` 在上游是**已删除**的路由：`server/cmd/server/integration_test.go:786` 主动断言它 **404**，
且它**不在 456 条**（`grep -n group-routes docs/fixtures/upstream-routes.tsv` 无输出）。但 ⑨ 里有一条 fixture 走它
（`workspaces/TestDingTalkGroupsThroughRouterSupportsFilteredWorkspaceAndAgentScopes@…integration_test.go:786#6`，当前 `unevaluable`）。

⇒ 写进 M7-9 的 DoD：**`group-routes` 必须保持不存在**（返回 404），且那条 fixture 的期望是「404」而不是「200 空数组」——
这是本波唯一一条「**反向**验收」（别的片要求路由存在，这条要求路由**不在**）。

---

## 2. 目标架构与落点（含取舍）

### 2.1 分层落点

| 层 | 落点 | 内容 |
|---|---|---|
| 领域类型（跨 crate） | `crates/mc-core/src/channel.rs`（**已存在**，85 行）+ `crates/mc-core/src/channel/{installation,message,binding,install_session}.rs`（anchor 建） | `ChannelKind`（已有 6 值）、`ChannelInstallation`、`InboundMessage`/`OutboundMessage` 的规范化形态、`BindingToken` 形态、安装会话状态枚举 |
| 运行时 + 5 个 adapter | **`crates/mc-channel/`**（新 crate） | `Channel` trait（5 方法）+ `Registry` + `Capability` 位图 + `InboundHandler` + `Supervisor`（退避重连/租约）+ `Engine{router,session,resolvers,batcher}` + `slack/ lark/ dingtalk/ wecom/ telegram/` 五个模块 |
| 密钥端口 | `crates/mc-secrets/src/secretbox.rs`（新模块） | `MULTICA_*_SECRET_KEY` → `Box`（AES-256-GCM，`nonce‖ct‖tag`），与 `mc-secrets` 已有的 `aes-gcm`/`base64`/`zeroize` 依赖同 crate |
| PG 仓储 | `crates/mc-repos/src/channel/{installation,binding,session,inbound_audit,dedup,outbound,media,delivery}.rs` | 22 张表按面分文件（§3.3），anchor 只建 `channel/mod.rs` 与 `pub use` |
| HTTP 面 | `crates/mc-http/src/routes/channels/{slack,telegram,dingtalk,lark,wecom}.rs` + `channels/mod.rs` | 24 条路由，**每渠道一个文件 = 一个写者** |
| 长连接宿主 | `apps/mc-server/src/channels.rs`（新）+ `main.rs` 一次调用 | `Registry` 装配 + `Supervisor::spawn` + graceful shutdown 句柄（与 M5-9 的 `scheduler::start` 同造型） |

### 2.2 为什么是**一个**新 crate `mc-channel`（issue 的三条判据 + 被否决的备选）

**判据 1：是否被 daemon 与 http 同时依赖？** —— **否**。上游 `internal/daemon`（84 文件 / 45,571 行）**零引用** `internal/integrations`：
渠道侧与 `runtime` 侧的通信是**经表**的（渠道入站建 issue/task 行，daemon 认领 task 行），不是经进程内 API。
⇒ 「被两个进程同时依赖」这条**不成立**，渠道运行时**不该**落到 `mc-core` 或 `mc-daemon`。

**判据 2：是否需要 per-channel trait 抽象？** —— **是，而且是硬需求**。上游给出的是三件套：
`channel.Channel` 接口（`Type/Connect/Disconnect/Send/Capabilities` 五方法，`Connect` **阻塞跑接收循环**）、
`channel.Registry`（`Type → Factory`，last-writer-wins，`Build(cfg)`）、`channel.InboundHandler`（`func(ctx, InboundMessage) error`，由 supervisor **单点注入**给每个 adapter）。
⇒ 需要一条「engine 只依赖 trait、adapter 只依赖 trait」的**编译边界**：engine 不能知道任何平台 SDK / wire 格式，adapter 不能知道 engine 的 DB 细节。
这条边界就是一个 crate 内的 trait + 模块，因此**crate 数 = 1**（trait 与其 5 个实现放同 crate 才能避免"第 6 个 crate 装共享件"）。

**判据 3：凭据与验签是否共用？** —— **凭据共用，验签不存在**（§1.5 结论 2）。共用件是**密钥端口**（5 个 `MULTICA_<CHANNEL>_SECRET_KEY` + 一个 `secretbox` 算法），
落点选 `mc-secrets`（那里已经有 `aes-gcm` / `base64` / `zeroize`，且它已是 `mc-http` 的依赖 ⇒ **零新包、零新依赖边**）。
这条**不构成**「把 5 个 adapter 也塞进一个共享 crate」的理由——它们本来就在同一个 crate，理由是判据 2。

**被否决的备选（各写代价）**：

| 备选 | 否决理由 |
|---|---|
| 5 个 adapter 各建一个 crate（`mc-channel-slack` ×5） | `channel/`(733) + `channel/engine/`(4,582) = **5,315 行跨渠道共享件**会无处可去 ⇒ 必须再造第 6 个 crate 装共享件；workspace 成员 29 → 34，冷构建成本上升，而 adapter 之间**零互相依赖** ⇒ 无隔离收益。 |
| 整块塞进 `mc-http` | 违反 `plan1.md` §3.1 的「`crates/api/*` 是薄适配层」；4.6 万行运行时装进去会让 `mc-http` 变成继 `mc-repos` 之后第二个巨型 crate；且长连接的重连/退避/租约**不属于** HTTP 请求面。 |
| 复用 `mc-daemon` | daemon 是「执行环境侧」的进程内协议面（`plan1.md` §3.3 归 W3），渠道是「服务端进程内的 IM 适配」；两者都过长连接，但一个在 W3 已收口、一个在 W7，合并会把两个波次的写集缠在一起。 |
| 落 `mc-realtime` | `mc-realtime` 是**事件总线**（`RealtimeHandle`/`Bus`/`EventEnvelope`），渠道要**用它**（把渠道事件广播出去），不能**变成**它（会形成 `realtime → channel → realtime` 的环）。 |

**依赖方向（无环，anchor 一次性接好）**：
`mc-channel` → `mc-core` / `mc-repos` / `mc-realtime` / `mc-secrets` / `mc-telemetry` / `mc-task` / `mc-chat`；
`mc-http` → `mc-channel`（新增一条 `path` 边）；`apps/mc-server` → `mc-channel`（新增一条 `path` 边）。

### 2.3 凭据、部署密钥与 redaction（**逐字复刻上游 `secretbox`**）

* **算法/形态逐字复刻**：上游 `server/internal/util/secretbox/secretbox.go` = AES-256-GCM，`Seal` 返回 **`nonce(12) ‖ ciphertext ‖ tag`** 的**单块字节**（不 base64），
  `LoadKey(envVar)` = 该 env 是**base64 编码的 32 字节**，空串与非 32 字节都是错误。列侧实测：`lark_installation.app_secret_encrypted BYTEA NOT NULL`（`migrations/upstream/109`）；
  泛化表 `channel_installation`（`migrations/upstream/124`）把同一份密文放在 `config` 里（注释写 `app_secret_encrypted (base64)`）。
* **不复用 `mc_secrets::cipher`**：它是 `{nonce: base64, ciphertext: base64}` 的 **JSON payload** 形态（better-auth 兼容，见 `crates/mc-secrets/src/cipher.rs`），
  与上游 `secretbox` 的字节布局**不兼容**。M7 在 `mc-secrets` 里**新开一个模块**（不改 `cipher.rs`），
  并让 `PluginSecretKey`（M6-0 落在 `crates/mc-http/src/state.rs:207`，已合、**冻结**）**保持不动** ⇒ 两处同源不同形态，登记为 §8 **R-M7-4**（收敛票另开，不在 M7 写集内）。
* **redaction 是硬约束，不是风格**：渠道凭据（Slack bot/app token、Lark `app_secret`、WeCom corpsecret、Telegram bot token、DingTalk appkey/appsecret）
  **必须经 `mc-telemetry` 的 redaction 通道**，照 `docs/33` **§12.2 第 2 条**的先例（「凭据只经 `mc-telemetry` 的 redaction 通道」「本层只记录路径与错误类型、从不把文件内容写进日志」）。
  落地判据（写进所有涉密片的 DoD）：
  1. 承载密钥/密文/明文 secret 的类型**手写 `Debug`**，输出 `<redacted>`（照 `PluginSecretKey` 的 `crates/mc-http/src/state.rs:246` 先例）；
  2. 任何 `tracing::*` 调用**不得**插值这些类型的字段（clippy 的 `doc_markdown`/`unused` 抓不到，靠测试）；
  3. 新增一条「解析器/客户端错误路径**不回显**凭据」的用例（照 `docs/33` §12.2 的 `execenv_errors_never_echo_file_contents` 同款）；
  4. `mc_telemetry::redact::Redactor::is_sensitive` 的 `SENSITIVE_KEYS` 需要覆盖渠道键名（若不足，在 anchor 一次性补齐并给出用例）。
* **`state.rs` 的落法照 M6-0 判例**：**不新增 `AppState::new` 参数**（21 个调用点全不动），密钥在 `AppState` 构造体内读 env（`PluginSecretKey::from_env` 先例），
  并给一个唯一出口；测试用的字面量构造点全仓只有 `crates/mc-http/src/routes/auth.rs` 一处（M6-0 已建立该纪律）。

### 2.4 长连接的宿主与生命周期（**`apps/mc-server`，不是 `mc-http`**）

* 上游在 `buildHandler` 里就把 5 个块**整块**装配（`secretbox.LoadKey(...) == nil` 才进块）并起 goroutine（supervisor.Run / WS 连接 / 后台 backfill）。
  本仓对应物 = `apps/mc-server/src/channels.rs`：持有 `mc_channel::Registry`，对每个已配置渠道 `Register` 一个工厂，再 `Supervisor::spawn` 一个任务。
* **宿主为什么在 `apps/mc-server`**：M5-9 已把「后台任务宿主」定在这里（`apps/mc-server/src/scheduler.rs` + `main.rs` 第 7 步，`docs/48` §7.1），
  且 `main.rs` 已有「先停调度器、再停 actor」的优雅停机顺序 ⇒ 渠道 supervisor 加进同一条链（新增一步：**先停渠道连接**、再停调度器）。
* **「未配置」语义逐条对齐（关键）**：缺部署密钥时该渠道**整体不装配**，但其 HTTP 路由**仍然存在**并返回"未配置"语义。
  上游口径是**按端点而异**的，⑨ 的 7 条 lark fixture 正是在钉这些差异（`TestListLarkInstallations_NotConfiguredReturnsEmpty`、
  `…_HardCodedInstallSupportedFalse`、`TestBeginLarkInstall_NotConfigured`、`TestGetLarkInstallStatus_NotConfigured`、
  `TestRevokeLarkInstallation_NotConfigured`、`TestRedeemLarkBindingToken_NotConfigured`、`TestListTelegramInstallationsNotConfiguredReturnsEmpty`）
  ⇒ 各片必须**逐 fixture 复核**（不许"统一返回 503"了事：lark 列表是 200 空 + `install_supported:false`）。

### 2.5 Redis 的四处用途 → **进程内替身**（登记偏离，R-M7-1）

本仓**没有 Redis 依赖**（根 `Cargo.toml` 无 `redis`，`crates/**/Cargo.toml` 零命中，`mc-config` 也没有 redis 配置）。
上游渠道面有 **4 处** Redis（全部是**跨副本协调**，不是缓存）：

| 上游文件 | 行数 | 用途 | 本仓替身 |
|---|---:|---|---|
| `channel/engine/redis_lease_store.go` | 167 | WS 租约 CAS：多副本下只有一个副本持有某 installation 的长连接 | 进程内 registry + **单副本假设**；只保留 `LeaseStore` trait 与 `LeaseMetrics`，实现换成进程内 |
| `wecom/dedupe_redis.go` | 200 | 入站消息去重（跨副本） | 进程内 TTL 集合（`Mutex<HashMap<_, Instant>>` + 容量上限） |
| `lark/install_session_redis_store.go` | 187 | 扫码安装会话跨副本共享（`SetInstallSessionStore`） | 进程内安装会话表（上游无 Redis 时**本身就是**降级分支：`router.go:742` 有 `slog.Warn("…no Redis; bind sessions are per-process…")`） |
| `wecom/relay_outbound.go`（14 处命中） | — | 跨副本出站重投递（re-offer chain） | 进程内重投递队列 |

**判据**：上游这 4 处**全部**有"无 Redis 时"的降级路径或等价语义（第 3 处有明文 warn），所以**不是**伪造行为、而是**换部署形态**。
写进 `docs/32` 偏离表（R-M7-1）：**多副本部署下同一 installation 可能被两个副本同时连接** ⇒ 生产部署契约 = 单副本或"渠道连接只在一个副本上开"。
本波**不引入 Redis**（引一条新依赖会让 anchor 的 `Cargo.lock` 变更面从"加 1 个成员 crate"变成"加一条外部依赖链"，且与 M5/M6 的选型纪律相悖）。

### 2.6 边界契约（写进各片 DoD，逐条可测）

1. **engine 不知道平台**：`mc-channel` 的 engine 模块（M7-1/M7-2 写）不得 `use` 任何 `slack::`/`lark::`/… 具体类型；跨边界只走 `Channel` trait / `InboundMessage` / `OutboundMessage`。
   反向同理：adapter 不得直接写 DB（只走注入进来的 port）。
2. **凭据只经 `secretbox`**：任何 adapter/路由**不得**出现 `String` 形式的明文 secret 字段的 `Debug`/`Display`/日志插值（§2.3 四条判据）。
3. **未配置 = 该渠道不装配**：装配点在 `apps/mc-server/src/channels.rs`，判据是**部署密钥存在**；路由侧的"未配置"响应**逐 fixture 对齐**（§2.4）。
4. **入站是 push，不是 poll**：`Channel::connect` **阻塞跑接收循环**并把归一化消息交给构造时注入的 `InboundHandler`（上游 `channel/handler.go` 的契约逐字：非 nil error = 基础设施失败，nil = 已分类，产品性丢弃不是错误）。
5. **出站不阻塞 ACK**：handler 触发的任何回复（绑定卡 / 离线提示 / 打字指示）**脱离 adapter 的 ACK 路径**（上游 `handler.go` 原文要求）。
6. **不实现已退役路由**（§1.6）。
7. **不改 `Route 456` 的形态**：只按上游字面量注册（§1.4）。

---

## 3. 写集与并发

### 3.1 共享锚点（**只在 M7-0 动一次**，其余片只读）

| 共享件 | 动作 |
|---|---|
| `Cargo.toml`（根 `[workspace.dependencies]`） | **无新增包**（`tokio-tungstenite` / `reqwest` / `serde_yaml`? 不需要 / `aes-gcm` / `sha2` / `hmac` / `base64` / `hex` / `uuid` / `chrono` 都已在）。members 是 `crates/*` glob ⇒ **连 members 行都不用改** |
| `Cargo.lock` | 只重新生成（**只有 anchor 能改**）：新增 `mc-channel` 一个 workspace 成员 |
| `crates/mc-channel/Cargo.toml` + `src/lib.rs` | **建骨架**（`pub mod` 树 + trait 位 + 空 adapter 注册函数），依赖边一次接好 |
| `crates/mc-http/Cargo.toml` | 加一条 `mc-channel` `path` 边 |
| `apps/mc-server/Cargo.toml` | 加一条 `mc-channel` `path` 边 |
| `crates/mc-http/src/routes/{mod.rs,mount.rs}` | 加 `pub mod channels;` + `mount_slice_channel()`（**anchor 期是空 router** ⇒ 注册键不变） |
| `crates/mc-http/src/state.rs` | 加渠道密钥/注册表字段（**在 `AppState::new` 内部读 env**，不新增参数）+ 唯一出口 |
| `crates/mc-http/src/routes/auth.rs` | 测试里唯一的 `AppState { … }` 字面量补字段 |
| `crates/mc-core/src/channel{,.rs 的子模块}` | 建 `channel/{installation,message,binding,install_session}.rs` + 扩展 `channel.rs`（各片只读） |
| `crates/mc-repos/src/lib.rs` + `mc-repos/src/channel/mod.rs` | 模块树 + `pub use`（各面文件由各片新建） |
| `crates/mc-secrets/src/{lib.rs,secretbox.rs}` | 新模块 + `pub use`（`cipher.rs` **不动**） |
| `apps/mc-server/src/{main.rs,channels.rs}` | 新文件 + `main.rs` 一次调用 + 停机链一行 |
| `docs/fixtures/route-parity-baseline.json` | **本 anchor 不动**（0 路由、0 占位删除）⇒ 归 **M7-INT（M7-21）** 刷新 |
| `docs/32` 偏离表 / `docs/37` 口径表 | 按本文 §9 + R-M7-1…5 追加修订条（**anchor 一次落，后续片不回来改**） |

### 3.2 与 **M6 热点**的交集（逐文件；M6 收口后这些是**只读面**）

| M6 热点文件 | M6 的改动 | M7 是否需要碰 | 结论 |
|---|---|---|---|
| `crates/mc-http/src/state.rs` | M6-0 加 `plugin_key` / `plugin_surface_origin`（**375 → 530 行**，实测 `git show 679959b0:crates/mc-http/src/state.rs \| wc -l`） | **是**（M7-0 加渠道字段） | **同文件、不同块**，但 M6-INT 还要追加文档/基线 ⇒ **M6 全合前 M7-0 不得开跑** |
| `crates/mc-http/src/routes/mod.rs` | M6-0 加 5 个 `pub mod` | **是**（加 1 个 `pub mod channels`） | 同上（同文件） |
| `crates/mc-http/src/routes/mount.rs` | M6-0 接 5 个 `mount_slice_*` + 删 2 条占位 | **是**（追加 1 个 `mount_slice_channel()`） | 同上（同文件） |
| `crates/mc-http/src/routes/{skills,plugins,plugin_bridge,v1,surfaces}*` | M6-1…M6-8 的实现在此 | **否** | 面包屑相邻但**零写集交集**；M7 只新增 `routes/channels/` |
| `crates/mc-core/src/{plugin.rs,skill.rs}` | M6-0 重写 | **否** | 同上；M7 只新增 `mc-core/src/channel*` |
| `crates/mc-plugin-host` / `mc-mcp` / `mc-skill` | M6 新增的 3 个 crate | **否** | M7 不依赖它们（渠道与插件面无关） |
| `docs/fixtures/route-parity-baseline.json` | M6-0 `--write-baseline`（344 的来处之一） | **是（但归 INT）** | M7-21 刷新；M7-0…M7-20 **都不动基线** |
| `docs/fixtures/slash-alias-allowlist.tsv` | M6-0 删最后 2 行 ⇒ **现已空** | **否** | M7 无形态欠账（§1.4）⇒ 不需要也不得加回任何行 |

### 3.3 写集（**一格 = 一个本地文件 = 一个写者**；逐字路径，禁 glob/花括号）

| 本地文件（逐字） | 唯一写者 | 读者 |
|---|---|---|
| `crates/mc-channel/src/{channel.rs,registry.rs,capability.rs,message.rs,engine/{mod.rs,router.rs,supervisor.rs,resolvers.rs}}` | M7-1 | M7-2…M7-20 |
| `crates/mc-channel/src/engine/{session.rs,batcher.rs,lease.rs,commands.rs}` | M7-2 | 各 adapter 片 |
| `crates/mc-channel/src/slack/{inbound.rs,resolvers.rs,media.rs,mrkdwn.rs,config.rs}` | M7-3 | M7-4 |
| `crates/mc-channel/src/slack/{outbound.rs,replier.rs,typing.rs,history.rs,slash.rs,install.rs,binding.rs}` | M7-4 | — |
| `crates/mc-channel/src/telegram/{inbound.rs,resolvers.rs,replier.rs,install.rs,binding.rs,config.rs}` | M7-5 | M7-6 |
| `crates/mc-channel/src/telegram/{outbound.rs,delivery.rs,sender.rs,api.rs,markdown.rs}` | M7-6 | — |
| `crates/mc-channel/src/dingtalk/{inbound.rs,stream.rs,resolvers.rs,dispatch.rs,emotion.rs}` | M7-7 | M7-8 / M7-9 |
| `crates/mc-channel/src/dingtalk/{outbound.rs,replier.rs,media.rs,ack.rs,markdown.rs}` | M7-8 | — |
| `crates/mc-channel/src/dingtalk/{install.rs,binding.rs,client.rs,config.rs,group_identity.rs}` | M7-9 | — |
| `crates/mc-channel/src/lark/{http_client.rs,client.rs,types.rs,params.rs}` | M7-10 | M7-11…M7-14 |
| `crates/mc-channel/src/lark/{ws_connector.rs,ws_frame.rs,ws_endpoint.rs}` | M7-11 | M7-12 / M7-14 |
| `crates/mc-channel/src/lark/{feishu_channel.rs,enricher.rs,resolvers.rs,media.rs,content_flatten.rs}` | M7-12 | M7-13 |
| `crates/mc-channel/src/lark/{outbound.rs,replier.rs,typing.rs,channel_store.rs,store.rs,audit.rs}` | M7-13 | — |
| `crates/mc-channel/src/lark/{installation.rs,registration.rs,binding.rs,backfill.rs}` | M7-14 | — |
| `crates/mc-channel/src/wecom/{credentials.rs,installation.rs,store.rs,binding.rs,types.rs,strings.rs,metrics.rs}` | M7-15 | M7-16…M7-20 |
| `crates/mc-channel/src/wecom/{ws_frame.rs,ws_sender.rs,stream_store.rs}` | M7-16 | M7-17 / M7-19 |
| `crates/mc-channel/src/wecom/{relay.rs,outbound.rs,outcome.rs,replier.rs}` | M7-17 | M7-19 |
| `crates/mc-channel/src/wecom/{outbound_media.rs,media_ingest.rs,media_download.rs,media_upload.rs,media_guard.rs,media_crypt.rs}` | M7-18 | — |
| `crates/mc-channel/src/wecom/{wecom_channel.rs,resolvers.rs,inbox_message.rs,markdown.rs,seal.rs}` | M7-19 | — |
| `crates/mc-channel/src/wecom/{typing.rs,rate_limit.rs,senders.rs,dedupe.rs,trace.rs}` | M7-20 | — |
| `crates/mc-http/src/routes/channels/slack.rs` | M7-4 | — |
| `crates/mc-http/src/routes/channels/telegram.rs` | M7-5 | — |
| `crates/mc-http/src/routes/channels/dingtalk.rs` | M7-9 | — |
| `crates/mc-http/src/routes/channels/lark.rs` | M7-14 | — |
| `crates/mc-http/src/routes/channels/wecom.rs` | M7-15 | — |
| `crates/mc-repos/src/channel/{installation.rs,binding.rs}` | M7-1 | M7-4/5/9/14/15 |
| `crates/mc-repos/src/channel/{session.rs,inbound_audit.rs,dedup.rs}` | M7-2 | 各入站片 |
| `crates/mc-repos/src/channel/{outbound.rs,delivery.rs,media.rs}` | M7-1 | 各出站片 |

> **同 stage 内两片可以读写同一张 DB 表，但必须走各自的本地文件**（矩阵的「写 / 读」区分的就是这个）。
> **记法纪律（承接 `docs/57` §3.2，`LUM-1739` 起生效）**：写集一律写**逐字路径**，禁 glob / 花括号 / 「某某段」。
> 上表每一格的文件路径在锚点落地后 `ls` 一次必须存在（新建的除外），且父模块声明必须已存在或在该片自己的写集里。

---

## 4. 切片表（派发用）

### 4.1 全景（**1 anchor + 20 代码片 + 1 INT = 22 个 issue**）

| # | 切片 | 路由 | 上游行数 | stage | 硬前置 |
|---|---|---:|---:|---|---|
| M7-0 | anchor（骨架 + 契约位 + 密钥端口 + 宿主位） | 0 | — | 1 | M6 全合（`LUM-1673` + `LUM-1675`） |
| M7-1 | 渠道契约层 + engine 路由/监管/解析 | 0 | 3,317 | 2 | M7-0 |
| M7-2 | engine 会话/命令/租约 + 会话/审计/去重仓储 | 0 | 1,998 | 2 | M7-0 |
| M7-3 | slack 入站回路 | 0 | 1,786 | 2 | M7-0/1/2 |
| M7-4 | slack 出站/回复/命令历史 + 安装与绑定面 | 4 | 2,986 | 3 | M7-1/2/3 |
| M7-5 | telegram 入站 + 安装与绑定面 | 4 | 2,062 | 3 | M7-1/2 |
| M7-6 | telegram 出站与投递 | 0 | 2,707 | 3 | M7-5 |
| M7-7 | dingtalk 入站与 Stream 连接 | 0 | 2,135 | 4 | M7-1/2 |
| M7-8 | dingtalk 出站/媒体/回复 | 0 | 2,486 | 4 | M7-7 |
| M7-9 | dingtalk 安装/凭据/群身份 + agent 群面 | 7 | 2,071 | 4 | M7-1/2/7 |
| M7-10 | lark 客户端与类型 | 0 | 2,215 | 5 | M7-1 |
| M7-11 | lark 长连接（WS） | 0 | 1,793 | 5 | M7-10 |
| M7-12 | lark 入站回路 | 0 | 2,316 | 5 | M7-2/11 |
| M7-13 | lark 出站/回复/会话桥 | 0 | 2,253 | 6 | M7-12 |
| M7-14 | lark 安装与绑定面 | 5 | 2,818 | 6 | M7-10/11/12 |
| M7-15 | wecom 契约/凭据/安装与绑定面 | 4 | 2,171 | 6 | M7-1/2 |
| M7-16 | wecom WS 帧与发送 | 0 | 3,437 | 7 | M7-15 |
| M7-17 | wecom 中继与出站回复 | 0 | 3,202 | 7 | M7-15/16 |
| M7-18 | wecom 媒体面 | 0 | 2,614 | 7 | M7-15 |
| M7-19 | wecom 入站与解析 | 0 | 1,785 | 8 | M7-16/17 |
| M7-20 | wecom 打字/限流/去重/追踪 | 0 | 2,214 | 8 | M7-19 |
| M7-21 | INT（集成、快照刷新与缺口登记） | 0 | — | 9 | 全波 |

**路由账**：4 + 4 + 7 + 5 + 4 = **24** ✓（M7-0/1/2/3/6/7/8/10/11/12/13/16/17/18/19/20/21 是 0 路由片）
**行数账**：20 片合计 **48,366** ✓（每片 ≤ **3,500**，最大 M7-16 = 3,437）

> **这两个数不是手算的**：逐文件分配底稿在 `docs/fixtures/m7-slice-upstream-files.tsv`（149 行 = 144 个非测试 `.go` + 5 个 handler 文件；
> 每行 `片<TAB>上游路径<TAB>行数`）。**144 个文件全部有归属、无重复**，且表内行数与上游副本逐文件核对一致（复算命令见 §10 命令 2）。

### 4.2 每渠道端到端收发回路（**验收门禁的替身方案**）

`plan1.md` §5 W7 行的门禁逐字是「**每渠道至少 1 条真实收发回路**」。五个平台 API **都**无法在 CI 真连
（Slack Socket Mode 要真 app token、Lark 要 app_id/app_secret、DingTalk/WeCom 要企业凭据、Telegram 要真 bot token），
所以判据必须是**离线可复现**的：**本地平台替身 + 真实 wire 帧 + 真库**。

| 渠道 | 承担端到端证据的片 | 收（入站）来自 | 发（出站）来自 | 替身 |
|---|---|---|---|---|
| slack | **M7-4** | M7-3 | M7-4 | 本地 WS 服务端（Socket Mode 信封帧：`apps.connections.open` 引导 + `events_api` 信封），tokio-tungstenite |
| telegram | **M7-6** | M7-5 | M7-6 | 本地 HTTP 服务端（`getMe`/`getUpdates`/`sendMessage`），`api.rs` 的 base URL 已是可替换 seam |
| dingtalk | **M7-8** | M7-7 | M7-8 | 本地 WS 服务端（Stream 帧）；**帧格式用 `dingtalk/testdata/*.json` 四份 golden**（`quoted_card_link_group/private`、`quoted_bot_channels`、`quoted_interactive_card`） |
| lark | **M7-13** | M7-12 | M7-13 | 本地 WS 服务端 + `POST /callback/ws/endpoint` 引导接口（帧格式照 `ws_frame_decoder.rs` 的 350 行解码器） |
| wecom | **M7-19** | M7-19 | M7-17 | 本地 WS 服务端（aibot 帧，`ws_frame.rs` 1,187 行 + `media_crypt`）—— M7-19 是 wecom 最后一个落地片，收/发两半都在 |

**替身纪律（三条，写进上表 5 片的 DoD）**：

1. **只替平台 wire，不替业务路径**：替身是「假平台」，不是「假 engine」。断言链必须是
   `替身造一帧 → 真 WS/HTTP 入站 → 真 DB 写入（issue/task/binding 行）→ 真出站 → 帧回到替身`，中间**零 mock**。
2. **帧要逐字节/逐字段比对**：入站方向断言解析结果（归一化的 `InboundMessage` 字段逐个），出站方向断言**替身收到的原始帧**（JSON 字段级，含卡片/引用/媒体形态）。
3. **两个反例必测**：帧未签名/错会话（去重命中 ⇒ 丢弃且**不**回报错误）、连接断开（退避重连且不重复投递）。

### 4.3 波次（并发 ≤3；stage 内三片可并行，上一 stage 未合不进下一 stage）

```
stage 1  M7-0
stage 2  M7-1 ∥ M7-2 ∥ M7-3
stage 3  M7-4 ∥ M7-5 ∥ M7-6
stage 4  M7-7 ∥ M7-8 ∥ M7-9
stage 5  M7-10 ∥ M7-11 ∥ M7-12
stage 6  M7-13 ∥ M7-14 ∥ M7-15
stage 7  M7-16 ∥ M7-17 ∥ M7-18
stage 8  M7-19 ∥ M7-20
stage 9  M7-21
```

**串行链（必须写清，避免同 stage 内抢同一文件）**：M7-1 → M7-2 → 各入站片 → 各出站片 →（每渠道）安装面片。
具体三条：`M7-3 → M7-4`（slack 收发同文件目录）、`M7-7 → M7-8 → M7-9`（dingtalk 三片共用 `dingtalk/` 目录与 `config.rs` 的读取口）、
`M7-16 → M7-17 → M7-19`（wecom 的 senders registry 与 stream store 按此序建立）。

> 🔴 **上表 `stage 8  M7-19 ∥ M7-20` 的 `∥` 是笔误：stage 8 按串行执行（`M7-19 → M7-20`），两片不得同飞。**
> 依据（三份声明冲突时以「只读清单（真实数据依赖）+ §4.1 前置列」为准）：① `:398` 的前置列给 M7-20 的硬前置就是 **M7-19**；② `dedupe.rs` 按 §3.2 写者表归 **M7-20**，M7-19 不得把它当前置；③ 两片同抢 `wecom/mod.rs` 的追加段。
> 裁决与取证：`docs/37-M3-W3C-PREFLIGHT.md` §118.5 / §119.5，以及 `LUM-1784` / `LUM-1785` 的 rev 2 描述。

---

## 5. M7-0 anchor：逐文件预扩展清单

| 文件 | 动作 | 关键点 |
|---|---|---|
| `crates/mc-channel/Cargo.toml` | 新建 | 依赖：`mc-core` / `mc-repos` / `mc-realtime` / `mc-secrets` / `mc-telemetry` / `mc-task` / `mc-chat` + `tokio` / `async-trait` / `serde` / `serde_json` / `reqwest` / `tokio-tungstenite` / `futures-util` / `thiserror` / `tracing` / `chrono` / `uuid` / `base64` / `hex` / `sha2` / `hmac`。**零新外部包** |
| `crates/mc-channel/src/lib.rs` | 新建骨架 | `pub mod {channel,registry,capability,message,engine,slack,lark,dingtalk,wecom,telegram};`，每块只有 trait/类型位与**空实现** |
| `crates/mc-channel/src/{channel.rs,registry.rs,capability.rs,message.rs}` | 建（类型位） | `Channel` trait（5 方法，`connect` 阻塞）、`Registry`（`Type → Factory`）、`Capability` 位图（8 位）、`InboundHandler`。**不写任何平台分支** |
| `crates/mc-channel/src/engine/{mod.rs,router.rs,supervisor.rs,resolvers.rs}` | 建空模块 | 只放签名与 `todo!()` 位（**anchor 不实现**）；`supervisor.rs` 放 `LeaseStore` trait + `InstallationStore` trait（**port 先定，实现归 M7-1/2**） |
| `crates/mc-channel/src/{slack,lark,dingtalk,wecom,telegram}/mod.rs` | 建（各 1 文件） | 每渠道一个 `pub fn register(registry: &mut Registry, deps: …)` **空实现**；5 个片的写集从这里展开 |
| `crates/mc-secrets/src/secretbox.rs`（+ `lib.rs` 一行 `pub use`） | 新建 | `Box::{new,seal,open}`（`nonce(12)‖ct‖tag`）+ `load_key(env_var)`（base64 → 32 字节，空/长度错 ⇒ `None`）+ 手写 `Debug` 脱敏。**`cipher.rs` 不动** |
| `crates/mc-core/src/channel.rs` | 扩展（85 → 目标 ≈150） | 保留 `ChannelKind` 六值与 `ChannelInstallation`（**不动既有 shape**，M4 的 `IssueOrigin` 引用它） |
| `crates/mc-core/src/channel/{installation.rs,message.rs,binding.rs,install_session.rs}` | 新建 | `Installation`/`InboundMessage`/`OutboundMessage`/`BindingToken`/`InstallSession{state}`。**用 `channel.rs` + `channel/` 目录并存**（Rust 2018 允许；本仓已有 `mc-repos/src/agent.rs` + `agent/` 先例） |
| `crates/mc-repos/src/channel/{mod.rs,installation.rs,binding.rs,session.rs,inbound_audit.rs,dedup.rs,outbound.rs,delivery.rs,media.rs}` | 建模块树 + `pub use`（各面实现归各片） | `crates/mc-repos/src/lib.rs` 加 `pub mod channel;` |
| `crates/mc-http/src/routes/channels/{mod.rs,slack.rs,telegram.rs,dingtalk.rs,lark.rs,wecom.rs}` | 建（`mod.rs` 聚合 5 个空 router） | 各渠道文件 anchor 期是**空 `Router::new()`** ⇒ 写者 5 片各写自己的 |
| `crates/mc-http/src/routes/mod.rs` | 加 `pub mod channels;` | 同块风险已排除（M6 收口后 M6-8 不再动本文件） |
| `crates/mc-http/src/routes/mount.rs` | 加 `mount_slice_channel()` + `.merge(...)` 一行 | **anchor 期合并后注册键逐字不变**（空 router）⇒ ⑦ 无变化 |
| `crates/mc-http/src/state.rs` | 加 `channel_keys: ChannelKeys`（5 个 `Option<SecretBox>` 的聚合）+ 唯一出口 | **不新增 `AppState::new` 参数**；在构造体内读 5 个 env（`PluginSecretKey::from_env` 先例） |
| `crates/mc-http/src/routes/auth.rs` | 测试 `AppState { … }` 字面量补字段 | 全仓唯一字面量构造点 |
| `apps/mc-server/src/channels.rs` | 新建 | `Registry` 装配（5 个 `register` 调用）+ `start(...) -> ChannelHandles`（**anchor 期空跑**：无密钥 ⇒ 不装配、返回空 handle） |
| `apps/mc-server/src/main.rs` | 加 `mod channels;` + 在 `scheduler::start` **之前**调用 + 停机链一行 | 停机顺序：**先停渠道连接，再停调度器，最后停 actor** |
| `apps/mc-server/Cargo.toml` | 加 `mc-channel` `path` 边 | — |
| `Cargo.lock` | 重新生成 | 只多一个 workspace 成员；**无新外部包** |
| `docs/32` / `docs/37` | 追加偏离与口径修订 | 承接 §9 + R-M7-1…R-M7-5，**anchor 一次落** |
| `docs/fixtures/route-parity-baseline.json` | **不动** | M7-0 零路由删除；刷新归 M7-21 |

> 纪律（与 M5-0 / M6-0 相同）：anchor **不实现任何路由逻辑**、**不写任何平台 wire 代码** —— 只固定 trait、模块边界、密钥端口、宿主位与账本。
> anchor 的测试只有三类：编译（`cargo check --workspace --all-targets`）、门 ⑦/⑩ 读数、`secretbox` 的 **5 条向量用例**
> （seal→open 往返 / 篡改一字节必失败 / 密文短于 `nonce+tag` 必失败 / `load_key` 拒空串与错长度 / `Debug` 不含密钥字节）。

---

## 6. 门禁与验收

### 6.1 门 ⑦（路由对齐）

| 时点 | local | implemented | known_gap | owners.M7 | baseline | 备注 |
|---|---:|---:|---:|---:|---:|---|
| `2394bfcc`（现状） | 405 | 329 | 127 | **24** | 344 | local_only 9 |
| `LUM-1675`（M6-INT）后（**预测**） | 406 | 330 | 126 | 24 | 344 | M6-8 的 `POST /api/plugin-bridge/v1/hooks/{key}` 落地 |
| M7-0 后 | 406 | 330 | 126 | 24 | **344（不动）** | anchor 0 路由、0 占位删除 ⇒ **本波是唯一不刷基线的 anchor** |
| M7-4 后 | 410 | 334 | 122 | 20 | 344 | +4 |
| M7-5 后 | 414 | 338 | 118 | 16 | 344 | +4 |
| M7-9 后 | 421 | 345 | 111 | 9 | 344 | +7 |
| M7-14 后 | 426 | 350 | 106 | 4 | 344 | +5 |
| M7-15 后 | **430** | **354** | **102** | **0** | 344 | +4 |
| M7-21（INT）后 | 430 | 354 | 102 | 0 | **430** | `--write-baseline` 344 → 430 |

不变式（每片自检）：`implemented + known_gap == 456`、`regressions == 0`、`unclaimed == 0`、`local_only == 9`（M7 不新增 local_only）。
M7-4/5/9/14/15 是**仅有的 5 个动读数的片**；其余 15 片读数必须**逐字不变**（0 路由）。

### 6.2 门 ⑨（契约等价）——M7 相关 fixture 现状与目标

```
$ cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json
totals: fixtures 365 · pass 5 · mismatch 23 · unmounted 31 · unevaluable 306
```

按「路径落在 M7 24 条路由上」过滤（复算见 §10 命令 4）：**12 条**。

| fixture | 路由 | 现状 | 由哪片转绿 |
|---|---|---|---|
| `workspaces/TestListLarkInstallations_NotConfiguredReturnsEmpty` | `GET …/lark/installations` | unmounted | **M7-14** |
| `workspaces/TestListLarkInstallations_NotConfigured_HardCodedInstallSupportedFalse` | 同上 | unmounted | **M7-14** |
| `workspaces/TestListLarkInstallations_StubClientReportsInstallNotSupported` | 同上 | unmounted | **M7-14** |
| `workspaces/TestBeginLarkInstall_NotConfigured` | `POST …/lark/install/begin` | unmounted | **M7-14** |
| `workspaces/TestGetLarkInstallStatus_NotConfigured` | `GET …/lark/install/{sessionId}/status` | unmounted | **M7-14** |
| `workspaces/TestRevokeLarkInstallation_NotConfigured` | `DELETE …/lark/installations/{id}` | unmounted | **M7-14** |
| `lark/TestRedeemLarkBindingToken_NotConfigured` | `POST /api/lark/binding/redeem` | unmounted | **M7-14** |
| `workspaces/TestListTelegramInstallationsNotConfiguredReturnsEmpty` | `GET …/telegram/installations` | unmounted | **M7-5** |
| `agents/TestDingTalkGroupsThroughRouterSupportsFilteredWorkspaceAndAgentScopes#5` | `GET /api/agents/{id}/dingtalk/groups` | unevaluable | **M7-9** |
| `workspaces/TestForgetDingTalkGroup_AdminOnlyAndKeepsInstallation#11` | `GET …/dingtalk/groups` | unevaluable | **M7-9** |
| `workspaces/TestDingTalkGroupsThroughRouter…Scopes#7` | `GET …/dingtalk/groups` | unevaluable | **M7-9** |
| `workspaces/TestDingTalkGroupsThroughRouter…Scopes#8` | `GET …/dingtalk/groups` | unevaluable | **M7-9** |

⇒ 本波**承诺 8 条 `unmounted → pass`**（全部是「未配置语义」用例，M7-14 七条 + M7-5 一条）。
剩下 4 条 `unevaluable`（dingtalk 群的三方 scope 矩阵，需要多 actor + 真库）**只承诺「转 evaluable 并给出实测结论」**，
**不承诺 pass** —— 承接 `docs/57` §6.2 的纪律：**不许把「不可判」直接改写成「通过」**。

**另有 11 条关键词命中但不在 M7 24 条上**（只登记，不承诺）：`GET /api/chat/history` ×5 与 `GET|DELETE /api/chat/sessions*` ×2（owner **M4**）、
`POST /api/issues` ×3（owner **M2**）、`GET /api/workspaces/{id}/dingtalk/group-routes` ×1（**已退役路由**，§1.6）。

### 6.3 门 ⑩（文件大小）——预飞

门 ⑩ 只扫 `git ls-files` 的代码文件（`docs/**` 不查），规则「只减不增」，清单外硬限 **800 行**。
M7 写集里**没有任何文件**在 `scripts/file_size_baseline.tsv`（该表只剩 10 个条目：`mc-conformance/src/lib.rs`、`routes/auth.rs`、`routes/inbox.rs`、
`tests/inbox.rs`、`mc-repos/src/{comment,inbox,invitation,issue}.rs`、两个 `scripts/*.py`）⇒ **全部走 800 行硬限**。预飞：

| 本地文件 | 当前 | 计划 | 风险与对策 |
|---|---:|---|---|
| `crates/mc-http/src/state.rs` | 530 | ≈680 | 安全（+150） |
| `crates/mc-core/src/channel.rs` | 85 | ≈150 | 安全；溢出走 `channel/*.rs` 子模块（anchor 已建） |
| `crates/mc-http/src/routes/mount.rs` | 341 | ≈370 | 安全 |
| `crates/mc-http/src/routes/mod.rs` | 89 | ≈95 | 安全 |
| `apps/mc-server/src/main.rs` | 213 | ≈235 | 安全 |
| `crates/mc-channel/src/wecom/ws_frame.rs` | — | 600–800 | 上游 1,187 行 ⇒ 按「帧编解码 / 帧路由」拆两文件 |
| `crates/mc-channel/src/wecom/typing.rs` | — | 500–800 | 上游 978 行 ⇒ 与 `stream_store.rs`（1,122）分文件 |
| `crates/mc-channel/src/lark/http_client.rs` | — | 700–800 | 上游 1,370 行 ⇒ 按「请求构建 / 响应解码 / 错误分类」拆 |
| `crates/mc-channel/src/telegram/outbound.rs` | — | 800+ | 上游 1,633 行 ⇒ **必须**先拆（发送 / 更新 / 媒体三段） |
| `crates/mc-channel/src/wecom/relay.rs` | — | 700–800 | 上游 1,578 行 ⇒ 按「重投递链 / 优先级队列」拆 |
| `crates/mc-http/src/routes/channels/dingtalk.rs` | — | 400–600 | 上游 handler 782 行，含 7 条路由 |
| `crates/mc-repos/src/channel/*.rs` | — | 300–500 | 22 张表按面分 9 文件（§3.3） |

⇒ **四片必须在实现前先拆分**：M7-6（telegram/outbound）、M7-10（lark/http_client）、M7-16（wecom/ws_frame）、M7-17（wecom/relay）。
拆分是**回归上游结构**（上游 `*_test.go` 与实现文件同目录分文件），不是凑门。

### 6.4 迁移条数 = **0**

22 张渠道表**全部已在** `migrations/upstream/`（复算见 §10 命令 5）：

| 族 | 表 | 张数 |
|---|---|---:|
| 泛化渠道层（slack / wecom / telegram 走这层，lark 也在迁） | `channel_installation`、`channel_binding_token`、`channel_user_binding`、`channel_chat_session_binding`、`channel_chat_context_generation`、`channel_inbound_audit`、`channel_inbound_message_dedup`、`channel_outbound_message`、`channel_outbound_card_message`、`channel_reply_delivery`、`channel_task_delivery`、`channel_media_pending_object` | 12 |
| lark 遗留（泛化前的 per-channel 表，**现仍在用**） | `lark_installation`、`lark_binding_token`、`lark_user_binding`、`lark_chat_session_binding`、`lark_inbound_audit`、`lark_inbound_message_dedup`、`lark_outbound_card_message` | 7 |
| dingtalk 专属 | `dingtalk_bot_identity`、`dingtalk_group_presence`、`dingtalk_group_route` | 3 |

⇒ 本波**不写任何 `migrations/**`**，也不刷 `contracts/upstream-apply-exceptions.tsv`。门 ⑧（schema-drift）在 INT 片跑一次即可。
⚠️ 注意 **lark 两套表并存**（泛化层 + 遗留层）：`lark_installation` 与 `channel_installation` 在上游**同时在用**
（`grep -rn 'lark_installation' --include=*.go server/internal/integrations | grep -v _test` 25 处命中）⇒ 仓储层**不得**把 lark 强行并到泛化层（会静默丢数据），这条写进 M7-14 的 DoD。

### 6.5 每片 DoD（通用 + 专属）

**通用（每片都跑，命令见 §10 命令 6）**

1. `bash scripts/gates.sh` **8/8 绿**；触碰 DB 的片（除 M7-0/10/11 外的机乎全部）追加 `--with-db` **10/10**。
2. ⑦ 读数与 §6.1 该片行一致（`implemented + known_gap == 456`、`regressions == 0`、`local_only == 9`）。
3. 形态门：本片不得引入 `MISSING_ALIAS` / `MISSING_EXACT` / `EXTRA_ALIAS`（M7 无欠账 ⇒ 三类都是硬失败）。
4. ⑩：新文件 ≤800 行；`scripts/file_size_baseline.tsv` 不动或缩小。
5. 每条路由至少一条测试（handler 级或 e2e），且**不用** `health::placeholder`。
6. 凭据面：手写 `Debug` 脱敏 + 「错误路径不回显凭据」用例（§2.3）。
7. 偏离（Redis 替身 / 未接字段 / 单副本假设）必须写进 `docs/32` 偏离表。

**专属**

| 片 | 专属验收 |
|---|---|
| M7-0 | ⑦ 读数逐字不变（`406/330/126`，baseline 344 不动）；`cargo metadata` 通过且 `mc-channel` 是成员；`secretbox` 5 条向量用例；`mount_slice_channel()` 是空 router（注册键集合逐字不变） |
| M7-1 | `Channel` trait 的 5 方法签名与上游 `channel.Channel` 逐条对应（`connect` 阻塞语义有文档 + 一条「取消即返回 nil」用例）；`Registry` last-writer-wins + `ErrUnknownType`；`Capability` 8 位 `String()` 稳定 |
| M7-2 | 会话状态机（含 `channel_chat_context_generation` 的代际语义）；去重命中 ⇒ **丢弃且不报错**；租约 acquisition/release；退避重连时间序列可测（注入 `Now`） |
| M7-3 | slack 入站：信封解析 + `events_api` 去重 + 未绑定发件人 ⇒ 绑定卡（**不出错**） |
| M7-4 | **slack 端到端回路（§4.2）**；4 条路由的未配置语义；`/slack/binding/redeem` 幂等（重复兑换不重复插行） |
| M7-5 | telegram 入站 `getUpdates` offset 持久化；4 条路由未配置语义（`TestListTelegramInstallationsNotConfiguredReturnsEmpty` 转 pass） |
| M7-6 | **telegram 端到端回路**；出站发送的 429/重试与 `delivery` 状态机 |
| M7-7 | dingtalk Stream 帧编解码（照 `ws_frame.rs`）+ 4 份 `testdata/*.json` golden 全部解码通过 |
| M7-8 | **dingtalk 端到端回路**；引用卡片 / 互动卡片 / 媒体三类出站帧字段级断言 |
| M7-9 | 7 条路由；**含 agent 级 1 条**（scope 矩阵：workspace × 私有 agent）；`group-routes` **必须 404**（§1.6）；4 条 `unevaluable` fixture 转 evaluable 并给结论 |
| M7-10 | lark HTTP 客户端：`tenant_access_token` 缓存与过期刷新；错误分类（限流/失效/网络）各一反例 |
| M7-11 | WS 长连接：`/callback/ws/endpoint` 引导 + 分片重组（`ws_chunk_assembler` 语义）+ 帧解码器 350 行的等价用例 |
| M7-12 | lark 入站：富文本 flatten、媒体引用、@提及、union_id/region 回填的可测性 |
| M7-13 | **lark 端到端回路**；卡片 patch（`message_edit` 能力位）与打字指示 |
| M7-14 | 5 条路由；**7 条 fixture 转 pass**；设备流扫码会话（`begin` → `status` 轮询 → 终态）；**不得把 lark 并入泛化表**（§6.4） |
| M7-15 | 4 条路由；BYO 凭据落库 = `secretbox` 密文（**明文不得入库**，反例：明文入库即失败）；5 个部署密钥缺失时的降级 |
| M7-16 | wecom WS 帧编解码 + `ws_sender` 的并发写；帧大小上限 |
| M7-17 | **wecom 端到端回路**（收在 M7-19）；中继重投递链的幂等与顺序 |
| M7-18 | 媒体下载/上传 + `media_guard`（CIDR 白名单）+ `media_crypt`；SSRF 反例（非白名单 CIDR 一律拒） |
| M7-19 | wecom 入站 + **端到端回路**；`dedupe` 命中语义；seal/outcome 记账 |
| M7-20 | 打字指示生命周期（开始/续期/结束，含超时兜底）；限流桶按安装分片；sentinel 注册表的并发读写 |
| M7-21 | 快照三件套刷新（⑦/⑨/⑩）+ `--write-baseline`（344 → 430）+ 缺口登记；**无代码改动** |

---

## 7. 晋升顺序与前置

1. **M6 全合**：`LUM-1673`（M6-8）与 `LUM-1675`（M6-INT）落地。理由两条：
   (a) 三个共享文件（`routes/mod.rs` / `routes/mount.rs` / `state.rs`）与 M6 同块（§3.2）；
   (b) ⑦ 基线/快照的刷新归 M6-INT，M7-0 要读**刷新后**的 base sha 与读数（`docs/37` §46 的 lesson：切片起手必须重取当轮 base sha）。
   M6-8 在飞（2026-09-24 17:29Z 起）时**不得**提前开 M7-0。
2. **并发位**：`docs/plan1.md` 的 3 槽约束 ⇒ 最多同时 3 片；本文 §4.3 的 9 个 stage 已按此排。
3. **本片（`LUM-1764`）在 M6 收口后即可整体交付**：它是 docs + fixture + 建 issue 片，**不改任何 `.rs`**、不动 `migrations/**`、不动 `Cargo.lock`、不刷基线。
4. 子 issue（全部 `backlog`，`--parent LUM-1764`，stage 与 §4.1 一一对应）：

| # | 切片 | 子 issue | stage | 路由 |
|---|---|---|---|---:|
| 1 | M7-0 anchor | **LUM-1765** | 1 | 0 |
| 2 | M7-1 渠道契约层 + engine 路由/监管/解析 | **LUM-1766** | 2 | 0 |
| 3 | M7-2 engine 会话/命令/租约 | **LUM-1767** | 2 | 0 |
| 4 | M7-3 slack 入站回路 | **LUM-1768** | 2 | 0 |
| 5 | M7-4 slack 出站 + 安装与绑定面 | **LUM-1769** | 3 | 4 |
| 6 | M7-5 telegram 入站 + 安装与绑定面 | **LUM-1770** | 3 | 4 |
| 7 | M7-6 telegram 出站与投递 | **LUM-1771** | 3 | 0 |
| 8 | M7-7 dingtalk 入站与 Stream 连接 | **LUM-1772** | 4 | 0 |
| 9 | M7-8 dingtalk 出站/媒体/回复 | **LUM-1773** | 4 | 0 |
| 10 | M7-9 dingtalk 安装/凭据/群身份 + agent 群面 | **LUM-1774** | 4 | 7 |
| 11 | M7-10 lark 客户端与类型 | **LUM-1775** | 5 | 0 |
| 12 | M7-11 lark 长连接（WS） | **LUM-1776** | 5 | 0 |
| 13 | M7-12 lark 入站回路 | **LUM-1777** | 5 | 0 |
| 14 | M7-13 lark 出站/回复/会话桥 | **LUM-1778** | 6 | 0 |
| 15 | M7-14 lark 安装与绑定面 | **LUM-1779** | 6 | 5 |
| 16 | M7-15 wecom 契约/凭据/安装与绑定面 | **LUM-1780** | 6 | 4 |
| 17 | M7-16 wecom WS 帧与发送 | **LUM-1781** | 7 | 0 |
| 18 | M7-17 wecom 中继与出站回复 | **LUM-1782** | 7 | 0 |
| 19 | M7-18 wecom 媒体面 | **LUM-1783** | 7 | 0 |
| 20 | M7-19 wecom 入站与解析 | **LUM-1784** | 8 | 0 |
| 21 | M7-20 wecom 打字/限流/去重/追踪 | **LUM-1785** | 8 | 0 |
| 22 | M7-21 INT | **LUM-1786** | 9 | 0 |

（路由账：4 + 4 + 7 + 5 + 4 = 24 ✓；stage 分布 1/3/3/3/3/3/3/2/1 = 22）

> 晋升规则与 M5/M6 相同：`backlog → todo` 才起跑；同 stage 内三片可并行；上一 stage 未合不进下一 stage。

---

## 8. 风险登记（每条对应一个 DoD 或一个「登记不实现」的决定）

| ID | 风险 | 缓解 / 决定 |
|---|---|---|
| **R-M7-1** | **无 Redis**：上游 4 处 Redis 用于跨副本协调（租约 CAS / 入站去重 / 安装会话 / 出站重投递） | 换进程内实现 + **单副本部署契约**（§2.5）。上游 4 处都有等价降级语义（`router.go:742` 明文 warn）⇒ 不是伪造行为，是换部署形态。进 `docs/32`；**不引入 Redis 依赖** |
| **R-M7-2** | 5 个平台 API 无法在 CI 真连 ⇒ 「真实收发回路」门禁可能被"假绿"绕过 | 替身三条纪律（§4.2）：只替 wire 不替业务、帧逐字段断言、两个反例必测；5 片 DoD 逐条点名 |
| **R-M7-3** | 5 个部署密钥的「未配置」语义**按端点而异**（lark 列表 200 空 + `install_supported:false`，wecom 端点 503） | §2.4 + ⑨ 的 7 条 lark fixture 逐条钉；禁止"统一 503" |
| **R-M7-4** | `secretbox` 会有两份实现（M6 的 `PluginSecretKey` 在 `mc-http`，M7 的在 `mc-secrets`） | M6 面**冻结不改**（已合）；M7 只新增模块（§2.3）。收敛票另开，登记在 `docs/32`，**不在 M7 写集内** |
| **R-M7-5** | lark **两套表并存**（泛化 `channel_*` + 遗留 `lark_*`） | M7-14 DoD 明写「不得合并」；仓储层按上游实际读取路径实现（§6.4） |
| **R-M7-6** | 渠道**读侧**有一条属 M4（`GET /api/chat/history`）⇒ 渠道"M7 全绿"不等于渠道面全绿 | §1.3 写明边界；M7 只提供数据，不重实现该路由；INT 片登记 |
| **R-M7-7** | 长连接在 `apps/mc-server` 引入后台任务 ⇒ 停机顺序错误会让连接挂着不退 | 停机链固定为「先停渠道连接 → 再停调度器 → 最后停 actor」（§2.4），并各写一条停机用例 |
| **R-M7-8** | 20 片是本仓迄今最大一波（M6 是 9+1+1）⇒ 派发/在飞管理复杂度上升 | 9 个 stage × ≤3 并发；每 stage 的 0 路由片与有路由片混排，避免同一 stage 内三片都改 `state.rs`/`mount.rs`（这三处只在 M7-0 动） |
| **R-M7-9** | 媒体面（wecom 7 文件 2,614 行 / lark 501 行 / dingtalk 385 行）是 SSRF 与内容注入的高风险面 | `media_guard`（CIDR 白名单）与 `media_crypt` 的**反例测试**是 M7-18 的 DoD；出站媒体同样过 redaction 纪律（§2.3） |

---

## 9. 与 `docs/plan1.md` §5 / §8 的差异（口径修订，逐条）

### 9.1 口径修订一：§1.2 那行 `wecom 89 / dingtalk 76 / lark 72 / …` 的单位是「**一切文件数**」

* 原文：`internal/integrations | 153 | 48,907 | wecom 89 / dingtalk 76 / lark 72 / channel 42 / slack 32 / telegram 23 / ghsnapshot 7 / composio 6 / vcs 4`
* 实测：子目录那串 = **`find <dir> -type f | wc -l`（递归、含 `_test.go` 与 `testdata/*.json`）**；总量那串 = **非测试 `.go` 文件数 / LOC**。⇒ **一行两个单位**。
* 两处偏差：`wecom` 实测 **90**（原文 89，差 1，求和 351 vs 352）；LOC 实测 **49,044**（原文 48,907，差 137 ≈ 三个 `doc.go` 的 138 行）。
* M7 报数一律用自己实测的 **149 文件 / 48,366 行**（§1.2 第二张表），**不引用** 351/48,907。

### 9.2 口径修订二：§5 W7 行把 `internal/integrations`(48.9k) 当成"一个波次的一块"，实际它是**一个 crate + 9 个 stage**

* 原文的 W7 工时 **5 周**、验收「每渠道至少 1 条真实收发回路」**成立**，但"上游对应"一栏只写 `internal/integrations`(48.9k) ⇒ 读者会以为是一次性落地。
* 实测构成：渠道面 46,198 行**不含** `composio`(1,050) / `ghsnapshot`(1,147) / `vcs`(649) —— 这三者分别归 W9/W8/W8，
  `plan1.md` §5 的 W8 行写 `integrations/vcs`、W9 行写商业面，与本文一致；**但 §1.2 把它们和渠道混在同一行里**，派发时容易误算 M7 体量（48,907 vs 本文的 48,366）。
* 结论：**W7 的体量口径以本文 §1.2 为准**（渠道面 46,198 上游行 + handler 2,168 = 48,366）。

### 9.3 口径修订三：渠道**入站不在 456 条路由表里**（§1.3 的 440 已复核）

* `plan1.md` §1.3 的「456 条，其中 `/api`+`/auth` 440 条」**实测成立**（16 条非 `/api`/`/auth`，逐条见 §1.5，**0 条渠道入站**）。
* 但 `plan1.md` 未点出这个后果：**渠道的"真实收发回路"门禁不可能用 HTTP 层 e2e 表达**（入站是出站长连接）⇒ 验收必须落 §4.2 的替身方案。
  这是本文对 §5 W7 行验收判据的**唯一实质性补充**。

### 9.4 口径修订四：§8 的进度度量缺「owners 归零」这一维

* `plan1.md` §8 用航线图与百分比度量；M5/M6 两轮的实际纪律是**按 owner 归零**（`owners.M5 = 0`、`owners.M6 = 0`）。
* 本文沿用：M7 的终点是 **`owners.M7 = 0`** + `implemented + known_gap == 456` 恒等（§6.1），而不是"完成度 x%"。

### 9.5 与 M6 计划（`docs/57`）的结构一致性

承接 `docs/57` 的骨架（§0 速览 / §1 测绘 / §2 架构 / §3 写集 / §4 切片 / §5 anchor / §6 门禁 / §7 晋升 / §8 风险 / §9 差异 / §10 复算），
并沿用其三条纪律：**写集逐字路径**、**`unevaluable` 不许改写成 `pass`**、**落笔前 `git fetch` 复核号段**（本轮 `58` 已被 M6-INT 预留、`59` 是 M2-E，本文取 `60`）。
**与 M6 的两处结构差异**：（a）M7 无 allowlist 退路**也不需要**退路（形态欠账 0）；（b）M7-0 是五轮里**第一个不刷 ⑦ 基线的 anchor**（零路由删除）。

---

## 10. 复算命令（全部只读，可在任意 workdir 复现）

```bash
# 前置：本片的上游只读副本（钉住 commit f41fae6b08fb）
#   UP_ROOT=<本 run 的 workdir>；上游 = $UP_ROOT/multica（git commit f41fae6b08fb），本地 = $UP_ROOT/paperclip-rs
#   纪律：副本只克隆进**本 run 的 workdir**，不依赖 /tmp 或别的 run 的 workdir（会被 GC 掉）
multica repo checkout https://github.com/louloulin/multica
cd multica && git log --oneline -1 f41fae6b08fb   # ⇒ f41fae6b0 fix(cursor): own Windows background shells…
# 本地仓：git fetch && git log --oneline -1         # ⇒ 2394bfcc

# 1. 路由表：本文 §1.1 与上游 owner=M7 行集合相等（应无输出）
diff <(awk -F'\t' '!/^#/ && $3=="M7"{print $1"\t"$2}' docs/fixtures/upstream-routes.tsv | sort) \
     <(grep -v '^#' docs/fixtures/m7-declared-routes.tsv | tail -n +2 | sort)

# 1b. 渠道面没有散落在别的 owner（应只输出 "M7"）
grep -iE 'slack|lark|dingtalk|wecom|telegram' docs/fixtures/upstream-routes.tsv | awk -F'\t' '{print $3}' | sort -u

# 1c. 每渠道条数（应为 slack=4 lark=5 dingtalk=7 wecom=4 telegram=4）
for c in slack lark dingtalk wecom telegram; do printf "%s=%s " $c "$(grep -ciE "$c" docs/fixtures/upstream-routes.tsv)"; done; echo

# 2. 形态门（预测模式：M7 期望 `0 defect(s)` 且 **exit 0**，与他波的 FAIL:N 不同）
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv
python3 scripts/slash_alias_audit.py                    # 本地实况：0 defect

# 3. 上游 16 条非 /api、/auth 路由（§1.5；应为 16 条且无渠道入站）
awk -F'\t' '!/^#/ && NF>=2 && $2 !~ /^\/(api|auth)/ {print $1"\t"$2"\t"$3}' docs/fixtures/upstream-routes.tsv

# 4. ⑦ 读数（§1.3 / §6.1）
python3 scripts/route_parity.py
python3 scripts/route_parity.py --list-gaps | awk '/^    \[M7\]/,/^    \[M8\]/'

# 5. 上游文件与行数（§1.2；UP=<f41fae6b08fb 的只读副本>/server/internal/integrations）
UP=<clone>/server/internal/integrations
for d in channel slack telegram dingtalk lark wecom; do
  printf "%-10s files_all=%-4s go_nontest=%-4s loc_nontest=%s\n" "$d" \
    "$(find $UP/$d -type f | wc -l)" \
    "$(find $UP/$d -name '*.go' ! -name '*_test.go' | wc -l)" \
    "$(find $UP/$d -name '*.go' ! -name '*_test.go' -exec cat {} + | wc -l)"
done
# ⇒ channel 42/21/5315 · slack 32/16/4490 · telegram 23/12/4504 · dingtalk 76/25/5910 · lark 72/37/10957 · wecom 90/33/15022
#   小计 335 / 144 / 46198；加 handler 5 文件 2168 ⇒ M7 全波 149 文件 / 48366 行
wc -l <clone>/server/internal/handler/{slack,lark,dingtalk,telegram,wecom_web}.go

# 6. 逐文件分配核对（§1.2 / §4.1 的底稿）：表内行数必须与上游副本逐文件一致（应无 MISMATCH）
awk -F'\t' '!/^#/ && $1!="SLICE"{s[$1]+=$3} END{for(k in s) printf "%-6s %6d\n", k, s[k]}' \
  docs/fixtures/m7-slice-upstream-files.tsv | sort -V
# ⇒ M7-1 3317 · M7-2 1998 · M7-3 1786 · M7-4 2986 · M7-5 2062 · M7-6 2707 · M7-7 2135 · M7-8 2486 · M7-9 2071
#    M7-10 2215 · M7-11 1793 · M7-12 2316 · M7-13 2253 · M7-14 2818 · M7-15 2171 · M7-16 3437 · M7-17 3202
#    M7-18 2614 · M7-19 1785 · M7-20 2214   （最大 3437 ≤ 3500）
awk -F'\t' '!/^#/ && $1!="SLICE"{s+=$3} END{print "合计", s}' docs/fixtures/m7-slice-upstream-files.tsv   # ⇒ 合计 48366
# 逐文件行数核对（UP = 上游副本的 server/ 目录；应无 MISMATCH）
while IFS=$'\t' read -r slice path loc; do
  case "$slice" in \#*|SLICE|"") continue;; esac
  real=$(wc -l < "$UP/$path"); [ "$real" = "$loc" ] || echo "MISMATCH $path $loc != $real"
done < docs/fixtures/m7-slice-upstream-files.tsv

# 7. 22 张渠道表是否都在（§6.4；每行应输出 1，缺 CREATE 的表数为 0）
for t in channel_binding_token channel_chat_context_generation channel_chat_session_binding \
         channel_inbound_audit channel_inbound_message_dedup channel_installation \
         channel_media_pending_object channel_outbound_card_message channel_outbound_message \
         channel_reply_delivery channel_task_delivery channel_user_binding \
         dingtalk_bot_identity dingtalk_group_presence dingtalk_group_route \
         lark_binding_token lark_chat_session_binding lark_inbound_audit \
         lark_inbound_message_dedup lark_installation lark_outbound_card_message lark_user_binding; do
  printf "%-40s %s\n" "$t" "$(grep -rliE "CREATE TABLE (IF NOT EXISTS )?\"?$t\"?[ (]" migrations/upstream/*.sql | wc -l)"
done
grep -rliE "channel|dingtalk|lark|slack|wecom|telegram" migrations/upstream/*.sql | wc -l   # ⇒ 62

# 8. ⑨ M7 相关 fixture 现状（§6.2；应 12 条：8 unmounted + 4 unevaluable）
python3 - <<'PY'
import json,re,collections
d=json.load(open('crates/mc-conformance/report.json'))['fixtures']
decl=[l.rstrip('\n').split('\t') for l in open('docs/fixtures/m7-declared-routes.tsv') if l.strip() and not l.startswith('#')][1:]
segs=lambda p:[s for s in p.strip().rstrip('/').split('/')]
keys=[(m,segs(p)) for m,p in decl]
def hit(m,p):
    ps=segs(p)
    return any(m==dm and len(ds)==len(ps) and all(a.startswith('{') or a.startswith(':') or a==b for a,b in zip(ds,ps)) for dm,ds in keys)
rows=[f for f in d if hit(f['method'],f['path'])]
print(len(rows), dict(collections.Counter(f['outcome'] for f in rows)))
PY

# 9. 门 ⑩ 预飞：M7 写集是否已有人被列入白名单（应无输出）
grep -E 'mc-channel|routes/channels|mc-core/src/channel|mc-secrets/src/secretbox' scripts/file_size_baseline.tsv

# 10. 全量门禁（每片交付前；有 DB 触碰的片追加 --with-db）
bash scripts/gates.sh
```

---

## 11. M7-INT 落地记录（`LUM-1786`，**0 代码**）

> **本节由 M7-21（`LUM-1786`）填写**（§4.1 的 stage 9 / §6.5 的 M7-21 行）。
> **起手 base = `dd0e8c51`**（= M10-0 的 `4f11bb7f` + 09:30 cycle `LUM-2122` 的 `docs/37` §134 docs-only 直推）；
> 全波 **1 anchor + 20 代码片**全部合入 ⇒ 头号硬前置 **`owners.M7 = 0` 当场成立**（判据 = `owners` 字典里**没有 `M7` 键**）。
> 本节每个数都在**当轮**实测（命令见 §11.9）；**§6.1 的计划期预测值一律标为过期**。
> 详细段落：偏离表收口见 `docs/32` §40、口径表收口见 `docs/37` §135。

### 11.1 ⑦（路由对齐）：终点读数与硬前置

```
$ python3 scripts/route_parity.py
upstream 456 (commit f41fae6b08fb) | local 473 registered | baseline 473
  implemented  388 real +   3 placeholder =  391 / 456   known_gap   65   unclaimed    0   regression   0   local_only    8
  gaps by owner: M9=33  M3+=16  M3=11  M10=5
OK: every upstream route is either implemented or owned
```

* **硬前置（当轮实测）**：`owners` 里**没有 `M7` 键** ⇒ 24 条 M7 上游路由**全部落地**（`docs/fixtures/m7-declared-routes.tsv` 的 24 行逐条对上 `implemented`，**缺 0** —— 复算见 §11.9 命令 3）。
* **不变式**：`implemented + known_gap = 391 + 65 = 456` ✓；`unclaimed 0` ✓；`regression 0` ✓。
* `files_scanned 206`（M7-0 起手时 201 ⇒ 本波新增 5 个 `crates/mc-http/src` 文件 = `routes/channels/{mod,slack,lark,dingtalk,wecom}.rs` 一族；其中 `wecom.rs` 由 M7-15 建、其余四片各追加自己的平台段）。

### 11.2 与 §6.1 计划表的偏差（逐项，一处不抹平）

| 项 | §6.1 的计划（`2394bfcc` 计划期） | 当轮实测（交付树） | 差异与原因 |
|---|---:|---:|---|
| `local` | 430 | **473** | **+43**：§6.1 只算 M7 自己那本账（405 → 430），而 base 上还有**别波的合法增量**（M8 全波 + M10-0 的 −1 幽灵占位）⇒ `430` 是「M7 一家」的数，`473` 是「全仓」的数 |
| `implemented` | 354（全 real） | **391 = 388 real + 3 placeholder** | **+37 real**：同样是全仓口径（含 M8/M9）；**M7 自己的 24 条全部是 real（0 占位）** |
| `known_gap` | 102 | **65** | `M7` 键**消失**；余下的 `{M9 33, M3+ 16, M3 11, M10 5}` 全属别的波 |
| `owners.M7` | 0 | **0（键不存在）** | ✓ 唯一逐字命中的一项 |
| `baseline` | `344 → 430` | **`457 → 473`（+16）** | 见 §11.3：`344` 是计划期值，`457` 是 **M10-0 的删键型最小刷新**之后的当轮**片前值** |
| `local_only` | 9 | **8** | **−1 不是 M7 的回归**：M10-0（`LUM-2102`）预删幽灵占位 `GET /api/feature-flags`（上游**无**此键）⇒ `local_only 9 → 8` ⇒ **§6.5 第 2 条的 `local_only == 9` 这条不变式从 M10-0 起就已过期** |

⇒ **§6.1 整张表只能作"形态预测"读**（每片的 ⊿、`owners.M7` 单调递减到 0、`--write-baseline` 由 INT 独占、`regressions/unclaimed` 恒 0）；**任何绝对读数都必须在当轮重取**。这一波为此留下了 **5 个修订版**（issue 的 rev 2 → rev 16），最后一次作废的是 `baseline 458` / `local 474` 这一对。

### 11.3 `--write-baseline`：**`457 → 473`**（本波唯一一次）

* **片前值 = `457`** = 当轮 `baseline` 字段的实测值。`344`（计划期）/ `406`（M8-INT 前）/ `430`（计划预测）/ `458`（M8-INT 后）**都已过期**。`457` 的来处 = M8-7 INT（`LUM-1804`）的整树刷新 `406 → 458` **再减** M10-0 的删键型最小刷新（`458 → 457`，只删幽灵占位一键）。
* **片后值 = `473` = 当轮 `local` 的实测值**（`local` 数的是**未折叠的注册点**：`/x` 与 `/x/` 各算一条，`docs/37` §95 的先例 ⇒ **不许**按折叠键推算）。`+16` **逐条都是 M7 自己的渠道键**：M7-9 dingtalk 7 + M7-14 lark 5 + M7-15 wecom 4；三片落地（`32def21a` 09-25 13:33 / `c45a5881` 09-25 22:02 / `0fad9de7` 09-25 16:45）**都晚于** M8-7 的最后一次整树刷新 `f3e9794e`（09-25 13:18），逐条取证见 `docs/32` §39.3。
* **幂等已由本片独立复核**：在**交付树**上再跑一次 `python3 scripts/route_parity.py --write-baseline`，产出与文件**逐字节相同**（`git diff` 空、两版 JSON 归一后 `a == b`、键数都 473）⇒ 这次刷新的语义就是「把基线对齐到当轮 live 集」，**没有人手改格式**。
* **与别的基线写者不同轮**：M8-INT 的配额已用掉；**M9-INT（`LUM-1825`）与 M10-9 INT 都未起手** ⇒ 本轮**唯一**刷这个文件的片就是本片。
* **形态门没有退路**：`docs/fixtures/slash-alias-allowlist.tsv` 是 **0 数据行**（M7 的 24 键上游全是 plain 注册，§1.4 实测双形态 0 键）⇒ `MISSING_ALIAS` / `MISSING_EXACT` / `EXTRA_ALIAS` 任一出即红。当轮：`0 defect(s) / 0 warning(s)`；`registered upstream-key literals = 470`（= 469（`c23fcfad` 实测）+ 1：M10-0 的 `crates/mc-http/src/routes/config.rs` 里的 `/api/config` 字面量，**不是** M7 加的）。

### 11.4 门 ⑨（契约等价）：**本片不动**，只核对

* 当轮：`report.json` blob `3eb0430a39c8cf6c50ccafe626542abba3eaa0d4`；totals `fixtures 365 / pass 14 / mismatch 23 / unmounted 22 / placeholder 0 / unevaluable 306`（`contract_equivalence_rate 0.038356` / `mounted_equivalence_rate 0.378378`）；门 ⑨ **绿**（`report matches`）。
* **M7 面的 12 条**（按 §6.2 的 24 条路由过滤）实测 = **8 `pass` + 4 `unevaluable`**：
  * **8 条 `pass`** = §6.2 承诺的那 8 条（lark 7 + telegram 1）—— **已由 M7-14 随 PR #111 刷进快照**（`pass 7 → 14` / `unmounted 29 → 22`），本片**核对**、**不重复刷**；
  * **4 条 `unevaluable`**（全是 `GET …/dingtalk/groups`，`actor=member`、`via=router|handler`）= §6.2 那 4 条 dingtalk 三方 scope 矩阵 fixture ⇒ **本波结束时仍是 `unevaluable`**（承诺原文只承诺"转 evaluable + 给实测结论"）⇒ 如实登记为缺口 **G-4**。
* **wecom 的 fixture = 0 条**（**阴性对照组**，连续第 6 轮）：M7-20 是 0 路由片 ⇒ wecom 面**不该**动 ⑨，实测也确实没动。
* **本片不刷 `report.json`**：M7 面没有新的「`unmounted` ⇒ 可判定」位移。下一批归 **M10-INT（`LUM-2111`）**（`/api/config` 17 条 + `/health` 1 条 = 18 条 `unmounted` ⇒ 离线可判定）。

### 11.5 门 ⑩（文件大小）

* 当轮 `scanned 1170 / baseline 10 / violations 0`；`scripts/file_size_baseline.tsv` **未动、未缩**（该表只剩 10 个存量条目，**M7 写集里没有任何文件在表内** ⇒ §6.3 的预飞成立）。
* **本波 0 新超限**。各片自报的最长新文件（`docs/32` §NN 的 ⑩ 行）：M7-16 **775**（`stream_store/tests.rs`）、M7-17 **705**（`relay/dispatch.rs`）、M7-18 **784**（`outbound_media.rs`）、M7-19 **773**（`wecom_channel/inbound.rs`）、M7-20 **738**（`trace.rs`）⇒ 全波最大值 **784 ≤ 800** ✓。
* ⚠️ 本波两次实测到门 ⑩ 的读数坑（§6.3 已记）：它只扫 `git ls-files` ⇒ **未 `git add` 的新文件它看不见**（M7-19 的 `round_trip.rs` 820 行而门报绿、M7-15/M7-18 也各中一次）。

### 11.6 五渠道端到端回路（§4.2 的验收判据）—— 取证位置

§4.2 的判据是「**每渠道至少 1 条真实收发回路**：替身造帧 → 真入站 → 真 DB → 真出站 → 帧回替身，中间零 mock」。当轮逐渠道取证（`git ls-files` + 用例名）：

| 渠道 | 承担片 | 取证文件（逐字路径，当轮实测存在） | 证据形态 | 判定 |
|---|---|---|---|---|
| slack | M7-4 | `crates/mc-channel/src/slack/tests.rs`（`the_slack_round_trip_closes_on_a_local_platform_stand_in` + 重放/断连两条反例）+ `crates/mc-http/tests/channels/slack.rs`（672 行，真库路由面） | **两级**：WS 替身回路（进程内）+ 真库 HTTP 面 | ✓ |
| telegram | M7-6 | `crates/mc-http/tests/channels/telegram_round_trip.rs`（616 行：`BotState` 真方法名替身 + `drive_inbound` = 真 `TelegramChannel` + 真 engine `Router` + 5 个真 PG 端口 + `drive_streaming_delivery`） | **单文件整链**（收 + 判决 + 发 + 流式），最完整 | ✓ |
| dingtalk | M7-8 | `crates/mc-channel/src/dingtalk/stream/tests.rs`（Stream 帧 + 四份 `testdata/*.json` golden）+ `dingtalk/outbound/tests/http.rs`（出站帧字段级）+ `crates/mc-http/tests/channels/dingtalk.rs`（563 行，真库 7 路由） | **三段**：帧编解码 ∥ 出站帧 ∥ 真库路由 | 部分（见下） |
| lark | M7-13 | `crates/mc-channel/src/lark/tests/round_trip.rs`（442 行：`a_bound_turn_lights_the_indicator_then_answers_in_thread` 等 9 条） | 收 + 判决 + 发（进程内 WS 替身 + 真端口） | ✓ |
| wecom | M7-19 | `crates/mc-channel/src/wecom/wecom_channel/tests/round_trip.rs`（554 行：`a_bound_text_message_travels_the_whole_pipeline` / `a_duplicate_frame_is_dropped_without_an_error`） | 收 + 判决 + 发（自造帧 + 真端口） | ✓ |

* **一处需要如实说的边界**（**不是**假绿，也不抹平）：五条回路的「真」**都是进程内的** —— `mc-channel` 里没有 `sqlx`（§36.5 R1 的边界）⇒「收 → 判决 → 发」与「仓储」分处两套证据（前者在 `mc-channel` 的替身 + 端口，后者在 `mc-http` 的真库 e2e）。**跨这两半的整链**只在 telegram（真库 + 真端口 + 真 channel 的单文件）与 dingtalk（真库 7 路由 + 出站帧替身，三点分散）上有证据；slack/lark/wecom 的「真库那一半」在 `mc-http/tests/channels/**` 有，但没有与帧回路合成一条用例。
* **更大的那一处**：这五条回路**没有任何一条**被**生产宿主**跑起来 —— 见 §11.8 的掉棒 **D-1**。

### 11.7 缺口登记（**本波实际未落地的项**，逐条；"归属"写的是**谁该做**，不是谁写了）

| # | 缺口 | 当轮证据 | 归属 / 处置 |
|---|---|---|---|
| **G-1** | **无 Redis**：上游 4 处跨副本协调（WS 租约 CAS / 入站去重 / 安装会话 / 出站重投递）**全部**换成**进程内替身** | 根 `Cargo.toml` 与 `crates/**/Cargo.toml` **零** `redis` 依赖；替身落点 = `engine/lease.rs` 的 `InProcessLeaseStore`、`wecom/dedupe.rs`（M7-20）、M7-11 的进程内安装会话表、`wecom/relay/**` 的进程内重投递 | **登记不实现**（R-M7-1）：生产部署契约 = **单副本**，或"渠道连接只在一个副本上开"。接真 Redis 属**收敛票**，不在 M7 写集内 |
| **G-2** | `secretbox` **两份实现**：`mc-plugin-host::credentials::SecretBox`（M6 面，已合、**冻结**）与 `mc-secrets::secretbox`（M7-0 新开） | 两份都是 AES-256-GCM + `nonce‖ct‖tag`，**签名不同**（M6 面 `seal(plaintext, rng)`；M7 面 `seal` 自取 nonce、另给 `seal_with_nonce` 做可复现向量） | **登记不实现**（R-M7-4 / R-M7-11）：收敛票**不在 M7 写集内**（改 `mc-plugin-host` 会动已冻结的 M6 面）。方向 `mc-plugin-host → mc-secrets` ⇒ `mc-secrets` **不能**复用它（成环） |
| **G-3** | 渠道面**读侧**有一条不属 M7：`GET /api/chat/history`（上游 owner = `M4`） | 当轮 ⑦ 实测该键在 `implemented` 里（`router.go:2373`）⇒ **M4 侧已落地**；M7 只提供它读的数据（`channel_chat_session_binding` 等表），**不重实现** | **R-M7-6 = 闭**（不是缺口）。口径一条：**"M7 全绿" ≠ "渠道面全绿"**（渠道出入站大多不在 456 条路由表里，§1.5） |
| **G-4** | **4 条 dingtalk 群 scope 矩阵 fixture 仍是 `unevaluable`**（`actor=member`） | 当轮 ⑨ M7 面 12 条 = `8 pass + 4 unevaluable`；`mc-conformance` 的 stateless 层没有 member 会话 ⇒ 这 4 条连"转 evaluable"都没做到 | **登记为缺口**（承诺原文只承诺"转 evaluable + 给结论"）：要**真库 + 多 actor**，属 **⑨ 工具面**（`mc-conformance` 的 harness），不在 M7 写集 |
| **G-5** | `channel_outbound_message` **没有** `channel_context_revision` 列 ⇒ slack history 读面用「按 `(binding_id, route_revision)` 列出的出站行」**近似** | `migrations/upstream/425_channel_outbound_message.up.sql` 只有 `route_revision`；迁移 `377` 补的是 `chat_message` / `agent_task_queue` / `channel_chat_session_binding` 三张表 | **登记为缺口**（`docs/32` §15.2 D6(a)）：方向是**多**放行同代际内早前轮次的本 bot 消息（**不会少放行**）⇒ 是上游列缺失下的**近似**，不是"漏实现" |
| **G-6** | `mc-telemetry` 的 `Redactor::is_sensitive` **未覆盖渠道键名**：`app_key` / `appkey` / `aeskey` / `dingkey` / `encrypt_key` **五个**不在 `SENSITIVE_KEYS` 的子串表里 | 当轮逐名实测：`app_secret` / `app_secret_encrypted` / `corpsecret` / `bot_token` / `app_token` / `verification_token` / `signing_secret` / `tenant_access_token` **已被**子串覆盖（表里 `secret`/`token`/`apikey` 三条兜住绝大部分）；上列五个**未覆盖**。**可达性**：`redact_log` 全仓唯一消费者是 `mc-autopilot/src/credential.rs`（autopilot 凭据路径）；M7 代码**零** `redact_log` 调用、**零** `tracing::*` 凭据插值 ⇒ **当前不可达** | **登记为缺口 + 具体洞清单**：`crates/mc-telemetry/**` **不在本片写集**（本片 0 代码）⇒ 见 §11.8 的**转派**。`aeskey` / `dingkey` / `encrypt_key` 是最容易漏的三个 |
| **G-7** | **宿主装配**（`apps/mc-server/src/channels.rs`）停在 M7-0 anchor 形态 | 见 §11.8 掉棒 **D-1**（四条实证） | **掉棒**：`apps/mc-server/src/channels.rs` 是 §3.1 的**共享锚点、其余片只读** ⇒ 全波**没有任何片**的写集包含"把 5 条 `register_with` 接上"这一步 |
| **G-8** | wecom BYO 安装的**凭据探针**在生产接线里仍是 `PendingWsTransport` ⇒ 该端点**永远回 503**（`docs/32` §31 的 D9 的待办） | `crates/mc-http/src/routes/channels/wecom.rs:158` + `:172-190`（当轮实测；M7-16 的 `ws_frame.rs`/`ws_sender.rs` 已在，但**接线那一行未换**） | **掉棒 D-2**（§11.8）：`routes/channels/wecom.rs` 是 **M7-15 的写集**而驱动它的传输是 **M7-16 的写集** ⇒ 两片都不具备改对方的授权 |

**三类"登记不实现"的裁决**（上游自身不闭合，照抄并标出，**不是缺口**）：`docs/32` §34.5 R2（`errStreamAckTimeout` 的分类 + `Resolve(absent)` 不设围栏）+ §38.5 R2（`traceOutFields` 只读 `markdown.content`，流帧正文不进 trace）。改它们会让两个部署的读数漂开 ⇒ 上游照抄 + 各一条用例钉住现状。

**其余已逐条登记的"观察项/收敛项"**（`docs/32` §40.4 收齐，这里只点名出处）：M7-8 §22.2 D12（`reply_source` 的**写入点仍缺**）、§24.2 D2 + §22 的 D4（`Decrypter` 三分法的**同一张收敛票**）、§32.2 D11（`decode_secret` 的重复实现）、§34.4 H1（`on_chat_done/on_task_failed/on_task_cancelled` 的**调用点**）、§38.4 H1（两份 `task_address` 的收敛票）、§29.5 R5（= G-6）。

> 🔴 **§31 D9 的复核（结论是"**未**转绿"，不是"已绿"）**：M7-15 的 D9 写「wecom BYO 安装得到 **503 `wecom_credentials_unverifiable`** 且一行都不写，等 **M7-16 落传输**、**M7-21 复核该端点转绿**」。**当轮复核 = 该端点仍回 503**：`crates/mc-http/src/routes/channels/wecom.rs:158` 的生产接线仍是 `HandshakeProbe::new(Arc::new(PendingWsTransport))`，而 `PendingWsTransport::subscribe_ack` 永远回 `TransportError::Failed { stage: "ws-transport-not-wired" }`（`wecom.rs:172-190`）。M7-16 **确实落了**传输（`crates/mc-channel/src/wecom/{ws_frame.rs,ws_sender.rs,stream_store.rs}` 三个文件在），但**没有人把 `wecom.rs:158` 那一行换掉** —— 因为 `routes/channels/wecom.rs` 是 **M7-15 的写集**（§3.3），M7-16 的写集只有 `crates/mc-channel/src/wecom/**`。⇒ D9 与 **D-1 是同一族**（接线点在**已冻结的写集**里）⇒ 登记为本波**第二项掉棒 D-2**（§11.8）。

### 11.8 跨波掉棒审计（照 `docs/57` §9.8 的做法）

**结论：本波有 2 项掉棒（**同一族**：接线点落在**已冻结的写集**里 —— 一边改了，另一边没人有权改）；另有 1 项被"归本片"但本片写集做不到的事，如实转派。**

| 项 | 裁定出处 | 裁定归谁 | 该片实际交付 | 判定 |
|---|---|---|---|---|
| **D-1** 五渠道**生产装配**（`register_with` × 5 + `resolver_set` / `senders` / `TypingIndicator` / `Supervisor::spawn` / `InstallationStore`+`LeaseStore` 的生产实现 + `main.rs` 的 `deps` 非 `None`） | `docs/60` §2.4 与 §3.1「宿主（`apps/mc-server/src/channels.rs`，anchor 写集）」；`docs/32` §13.5 #2 / §17.4 #1 / §18.4 #1 / §19.4 #4 / §24.4 #3 / §31 #3 / §36.4 H2 等**逐片都写"装配归宿主"** | 「宿主」= **M7-0 的写集**，而 M7-0 **早已合入**（`ab998afe`） | M7-0 只落了**宿主位**（`start(keys, deps)` 骨架 + 停机链），并把 `deps` 定为 `None` 的**显式空跑**；此后**没有任何片**被授权改这个文件 | **掉棒** |
| **D-2** wecom BYO 安装的**凭据探针接线**（把 `wecom.rs:158` 的 `PendingWsTransport` 换成 M7-16 的真传输） | `docs/32` §31.2 **D9** 逐字写「M7-16 落传输、**M7-21 复核该端点转绿**」 | 上半场归 **M7-16**（写集 = `crates/mc-channel/src/wecom/{ws_frame,ws_sender,stream_store}.rs`），下半场归 **M7-21**（本片） | M7-16 **落了传输**（三个文件在），但**接线那一行在 `crates/mc-http/src/routes/channels/wecom.rs`**（= **M7-15 的写集**）⇒ 两个片都没有改动它的授权；本片 **0 代码**也改不了 | **掉棒**（本片只能**如实登记"未转绿"**） |

**D-1 的四条实证（逐条可复算）**：

1. `git log --oneline -1 -- apps/mc-server/src/channels.rs` = **`ab998afe`**（M7-0 anchor 的提交）⇒ 该文件**全波未再被碰过**（198 行，与 anchor 逐字相同）。
2. `apps/mc-server/src/main.rs:184` = `channels::start(&channel_keys, None)` —— **`None` 是硬编码**，不是"端口还没实现"（`InstallationStore` / `LeaseStore` / `InboundHandler` 三件套的实现**早已存在**）。
3. `grep -rn 'register_with' apps crates/mc-http --include=*.rs` = **0 个生产调用点**（五个 `pub fn register_with` 全在 `mc-channel` 内，只有各自的 `#[cfg(test)]` 用）。
4. `grep -rn 'Supervisor::spawn' apps crates --include=*.rs` = **0 个生产调用点**（`supervisor.rs:354` 的实现**已成**；`channels.rs:157` 的注释仍写「`Supervisor::spawn` 现在仍是 `todo!()`」⇒ **注释也已过期** —— `grep -c 'todo!' crates/mc-channel/src/engine/supervisor.rs` = **0**）。

**D-2 的两条实证**：① `crates/mc-http/src/routes/channels/wecom.rs:158` 当轮仍 = `Arc::new(HandshakeProbe::new(Arc::new(PendingWsTransport))) as Arc<dyn CredentialProbe>`；② `PendingWsTransport::subscribe_ack` 的返回值恒为 `Err(TransportError::Failed { stage: "ws-transport-not-wired" })`，而 `routes/channels/wecom/tests/db.rs:293` 的 `#[ignore]` 真库用例（`byo_answers_503_while_the_probe_transport_is_not_wired`）就把 `wecom_credentials_unverifiable` 钉成**当前期望**（`assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE)` + 「一行都不写」） ⇒ 该用例**在当前形态下是绿的**，但它是"未接线"的绿，不是"已转绿"。

**处置**（照 §9.8.3 的执行规则）：本片**只登记**（写集不含 `apps/**` 与 `crates/mc-http/src/**`，且本片 0 代码），**不**顺手改。**建议**：开一片 M7-FU 切片，写集 = `apps/mc-server/src/{main.rs,channels.rs}` + `crates/mc-http/src/routes/channels/wecom.rs`。**该片交付时的可见判据**：ⓐ`main.rs` 传 `Some(deps)`、`channels.rs` 对每个已配置平台调 `register_with`（`SecretBox` 由 `ChannelKeys::get(kind)` 解出）、`Supervisor::spawn` 的返回值进 `ChannelHandles::supervisors`，并写一条「配了密钥 ⇒ `has_connections() == true`」的用例；ⓑ `wecom.rs` 的 `PendingWsTransport` 换成真传输、把 `tests/db.rs:293` 那条用例从「期望 503」改成「期望 201 + 落行」。

**转派项（不是掉棒）**：`docs/32` §29.5 的 **R5**（`mc-telemetry` 的 `SENSITIVE_KEYS` 补齐）在第 29 节里被**归给 M7-21（INT）**，但 §4.1 给本片的写集是 `docs/32` / `docs/37` / `docs/60 §11` / `docs/fixtures/route-parity-baseline.json`（**0 代码**）⇒ 这条裁定**在写集上不可执行**。本片按 §9.8.3 的执行规则 1 把它**转派成 G-6**（带具体洞清单 + 可达性取证），**不**沉默跳过。

### 11.9 复算命令（逐字）

```bash
# 0. 起手：base 当轮重取（`multica repo checkout` 会落 main 线 ⇒ 先切分支）
git fetch origin feat/multica-rs-initial
git checkout -B agent/devbox5/<suffix> origin/feat/multica-rs-initial
git rev-parse origin/feat/multica-rs-initial        # ⇒ dd0e8c51

# 1. 硬前置：owners 里没有 M7 键
python3 scripts/route_parity.py | grep -A1 'gaps by owner'   # ⇒ M9=33 M3+=16 M3=11 M10=5

# 2. ⑦ 九个数 + 不变式
python3 scripts/route_parity.py

# 3. 24 条声明路由逐条对 implemented（应缺 0）
python3 - <<'PY'
import json,subprocess
d=json.loads(subprocess.run(['python3','scripts/route_parity.py','--json'],capture_output=True,text=True).stdout)
impl={(r['method'],r['path'].rstrip('/')) for r in d['implemented']}
decl=[l.rstrip('\n').split('\t') for l in open('docs/fixtures/m7-declared-routes.tsv') if l.strip() and not l.startswith('#')][1:]
print(len(decl),'declared;',sum((m,p.rstrip('/')) not in impl for m,p in decl),'missing')
PY

# 4. --write-baseline 的幂等复核（片前 457 已在 base；片后 = local 实测）
cp docs/fixtures/route-parity-baseline.json /tmp/before.json
python3 scripts/route_parity.py --write-baseline
git diff --stat -- docs/fixtures/route-parity-baseline.json      # ⇒ 空（幂等）
python3 -c "import json;a=json.load(open('/tmp/before.json'))['routes'];b=json.load(open('docs/fixtures/route-parity-baseline.json'))['routes'];print(len(a),len(b),a==b)"

# 5. 形态门（M7 无 allowlist 退路）
python3 scripts/slash_alias_audit.py                                   # ⇒ 0 defect(s), 0 warning(s)
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m7-declared-routes.tsv   # ⇒ exit 0

# 6. ⑨（本片不动，只核对）+ M7 面 12 条的拆分（应 8 pass / 4 unevaluable）
cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json

# 7. ⑩ + 全量门禁（0 代码 ⇒ 无需 --with-db）
python3 scripts/file_size_check.py            # ⇒ scanned=1170 baseline=10 violations=0
bash scripts/gates.sh                         # ⇒ 8/8

# 8. 掉棒 D-1 的四条实证
git log --oneline -1 -- apps/mc-server/src/channels.rs        # ⇒ ab998afe（M7-0）
grep -n 'channels::start' apps/mc-server/src/main.rs          # ⇒ None 硬编码
grep -rn 'register_with' apps crates/mc-http --include=*.rs   # ⇒ 0 个生产调用点
grep -rn 'Supervisor::spawn' apps crates --include=*.rs       # ⇒ 0 个生产调用点
```

### 11.10 门禁读数（交付树）

```
① fmt 0 · ② build 0 · ③ clippy 0 · ④ clippy-test-util 0 · ⑤ test 0 · ⑦ route-parity 0 · ⑨ conformance 0 · ⑩ file-size 0   ⇒ 8/8
```

* ⑥/⑧ 需要真 PostgreSQL；本片 **0 代码 / 0 迁移** ⇒ 按 §6.5 与 `docs/57` M6-10 的先例**只跑离线 8 门**（触碰 DB 的 10/10 那条不适用）。
* 逐门耗时（交付树当场跑）：① 3s · ② 212s · ③ 88s · ④ 49s · ⑤ 59s · ⑦ 0s · ⑨ 88s · ⑩ 0s ⇒ **总 500s**；日志与逐门 `GATE_*_EXIT` 见交付 PR 的描述。

**§11 一句话**：M7 的**代码面**（24 条路由 + 48,366 行上游对应物 + 5 条进程内端到端回路）**全部落地**，⑦/⑨/⑩ 三件套**已收口**；**未落地的是两处接线**（掉棒 **D-1** = 长连接进生产宿主、**D-2** = wecom 凭据探针接线）与 6 条已登记的缺口（G-1…G-6）—— 本节把它们逐条写清，**不留"看起来完成了"的假绿**。
