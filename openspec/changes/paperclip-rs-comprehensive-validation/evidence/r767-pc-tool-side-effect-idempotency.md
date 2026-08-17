# R767 — pc-tool 4 个 pure 模块 集成测试

日期: 2026-08-17
范围: `crates/pc-tool` 4 个 pure 模块
新增: 17 个 R767 单元测试

## 目标

为 pc-tool crate 的 4 个核心 pure 模块补充 R767 边缘集成测试，
覆盖先前 760 TS 端口过程中可能未被触达的边界条件。

## 验证

```
cargo test -p pc-tool r767
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 224 filtered out

cargo test -p pc-tool --lib
test result: ok. 241 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 新增测试

### side_effect_idempotency (3)
- r767_risk_rank_mapping — 5 档 risk: read/low=1, write/medium=2, destructive/high=3, critical=4, unknown/empty=0
- r767_audit_outcome_decision_mapping — 4 个 ToolAccessDecision → 4 个 AuditOutcome
- r767_idempotency_key_deterministic — 同输入产生相同 key (format: "side_effect:<sha256-hex>")

### tool_invocation_pure (6)
- r767_number_value_edges — 0, 负数, 空格, 空, NaN, Infinity
- r767_percent_edges — 0/100, 100/100, 150/100, 1/3, 2/3, 0 分母, 负分母
- r767_percentile_edges — 单元素, p=0, p=100, 重复值
- r767_normalize_key_all_invalid — 全部非法字符 → "tool" fallback
- r767_connection_uid_edges — 长 / 短 connection_id 截断
- r767_actor_type_all_variants — Agent/User/System/Plugin 4 个变体字符串稳定

### descriptor_hash (4)
- r767_descriptor_hash_includes_description — description 字段影响 hash
- r767_descriptor_hash_includes_title — title 字段影响 hash
- r767_flatten_keys_nested_objects — 数组内嵌套对象 + null 叶子
- r767_stable_hash_key_order_invariant — BTreeSet 保证 key 顺序无关

### selector_match (4)
- r767_tool_name_single_matches_upstream — tool_name 单值匹配 upstream_tool_name
- r767_tool_names_many_or_match — tool_names 数组 OR 匹配
- r767_many_selector_rejects_undefined_actual — many selector + actual=None → fail
- r767_empty_selector_matches_empty_ctx — 空 selector + 空 ctx → 通过

## 累计

| crate | PASS | 增量 |
|---|---:|---:|
| pc-tool | 241 | +17 |
| R756-R767 合计 | 2127 | +95 |

## R768+ 后续计划

- R768 — pc-decisions wakeup/lifecycle 剩余边缘
- R768 — pc-issues continuation_summary / dependency_wakeups 剩余
- R768 — pc-routines activity_gate / attention 剩余
- Adapter 仍按硬约束保持不动
