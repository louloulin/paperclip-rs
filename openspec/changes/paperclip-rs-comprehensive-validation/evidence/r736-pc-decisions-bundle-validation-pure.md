# R736 — pc-decisions/src/bundle_validation_pure.rs

## 目标

补足 Node paperclip/server/src/services/decisions.ts 中 DecisionBundle 业务层
零 DB validation helpers（uuid nil + title + filter normalization + state enum）。

## 新增 helpers (5 个)

| Node 语义 | Rust 函数 |
|---|---|
| uuid 非 nil 校验 | require_non_nil(id, field) |
| bundle title 校验 | validate_bundle_title(title) |
| filter 规范化（lowercase + clamp limit） | normalize_bundle_filter(filter) |
| bundle state 是否合法 | is_valid_bundle_state(state) |
| bundle state 解析（round-trip） | BundleState enum + from_str |

## 测试结果

cargo test -p pc-decisions --lib bundle_validation_pure
test result: ok. 12 passed; 0 failed

## 关键设计

- require_non_nil 与 bundle_service.rs 中现有的私有 helper 1:1 对齐（用 pub 替代 fn）
- validate_bundle_title 限制 title 长度 <= 256
- normalize_bundle_filter 用 clamp(1, 500) 限制 limit（防 DoS）
- BundleState 枚举覆盖 done / open / cancelled / pending / expired

## 文件

- 新增：crates/pc-decisions/src/bundle_validation_pure.rs (5077 bytes)
- 修改：crates/pc-decisions/src/lib.rs (+1 行 pub mod bundle_validation_pure;)
