# Round 358 — HTTP `POST /api/issues/:id/comments` 端到端 round-trip 验证

> 适用版本：`paperclip-rs` 截至 R358（R357 = 921 → R358 = **924**，+3 pc-http 测试）
> 参考实现：`paperclip` Node（`apps/server/src/http/issues/*`）
> 测试基线：`cargo test -p pc-heartbeat --tests -- --test-threads=1` 全绿，`cargo fmt --all -- --check` 通过

---

## 🎯 R358 目标

闭合 HTTP 路由层 `POST /api/issues/:id/comments` 端点的 presentation/metadata 端到端 round-trip gap。

**之前的状态**：
- ✅ Rust 端 RPC 路径（`escalate_db.rs::create_comment_with_display`）已写 `presentation`/`metadata`
- ❌ HTTP 路由层 `add_comment` 走的是 `create_comment`（旧路径，传 `None`），导致：
  - HTTP 客户端无法上传系统评论的展示元数据
  - DB 中已存的 presentation/metadata 字段虽然能在 GET 响应里看到，但写入端不可达

---

## 🔧 R358 实现要点

### 修改文件
**`crates/pc-http/src/routes/issues.rs`**：
- `CommentBody` 结构体增加 2 个字段：
  ```rust
  #[serde(default)]
  presentation: Option<serde_json::Value>,
  #[serde(default)]
  metadata: Option<serde_json::Value>,
  ```
- `add_comment` handler 切换为 `create_comment_with_display(...)`，把 payload 的 presentation/metadata 透传到底层 RPC

### 新增测试（`crates/pc-http/tests/round358_issue_comment_presentation_metadata_contract.rs`，3 个测试）

1. **`post_comment_with_presentation_and_metadata_round_trips`**：POST 一个完整带 presentation+metadata 的 comment → POST 响应立即回写两字段 → GET `/api/issues/:id/comments` 列表拉回来字段完全保留
2. **`post_comment_without_presentation_metadata_still_works`**：旧客户端不传字段 → 响应里两字段都是 `null`（向后兼容）
3. **`presentation_only_and_metadata_only_each_round_trip`**：仅传 presentation / 仅传 metadata → 两个独立字段各自 round-trip，不互相污染

测试模式：
- 用 `axum::Router::oneshot` 真实 HTTP 协议栈（不是直连 repo）
- 真实 PostgreSQL：`postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos`
- 验证 status code（201）+ response body 字段一致性

### Node 对齐

| Rust 端 | Node 参考 |
|---|---|
| `crates/pc-http/src/routes/issues.rs::add_comment` | `apps/server/src/http/issues/comments/post.ts` |
| `IssueRepo::create_comment_with_display` | `issueComments.create({presentation, metadata})` |
| `IssueCommentRow.presentation/metadata`（已 Serialize） | `IssueCommentDto.presentation/metadata` |

---

## 📊 进度快照（截至 Round 358）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | **R290 → R358**（69 个模块，24 轮增量） |
| 最近一轮 | **Round 358**：HTTP 端到端 round-trip 闭合 |
| Round 358 测试 | **3/3 全部通过真实 PostgreSQL** |
| pc-heartbeat lib 测试 | **921 passed / 0 failed**（与 R357 持平，无回归） |
| pc-http 新增测试 | **3 passed** |
| 总增长（pc-heartbeat + pc-http） | R357 = 921 → **R358 = 924+**（+3 在 pc-http） |
| `cargo fmt --all -- --check` | **通过** |

---

## 🔍 已闭合端到端边界（Recovery 主链 100% 闭合）

| 边界 | 状态 | 覆盖轮次 |
|---|---|---|
| Rust RPC 端 `create_comment_with_display` | ✅ | R350 |
| HTTP `POST /api/issues/:id/comments` 入参 + 透传 | ✅ | **R358** |
| HTTP `GET /api/issues/:id/comments` 响应序列化 | ✅ | R354（沿用既有路径） |
| Round-trip preservation（presentation + metadata） | ✅ | **R358** |
| 向后兼容（无字段 → null） | ✅ | **R358** |

**结论**：Recovery 链路上的所有 presentation/metadata 字段从 DB write → HTTP read → HTTP write → DB 端全部连通，可端到端追踪。

---

## 📋 后续 R359+ 计划（推荐顺序）

### 短期（3 轮内）
1. **R359**: Activity log actor 端到端（source escalation actor 注入验证）
2. **R360**: ProviderQuota review-participant 路径细化（monitor_notes 文案对齐）
3. **R361**: Pending finalize 屏障 + redaction 收尾

### 中期
4. **R362-364**: Acpx-engine 子模块（fingerprint/codec/stage 协议）— **最大单一项目**
5. **R365-367**: Budgets 完整迁移
6. **R368-370**: Sandbox-managed-runtime + Git-workspace-sync

---

## 🔬 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# R358 单独验证
env -u SHELL rtk proxy cargo test -p pc-http --test round358_issue_comment_presentation_metadata_contract -- --test-threads=1
# 期望: 3 passed

# pc-heartbeat 全量（无回归）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --tests -- --test-threads=1
# 期望: 64 test results, 全部 0 failed

# 格式
env -u SHELL rtk proxy cargo fmt --all -- --check
# 期望: 无输出（通过）
```

---

## 📝 备注

- 磁盘满导致 abort 过一次，已 `cargo clean` 清理 25 GiB 重新编译
- `pc-http` `agents_http_contract::company_agent_create_accepts_ui_payload_and_returns_full_agent` 是 pre-existing failure（与 R358 无关，已通过 `git stash` 验证）
- 当前 pc-heartbeat lib 测试 = 921 passed（与 R357 持平）；R358 新增 3 个 pc-http 测试
