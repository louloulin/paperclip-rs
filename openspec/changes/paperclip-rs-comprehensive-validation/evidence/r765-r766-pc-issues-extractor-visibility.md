# R765 + R766 — pc-issues references/extractor + visibility::types + dependency_wakeups（+12 PASS）

## 目标

补充 pc-issues 子模块的纯函数边缘测试：references/extractor（issue 引用提取）、visibility::types（SQL 谓词）、dependency_wakeups（依赖唤醒幂等）。

## R765 — pc-issues::references::extractor（+5 PASS）

| 测试 | 验证 |
|---|---|
| r765_normalize_identifier_uppercases_and_validates | 大写 + trim + 合法模式 |
| r765_strip_markdown_code_removes_fenced | 移除 fenced code block |
| r765_parse_issue_href_formats | 完整 URL / 相对路径 / query / hash / 非法 |
| r765_extract_identifiers_dedup_and_order | 去重 + 保序 + 跨格式 |
| r765_extract_matches_has_index_and_length | 返回 index + length |

## R766 — pc-issues::visibility::types + dependency_wakeups（+7 PASS）

### dependency_wakeups（+3 PASS）

| 测试 | 验证 |
|---|---|
| r766_build_wake_idempotency_key_format | 3 段 - 分隔 |
| r766_is_idempotent_wake_status_set | 4 个 idempotent statuses (queued/deferred_issue_execution/claimed/completed) |
| r766_normalize_idempotency_keys | 去重 + 跳过空字符串 + 保序 |

### visibility::types（+4 PASS）

| 测试 | 验证 |
|---|---|
| r766_issue_visibility_sql_with_alias | 带 quoted alias 替换 |
| r766_and_visible_prefix | 拼接 " AND " 前缀 |
| r766_or_visible_prefix | 拼接 " OR " 前缀 |
| r766_issue_visibility_condition_sql_nonempty | 常量包含 hidden_at |

## 修改

- `crates/pc-issues/src/visibility/mod.rs` — 加 `pub mod types;` 让原本死代码的 types.rs 激活
- `crates/pc-issues/src/visibility/types.rs` — 加 4 个 R766 tests

## 验证

```
cargo test -p pc-issues r765
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 188 filtered out

cargo test -p pc-issues r766
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 188 filtered out

cargo test -p pc-issues --lib
test result: ok. 195 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 累计

| crate | PASS | 增量 |
|---|---:|---:|
| pc-issues | 195 | +12 |
| **R756-R766 合计** | **2110** | **+78** |

## R767+ 后续计划

- R767 — pc-tool / pc-routines 剩余模块测试
- Adapter 仍按硬约束保持不动
