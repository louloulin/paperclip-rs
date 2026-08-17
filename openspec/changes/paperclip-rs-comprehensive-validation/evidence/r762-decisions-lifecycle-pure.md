# R762 — pc-decisions lifecycle_pure + pure 集成测试（+10 PASS）

## 目标

补充 pc-decisions 核心纯函数模块（lifecycle_pure / pure）的边缘测试，覆盖 decision lifecycle、effect classify、idempotency signing 等关键路径。

## 测试覆盖（+10 PASS）

### lifecycle_pure（+5 PASS）

| 测试 | 验证 |
|---|---|
| r762_should_resume_decision | 只有 execution_status="running" 才 resume；None/其他都 false |
| r762_is_decision_expired_basic | status=open + expires_at<=now → true；非 open / 未到期 / equals now 都正确 |
| r762_parse_sweep_batch_size | 合法数字 / None fallback / 非法 fallback |
| r762_parse_recovery_grace_ms | 同上 |
| r762_merge_unique_ids_dedup_preserves_order | ttl + target 去重，首序保留 |

### pure（+5 PASS）

| 测试 | 验证 |
|---|---|
| r762_classify_effect_type_known | comment_on_issue → Comment；其他 → Mutate |
| r762_classify_effect_type_unknown_returns_mutate | unknown 类型 → Mutate（safe default，与 Node upstream 一致）|
| r762_same_ids_set_equality | BTreeSet 比较，顺序无关 |
| r762_interpolate_replaces_keys | {{input.<id>}} 占位符 + missing → 空字符串 |
| r762_sign_verify_decision_spec_round_trip | 签名/验证 round-trip + 错 secret/篡改值都失败 |

## 验证

```
cargo test -p pc-decisions r762
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 169 filtered out

cargo test -p pc-decisions --lib
test result: ok. 179 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 累计

| crate | PASS | 增量 |
|---|---:|---:|
| pc-decisions | 179 | +10 |
| **R760+R762 合计** | | **+26** |

## R763+ 后续计划

- R763 — 其他模块集成测试（pc-tool / pc-routines / pc-issues 剩余边缘）
- Adapter 仍按硬约束保持不动
