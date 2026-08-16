# R692 — startPluginEnvironmentInteractiveSetup + getPluginEnvironmentInteractiveSetup (Node 1:1 parity)

## 目标
复刻 Node startPluginEnvironmentInteractiveSetup + getPluginEnvironmentInteractiveSetup 两个 async 函数至 Rust
pc-environment::environment_setup 模块。两者都需要在 wire params 中强制覆盖 driverKey + config (从 config arg)。

## 实现摘要

### 新增模块 crates/pc-environment/src/environment_setup.rs (258 行)

**Enums (mirror Node union types, snake_case wire)**:
- PluginEnvironmentInteractiveSetupStatus: Starting / WaitingForUser / Capturing / Promoted / Cancelled / TimedOut / Failed / Missing
- PluginEnvironmentTemplateRefKind: Snapshot / Image / ProviderTemplate / Unknown

**Connection types**:
- PluginEnvironmentInteractiveSetupConnectionSummary { type, username?, hostRedacted, portRedacted, commandRedacted?, expiresAt?, metadata }
- PluginEnvironmentInteractiveSetupConnectionPayload { type, command?, token?, expiresAt?, metadata }

**Session type**:
- PluginEnvironmentInteractiveSetupSession { providerLeaseId, status, connectionSummary, connectionPayload?, expiresAt?, metadata }

**Params (mirror Node Start/Get InteractiveSetup Params)**:
- PluginEnvironmentStartInteractiveSetupParams { base + sessionId, sourceTemplateRef?, sourceTemplateKind?, connectionExpiresInMinutes?, expiresAt? }
- PluginEnvironmentGetInteractiveSetupParams { base + providerLeaseId?, setupMetadata, includeConnectionPayload?, connectionExpiresInMinutes? }

**错误类型**:
- SetupError: Resolve / WorkerRpc / Serialization / InvalidPayload
- 4 个 From impl 让 ? 自然传播

**核心函数**:
1. start_plugin_environment_interactive_setup(registry, worker_manager, config, params)
   - 复用 R688 resolve_plugin_environment_driver 获取 Resolved
   - 序列化 params 为 Value, 然后覆盖 driverKey + config (从 config arg)
   - 调用 resolve_plugin_execute_rpc_timeout_ms(None, config.driver_config) 决定 timeout
   - 调用 worker_manager.call_raw(plugin_id, "environmentStartInteractiveSetup", wire_params, timeout_ms)
   - 反序列化为 PluginEnvironmentInteractiveSetupSession

2. get_plugin_environment_interactive_setup(registry, worker_manager, config, params)
   - 同上结构, 但调用 "environmentGetInteractiveSetup"

### 测试 crates/pc-environment/tests/environment_setup_tests.rs (366 行, 13 tests)

start (6 tests):
- r692_start_happy_path_returns_session
- r692_start_with_status_starting
- r692_start_plugin_not_found
- r692_start_worker_method_not_registered
- r692_start_invalid_payload_propagates
- r692_start_overrides_driver_key_and_config

get (7 tests):
- r692_get_happy_path_returns_session
- r692_get_with_null_provider_lease_id
- r692_get_with_connection_payload
- r692_get_plugin_not_found
- r692_get_worker_not_running
- r692_get_worker_handler_error_propagates
- r692_get_with_config_timeout_fallback


## 真实验证结果

```
$ cargo test -p pc-environment --test environment_setup_tests
running 13 tests
... 13 passed; 0 failed
```

### pc-environment 全套回归 (R692 后)

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
| probe_environment_driver_tests | 18 | OK R689 |
| environment_lease_tests | 16 | OK R690 |
| environment_workspace_tests | 12 | OK R691 |
| **environment_setup_tests** | **13** | **OK R692 NEW** |
| service | 7 | OK |
| **合计** | **431** | **0 fail** |

(R691 时 418 + R692 新增 13 = 431)

## 与 Node 原版的 parity 对比

| Node 函数 / 行为 | Rust 函数 / 行为 | Parity |
|---|---|:--:|
| startPluginEnvironmentInteractiveSetup | start_plugin_environment_interactive_setup | 100% |
| getPluginEnvironmentInteractiveSetup | get_plugin_environment_interactive_setup | 100% |
| PluginEnvironmentInteractiveSetupStatus union | PluginEnvironmentInteractiveSetupStatus enum (8 variant) | 100% |
| PluginEnvironmentTemplateRefKind union | PluginEnvironmentTemplateRefKind enum (4 variant) | 100% |
| PluginEnvironmentInteractiveSetupConnectionSummary | 同 (含 type 字段 rename) | 100% |
| PluginEnvironmentInteractiveSetupConnectionPayload | 同 | 100% |
| PluginEnvironmentInteractiveSetupSession | 同 | 100% |
| PluginEnvironmentStartInteractiveSetupParams | 同 (含 7 个字段) | 100% |
| PluginEnvironmentGetInteractiveSetupParams | 同 (含 8 个字段) | 100% |
| driverKey + config 强制覆盖 (从 config arg) | wire_params 覆盖 | 100% |
| Snake_case wire format (status, templateKind) | #[serde(rename_all = "snake_case")] | 100% |
| type 字段 rename | #[serde(rename = "type")] | 100% |
| Worker RPC method: environmentStartInteractiveSetup | 同 | 100% |
| Worker RPC method: environmentGetInteractiveSetup | 同 | 100% |
| Timeout: config.timeoutMs fallback | resolve_plugin_execute_rpc_timeout_ms (R679) | 100% |
| Camelcase wire format (其他字段) | #[serde(rename_all = "camelCase")] | 100% |

## 关键设计要点

- **wire params 覆盖**: 镜像 Node 的 ...input.params, driverKey: input.config.driverKey, config: input.config.driverConfig 模式
- **enum status snake_case**: Node union 转换为 Rust enum + #[serde(rename_all = "snake_case")] 保持 wire format 1:1
- **type 字段 rename**: 使用 #[serde(rename = "type")] 因为 Rust 不允许字段名为关键字
- **call_raw 复用**: 借用 R690 的 call_raw 抽象
- **resolve_plugin_environment_driver 复用**: 借用 R688
- **resolve_plugin_execute_rpc_timeout_ms 复用**: 借用 R679

## 后续计划

### R693 (候选)
- capturePluginEnvironmentTemplate + cancelInteractiveSetup + deleteTemplate
- 类型: PluginEnvironmentCaptureTemplateParams / Result, PluginEnvironmentCancelInteractiveSetupParams / Result, PluginEnvironmentDeleteTemplateParams / Result
- 复用 environment_setup 模块, 但需要扩展 SetupError 或新增 TemplateError
- 预计 ~15 tests

### UI 接入 (用户授权已开始)
- UI-1: OpenAPI -> 自动生成 TS 客户端类型
- UI-2: 前端路由 <-> 后端 endpoint 1:1 映射表核查
- UI-3: 核心用例 UI 真实连入

### Adapter (用户硬约束 #2 锁定)
- 13 个 Adapter 逐个复刻
- remote-execution / Hermes 真正接入

## 关键文件路径

- 源: crates/pc-environment/src/environment_setup.rs (258 行)
- 测试: crates/pc-environment/tests/environment_setup_tests.rs (366 行)
- 模块声明: crates/pc-environment/src/lib.rs (pub mod environment_setup)
- 复用: crates/pc-environment/src/plugin_environment_driver_pure.rs (resolve_plugin_execute_rpc_timeout_ms)
- 复用: crates/pc-environment/src/validate_environment_driver.rs (resolve_plugin_environment_driver)
- 复用: crates/pc-environment/src/plugin_worker_manager.rs (call_raw)
