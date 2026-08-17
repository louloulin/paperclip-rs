# R773 — pc-pipeline-* 4 个核心模块边缘测试 (+31 PASS)

日期: 2026-08-17
范围: pc-pipeline-case-type / pc-pipeline-health / pc-pipeline-case-outputs / pc-pipeline-conversation-context
新增: 31 个 R773 单元测试 (r773_ 前缀, 用于回归索引)

## 背景

R773 任务原本以为 pc-pipeline-* 几个 crate 是"空占位", 经实际探查后发现它们
**已经按 Node 端 1:1 实现**, 但 pure 模块的边缘覆盖不够。本轮为每个 crate 增加
聚焦边缘场景的 r773_ 单测, 不重复 R538/R554/R639 已覆盖路径。

## 验证

cargo test -p pc-pipeline-case-type --lib           11 passed (+6 R773)
cargo test -p pc-pipeline-health --lib              39 passed (+7 R773)
cargo test -p pc-pipeline-case-outputs --lib        21 passed (+11 R773)
cargo test -p pc-pipeline-conversation-context --lib 22 passed (+7 R773)

## 新增测试

### pc-pipeline-case-type (6)
- r773_case_type_matches_returns_true_when_declared_is_none — 缺省 None 视为通过
- r773_case_type_matches_returns_true_when_declared_is_empty — 空串视为通过
- r773_case_type_matches_returns_true_when_declared_matches_derived_key — 一致通过
- r773_case_type_matches_returns_true_when_declared_matches_derived_id_fallback — id fallback 一致通过
- r773_case_type_matches_returns_false_when_declared_mismatches — 不一致拒绝
- r773_case_type_matches_handles_whitespace_only_key_fallback — 全空白 key 时退化到 id

### pc-pipeline-health (7)
- r773_group_warnings_by_stage_returns_empty_map_for_empty_input — 空输入边界
- r773_group_warnings_by_stage_preserves_warning_order — 同 stage 内 warnings 顺序保持
- r773_is_pipeline_terminal_stage_kind_returns_false_for_unrecognized — 未知 kind 不误判
- r773_compute_pipeline_health_ok_when_stages_fully_configured — 满配 ok=true
- r773_compute_pipeline_health_reports_failed_automation_dedup — 同 caseId 多 fail dedup
- r773_compute_pipeline_health_reports_multiple_failed_automation_cases — 不同 caseId 各自上报
- r773_compute_pipeline_health_pipeline_id_propagates_to_report — pipeline_id 透传

### pc-pipeline-case-outputs (11)
- r773_normalize_preview_text_handles_empty_string — empty/None/whitespace → None
- r773_normalize_preview_text_keeps_short_text_intact — 短文本不截断
- r773_truncate_context_excerpt_returns_truncated_flag — 长文本触发 truncated=true
- r773_truncate_context_excerpt_returns_input_when_short — 短文本原样返回
- r773_deliverable_rank_returns_99_for_non_document — WorkProduct/Attachment → 99
- r773_output_sort_group_classifies_each_kind — Document/WorkProduct/Attachment 分组
- r773_source_issue_path_falls_back_to_id_when_identifier_empty — identifier 为空/None 回退
- r773_source_document_path_concatenates_document_key — 路径拼接正确
- r773_context_fetch_hint_attachment_includes_untrusted_warning — 附件提示带 untrusted
- r773_sanitize_output_context_summary_caps_total_length — 总长度封顶
- r773_summarize_pipeline_case_outputs_handles_empty_response — 空 items 边界

### pc-pipeline-conversation-context (7)
- r773_truncate_with_flag_handles_empty_string — 空串边界
- r773_truncate_with_flag_handles_unicode_codepoints — Unicode 字符边界 (按 chars 计数)
- r773_truncate_with_flag_at_exact_boundary_is_not_truncated — 边界等号不截断
- r773_truncate_with_flag_max_zero_returns_empty — max=0 边界
- r773_fence_markdown_always_at_least_three_backticks — 默认 3 backtick 包裹
- r773_fence_markdown_breaks_with_consecutive_backticks — 值内连续 backtick 自动加长 fence
- r773_fence_markdown_preserves_value_verbatim — 值不被修改

## 累计 (4 个 pc-pipeline-* crate 合计)

| crate | R772 | R773 | 增量 |
|---|---:|---:|---:|
| pc-pipeline-case-type | 5 | 11 | +6 |
| pc-pipeline-health | 32 | 39 | +7 |
| pc-pipeline-case-outputs | 10 | 21 | +11 |
| pc-pipeline-conversation-context | 15 | 22 | +7 |
| **R773 增量** | **62** | **93** | **+31** |

R756-R773 累计 24 跟踪 crate 共 3040 PASS (+31 R773)。

## 设计决策

1. **不破坏 Node 1:1 映射**: r773_ 测试仅覆盖纯函数行为, 不动 pure.rs 的实现。
   端到端集成测试 (tests/r639_*, e2e.rs) 仍按既有结构保留。
2. **R538/R554/R639 测试不动**: 旧测试保留, 新增独立 r773_ 模块, 便于 grep 回归。
3. **测试命名规范**: 全用 r773_ 前缀, 未来 R774+ 边缘测试保持一致。
4. **未触碰 Adapter**: 仍按硬约束 #2 不动 pc-adapter-* 任何文件。

## R774+ 后续计划

- R774 — pc-heartbeat 剩余 recovery (scrum / wake_dispatch / task_* 系列)
- R775 — 真实浏览器 UI 链路 Round 2 (7 页 + mutation 全链路)
- R776 — 架构整合 (lib.rs 公共 API 形状统一 + pc-server 依赖收敛)
- Adapter 永远跳过 (硬约束 #2)
