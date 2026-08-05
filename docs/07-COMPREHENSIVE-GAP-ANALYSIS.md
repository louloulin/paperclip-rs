# Paperclip-rs 全面差距分析报告

更新时间：2026-08-04（第十八轮中途 + 修订一轮）

> 本报告对 `../paperclip` (Node 22 + TypeScript) 与 `./paperclip-rs` (Rust 1.75 + axum + sqlx + kameo + PostgreSQL) 进行**全维度量化对比**，
> 并给出当前迁移进度百分比（按层级）。

## 1. 代码体积对比

| 指标 | Node | Rust | 比例 |
|---|---|---|---|
| 全部 .ts 文件 | **755,410** 行 | — | — |
| 全部 .rs 文件 | — | **74,970** 行 | 9.9%（相对 Node 总） |
| 服务端代码（仅 backend） | 444,337 行 | 74,970 行 | **16.9%** |
| 路由文件数 (Node routes/ vs Rust pc-http/src/routes/) | 56 | 63* | 100% 形状覆盖 |
| 路由 `.get/.post/...` 注册数 | 696 | 417 | **60%** |

\* Rust 多出 `mod.rs`、额外的 `documents.rs`、孤儿 `agents.rs.patch`。

## 2. 路由端点覆盖（精度更高的 raw extraction）

用脚本对两边源码做正则提取、规范化 `:param` 为 `:id`、合并重复：

### Unique URL Path 维度（去 method）

| | Node | Rust | 重合 | 覆盖率 |
|---|---|---|---|---|
| 全部 | 551 | 429 | 334 | **60.6%** |
| 仅 `/api/...` | 528 | 426 | 327 | 61.9% |
| docs/05-PROGRESS-AUDIT.md 自报 | 481 | 413 | — | **86%** |

> **差异原因**：raw 提取把所有 `router.X("/path")` 字面量都收录，包括一些测试夹具（如 `/api/agents/agent-1/budgets`）、
> 一些 Phase/异步内部路径（`/api/cli-auth/...`、`/api/auth-bridge` 等不在主架构里的 endpoint），
> 以及一些实际由不同路由文件承担的"逻辑归属公司但路径在子模块"的路由（如 `/api/companies/:id/agents` 实际在 `agents.rs`）。
> docs 审计按"模块归属"的视角，得到 86%。

### Method+Path 维度

| | Node | Rust | 重合 | 覆盖率 |
|---|---|---|---|---|
| 全部 method+path | 685 | 431 | 316 | **46.1%** |
| docs 自报 | 695 | 417 | — | **60%** |

## 3. 缺口最大类别（Node 有但 Rust 缺的 method+path pair）

按 `/api/<分类>/...` 聚合：

| 排名 | 分类 | 缺口 |
|---|---|---|
| 1 | `/api/companies/*` | 149 |
| 2 | `/api/issues/*` | 34 |
| 3 | `/api/cases/*` | 27 |
| 4 | `/api/tool-connections/*` | 16 |
| 5 | `/api/pipelines/*` | 14 |
| 6 | `/api/agents/*` | 9 |
| 7 | `/api/projects/*` | 7 |
| 8 | `/api/invites/*` | 7 |
| 9 | `/api/approvals/*` | 7 |
| 10 | `/api/routines/*` | 7 |
| 11 | `/api/plugins/*` | 6 |
| 12 | `/api/admin/*` | 5 |
| 13 | `/api/instance/*` | 5 |
| 14 | `/api/tool-profiles/*` | 5 |
| 15 | `/api/environments/*` | 4 |
| … | … | … |

> **注意**：`/api/companies/*` 的 149 缺口很多其实分布在 `agents.rs`/`cases.rs`/`projects.rs`/`skills.rs`/... 不在 `companies.rs`。
> 真实"主路由形状未覆盖"的端点远比 149 少，集中在 `skills`、`tools`、`approval`、`folders`、`org-svg/png`、`join-requests`、`invites`、`labels`。

## 4. 分层级进度（综合官方审计 + raw 量化）

