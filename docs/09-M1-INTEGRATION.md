# M1 集成计划：合并 m1a / m1b / m1c 三分支 + 对账 feat/multica-rs-m1

> 本文件是 LUM-1346（13:30 autopilot cycle）的交付物之一，作为 M1-D 集成任务的操作手册。
> 触发条件：LUM-1343（m1a）、LUM-1345（m1b）、LUM-1344（m1c）三个分支全部完成并推送后执行。

## 1. 当前实现状态快照（2026-09-22 13:40 CST）

| 分支 | issue | 状态 | 已改动（未提交 diff） | 范围 |
| --- | --- | --- | --- | --- |
| `feat/multica-rs-initial` | — | `056d2ae`（M0 + M1 scaffold），基线 | — | 16 crate + 22 表 + 4 mount 切片 |
| `feat/multica-rs-m1a-workspace-member` | LUM-1343 | running ~35min | `mc-repos/{workspace,member,user}.rs` +1091 行 | WorkspaceRepo / MemberRepo / UserRepo（进行中，routes 未开始） |
| `feat/multica-rs-m1b-auth` | LUM-1345 | running ~35min | 8 文件 +800 行 + 新增 `routes/auth.rs`、`docs/06` | VerificationCodeRepo / PatRepo + auth 路由 + session |
| `feat/multica-rs-m1c-invitation-pat` | LUM-1344 | running ~35min | 10 文件 +881 行 + 新增 `routes/{invitations,pats,auth_user}.rs`、`docs/07/08` | InvitationRepo + 邀请路由 + `/api/me/pats` |
| `feat/multica-rs-m1`（LUM-1335，平行实现） | LUM-1335 | in_review，已推送 `3ad402e` | 单 commit +5334 行 | 完整 M1 + **share-link + cli-token + `migrations/0002_auth_and_invitations.up.sql`**（三分支均未覆盖的部分） |

## 2. 仲裁决策（先读，冲突解决以本节为准）

1. **三分支（m1a/m1b/m1c）为 canonical M1**。`feat/multica-rs-m1` 是 LUM-1335 的单体平行实现，
   不整支合并——它的 `routes/{auth,workspace,member,invitation,pat}.rs` 与三分支同名文件
   内容与命名约定不同（单数 vs 复数文件名、header-based auth），整支合并会制造无法调和的冲突。
2. **从 `3ad402e` cherry-pick 三分支没有的增量**（集成任务的独立 commit）：
   - `migrations/0002_auth_and_invitations.up.sql`：`workspace_share_link` 表、
     `workspace_invitation` 的 `status / invitee_user_id / declined_at / revoked_at_explicit` 列、
     `personal_access_token.token_prefix` + 回填、verification_code 查询索引。
   - `crates/mc-repos/src/share_link.rs` + share-link 路由（三分支范围外，上游 M1 含它）。
   - `POST /api/auth/cli-token` handler。
   - `member` owner-safeguard（最后一个 owner 不可降级/移除）——若 m1a 已实现等价逻辑则跳过。
3. **`POST /api/workspaces/{id}/members` 语义**：上游是 `CreateInvitation`（管理员按 email 邀请），
   即 sub-issue C 的实现为准。sub-issue A 若实现了「直接 add existing user」，
   集成时删除该 handler 或迁移到非冲突路径（如 `PATCH /api/workspaces/{id}/members/{memberId}` 已覆盖的场景），并在 PR 描述里注明。
4. **`/api/me` 归 sub-issue A**（`routes/workspaces.rs`），B 的 `routes/auth.rs` 不得重复注册。
5. **session middleware**：A 先落占位（接受任意 session id）。集成时若 B 的 DB SessionStore
   已可用，则把占位收紧为「查 SessionStore → user_id」；否则保留占位并留 `TODO(LUM-xxxx)`。

## 3. 合并顺序与冲突热点

顺序：**A → B → C**（A 只动 `mc-repos` 三个文件，冲突面最小；B、C 重叠最多，最后合 C）。

