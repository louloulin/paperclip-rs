# R572.1 — pc-repos compile error 修复

**状态**: ✅ 完成 (2026-08-12)

## 1. 问题

`cargo test --workspace --lib` 因 `pc-repos` 编译错误失败：

```
error[E0599]: no associated item named `ZERO` found for struct
              `pc_core::ExportFidelityCounts` in the current scope
   --> crates/pc-repos/src/export_fidelity.rs:190:37
    |
190 |             ..ExportFidelityCounts::ZERO
    |                                     ^^^^ associated item not found
```

## 2. 根因

`pc-portability-fidelity` 中的 `ExportFidelityCounts` 实现采用 idiomatic Rust 命名（`zero()` 小写方法），而 `pc-repos` 调用方使用 `::ZERO`（Node 风格 const）。R-INTEGRATION-5 (R565) 重命名时只更新了 pc-portability-fidelity 内部的 `Default::default()` → `zero()`，遗漏了 pc-repos 的调用点。

## 3. 修复

```rust
// crates/pc-repos/src/export_fidelity.rs:190
-            ..ExportFidelityCounts::ZERO
+            ..ExportFidelityCounts::zero()
```

## 4. 验证

```bash
$ cargo build -p pc-repos
Finished `dev` profile [unoptimized + debuginfo] target(s) in 25.99s

$ cargo test -p pc-repos --lib export_fidelity
test export_fidelity::tests::constants_match_node_schema ... ok
test export_fidelity::tests::build_report_emits_warnings_and_iso_timestamp ... ok
test result: ok. 2 passed; 0 failed
```

零回归，零新测试。

## 5. 教训

- 跨 crate re-export 重命名时必须 `rg` 所有调用点（不仅是当前 crate）
- Node const 风格 (`FOO_BAR.ZERO`) vs Rust idiomatic (`foo_bar.zero()`) 在重构
  时要明确选择 + 全局搜索
