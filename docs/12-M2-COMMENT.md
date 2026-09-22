# M2-B：comment Repo + 评论线程 / resolve / reactions 路由

> 对应任务：**LUM-1350**（`M2-B: comment Repo + /api/issues/{id}/comments + comment/{id} 线程/resolve/reactions`）
> 分支：`feat/multica-rs-m2b-comment` → 合并目标 `feat/multica-rs-initial`
> 基线：`fd6dfd6`（M1-D 集成 + M2 anchor scaffold `4aa275a`）
> 计划：`docs/10-M2-PLAN.md` §2 M2-B

## 1. 范围

| 层 | 文件 | 内容 |
|---|---|---|
| Repo | `crates/mc-repos/src/comment.rs` | `CommentRepo` 全量 CRUD、线程分页、软删可见性、resolve/unresolve、评论级 reaction |
| 路由 | `crates/mc-http/src/routes/comments.rs` | `comments::router()`，由 scaffold 的 `mount_slice_comment()` 挂载 |
| 测试 | `crates/mc-repos/src/comment.rs`（内嵌）+ `crates/mc-http/tests/comments.rs` | repo 单测 4 + repo DB 测 8（`#[ignore]`）；路由单测 7 + e2e 6（`#[ignore]`） |

**没碰**的文件（公共锚点，M2 三切片并行）：`mount.rs`、`routes/mod.rs`、`state.rs`、`main.rs`、
各 `Cargo.toml`、`migrations/*`、`mc-repos/src/lib.rs`。`mount_slice_comment()` 与
`pub mod comment;` / `0004_reactions_and_subscribers.up.sql` 全部由 M1-D 的 scaffold 提供。

> ⚠️ **W0-B2（LUM-1387）之后**：`migrations/0001`–`0004` 已退役，本片的 repo 代码改到上游列上——
> `comment.body` → 上游 `content`（读出仍叫 `body`）、`author_id`/`actor_id` 上游是 `uuid`（读出 `::text` 别名，写入 `$n::uuid`），
> 见 `docs/26-W0-SCHEMA-SWITCHOVER.md` §5。HTTP 面与 `CommentRepo` 的签名未变。

## 2. 路由表（与上游 1:1）

上游记法里的 `{id}` 落到 axum 0.7 的 `.route()` 时**必须写成 `:id`**（`{id}` 会被 matchit 当字面量段，恒 404）。

| method | path | 上游 handler | 本仓状态 |
|---|---|---|---|
| GET | `/api/issues/:id/comments` | `ListComments` | ✅ |
| POST | `/api/issues/:id/comments` | `CreateComment` | ✅ 201 |
| PUT | `/api/comments/:commentId` | `UpdateComment` | ✅ 200 |
| DELETE | `/api/comments/:commentId` | `DeleteComment` | ✅ 204 |
| DELETE | `/api/comments/:commentId/keep-replies` | `DeleteComment`（同一 handler） | ✅ 204 |
| POST | `/api/comments/:commentId/resolve` | `ResolveComment` | ✅ 200 |
| DELETE | `/api/comments/:commentId/resolve` | `UnresolveComment` | ✅ 200 |
| POST | `/api/comments/:commentId/reactions` | `AddReaction`（comment 分支） | ✅ 201 |
| DELETE | `/api/comments/:commentId/reactions` | `RemoveReaction`（comment 分支） | ✅ 204 |
| POST | `/api/comments/:commentId/sub-issues` | `CreateCommentSubIssue` | ⚠️ **501**（见 §6） |

未实现的**裸路径** `/api/comments`（M0 占位）已由 M1-D 删除；上游也没有这条，故不再注册。

### 查询串（`GET /api/issues/:id/comments`）

| 参数 | 支持 | 语义 |
|---|---|---|
| `limit` | ✅ | 1..=`COMMENT_MAX_LIMIT`(200)，默认 50；超限/非法 → 400 |
| `before` + `before_id` | ✅ | 游标翻页；**必须成对出现**，否则 400。`before` 为 RFC3339 |
| `since` | ✅ | 只返回 `created_at >= since` |
| `thread` | ✅ | 只看该线程（根 + 后代）；非法 uuid → 400 |
| `roots_only` | ✅ | 只返回根评论 |
| `recent` / `tail` / `summary` / `fold` | ❌ **显式 400** | 见 §5 |
| 连字符别名 | ✅ | `before-id` / `roots-only` 与下划线写法等价（兼容上游客户端） |

布尔参数按**严格**布尔解析（`true`/`false`/`1`/`0` 大小写不敏感），`?roots_only=1` 这类上游严格模式会给 400 的写法**同样 400**，而不是被静默当成真。

