# R691 — realizePluginEnvironmentWorkspace + executePluginEnvironmentCommand (Node 1:1 parity)

## 目标
复刻 Node realizePluginEnvironmentWorkspace + executePluginEnvironmentCommand 两个 async 函数至 Rust
pc-environment::environment_workspace 模块。两者都接受 plugin_id 可选参数, 若提供则跳过 resolve。

## 实现摘要

### 新增模块 crates/pc-environment/src/environment_workspace.rs (210 行)

**类型 (serde camelCase rename 匹配 wire format)**:
- PluginEnvironmentWorkspaceSpec { local_path?, remote_path?, mode?, metadata: Map }
- PluginEnvironmentRealizeWorkspaceParams { driver_key, company_id, environment_id, issue_id?, config, lease, workspace }
- PluginEnvironmentRealizeWorkspaceResult { cwd, metadata: Map }
- PluginEnvironmentExecuteParams { driver_key, company_id, environment_id, issue_id?, config, lease, command, args?, cwd?, env: HashMap<String, String>, stdin?, timeout_ms? }
- PluginEnvironmentExecuteResult { exit_code?, signal?, timed_out }

**错误类型**:
- WorkspaceError: Resolve / WorkerRpc / InvalidPayload / Serialization
- 4 个 From impl 让 ? 自然传播

**核心函数**:
1. realize_plugin_environment_workspace(registry, worker_manager, plugin_id?, params, config)
   - resolve_plugin_id 辅助函数: 若 plugin_id 显式提供则使用, 否则调用 resolve_plugin_environment_driver
   - 调用 worker_manager.call_raw(plugin_id, "environmentRealizeWorkspace", params.to_value(), None)
   - 反序列化为 PluginEnvironmentRealizeWorkspaceResult

2. execute_plugin_environment_command(registry, worker_manager, plugin_id?, params, config)
   - 复用 resolve_plugin_id 辅助
   - 调用 resolve_plugin_execute_rpc_timeout_ms(R679 pure 函数) 决定 timeout
   - 调用 worker_manager.call_raw(plugin_id, "environmentExecute", params.to_value(), timeout_ms)
   - 反序列化为 PluginEnvironmentExecuteResult

### 测试 crates/pc-environment/tests/environment_workspace_tests.rs (334 行, 12 tests)

realize (6 tests):
- r691_realize_happy_path_returns_cwd
- r691_realize_uses_explicit_plugin_id_when_provided
- r691_realize_plugin_not_found
- r691_realize_worker_not_running
- r691_realize_worker_method_not_registered
- r691_realize_invalid_payload_propagates

execute (6 tests):
- r691_execute_happy_path_returns_result
- r691_execute_uses_explicit_plugin_id
- r691_execute_timed_out_result
- r691_execute_plugin_not_found
- r691_execute_worker_handler_error_propagates
- r691_execute_with_config_timeout_fallback


## 真实验证结果

```
$ cargo test -p pc-environment --test environment_workspace_tests
running 12 tests
... 12 passed; 0 failed
```

### pc-environment 全套回归 (R691 后)

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
| **environment_workspace_tests** | **12** | **OK R691 NEW** |
| service | 7 | OK |
| **合计** | **418** | **0 fail** |

(R690 时 406 + R691 新增 12 = 418)

## 与 Node 原版的 parity 对比

| Node 函数 / 行为 | Rust 函数 / 行为 | Parity |
|---|---|:--:|
| realizePluginEnvironmentWorkspace | realize_plugin_environment_workspace | 100% |
| executePluginEnvironmentCommand | execute_plugin_environment_command | 100% |
| PluginEnvironmentRealizeWorkspaceParams interface | 同 (含嵌套 lease + workspace) | 100% |
| PluginEnvironmentRealizeWorkspaceResult { cwd, metadata } | 同 | 100% |
| PluginEnvironmentExecuteParams interface | 同 (含 lease + command + args + cwd + env + stdin + timeoutMs) | 100% |
| PluginEnvironmentExecuteResult { exitCode, signal?, timedOut } | 同 | 100% |
| plugin_id 可选短路 resolve | resolve_plugin_id helper | 100% |
| Worker RPC method: environmentRealizeWorkspace | 同 | 100% |
| Worker RPC method: environmentExecute | 同 | 100% |
| Timeout 解析: requestedTimeoutMs > config.timeoutMs > undefined | resolve_plugin_execute_rpc_timeout_ms (R679) + RPC_OVERHEAD_BUFFER_MS | 100% |
| Camelcase wire format | #[serde(rename_all = "camelCase")] | 100% |

## 关键设计要点

- **resolve_plugin_id 辅助函数**: 若 plugin_id 显式提供则使用, 否则复用 R688 resolve_plugin_environment_driver, 与 Node 三元表达式 1:1
- **call_raw 复用**: 借用 R690 的 call_raw 抽象, 避免重复 trait 扩展
- **resolve_plugin_execute_rpc_timeout_ms 复用**: 借用 R679 的 pure 函数, 实现 timeoutMs 优先级解析
- **HashMap vs Map<String, Value>**: env 字段使用 HashMap<String, String> 而非 serde_json::Map, 因为 serde_json::Map<String, String> 在 Rust 标准 derive macro 下不实现 Debug/Clone/Serialize/Deserialize 等 trait (取决于 BTreeMap vs IndexMap 的特定实例化)
- **InvalidPayload / Serialization 错误**: 与 R690 保持一致的双错误模式, 让 worker 返回值与序列化错误分离

## 后续计划

### R692 (候选)
- startInteractiveSetup + getInteractiveSetup + captureTemplate + cancelInteractiveSetup + deleteTemplate
- 需要 PluginJobStore trait (持久化交互式 setup sessions)
- 5 个函数, 工作量较大, 建议分 2 轮完成: R692 = start + get, R693 = capture + cancel + delete

### R693+
- R693: captureTemplate + cancelInteractiveSetup + deleteTemplate
- 可能需要 PluginEnvironmentTemplate type 引入

### UI 接入 (用户授权已开始)
- UI-1: OpenAPI -> 自动生成 TS 客户端类型
- UI-2: 前端路由 <-> 后端 endpoint 1:1 映射表核查
- UI-3: 核心用例 UI 真实连入

### Adapter (用户硬约束 #2 锁定)
- 13 个 Adapter 逐个复刻
- remote-execution / Hermes 真正接入

## 关键文件路径

- 源: crates/pc-environment/src/environment_workspace.rs (210 行)
- 测试: crates/pc-environment/tests/environment_workspace_tests.rs (334 行)
- 模块声明: crates/pc-environment/src/lib.rs (pub mod environment_workspace)
- 复用: crates/pc-environment/src/plugin_environment_driver_pure.rs (resolve_plugin_execute_rpc_timeout_ms)
- 复用: crates/pc-environment/src/environment_lease.rs (PluginEnvironmentLease)
- 复用: crates/pc-environment/src/plugin_worker_manager.rs (call_raw)
