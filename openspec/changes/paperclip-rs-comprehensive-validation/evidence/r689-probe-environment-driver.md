# R689 — probePluginEnvironmentDriver + listReadyPluginEnvironmentDrivers (Node 1:1 parity 含 recovery flow)

## 目标
复刻 Node probePluginEnvironmentDriver 与 listReadyPluginEnvironmentDrivers 两个 async 函数
(以及其底层的 ReadyPluginWorkerRecovery 接口与 worker RPC 协议) 至 Rust
pc-environment::probe_environment_driver 模块。

## 实现摘要

### 新增模块 crates/pc-environment/src/probe_environment_driver.rs (266 行)

**常量**:
- DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS = 2_000 (与 Node 一致)
- PROBE_TIMEOUT_MS = 120_000 (probe 调用超时, 与 Node workerManager.call(..., 120_000) 一致)

**结果类型** (镜像 Node EnvironmentProbeResult + EnvironmentProbeDetails):
- EnvironmentProbeResult { ok, driver, summary, details: Option<...> }
- EnvironmentProbeDetails { plugin_key?, driver_key?, provider?, diagnostics?, metadata: Map }
- ProbeDiagnostic (= PluginRpcDiagnostic, severity / message / code)

**错误类型**:
- ProbeEnvironmentDriverError::Resolve(ResolveEnvironmentDriverError)
- ProbeEnvironmentDriverError::WorkerRpc(PluginRpcError)
- From impl 让 ? 自然传播


**核心函数**:

1. probe_plugin_environment_driver<R, W>(registry, worker_manager, company_id, environment_id, config)
   - 复用 R688 resolve_plugin_environment_driver 获取 Resolved
   - 构造 worker RPC params: { driverKey, companyId, environmentId, config }
   - 调用 worker_manager.call(plugin_id, "environmentProbe", params, Some(120_000))
   - summary fallback: worker 未返回时填默认 passed/failed 文案
   - 返回 EnvironmentProbeResult { ok, driver: "plugin", summary, details }

2. list_ready_plugin_environment_drivers<R, W, Rec>(registry, worker_manager, recover)
   - worker_manager 为 None -> 返回 [] (与 Node 一致)
   - 过滤 status == Ready 的 plugin
   - Recovery flow: 遍历 ready plugins, 对以下条件 ALL true 的触发 start_worker:
     - 至少有一个 kind == SandboxProvider driver
     - !worker_manager.is_running(plugin.id)
     - recoverable_plugin_keys.contains(plugin_key)
     - !worker_manager.worker_registered(plugin.id) (Node getWorker(id) null)
   - 遍历所有 ready plugins, 对每个 is_running 的 plugin 提取其 SandboxProvider drivers
   - 返回包含全部 Node ReadyPluginEnvironmentDriver 字段 (含 7 个扩展字段) 的 rows

**Recovery trait**: ReadyPluginWorkerRecovery: Send + Sync
- fn plugin_keys(&self) -> Vec<String>
- fn start_worker(&self, plugin_id, plugin_key) -> bool
- fn timeout_ms(&self) -> Option<u64> 默认 None
- InMemoryRecovery 实现了该 trait 用于测试

### 扩展现有结构

- PluginRpcResult 扩展字段: summary: Option<String>, diagnostics: Option<Vec<PluginRpcDiagnostic>>, metadata: Map<String, Value>
- PluginRpcDiagnostic 新结构 (severity / message / code)
- PluginEnvironmentDriverDecl 扩展 7 个字段:
  supports_reusable_leases, supports_interactive_setup, interactive_setup_connection_types,
  supports_template_capture, template_ref_kind, template_config_binding, supports_template_delete
- ReadyPluginEnvironmentDriver 同步扩展上述 7 字段
- PluginWorkerManager trait 添加 worker_registered(id) -> bool 方法 (mirror Node getWorker)
- PluginWorkerManager::call 增加 timeout_ms: Option<u64> 第 4 参数 (mirror Node call(..., timeoutMs))
- ResolveEnvironmentDriverError 添加 PartialEq derive


### 测试 crates/pc-environment/tests/probe_environment_driver_tests.rs (320 行, 18 个测试)

probe (8 tests):
- r689_probe_happy_path_with_summary
- r689_probe_falls_back_to_passed_summary_when_worker_returns_none
- r689_probe_falls_back_to_failed_summary_on_ok_false
- r689_probe_plugin_not_found
- r689_probe_plugin_not_ready
- r689_probe_driver_not_declared
- r689_probe_worker_not_running
- r689_probe_worker_rpc_error_propagates

