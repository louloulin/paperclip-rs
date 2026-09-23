# docs/57 — M6（W6 可扩展面：skill / plugin host / MCP）切片计划

**状态**：M6 计划片（`LUM-1652`）交付物。M6 代码切片已按本文建为 `LUM-1652` 的子 issue（全部 `backlog`，见 §7），
**待 M5 收口 + 并发位空出后晋升**。
**上游口径**：`multica` @ `90e0bdf`（只读副本 `/tmp/ups_multica`），路由表取自
`docs/fixtures/upstream-routes.tsv`（上游 commit `f41fae6b08fb`）。
**本地基线**：`paperclip-rs` @ `eaba357`（`origin/feat/multica-rs-initial`；= `eed6969`（PR #60 / M5-8 合并）+ docs-only `docs/37` §48）。
本文所有 ⑦/⑨/⑩ 读数在 `eed6969` 上测得，§48 只加文档、不改读数；**切片起手时仍要重取当轮 base sha**（`docs/37` §46 的 lesson）。
**文档编号**：`56` 留给 M5-INT（`LUM-1572`），本文取 `57`（`55` 已是 M5-8）。

> 本文的口径只改三处（见 §9）：**上游 schema 是 14 张表不是 26**、
> **plugin host 面实测 17 + bridge 20 不是 plan1 的 10+10**、
> **`pkg/plugincontract` 不是 JSON-RPC**。其余承接 `docs/44-M5-PLAN.md` 的骨架与纪律。

---

## 0. 结论速览

| 项 | 结论 |
|---|---|
| 本波路由 | **57 条**（`/api/skills*` 14 + `/api/agents/{id}/skills*` 6 + `/api/workspaces/{id}/plugins*` 17 + `/v1/*` 9 + `/api/plugin-bridge/v1/*` 10 + `/plugin-surfaces/{token}` 1） |
| 本波上游体量 | 非测试手写 **≈19.3k 行**（其中代码 **≈17.0k 行**，另 **2,327 行内置 skill 资产**），≈70 文件（§1.2 逐文件） |
| 本波新迁移 | **0**。14 张 M6 表已在 `migrations/upstream/`（§1.3）；`344_plugin_v2_reset` 已把 26 → 14 收敛完 |
| 新 crate | `mc-plugin-host`（契约/凭据）、`mc-mcp`（MCP wire）、`mc-skill`（SKILL.md 规则 + 内置资产）；`pkg/publicapi/v1` 台账落 `mc-openapi/src/v1.rs`（不新建第 4 个 crate） |
| 切片数 | **9 个代码片 + 1 个 anchor + 1 个 INT = 11 个 issue**，5 个 stage，stage 内并发 ≤ **3** |
| 关键依赖 | M6-1（契约与凭据层）是 4 个 plugin 片的硬前置；M6-8（hook job）依赖 M5 遗留的**调度器接线**（§8 R-M6-6） |
| 尾斜杠双形态 | **5 键**全在 M6-2；现在 `slash_alias_audit --declared docs/fixtures/m6-declared-routes.tsv` = `FAIL: 3`（2 键被 allowlist 豁免），M6-0 删掉那 2 行后 = `FAIL: 5` |
| ⑦ 目标 | anchor 后 `local 328→324 / implemented 264→262 / known_gap 192→194 / owners.M6 55→57`；全波落地后 `implemented 319 / known_gap 137 / owners.M6 0`（`implemented+known_gap` 恒 = 456） |
| ⑨ 目标 | M6 相关 fixture **20 条**，其中 **18 条**在本波变为可判（5 skill + 7 agent + 1 context + 5 issue）；2 条（`/api/config`、`POST /api/labels`）归属它波，只登记不承诺 |
| 最大风险 | R-M6-1 插件包 JS 校验（上游用完整 AST，本地不引 JS parser）；R-M6-2 两处**同一份 DB 表被两片读写**；R-M6-6 调度器接线仍未落 |

---

## 1. 上游面测绘（全部为 `90e0bdf` 实测）

### 1.1 路由表（57 条）

上游 `router.go` 的 M6 行逐条抄进 `docs/fixtures/m6-declared-routes.tsv`（57/57 相等，复算见 §10 命令 1）。
按 router.go 行号分五簇：

| 簇 | 行号 | 条数 | 说明 |
|---|---|---|---|
| skill 读写面 | L2234–L2248 | 14 | `/api/skills/` 是 chi `Mount`，子路由 `"/"` ⇒ 两种形态 |
| agent-skill 绑定 | L2193–L2201 | 6 | `GET|PUT /api/agents/{id}/skills`、`/add`、`{skillId}` 改删、`{skillId}/enabled`、`runtime-skills/enabled` |
| workspace plugin 面 | L1690、L1724–L1747 | 17 | 安装/预览/包管理/配置/启停/卸载 + 令牌 2 + 调用记录 1 + MCP 工具 2 + surface launch 1 |
| 公开 Action API | L103–L111（**注册两次**） | 18 | 同一批 handler：`/v1/*`（`PluginBearerOnly` + `PluginRateLimit`，L1582）与 `/api/plugin-bridge/v1/*`（`middleware.Auth` 会话，L1595） |
| hook 出站回调 + surface 承载 | L1598、L1462 | 2 | `POST /api/plugin-bridge/v1/hooks/{key}`（HMAC 签名）、`GET /plugin-surfaces/{token}`（无会话、凭路径 token） |

`/api/plugin-bridge/v1/*` 共 10 条：9 条 Action + 1 条 hook（hook **不**在 `/v1` 侧注册 —— 它不是插件读 Multica 的入口，是 Multica 调插件）。

### 1.2 上游文件与行数（非测试）

| 切片 | 上游文件（行数） | 小计 |
|---|---|---|
| **M6-1 契约与凭据** | `pkg/plugincontract/{manifest.go 837, bundle.go 409, capabilities.go 103}`；`pkg/remotemcp/{client.go 406, oauth.go 383, devorigin.go 77, types.go 55}`；`pkg/publicapi/v1/{types.go 106, problem.go 111, foundation.go 79, routes.go 67, spec.go 14}`；`internal/service/plugin_token.go 245` | **2,892** |
| **M6-2 skill 读写** | `internal/handler/skill.go` L1–764 + L2479–2585 = **871**；`skill_create.go 237`；`internal/skill/{frontmatter.go 76, binary.go 40, reserved.go 27}` = 143 | **1,251** |
| **M6-3 skill 导入/刷新** | `skill.go` L765–2478 = **1,714**（import 机制独占）；`skill_import_archive.go 293`；`skill_refresh.go 202`（`pkg/skillbundle/hash.go 83` 已由 M3-7 落地，本波只复用，不计新增） | **2,209** |
| **M6-4 skill 供给面** | `skill.go` L2586–2812 = **227**；`agent_runtime_skills.go 211`；`service/builtin_skills.go 147`；`builtin_skills/_builtin_skills_legacy/**` **资产 2,327**（11 文件 markdown）；`routes/daemon/skills.rs` 源扩展 ≈120 | **≈3,032** |
| **M6-5 plugin 生命周期与包管理** | `handler/plugin.go 355`；`handler/plugin_package.go 151`；`service/plugin.go 786`；`service/plugin_package.go 524`；`service/plugin_skill.go 94`；`routes/daemon/skills.rs` plugin 源 ≈80 | **≈1,990** |
| **M6-6 插件运行时面** | `handler/workspace_mcp_api.go 530`；`handler/plugin_surface.go` 一半 ≈183；`handler/plugin_mcp.go 159`；`handler/mcp_overlay.go 160`；`handler/plugin_hook.go`（invocations 段）≈30；`service/plugin_mcp_transport.go 301` | **≈1,363** |
| **M6-7 公开 Action API + bridge** | `handler/plugin_action.go 746`；`service/plugin_action.go 171`；`service/plugin_storage.go 175`；`handler/plugin_surface.go` 另一半 ≈183；`middleware/plugin_auth.go 47`；`middleware/plugin_ratelimit.go 40` | **≈1,362** |
| **M6-8 hook 引擎 + MCP 传输** | `service/plugin_hook.go 616`；`plugin_event_dispatch.go 323`；`plugin_mcp_transport.go`（传输段）≈150；`scheduler/jobs_plugin_hook.go 353`；`plugin_agent_tools.go 152`；`plugin_schedule.go 131`；`plugin_event_bridge.go 108`；`handler/plugin_agent_hook.go 74`；`handler/plugin_hook.go`（invoke 段）≈175 | **≈2,082** |
| **M6-9 daemon 执行面** | `internal/daemon/local_skills.go 726`；`runtime_mcp.go 578`；`remote_mcp_broker.go 475`；`plugin_hook_mcp.go 246`；`skill_cache.go 192`；`slash_skill.go 35`；`execenv/{cursor_mcp.go 361, runtime_skill_policy.go 159, codex_user_skills.go 106, skill_visibility.go 99, codex_skill_strip.go 87, omp_mcp.go 33}` = 845 | **3,097** |

**不计入本波**（避免与它波撞面）：

- `internal/handler/runtime_local_skills*.go`（1,578 行）—— 其路由 `/api/runtimes/{id}/local-skills*`、`/api/daemon/runtimes/{id}/local-skills*`
  在 `scripts/route-owners.tsv` 里是 **M3**。M6-4 只消费 M3 已落地的 runtime-local-skills 子系统（`runtime-skills/enabled` 开关）。
