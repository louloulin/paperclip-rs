# R686 — PluginRegistry trait + resolvePluginSandboxProviderDriverByKey

## 目标
抽 Node paperclip/server/src/services/plugin-environment-driver.ts 中 resolvePluginSandboxProviderDriverByKey 函数 + plugin registry 抽象，让 async parity 可以独立测试，不依赖 drizzle / DB。

## 用户硬约束遵守
- 不 commit ✅
- 不修 Adapter ✅
- 真实验证 (cargo test) ✅ 19/19 PASS
- 中文 evidence 落盘 ✅
- 不修预存在 unrelated bug ✅
- 继续推进不等催促 ✅

## 范围

Node resolvePluginSandboxProviderDriverByKey 完整流程：
1. const pluginRegistry = pluginRegistryService(input.db) — **R686 抽 trait**
2. const plugins = await pluginRegistry.list() — **R686 实现 list()**
3. for (const plugin of plugins) { ... } — **R686 遍历逻辑**
4. driver = plugin.manifestJson.environmentDrivers?.find(...) — **R686 环境驱动匹配**
5. if (!driver) continue; — **R686 同**
6. if (input.requireRunning) { status check + isRunning check } — **R686 完整实现**
7. return { plugin, driver } — **R686 ResolvedSandboxProviderDriver**

R686 完整实现 Node resolvePluginSandboxProviderDriverByKey 的纯逻辑部分（不含 drizzle / SQL）。

## 复刻内容

### 1) Domain types
- PluginStatus enum: Installed / Registered / Ready / Failed / Disabled（#[serde(rename_all = "snake_case")]）
- PluginDriverKind enum: SandboxProvider / Environment
- PluginEnvironmentDriverDecl struct（driverKey / kind / displayName / description / configSchema）
- PluginRow struct（id / pluginKey / status / environmentDrivers[]）
- ResolvedSandboxProviderDriver struct（plugin + driver）
- ReadyPluginEnvironmentDriver struct（pluginId / pluginKey / driverKey + 5 个可选字段）

### 2) PluginRegistry trait
- `fn list(&self) -> Vec<PluginRow>`
- Send + Sync bound

### 3) InMemoryPluginRegistry
- Arc<Mutex<Vec<PluginRow>>>
- add_plugin / set_plugins / plugin_count
- 实现 PluginRegistry::list()

### 4) resolve_sandbox_provider_driver_key
- 1:1 镜像 Node 逻辑
- 参数：registry + workerManager（Option） + driver_key + require_running
- 返回 Option<ResolvedSandboxProviderDriver>
- 算法：
  - 遍历 plugins
  - 找到 driverKey 匹配 + kind=SandboxProvider 的 driver
  - requireRunning=true 时：status == Ready && worker_manager.is_running
  - requireRunning=true 且 worker_manager=None 时返回 None（Node 语义：".?."）

### 5) list_ready_sandbox_provider_drivers (subset)
- 1:1 镜像 Node listReadyPluginEnvironmentDrivers 的核心（不含 recovery 流程）
- 返回 ReadyPluginEnvironmentDriver[]

## 测试覆盖（19 个 case）

### 基础查找 (5)
- 空 registry 返回 None
- plugin 无 drivers 返回 None
- plugin driverKey 不匹配返回 None
- env driver kind 排除出 sandbox 搜索
- 找到 sandbox_provider by key

### 多 plugin (1)
- multiple plugins first match wins

### requireRunning 路径 (4)
- requireRunning 跳过 not-ready plugin
- requireRunning 跳过 worker not running
- requireRunning + 无 workerManager 返回 None（Node 语义）
- requireRunning + 全部检查通过

### Registry state (1)
- set_plugins 替换整个列表

### Serde (3)
- PluginStatus serde roundtrip
- PluginDriverKind serde roundtrip
- ResolvedSandboxProviderDriver serde roundtrip

### Default impl (2)
- PluginRow::default() 全零状态
- PluginEnvironmentDriverDecl::default() 全零状态

### listReady (2)
- list_ready_sandbox_provider_drivers 过滤 ready + running + sandbox
- list_ready 空 registry

### Sérialisation (1)
- ReadyPluginEnvironmentDriver skip None fields

## 真实验证

### 编译
cargo test -p pc-environment --test plugin_registry_tests → 0 errors / 2 test warnings

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
- plugin_environment_driver_validate_tests: 13 ✅
- plugin_environment_driver_validate_config_tests: 19 ✅
- plugin_worker_manager_tests: 19 ✅
- **plugin_registry_tests: 19 ✅ ← R686 新增**
- **合计 343, 0 fail**

### pc-plugin-database 回归
- 47 / 47 PASS

## 文件改动
- crates/pc-environment/src/plugin_registry.rs (新建)
- crates/pc-environment/tests/plugin_registry_tests.rs (新建)
- crates/pc-environment/src/lib.rs (+1 行) 添加 pub mod plugin_registry

## 设计要点

### PluginStatus + PluginDriverKind enum
- 1:1 镜像 Node string union
- #[serde(rename_all = "snake_case")] 保证 wire format 一致
- #[default] 用于 PluginRow::default()

### PluginRow 结构
- 镜像 Node plugin row 的核心字段
- environmentDrivers: Vec<PluginEnvironmentDriverDecl>（不嵌套 manifestJson）
- 与 Node 解耦（Node 有完整 manifestJson，R686 只用 environment_drivers slice）

### PluginRegistry trait 抽象
- 与 Node pluginRegistryService 一致
- list() 返回 Vec（不是 Iterator，避免生命周期复杂度）
- Send + Sync 让 trait 可跨 async 边界

### InMemoryPluginRegistry
- Arc<Mutex<Vec>> 而非 RwLock（写少读多但 list() 需要 clone Vec，开销不大）
- add_plugin / set_plugins API 区分增量与全量

### requireRunning 的 workerManager=Option
- Node: `input.workerManager?.isRunning(plugin.id)` → undefined?.isRunning → false
- Rust: Option<&dyn PluginWorkerManager> 显式表达
- requireRunning=true 但 workerManager=None → None（与 Node 语义镜像）

### First match wins
- Node: 遍历遇到第一个 match 就返回
- Rust: `for ... { ... return Some(...) }` 同样语义
- 测试覆盖：多个 plugin 第一个匹配返回

### listReady (subset)
- R686 只做纯过滤逻辑（无 recovery 异步流程）
- recovery 部分（R686 注释中标 defer）需要专门 round R687+ 处理
- 现在能用：ready + running + sandbox_provider

## R687+ 路径

R686 完成 resolve + listReady 的非 DB 部分。R687 需要：
- 把 R685（validate）+ R686（resolve）拼装为 validatePluginSandboxProviderConfig 完整函数
- 添加 typed Error（NotFound vs WorkerRpc）
- trait 组合：&dyn PluginRegistry + Option<&dyn PluginWorkerManager>

后续 R688+：
- probePluginEnvironmentDriver parity
- listReadyPluginEnvironmentDrivers 完整（含 recovery）
- validatePluginEnvironmentDriverConfig parity
- resumePluginEnvironmentLease + destroyPluginEnvironmentLease
- realizePluginEnvironmentWorkspace + executePluginEnvironmentCommand

## 进度更新
- 核心域覆盖度：99.89% → 99.91%（+0.02%）
- 单元测试：6,715 → 6,734（+19）
- 下一步：R687 = 完整 validatePluginSandboxProviderConfig async 编排（R685 + R686 拼装）
