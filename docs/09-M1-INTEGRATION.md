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

- 本机 `cargo 1.75 / rustc 1.75`（无 rustup，`rust-toolchain.toml` 不生效）；crates.io 索引在
  多 agent 并行时会抢 `/home/devbox/.cargo/.package-cache` 锁——集成任务应**独占跑**，
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