- `internal/handler/label.go`（700 行）—— skill 标签三个 handler（L639–L700，≈62 行）算 M6-2，**其余仍属 M2**；
  本地实现落在**新文件**里，不碰 M2 的 `crates/mc-http/src/routes/agents/labels.rs`。
- `pkg/plugincontract/*_test.go`、`pkg/remotemcp/remotemcptest/fixture.go`（111）、`internal/daemon/*_test.go`。
- 生成码：无（M6 面不碰 `pkg/db/generated`）。

### 1.3 本地现状与缺口（`eed6969` / `eaba357` 实测，两者读数相同）

**路由**（`python3 scripts/route_parity.py --json`）：

```
upstream 456 · local 328 · implemented 264（真 262 + 占位 2）· known_gap 192 · unclaimed 0
regressions 0 · local_only 11（含占位 3）· owners.M6 = 55
```

- M6 的 57 键里，本地**真实现 0 条**：只有 `GET|POST /api/skills` 与 `GET|POST /api/plugins` 四条 **M0 占位**
  （`crates/mc-http/src/routes/mount.rs:48-56`，`health::placeholder` = 200 空响应）。
  其中 `/api/skills` 两条因尾斜杠折算被算作 `implemented_placeholder`，`/api/plugins` 两条根本不存在的路径是 `local_only_placeholder`。
- 形态门：`slash_alias_audit.py` 无参跑 **0 缺陷**，2 行 allowlist（`GET|POST /api/skills`，owner M6）是 M0 占位期的欠账。

**代码**：

- `crates/mc-core/src/{skill.rs 43, plugin.rs 68}` 是 M0 的字段占位，须按 M5-0 的规矩**重写**（不是扩充）。
- `crates/mc-repos` 无 skill / plugin 任何模块（`lib.rs` 107 行里没有）。
- `mc-daemon` 无 skill / mcp 模块：`execenv/{path,mod,lib,client}.rs` 只有注释提到 skill 落盘未接。
- `mc-http` 已有 `routes/daemon/skills.rs`（253 行，M3-7）—— `pkg/skillbundle` 的 hash 口径**已逐字移植**，
  且注释明确「本仓未建 builtin / plugin skill 子系统，只产出 workspace 源」。**M6 正是来补这两个源**。
- `crates/mc-plugin-protocol`（343 行）**零依赖者**，建模的是 stdio JSON-RPC（`initialize`/`runJob`/`performAction`…），
  上游**没有**这个协议（§9.3）⇒ M6-0 删除该 crate。

**数据库**：14 张 M6 表**全部已在** `migrations/upstream/`，本波 **0 新迁移**：

| 表 | 定义文件 | 归属片 |
|---|---|---|
| `skill`、`skill_file`、`agent_skill` | `008_structured_skills.up.sql` | M6-2 / M6-3 / M6-4 |
| `skill_to_label` | `162_resource_labels.up.sql` | M6-2 |
| `agent_mcp_server`、`workspace_mcp_server` | `315_workspace_mcp_server.up.sql` | M6-6 |
| `plugin_installation` | `285_plugin_lifecycle_v1.up.sql` → **重建**于 `344_plugin_v2_reset.up.sql` | M6-5 |
| `plugin_secret` | `344_plugin_v2_reset.up.sql` | M6-5 |
| `plugin_storage` | `344_plugin_v2_reset.up.sql` | M6-7 |
| `plugin_package`、`plugin_package_version`、`plugin_package_file` | `392_plugin_package_publishing.up.sql` | M6-5 |
| `plugin_invocation` | `362_plugin_hook_engine.up.sql` | M6-6 / M6-8 |
| `plugin_hook_schedule` | `399_plugin_hook_schedule.up.sql` | M6-8 |

复算见 §10 命令 6。**口径修订**：`docs/plan1.md` 说「W6 相关 26 张表」是**旧的**。`plugin_*` 在 `migrations/upstream/` 里有 **22 个**
`CREATE TABLE`，其中 **14 个**被 `344_plugin_v2_reset.up.sql` DROP（同文件重建了 `plugin_installation` 一张）
⇒ 存活 **8 张** plugin 表 + skill/MCP 侧 **6 张** = **14**（`contracts/upstream-schema.json` 里 0 命中
`plugin_identity`、`plugin_release`、`plugin_grant` 等被 DROP 的表名，可作旁证）。

### 1.4 尾斜杠双形态：本波实测

```
$ python3 scripts/slash_alias_audit.py --declared docs/fixtures/m6-declared-routes.tsv   # 节选
FAIL: 3 trailing-slash shape defect(s)
  declared 57 upstream key(s); dual-form required: 5 | single-form: 52
  MISSING_ALIAS (5): /api/skills/ ×2（[allowlisted: M6]）、/api/skills/{id}/ ×3
=> 3 defect(s) from findings, 0 warning(s); 2 allowlisted
```

- **双形态清单（5 键，全部 M6-2）**：`GET|POST /api/skills`、`GET|PUT|DELETE /api/skills/{id}` ——
  上游 `r.Route("/api/skills", …)` 是 Mount，子路由写 `"/"`，两种形态都服务；axum 0.7 对未注册的那一种是 **404（不是 307）**。
- 现在报 `FAIL: 3` 是因为 2 键还在 allowlist 里被豁免；**M6-0 删占位时必须同一 PR 删掉那 2 行**
  （残留行 = `STALE` 缺陷，规则见 `docs/37` §15.3 与 allowlist 头部注释），此后预测值变 `FAIL: 5`。
- 其余 52 键上游是 plain 注册（无尾斜杠形态），只需一种形态。
- `/v1/openapi`… 无关；`/plugin-surfaces/{token}` 是路径 token，不是参数段。

---

## 2. 目标架构与落点（含取舍）

### 2.1 分层落点

```
mc-openapi/src/v1.rs        ← /v1 契约台账（Operations/scope/凭据/限流档），M6-1
mc-plugin-host/             ← 插件契约与凭据（纯逻辑，无 DB/无 HTTP），M6-1
mc-mcp/                     ← MCP wire（JSON-RPC client / oauth / devorigin / pinned tools），M6-1
mc-skill/                   ← SKILL.md 规则（frontmatter/binary/reserved）+ 归档 + 内置资产，M6-2 / M6-3 / M6-4
mc-core/{skill.rs,plugin.rs}← 领域类型（由 M6-0 重写，各片只读）
mc-repos/src/skill/*        ← skill/skill_file/skill_to_label/agent_skill 仓储（按面拆文件）
mc-repos/src/plugin/*       ← plugin_installation/package/storage/invocation/token 仓储（按面拆文件）
mc-http/src/routes/skills/* 、plugins/* 、plugin_bridge/* 、v1/* 、surfaces/*  ← HTTP 面
mc-daemon/src/{skill/*,mcp/*}、execenv/*  ← daemon 执行面，M6-9
```

### 2.2 为什么是这三个 crate（判据 + 被否决的备选）

| 决定 | 判据 | 被否决的备选 |
|---|---|---|
| `mc-plugin-host` 独立成 crate | `pkg/plugincontract` 是**纯校验器**（manifest/capabilities/bundle，1,349 行），被 handler 5 / service 10 / scheduler 1 共 16 处引用；本地 4 个切片都要用它，放 `mc-http` 会让 4 片共写一个 crate 的公开面 | 塞进 `mc-http/src/plugin/`（跨 crate 依赖倒置：`mc-scheduler` 也要校验 hook 声明） |
| `mc-mcp` 独立成 crate | `pkg/remotemcp` 被 **handler 与 daemon 两侧**共用（M6-6/7 服务端 + M6-9 daemon broker）。daemon 不该依赖 HTTP 层 | 放 `mc-daemon`（服务端要反过来依赖 daemon） |
| `mc-skill` 独立成 crate | SKILL.md 规则被读面、导入面、供给面、daemon 缓存校验四处复用；且要承载 2,327 行 `include_str!` 资产，塞 `mc-core` 会让 `mc-core` 变成资产仓库 | 全部放 `mc-core`（`mc-core` 是零依赖领域层，加 `zip`/`serde_yaml` 会污染全仓） |
| `/v1` 台账落 `mc-openapi/src/v1.rs` | 台账是「路径 + 策略」的静态表（377 行），与 `mc-openapi::OpenApiSpec` 同性质；新建第 4 个 crate 的收益不抵清单成本 | 新建 `mc-public-api`（1 个文件 377 行，不值一个 crate） |
| **删除 `mc-plugin-protocol`** | 零依赖者 + 建模的 stdio JSON-RPC 上游不存在（§9.3）。留着等于给未来的切片一个「照它实现」的陷阱 | 改造为契约层（内容 100% 要重写；改名/改语义的迁移成本 > 删除成本） |
| `tower-governor` 进 `mc-http` | `/v1` 要 `PluginRateLimit`（上游 `RateLimitPluginStrict`），本地**全仓无任何限流实现**（实测 `grep tower_governor` 仅命中 task 重试语义） | 自写令牌桶（要自己处理时钟/分片/测试，收益低）；不加限流（违反 §2.4 边界契约） |

### 2.3 五个信任面（**不是一种**，切片必须分清）

