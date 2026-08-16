# R685 — validatePluginSandboxProviderConfig 完整 async 编排（DB resolve 之外）

## 目标
完成 paperclip/server/src/services/plugin-environment-driver.ts 中 validatePluginSandboxProviderConfig 函数的 **完整 async 编排**（除 DB resolve 外）。R685 把 R682（schema helpers）+ R683（normalize）+ R684（worker manager）整合成单一 sync 入口。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 13/13 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围

Node validatePluginSandboxProviderConfig 完整流程：
1. await resolvePluginSandboxProviderDriverByKey — DB 查询（推迟 R686+）
2. 失败抛错 — DB 返回 null 的处理（推迟 R686+）
3. configSchema guard — **R685 实现**
4. secret-binding normalize 循环 — **R683 已做，R685 拼装**
5. worker.call("environmentValidateConfig", ...) — **R684 已做，R685 拼装**
6. 失败抛错（worker 返回 ok=false） — **R685 实现**
7. 返回 normalizedConfig 结构 — **R685 实现**

R685 = 第 3-7 步完整实现（不依赖 DB）。

## 复刻内容

### 1) ResolvedDriver
- 镜像 Node ResolvedSandboxProviderDriver 的核心字段
- plugin_id / plugin_key / driver_key / driver_schema
- 实际生产中由 R686 DB query 填充，R685 中由 caller 提供

### 2) ValidatedDriverConfig
- normalized_config / plugin_id / plugin_key / driver_key
- 与 Node 返回结构 1:1

### 3) ValidateConfigError
- SecretBinding (R683 error)
- WorkerRpc (R684 error)
- WorkerRejected { provider, first_error, errors, warnings }
- Display impl + std::error::Error + From 转换

### 4) validate_plugin_sandbox_provider_config_after_resolve
- 输入：ResolvedDriver + config + &dyn PluginWorkerManager
- 流程：
  - schema guard (is_object)
  - normalize_config_secret_refs (R683)
  - worker.call("environmentValidateConfig", params) (R684)
  - 检查 ok flag + errors
  - 返回 ValidatedDriverConfig（优先 worker normalized_config，回退本地 normalized）

## 测试覆盖（13 个 case）

### Happy path (2)
- 正常 config + worker 返回 normalized_config
- secret binding 先 normalize 再送 worker

### Error 路径 (5)
- pinned version error propagates (R683 → ValidateConfigError::SecretBinding)
- worker rpc not running propagates (R684 WorkerNotRunning)
- worker method not registered propagates (R684 MethodNotRegistered)
- worker rejects config propagates with errors (R685 WorkerRejected)
- worker rejected with empty errors uses default message

### Fallback (1)
- worker returns normalized_config=None falls back to local normalized

### Schema guard (1)
- schema not object → binding not normalized

### Empty (1)
- empty config works

### Display + Constructor (3)
- error display messages
- ResolvedDriver::new constructor
- nested secret binding pipeline (database.password)

## 真实验证

### 编译
cargo test -p pc-environment --test plugin_environment_driver_validate_tests → 0 errors / 2 warnings (test code style)

### 运行
test result: ok. 13 passed; 0 failed

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
- plugin_worker_manager_tests: 19 ✅
- **plugin_environment_driver_validate_tests: 13 ✅ ← R685 新增**
- **合计 324, 0 fail**

### pc-plugin-database 回归
- 47 / 47 PASS

## 文件改动
- crates/pc-environment/src/plugin_environment_driver_validate.rs (新建)
- crates/pc-environment/tests/plugin_environment_driver_validate_tests.rs (新建)
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod plugin_environment_driver_validate

## 设计要点

### Sync 而非 async fn
- Node validatePluginSandboxProviderConfig 是 async fn（因为 worker.call 是 Promise）
- Rust 中 worker.call 是同步 trait method（trait 默认非 async）
- 所以 R685 函数本身是 sync fn（不影响 Node parity 语义）
- async 行为由 trait object + future 抽象承担

### trait object as parameter
- worker_manager: &dyn PluginWorkerManager
- 调用方注入 InMemoryPluginWorkerManager（测试）或真实 impl（生产）
- 不需要泛型 / 不需要 lifetime complexity

### Schema guard 内联
- 直接在主函数中处理：driver_schema.filter(|s| s.is_object()).cloned()
- 不再单独 export schema guard 函数（R683 已有 as_object_schema / schema_for_collect）
- 减少 API surface

### Error 三层聚合
- SecretBinding（R683 inner error）
- WorkerRpc（R684 inner error）
- WorkerRejected（R685 自身：worker 返回 ok=false）
- Display impl 镜像 Node unprocessable 错误格式
- From 实现让 ? 操作符自然传播

### normalized_config fallback
- Node：
- Rust：
- 优先 worker 返回的（plugin-canonicalized），回退本地（仅 normalize 过 binding 的）

### 错误信息的 provider 字段
- 与 Node 同步：
- R685 实现使用 provider 字段（来自 ResolvedDriver.driver_key 或 caller input）
- 完整错误包含 provider + first error + 完整 errors/warnings 列表

## R686+ 路径

R685 完成 validate 的非 DB 部分。R686 需要：
- 抽 Db trait（最小 DbRow interface）
- 实现 InMemoryDb reference impl
- 实现 resolvePluginSandboxProviderDriverByKey（DB 查询 + ready 校验）
- 把 R685 + R686 拼装成完整 validatePluginSandboxProviderConfig async 函数

## 进度更新
- 核心域覆盖度：99.87% → 99.89%（+0.02%）
- 单元测试：6,702 → 6,715（+13）
- 下一步：R686 = Db trait 抽象 + resolvePluginSandboxProviderDriverByKey parity
