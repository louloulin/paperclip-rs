# R772 — 业务核心域 R772 边缘测试 (+14 PASS)

日期: 2026-08-17
范围: pc-issues / pc-routines / pc-heartbeat
新增: 14 个 R772 单元测试

## 验证

cargo test -p pc-issues r772 --lib       3 passed
cargo test -p pc-routines r772 --lib    7 passed
cargo test -p pc-heartbeat r772 --lib   4 passed

## 新增测试

### pc-issues::goal_fallback (3)
- resolve_issue_goal_id 4 优先级分支
- resolve_issue_goal_id 有 project 但无 project_goal_id → None
- resolve_next_issue_goal_id 4 种 next 优先级

### pc-routines::activity_gate_pure (7)
- gate_required_for_policy 4 种 (require_external / always / never / unknown)
- parse_scope 4 种 (global / project / agent / unknown)
- is_ignored_action 4 已知 + 2 未知
- is_self_loop_by_details_routine_id 4 种
- verdict_fire_default 4 字段
- verdict_fire_first 保留 scope
- verdict_skip window_start

### pc-heartbeat::recovery::build_recovery_comment_display (4)
- recovery_cause_title 8 变体 + fallback
- build_compact_recovery_presentation 字段
- build_compact_recovery_presentation 截断 160
- build_recovery_notice_metadata sections/rows

## 累计 (20 跟踪 crate)

| crate | PASS | R772 增量 |
|---|---:|---:|
| pc-issues | 198 | +3 |
| pc-routines | 207 | +7 |
| pc-heartbeat | 666 | +4 |
| R772 增量 | — | +14 |
| R756-R772 合计 | 3009 | +28 R772 / +284 total |

## R773+ 后续计划

- R773 — pc-pipelines 额外 pure 模块 (conversations / health)
- R774 — pc-heartbeat 剩余 recovery 模块
- R775 — 真实浏览器 UI 链路 Round 2 (修复 Layout 类名)
- R776 — 架构整合 (lib.rs 公共 API 形状)
- Adapter 仍按硬约束保持不动