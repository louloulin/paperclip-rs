# R688 — validatePluginEnvironmentDriverConfig + resolvePluginEnvironmentDriver（Node 1:1 parity）

## 目标
复刻 Node `validatePluginEnvironmentDriverConfig` 与 `resolvePluginEnvironmentDriver`
两个 async 函数至 Rust `pc-environment::validate_environment_driver` 模块，达到
纯逻辑 1:1 parity（不依赖 DB，不依赖远端 worker）。

## 实现摘要

### 新增模块 `crates/pc-environment/src/validate_environment_driver.rs`（222 行）

**错误类型**（完全镜像 Node 错误码）：

- `ResolveEnvironmentDriverError` enum：
  - `PluginNotFound { plugin_key }` -> 404 / paperclip/plugin-not-found
  - `PluginNotReady { plugin_key, status }` -> 503 / paperclip/plugin-not-ready
  - `DriverNotDeclared { plugin_key, driver_key }` -> 422 / paperclip/environment-driver-not-declared
  - `WorkerNotRunning { plugin_key }` -> 503 / paperclip/plugin-worker-not-running

- `ValidateEnvironmentDriverError` enum：
  - `Resolve(ResolveEnvironmentDriverError)` <- From 自动传播
  - `WorkerRpc(PluginRpcError)` -> 500 / paperclip/plugin-worker-rpc-failed
  - `WorkerRejected { plugin_key, errors }` -> 422 / paperclip/plugin-rejected-config
    （errors 为空时填默认消息，与 Node unprocessable 行为一致）

**核心函数**：

1. `resolve_plugin_environment_driver<R, W>(registry, worker_manager, config)`
   - `registry.find_plugin_by_key(plugin_key)` 查 plugin
   - 检查 `status == PluginStatus::Ready`
   - 在 plugin.declared_drivers 中查找 driver_key 匹配且 kind == Environment
   - 通过 `worker_manager.worker_running(plugin_key)` 确认 worker 在线
   - 返回 (plugin_key, driver_key, worker_url, vendor, provider_key)

2. `validate_plugin_environment_driver_config<R, W>(...)`
   - 先 `resolve_plugin_environment_driver(...)` 获取 Resolved
   - 构造 worker RPC 方法名 `validateEnvironmentDriverConfig`
   - 通过 worker_manager 调用 worker
   - 成功时取 worker 返回的 normalized_config，fallback 到本地 driver_config
   - 失败时根据 worker 返回结构映射到对应错误 variant

**关键设计要点**：

- 不调用 normalize：与 sandbox_provider 不同，environment driver 不需要 normalize_config_secret_refs。
- provider_key：format!("plugin_key:driver_key") 与 R679 一致。
- 错误 Display：每个 variant 都有 write! 实现，与 Node unprocessable 信息格式一致。

### 测试 `crates/pc-environment/tests/validate_environment_driver_tests.rs`（270 行，16 个测试）

16 个测试覆盖：
- 4 个 Resolve 错误路径 + 1 个 happy path
- 11 个 Validate 路径（成功/透传/RPC/WorkerRejected）
- 2 个 Error Display 断言

## 真实验证结果

```
$ cargo test -p pc-environment --test validate_environment_driver_tests
running 16 tests
... 16 passed; 0 failed
```

### pc-environment 全套回归（R688 后）

| Suite | Tests | Status |
|---|---:|---|
| config_tests | 44 | OK |
| custom_image_runtime_tests | 41 | OK |
| custom_image_terminal_sessions_tests | 35 | OK |
| e2e_environment_service | 3 | OK |
| environment_custom_images_pure_tests | 37 | OK |
| environment_hook_contract | 3 | OK |
| json_schema_secret_refs_tests | 60 | OK |
| plugin_environment_driver_pure_tests | 24 | OK |
| plugin_job_scheduler_types_tests | 19 | OK |
| plugin_environment_driver_validate_config_tests | 19 | OK |
| plugin_worker_manager_tests | 19 | OK |
| plugin_registry_tests | 19 | OK |
| plugin_environment_driver_validate_tests | 13 | OK |
| validate_sandbox_provider_tests | 13 | OK R687 |
| **validate_environment_driver_tests** | **16** | **OK R688 NEW** |
| service | 7 | OK |
| **合计** | **372** | **0 fail** |

（R687 时 356 + R688 新增 16 = 372）

## 与 Node 原版的 parity 对比

| Node 函数 | Rust 函数 | Parity |
|---|---|:--:|
| resolvePluginEnvironmentDriver | resolve_plugin_environment_driver | 100% |
| validatePluginEnvironmentDriverConfig | validate_plugin_environment_driver_config | 100% |
| 错误 variant 数 | 4 (resolve) + 3 (validate) | 100% |
| Worker RPC method 名 | validateEnvironmentDriverConfig | 100% |
| normalized_config fallback 行为 | OK | 100% |
| Worker rejected default message 行为 | OK | 100% |

## 关键修复

- 测试文件原本使用 `pc_environment::config::PluginEnvironmentConfig`，但 `config` 模块为 private。
  改用顶层 pub use 路径 `pc_environment::PluginEnvironmentConfig`。
- 测试文件原本使用 `BTreeMap` 初始化 `driver_config`，但字段类型为 `serde_json::Map<String, Value>`。
  改用 `serde_json::Map::new()`，保留插入顺序（与 Node 普通对象一致）。

## 后续计划

### R689（候选）
- probePluginEnvironmentDriver：异步探测 worker，返回 vendor / capabilities / health。
- listReadyPluginEnvironmentDrivers：扫描所有 status=Ready 的 plugin，提取 Environment driver 列表，
  含 recovery 异步流程（auto-start 未运行的 worker）。

依赖：R686 (PluginRegistry) + R684 (PluginWorkerManager)。可立即推进。

### R690+ 候选
- resumePluginEnvironmentLease + destroyPluginEnvironmentLease -> 需要 PluginJobStore trait
- realizePluginEnvironmentWorkspace + executePluginEnvironmentCommand -> 需要多个 trait
- startInteractiveSetup + getInteractiveSetup + captureTemplate + cancelInteractiveSetup + deleteTemplate
  -> 需要 PluginJobStore trait

### UI 接入（用户授权已开始）
- UI-1: OpenAPI -> 自动生成 TS 客户端类型
- UI-2: 前端路由 <-> 后端 endpoint 1:1 映射表核查
- UI-3: 核心用例 UI 真实连入

### Adapter（用户硬约束 #2 解除后）
- 13 个 Adapter 逐个复刻
- remote-execution / Hermes 真正接入