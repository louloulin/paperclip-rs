# R585 — codex-local staged codex home teardown + Drop guard（G6 收尾）

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

在 `pc-adapter-codex-local::codex_home_staging` 模块新增：

1. **`pub async fn teardown_staged_codex_home(staged_home: &Path)`**
   - 显式删除 staging tmpdir（对齐 Node `fs.rm(stagedCodexHomeDir, {recursive, force})`）
   - 幂等：ENOENT 容忍
   - 其他错误吞掉但 warn（best-effort 永 panic-free）

2. **`pub struct StagedCodexHomeGuard`** — RAII Drop guard
   - `StagedCodexHomeGuard::new(path)` — 创建守卫
   - `path()` — 访问器
   - `disarm()` — 显式 disarm（保留 staging 供调试）
   - `Drop::drop` — 同步 `remove_dir_all`（Drop 不能 await）
   - 错误处理与 `teardown_*` 一致

3. **6 个集成测试** 覆盖 happy + 边界 + Drop 行为

## 2. 设计要点

### 2.1 不引入 tracing 依赖

原 `pc-adapter-codex-local/Cargo.toml` 没有 `tracing` 直接依赖。teardown 警告改用 `eprintln!`（最小依赖增量），符合 "best-effort，永不 panic" 原则。

### 2.2 Drop 必须同步

`Drop::drop` 是 `&mut self`，不能 await。StagedCodexHomeGuard 用 `std::fs::remove_dir_all`（同步）兜底：
- Drop 在 hot path（run 出口）频繁触发
- 同步删除足够快（staging 通常 < 100 个文件）

### 2.3 disarm() 模式

允许上层"保留 staging 供 debug"：
```rust
let guard = StagedCodexHomeGuard::new(staged);
let preserved = guard.disarm();
// Drop 不触发，preserved 路径可被外部消费
```

### 2.4 force=true 语义对齐 Node

`fs.rm(path, {recursive: true, force: true})` 等价于：
- recursive → 递归删子目录（`remove_dir_all` 默认行为）
- force → 容忍 ENOENT（不存在）

Rust 实现完全对齐。

## 3. 测试覆盖

```rust
#[test] fn teardown_removes_staged_home() {}           // happy path
#[test] fn teardown_is_idempotent_on_missing() {}       // ENOENT x 3
#[test] fn teardown_tolerates_permission_errors_on_cleanup() {}  // 先删再 teardown
#[test] fn guard_drop_cleans_up_staged_home() {}        // RAII
#[test] fn guard_disarm_preserves_staged_home() {}      // disarm()
#[test] fn guard_path_accessor_returns_staged_home() {} // accessor
```

**测试结果**：
```
running 6 tests
test guard_path_accessor_returns_staged_home ... ok
test teardown_is_idempotent_on_missing ... ok
test guard_disarm_preserves_staged_home ... ok
test guard_drop_cleans_up_staged_home ... ok
test teardown_tolerates_permission_errors_on_cleanup ... ok
test teardown_removes_staged_home ... ok

test result: ok. 6 passed; 0 failed
```

## 4. 无回归

| 验证项 | 结果 |
|---|---|
| `cargo test -p pc-adapter-codex-local --lib` | ✅ 390 passed (含原有 384 + 新 6) |
| 新加的 `r585` 测试 | ✅ 6/6 passed |
| 编译警告 | 仅原有未使用变量警告（与本 R 无关） |

## 5. 使用模式示例

```rust
use pc_adapter_codex_local::codex_home_staging::{
    stage_codex_home_for_sync, teardown_staged_codex_home, StagedCodexHomeGuard,
};

// 模式 1: 显式 teardown
let staged = stage_codex_home_for_sync(home, opts).await?;
// ... run adapter (staged 是 CODEX_HOME 指向) ...
teardown_staged_codex_home(&staged).await;

// 模式 2: RAII guard（推荐）
let staged = stage_codex_home_for_sync(home, opts).await?;
let _guard = StagedCodexHomeGuard::new(staged);
// ... run adapter ...
// _guard 在 scope 结束时自动 drop → 自动 teardown

// 模式 3: 保留 staging 供调试
let staged = stage_codex_home_for_sync(home, opts).await?;
let guard = StagedCodexHomeGuard::new(staged);
let preserved = guard.disarm();  // 显式 disarm，Drop 不清理
// preserved 可传给 debug 工具 / artifact collector
```

## 6. 与 Node 上游语义对齐

| Node 行为 | Rust 实现 | 状态 |
|---|---|---|
| `fs.rm(stagedCodexHomeDir, {recursive, force})` | `teardown_staged_codex_home` | ✅ 对齐 |
| 容忍 ENOENT（force） | `if e.kind() == NotFound` 静默返回 | ✅ 对齐 |
| 不 panic 失败 | `eprintln!` warn 后吞错 | ✅ 对齐 |
| 失败时不返回错误 | 返回 `()` 而非 `Result` | ✅ 对齐 |
| RAII lifecycle（隐式） | `StagedCodexHomeGuard` Drop | ✅ 增强 |

## 7. G6 剩余工作

- ✅ R585: staged teardown + Drop guard（本次完成）
- ❌ remote_codex_config_dir（仍 TODO；等待 G5 完成后再串接）
- ❌ remote 桥接 server/worker 执行器（更大工作；延后到 R586+）

## 8. 验收清单

- [x] `teardown_staged_codex_home` 公开 API ✅
- [x] `StagedCodexHomeGuard` Drop guard ✅
- [x] 6 集成测试通过 ✅
- [x] 原 384 lib 测试无回归 ✅
- [x] 与 Node `fs.rm` force=true 语义对齐 ✅
- [x] 不引入新依赖（tracing → eprintln）✅
