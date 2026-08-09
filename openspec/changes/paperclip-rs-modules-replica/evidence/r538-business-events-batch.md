# R538 / M38 — 业务埋点批量接入（5 域）

## 本轮完成

在 5 个核心业务 route 加入真实 `global::track()`：

| 域 | 事件 | 位置 |
|---|---|---|
| agents | `agent.created` | `crates/pc-http/src/routes/agents.rs` |
| approvals | `approval.approved` | `crates/pc-http/src/routes/approvals.rs` |
| pipelines | `pipeline.created` | `crates/pc-http/src/routes/pipelines.rs` |
| pipelines | `pipeline.case.transitioned` | 同上 |
| routines | `routine.run.triggered` | `crates/pc-http/src/routes/routines.rs` |

`pc-telemetry::global` 内部将 `OnceLock` 替换为 `Mutex<Option<Arc<...>>>`，并增加 `install_for_tests()`：测试间可替换客户端；`track()` 改为同步入队，避免多测试共享全局时事件丢失。

## 验证

- `pc-telemetry --all-targets`：32/32（新增 1 个 m38 全事件验证测试）。
- `cargo check -p pc-http`：0 errors。
- `cargo check -p pc-server`：0 errors。
- 真实 HTTP collector 接收 5 类事件名称断言通过。

## 累计业务埋点（M37+M38）

- auth.signed_in
- company.created
- issue.created
- agent.created
- approval.approved
- pipeline.created
- pipeline.case.transitioned
- routine.run.triggered

合计 8 类事件；其他 `pipeline.* / approval.* / routine.*` 路径（pipeline.case.created / pipeline.case.claimed / pipeline.case.released / pipeline.archived / approval.rejected / approval.revision_requested / approval.comment_added / approval.resubmitted / routine.created / routine.updated）尚未补 `track()`，留待后续。