| 文件 | 谁动 | 冲突概率 | 解决规则 |
| --- | --- | --- | --- |
| `crates/mc-config/src/lib.rs` `AuthConfig` struct | B（+3 字段）与 C（+1 字段）**同一插入点**（`require_email_verified` 之后） | 必冲突 | 四个字段全保留 |
| `Config::default()` 同一插入点 | B、C 都加默认值 | 必冲突 | 全保留 |
| env lookup（`MULTICA_AUTH_SESSION_TTL_SECS` 之后插入） | B、C 都在同位置插 | 必冲突 | 全保留 |
| `crates/mc-http/src/state.rs` `ConfigSnapshot` | B（`dev_mode/session_ttl_secs/send_code_per_email_per_min`）与 C（`invitation_per_workspace_per_hour`）都插在 `csrf_header` 之后 | 必冲突 | 全保留 |
| `apps/mc-server/src/main.rs` ConfigSnapshot 构造 | B、C 都在 `csrf_header` 行后加行 | 必冲突 | 全保留 |
| `crates/mc-db/src/pool.rs` | B 加 `connect_lazy`（普通 fn），C 加 `#[cfg(any(test, feature="test-util"))] placeholder()/from_pool` | 高 | 同一位置两个新 fn，都保留（注意 B 的 `connect_lazy` 是否也该 cfg 掉——按需收紧） |
| `crates/mc-http/src/routes/mod.rs` | B 加 `pub mod auth;`，C 加 `auth_user/invitations/pats` | 中（相邻插入） | 全保留，按字母序排 |
| `crates/mc-http/src/routes/mount.rs` | B 改 `mount_slice_auth` + `use super::auth`；C 改 `mount_slice_invitation/pat` + 顶部注释 | 低（不同 hunk） | 各取己方；`router()` 里的 `.merge` 行三方都没动 |
| `crates/mc-http/Cargo.toml`、`crates/mc-repos/Cargo.toml` | B、C 都加依赖 | 中 | 依赖并集 |
| `apps/mc-server` 其余 / `mc-db/Cargo.toml` | C 单独 | 无 | 直接合 |

### M0 占位路由与真实 handler 的重复注册（合并后必须处理）

`mount.rs::router()` 仍注册了一批 `health::placeholder` 占位路由。切片合入 axum 后，
同 path + 同 method 会出现**重复注册**（axum 0.7 行为需实测：panic 或后者覆盖）。
每合入一个切片，删除对应占位行：

- 合入 B：删 `/api/auth/logout` 占位（`login`/`session` 若 B 未实现则保留）。
- 合入 A：删 `/api/workspaces`、`/api/workspaces/{id}`、`/api/workspaces/{id}/members` 占位。
- 合入 C：如占位与邀请路由重叠则删。
- `/api/issues`、`/api/comments`、`/api/inbox`、`/api/agents` 等占位留到 M2 各切片落地时删。

## 4. 验证门（M1-D 的完成定义）

```bash
cd paperclip-rs
cargo build --workspace
cargo clippy --workspace --all-targets -- -- -D warnings
cargo test  --workspace
cargo fmt   --all --check
```

注意：

- **工具链（2026-09-22 15:30 实测更正）**：`PATH` 上的 `/usr/bin/cargo` 是 **1.75.0，无法构建本仓库**
  （workspace 声明 `rust-version = "1.80"`）。可用工具链是 rustup stable **1.98.1**，位于 `~/.cargo/bin`
  （`~/.cargo/bin/cargo --version` → `cargo 1.98.1`）。集成任务必须显式加 PATH：
  `export PATH="$HOME/.cargo/bin:$PATH"`（或 `PATH="$HOME/.cargo/bin:$PATH" cargo build --workspace`）。
- crates.io 索引在多 agent 并行时会抢 `/home/devbox/.cargo/.package-cache` 锁——集成任务应**独占跑**，
  避开其它 agent 的 cargo 进程（`ps aux | grep cargo` 确认空闲）。
- 仓库**没有提交 `Cargo.lock`**，而 CI 用 `--locked`。集成时提交一份 `Cargo.lock`，
  或在 CI 配置里去掉 `--locked`（二选一，建议前者）。
- sub-issue 描述里约定的测试名（如 `cargo test --package mc-repos --lib`）逐个跑并记录结果。

## 5. 合入后的收尾

1. push `feat/multica-rs-initial`（三分支 merge commit + cherry-pick commit + 占位清理 commit）。
2. 给 LUM-1334 / LUM-1342 回评：合并结果、仲裁决策落实情况（`/api/me` members 语义、
   session 收紧程度）、测试结果。
3. **然后才允许**执行第 6 节的 M2 anchor scaffold，再晋升 M2 子任务。

## 6. M2 anchor scaffold（集成任务的最后一步）

仿照 `056d2ae` 的做法，在 `feat/multica-rs-initial` 上做**一个**预扩展 commit，
让后续三个 M2 分支互不踩公共文件：

- `crates/mc-repos/src/lib.rs`：一次性声明 `pub mod issue;` / `pub mod comment;` /
  `pub mod inbox;`，每个配最小 stub 文件（`issue.rs` / `comment.rs` / `inbox.rs`）。
