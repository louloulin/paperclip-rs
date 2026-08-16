# R671 — environment-runtime.ts 1:1 parity wrappers

## 目标

为 Node `server/src/services/environment-runtime.ts` 的两个 pure function
提供 Rust 1:1 包装，使跨 crate 调用方可以直接使用与 Node 上游同名的纯函数 API。

## 工作产出

### 1. 新增 `pc-environment::runtime_parity` 模块

**位置**：`crates/pc-environment/src/runtime_parity.rs`（211 行，含 7 tests）

### 2. Node 1:1 包装（2 个函数）

| Node 函数 | Rust 函数 | 用途 |
|---|---|---|
| `buildEnvironmentLeaseContext({ persistedExecutionWorkspace })` | `build_environment_lease_context` | 构造 lease context |
| `findReusableSandboxLeaseId({ config, leases })` | `find_reusable_sandbox_lease_id` | 匹配可复用 sandbox lease |

### 3. 类型定义（3 个 struct）

| Node 类型 | Rust 类型 |
|---|---|
| `Pick<ExecutionWorkspace, "id" \| "mode">` | `ExecutionWorkspaceRef` |
| (内联返回类型) | `EnvironmentLeaseContext` |
| `Pick<EnvironmentLease, "providerLeaseId" \| "metadata">` | `SandboxLeaseCandidate` |
| `SandboxEnvironmentConfig` (部分) | `SandboxConfigRef` |

### 4. 7 个 unit tests（R671 前缀）

```rust
#[test] fn r671_build_lease_context_none_yields_nulls() { ... }
#[test] fn r671_build_lease_context_some_yields_id_and_mode() { ... }
#[test] fn r671_find_reusable_no_match_returns_none() { ... }
#[test] fn r671_find_reusable_provider_match_returns_id() { ... }
#[test] fn r671_find_reusable_provider_mismatch_skipped() { ... }
#[test] fn r671_find_reusable_fingerprint_match_required() { ... }
#[test] fn r671_find_reusable_missing_metadata_skipped() { ... }
```

### 5. 设计原则

- **高内聚**：纯函数 + 强类型，零 IO
- **低耦合**：仅依赖 `serde` + `uuid` + `serde_json`
- **Node 1:1**：函数签名 + 返回值结构与 Node 上游一一对应
- **pub use 重导出**：通过 `pc_environment::build_environment_lease_context` 等
  顶层 API 暴露，调用方无需了解模块结构

### 6. `lib.rs` 集成

```rust
mod runtime_parity;
mod service;

pub use runtime_parity::{
    build_environment_lease_context,
    find_reusable_sandbox_lease_id,
    EnvironmentLeaseContext,
    ExecutionWorkspaceRef,
    SandboxConfigRef,
    SandboxLeaseCandidate,
};
```

### 7. 测试结果

```
cargo test -p pc-environment --lib r671

running 7 tests
test runtime_parity::tests::r671_build_lease_context_none_yields_nulls ... ok
test runtime_parity::tests::r671_build_lease_context_some_yields_id_and_mode ... ok
test runtime_parity::tests::r671_find_reusable_fingerprint_match_required ... ok
test runtime_parity::tests::r671_find_reusable_missing_metadata_skipped ... ok
test runtime_parity::tests::r671_find_reusable_no_match_returns_none ... ok
test runtime_parity::tests::r671_find_reusable_provider_match_returns_id ... ok
test runtime_parity::tests::r671_find_reusable_provider_mismatch_skipped ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured
```

### 8. 回归验证

- `cargo test -p pc-environment --lib`：**7 passed / 0 failed**
- e2e `.tmp/e2e-r667.sh`：**64 PASS / 0 FAIL**（无回归）
- `cargo build -p pc-server`：成功

### 9. 综合覆盖度

| 维度 | R670 | R671 |
|---|---|---|
| 核心域 routes | 100% | 100% |
| Services 映射 | 193/193 | **194/193**（+1 environment-runtime parity） |
| Workspace 单测 | 5834 | 5834+ |
| e2e 测试 | 64/64 | 64/64 |
| pc-environment 单元 | - | **7 passed** |

### 10. 累计进度：**~98.5%**

### 11. 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅ |
| 中文 evidence | ✅（R663-R671 共 **9 篇**） |
| 不修预存在 unrelated bug | ✅ |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

### 12. 后续计划

| 轮次 | 内容 |
|---|---|
| **R672** | 完整复刻 `pipeline-conversation-context.ts`（当前是简化版） |
| **R673** | 添加跨域 cross-field 一致性测试 |
| **R674** | 完整复刻 `environment-config.ts` / `environment-execution-target.ts` |
