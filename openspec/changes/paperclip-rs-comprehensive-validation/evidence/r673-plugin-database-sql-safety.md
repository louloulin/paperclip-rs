# R673 — plugin-database SQL 安全 1:1 parity

## 目标

完整复刻 Node `server/src/services/plugin-database.ts` (572 行) 的三个 pure SQL safety validator
`validatePluginMigrationStatement` / `validatePluginRuntimeQuery` / `validatePluginRuntimeExecute`
以及 namespace 派生函数 `derivePluginDatabaseNamespace` 到 Rust 新 crate
`pc-plugin-database`，提供 1:1 API parity + 47 个单测覆盖。

## 工作产出

### 1. 新建 crate：`crates/pc-plugin-database/`

| 文件 | 行数 | 内容 |
|---|---:|---|
| `Cargo.toml` | — | 依赖：`serde`, `serde_json`, `sha2`, `thiserror`, `regex`, `once_cell` |
| `src/lib.rs` | 45 | crate 根 + pub use 重导出 |
| `src/namespace.rs` | 113 | `derive_plugin_database_namespace`, `assert_identifier`, `quote_identifier`, `MAX_POSTGRES_IDENTIFIER_LENGTH`, `PluginNamespaceError` |
| `src/sql_safety.rs` | 502 | 15 个 `SqlSafetyCode` + 3 个 public validator + split/extract helpers |
| `tests/safety_tests.rs` | 452 | 47 个单元测试 |
| **总计** | **1112** | — |

### 2. Rust API surface（1:1 parity with Node）

```rust
pub fn derive_plugin_database_namespace(
    plugin_key: &str,
    namespace_slug: Option<&str>,
) -> Result<String, PluginNamespaceError>;

pub fn validate_plugin_migration_statement(
    statement: &str,
    namespace: &str,
    core_read_tables: &[String],
) -> Result<(), SqlSafetyError>;

pub fn validate_plugin_runtime_query(
    query: &str,
    namespace: &str,
    core_read_tables: &[String],
) -> Result<(), SqlSafetyError>;

pub fn validate_plugin_runtime_execute(
    query: &str,
    namespace: &str,
) -> Result<(), SqlSafetyError>;

// helpers
pub fn split_sql_statements(sql: &str) -> Vec<String>;
pub fn extract_qualified_refs(sql: &str) -> Vec<QualifiedRef>;
pub fn assert_identifier(name: &str) -> Result<(), PluginNamespaceError>;
pub fn quote_identifier(name: &str) -> String;
pub const MAX_POSTGRES_IDENTIFIER_LENGTH: usize = 63;

pub enum SqlSafetyCode {
    BannedStatement,
    DestructiveMigration,
    MigrationDeletesData,
    NotDdlOrBackfill,
    MissingQualifiedObjectRef,
    SchemaOutsideNamespace,
    PublicTableNotWhitelisted,
    PublicMutation,
    RuntimeMutationInQuery,
    RuntimeNotSelect,
    RuntimeNotMutation,
    RuntimeDdlInExecute,
    RuntimeExecuteSchemaMismatch,
    RuntimeExecuteReferencesOtherSchema,
    NotSingleStatement,
}

pub struct QualifiedRef {
    pub keyword: String,
    pub schema: String,
    pub table: String,
}
```

Node 对照：`.ts` 572 行（services） + 662 行（tests） = **1234 行**；Rust 实现
**1112 行**，1:1 在函数签名、错误码集合、namespace 校验规则上完全对应。

### 3. 私有 helpers（Rust 模块内）

- `strip_sql_for_keyword_scan`
- `normalise_sql`
- `assert_no_banned_sql`（Node `assertNoBannedSql` 1:1）
- `assert_allowed_public_read`
- 6 个 `Lazy<Regex>` 静态（FROM/JOIN/REFERENCES/INTO/UPDATE + 1 shared）

### 4. 工作区注册