- `crates/mc-http/src/routes/mount.rs`：新增 `mount_slice_issue()` / `mount_slice_comment()` /
  `mount_slice_inbox()` 三个空切片 + `router()` 里对应 `.merge` 行。
- `migrations/`：追加下一个可用编号的迁移（如 `0003_reactions_and_subscribers.up.sql`），
  一次性建好 `comment_reaction`、`issue_reaction`、`issue_subscriber` 三张表
  （上游 026/027/015 对应，multica-rs `0001_init` 尚缺）——避免两个 M2 分支各自加同号迁移。
- **依赖前置**：`mc-repos/Cargo.toml` 若 M2 需要新依赖（如 `serde_json` 查询 filter），
  在本 scaffold commit 里一并加。

## 7. 实测增量（2026-09-22 15:30 CST，LUM-1358 cycle）

本节是集成任务开工前的最新实测情报（直接取自三个切片的活动工作树 + GitHub 分支），
与第 1 节 13:40 的历史快照冲突时**以本节为准**。

### 7.1 切片状态（实测）

| 切片 | 分支 / commit | 状态 | 证据 |
| --- | --- | --- | --- |
| C（LUM-1344） | `feat/multica-rs-m1c-invitation-pat` @ `d88b259` | **已交付并 push** | commit message + 工作树日志：`cargo build/test --workspace` 通过（39 suites/0 failed）；PG16 e2e invitations 3/3、pats 3/3、invitation repo 5/5 |
| B（LUM-1345） | `feat/multica-rs-m1b-auth` @ `89f5e94` + 未提交改动 | 运行中，仍在编辑（最后写盘 07:30 UTC） | 新增 `routes/auth.rs` 已落 5 条路由（send-code / verify-code / logout / refresh / `/api/me` 占位）+ `mc-repos/{pat,verification_code}.rs`、`docs/06-M1-AUTH.md` |
| A（LUM-1343） | `feat/multica-rs-m1a-workspace-member` @ `2c4da9f` + 未提交改动 | 运行中，**routes 仍未开始** | 已完成 `mc-repos/{workspace,member,user}.rs` + 新建 `crates/mc-http/src/middleware/`（session 中间件，对应仲裁 #5）；`crates/mc-http/src/routes/` 下无 workspaces.rs |

A 是 M1 关键路径：它的路由部分尚未落地，若下一 cycle 仍无进展，集成会卡在 A。

### 7.2 M0 基线 `056d2ae` 不编译——三个切片各自做了同构修复

`cargo build --workspace` 在 `056d2ae` 上直接失败（首个错误：`crates/mc-errors/src/lib.rs:163`
`E0433: cannot find module or crate \`anyhow\``）。三个切片**各自独立**修了同一批 M0 缺陷，
且修复内容逐字节相同：

| 文件 | 修复 |
| --- | --- |
| `crates/mc-errors/Cargo.toml` | `+ anyhow = { workspace = true }`（C 另加 `+ sqlx`） |
| `crates/mc-config/Cargo.toml` | `+ dirs` |
| `crates/mc-migrate/Cargo.toml` | `+ tokio` |
| `crates/mc-auth/Cargo.toml` | `+ mc-secrets` |
| `crates/mc-auth/src/container.rs` | `use crate::store::…` → `use mc_secrets::…` |
| `crates/mc-secrets/Cargo.toml` | `+ tempfile`、`+ async-trait`（`features` 同步加 `async_trait::async_trait` import） |
| `crates/mc-plugin-protocol/Cargo.toml`、`crates/mc-storage/src/lib.rs` | 同构小改（各 1 行） |

**规则：这些文件取任意一方即可**——三方 hunk 相同，属"假冲突"，不要逐 hunk 手工仲裁，
否则会把同一处修复叠成三份。

### 7.3 实测冲突矩阵（`git diff --name-only 056d2ae`，含工作树未提交改动）

- **三方共有（15 个）**：`apps/mc-server/src/main.rs`、`crates/mc-db/src/migrate.rs`、
  `crates/mc-http/src/state.rs`、`crates/mc-auth/src/container.rs`、`crates/mc-authz/src/lib.rs`、
  `crates/mc-realtime/src/lib.rs`、`crates/mc-storage/src/lib.rs`、`crates/mc-telemetry/src/redact.rs`、
  `crates/{mc-auth,mc-config,mc-errors,mc-http,mc-migrate,mc-plugin-protocol,mc-secrets}/Cargo.toml`。
- **A+C 共有（2 个）**：`crates/mc-http/src/lib.rs`、`crates/mc-http/src/middleware.rs`。
- **B+C 共有（5 个）**：`crates/mc-config/src/lib.rs`、`crates/mc-db/src/pool.rs`、
  `crates/mc-http/src/routes/{mod.rs,mount.rs}`、`crates/mc-repos/Cargo.toml`。
