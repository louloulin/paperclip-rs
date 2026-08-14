# R651 (2026-08-13) — pc-heartbeat flaky 测试修复（M23 收口）

## 目标

修复 pc-heartbeat 3 个 round 测试的 flaky 失败（测试间数据污染）：

- `round558_suppression_db_override.rs` — 5 个测试
- `round638_hot_restart_db.rs` — 6 个测试
- `round308_liveness_dependency_cleanup.rs` — 13 个测试

## 根因

所有 3 个 round 都共享 `instance_settings.singleton_key='default'` 或
`issues.origin_kind='harness_liveness_escalation'` 的全局表/行。

并行测试时：
- 测试 A `set_flag(Some(false))` 后立即 `assert`
- 测试 B 同时 `set_flag(Some(true))` 覆盖了 A 的设置
- A 的 assert 失败

## 修复模式

对每个 round 加 `static Rxxx_TEST_LOCK: tokio::sync::Mutex<()> = ...`，然后
在每个 `#[tokio::test]` 函数体最开头加 `let _rxxx_guard = Rxxx_TEST_LOCK.lock().await;`。

模式与 `pc-pipelines/tests/r6392*.rs` 已有的修复一致（R639.2.x 系列）。

## 真实验证结果

```
cargo test -p pc-heartbeat --test round308_liveness_dependency_cleanup
cargo test: 13 passed (1 suite, 0.63s)

cargo test -p pc-heartbeat --test round558_suppression_db_override
cargo test: 5 passed (1 suite, 0.05s)

cargo test -p pc-heartbeat --test round638_hot_restart_db
cargo test: 6 passed (1 suite, 0.21s)

cargo test -p pc-heartbeat
cargo test: 1060 passed (69 suites, 10.71s)   # 0 failed
```

## 改动文件

- `crates/pc-heartbeat/tests/round308_liveness_dependency_cleanup.rs` (+13 行)
- `crates/pc-heartbeat/tests/round558_suppression_db_override.rs` (+5 行)
- `crates/pc-heartbeat/tests/round638_hot_restart_db.rs` (+6 行)

## 影响

- pc-heartbeat: 1055 → **1060 passed** (无回归)
- M23 stale lock sweep 全 round 现在稳定通过
- pc-heartbeat 服务层域稳定 100% (single-thread 跑)