| 信任面 | 路由 | 凭据 | 中间件 |
|---|---|---|---|
| workspace 会话面 | `/api/skills*`、`/api/agents/{id}/skills*`、`/api/workspaces/{id}/plugins*` | 用户会话（既有 `middleware/authn`） | 既有 workspace 成员/角色判定 |
| 插件 Bearer 面 | `/v1/*`（9） | **安装令牌 `mpi_…`**（长期、可轮换、DB 里只存 keyed hash、只验不发）**或 回调令牌 `mpc_…`**（按**一次 invocation** 签发、5 分钟 TTL、到期前可重复调用、**进程内存持有**） | **新** `PluginBearerOnly` + `PluginRateLimit`（M6-7）+ manifest scope 判定（M6-1） |
| 会话中继面 | `/api/plugin-bridge/v1/*`（10） | **用户会话**（iframe 里的同源浏览器请求） | 既有 `middleware.Auth`；**不认**插件令牌 |
| 路径 token 面 | `/plugin-surfaces/{token}` | **加密 claims**（AES-GCM，域分离自部署密钥；**2 分钟** TTL；无 DB 状态、无会话） | 无会话：token 即凭据 + Host 边界校验（M6-7） |
| 出站回调面 | `POST /api/plugin-bridge/v1/hooks/{key}` | hook key + **HMAC 签名**（`plugin_secret`） | 无会话：签名校验（M6-8） |

### 2.4 边界契约（写进各片 DoD）

1. **上游一个 Go 文件 → 本地按面拆多个文件**：写集以「本地文件」为单位，一格一个写者；同一 stage 内不允许两片写同一个本地文件。
2. **`/v1` 与 `/api/plugin-bridge/v1` 必须同片**：上游 `registerPluginActionRoutes` 被调用两次（router.go:1582/1595），
   同批 handler 两个挂载点。拆到两片会让「同一 handler 两套中间件」的差异无处安放。
3. **插件的三条凭据链互不替代**：安装令牌 ≠ 调用令牌 ≠ surface 令牌 ≠ hook 签名。任一处的校验函数被另一处复用要显式声明。
4. **scope 判定只有一个实现点**（M6-1，`mc-plugin-host`）：`issues:read`/`issues:write`/`comments:read`/`comments:write`
   + `ContractPluginExtension` 的无 scope 路径。`/v1` 与 bridge 两侧共用，禁止各写一份。
5. **`/api/plugin-bridge/*` 不做 scope 判定**（会话面，权限来自 workspace 成员身份），但要与 `/v1` 共享**资源 DTO 与错误体**
   （`publicapiv1.WriteProblem` ≡ 本地 `ProblemDetail`）。
6. **不新写 issue/comment 服务**：`/v1/issues*` 复用 M2 已落地的 issue/comment 服务与权限判定，只加 scope 门。
7. **禁止 `health::placeholder` 进任何新路由**：M0 占位只减不增（门 ⑦ 的 `implemented_placeholder` 必须保持 0）。
8. **`routes/daemon/skills.rs` 只有一个写者（M6-4）**：workspace/builtin/plugin 三个源全在该文件内实现；
   M6-5 只负责**写** `source='plugin'` 的 skill 行，不碰该文件。
9. **部署密钥只有一个来源**：`MULTICA_PLUGIN_SECRET_KEY`（raw 32 字节，`secretbox.LoadKey` 读）—— 一处直用 +
   三处 HMAC 域分离派生（§2.6）。**禁止**任何切片自造第二把密钥、把某个派生写成常量、或把安装令牌的哈希改成 keyed。
10. **回调令牌（`mpc_`）不落库、不单次消费**：上游 `CallbackTokens` 是进程内 map（`sync.Mutex` + 到期清扫），
   语义是「按 invocation 签发、5 分钟内可重复调用」——单次消费会让「先读 issue 再发注释」的 handler 在第二次调用就 403
   （上游注释记录了这次实测）。多实例/重启后**提前失效**（403，可重试）是**设计**而非缺陷。

### 2.5 依赖与算法取舍（anchor 定的，切片不许私自换）

| 问题 | 决定 | 理由 / 残余风险 |
|---|---|---|
| SKILL.md frontmatter 解析 | `serde_yaml = "0.9"` | 上游 `gopkg.in/yaml.v3` 解成 `map[string]any` 再逐键强制转换（scalar 用字面量、序列/映射用 JSON），语义必须对齐 `packages/core/skills/frontmatter.ts`。`serde_yaml` 已停止维护但稳定且无重依赖；若 workspace 拒绝，退 `serde_yml`（fork），**换包要在 anchor 一次定死**。 |
| skill 归档导入 | `zip = "2"` + `axum` 的 `multipart` feature | 上游 `archive/zip`；`POST /api/skills/import` 既收 JSON 又收 multipart（`isMultipartUpload` 按 Content-Type 分支）。当前 `axum` features = `["macros","ws"]`，**没有 multipart** ⇒ anchor 加。 |
| 插件包 JS 校验 | 自写**窄口径词法扫描器**（字符串 / 模板串 / 正则字面量 / 注释）+ 登记偏离 | 上游用 `tdewolff/parse/v2` 走 AST，检测 surface 入口有无 top-level `import`/`export`/`await`/`import.meta`。为一条校验引 `oxc_parser`/`swc` 不成比例；上游注释本身记录了「朴素正则会被 `import ("x")`、注释、单行两条语句」骗过。**残余风险**：误判会放过一个运行时才失败的 surface（影响面是浏览器里 iframe 报错，不是越权），见 §8 R-M6-1。 |
| 限流 | `tower-governor`（`mc-http`） | plan1 §4 已点名；本地零实现。 |
| 内置 skill 资产 | `include_str!` / 目录常量表（`mc-skill/assets/builtin_skills/**`） | 不引 `include_dir!`（多一个 proc-macro 依赖）；资产是 `.md`，**不被门 ⑩ 扫描**（只扫 `.rs/.py/.sh/.yml`）。 |
| MCP 传输 | `mc-mcp` 出 JSON-RPC 帧，服务端/daemon 各自持有 client | 上游两侧共用 `pkg/remotemcp`；本地同样共用 crate，但**不共用连接**（服务端调用 vs daemon 本地调用）。 |
| 部署密钥装配 | 复用 `mc-secrets`（`cipher.rs` 已是 AES-GCM、`ensure_root_key` 是既有 root-key 模式），env 名与上游一致：`MULTICA_PLUGIN_SECRET_KEY` | anchor 决定装配点（`apps/mc-server` 读 env ⇒ 进 `McState`），M6-1 实现四把派生。**本地当前没有任何 plugin 密钥读取路径** ⇒ 不装配则整个插件面只能测「降级分支」 |
| 派生与凭据实现 | 全在 `mc-plugin-host`（§2.6），一函数一用途、域分离标签逐字照抄 | 上游把四处散在 `plugin.go` / `plugin_hook.go` / `plugin_surface.go` 三个文件里，本地合并成一个模块，避免 M6-5/6/7/8 各写一份 |

### 2.6 部署密钥与凭据派生（**一片实现，四片消费**）

上游把插件面的凭据散在三个文件里，但它们**只认同一把部署密钥**：`MULTICA_PLUGIN_SECRET_KEY`
（`secretbox.LoadKey` 读；上游注释明确「与 VCS / 渠道密钥分开，为的是独立轮换与独立爆炸半径」）。本地实测**没有任何
plugin 密钥读取路径**，所以这是 anchor 必须一次定死的装配点。

| # | 用途 | 上游做法 | 本地落点 | 未配置时 |
|---|---|---|---|---|
| 1 | `plugin_secret` 配置值的**封装** | 直接用部署密钥做 AES-GCM（`secretbox.New(pluginKey)`） | `mc-secrets::cipher` | 写 `secret` 字段**fail closed**（绝不落明文） |
| 2 | hook **签名密钥**（按 installation 派生） | `HMAC-SHA256(部署密钥, "multica-plugin-hook-signature:v1:" ‖ installation_id)` ⇒ 32 字节；对外形态 `whsec_` + hex | `mc-plugin-host::credentials`（M6-1） | hooks 报 `hooks are disabled`（**不是** panic） |
| 3 | surface launch token 的 **box** | `HMAC-SHA256(部署密钥, "multica/plugin-surface-launch/v1")` ⇒ AES-GCM key | 同上（M6-1），消费在 M6-7 | surfaces 关闭（`plugin_surfaces_not_configured`） |
| 4 | 安装令牌的**哈希** | `sha256(token)` —— **不加 key**（不是派生！） | `mc-plugin-host::credentials`（M6-1） | **仍可用**（Public API 不受部署密钥影响） |

> **方向不对称（照抄，别自作聪明）**：安装令牌由**插件**产生、宿主**只验不发** ⇒ 存无 key 哈希；
> hook 签名密钥由**宿主**产生、必须能**按需复现** ⇒ 派生而非落库。**落库一份可复原的签名密钥 = 任何一次 DB 读取都能拿到它。**

**hook 出站 wire 契约（逐字对齐，外部插件服务器按它自行校验）**：

```
POST <hook URL>            Content-Type: application/json
X-Multica-Timestamp: <unix 秒>          X-Multica-Signature: v1=<hex>
X-Multica-Plugin-Installation: <uuid>   User-Agent: Multica-Hooks/1
签名 = HMAC-SHA256(派生密钥, timestamp ‖ "." ‖ body)     容差 = ±5 分钟
校验 = 常量时间比较（上游注释：逐字节比较会泄露「猜对了多少」）
```

**哪一条是跨实现契约、哪一条只需自洽（别搞反）**：