翻页响应头（仅当 `has_more`）：`X-Multica-Next-Before` / `X-Multica-Next-Before-Id`。

> ⚠️ **游标的时间戳用 `Z` 后缀**（`to_rfc3339_opts(SecondsFormat::Micros, true)`），
> 与上游 Go `RFC3339Nano` 对 UTC 的输出一致。`+00:00` 形式在 query 里 `+` 会被解码成空格
> → 客户端把 header 直接拼回 `?before=` 会 400（e2e `list_pagination_sets_next_cursor_headers` 断言了这一点）。

## 3. Repo API（`CommentRepo`）

```rust
CommentRepo::new(&Db) / with_pool(PgPool)
create(NewComment)                        -> CommentRow          // 事务：插评论 + bump issue.revision/last_activity_at
get(Id) / get_in_workspace(Id, Id)        -> CommentRow
list_for_issue(CommentFilter)             -> CommentList { comments, has_more }
update(Id, CommentPatch)                  -> CommentRow          // expected_revision 乐观锁
soft_delete(Id, keep_replies: bool)       -> ()
resolve(Id) / unresolve(Id)               -> CommentRow          // 幂等
add_reaction(comment_id, actor_type, actor_id, emoji, workspace_id) -> (CommentReactionRow, bool /*inserted*/)
remove_reaction(...)                      -> bool                // 是否真的删掉
list_reactions(&[Id])                     -> Vec<CommentReactionRow>
```

- `NewComment` 的 `parent_id` 必须属于**同一 issue 且未软删**，否则 `Validation`（→400）。
- 软删 / 恢复 / 反应都以 `workspace_id` 做**租户内**约束（`get_in_workspace`、`remove_reaction` 校验 workspace）。
- revision：`create` = 1；`update`/`resolve`/`unresolve` 自增；reaction **首次**插入 bump 评论 revision（上游行为），重复插入是 no-op。

## 4. 软删可见性规则（本仓唯一实质性的语义自定）

上游做法：`TombstoneComment`（有回复时留 tombstone，无回复则物理删）+ `DeleteReplylessCommentTombstone`
祖先剪枝（`commentTombstonePruneDepth=256`）。本仓 0001 的 `comment` 表只有 `deleted_at`（无 tombstone 标记列），
所以用**等价的一条可见性规则**替代物理删除：

> 一条软删评论在默认列表里**可见**，当且仅当它仍存在**活的传递后代**（`LIVE_DESCENDANT_EXISTS` 递归 CTE）；
> 内容清空为 `""`、`deleted_at` 非空，作为线程锚点保留。整条线程死光后，根与全部 tombstone 一起从默认列表消失。
> `filter.include_deleted = true` 返回全部（审计/折叠需要）。

`soft_delete(id, keep_replies)`：
- `keep_replies = true` → 只软删自身（HTTP 的 `DELETE /` 与 `DELETE /keep-replies` 都走这条，**两者等价**——上游 `router.go:2169` 把两条路径接给同一个 handler）。
- `keep_replies = false` → 一并软删全部后代。
  **HTTP 面不可达**：上游没有"级联删回复"的路径，本仓不额外发明一条；该能力只保留给 M3 的物理 purge / 管理操作。

实测 `EXPLAIN` 显示递归 `EXISTS` 走 `comment_parent_idx`；`include_deleted=true` 时退化为普通扫描。

## 5. 偏离清单（与上游不同、或有意不实现）

| 项 | 处理 | 理由 |
|---|---|---|
| `summary` / `fold` / `recent` / `tail` | **400** `query parameter X is not implemented yet (M2-B slice)` | 上游的线程折叠策略（`completeCommentThreads` / fold）在 Go 侧实现，端口未做。显式 400 比静默降级安全（客户端会立刻发现而不是拿到"少了回复"的列表） |
| `reactions.comment_revision` | 不返回（DTO 无此字段） | 列不存在；reaction 幂等语义已由 revision 自增覆盖 |
| `CommentDto.type` | 恒 `"comment"`，字段名 `type` | 上游 `clientAuthorableCommentTypes = {comment, progress_update}`；本仓 `comment` 表**无 `type` 列**，请求体传 `progress_update` → 400 `comment type 'progress_update' is not supported yet (comment.type column TODO)`，未知名 → 400 `invalid comment type` |
| `CommentDto.attachments` | 恒 `[]` | 附件表属 M2+ / 独立切片；字段保留以免上游客户端解析失败 |
| `resolved_by_type` / `resolved_by_id` / `quick_action_id` / `issue_revision` | 不返回 | 列不存在（`resolved_at` 有） |
| 单线程唯一 resolve（上游 `ClearOtherThreadResolutions`） | 未做（TODO） | 本仓只在自身行上写 `resolved_at`；跨评论的不变量需要额外事务扫描，留到 M2 集成后与上游对齐 |
| `trigger-preview` / `POST /api/comments/:commentId/trigger` | 不实现 | 属 agent 派单（`triggerTasksForComment` 全套），依赖 M3 task queue |
| mention 触发 agent 派单、`suppress_agent_ids` | 不实现（接住但忽略） | 同上，M3 |
| `sub-issue-preview` | 不实现 | 上游 human-only 端点 |
| `POST /:commentId/sub-issues` | **501 + 代码 TODO** | M2-A 已并入（`IssueRepo` 可用），真正阻塞是 `mc-source-context` crate 本仓不存在 → 集成时按仲裁**维持 501**（见 §6.1） |

