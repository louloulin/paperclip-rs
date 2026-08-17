# R779 - pc-tool 加精选 root re-exports (R776 改进 4.2)

日期: 2026-08-17
范围: crates/pc-tool/src/lib.rs
新增: 10 个子模块精选 root re-export，约 70 个公开项

## 背景

R776 架构审计发现 pc-tool 15 个子模块中仅 4 个 (connection_health / risk / runtime_metrics / service)
有 root re-export，其余 11 个调用方必须深路径导入。公共 API 使用体验差。本轮按"高内聚低耦合"补充精选 re-export。

## 验证

cargo build -p pc-tool           编译成功 (0 error)
cargo test -p pc-tool --lib     241 passed (基线一致)

## 新增 re-export 子模块 (10 个)

| 子模块 | re-export 项数 | 关键项 |
|---|---:|---|
| descriptor_hash | 3 | descriptor_hash, stable_hash, flatten_keys |
| profile_binding | 5 | narrowest_scope_bindings, ToolProfileBindingTargetType |
| policy_validation | 6 | iso_date_or_null, TrustRuleConfig, RateLimitRule |
| summarize_redact | 4 | summarize_and_redact, RedactionResult, RedactionPlan |
| side_effect_idempotency | 6 | side_effect_idempotency_key, ToolAccessDecision |
| argument_condition | 3 | read_path, argument_filters_match, ArgumentFilters |
| selector_match | 3 | selector_matches, ToolAccessContext, ToolAccessSelector |
| misc_pure | 8 | normalize_key, percent, percentile, CONNECTION_KEY_MAX_LEN |
| tool_invocation_pure | 6 | connection_uid, oauth_actor_type, ActorType |
| tool_validation_pure | 8 | validate_tool_*, is_tool_*_allowed, ALLOWED_TOOL_* |
| profile_helpers | 6 | profile_entry_matches_catalog, summarize_profile |

## 设计决策

1. 精选而非全量: 不使用 pub use xxx::*; 全量 re-export，避免根命名空间污染。手动列出每个公共项。
2. 重名 alias: misc_pure::normalize_key 与 tool_invocation_pure::normalize_key 同名；后者用 as invocation_normalize_key 别名。同样对 number_value 处理。
3. 未触碰子模块: 不修改任何子模块文件，仅在 lib.rs 追加 use 语句，无行为变化。
4. 未删旧 re-export: 保留既有 connection_health / risk / runtime_metrics / service re-export。

## 调用方使用改进示例

之前 (R779 前): use pc_tool::side_effect_idempotency::side_effect_idempotency_key;
之后 (R779 后): use pc_tool::side_effect_idempotency_key;

## 累计

R756-R779 累计 25 跟踪 crate 共 3055 PASS (R779 无新增单测，纯 API 形状改进)。

## R780+ 后续计划

- R780 - pc-core 加精选 root re-exports (R776 改进 4.4)
- R781 - pc-pipeline-conversation-context 拆分 pure.rs/service.rs (R776 改进 4.1)
- R782+ - pc-repos 拆分 pure/db (R776 改进 4.3 长期)
- Adapter 永远跳过 (硬约束 #2)