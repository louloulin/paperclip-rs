# R753 — pc-issues execution_policy apply_to_row patch applicator tests

## 目标

`ApplyTransitionOutcome::apply_to_row` 与 `MonitorPatchOutcome::apply_to_row` 是 pc-issues 服务层把 transition patch 写回 `IssueRow` 的核心路径。

之前 pc-core 已有 85 个 transition 单测，但 `apply_to_row` 的字段映射行为没有 Rust 单测覆盖，存在以下风险：

- status / assignee 字段是否正确同步
- monitor_next_check_at 是否按 RFC3339 解析并写入 `Timestamp`
- 未知 patch key 是否被安全忽略
- 未触达字段是否保留原值

本轮对 `crates/pc-issues/src/execution_policy/types.rs` 增加 3 个 r753_ 前缀单测，覆盖以上路径。

## 实现

- 新增测试模块：crates/pc-issues/src/execution_policy/types.rs::apply_to_row_tests
- 引入 `pc_core::Timestamp` 与 `serde_json::json!`，构造最小 IssueRow fixture。

### 测试覆盖

1. `r753_apply_to_row_status_and_assignee_round_trip`
   - status = in_progress、assigneeAgentId、assigneeUserId 三键同时存在
   - 断言 status / 两个 assignee 字段都被刷新，未触达字段保持原值
2. `r753_apply_to_row_monitor_next_check_parses_iso_string`
   - monitorNextCheckAt = RFC3339 字符串、monitorNotes、monitorAttemptCount
   - 断言时间戳被正确解析成 `Timestamp`、note 写入、计数写入
3. `r753_apply_to_row_unknown_keys_are_ignored`
   - 同时包含 status 与一个不存在字段
   - 断言未知字段被忽略、status 被刷新、其他字段未被改动

## 验证结果

定向:

```
cargo test -p pc-issues execution_policy::types::apply_to_row_tests --lib
cargo test: 3 passed, 173 filtered out (1 suite, 0.00s)
```

pc-issues 全量:

```
cargo test -p pc-issues --lib
cargo test: 176 passed (1 suite, 0.01s)
```

## 关键决策

- 本轮只覆盖 apply_to_row 的字段映射，不修改 transition 行为，零 Node 业务逻辑变更。
- 使用 `pc_core::Timestamp::now()` / 显式 RFC3339 字符串，确保时间语义与 Node 端一致。

## 后续重点

- R754 — pc-routines::scheduler 调度计算补充测试
- R755 — pc-feedback::share / trace pure 补足
- UI mutation 冒烟：agent / routine / tool / environment