请求体里出现 DTO 未声明的字段（如 `suppress_agent_ids`）不会 400——上游客户端可能带，忽略即可；**已知会改变语义**的字段才显式拒绝（`type`）。

### 鉴权与错误码

| 场景 | 状态码 |
|---|---|
| 缺 `X-Multica-User-Id` / 非法 uuid | 401（`AuthUser` 提取器） |
| 非 workspace 成员（评论/issue 域） | **404**（不泄露跨租户存在性） |
| 评论编辑 / 删除非作者且非 `owner`/`admin` | **403**（`only comment author or admin can edit` / `...delete`） |
| `guest` 角色 | 不算 admin → 403 |
| `expected_revision` 不匹配 | 409 |
| 目标不存在 / 已 tombstone 再操作 | 404 |
| 空内容（NUL 剥离后）、非法 uuid、严格布尔失败、未实现参数 | 400 |
| reaction 重复添加 | 201（幂等，返回同一行） |
| `remove_reaction` 不存在 | 204（幂等） |

`resolve` / `unresolve` / reactions **不校验作者**（任意 workspace 成员可用），与上游一致。

`content` 落库前剥离 NUL 字节（上游 `CreateComment` 同样 sanitize），随后要求非空——**只含 NUL 的评论**会得到 400 而不是存进一个空 body。

## 6. `POST /api/comments/:commentId/sub-issues` = 501（M2 集成后**维持**）

上游 `CreateCommentSubIssue` 的完整流程：`ParseSourceContextToken` → 校验 token 里的 issue/revision digest
（不匹配 → 409 `source_context_changed`）→ 按 `mode`（`manual` / `agent`）建子 issue 并回写来源。

M2-B 开工时 M2-A（LUM-1348）尚未合并，`IssueRepo` 不存在，所以本切片：

1. **鉴权边界不放松**：先做 `AuthUser` + workspace 成员校验（非成员 404），再返回 501；
2. 响应体：`{ code: "not_implemented", message: ..., todo: ... }`；
3. 代码内 TODO 注释 + 本 issue 评论里给出**期望的 `IssueRepo` 接口签名**，供 M2 集成时对齐。

### 6.1 M2 集成仲裁（LUM-1354，2026-09-22）：**维持 501，不在集成切片里实现**

M2-A 已并入 `feat/multica-rs-initial`（`IssueRepo::create` / `has_ancestor` 等可用），但是**真正的阻塞不在 M2-A**：

- 上游的 `ParseSourceContextToken` / `BuildSourceContext` 属于 `mc-source-context`——`docs/01-PLAN.md:162`
  计划中的 crate，**本仓 `crates/` 下不存在**（集成时实测：全仓 `grep -rn "ParseSourceContextToken" crates/`
  只命中本文件对应 handler 的 TODO 注释）。`issue.source_context_id` 列在 `0001` 里存在，
  但 token 的签发 / 解析 / digest 比对整套缺失。
- 该 token 流程是**独立子系统**（含 `source_context_changed` 409 语义与 `createCommentSubIssueRequest`
  请求体 schema），既不在 M2-A/B/C 任一切片的文件面内，也不是“接线”能补齐的。

因此 M2 集成按 `docs/10-M2-PLAN.md` §2 仲裁给出的第二选项（“替换为真实实现**或**明确记录到后续 issue”）执行后者：

- **维持 501**（`code: not_implemented`）；鉴权 + workspace 成员校验 + 404 语义保持在现有实现上，不放松；
- 遗留项登记为**后续切片**：`mc-source-context` crate + `POST /api/comments/{commentId}/sub-issues` 真实实现
  （含上游 `source_context.go` 的请求体 schema 与 409 语义）。集成切片**不**凭空设计该子系统
  （`docs/21-M2-INTEGRATION-RECIPE.md` §10.3 有同一结论与证据命令）。
