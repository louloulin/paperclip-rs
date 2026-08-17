# R771 — 用户/权限/反馈 R771 边缘测试 (+25 PASS)

日期: 2026-08-17
范围: pc-feedback / pc-auth / pc-authz / pc-decisions
新增: 25 个 R771 单元测试

## 验证

cargo test -p pc-feedback r771         8 passed
cargo test -p pc-auth r771             4 passed
cargo test -p pc-authz r771            7 passed
cargo test -p pc-decisions r771        6 passed

## 新增测试

### pc-feedback::pure (8)
- as_record / as_string / as_number / as_boolean 类型转换
- unique_non_empty 去重
- content_type_for_path 5 类
- build_issue_path (/{prefix}/issues/{id})
- parse_feedback_vote 4 种
- normalize_reason (Up→None, Down→reason)
- append_note

### pc-auth::password_validation_pure (4)
- PasswordStrength 5 变体 as_str
- is_acceptable (>= Medium)
- evaluate_password_strength 5 种典型
- character_class_count 4 类

### pc-authz::mentions (7)
- 6 个 mention scheme 常量
- parse_agent_mention_href 4 种
- parse_user_mention_href
- extract_agent_mention_ids (去重保序)
- extract_user_mention_ids
- build_agent_mention_href round-trip
- build_user_mention_href round-trip

### pc-decisions::effect_outcome_pure (6)
- EffectExecutionStatus 4 变体 as_str
- from_str (大小写 + trim)
- is_successful (仅 Executed)
- is_final_success / is_partial_success
- aggregate_outcomes 仅 Pending
- aggregate_outcomes executed + skipped

## 累计 (20 跟踪 crate)

| crate | PASS | R771 增量 |
|---|---:|---:|
| pc-feedback | 132 | +8 |
| pc-auth | 102 | +4 |
| pc-authz | 129 | +7 |
| pc-decisions | 185 | +6 |
| R771 增量 | — | +25 |
| R756-R771 合计 | 2995 | +88 R771 / +270 total |

## R772+ 后续计划

- R772 — pc-isssue references / reroute / mention_extraction_hook
- R773 — pc-routines attention / scheduler / worktree
- R774 — pc-heartbeat recovery / wake_dispatch / scrum
- R775 — 真实浏览器 UI 链路 Round 2 (修复 Layout 类名)
- Adapter 仍按硬约束保持不动