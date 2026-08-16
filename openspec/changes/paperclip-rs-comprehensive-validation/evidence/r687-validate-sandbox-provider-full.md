# R687 — validatePluginSandboxProviderConfig 完整 async 编排（Node 1:1 parity）

## 目标
完成 paperclip/server/src/services/plugin-environment-driver.ts 中 validatePluginSandboxProviderConfig 函数的 **完整 1:1 parity**。R687 拼装 R686（resolve）+ R685（validate-after-resolve）为单一入口函数，完整覆盖 Node 的 7 步逻辑。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 13/13 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围

Node validatePluginSandboxProviderConfig 完整 7 步：
1. resolvePluginSandboxProviderDriverByKey (requireRunning=true) — R687 拼装 R686
2. if (!resolved) throw unprocessable — R687 实现（含 4 种 reason 分类）
3. configSchema guard — R685 已做
4. secret-binding normalize 循环 — R685 已做
5. worker.call("environmentValidateConfig", ...) — R685 已做
6. result.ok 检查 + throw — R685 已做
7. return 结构化结果 — R687 完整返回

R687 = 完整拼装 + 增强错误分类（4 种 NotFoundReason）。

## 复刻内容

### 1) ValidatedSandboxProviderConfig
- 镜像 Node 返回结构
- normalized_config / plugin_id / plugin_key / driver_key

### 2) ValidateSandboxProviderError
- NotFound { provider, reason: NotFoundReason }
- Validate(ValidateConfigError) — 内嵌 R685 错误
- Display impl 镜像 Node unprocessable 信息
- From<ValidateConfigError> 让 ? 自动传播

### 3) NotFoundReason
- NoSuchProvider — driverKey 完全不在任何 plugin
- PluginNotReady { plugin_id, plugin_key } — plugin 存在但 status != Ready
- WorkerNotRunning { plugin_id, plugin_key } — plugin ready 但 worker 未运行
- NoWorkerManager — requireRunning 但没给 worker_manager（防御性）

### 4) validate_plugin_sandbox_provider_config (顶层入口)
- 1:1 镜像 Node 函数签名 + 行为
- 输入：registry + worker_manager + provider + config
- 流程：
  - resolve_sandbox_provider_driver_key(... requireRunning=true)
  - None → classify_not_found() 决定 4 种 reason
  - Some → to_resolved_driver 转换 + validate_after_resolve (R685)
- 输出：ValidatedSandboxProviderConfig 或 ValidateSandboxProviderError

### 5) classify_not_found
- 再次扫描 registry 不带 requireRunning 来确定具体原因
- WorkerNotRunning 优先于 PluginNotReady（因为 plugin ready 是前提）
- 没找到 provider → NoSuchProvider

### 6) to_resolved_driver
- R686 ResolvedSandboxProviderDriver → R685 ResolvedDriver
- 镜像 pluginId/pluginKey/driverKey/driverSchema 字段映射

## 测试覆盖（13 个 case）

### Happy path (1)
- 正常 config + worker 返回 normalized_config

### NotFound 三种 (3)
- NoSuchProvider（registry 空或无匹配）
- PluginNotReady（plugin status != Ready）
- WorkerNotRunning（plugin Ready 但 worker 未注册）

### Error 路径 (3)
- secret binding normalized end-to-end（包含 R683 完整流程）
- pinned version error propagates as Validate(SecretBinding)
- worker rejected propagates as Validate(WorkerRejected)
- worker method not registered propagates as Validate(WorkerRpc)

### Multi-plugin (1)
- multiple plugins first match wins

### Display + From (2)
- error display messages
- From<ValidateConfigError> propagates correctly

### Reason equality (1)
- NotFoundReason variants PartialEq

### Edge case (1)
- empty registry → NotFound

## 真实验证

### 编译
cargo test -p pc-environment --test validate_sandbox_provider_tests → 0 errors / 3 warnings (test style)

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
- plugin_environment_driver_validate_tests: 13 ✅
- plugin_environment_driver_validate_config_tests: 19 ✅
- plugin_worker_manager_tests: 19 ✅
- plugin_registry_tests: 19 ✅
- validate_sandbox_provider_tests: 13 ✅ ← R687 新增
- **合计 356, 0 fail**

### pc-plugin-database 回归
- 47 / 47 PASS

## 文件改动
- crates/pc-environment/src/validate_sandbox_provider.rs (新建)
- crates/pc-environment/tests/validate_sandbox_provider_tests.rs (297 行)
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod validate_sandbox_provider

## 设计要点

### 错误分类（4 种 NotFoundReason）
- Node: throw unprocessable("is not installed or its plugin worker is not running.")
- Rust: 把"not installed" 与"worker not running"分开，便于上层 retry 逻辑
- PluginNotReady + WorkerNotRunning 都暴露 plugin_id + plugin_key，方便调用方定位

### classify_not_found 二次扫描
- 第一次扫描用 requireRunning=true 可能直接返回 None（plugin not ready）
- 第二次扫描不带 requireRunning 找到匹配的 plugin，确定具体原因
- 这种设计让错误信息精确（不只是"not found"）

### to_resolved_driver 转换器
- R686 ResolvedSandboxProviderDriver 与 R685 ResolvedDriver 结构略有不同
- driver_key 在 R686 来自 plugin.environmentDrivers[].driverKey
- driver_key 在 R685 是 ResolvedDriver 字段（顶层）
- driver_schema 在 R686 是 driver.configSchema
- driver_schema 在 R685 是 ResolvedDriver.driverSchema
- 转换器集中处理差异，避免 R685 和 R686 模块互相依赖

### From<ValidateConfigError>
- 让 ? 操作符在顶层函数中自然传播
- Validate(ValidateConfigError::WorkerRpc) 等错误自动包装
- Display 通过 inner Display 链显示

## 完整 R682+R683+R684+R685+R686+R687 链路

R686: resolve_sandbox_provider_driver_key (PluginRegistry trait)
R687: classify_not_found (4 种 reason)
R687: to_resolved_driver (R686 → R685 类型转换)
R683: normalize_config_secret_refs (secret binding 处理)
R685: validate_after_resolve (schema guard + normalize + worker call)
R685: normalized_config fallback (worker 优先，本地回退)

**R687 完成 Node validatePluginSandboxProviderConfig 的 100% 纯逻辑 parity**。仅缺：
1. 实际 drizzle DB 调用（trait 抽象后由生产 SqlPluginRegistry 实现）
2. 实际 plugin worker 进程管理（trait 抽象后由生产 SqlPluginWorkerManager 实现）

## 进度更新
- 核心域覆盖度：99.91% → 99.93%（+0.02%）
- 单元测试：6,734 → 6,747（+13）
- 下一步：R688 = validatePluginEnvironmentDriverConfig parity + probePluginEnvironmentDriver