- 本节原先那句“集成时需要做的：用 `IssueRepo` 建子 issue……”已被本次仲裁取代：`IssueRepo` 就绪
  但 token 子系统未就绪，所以该路由**不是**一条接线任务。

## 7. 测试

仓库测试（`crates/mc-repos/src/comment.rs`）：

- 单测（无需 DB）：`parse_author_type_covers_schema_domain`、`row_accessors_and_flags`、`filter_defaults_and_limit_clamp`、`reaction_row_accessors`
- 路由单测（`cargo test -p mc-http --lib`）：`list_query_defaults_are_empty`、`list_query_accepts_hyphen_aliases`、`list_query_rejects_bad_input`、`list_query_rejects_unimplemented_modes`、`content_sanitization`、`comment_type_gate`、`author_or_admin_gate`
- DB（`#[ignore]`）：`db_create_list_and_thread_assembly`、`db_list_cursor_pagination_reports_has_more`、
  `db_list_since_filters_and_comment_bumps_issue_activity`、`db_update_revision_conflict_and_tombstone_not_editable`、
  `db_soft_delete_visibility_and_keep_replies`、`db_cascade_soft_delete_hides_whole_thread`、
  `db_resolve_unresolve_is_idempotent`、`db_reaction_add_remove_is_idempotent`

路由测试（`crates/mc-http/tests/comments.rs`，`#[ignore]`，需 `--features test-util`）：

- `create_list_and_thread_reads`：建根 + 回复；默认列表 / `roots_only` / `thread` 三种读法；
  未实现参数与坏输入 400；空内容 400；坏 parent 400；建评论 bump issue revision
- `update_permissions_and_revision_conflict`：admin 改他人评论 200、作者改 200 + revision 自增、
  过期 `expected_revision` 409、非作者非 admin 成员 403（改 + 删）、非成员 404、无 header 401、
  作者删除 204 → 重复删除 404 → 列表为空
- `resolve_and_reactions`：resolve/unresolve 幂等（不重复推进 revision）、reaction 加/删幂等（201/204）、空 emoji 400
- `delete_keeps_replies_visible`：`DELETE /` 后 tombstone 当线程锚点、回复仍可读；整条线程死光后消失；`/keep-replies` 与 `/` 同一 handler
- `comment_sub_issue_is_not_implemented`：501 + `code=not_implemented`；非成员先 404
- `list_pagination_sets_next_cursor_headers`：`limit=1` 返回最新根 + 游标响应头（且 header 是 URL-safe 的 `Z` 形式），用游标翻到更早的根

本地跑法（本机已备 PG16，库 `multica_test`，账号 `multica/multica`）：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export MULTICA_TEST_DATABASE_URL='postgres://multica:multica@127.0.0.1:5432/multica_test'
cargo test -p mc-repos -- --ignored
cargo test -p mc-http --features test-util -- --ignored
```

> 不设 `MULTICA_TEST_DATABASE_URL` 时 `#[ignore]` 的 DB 测试**静默跳过**——"全绿"可能是空跑。
> `multica_test` 在本次开工时只有 `0001` 的表，本切片已就地补应用 `0002`/`0003`/`0004`
> （0004 是 `comment_reaction` 等三张 M2 表的来源；三张表是 M2-A/B/C 共同前置）。

## 8. 验证门（2026-09-22，本机 rustup stable 1.98.1，PG16）

| # | 命令 | 结果 |
|---|---|---|
| 1 | `cargo build --workspace` | ✅ exit 0 |
| 2 | `cargo clippy --workspace --all-targets -- -D warnings` | ✅ 0 warning |
| 3 | `cargo clippy -p mc-http --all-targets --features test-util -- -D warnings` | ✅ 0 warning |
| 4 | `cargo test --workspace` | ✅ 全绿（无 failed；`mc-repos` 52 项中 24 ignored，含本切片 8 项 DB 测） |
| 5 | `cargo fmt --all -- --check` | ✅ exit 0 |
| 6 | `cargo test -p mc-http --features test-util -- --ignored`（真库） | ✅ 12 passed / 0 failed（含本切片 6 项 e2e） |
| 6b | `cargo test -p mc-repos -- --ignored`（真库，本切片证据） | ✅ 24 passed / 0 failed（含本切片 8 项） |

MSRV 1.80 注意：不要采纳 clippy 1.98 建议的 `Option::is_none_or` / `Duration::from_mins`（超出 MSRV），
等价写法是 `!x.is_some_and(..)` / `Duration::from_secs`。
