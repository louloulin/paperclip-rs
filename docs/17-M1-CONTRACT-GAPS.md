# M1-E — 契约缺口补齐（LUM-1362）

本文件记录 M1-E 对 **M1 合并树 vs 上游 multica** 之间全部差异的处置：
路由清单（含上游行号）、每项决策与理由、以及**刻意保留的残留偏离**（后续切片接手）。

- issue：`LUM-1362`（M1-E，由 LUM-1361 立项，LUM-1368 / LUM-1371 cycle 补充实测）
- 分支：`feat/multica-rs-m1e-contract-gaps`
- 基线：`feat/multica-rs-initial` @ `69e9f4b`（本文写作时的 head）
- 合约基准：上游 `louloulin/multica`，`server/cmd/server/router.go`，`main` @ `f41fae6`
  （2610 行；抓取配方见 `docs/20-UPSTREAM-ANALYSIS-RECIPE.md`）
- 相关文档：`docs/05`（workspace/member）、`docs/06`（auth）、`docs/07`（invitation）、
  `docs/08`（PAT，本 issue 更正其中一处事实错误）、`docs/09` §9.5（验证门口径）

## 0. 缺口总表与处置

| # | 类别 | 路由 | 上游出处 | 处置 |
| --- | --- | --- | --- | --- |
| 1 | 缺失 | `PUT /api/workspaces/{id}` | `router.go:1699` | 补齐；与 PATCH 同指 `update_workspace` |
| 2 | 缺失 | `PATCH /api/workspaces/{id}/members/{memberId}` | `router.go:1703` | 补齐；handler `update_member` |
| 3 | 缺失 | `DELETE /api/workspaces/{id}/members/{memberId}` | `router.go:1704` | 补齐；handler `delete_member` |
| 4 | 偏离 | `/api/tokens`（GET / POST / POST `current/renew` / DELETE `{id}`） | `router.go:1879-1884` | 主路径迁到上游路径；`/api/me/pats` 降为 deprecated alias |
| 5 | 多出 | `POST /api/workspaces/{id}/invitations` | 上游同路径只有 GET（`1667`） | **删除**；创建邀请只走 `POST /members`（`1701`） |
| 6 | 多出 | `POST /api/auth/login`、`GET /api/auth/session` | 上游不存在 | **删除**（M1-D 已删 `/api/auth/logout` 占位） |
| 7 | 偏离 | `POST /api/auth/cli-token` → `POST /api/cli-token` | `router.go:1628` | 修正注册路径 + 源码注释 + 单测 URI |
| 8 | 文档 | `docs/08-M1-PAT.md:7-8` 写「上游没有显式 PAT REST endpoint」 | 实际上游有 `/api/tokens` | 更正；`pats.rs` 模块注释同一错误一并改 |

## 1. 缺失的三条路由

上游把 `PUT` 与 `PATCH` 挂在同一个 handler 上（`router.go:1699-1700` 都指向
`UpdateWorkspace`），member 的两条挂在 workspace 的 admin 组（`RequireWorkspaceRoleFromURL(owner, admin)`）。

本仓实现（`crates/mc-http/src/routes/workspaces.rs`）：

| Method | Path | Handler | 路由级 guard | handler 级校验 |
| --- | --- | --- | --- | --- |
| PUT | `/api/workspaces/:id` | `update_workspace` | `require_role(Owner\|Admin)` | — |
| PATCH | `/api/workspaces/:id/members/:memberId` | `update_member` | `require_role(Owner\|Admin)` | 目标/新角色为 owner 时仅 owner 可改（否则 403） |
| DELETE | `/api/workspaces/:id/members/:memberId` | `delete_member` | `require_role(Owner\|Admin)` | 目标是 owner 时仅 owner 可删（否则 403） |

上游没有 handler 级 owner 校验之外的额外语义，逐条对齐：