- **独有**：A = `mc-repos/{workspace,member,user}.rs` + `mc-http/src/middleware/`（新目录）；
  B = `routes/auth.rs`、`mc-repos/{pat,verification_code}.rs`、`docs/06`、`.cargo/config.toml`；
  C = `routes/{auth_user,invitations,pats}.rs`、`mc-repos/invitation.rs`、`mc-http/tests/{invitations,pats}.rs`、
  `mc-db/Cargo.toml`、`docs/07/08`。

**真正需要仲裁的分歧文件**（其余共有文件按 §2/§3 规则合并）：

| 文件 | 三方 diff 规模（+ / −） | 建议 |
| --- | --- | --- |
| `crates/mc-telemetry/src/redact.rs` | A 99/82、B 65/75、C 64/83 | `redact_str` 被三方各自重写为互不兼容的实现，**取一份**（推荐 C：其 workspace test 已全绿），不要三方合并 |
| `crates/mc-db/src/migrate.rs` | A 58/2、B 12/1、C 5/6 | A 改动最大，取 A 的结构为底，再补 B/C 的索引/查询 |
| `crates/mc-http/src/state.rs` + `apps/mc-server/src/main.rs` | B 26/1、C 5/1 | `ConfigSnapshot` 字段取并集（§3 已列为必冲突点） |
| `crates/mc-http/Cargo.toml` | C 追加最多 | 依赖并集；**必须保留 C 的 `test-util` feature 与 dev-deps**（`http-body-util`/`tower`/`hyper`/`pretty_assertions`），C 的 e2e 依赖它 |

### 7.4 axum 0.7 路由语法缺陷（集成后必须全仓扫一遍）

workspace 用 `axum = "0.7"`（matchit 0.7）：路径参数必须写成 `:id`，`{id}` 会被当作**字面量段**——
编译通过、注册成功，但请求恒返 404（C 在 e2e 里踩到并已全修）。

- **未修 · A 即将写的路由**：`/api/workspaces/{id}`、`/api/workspaces/{id}/members` 等仍会是 `{id}` 写法。
- **未修 · M0 遗留占位**：`crates/mc-http/src/routes/mount.rs` 的 `/api/workspaces/{id}`、`/api/issues/{id}`——
  M2 切片照抄就会复发。
- 集成后检查：

```bash
# 命中的应当是 format!/json! 字符串；若命中 .route("…{param}…") 即为缺陷
grep -rn '\.route(' crates/mc-http/src | grep -E '\{[a-zA-Z_]+\}'
```

### 7.5 其它集成注意

- B 新增仓库级 `.cargo/config.toml`（`incremental = true` / sparse registry / `git-fetch-with-cli` / musl static flags），**保留**。
- B 在 `crates/mc-http/src/routes/auth.rs:54` 注册了 `/api/me` 的 `me_placeholder`（代码注释已标注"由 sub-issue A 提供真实现"）。
  按 §2 仲裁 #4，集成时删除该占位、只留 A 的 handler，否则同 path 重复注册。
- `Cargo.lock` 三方都未提交（工作树里是新生成的未跟踪文件），按 §4 由 M1-D 统一生成并提交。

## 8. 交付收尾 + 路由重复注册实测（2026-09-22 16:00 CST，LUM-1359 cycle）

**本节取代第 7.1 节的"运行中"状态**：三个切片全部交付，运行已结束（`multica issue runs … --siblings --active`
实测并发位只剩协调 run）。

### 8.1 交付清单（GitHub 实测 head）

| 切片 | 分支 @ head | PR | 交付时间（UTC） |
| --- | --- | --- | --- |
| A（LUM-1343） | `feat/multica-rs-m1a-workspace-member` @ `f2e2e8b` | #3 | 07:53 |
| B（LUM-1345） | `feat/multica-rs-m1b-auth` @ `cce354c` | #2 | 07:46 |
| C（LUM-1344） | `feat/multica-rs-m1c-invitation-pat` @ `d88b259` | #1 | 07:33 |
| 基线 | `feat/multica-rs-initial` @ `7e26c07` | — | — |

- 三个 PR 的 base 都是 `feat/multica-rs-initial`。**若 M1-D 在本地完成合并，请把 #1 / #2 / #3 三个 PR 一起关闭**
  （原描述只提到 #1 冗余）；否则改为在 GitHub 上逐个 merge 三个 PR。
- 各切片相对 `056d2ae` 的改动面：A 29 文件、B 34 文件、C 34 文件。

