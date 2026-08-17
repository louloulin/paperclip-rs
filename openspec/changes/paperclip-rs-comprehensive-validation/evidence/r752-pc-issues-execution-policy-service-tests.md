# R752 — pc-issues execution policy service tests gap closing

## 目标

pc-issues::execution_policy::IssueExecutionPolicyService 是 Node issue-execution-policy.ts 复刻的业务服务层。

此前 pc-core 提供 85 个纯函数测试，service 层的 hook 生命周期、monitor patch 与 invalid clear reason 路径缺乏服务级回归。

本轮对 service.rs 增加 3 个 r752_ 前缀的 tokio 单元测试，并把 hook event 升级为 PartialEq + Eq，确保未来 service 行为变动能被立即捕获。

## 实现

- 新增测试模块：crates/pc-issues/src/execution_policy/service.rs::service_tests
- hook event 补 PartialEq + Eq：crates/pc-issues/src/execution_policy/hook.rs

### 测试覆盖

1. r752_apply_transition_records_hook_lifecycle
   - 调用 apply_transition 并使用 RecordingIssueExecutionPolicyHook
   - 断言 BeforeTransition / AfterTransition 事件顺序，event 携带 issue_id 与 patch_size
2. r752_monitor_only_marks_outcome_as_monitor_only
   - 调用 apply_monitor_only
   - 断言 outcome.monitor_only == true 且 AfterTransition 事件的 has_decision == false
3. r752_invalid_monitor_clear_reason_is_rejected
   - 使用非法字符串触发 clear_monitor
   - 断言返回 IssueExecutionPolicyError::Validation 且 BeforeMonitorChange 事件以 "clear" 记录

## 验证结果

```
cargo test -p pc-issues execution_policy::service::service_tests --lib
cargo test: 3 passed, 170 filtered out (1 suite, 0.00s)
```

```
cargo test -p pc-issues --lib
cargo test: 173 passed (1 suite, 0.01s)
```

## 关键决策

- 本轮不改任何 Node 业务逻辑，仅在 Rust 服务层补足回归。
- hook event 升级为 PartialEq + Eq 是为了让 assert_eq! 能直接比对事件序列。
- 测试只用 Noop / Recording hook，不触碰 pc-http 中的真实 IssueExecutionPolicyHook 实现。

## 后续重点

- 继续扩展 service 层实例化测试：in_progress / in_review / done 真实 transition、monitor trigger / clear 周期。
- 串通 pc-http 路由做 issue 状态机的端到端 API 验证。
