# R750 — pc-routines/activity_gate_pure 纯函数模块

## 目标

把 pc-routines/src/activity_gate.rs 中的纯函数（policy 判断 / scope 解析 /
self-loop 检测 / verdict 构造）抽到独立 activity_gate_pure 模块。

## 新增内容

### crates/pc-routines/src/activity_gate_pure.rs (9.7 KB / 20 单测)

#### 公开 API

| 函数 / 常量 | 用途 | 对齐 Node |
|---|---|---|
| DEFAULT_POLICIES | always / none / disabled / "" 默认集合 | service 内联 |
| REQUIRE_EXTERNAL_ACTIVITY_POLICY | require_external_activity 字面量 | service 内联 |
| IGNORED_ACTIONS | issue.read_marked 等 4 个 | ACTIVITY_GATE_IGNORED_ACTIONS |
| ROUTINE_SCHEDULER_ACTOR_ID | routine-scheduler 字面量 | service 内联 |
| gate_required_for_policy | policy -> bool | service 内联判断 |
| parse_scope | "project" -> Project / 其它 -> Global | service 内联 |
| is_ignored_action | 检查 action 是否在 IGNORED_ACTIONS | service 内联 |
| is_self_loop_by_details_routine_id | 三参数 self-loop 判断 | service 内联 |
| is_self_loop | 五参数完整 self-loop 判断（详情 + entity_type + entity_id）| service 内联 |
| verdict_fire_default | 默认 fire (策略不是 require_external_activity) | service 内联 |
| verdict_fire_first | 首次 dispatch fire (无 window_start) | service 内联 |
| verdict_fire_matched | 匹配到活动 fire (含 matched_activity_id) | service 内联 |
| verdict_skip | gate 拒绝 (fire=false) | service 内联 |

#### 设计要点

1. 零 DB / 零 IO：所有函数只消费字符串 / Uuid / DateTime / verdict struct。
2. self-loop 三参数版（仅 details.routineId）+ 五参数完整版（同时支持 entity_type='routine' & entity_id=routine_id）。
3. verdict_* 构造器封装 ActivityGateVerdict 字面量，调用方一行搞定。
4. constants 与 Node 字面量 1:1 对齐（注释里写明来源）。
5. tests 全部命名 r750_*。

## 验证

cargo test -p pc-routines --lib activity_gate_pure
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 164 filtered out

cargo test -p pc-routines --lib
test result: ok. 184 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

## 与 Node parity

| 行为 | Node | Rust | 一致 |
|---|---|---|---|
| 默认策略 always/none/disabled 直接 fire | Y | Y | OK |
| require_external_activity 触发 gate | Y | Y | OK |
| scope project/company/global | Y | Y | OK |
| ignored actions 4 项 | Y | Y | OK |
| self-loop actor + details.routineId | Y | Y | OK |
| self-loop actor + entity_type=routine + entity_id | Y | Y | OK |
| verdict 4 种构造 | Y | Y | OK |

## 累计

| 项 | 之前 | R750 后 |
|---|---:|---:|
| pc-routines lib tests | 164 | 184 |
| pc-routines R750 新增 | - | +20 |
| 累计 R712-R750 新增 | 452 | +20 = 472 PASS |
| 累计新代码行数 | ~12000 | ~12500 |

## 后续

- R751+ — UI 真实 mutation (POST/PATCH/DELETE) 流通验证
- Adapter 解锁后接通 13 个 adapter（硬约束 #2）
