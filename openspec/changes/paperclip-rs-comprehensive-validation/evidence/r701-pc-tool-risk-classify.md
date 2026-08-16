# R701 — pc-tool risk_classify 实现 (2026-08-16)

## 目标

补足 Node `services/tool-access.ts::classifyRisk` (7,028 行 Node monolith)
中第一个缺失的核心 pure function。

## 设计

- **Pure functions**: 无 DB / IO / 时间依赖
- **Module**: 新增 `crates/pc-tool/src/risk.rs`（独立子模块，不动现有 service.rs）
- **公开 API**: `classify_risk`, `verb_matches`, `McpToolAnnotations`,
  `McpToolDescriptor`, `ToolRiskLevel`, `DESTRUCTIVE_VERBS`, `WRITE_VERBS`
- **优先级 (与 Node 完全一致)**:
  1. `destructive_hint === true` OR `destructive === true` → Destructive
  2. `read_only_hint === false` OR `write_hint === true` → Write
  3. name 匹配 DESTRUCTIVE_VERBS → Destructive
  4. name 匹配 WRITE_VERBS → Write
  5. fallback → Read

## 测试

```
running 11 tests
test risk::internal_tests::not_read_only_annotation ... ok
test risk::internal_tests::destructive_legacy_alias_wins ... ok
test risk::internal_tests::destructive_annotation_wins ... ok
test risk::internal_tests::annotation_priority_over_verb ... ok
test risk::internal_tests::verb_matches_basic ... ok
test risk::internal_tests::verb_matches_case_insensitive ... ok
test risk::internal_tests::destructive_verb_in_name ... ok
test risk::internal_tests::verb_matches_handles_empty_pattern ... ok
test risk::internal_tests::read_fallback ... ok
test risk::internal_tests::write_hint_annotation ... ok
test risk::internal_tests::write_verb_in_name ... ok

test result: ok. 11 passed; 0 failed
```

## 关键 parity 验证

- `classify_risk` 1:1 复刻 Node `classifyRisk` 优先级
- `verb_matches` 1:1 复刻 Node `verbMatches` (case-insensitive contains)
- `DESTRUCTIVE_VERBS` / `WRITE_VERBS` 与 Node 字面量一致
- `McpToolDescriptor` / `McpToolAnnotations` / `ToolRiskLevel` 与 Node type 形状一致
- serde `rename_all = "camelCase"` 镜像 Node wire format

## 文件

- 新增: `crates/pc-tool/src/risk.rs` (164 行, 含 11 个单测)
- 修改: `crates/pc-tool/src/lib.rs` (新增 `pub mod risk;` + re-exports)

## R701 关键交付

- [x] risk.rs 模块 + 11 个单测 PASS
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `classifyRisk` 100% parity
- [x] 真实验证 (cargo test)

## 后续

- R702 — pc-tool descriptor_hash + sanitizeHttpFailure 错误分类
- R703 — pc-tool policy decision 决策
- R704 — pc-tool connection health check
- 后续聚焦最大缺口 pc-execution-workspace-guards