- **hook 签名 = 跨实现契约**：`whsec_…` 会交给外部插件服务器，签名头与算法必须**逐字**（上面那一段就是规格）。
- **`plugin_secret` 封装 + surface token = 进程内自洽**：两者都不与外系统交换字节串 ⇒ 本地用 `mc-secrets::cipher`
  （实测格式 `base64(nonce) + base64(ct+tag)`，上游 `secretbox.Seal` 是裸拼接 `nonce‖ct‖tag`，**不同也不需要相同**）。
  只要求**域分离标签照抄**（`multica/plugin-surface-launch/v1`、`multica-plugin-hook-signature:v1:`）—— 便于对照审计。

**本地装配（anchor 定死）**：`apps/mc-server` 读 `MULTICA_PLUGIN_SECRET_KEY`（名字与上游一致）⇒ 进 `McState`；
四处的**实现**都在 `mc-plugin-host`（一处实现、四片消费），切片**不得**各自再写一份派生或自造第二把密钥。

---

## 3. 写集与并发

### 3.1 共享锚点（只在 M6-0 动一次，其余片只读）

| 共享件 | 动作 |
|---|---|
| `Cargo.toml`（根 `[workspace.dependencies]`） | 加 `tower-governor`、`zip`；`axum` 加 `multipart` feature |
| `Cargo.lock` | 同提交刷新（**只有 anchor 能改**） |
| `crates/mc-http/Cargo.toml` | 加 `tower-governor`/`zip`/`mc-skill`/`mc-plugin-host`/`mc-mcp` 依赖边 |
| `crates/mc-{skill,plugin-host,mcp}/Cargo.toml` + `src/lib.rs` | **建骨架**（空模块 + 公开面 `pub use` 位） |
| `crates/mc-core/src/{skill.rs,plugin.rs}` | **重写**领域类型（各片只读） |
| `crates/mc-http/src/routes/{mount.rs,mod.rs}` | 删 4 条 M0 占位；加 `mount_slice_{skill,plugin,plugin_bridge,plugin_surface,v1}` 空 router 挂点 |
| `crates/mc-http/src/state.rs` | 加 `plugin_key: Option<…>`（**在 `AppState::new` 内部读 env**，照 `GoogleOAuthConfig::from_env()` 的先例）+ 唯一的 `pub fn plugin_key(&self)` 出口 |
| `crates/mc-http/src/routes/auth.rs` | `#[cfg(test)]` 里的 `AppState { … }` 字面量补一个字段（**唯一**的字面量构造点） |
| `crates/mc-http/src/routes/skills/{mod.rs,helpers.rs}` | 建文件（skill 会话解析 / `load_skill_for_user` / DTO 转换的**共用件**） |
| `crates/mc-repos/src/{lib.rs,skill/mod.rs,plugin/mod.rs}` | 建模块树与 `pub use` |
| `crates/mc-plugin-protocol/` | **删除**（零依赖者） |
| `docs/fixtures/route-parity-baseline.json` | `--write-baseline`（300 → 296） |
| `docs/fixtures/slash-alias-allowlist.tsv` | 删 M6 的 2 行 |
| `docs/32` 偏离表 / `docs/37` 口径表 | 按本文 §9 追加修订条 |

### 3.2 写集矩阵（一格 = 一个本地文件 = 一个写者）

| 文件 | M6-1 | M6-2 | M6-3 | M6-4 | M6-5 | M6-6 | M6-7 | M6-8 | M6-9 |
|---|---|---|---|---|---|---|---|---|---|
| `mc-plugin-host/src/*` | **写** | | | | 读 | 读 | 读 | 读 | |
| `mc-mcp/src/*` | **写** | | | | | 读 | 读 | 读 | 读 |
| `mc-openapi/src/v1.rs` | **写** | | | | | | 读 | | |
| `mc-skill/src/{frontmatter,binary,reserved}.rs` | | **写** | 读 | 读 | | | | | |
| `mc-skill/src/{archive,git}.rs` | | | **写** | | | | | | |
| `mc-skill/src/builtin.rs` + `assets/**` | | | | **写** | | | | | |
| `mc-http/src/routes/skills/{crud,files,labels}.rs` | | **写** | | | | | | | |
| `mc-http/src/routes/skills/{import,refresh}.rs` | | | **写** | | | | | | |
| `mc-http/src/routes/agents/skills.rs` | | | | **写** | | | | | |
| `mc-repos/src/skill/{read,write}.rs` | | **写** | 读 | 读 | | | | | |
| `mc-repos/src/skill/import.rs` | | | **写** | | | | | | |
| `mc-repos/src/skill/binding.rs` | | | | **写** | 读 | | | | |
| `mc-http/src/routes/plugins/{install,packages}.rs` | | | | | **写** | | | | |
| `mc-repos/src/plugin/{installation,package,skill}.rs` | | | | | **写** | | | | |
| `mc-http/src/routes/plugins/{mcp,surface_launch}.rs` | | | | | | **写** | | | |
| `mc-repos/src/plugin/{mcp_approval,invocation_read}.rs` | | | | | | **写** | | | |
| `mc-http/src/routes/plugin_bridge/*.rs` | | | | | | | **写** | hook 段**写** | |
| `mc-http/src/routes/v1/*.rs`、`routes/surfaces.rs` | | | | | | | **写** | | |
| `mc-repos/src/plugin/storage.rs` | | | | | | | **写** | | |
| `mc-http/src/routes/plugins/hooks_job.rs` | | | | | | | | **写** | |
| `mc-repos/src/{plugin/hook.rs,scheduler.rs}` | | | | | | | | **写** | |
| `mc-daemon/src/skill/*`、`mcp/*`、`execenv/*` | | | | | | | | | **写** |

> 同 stage 内两片**可以**读写同一张 DB 表，但必须走各自的本地文件（矩阵里「写 / 读」列区分的就是这个）。

---

## 4. 切片表（派发用）

### 4.1 全景

| # | 切片 | 路由 | 上游体量 | stage | 硬前置 |
|---|---|---|---|---|---|
| M6-0 | anchor（占位清理 + 骨架 + 契约类型） | 0 | — | 1 | M5 全合 |
| M6-1 | 契约与凭据层 | 0 | 2,892 | 2 | M6-0 |
| M6-2 | skill 读写面 | 12 | 1,251 | 2 | M6-0 |
| M6-3 | skill 导入/刷新 | 2 | 2,209 | 2 | M6-0 |
| M6-4 | skill 供给面（agent 绑定 + builtin/plugin 源） | 6 | ≈3,032 | 3 | M6-0/2 |
| M6-5 | plugin 生命周期与包管理 | 13 | ≈1,990 | 3 | M6-0/1 |
| M6-6 | 插件运行时面（调用记录 / MCP 采纳 / surface 发放） | 4 | ≈1,363 | 3 | M6-0/1 |
| M6-7 | 公开 Action API + bridge + surface 承载 | 19 | ≈1,362 | 4 | M6-0/1 |
| M6-8 | hook 引擎 + MCP 传输 + hook job | 1 | ≈2,082 | 4 | M6-1/5/6 + `LUM-1659`（M5-9 接线） |
| M6-9 | daemon 侧 skill/MCP 执行面 | 0 | 3,097 | 4 | M6-1（hook MCP 段另需 `LUM-1659`） |
| M6-10 | INT（集成与快照刷新） | 0 | — | 5 | 全波 |

路由账：12 + 2 + 6 + 13 + 4 + 19 + 1 = **57** ✓（M6-1 / M6-9 是 0 路由片）。

### 4.2 逐片要点（评审用，不替代切片自己的 DoD）

**M6-0 anchor** —— 见 §5 逐文件清单。DoD 额外含：⑦ `local 324 / implemented 262 / known_gap 194 / owners.M6 57`、
allowlist 只剩 0 行 M6 相关、baseline 296、`mc-plugin-protocol` 目录已删且 `cargo metadata` 不报未用成员。

**M6-1 契约与凭据层**（0 路由）
- `pkg/plugincontract` → `mc-plugin-host`：`ManifestVersion1`、`Capabilities`、`Hook`、`NetDomains`、`ConfigSecret`、`Resource`、
  `TriggerSchedule`、`MaxBundleSize`；bundle 校验（zip 条目白名单、单文件 surface、大小上限）；**scope 判定唯一实现点**。
- `pkg/remotemcp` → `mc-mcp`：`initialize` → `notifications/initialized` → `tools/list` → `tools/call` 的 JSON-RPC 客户端、
  OAuth（`oauth.go`）、dev origin 白名单、`validatePinnedRemoteMCPTools`。
- `pkg/publicapi/v1` → `mc-openapi/src/v1.rs`：9 条 Operation + 4 种凭据 + 2 档限流 + `ProblemDetail`。
- `service/plugin_token.go` → 凭据编解码（M6-5/6/7 三处消费者，因此放本片而不是某一片）。
- DoD：`plugincontract` 的 409 行 bundle 规则有单测（含「注释夹在 `import (` 中间」「一行两条语句」两个反例）；
  scope 判定矩阵表驱动；**0 路由**（⑦ 读数不变）。

**M6-2 skill 读写面**（12 路由）
- `skill.go` L1–765（去掉 import 专属 helper）+ L2479–2585 + `skill_create.go` + `internal/skill/*`。
- **5 个双形态键全在本片**：`/api/skills` 与 `/api/skills/{id}` 两种形态一起注册，**并删 allowlist 的 2 行**
  （M6-0 已删；本片只需保证不再需要加回）。