### 8.2 axum 0.7.9 `.merge` 重复注册 = **panic**（实测，非推断）

在 workspace 锁定的同一版本组合（`axum 0.7.9` + `matchit 0.7.3`）上跑最小复现：

| 场景 | 结果 |
| --- | --- |
| 两个 Router 各自 `.route("/api/me", get(..))` 后 `.merge` | **panic**：`Overlapping method route. Handler for \`GET /api/me\` already exists`（`routing/path_router.rs:70`） |
| 同 path **不同** method（A 的 `GET /api/workspaces/:id/members` + C 的 `POST` 同 path） | 合并成功 |
| 字面量 `/api/workspaces/{id}` 与参数 `/api/workspaces/:id` 共存 | 合并成功（字面量段永远匹配不上 UUID → 静默 404，不 panic） |

**结论**：M0 占位行里只有"同 path + 同 method"的那几条会炸；`{id}` 字面量占位不会炸但也不通。
A 的 `routes/workspaces.rs` 已在注释里记录了同一结论（同一 path+method 重复注册会 panic）。

### 8.3 合并后**必须**删除的占位行（否则 `mc_http::router()` 构造即 panic）

| 占位行（`mount.rs::router()`） | 与谁冲突 | 谁已删 | 处理 |
| --- | --- | --- | --- |
| `GET /api/me`（B 的 `routes/auth.rs:54` `me_placeholder`） | A 的真实 `/api/me` | 未删 | **删 B 的占位行**（§2 仲裁 #4） |
| `GET+POST /api/workspaces` | A 的真实 `get(list_my_workspaces).post(create_workspace)` | **A 已删** | 合并 B/C 时**不要把它加回来** |
| `GET /api/workspaces/{id}`、`GET /api/workspaces/{id}/members` | 与 A 的 `:id` 形式不冲突（字面量段） | **A 已删** | 同上：保留 A 的删除结果 |
| `POST /api/auth/logout` | B 的真实路由是 **`/auth/logout`**（不同 path） | 三方都未删 | 不 panic；上游也无 `/api/auth/*`（除 refresh），可留可删（留 = 404 之外多一个 stub） |

**合并顺序建议不变（A → B → C），但 `mount.rs::router()` 的最终形态应以 A 的版本为底**：
A 已经把 `/api/workspaces` 系列的 M0 占位行连同注释一起清掉了，B/C 版本里那几行是**回退**，
逐 hunk 仲裁反而会把它们留下。检查手段（合并后、跑测试前）：

```bash
# 期望输出为空：真实切片已覆盖却仍保留的同 path+method 占位
sed -n '/fn router/,/^}/p' crates/mc-http/src/routes/mount.rs \
  | grep -nE 'health::placeholder'   # 对照各 slice 的 .route("…") 清单人工核
```

### 8.4 B 的路由前缀是对的，别"顺手"改

上游 `server/cmd/server/router.go` 实测：

| 上游行号 | 路径 |
| --- | --- |
| 1472–1475 | `POST /auth/send-code`、`/auth/verify-code`、`/auth/google`、`/auth/logout`（**无 `/api` 前缀**） |
| 1615–1616 | `GET` / `PATCH /api/me` |
| 1632 | `POST /api/auth/refresh`（有 `/api`） |
| 1657 | `r.Route("/api/workspaces", …)` |

B 的 `routes/auth.rs` 与上游逐条一致（`/auth/*` 三条 + `/api/auth/refresh` + `/api/me` 占位），
M1-B 的 issue 描述里也是这么写的 → **不要**在集成时把它们"统一"成 `/api/auth/send-code`。
M0 的 `/api/auth/login` / `/api/auth/session` 占位在上游并不存在（M0 自造），可保留到 M2+ 再清理。

### 8.5 切片互不冲突的真实路由面（供合并后核对）

- A：`GET+POST /api/workspaces`、`GET+PATCH /api/me`、`GET /api/workspaces/:id`、
  `PATCH`/`DELETE /api/workspaces/:id`、`POST /api/workspaces/:id/leave`、`GET /api/workspaces/:id/members`
- B：`POST /auth/{send-code,verify-code,logout}`、`POST /api/auth/refresh`、`GET /api/me`（占位→删）
- C：`POST /api/workspaces/:id/members`（create invitation）、`GET /api/invitations`、
  `GET /api/invitations/:id`、`POST /api/invitations/:id/{accept,decline}`、
  `GET+POST /api/me/pats`、`DELETE /api/me/pats/:id`
- 交叉点只有两处，都无同 path+method 冲突：`/api/me`（删 B 占位）、
  `/api/workspaces/:id/members`（A 的 GET + C 的 POST，实测可合并）。