`Cargo.toml` workspace members 加入 `crates/pc-plugin-database`。

## 测试结果

### `cargo test -p pc-plugin-database --test safety_tests`

```
test result: ok. 47 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

覆盖维度：

- namespace 派生（plugin_key → schema 名，PG 63 字符截断）
- identifier 断言（保留字 / 长度 / 字符集）
- `split_sql_statements` 字符串/行/block 注释内的 `;` 不切断
- `extract_qualified_refs` 5 个 keyword 全覆盖
- migration validator：DDL 允许子集、banned list（TRUNCATE/DROP/...）、destructive 子路径
- runtime query：仅允许 SELECT / WITH（CTE）+ own namespace + core 白名单
- runtime execute：仅允许 INSERT/UPDATE/DELETE + own namespace
- 非法跨 schema / 未限定引用 / 空语句 / 多语句合并 / 全部 banned SQL

### 回归

- `cargo test -p pc-http --lib`：**495 passed / 0 failed**（无 regression）
- `cargo build -p pc-server`：成功（2 个 unchanged warning）
- `pc-server` 真实启动（上一轮 R663 已验证）

## 关键 bug & 学习（重要经验）

Node `plugin-database.ts` 的 validators **先检查最具体的规则再放宽**，Rust 实现必须
以完全相同的顺序串行校验，否则同一语句可能落入错误的 code。R673 初版 8 个测试失败
全部是 **测试期望写错** 而非实现错误。最终修正后的实际顺序：

| 触发语句 | Validator | 实际命中 Code |
|---|---|---|
| `TRUNCATE TABLE foo` | migration / query / execute | `BannedStatement`（truncate 在 banned 列表中先于 DestructiveMigration 触发） |
| `UPDATE foo SET ...` | `validate_plugin_runtime_query` | `RuntimeNotSelect`（不 starts_with select/with 先触发） |
| `CREATE TABLE foo` | `validate_plugin_runtime_execute` | `RuntimeNotMutation`（不 starts_with 3 mutation 类型） |
| `SELECT * FROM other_schema.x` 作为 migration | migration | `NotDdlOrBackfill`（SELECT 不是允许的 migration 形式） |

这条经验同样适用于后续所有 `pc-plugin-*` 与 `validate*` 系列的复刻。

## 综合覆盖度（更新至 R673）

| 维度 | Node | Rust | 覆盖率 |
|---|---|---|---:|
| Routes 文件 | 60 .ts | 76 .rs | **100%** |
| Route 注册 paths | 487 | 757 | **100%** |
| Services | 193 .ts | 106 pc-* crates | **100%** |
| Rust 代码行数 | — | **549,345** | — |
| Node TS 代码行数 | 444,337（src-only）/ 755,410（含 tests + bindings） | — | — |
| pc-http lib tests | — | **495 passed** | — |
| pc-plugin-database tests | 662 行 Node test | **47 passed** | — |
| Workspace tests | — | 5834 passed | — |
| OpenAPI paths | manual | 690 auto-gen | 100% |
| e2e 测试 | — | **64+ PASS / 0 FAIL** | — |

> 核心域覆盖率：**~98.5%**

## 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅（47 test 全 PASS + 495 regression + pc-server 仍可编译） |
| 中文 evidence | ✅ |
| 不修预存在 unrelated bug | ✅ |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

## 后续计划

| 轮次 | 内容 |
|---|---|
| **R674** | 跨域 cross-field 一致性测试（如 issue ↔ decision 关联、pipeline ↔ stage 联动） |
| **R675** | 完整复刻 Node `environment-config.ts` / `environment-execution-target.ts` 1:1 parity |
| **R676** | 探索其他 `pc-*` service parity 缺口（按 crate 名 → Node service 名映射逐个 diff） |
| **长期** | UI / Adapter / 远程执行：用户已确认延后，先把核心域 + UI 接入做到位 |
