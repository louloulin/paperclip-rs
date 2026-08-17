# R739 — pc-issues/src/visibility_pure.rs

## 目标

补足 Node paperclip/server/src/services/issue-visibility.ts 中 visibility 分类与
统计聚合 pure helpers（独立于 IssueRow 的强类型 enum + 聚合函数）。

## 新增 helpers (10 个)

| Node 语义 | Rust 函数 |
|---|---|
| VisibilityReason → label | VisibilityReason::as_str() |
| VisibilityReason → parse | VisibilityReason::from_str(s) |
| 是否阻碍可见性 | blocks_visibility() |
| 是否为 hidden 类 | is_hidden() |
| 是否为 harness 类 | is_harness() |
| 聚合 entries → stats | aggregate_visibility(entries) |
| 按 reason 分组计数 | count_by_reason(entries) |
| 是否全部可见 | is_all_visible(agg) |
| visible ratio 计算 | visible_ratio(agg) |

## 测试结果

cargo test -p pc-issues --lib visibility_pure
test result: ok. 16 passed; 0 failed

## 关键设计

- VisibilityReason enum 与 visibility::types::IssueVisibilityReason 1:1 对齐
- VisibilityAggregate 是独立 pure 结构（不依赖 IssueRow / DB）
- from_str 支持 snake_case + camelCase legacy + case-insensitive
- visible_ratio 返回 0.0 ~ 1.0（empty → 0.0）

## 文件

- 新增：crates/pc-issues/src/visibility_pure.rs (7285 bytes)
- 修改：crates/pc-issues/src/lib.rs (+1 行 pub mod visibility_pure)