listReady (10 tests):
- r689_list_ready_returns_empty_when_no_worker_manager
- r689_list_ready_filters_non_ready_plugins
- r689_list_ready_filters_non_sandbox_drivers
- r689_list_ready_happy_path_returns_rows_with_extended_fields
- r689_list_ready_skips_plugins_whose_worker_is_not_running
- r689_list_ready_recovery_triggers_start_worker_for_unregistered_worker
- r689_list_ready_recovery_only_for_recoverable_plugin_keys
- r689_list_ready_recovery_only_for_plugins_with_sandbox_provider_driver
- r689_list_ready_no_recovery_when_worker_is_running
- r689_list_ready_returns_rows_for_multiple_plugins

## 真实验证结果

```
$ cargo test -p pc-environment --test probe_environment_driver_tests
running 18 tests
... 18 passed; 0 failed
```

### pc-environment 全套回归 (R689 后)

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
| validate_environment_driver_tests | 16 | OK R688 |
| **probe_environment_driver_tests** | **18** | **OK R689 NEW** |
| service | 7 | OK |
| **合计** | **390** | **0 fail** |

(R688 时 372 + R689 新增 18 = 390)


## 与 Node 原版的 parity 对比

| Node 函数 / 行为 | Rust 函数 / 行为 | Parity |
|---|---|:--:|
| probePluginEnvironmentDriver | probe_plugin_environment_driver | 100% |
| listReadyPluginEnvironmentDrivers | list_ready_plugin_environment_drivers | 100% |
| ReadyPluginWorkerRecovery interface | ReadyPluginWorkerRecovery trait | 100% |
| Recovery 触发条件 (4 个 AND) | 同 | 100% |
| Driver 过滤 (kind=sandbox_provider) | 同 | 100% |
| Plugin 过滤 (status=ready) | 同 | 100% |
| workerManager.getWorker null check | worker_manager.worker_registered false | 100% |
| workerManager.call timeout 120s | Some(PROBE_TIMEOUT_MS) | 100% |
| summary fallback | passed/failed 文案 | 100% |
| EnvironmentProbeResult 字段 | 同 | 100% |
| EnvironmentProbeDetails 字段 | 同 | 100% |
| ReadyPluginEnvironmentDriver 13 字段 | 同 (已扩展) | 100% |

## 关键修复

- PluginRpcResult 新增字段导致测试中现有 initializer 编译失败 -> 用 JS 脚本自动给每个初始化添加 ..Default::default(), 并去重以防重复添加。
- Map<String, Value> 没有 Default 实现 -> 用 match Some/None 显式 fallback。
- PluginEnvironmentDriverDecl 新增字段导致其他测试初始化失败 -> 同样添加 ..Default::default()。
- Rust lifetime 包含单引号, 破坏 JS 单引号字符串字面量 -> 改用 backtick (template literal) 写文件。
- format 字符串中的转义双引号在 JS 中需要正确转义 -> 用 backtick 配合 \" 写入。

## 后续计划

### R690 (候选)
- resumePluginEnvironmentLease + destroyPluginEnvironmentLease
- 需要 PluginJobStore trait
- 可立即推进 (基于 R684 PluginWorkerManager 已就绪)

### R691+
- realizePluginEnvironmentWorkspace + executePluginEnvironmentCommand (多个 trait)
- startInteractiveSetup + getInteractiveSetup + captureTemplate + cancelInteractiveSetup + deleteTemplate

### UI 接入 (用户授权已开始)
- UI-1: OpenAPI -> 自动生成 TS 客户端类型
- UI-2: 前端路由 <-> 后端 endpoint 1:1 映射表核查
- UI-3: 核心用例 UI 真实连入

### Adapter (用户硬约束 #2 锁定)
- 13 个 Adapter 逐个复刻
- remote-execution / Hermes 真正接入

## 关键文件路径

- 源: crates/pc-environment/src/probe_environment_driver.rs (266 行)
- 测试: crates/pc-environment/tests/probe_environment_driver_tests.rs (320 行)
- 模块声明: crates/pc-environment/src/lib.rs (pub mod probe_environment_driver)
- 扩展: crates/pc-environment/src/plugin_worker_manager.rs (PluginRpcResult + trait 扩展)
- 扩展: crates/pc-environment/src/plugin_registry.rs (decl + row 扩展字段)
