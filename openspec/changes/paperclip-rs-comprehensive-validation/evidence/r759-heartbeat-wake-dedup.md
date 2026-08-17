# R759 — pc-heartbeat::wake_dedup 集成测试（+7 PASS）

## 目标

补充 pc-heartbeat::wake_dedup 模块的纯函数测试，确保 wake dedup 决策逻辑（Create / Coalesce / Skip）覆盖所有分支。

## 测试覆盖（+7 PASS）

| 测试 | 验证 |
|---|---|
| r759_decide_wake_no_existing_creates | None snapshot → Create |
| r759_decide_wake_completed_status_creates | completed status → Create（completed 不在 active set）|
| r759_decide_wake_company_mismatch_skips | company 不匹配 → Skip with company mismatch reason |
| r759_decide_wake_agent_mismatch_skips | agent 不匹配 → Skip with agent mismatch reason |
| r759_decide_wake_active_status_coalesces | 同 company/agent + active status → Coalesce increment=1 |
| r759_is_active_wakeup_status_covers_four_states | 4 个 active statuses + completed/failed/空 → false |
| r759_merge_wake_payloads_both_none | 双 None → JSON null |

## 验证

```
cargo test -p pc-heartbeat r759
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 655 filtered out

cargo test -p pc-heartbeat --lib
test result: ok. 662 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 累计

| 模块 | 累计 PASS | 增量 |
|---|---:|---:|
| pc-heartbeat | 662 | +7 |

## R760+ 后续计划

- R760 — pc-decisions / wakeup / execution 集成测试
- 真实 Chromium 浏览器对核心页面完成 mutation 流程
- Adapter 仍按硬约束保持不动
