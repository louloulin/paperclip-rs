# R770 — 4 个核心域 pure 模块 R770 边缘测试 (+27 PASS)

日期: 2026-08-17
范围: pc-pipelines / pc-storage / pc-portability / pc-execution-workspace-guards
新增: 27 个 R770 单元测试

## 目标

R768 覆盖支持域边缘。R770 继续扩展核心域 pure 模块的 R7xx 测试覆盖深度。

## 验证

cargo test -p pc-pipelines r770                 6 passed
cargo test -p pc-storage r770                  7 passed
cargo test -p pc-portability r770              7 passed
cargo test -p pc-execution-workspace-guards r770  7 passed

## 新增测试

### pc-pipelines::aggregation (6)
- r770_attention_caller_predicates (is_user / is_agent / agent_id)
- r770_attention_caller_serde (tag + 字段)
- r770_bounded_limit_edges (None / 0 / 负 / max / 超)
- r770_payload_string_type_variants (String / Number / Object / null / missing)
- r770_pipeline_attention_counts_default (全 0)
- r770_attention_pipeline_ref_serde (camelCase)

### pc-storage::service (7)
- r770_sanitize_segment_empty_fallback (空 / 全空白 / 全非法 -> "file")
- r770_sanitize_segment_collapses_underscores (折叠连续 _)
- r770_sanitize_segment_truncates (120 截断)
- r770_normalize_namespace (空 / "misc" / 多段 / 折叠)
- r770_split_filename_variants (None / 无 ext / 多点 / 特殊字符)
- r770_hash_buffer_deterministic (64 hex, 一致)
- r770_ensure_company_prefix (4 种失败模式)

### pc-portability::export_readme (7)
- r770_mermaid_id_basic (5 种字符)
- r770_mermaid_escape (3 特殊字符)
- r770_role_label (8 固定 + 1 fallback)
- r770_generate_org_chart_empty_returns_none
- r770_generate_org_chart_single
- r770_generate_org_chart_hierarchy
- r770_skill_source_type_unknown_fallback (serde other)

### pc-execution-workspace-guards::lib (7)
- r770_workspace_status_as_str (5 变体)
- r770_workspace_status_parse (5 + 1 fallback)
- r770_workspace_mode_as_str (3 变体)
- r770_workspace_mode_parse (3 + 1 fallback)
- r770_closed_statuses_count (2 个)
- r770_is_closed_isolated_workspace_combinations (None / 4 状态)
- r770_message_contains_required_fields

## 累计

| crate | PASS | R770 增量 |
|---|---:|---:|
| pc-pipelines | 74 | +6 |
| pc-storage | 56 | +7 |
| pc-portability | 60 | +7 |
| pc-execution-workspace-guards | 55 | +7 |
| R770 增量 | — | +27 |
| R756-R770 合计 (17 crate) | 2626 | +63 R770 / +245 total |

## R771+ 后续计划

- R771 — pc-feedback pc-auth pc-authz (大 module 边缘)
- R772 — roadmap-decisions / 心跳恢复 / 端口核心
- R773 — 真实浏览器 UI 链路 (Round 2, 修复 Layout 类名)
- Adapter 仍按硬约束保持不动