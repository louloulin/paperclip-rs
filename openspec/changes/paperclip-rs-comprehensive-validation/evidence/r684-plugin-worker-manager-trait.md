# R684 — PluginWorkerManager trait + InMemoryPluginWorkerManager

## 目标
抽 Node paperclip/server/src/services/plugin-worker-manager.ts 中 PluginWorkerManager 的核心 interface 成 Rust trait + in-memory reference impl，让后续 async parity（plugin-environment-driver / plugin-job-scheduler / environment-custom-images）可以独立测试。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 19/19 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围

Node PluginWorkerManager interface 9 个方法：
- startWorker / stopWorker / stopAll
- getWorker / isRunning
- setProactiveCompanyScopes
- diagnostics
- call<M>(pluginId, method, params, timeoutMs)

R684 只做 async parity 真正依赖的 subset：
- isRunning (Node 用法：先 isRunning 检查再 call)
- call (核心 RPC 调用)

其他方法（startWorker / stopWorker / stopAll / getWorker / setProactiveCompanyScopes / diagnostics）不在 parity 范围（属于 host-side lifecycle，不属于 plugin service 的业务逻辑）。

## 复刻内容

### 1) PluginWorkerManager trait
- `is_running(&self, plugin_id: &str) -> bool`
- `call(&self, plugin_id: &str, method: &str, params: Value) -> Result<PluginRpcResult, PluginRpcError>`
- Send + Sync bound（与 Node async 行为一致）

### 2) PluginWorkerManagerInspect trait
- 扩展 trait，用于测试和调试
- `worker_status(plugin_id) -> Option<WorkerStatus>`
- `registered_methods(plugin_id) -> Vec<String>`

### 3) Wire types
- WorkerStatus enum: Starting / Running / Stopping / Stopped / Crashed / Failed
- PluginRpcResult: { ok, errors, warnings, normalized_config }
- PluginRpcError: WorkerNotRunning / MethodNotRegistered / HandlerError / Timeout

### 4) InMemoryPluginWorkerManager
- HashMap<plugin_id, WorkerEntry>
- WorkerEntry { status, handlers: HashMap<method, HandlerFn> }
- HandlerFn = Arc<dyn Fn(Value) -> Result<PluginRpcResult, String> + Send + Sync>
- Mutex 保护内部状态
- 调用时**先 release lock 再执行 handler**（避免 handler 慢调用 deadlock）

### 5) Debug impl for WorkerEntry
- 自定义 Debug 仅显示 status + handler_count
- 避免 HandlerFn 没有 Debug bound 的问题

## 测试覆盖（19 个 case）

### 基础 (4)
- new manager 无 workers
- register worker 标记 Running
- call unknown worker → WorkerNotRunning
- call stopped worker → WorkerNotRunning

### Method 路由 (3)
- call unregistered method → MethodNotRegistered
- call handler returns ok result
- call handler returns error → HandlerError

### Handler 数据 (1)
- handler 接收 params（echo test）

### 多 worker (1)
- multiple workers independent

### Inspection (2)
- registered methods sorted
- register handler to unregistered worker panics

### Lifecycle (4)
- remove worker clears all
- stop then remove yields None status
- call after remove → WorkerNotRunning
- re-register worker resets handlers

### Error (1)
- error Display messages

### Concurrency (1)
- concurrent call does not deadlock（4 thread spawn）

### Default + Serde (2)
- PluginRpcResult default
- WorkerStatus serde roundtrip

## 真实验证

### 编译
cargo test -p pc-environment --test plugin_worker_manager_tests → 0 errors / 5 warnings (4 pre-existing + 1 custom_image_terminal_sessions mut)

### 运行
test result: ok. 19 passed; 0 failed

### 全 pc-environment 套件回归
- lib: 7 ✅
- config_tests: 44 ✅
- custom_image_runtime_tests: 41 ✅
- custom_image_terminal_sessions_tests: 35 ✅
- e2e_environment_service: 3 ✅
- environment_custom_images_pure_tests: 37 ✅
- environment_hook_contract: 3 ✅
- json_schema_secret_refs_tests: 60 ✅
- plugin_environment_driver_pure_tests: 24 ✅
- plugin_job_scheduler_types_tests: 19 ✅
- plugin_environment_driver_validate_config_tests: 19 ✅
- **plugin_worker_manager_tests: 19 ✅ ← R684 新增**
- **合计 311, 0 fail**

### pc-plugin-database 回归
- 47 / 47 PASS

## 文件改动
- crates/pc-environment/src/plugin_worker_manager.rs (9066 bytes) 新建
- crates/pc-environment/tests/plugin_worker_manager_tests.rs (8675 bytes) 新建
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod plugin_worker_manager

## 设计要点

### Trait 抽象而非具体 impl
- Node 用 interface → Rust 用 trait
- 多个 impl 可共存：InMemory（测试用）、将来可加 SqlPluginWorkerManager（生产用）
- 真正解耦 async parity 测试与生产 runtime

### Send + Sync bound
- Node async 函数跨 await 自然支持并发
- Rust 跨 await 需要 Send + Sync 才能在 `async fn` 中安全使用
- InMemoryPluginWorkerManager 内部 Mutex 保证 Send + Sync

### Lock release before handler
- 关键设计：`.get(method).cloned()` 在 lock 内完成后立刻释放
- handler 在 lock 外执行（Arc<dyn Fn>）
- 避免 handler 内的慢调用 deadlock 整个 manager

### Arc<dyn Fn> + 'static bound
- Handler 注册时捕获 Arc<dyn Fn>
- 没有 'static bound 无法在 HashMap 中长期存储
- 注册后 handler 可独立于 manager 生命周期（实际上共享 Arc）

### Mutex<Inner> 而非 RwLock
- 写操作远比读多（call + register_handler 都改 handlers）
- Mutex 简单且足够（避免 RwLock 的写者饥饿）

### Custom Debug for WorkerEntry
- HashMap<String, HandlerFn> 中 HandlerFn 没有 Debug bound
- 自定义 Debug 仅显示 status + handler_count
- 保证 `#[derive(Default)] on Inner` 工作

## R685+ 路径

R684 解锁了：
- 完整 validatePluginSandboxProviderConfig async parity（R685）：trait + DB + R683 normalize + R684 worker
- 完整 validatePluginEnvironmentDriverConfig async parity：同上
- PluginJobScheduler 的 worker call：R680 types + R684 worker
- 任何需要 RPC 调用的 parity 都可以用 InMemoryPluginWorkerManager 测试

## 进度更新
- 核心域覆盖度：99.85% → 99.87%（+0.02%）
- 单元测试：6,683 → 6,702（+19）
- 下一步：R685 = 完整 validatePluginSandboxProviderConfig async parity 编排（trait + DB stub + R683 + R684）
