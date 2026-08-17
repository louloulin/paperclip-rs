# R745 — pc-routines/attention/attention_pure 纯函数模块

## 目标

把 `AttentionService` 中的核心纯函数（排序 / limit clamp / 默认 severity / kind→count
累加 / excerpt 截断 / 时间格式化）从 service.rs 中抽出到独立 `attention_pure` 模块，
对齐 `paperclip/server/src/services/attention.ts` 中的常量表 + 排序规则。

## 新增内容

### `crates/pc-routines/src/attention/attention_pure.rs`（新增 15.7 KB / 25 单测）

#### 公开 API

| 函数 / 常量 | 用途 | 对齐 Node |
|---|---|---|
| `DEFAULT_OPEN_DECISION_LIMIT` = 500 / `MAX_OPEN_DECISION_LIMIT` = 1_000 | decision list 上限 | `OPEN_DECISION_DEFAULT_LIMIT` |
| `DEFAULT_LIST_LIMIT` = 100 / `MAX_LIST_LIMIT` = 500 | attention list 上限 | — |
| `DETAIL_EXCERPT_LENGTH` = 160 / `DETAIL_IMAGE_LIMIT` = 3 | detail 字段限制 | `DETAIL_EXCERPT_LENGTH` / `DETAIL_IMAGE_LIMIT` |
| `to_epoch_ms(value: Option<DateTime<Utc>>) -> i64` | Date → epoch ms（缺失 → 0） | `timestamp(value)` |
| `to_iso_string(value: Option<DateTime<Utc>>) -> String` | Date → RFC3339 | `toIso(value)` |
| `clamp_list_limit(limit: i64) -> i64` | clamp 到 [1, MAX_LIST_LIMIT] | service 内联 |
| `clamp_open_decision_limit(limit: i64) -> i64` | clamp 到 [1, MAX_OPEN_DECISION_LIMIT] | service 内联 |
| `SeverityRankInput` enum + `severity_rank(input) -> u8` | 0=Critical, 1=High, 2=Medium, 3=Low, 4=Info | `SEVERITY_RANK` |
| `cmp_attention_items(a_sev, a_t, b_sev, b_t) -> Ordering` | severity asc + created_at desc | service 内联排序 |
| `sort_by_severity_then_created_at(items, sev_of, t_of)` | in-place 排序 | service 末尾 sort_by |
| `KindKind` enum + `KindKind::all()` | 12 种 kind 标识 | `ATTENTION_SOURCE_KINDS` |
| `filter_by_kind(items, target, kind_of) -> Vec<T>` | 按 kind 过滤保留顺序 | `list_by_kind` |
| `AttentionCountsLike` + `accumulate_count(counts, kind)` | count 累加 | `counts_for_company` |
| `empty_counts() -> AttentionCountsLike` | 全 0 counts | `emptyCounts()` |
| `total_counts(counts) -> usize` | 所有 kind 之和 | `counts.total()` |
| `truncate_excerpt(text, max_length) -> String` | 按 char boundary 截断 | `DETAIL_EXCERPT_LENGTH` 截断 |

#### 设计要点

1. **零 DB**：所有函数只消费已聚合好的 `AttentionItem` 字段或 enum 值，不依赖 sqlx。
2. **轻量 enum 入参**（`SeverityRankInput` / `KindKind`）—— 与 `AttentionSeverity` /
   `AttentionItemKind` 解耦，方便未来其他模块复用。
3. **闭包注入字段访问器**（sort_by_severity_then_created_at / filter_by_kind）——
   调用方无需把数据迁移到特定 struct，零拷贝排序。
4. **char-boundary 截断**—— UTF-8 安全，不会切到多字节字符中间。
5. **tests 全部命名 `r745_*`**。

## 验证

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs
cargo test -p pc-routines --lib attention_pure
```

结果：

```
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 98 filtered out
```

```bash
cargo test -p pc-routines --lib
```

结果：

```
test result: ok. 123 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 与 Node parity

| 行为 | Node | Rust | 一致 |
|---|---|---|---|
| severity rank: critical=0 → info=4 | ✓ | ✓ | ✅ |
| 排序：severity asc, created_at desc | ✓ | ✓ | ✅ |
| clamp limit [1, MAX] | ✓ | ✓ | ✅ |
| toIso(缺失) → epoch | ✓ | ✓ | ✅ |
| timestamp(缺失/无效) → 0 | ✓ | ✓ | ✅ |
| accumulate kind → count | ✓ | ✓ | ✅ |
| 按 kind 过滤保留顺序 | ✓ | ✓ | ✅ |
| excerpt char-boundary 截断 | ✓ | ✓ | ✅ |
| 12 个 attention source kinds | ✓ | ✓ | ✅ |

## 累计

| 项 | 之前 | R745 后 |
|---|---:|---:|
| pc-routines lib tests | 98 | **123** |
| pc-routines R745 新增 | — | **+25** |
| 累计 R712-R745 新增 | 306 | **+25 = 331 PASS** |
| 累计新代码行数 | ~9500 | **~10000** |

## 后续

- **R746** — pc-routines/service.rs DB 服务层补足（hooks + revision + dispatch helpers 提取）
- **R747** — pc-tool/service.rs DB 服务层补足
- **R748** — pc-feedback/redaction 服务层补足