| 场景 | 上游 | 本仓 |
| --- | --- | --- |
| `memberId` 不存在 | 404 | 404（`MemberRepo::get` → `RepoError::NotFound`） |
| `memberId` 属于别的 workspace | 404（`workspace.go:579` 折叠，避免泄露存在性） | 404（`load_member_in_workspace` 显式比较 `workspace_id`） |
| `memberId` 不是 UUID | 404 | 400（`Id::parse` 失败 → `Validation`） |
| `role` 缺失 / 空串 | 400 `role is required` | 400 同文案 |
| `role` 非法（含 `guest`） | 400 `invalid member role` | 400 同文案（`normalize_member_role`） |
| 目标或新角色是 owner，requester 不是 owner | 403 | 403 |
| 降级/移除最后一个 owner | **400** `workspace must have at least one owner` | **409** 同文案（决策 D2） |
| 成功更新 | 200 + member+user | 200 + `MemberWithUser` |
| 成功移除 | 204 | 204 |

`memberId` 非 UUID 时本仓返回 400 而上游返回 404：上游用 `uuid.Parse` 失败后直接
`404 member not found`。这是刻意的（400 更能说明「请求本身错了」），列入残留偏离 R1。

## 2. 决策

### D1 — `PUT` 与 `PATCH` 共用一个 handler

上游两者语义完全一致（`UpdateWorkspaceParams` 用 `pgtype.Text.Valid` 区分「字段未提供」与
「显式置空」）。本仓 `WorkspaceUpdate` 的字段都是 `Option<T>`，同样是「只改出现的字段」，
故直接 `patch(update_workspace).put(update_workspace)`，不引入第二个 handler。
副作用：`PUT` 在本仓不是「整体替换」语义（无字段清空能力），与上游一致。

### D2 — 最后一个 owner 违规返回 **409** 而非上游的 400

`MemberRepo::update` / `delete`（LUM-1335 移植，`FOR UPDATE` + 其它 owner 计数）
返回 `RepoError::Conflict`，本仓把它映射为 409（`member_mutation_err`）：

- 409 才能区分「请求体不合法」（400，如 role 缺失/非法）与「请求合法但与当前状态冲突」；
- 这个映射在 M1-D 集成报告里已经作为既成事实记录（`docs/09` §9.3 第 2 条），
  本 issue 只是把它落到 HTTP 边界；
- issue 描述本身允许「违规返回 403 或 409」，故不构成违约。

客户端兼容性影响：上游客户端若按 400 分支处理会走到默认分支，但这是一条**错误路径**，
不影响正常流程。

### D3 — 删除 `POST /api/workspaces/{id}/invitations`

M1-C 曾在 GET 上再挂一个 `POST`（同一个 `create_invitation`），理由是「上游把
`POST /members` 实现为创建邀请」。**路由层的结论应与上游一致**：该 path 上游只有 GET
（`1667`），创建邀请走 `POST /members`（`1701`）。保留两个入口的代价是：

- 静态路由表对不上上游，任何基于路由表的一致性检查都会报「多出」；
- 未来若上游给 `POST /workspaces/{id}/invitations` 赋予别的语义（例如批量邀请），
  本仓会与上游**同一路径不同语义**地冲突。

故删除 `POST`，只保留 `GET`；`docs/07` 的路由表与「兼容决策」段同步更正。
`crates/mc-http/tests/invitations.rs` 的三处 POST 目标改为 `/api/workspaces/{ws}/members`。

### D4 — PAT 主路径迁到 `/api/tokens`，`/api/me/pats` 保留为 deprecated alias

issue 给了两个选项（删除 / 保留 alias），选**保留 alias**：

- M1 的 `pats.rs` 集成测试与 `docs/08` 的示例都用 `/api/me/pats`，直接删除会让
  `crates/mc-http/tests/pats.rs` 必须重写（本 issue 范围限制是「只补路由与必要的权限校验」）；
- alias 带 `Deprecation: true` + `Link: </api/tokens>; rel="successor-version"`
  （RFC 8594 / draft-dalal-deprecation-header），客户端能自动发现新路径。

