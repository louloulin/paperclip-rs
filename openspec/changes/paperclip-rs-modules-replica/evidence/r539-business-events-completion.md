# R539 / M39 — 剩余业务埋点补完（11 类事件）

## 本轮完成

在 11 处业务路径成功处补 `global::track()`：

| 域 | 事件 | 位置 |
|---|---|---|
| approvals | `approval.created` | `crates/pc-http/src/routes/approvals.rs` |
| approvals | `approval.rejected` | 同上 |
| approvals | `approval.resubmitted` | 同上 |
| approvals | `approval.revision_requested` | 同上 |
| approvals | `approval.comment_added` | 同上 |
| pipelines | `pipeline.stage.created` | `crates/pc-http/src/routes/pipelines.rs` |
| pipelines | `pipeline.case.created` | 同上 |
| pipelines | `pipeline.case.claimed` | 同上 |
| pipelines | `pipeline.archived` | 同上 |
| routines | `routine.created` | `crates/pc-http/src/routes/routines.rs` |
| routines | `routine.updated` | 同上 |

## 修复

- `pipelines.rs`：`pipeline.archived` 的 `global::track()` 调用原本被错放在文件末尾（孤儿代码，编译失败）。修正到 `archive_pipeline` 函数体内、`LiveEvent::new("pipeline.archived", ...)` 发布之后。
- `routines.rs`：上一轮遗留的 `use pc_telemetry::global;` / `use std::collections::BTreeMap;` 两条孤儿导入位于文件末尾，移到顶部，与 track 调用对应。
- `approvals.rs`：同上，把两条孤儿导入移到顶部（顶部追加 `BTreeMap` + `global`）。

## 验证

- `cargo check -p pc-http`：0 errors。
- `cargo check -p pc-server`：0 errors。
- `cargo test -p pc-telemetry --all-targets -- --test-threads=1`：32/32。
- `cargo test --workspace --lib -- --test-threads=1`：4934/4934（40 suites）。
- `bash scripts/diff-routes.sh`：100.0% (node=581 rust=883 missing=0)。

## 累计业务埋点（M37+M38+M39 = 19 类）

| 域 | 事件 |
|---|---|
| auth | `auth.signed_in` |
| companies | `company.created` |
| issues | `issue.created` |
| agents | `agent.created` |
| approvals | `approval.created` |
| approvals | `approval.approved` |
| approvals | `approval.rejected` |
| approvals | `approval.resubmitted` |
| approvals | `approval.revision_requested` |
| approvals | `approval.comment_added` |
| pipelines | `pipeline.created` |
| pipelines | `pipeline.stage.created` |
| pipelines | `pipeline.case.created` |
| pipelines | `pipeline.case.transitioned` |
| pipelines | `pipeline.case.claimed` |
| pipelines | `pipeline.archived` |
| routines | `routine.created` |
| routines | `routine.updated` |
| routines | `routine.run.triggered` |

合计 19 类业务事件，全部在真实成功路径后调用 `pc_telemetry::global::track` 同步入队 → 周期 flush → 远端 endpoint（带 Retry-After 解析 + 字节分批 + RetryQueue 状态机）。

## 复刻完成度（持续追踪）

- Telemetry 子系统：Rust 端核心链路**完成**（M31-M39）。
- 路由表面：100% 覆盖（M30）。
- Adapter：仅 `claude-local` + `codex-local`（按用户指示，其他 deferred）。
- `pc-authz` 完整复刻：未启动（128 src vs Node 2187 行）。
- 远程 bridge IPC：未闭环（决策层 `pc-acpx::execution_target_decision` 完成）。
- 业务事件埋点：M39 后 19 类。
- UI 切流：已通；视觉与交互对齐仍非 100%。
