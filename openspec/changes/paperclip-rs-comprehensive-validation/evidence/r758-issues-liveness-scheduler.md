# R758 — pc-issues::liveness::incident_key + pc-routines::scheduler 集成测试（+12 PASS）

## 目标

补充 pc-issues liveness 模块和 pc-routines scheduler 模块的纯函数边缘测试，
确保 incident key 构造/解析、catch-up 计算、cron next tick 的边界行为正确。

## 1. pc-issues::liveness::incident_key（+7 PASS）

| 测试 | 验证 |
|---|---|
| r758_incident_key_blocker_priority | blocker_issue_id 优先于 participant_agent_id |
| r758_incident_key_none_fallback | blocker + participant 都 None 时用 "none" |
| r758_incident_key_round_trip | 构造 → 解析 round-trip 一致 |
| r758_parse_invalid_prefix | 错误前缀返回 None |
| r758_parse_wrong_field_count | 字段数不对返回 None |
| r758_parse_invalid_uuid | UUID 解析失败返回 None |
| r758_parse_empty_state | 空 state 返回 None |

### 验证

```
cargo test -p pc-issues liveness::incident_key
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 176 filtered out

cargo test -p pc-issues --lib
test result: ok. 183 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 2. pc-routines::scheduler（+5 PASS）

| 测试 | 验证 |
|---|---|
| r758_compute_catch_up_skip_missed | catch_up_policy != enqueue_missed_with_cap → run_count=1 |
| r758_compute_catch_up_sub_hourly_caps_to_one | sub-hourly cron 即使 cap policy 也只 run 1 次 |
| r758_compute_catch_up_hourly_drift | hourly 5h drift → 至少 5 catch-up runs |
| r758_compute_catch_up_respects_max_cap | 极大 drift（1000h）受 MAX_CATCH_UP_RUNS 上限约束 |
| r758_next_cron_tick_across_midnight | 跨日（23:59 + 1h cron）→ 次日 00:00 |

### 验证

```
cargo test -p pc-routines r758
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 188 filtered out

cargo test -p pc-routines --lib
test result: ok. 193 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 累计进度

| 模块 | 累计 PASS | 增量 |
|---|---:|---:|
| pc-issues | 183 | +7 |
| pc-routines | 193 | +5 |
| **本轮合计** | **376** | **+12** |

## R759+ 后续计划

- R759 — pc-heartbeat / reconcile 集成测试
- R760 — pc-decisions / wakeup / execution 集成测试
- 真实 Chromium 浏览器对核心页面完成 mutation 流程
- Adapter 仍按硬约束保持不动