alias 与主路径共用同一个 handler，因此在 alias 上创建的 PAT 与在主路径上创建的**完全等价**
（同一个 `PatStore`），不存在双写。移除时间点：`docs/08` 已标注「保留一个发布周期」。

注意上游 `r.Route("/api/tokens", …)` + `r.Get("/")` 的字面路径是 `/api/tokens/`（带尾斜杠），
但上游自己的两个客户端都请求**不带**尾斜杠的形式：

- CLI：`server/cmd/multica/cmd_auth.go:329` → `/api/tokens`
- daemon：`server/internal/daemon/client.go:704` → `/api/tokens/current/renew`

故本仓只注册无尾斜杠形式（axum 0.7 不做末尾斜杠归一化）。这是刻意的取舍：对齐**实际客户端**
而非 chi 的注册文本。列入残留偏离 R3。

### D5 — `POST /api/tokens/current/renew` 的鉴权只依赖 bearer PAT

上游该 handler 的调用者身份来自 auth 中间件（中间件解析 PAT → userID → 塞进 context），
handler 里再从 context 取 userID。本仓 M1 没有 PAT 消费中间件（那是 M3），
故 `renew_current_pat` 直接按 `sha256(raw)` 取行，用 **行里的 `user_id`** 作为身份：

- 不需要 `x-multica-user-id`（否则上游 daemon 的纯 bearer 调用会被 401 拒掉）；
- 语义等价：身份仍然只来自 bearer token 本身；
- 不轮换明文 token（上游刻意如此：CLI/daemon 多进程共享同一 PAT，轮换会同时打断所有进程）。

补充上游未覆盖的一处：上游由中间件拦掉**已过期** token，本仓没有该中间件，故 handler 内
显式检查 `expires_at <= now` → 401，避免把已过期的 PAT「复活」成 now+90d。

### D6 — 删除 M0 幽灵 auth 占位

`POST /api/auth/login`、`GET /api/auth/session` 是 M0 自造、返回 200 empty placeholder 的
「静默假成功」路由（上游只有 `/auth/{send-code,verify-code,google,logout}` `1472-1475`
与 `POST /api/auth/refresh` `1632`）。M1-D 已删 `/api/auth/logout` 占位，本 issue 删掉剩下两条
与「保留给 M2+ 实现」的错误注释。

### D7 — `cli-token` 路径修正

上游 `router.go:1628` = `r.Post("/api/cli-token", h.IssueCliToken)`，**没有** `/auth` 这一层。
本仓 M1-B 注册的是 `/api/auth/cli-token`（多了 `/auth`），属于「同功能不同路径」的静默偏离：
按上游路径调用的客户端会 404。已改为 `/api/cli-token`，并同步 `auth.rs` 内的注释与 3 处单测 URI。

M1-B 的语义替换（无 JWT 链 → 返回 30 天 TTL 的 PAT，落 `PatStore`，`scopes = ["cli"]`）
**保持不变**，见 `docs/06-M1-AUTH.md` 与 `docs/09` §9.3 第 3 条；本 issue 只改路径。

## 3. 验证

见 `docs/09` §9.5 的六条口径（本 issue 交付时全部绿）：

| 命令 | 说明 |
| --- | --- |
| `cargo build --workspace` | |
| `cargo clippy --workspace --all-targets -- -D warnings` | |
| `cargo clippy -p mc-http --all-targets --features test-util -- -D warnings` | 默认命令不覆盖 `tests/*.rs` |
| `cargo test --workspace` | |
| `cargo fmt --all --check` | |

> **fmt 门的一处基线欠账**：`69e9f4b` 自身 `cargo fmt --all --check` **即失败**（54 处 diff，全部在
> M2-C / LUM-1349 新并入的 5 个文件：`mc-http/src/routes/{inbox,subscribers}.rs`、
> `mc-http/tests/inbox.rs`、`mc-repos/src/{inbox,subscriber}.rs`）。本分支附了一个独立的
> `chore(fmt)` commit（纯 `cargo fmt --all`，零语义改动）以让该门转绿；M1-E 自己的文件本来就是
> fmt-clean 的（可在回归时用 `git diff --name-only 69e9f4b <feat-commit>` 复现）。
| `MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --features test-util -- --ignored` | PG e2e |

