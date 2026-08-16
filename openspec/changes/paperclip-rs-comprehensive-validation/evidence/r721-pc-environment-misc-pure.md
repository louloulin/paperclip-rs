# R721 — pc-environment/src/misc_pure.rs

## 目标

补足 Node services/environments.ts 中零 DB pure helpers。

## 新增 helpers（3 个）

| Node 函数 | Rust 函数 |
|---|---|
| cloneRecord | clone_record(value, fallback) |
| readEnum | read_enum(value, allowed, field_name) |
| hasConstraintName | has_constraint_name(error, constraint_name) |
| resolveListFilters | resolve_list_filters_string_or_object |

## 测试结果

cargo test -p pc-environment --lib misc_pure
running 11 tests
...
test result: ok. 11 passed; 0 failed

## 关键设计

- read_enum 接受 &'static [&'static str] 让 caller 可以直接传静态常量数组
- has_constraint_name 走 32 层 cause 链防止循环依赖
- clone_record 严格区分 object/array/scalar