- 标签三键复用 M2 的资源标签仓储（`resource_type='skill'`），实现落新文件。
- DoD：5 条 `contracts/golden/skills/*` 从 `unevaluable` 变可判；`GET /api/skills` 默认**不含 content**（`include` 参数语义）。
- ⚠️ 该片 `skill` 表的 `content_hash` 计算与 M6-3/4 的 bundle hash 必须同源（`mc-skill`）。

**M6-3 skill 导入/刷新**（2 路由）
- `skill.go` L765–2478 是**导入生态**：GitHub / ClawHub / skills.sh 三类来源的抓取 + `on_conflict`（fail/overwrite/rename/skip）
  + `skill_import_archive.go`（multipart zip）+ `skill_refresh.go`。
- 上游 `doGitHubAPIGet` 与 W8 的 GitHub 客户端面重叠 ⇒ 本片**定义一个 `SkillSourceFetcher` port**（trait），
  默认实现走 `reqwest`；**不建**第二个 GitHub 抽象层，W8 落地后由其实现该 port（§8 R-M6-3）。
- DoD：`POST /api/skills/import` 两种 Content-Type 都测；`on_conflict=skip` 返回 200 + skipped 计数（不是 409）。

**M6-4 skill 供给面**（6 路由）
- agent 绑定 5 键 + `runtime-skills/enabled` 1 键；`routes/daemon/skills.rs` 补 **builtin / plugin 两个源**（M3-7 只做了 workspace）。
- 内置资产 2,327 行落在 `mc-skill/assets/builtin_skills/**`（含 `multica-platform` 与 `multica-onboarding`，及 legacy redirect stub）。
- `PUT /plugin-source` 场景的 pinned hash 校验（`skill_hash != ref.hash` ⇒ 409）。
- DoD：7 条 `contracts/golden/agents/*` 里与本片相关的 5 条离开 `unevaluable`；
  `GET /api/agents/{id}` 响应**包含** `skills` 字段 ⇒ 要动 `routes/agents/dto.rs`（694 行，见 §6.3 预飞）。

**M6-5 plugin 生命周期与包管理**（13 路由）
- 安装/预览/配置/启停/卸载 + 包四键 + 令牌 2 键；`service/plugin.go` 786 行是最大单文件，本地按 安装 / 配置 / 生命周期 三段拆。
- 安装时**物化** `source='plugin'` 的 skill 行（`service/plugin_skill.go`）——只写行，不碰 resolve 文件。
- 令牌 2 键：rotate（返回明文一次）/ revoke（幂等 204）。
- `preview`（`POST /plugins/preview`）是**无副作用**的 manifest 校验入口，必须与 install 共用 M6-1 的校验器。
- DoD：包发布 `POST /packages` 的 JS 校验（§2.5）有正反例；`plugin_installation` 的 15 列全有写入路径或显式登记未用。

**M6-6 插件运行时面**（4 路由）
- 调用记录读取（`plugin_invocation`，按 installationId 分页）、MCP 工具采纳（读 manifest 声明 → `tools/list` → 落 `mcp_approvals` jsonb）、
  surface launch 令牌签发。
- `workspace_mcp_api.go` 530 行含 workspace 级 MCP server 台账（`workspace_mcp_server` 表），与 `agent_mcp_server`（M8 面）**表相同、路由不同** ⇒ 只做 workspace 侧。
- DoD：MCP 采纳写入的 schema digest 变化会失效旧批准（与上游 `PluginMCPApproval` 同语义）。

**M6-7 公开 Action API + bridge + surface 承载**（19 路由）
- `/v1/*` 9 条 + `/api/plugin-bridge/v1/*` 9 条 Action + `/plugin-surfaces/{token}` 1 条，**同一批 handler 两个挂载点**。
- 新中间件：`PluginBearerOnly`（认 `mpi_`/`mpc_`，拒绝用户会话）+ `PluginRateLimit`（`tower-governor`，`RateLimitPluginStrict`）。
- `plugin_storage` KV（scope + key，含大小上限与 scope 前缀隔离）。
- 特征开关：`plugins_v1` 关闭时 `/v1/context` 返回 403 `plugin_api_disabled`（`contracts/golden/context/001` 就是它）。
- DoD：5 条 `issues/09x` plugin 令牌 fixture + `context/001` 离开 `unevaluable`；
  `/v1` 与 bridge 两侧**同 DTO**（同一 handler，两侧响应字节相同）。

**M6-8 hook 引擎 + MCP 传输 + hook job**（1 路由）
- hook 入站（HMAC 签名校验 + 重放窗口）、事件分发/桥接、hook 调度落 `plugin_hook_schedule`、
  scheduler job（`internal/scheduler/jobs_plugin_hook.go` 353 行）+ `plugin_agent_tools`。
- ⚠️ **依赖 M5 遗留的接线片 `LUM-1659`（M5-9）**：`apps/mc-server` 至今没有 `mc-scheduler` 依赖边、`main.rs` 一行未动
  （M5-8 按降级方案交付；M5-INT 的写集只有 docs + baseline + 报告）⇒ 本片的 hook job 在 M5-9 落地前**只有桩级证据**。
  前置 = `LUM-1659` 合入（它自己挂在 owner 的 P0 裁决上，见 `docs/37` §48.3）；本片**不接手**该接线，只登记依赖（§8 R-M6-6）。
- DoD：hook 签名正/反例（错签名 401、过期 401、未知 key 404）；job 幂等（同一 schedule 桶只派发一次）。

**M6-9 daemon 侧 skill/MCP 执行面**（0 路由）
- `local_skills.go`（本地 skill 扫描/导入/落盘）、`skill_cache.go`（bundle 缓存校验）、`runtime_mcp.go`（运行时 MCP 装配）、
  `remote_mcp_broker.go`（broker + pinned tools 校验）、`plugin_hook_mcp.go`、`execenv/*`（各 runtime 的 skill/MCP 注入口）。
- 与 M3 已落的 `execenv/{guard,lock,path,temp}` **同目录加新文件**，不改既有文件。
- DoD：`mc-daemon` 侧 `cargo test -p mc-daemon` 全绿；bundle 缓存校验用 M3-7 已移植的同款 hash（禁止另起一套）。

**M6-10 INT** —— 快照刷新（⑦/⑨/`route-parity-baseline`/`slash-alias-allowlist` 最终态）+ `docs/58-M6-INTEGRATION.md`
+ 跨片缺口登记。**不写代码**；`docs/56` 是 M5-INT 的编号，M6 集成报告另取 58。

### 4.3 波次（并发 ≤3）

```
stage 1 : M6-0
stage 2 : M6-1 ∥ M6-2 ∥ M6-3          （契约层与两个 skill 面互不依赖）
stage 3 : M6-4 ∥ M6-5 ∥ M6-6          （供给面 / 生命周期 / 运行时面）
stage 4 : M6-7 ∥ M6-8 ∥ M6-9          （公开面、hook+job、daemon 面）
stage 5 : M6-10
```

---

## 5. M6-0 anchor：逐文件预扩展清单

| 文件 | 动作 | 关键点 |
|---|---|---|
| `crates/mc-http/src/routes/mount.rs` | 删 2 个 `.route(...)` 块（`/api/skills` 与 `/api/plugins`，各 get+post = **4 个键**）+ 加 5 个 `mount_slice_*` merge | 4 个键从 baseline 消失 ⇒ 必须同 PR `--write-baseline`（300 → 296） |
| `crates/mc-http/src/routes/mod.rs` | 声明 `skills`、`plugins`、`plugin_bridge`、`v1`、`surfaces` | 空 router 只建文件，不注册路径 |
| `crates/mc-core/src/skill.rs` | **重写**（43 → 目标 ≈150） | `Skill`/`SkillFile`/`SkillSource{Workspace,Builtin,Plugin}`/`SkillRef` |
| `crates/mc-core/src/plugin.rs` | **重写**（68 → 目标 ≈200） | `PluginInstallation`/`PluginStatus`/`PluginScope`/`PluginSurface`/`PluginHook`/`McpApproval`/`PluginTokenKind` |
| `crates/mc-{skill,plugin-host,mcp}/{Cargo.toml,src/lib.rs}` | 新建骨架 | 只放 `pub mod` + 空的公开面；`#![deny(missing_docs)]` 视仓规 |
| `crates/mc-openapi/src/v1.rs` | 建文件（空 `Operations` 常量） | 供 M6-1 填充 |
| `crates/mc-repos/src/{lib.rs,skill/mod.rs,plugin/mod.rs}` | 模块树 + `pub use` | 各面文件由各片新建 |
| `crates/mc-http/src/routes/skills/{mod.rs,helpers.rs}` | 建共用件 | 会话/工作区解析、`load_skill_for_user`、响应转换 |
| `crates/mc-http/src/state.rs` | 加 `plugin_key` 字段 + 出口 | **不新增 `AppState::new` 参数**：21 个调用点全不动，env 在构造体内读（`GoogleOAuthConfig::from_env()` 先例）；**不碰 `apps/mc-server`** ⇒ 与 `LUM-1659`（M5-9）**零写集交集** |
| `crates/mc-http/src/routes/auth.rs` | 测试里 `AppState { … }` 字面量补一字段 | 全仓唯一的字面量构造点（L1022 起） |
| `Cargo.toml`（根）+ `Cargo.lock` | 加依赖 / 刷新锁 | `tower-governor`、`zip`、`serde_yaml`、`axum` 的 `multipart`。⚠️ `LUM-1659` 也会动 `Cargo.lock`（加一条 `mc-scheduler` 边）⇒ 两片不同时在飞；若同飞，**只重新生成、不手工合并** |
| `crates/mc-http/Cargo.toml` | 依赖边 | 三条 `path` 边 + 上面两包 |
| `crates/mc-plugin-protocol/` | **删除** | 零依赖者，`cargo metadata` 后目录消失 |
| `docs/fixtures/route-parity-baseline.json` | `--write-baseline` | 300 → 296 |
| `docs/fixtures/slash-alias-allowlist.tsv` | 删 2 行 | 残留 = `STALE` 缺陷 |
| `docs/32`（偏离表）/ `docs/37`（口径表） | 追加口径修订 | 承接 §9（三处修订各一条） |

