# R768 — 跨 crate 边缘测试 (+41 PASS)

日期: 2026-08-17
范围: 7 个非核心域 crate 补充测试
新增: 41 个 R768 单元测试

## 目标

从核心域 (pc-tool/decisions/issues/routines/heartbeat/repos) 扩展到支持域
(mentions / costs / budgets / goals / approval / workflow / status-card-engine),
补充所有 R7xx 缺失的边缘和枚举派生测试。

## 验证

```
cargo test -p pc-mentions                    39 passed
cargo test -p pc-status-card-update-engine   53  (+5)
cargo test -p pc-budgets                     39  (+6)
cargo test -p pc-costs                       13  (+4)
cargo test -p pc-approvals                   57  (+6)
cargo test -p pc-workflow                    75  (+6)
cargo test -p pc-goals                        6  (+6)
```

## 新增测试

### pc-mentions (8)
- 6 个 scheme (project / agent / user / skill / routine / pipeline) round-trip
- extract_mentions_dedup (markdown 提取去重)
- wrong_scheme_rejected (跨 scheme 拒绝)

### pc-status-card-update-engine (5)
- ChangeKind::as_str 6 变体
- UpdateKind::as_str 2 变体
- PolicyAction::as_str 4 变体
- UpdateKind/PolicyAction serde snake_case
- ChangeKind serde

### pc-budgets (6)
- infer_status zero_amount 永远 Ok
- infer_status no_warning (warn_percent <= 0)
- infer_status at_limit HardStop
- infer_status warn_threshold ceil 精度
- normalize_scope_name company 保留原名
- normalize_scope_name 非 company trim + fallback

### pc-costs (4)
- current_utc_month_window 1 月
- current_utc_month_window 12 月跨年
- current_utc_month_window 月初 00:00:00 边界
- RecordingCostHook 同步 helper 生命周期

### pc-approvals (6)
- validate_transition Pending → Pending 非法
- validate_transition 终态 → 任意状态 全部非法
- validate_transition Pending → RevisionRequest 非法
- can_request_revision 仅 pending
- can_decide / can_cancel 仅 pending
- can_resubmit 永远 false

### pc-workflow (6)
- workflow_kind_label 2 变体
- step_status_label 5 变体
- workflow_run_state_label 6 变体
- is_terminal_run_state 3 终态
- is_terminal_step_status 3 终态
- trigger_spec_helpers (cron / event / manual)

### pc-goals (6)
- RecordingGoalHook events_snapshot
- RecordingGoalHook clear
- validate_goal_patch title 空白
- validate_goal_patch title 合法
- validate_goal_patch title None
- normalize_goal_patch trim + 空白置 None

## 累计

| crate | PASS | 增量 |
|---|---:|---:|
| pc-mentions | 39 | +8 R768 |
| pc-status-card-update-engine | 53 | +5 |
| pc-budgets | 39 | +6 |
| pc-costs | 13 | +4 |
| pc-approvals | 57 | +6 |
| pc-workflow | 75 | +6 |
| pc-goals | 6 | +6 |
| **R768 增量** | — | **+41** |
| **R756-R768 合计** | **2168** | **+136** |

## R769+ 后续计划

- R769 — 真实浏览器 UI 链路 (Dashboard / Issue / Routine / Tool 完整截图)
- R770 — 架构整合 (lib.rs 公共 API 形状统一)
- Adapter 仍按硬约束保持不动
