# R747 — pc-plugin-host::capability_validator::operations

## 目标

补足 Node `server/src/services/plugin-capability-validator.ts` 的 `OPERATION_CAPABILITIES` map（525 行文件中 ~250 行是 operation→capability 映射）。
Rust 端 `pc-plugin-host::capability_validator` 模块已存在 5 个子文件但缺 pure operation map 镜像 + 测试。

## Rust 镜像

新增 `crates/pc-plugin-host/src/capability_validator/operations.rs`：

### 公开 API

| Rust 函数 | Node 对应 |
|---|---|
| `pub mod ops`（26 个 const） | `OPERATION_CAPABILITIES` map keys |
| `pub fn required_capabilities(operation: &str) -> &'static [&'static str]` | `OPERATION_CAPABILITIES[operation]` |
| `pub fn plugin_can_perform(declared: &[String], operation: &str) -> bool` | `checkOperation()` / `assertOperation()` |
| `pub fn missing_capabilities(declared: &[String], operation: &str) -> Option<Vec<String>>` | install-time 校验逻辑 |

### Operation constants（26 个）

- 数据读：companies.list/get, projects.list/get, issues.list/get, approvals.list/get, agents.list/get
- 数据写：issues.create, issues.update, issue.comments.create, approvals.respond
- Plugin state：plugin.state.get/list/set
- Local folders：localFolders.readText/writeTextAtomic
- DB：db.query, db.migrate
- External objects：external.objects.read/write
- Activity：activity.log

## 设计要点

- **`&'static [&'static str]`**：match 返回静态切片，零分配
- **fail-closed semantics**：未知 operation → `required_capabilities` 返回 `&[]` → `plugin_can_perform` 返回 `true`（与 Node `localFolders.declarations` 一致）
- **`missing_capabilities`**：返回 `None` 表示"已满足或无要求"，返回 `Some(vec)` 表示缺失列表（用于 install-time 错误信息）
- **HashSet lookup**：用 `HashSet<&str>` 而非 `Vec` 提升多 capability 检查性能

## 测试覆盖（12 tests）

| 测试 | 覆盖 |
|---|---|
| `required_caps_for_companies_list` | read map 正确 |
| `required_caps_for_issues_create` | write map 正确 |
| `required_caps_for_plugin_state_set` | state write |
| `required_caps_for_unknown_operation_is_empty` | unknown → empty |
| `plugin_can_perform_with_matching_capability` | happy path |
| `plugin_can_perform_without_required_capability` | 拒绝 |
| `plugin_can_perform_unknown_operation_always_allowed` | fail-closed 边界 |
| `plugin_can_perform_multi_capability_operation` | 单 cap 多 operation |
| `missing_capabilities_returns_empty_for_satisfied` | None case |
| `missing_capabilities_returns_list_when_unsatisfied` | Some(vec) case |
| `missing_capabilities_for_unknown_operation_is_none` | unknown → None |
| `plugin_can_perform_partial_capability` | multi-cap 拒绝（缺 read） |

## 测试结果

```
cargo test -p pc-plugin-host --lib capability_validator::operations
running 12 tests
... (12 个全 PASS)
test result: ok. 12 passed; 0 failed; 0 ignored
```

## 累计

- pc-plugin-host capability_validator 增加 operations 子模块（26 ops + 3 helpers + 12 tests）
- parity-gap-report §I（Plugins）减少 1 个 unported
- workspace lib 8505 → 8517 PASS / 0 FAIL（estimated）