> 纪律（与 M5-0 相同）：anchor **不实现任何路由逻辑**，也不预写 M6-1 的契约实现 —— 只固定类型、挂点、依赖边与账本。
> anchor 的测试只有两类：编译（`cargo check --workspace`）与门 ⑦/⑩ 读数。

---

## 6. 门禁与验收

### 6.1 门 ⑦（路由对齐）——预测值与本波目标

| 时点 | local | implemented | known_gap | owners.M6 | 备注 |
|---|---|---|---|---|---|
| `eaba357`（现状） | 328 | 264（含占位 2） | 192 | 55 | local_only 11（占位 3） |
| M6-0 后 | **324** | **262** | **194** | **57** | 4 个占位键删除；baseline 296；local_only 9（占位 1） |
| M6-2 后 | 336 | 274 | 182 | 45 | +12 |
| M6-3 后 | 338 | 276 | 180 | 43 | +2 |
| M6-4 后 | 344 | 282 | 174 | 37 | +6 |
| M6-5 后 | 357 | 295 | 161 | 24 | +13 |
| M6-6 后 | 361 | 299 | 157 | 20 | +4 |
| M6-7 后 | 380 | 318 | 138 | 1 | +19 |
| M6-8 后 | 381 | 319 | 137 | **0** | +1 |
| M6-9 / INT 后 | 381 | 319 | 137 | 0 | 0 路由片不动读数 |

不变式（每片都要自检）：`implemented + known_gap == 456`、`regressions == 0`、`unclaimed == 0`。

### 6.2 门 ⑨（契约等价）——M6 相关 fixture 现状与目标

```
$ cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json
totals: fixtures 365 · pass 5 · mismatch 23 · unmounted 31 · unevaluable 306
```

M6 面的 fixture 共 **20 条**（按 path/id 过滤，复算见 §10 命令 7）：

| fixture | 路由 | 现状 | 由哪片转绿 |
|---|---|---|---|
| `skills/*` 5 条 | `/api/skills`、`/api/skills/search` | unevaluable | **M6-2** |
| `agents/TestListAgentSkills_OmitsContent`、`TestSetAgentSkillsRejectsMalformedSkillID`、`TestAddAgentSkillsRejectsMalformedSkillID` | `/api/agents/{id}/skills*` | unevaluable | **M6-4** |
| `agents/TestUpdateAgent_PreservesSkillsInResponse`（GET+PUT）、`TestArchiveRestoreAgent_PreservesSkillsInResponse`（2 条） | `/api/agents/{id}`、`/archive`、`/restore` | unevaluable | **M6-4**（要 agent DTO 带 skills） |
| `context/TestPluginActionRequiresTheFeatureFlag` | `/v1/context` | unevaluable | **M6-7** |
| `issues/TestPluginInstallToken*` 5 条 | `/v1/issues*` | unevaluable | **M6-7** |
| `config/TestGetConfigExposesEnabledPluginsV1Flag` | `/api/config` | unmounted（404） | **不承诺**：`^/api/config$` owner = **M10**；M6-7 只落开关语义 |
| `labels/TestListSkills_IncludesAttachedLabelsAndOmitsContent` | `POST /api/labels` | unevaluable | **不承诺**：该行 owner = **M2**；M6-2 只保证 skill 侧标签语义 |

⇒ 本波承诺 **18 条**。⑨ 的 `unevaluable` 总数会低于 306，但**不许**把「不可判」直接改写成「通过」：
每片的 DoD 要求该片的 fixture 从 `unevaluable` 变 `pass`（或按偏离表判 `mismatch` 并登记理由）。

### 6.3 门 ⑩（文件大小）——预飞：本波哪些文件会撞 800 行

门 ⑩ 只扫 `.rs/.py/.sh/.yml`，**不扫 `docs/**/*.md` 与资产 `.md`**（2,327 行内置资产天然免疫），规则是「只减不增」。
M6 写集里**没有任何文件**在 `scripts/file_size_baseline.tsv`（复算见 §10 命令 8）⇒ 全部走 800 行硬限。预飞：

| 本地文件 | 计划行数 | 风险 |
|---|---|---|
| `routes/skills/crud.rs`、`routes/skills/import.rs` | 500–700 | 上游对应段 1,714 行 ⇒ **必须**按 来源解析 / on_conflict / 归档 / 刷新 四文件拆 |
| `routes/plugin_bridge/mod.rs` | 400–600 | 9 条 Action 与 `/v1` 共享 handler ⇒ 实现在 `v1/handlers.rs`，bridge 侧只做挂载 |
| `routes/v1/handlers.rs` | 600–750 | 5 类资源 9 条路由 ⇒ 按 context / issue / comment / storage 拆文件 |
| `routes/agents/dto.rs` | 694 → **可能破 800** | M6-4 加 `skills` 字段/序列化 ≈60–100 行 ⇒ **先拆** `dto/response.rs` 再改 |
| `mc-repos/src/skill/read.rs`、`plugin/installation.rs` | 300–500 | 上游 `service/plugin.go` 786 行按面拆 |
| `mc-skill/src/archive.rs` | 200–350 | zip 解析 + 路径校验（`validateFilePath`） |
| `mc-daemon/src/mcp/broker.rs` | 400–600 | 上游 `remote_mcp_broker.go` 475 行 |

### 6.4 每片 DoD（通用 + 专属）

**通用（每片都跑，命令见 §10 命令 9）**

1. `bash scripts/gates.sh` **8/8 绿**；有 DB 触碰的片（M6-2/3/4/5/6/7/8）追加 `--with-db` **10/10**。
2. ⑦ 读数与 §6.1 的该片行一致（`implemented + known_gap == 456`、`regressions == 0`）。
3. 形态门：本片不得引入 `MISSING_ALIAS`；allowlist 不新增行。
4. ⑩：新文件 ≤800 行；`scripts/file_size_baseline.tsv` 不动或缩小。
5. 该片路由**每条都有至少一条测试**（handler 级或 e2e），且**不用** `health::placeholder`。
6. 偏离（无 scope 判定 / 未用列 / 未接开关）必须写进 `docs/32` 偏离表，**不许**默默略过。

**专属**

| 片 | 专属验收 |
|---|---|
| M6-0 | ⑦ 读数 = §6.1 的「M6-0 后」行；`grep -r mc-plugin-protocol` 只剩历史文档；`cargo metadata` 通过 |
| M6-1 | bundle 校验的 4 个反例（多文件 surface、超限、非法路径、top-level import）各一条测试；scope 矩阵表驱动；§2.6 的三把派生**逐字对齐**（域分离标签、`whsec_` 形态、`sha256` 无 key 哈希）各一条向量测试；部署密钥**缺失**时四个消费者各自走降级分支 |
| M6-2 | 5 键双形态实测（`/api/skills` 与 `/api/skills/{id}` 两形态各一断言）；`include` 语义；标签复用 M2 仓储 |
| M6-3 | import 两形态（JSON/multipart）、4 种 `on_conflict`、refresh 的 source 归属 |
| M6-4 | 三个 bundle 源（workspace/builtin/plugin）；plugin 源 pinned hash 不符 ⇒ 409；agent 响应含 skills |
| M6-5 | install/preview 共用校验器；包发布 JS 校验正反例；令牌 rotate 只回明文一次（`mpi_` + `whsec_` 同时返回）；`plugin_secret` 在部署密钥缺失时**拒绝落库**（不落明文） |
| M6-6 | 调用记录分页；MCP 采纳 digest 失效语义；surface launch 令牌 TTL |
| M6-7 | 两侧挂载同 handler（响应字节比对）；限流 429；`plugins_v1` 关闭 ⇒ 403 `plugin_api_disabled`；surface token = 2 分钟 TTL + 篡改/过期/错域三种拒绝；`mpc_` 回调令牌**可重复调用直到过期**（第二次调用不得 403） |
| M6-8 | hook 出站四个头**逐字**（`X-Multica-Timestamp`/`-Signature: v1=`/`-Plugin-Installation`/`User-Agent: Multica-Hooks/1`）+ 签名字节向量；反例三种（错签名 / 超 ±5 分钟容差 / 错 installation）；job 幂等。**调度器接线归 `LUM-1659`（M5-9）**：届时未接线 ⇒ 本片交桩级证据 + 登记，不留「假绿」 |
| M6-9 | bundle 缓存校验复用 M3-7 同款 hash；broker 的 pinned tools 拒绝用例 |
| M6-10 | 快照三件套刷新 + 报告；无代码改动 |

---

## 7. 晋升顺序（M6 可以立刻派发的前提）