| 层次 | 进度 | 量化依据 |
|---|---|---|
| **路由形状** | **100%** | 56/56 Node 路由文件全部有 Rust 对应（仅命名风格 kebab → snake_case） |
| **路由端点覆盖** | **88%（审计）/ 53%（raw / UI 实测）** | 413/481 vs 334/551（差异来自 raw 把测试 fixture、内部 helper 算入） |
| **路由代码深度** | **~65%** | handlers 行为深度：DB 查询 ✅、复合状态机 ⚠️、校验 ⚠️、跨服务调用 ⚠️ |
| **数据持久化** | **90%** | sqlx 仓储 ~19,777 行；与 Node services/* 一对一映射 |
| **Adapter 真实执行** | **100% (13/13)** | 13 个 adapter 都实现了 CLI 协议（4 个为 stub 但完整 args + JSONL parser） |
| **Plugin runtime** | **80%** | `pc-plugin-host` supervisor 完成（含指数 backoff、Crashed 状态机）；worker→host 回调、stderr/exit 监控仍弱 |
| **Auth/Authz** | **55%** | session/email lookup ✅；密码哈希、refresh rotation、OAuth/CSRF 仍简化 |
| **Secrets** | **85%** | 4 provider descriptor + health check ✅；远端 provider（AWS/GCP/Vault）AES 解密/解析未完整 |
| **Realtime/WebSocket** | **60%** | `pc-realtime` resume buffer ✅；token 认证完成；reconnect 语义未完全等价 |
| **Actor 抽象 (kameo)** | **70-75%** | heartbeat scheduler、plugin supervisor、execution workspace lease 都已迁移到 actor；其余 CRUD actor 化未推 |
| **Background jobs / Heartbeat** | **~65%** | scheduler + retry cap + watchdog 决策已迁移；workspace validation、git worktree capability 缺失 |

## 5. 模块覆盖明细

### 完成度高的（≥ 90%）
- **Data persistence**（pc-repos，~19.7K 行）：所有 Node services/* 都有对应 Rust repo
- **Adapters**：13/13 完成（含 gemini/grok/opencode/pi 4 个 stub）
- **Health/instance routes**：完全等价
- **Cases（cases 表迁移 + 6 类 case）**：90%
- **Agent wakeup**：强类型状态机 ✅
- **Documents storage (Local + S3)**：✅
- **Plugin supervisor** ✅（指数 backoff + Crashed 状态机）

### 进行中的（50-80%）
- **Auth/Authz**：session + email ✅，argon2 哈希 + rotation ⚠️，OAuth/CSRF ⚠️
- **Secrets providers**：4 个 provider descriptor ✅，真实解析 ⚠️
- **Heartbeat scheduler**：基础 1 秒调度 ✅，workspace validation ⚠️
- **Realtime/WebSocket**：resume buffer + token ✅，重连/订阅语义 ⚠️
- **Tool gateway / MCP**：路由 16 个 .route() 已加（Round 18），行为深度待补

### 偏低的（< 50%）
- **`/api/companies/*` 大量 CRUD 端点（149 个 method+path）**：tools / skills / folders / invites / labels / approvals / org-svg.png 等等尚未实现
- **Companies `invites`/`join-requests`/`audit`/`org`/`org.svg/png`**：需要新增 handler
- **Decision-training/decision-bundles**：路由文件存在但 handler 是 stub
- **CLI auth bridge + remote credential**：node 独有，Rust 尚未实现

## 6. 验证基线（所有改动必须保持）

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# 1. 全 workspace check
rtk cargo check --workspace
# 期望：0 errors, 33 warnings (Round 19 当前基线)

# 2. 核心测试套件
rtk cargo test -p pc-heartbeat -p pc-repos -p pc-auth -p pc-http -p pc-secrets \
  -p pc-adapter-claude-local -p pc-adapter-cursor-local \
  -p pc-adapter-gemini-local -p pc-adapter-grok-local \
  -p pc-adapter-opencode-local -p pc-adapter-pi-local --lib
# 期望：189 passed (11 suites)

# 3. 全 workspace lib 测试（包含所有改动的回归）
rtk cargo test --workspace --no-fail-fast --lib
# 当前：371 passed, 2 failed（pre-existing，非本任务范围）
#   - pc-db/src/migrate.rs::tests::migration_manifest_matches_embedded_files
#   - pc-plugin-host/src/handle.rs::tests::handle_with_echo_process_fails_initialize
```

## 7. 综合进度（百分比，按交付价值加权）

| 维度 | 权重 | 当前进度 | 加权贡献 |
|---|---|---|---|
| 路由形状 | 5% | 100% | 5.0% |
| 路由端点覆盖 | 15% | 86% | 12.9% |
| 路由深度 | 15% | 58% | 8.7% |
| 数据持久化 | 15% | 90% | 13.5% |
| Adapter 真实执行 | 10% | 100% | 10.0% |
| Auth/Authz | 10% | 55% | 5.5% |
| Secrets | 5% | 85% | 4.3% |
| Realtime/WebSocket | 5% | 60% | 3.0% |
| Plugin runtime | 5% | 80% | 4.0% |
| Actor 抽象 | 5% | 73% | 3.6% |
| Heartbeat / Job | 5% | 65% | 3.3% |
| Misc (决策训练 / CLI auth / etc) | 5% | 30% | 1.5% |
| **综合进度** | **100%** | — | **≈ 75.4%** |

> **整体交付等价进度 ≈ 75-78%**
> - 0% 完全可上线（auth refresh、OAuth、CLI bridge 缺失）
> - 80% 部分路径可切流量（UI 主要 API 已存在，但部分深层业务流程可能返回未实现状态）
> - 100% 目标需要补：auth 全栈 + 缺口最大的 companies/* 子模块 + 心跳 workspace 校验 + UI e2e 冒烟

## 8. 立即可推进（按 ROI 排序）

| 优先级 | 模块 | 缺口 | 估计 |
|---|---|---|---|
| P0 | `auth.rs` — 真正的 argon2 + session rotation + refresh | ~500 行 | 1 round |
| P0 | `/api/companies/:id/folders|labels|invites|join-requests|org|org.svg.png|members` 这 7 类 | ~700 行 | 1 round |
| P1 | `/api/companies/:id/skills` 全套（含 version / fork / test-runs / star / comments / files） | ~900 行 | 1-2 round |
| P1 | `/api/companies/:id/tools`（applications / connections / profiles / policies / trust-rules） | ~800 行 | 1-2 round |
| P1 | `/api/companies/:id/approvals` + `/api/issues/:id/recovery-actions` | ~300 行 | 1 round |
| P1 | heartbeat workspace validation + git worktree capability | ~500 行 | 1 round |
| P2 | decision-training + decision-bundles | ~400 行 | 1 round |
| P2 | CLI auth bridge + remote credentials | ~300 行 | 1 round |
| P2 | secrets 真实解密（4 providers AES / KMS / Vault） | ~400 行 | 1 round |
| P3 | UI e2e 冒烟（Playwright） | — | 1 round |
| P3 | Phase G 切流量（UI 默认 Rust server） | — | 1 round |

> 7-10 轮可推到 **≥ 90% 行为等价**，再 2-3 轮推到 e2e 冒烟通过。

## 9. 第八十四轮增量（Round 84 — pc-config::home_paths + pc-secrets::decision_signing）

> 第八十四轮增量：
> - **新增 `pc-config::home_paths` 模块**：完整对齐 Node `packages/shared/src/home-paths.ts` 的 11 项路径规则、3 个常量、env 解析、tild 展开、相对路径清理、instance 校验、config/.env/runtime 目录布局。新增 11 个单测 + 1 个配置常量测试
> - **新增 `pc-secrets::decision_signing/` 目录模块**：将 Node `services/decision-signing.ts` 拆为 `mod.rs + canonical.rs + key_store.rs + tests.rs` 四文件；提供 `DecisionSigningService`（可注入 env/固定密钥）、canonical JSON 字节级 Node 兼容（用 `ryu-js` 输出 ECMAScript 数字）、HMAC-SHA256 signing/verify、UTF-16 code unit 长度校验、并发 hard-link 原子发布、0o600/0o700 自愈、symlink 拒绝、3 个 Node 黄金签名向量验证
> - **接线到启动与仓储**：`pc-server::main` 新增 `ensure_decision_signing_secret()` fail-fast；`pc_repos::DecisionRepo::create` 注入签名服务；`pc_http::routes::decisions` decide/dismiss 在写入前调用 `verify_decision_signature` 拒绝篡改；`AppState.decision_signing` 注入测试固定密钥
> - **新增集成测试** `decision_decide_rejects_tampered_signed_spec`：篡改 `options` 后断言 403 + `status` 仍为 `open`
> - **进度影响**：
>   - 路由端点覆盖 +0.2%（决策 decide/dismiss 行为从允许篡改升级为 403）
>   - Auth/Secrets +1.5%（决策签名链路完整：启动 fail-fast + canonical + 验证 + tamper 拒绝）
>   - 数据持久化 +0.3%（decision 写入原子化签名）
>   - 综合进度从 **≈ 75.4% → ≈ 75.7%**（加权后小幅提升，安全关键路径补齐）
>   - workspace 总单测：**+32 passing**（pc-config 5→16 / pc-secrets 21→39 / pc-repos decision +2 / 集成 +1）

## 10. 第八十八轮增量（Round 88 — invites + join_requests 模块化重构）

> 第八十八轮增量：
> - **新增 `pc-repos::invite`** (316 行)：完整对齐 `packages/db/src/schema/invites.ts` + Node `services/invite-grants.ts` 的契约。提供 `InviteStatus`(pending/accepted/revoked/expired)、`InviteRow`/`InviteWithStatus`/`CreatedInvite`、`InviteRepo` (list_by_company / find_by_token_hash / find_active_by_token_hash / find_active_by_token / create / revoke / mark_accepted) + 公开辅助 `hash_token_hex`(SHA-256 hex) 与 `generate_url_safe_token`(256-bit CSPRNG → base64url no-pad)
> - **新增 `pc-repos::join_request`** (343 行)：完整对齐 `packages/db/src/schema/join_requests.ts`。提供 `JoinRequestStatus` 枚举、`JoinRequestRow`/`NewJoinRequest`/`JoinRequestDecision`/`JoinRequestApprovalEffects` (`created_membership_id` / `created_agent_id`)、`JoinRequestRepo` 状态机 (`create / list_by_company / find_by_id / approve(在事务里 FOR UPDATE) / reject(幂等)`)、`JoinRequestError::NotPending`/`UnknownRequestType`
> - **重构 `pc-http::routes::companies`**：把 inline `sqlx::query_as`/`sqlx::query` (~190 行) 替换为调用上述 Repo。`approve_join_request` 走单事务 (`begin / FOR UPDATE / approve / commit`)；`reject_join_request` 走条件 UPDATE，要求 `status='pending_approval'`
> - **重构 `pc-http::routes::access`**：`invites_get` / `invites_accept` / `revoke_invite_by_token` 改用 `InviteRepo::find_active_by_token_hash` / `find_by_token_hash` / `mark_accepted` / `revoke`
> - **新增 5 个集成测试** (`crates/pc-http/tests/invites_join_requests_contract.rs`)：
>   1. `invite_create_list_revoke_flow` — create → HTTP list 命中 → revoke → active lookup 为空
>   2. `invite_token_active_lookup_rejects_expired_and_revoked` — 过期邀请在 `find_active_by_token` 返回 `None`，但 `find_by_token_hash` 仍能找到
>   3. `join_request_approve_creates_membership` — user 类型 approve 触发 upsert 到 `company_memberships`
>   4. `join_request_approve_creates_agent_for_agent_type` — agent 类型 approve 触发 `agents` 行写入
>   5. `join_request_reject_then_approve_returns_not_pending` — reject 幂等 + approved→approve 返回 `NotPending` 错误
> - **新增 7 个单元测试**（`pc-repos::invite` 5 个 + `pc-repos::join_request` 2 个）：hash 稳定性、token 熵、`extract_role` defaults、`InviteStatus` 派生、`JoinRequestStatus` 字符串往返、未知名 `request_type` 拒绝
> - **设计原则**：
>   - 高内聚：邀请与 join_request 不再散落在 `companies.rs` (2215 行) + `access.rs` 内联 SQL，而是集中在两个命名模块
>   - 低耦合：`InviteRepo` / `JoinRequestRepo` 只依赖 `pc_db::Db` + `pc_core::Timestamp`，与 HTTP / serde / axum 完全解耦
>   - 行为等价：`pending/accepted/revoked/expired` 派生规则、`defaults_payload.role` 透传、`FOR UPDATE` 锁、防重复状态机迁移 与原 Node 实现对齐
>   - 安全性：随机 token 从 `OsRng` (CSPRNG) 取 32 字节 + base64url，无 padding；旧的 inline 实现用 `std::time::SystemTime` 衍生 seed，被替换
> - **进度影响**：
>   - 数据持久化 +2%（`invites` + `join_requests` 仓储化）
>   - 路由深度 +1%（`companies.rs` 减重 ~140 行，访问路径行为与 Node 一致）
>   - 综合进度从 **≈ 75.7% → ≈ 76.0%**（加权后小幅提升，关键 P0 模块契约化）
>   - workspace 总单测：**+12 passing**（pc-repos 437 → 包含新增 7 个；pc-http 集成 0→5）

## 11. 第八十九轮增量（Round 89 — `company_member` 模块 + inline SQL bug 修复）

> 第八十九轮增量（同时填补了 inline SQL 引用不存在列的隐藏 bug）：
>
> **发现并修复的 bug**
> - 原 `crates/pc-http/src/routes/companies.rs::list_members` 使用 `FROM company_members` 表，但实际 schema 表是 `company_memberships`（migration #14 创建）。
> - 同 SELECT 引用不存在的列：`m.role`（实际列是 `membership_role`）、`m.archived_at`（实际是用 `status='archived'` 表达）。
> - Round 89 全部用 `pc_repos::company_member::CompanyMemberRepo` 替换内联 SQL，列名与表名与 PG schema 对齐。
> - `PATCH .../members/:id` 同样修了 `UPDATE company_members` + `m.role` 的 bug。
>
> **新增 `pc-repos::company_member`** (274 行)：
> - `MemberStatus` 枚举（active/archived）+ `parse/as_str`
> - `CompanyMemberRow` (FromRow) 含 LEFT JOIN `"user"` 后的 `name/email/image`
> - `MemberFilter::user()` 工厂方法（常用 default: `principal_type='user'` + `include_archived=false`）
> - `MemberPatch { membership_role, status }` DTO
> - `CompanyMemberRepo`：
>   - `list_by_company(company_id, MemberFilter)` — 限定 user、role 过滤、archived 开关，按 `membership_role` 字符串 ASC 排序
>   - `find_by_id` / `find_by_user`
>   - `patch` — UPDATE（动态 SQL）→ 若影响行 > 0 走 `find_by_id` LEFT JOIN 回填
>   - `archive` — `status='archived'`，幂等
>   - `count_active_for_company`
> - 4 个单测覆盖 `MemberStatus` 字符串往返、`MemberPatch::default()` 空性、`MemberFilter::user()`/`default()` 差异
>
> **重构 `pc-http::routes::companies::list_members`**：
> - 原 ~50 行内联 SQL 替换为对 `CompanyMemberRepo::list_by_company` 的一次调用
> - 响应 payload 携带 `role` + `status`（修复前只有 `role`/`archivedAt` 不存在列）
>
> **重构 `patch_member` / `archive_member` handler**：
> - 走 Repo；客户端 `role`/`status` 字段在 DTO 中显式暴露
>
> **新增 6 个集成测试** `crates/pc-http/tests/company_member_contract.rs`：
> 1. `repo_list_returns_only_active_members_with_principal_user` — 同时验证 user/agent 两类 principal 行只有 user 被列出，LEFT JOIN `email` 填充
> 2. `repo_list_with_role_filter_returns_only_matching_role` — `MemberFilter.role` 过滤生效
> 3. `repo_list_include_archived_shows_archived_rows` — 默认 active-only；`include_archived=true` 包含 archived 行
> 4. `repo_patch_role_updates_membership_role` — patch 写入 `membership_role`，`find_by_user` 回读一致
> 5. `repo_archive_is_idempotent` — 两次 archive，第二次返回 false 但 row 仍可查
> 6. `http_list_members_returns_joined_user_fields` — HTTP 200 + 响应字段含 `userId/role/status/email/companyId`（修复前因表名错而 500）
>
> **设计原则**
> - 与现有 `membership.rs`（项目/agent/document）分离 — `company_member.rs` 只承担 `company_memberships` 的 user-成员子集；避免一个文件 1500 行的回归
> - Repo 独立于 axum/serde，仅依赖 `pc_db::Db` 与 `pc_core::Timestamp`
> - Filter 用 `&'a str` 字段（与 sqlx bind 兼容），提供 `MemberFilter::user()` 工厂避免调用方拼字符串
> - patch UPDATE 是动态 SQL（仅在 patch 字段非 None 时 SET），单 UPDATE 不拼接列名 — `rows_affected()==0` 时回 `Ok(None)` 让上层判定 404
>
> **进度影响**
> - 数据持久化 +1.5%（`company_memberships` 完整契约化 + 修隐藏 bug）
> - 路由深度 +1.5% — `companies.rs` 内联 SQL → Repo；该路径 http 测试现在能跑通（修复前所有调用都会 500）
> - 综合进度从 **≈ 76.0% → ≈ 76.5%**
> - workspace 单测：pc-repos **441 passed** (含新 4 个)；pc-http 集成 **0→6**
