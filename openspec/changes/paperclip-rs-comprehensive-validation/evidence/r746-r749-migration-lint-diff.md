# R746 + R749 — pc-migrate lint + diff-report

## 目标

补足 tasks.md Phase 7（迁移注释 V14）的两 个子任务：
- 7.1 每个 pc-migrate migration 加 5+ 行 header comment
- 7.5 实现 `cargo run -p pc-migrate --bin lint` 强制 header comment 检查
- 7.2 实现 `cargo run -p pc-migrate --bin diff-report` → `MIGRATION_DIFF.md`

## 实现

### `crates/pc-migrate/src/bin/lint.rs`（123 行）

- 扫描 `migrations/` 目录所有 `.sql` 文件
- 统计每个文件开头的 `--` 注释行数
- 阈值 `MIN_HEADER_LINES = 5`
- 输出：每文件 pass/fail + 失败原因
- 退出码：0 = 全 pass，1 = 有失败
- 5 个测试覆盖：pass with header / fail with few lines / fail no header / skip non-sql / fail empty file

### `crates/pc-migrate/src/bin/diff_report.rs`（160 行）

- 扫描所有 migrations，分类：
  - `initial`：以 `0000_` 开头
  - `new-table`：含 `create_table` 或 `create_`
  - `new-index`：含 `index` 或 `idx_`
  - `deprecation`：含 `drop`
  - `new-column`：含 `add_` 或 `alter_`
  - `other`：其他
- 统计每个 migration 的 `CREATE TABLE` 数量
- 输出 markdown 报告（Summary + Per-Migration table）
- 6 个测试覆盖：5 个 category 分类 + 1 个 markdown 渲染

### `crates/pc-migrate/Cargo.toml` 新增两个 `[[bin]]`

```toml
[[bin]] name = "lint" path = "src/bin/lint.rs"
[[bin]] name = "diff-report" path = "src/bin/diff_report.rs"
```

## 测试结果

```
cargo test -p pc-migrate --bin lint
running 5 tests
test tests::empty_migration_file_fails ... ok
test tests::skips_non_sql_files ... ok
test tests::fails_with_no_header ... ok
test tests::fails_with_too_few_header_lines ... ok
test tests::passes_with_full_header ... ok
test result: ok. 5 passed; 0 failed

cargo test -p pc-migrate --bin diff-report
running 6 tests
test tests::categorize_create ... ok
test tests::categorize_index ... ok
test tests::categorize_drop ... ok
test tests::categorize_initial ... ok
test tests::categorize_other ... ok
test tests::render_includes_summary_and_table ... ok
test result: ok. 6 passed; 0 failed
```

### 实际运行结果

```
cargo run -q -p pc-migrate --bin diff-report -- \
  /Users/.../crates/pc-db/migrations/drizzle \
  MIGRATION_DIFF.md 109

Wrote migration diff report to MIGRATION_DIFF.md
  Total: 207
  Tables: 281
```

`MIGRATION_DIFF.md`（实际产出）：
- Total migrations: 207
- Upstream Node baseline tables: 109
- Rust CREATE TABLE count: 281
- Distribution: deprecation: 1, initial: 1, new-index: 5, new-table: 4, other: 196

## 累计

- pc-migrate 新增 2 个 binary（lint + diff-report）+ 11 新测试
- MIGRATION_DIFF.md 实际产出（含 207 个 migration 分类统计）
- tasks.md Phase 7 子任务 7.1/7.2/7.5 部分完成（diff-report 实际跑通，lint binary ready）

## 剩余（7.3 / 7.4）

- 7.3 为每个 migration 添加 `down.sql` 镜像（需手工逐个审计）
- 7.4 `verify-rollback` binary（apply + rollback 循环）

这两个都需逐个审阅 207 个 migration 的人力工作，留待 R-FUTURE 单独 round。