1. **M5 全合**：`LUM-1570`（M5-5，webhook）、`LUM-1571`（M5-8，scheduler jobs）、`LUM-1572`（M5-INT）落地 —— M6-0 的 baseline/读数/依赖都按 M5 收口后的 base 取。
   **另需 `LUM-1659`（M5-9，调度器接线）**，但它挂在 owner 的 P0 裁决上（`docs/37` §48.3）⇒ **不阻塞 M6-0..M6-7 与 M6-9**，只阻塞 **M6-8** 的运行时效果（§8 R-M6-6）。
2. **并发位**：`docs/plan1.md` 的 3 槽约束 ⇒ M5 收口后最多同时 3 片；M6 的 stage 划分已按此排（§4.3）。
   §48 已把 `LUM-1652` 晋升占末位（3/3 满），M6 代码片的晋升由 cycle 按空位做。
3. 子 issue（全部 `backlog`，`--parent LUM-1652`，stage 与 §4.1 一一对应）：

| # | 子 issue | 标识 | 标题 | stage |
|---|---|---|---|---|
| 1 | M6-0 | **LUM-1665** | M6-0 anchor：占位清理 + 三 crate 骨架 + 契约类型与部署密钥装配 | 1 |
| 2 | M6-1 | **LUM-1666** | M6-1 契约与凭据层（plugincontract / remotemcp / v1 台账 / 四条凭据链） | 2 |
| 3 | M6-2 | **LUM-1667** | M6-2 skill 读写面（12 路由，含全部 5 个双形态键） | 2 |
| 4 | M6-3 | **LUM-1668** | M6-3 skill 导入/刷新生态（2 路由） | 2 |
| 5 | M6-4 | **LUM-1669** | M6-4 skill 供给面：agent 绑定 + builtin/plugin 源（6 路由） | 3 |
| 6 | M6-5 | **LUM-1670** | M6-5 plugin 生命周期与包管理 + 安装令牌（13 路由） | 3 |
| 7 | M6-6 | **LUM-1671** | M6-6 插件运行时面：调用记录 / MCP 采纳 / surface 发放（4 路由） | 3 |
| 8 | M6-7 | **LUM-1672** | M6-7 公开 Action API + plugin-bridge + surface 承载（19 路由） | 4 |
| 9 | M6-8 | **LUM-1673** | M6-8 hook 引擎 + MCP 传输 + hook job（1 路由） | 4 |
| 10 | M6-9 | **LUM-1674** | M6-9 daemon 侧 skill/MCP 执行面（0 路由） | 4 |
| 11 | M6-10 | **LUM-1675** | M6-10 INT：集成、快照刷新与缺口登记（0 代码） | 5 |

> 晋升规则与 M5 相同：`backlog → todo` 才起跑；同 stage 内三片可并行；上一 stage 未合不进下一 stage。

---

## 8. 风险登记（每条对应一个 DoD 或一个「登记不实现」的决定）

| ID | 风险 | 缓解 / 决定 |
|---|---|---|
| **R-M6-1** | 插件包 JS 校验用窄口径扫描器 ⇒ 可能放过藏在词法陷阱里的 top-level `import` | 影响面 = 浏览器里 surface 运行失败（iframe + CSP，不是越权）；§2.5 登记偏离；若实测出现漏判，升级 `oxc_parser` 并重开一片 |
| **R-M6-2** | `skill` 表被 M6-2（读写）、M6-3（导入）、M6-4（绑定+源）、M6-5（plugin 源写入）四方读写 | 写集按**本地文件**隔离（§3.2）；`content_hash` 单一实现点在 `mc-skill`；跨片契约写进各片 DoD |
| **R-M6-3** | skill 导入自建 GitHub 抓取，与 W8 的 GitHub 面重复 | M6-3 定义 `SkillSourceFetcher` port；W8 落地后由其实现；偏离登记 |
| **R-M6-4** | `/v1` 与 bridge 两侧 handler 分叉（响应不一致） | 同片（M6-7）+ DoD 要求两侧响应字节比对 |
| **R-M6-5** | M6-6 的 MCP 采纳与 M8 的 `agent_mcp_server` 面语义重叠 | 本波只做 workspace 级（`workspace_mcp_server`），路由归属已在 fixture 明确；M8 面不动 |
| **R-M6-6** | `apps/mc-server` 没有 `mc-scheduler` 依赖与 spawn（M5-8 按降级方案交付、M5-INT 写集不含） ⇒ M6-8 的 hook job 无处运行 | 已由 **`LUM-1659`（M5-9 接线片）** 承接（挂在 owner P0 裁决上）。M6-8 的前置 = `LUM-1659` 合入；未合时本片只交桩级证据 + 登记，**不重复实现**接线 |
| **R-M6-7** | 内置 skill 资产 2,327 行是**内容**移植，不是代码翻译 ⇒ 「看起来完成了但语义漂移」 | M6-4 用「资产清单 + 与上游逐文件 sha 对比」作验收，不靠人工目测 |
| **R-M6-8** | `routes/agents/dto.rs` 694 → 破 800 | §6.3 预飞：先拆 `dto/response.rs` |
| **R-M6-9** | `serde_yaml` 停止维护 | §2.5 已定；anchor 一次定死，不留给切片挑 |
| **R-M6-10** | M6 的 20 条 fixture 里 2 条归属它波（`/api/config` M10、`POST /api/labels` M2） | 本波**不承诺**这两条，只在 M6-10 报告里登记联动依赖 |
| **R-M6-11** | 插件面是**安全敏感面**（安装令牌 / 签名 / surface token） | M6-1 的 scope 判定与 §2.3 的五条凭据链是唯一实现点；每片 DoD 要求正反例测试；`mpi_` 只存 hash、hook 签名密钥**只派生不落库** |
| **R-M6-12** | 部署密钥（`MULTICA_PLUGIN_SECRET_KEY`）本地**零装配路径** ⇒ 「插件面整体降级」会被误当成实现缺陷 | §2.6 定死：降级是**上游真实行为**（secrets fail closed、hooks/surfaces 关闭、Public API 仍可用），本地必须用**显式错误码**（`plugin_disabled` / `plugin_surfaces_not_configured`）表达，**不许 panic、不许静默放行**。anchor 负责读 env 进 `AppState`，M6-1 负责四个消费者 |
| **R-M6-13** | 回调令牌 `mpc_` 是**进程内存 + 到期前可多次调用**的语义，容易被我方写成「一次性」或「落库」 | §2.4 第 10 条写死；M6-7 DoD 含「第二次调用不得 403」；多实例提前失效（403）写进偏离表作为已知行为 |

---

## 9. 与 `docs/plan1.md` §5 / §8 的差异（必须修订项）

### 9.1 口径修订一：上游 schema 是 **14 张表**，不是 26

`docs/plan1.md` §5 W6 写「W6 相关 26 张表已在 `migrations/upstream/`」——实测 `plugin_*` 面有 **22 个**
`CREATE TABLE`，其中 **14 个**被 `migrations/upstream/344_plugin_v2_reset.up.sql` DROP，
**同文件重建** `plugin_installation` 一张（⇒ 存活 **8** 张 plugin 表）；加 skill/MCP 侧 **6 张** = **14 张**（§1.3 表）。
`contracts/upstream-schema.json` 里 `plugin_identity`、`plugin_release`、`plugin_grant` 等被 DROP 的表名 0 命中即为证据（复算见 §10 命令 6b）。
⇒ **结论不变（0 新迁移）**，但数字必须改对，否则 M6-5/6/7 会去建不存在的表。

### 9.2 口径修订二：plugin host 面实测 **17 + bridge 20**，不是「host 10」

`plan1.md` §5 W6 的分组写「skill 14 + plugin host 10 + MCP」。路由表实测（commit `f41fae6b08fb`）：

- workspace plugin 面 **17** 条（原口径 10 条漏掉了包管理 4、MCP 工具 2、surface launch 1）；
- 公开面 **20** 条 = `/v1` 9 + `/api/plugin-bridge/v1` 10 + `/plugin-surfaces/{token}` 1（原口径把 `/v1` 与 bridge 当成一套，
  实际是**同一批 handler 注册两次**，路由账要算两份）。

⇒ 本波路由总数 57（不是 46），切片数因此从「5 片」变为 **9 片 + anchor + INT**。

### 9.3 口径修订三：`pkg/plugincontract` **不是** JSON-RPC

`plan1.md` §5/§10.5 写「上游 plugincontract 就是 JSON-RPC」。实测 `pkg/plugincontract` 是**声明式**的
manifest / bundle / capabilities 校验器（`ManifestVersion1`、`Capabilities`、`MaxBundleSize`、`Hook`、`NetDomains`、
`ConfigSecret`、`Resource`、`TriggerSchedule`），**没有任何 RPC 帧**；它不被 daemon 引用（引用者：handler 5 / service 10 / scheduler 1）。

真正的 JSON-RPC 在手边两个地方，且都不是「插件 ↔ host」：

1. **MCP**：`pkg/remotemcp/client.go` 走 `initialize` → `notifications/initialized` → `tools/list` → `tools/call`（`"jsonrpc":"2.0"`）；
2. **插件 surface 的浏览器侧**：`packages/plugin-sdk/protocol.ts` 的 MessagePort 桥（`BRIDGE_PROTOCOL_VERSION = 2`），不是 stdio。

⇒ 本波验收口径改写为「**hook 的 HTTP 签名调用 + MCP 工具采纳/调用端到端**」；本地已有的 `mc-plugin-protocol`
（343 行 stdio JSON-RPC，零依赖者）**删除**（§2.2），不再作为落点。

