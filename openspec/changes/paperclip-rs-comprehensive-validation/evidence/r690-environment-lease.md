# R690 — resumePluginEnvironmentLease + destroyPluginEnvironmentLease (Node 1:1 parity)

## 目标
复刻 Node resumePluginEnvironmentLease + destroyPluginEnvironmentLease 两个 async 函数至 Rust
pc-environment::environment_lease 模块。

## 实现摘要

### 新增模块 crates/pc-environment/src/environment_lease.rs (180 行)

**结果类型**:
- PluginEnvironmentLease { provider_lease_id?, metadata?, expires_at? } (serde camelCase rename 匹配 wire format)
- PluginEnvironmentLease::from_worker_payload(Value) -> Result<Self, serde_json::Error>

**错误类型**:
- ResumeEnvironmentLeaseError: Resolve / WorkerRpc / InvalidPayload
- DestroyEnvironmentLeaseError: Resolve / WorkerRpc
- 两者都有 From<ResolveEnvironmentDriverError> 和 From<PluginRpcError> 自动传播

**核心函数**:
1. resume_plugin_environment_lease(registry, worker_manager, company_id, environment_id, issue_id?, config, provider_lease_id, lease_metadata?)
   - 复用 R688 resolve_plugin_environment_driver 获取 Resolved
   - 构造 worker RPC params: { driverKey, companyId, environmentId, issueId, config, providerLeaseId, leaseMetadata }
   - 调用 worker_manager.call_raw(plugin_id, "environmentResumeLease", params, None)
   - 通过 from_worker_payload 反序列化为 PluginEnvironmentLease

2. destroy_plugin_environment_lease(registry, worker_manager, company_id, environment_id, issue_id?, config, provider_lease_id?, lease_metadata?)
   - 复用 resolve_plugin_environment_driver
   - 调用 worker_manager.call_raw(plugin_id, "environmentDestroyLease", params, None)
   - 返回 () (void, 与 Node 一致)

### 扩展 PluginWorkerManager trait

新增 call_raw 方法:
- fn call_raw(&self, plugin_id, method, params, timeout_ms) -> Result<Value, PluginRpcError>
- 返回原始 JSON Value (用于 lease 等非结构化 handler)
- InMemoryPluginWorkerManager 增加 RawHandlerFn type alias + register_raw_handler 方法
- WorkerEntry 扩展 raw_handlers HashMap

### 测试 crates/pc-environment/tests/environment_lease_tests.rs (365 行, 16 tests)

resume (8 tests):
- r690_resume_happy_path_returns_lease
- r690_resume_returns_null_provider_lease_id
- r690_resume_plugin_not_found
- r690_resume_plugin_not_ready
- r690_resume_worker_not_running
- r690_resume_worker_method_not_registered
- r690_resume_worker_handler_error_propagates
- r690_resume_invalid_payload_propagates

destroy (7 tests):
- r690_destroy_happy_path_returns_ok
- r690_destroy_with_null_provider_lease_id
- r690_destroy_plugin_not_found
- r690_destroy_worker_not_running
- r690_destroy_worker_method_not_registered
- r690_destroy_worker_handler_error_propagates
- r690_destroy_with_issue_id_and_metadata

helpers (1 test):
- r690_plugin_environment_lease_default_matches_node


## 真实验证结果

```
$ cargo test -p pc-environment --test environment_lease_tests
running 16 tests
... 16 passed; 0 failed
```

### pc-environment 全套回归 (R690 后)

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
| **environment_lease_tests** | **16** | **OK R690 NEW** |
| service | 7 | OK |
| **合计** | **406** | **0 fail** |

(R689 时 390 + R690 新增 16 = 406)

## 与 Node 原版的 parity 对比

| Node 函数 / 行为 | Rust 函数 / 行为 | Parity |
|---|---|:--:|
| resumePluginEnvironmentLease | resume_plugin_environment_lease | 100% |
| destroyPluginEnvironmentLease | destroy_plugin_environment_lease | 100% |
| PluginEnvironmentLease interface | PluginEnvironmentLease struct | 100% |
| Worker RPC method: environmentResumeLease | 同 | 100% |
| Worker RPC method: environmentDestroyLease | 同 | 100% |
| Params: driverKey, companyId, environmentId, issueId, config, providerLeaseId, leaseMetadata | 同 | 100% |
| issueId null fallback | None -> null | 100% |
| providerLeaseId null accepted | Option<String> | 100% |
| expiresAt string ISO-8601 | String | 100% |
| metadata optional Map | Option<Map<String, Value>> | 100% |
| Camelcase wire format | #[serde(rename_all = "camelCase")] | 100% |

## 关键设计要点

- **call_raw 新方法**: 为 lease/destroy 等非结构化 handler 提供原始 JSON 返回值, 与 call() (返回 PluginRpcResult) 并存
- **RawHandlerFn type alias**: 独立的 handler 注册机制, 与现有 HandlerFn (返回 PluginRpcResult) 分开
- **serde camelCase**: PluginEnvironmentLease 字段使用 rename_all = "camelCase" 自动处理 providerLeaseId <-> provider_lease_id 转换
- **From impl**: Resume/Destroy Error 都有 From<ResolveEnvironmentDriverError> + From<PluginRpcError> 自动 ? 传播
- **InvalidPayload**: resume 路径独有, 因为 worker 返回的数据结构不一定有效

## 后续计划

### R691 (候选)
- realizePluginEnvironmentWorkspace + executePluginEnvironmentCommand
- 需要 PluginEnvironmentRealizeWorkspaceParams + PluginEnvironmentRealizeWorkspaceResult types
- 可立即推进 (基于 call_raw 已就绪)

### R692+
- startInteractiveSetup + getInteractiveSetup + captureTemplate + cancelInteractiveSetup + deleteTemplate
- 需要 PluginJobStore trait (持久化交互式 setup sessions)

### UI 接入 (用户授权已开始)
- UI-1: OpenAPI -> 自动生成 TS 客户端类型
- UI-2: 前端路由 <-> 后端 endpoint 1:1 映射表核查
- UI-3: 核心用例 UI 真实连入

### Adapter (用户硬约束 #2 锁定)
- 13 个 Adapter 逐个复刻
- remote-execution / Hermes 真正接入

## 关键文件路径

- 源: crates/pc-environment/src/environment_lease.rs (180 行)
- 测试: crates/pc-environment/tests/environment_lease_tests.rs (365 行)
- 模块声明: crates/pc-environment/src/lib.rs (pub mod environment_lease)
- 扩展: crates/pc-environment/src/plugin_worker_manager.rs (call_raw trait method)
