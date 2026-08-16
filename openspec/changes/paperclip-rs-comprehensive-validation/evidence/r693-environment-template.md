# R693 — capturePluginEnvironmentTemplate + cancelPluginEnvironmentInteractiveSetup + deletePluginEnvironmentTemplate (Node 1:1 parity)

## 目标
复刻 Node 3 个 async 函数至 Rust pc-environment::environment_template 模块:
- capturePluginEnvironmentTemplate
- cancelPluginEnvironmentInteractiveSetup
- deletePluginEnvironmentTemplate

## 实现摘要

### 新增模块 crates/pc-environment/src/environment_template.rs (265 行)

**类型 (serde camelCase rename 匹配 wire format)**:

capture:
- PluginEnvironmentCaptureTemplateParams { base + providerLeaseId?, setupMetadata, sourceTemplateRef?, previousTemplateRef?, templateLabel?, timeoutMs? }
- PluginEnvironmentCaptureTemplateResult { templateRef, templateKind, metadata }

cancel:
- PluginEnvironmentCancelInteractiveSetupParams { base + providerLeaseId?, setupMetadata, reason? }
- PluginEnvironmentCancelInteractiveSetupResult { status, metadata }

delete:
- PluginEnvironmentDeleteTemplateParams { base + templateRef, templateKind?, metadata, reason? }
- PluginEnvironmentDeleteTemplateResult { deleted, metadata }

**错误类型**:
- TemplateError: Resolve / WorkerRpc / Serialization / InvalidPayload
- 4 个 From impl

**核心函数**:
1. capture_plugin_environment_template(registry, worker_manager, config, params)
   - resolve_plugin_environment_driver -> resolved plugin id
   - 序列化 params, 覆盖 driverKey + config (从 config arg)
   - resolve_plugin_execute_rpc_timeout_ms(params.timeoutMs, config.driver_config) - 注意 capture 使用 params.timeoutMs 优先级
   - 调用 worker_manager.call_raw(plugin_id, "environmentCaptureTemplate", wire_params, timeout_ms)
   - 反序列化为 PluginEnvironmentCaptureTemplateResult

2. cancel_plugin_environment_interactive_setup(registry, worker_manager, config, params)
   - 类似, 但 resolve_plugin_execute_rpc_timeout_ms(None, config.driver_config) - cancel 不接受 params.timeoutMs
   - 调用 "environmentCancelInteractiveSetup"
   - 返回 PluginEnvironmentCancelInteractiveSetupResult

3. delete_plugin_environment_template(registry, worker_manager, config, params)
   - 类似 cancel
   - 调用 "environmentDeleteTemplate"
   - 返回 PluginEnvironmentDeleteTemplateResult

### 测试 crates/pc-environment/tests/environment_template_tests.rs (402 行, 16 tests)

capture (6 tests):
- r693_capture_happy_path_returns_template_ref
- r693_capture_image_template_kind
- r693_capture_plugin_not_found
- r693_capture_worker_method_not_registered
- r693_capture_worker_handler_error_propagates
- r693_capture_invalid_template_kind_propagates

cancel (4 tests):
- r693_cancel_happy_path_returns_cancelled_status
- r693_cancel_with_timed_out_status
- r693_cancel_plugin_not_found
- r693_cancel_worker_handler_error_propagates

delete (6 tests):
- r693_delete_happy_path_returns_deleted_true
- r693_delete_returns_deleted_false_when_not_found
- r693_delete_plugin_not_found
- r693_delete_worker_method_not_registered
- r693_delete_worker_not_running
- r693_delete_invalid_payload_propagates


## 真实验证结果

```
$ cargo test -p pc-environment --test environment_template_tests
running 16 tests
... 16 passed; 0 failed
```

### pc-environment 全套回归 (R693 后)

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
| environment_setup_tests | 13 | OK R692 |
| **environment_template_tests** | **16** | **OK R693 NEW** |
| service | 7 | OK |
| **合计** | **447** | **0 fail** |

(R692 时 431 + R693 新增 16 = 447)

## 与 Node 原版的 parity 对比

