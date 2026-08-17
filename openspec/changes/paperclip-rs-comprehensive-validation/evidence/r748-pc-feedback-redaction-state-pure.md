# R748 — pc-feedback/redaction/redaction_state_pure 纯函数模块

## 目标

把 feedback-redaction.ts 中的 pure helpers 对齐到 Rust 独立模块：
稳定的 JSON 序列化、sha256 hex、redaction state 聚合、summary 构建、field path helpers。

## 新增内容

### crates/pc-feedback/src/redaction/redaction_state_pure.rs (13.6 KB / 24 单测)

#### 公开 API

| 函数 / 类型 | 用途 | 对齐 Node |
|---|---|---|
| stable_stringify(value) | 按 key 字典序排序的 JSON 序列化 | stableStringify |
| sha256_hex_digest(value) | sha256(stable_stringify) -> hex | sha256Digest |
| RedactionStateLike struct | redacted/truncated/omitted/notes/counts | FeedbackRedactionState |
| record_redaction/truncation/omission/path | 标记 field path | service 内联 |
| increment(kind, count) | 累加 pattern 命中计数 | increment(state, kind, count) |
| note(msg) / merge_from(other) | 注释 / 合并 state | service 内联 |
| RedactionSummary + from_state | 序列化成 camelCase JSON | finalizeFeedbackRedactionSummary |
| finalize_redaction_summary(state) | state -> Value summary | 同上 |
| join_field_path(parent, child) | 嵌套字段路径 | fieldPath.key |
| array_index_path(parent, index) | 数组索引路径 | fieldPath[index] |
| truncate_to_chars(text, max_chars) | 截断 + 省略号 | output.slice(0, maxChars - 1) + ... |
| DEFAULT_MAX_CHARS = 16 * 1024 | max chars 上限 | DEFAULT_MAX_CHARS |

#### 设计要点

1. 零 DB / 零 IO：所有函数只消费 Value / String / 整数 / 自定义 struct。
2. canonical JSON：按 ASCII 字典序排序 key，序列化结果稳定可哈希。
3. BTreeMap / BTreeSet 容器：天然字典序，避免额外排序步骤。
4. serde rename_all = camelCase：与 Node 协议字段名 (redactedFields 等) 对齐。
5. truncate_to_chars 截断语义：max_chars = max 原文字符数；输出 = max_chars chars + ...
6. tests 全部命名 r748_*。

## 验证

cargo test -p pc-feedback --lib redaction_state_pure
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 66 filtered out

cargo test -p pc-feedback --lib
test result: ok. 90 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

## 与 Node parity

| 行为 | Node | Rust | 一致 |
|---|---|---|---|
| stable_stringify 排序 keys | Y | Y | OK |
| stable_stringify 嵌套递归 | Y | Y | OK |
| sha256Digest 稳定 (与 key 顺序无关) | Y | Y | OK |
| field path 拼接 (.) | Y | Y | OK |
| 数组索引路径 ([N]) | Y | Y | OK |
| RedactionState 集合语义 | Y | Y | OK |
| Summary strategy = deterministic_feedback_v2 | Y | Y | OK |
| Summary 字段排序 + camelCase | Y | Y | OK |
| truncate 加 ... | Y | Y | OK |

## 累计

| 项 | 之前 | R748 后 |
|---|---:|---:|
| pc-feedback lib tests | 66 | 90 |
| pc-feedback R748 新增 | - | +24 |
| 累计 R712-R748 新增 | 404 | +24 = 428 PASS |
| 累计新代码行数 | ~11000 | ~11500 |

## 后续

- R749 — pc-companies/search_rate_limit 补足
- R750 — pc-routines/activity_gate pure helper 抽取