### 9.4 与 M5 计划的结构一致性

§3/§4/§5/§6/§7/§8/§10 与 `docs/44-M5-PLAN.md` 逐节同构（anchor 机制、写集矩阵、波次 ≤3、门 ⑦ 预测表、门 ⑩ 预飞、
风险登记、复算命令），便于跨波对比；不重复解释已在 `docs/44` 立过的规矩。

### 9.5 落地修订（M6-0 anchor `LUM-1665` 实测，**M6-1…M6-9 按本节读计划**）

§5「每文件预扩张清单」落地时有 4 处归位判断与 1 处新增，逐条如下（细节与理由见 `docs/32-M3-DAEMON-FACE.md` §9）：

| # | §5 原文 | 实际落点 | 为什么 |
| - | --- | --- | --- |
| 1 | `mc-skill/src/git.rs`（M6-3） | **`mc-skill/src/source.rs`**（M6-3） | 上游**没有** git 克隆路径：`internal/handler/skill.go:943-945` 只有 `clawHub` / `skillsSh` / `github` 三个 **HTTP** 源（`detectImportSource` L950）；保留 `git.rs` 会误导实现者去写 `git clone` |
| 2 | `POST /api/plugin-bridge/v1/hooks/{key}` 归 `routes/plugins/hooks_job.rs`（M6-8） | **`routes/plugin_bridge/hooks.rs`**（M6-8）；`hooks_job.rs` 退为 **0 路由**的 job 粘合落点 | 路由前缀与文件所在目录一致，漏挂风险最小；注册键总数不变（57/57） |
| 3 | `/api/agents/{id}/skills*` 6 条（M6-4） | M6-4 **自己**建 `routes/agents/skills.rs` + 在 `routes/agents.rs` 加 `mod skills;` 与 merge（锚点**不**碰 M2/M4 的既成文件） | `routes/agents.rs` 在 M6 内由 M6-4 独占；锚点越界会破「一文件一写者」 |
| 4 | `state.rs` 只点名 `plugin_key` | 同处**加** `plugin_surface_origin: Option<String>` | `state.rs` 被锚点冻结（M6 各片只读）⇒ 留给 M6-6/M6-7 会逼它们改冻结文件 |
| 5 | `plugin_key` 的类型未定 | `mc-http::state::PluginSecretKey { key: [u8; 32] }`（newtype，仿 `GoogleOAuthConfig`）；`mc-plugin-host::credentials` 只收 `&[u8]` | 唯一出口放 `state.rs`，骨架 crate 不读 env，便于单测（`state.rs` 已有 fake getter 的 `env()` 辅助） |

**依赖版本（按声明的 `rust-version = 1.80` 选，不是「取最新」）**：`tower_governor 0.4`（0.8 拉 axum 0.8 + tonic 0.14
⇒ lock 里出现第二个 axum 大版本，`Body` 不兼容；⚠️ rsproxy 索引下 manifest 键名必须写**下划线** `tower_governor`）、
`zip 2`（6.0 MSRV 1.83 / 8.6 MSRV 1.88 均高于声明 MSRV）、`serde_yaml 0.9`、axum 加 `multipart`。

**本锚点实测口径（后续各片起手基准）**：⑦ `local 325 / baseline 325`、`implemented 263 real + 0 placeholder`、
`known_gap 193`、`owners.M6 57`、`local_only 9`；`slash-alias-allowlist.tsv` **已空**
（`slash_alias_audit --declared docs/fixtures/m6-declared-routes.tsv` = `5 defect`，即 5 个双形态键全在 M6-2，**非回归**）。
注意 §6.1 的预测表是在 `eaba357` 上测的：**delta 有效，绝对值过期**（`docs/37` §51.4 已按 delta 平移过一轮）。

---

## 10. 复算命令（全部只读，可在任意 workdir 复现）

```bash
# 1. 路由表：本文 §1.1 与上游 owner=M6 行集合相等（应无输出）
diff <(awk -F'\t' '!/^#/ && $3=="M6"{print $1"\t"$2}' docs/fixtures/upstream-routes.tsv | sort) \
     <(grep -v '^#' docs/fixtures/m6-declared-routes.tsv | tail -n +2 | sort)

# 2. 形态门（预测模式：期望 "FAIL: 3"，5 键需双形态、其中 2 键在 allowlist；exit 1 是预期）
#    M6-0 删掉那 2 行 allowlist 之后，同一命令应变成 "FAIL: 5"
python3 scripts/slash_alias_audit.py --declared docs/fixtures/m6-declared-routes.tsv

# 3. 本地形态现状（0 defect；M6 的 2 行 allowlist 在 M6-2 合并后必须消失）
python3 scripts/slash_alias_audit.py

# 4. ⑦ 读数（local 328 / implemented 264 / known_gap 192 / owners.M6 55；implemented+known_gap 必须 = 456）
python3 scripts/route_parity.py --quiet && python3 - <<'PY'
import json,subprocess
d=json.loads(subprocess.run(["python3","scripts/route_parity.py","--json"],capture_output=True,text=True).stdout)
print(d["counts"]); print("owners.M6 =", d["owners"].get("M6"))
PY

# 5. 上游行数（§1.2 表；需要 /tmp/ups_multica @ 90e0bdf）
cd /tmp/ups_multica/server && for f in internal/handler/skill.go internal/handler/skill_create.go \
  internal/handler/skill_import_archive.go internal/handler/skill_refresh.go internal/handler/agent_runtime_skills.go \
  internal/handler/plugin.go internal/handler/plugin_action.go internal/handler/plugin_hook.go \
  internal/handler/plugin_package.go internal/handler/plugin_surface.go internal/handler/plugin_mcp.go \
  internal/handler/workspace_mcp_api.go internal/handler/mcp_overlay.go internal/handler/plugin_agent_hook.go \
  internal/service/plugin.go internal/service/plugin_action.go internal/service/plugin_hook.go \
  internal/service/plugin_package.go internal/service/plugin_storage.go internal/service/plugin_token.go \
  internal/service/plugin_skill.go internal/service/plugin_mcp_transport.go internal/service/plugin_event_dispatch.go \
  internal/service/plugin_event_bridge.go internal/service/plugin_schedule.go internal/service/plugin_agent_tools.go \
  internal/service/builtin_skills.go internal/scheduler/jobs_plugin_hook.go \
  pkg/plugincontract/manifest.go pkg/plugincontract/bundle.go pkg/plugincontract/capabilities.go; do
  printf "%6d  %s\n" "$(wc -l < $f)" "$f"; done
# 内置资产（§1.2 末行 = 2,327 行 / 11 文件）
find internal/service/builtin_skills internal/service/builtin_skills_legacy -type f | wc -l
find internal/service/builtin_skills internal/service/builtin_skills_legacy -type f -exec cat {} + | wc -l

# 6. 14 张 M6 表是否都在（应逐行有输出；本波 0 新迁移）
for t in skill skill_file skill_to_label agent_skill agent_mcp_server workspace_mcp_server \
         plugin_installation plugin_storage plugin_package plugin_package_version \
         plugin_package_file plugin_secret plugin_invocation plugin_hook_schedule; do
  printf "%-24s %s\n" "$t" "$(grep -rl "CREATE TABLE.*\b$t\b" migrations/ | tr '\n' ' ')"; done

# 6b. 「26 → 14」的算术：plugin_* 的 CREATE 数、344 的 DROP 数、DROP 后是否重建
grep -rho 'CREATE TABLE plugin[a-z_]*' migrations/upstream/*.sql | wc -l                                    # 22
grep -c 'DROP TABLE' migrations/upstream/344_plugin_v2_reset.up.sql                                          # 14（全是 plugin_*）
grep -c 'CREATE TABLE plugin_installation' migrations/upstream/344_plugin_v2_reset.up.sql                    # 1（重建）
# ⇒ 22 − 14 + 1 = 8 张存活 plugin 表；加 skill/skill_file/agent_skill/skill_to_label/agent_mcp_server/workspace_mcp_server = 14

# 7. ⑨ M6 fixture 现状（§6.2）：20 条相关 fixture 的分类
python3 - <<'PY'
import json
d=json.load(open("crates/mc-conformance/report.json"))
sel=[f for f in d["fixtures"] if "/api/skills" in (f["path"] or "") or "plugin" in (f["path"] or "")
     or (f["path"] or "").startswith("/v1") or "skill" in f["id"].lower() or "plugin" in f["id"].lower()]
print(len(sel)); [print(f"{f['id'][:60]:<60} {f['outcome']:<12} {f['status_expected']}") for f in sel]
PY

# 8. 门 ⑩ 预飞：M6 写集是否已有人被列入白名单（应无输出）
awk -F'\t' '!/^#/ && NF{print $1}' scripts/file_size_baseline.tsv | grep -E 'skill|plugin|mcp|agents/dto' || echo "NONE"

# 9. 全量门禁（每片交付前；有 DB 触碰的片追加 --with-db）
bash scripts/gates.sh                # ①fmt ②build ③clippy ④clippy-test-util ⑤test ⑦route-parity ⑨conformance ⑩file-size
bash scripts/gates.sh --with-db      # 追加 ⑥db ⑧schema-drift（需 MULTICA_TEST_DATABASE_URL）
```

---

## 11. M6-INT 落地记录（占位，由 M6-10 填写）

_（待 M6-10 交付后填写：⑦/⑨/基线与 allowlist 的最终读数、跨片缺口、与本文预测的差异。）_