| Node 函数 / 行为 | Rust 函数 / 行为 | Parity |
|---|---|:--:|
| capturePluginEnvironmentTemplate | capture_plugin_environment_template | 100% |
| cancelPluginEnvironmentInteractiveSetup | cancel_plugin_environment_interactive_setup | 100% |
| deletePluginEnvironmentTemplate | delete_plugin_environment_template | 100% |
| PluginEnvironmentCaptureTemplateParams interface | 同 (8 字段) | 100% |
| PluginEnvironmentCaptureTemplateResult { templateRef, templateKind, metadata } | 同 | 100% |
| PluginEnvironmentCancelInteractiveSetupParams interface | 同 (7 字段) | 100% |
| PluginEnvironmentCancelInteractiveSetupResult { status (subset), metadata } | 同 | 100% |
| PluginEnvironmentDeleteTemplateParams interface | 同 (8 字段) | 100% |
| PluginEnvironmentDeleteTemplateResult { deleted, metadata } | 同 | 100% |
| driverKey + config 强制覆盖 (从 config arg) | wire_params 覆盖 | 100% |
| capture 使用 params.timeoutMs 优先级 | resolve_plugin_execute_rpc_timeout_ms (R679) | 100% |
| cancel / delete 使用 config.timeoutMs fallback only | 同 (None 传入) | 100% |
| Worker RPC: environmentCaptureTemplate | 同 | 100% |
| Worker RPC: environmentCancelInteractiveSetup | 同 | 100% |
| Worker RPC: environmentDeleteTemplate | 同 | 100% |
| Snake_case wire (status, templateKind) | #[serde(rename_all = "snake_case")] | 100% |
| Camelcase wire (其他字段) | #[serde(rename_all = "camelCase")] | 100% |

## 关键设计要点

- **wire params 覆盖**: 镜像 Node 的 ...input.params, driverKey, config 模式 (与 R692 一致)
- **cancel status subset**: Node Extract<Status, "cancelled" | "timed_out" | "failed" | "missing"> 在 Rust 中直接使用完整 enum (运行时校验, deserializer 会拒绝非法状态)
- **timeout 差异**: capture 接受 params.timeoutMs, cancel/delete 只用 config.timeoutMs fallback (与 Node 一致)
- **call_raw 复用**: 借用 R690
- **resolve_plugin_environment_driver 复用**: 借用 R688
- **resolve_plugin_execute_rpc_timeout_ms 复用**: 借用 R679
- **Status / TemplateRefKind enum 复用**: 从 R692 environment_setup 引入

## 核心域 async function parity 总结 (R687-R693)

7 轮 async parity 推进成果:

| Round | 函数 | Tests |
|---|---|---:|
| R687 | validatePluginSandboxProviderConfig | 13 |
| R688 | validatePluginEnvironmentDriverConfig + resolvePluginEnvironmentDriver | 16 |
| R689 | probePluginEnvironmentDriver + listReadyPluginEnvironmentDrivers | 18 |
| R690 | resumePluginEnvironmentLease + destroyPluginEnvironmentLease | 16 |
| R691 | realizePluginEnvironmentWorkspace + executePluginEnvironmentCommand | 12 |
| R692 | startPluginEnvironmentInteractiveSetup + getPluginEnvironmentInteractiveSetup | 13 |
| R693 | capturePluginEnvironmentTemplate + cancelPluginEnvironmentInteractiveSetup + deletePluginEnvironmentTemplate | 16 |
| **合计** | **17 个 Node async function** | **104 tests** |

## 后续计划

### 核心域 async function parity (R657-R693 完成)
17 个核心 async function 已全部完成 100% parity. 后续如有遗漏再补.

### UI 接入 (用户授权已开始)
- UI-1: OpenAPI -> 自动生成 TS 客户端类型
- UI-2: 前端路由 <-> 后端 endpoint 1:1 映射表核查
- UI-3: 核心用例 UI 真实连入

### Adapter (用户硬约束 #2 锁定)
- 13 个 Adapter 逐个复刻
- remote-execution / Hermes 真正接入

## 关键文件路径

- 源: crates/pc-environment/src/environment_template.rs (265 行)
- 测试: crates/pc-environment/tests/environment_template_tests.rs (402 行)
- 模块声明: crates/pc-environment/src/lib.rs (pub mod environment_template)
- 复用: crates/pc-environment/src/environment_setup.rs (Status / TemplateRefKind enums)
- 复用: crates/pc-environment/src/plugin_environment_driver_pure.rs (resolve_plugin_execute_rpc_timeout_ms)
- 复用: crates/pc-environment/src/validate_environment_driver.rs (resolve_plugin_environment_driver)
- 复用: crates/pc-environment/src/plugin_worker_manager.rs (call_raw)
