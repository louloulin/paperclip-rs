# R743 — pc-pipelines::aggregation_pure

## 目标

补足 Node `server/src/services/pipelines-aggregation.ts`（P0 gap from parity-gap-report §F）的核心 pure helpers。
Node 全文 777 行（多为 sqlx-style queries），本 round 镜像 pure 部分。

## Rust 镜像

新增 `crates/pc-pipelines/src/aggregation_pure.rs`（纯函数模块）：

### 公开 API（7 const + 6 fn）

| Rust 函数/常量 | Node 对应 |
|---|---|
| `PIPELINE_ATTENTION_DEFAULT_LIMIT = 50` | `PIPELINE_ATTENTION_DEFAULT_LIMIT = 50` |
| `PIPELINE_ATTENTION_MAX_LIMIT = 100` | `PIPELINE_ATTENTION_MAX_LIMIT = 100` |
| `COMPANY_CASE_EVENTS_DEFAULT_LIMIT = 50` | `COMPANY_CASE_EVENTS_DEFAULT_LIMIT = 50` |
| `COMPANY_CASE_EVENTS_MAX_LIMIT = 100` | `COMPANY_CASE_EVENTS_MAX_LIMIT = 100` |
| `COMPANY_CASE_EVENTS_MAX_TYPES = 10` | `COMPANY_CASE_EVENTS_MAX_TYPES = 10` |
| `CASE_CHILDREN_TREE_MAX_NODES = 1_000` | `CASE_CHILDREN_TREE_MAX_NODES = 1_000` |
| `CASE_CHILDREN_TREE_MAX_DEPTH = 10` | `CASE_CHILDREN_TREE_MAX_DEPTH = 10` |
| `bounded_limit(limit, fallback, max) -> u32` | `boundedLimit(limit, fallback, max)` |
| `payload_string(value, key) -> Option<String>` | `payloadString(value, key)` |
| `exceeds_max_event_types(types) -> bool` | inline `types.length > MAX_TYPES` |
| `clamp_event_types(types) -> Vec<String>` | inline `types.slice(0, MAX_TYPES)` |
| `exceeds_max_tree_nodes(count) -> bool` | inline `count > MAX_NODES` |
| `exceeds_max_tree_depth(depth) -> bool` | inline `depth > MAX_DEPTH` |

## 设计要点

- **Pure facade pattern**：所有 const + helper 都是纯函数，DB-bound queries 留在 `aggregation_db.rs` / `case_events_db.rs`
- **typed Rust API**：`payload_string` 接受 `Option<&serde_json::Value>` 替代 Node `unknown`，类型安全
- **Option-based**：所有 helper 接受 `Option<u32>` / `Option<&str>` 避免 sentinel 值
- **edge case 明确化**：bounded_limit 在 None / 0 / overflow / underflow 时都有明确行为

## 测试覆盖（20 tests）

| 测试类别 | 测试数 |
|---|---|
| `bounded_limit_*` (5) | None / 0 / cap / floor / normal |
| `payload_string_*` (5) | extract / missing key / empty / non-object / non-string |
| `event_types_*` (3) | within / at limit / exceeds |
| `clamp_event_types_*` (2) | under / over |
| `tree_nodes_*` (2) | under / exceeds |
| `tree_depth_*` (2) | under / exceeds |
| `constants_match_node_upstream` (1) | 7 常量验证 |

## 测试结果

```
cargo test -p pc-pipelines --lib aggregation_pure
running 20 tests
... (20 个全 PASS)
test result: ok. 20 passed; 0 failed; 0 ignored
```

```
cargo test --workspace --lib --exclude pc-adapter-process
TOTAL PASS: 8492 (+20 vs 8472 baseline)
```

## 累计

- pc-pipelines 增加 aggregation_pure 模块（20 新单测 + 7 const）
- parity-gap-report §F（Pipelines & Workflows）减少 1 个 unported
- workspace lib 8472 → 8492 PASS / 0 FAIL