新增 e2e：`crates/mc-http/tests/contract_gaps.rs`

- 无 DB（`InMemoryPatStore`）：`/api/tokens` create→list→revoke、`expires_in_days` 兼容、
  `current/renew` 三种分支（窗口内延长 / 窗口外 no-op / 非 PAT 400 / 未知 token 401）、
  alias 的 `Deprecation` 头、三条幽灵占位都已 404；
- 需 DB（`#[ignore]` + `MULTICA_TEST_DATABASE_URL`）：`PUT` 更新成功 + member 403、
  `PATCH` 角色（含 `guest` 400、admin 提升 owner 403、未知 member 404）、
  `DELETE` 204 + 幂等 404、最后一个 owner 降级/移除 409。

## 4. 残留偏离（登记，不属本 issue 范围）

| 编号 | 偏离 | 影响 | 建议接手方 |
| --- | --- | --- | --- |
| R1 | 非 UUID 的 `memberId` 返回 400，上游 404 | 仅错误码差异 | 低优先级；若前端依赖 404 再改 |
| R2 | 上游 `expires_in_days` 的 `nil`/`<=0` = **永不过期**；本仓 `Pat.expires_at` 非 `Option`，退化为默认 30 天 | 「永不过期」无法表达 | PAT DB repo 切片（`personal_access_token` 表已有 `expires_at` 列） |
| R3 | 只注册无尾斜杠 `/api/tokens`，上游字面是 `/api/tokens/` | 若某客户端打尾斜杠会 404 | 需同时确认上游实际客户端行为（已确认两个都是无斜杠） |
| R4 | 创建响应字段：上游 `token_prefix`（前 12 字符）；本仓 `token_last4` + `display_token` | daemon 若读 `token_prefix` 会拿到空 | PAT DTO 对齐切片；需 `Pat` 增列存前缀 |
| R5 | 创建响应/请求的其他差异：本仓多 `scopes`，且 `expires_at` 始终存在 | 上游多发字段，本仓忽略；反之缺失字段为 `null` | 同上 |
| R6 | `PATCH` member 响应字段名（`name`/`email` 扁平 vs 上游 `user.name`） | 前端渲染差异 | member DTO 对齐切片 |
| R7 | PAT 仍存 `InMemoryPatStore`（重启丢失），上游落 `personal_access_token` 表 | 重启后 token 失效 | PAT DB repo 切片（迁移 `0003` 已有表） |
| R8 | PAT 的 daemon 消费中间件（`Authorization: Bearer` → session）未实现 | daemon 无法用 PAT 调本仓 | M3 |

## 5. 上游行号索引（本次用到的）

| 行 | 内容 |
| --- | --- |
| 1472-1475 | `POST /auth/{send-code,verify-code,google,logout}`（**无 `/api` 前缀**） |
| 1628 | `POST /api/cli-token` |
| 1632 | `POST /api/auth/refresh` |
| 1658-1659 | `GET`/`POST /api/workspaces` |
| 1664-1667 | `GET /api/workspaces/{id}`、`GET …/members`、`POST …/leave`、`GET …/invitations` |
| 1699-1700 | `PUT` + `PATCH /api/workspaces/{id}`（同指 `UpdateWorkspace`） |
| 1701 | `POST /api/workspaces/{id}/members` = `CreateInvitation` |
| 1703-1704 | `PATCH` / `DELETE /api/workspaces/{id}/members/{memberId}` |
| 1706 | `DELETE /api/workspaces/{id}/invitations/{invitationId}` |
| 1879-1884 | `/api/tokens` 组（GET / POST / POST `current/renew` / DELETE `{id}`） |
