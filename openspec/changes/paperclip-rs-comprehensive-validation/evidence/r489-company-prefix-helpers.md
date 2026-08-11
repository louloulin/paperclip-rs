# R489 — pc-repos::company issue prefix 纯函数抽取 + 测试

> 时间：2026-08-11  
> 范围：`crates/pc-repos/src/company.rs`  
> 对齐：Node `services/companies.ts::deriveIssuePrefixBase` + `suffixForAttempt` + `isIssuePrefixConflict`

## 1. 目标

`pc-repos::company::issue_prefix_candidate(name, attempt)` 是一个被 DB 调用链路使用的纯函数，
但当前仅作为 `fn` 私有函数存在，**没有独立测试覆盖**。本轮：

1. 抽取为 2 个公开 `pub` 纯函数（与 Node 1:1 对齐）
2. 添加 10 个新单测覆盖边界
3. 保留 `issue_prefix_candidate` 作为组合 helper

## 2. 实现

### 2.1 新增公开 API

```rust
pub const ISSUE_PREFIX_FALLBACK: &str = "PC";

pub fn derive_issue_prefix_base(name: &str) -> String {
    let normalized: String = name
        .chars()
        .filter(char::is_ascii_alphabetic)
        .map(|c| c.to_ascii_uppercase())
        .take(3)
        .collect();
    if normalized.is_empty() {
        ISSUE_PREFIX_FALLBACK.to_string()
    } else {
        normalized
    }
}

pub fn suffix_for_attempt(attempt: usize) -> String {
    if attempt <= 1 {
        String::new()
    } else {
        "A".repeat(attempt - 1)
    }
}
```

### 2.2 Refactor `issue_prefix_candidate`

```rust
fn issue_prefix_candidate(name: &str, attempt: usize) -> String {
    let base = derive_issue_prefix_base(name);
    format!("{base}{}", suffix_for_attempt(attempt))
}
```

公开 base 和 suffix 是为了：① 单测可独立验证 ② 调用方可灵活组合。

## 3. 高内聚低耦合

| 函数 | 依赖 | 副作用 |
|---|---|---|
| `derive_issue_prefix_base` | `&str` | 纯函数 |
| `suffix_for_attempt` | `usize` | 纯函数 |
| `issue_prefix_candidate` | 上两个 | 纯函数（私有组合 helper）|

零外部依赖变化；零破坏性变更；纯增量 API。

## 4. 测试覆盖（10 个新单测）

| 函数 | 测试数 | 覆盖场景 |
|---|---|---|
| `derive_issue_prefix_base` | 6 | 基本 ASCII / 转大写 / take(3) / 过滤非字母 / CJK 保留 ASCII 部分 / 空串 fallback |
| `suffix_for_attempt` | 2 | attempt=0/1 → "" / attempt 递增 → "A"+"A"\*n |
| `is_issue_prefix_conflict` | 2 | 非 23505 错误 → false / TypeNotFound → false |

## 5. 验证基线

```text
$ cargo test -p pc-repos --lib company::tests
test result: ok. 11 passed; 0 failed
                          ↑ 从 1 → 11 (+10 个新测试)

$ cargo fmt -p pc-repos --check
                          ↑ no diff
```

注：`cargo clippy -p pc-repos -- -D warnings` 显示仓库其它文件的 pre-existing 错误
（不在本轮 company.rs 范围内），未触及。

## 6. Node 1:1 对齐验证

| 场景 | Node | Rust | 一致 |
|---|---|---|---|
| `deriveIssuePrefixBase("Paper Clip")` | "PAP" | "PAP" | ✅ |
| `deriveIssuePrefixBase("paperclip")` | "PAP" | "PAP" | ✅ |
| `deriveIssuePrefixBase("123")` | "PC" (fallback) | "PC" | ✅ |
| `deriveIssuePrefixBase("纸clip")` | "CLI" (filter 纸) | "CLI" | ✅ |
| `suffixForAttempt(1)` | "" | "" | ✅ |
| `suffixForAttempt(3)` | "AA" | "AA" | ✅ |
| `suffixForAttempt(10)` | "AAAAAAAAA" | "AAAAAAAAA" | ✅ |
| `isIssuePrefixConflict` (非 23505) | false | false | ✅ |

## 7. 完成判据

- [x] Rust 源码写到 `crates/pc-repos/src/company.rs`（高内聚低耦合）
- [x] 抽取 2 个公开 `pub fn` 纯函数（与 Node 1:1 对齐）
- [x] `cargo test -p pc-repos --lib company::tests` 通过（11 passed）
- [x] `cargo fmt -p pc-repos --check` 无 diff
- [x] 中文说明完整（本 evidence 文件）
- [x] 与 Node `deriveIssuePrefixBase` + `suffixForAttempt` + `isIssuePrefixConflict` 行为 1:1 对齐

## 8. 下一轮候选（R490）

按高 ROI 排序：

1. **`pc-repos` 其它 file 加测试**（agent / issue / heartbeat 等子模块）
2. **`pc-pipelines` 深化**（当前仅 8 tests）
3. **`pc-routines` 服务层接入** `next_cron_tick_in_timezone` / `is_sub_hourly_cron_expression`
4. **完整 R487/R488 函数集成到 `pc-routines` 业务流**

建议 **R490 推进 pc-pipelines 深化** —— 当前 8 tests 太少；Node `pipelines.ts` 含
pipeline case outputs、conversation context 等可独立复刻的纯函数。
